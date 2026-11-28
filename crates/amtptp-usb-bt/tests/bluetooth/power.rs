//! Stop/resume, PnP restart and recovery sequences of the corrected driver (no C oracle: these are the
//! cases where the C driver was wrong; see `wire.rs` and `lifecycle.rs` for the replayed records).

use super::common;

use amtptp_usb_bt::bluetooth::config::MAX_IMMEDIATE_READ_RETRIES;
use amtptp_usb_bt::bluetooth::{
    DeviceState, LowerReadClass, NtStatus, ReadTicket, STATUS_CANCELLED, STATUS_SUCCESS,
};
use common::dispatch::dispatch_internal_ioctl;
use common::pipeline::{LowerReadOutcome, complete_lower_read, work_item_callback};
use common::pnp::{
    d0_entry, d0_exit, prepare_hardware, recovery_timer_callback, self_managed_io_init,
    self_managed_io_restart, source_closed, source_opened,
};
use common::*;

use amtptp_core::hid::input::PTP_REPORT_LEN;
use amtptp_core::hid::ioctl::IOCTL_HID_READ_REPORT;

/// `STATUS_DEVICE_NOT_CONNECTED`: what a lower read fails with once the Bluetooth link is gone.
const DEVICE_NOT_CONNECTED: NtStatus = 0xC000_009D_u32 as NtStatus;
/// `STATUS_DEVICE_DATA_ERROR`: what `HidBth` fails a read with now and then during normal use.
const DEVICE_DATA_ERROR: NtStatus = 0xC000_009C_u32 as NtStatus;
/// A one-contact Bluetooth multitouch packet (report ID 0x31).
const BT_PACKET: [u8; 13] = [0x31, 0, 0, 0, 0, 0, 0, 0, 0x14, 0x14, 0, 0, 0];

/// The completion of lower read `sent` (its id and ticket) with `status` and `packet` in its buffer
/// (`Information` = the packet length).
fn complete(
    m: &mut Mock,
    state: &DeviceState,
    sent: (usize, ReadTicket),
    status: NtStatus,
    packet: &[u8],
) -> LowerReadOutcome {
    m.lower_buf[..packet.len()].copy_from_slice(packet);
    let (read, ticket) = sent;
    let outcome = complete_lower_read(m, state, read, (), ticket, status, packet.len());
    m.lower_reads_pending -= 1;
    outcome
}

fn work_items(m: &Mock) -> usize {
    m.ev.iter()
        .filter(|e| e.starts_with("WdfWorkItemEnqueue("))
        .count()
}

/// A Bluetooth Magic Trackpad 2.
fn bluetooth() -> (DeviceState, Mock) {
    (DeviceState::new(), Mock::new())
}

/// `D0Entry` + `SelfManagedIoInit`, then the data source opens, as at the first start.
fn start(state: &DeviceState, m: &mut Mock) {
    assert_eq!(d0_entry(state), 0);
    assert_eq!(self_managed_io_init(m, state), 0);
    source_opened(m, state);
}

/// An upper `READ_REPORT` from HIDCLASS.
fn read_report(state: &DeviceState, m: &mut Mock) -> usize {
    let req = m.add_upper(Upper::with_output(50, 0xEE));
    dispatch_internal_ioctl(m, state, req, 50, 0, IOCTL_HID_READ_REPORT);
    req
}

fn timer_starts(m: &Mock) -> usize {
    m.ev.iter()
        .filter(|e| e.starts_with("WdfTimerStart("))
        .count()
}

/// `Init -> D0Exit -> PrepareHardware -> D0Entry -> Restart` (a resource rebalance): the IDs survive
/// `PrepareHardware`, so the restart sends the Bluetooth mode switch again and configures the device.
#[test]
fn pnp_restart_sends_the_mode_switch_again() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    assert!(state.device_configured());
    assert_eq!(d0_exit(&mut m, &state), 0);
    assert_eq!(prepare_hardware(&state), 0);
    assert!(!state.device_configured());
    assert_eq!(d0_entry(&state), 0);
    m.ev.clear();
    assert_eq!(self_managed_io_restart(&mut m, &state), 0);
    assert_eq!(m.sends.len(), 2);
    assert_eq!(m.sends[1].xfer_id, 0xF1);
    assert_eq!(m.sends[1].payload, [0xF1, 0x02, 0x01]);
    assert!(state.device_configured());
    assert_eq!((state.vendor_id(), state.product_id()), (0x004C, 0x0265));
    assert_eq!(timer_starts(&m), 0);
}

/// `WdfRequestSend` returns TRUE but the lower driver failed the `SET_FEATURE`: the start reports success,
/// the device stays unconfigured and the recovery timer is armed.
#[test]
fn lower_failure_of_a_sent_mode_switch_is_not_success() {
    let (state, mut m) = bluetooth();
    m.faults.cfg_send_true_status = Some(TRUE_STATUS_FAIL);
    start(&state, &mut m);
    assert!(!state.device_configured());
    assert!(m.ev.contains(&"WdfRequestGetStatus(req=cfg_req)=0xc0000001".to_owned()));
    assert_eq!(timer_starts(&m), 1);
    // The next attempt succeeds and configures the device.
    m.faults.cfg_send_true_status = None;
    recovery_timer_callback(&mut m, &state);
    assert!(state.device_configured());
}

/// A recovery timer that fires after `D0Exit` does nothing: no mode switch, no read, no new timer.
#[test]
fn recovery_timer_after_d0_exit_does_nothing() {
    let (state, mut m) = bluetooth();
    m.faults.cfg_send_false = Some(SEND_FAIL);
    start(&state, &mut m);
    assert_eq!(timer_starts(&m), 1);
    assert_eq!(d0_exit(&mut m, &state), 0);
    m.faults.cfg_send_false = None;
    m.ev.clear();
    recovery_timer_callback(&mut m, &state);
    assert_eq!(m.ev, Vec::<String>::new());
    assert!(!state.device_configured());
    assert_eq!(m.sends.len(), 1);
}

/// Upper reads parked while the mode switch failed get their lower reads when the timer configures the
/// device.
#[test]
fn recovery_timer_serves_the_parked_reads() {
    let (state, mut m) = bluetooth();
    m.faults.cfg_send_false = Some(SEND_FAIL);
    start(&state, &mut m);
    let _first = read_report(&state, &mut m);
    let _second = read_report(&state, &mut m);
    assert_eq!(m.read_queue.len(), 2);
    assert_eq!(m.lower_reads_pending, 0);
    m.faults.cfg_send_false = None;
    recovery_timer_callback(&mut m, &state);
    assert!(state.device_configured());
    assert_eq!(m.lower_reads_pending, 2);
    assert_eq!(m.in_flight(), 2);
}

/// A failed retry arms the timer again while the device is in D0, and issues no read.
#[test]
fn failed_recovery_rearms_the_timer() {
    let (state, mut m) = bluetooth();
    m.faults.cfg_send_false = Some(SEND_FAIL);
    start(&state, &mut m);
    let _parked = read_report(&state, &mut m);
    recovery_timer_callback(&mut m, &state);
    assert_eq!(timer_starts(&m), 2);
    assert_eq!(m.lower_reads_pending, 0);
    assert!(!state.device_configured());
}

/// The timer armed before a stop fires after the successful restart: the device is configured, so it
/// neither sends the mode switch again nor issues a read.
#[test]
fn stale_timer_after_successful_restart_does_nothing() {
    let (state, mut m) = bluetooth();
    m.faults.cfg_send_false = Some(SEND_FAIL);
    start(&state, &mut m);
    assert_eq!(d0_exit(&mut m, &state), 0);
    assert_eq!(d0_entry(&state), 0);
    m.faults.cfg_send_false = None;
    assert_eq!(self_managed_io_restart(&mut m, &state), 0);
    assert!(state.device_configured());
    m.ev.clear();
    recovery_timer_callback(&mut m, &state);
    assert_eq!(m.ev, Vec::<String>::new());
    assert_eq!(m.sends.len(), 2);
}

/// An upper read parked between `D0Exit` and the restart gets its lower read from the restart.
#[test]
fn restart_resumes_reads_parked_while_stopped() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    assert_eq!(d0_exit(&mut m, &state), 0);
    let _parked = read_report(&state, &mut m);
    assert_eq!(m.lower_reads_pending, 0, "no read for a stopped device");
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_restart(&mut m, &state), 0);
    assert_eq!(m.lower_reads_pending, 1);
}

/// The work item (after a zero-length completion) re-issues the missing read for a configured device,
/// and does nothing for a stopped one.
#[test]
fn work_item_only_serves_a_configured_device() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let _parked = read_report(&state, &mut m);
    assert_eq!(m.in_flight(), 1);
    let ticket = m.tickets[0];
    assert_eq!(
        complete(&mut m, &state, ticket, STATUS_SUCCESS, &[]),
        LowerReadOutcome::Handled(LowerReadClass::Empty)
    );
    assert!(m.ev.contains(&"WdfWorkItemEnqueue(item=workitem)".to_owned()));
    assert_eq!(m.in_flight(), 0);
    work_item_callback(&mut m, &state);
    assert_eq!(m.lower_reads_pending, 1);
    assert_eq!(m.in_flight(), 1);

    assert_eq!(d0_exit(&mut m, &state), 0);
    m.ev.clear();
    work_item_callback(&mut m, &state);
    assert_eq!(m.ev, Vec::<String>::new());
}

/// HF-B7 (P3): a failed lower read is re-issued at once by the work item, as C did. `HidBth` fails a read
/// with `STATUS_DEVICE_DATA_ERROR` now and then during normal use; waiting 3 s for each froze input on
/// hardware once two failed in a row.
#[test]
fn isolated_failed_reads_are_reissued_at_once() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let upper = read_report(&state, &mut m);
    for _ in 0..2 {
        m.ev.clear();
        let sent = *m.tickets.last().expect("a read was sent");
        assert_eq!(
            complete(&mut m, &state, sent, DEVICE_DATA_ERROR, &[]),
            LowerReadOutcome::Handled(LowerReadClass::Failed {
                status: DEVICE_DATA_ERROR
            })
        );
        assert_eq!((work_items(&m), timer_starts(&m)), (1, 0));
        assert!(
            m.uppers[upper].completed.is_none(),
            "the upper read stays parked"
        );
        work_item_callback(&mut m, &state);
        assert_eq!(m.in_flight(), 1, "the work item issued the read again");
    }
    // A successful read ends the run of failures and serves the upper read.
    let sent = *m.tickets.last().expect("a read was sent");
    assert_eq!(
        complete(&mut m, &state, sent, STATUS_SUCCESS, &BT_PACKET),
        LowerReadOutcome::Handled(LowerReadClass::Report { length: 13 })
    );
    assert_eq!(m.uppers[upper].completed, Some((0, PTP_REPORT_LEN)));
}

/// HF-B7 (P3): a lower driver that fails every read at once (here the Bluetooth link is gone) cannot keep
/// the work item in a zero-delay loop: after `MAX_IMMEDIATE_READ_RETRIES` failures in a row the next
/// failure waits for the 3 s recovery timer, which then issues the read. A success resets the count.
#[test]
fn a_run_of_failed_reads_backs_off_to_the_timer() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let _upper = read_report(&state, &mut m);
    let fail_once = |m: &mut Mock| {
        m.ev.clear();
        let sent = *m.tickets.last().expect("a read was sent");
        let _outcome = complete(m, &state, sent, DEVICE_NOT_CONNECTED, &[]);
        (work_items(m), timer_starts(m))
    };
    for _ in 0..MAX_IMMEDIATE_READ_RETRIES {
        assert_eq!(fail_once(&mut m), (1, 0));
        work_item_callback(&mut m, &state);
    }
    assert_eq!(
        fail_once(&mut m),
        (0, 1),
        "the next failure waits for the timer"
    );
    assert!(m.ev.contains(&"WdfTimerStart(timer=timer,due=-30000000)".to_owned()));
    assert_eq!(m.in_flight(), 0);
    recovery_timer_callback(&mut m, &state);
    assert_eq!(m.in_flight(), 1, "the timer issued the read again");

    // A completion that is not a failure ends the run: the next failure is re-issued at once again.
    let sent = *m.tickets.last().expect("a read was sent");
    let _outcome = complete(&mut m, &state, sent, STATUS_SUCCESS, &[]);
    work_item_callback(&mut m, &state);
    assert_eq!(fail_once(&mut m), (1, 0));
}

/// HF-B14 (P3): `D0Exit` stops the recovery timer, cancels the lower reads in flight and completes the
/// parked upper reads. A cancelled read that completes afterwards (even after the resume, with data) is
/// stale: it serves no upper read and arms nothing.
#[test]
fn d0_exit_cancels_the_reads_in_flight_and_drops_their_completions() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let first = read_report(&state, &mut m);
    let _second = read_report(&state, &mut m);
    assert_eq!(m.in_flight(), 2);
    let old = m.tickets[0];
    assert_eq!(d0_exit(&mut m, &state), 0);
    assert_eq!((m.timer_stops, m.cancelled_reads), (1, 2));
    assert_eq!(m.in_flight(), 0);
    assert_eq!(
        m.uppers[first].completed.map(|(s, _)| s),
        Some(STATUS_CANCELLED)
    );

    // Resume; a new upper read gets a new lower read.
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_restart(&mut m, &state), 0);
    let upper = read_report(&state, &mut m);
    assert_eq!(m.in_flight(), 1);
    m.ev.clear();
    // The old reads complete late: one cancelled, one with valid data.
    assert_eq!(
        complete(&mut m, &state, old, STATUS_CANCELLED, &[]),
        LowerReadOutcome::Stale
    );
    assert_eq!(
        complete(&mut m, &state, old, STATUS_SUCCESS, &BT_PACKET),
        LowerReadOutcome::Stale
    );
    assert_eq!(
        m.ev,
        [
            "WdfObjectDelete(obj=lower_read)",
            "WdfObjectDelete(obj=lower_mem)",
            "WdfObjectDelete(obj=lower_read)",
            "WdfObjectDelete(obj=lower_mem)",
        ]
    );
    assert!(m.uppers[upper].completed.is_none());
    assert_eq!(m.in_flight(), 1, "the current read is still registered");
    // The current read serves the new upper read.
    let current = *m.tickets.last().expect("a read was sent");
    assert_eq!(
        complete(&mut m, &state, current, STATUS_SUCCESS, &BT_PACKET),
        LowerReadOutcome::Handled(LowerReadClass::Report { length: 13 })
    );
    assert_eq!(m.uppers[upper].completed, Some((0, PTP_REPORT_LEN)));
    assert_eq!(m.in_flight(), 0);
}

/// After `D0Exit` neither the work item nor the timer sends a read.
#[test]
fn no_read_is_sent_after_d0_exit() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let _parked = read_report(&state, &mut m);
    assert_eq!(d0_exit(&mut m, &state), 0);
    let _late = read_report(&state, &mut m);
    m.ev.clear();
    work_item_callback(&mut m, &state);
    recovery_timer_callback(&mut m, &state);
    assert_eq!(m.ev, Vec::<String>::new());
    assert_eq!(m.in_flight(), 0);
}

/// HF-B8 (P3): a report that is not the multitouch report (here a battery report, 0x90) is dropped
/// and the work item issues the next read, instead of being parsed as fingers or stalling input for
/// 3 s.
#[test]
fn another_report_is_dropped_and_the_read_reissued() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let upper = read_report(&state, &mut m);
    let ticket = m.tickets[0];
    m.ev.clear();
    assert_eq!(
        complete(&mut m, &state, ticket, STATUS_SUCCESS, &[0x90, 0x04, 0x64]),
        LowerReadOutcome::Handled(LowerReadClass::OtherReport { report_id: 0x90 })
    );
    assert_eq!((work_items(&m), timer_starts(&m)), (1, 0));
    assert!(m.uppers[upper].completed.is_none());
    work_item_callback(&mut m, &state);
    assert_eq!(
        m.lower_reads_pending, 1,
        "the work item issued the read again"
    );
}

/// Not in C: after its power switch, the trackpad comes back in mouse mode while the HID device stays
/// (hardware: 8-byte reports with ID 0x02, cursor dead). Another report makes the work item send the
/// Bluetooth mode switch again, once, before it re-issues the read; the device stays configured.
#[test]
fn mouse_mode_reports_make_the_work_item_send_the_mode_switch_again() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    assert_eq!(m.sends.len(), 1, "the start sent the mode switch");
    let _upper = read_report(&state, &mut m);
    let mouse_report = [0x02, 0x01, 0, 0, 0, 0, 0, 0];
    let sent = m.tickets[0];
    let _outcome = complete(&mut m, &state, sent, STATUS_SUCCESS, &mouse_report);
    assert!(state.device_configured(), "input keeps flowing meanwhile");
    work_item_callback(&mut m, &state);
    assert_eq!(m.sends.len(), 2, "the work item sent the mode switch again");
    assert_eq!(m.sends[1].payload, [0xF1, 0x02, 0x01]);
    assert_eq!(m.in_flight(), 1, "and issued the read again");
    // The request is taken once: the next work item does not switch again.
    work_item_callback(&mut m, &state);
    assert_eq!(m.sends.len(), 2);
}

/// A mode-switch request still pending at `D0Exit` is not acted on: the restart sends the switch.
#[test]
fn no_mode_switch_from_the_work_item_after_d0_exit() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let _upper = read_report(&state, &mut m);
    let sent = m.tickets[0];
    let _outcome = complete(
        &mut m,
        &state,
        sent,
        STATUS_SUCCESS,
        &[0x02, 0, 0, 0, 0, 0, 0, 0],
    );
    assert_eq!(d0_exit(&mut m, &state), 0);
    m.ev.clear();
    work_item_callback(&mut m, &state);
    assert_eq!(m.ev, Vec::<String>::new());
    assert_eq!(m.sends.len(), 1);
}

/// HF-B29 (P3): `Information` above the 1024-byte buffer is not a packet length (C took its low 32
/// bits, so `0x1_0000_000D` was a 13-byte packet): the 3 s timer, no report.
#[test]
fn information_above_the_buffer_is_not_a_length() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let upper = read_report(&state, &mut m);
    let ticket = m.tickets[0];
    m.lower_buf[..BT_PACKET.len()].copy_from_slice(&BT_PACKET);
    m.ev.clear();
    assert_eq!(
        complete_lower_read(
            &mut m,
            &state,
            ticket.0,
            (),
            ticket.1,
            STATUS_SUCCESS,
            0x1_0000_000D
        ),
        LowerReadOutcome::Handled(LowerReadClass::Oversize)
    );
    assert_eq!(timer_starts(&m), 1);
    assert!(m.uppers[upper].completed.is_none());
}

// ---- the data source (the vendor collection; not in C) ------------------------------------------

/// Until the data source opens, the start sends nothing and parks the upper reads; when it opens, the
/// mode switch goes out and every parked read gets its lower read.
#[test]
fn the_source_opening_configures_the_device_and_serves_parked_reads() {
    let (state, mut m) = bluetooth();
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_init(&mut m, &state), 0);
    assert_eq!((state.vendor_id(), state.product_id()), (0x004C, 0x0265));
    assert!(!state.source_open() && !state.device_configured());
    assert!(m.ev.is_empty(), "{:?}", m.ev);
    let _first = read_report(&state, &mut m);
    let _second = read_report(&state, &mut m);
    assert_eq!((m.read_queue.len(), m.lower_reads_pending), (2, 0));

    source_opened(&mut m, &state);
    assert!(state.source_open() && state.device_configured());
    assert_eq!(m.sends.len(), 1);
    assert_eq!(m.sends[0].payload, [0xF1, 0x02, 0x01]);
    assert_eq!(m.lower_reads_pending, 2);
}

/// The source going away (query-remove) unconfigures the device and cancels the reads in flight; the
/// upper reads stay parked, and the source coming back configures the device again.
#[test]
fn the_source_closing_cancels_reads_and_reopening_reconfigures() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);
    let _first = read_report(&state, &mut m);
    assert_eq!(m.in_flight(), 1);

    source_closed(&mut m, &state);
    assert!(!state.source_open() && !state.device_configured());
    assert_eq!(m.cancelled_reads, 1);
    assert_eq!(m.in_flight(), 0);
    assert_eq!(m.read_queue.len(), 1, "the upper read stays parked");
    // Nothing is sent while the source is closed.
    let pending = m.lower_reads_pending;
    let _second = read_report(&state, &mut m);
    work_item_callback(&mut m, &state);
    recovery_timer_callback(&mut m, &state);
    assert_eq!(m.lower_reads_pending, pending);
    assert_eq!(m.sends.len(), 1);

    source_opened(&mut m, &state);
    assert!(state.device_configured());
    assert_eq!(m.sends.len(), 2);
    assert_eq!(m.in_flight(), 2, "both parked reads get a lower read");
}

/// A resume before the source opened does nothing (C: HF-B11 marked the device configured and failed
/// the restart); a resume with the source open sends the mode switch.
#[test]
fn restart_waits_for_the_source() {
    let (state, mut m) = bluetooth();
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_init(&mut m, &state), 0);
    assert_eq!(d0_exit(&mut m, &state), 0);
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_restart(&mut m, &state), STATUS_SUCCESS);
    assert!(!state.device_configured());
    assert!(m.sends.is_empty());

    source_opened(&mut m, &state);
    assert!(state.device_configured());
    assert_eq!(d0_exit(&mut m, &state), 0);
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_restart(&mut m, &state), STATUS_SUCCESS);
    assert!(state.device_configured());
    assert_eq!(m.sends.len(), 2);
}

/// The source opening while the device is out of D0 only records it; the restart configures.
#[test]
fn the_source_opening_out_of_d0_waits_for_the_restart() {
    let (state, mut m) = bluetooth();
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_init(&mut m, &state), 0);
    assert_eq!(d0_exit(&mut m, &state), 0);
    source_opened(&mut m, &state);
    assert!(state.source_open() && !state.device_configured());
    assert!(m.sends.is_empty());
    assert_eq!(d0_entry(&state), 0);
    assert_eq!(self_managed_io_restart(&mut m, &state), STATUS_SUCCESS);
    assert!(state.device_configured());
    assert_eq!(m.sends.len(), 1);
}

#[test]
fn suspend_between_prepare_and_send_cancels_once_sent() {
    use amtptp_usb_bt::bluetooth::PostSendAction;

    let (state, mut m) = bluetooth();
    start(&state, &mut m);

    // Phase 1: lower read is prepared while configured, before WdfRequestSend runs.
    let ticket = m.tracker.prepare(42).expect("prepared");
    assert_eq!(m.in_flight(), 1);

    // Concurrent D0Exit runs before WdfRequestSend returns: must NOT cancel an unsent request,
    // only mark it CancelWhenSent.
    assert_eq!(d0_exit(&mut m, &state), 0);
    assert_eq!(
        m.cancelled_reads, 0,
        "unsent request must not be cancelled early"
    );

    // Phase 2: WdfRequestSend succeeds; complete_send now returns CancelSent.
    let action = m.tracker.complete_send(42, ticket, true);
    assert_eq!(action, PostSendAction::CancelSent);
}

#[test]
fn per_request_buffer_error_does_not_fail_device_or_consume_liftoff() {
    let (state, mut m) = bluetooth();
    start(&state, &mut m);

    // Frame 1: touch down delivered to host (contact ID 4 at positive tip pressure).
    let touch_req = read_report(&state, &mut m);
    let sent1 = *m.tickets.last().expect("sent 1");
    let mut touch_pkt = [0_u8; 13];
    touch_pkt[0] = 0x31;
    touch_pkt[4 + 4] = 30; // tip pressure > 0
    touch_pkt[4 + 8] = 4; // hw contact id 4
    assert_eq!(
        complete(&mut m, &state, sent1, STATUS_SUCCESS, &touch_pkt),
        LowerReadOutcome::Handled(LowerReadClass::Report { length: 13 })
    );
    assert_eq!(m.uppers[touch_req].completed, Some((0, PTP_REPORT_LEN)));
    assert_eq!(m.uppers[touch_req].buf[48], 1);

    // Frame 2: empty packet (liftoff), but upper request buffer is too short (10 bytes).
    // Must complete ONLY that request with BUFFER_TOO_SMALL, WITHOUT WdfDeviceSetFailed,
    // and WITHOUT advancing ContactTracker past the liftoff!
    let bad_upper = m.add_upper(Upper::with_output(10, 0xEE));
    dispatch_internal_ioctl(&mut m, &state, bad_upper, 10, 0, IOCTL_HID_READ_REPORT);
    let sent2 = *m.tickets.last().expect("sent 2");
    m.ev.clear();
    let empty_pkt = [0x31, 0, 0, 0];
    assert_eq!(
        complete(&mut m, &state, sent2, STATUS_SUCCESS, &empty_pkt),
        LowerReadOutcome::Handled(LowerReadClass::Report { length: 4 })
    );
    assert!(
        !m.ev.iter().any(|e| e.starts_with("WdfDeviceSetFailed(")),
        "single-request buffer error must not restart device: {:?}",
        m.ev
    );
    assert_eq!(
        m.uppers[bad_upper].completed,
        Some((0xC000_0023_u32 as i32, INFO_IN))
    );

    // Frame 3: next valid upper READ_REPORT receives the preserved liftoff report!
    let next_req = read_report(&state, &mut m);
    let sent3 = *m.tickets.last().expect("sent 3");
    assert_eq!(
        complete(&mut m, &state, sent3, STATUS_SUCCESS, &empty_pkt),
        LowerReadOutcome::Handled(LowerReadClass::Report { length: 4 })
    );
    assert_eq!(m.uppers[next_req].completed, Some((0, PTP_REPORT_LEN)));
    assert_eq!(
        (m.uppers[next_req].buf[1] & 0x02, m.uppers[next_req].buf[48]),
        (0x00, 1),
        "liftoff frame delivered to the next valid READ_REPORT"
    );
}

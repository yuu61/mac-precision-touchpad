//! Stop/resume, PnP restart and recovery sequences of the corrected driver (no C oracle: these are the
//! cases where the C driver was wrong; see `wire.rs` and `lifecycle.rs` for the replayed records).

#![cfg(test)]

mod common;

use amtptp_hidfilter_core::{
    DeviceState, complete_lower_read, d0_entry, d0_exit, dispatch_internal_ioctl,
    prepare_hardware, recovery_timer_callback, self_managed_io_init, self_managed_io_restart,
    work_item_callback,
};
use common::*;
use ptp_hid::ioctl::IOCTL_HID_READ_REPORT;

/// A Bluetooth Magic Trackpad 2 behind a working detour.
fn bluetooth() -> (DeviceState, Mock) {
    let mut m = Mock::new();
    m.attr = (0x004C, 0x0265, 0x0100);
    (DeviceState::new(), m)
}

/// `D0Entry` + `SelfManagedIoInit`, as at the first start.
fn start(state: &DeviceState, m: &mut Mock) {
    assert_eq!(d0_entry(state), 0);
    assert_eq!(self_managed_io_init(m, state), 0);
}

/// An upper `READ_REPORT` from HIDCLASS.
fn read_report(state: &DeviceState, m: &mut Mock) -> usize {
    let req = m.add_upper(Upper::with_output(50, 0xEE));
    dispatch_internal_ioctl(m, state, req, 50, 0, IOCTL_HID_READ_REPORT);
    req
}

fn timer_starts(m: &Mock) -> usize {
    m.ev.iter().filter(|e| e.starts_with("WdfTimerStart(")).count()
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
    assert!(
        m.ev
            .contains(&"WdfRequestGetStatus(req=cfg_req)=0xc0000001".to_owned())
    );
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
    assert_eq!(state.in_flight_reads(), 2);
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
    assert_eq!(state.in_flight_reads(), 1);
    complete_lower_read(&mut m, &state, (), (), 0);
    m.lower_reads_pending -= 1;
    assert!(m.ev.contains(&"WdfWorkItemEnqueue(item=workitem)".to_owned()));
    assert_eq!(state.in_flight_reads(), 0);
    work_item_callback(&mut m, &state);
    assert_eq!(m.lower_reads_pending, 1);
    assert_eq!(state.in_flight_reads(), 1);

    assert_eq!(d0_exit(&mut m, &state), 0);
    m.ev.clear();
    work_item_callback(&mut m, &state);
    assert_eq!(m.ev, Vec::<String>::new());
}

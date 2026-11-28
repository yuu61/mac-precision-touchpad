//! The read pipeline (Input.c): one lower `IOCTL_HID_READ_REPORT` per trigger, and its completion,
//! which turns the raw packet into one 50-byte PTP report for one parked upper read.
//!
//! Triggers of a lower read (spec 6.5): an upper `READ_REPORT` while `DeviceConfigured`, and
//! [`issue_backlog_reads`] after the device became configured, from the recovery timer, and from the
//! recovery work item after an empty, failed or other report. A successful completion does not issue
//! another read; the pipeline is 1:1 with upper reads.
//!
//! Every lower read is registered in the device's read tracker (`with_reads`) before it is
//! sent, and unregistered when it completes (not in C). The registration is refused once the device is
//! not configured, `D0Exit` cancels the reads in flight and starts a new epoch, and a read sent before
//! that is dropped when it completes ([`LowerReadOrigin::Stale`]), so it never serves an upper read
//! parked after the resume (P3 fix of HF-B14). The reads in flight of the current epoch are what the
//! backlog subtracts from the parked upper reads.

use amtptp_core::domain::Contact;
use amtptp_core::hid::common::MAX_FINGERS;
use amtptp_core::hid::delivery::{CompletionQueue, EnqueueOutcome};
use amtptp_core::hid::input::PTP_REPORT_LEN;
use amtptp_core::hid::read_tracker::{LowerReadOrigin, PostSendAction, ReadTicket, ReadTracker};

use super::pnp::configure_multitouch;
use super::{FailedAction, Filter, LowerRead, QueuedCompletion, ReadMemory};
use amtptp_usb_bt::bluetooth::config::TRANSPORT_RETRY_DUE_TIME;
use amtptp_usb_bt::bluetooth::{ContactTracker, DeviceState, ReadAction};
use amtptp_usb_bt::bluetooth::{
    LowerReadClass, classify_lower_read, decode_raw_packet, finish_tracked_report,
};
use amtptp_usb_bt::bluetooth::{NtStatus, nt_success};

type DecodedFrame = ([Contact; MAX_FINGERS as usize], usize, bool);

/// `PtpFilterInputIssueTransportRequest` (Input.c:39-117): creates, formats and sends one lower read
/// with two-phase `prepare` / `complete_send` ownership so `WdfRequestCancelSentRequest` is never
/// called before `WdfRequestSend` succeeds.
pub(crate) fn issue_lower_read(io: &mut Filter<'_>, state: &DeviceState) {
    if !state.source_open() {
        return;
    }
    let Ok(read) = io.create_lower_read() else {
        state.set_device_configured(false);
        io.start_recovery_timer(TRANSPORT_RETRY_DUE_TIME);
        return;
    };
    let Ok(memory) = io.create_lower_read_memory(read) else {
        io.delete_lower_read(read);
        io.set_failed(FailedAction::AttemptRestart);
        return;
    };
    if !nt_success(io.format_lower_read(read, memory)) {
        io.delete_lower_memory(memory);
        io.delete_lower_read(read);
        io.set_failed(FailedAction::AttemptRestart);
        return;
    }
    io.set_lower_read_completion(read);
    let Some(ticket) = io.prepare_lower_read(state, read) else {
        io.delete_lower_memory(memory);
        io.delete_lower_read(read);
        return;
    };
    let sent = io.send_lower_read(read, ticket);
    if io.complete_lower_read_send(read, ticket, sent) == PostSendAction::DeleteUnsent {
        state.set_device_configured(false);
        io.start_recovery_timer(TRANSPORT_RETRY_DUE_TIME);
        io.delete_lower_memory(memory);
        io.delete_lower_read(read);
    }
}

/// While the device is configured, issues one lower read for every parked upper read that no read in
/// flight serves (`WdfIoQueueGetState` minus the reads of the current epoch in flight).
pub(crate) fn issue_backlog_reads(io: &mut Filter<'_>, state: &DeviceState) {
    if !state.device_configured() {
        return;
    }
    let in_flight = io.in_flight_reads();
    let backlog = io.parked_read_count().saturating_sub(in_flight);
    for _ in 0..backlog {
        issue_lower_read(io, state);
        if !state.device_configured() {
            break;
        }
    }
}

/// `PtpFilterWorkItemCallback` (Input.c:30-37), enqueued after an empty, failed or other report:
///
/// 1. if a report other than the multitouch report asked for it, and the device is configured, in D0
///    and its source is open, send the mode switch again ([`configure_multitouch`], `PASSIVE_LEVEL`). Not in C:
///    when the trackpad restarts (its power switch) while the HID device stays, it comes back in mouse
///    mode, and nothing else would switch it back (hardware: a stream of 8-byte reports with ID 0x02 and
///    a dead cursor until the device was re-added). A failure is not retried here; the next such report
///    asks again;
/// 2. [`issue_backlog_reads`]. C issued one lower read without checking `DeviceConfigured`, so the work
///    item also sent reads to a stopped or unconfigured device (P3 fix).
pub(crate) fn work_item_callback(io: &mut Filter<'_>, state: &DeviceState) {
    if state.take_mode_switch_request()
        && state.device_configured()
        && state.in_d0()
        && state.source_open()
    {
        let _status = configure_multitouch(io, state);
    }
    issue_backlog_reads(io, state);
}

/// What [`complete_lower_read`] did with a lower read (for the glue's trace).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LowerReadOutcome {
    /// The read was sent before the last `D0Exit` (usually cancelled by it): its data was dropped and
    /// nothing else happened.
    Stale,
    /// The read belongs to the current epoch and was handled according to its class.
    Handled(LowerReadClass),
}

/// `PtpFilterInputRequestCompletionCallback` (Input.c:119-253) for lower read `read`, completed with
/// `status` (`WdfRequestGetStatus`) and `information` (`WdfRequestGetInformation`, a `ULONG_PTR`).
///
/// Unregisters `read`, classifies the packet, and—when eligible to deliver—retrieves the oldest parked
/// upper `READ_REPORT` and commits `ContactTracker` inside the same critical section as epoch validation.
/// Upper `WdfRequestComplete` runs outside the spinlock in strict FIFO order via [`CompletionQueue`],
/// and per-request output-buffer failures complete only that request without calling `WdfDeviceSetFailed`.
pub(crate) fn complete_lower_read(
    io: &mut Filter<'_>,
    state: &DeviceState,
    read: LowerRead,
    memory: ReadMemory,
    ticket: ReadTicket,
    status: NtStatus,
    information: usize,
) -> LowerReadOutcome {
    let layout = state.input_layout();
    let (first_byte, decoded_frame) = {
        let buffer = io.lower_read_buffer(memory);
        let first = buffer.first().copied();
        let class_preview =
            classify_lower_read(state.vendor_id(), &layout, status, information, first);
        let frame = if let LowerReadClass::Report { length } = class_preview {
            Some(decode_raw_packet(&layout, length, buffer))
        } else {
            None
        };
        (first, frame)
    };

    let (outcome, action, queued) = io.with_pipeline(
        |io, reads: &mut ReadTracker<LowerRead>, contacts, completions| {
            if reads.finish(read, ticket) == LowerReadOrigin::Stale {
                return (LowerReadOutcome::Stale, ReadAction::Drop, None);
            }
            let class =
                classify_lower_read(state.vendor_id(), &layout, status, information, first_byte);
            let action = state.on_read(class);
            let queued = if matches!(action, ReadAction::Deliver(_)) {
                deliver_in_lock(io, state, decoded_frame.as_ref(), contacts, completions)
            } else {
                None
            };
            (LowerReadOutcome::Handled(class), action, queued)
        },
    );

    match action {
        ReadAction::FailDevice => io.set_failed(FailedAction::NoRestart),
        ReadAction::Retry => io.enqueue_recovery_work_item(),
        ReadAction::ArmTimer => io.start_recovery_timer(TRANSPORT_RETRY_DUE_TIME),
        ReadAction::Drop | ReadAction::Deliver(_) => {}
    }
    if let Some(enqueued) = queued {
        io.drain_upper_completions(enqueued);
    }
    io.delete_lower_read(read);
    io.delete_lower_memory(memory);
    outcome
}

fn deliver_in_lock(
    io: &mut Filter<'_>,
    state: &DeviceState,
    decoded: Option<&DecodedFrame>,
    contacts: &mut ContactTracker,
    completions: &mut CompletionQueue<QueuedCompletion>,
) -> Option<EnqueueOutcome<QueuedCompletion>> {
    let Ok(upper) = io.retrieve_next_parked_read() else {
        return None;
    };
    let scan_time = state.sample_scan_time(io.query_performance_counter());
    let (fingers, count, button_pressed) = decoded?;
    let slice = fingers.get(..*count).unwrap_or_default();
    let mut staged = *contacts;
    let report: [u8; PTP_REPORT_LEN] = finish_tracked_report(
        slice,
        *button_pressed,
        scan_time,
        state.effective_reporting(),
        &mut staged,
    );
    let upper_memory = match io.retrieve_output_memory(upper) {
        Ok(m) => m,
        Err(status) => return Some(completions.enqueue(Filter::pack_completion(upper, status))),
    };
    let status = io.copy_to_memory(upper_memory, &report);
    if !nt_success(status) {
        return Some(completions.enqueue(Filter::pack_completion(upper, status)));
    }
    *contacts = staged;
    io.set_information(upper, PTP_REPORT_LEN);
    Some(completions.enqueue(Filter::pack_completion(upper, status)))
}

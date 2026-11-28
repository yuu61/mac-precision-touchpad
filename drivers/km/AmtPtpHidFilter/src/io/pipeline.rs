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

use ptp_hid::input::PTP_REPORT_LEN;
use ptp_hid::read_tracker::{LowerReadOrigin, ReadTicket};

use super::pnp::configure_multitouch;
use super::{FailedAction, Filter, LowerRead, ReadMemory};
use amtptp_hidfilter_core::config::TRANSPORT_RETRY_DUE_TIME;
use amtptp_hidfilter_core::{DeviceState, ReadAction};
use amtptp_hidfilter_core::{
    InputLayout, LowerReadClass, classify_lower_read, decode_raw_packet, finish_tracked_report,
};
use amtptp_hidfilter_core::{NtStatus, nt_success};

/// `PtpFilterInputIssueTransportRequest` (Input.c:39-117): creates, formats and sends one lower read.
///
/// - `WdfRequestCreate` fails: `DeviceConfigured = FALSE`, recovery timer 3 s.
/// - lookaside memory fails: delete the request, `WdfDeviceSetFailed(AttemptRestart)`.
/// - formatting fails: delete memory and request, `WdfDeviceSetFailed(AttemptRestart)`.
/// - the read tracker refuses the read (not in C): the device is no longer configured (it left D0), or
///   every slot is taken. Delete memory and request; the upper read stays parked.
/// - `WdfRequestSend` returns FALSE: `DeviceConfigured = FALSE`, timer 3 s, delete memory and request.
///
/// Without an open data source nothing is issued (C, before the detour, would have formatted the read
/// against a NULL handle, HF-B25).
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
    // Registered before the send (the completion can run before `send_lower_read` returns). The
    // configured check is made under the tracker's lock, so a read racing with `D0Exit` is either
    // handed to its cancellation or refused here.
    let ticket = io.with_reads(|reads| {
        if state.device_configured() {
            reads.claim(read)
        } else {
            None
        }
    });
    let Some(ticket) = ticket else {
        io.delete_lower_memory(memory);
        io.delete_lower_read(read);
        return;
    };
    if !io.send_lower_read(read, ticket) {
        let _origin = io.with_reads(|reads| reads.finish(read, ticket));
        state.set_device_configured(false);
        io.start_recovery_timer(TRANSPORT_RETRY_DUE_TIME);
        io.delete_lower_memory(memory);
        io.delete_lower_read(read);
    }
}

/// While the device is configured, issues one lower read for every parked upper read that no read in
/// flight serves (`WdfIoQueueGetState` minus the reads of the current epoch in flight). Stops early if
/// an issue fails or is refused (the device is no longer configured). Does nothing for a device that
/// is not configured, e.g. one that left D0.
pub(crate) fn issue_backlog_reads(io: &mut Filter<'_>, state: &DeviceState) {
    if !state.device_configured() {
        return;
    }
    let in_flight = io.with_reads(|reads| reads.in_flight());
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
/// `memory` and `ticket` are the request context's `RequestMemory` and ticket; the glue reads them
/// **before** calling this (the C code reads `RequestMemory` after deleting the request, HF-B10). This
/// function ends by deleting the request and then the memory on every path, so the glue must not touch
/// either afterwards.
///
/// The read is unregistered from the tracker first. A [`LowerReadOrigin::Stale`] read (sent before the
/// last `D0Exit`) is only deleted. Otherwise, by [`classify_lower_read`] (C order for a successful
/// multitouch packet):
/// 1. vendor not Apple: `WdfDeviceSetFailed(NoRestart)`;
/// 2. failed read (P3 fix of HF-B7): enqueue the recovery work item, as C did for its zero-length
///    result, until `MAX_IMMEDIATE_READ_RETRIES` reads have failed in a row; after that the 3 s
///    recovery timer, while the device is in D0. Any other completion ends the run;
/// 3. empty read: enqueue the recovery work item;
/// 4. another report than multitouch (P3 fix of HF-B8): ask for the mode switch again and enqueue the
///    recovery work item ([`work_item_callback`]);
/// 5. `Information` above the buffer (P3 fix of HF-B29) or malformed length: recovery timer 3 s;
/// 6. no parked upper read: drop the packet;
/// 7. otherwise stamp the reporting clock, build the report (honouring the selective-reporting
///    switches), retrieve the upper output memory, copy 50 bytes, `Information = 50`, complete with the
///    copy status (SUCCESS). If retrieving or copying fails: `WdfDeviceSetFailed(AttemptRestart)` and,
///    unlike C, which leaks the request (HF-B9), complete it with that failure status (port policy P2).
pub(crate) fn complete_lower_read(
    io: &mut Filter<'_>,
    state: &DeviceState,
    read: LowerRead,
    memory: ReadMemory,
    ticket: ReadTicket,
    status: NtStatus,
    information: usize,
) -> LowerReadOutcome {
    let origin = io.with_reads(|reads| reads.finish(read, ticket));
    let outcome = match origin {
        LowerReadOrigin::Stale => LowerReadOutcome::Stale,
        LowerReadOrigin::Current => {
            let layout = state.input_layout();
            let first_byte = io.lower_read_buffer(memory).first().copied();
            let class =
                classify_lower_read(state.vendor_id(), &layout, status, information, first_byte);
            match state.on_read(class) {
                ReadAction::Drop => {}
                ReadAction::FailDevice => io.set_failed(FailedAction::NoRestart),
                ReadAction::Retry => io.enqueue_recovery_work_item(),
                ReadAction::ArmTimer => io.start_recovery_timer(TRANSPORT_RETRY_DUE_TIME),
                ReadAction::Deliver(length) => deliver_report(io, state, &layout, memory, length),
            }
            LowerReadOutcome::Handled(class)
        }
    };
    io.delete_lower_read(read);
    io.delete_lower_memory(memory);
    outcome
}

fn deliver_report(
    io: &mut Filter<'_>,
    state: &DeviceState,
    layout: &InputLayout,
    memory: ReadMemory,
    length: usize,
) {
    let Ok(upper) = io.retrieve_next_parked_read() else {
        return;
    };
    let scan_time = state.sample_scan_time(io.query_performance_counter());
    let (decoded, count, button_pressed) = {
        let buffer = io.lower_read_buffer(memory);
        decode_raw_packet(layout, length, buffer)
    };
    let selective = state.effective_reporting();
    let slice = decoded.get(..count).unwrap_or_default();
    let report: [u8; PTP_REPORT_LEN] = io.with_contacts(|contacts| {
        finish_tracked_report(slice, button_pressed, scan_time, selective, contacts)
    });
    let upper_memory = match io.retrieve_output_memory(upper) {
        Ok(m) => m,
        Err(status) => {
            io.set_failed(FailedAction::AttemptRestart);
            io.complete(upper, status); // P2 fix of HF-B9
            return;
        }
    };
    let status = io.copy_to_memory(upper_memory, &report);
    if !nt_success(status) {
        io.set_failed(FailedAction::AttemptRestart);
        io.complete(upper, status); // P2 fix of HF-B9
        return;
    }
    io.set_information(upper, PTP_REPORT_LEN);
    io.complete(upper, status);
}

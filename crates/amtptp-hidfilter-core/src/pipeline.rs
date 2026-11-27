//! The read pipeline (Input.c): one lower `IOCTL_HID_READ_REPORT` per trigger, and its completion,
//! which turns the raw packet into one 50-byte PTP report for one parked upper read.
//!
//! Triggers of a lower read (spec 6.5): an upper `READ_REPORT` while `DeviceConfigured`, and
//! [`issue_backlog_reads`] after the device became configured, from the recovery timer, and from the
//! recovery work item after a zero-length completion. A successful completion does not issue another
//! read; the pipeline is 1:1 with upper reads. The lower reads in flight are counted, so the backlog is
//! the parked upper reads that no read serves yet.

use ptp_hid::input::PTP_REPORT_LEN;

use crate::config::TRANSPORT_RETRY_DUE_TIME;
use crate::device::DeviceState;
use crate::input::{InputLayout, LowerReadClass, build_report, classify_lower_read};
use crate::io::{FailedAction, FilterIo};
use crate::status::nt_success;

/// `PtpFilterInputIssueTransportRequest` (Input.c:39-117): creates, formats and sends one lower read.
///
/// - `WdfRequestCreate` fails: `DeviceConfigured = FALSE`, recovery timer 3 s.
/// - lookaside memory fails: delete the request, `WdfDeviceSetFailed(AttemptRestart)`.
/// - formatting fails: delete memory and request, `WdfDeviceSetFailed(AttemptRestart)`.
/// - `WdfRequestSend` returns FALSE: `DeviceConfigured = FALSE`, timer 3 s, delete memory and request.
///
/// A sent read counts as in flight until its completion.
///
/// P2 (HF-B25): without an I/O target (detour not applied) C would format against a NULL handle and
/// bugcheck; the port issues nothing. That state is unreachable in practice.
pub fn issue_lower_read<P: FilterIo>(io: &mut P, state: &DeviceState) {
    if !state.io_target_set() {
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
    // Counted before the send: the completion can run before `send_lower_read` returns.
    state.read_sent();
    if !io.send_lower_read(read) {
        state.read_done();
        state.set_device_configured(false);
        io.start_recovery_timer(TRANSPORT_RETRY_DUE_TIME);
        io.delete_lower_memory(memory);
        io.delete_lower_read(read);
    }
}

/// While the device is configured, issues one lower read for every parked upper read that no read in
/// flight serves (`WdfIoQueueGetState` minus the in-flight count). Stops early if an issue fails (that
/// clears `DeviceConfigured` and arms the recovery timer). Does nothing for a device that is not
/// configured, e.g. one that left D0.
pub fn issue_backlog_reads<P: FilterIo>(io: &mut P, state: &DeviceState) {
    if !state.device_configured() {
        return;
    }
    let backlog = io.parked_read_count().saturating_sub(state.in_flight_reads());
    for _ in 0..backlog {
        issue_lower_read(io, state);
        if !state.device_configured() {
            break;
        }
    }
}

/// `PtpFilterWorkItemCallback` (Input.c:30-37), enqueued after a zero-length completion:
/// [`issue_backlog_reads`]. C issued one lower read without checking `DeviceConfigured`, so the work
/// item also sent reads to a stopped or unconfigured device (P3 fix).
pub fn work_item_callback<P: FilterIo>(io: &mut P, state: &DeviceState) {
    issue_backlog_reads(io, state);
}

/// `PtpFilterInputRequestCompletionCallback` (Input.c:119-253) for lower read `read` whose
/// `IoStatus.Information` is `information` (`WdfRequestGetInformation`, a `ULONG_PTR`). The completion
/// status is ignored (HF-B7).
///
/// `memory` is the request context's `RequestMemory`; the glue reads it **before** calling this (the
/// C code reads it after deleting the request, HF-B10). This function ends by deleting the request and
/// then the memory on every path, so the glue must not touch either afterwards.
///
/// The read is no longer in flight. Paths, in C order:
/// 1. vendor not Apple: `WdfDeviceSetFailed(NoRestart)`;
/// 2. length 0: enqueue the recovery work item;
/// 3. malformed length: recovery timer 3 s;
/// 4. no parked upper read: drop the packet;
/// 5. otherwise stamp `LastReportTime` (the QPC frequency), build the report (honouring the
///    selective-reporting switches), retrieve the upper output
///    memory, copy 50 bytes, `Information = 50`, complete with the copy status (SUCCESS). If retrieving
///    or copying fails: `WdfDeviceSetFailed(AttemptRestart)` and, unlike C, which leaks the request
///    (HF-B9), complete it with that failure status (port policy P2).
pub fn complete_lower_read<P: FilterIo>(
    io: &mut P,
    state: &DeviceState,
    read: P::LowerRead,
    memory: P::LowerMemory,
    information: usize,
) {
    state.read_done();
    let layout = state.input_layout();
    match classify_lower_read(state.vendor_id(), &layout, information) {
        LowerReadClass::UnsupportedVendor => io.set_failed(FailedAction::NoRestart),
        LowerReadClass::Empty => io.enqueue_recovery_work_item(),
        LowerReadClass::Malformed => io.start_recovery_timer(TRANSPORT_RETRY_DUE_TIME),
        LowerReadClass::Report { length } => deliver_report(io, state, &layout, memory, length),
    }
    io.delete_lower_read(read);
    io.delete_lower_memory(memory);
}

fn deliver_report<P: FilterIo>(
    io: &mut P,
    state: &DeviceState,
    layout: &InputLayout,
    memory: P::LowerMemory,
    length: usize,
) {
    let Ok(upper) = io.retrieve_next_parked_read() else {
        return;
    };
    let scan_time = state.scan_clock.sample(io.query_performance_counter());
    let report: [u8; PTP_REPORT_LEN] = build_report(
        layout,
        length,
        io.lower_read_buffer(memory),
        scan_time,
        state.selective_reporting(),
    );
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

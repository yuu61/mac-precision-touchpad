//! Reader completion → PTP input report: `AmtPtpEvtUsbInterruptPipeReadComplete` (Interrupt.c:89-250).
//!
//! The C routine, and how the glue reproduces it with this module:
//!
//! | C step | Rust |
//! |---|---|
//! | length gate (`N < header` or `(N - header) % fsize != 0`) → return, no request touched | [`process_packet`] returns [`PacketAction::Discard`] |
//! | `WdfIoQueueRetrieveNextRequest(InputQueue)` fails → return (packet lost) | glue, after `Deliver` |
//! | `WdfRequestRetrieveOutputMemory` fails → return **without completing** (B6) | [`complete_read_report_to`] with `Err(status)`: completes with that status (P2) |
//! | build the report; more than 5 finger blocks → `WdfRequestComplete(STATUS_DATA_ERROR)` | [`ReadReportResult::DataError`] |
//! | `WdfMemoryCopyFromBuffer` (< 50 bytes) fails → return **without completing** (B7) | [`complete_read_report_to`]: completed with the copy status, `STATUS_BUFFER_TOO_SMALL` (P2) |
//! | `SetInformation(50)`, `Complete(SUCCESS)` | [`complete_read_report_to`]: success, 50 |
//!
//! [`process_packet`] builds the report and samples the clock before the glue dequeues a request.
//! [`complete_read_report`] preserves the completion precedence: an output-memory retrieval failure
//! takes priority over `DATA_ERROR`, which takes priority over a short output buffer.
//!
//! Arithmetic mirrors the C exactly: coordinates are `(int)(signed short)` values, X/Y are clamped below at
//! 0 but not above (B19), Y is inverted, contact IDs are slot indices (B18), more than 5 contacts fail the
//! read (B8), Tip = `major*2 >= 200 || minor*2 >= 150`, Confidence = `minor*2 > 0`.

use bcm5974::input::{build_usb_report, usb_contact_count, wellspring_contact};
use ptp_hid::common::PTP_CONTACT_SLOTS;
use ptp_hid::input::PTP_REPORT_LEN;

use crate::config::T2Device;
use crate::output::OutputMemory;
use crate::state::{DeviceState, InputGates};
use crate::status::{Completion, NtStatus, STATUS_DATA_ERROR, nt_success};

pub use bcm5974::input::TRACKPAD_FINGER_LEN;

/// What the reader completion does with one report built from a valid packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadReportResult {
    /// More than [`PTP_CONTACT_SLOTS`] finger blocks: complete the request with `STATUS_DATA_ERROR`,
    /// `Information = 0`, buffer untouched (spec B8, preserved).
    DataError,
    /// The 50-byte input report 0x05 to copy into the request.
    Report([u8; PTP_REPORT_LEN]),
}

/// The outcome of [`process_packet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketAction {
    /// The length gate failed: drop the packet and do not touch the `InputQueue` (the pended `READ_REPORT`
    /// stays queued).
    Discard,
    /// Dequeue the next `READ_REPORT` (`WdfIoQueueRetrieveNextRequest`); if there is none, drop the packet.
    /// Otherwise complete it with [`complete_read_report`].
    Deliver(ReadReportResult),
}

/// Builds the report from a raw packet (`packet.len()` = `NumBytesTransferred`), with the given config
/// row, gates and `ScanTime`. Pure; see the module docs for the arithmetic.
#[must_use]
pub fn parse_packet(
    cfg: &T2Device,
    packet: &[u8],
    gates: InputGates,
    scan_time: u16,
) -> PacketAction {
    parse_packet_with_clock(cfg, packet, gates, || scan_time)
}

fn parse_packet_with_clock<F: FnOnce() -> u16>(
    cfg: &T2Device,
    packet: &[u8],
    gates: InputGates,
    scan_time: F,
) -> PacketAction {
    let format = cfg.profile.tp_type;
    let Some(raw_count) = usb_contact_count(format, packet.len()) else {
        return PacketAction::Discard;
    };
    if gates.report_touch && raw_count > PTP_CONTACT_SLOTS {
        return PacketAction::Deliver(ReadReportResult::DataError);
    }

    let mut report = build_usb_report(
        format,
        packet,
        gates.report_touch,
        gates.report_button,
        |finger, slot| wellspring_contact(&cfg.profile, finger, slot, Some(150)),
    );
    report.scan_time = scan_time();
    PacketAction::Deliver(ReadReportResult::Report(report.to_bytes()))
}

/// Parses a packet using the device's selected config, input gates and reporting clock.
pub fn process_packet(state: &DeviceState, packet: &[u8], counter: i64) -> PacketAction {
    parse_packet_with_clock(state.config(), packet, state.input_gates(), || {
        state.scan_clock.sample(counter)
    })
}

/// Completes a dequeued `READ_REPORT`.
///
/// `output` is the result of `WdfRequestRetrieveOutputMemory`: the driver passes its `WDFMEMORY` wrapper
/// (so the copy is the framework's `WdfMemoryCopyFromBuffer(mem, 0, report, 50)`), tests pass a byte slice.
///
/// - `Err(status)`: the C returns without completing the request (permanent, non-cancelable leak, spec
///   B6); here it is completed with `status` (P2).
/// - [`ReadReportResult::DataError`]: `STATUS_DATA_ERROR`, nothing written.
/// - [`ReadReportResult::Report`]: the copy. If it fails (output shorter than 50 bytes:
///   `STATUS_BUFFER_TOO_SMALL`), the C returns without completing the request (B7); here it is completed
///   with the copy status and nothing is written (P2). Otherwise the 50 bytes are at the start of the
///   buffer (the rest is untouched) and the request completes with the copy status (`STATUS_SUCCESS`),
///   `Information = 50`.
#[must_use]
pub fn complete_read_report_to<M: OutputMemory + ?Sized>(
    result: &ReadReportResult,
    output: Result<&mut M, NtStatus>,
) -> Completion {
    let memory = match output {
        Ok(memory) => memory,
        Err(status) => return Completion::status(status),
    };
    match result {
        ReadReportResult::DataError => Completion::status(STATUS_DATA_ERROR),
        ReadReportResult::Report(bytes) => {
            let status = memory.copy_from_buffer(bytes);
            if nt_success(status) {
                Completion {
                    status,
                    information: PTP_REPORT_LEN,
                }
            } else {
                Completion::status(status)
            }
        }
    }
}

/// [`complete_read_report_to`] on a byte slice (the [`OutputMemory`] model of `WdfMemoryCopyFromBuffer`:
/// `STATUS_BUFFER_TOO_SMALL` below 50 bytes).
#[must_use]
pub fn complete_read_report(
    result: &ReadReportResult,
    output: Result<&mut [u8], NtStatus>,
) -> Completion {
    complete_read_report_to(result, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FALLBACK_CONFIG, config_for_pid};
    use crate::status::{STATUS_BUFFER_TOO_SMALL, STATUS_INVALID_BUFFER_SIZE};

    /// An output memory whose copy fails with a framework status other than `BUFFER_TOO_SMALL`.
    struct FailingMemory(NtStatus);

    impl OutputMemory for FailingMemory {
        fn copy_from_buffer(&mut self, _src: &[u8]) -> NtStatus {
            self.0
        }
    }

    #[test]
    fn copy_status_is_the_frameworks() {
        let report = ReadReportResult::Report([5; 50]);
        let mut memory = FailingMemory(STATUS_INVALID_BUFFER_SIZE);
        assert_eq!(
            complete_read_report_to(&report, Ok(&mut memory)),
            Completion::status(STATUS_INVALID_BUFFER_SIZE)
        );
        // DATA_ERROR never copies.
        assert_eq!(
            complete_read_report_to(&ReadReportResult::DataError, Ok(&mut memory)),
            Completion::status(STATUS_DATA_ERROR)
        );
    }

    #[test]
    fn golden_a_header_only() {
        let mut packet = [0_u8; 46];
        packet[31] = 1;
        let PacketAction::Deliver(ReadReportResult::Report(r)) =
            parse_packet(&FALLBACK_CONFIG, &packet, InputGates::ON, 0)
        else {
            panic!("expected a report");
        };
        let mut expect = [0_u8; 50];
        expect[0] = 5;
        expect[49] = 1;
        assert_eq!(r, expect);
    }

    #[test]
    fn length_gate_and_data_error() {
        let cfg = config_for_pid(0x027a);
        assert_eq!(
            parse_packet(cfg, &[0; 45], InputGates::ON, 0),
            PacketAction::Discard
        );
        assert_eq!(
            parse_packet(cfg, &[0; 47], InputGates::ON, 0),
            PacketAction::Discard
        );
        assert_eq!(
            parse_packet(cfg, &[0; 226], InputGates::ON, 0),
            PacketAction::Deliver(ReadReportResult::DataError)
        );
        // The DATA_ERROR check is inside the touch gate.
        let gates = InputGates {
            report_touch: false,
            report_button: true,
        };
        assert!(matches!(
            parse_packet(cfg, &[0; 226], gates, 0),
            PacketAction::Deliver(ReadReportResult::Report(_))
        ));
    }

    #[test]
    fn completion_precedence() {
        let report = ReadReportResult::Report([5; 50]);
        assert_eq!(
            complete_read_report(&ReadReportResult::DataError, Err(STATUS_BUFFER_TOO_SMALL)),
            Completion::status(STATUS_BUFFER_TOO_SMALL)
        );
        let mut short = [0xa5_u8; 49];
        assert_eq!(
            complete_read_report(&ReadReportResult::DataError, Ok(&mut short)),
            Completion::status(STATUS_DATA_ERROR)
        );
        assert_eq!(
            complete_read_report(&report, Ok(&mut short)),
            Completion::status(STATUS_BUFFER_TOO_SMALL)
        );
        assert!(short.iter().all(|&b| b == 0xa5));
        let mut long = [0xa5_u8; 51];
        assert_eq!(
            complete_read_report(&report, Ok(&mut long)),
            Completion::success(50)
        );
        assert_eq!(long[50], 0xa5);
    }
}

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
//! [`process_packet`] is pure, so calling it before dequeuing the request is observably identical to the
//! C order. The precedence the C order implies is kept by [`complete_read_report`]: a failed output-memory
//! retrieval wins over `DATA_ERROR`, and `DATA_ERROR` wins over a too-small output buffer.
//!
//! Arithmetic mirrors the C exactly: coordinates are `(int)(signed short)` values, X/Y are clamped below at
//! 0 but not above (B19), Y is inverted, contact IDs are slot indices (B18), more than 5 contacts fail the
//! read (B8), Tip = `major*2 >= 200 || minor*2 >= 150`, Confidence = `minor*2 > 0`, and `ScanTime` is
//! [`scan_time_as_c`] (always 0 in the real driver, B1).

use bcm5974::input::wellspring_contact;
use ptp_hid::common::PTP_CONTACT_SLOTS;
use ptp_hid::input::{PTP_REPORT_LEN, PtpReport};

use crate::config::T2Device;
use crate::output::OutputMemory;
use crate::state::{DeviceState, InputGates};
use crate::status::{Completion, NtStatus, STATUS_DATA_ERROR, nt_success};

pub use bcm5974::input::TRACKPAD_FINGER_LEN;

/// `ScanTime` exactly as the C computes it (Interrupt.c:177-183):
///
/// ```c
/// KeQueryPerformanceCounter(&CurrentPerfCounter);   // stores the QPC *frequency*
/// PerfCounterDelta = (CurrentPerfCounter.QuadPart - LastReportTime.QuadPart) / 100;
/// if (PerfCounterDelta > 0xFF) PerfCounterDelta = 0xFF;
/// ScanTime = (USHORT)PerfCounterDelta;
/// ```
///
/// `perf_counter_out` is the value `KeQueryPerformanceCounter` writes to its out-parameter (the
/// frequency) and `last_report_time` is what `D0Entry` stored the same way, so in the real driver both are
/// the QPC frequency and the result is always 0 (spec B1, preserved). The clamp is signed only: a negative
/// delta passes through and the `(USHORT)` cast wraps it (-1 → 0xFFFF). The subtraction wraps on
/// overflow (two's complement, as MSVC does; P2 for the signed-overflow UB).
#[must_use]
pub const fn scan_time_as_c(perf_counter_out: i64, last_report_time: i64) -> u16 {
    // LONGLONG arithmetic; `/` truncates toward zero like C, and 100 is never 0 or -1.
    let mut delta = perf_counter_out.wrapping_sub(last_report_time) / 100;
    if delta > 0xFF {
        delta = 0xFF;
    }
    delta as u16 // (USHORT) truncation
}

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
    let n = packet.len();
    let format = cfg.profile.tp_type;
    let header = format.header() as usize;
    let fsize = format.finger_size() as usize;

    // Length gate. A zero fsize (no table row) would be a division by zero in C; treat it as malformed.
    let Some(payload) = n.checked_sub(header) else {
        return PacketAction::Discard;
    };
    match payload.checked_rem(fsize) {
        Some(0) => {}
        _ => return PacketAction::Discard,
    }
    let mut raw_n = payload.checked_div(fsize).unwrap_or(0);

    let mut report = PtpReport::EMPTY; // RtlZeroMemory + ReportID 0x05
    report.scan_time = scan_time;

    if gates.report_touch {
        // if (raw_n >= PTP_MAX_CONTACT_POINTS) raw_n = PTP_MAX_CONTACT_POINTS;
        raw_n = raw_n.min(PTP_CONTACT_SLOTS);
        // size_t arithmetic; raw_n <= 5, so this cannot wrap for any realistic fsize.
        if raw_n.wrapping_mul(fsize) < payload {
            return PacketAction::Deliver(ReadReportResult::DataError);
        }
        report.contact_count = raw_n as u8; // (UCHAR) raw_n, raw_n <= 5

        // f_base = TouchBuffer + header + tp_delta (int added to a pointer: sign-extended).
        let f_base = header.wrapping_add(format.delta() as usize);
        for (i, contact) in report.contacts.iter_mut().enumerate().take(raw_n) {
            let f = f_base.wrapping_add(i.wrapping_mul(fsize));
            let finger = packet.get(f..).unwrap_or_default();
            *contact = wellspring_contact(&cfg.profile, finger, i, Some(150));
        }
    }

    if gates.report_button {
        // TouchBuffer[tp_button] (int index, sign-extended); byte 31 < header <= N.
        let button = packet.get(format.button() as usize).copied().unwrap_or(0);
        if button != 0 {
            report.is_button_clicked = 1;
        }
    }

    PacketAction::Deliver(ReadReportResult::Report(report.to_bytes()))
}

/// [`parse_packet`] with the device's selected config row, its input gates and
/// `ScanTime = scan_time_as_c(perf_counter_out, LastReportTime)`.
///
/// `perf_counter_out` is the value `KeQueryPerformanceCounter(&x)` stores in `x` (the QPC frequency), as
/// the C computes it.
pub fn process_packet(state: &DeviceState, packet: &[u8], perf_counter_out: i64) -> PacketAction {
    parse_packet(
        state.config(),
        packet,
        state.input_gates(),
        scan_time_as_c(perf_counter_out, state.last_report_time()),
    )
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
    fn scan_time_matches_c() {
        assert_eq!(scan_time_as_c(10_000_000, 10_000_000), 0);
        assert_eq!(scan_time_as_c(10_000_000, 0), 0xFF);
        assert_eq!(scan_time_as_c(10_000_000, 10_000_100), 0xFFFF);
        assert_eq!(scan_time_as_c(10_000_000, 10_000_250), 0xFFFE);
        assert_eq!(scan_time_as_c(10_000_000, 9_999_901), 0);
        assert_eq!(scan_time_as_c(10_000_000, 9_999_900), 1);
        assert_eq!(scan_time_as_c(i64::MIN, 1), 0xFF); // wraps like MSVC
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

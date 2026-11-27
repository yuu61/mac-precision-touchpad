//! Interrupt-packet parsing and the continuous-reader completion (InputInterrupt.c:82-597).
//!
//! The pure building blocks ([`scan_time`], [`wellspring_contact`], [`mt2_contact`], [`build_report`],
//! [`classify_packet`]) reproduce the C integer semantics exactly (sign extension, truncation, the MSVC /
//! `-fwrapv` two's-complement results of the C UB). [`on_read_complete`] strings them together in the C
//! order, driving the `READ_REPORT` queue and the control pipe through the [`crate::io`] traits.

use ptp_hid::common::PTP_CONTACT_SLOTS;
use ptp_hid::input::{PTP_REPORT_LEN, PtpContact, PtpReport};

use crate::config::{TrackpadType, WellspringDevice};
use crate::io::{ControlPipe, HidRequest as _, ReadQueue};
use crate::state::DeviceState;
use crate::status::{NtStatus, STATUS_SUCCESS};
use crate::usb::emergency_reset;

pub use bcm5974::input::MT2_FINGER_LEN;

/// `ScanTime` exactly as the C code computes it (InputInterrupt.c:275-287 / :470-482, UM-B12 P3
/// PRESERVED):
///
/// ```c
/// PerfCounterDelta = (Current.QuadPart - DeviceContext->PerfCounter.QuadPart) / 100;
/// if (PerfCounterDelta > 0xFF) PerfCounterDelta = 0xFF;
/// PtpReport.ScanTime = (USHORT) PerfCounterDelta;
/// ```
///
/// The subtraction wraps (two's complement, P2 for the signed-overflow UB), the division truncates toward
/// zero, only the upper bound is clamped, and a negative delta is truncated to 16 bits (for example a
/// delta of -1 gives 0xFFFF).
#[must_use]
pub const fn scan_time(qpc_now: i64, perf_counter: i64) -> u16 {
    let delta = qpc_now.wrapping_sub(perf_counter) / 100;
    let delta = if delta > 0xFF { 0xFF } else { delta };
    // C: `(USHORT) PerfCounterDelta` (truncation of a LONGLONG).
    delta as u16
}

/// Decodes one TYPE2–4 contact using the UMDF tip-switch threshold.
#[must_use]
pub fn wellspring_contact(cfg: &WellspringDevice, finger: &[u8], slot: usize) -> PtpContact {
    bcm5974::input::wellspring_contact(&cfg.profile, finger, slot, None)
}

/// Decodes a Magic Trackpad 2 contact using the selected raw coordinate limits.
#[must_use]
pub const fn mt2_contact(cfg: &WellspringDevice, finger: &[u8; MT2_FINGER_LEN]) -> PtpContact {
    bcm5974::input::mt2_contact(finger, cfg.profile.x.min, cfg.profile.y.min)
}

/// What the read completion does with a packet before any request is touched (InputInterrupt.c:108-153).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketDisposition {
    /// `len < tp_header || (len - tp_header) % tp_fsize != 0`: run [`emergency_reset`] and return
    /// (UM-B11). A zero-length packet is malformed too.
    Malformed,
    /// `!IsWellspringModeOn`: drop silently; pending reads stay queued.
    ModeOff,
    /// TYPE1: "not yet supported", drop (UM-B24).
    Unsupported,
    /// Parse and complete the next pending read with a report of `fingers` raw finger blocks.
    Report {
        /// `(len - tp_header) / tp_fsize`, before the 5-contact limit.
        fingers: usize,
    },
}

/// The length validation, mode gate and type dispatch of `AmtPtpEvtUsbInterruptPipeReadComplete`.
#[must_use]
pub const fn classify_packet(
    cfg: &WellspringDevice,
    wellspring_mode_on: bool,
    len: usize,
) -> PacketDisposition {
    let header = cfg.profile.tp_type.header() as usize;
    let finger = cfg.profile.tp_type.finger_size() as usize;
    let fingers = match len.checked_sub(header) {
        Some(body) => match (body.checked_rem(finger), body.checked_div(finger)) {
            (Some(0), Some(fingers)) => fingers,
            _ => return PacketDisposition::Malformed,
        },
        None => return PacketDisposition::Malformed,
    };
    if !wellspring_mode_on {
        return PacketDisposition::ModeOff;
    }
    match cfg.profile.tp_type {
        TrackpadType::Type1 => PacketDisposition::Unsupported,
        TrackpadType::Type2 | TrackpadType::Type3 | TrackpadType::Type4 | TrackpadType::Type5 => {
            PacketDisposition::Report { fingers }
        }
    }
}

/// Builds input report 0x05 from a validated packet (`AmtPtpServiceTouchInputInterrupt[Type5]` minus the
/// request handling).
///
/// The report starts zero-initialized (UM-B13, P2 FIXED: C leaves unused slots, padding bits and, with
/// the surface off, `ContactCount` and all contacts as stack garbage). Then:
/// - `ScanTime = scan_time`;
/// - if `surface_on`: `ContactCount = min((len - h) / f, 5)` and contacts `0..n` decoded from the finger
///   block at `h + tp_delta + i * f` (TYPE2-4: [`wellspring_contact`], TYPE5: [`mt2_contact`]);
/// - if `button_on && packet[tp_button] != 0`: `IsButtonClicked = 1`.
#[must_use]
pub fn build_report(
    cfg: &WellspringDevice,
    packet: &[u8],
    surface_on: bool,
    button_on: bool,
    scan_time: u16,
) -> PtpReport {
    let mut report = PtpReport {
        scan_time,
        ..PtpReport::EMPTY
    };
    if surface_on {
        let header = cfg.profile.tp_type.header() as usize;
        let finger_len = cfg.profile.tp_type.finger_size() as usize;
        let fingers = packet
            .len()
            .checked_sub(header)
            .and_then(|body| body.checked_div(finger_len))
            .unwrap_or(0);
        let count = fingers.min(PTP_CONTACT_SLOTS);
        // count <= 5
        report.contact_count = count as u8;
        let base = header.saturating_add(cfg.profile.tp_type.delta() as usize);
        for (slot, contact) in report.contacts.iter_mut().enumerate().take(count) {
            let start = base.saturating_add(slot.saturating_mul(finger_len));
            // Only the bytes of the finger struct are sliced: for TYPE4 a full stride from `start` would
            // run 2 bytes past the end of the last finger block.
            let finger = packet.get(start..).unwrap_or(&[]);
            *contact = match cfg.profile.tp_type {
                TrackpadType::Type5 => match finger.first_chunk::<MT2_FINGER_LEN>() {
                    Some(block) => mt2_contact(cfg, block),
                    None => mt2_contact(cfg, &[0; MT2_FINGER_LEN]),
                },
                TrackpadType::Type1
                | TrackpadType::Type2
                | TrackpadType::Type3
                | TrackpadType::Type4 => wellspring_contact(cfg, finger, slot),
            };
        }
    }
    if button_on
        && packet
            .get(cfg.profile.tp_type.button() as usize)
            .copied()
            .unwrap_or(0)
            != 0
    {
        report.is_button_clicked = 1;
    }
    report
}

/// What [`on_read_complete`] did, for tracing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadCompleteOutcome {
    /// Malformed length: the emergency reset ran (UM-B11); `status` is its result.
    EmergencyReset {
        /// Status of the second mode switch (`AmtPtpEmergResetDevice`'s return value).
        status: NtStatus,
    },
    /// Wellspring mode is off; the packet was dropped.
    DroppedModeOff,
    /// TYPE1 device; the packet was dropped.
    DroppedUnsupported,
    /// `DeviceInfo` is not set (cannot happen after a successful `PrepareHardware`; the C code would
    /// dereference `NULL`). The packet was dropped.
    DroppedNoConfig,
    /// No `READ_REPORT` pending (`WdfIoQueueRetrieveNextRequest` failed with `status`); the frame is
    /// dropped, nothing is buffered.
    NoPendingRequest {
        /// The retrieve status.
        status: NtStatus,
    },
    /// The head request was completed with `STATUS_SUCCESS` and Information 50.
    Completed,
    /// UM-B6, P2 FIXED: retrieving the request's output memory or copying the 50 report bytes into it
    /// failed (C: `goto exit`, the request is never completed and hangs removal). The request was
    /// completed with that `status` and Information 0.
    CompletedWithError {
        /// The failing framework status.
        status: NtStatus,
    },
}

/// `AmtPtpEvtUsbInterruptPipeReadComplete` (InputInterrupt.c:82-211) plus
/// `AmtPtpServiceTouchInputInterrupt[Type5]`.
///
/// `packet` is the reader buffer cut to `NumBytesTransferred`. `query_performance_counter` is called at
/// most once, at the point where the C code calls `QueryPerformanceCounter` (TYPE2-4: right after a
/// request was retrieved; TYPE5: after its output memory was retrieved).
///
/// Order: [`classify_packet`] (malformed → [`emergency_reset`], synchronous control I/O inside the
/// completion, as in C); retrieve the next `READ_REPORT` (none → drop); build the report
/// ([`build_report`]); copy the 50 bytes into the request's output memory; complete with
/// `STATUS_SUCCESS`/50. A failure after the request was retrieved completes it with that status (UM-B6).
pub fn on_read_complete<Q, C, F>(
    state: &DeviceState,
    packet: &[u8],
    queue: &mut Q,
    control: &mut C,
    query_performance_counter: F,
) -> ReadCompleteOutcome
where
    Q: ReadQueue,
    C: ControlPipe,
    F: FnOnce() -> i64,
{
    let Some(cfg) = state.config() else {
        return ReadCompleteOutcome::DroppedNoConfig;
    };
    match classify_packet(cfg, state.is_wellspring_mode_on(), packet.len()) {
        PacketDisposition::Malformed => {
            let reset = emergency_reset(state, cfg, control);
            return ReadCompleteOutcome::EmergencyReset {
                status: reset.status,
            };
        }
        PacketDisposition::ModeOff => return ReadCompleteOutcome::DroppedModeOff,
        PacketDisposition::Unsupported => return ReadCompleteOutcome::DroppedUnsupported,
        // build_report derives the finger count from the packet length again.
        PacketDisposition::Report { .. } => {}
    }
    let mut request = match queue.retrieve_next_request() {
        Ok(request) => request,
        Err(status) => return ReadCompleteOutcome::NoPendingRequest { status },
    };
    let mut qpc = Some(query_performance_counter);
    let mut take_scan_time = || match qpc.take() {
        Some(query) => scan_time(query(), state.perf_counter()),
        None => 0,
    };
    let type5 = cfg.profile.tp_type == TrackpadType::Type5;
    // TYPE2-4 read the clock before retrieving the output memory, TYPE5 after.
    let early_scan_time = if type5 { None } else { Some(take_scan_time()) };
    if let Err(status) = request.output_memory_len() {
        queue.complete_request(request, status, 0);
        return ReadCompleteOutcome::CompletedWithError { status };
    }
    let scan = match early_scan_time {
        Some(scan) => scan,
        None => take_scan_time(),
    };
    let report = build_report(
        cfg,
        packet,
        state.is_surface_report_on(),
        state.is_button_report_on(),
        scan,
    );
    let bytes: [u8; PTP_REPORT_LEN] = report.to_bytes();
    match request.copy_to_output_memory(&bytes) {
        Ok(()) => {
            queue.complete_request(request, STATUS_SUCCESS, PTP_REPORT_LEN);
            ReadCompleteOutcome::Completed
        }
        Err(status) => {
            queue.complete_request(request, status, 0);
            ReadCompleteOutcome::CompletedWithError { status }
        }
    }
}

//! Interrupt-packet parsing and the continuous-reader completion (InputInterrupt.c:82-597).
//!
//! The pure building blocks ([`scan_time`], [`wellspring_contact`], [`mt2_contact`], [`build_report`],
//! [`classify_packet`]) reproduce the C integer semantics exactly (sign extension, truncation, the MSVC /
//! `-fwrapv` two's-complement results of the C UB). [`on_read_complete`] strings them together in the C
//! order, driving the `READ_REPORT` queue and the control pipe through the [`crate::io`] traits.

use ptp_hid::common::PTP_CONTACT_SLOTS;
use ptp_hid::input::{PTP_REPORT_LEN, PtpContact, PtpReport};

use crate::config::{Bcm5974Config, TrackpadType};
use crate::io::{ControlPipe, HidRequest as _, ReadQueue};
use crate::state::DeviceState;
use crate::status::{NtStatus, STATUS_SUCCESS};
use crate::usb::emergency_reset;

/// Length of a TYPE5 (Magic Trackpad 2) finger block.
pub const MT2_FINGER_LEN: usize = 9;

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

/// `AmtRawToInteger`: `(signed short) x` widened to `int`.
const fn raw_to_integer(x: u16) -> i32 {
    // Reinterpret the 16 bits as signed, then sign-extend.
    x as i16 as i32
}

/// Reads the little-endian `USHORT` at `offset` of `bytes`; a missing field reads as 0.
fn le_u16_at(bytes: &[u8], offset: usize) -> u16 {
    match bytes.get(offset..).and_then(|rest| rest.first_chunk::<2>()) {
        Some(pair) => u16::from_le_bytes(*pair),
        None => 0,
    }
}

/// Offsets of the used fields in `struct TRACKPAD_FINGER` (AppleDefinition.h:128-142).
const FINGER_ABS_X: usize = 2;
const FINGER_ABS_Y: usize = 4;
const FINGER_TOUCH_MAJOR: usize = 16;
const FINGER_TOUCH_MINOR: usize = 18;

/// TYPE2-4 contact decode (InputInterrupt.c:322-339) of the `TRACKPAD_FINGER` at the start of `finger`.
///
/// Only bytes 2..20 are read (`abs_x`, `abs_y`, `touch_major`, `touch_minor`; all le16 interpreted as i16).
///
/// - `X = ax - x.min > 0 ? (USHORT)(ax - x.min) : 0` (no upper clamp, UM-B18)
/// - `Y = y.max - ay > 0 ? (USHORT)(y.max - ay) : 0` (inverted, no upper clamp)
/// - `ContactID = slot` (not a tracking ID)
/// - `TipSwitch = (touch_major << 1) >= 200`, `Confidence = (touch_minor << 1) > 0`
#[must_use]
pub fn wellspring_contact(cfg: &Bcm5974Config, finger: &[u8], slot: usize) -> PtpContact {
    let abs_x = raw_to_integer(le_u16_at(finger, FINGER_ABS_X));
    let abs_y = raw_to_integer(le_u16_at(finger, FINGER_ABS_Y));
    let touch_major = raw_to_integer(le_u16_at(finger, FINGER_TOUCH_MAJOR));
    let touch_minor = raw_to_integer(le_u16_at(finger, FINGER_TOUCH_MINOR));
    let dx = abs_x.wrapping_sub(cfg.x.min);
    let dy = cfg.y.max.wrapping_sub(abs_y);
    PtpContact {
        // `<< 1` of a negative int is UB in C; MSVC gives two's complement, i.e. `* 2` (no overflow for
        // i16 inputs).
        confidence: touch_minor.wrapping_mul(2) > 0,
        tip_switch: touch_major.wrapping_mul(2) >= 200,
        // C: `ContactID = (UCHAR) i` with i < 5.
        contact_id: u32::from(slot as u8),
        // C: `(USHORT)(...)` truncation of a positive int (at most 39571).
        x: if dx > 0 { dx as u16 } else { 0 },
        y: if dy > 0 { dy as u16 } else { 0 },
    }
}

/// TYPE5 (Magic Trackpad 2) contact decode (InputInterrupt.c:500-524) of one 9-byte finger block.
///
/// ```c
/// USHORT tmp_x = (*((USHORT*)f)) & 0x1fff;
/// UINT   tmp_y = (INT)(*((UINT*)f));
/// x = (SHORT)(tmp_x << 3) >> 3;          // 13-bit sign extension
/// y = -(INT)(tmp_y << 6) >> 19;          // negate FIRST, then arithmetic shift
/// ```
///
/// The negation wraps (`INT_MIN` stays `INT_MIN`, as with MSVC / `-fwrapv`; P2), so the literal expression
/// is reproduced including the `F == -4096 && L == 0` case. Then `X = x - x.min > 0 ? x - x.min : 0`,
/// `Y = y - y.min > 0 ? y - y.min : 0` (no upper clamp), `ContactID = byte 8 & 0x0F` (device ID),
/// `TipSwitch = TouchMajor * 2 > 0`, `Confidence = TouchMinor * 2 < 345` (duplicated condition, UM-B17).
#[must_use]
pub fn mt2_contact(cfg: &Bcm5974Config, finger: &[u8; MT2_FINGER_LEN]) -> PtpContact {
    let [
        b0,
        b1,
        b2,
        b3,
        touch_major,
        touch_minor,
        _size,
        _pressure,
        id_orientation,
    ] = *finger;
    let tmp_x = u16::from_le_bytes([b0, b1]) & 0x1fff;
    let tmp_y = u32::from_le_bytes([b0, b1, b2, b3]);
    // `tmp_x << 3` is an int (at most 0xFFF8); `(SHORT)` keeps the low 16 bits as signed.
    let x = i32::from((i32::from(tmp_x) << 3) as u16 as i16) >> 3;
    // `tmp_y << 6` is a UINT shift (high bits drop), `(INT)` reinterprets, `-` wraps, `>>` is arithmetic.
    let y = ((tmp_y << 6) as i32).wrapping_neg() >> 19;
    let dx = x.wrapping_sub(cfg.x.min);
    let dy = y.wrapping_sub(cfg.y.min);
    let x = if dx > 0 { dx } else { 0 };
    let y = if dy > 0 { dy } else { 0 };
    let minor2 = i32::from(touch_minor) * 2;
    PtpContact {
        // UM-B17 (P3 PRESERVED): the C condition is `minor << 1 < 345 && minor << 1 < 345`; the duplicated
        // term (probably meant to test TouchMajor) has no effect, so it is written once.
        confidence: minor2 < 345,
        tip_switch: i32::from(touch_major) * 2 > 0,
        // `ContactIdentifier.Id`: the low nibble (MSVC allocates bit-fields LSB first).
        contact_id: u32::from(id_orientation & 0x0f),
        // C: `(USHORT) x` / `(USHORT) y` (truncation of a non-negative int).
        x: x as u16,
        y: y as u16,
    }
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
    cfg: &Bcm5974Config,
    wellspring_mode_on: bool,
    len: usize,
) -> PacketDisposition {
    let header = cfg.header_len();
    let finger = cfg.finger_len();
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
    match cfg.tp_type {
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
    cfg: &Bcm5974Config,
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
        let header = cfg.header_len();
        let finger_len = cfg.finger_len();
        let fingers = packet
            .len()
            .checked_sub(header)
            .and_then(|body| body.checked_div(finger_len))
            .unwrap_or(0);
        let count = fingers.min(PTP_CONTACT_SLOTS);
        // count <= 5
        report.contact_count = count as u8;
        let base = header.saturating_add(cfg.delta_len());
        for (slot, contact) in report.contacts.iter_mut().enumerate().take(count) {
            let start = base.saturating_add(slot.saturating_mul(finger_len));
            // Only the bytes of the finger struct are sliced: for TYPE4 a full stride from `start` would
            // run 2 bytes past the end of the last finger block.
            let finger = packet.get(start..).unwrap_or(&[]);
            *contact = match cfg.tp_type {
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
    if button_on && packet.get(cfg.button_offset()).copied().unwrap_or(0) != 0 {
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
    let type5 = cfg.tp_type == TrackpadType::Type5;
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

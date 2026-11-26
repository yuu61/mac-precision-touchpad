//! Raw Apple multitouch packet -> 50-byte PTP input report (Input.c:150-223), with the exact C integer
//! semantics.
//!
//! The lower read completes with a 1024-byte lookaside buffer and an `Information` value. The C code
//! never examines the completion status (HF-B7) and never checks the report ID (HF-B8): it classifies
//! the packet by length alone ([`classify_lower_read`]) and then parses up to five 9-byte finger
//! records ([`build_report`]).

use ptp_hid::common::{PTP_CONTACT_SLOTS, PTP_MAX_CONTACT_POINTS};
use ptp_hid::input::{PTP_REPORT_LEN, PtpContact, PtpReport};

use crate::device::is_apple_vendor;

/// `REPORT_BUFFER_SIZE` (Driver.h:33): size of each lower `IOCTL_HID_READ_REPORT` output buffer
/// (lookaside list entries, NonPagedPoolNx, tag [`crate::config::READ_BUFFER_POOL_TAG`]).
pub const REPORT_BUFFER_SIZE: usize = 1024;

/// Size of one raw `TRACKPAD_FINGER_TYPE5` record.
pub const FINGER_RECORD_LEN: usize = 9;

/// The device-context fields the completion routine reads (snapshot them with
/// [`crate::DeviceState::input_layout`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputLayout {
    /// `InputHeaderSize`.
    pub header_size: usize,
    /// `InputFingerSize` (the stride between finger records).
    pub finger_size: usize,
    /// `InputFingerDelta`.
    pub finger_delta: usize,
    /// `InputButtonDelta` (absolute offset of the button byte).
    pub button_delta: usize,
    /// `X.min`.
    pub x_min: i32,
    /// `Y.min`.
    pub y_min: i32,
}

/// `(size_t)(LONG)WdfRequestGetInformation(Request)` (Input.c:150, HF-B29): the low 32 bits of the
/// `ULONG_PTR`, sign-extended. Values with bit 31 set become huge lengths.
pub const fn response_length(information: usize) -> usize {
    // ULONG_PTR -> LONG truncates to 32 bits; LONG -> size_t sign-extends.
    information as u32 as i32 as isize as usize
}

/// What the completion routine does with a finished lower read, decided before any upper request is
/// touched (Input.c:156-173, in that order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowerReadClass {
    /// `VendorID` is not Apple: `WdfDeviceSetFailed(WdfDeviceFailedNoRestart)`. Checked first, so it
    /// also wins over a zero length (fixture `vid-1234-len0`).
    UnsupportedVendor,
    /// The response length is 0 (including any failed read): enqueue the recovery work item, which
    /// issues a new read at PASSIVE_LEVEL.
    Empty,
    /// `len < InputHeaderSize` or `(len - InputHeaderSize) % InputFingerSize != 0`: start the 3 s
    /// recovery timer. With `InputFingerSize == 0` C divides by zero; this port classifies it as
    /// malformed instead (HF-B18, port policy P2).
    Malformed,
    /// A parseable packet of `length` bytes (`hdr + 9n`, n >= 0, possibly a huge B29 length).
    Report {
        /// The response length as C computes it ([`response_length`]).
        length: usize,
    },
}

/// Classifies a completed lower read (Input.c:156-173).
pub fn classify_lower_read(
    vendor_id: u16,
    layout: &InputLayout,
    information: usize,
) -> LowerReadClass {
    if !is_apple_vendor(vendor_id) {
        return LowerReadClass::UnsupportedVendor;
    }
    let length = response_length(information);
    if length == 0 {
        return LowerReadClass::Empty;
    }
    // `len < hdr || (len - hdr) % fsz != 0`; `checked_rem` is None for fsz == 0 (HF-B18 guard).
    match length.checked_sub(layout.header_size) {
        Some(payload) if payload.checked_rem(layout.finger_size) == Some(0) => {
            LowerReadClass::Report { length }
        }
        _ => LowerReadClass::Malformed,
    }
}

/// `ContactCount` for a [`LowerReadClass::Report`] length: `(len - hdr) / fsz`, capped at 5 (the first
/// five records in packet order are used). Returns 0 for lengths [`classify_lower_read`] rejects.
pub fn contact_count(layout: &InputLayout, length: usize) -> u8 {
    let raw = length
        .checked_sub(layout.header_size)
        .and_then(|p| p.checked_div(layout.finger_size))
        .unwrap_or(0);
    // `if (raw_n >= 5) raw_n = 5; (UCHAR)raw_n`
    raw.min(PTP_CONTACT_SLOTS) as u8
}

/// `ScanTime` exactly as Input.c:187-189 computes it, given the value `KeQueryPerformanceCounter`
/// wrote through its *frequency* pointer (`now`) and the previous `LastReportTime` (`last`):
/// `delta = (now - last) / 100` (LONGLONG, truncating), then `delta >= 0xFF ? 0xFF : (USHORT)delta`.
///
/// Because `now` is always the QPC frequency, the driver emits 0 except for 0xFF on a report while
/// `LastReportTime` was never stamped (HF-B6, preserved). A negative delta truncates
/// (`-100 / 100 = -1` gives 0xFFFF). The subtraction wraps like MSVC instead of overflowing.
pub const fn scan_time(now: i64, last: i64) -> u16 {
    let delta = now.wrapping_sub(last) / 100;
    if delta >= 0xFF {
        0xFF
    } else {
        delta as u16 // (USHORT) truncation of a LONGLONG
    }
}

/// Parses one 9-byte `TRACKPAD_FINGER_TYPE5` record into a PTP contact (Input.c:202-217).
///
/// - X: the low 13 bits of the u16 at bytes 0-1, sign-extended (`(SHORT)(tmp_x << 3) >> 3`).
/// - Y: `-(INT)(u32 << 6) >> 19`, i.e. `-Y13` when the X bits are zero and `-Y13 - 1` otherwise
///   (HF-B2); negating `INT_MIN` wraps like MSVC.
/// - Both minus their `min`, clamped below at 0, never above (HF-B5), then truncated to `USHORT`.
/// - ContactID: low nibble of byte 8. TipSwitch: byte 4 != 0. Confidence: byte 5 <= 172 (the C code
///   tests TouchMinor twice, HF-B3).
pub const fn parse_finger(f: &[u8; FINGER_RECORD_LEN], x_min: i32, y_min: i32) -> PtpContact {
    let [
        b0,
        b1,
        b2,
        b3,
        touch_major,
        touch_minor,
        _size,
        _pressure,
        orientation_origin,
    ] = *f;
    let tmp_x = u16::from_le_bytes([b0, b1]) & 0x1FFF;
    let tmp_y = u32::from_le_bytes([b0, b1, b2, b3]);
    // tmp_x << 3 <= 0xFFF8; (SHORT) reinterprets, >> on a negative SHORT is arithmetic.
    let x = (((tmp_x << 3) as i16) >> 3) as i32;
    // (tmp_y << 6) drops the top 6 bits; (INT) reinterprets; unary minus binds before >>.
    let y = ((tmp_y << 6) as i32).wrapping_neg() >> 19;
    let x = clamp_offset(x, x_min);
    let y = clamp_offset(y, y_min);
    PtpContact {
        // ((signed short)TouchMinor << 1) < 345, tested twice.
        confidence: ((touch_minor as i16 as i32) << 1) < 345,
        // ((signed short)TouchMajor << 1) > 0.
        tip_switch: ((touch_major as i16 as i32) << 1) > 0,
        contact_id: (orientation_origin & 0x0F) as u32,
        x: x as u16, // (USHORT)x
        y: y as u16, // (USHORT)y
    }
}

/// `(v - min) > 0 ? (v - min) : 0`, with the INT subtraction wrapping like MSVC.
const fn clamp_offset(v: i32, min: i32) -> i32 {
    let d = v.wrapping_sub(min);
    if d > 0 { d } else { 0 }
}

/// Reads `buffer[base + offset]`, or 0 if that is outside `buffer` (port policy P2). With the 1024-byte
/// lookaside buffer every byte C reads is inside it.
fn byte_at(buffer: &[u8], base: Option<usize>, offset: usize) -> u8 {
    base.and_then(|b| b.checked_add(offset))
        .and_then(|i| buffer.get(i))
        .copied()
        .unwrap_or(0)
}

/// Builds the 50-byte `PTP_REPORT` for a [`LowerReadClass::Report`] packet (Input.c:182-223).
///
/// `buffer` is the lower read's output buffer. Finger `i` (i < [`contact_count`]) is read at
/// `header_size + finger_delta + i * finger_size`, and the button is bit 0 of
/// `buffer[button_delta]`. Bytes C would read outside `buffer` read as 0. Unused contact slots and the
/// contact padding bits are zero (C leaves them uninitialised, HF-B4, fixed).
pub fn build_report(
    layout: &InputLayout,
    length: usize,
    buffer: &[u8],
    scan_time: u16,
) -> [u8; PTP_REPORT_LEN] {
    let count = contact_count(layout, length);
    let mut report = PtpReport {
        scan_time,
        contact_count: count,
        ..PtpReport::EMPTY
    };
    let first = layout.header_size.checked_add(layout.finger_delta);
    for (i, slot) in report.contacts.iter_mut().enumerate() {
        if i >= usize::from(count) {
            break;
        }
        let base = first.and_then(|f| {
            i.checked_mul(layout.finger_size)
                .and_then(|o| f.checked_add(o))
        });
        let mut record = [0u8; FINGER_RECORD_LEN];
        for (j, byte) in record.iter_mut().enumerate() {
            *byte = byte_at(buffer, base, j);
        }
        *slot = parse_finger(&record, layout.x_min, layout.y_min);
    }
    if byte_at(buffer, Some(layout.button_delta), 0) & 1 != 0 {
        report.is_button_clicked = 1;
    }
    report.to_bytes()
}

const _: () = assert!(PTP_MAX_CONTACT_POINTS as usize == PTP_CONTACT_SLOTS);

#[cfg(test)]
mod tests {
    use super::*;

    const BT: InputLayout = InputLayout {
        header_size: 4,
        finger_size: 9,
        finger_delta: 0,
        button_delta: 1,
        x_min: -3678,
        y_min: -2479,
    };

    #[test]
    fn length_conversion_sign_extends() {
        assert_eq!(response_length(0x8000_0008), 0xFFFF_FFFF_8000_0008);
        assert_eq!(response_length(0x1_0000_000D), 13);
        assert_eq!(response_length(0xFFFF_FFFF_0000_000D), 13);
    }

    #[test]
    fn classification_order() {
        assert_eq!(
            classify_lower_read(0x1234, &BT, 0),
            LowerReadClass::UnsupportedVendor
        );
        assert_eq!(classify_lower_read(0x4C, &BT, 0), LowerReadClass::Empty);
        assert_eq!(classify_lower_read(0x4C, &BT, 3), LowerReadClass::Malformed);
        assert_eq!(
            classify_lower_read(0x4C, &BT, 4),
            LowerReadClass::Report { length: 4 }
        );
        assert_eq!(
            classify_lower_read(0x4C, &BT, 12),
            LowerReadClass::Malformed
        );
        let zero = InputLayout::default();
        assert_eq!(
            classify_lower_read(0x4C, &zero, 5),
            LowerReadClass::Malformed
        );
    }

    #[test]
    fn scan_time_formula() {
        for (d, t) in [
            (0i64, 0u16),
            (99, 0),
            (2550, 25),
            (25499, 254),
            (25500, 255),
            (10_000_000, 255),
            (-50, 0),
            (-100, 0xFFFF),
        ] {
            assert_eq!(scan_time(d, 0), t, "delta {d}");
        }
    }

    #[test]
    fn int_min_negation_wraps() {
        let c = parse_finger(&[0, 0, 0, 2, 10, 10, 0, 0, 6], -3678, -2479);
        assert_eq!((c.x, c.y), (3678, 0));
    }
}

//! Raw Apple multitouch packet -> 50-byte PTP input report (Input.c:150-223).
//!
//! The lower read completes with a 1024-byte lookaside buffer, a status and an `Information` value. The
//! C code examined neither the status (HF-B7) nor the report ID (HF-B8) and classified the packet by its
//! sign-extended length alone (HF-B29). [`classify_lower_read`] checks all three, in an order that keeps
//! the C outcome of every successful multitouch packet, and [`build_report`] then parses up to five
//! 9-byte finger records.

use ptp_hid::common::{PTP_CONTACT_SLOTS, PTP_MAX_CONTACT_POINTS};
use ptp_hid::feature::SelectiveReportBits;
use ptp_hid::input::{PTP_REPORT_LEN, PtpReport};

use crate::device::{HID_VID_APPLE_USB, is_apple_vendor};
use crate::status::{NtStatus, nt_success};

/// `REPORT_BUFFER_SIZE` (Driver.h:33): size of each lower `IOCTL_HID_READ_REPORT` output buffer
/// (lookaside list entries, `NonPagedPoolNx`, tag [`crate::config::READ_BUFFER_POOL_TAG`]).
pub const REPORT_BUFFER_SIZE: usize = 1024;

pub use bcm5974::input::{MT2_FINGER_LEN as FINGER_RECORD_LEN, mt2_contact as parse_finger};

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

/// The report ID of the Magic Trackpad 2's multitouch input report on the transport that `vendor_id`
/// identifies: 0x02 over USB (`TRACKPAD2_USB_REPORT_ID` in Linux hid-magicmouse) and 0x31 over
/// Bluetooth (`TRACKPAD2_BT_REPORT_ID`). Every edge packet of the C oracle starts with these bytes.
#[must_use]
pub const fn multitouch_report_id(vendor_id: u16) -> u8 {
    if vendor_id == HID_VID_APPLE_USB {
        0x02
    } else {
        0x31
    }
}

/// What the completion routine does with a finished lower read, decided before any upper request is
/// touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowerReadClass {
    /// `VendorID` is not Apple: `WdfDeviceSetFailed(WdfDeviceFailedNoRestart)`. Checked first, as in C,
    /// so it also wins over a zero length (fixture `vid-1234-len0`).
    UnsupportedVendor,
    /// The read failed (P3 fix of HF-B7). C ignored the status, so a failed read with `Information == 0`
    /// took the [`Self::Empty`] path, whose work item issued the next read at once: a lower driver that
    /// fails every read (a Bluetooth link going down, a cancelled read) made a zero-delay loop between
    /// the work item and the completion. The port still re-issues at once, but only for
    /// [`crate::config::MAX_IMMEDIATE_READ_RETRIES`] failures in a row; then it waits for the 3 s
    /// recovery timer, while the device is in D0.
    Failed {
        /// The completion status.
        status: NtStatus,
    },
    /// A successful read of 0 bytes: enqueue the recovery work item, which issues a new read at
    /// `PASSIVE_LEVEL` (as in C).
    Empty,
    /// `Information` is larger than the read's buffer ([`REPORT_BUFFER_SIZE`]), so it is not a packet
    /// length: start the 3 s recovery timer, as for [`Self::Malformed`] (P3 fix of HF-B29: C took the low
    /// 32 bits sign-extended, which made some of these values small valid lengths and others huge ones).
    Oversize,
    /// The first byte is not the transport's multitouch report ID ([`multitouch_report_id`]): another
    /// report of the trackpad, e.g. a mouse-mode report after the trackpad restarted, or its battery
    /// status (P3 fix of HF-B8: C parsed such a packet as finger data when its length happened to fit,
    /// and otherwise stalled input for 3 s as a malformed packet). The packet is dropped; the recovery
    /// work item sends the mode switch again and issues the next read.
    OtherReport {
        /// The packet's first byte (0 for an empty buffer).
        report_id: u8,
    },
    /// `len < InputHeaderSize` or `(len - InputHeaderSize) % InputFingerSize != 0`: start the 3 s
    /// recovery timer. With `InputFingerSize == 0` C divides by zero; this port classifies it as
    /// malformed instead (HF-B18, port policy P2).
    Malformed,
    /// A parseable multitouch packet of `length` bytes (`hdr + 9n`, n >= 0, at most
    /// [`REPORT_BUFFER_SIZE`]).
    Report {
        /// `Information`: the packet length.
        length: usize,
    },
}

/// Classifies a completed lower read. The checks, in order: vendor, completion status, length 0,
/// length above the buffer, report ID (`first_byte` is the packet's byte 0, `None` for an empty
/// buffer), packet shape. A successful multitouch packet gets the C classification (Input.c:156-173).
#[must_use]
pub fn classify_lower_read(
    vendor_id: u16,
    layout: &InputLayout,
    status: NtStatus,
    information: usize,
    first_byte: Option<u8>,
) -> LowerReadClass {
    if !is_apple_vendor(vendor_id) {
        return LowerReadClass::UnsupportedVendor;
    }
    if !nt_success(status) {
        return LowerReadClass::Failed { status };
    }
    let length = information;
    if length == 0 {
        return LowerReadClass::Empty;
    }
    if length > REPORT_BUFFER_SIZE {
        return LowerReadClass::Oversize;
    }
    let report_id = first_byte.unwrap_or(0);
    if first_byte != Some(multitouch_report_id(vendor_id)) {
        return LowerReadClass::OtherReport { report_id };
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
#[must_use]
pub fn contact_count(layout: &InputLayout, length: usize) -> u8 {
    let raw = length
        .checked_sub(layout.header_size)
        .and_then(|p| p.checked_div(layout.finger_size))
        .unwrap_or(0);
    // `if (raw_n >= 5) raw_n = 5; (UCHAR)raw_n`
    raw.min(PTP_CONTACT_SLOTS) as u8
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
#[must_use]
pub fn build_report(
    layout: &InputLayout,
    length: usize,
    buffer: &[u8],
    scan_time: u16,
    selective: SelectiveReportBits,
) -> [u8; PTP_REPORT_LEN] {
    // Selective reporting (P3 fix of HF-B20): no contacts with the surface switch off, no button with
    // the button switch off.
    let count = if selective.surface_report {
        contact_count(layout, length)
    } else {
        0
    };
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
            let o = i.checked_mul(layout.finger_size)?;
            f.checked_add(o)
        });
        let mut record = [0_u8; FINGER_RECORD_LEN];
        for (j, byte) in record.iter_mut().enumerate() {
            *byte = byte_at(buffer, base, j);
        }
        *slot = parse_finger(&record, layout.x_min, layout.y_min);
    }
    if selective.button_report && byte_at(buffer, Some(layout.button_delta), 0) & 1 != 0 {
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

    const OK: NtStatus = 0;
    const CANCELLED: NtStatus = 0xC000_0120_u32 as NtStatus;
    const BT_ID: Option<u8> = Some(0x31);

    fn class(vid: u16, layout: &InputLayout, status: NtStatus, info: usize, first: Option<u8>) -> LowerReadClass {
        classify_lower_read(vid, layout, status, info, first)
    }

    #[test]
    fn classification_order() {
        assert_eq!(
            class(0x1234, &BT, CANCELLED, 0, None),
            LowerReadClass::UnsupportedVendor
        );
        assert_eq!(
            class(0x4C, &BT, CANCELLED, 0, None),
            LowerReadClass::Failed { status: CANCELLED }
        );
        assert_eq!(class(0x4C, &BT, OK, 0, None), LowerReadClass::Empty);
        assert_eq!(class(0x4C, &BT, OK, 1025, BT_ID), LowerReadClass::Oversize);
        // C took the low 32 bits, sign-extended: a 13-byte packet and a huge length.
        assert_eq!(class(0x4C, &BT, OK, 0x1_0000_000D, BT_ID), LowerReadClass::Oversize);
        assert_eq!(class(0x4C, &BT, OK, 0x8000_0008, BT_ID), LowerReadClass::Oversize);
        assert_eq!(
            class(0x4C, &BT, OK, 3, Some(0x90)),
            LowerReadClass::OtherReport { report_id: 0x90 }
        );
        // The Bluetooth ID on the USB transport.
        assert_eq!(
            class(0x5AC, &BT, OK, 4, BT_ID),
            LowerReadClass::OtherReport { report_id: 0x31 }
        );
        assert_eq!(
            class(0x4C, &BT, OK, 4, None),
            LowerReadClass::OtherReport { report_id: 0 }
        );
        assert_eq!(class(0x4C, &BT, OK, 3, BT_ID), LowerReadClass::Malformed);
        assert_eq!(
            class(0x4C, &BT, OK, 4, BT_ID),
            LowerReadClass::Report { length: 4 }
        );
        assert_eq!(class(0x4C, &BT, OK, 12, BT_ID), LowerReadClass::Malformed);
        let zero = InputLayout::default();
        assert_eq!(class(0x4C, &zero, OK, 5, BT_ID), LowerReadClass::Malformed);
    }

    #[test]
    fn report_ids_per_transport() {
        assert_eq!(multitouch_report_id(0x05AC), 0x02);
        assert_eq!(multitouch_report_id(0x004C), 0x31);
    }

    #[test]
    fn minimum_signed_y_maps_to_top_of_logical_range() {
        let c = parse_finger(&[0, 0, 0, 2, 10, 10, 0, 0, 6], -3678, -2479);
        assert_eq!((c.x, c.y), (3678, 5065));
    }
}

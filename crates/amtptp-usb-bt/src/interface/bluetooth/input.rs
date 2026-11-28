//! Lower-read classification and conversion of decoded input to a 50-byte PTP report.

use amtptp_core::application::report_input;
use amtptp_core::domain::{Contact, ContactMatching, ContactTracker};
use amtptp_core::hid::feature::SelectiveReportBits;
use amtptp_core::hid::input::{PTP_REPORT_LEN, PtpReport};
use amtptp_core::hid::status::{NtStatus, nt_success};

use crate::application::bluetooth::LowerReadClass;
use crate::infrastructure::magic_trackpad::{
    InputLayout, decode_raw_packet, is_apple_vendor, multitouch_report_id,
};

/// `REPORT_BUFFER_SIZE` (Driver.h:33): size of each lower `IOCTL_HID_READ_REPORT` output buffer
/// (lookaside list entries, `NonPagedPoolNx`, tag [`crate::bluetooth::config::READ_BUFFER_POOL_TAG`]).
pub const REPORT_BUFFER_SIZE: usize = 1024;

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
    if layout.holds_whole_records(length) {
        LowerReadClass::Report { length }
    } else {
        LowerReadClass::Malformed
    }
}

/// Assembles a tracked 50-byte `PTP_REPORT` from decoded Magic Trackpad 2 observations using
/// `contacts` for persistent hardware-ID tracking and 1-frame `TipSwitch = 0` liftoff synthesis.
#[must_use]
pub fn finish_tracked_report(
    decoded: &[Contact],
    button_pressed: bool,
    scan_time: u16,
    selective: SelectiveReportBits,
    contacts: &mut ContactTracker,
) -> [u8; PTP_REPORT_LEN] {
    let frame = report_input(
        contacts,
        ContactMatching::HardwareId,
        decoded,
        u8::from(button_pressed),
        selective.into(),
    );
    PtpReport::from_frame(&frame, scan_time).to_bytes()
}

/// Builds a tracked 50-byte `PTP_REPORT` for a [`LowerReadClass::Report`] packet using `contacts`.
#[must_use]
pub fn build_report(
    layout: &InputLayout,
    length: usize,
    buffer: &[u8],
    scan_time: u16,
    selective: SelectiveReportBits,
    contacts: &mut ContactTracker,
) -> [u8; PTP_REPORT_LEN] {
    let (decoded, count, button_pressed) = decode_raw_packet(layout, length, buffer);
    let slice = decoded.get(..count).unwrap_or_default();
    finish_tracked_report(slice, button_pressed, scan_time, selective, contacts)
}

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

    fn class(
        vid: u16,
        layout: &InputLayout,
        status: NtStatus,
        info: usize,
        first: Option<u8>,
    ) -> LowerReadClass {
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
        assert_eq!(
            class(0x4C, &BT, OK, 0x1_0000_000D, BT_ID),
            LowerReadClass::Oversize
        );
        assert_eq!(
            class(0x4C, &BT, OK, 0x8000_0008, BT_ID),
            LowerReadClass::Oversize
        );
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
}

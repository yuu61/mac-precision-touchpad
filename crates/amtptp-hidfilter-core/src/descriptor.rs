//! The synthetic HID identity the filter reports upward: report descriptor, HID descriptor and device
//! attributes (Hid.c:6-140, `StaticHidRegistry.h`).
//!
//! The filter has no string table: `IOCTL_HID_GET_STRING` is forwarded to the PDO (spec 5.4).

use ptp_hid::common::DEVICE_VID;
use ptp_hid::descriptor::{Axes, REPORT_DESCRIPTOR_U32_ID_LEN, report_descriptor_u32_id};
use ptp_hid::hidport::{HID_DESCRIPTOR_LEN, HID_DESCRIPTOR_U32_ID, HidDeviceAttributes};

use crate::device::{DEVICE_VERSION, HID_PID_MAGIC_TRACKPAD_2};

/// The Magic Trackpad 2 axis values hard-coded in `MagicTrackpad2.h`: X 7612 / 1600, Y 1149 / 5065.
pub const MT2_AXES: Axes = Axes {
    x_logical_max: 7612,
    x_physical_max: 1600,
    y_physical_max: 1149,
    y_logical_max: 5065,
};

/// `PtpReportDescriptorMagicTrackpad2`: the touchpad TLC plus the Windows configuration TLC, 493 bytes
/// (spec Appendix A.1; no user-mode-app TLC).
pub static REPORT_DESCRIPTOR: [u8; REPORT_DESCRIPTOR_U32_ID_LEN] =
    report_descriptor_u32_id(MT2_AXES);

/// `PtpDefaultHidDescriptorMagicTrackpad2`: `09 21 00 01 00 01 22 ED 01`.
pub static HID_DESCRIPTOR: [u8; HID_DESCRIPTOR_LEN] = HID_DESCRIPTOR_U32_ID;

const _: () = assert!(REPORT_DESCRIPTOR_U32_ID_LEN == 493);
// C copies `DescriptorList[0].wReportLength` bytes of the report descriptor; it equals its length.
const _: () = assert!(
    u16::from_le_bytes([HID_DESCRIPTOR_U32_ID[7], HID_DESCRIPTOR_U32_ID[8]]) as usize
        == REPORT_DESCRIPTOR_U32_ID_LEN
);

/// The HID descriptor for `IOCTL_HID_GET_DEVICE_DESCRIPTOR`: only product ID 0x0265 has one
/// (Hid.c:28-36). `None` means `STATUS_NOT_SUPPORTED`. The vendor ID is not checked.
pub fn hid_descriptor_for(product_id: u16) -> Option<&'static [u8]> {
    if product_id == HID_PID_MAGIC_TRACKPAD_2 {
        Some(&HID_DESCRIPTOR)
    } else {
        None
    }
}

/// The report descriptor for `IOCTL_HID_GET_REPORT_DESCRIPTOR`: only product ID 0x0265 has one
/// (Hid.c:113-120). `None` means `STATUS_NOT_SUPPORTED`.
pub fn report_descriptor_for(product_id: u16) -> Option<&'static [u8]> {
    if product_id == HID_PID_MAGIC_TRACKPAD_2 {
        Some(&REPORT_DESCRIPTOR)
    } else {
        None
    }
}

/// `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (Hid.c:78-83): the fake vendor ID 0x8910, the **real** product
/// ID and version 0x0100, for any product ID. Write it with [`HidDeviceAttributes::write_to`], which
/// leaves `Reserved` untouched like the C code (HF-B22).
pub const fn device_attributes(product_id: u16) -> HidDeviceAttributes {
    HidDeviceAttributes {
        vendor_id: DEVICE_VID,
        product_id,
        version_number: DEVICE_VERSION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        assert_eq!(
            HID_DESCRIPTOR,
            [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xED, 0x01]
        );
        assert_eq!(REPORT_DESCRIPTOR[..6], [0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01]);
        assert_eq!(
            REPORT_DESCRIPTOR[487..],
            [0x95, 0x06, 0xB1, 0x03, 0xC0, 0xC0]
        );
        assert!(hid_descriptor_for(0x0266).is_none());
        assert!(report_descriptor_for(0).is_none());
        assert_eq!(
            device_attributes(0x0265).header_bytes(),
            [0x20, 0, 0, 0, 0x10, 0x89, 0x65, 0x02, 0x00, 0x01]
        );
    }
}

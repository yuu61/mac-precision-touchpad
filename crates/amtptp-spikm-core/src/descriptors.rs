//! The HID identity this driver reports upward: the 9-byte HID descriptor, the eight 519-byte report
//! descriptors (4 families x {Touch Pad, Touch Screen}) and `HID_DEVICE_ATTRIBUTES`.

use ptp_hid::common::DEVICE_VID;
use ptp_hid::descriptor::{
    Axes, REPORT_DESCRIPTOR_SPI_UMAPP_LEN, TlcUsage, report_descriptor_spi_umapp,
};
use ptp_hid::hidport::{
    HID_DESCRIPTOR_LEN, HID_DESCRIPTOR_SPI_UMAPP, HID_DEVICE_ATTRIBUTES_LEN, HidDeviceAttributes,
};

use crate::device::{DeviceConfig, Family, ReportType, family_for_product};
use crate::status::{NtStatus, STATUS_NOT_FOUND};

/// Length of every `SpiKm` report descriptor: 519 bytes (0x0207).
pub const REPORT_DESCRIPTOR_LEN: usize = REPORT_DESCRIPTOR_SPI_UMAPP_LEN;
const _: () = assert!(REPORT_DESCRIPTOR_LEN == 519);

/// Series 1 axis items (SpiTrackpadSeries1.h), with `y_logical_max` aligned to the Series 1 sensor Y span
/// (`6089 - (-128) = 6217`).
pub const AXES_F1: Axes = Axes {
    x_logical_max: 10666,
    x_physical_max: 1118,
    y_physical_max: 711,
    y_logical_max: 6217,
};
/// Series 2 axis items (SpiTrackpadSeries2.h).
pub const AXES_F2: Axes = Axes {
    x_logical_max: 10030,
    x_physical_max: 1064,
    y_physical_max: 773,
    y_logical_max: 6880,
};
/// Series 3, 13-inch axis items (SpiTrackpadSeries3.h, `SERIES3_13`).
pub const AXES_F3A: Axes = Axes {
    x_logical_max: 12992,
    x_physical_max: 1352,
    y_physical_max: 836,
    y_logical_max: 7855,
};
/// Series 3, 15-inch axis items (SpiTrackpadSeries3.h, `SERIES3_15`).
pub const AXES_F3B: Axes = Axes {
    x_logical_max: 15432,
    x_physical_max: 1584,
    y_physical_max: 992,
    y_logical_max: 9446,
};

/// `AmtPtpSpiFamily1ReportDescriptor`.
pub static REPORT_DESCRIPTOR_F1: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_spi_umapp(TlcUsage::TouchPad, AXES_F1);
/// `AmtPtpSpiFamily1TouchscreenReportDescriptor`.
pub static REPORT_DESCRIPTOR_F1_TOUCHSCREEN: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_spi_umapp(TlcUsage::TouchScreen, AXES_F1);
/// `AmtPtpSpiFamily2ReportDescriptor`.
pub static REPORT_DESCRIPTOR_F2: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_spi_umapp(TlcUsage::TouchPad, AXES_F2);
/// `AmtPtpSpiFamily2TouchscreenReportDescriptor`.
pub static REPORT_DESCRIPTOR_F2_TOUCHSCREEN: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_spi_umapp(TlcUsage::TouchScreen, AXES_F2);
/// `AmtPtpSpiFamily3aReportDescriptor`.
pub static REPORT_DESCRIPTOR_F3A: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_spi_umapp(TlcUsage::TouchPad, AXES_F3A);
/// `AmtPtpSpiFamily3aTouchscreenReportDescriptor`.
pub static REPORT_DESCRIPTOR_F3A_TOUCHSCREEN: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_spi_umapp(TlcUsage::TouchScreen, AXES_F3A);
/// `AmtPtpSpiFamily3bReportDescriptor`.
pub static REPORT_DESCRIPTOR_F3B: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_spi_umapp(TlcUsage::TouchPad, AXES_F3B);
/// `AmtPtpSpiFamily3bTouchscreenReportDescriptor`.
pub static REPORT_DESCRIPTOR_F3B_TOUCHSCREEN: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_spi_umapp(TlcUsage::TouchScreen, AXES_F3B);

/// `AmtPtpSpiFamily{1,2,3a,3b}DefaultHidDescriptor`: identical for all families,
/// `09 21 00 01 00 01 22 07 02`. `wReportLength` (519) is also the `CopiedSize` for the report
/// descriptor.
pub static HID_DESCRIPTOR: [u8; HID_DESCRIPTOR_LEN] = HID_DESCRIPTOR_SPI_UMAPP;

impl Family {
    /// The family's `Axes`.
    #[must_use]
    pub const fn axes(self) -> Axes {
        match self {
            Self::F1 => AXES_F1,
            Self::F2 => AXES_F2,
            Self::F3a => AXES_F3A,
            Self::F3b => AXES_F3B,
        }
    }

    /// The family's HID descriptor (the same 9 bytes for every family).
    #[must_use]
    pub fn hid_descriptor(self) -> &'static [u8; HID_DESCRIPTOR_LEN] {
        &HID_DESCRIPTOR
    }

    /// The family's report descriptor for `report_type`: `PrecisionTouchpad` (0) gives the Touch Pad
    /// variant, `Touchscreen` (1) the Touch Screen variant, any other value `None` (C leaves `Descriptor`
    /// NULL).
    #[must_use]
    pub fn report_descriptor(
        self,
        report_type: ReportType,
    ) -> Option<&'static [u8; REPORT_DESCRIPTOR_LEN]> {
        match (self, report_type) {
            (Self::F1, ReportType::PRECISION_TOUCHPAD) => Some(&REPORT_DESCRIPTOR_F1),
            (Self::F1, ReportType::TOUCHSCREEN) => Some(&REPORT_DESCRIPTOR_F1_TOUCHSCREEN),
            (Self::F2, ReportType::PRECISION_TOUCHPAD) => Some(&REPORT_DESCRIPTOR_F2),
            (Self::F2, ReportType::TOUCHSCREEN) => Some(&REPORT_DESCRIPTOR_F2_TOUCHSCREEN),
            (Self::F3a, ReportType::PRECISION_TOUCHPAD) => Some(&REPORT_DESCRIPTOR_F3A),
            (Self::F3a, ReportType::TOUCHSCREEN) => Some(&REPORT_DESCRIPTOR_F3A_TOUCHSCREEN),
            (Self::F3b, ReportType::PRECISION_TOUCHPAD) => Some(&REPORT_DESCRIPTOR_F3B),
            (Self::F3b, ReportType::TOUCHSCREEN) => Some(&REPORT_DESCRIPTOR_F3B_TOUCHSCREEN),
            _ => None,
        }
    }
}

/// `AmtPtpGetHidDescriptor` (Hid.c:107-304) after `WdfRequestRetrieveOutputMemory` succeeded: the bytes
/// to copy to offset 0 of the output memory, or `STATUS_NOT_FOUND` for a PID outside the switch.
///
/// The glue copies them with `WdfMemoryCopyFromBuffer` (a buffer shorter than 9 bytes fails there with
/// `STATUS_BUFFER_TOO_SMALL`, which is returned as is) and then sets Information to `bytes.len()` (9).
/// `ReportType` is not consulted.
///
/// # Errors
/// Returns `STATUS_NOT_FOUND` if the product has no HID descriptor.
pub fn hid_descriptor_for(config: &DeviceConfig) -> Result<&'static [u8], NtStatus> {
    match family_for_product(config.hid_product_id) {
        Some(family) => Ok(family.hid_descriptor()),
        None => Err(STATUS_NOT_FOUND),
    }
}

/// `AmtPtpGetReportDescriptor` (Hid.c:364-531) after `WdfRequestRetrieveOutputMemory` succeeded: the
/// 501 bytes to copy to offset 0 of the output memory, or `STATUS_NOT_FOUND` when the PID is outside the
/// switch or `ReportType` is neither 0 nor 1.
///
/// The glue copies them with `WdfMemoryCopyFromBuffer` (shorter than 501 bytes: `STATUS_BUFFER_TOO_SMALL`)
/// and sets Information to `bytes.len()` (501). C's `CopiedSize == 0` branch
/// (`STATUS_INVALID_DEVICE_STATE`) is unreachable because the length is the constant 501.
///
/// # Errors
/// Returns `STATUS_NOT_FOUND` if the product or report type has no descriptor.
pub fn report_descriptor_for(config: &DeviceConfig) -> Result<&'static [u8], NtStatus> {
    match family_for_product(config.hid_product_id) {
        Some(family) => match family.report_descriptor(config.report_type) {
            Some(desc) => Ok(desc),
            None => Err(STATUS_NOT_FOUND),
        },
        None => Err(STATUS_NOT_FOUND),
    }
}

/// Information for a successful `IOCTL_HID_GET_DEVICE_ATTRIBUTES`: `sizeof(HID_DEVICE_ATTRIBUTES)` = 32.
pub const DEVICE_ATTRIBUTES_INFORMATION: usize = HID_DEVICE_ATTRIBUTES_LEN;

/// `AmtPtpGetDeviceAttribs` (Hid.c:306-362): `VendorID` = 0x8910 (the deliberate fake `DEVICE_VID`),
/// `ProductID` = `HidProductID`, `VersionNumber` = `HidVersionNumber`.
///
/// The glue gets the buffer with `WdfRequestRetrieveOutputBuffer(Request, 32, ...)` (failure: complete
/// with that status), writes it with [`HidDeviceAttributes::write_to`] (including zeroed reserved fields) and sets Information to [`DEVICE_ATTRIBUTES_INFORMATION`].
/// Any PID is reported, even one outside the table.
#[must_use]
pub const fn device_attributes(config: &DeviceConfig) -> HidDeviceAttributes {
    HidDeviceAttributes {
        vendor_id: DEVICE_VID,
        product_id: config.hid_product_id,
        version_number: config.hid_version_number,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_descriptor_bytes() {
        assert_eq!(
            HID_DESCRIPTOR,
            [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0x07, 0x02]
        );
    }

    #[test]
    fn touchscreen_differs_only_at_byte_3() {
        for (a, b) in [
            (&REPORT_DESCRIPTOR_F1, &REPORT_DESCRIPTOR_F1_TOUCHSCREEN),
            (&REPORT_DESCRIPTOR_F2, &REPORT_DESCRIPTOR_F2_TOUCHSCREEN),
            (&REPORT_DESCRIPTOR_F3A, &REPORT_DESCRIPTOR_F3A_TOUCHSCREEN),
            (&REPORT_DESCRIPTOR_F3B, &REPORT_DESCRIPTOR_F3B_TOUCHSCREEN),
        ] {
            let diffs: usize = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count();
            assert_eq!(diffs, 1);
            assert_eq!((a[3], b[3]), (0x05, 0x04));
        }
    }
}

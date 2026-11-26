//! Routing of the internal IOCTLs that mshidkmdf sends down (`AmtPtpDeviceSpiKmEvtIoInternalDeviceControl`,
//! Queue.c:133-222).

use ptp_hid::ioctl::{
    IOCTL_HID_GET_DEVICE_ATTRIBUTES, IOCTL_HID_GET_DEVICE_DESCRIPTOR, IOCTL_HID_GET_FEATURE,
    IOCTL_HID_GET_REPORT_DESCRIPTOR, IOCTL_HID_GET_STRING, IOCTL_HID_READ_REPORT,
    IOCTL_HID_SET_FEATURE,
};

use crate::status::{NtStatus, STATUS_NOT_SUPPORTED};

/// What the dispatcher does with an `IRP_MJ_INTERNAL_DEVICE_CONTROL` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InternalIoctl {
    /// `IOCTL_HID_GET_DEVICE_DESCRIPTOR` (0x000B0003): [`crate::descriptors::hid_descriptor_for`], then
    /// complete.
    GetDeviceDescriptor,
    /// `IOCTL_HID_GET_REPORT_DESCRIPTOR` (0x000B0007): [`crate::descriptors::report_descriptor_for`],
    /// then complete.
    GetReportDescriptor,
    /// `IOCTL_HID_READ_REPORT` (0x000B000B): forward to the manual `HidQueue`; if that fails complete
    /// with its status, otherwise issue one lower read when
    /// [`crate::runtime::DeviceRuntime::should_issue_lower_read`]. Never completed by the dispatcher.
    ReadReport,
    /// `IOCTL_HID_GET_STRING` (0x000B0013): `WdfRequestFormatRequestUsingCurrentType` and send-and-forget
    /// to the lower target; if `WdfRequestSend` returns FALSE complete with `WdfRequestGetStatus`.
    GetString,
    /// `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (0x000B0027): [`crate::descriptors::device_attributes`], then
    /// complete.
    GetDeviceAttributes,
    /// `IOCTL_HID_GET_FEATURE` (0x000B0192): [`crate::feature::get_feature`], then complete (Information
    /// stays 0).
    GetFeature,
    /// `IOCTL_HID_SET_FEATURE` (0x000B0191): [`crate::runtime::DeviceRuntime::set_feature`], then
    /// complete (Information stays 0).
    SetFeature,
    /// Everything else, including `WRITE_REPORT`, `ACTIVATE/DEACTIVATE_DEVICE`, the idle notification and
    /// the `IOCTL_UMDF_HID_*` codes: complete with [`NOT_SUPPORTED_STATUS`]. Not forwarded (preserved
    /// known issue SPI-B16).
    NotSupported,
}

/// Completion status of [`InternalIoctl::NotSupported`] requests: `STATUS_NOT_SUPPORTED`.
pub const NOT_SUPPORTED_STATUS: NtStatus = STATUS_NOT_SUPPORTED;

/// Classifies an internal IOCTL code exactly like the C `switch (IoControlCode)`.
#[must_use]
pub const fn classify_internal_ioctl(io_control_code: u32) -> InternalIoctl {
    match io_control_code {
        IOCTL_HID_GET_DEVICE_DESCRIPTOR => InternalIoctl::GetDeviceDescriptor,
        IOCTL_HID_GET_DEVICE_ATTRIBUTES => InternalIoctl::GetDeviceAttributes,
        IOCTL_HID_GET_REPORT_DESCRIPTOR => InternalIoctl::GetReportDescriptor,
        IOCTL_HID_GET_STRING => InternalIoctl::GetString,
        IOCTL_HID_READ_REPORT => InternalIoctl::ReadReport,
        IOCTL_HID_GET_FEATURE => InternalIoctl::GetFeature,
        IOCTL_HID_SET_FEATURE => InternalIoctl::SetFeature,
        _ => InternalIoctl::NotSupported,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_section_4_table() {
        assert_eq!(
            classify_internal_ioctl(0x000B_0003),
            InternalIoctl::GetDeviceDescriptor
        );
        assert_eq!(
            classify_internal_ioctl(0x000B_0007),
            InternalIoctl::GetReportDescriptor
        );
        assert_eq!(
            classify_internal_ioctl(0x000B_000B),
            InternalIoctl::ReadReport
        );
        assert_eq!(
            classify_internal_ioctl(0x000B_0013),
            InternalIoctl::GetString
        );
        assert_eq!(
            classify_internal_ioctl(0x000B_0027),
            InternalIoctl::GetDeviceAttributes
        );
        assert_eq!(
            classify_internal_ioctl(0x000B_0192),
            InternalIoctl::GetFeature
        );
        assert_eq!(
            classify_internal_ioctl(0x000B_0191),
            InternalIoctl::SetFeature
        );
        for code in [
            0x000B_000F,
            0x000B_001F,
            0x000B_0023,
            0x000B_002B,
            0x000B_005B,
            0x000B_005F,
            0x000B_0053,
            0x000B_0057,
            0x000B_01A2,
            0x000B_0195,
            0x000B_01E2,
        ] {
            assert_eq!(classify_internal_ioctl(code), InternalIoctl::NotSupported);
        }
    }
}

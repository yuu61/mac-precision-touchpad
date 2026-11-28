//! Routing of the internal IOCTLs that mshidkmdf sends down (`AmtPtpDeviceSpiKmEvtIoInternalDeviceControl`,
//! Queue.c:133-222).

use crate::status::{NtStatus, STATUS_NOT_SUPPORTED};

/// Status for an unsupported request.
pub const NOT_SUPPORTED_STATUS: NtStatus = STATUS_NOT_SUPPORTED;
pub use ptp_hid::ioctl::InternalIoctl;
/// Classifies an internal HID request.
#[must_use]
pub const fn classify_internal_ioctl(code: u32) -> InternalIoctl {
    InternalIoctl::from_code(code)
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

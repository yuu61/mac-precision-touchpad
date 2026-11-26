//! HID minidriver data and the synchronous internal-IOCTL handlers of `AmtPtpDeviceUsbKmEvtIoDeviceControl`
//! (Queue.c:105-182, Hid.c).
//!
//! The filter answers the HID minidriver IOCTLs that `mshidkmdf` forwards as
//! `IRP_MJ_INTERNAL_DEVICE_CONTROL`. Every other internal IOCTL is completed with `STATUS_NOT_SUPPORTED`
//! and never forwarded. [`route_internal_ioctl`] is the `switch`; the handlers below take the buffer the
//! glue retrieved from WDF and return the [`Completion`].
//!
//! Output buffers: for the `METHOD_NEITHER` minidriver IOCTLs KMDF exposes `Irp->UserBuffer` as the output
//! buffer. The glue calls `WdfRequestRetrieveOutputMemory` (descriptors; the copy is then
//! `WdfMemoryCopyFromBuffer` through [`crate::output::OutputMemory`]) or
//! `WdfRequestRetrieveOutputBuffer(Request, 32, ..)` (attributes) and passes `Ok(buffer)` or
//! `Err(status)`; an `Err` is completed as is (the C returns the retrieval status).

use ptp_hid::descriptor::{Axes, REPORT_DESCRIPTOR_U32_ID_LEN, report_descriptor_u32_id};
use ptp_hid::hidport::{HID_DESCRIPTOR_LEN, HID_DESCRIPTOR_U32_ID, HidDeviceAttributes};
use ptp_hid::ioctl::{
    IOCTL_HID_GET_DEVICE_ATTRIBUTES, IOCTL_HID_GET_DEVICE_DESCRIPTOR, IOCTL_HID_GET_FEATURE,
    IOCTL_HID_GET_REPORT_DESCRIPTOR, IOCTL_HID_READ_REPORT, IOCTL_HID_SET_FEATURE,
};

pub use ptp_hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;

use crate::output::OutputMemory;
use crate::state::UsbDeviceIds;
use crate::status::{
    Completion, NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_NOT_SUPPORTED, nt_success,
};

/// The axis values of `AAPL_WELLSPRING_T2_PTP_TLC` (WellspringT2.h:28-37): X logical max 20000, X physical
/// max 1300, Y physical max 850, Y logical max 12000. (The comments at WellspringT2.h:74/77 claim 1045/750;
/// the bytes say 1300/850, and the bytes are what is reported.)
pub const T2_AXES: Axes = Axes {
    x_logical_max: 20000,
    x_physical_max: 1300,
    y_physical_max: 850,
    y_logical_max: 12000,
};

/// Length of [`REPORT_DESCRIPTOR`]: 493.
pub const REPORT_DESCRIPTOR_LEN: usize = REPORT_DESCRIPTOR_U32_ID_LEN;

/// `AmtPtpT2ReportDescriptor` (Hid.c:7-10): `{ AAPL_WELLSPRING_T2_PTP_TLC,
/// AAPL_PTP_WINDOWS_CONFIGURATION_TLC }`, 493 bytes, identical for every PID.
pub static REPORT_DESCRIPTOR: [u8; REPORT_DESCRIPTOR_LEN] = report_descriptor_u32_id(T2_AXES);

/// `AmtPtpT2DefaultHidDescriptor` (Hid.c:12-22): `09 21 00 01 00 01 22 ed 01`.
pub const HID_DESCRIPTOR: [u8; HID_DESCRIPTOR_LEN] = HID_DESCRIPTOR_U32_ID;

/// `DEVICE_VERSION` (include/Hid.h:14): the HID `VersionNumber`, a constant (not `bcdDevice`).
pub const DEVICE_VERSION: u16 = 0x0001;

/// The C dispatcher's `switch (IoControlCode)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InternalIoctl {
    /// `IOCTL_HID_GET_DEVICE_DESCRIPTOR` → [`get_hid_descriptor`].
    GetDeviceDescriptor,
    /// `IOCTL_HID_GET_DEVICE_ATTRIBUTES` → [`get_device_attributes`].
    GetDeviceAttributes,
    /// `IOCTL_HID_GET_REPORT_DESCRIPTOR` → [`get_report_descriptor`].
    GetReportDescriptor,
    /// `IOCTL_HID_READ_REPORT`: `WdfRequestForwardToIoQueue(Request, InputQueue)`. On success the request
    /// is pending (not completed here); on failure it is completed with the forward status and
    /// `Information = 0`. No buffer validation at enqueue time (spec B25, preserved).
    ReadReport,
    /// `IOCTL_HID_GET_FEATURE` → [`crate::feature::get_feature`].
    GetFeature,
    /// `IOCTL_HID_SET_FEATURE` → [`crate::feature::set_feature`].
    SetFeature,
    /// Everything else (`GET_STRING`, `WRITE_REPORT`, ACTIVATE/DEACTIVATE, idle notification, the UMDF
    /// codes, `GET_INPUT_REPORT`, any `IOCTL_INTERNAL_USB_*`, unknown codes): complete with
    /// [`not_supported`]. Never forwarded.
    NotSupported,
}

/// Routes an internal IOCTL code like the C dispatcher.
#[must_use]
pub const fn route_internal_ioctl(io_control_code: u32) -> InternalIoctl {
    match io_control_code {
        IOCTL_HID_GET_DEVICE_DESCRIPTOR => InternalIoctl::GetDeviceDescriptor,
        IOCTL_HID_GET_DEVICE_ATTRIBUTES => InternalIoctl::GetDeviceAttributes,
        IOCTL_HID_GET_REPORT_DESCRIPTOR => InternalIoctl::GetReportDescriptor,
        IOCTL_HID_READ_REPORT => InternalIoctl::ReadReport,
        IOCTL_HID_GET_FEATURE => InternalIoctl::GetFeature,
        IOCTL_HID_SET_FEATURE => InternalIoctl::SetFeature,
        _ => InternalIoctl::NotSupported,
    }
}

/// The completion of every unsupported internal IOCTL: `STATUS_NOT_SUPPORTED`, `Information = 0`.
#[must_use]
pub const fn not_supported() -> Completion {
    Completion::status(STATUS_NOT_SUPPORTED)
}

/// The completion of a `READ_REPORT` whose `WdfRequestForwardToIoQueue` failed with `status`.
#[must_use]
pub const fn read_report_forward_failed(status: NtStatus) -> Completion {
    Completion::status(status)
}

/// `WdfMemoryCopyFromBuffer(mem, 0, src, src.len())`, then `WdfRequestSetInformation(src.len())` on
/// success. The completion status is the copy status (the C returns it as is); a failed retrieval
/// (`Err`) or a failed copy completes with that status and `Information = 0`.
fn copy_completion<M: OutputMemory + ?Sized>(
    output: Result<&mut M, NtStatus>,
    src: &[u8],
) -> Completion {
    match output {
        Ok(memory) => {
            let status = memory.copy_from_buffer(src);
            if nt_success(status) {
                Completion {
                    status,
                    information: src.len(),
                }
            } else {
                Completion::status(status)
            }
        }
        Err(status) => Completion::status(status),
    }
}

/// `AmtPtpGetHidDescriptor` (Hid.c:26-119): copies the 9-byte [`HID_DESCRIPTOR`] (the per-PID `switch` has
/// identical branches) and completes with `Information = 9`; `STATUS_BUFFER_TOO_SMALL` if the output is
/// shorter than 9 bytes.
///
/// `output` is the `WdfRequestRetrieveOutputMemory` result. The driver passes its `WDFMEMORY` wrapper (the
/// copy is then the framework's `WdfMemoryCopyFromBuffer`); tests pass a byte slice.
#[must_use]
pub fn get_hid_descriptor_to<M: OutputMemory + ?Sized>(
    output: Result<&mut M, NtStatus>,
) -> Completion {
    copy_completion(output, &HID_DESCRIPTOR)
}

/// [`get_hid_descriptor_to`] on a byte slice (the [`OutputMemory`] model of `WdfMemoryCopyFromBuffer`).
#[must_use]
pub fn get_hid_descriptor(output: Result<&mut [u8], NtStatus>) -> Completion {
    get_hid_descriptor_to(output)
}

/// `AmtPtpGetReportDescriptor` (Hid.c:169-285): copies the 493-byte [`REPORT_DESCRIPTOR`] and completes
/// with `Information = 493`; `STATUS_BUFFER_TOO_SMALL` if the output is shorter. `output` as in
/// [`get_hid_descriptor_to`].
#[must_use]
pub fn get_report_descriptor_to<M: OutputMemory + ?Sized>(
    output: Result<&mut M, NtStatus>,
) -> Completion {
    copy_completion(output, &REPORT_DESCRIPTOR)
}

/// [`get_report_descriptor_to`] on a byte slice.
#[must_use]
pub fn get_report_descriptor(output: Result<&mut [u8], NtStatus>) -> Completion {
    get_report_descriptor_to(output)
}

/// The `HID_DEVICE_ATTRIBUTES` the driver reports: the cached USB `idVendor` (0x05AC on real hardware,
/// not the 0x8910 the other drivers use), the real `idProduct`, and `VersionNumber` [`DEVICE_VERSION`].
#[must_use]
pub const fn device_attributes(ids: UsbDeviceIds) -> HidDeviceAttributes {
    HidDeviceAttributes {
        vendor_id: ids.vendor_id,
        product_id: ids.product_id,
        version_number: DEVICE_VERSION,
    }
}

/// `AmtPtpGetDeviceAttribs` (Hid.c:121-167).
///
/// `output` is the result of `WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_DEVICE_ATTRIBUTES), ..)`
/// (which already fails with `STATUS_BUFFER_TOO_SMALL` below 32 bytes; a shorter `Ok` buffer gives the
/// same status here). Writes `Size`, `VendorID`, `ProductID` and `VersionNumber` only: `Reserved` (bytes
/// 10..32) keeps the caller's bytes, as in C (spec B17, preserved). Completes with `Information = 32`.
///
/// Before the first `PrepareHardware` the cached descriptor is all zero, so `VendorID` and `ProductID` are 0.
#[must_use]
pub fn get_device_attributes(output: Result<&mut [u8], NtStatus>, ids: UsbDeviceIds) -> Completion {
    match output {
        Ok(buf) => match device_attributes(ids).write_to(buf) {
            Ok(()) => Completion::success(HID_DEVICE_ATTRIBUTES_LEN),
            Err(ptp_hid::BufferTooSmall) => Completion::status(STATUS_BUFFER_TOO_SMALL),
        },
        Err(status) => Completion::status(status),
    }
}

const _: () = assert!(REPORT_DESCRIPTOR_LEN == 493);
const _: () = assert!(HID_DEVICE_ATTRIBUTES_LEN == 32);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{
        STATUS_INVALID_BUFFER_SIZE, STATUS_INVALID_DEVICE_REQUEST, STATUS_SUCCESS,
    };

    struct FailingMemory(NtStatus);

    impl OutputMemory for FailingMemory {
        fn copy_from_buffer(&mut self, _src: &[u8]) -> NtStatus {
            self.0
        }
    }

    #[test]
    fn copy_status_is_the_frameworks() {
        let mut memory = FailingMemory(STATUS_INVALID_BUFFER_SIZE);
        assert_eq!(
            get_report_descriptor_to(Ok(&mut memory)),
            Completion::status(STATUS_INVALID_BUFFER_SIZE)
        );
        let mut ok = FailingMemory(STATUS_SUCCESS);
        assert_eq!(get_hid_descriptor_to(Ok(&mut ok)), Completion::success(9));
    }

    #[test]
    fn hid_descriptor_bytes() {
        assert_eq!(
            HID_DESCRIPTOR,
            [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xed, 0x01]
        );
        let mut out = [0xcc_u8; 10];
        assert_eq!(get_hid_descriptor(Ok(&mut out)), Completion::success(9));
        assert_eq!(out[..9], HID_DESCRIPTOR);
        assert_eq!(out[9], 0xcc);
        let mut short = [0xcc_u8; 8];
        assert_eq!(
            get_hid_descriptor(Ok(&mut short)),
            Completion::status(STATUS_BUFFER_TOO_SMALL)
        );
        assert_eq!(short, [0xcc; 8]);
        assert_eq!(
            get_hid_descriptor(Err(STATUS_INVALID_DEVICE_REQUEST)),
            Completion::status(STATUS_INVALID_DEVICE_REQUEST)
        );
    }

    #[test]
    fn routing() {
        assert_eq!(
            route_internal_ioctl(0x000b_0003),
            InternalIoctl::GetDeviceDescriptor
        );
        assert_eq!(
            route_internal_ioctl(0x000b_0027),
            InternalIoctl::GetDeviceAttributes
        );
        assert_eq!(
            route_internal_ioctl(0x000b_0007),
            InternalIoctl::GetReportDescriptor
        );
        assert_eq!(route_internal_ioctl(0x000b_000b), InternalIoctl::ReadReport);
        assert_eq!(route_internal_ioctl(0x000b_0192), InternalIoctl::GetFeature);
        assert_eq!(route_internal_ioctl(0x000b_0191), InternalIoctl::SetFeature);
        for code in [
            0x000b_0013,
            0x000b_000f,
            0x000b_005b,
            0x000b_005f,
            0x000b_01a2,
            0x0022_0003,
        ] {
            assert_eq!(route_internal_ioctl(code), InternalIoctl::NotSupported);
        }
        assert_eq!(not_supported().status, STATUS_NOT_SUPPORTED);
        assert_eq!(read_report_forward_failed(-1).information, 0);
        assert_ne!(STATUS_SUCCESS, STATUS_NOT_SUPPORTED);
    }

    #[test]
    fn attributes_leave_reserved() {
        let ids = UsbDeviceIds {
            vendor_id: 0x05ac,
            product_id: 0x027a,
            bcd_device: 0x0100,
        };
        let mut out = [0xcc_u8; 48];
        assert_eq!(
            get_device_attributes(Ok(&mut out), ids),
            Completion::success(32)
        );
        assert_eq!(
            out[..10],
            [0x20, 0, 0, 0, 0xac, 0x05, 0x7a, 0x02, 0x01, 0x00]
        );
        assert!(out[10..].iter().all(|&b| b == 0xcc));
    }
}

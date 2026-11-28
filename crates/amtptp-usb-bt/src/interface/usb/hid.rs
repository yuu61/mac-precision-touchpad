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
//! `WdfMemoryCopyFromBuffer` through [`crate::usb::output::OutputMemory`]) or
//! `WdfRequestRetrieveOutputBuffer(Request, 32, ..)` (attributes) and passes `Ok(buffer)` or
//! `Err(status)`; an `Err` is completed as is (the C returns the retrieval status).

use crate::usb::config::FALLBACK_CONFIG;
use crate::usb::config::T2_13_CONFIG;
use amtptp_core::hid::descriptor::REPORT_DESCRIPTOR_U32_ID_LEN;
use amtptp_core::hid::hidport::{HID_DESCRIPTOR_LEN, HID_DESCRIPTOR_U32_ID, HidDeviceAttributes};

pub use amtptp_core::hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;

use crate::usb::config::UsbModel;
use crate::usb::output::OutputMemory;
use crate::usb::state::UsbDeviceIds;
use crate::usb::status::{
    Completion, NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_NOT_SUPPORTED, nt_success,
};

/// Length of every `UsbKm` report descriptor: 511 bytes.
pub const REPORT_DESCRIPTOR_LEN: usize = REPORT_DESCRIPTOR_U32_ID_LEN;

/// Report descriptor for 15-inch / 16-inch and fallback T2 models (`20000 x 12000`), 511 bytes.
pub static REPORT_DESCRIPTOR: [u8; REPORT_DESCRIPTOR_LEN] = FALLBACK_CONFIG.descriptor;

/// Report descriptor for 13-inch T2 models (`0x027A`, `0x027B`, `12992 x 7855`), 511 bytes.
pub static REPORT_DESCRIPTOR_T2_13: [u8; REPORT_DESCRIPTOR_LEN] = T2_13_CONFIG.descriptor;

/// `AmtPtpT2DefaultHidDescriptor`: `09 21 00 01 00 01 22 ff 01`.
pub const HID_DESCRIPTOR: [u8; HID_DESCRIPTOR_LEN] = HID_DESCRIPTOR_U32_ID;

/// `DEVICE_VERSION` (include/Hid.h:14): the HID `VersionNumber`, a constant (not `bcdDevice`).
pub const DEVICE_VERSION: u16 = 0x0001;

pub use amtptp_core::hid::ioctl::InternalIoctl;
/// Includes the first NUL terminator in a USB string returned to HIDCLASS.
#[must_use]
pub fn terminated_usb_string(units: &[u16], length: u16) -> Option<&[u16]> {
    let text = units.get(..=usize::from(length))?;
    let end = text.iter().position(|&unit| unit == 0)?;
    text.get(..=end)
}

/// Classifies an internal HID request.
#[must_use]
pub const fn route_internal_ioctl(code: u32) -> InternalIoctl {
    InternalIoctl::from_code(code)
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

/// Selects the report descriptor for the enumerated USB model.
#[must_use]
pub const fn report_descriptor_for(cfg: &UsbModel) -> &[u8; REPORT_DESCRIPTOR_LEN] {
    &cfg.descriptor
}

/// Copies the 511-byte report descriptor for `cfg` into `output` and completes with
/// `Information = 511`.
#[must_use]
pub fn get_report_descriptor_for_to<M: OutputMemory + ?Sized>(
    cfg: &UsbModel,
    output: Result<&mut M, NtStatus>,
) -> Completion {
    copy_completion(output, report_descriptor_for(cfg))
}

/// [`get_report_descriptor_for_to`] on a byte slice.
#[must_use]
pub fn get_report_descriptor_for(
    cfg: &UsbModel,
    output: Result<&mut [u8], NtStatus>,
) -> Completion {
    get_report_descriptor_for_to(cfg, output)
}

/// Copies the default 511-byte [`REPORT_DESCRIPTOR`] (`20000 x 12000`) and completes with
/// `Information = 511`; `STATUS_BUFFER_TOO_SMALL` if the output is shorter.
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
/// same status here). Writes `Size`, `VendorID`, `ProductID` and `VersionNumber` and zeroes `Reserved` (bytes 10..32). Completes with `Information = 32`.
///
/// Before the first `PrepareHardware` the cached descriptor is all zero, so `VendorID` and `ProductID` are 0.
#[must_use]
pub fn get_device_attributes(output: Result<&mut [u8], NtStatus>, ids: UsbDeviceIds) -> Completion {
    match output {
        Ok(buf) => match device_attributes(ids).write_to(buf) {
            Ok(()) => Completion::success(HID_DEVICE_ATTRIBUTES_LEN),
            Err(amtptp_core::hid::BufferTooSmall) => Completion::status(STATUS_BUFFER_TOO_SMALL),
        },
        Err(status) => Completion::status(status),
    }
}

const _: () = assert!(REPORT_DESCRIPTOR_LEN == 511);
const _: () = assert!(HID_DEVICE_ATTRIBUTES_LEN == 32);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb::config::config_for_pid;
    use crate::usb::status::{
        STATUS_INVALID_BUFFER_SIZE, STATUS_INVALID_DEVICE_REQUEST, STATUS_SUCCESS,
    };

    struct FailingMemory(NtStatus);

    impl OutputMemory for FailingMemory {
        fn copy_from_buffer(&mut self, _src: &[u8]) -> NtStatus {
            self.0
        }
    }

    #[test]
    fn usb_string_length_includes_one_terminator() {
        assert_eq!(
            terminated_usb_string(&[65, 66, 0], 2),
            Some(&[65, 66, 0][..])
        );
        assert_eq!(
            terminated_usb_string(&[65, 0, 66, 0], 3),
            Some(&[65, 0][..])
        );
        assert_eq!(terminated_usb_string(&[0], 0), Some(&[0][..]));
        assert_eq!(terminated_usb_string(&[65], 1), None);
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
    fn hid_descriptor_bytes_and_per_model_report_descriptors() {
        assert_eq!(
            HID_DESCRIPTOR,
            [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xff, 0x01]
        );
        assert_eq!(
            report_descriptor_for(config_for_pid(0x027a)),
            &REPORT_DESCRIPTOR_T2_13
        );
        assert_eq!(
            report_descriptor_for(config_for_pid(0x027b)),
            &REPORT_DESCRIPTOR_T2_13
        );
        assert_eq!(
            report_descriptor_for(config_for_pid(0x027c)),
            &REPORT_DESCRIPTOR
        );
        assert_eq!(
            report_descriptor_for(config_for_pid(0xffff)),
            &REPORT_DESCRIPTOR
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
        assert_eq!(route_internal_ioctl(0x000b_0013), InternalIoctl::GetString);
        for code in [
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
    fn attributes_zero_reserved() {
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
        assert_eq!(out[10..32], [0; 22]);
        assert_eq!(out[32..], [0xcc; 16]);
    }
}

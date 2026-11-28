//! The bytes and parameters of the driver's own requests to the lower Apple SPI PDO, plus the fixed
//! configuration values of `AddDevice`.
//!
//! The driver sends exactly three kinds of internal IOCTLs down, none with a timeout (spec §6.1):
//! 1. `IOCTL_HID_GET_DEVICE_ATTRIBUTES`, synchronous, in `PrepareHardware`: output = a zeroed 32-byte
//!    `HID_DEVICE_ATTRIBUTES` ([`LOWER_ATTRIBUTES_LEN`]), no input; then
//!    [`crate::device::DeviceConfig::identify`].
//! 2. `IOCTL_HID_SET_FEATURE`, synchronous, the mode switch: input = the 255-byte buffer supplied by [`with_set_state_packet`],
//!    no output.
//! 3. `IOCTL_HID_READ_REPORT`, asynchronous, one per upstream read: output = a 1024-byte lookaside
//!    buffer ([`LOWER_READ_BUFFER_LEN`]), no input.

use ptp_hid::common::REPORTID_STANDARDMOUSE;
use ptp_hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;
use ptp_hid::ioctl::{
    IOCTL_HID_GET_DEVICE_ATTRIBUTES, IOCTL_HID_READ_REPORT, IOCTL_HID_SET_FEATURE,
};

use crate::feature::HID_XFER_PACKET_LEN;
use crate::status::{NtStatus, STATUS_INVALID_DEVICE_STATE};

#[cfg(not(target_pointer_width = "64"))]
compile_error!("HID_XFER_PACKET is laid out for 64-bit targets (x64, ARM64) only");

/// IOCTL of the `PrepareHardware` identification request: `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (0x000B0027).
pub const LOWER_ATTRIBUTES_IOCTL: u32 = IOCTL_HID_GET_DEVICE_ATTRIBUTES;
/// Output length of the identification request: `sizeof(HID_DEVICE_ATTRIBUTES)` = 32, zero-filled
/// before the send.
pub const LOWER_ATTRIBUTES_LEN: usize = HID_DEVICE_ATTRIBUTES_LEN;

/// IOCTL of the mode switch: `IOCTL_HID_SET_FEATURE` (0x000B0191, `METHOD_IN_DIRECT`). Sent with
/// `WdfIoTargetSendInternalIoctlSynchronously` and an **input** memory descriptor of
/// [`SET_STATE_PACKET_LEN`] bytes; KMDF passes the buffer itself as `Irp->AssociatedIrp.SystemBuffer`.
pub const SET_STATE_IOCTL: u32 = IOCTL_HID_SET_FEATURE;
/// `HID_XFER_PACKET_SIZE` (255): length of the mode-switch input buffer.
pub const SET_STATE_PACKET_LEN: usize = 255;
/// `HID_REPORTID_MOUSE` (2): Apple's feature report ID, used as `reportId` and as report byte 0
/// (`SPI_SET_FEATURE.BusLocation`).
pub const SET_STATE_REPORT_ID: u8 = REPORTID_STANDARDMOUSE;
/// `sizeof(SPI_SET_FEATURE)` (2): `reportBufferLen` of the mode switch.
pub const SET_STATE_REPORT_LEN: u32 = 2;
/// Offset of the report inside the mode-switch buffer: right after the `HID_XFER_PACKET` (16).
pub const SET_STATE_REPORT_OFFSET: usize = HID_XFER_PACKET_LEN;

/// IOCTL of the lower read: `IOCTL_HID_READ_REPORT` (0x000B000B).
pub const LOWER_READ_IOCTL: u32 = IOCTL_HID_READ_REPORT;
/// `REPORT_BUFFER_SIZE` (1024): size of each lookaside buffer used as a lower read's output.
pub const LOWER_READ_BUFFER_LEN: usize = 1024;

/// `PTP_LIST_POOL_TAG` `'LTPA'` = 0x4C545041 (shown as "APTL" by pool tools), the lookaside pool tag.
pub const POOL_TAG: u32 = u32::from_le_bytes(*b"APTL");
const _: () = assert!(POOL_TAG == 0x4C54_5041);

/// `AddDevice` result when `WdfDeviceGetIoTarget` returns NULL (Device.c:125-129).
pub const NO_IO_TARGET_STATUS: NtStatus = STATUS_INVALID_DEVICE_STATE;

/// Constructs the lower target's mode-switch input and keeps it in place during the send.
pub fn with_set_state_packet<R, F: FnOnce(&[u8; SET_STATE_PACKET_LEN]) -> R>(
    enabled: bool,
    send: F,
) -> Option<R> {
    const {
        assert!(SET_STATE_PACKET_LEN >= HID_XFER_PACKET_LEN + 2);
    }
    // The two-byte payload fits the fixed-size ABI buffer.
    ptp_hid::hidport::with_xfer_packet(
        SET_STATE_REPORT_ID,
        &[SET_STATE_REPORT_ID, u8::from(enabled)],
        send,
    )
}

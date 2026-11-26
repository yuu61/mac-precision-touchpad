//! The bytes and parameters of the driver's own requests to the lower Apple SPI PDO, plus the fixed
//! configuration values of `AddDevice`.
//!
//! The driver sends exactly three kinds of internal IOCTLs down, none with a timeout (spec §6.1):
//! 1. `IOCTL_HID_GET_DEVICE_ATTRIBUTES`, synchronous, in `PrepareHardware`: output = a zeroed 32-byte
//!    `HID_DEVICE_ATTRIBUTES` ([`LOWER_ATTRIBUTES_LEN`]), no input; then
//!    [`crate::device::DeviceConfig::identify`].
//! 2. `IOCTL_HID_SET_FEATURE`, synchronous, the mode switch: input = the 255-byte [`SetStatePacket`],
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

/// A GUID in its C `GUID` field layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guid {
    /// `Data1`.
    pub data1: u32,
    /// `Data2`.
    pub data2: u16,
    /// `Data3`.
    pub data3: u16,
    /// `Data4`.
    pub data4: [u8; 8],
}

/// `GUID_DEVINTERFACE_AmtPtpDeviceSpiKm` {f271e39d-d211-4a6e-a439-63e51940d210} (Public.h:22-24),
/// registered with `WdfDeviceCreateDeviceInterface` (functionally dead, kept for compatibility).
pub const DEVICE_INTERFACE_GUID: Guid = Guid {
    data1: 0xf271_e39d,
    data2: 0xd211,
    data3: 0x4a6e,
    data4: [0xa4, 0x39, 0x63, 0xe5, 0x19, 0x40, 0xd2, 0x10],
};

/// The 255-byte input buffer of the mode switch (`AmtPtpSpiSetState`, Device.c:457-539), aligned like a
/// `HID_XFER_PACKET` (P2: C cast an unaligned `UCHAR[255]`).
///
/// Layout (x64/ARM64): bytes 0..8 `reportBuffer` = the address of byte 16 **of this same buffer**;
/// 8..12 `reportBufferLen` = 2; 12 `reportId` = 2; 13..16 zero; 16 = 2 (`BusLocation`, the report ID);
/// 17 = 1 to enable (0 to disable); the rest zero.
///
/// Because of the self-pointer the glue must [`fill`](Self::fill) it in place and keep it at that address
/// (not move it) until the synchronous send returns.
#[derive(Clone, Debug, PartialEq, Eq)]
#[repr(C, align(8))]
pub struct SetStatePacket {
    /// The raw bytes.
    pub bytes: [u8; SET_STATE_PACKET_LEN],
}

impl Default for SetStatePacket {
    fn default() -> Self {
        Self::zeroed()
    }
}

impl SetStatePacket {
    /// An all-zero buffer (C: `RtlZeroMemory`).
    #[must_use]
    pub const fn zeroed() -> Self {
        Self {
            bytes: [0; SET_STATE_PACKET_LEN],
        }
    }

    /// The buffer contents for `desired_state` when the buffer starts at `buffer_address`. Byte 0..8 hold
    /// `buffer_address + 16` (little-endian, pointer width).
    #[must_use]
    pub fn image(desired_state: bool, buffer_address: usize) -> [u8; SET_STATE_PACKET_LEN] {
        let mut bytes = [0_u8; SET_STATE_PACKET_LEN];
        let report_buffer = buffer_address.wrapping_add(SET_STATE_REPORT_OFFSET);
        let header = xfer_header(report_buffer, SET_STATE_REPORT_LEN, SET_STATE_REPORT_ID);
        let report = [SET_STATE_REPORT_ID, u8::from(desired_state)];
        if let Some((head, rest)) = bytes.split_first_chunk_mut::<HID_XFER_PACKET_LEN>() {
            *head = header;
            if let Some((r, _)) = rest.split_first_chunk_mut::<2>() {
                *r = report;
            }
        }
        bytes
    }

    /// Fills the buffer in place for `desired_state` (C calls it only with TRUE), with the self-pointer
    /// set to this buffer's own byte 16.
    pub fn fill(&mut self, desired_state: bool) {
        let address = self.bytes.as_ptr().addr();
        self.bytes = Self::image(desired_state, address);
    }

    /// Pointer to the first byte, for `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER`.
    pub const fn as_mut_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr()
    }
}

/// The 16-byte x64/ARM64 `HID_XFER_PACKET`: `reportBuffer` @0 (8 bytes), `reportBufferLen` @8, `reportId`
/// @12, padding.
const fn xfer_header(
    report_buffer: usize,
    report_buffer_len: u32,
    report_id: u8,
) -> [u8; HID_XFER_PACKET_LEN] {
    // usize is 64 bits here (checked by the compile_error above), so the conversion is lossless.
    let [p0, p1, p2, p3, p4, p5, p6, p7] = (report_buffer as u64).to_le_bytes();
    let [l0, l1, l2, l3] = report_buffer_len.to_le_bytes();
    [
        p0, p1, p2, p3, p4, p5, p6, p7, l0, l1, l2, l3, report_id, 0, 0, 0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_state_image_matches_spec_6_2() {
        let img = SetStatePacket::image(true, 0);
        assert_eq!(
            img[..18],
            [16, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 2, 0, 0, 0, 2, 1]
        );
        assert!(img[18..].iter().all(|&b| b == 0));
        assert_eq!(SetStatePacket::image(false, 0)[17], 0);
    }

    #[test]
    fn fill_points_into_itself() {
        let mut p = SetStatePacket::zeroed();
        p.fill(true);
        let base = p.bytes.as_ptr() as usize;
        assert_eq!(p.bytes[..8], ((base + 16) as u64).to_le_bytes());
        assert_eq!(core::mem::align_of::<SetStatePacket>(), 8);
    }
}

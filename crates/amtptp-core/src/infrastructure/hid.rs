//! Windows Precision Touchpad (PTP) HID building blocks shared by the four AmtPtp drivers
//! (`AmtPtpDeviceUsbKm`, `AmtPtpDeviceSpiKm`, `AmtPtpHidFilter`).
//!
//! Everything here is hardware-independent and `no_std`. It has no WDK dependency and is host-testable:
//!
//! - [`common`]: report IDs, PTP constants and usages from `HidCommon.h` / `Hid.h`.
//! - [`item`]: HID short-item prefix bytes from `HidCommon.h`.
//! - [`ioctl`]: `CTL_CODE` as a `const fn` and every HID IOCTL code the drivers handle or send.
//! - [`descriptor`]: `const fn` report-descriptor builders for all four drivers.
//! - [`hidport`]: `HID_DESCRIPTOR` (9 bytes) and `HID_DEVICE_ATTRIBUTES` (32 bytes) byte codecs.
//! - [`feature`]: feature reports: device caps 0x07, HQA 0x08, input mode 0x04, selective reporting 0x06,
//!   and the user-mode-app configuration report 0x09.
//! - [`input`]: input report 0x05 in its two wire shapes: 9-byte contacts (50 bytes) and SPI 5-byte
//!   contacts (30 bytes).
//! - [`read_tracker`]: the lower reads a driver has in flight, and the epoch that makes a read issued
//!   before a suspend stale.
//! - [`scan_time`]: the per-session Scan Time clock.
//!
//! All wire formats are encoded and decoded explicitly as little-endian bytes. There are no packed or
//! bitfield structs and no `unsafe`.
//!
//! # Example
//!
//! A core crate owns its per-family axis values and builds its descriptors at compile time:
//!
//! ```
//! use amtptp_core::hid::descriptor::{Axes, REPORT_DESCRIPTOR_U32_ID_LEN, report_descriptor_u32_id};
//! use amtptp_core::hid::hidport::HID_DESCRIPTOR_U32_ID;
//! use amtptp_core::hid::input::{PtpContact, PtpReport};
//!
//! const MT2: Axes = Axes {
//!     x_logical_max: 7612,
//!     x_physical_max: 1600,
//!     y_physical_max: 1149,
//!     y_logical_max: 5065,
//! };
//! pub static REPORT_DESCRIPTOR: [u8; REPORT_DESCRIPTOR_U32_ID_LEN] =
//!     report_descriptor_u32_id(MT2);
//!
//! assert_eq!(REPORT_DESCRIPTOR.len(), 511);
//! assert_eq!(HID_DESCRIPTOR_U32_ID, [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xff, 0x01]);
//!
//! let mut report = PtpReport { contact_count: 1, ..PtpReport::EMPTY };
//! report.contacts[0] = PtpContact { confidence: true, tip_switch: true, contact_id: 0, x: 100, y: 200 };
//! assert_eq!(report.to_bytes()[..10], [0x05, 0x03, 0, 0, 0, 0, 100, 0, 200, 0]);
//! ```

mod bytes;
pub mod common;
pub mod delivery;
pub mod descriptor;
pub mod feature;
pub mod hidport;
pub mod input;
pub mod ioctl;
pub mod item;
pub mod read_tracker;
pub mod scan_time;
pub mod status;

/// The caller's buffer is shorter than the report or structure that has to be written into (or read
/// from) it.
///
/// Each core crate maps this to the NTSTATUS its C driver returned on that path. For a feature report
/// that is usually `STATUS_INVALID_BUFFER_SIZE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferTooSmall;

//! Windows Precision Touchpad (PTP) HID building blocks shared by the four AmtPtp drivers
//! (AmtPtpDeviceUsbUm, AmtPtpDeviceUsbKm, AmtPtpDeviceSpiKm, AmtPtpHidFilter).
//!
//! Everything here is hardware-independent and `no_std`. It has no WDK dependency and is host-testable:
//!
//! - [`common`]: report IDs, PTP constants and usages from `HidCommon.h` / `Hid.h`.
//! - [`item`]: HID short-item prefix bytes from `HidCommon.h`.
//! - [`ioctl`]: `CTL_CODE` as a `const fn` and every HID IOCTL code the drivers handle or send.
//! - [`descriptor`]: `const fn` report-descriptor builders that reproduce all 16 C report descriptors
//!   byte for byte.
//! - [`hidport`]: `HID_DESCRIPTOR` (9 bytes) and `HID_DEVICE_ATTRIBUTES` (32 bytes) byte codecs.
//! - [`feature`]: feature reports: device caps 0x07, HQA 0x08, input mode 0x04, selective reporting 0x06,
//!   and the user-mode-app configuration report 0x09.
//! - [`input`]: input report 0x05 in its two wire shapes: 9-byte contacts (50 bytes) and SPI 5-byte
//!   contacts (30 bytes).
//!
//! All wire formats are encoded and decoded explicitly as little-endian bytes. There are no packed or
//! bitfield structs and no `unsafe`.
//!
//! # Example
//!
//! A core crate owns its per-family axis values and builds its descriptors at compile time:
//!
//! ```
//! use ptp_hid::descriptor::{Axes, REPORT_DESCRIPTOR_U32_ID_UMAPP_LEN, report_descriptor_u32_id_umapp};
//! use ptp_hid::hidport::HID_DESCRIPTOR_U32_ID_UMAPP;
//! use ptp_hid::input::{PtpContact, PtpReport};
//!
//! const MT2: Axes = Axes {
//!     x_logical_max: 7612,
//!     x_physical_max: 1600,
//!     y_physical_max: 1149,
//!     y_logical_max: 5065,
//! };
//! pub static REPORT_DESCRIPTOR: [u8; REPORT_DESCRIPTOR_U32_ID_UMAPP_LEN] =
//!     report_descriptor_u32_id_umapp(MT2);
//!
//! assert_eq!(REPORT_DESCRIPTOR.len(), 516);
//! assert_eq!(HID_DESCRIPTOR_U32_ID_UMAPP, [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0x04, 0x02]);
//!
//! let mut report = PtpReport { contact_count: 1, ..PtpReport::EMPTY };
//! report.contacts[0] = PtpContact { confidence: true, tip_switch: true, contact_id: 0, x: 100, y: 200 };
//! assert_eq!(report.to_bytes()[..10], [0x05, 0x03, 0, 0, 0, 0, 100, 0, 200, 0]);
//! ```

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

mod bytes;
pub mod common;
pub mod descriptor;
pub mod feature;
pub mod hidport;
pub mod input;
pub mod ioctl;
pub mod item;

/// The caller's buffer is shorter than the report or structure that has to be written into (or read
/// from) it.
///
/// Each core crate maps this to the NTSTATUS its C driver returned on that path. For a feature report
/// that is usually `STATUS_INVALID_BUFFER_SIZE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferTooSmall;

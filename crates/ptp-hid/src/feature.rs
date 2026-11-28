//! Feature reports of the PTP descriptors: device caps 0x07, HQA certification 0x08, input mode 0x04,
//! selective reporting 0x06 and the user-mode-app configuration report 0x09.
//!
//! Every report is (de)serialized explicitly, byte by byte. The C structs (`PTP_DEVICE_*_REPORT`,
//! `PTP_USERMODEAPP_CONF_REPORT`) are only used as byte layouts here; there are no packed or bitfield
//! structs.
//!
//! Conventions shared by all decoders in this module:
//! - Byte 0 (the report ID) is **not** checked. The C drivers never check it either: the KMDF drivers
//!   switch on `HID_XFER_PACKET::reportId`, and `UsbUm` takes the ID of a SET from the output buffer length.
//! - A buffer that is too short for the bytes the decoder reads gives `None`. What a driver does in that
//!   case (the C code often reads past the end) is decided by its core crate.
//!
//! Encoders that write into a caller's buffer (`write_to`, [`write_hqa_report`]) write only the bytes of
//! their report and leave every other byte of the buffer untouched.

use crate::BufferTooSmall;
use crate::bytes::{concat, write_prefix};
use crate::common::{
    PTP_BUTTON_TYPE_CLICK_PAD, PTP_MAX_CONTACT_POINTS, REPORTID_DEVICE_CAPS, REPORTID_FUNCSWITCH,
    REPORTID_PTPHQA, REPORTID_REPORTMODE,
};
use core::sync::atomic::{AtomicU8, Ordering};

// ---------------------------------------------------------------------------------------------------
// 0x07 device capabilities
// ---------------------------------------------------------------------------------------------------

/// `sizeof(PTP_DEVICE_CAPS_FEATURE_REPORT)`: 3 bytes (ID, maximum contacts, button type). The GET
/// handlers of all four drivers reject a shorter report buffer with `STATUS_INVALID_BUFFER_SIZE`.
pub const DEVICE_CAPS_REPORT_LEN: usize = 3;

/// Feature report 0x07 (`PTP_DEVICE_CAPS_FEATURE_REPORT`): `[0x07, MaximumContactPoints, ButtonType]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceCapsReport {
    /// `MaximumContactPoints` (byte 1), usage Contact Count Maximum (0x55).
    pub max_contact_points: u8,
    /// `ButtonType` (byte 2), usage Pad Type (0x59).
    pub button_type: u8,
}

impl DeviceCapsReport {
    /// What all four drivers report: [`PTP_MAX_CONTACT_POINTS`] (5) contacts and
    /// [`PTP_BUTTON_TYPE_CLICK_PAD`] (0).
    pub const PTP: Self = Self {
        max_contact_points: PTP_MAX_CONTACT_POINTS,
        button_type: PTP_BUTTON_TYPE_CLICK_PAD,
    };

    /// Encodes the report, ID included.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DEVICE_CAPS_REPORT_LEN] {
        [
            REPORTID_DEVICE_CAPS,
            self.max_contact_points,
            self.button_type,
        ]
    }

    /// Decodes bytes 1 and 2. Returns `None` if `buf` is shorter than 3 bytes.
    #[must_use]
    pub const fn from_bytes(buf: &[u8]) -> Option<Self> {
        match buf.first_chunk::<DEVICE_CAPS_REPORT_LEN>() {
            Some(&[_, max_contact_points, button_type]) => Some(Self {
                max_contact_points,
                button_type,
            }),
            None => None,
        }
    }

    /// Writes the 3 report bytes to the start of `buf`, as the C GET handlers do. Bytes after the third
    /// are left untouched. Returns [`BufferTooSmall`] (the C drivers: `STATUS_INVALID_BUFFER_SIZE`) and
    /// writes nothing if `buf` is shorter than 3 bytes.
    ///
    /// # Errors
    /// Returns [`BufferTooSmall`] if the buffer cannot hold the report.
    pub fn write_to(self, buf: &mut [u8]) -> Result<(), BufferTooSmall> {
        write_prefix(buf, &self.to_bytes())
    }
}

/// The bytes every driver returns for `GET_FEATURE` 0x07: `07 05 00`.
pub const DEVICE_CAPS_REPORT: [u8; DEVICE_CAPS_REPORT_LEN] = DeviceCapsReport::PTP.to_bytes();

// ---------------------------------------------------------------------------------------------------
// 0x08 HQA certification
// ---------------------------------------------------------------------------------------------------

/// Length of [`HQA_BLOB`].
pub const HQA_BLOB_LEN: usize = 256;
/// `sizeof(PTP_DEVICE_HQA_CERTIFICATION_REPORT)`: report ID plus the 256-byte blob. The GET handlers of all
/// four drivers reject a shorter report buffer with `STATUS_INVALID_BUFFER_SIZE`.
pub const HQA_REPORT_LEN: usize = 1 + HQA_BLOB_LEN;

/// `DEFAULT_PTP_HQA_BLOB`, the 256-byte PTP certification blob. Byte-identical in all four drivers'
/// headers (SHA-256 `b57b851d567808906f61a0122273da05c972140f1007355390aaf5dda8a072af`).
#[rustfmt::skip]
pub const HQA_BLOB: [u8; HQA_BLOB_LEN] = [
    0xfc, 0x28, 0xfe, 0x84, 0x40, 0xcb, 0x9a, 0x87, 0x0d, 0xbe, 0x57, 0x3c, 0xb6, 0x70, 0x09, 0x88,
    0x07, 0x97, 0x2d, 0x2b, 0xe3, 0x38, 0x34, 0xb6, 0x6c, 0xed, 0xb0, 0xf7, 0xe5, 0x9c, 0xf6, 0xc2,
    0x2e, 0x84, 0x1b, 0xe8, 0xb4, 0x51, 0x78, 0x43, 0x1f, 0x28, 0x4b, 0x7c, 0x2d, 0x53, 0xaf, 0xfc,
    0x47, 0x70, 0x1b, 0x59, 0x6f, 0x74, 0x43, 0xc4, 0xf3, 0x47, 0x18, 0x53, 0x1a, 0xa2, 0xa1, 0x71,
    0xc7, 0x95, 0x0e, 0x31, 0x55, 0x21, 0xd3, 0xb5, 0x1e, 0xe9, 0x0c, 0xba, 0xec, 0xb8, 0x89, 0x19,
    0x3e, 0xb3, 0xaf, 0x75, 0x81, 0x9d, 0x53, 0xb9, 0x41, 0x57, 0xf4, 0x6d, 0x39, 0x25, 0x29, 0x7c,
    0x87, 0xd9, 0xb4, 0x98, 0x45, 0x7d, 0xa7, 0x26, 0x9c, 0x65, 0x3b, 0x85, 0x68, 0x89, 0xd7, 0x3b,
    0xbd, 0xff, 0x14, 0x67, 0xf2, 0x2b, 0xf0, 0x2a, 0x41, 0x54, 0xf0, 0xfd, 0x2c, 0x66, 0x7c, 0xf8,
    0xc0, 0x8f, 0x33, 0x13, 0x03, 0xf1, 0xd3, 0xc1, 0x0b, 0x89, 0xd9, 0x1b, 0x62, 0xcd, 0x51, 0xb7,
    0x80, 0xb8, 0xaf, 0x3a, 0x10, 0xc1, 0x8a, 0x5b, 0xe8, 0x8a, 0x56, 0xf0, 0x8c, 0xaa, 0xfa, 0x35,
    0xe9, 0x42, 0xc4, 0xd8, 0x55, 0xc3, 0x38, 0xcc, 0x2b, 0x53, 0x5c, 0x69, 0x52, 0xd5, 0xc8, 0x73,
    0x02, 0x38, 0x7c, 0x73, 0xb6, 0x41, 0xe7, 0xff, 0x05, 0xd8, 0x2b, 0x79, 0x9a, 0xe2, 0x34, 0x60,
    0x8f, 0xa3, 0x32, 0x1f, 0x09, 0x78, 0x62, 0xbc, 0x80, 0xe3, 0x0f, 0xbd, 0x65, 0x20, 0x08, 0x13,
    0xc1, 0xe2, 0xee, 0x53, 0x2d, 0x86, 0x7e, 0xa7, 0x5a, 0xc5, 0xd3, 0x7d, 0x98, 0xbe, 0x31, 0x48,
    0x1f, 0xfb, 0xda, 0xaf, 0xa2, 0xa8, 0x6a, 0x89, 0xd6, 0xbf, 0xf2, 0xd3, 0x32, 0x2a, 0x9a, 0xe4,
    0xcf, 0x17, 0xb7, 0xb8, 0xf4, 0xe1, 0x33, 0x08, 0x24, 0x8b, 0xc4, 0x43, 0xa5, 0xe5, 0x24, 0xc2,
];

/// Feature report 0x08: the report ID followed by the 256-byte [`HQA_BLOB`].
pub const HQA_REPORT: [u8; HQA_REPORT_LEN] = concat(&[REPORTID_PTPHQA], &HQA_BLOB);

/// Writes all 257 bytes of feature report 0x08, leaving any trailing bytes untouched.
///
/// The `Information` value differs per driver (`UsbUm` 257, the KMDF drivers 0) and is the caller's job.
///
/// # Errors
/// Returns [`BufferTooSmall`] if the buffer cannot hold the report.
pub fn write_hqa_report(buf: &mut [u8]) -> Result<(), BufferTooSmall> {
    write_prefix(buf, &HQA_REPORT)
}

// ---------------------------------------------------------------------------------------------------
// 0x04 input mode
// ---------------------------------------------------------------------------------------------------

/// `sizeof(PTP_DEVICE_INPUT_MODE_REPORT)`: 2 bytes (ID, mode).
pub const INPUT_MODE_REPORT_LEN: usize = 2;
/// Offset of the `Mode` byte in feature report 0x04.
pub const INPUT_MODE_OFFSET: usize = 1;

/// Feature report 0x04 (`PTP_DEVICE_INPUT_MODE_REPORT`): `[0x04, Mode]`.
///
/// `mode` is kept raw: [`crate::common::PTP_COLLECTION_MOUSE`] (0), [`crate::common::PTP_COLLECTION_WINDOWS`]
/// (3), or any other value Windows sends (the drivers ignore other values in different ways).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputModeReport {
    /// `Mode` (byte 1), usage Input Mode (0x52).
    pub mode: u8,
}

impl InputModeReport {
    /// Encodes the report, ID included.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; INPUT_MODE_REPORT_LEN] {
        [REPORTID_REPORTMODE, self.mode]
    }

    /// Decodes byte 1. Returns `None` if `buf` is shorter than 2 bytes. Byte 0 is not checked.
    #[must_use]
    pub const fn from_bytes(buf: &[u8]) -> Option<Self> {
        match buf.first_chunk::<INPUT_MODE_REPORT_LEN>() {
            Some(&[_, mode]) => Some(Self { mode }),
            None => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// 0x06 selective reporting (function switch)
// ---------------------------------------------------------------------------------------------------

/// Length of feature report 0x06 as the descriptor defines it: ID plus one byte with two 1-bit switches
/// and 6 padding bits. This is also `sizeof(PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT)` in `UsbUm` and `HidFilter`.
pub const SELECTIVE_REPORT_LEN: usize = 2;
/// Offset of the switch byte in feature report 0x06 as the descriptor defines it. Every driver reads it
/// here.
pub const SELECTIVE_REPORT_BITS_OFFSET: usize = 1;
/// `sizeof(PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT)` in the C `UsbKm` and `SpiKm`: their struct had an extra
/// `DeviceMode` byte at offset 1 that is not in the descriptor, so they read the switches from byte 2,
/// outside the 2-byte report (spec A7.2). The port reads byte 1 everywhere.
pub const SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN: usize = 3;

/// The switch byte of feature report 0x06: bit 0 is the Surface Switch (Digitizers usage 0x57, the first
/// usage in the descriptor), bit 1 the Button Switch (usage 0x58); bits 2-7 are padding.
///
/// The C drivers named the bits the other way round (`ButtonReport` for bit 0, UM-B14); `UsbUm` applied
/// them swapped, `UsbKm` ignored the report, and `SpiKm` read the wrong byte. All four drivers now decode
/// the report with this type and start from [`SelectiveReportBits::POWER_ON`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectiveReportBits {
    /// Bit 0, Surface Switch: report surface contacts.
    pub surface_report: bool,
    /// Bit 1, Button Switch: report the button state.
    pub button_report: bool,
}

impl Default for SelectiveReportBits {
    fn default() -> Self {
        Self::POWER_ON
    }
}

impl SelectiveReportBits {
    /// The state after a cold boot or power reset: report surface contacts and the button state. It is
    /// not kept across a power reset, so the drivers restore it whenever the device (re)starts
    /// (`PrepareHardware`).
    pub const POWER_ON: Self = Self {
        surface_report: true,
        button_report: true,
    };

    /// Decodes bits 0 and 1 of the switch byte. The padding bits are ignored.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        Self {
            surface_report: byte & 0x01 != 0,
            button_report: byte & 0x02 != 0,
        }
    }

    /// Encodes the switch byte with the padding bits zero.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        (self.surface_report as u8) | ((self.button_report as u8) << 1)
    }

    /// Decodes the switch byte at [`SELECTIVE_REPORT_BITS_OFFSET`]. Returns `None` if `buf` is shorter than
    /// 2 bytes. Byte 0 is not checked.
    #[must_use]
    pub const fn from_report(buf: &[u8]) -> Option<Self> {
        match buf.first_chunk::<SELECTIVE_REPORT_LEN>() {
            Some(&[_, byte]) => Some(Self::from_byte(byte)),
            None => None,
        }
    }

    /// Encodes feature report 0x06 in the descriptor's 2-byte layout: `[0x06, switch byte]`.
    #[must_use]
    pub const fn to_report(self) -> [u8; SELECTIVE_REPORT_LEN] {
        [REPORTID_FUNCSWITCH, self.to_byte()]
    }
}

// ---------------------------------------------------------------------------------------------------
/// The surface and button switches published together by one `SET_FEATURE`.
#[derive(Debug, Default)]
pub struct SelectiveReportState(AtomicU8);

impl SelectiveReportState {
    /// Initial switch settings.
    #[must_use]
    pub const fn new(bits: SelectiveReportBits) -> Self {
        Self(AtomicU8::new(bits.to_byte()))
    }
    /// Loads both switches from the same report.
    pub fn load(&self) -> SelectiveReportBits {
        SelectiveReportBits::from_byte(self.0.load(Ordering::Relaxed))
    }
    /// Publishes both switches together.
    pub fn store(&self, bits: SelectiveReportBits) {
        self.0.store(bits.to_byte(), Ordering::Relaxed);
    }
}

// C struct sizes measured by the C harness (crosscut spec A3; fixtures `sizeof` lines).
const _: () = assert!(DEVICE_CAPS_REPORT_LEN == 3);
const _: () = assert!(HQA_REPORT_LEN == 257);
const _: () = assert!(INPUT_MODE_REPORT_LEN == 2);
const _: () = assert!(SELECTIVE_REPORT_LEN == 2);
const _: () = assert!(SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN == 3);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_caps_bytes() {
        assert_eq!(DEVICE_CAPS_REPORT, [0x07, 0x05, 0x00]);
        assert_eq!(
            DeviceCapsReport::from_bytes(&[0x07, 0x05, 0x00]),
            Some(DeviceCapsReport::PTP)
        );
        assert_eq!(DeviceCapsReport::from_bytes(&[0x07, 0x05]), None);
    }

    #[test]
    fn device_caps_write_to() {
        // HidFilter spec D.5: id 0x07, buffer of length 3 -> `07 05 00`; length 2 -> untouched.
        let mut buf = [0xAA_u8; 3];
        assert_eq!(DeviceCapsReport::PTP.write_to(&mut buf), Ok(()));
        assert_eq!(buf, [0x07, 0x05, 0x00]);
        let mut short = [0xAA_u8; 2];
        assert_eq!(
            DeviceCapsReport::PTP.write_to(&mut short),
            Err(BufferTooSmall)
        );
        assert_eq!(short, [0xAA, 0xAA]);
        let mut long = [0xAA_u8; 8];
        assert_eq!(DeviceCapsReport::PTP.write_to(&mut long), Ok(()));
        assert_eq!(long, [0x07, 0x05, 0x00, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA]);
    }

    #[test]
    fn hqa_blob_ends() {
        assert_eq!(HQA_BLOB[..4], [0xfc, 0x28, 0xfe, 0x84]);
        assert_eq!(HQA_BLOB[252..], [0xa5, 0xe5, 0x24, 0xc2]);
        assert_eq!(HQA_REPORT[0], 0x08);
        assert_eq!(HQA_REPORT[1..], HQA_BLOB);
    }

    #[test]
    fn hqa_writes_the_full_report() {
        let mut buf = [0xAA_u8; HQA_REPORT_LEN];
        assert_eq!(write_hqa_report(&mut buf), Ok(()));
        assert_eq!(buf, HQA_REPORT);
        // Length 256 -> rejected, untouched.
        let mut short = [0xAA_u8; HQA_REPORT_LEN - 1];
        assert_eq!(write_hqa_report(&mut short), Err(BufferTooSmall));
        assert!(short.iter().all(|&b| b == 0xAA));
        let mut long = [0x55_u8; 300];
        assert_eq!(write_hqa_report(&mut long), Ok(()));
        assert_eq!(long[..HQA_REPORT_LEN], HQA_REPORT);
        assert!(long[HQA_REPORT_LEN..].iter().all(|&b| b == 0x55));
    }

    #[test]
    fn input_mode() {
        assert_eq!(InputModeReport { mode: 3 }.to_bytes(), [0x04, 0x03]);
        assert_eq!(
            InputModeReport::from_bytes(&[0x04, 0x00]),
            Some(InputModeReport { mode: 0 })
        );
        // Byte 0 is not validated; extra bytes are ignored.
        assert_eq!(
            InputModeReport::from_bytes(&[0xEE, 0x05, 0x77]),
            Some(InputModeReport { mode: 5 })
        );
        assert_eq!(InputModeReport::from_bytes(&[0x04]), None);
        assert_eq!(InputModeReport::from_bytes(&[]), None);
    }

    #[test]
    fn selective_bits() {
        for byte in 0..=u8::MAX {
            let bits = SelectiveReportBits::from_byte(byte);
            assert_eq!(bits.surface_report, byte & 1 == 1);
            assert_eq!(bits.button_report, byte & 2 == 2);
            assert_eq!(bits.to_byte(), byte & 3);
        }
        // `06 03` -> both on; `06 01` -> surface only (usage 0x57 is bit 0); `06 02` -> button only.
        assert_eq!(
            SelectiveReportBits::from_report(&[0x06, 0x03]),
            Some(SelectiveReportBits::POWER_ON)
        );
        assert_eq!(
            SelectiveReportBits::from_report(&[0x06, 0x01]),
            Some(SelectiveReportBits {
                surface_report: true,
                button_report: false
            })
        );
        assert_eq!(
            SelectiveReportBits::from_report(&[0x06, 0x02]),
            Some(SelectiveReportBits {
                surface_report: false,
                button_report: true
            })
        );
        assert_eq!(SelectiveReportBits::from_report(&[0x06]), None);
    }

    #[test]
    fn selective_report_encode() {
        let bits = SelectiveReportBits {
            surface_report: false,
            button_report: true,
        };
        assert_eq!(bits.to_report(), [0x06, 0x02]);
        assert_eq!(SelectiveReportBits::default().to_report(), [0x06, 0x03]);
    }
}

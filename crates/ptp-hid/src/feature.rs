//! Feature reports of the PTP descriptors: device caps 0x07, HQA certification 0x08, input mode 0x04,
//! selective reporting 0x06 and the user-mode-app configuration report 0x09.
//!
//! Every report is (de)serialized explicitly, byte by byte. The C structs (`PTP_DEVICE_*_REPORT`,
//! `PTP_USERMODEAPP_CONF_REPORT`) are only used as byte layouts here; there are no packed or bitfield
//! structs.
//!
//! Conventions shared by all decoders in this module:
//! - Byte 0 (the report ID) is **not** checked. The C drivers never check it either: the KMDF drivers
//!   switch on `HID_XFER_PACKET::reportId`, and UsbUm takes the ID of a SET from the output buffer length.
//! - A buffer that is too short for the bytes the decoder reads gives `None`. What a driver does in that
//!   case (the C code often reads past the end) is decided by its core crate.
//!
//! Encoders that write into a caller's buffer (`write_to`, [`write_hqa_report_as_c`]) write exactly the
//! bytes the C code writes and leave every other byte of the buffer untouched.

use crate::BufferTooSmall;
use crate::bytes::{concat, write_prefix};
use crate::common::{
    PTP_BUTTON_TYPE_CLICK_PAD, PTP_MAX_CONTACT_POINTS, REPORTID_DEVICE_CAPS, REPORTID_FUNCSWITCH,
    REPORTID_PTPHQA, REPORTID_REPORTMODE, REPORTID_UMAPP_CONF,
};

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
    pub const fn to_bytes(self) -> [u8; DEVICE_CAPS_REPORT_LEN] {
        [
            REPORTID_DEVICE_CAPS,
            self.max_contact_points,
            self.button_type,
        ]
    }

    /// Decodes bytes 1 and 2. Returns `None` if `buf` is shorter than 3 bytes.
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
    pub fn write_to(self, buf: &mut [u8]) -> Result<(), BufferTooSmall> {
        write_prefix(buf, &self.to_bytes())
    }
}

/// The bytes every driver returns for GET_FEATURE 0x07: `07 05 00`.
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
///
/// Preserved known issue: no C driver actually sends this blob. See [`write_hqa_report_as_c`].
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

/// The first byte of [`HQA_BLOB`] (0xFC): the only blob byte the C drivers write.
pub const HQA_BLOB_FIRST_BYTE: u8 = match HQA_BLOB.first() {
    Some(b) => *b,
    None => 0,
};
const _: () = assert!(HQA_BLOB_FIRST_BYTE == 0xfc);

/// The *intended* feature report 0x08: report ID 0x08 followed by the whole [`HQA_BLOB`] (257 bytes).
///
/// No C driver sends this (see [`write_hqa_report_as_c`]). It is provided for completeness and tests.
pub const HQA_REPORT: [u8; HQA_REPORT_LEN] = concat(&[REPORTID_PTPHQA], &HQA_BLOB);

/// Writes feature report 0x08 exactly as all four C drivers do.
///
/// The C code is `*certReport->CertificationBlob = DEFAULT_PTP_HQA_BLOB;`. Because the macro is a comma
/// list, this assigns only the first blob byte (comma operator; spec A7.1, preserved known issue P3). So:
/// - if `buf` is shorter than [`HQA_REPORT_LEN`] (257), nothing is written and [`BufferTooSmall`] is
///   returned (the C drivers: `STATUS_INVALID_BUFFER_SIZE`);
/// - otherwise `buf[0] = 0x08` and `buf[1] = 0xFC` are written, and `buf[2..]` is left untouched.
///
/// The `Information` value differs per driver (UsbUm 257, the KMDF drivers 0) and is the caller's job.
pub fn write_hqa_report_as_c(buf: &mut [u8]) -> Result<(), BufferTooSmall> {
    if buf.len() < HQA_REPORT_LEN {
        return Err(BufferTooSmall);
    }
    write_prefix(buf, &[REPORTID_PTPHQA, HQA_BLOB_FIRST_BYTE])
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
    pub const fn to_bytes(self) -> [u8; INPUT_MODE_REPORT_LEN] {
        [REPORTID_REPORTMODE, self.mode]
    }

    /// Decodes byte 1. Returns `None` if `buf` is shorter than 2 bytes. Byte 0 is not checked.
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
/// and 6 padding bits. This is also `sizeof(PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT)` in UsbUm and HidFilter.
pub const SELECTIVE_REPORT_LEN: usize = 2;
/// Offset of the switch byte in feature report 0x06 as the descriptor defines it. UsbUm and HidFilter read
/// it here.
pub const SELECTIVE_REPORT_BITS_OFFSET: usize = 1;
/// `sizeof(PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT)` in UsbKm and SpiKm: their C struct has an extra
/// `DeviceMode` byte at offset 1 that is not in the descriptor.
pub const SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN: usize = 3;
/// Offset at which UsbKm and SpiKm read the switch byte (after their spurious `DeviceMode` byte). It lies
/// outside the 2-byte report that the descriptor defines (spec A7.2). How each driver deals with that is
/// decided in its core crate.
pub const SELECTIVE_REPORT_DEVICE_MODE_BITS_OFFSET: usize = 2;

/// The switch byte of feature report 0x06, with the C bitfield names (MSVC allocates bitfields from the
/// LSB): bit 0 `ButtonReport`, bit 1 `SurfaceReport`, bits 2-7 padding.
///
/// Preserved naming bug (UM-B14): in the descriptor, bit 0 has usage 0x57 and bit 1 usage 0x58, which the
/// HID Usage Tables call *Surface Switch* and *Button Switch*. The C names are therefore swapped relative
/// to the standard. This type keeps the C names; it only maps bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectiveReportBits {
    /// Bit 0 (`ButtonReport : 1`).
    pub button_report: bool,
    /// Bit 1 (`SurfaceReport : 1`).
    pub surface_report: bool,
}

impl SelectiveReportBits {
    /// Decodes bits 0 and 1 of the switch byte. The padding bits are ignored.
    pub const fn from_byte(byte: u8) -> Self {
        Self {
            button_report: byte & 0x01 != 0,
            surface_report: byte & 0x02 != 0,
        }
    }

    /// Encodes the switch byte with the padding bits zero.
    pub const fn to_byte(self) -> u8 {
        (self.button_report as u8) | ((self.surface_report as u8) << 1)
    }

    /// Decodes the switch byte at [`SELECTIVE_REPORT_BITS_OFFSET`] (the descriptor's layout, used by UsbUm
    /// and HidFilter). Returns `None` if `buf` is shorter than 2 bytes. Byte 0 is not checked.
    pub const fn from_report(buf: &[u8]) -> Option<Self> {
        match buf.first_chunk::<SELECTIVE_REPORT_LEN>() {
            Some(&[_, byte]) => Some(Self::from_byte(byte)),
            None => None,
        }
    }

    /// Decodes the switch byte at [`SELECTIVE_REPORT_DEVICE_MODE_BITS_OFFSET`] (the 3-byte C struct layout of
    /// UsbKm and SpiKm). Returns `None` if `buf` is shorter than 3 bytes. Bytes 0 and 1 are not checked.
    pub const fn from_device_mode_struct(buf: &[u8]) -> Option<Self> {
        match buf.first_chunk::<SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN>() {
            Some(&[_, _, byte]) => Some(Self::from_byte(byte)),
            None => None,
        }
    }

    /// Encodes feature report 0x06 in the descriptor's 2-byte layout: `[0x06, switch byte]`.
    pub const fn to_report(self) -> [u8; SELECTIVE_REPORT_LEN] {
        [REPORTID_FUNCSWITCH, self.to_byte()]
    }
}

// ---------------------------------------------------------------------------------------------------
// 0x09 user-mode app configuration
// ---------------------------------------------------------------------------------------------------

/// `sizeof(PTP_USERMODEAPP_CONF_REPORT)`: 4 bytes. The UMAPP TLC declares 3 × 8 bits after the ID.
pub const UMAPP_CONF_REPORT_LEN: usize = 4;

/// Feature report 0x09 (`PTP_USERMODEAPP_CONF_REPORT`), exchanged with the settings app:
/// `[0x09, PressureQualificationLevel, SingleContactSizeQualificationLevel,
/// MultipleContactSizeQualificationLevel]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UmAppConfReport {
    /// `PressureQualificationLevel` (byte 1).
    pub pressure_qualification_level: u8,
    /// `SingleContactSizeQualificationLevel` (byte 2).
    pub single_contact_size_qualification_level: u8,
    /// `MultipleContactSizeQualificationLevel` (byte 3).
    pub multiple_contact_size_qualification_level: u8,
}

impl UmAppConfReport {
    /// Encodes the report, ID included.
    pub const fn to_bytes(self) -> [u8; UMAPP_CONF_REPORT_LEN] {
        [
            REPORTID_UMAPP_CONF,
            self.pressure_qualification_level,
            self.single_contact_size_qualification_level,
            self.multiple_contact_size_qualification_level,
        ]
    }

    /// Decodes bytes 1..=3. Returns `None` if `buf` is shorter than 4 bytes. Byte 0 is not checked.
    pub const fn from_bytes(buf: &[u8]) -> Option<Self> {
        match buf.first_chunk::<UMAPP_CONF_REPORT_LEN>() {
            Some(&[_, pressure, single, multiple]) => Some(Self {
                pressure_qualification_level: pressure,
                single_contact_size_qualification_level: single,
                multiple_contact_size_qualification_level: multiple,
            }),
            None => None,
        }
    }

    /// Writes the 4 report bytes to the start of `buf` (UsbUm GET 0x09). Bytes after the fourth are left
    /// untouched. Returns [`BufferTooSmall`] (UsbUm: `STATUS_INVALID_BUFFER_SIZE`) and writes nothing if
    /// `buf` is shorter than 4 bytes.
    pub fn write_to(self, buf: &mut [u8]) -> Result<(), BufferTooSmall> {
        write_prefix(buf, &self.to_bytes())
    }
}

// C struct sizes measured by the C harness (crosscut spec A3; fixtures `sizeof` lines).
const _: () = assert!(DEVICE_CAPS_REPORT_LEN == 3);
const _: () = assert!(HQA_REPORT_LEN == 257);
const _: () = assert!(INPUT_MODE_REPORT_LEN == 2);
const _: () = assert!(SELECTIVE_REPORT_LEN == 2);
const _: () = assert!(SELECTIVE_REPORT_DEVICE_MODE_STRUCT_LEN == 3);
const _: () = assert!(UMAPP_CONF_REPORT_LEN == 4);

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
        let mut buf = [0xAAu8; 3];
        assert_eq!(DeviceCapsReport::PTP.write_to(&mut buf), Ok(()));
        assert_eq!(buf, [0x07, 0x05, 0x00]);
        let mut short = [0xAAu8; 2];
        assert_eq!(
            DeviceCapsReport::PTP.write_to(&mut short),
            Err(BufferTooSmall)
        );
        assert_eq!(short, [0xAA, 0xAA]);
        let mut long = [0xAAu8; 8];
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
    fn hqa_as_c_writes_two_bytes_only() {
        // HidFilter spec D.5: length 257 prefilled with AA -> `08 FC AA AA ... AA`.
        let mut buf = [0xAAu8; HQA_REPORT_LEN];
        assert_eq!(write_hqa_report_as_c(&mut buf), Ok(()));
        assert_eq!(buf[..2], [0x08, 0xFC]);
        assert!(buf[2..].iter().all(|&b| b == 0xAA));
        // Length 256 -> rejected, untouched.
        let mut short = [0xAAu8; HQA_REPORT_LEN - 1];
        assert_eq!(write_hqa_report_as_c(&mut short), Err(BufferTooSmall));
        assert!(short.iter().all(|&b| b == 0xAA));
        // A longer buffer is accepted; only the first two bytes change.
        let mut long = [0x55u8; 300];
        assert_eq!(write_hqa_report_as_c(&mut long), Ok(()));
        assert_eq!(long[..2], [0x08, 0xFC]);
        assert!(long[2..].iter().all(|&b| b == 0x55));
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
            assert_eq!(bits.button_report, byte & 1 == 1);
            assert_eq!(bits.surface_report, byte & 2 == 2);
            assert_eq!(bits.to_byte(), byte & 3);
        }
        // HidFilter spec D.5: `06 03` -> both on; `06 02` -> button 0, surface 1.
        assert_eq!(
            SelectiveReportBits::from_report(&[0x06, 0x03]),
            Some(SelectiveReportBits {
                button_report: true,
                surface_report: true
            })
        );
        assert_eq!(
            SelectiveReportBits::from_report(&[0x06, 0x02]),
            Some(SelectiveReportBits {
                button_report: false,
                surface_report: true
            })
        );
        assert_eq!(SelectiveReportBits::from_report(&[0x06]), None);
    }

    #[test]
    fn selective_device_mode_layout_reads_byte_two() {
        assert_eq!(
            SelectiveReportBits::from_device_mode_struct(&[0x06, 0x03, 0x01]),
            Some(SelectiveReportBits {
                button_report: true,
                surface_report: false
            })
        );
        // The descriptor's 2-byte report is too short for the 3-byte C struct.
        assert_eq!(
            SelectiveReportBits::from_device_mode_struct(&[0x06, 0x03]),
            None
        );
        assert_eq!(
            SELECTIVE_REPORT_DEVICE_MODE_BITS_OFFSET,
            SELECTIVE_REPORT_LEN
        );
    }

    #[test]
    fn selective_report_encode() {
        let bits = SelectiveReportBits {
            button_report: true,
            surface_report: false,
        };
        assert_eq!(bits.to_report(), [0x06, 0x01]);
        assert_eq!(SelectiveReportBits::default().to_report(), [0x06, 0x00]);
    }

    #[test]
    fn umapp_round_trip() {
        let r = UmAppConfReport {
            pressure_qualification_level: 0x11,
            single_contact_size_qualification_level: 0x22,
            multiple_contact_size_qualification_level: 0x33,
        };
        assert_eq!(r.to_bytes(), [0x09, 0x11, 0x22, 0x33]);
        assert_eq!(
            UmAppConfReport::from_bytes(&[0x09, 0x11, 0x22, 0x33]),
            Some(r)
        );
        assert_eq!(UmAppConfReport::from_bytes(&[0x09, 0x11, 0x22]), None);
        let mut buf = [0xAAu8; 6];
        assert_eq!(r.write_to(&mut buf), Ok(()));
        assert_eq!(buf, [0x09, 0x11, 0x22, 0x33, 0xAA, 0xAA]);
        let mut short = [0xAAu8; 3];
        assert_eq!(r.write_to(&mut short), Err(BufferTooSmall));
        assert_eq!(short, [0xAA; 3]);
    }
}

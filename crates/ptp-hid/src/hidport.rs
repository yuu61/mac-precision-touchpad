//! Byte codecs for the two `km/hidport.h` structures a HID minidriver returns: `HID_DESCRIPTOR` (9 bytes,
//! `pshpack1`) and `HID_DEVICE_ATTRIBUTES` (32 bytes), plus the `hidport.h` descriptor-type and string-ID
//! constants.
//!
//! wdk-sys generates `hidport.h` only for WDM/KMDF, not for UMDF (WDR#355), so `UsbUm` cannot get these
//! types from wdk-sys. Every driver can use the codecs here instead.

use crate::BufferTooSmall;
use crate::bytes::{put, take, write_prefix};
use crate::descriptor::{
    REPORT_DESCRIPTOR_SPI_UMAPP_LEN, REPORT_DESCRIPTOR_U32_ID_LEN,
    REPORT_DESCRIPTOR_U32_ID_UMAPP_LEN,
};

/// `HID_HID_DESCRIPTOR_TYPE` (0x21): `bDescriptorType` of the HID descriptor.
pub const HID_HID_DESCRIPTOR_TYPE: u8 = 0x21;
/// `HID_REPORT_DESCRIPTOR_TYPE` (0x22): `bReportType` of the descriptor-list entry for the report
/// descriptor.
pub const HID_REPORT_DESCRIPTOR_TYPE: u8 = 0x22;
/// `HID_PHYSICAL_DESCRIPTOR_TYPE` (0x23). No driver uses it.
pub const HID_PHYSICAL_DESCRIPTOR_TYPE: u8 = 0x23;

/// `HID_STRING_ID_IMANUFACTURER` (14): string ID of `IOCTL_HID_GET_STRING`. The ID is the low 16 bits of
/// the IOCTL's 32-bit input value; the high 16 bits are the language ID.
pub const HID_STRING_ID_IMANUFACTURER: u16 = 14;
/// `HID_STRING_ID_IPRODUCT` (15): string ID of `IOCTL_HID_GET_STRING`.
pub const HID_STRING_ID_IPRODUCT: u16 = 15;
/// `HID_STRING_ID_ISERIALNUMBER` (16): string ID of `IOCTL_HID_GET_STRING`.
pub const HID_STRING_ID_ISERIALNUMBER: u16 = 16;

// ---------------------------------------------------------------------------------------------------
// HID_DESCRIPTOR
// ---------------------------------------------------------------------------------------------------

/// `sizeof(HID_DESCRIPTOR)` with one descriptor-list entry: 9 bytes (`pshpack1`, so `wReportLength` is at
/// the unaligned offset 7).
pub const HID_DESCRIPTOR_LEN: usize = 9;
/// `bcdHID` of every driver's HID descriptor: HID 1.00, encoded `00 01`.
pub const HID_DESCRIPTOR_BCD_HID: u16 = 0x0100;

/// `HID_DESCRIPTOR` with exactly one `DescriptorList` entry, as all four drivers define it.
///
/// Wire layout: `bLength, bDescriptorType, bcdHID (u16 LE), bCountry, bNumDescriptors, bReportType,
/// wReportLength (u16 LE)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HidDescriptor {
    /// `bLength` (byte 0). 9 in every driver.
    pub b_length: u8,
    /// `bDescriptorType` (byte 1). [`HID_HID_DESCRIPTOR_TYPE`] in every driver.
    pub b_descriptor_type: u8,
    /// `bcdHID` (bytes 2-3). [`HID_DESCRIPTOR_BCD_HID`] in every driver.
    pub bcd_hid: u16,
    /// `bCountry` (byte 4). 0 in every driver.
    pub b_country: u8,
    /// `bNumDescriptors` (byte 5). 1 in every driver.
    pub b_num_descriptors: u8,
    /// `DescriptorList[0].bReportType` (byte 6). [`HID_REPORT_DESCRIPTOR_TYPE`] in every driver.
    pub b_report_type: u8,
    /// `DescriptorList[0].wReportLength` (bytes 7-8): the report descriptor length.
    pub w_report_length: u16,
}

impl HidDescriptor {
    /// The HID descriptor all four C drivers use, `{0x09, 0x21, 0x0100, 0x00, 0x01, {0x22, len}}`, for a
    /// report descriptor of `report_length` bytes.
    #[must_use]
    pub const fn new(report_length: u16) -> Self {
        Self {
            b_length: HID_DESCRIPTOR_LEN as u8, // 9, fits
            b_descriptor_type: HID_HID_DESCRIPTOR_TYPE,
            bcd_hid: HID_DESCRIPTOR_BCD_HID,
            b_country: 0,
            b_num_descriptors: 1,
            b_report_type: HID_REPORT_DESCRIPTOR_TYPE,
            w_report_length: report_length,
        }
    }

    /// Encodes the 9 wire bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; HID_DESCRIPTOR_LEN] {
        let [bcd0, bcd1] = self.bcd_hid.to_le_bytes();
        let [len0, len1] = self.w_report_length.to_le_bytes();
        [
            self.b_length,
            self.b_descriptor_type,
            bcd0,
            bcd1,
            self.b_country,
            self.b_num_descriptors,
            self.b_report_type,
            len0,
            len1,
        ]
    }

    /// Decodes the first 9 bytes of `buf`. Returns `None` if `buf` is shorter than 9 bytes. No field is
    /// validated.
    #[must_use]
    pub const fn from_bytes(buf: &[u8]) -> Option<Self> {
        match buf.first_chunk::<HID_DESCRIPTOR_LEN>() {
            Some(
                &[
                    b_length,
                    b_descriptor_type,
                    bcd0,
                    bcd1,
                    b_country,
                    b_num_descriptors,
                    b_report_type,
                    len0,
                    len1,
                ],
            ) => Some(Self {
                b_length,
                b_descriptor_type,
                bcd_hid: u16::from_le_bytes([bcd0, bcd1]),
                b_country,
                b_num_descriptors,
                b_report_type,
                w_report_length: u16::from_le_bytes([len0, len1]),
            }),
            None => None,
        }
    }
}

/// The 9-byte HID descriptor for a report descriptor of `report_length` bytes:
/// `09 21 00 01 00 01 22 lo hi`.
#[must_use]
pub const fn hid_descriptor(report_length: u16) -> [u8; HID_DESCRIPTOR_LEN] {
    HidDescriptor::new(report_length).to_bytes()
}

const _: () = assert!(REPORT_DESCRIPTOR_U32_ID_UMAPP_LEN <= u16::MAX as usize);
const _: () = assert!(REPORT_DESCRIPTOR_U32_ID_LEN <= u16::MAX as usize);
const _: () = assert!(REPORT_DESCRIPTOR_SPI_UMAPP_LEN <= u16::MAX as usize);

/// `UsbUm`'s HID descriptor (all 6 families, 516-byte report descriptor): `09 21 00 01 00 01 22 04 02`.
pub const HID_DESCRIPTOR_U32_ID_UMAPP: [u8; HID_DESCRIPTOR_LEN] =
    hid_descriptor(REPORT_DESCRIPTOR_U32_ID_UMAPP_LEN as u16); // 516 fits, asserted above
/// The HID descriptor of `UsbKm` and `HidFilter` (493-byte report descriptor): `09 21 00 01 00 01 22 ed 01`.
pub const HID_DESCRIPTOR_U32_ID: [u8; HID_DESCRIPTOR_LEN] =
    hid_descriptor(REPORT_DESCRIPTOR_U32_ID_LEN as u16); // 493 fits, asserted above
/// `SpiKm`'s HID descriptor (all 4 families, both variants, 501-byte report descriptor):
/// `09 21 00 01 00 01 22 f5 01`.
pub const HID_DESCRIPTOR_SPI_UMAPP: [u8; HID_DESCRIPTOR_LEN] =
    hid_descriptor(REPORT_DESCRIPTOR_SPI_UMAPP_LEN as u16); // 501 fits, asserted above

// ---------------------------------------------------------------------------------------------------
// HID_DEVICE_ATTRIBUTES
// ---------------------------------------------------------------------------------------------------

/// `sizeof(HID_DEVICE_ATTRIBUTES)`: 32 bytes (`ULONG Size; USHORT VendorID, ProductID, VersionNumber;
/// USHORT Reserved[11]`). This is also the value of the `Size` field.
pub const HID_DEVICE_ATTRIBUTES_LEN: usize = 32;
/// Number of leading bytes the C drivers write (`Size`, `VendorID`, `ProductID`, `VersionNumber`). They
/// never write `Reserved` (bytes 10..32).
pub const HID_DEVICE_ATTRIBUTES_WRITTEN_LEN: usize = 10;

/// `HID_DEVICE_ATTRIBUTES` without `Size` (always 32 when encoded) and `Reserved`.
///
/// Which values each driver reports is decided in its core crate (UsbUm/SpiKm/HidFilter report
/// [`crate::common::DEVICE_VID`]; `UsbKm` reports the USB idVendor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HidDeviceAttributes {
    /// `VendorID` (bytes 4-5).
    pub vendor_id: u16,
    /// `ProductID` (bytes 6-7).
    pub product_id: u16,
    /// `VersionNumber` (bytes 8-9).
    pub version_number: u16,
}

impl HidDeviceAttributes {
    /// The 10 bytes the C drivers write: `Size` = 32 (u32 LE), `VendorID`, `ProductID`, `VersionNumber`
    /// (u16 LE each).
    #[must_use]
    pub const fn header_bytes(self) -> [u8; HID_DEVICE_ATTRIBUTES_WRITTEN_LEN] {
        let [s0, s1, s2, s3] = (HID_DEVICE_ATTRIBUTES_LEN as u32).to_le_bytes(); // 32 fits
        let [v0, v1] = self.vendor_id.to_le_bytes();
        let [p0, p1] = self.product_id.to_le_bytes();
        let [n0, n1] = self.version_number.to_le_bytes();
        [s0, s1, s2, s3, v0, v1, p0, p1, n0, n1]
    }

    /// Encodes the whole 32-byte structure with `Reserved` zeroed.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; HID_DEVICE_ATTRIBUTES_LEN] {
        let mut out = [0_u8; HID_DEVICE_ATTRIBUTES_LEN];
        put(&mut out, &self.header_bytes());
        out
    }

    /// Writes the structure into the caller's buffer the way the C drivers do.
    ///
    /// Like `WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_DEVICE_ATTRIBUTES), ...)`, it requires
    /// `buf` to be at least 32 bytes; otherwise it writes nothing and returns [`BufferTooSmall`]. It then
    /// writes only bytes 0..10 and leaves `Reserved` (bytes 10..32) untouched, as the C code does. A core
    /// crate that wants zeroed `Reserved` bytes copies [`Self::to_bytes`] instead.
    ///
    /// # Errors
    /// Returns [`BufferTooSmall`] if the buffer cannot hold the report.
    pub fn write_to(self, buf: &mut [u8]) -> Result<(), BufferTooSmall> {
        if buf.len() < HID_DEVICE_ATTRIBUTES_LEN {
            return Err(BufferTooSmall);
        }
        write_prefix(buf, &self.header_bytes())
    }

    /// Decodes `VendorID`, `ProductID` and `VersionNumber` from a 32-byte structure, for example the
    /// answer to the `IOCTL_HID_GET_DEVICE_ATTRIBUTES` that `SpiKm` and `HidFilter` send down their stacks.
    /// Returns `None` if `buf` is shorter than 32 bytes. `Size` and `Reserved` are not checked (the C
    /// drivers do not check them either).
    #[must_use]
    pub const fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < HID_DEVICE_ATTRIBUTES_LEN {
            return None;
        }
        let Some((_size, rest)) = take::<4>(buf) else {
            return None;
        };
        let Some((vendor_id, rest)) = take::<2>(rest) else {
            return None;
        };
        let Some((product_id, rest)) = take::<2>(rest) else {
            return None;
        };
        let Some((version_number, _reserved)) = take::<2>(rest) else {
            return None;
        };
        Some(Self {
            vendor_id: u16::from_le_bytes(vendor_id),
            product_id: u16::from_le_bytes(product_id),
            version_number: u16::from_le_bytes(version_number),
        })
    }
}

// `sizeof(HID_DESCRIPTOR)` (pshpack1) and `sizeof(HID_DEVICE_ATTRIBUTES)`, verified with a C program
// against the 10.0.28000.0 WDK headers (windows-drivers-rs spec section 2).
const _: () = assert!(HID_DESCRIPTOR_LEN == 9);
const _: () = assert!(HID_DEVICE_ATTRIBUTES_LEN == 32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_descriptors_of_all_drivers() {
        // usbum.md 4.3, usbkm.md Appendix B, spikm.md 5.3, hidfilter.md A.4.
        assert_eq!(
            HID_DESCRIPTOR_U32_ID_UMAPP,
            [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0x04, 0x02]
        );
        assert_eq!(
            HID_DESCRIPTOR_U32_ID,
            [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xed, 0x01]
        );
        assert_eq!(
            HID_DESCRIPTOR_SPI_UMAPP,
            [0x09, 0x21, 0x00, 0x01, 0x00, 0x01, 0x22, 0xf5, 0x01]
        );
    }

    #[test]
    fn hid_descriptor_round_trip() {
        let d = HidDescriptor::new(0x1234);
        let bytes = d.to_bytes();
        assert_eq!(bytes, hid_descriptor(0x1234));
        assert_eq!(bytes[7..], [0x34, 0x12]);
        assert_eq!(HidDescriptor::from_bytes(&bytes), Some(d));
        assert_eq!(HidDescriptor::from_bytes(&bytes[..8]), None);
        let odd = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(
            HidDescriptor::from_bytes(&odd),
            Some(HidDescriptor {
                b_length: 1,
                b_descriptor_type: 2,
                bcd_hid: 0x0403,
                b_country: 5,
                b_num_descriptors: 6,
                b_report_type: 7,
                w_report_length: 0x0908,
            })
        );
    }

    #[test]
    fn attributes_bytes() {
        // hidfilter.md D.6: `20 00 00 00 10 89 65 02 00 01`, Reserved untouched.
        let a = HidDeviceAttributes {
            vendor_id: 0x8910,
            product_id: 0x0265,
            version_number: 0x0100,
        };
        assert_eq!(
            a.header_bytes(),
            [0x20, 0x00, 0x00, 0x00, 0x10, 0x89, 0x65, 0x02, 0x00, 0x01]
        );
        let full = a.to_bytes();
        assert_eq!(full[..10], a.header_bytes());
        assert!(full[10..].iter().all(|&b| b == 0));
        assert_eq!(HidDeviceAttributes::from_bytes(&full), Some(a));
    }

    #[test]
    fn attributes_write_to_leaves_reserved() {
        let a = HidDeviceAttributes {
            vendor_id: 0x05ac,
            product_id: 0x027a,
            version_number: 0x0001,
        };
        let mut buf = [0xAA_u8; HID_DEVICE_ATTRIBUTES_LEN];
        assert_eq!(a.write_to(&mut buf), Ok(()));
        assert_eq!(
            buf[..10],
            [0x20, 0, 0, 0, 0xac, 0x05, 0x7a, 0x02, 0x01, 0x00]
        );
        assert!(buf[10..].iter().all(|&b| b == 0xAA));
        let mut short = [0xAA_u8; HID_DEVICE_ATTRIBUTES_LEN - 1];
        assert_eq!(a.write_to(&mut short), Err(BufferTooSmall));
        assert!(short.iter().all(|&b| b == 0xAA));
    }

    #[test]
    fn attributes_decode_needs_full_struct() {
        let mut buf = [0_u8; HID_DEVICE_ATTRIBUTES_LEN];
        buf[4..10].copy_from_slice(&[0xac, 0x05, 0x78, 0x02, 0x34, 0x12]);
        assert_eq!(
            HidDeviceAttributes::from_bytes(&buf),
            Some(HidDeviceAttributes {
                vendor_id: 0x05ac,
                product_id: 0x0278,
                version_number: 0x1234
            })
        );
        assert_eq!(HidDeviceAttributes::from_bytes(&buf[..31]), None);
    }

    #[test]
    fn string_ids() {
        assert_eq!(
            (
                HID_STRING_ID_IMANUFACTURER,
                HID_STRING_ID_IPRODUCT,
                HID_STRING_ID_ISERIALNUMBER
            ),
            (14, 15, 16)
        );
    }
}

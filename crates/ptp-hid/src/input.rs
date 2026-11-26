//! Input report 0x05 (`PTP_REPORT`) in its two wire shapes.
//!
//! - [`PtpReport`], 50 bytes, with 9-byte [`PtpContact`]s (UsbUm, UsbKm, HidFilter):
//!   `[0]` 0x05; contact `i` at `1 + 9i`: flags (bit 0 Confidence, bit 1 Tip Switch, bits 2-7 padding),
//!   ContactID u32 LE, X u16 LE, Y u16 LE; `[46..48]` ScanTime u16 LE; `[48]` ContactCount;
//!   `[49]` IsButtonClicked.
//! - [`SpiPtpReport`], 30 bytes, with 5-byte [`SpiPtpContact`]s (SpiKm):
//!   `[0]` 0x05; contact `i` at `1 + 5i`: flags (bit 0 Confidence, bit 1 Tip Switch, bits 2-4 ContactID,
//!   bits 5-7 padding), X u16 LE, Y u16 LE; `[26..28]` ScanTime u16 LE; `[28]` ContactCount;
//!   `[29]` IsButtonClicked.
//!
//! The bit positions follow MSVC, which allocates bitfields from the least significant bit.
//!
//! The encoders never normalize a value. ContactCount and IsButtonClicked are written as raw bytes (SpiKm
//! copies them straight from the Apple packet, so they can be anything up to 255). Padding bits and unused
//! contact slots are always zero: the C drivers leave them as uninitialized stack bytes, and the port
//! zero-initializes them (policy P2).

use crate::bytes::{put, take};
use crate::common::{
    PTP_CONTACT_CONFIDENCE_BIT, PTP_CONTACT_SLOTS, PTP_CONTACT_TIPSWITCH_BIT, REPORTID_MULTITOUCH,
};

/// `sizeof(PTP_CONTACT)` in UsbUm, UsbKm and HidFilter: 9 bytes.
pub const PTP_CONTACT_LEN: usize = 9;
/// `sizeof(PTP_REPORT)` in UsbUm, UsbKm and HidFilter: 50 bytes.
pub const PTP_REPORT_LEN: usize = 1 + PTP_CONTACT_SLOTS * PTP_CONTACT_LEN + 2 + 1 + 1;
/// `sizeof(PTP_CONTACT)` in SpiKm: 5 bytes.
pub const SPI_PTP_CONTACT_LEN: usize = 5;
/// `sizeof(PTP_REPORT)` in SpiKm: 30 bytes.
pub const SPI_PTP_REPORT_LEN: usize = 1 + PTP_CONTACT_SLOTS * SPI_PTP_CONTACT_LEN + 2 + 1 + 1;

/// Offset of ScanTime in [`PtpReport`]'s bytes.
pub const PTP_REPORT_SCAN_TIME_OFFSET: usize = 1 + PTP_CONTACT_SLOTS * PTP_CONTACT_LEN;
/// Offset of ScanTime in [`SpiPtpReport`]'s bytes.
pub const SPI_PTP_REPORT_SCAN_TIME_OFFSET: usize = 1 + PTP_CONTACT_SLOTS * SPI_PTP_CONTACT_LEN;

const _: () = assert!(PTP_REPORT_LEN == 50);
const _: () = assert!(SPI_PTP_REPORT_LEN == 30);
const _: () = assert!(PTP_REPORT_SCAN_TIME_OFFSET == 46);
const _: () = assert!(SPI_PTP_REPORT_SCAN_TIME_OFFSET == 26);

/// Mask of the 3-bit SPI ContactID after shifting it down by [`SPI_CONTACT_ID_SHIFT`].
pub const SPI_CONTACT_ID_MASK: u8 = 0x07;
/// Bit position of the SPI ContactID in the contact's flag byte.
pub const SPI_CONTACT_ID_SHIFT: u32 = 2;

/// Flag byte with Confidence and Tip Switch, padding zero.
const fn flags(confidence: bool, tip_switch: bool) -> u8 {
    (if confidence {
        PTP_CONTACT_CONFIDENCE_BIT
    } else {
        0
    }) | (if tip_switch {
        PTP_CONTACT_TIPSWITCH_BIT
    } else {
        0
    })
}

// ---------------------------------------------------------------------------------------------------
// 9-byte contacts, 50-byte report
// ---------------------------------------------------------------------------------------------------

/// One 9-byte `PTP_CONTACT` (UsbUm, UsbKm, HidFilter).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PtpContact {
    /// `Confidence : 1` (bit 0 of byte 0).
    pub confidence: bool,
    /// `TipSwitch : 1` (bit 1 of byte 0).
    pub tip_switch: bool,
    /// `ContactID` (bytes 1-4, u32 LE).
    pub contact_id: u32,
    /// `X` (bytes 5-6, u16 LE).
    pub x: u16,
    /// `Y` (bytes 7-8, u16 LE).
    pub y: u16,
}

impl PtpContact {
    /// An all-zero contact (what an unused slot encodes to).
    pub const EMPTY: Self = Self {
        confidence: false,
        tip_switch: false,
        contact_id: 0,
        x: 0,
        y: 0,
    };

    /// Encodes the 9 contact bytes. The 6 padding bits are zero.
    pub const fn to_bytes(self) -> [u8; PTP_CONTACT_LEN] {
        let [i0, i1, i2, i3] = self.contact_id.to_le_bytes();
        let [x0, x1] = self.x.to_le_bytes();
        let [y0, y1] = self.y.to_le_bytes();
        [
            flags(self.confidence, self.tip_switch),
            i0,
            i1,
            i2,
            i3,
            x0,
            x1,
            y0,
            y1,
        ]
    }

    /// Decodes 9 contact bytes. The padding bits are ignored.
    pub const fn from_bytes(bytes: &[u8; PTP_CONTACT_LEN]) -> Self {
        let [f, i0, i1, i2, i3, x0, x1, y0, y1] = *bytes;
        Self {
            confidence: f & PTP_CONTACT_CONFIDENCE_BIT != 0,
            tip_switch: f & PTP_CONTACT_TIPSWITCH_BIT != 0,
            contact_id: u32::from_le_bytes([i0, i1, i2, i3]),
            x: u16::from_le_bytes([x0, x1]),
            y: u16::from_le_bytes([y0, y1]),
        }
    }
}

/// Input report 0x05 with 9-byte contacts (`PTP_REPORT` of UsbUm, UsbKm and HidFilter), 50 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PtpReport {
    /// `Contacts[5]`. Slots at or after `contact_count` should be [`PtpContact::EMPTY`].
    pub contacts: [PtpContact; PTP_CONTACT_SLOTS],
    /// `ScanTime` (u16 LE), in 100 µs units according to the descriptor.
    pub scan_time: u16,
    /// `ContactCount`, raw.
    pub contact_count: u8,
    /// `IsButtonClicked`, raw. The descriptor declares bit 0 as Button 1 and bits 1-7 as padding.
    pub is_button_clicked: u8,
}

impl PtpReport {
    /// A report with no contacts, ScanTime 0, ContactCount 0 and no button. It encodes to `05` followed by
    /// 49 zero bytes.
    pub const EMPTY: Self = Self {
        contacts: [PtpContact::EMPTY; PTP_CONTACT_SLOTS],
        scan_time: 0,
        contact_count: 0,
        is_button_clicked: 0,
    };

    /// Encodes the 50 report bytes, report ID 0x05 included.
    pub const fn to_bytes(&self) -> [u8; PTP_REPORT_LEN] {
        let [c0, c1, c2, c3, c4] = self.contacts;
        let mut out = [0u8; PTP_REPORT_LEN];
        let rest = put(&mut out, &[REPORTID_MULTITOUCH]);
        let rest = put(rest, &c0.to_bytes());
        let rest = put(rest, &c1.to_bytes());
        let rest = put(rest, &c2.to_bytes());
        let rest = put(rest, &c3.to_bytes());
        let rest = put(rest, &c4.to_bytes());
        let rest = put(rest, &self.scan_time.to_le_bytes());
        let _ = put(rest, &[self.contact_count, self.is_button_clicked]);
        out
    }

    /// Decodes the first 50 bytes of `buf`. Returns `None` if `buf` is shorter than 50 bytes or byte 0 is
    /// not 0x05.
    pub const fn from_bytes(buf: &[u8]) -> Option<Self> {
        let Some(([REPORTID_MULTITOUCH], rest)) = take::<1>(buf) else {
            return None;
        };
        let Some((c0, rest)) = take::<PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some((c1, rest)) = take::<PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some((c2, rest)) = take::<PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some((c3, rest)) = take::<PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some((c4, rest)) = take::<PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some(([s0, s1, contact_count, is_button_clicked], _)) = take::<4>(rest) else {
            return None;
        };
        Some(Self {
            contacts: [
                PtpContact::from_bytes(&c0),
                PtpContact::from_bytes(&c1),
                PtpContact::from_bytes(&c2),
                PtpContact::from_bytes(&c3),
                PtpContact::from_bytes(&c4),
            ],
            scan_time: u16::from_le_bytes([s0, s1]),
            contact_count,
            is_button_clicked,
        })
    }
}

// ---------------------------------------------------------------------------------------------------
// SPI 5-byte contacts, 30-byte report
// ---------------------------------------------------------------------------------------------------

/// One 5-byte SpiKm `PTP_CONTACT`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpiPtpContact {
    /// `Confidence : 1` (bit 0 of byte 0).
    pub confidence: bool,
    /// `TipSwitch : 1` (bit 1 of byte 0).
    pub tip_switch: bool,
    /// `ContactID : 3` (bits 2-4 of byte 0). Only the low 3 bits are encoded, as assigning to the C
    /// bitfield truncates. The descriptor declares a logical maximum of 3, but SpiKm emits 0..=4
    /// (preserved known issue).
    pub contact_id: u8,
    /// `X` (bytes 1-2, u16 LE).
    pub x: u16,
    /// `Y` (bytes 3-4, u16 LE).
    pub y: u16,
}

impl SpiPtpContact {
    /// An all-zero contact (what an unused slot encodes to).
    pub const EMPTY: Self = Self {
        confidence: false,
        tip_switch: false,
        contact_id: 0,
        x: 0,
        y: 0,
    };

    /// Encodes the 5 contact bytes. ContactID is truncated to 3 bits; the padding bits are zero.
    pub const fn to_bytes(self) -> [u8; SPI_PTP_CONTACT_LEN] {
        let [x0, x1] = self.x.to_le_bytes();
        let [y0, y1] = self.y.to_le_bytes();
        let f = flags(self.confidence, self.tip_switch)
            | ((self.contact_id & SPI_CONTACT_ID_MASK) << SPI_CONTACT_ID_SHIFT);
        [f, x0, x1, y0, y1]
    }

    /// Decodes 5 contact bytes. The padding bits are ignored.
    pub const fn from_bytes(bytes: &[u8; SPI_PTP_CONTACT_LEN]) -> Self {
        let [f, x0, x1, y0, y1] = *bytes;
        Self {
            confidence: f & PTP_CONTACT_CONFIDENCE_BIT != 0,
            tip_switch: f & PTP_CONTACT_TIPSWITCH_BIT != 0,
            contact_id: (f >> SPI_CONTACT_ID_SHIFT) & SPI_CONTACT_ID_MASK,
            x: u16::from_le_bytes([x0, x1]),
            y: u16::from_le_bytes([y0, y1]),
        }
    }
}

/// Input report 0x05 with SPI 5-byte contacts (SpiKm's `PTP_REPORT`), 30 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpiPtpReport {
    /// `Contacts[5]`. Slots at or after the number of filled contacts should be [`SpiPtpContact::EMPTY`].
    pub contacts: [SpiPtpContact; PTP_CONTACT_SLOTS],
    /// `ScanTime` (u16 LE).
    pub scan_time: u16,
    /// `ContactCount`, raw (SpiKm does not clamp it to 5).
    pub contact_count: u8,
    /// `IsButtonClicked`, raw (SpiKm copies the whole Apple `ClickOccurred` byte).
    pub is_button_clicked: u8,
}

impl SpiPtpReport {
    /// A report with no contacts, ScanTime 0, ContactCount 0 and no button. It encodes to `05` followed by
    /// 29 zero bytes.
    pub const EMPTY: Self = Self {
        contacts: [SpiPtpContact::EMPTY; PTP_CONTACT_SLOTS],
        scan_time: 0,
        contact_count: 0,
        is_button_clicked: 0,
    };

    /// Encodes the 30 report bytes, report ID 0x05 included.
    pub const fn to_bytes(&self) -> [u8; SPI_PTP_REPORT_LEN] {
        let [c0, c1, c2, c3, c4] = self.contacts;
        let mut out = [0u8; SPI_PTP_REPORT_LEN];
        let rest = put(&mut out, &[REPORTID_MULTITOUCH]);
        let rest = put(rest, &c0.to_bytes());
        let rest = put(rest, &c1.to_bytes());
        let rest = put(rest, &c2.to_bytes());
        let rest = put(rest, &c3.to_bytes());
        let rest = put(rest, &c4.to_bytes());
        let rest = put(rest, &self.scan_time.to_le_bytes());
        let _ = put(rest, &[self.contact_count, self.is_button_clicked]);
        out
    }

    /// Decodes the first 30 bytes of `buf`. Returns `None` if `buf` is shorter than 30 bytes or byte 0 is
    /// not 0x05.
    pub const fn from_bytes(buf: &[u8]) -> Option<Self> {
        let Some(([REPORTID_MULTITOUCH], rest)) = take::<1>(buf) else {
            return None;
        };
        let Some((c0, rest)) = take::<SPI_PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some((c1, rest)) = take::<SPI_PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some((c2, rest)) = take::<SPI_PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some((c3, rest)) = take::<SPI_PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some((c4, rest)) = take::<SPI_PTP_CONTACT_LEN>(rest) else {
            return None;
        };
        let Some(([s0, s1, contact_count, is_button_clicked], _)) = take::<4>(rest) else {
            return None;
        };
        Some(Self {
            contacts: [
                SpiPtpContact::from_bytes(&c0),
                SpiPtpContact::from_bytes(&c1),
                SpiPtpContact::from_bytes(&c2),
                SpiPtpContact::from_bytes(&c3),
                SpiPtpContact::from_bytes(&c4),
            ],
            scan_time: u16::from_le_bytes([s0, s1]),
            contact_count,
            is_button_clicked,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> [u8; PTP_REPORT_LEN] {
        let v: [u8; PTP_REPORT_LEN] = core::array::from_fn(|i| {
            let tok = s.split_whitespace().nth(i).expect("too few bytes");
            u8::from_str_radix(tok, 16).expect("bad hex")
        });
        assert_eq!(s.split_whitespace().count(), PTP_REPORT_LEN);
        v
    }

    #[test]
    fn empty_reports() {
        let b = PtpReport::EMPTY.to_bytes();
        assert_eq!(b[0], 0x05);
        assert!(b[1..].iter().all(|&x| x == 0));
        let b = SpiPtpReport::EMPTY.to_bytes();
        assert_eq!(b[0], 0x05);
        assert!(b[1..].iter().all(|&x| x == 0));
        assert_eq!(PtpReport::default(), PtpReport::EMPTY);
        assert_eq!(SpiPtpReport::default(), SpiPtpReport::EMPTY);
    }

    #[test]
    fn usbkm_vector_a_header_only() {
        // usbkm.md 7.4 A: button only.
        let r = PtpReport {
            is_button_clicked: 1,
            ..PtpReport::EMPTY
        };
        let mut expected = [0u8; PTP_REPORT_LEN];
        expected[0] = 0x05;
        expected[49] = 0x01;
        assert_eq!(r.to_bytes(), expected);
    }

    #[test]
    fn usbkm_vector_b_two_fingers() {
        // usbkm.md 7.4 B (fallback config) and B' (13-inch config).
        let mut r = PtpReport {
            contact_count: 2,
            ..PtpReport::EMPTY
        };
        r.contacts[0] = PtpContact {
            confidence: true,
            tip_switch: true,
            contact_id: 0,
            x: 9500,
            y: 7000,
        };
        r.contacts[1] = PtpContact {
            contact_id: 1,
            ..PtpContact::EMPTY
        };
        assert_eq!(
            r.to_bytes(),
            hex(
                "05 03 00 00 00 00 1c 25 58 1b 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 \
                 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02 00"
            )
        );
        r.contacts[0].x = 5743;
        r.contacts[0].y = 4685;
        assert_eq!(
            r.to_bytes(),
            hex(
                "05 03 00 00 00 00 6f 16 4d 12 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 \
                 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02 00"
            )
        );
        assert_eq!(PtpReport::from_bytes(&r.to_bytes()), Some(r));
    }

    #[test]
    fn usbum_ws9_frame_vector() {
        // usbum.md 7.6 frame-level vector (ScanTime forced to 0xFF there).
        let mut r = PtpReport {
            scan_time: 0xFF,
            contact_count: 2,
            is_button_clicked: 1,
            ..PtpReport::EMPTY
        };
        r.contacts[0] = PtpContact {
            confidence: true,
            tip_switch: true,
            contact_id: 0,
            x: 4928,
            y: 6603,
        };
        r.contacts[1] = PtpContact {
            contact_id: 1,
            ..PtpContact::EMPTY
        };
        assert_eq!(
            r.to_bytes(),
            hex(
                "05 03 00 00 00 00 40 13 cb 19 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00 \
                 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 ff 00 02 01"
            )
        );
    }

    #[test]
    fn hidfilter_d2_vector() {
        // hidfilter.md D.2: five contacts, ScanTime 0, button 1.
        let xy = [
            (3678, 2479),
            (3778, 2428),
            (3878, 2378),
            (3978, 2328),
            (4078, 2278),
        ];
        let mut r = PtpReport {
            contact_count: 5,
            is_button_clicked: 1,
            ..PtpReport::EMPTY
        };
        for (i, (c, (x, y))) in r.contacts.iter_mut().zip(xy).enumerate() {
            *c = PtpContact {
                confidence: true,
                tip_switch: true,
                contact_id: i as u32,
                x,
                y,
            };
        }
        assert_eq!(
            r.to_bytes(),
            hex("05 \
                 03 00 00 00 00 5e 0e af 09 \
                 03 01 00 00 00 c2 0e 7c 09 \
                 03 02 00 00 00 26 0f 4a 09 \
                 03 03 00 00 00 8a 0f 18 09 \
                 03 04 00 00 00 ee 0f e6 08 \
                 00 00 05 01")
        );
    }

    #[test]
    fn contact_flags_and_decoding() {
        let c = PtpContact {
            confidence: true,
            tip_switch: false,
            contact_id: 0xDEAD_BEEF,
            x: 0x1234,
            y: 0xABCD,
        };
        assert_eq!(
            c.to_bytes(),
            [0x01, 0xEF, 0xBE, 0xAD, 0xDE, 0x34, 0x12, 0xCD, 0xAB]
        );
        assert_eq!(PtpContact::from_bytes(&c.to_bytes()), c);
        // Padding bits are ignored on decode.
        let mut b = c.to_bytes();
        b[0] |= 0xFC;
        assert_eq!(PtpContact::from_bytes(&b), c);
        let tip = PtpContact {
            tip_switch: true,
            ..PtpContact::EMPTY
        };
        assert_eq!(tip.to_bytes()[0], 0x02);
    }

    #[test]
    fn raw_count_and_button_are_not_normalized() {
        let r = PtpReport {
            contact_count: 0xFF,
            is_button_clicked: 0x81,
            scan_time: 0xBEEF,
            ..PtpReport::EMPTY
        };
        let b = r.to_bytes();
        assert_eq!(b[46..], [0xEF, 0xBE, 0xFF, 0x81]);
        assert_eq!(PtpReport::from_bytes(&b), Some(r));
        let s = SpiPtpReport {
            contact_count: 9,
            is_button_clicked: 0xFE,
            scan_time: 0x0102,
            ..SpiPtpReport::EMPTY
        };
        let b = s.to_bytes();
        assert_eq!(b[26..], [0x02, 0x01, 0x09, 0xFE]);
        assert_eq!(SpiPtpReport::from_bytes(&b), Some(s));
    }

    #[test]
    fn decode_rejects_short_or_wrong_id() {
        let b = PtpReport::EMPTY.to_bytes();
        assert_eq!(PtpReport::from_bytes(&b[..49]), None);
        let mut wrong = b;
        wrong[0] = 0x06;
        assert_eq!(PtpReport::from_bytes(&wrong), None);
        assert_eq!(PtpReport::from_bytes(&[]), None);
        // Longer buffers are accepted; only the first 50 bytes are read.
        let mut long = [0u8; 64];
        long[..50].copy_from_slice(&b);
        long[50] = 0xFF;
        assert_eq!(PtpReport::from_bytes(&long), Some(PtpReport::EMPTY));
        let s = SpiPtpReport::EMPTY.to_bytes();
        assert_eq!(SpiPtpReport::from_bytes(&s[..29]), None);
        let mut wrong = s;
        wrong[0] = 0x00;
        assert_eq!(SpiPtpReport::from_bytes(&wrong), None);
    }

    #[test]
    fn spi_contact_layout() {
        // spikm.md 7.4: Family 2 example, X=4850, Y=4730, tip, confidence, ID 0 -> `03 F2 12 7A 12`.
        let c = SpiPtpContact {
            confidence: true,
            tip_switch: true,
            contact_id: 0,
            x: 4850,
            y: 4730,
        };
        assert_eq!(c.to_bytes(), [0x03, 0xF2, 0x12, 0x7A, 0x12]);
        // Verification addendum: conf + tip + ID 4 = 0x13.
        let c4 = SpiPtpContact { contact_id: 4, ..c };
        assert_eq!(c4.to_bytes()[0], 0x13);
        assert_eq!(SpiPtpContact::from_bytes(&c4.to_bytes()), c4);
        // ID truncated to 3 bits like the C bitfield; padding bits stay zero.
        let wide = SpiPtpContact {
            contact_id: 0xFD,
            ..SpiPtpContact::EMPTY
        };
        assert_eq!(wide.to_bytes()[0], 0x05 << 2);
        assert_eq!(SpiPtpContact::from_bytes(&wide.to_bytes()).contact_id, 5);
        // Padding bits ignored on decode.
        assert_eq!(
            SpiPtpContact::from_bytes(&[0xE3, 0, 0, 0, 0]),
            SpiPtpContact {
                confidence: true,
                tip_switch: true,
                ..SpiPtpContact::EMPTY
            }
        );
    }

    #[test]
    fn spi_report_layout() {
        let mut r = SpiPtpReport {
            scan_time: 0,
            contact_count: 2,
            is_button_clicked: 1,
            ..SpiPtpReport::EMPTY
        };
        for (i, c) in r.contacts.iter_mut().take(2).enumerate() {
            *c = SpiPtpContact {
                confidence: true,
                tip_switch: true,
                contact_id: i as u8,
                x: 0x0102,
                y: 0x0304,
            };
        }
        let b = r.to_bytes();
        assert_eq!(
            b,
            [
                0x05, //
                0x03, 0x02, 0x01, 0x04, 0x03, //
                0x07, 0x02, 0x01, 0x04, 0x03, //
                0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, //
                0, 0, 0, 0, 0, //
                0x00, 0x00, 0x02, 0x01,
            ]
        );
        assert_eq!(SpiPtpReport::from_bytes(&b), Some(r));
    }
}

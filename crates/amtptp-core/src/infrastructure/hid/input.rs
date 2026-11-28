//! Input report 0x05 (`PTP_REPORT`) in its two wire shapes.
//!
//! - [`PtpReport`], 50 bytes, with 9-byte [`PtpContact`]s (`UsbUm`, `UsbKm`, HidFilter):
//!   `[0]` 0x05; contact `i` at `1 + 9i`: flags (bit 0 Confidence, bit 1 Tip Switch, bits 2-7 padding),
//!   `ContactID` u32 LE, X u16 LE, Y u16 LE; `[46..48]` `ScanTime` u16 LE; `[48]` `ContactCount`;
//!   `[49]` `IsButtonClicked`.
//! - [`SpiPtpReport`], 30 bytes, with 5-byte [`SpiPtpContact`]s (SpiKm):
//!   `[0]` 0x05; contact `i` at `1 + 5i`: flags (bit 0 Confidence, bit 1 Tip Switch, bits 2-4 `ContactID`,
//!   bits 5-7 padding), X u16 LE, Y u16 LE; `[26..28]` `ScanTime` u16 LE; `[28]` `ContactCount`;
//!   `[29]` `IsButtonClicked`.
//!
//! The bit positions follow MSVC, which allocates bitfields from the least significant bit.
//!
//! The encoders never normalize a value. `ContactCount` and `IsButtonClicked` are written as raw bytes; the
//! drivers bound `ContactCount` to the five slots, but `SpiKm` copies `IsButtonClicked` straight from the
//! Apple packet, so it can be anything up to 255. Padding bits and unused
//! contact slots are always zero: the C drivers leave them as uninitialized stack bytes, and the port
//! zero-initializes them (policy P2).

mod ptp;
mod spi;

pub use ptp::{PtpContact, PtpReport};
pub use spi::{SpiPtpContact, SpiPtpReport};

use crate::hid::common::{
    PTP_CONTACT_CONFIDENCE_BIT, PTP_CONTACT_SLOTS, PTP_CONTACT_TIPSWITCH_BIT,
};

/// `sizeof(PTP_CONTACT)` in `UsbUm`, `UsbKm` and `HidFilter`: 9 bytes.
pub const PTP_CONTACT_LEN: usize = 9;
/// `sizeof(PTP_REPORT)` in `UsbUm`, `UsbKm` and `HidFilter`: 50 bytes.
pub const PTP_REPORT_LEN: usize = 1 + PTP_CONTACT_SLOTS * PTP_CONTACT_LEN + 2 + 1 + 1;
/// `sizeof(PTP_CONTACT)` in `SpiKm`: 5 bytes.
pub const SPI_PTP_CONTACT_LEN: usize = 5;
/// `sizeof(PTP_REPORT)` in `SpiKm`: 30 bytes.
pub const SPI_PTP_REPORT_LEN: usize = 1 + PTP_CONTACT_SLOTS * SPI_PTP_CONTACT_LEN + 2 + 1 + 1;

/// Offset of `ScanTime` in [`PtpReport`]'s bytes.
pub const PTP_REPORT_SCAN_TIME_OFFSET: usize = 1 + PTP_CONTACT_SLOTS * PTP_CONTACT_LEN;
/// Offset of `ScanTime` in [`SpiPtpReport`]'s bytes.
pub const SPI_PTP_REPORT_SCAN_TIME_OFFSET: usize = 1 + PTP_CONTACT_SLOTS * SPI_PTP_CONTACT_LEN;

const _: () = assert!(PTP_REPORT_LEN == 50);
const _: () = assert!(SPI_PTP_REPORT_LEN == 30);
const _: () = assert!(PTP_REPORT_SCAN_TIME_OFFSET == 46);
const _: () = assert!(SPI_PTP_REPORT_SCAN_TIME_OFFSET == 26);

/// Mask of the 3-bit SPI `ContactID` after shifting it down by [`SPI_CONTACT_ID_SHIFT`].
pub const SPI_CONTACT_ID_MASK: u8 = 0x07;
/// Bit position of the SPI `ContactID` in the contact's flag byte.
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
        let mut expected = [0_u8; PTP_REPORT_LEN];
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
        let mut long = [0_u8; 64];
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

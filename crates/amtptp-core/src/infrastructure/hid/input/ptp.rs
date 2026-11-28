//! 50-byte input reports with 32-bit contact IDs.

use super::{PTP_CONTACT_LEN, PTP_REPORT_LEN, flags};
use crate::application::InputFrame;
use crate::domain::Contact;
use crate::hid::bytes::{put, take};
use crate::hid::common::{
    PTP_CONTACT_CONFIDENCE_BIT, PTP_CONTACT_SLOTS, PTP_CONTACT_TIPSWITCH_BIT, REPORTID_MULTITOUCH,
};

/// One 9-byte `PTP_CONTACT` (`UsbUm`, `UsbKm`, `HidFilter`).
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
    #[must_use]
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
    #[must_use]
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

impl From<Contact> for PtpContact {
    fn from(contact: Contact) -> Self {
        Self {
            confidence: contact.confidence,
            tip_switch: contact.tip_switch,
            contact_id: contact.contact_id,
            x: contact.x,
            y: contact.y,
        }
    }
}

/// Input report 0x05 with 9-byte contacts (`PTP_REPORT` of `UsbUm`, `UsbKm` and `HidFilter`), 50 bytes.
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
    /// Maps an application frame into the 50-byte PTP report format.
    #[must_use]
    pub fn from_frame(frame: &InputFrame, scan_time: u16) -> Self {
        Self {
            contacts: frame.contacts.contacts().map(PtpContact::from),
            contact_count: frame.contacts.count(),
            scan_time,
            is_button_clicked: frame.button,
        }
    }

    /// A report with no contacts, `ScanTime` 0, `ContactCount` 0 and no button. It encodes to `05` followed by
    /// 49 zero bytes.
    pub const EMPTY: Self = Self {
        contacts: [PtpContact::EMPTY; PTP_CONTACT_SLOTS],
        scan_time: 0,
        contact_count: 0,
        is_button_clicked: 0,
    };

    /// Encodes the 50 report bytes, report ID 0x05 included.
    #[must_use]
    pub const fn to_bytes(&self) -> [u8; PTP_REPORT_LEN] {
        let [c0, c1, c2, c3, c4] = self.contacts;
        let mut out = [0_u8; PTP_REPORT_LEN];
        let rest = put(&mut out, &[REPORTID_MULTITOUCH]);
        let rest = put(rest, &c0.to_bytes());
        let rest = put(rest, &c1.to_bytes());
        let rest = put(rest, &c2.to_bytes());
        let rest = put(rest, &c3.to_bytes());
        let rest = put(rest, &c4.to_bytes());
        let rest = put(rest, &self.scan_time.to_le_bytes());
        put(rest, &[self.contact_count, self.is_button_clicked]);
        out
    }

    /// Decodes the first 50 bytes of `buf`. Returns `None` if `buf` is shorter than 50 bytes or byte 0 is
    /// not 0x05.
    #[must_use]
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

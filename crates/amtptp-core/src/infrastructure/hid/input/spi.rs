//! 30-byte SPI input reports with 3-bit contact IDs.

use super::{
    SPI_CONTACT_ID_MASK, SPI_CONTACT_ID_SHIFT, SPI_PTP_CONTACT_LEN, SPI_PTP_REPORT_LEN, flags,
};
use crate::application::InputFrame;
use crate::domain::Contact;
use crate::hid::bytes::{put, take};
use crate::hid::common::{
    PTP_CONTACT_CONFIDENCE_BIT, PTP_CONTACT_SLOTS, PTP_CONTACT_TIPSWITCH_BIT, REPORTID_MULTITOUCH,
};

/// One 5-byte `SpiKm` `PTP_CONTACT`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpiPtpContact {
    /// `Confidence : 1` (bit 0 of byte 0).
    pub confidence: bool,
    /// `TipSwitch : 1` (bit 1 of byte 0).
    pub tip_switch: bool,
    /// `ContactID : 3` (bits 2-4 of byte 0). Only the low 3 bits are encoded (`0..=4` matches
    /// [`crate::hid::descriptor::SPI_CONTACT_ID_LOGICAL_MAX`]).
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

    /// Encodes the 5 contact bytes. `ContactID` is truncated to 3 bits; the padding bits are zero.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; SPI_PTP_CONTACT_LEN] {
        let [x0, x1] = self.x.to_le_bytes();
        let [y0, y1] = self.y.to_le_bytes();
        let f = flags(self.confidence, self.tip_switch)
            | ((self.contact_id & SPI_CONTACT_ID_MASK) << SPI_CONTACT_ID_SHIFT);
        [f, x0, x1, y0, y1]
    }

    /// Decodes 5 contact bytes. The padding bits are ignored.
    #[must_use]
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

impl From<Contact> for SpiPtpContact {
    fn from(contact: Contact) -> Self {
        Self {
            confidence: contact.confidence,
            tip_switch: contact.tip_switch,
            contact_id: (contact.contact_id & u32::from(SPI_CONTACT_ID_MASK)) as u8,
            x: contact.x,
            y: contact.y,
        }
    }
}

/// Input report 0x05 with SPI 5-byte contacts (`SpiKm`'s `PTP_REPORT`), 30 bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpiPtpReport {
    /// `Contacts[5]`. Slots at or after the number of filled contacts should be [`SpiPtpContact::EMPTY`].
    pub contacts: [SpiPtpContact; PTP_CONTACT_SLOTS],
    /// `ScanTime` (u16 LE).
    pub scan_time: u16,
    /// `ContactCount`, raw (`SpiKm` passes the tracked count, at most 5).
    pub contact_count: u8,
    /// `IsButtonClicked`, raw (`SpiKm` copies the whole Apple `ClickOccurred` byte).
    pub is_button_clicked: u8,
}

impl SpiPtpReport {
    /// Maps an application frame into the 30-byte PTP report format.
    #[must_use]
    pub fn from_frame(frame: &InputFrame, scan_time: u16) -> Self {
        Self {
            contacts: frame.contacts.contacts().map(SpiPtpContact::from),
            contact_count: frame.contacts.count(),
            scan_time,
            is_button_clicked: frame.button,
        }
    }

    /// A report with no contacts, `ScanTime` 0, `ContactCount` 0 and no button. It encodes to `05` followed by
    /// 29 zero bytes.
    pub const EMPTY: Self = Self {
        contacts: [SpiPtpContact::EMPTY; PTP_CONTACT_SLOTS],
        scan_time: 0,
        contact_count: 0,
        is_button_clicked: 0,
    };

    /// Encodes the 30 report bytes, report ID 0x05 included.
    #[must_use]
    pub const fn to_bytes(&self) -> [u8; SPI_PTP_REPORT_LEN] {
        let [c0, c1, c2, c3, c4] = self.contacts;
        let mut out = [0_u8; SPI_PTP_REPORT_LEN];
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

    /// Decodes the first 30 bytes of `buf`. Returns `None` if `buf` is shorter than 30 bytes or byte 0 is
    /// not 0x05.
    #[must_use]
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

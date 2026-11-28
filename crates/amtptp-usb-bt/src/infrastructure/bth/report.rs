//! Padding contract for reports crossing the Bluetooth HID stack.

use super::MULTITOUCH_REPORT_ID;

/// Largest raw `0x31` report accepted, ID included: a 4-byte header and 15 nine-byte fingers (the
/// limit Linux `hid-magicmouse` enforces).
pub const RAW_REPORT_MAX_LEN: usize = 4 + 9 * 15;
/// Length of a padded `0x31` report, ID included: the raw report, zeros, then one length byte.
pub const PADDED_REPORT_LEN: usize = RAW_REPORT_MAX_LEN + 1;
/// HIDP header of an input report on the interrupt channel (`DATA | Input`).
pub const HIDP_DATA_INPUT: u8 = 0xA1;
/// Length of a padded interrupt payload: the HIDP header and the padded report. `HidBth` reads each
/// interrupt transfer into a buffer of the upper read's size plus one, so a HIDCLASS read of
/// [`PADDED_REPORT_LEN`] bytes gives exactly this capacity.
pub const PADDED_PAYLOAD_LEN: usize = 1 + PADDED_REPORT_LEN;

/// Report ID of the mouse-mode report (Bluetooth). The trackpad sends it until it is switched to
/// multitouch mode, for example after its power switch restarted it.
pub const MOUSE_REPORT_ID: u8 = 0x02;

/// What a padded report read from the vendor collection holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaddedReport<'a> {
    /// A raw `0x31` multitouch report.
    Multitouch(&'a [u8]),
    /// The trackpad sent a report of this ID instead of a multitouch report: it is not in
    /// multitouch mode (the reader sends the mode switch again).
    ModeLost(u8),
}

/// Pads an interrupt payload in place, so HIDCLASS delivers it on the vendor collection.
///
/// `payload` is the whole transfer buffer and `len` the bytes received: the HIDP header
/// [`HIDP_DATA_INPUT`], then the raw report.
///
/// - A `0x31` report of up to [`RAW_REPORT_MAX_LEN`] bytes keeps its bytes, gets zeros up to
///   [`PADDED_REPORT_LEN`] report bytes, and its length in the last byte.
/// - A mouse-mode report ([`MOUSE_REPORT_ID`]) becomes a mode-lost marker: report ID `0x31`, the
///   original ID in the next byte, zeros, and length 0 in the last byte.
///
/// Returns the new payload length, [`PADDED_PAYLOAD_LEN`]; `None` (buffer untouched) for any other
/// payload, a `0x31` report that is too long, or a buffer that is not exactly
/// [`PADDED_PAYLOAD_LEN`] long.
#[must_use]
pub fn pad_interrupt_payload(payload: &mut [u8], len: usize) -> Option<usize> {
    if payload.len() != PADDED_PAYLOAD_LEN || len > payload.len() || len < 2 {
        return None;
    }
    if payload.first() != Some(&HIDP_DATA_INPUT) {
        return None;
    }
    let raw_len = len - 1;
    let (trailer, body) = payload.split_last_mut()?;
    match body.get(1).copied()? {
        MULTITOUCH_REPORT_ID if raw_len <= RAW_REPORT_MAX_LEN => {
            body.get_mut(len..)?.fill(0);
            *trailer = u8::try_from(raw_len).ok()?;
        }
        MOUSE_REPORT_ID => {
            let marker = body.get_mut(1..)?;
            marker.fill(0);
            let [id, original, ..] = marker else {
                return None;
            };
            *id = MULTITOUCH_REPORT_ID;
            *original = MOUSE_REPORT_ID;
            *trailer = 0;
        }
        _ => return None,
    }
    Some(PADDED_PAYLOAD_LEN)
}

/// Decodes a padded report read from the vendor collection; `None` if `report` is not a
/// well-formed padded report.
#[must_use]
pub fn unpad_report(report: &[u8]) -> Option<PaddedReport<'_>> {
    if report.len() != PADDED_REPORT_LEN || report.first() != Some(&MULTITOUCH_REPORT_ID) {
        return None;
    }
    match usize::from(*report.last()?) {
        0 => match report.get(1).copied()? {
            MULTITOUCH_REPORT_ID => None,
            id => Some(PaddedReport::ModeLost(id)),
        },
        raw_len if raw_len <= RAW_REPORT_MAX_LEN => {
            report.get(..raw_len).map(PaddedReport::Multitouch)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    use super::*;

    fn payload_with(raw: &[u8]) -> ([u8; PADDED_PAYLOAD_LEN], usize) {
        let mut p = [0xEE_u8; PADDED_PAYLOAD_LEN];
        p[0] = HIDP_DATA_INPUT;
        p[1..=raw.len()].copy_from_slice(raw);
        (p, 1 + raw.len())
    }

    fn raw_report(fingers: usize) -> Vec<u8> {
        let mut r = vec![MULTITOUCH_REPORT_ID, 0x60, 0xE0, 0x5A];
        for f in 0..fingers {
            r.extend((0..9).map(|i| (f * 9 + i) as u8 ^ 0x5C));
        }
        r
    }

    #[test]
    fn pads_and_unpads_every_finger_count() {
        for fingers in 0..=15 {
            let raw = raw_report(fingers);
            let (mut payload, len) = payload_with(&raw);
            assert_eq!(
                pad_interrupt_payload(&mut payload, len),
                Some(PADDED_PAYLOAD_LEN)
            );
            let report = &payload[1..];
            assert_eq!(report.len(), PADDED_REPORT_LEN);
            assert!(
                report[raw.len()..PADDED_REPORT_LEN - 1]
                    .iter()
                    .all(|&b| b == 0)
            );
            assert_eq!(
                unpad_report(report),
                Some(PaddedReport::Multitouch(raw.as_slice())),
                "{fingers} fingers"
            );
        }
    }

    #[test]
    fn a_mouse_report_becomes_a_mode_lost_marker() {
        let (mut payload, len) = payload_with(&[MOUSE_REPORT_ID, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            pad_interrupt_payload(&mut payload, len),
            Some(PADDED_PAYLOAD_LEN)
        );
        let report = &payload[1..];
        assert!(report.len() > 2);
        assert_eq!(report[0], MULTITOUCH_REPORT_ID);
        assert_eq!(report[1], MOUSE_REPORT_ID);
        assert!(report[2..].iter().all(|&b| b == 0));
        assert_eq!(
            unpad_report(report),
            Some(PaddedReport::ModeLost(MOUSE_REPORT_ID))
        );
    }

    #[test]
    fn leaves_other_payloads_alone() {
        // Battery report 0x90 (Col02 delivers it), an undeclared report.
        for raw in [&[0x90_u8, 0x03, 0x5A][..], &[0x13, 0x00]] {
            let (mut payload, len) = payload_with(raw);
            let before = payload;
            assert_eq!(pad_interrupt_payload(&mut payload, len), None);
            assert_eq!(payload, before);
        }
        // Too long (16 fingers), a continuation fragment, an empty transfer, a wrong capacity.
        let (mut payload, _) = payload_with(&raw_report(15));
        assert_eq!(
            pad_interrupt_payload(&mut payload, PADDED_PAYLOAD_LEN),
            None
        );
        let (mut payload, len) = payload_with(&raw_report(1));
        payload[0] = 0x5C;
        assert_eq!(pad_interrupt_payload(&mut payload, len), None);
        let (mut payload, _) = payload_with(&raw_report(1));
        assert_eq!(pad_interrupt_payload(&mut payload, 0), None);
        assert_eq!(pad_interrupt_payload(&mut payload, 1), None);
        let mut short = [HIDP_DATA_INPUT, MULTITOUCH_REPORT_ID, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(pad_interrupt_payload(&mut short, 5), None);
        let mut long = [0_u8; 1025];
        long[..2].copy_from_slice(&[HIDP_DATA_INPUT, MULTITOUCH_REPORT_ID]);
        assert_eq!(pad_interrupt_payload(&mut long, 14), None);
    }

    #[test]
    fn rejects_malformed_padded_reports() {
        let (mut payload, len) = payload_with(&raw_report(2));
        pad_interrupt_payload(&mut payload, len).unwrap();
        let mut report = payload[1..].to_vec();
        assert!(unpad_report(&report[..PADDED_REPORT_LEN - 1]).is_none());
        report[PADDED_REPORT_LEN - 1] = (RAW_REPORT_MAX_LEN + 1) as u8;
        assert!(unpad_report(&report).is_none());
        // Length 0 with 0x31 in the ID slot is no marker.
        report[PADDED_REPORT_LEN - 1] = 0;
        report[1] = MULTITOUCH_REPORT_ID;
        assert!(unpad_report(&report).is_none());
        report[PADDED_REPORT_LEN - 1] = 4;
        report[0] = 0x90;
        assert!(unpad_report(&report).is_none());
    }
}

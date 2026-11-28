//! USB input report assembly and contact decoding for Wellspring and Magic Trackpad 2.

use amtptp_core::domain::Contact;
use amtptp_core::hid::common::{MAX_FINGERS, PTP_CONTACT_SLOTS};
use amtptp_core::hid::input::{PtpContact, PtpReport};

use crate::bcm5974::config::{Bcm5974Config, MAGIC_TRACKPAD_2, TrackpadType};

/// Size of the unpacked TYPE1–4 finger structure, excluding any packet prefix.
pub const TRACKPAD_FINGER_LEN: usize = 28;
/// Size of a packed TYPE5 finger record.
pub const MT2_FINGER_LEN: usize = 9;
/// Multitouch HID report ID on USB Magic Trackpad 2 (`TRACKPAD2_USB_REPORT_ID`).
pub const MT2_USB_REPORT_ID: u8 = 0x02;
/// Upper bound on doubled `touch_major` / `touch_minor` before a Wellspring/T2 contact is flagged
/// as an unintended palm (`Confidence = 0`). Matches `SpiKm`'s `CONFIDENCE_TOUCH_LIMIT`.
pub const WELLSPRING_CONFIDENCE_LIMIT: i32 = 2500;

const MAX_RAW_USB_CONTACTS: usize = MAX_FINGERS as usize;
const FINGER_ABS_X: usize = 2;
const FINGER_ABS_Y: usize = 4;
const FINGER_TOUCH_MAJOR: usize = 16;
const FINGER_TOUCH_MINOR: usize = 18;

/// Counts the raw contacts in a frame with a complete header, valid report ID (for `Type5`), and
/// whole finger records.
#[must_use]
pub const fn usb_frame_contacts(format: TrackpadType, packet: &[u8]) -> Option<usize> {
    if matches!(format, TrackpadType::Type5) && !matches!(packet.first(), Some(&MT2_USB_REPORT_ID))
    {
        return None;
    }
    usb_contact_count(format, packet.len())
}

/// Counts the raw contacts in a frame with a complete header and whole finger records.
#[must_use]
pub const fn usb_contact_count(format: TrackpadType, packet_len: usize) -> Option<usize> {
    let Some(payload) = packet_len.checked_sub(format.header() as usize) else {
        return None;
    };
    let stride = format.finger_size() as usize;
    if payload.is_multiple_of(stride) {
        Some(payload / stride)
    } else {
        None
    }
}

/// Assembles a stateless PTP report from a USB frame without contact tracking.
#[must_use]
pub fn build_usb_report<F>(
    format: TrackpadType,
    packet: &[u8],
    report_touch: bool,
    report_button: bool,
    mut decode_contact: F,
) -> PtpReport
where
    F: FnMut(&[u8], usize) -> PtpContact,
{
    let mut report = PtpReport::EMPTY;
    if report_touch {
        let header = format.header() as usize;
        let stride = format.finger_size() as usize;
        let count = (packet.len().saturating_sub(header) / stride).min(PTP_CONTACT_SLOTS);
        report.contact_count = count as u8;
        let base = header + format.delta() as usize;
        for (slot, contact) in report.contacts.iter_mut().take(count).enumerate() {
            let start = base + slot * stride;
            let finger = packet.get(start..).unwrap_or_default();
            *contact = decode_contact(finger, slot);
        }
    }
    if report_button && usb_button_pressed(format, packet) {
        report.is_button_clicked = 1;
    }
    report
}

/// Decodes up to the supported number of observations from a validated USB frame.
pub(crate) fn decode_usb_contacts(
    profile: &Bcm5974Config,
    packet: &[u8],
    raw_count: usize,
    minor_tip_threshold: Option<i32>,
) -> ([Contact; MAX_RAW_USB_CONTACTS], usize) {
    let format = profile.tp_type;
    let header = format.header() as usize;
    let stride = format.finger_size() as usize;
    let count = raw_count.min(MAX_RAW_USB_CONTACTS);
    let base = header + format.delta() as usize;
    let mut decoded = [Contact::EMPTY; MAX_RAW_USB_CONTACTS];
    for (slot, dst) in decoded.iter_mut().take(count).enumerate() {
        let start = base + slot * stride;
        let finger = packet.get(start..).unwrap_or_default();
        *dst = if format == TrackpadType::Type5 {
            finger.first_chunk().map_or(Contact::EMPTY, |record| {
                mt2_contact(record, profile.x.min, profile.y.min)
            })
        } else {
            wellspring_contact(profile, finger, slot, minor_tip_threshold)
        };
    }
    (decoded, count)
}

/// Reads the button state from a USB frame.
pub(crate) fn usb_button_pressed(format: TrackpadType, packet: &[u8]) -> bool {
    packet
        .get(format.button() as usize)
        .is_some_and(|&byte| byte != 0)
}

/// Decodes a TYPE1–4 contact from the start of a finger block after any format-specific prefix.
///
/// The input values are signed little-endian fields. Coordinates are clamped to the device's
/// `0..=(max - min)` logical span, Y is inverted, and `Confidence` is cleared only when doubled
/// `touch_major` or `touch_minor` reaches [`WELLSPRING_CONFIDENCE_LIMIT`] (palm rejection).
/// Both USB drivers treat a doubled `touch_major` of at least 200 as a tip; `UsbKm` also treats
/// a doubled `touch_minor` of at least 150 as a tip.
#[must_use]
pub fn wellspring_contact(
    cfg: &Bcm5974Config,
    finger: &[u8],
    slot: usize,
    minor_tip_threshold: Option<i32>,
) -> Contact {
    let x = raw_i16(finger, FINGER_ABS_X);
    let y = raw_i16(finger, FINGER_ABS_Y);
    let major = raw_i16(finger, FINGER_TOUCH_MAJOR) * 2;
    let minor = raw_i16(finger, FINGER_TOUCH_MINOR) * 2;
    let x_span = cfg.x.max.saturating_sub(cfg.x.min);
    let y_span = cfg.y.max.saturating_sub(cfg.y.min);
    Contact {
        confidence: major < WELLSPRING_CONFIDENCE_LIMIT && minor < WELLSPRING_CONFIDENCE_LIMIT,
        tip_switch: major >= 200 || minor_tip_threshold.is_some_and(|threshold| minor >= threshold),
        contact_id: u32::from(slot as u8),
        x: bounded_offset(x, cfg.x.min, x_span),
        y: bounded_offset(cfg.y.max.saturating_sub(y), 0, y_span),
    }
}

fn raw_i16(bytes: &[u8], offset: usize) -> i32 {
    bytes
        .get(offset..)
        .and_then(<[u8]>::first_chunk::<2>)
        .map_or(0, |pair| i32::from(i16::from_le_bytes(*pair)))
}

/// Decodes the signed 13-bit X and Y fields of a Magic Trackpad 2 finger.
/// Invert Y after sign extension and keep both coordinates inside the HID logical range.
#[must_use]
pub const fn mt2_contact(f: &[u8; MT2_FINGER_LEN], x_min: i32, y_min: i32) -> Contact {
    let [
        b0,
        b1,
        b2,
        b3,
        touch_major,
        touch_minor,
        _size,
        _pressure,
        orientation_origin,
    ] = *f;
    let tmp_x = u16::from_le_bytes([b0, b1]) & 0x1FFF;
    let tmp_y = u32::from_le_bytes([b0, b1, b2, b3]);
    // tmp_x << 3 <= 0xFFF8; (SHORT) reinterprets, >> on a negative SHORT is arithmetic.
    let x = (((tmp_x << 3) as i16) >> 3) as i32;
    let y = -(((tmp_y << 6) as i32) >> 19);
    Contact {
        // ((signed short)TouchMinor << 1) < 345, tested twice.
        confidence: ((touch_minor as i16 as i32) << 1) < 345,
        // ((signed short)TouchMajor << 1) > 0.
        tip_switch: ((touch_major as i16 as i32) << 1) > 0,
        contact_id: (orientation_origin & 0x0F) as u32,
        x: bounded_offset(x, x_min, MAGIC_TRACKPAD_2.x.max - MAGIC_TRACKPAD_2.x.min),
        y: bounded_offset(y, y_min, MAGIC_TRACKPAD_2.y.max - MAGIC_TRACKPAD_2.y.min),
    }
}

const fn bounded_offset(value: i32, minimum: i32, logical_max: i32) -> u16 {
    let offset = value.saturating_sub(minimum);
    if offset < 0 {
        0
    } else if offset > logical_max {
        logical_max as u16
    } else {
        offset as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bcm5974::config::{Bcm5974Param, TrackpadType};

    const TEST_CFG: Bcm5974Config = Bcm5974Config {
        tp_type: TrackpadType::Type4,
        x: Bcm5974Param {
            min: -4824,
            max: 5349,
        },
        y: Bcm5974Param {
            min: -203,
            max: 6803,
        },
    };

    fn make_finger(x: i16, y: i16, major: i16, minor: i16) -> [u8; TRACKPAD_FINGER_LEN] {
        let mut f = [0_u8; TRACKPAD_FINGER_LEN];
        f[2..4].copy_from_slice(&x.to_le_bytes());
        f[4..6].copy_from_slice(&y.to_le_bytes());
        f[16..18].copy_from_slice(&major.to_le_bytes());
        f[18..20].copy_from_slice(&minor.to_le_bytes());
        f
    }

    #[test]
    fn wellspring_confidence_accepts_light_touch_and_rejects_palm() {
        let light = wellspring_contact(&TEST_CFG, &make_finger(0, 0, 100, 0), 0, None);
        assert!(light.tip_switch);
        assert!(light.confidence, "minor == 0 must not clear Confidence");

        let palm = wellspring_contact(&TEST_CFG, &make_finger(0, 0, 1300, 900), 0, None);
        assert!(palm.tip_switch);
        assert!(
            !palm.confidence,
            "large touch ellipse must clear Confidence"
        );
    }

    #[test]
    fn wellspring_coordinates_clamp_at_zero_and_logical_max() {
        let x_max = (TEST_CFG.x.max - TEST_CFG.x.min) as u16;
        let y_max = (TEST_CFG.y.max - TEST_CFG.y.min) as u16;

        let low = wellspring_contact(&TEST_CFG, &make_finger(-9000, 9000, 120, 40), 0, None);
        assert_eq!((low.x, low.y), (0, 0));

        let high = wellspring_contact(&TEST_CFG, &make_finger(9000, -9000, 120, 40), 0, None);
        assert_eq!((high.x, high.y), (x_max, y_max));
    }

    #[test]
    fn type5_usb_requires_report_id_0x02() {
        let mut pkt = [0_u8; 21];
        assert_eq!(usb_frame_contacts(TrackpadType::Type5, &pkt), None);
        pkt[0] = 0x31;
        assert_eq!(usb_frame_contacts(TrackpadType::Type5, &pkt), None);
        pkt[0] = MT2_USB_REPORT_ID;
        assert_eq!(usb_frame_contacts(TrackpadType::Type5, &pkt), Some(1));
    }
}

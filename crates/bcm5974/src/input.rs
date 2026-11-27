//! USB input report assembly and contact decoding for Wellspring and Magic Trackpad 2.

use ptp_hid::common::PTP_CONTACT_SLOTS;
use ptp_hid::input::{PtpContact, PtpReport};

use crate::config::{Bcm5974Config, MAGIC_TRACKPAD_2, TrackpadType};

/// Size of the unpacked TYPE1–4 finger structure, excluding any packet prefix.
pub const TRACKPAD_FINGER_LEN: usize = 28;
/// Size of a packed TYPE5 finger record.
pub const MT2_FINGER_LEN: usize = 9;

const FINGER_ABS_X: usize = 2;
const FINGER_ABS_Y: usize = 4;
const FINGER_TOUCH_MAJOR: usize = 16;
const FINGER_TOUCH_MINOR: usize = 18;

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

/// Assembles a PTP report from a USB frame. The caller handles malformed frames,
/// the scan clock, and the contact decoding policy.
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
            // TYPE4 has a two-byte prefix to each finger record. The contact decoders
            // read only their own fields from the remaining bytes.
            let finger = packet.get(start..).unwrap_or_default();
            *contact = decode_contact(finger, slot);
        }
    }
    if report_button
        && packet
            .get(format.button() as usize)
            .is_some_and(|&byte| byte != 0)
    {
        report.is_button_clicked = 1;
    }
    report
}

/// Decodes a TYPE1–4 contact from the start of a finger block after any format-specific prefix.
///
/// The input values are signed little-endian fields. Coordinates are clamped at zero as in the
/// original drivers. Both drivers treat a doubled touch-major value of at least 200 as a tip.
/// The KMDF USB driver also treats a doubled touch-minor value of at least 150 as a tip.
#[must_use]
pub fn wellspring_contact(
    cfg: &Bcm5974Config,
    finger: &[u8],
    slot: usize,
    minor_tip_threshold: Option<i32>,
) -> PtpContact {
    let x = raw_i16(finger, FINGER_ABS_X);
    let y = raw_i16(finger, FINGER_ABS_Y);
    let major = raw_i16(finger, FINGER_TOUCH_MAJOR) * 2;
    let minor = raw_i16(finger, FINGER_TOUCH_MINOR) * 2;
    let dy = cfg.y.max.wrapping_sub(y);
    PtpContact {
        confidence: minor > 0,
        tip_switch: major >= 200 || minor_tip_threshold.is_some_and(|threshold| minor >= threshold),
        contact_id: u32::from(slot as u8),
        x: clamp_offset(x, cfg.x.min) as u16,
        y: if dy > 0 { dy as u16 } else { 0 },
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
pub const fn mt2_contact(f: &[u8; MT2_FINGER_LEN], x_min: i32, y_min: i32) -> PtpContact {
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
    PtpContact {
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

/// `(v - min) > 0 ? (v - min) : 0`, with the INT subtraction wrapping like MSVC.
const fn clamp_offset(v: i32, min: i32) -> i32 {
    let d = v.wrapping_sub(min);
    if d > 0 { d } else { 0 }
}

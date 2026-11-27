//! Contact decoding for Wellspring and Magic Trackpad 2 input packets.

use ptp_hid::input::PtpContact;

use crate::config::Bcm5974Config;

/// Size of the unpacked TYPE1–4 finger structure, excluding any packet prefix.
pub const TRACKPAD_FINGER_LEN: usize = 28;
/// Size of a packed TYPE5 finger record.
pub const MT2_FINGER_LEN: usize = 9;

const FINGER_ABS_X: usize = 2;
const FINGER_ABS_Y: usize = 4;
const FINGER_TOUCH_MAJOR: usize = 16;
const FINGER_TOUCH_MINOR: usize = 18;

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

/// Decodes a 9-byte Magic Trackpad 2 contact with the legacy Windows driver arithmetic.
///
/// - X: the low 13 bits of the u16 at bytes 0-1, sign-extended (`(SHORT)(tmp_x << 3) >> 3`).
/// - Y: `-(INT)(u32 << 6) >> 19`, i.e. `-Y13` when the X bits are zero and `-Y13 - 1` otherwise
///   (HF-B2); negating `INT_MIN` wraps like MSVC.
/// - Both minus their `min`, clamped below at 0, never above (HF-B5), then truncated to `USHORT`.
/// - `ContactID`: low nibble of byte 8. `TipSwitch`: byte 4 != 0. Confidence: byte 5 <= 172 (the C code
///   tests `TouchMinor` twice, HF-B3).
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
    // (tmp_y << 6) drops the top 6 bits; (INT) reinterprets; unary minus binds before >>.
    let y = ((tmp_y << 6) as i32).wrapping_neg() >> 19;
    let x = clamp_offset(x, x_min);
    let y = clamp_offset(y, y_min);
    PtpContact {
        // ((signed short)TouchMinor << 1) < 345, tested twice.
        confidence: ((touch_minor as i16 as i32) << 1) < 345,
        // ((signed short)TouchMajor << 1) > 0.
        tip_switch: ((touch_major as i16 as i32) << 1) > 0,
        contact_id: (orientation_origin & 0x0F) as u32,
        x: x as u16, // (USHORT)x
        y: y as u16, // (USHORT)y
    }
}

/// `(v - min) > 0 ? (v - min) : 0`, with the INT subtraction wrapping like MSVC.
const fn clamp_offset(v: i32, min: i32) -> i32 {
    let d = v.wrapping_sub(min);
    if d > 0 { d } else { 0 }
}

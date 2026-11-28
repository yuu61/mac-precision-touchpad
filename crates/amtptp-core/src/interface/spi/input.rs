//! Apple SPI multitouch packet -> 30-byte PTP input report (`AmtPtpRequestCompletionRoutine`,
//! Input.c:157-321, spec §7).
//!
//! `SPI_TRACKPAD_PACKET` (346 bytes, little-endian, no padding): `ClickOccurred` @1, `NumOfFingers` @30,
//! `Fingers[10]` @46, 30 bytes each. `SPI_TRACKPAD_FINGER` is 15 signed 16-bit fields; the report uses
//! `X` @4, `Y` @6, `TouchMajor` @18, `TouchMinor` @20 and `Pressure` @26.

use crate::application::report_input;
use crate::domain::{Contact, ContactMatching, ContactTracker};
use crate::hid::common::PTP_CONTACT_SLOTS;
use crate::hid::feature::SelectiveReportBits;
use crate::hid::input::{SPI_PTP_REPORT_LEN, SpiPtpReport};

use crate::spi::device::SpiTrackpadInfo;
use crate::spi::status::{NtStatus, STATUS_DEVICE_DATA_ERROR};

/// `sizeof(SPI_TRACKPAD_PACKET)` up to `Fingers`: 46 bytes. Also the minimum accepted lower
/// Information (`SpiRequestLength < 46` fails).
pub const SPI_PACKET_HEADER_LEN: usize = 46;
/// `sizeof(SPI_TRACKPAD_FINGER)`: 30 bytes.
pub const SPI_FINGER_LEN: usize = 30;
/// `SPI_TRACKPAD_MAX_FINGERS`: 10.
pub const SPI_MAX_FINGERS: usize = 10;
/// `sizeof(SPI_TRACKPAD_PACKET)`: 346 bytes.
pub const SPI_PACKET_LEN: usize = SPI_PACKET_HEADER_LEN + SPI_MAX_FINGERS * SPI_FINGER_LEN;
const _: () = assert!(SPI_PACKET_LEN == 346);

/// Offset of `ClickOccurred` in the packet.
pub const CLICK_OCCURRED_OFFSET: usize = 1;
/// Offset of `NumOfFingers` in the packet.
pub const NUM_OF_FINGERS_OFFSET: usize = 30;
/// Offset of `Fingers[0]` in the packet.
pub const FINGERS_OFFSET: usize = SPI_PACKET_HEADER_LEN;

/// Offset of `OriginalX` in a finger (trace only).
pub const FINGER_ORIGINAL_X: usize = 0;
/// Offset of `OriginalY` in a finger (trace only).
pub const FINGER_ORIGINAL_Y: usize = 2;
/// Offset of `X` in a finger.
pub const FINGER_X: usize = 4;
/// Offset of `Y` in a finger.
pub const FINGER_Y: usize = 6;
/// Offset of `TouchMajor` in a finger.
pub const FINGER_TOUCH_MAJOR: usize = 18;
/// Offset of `TouchMinor` in a finger.
pub const FINGER_TOUCH_MINOR: usize = 20;
/// Offset of `Pressure` in a finger.
pub const FINGER_PRESSURE: usize = 26;

/// The literal `5` of Input.c:228: at most this many contacts are filled, and `ContactCount` is clamped to
/// it (P3 fix of SPI-B6: C reported the raw `NumOfFingers`). It equals `PTP_MAX_CONTACT_POINTS`, the
/// maximum the device-capabilities feature report declares; Windows drops every frame with more.
pub const MAX_FILLED_CONTACTS: u8 = 5;
const _: () = assert!(MAX_FILLED_CONTACTS as usize == PTP_CONTACT_SLOTS);
/// Confidence threshold (Input.c:242-243): Confidence = `TouchMajor < 2500 && TouchMinor < 2500`, signed.
pub const CONFIDENCE_TOUCH_LIMIT: i16 = 2500;

/// A read-only view of the valid bytes of a lower read. Every byte at or past the end of the view reads as
/// 0 (P2 fix of SPI-B9: C read finger bytes past the lower Information from stale lookaside memory).
#[derive(Clone, Copy, Debug)]
pub struct SpiPacket<'a> {
    bytes: &'a [u8],
}

/// The fields of one `SPI_TRACKPAD_FINGER` that the driver reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpiFinger {
    /// `OriginalX` (trace only).
    pub original_x: i16,
    /// `OriginalY` (trace only).
    pub original_y: i16,
    /// `X`.
    pub x: i16,
    /// `Y`.
    pub y: i16,
    /// `TouchMajor`.
    pub touch_major: i16,
    /// `TouchMinor`.
    pub touch_minor: i16,
    /// `Pressure`.
    pub pressure: i16,
}

impl<'a> SpiPacket<'a> {
    /// Wraps the valid packet bytes (the first `min((LONG)Information, buffer length)` bytes of the lower
    /// read buffer).
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    fn byte(&self, offset: usize) -> u8 {
        self.bytes.get(offset).copied().unwrap_or(0)
    }

    fn i16_at(&self, offset: usize) -> i16 {
        // SHORT, little-endian; a byte past the valid data is 0.
        i16::from_le_bytes([self.byte(offset), self.byte(offset.wrapping_add(1))])
    }

    /// `ClickOccurred` (byte 1).
    #[must_use]
    pub fn click_occurred(&self) -> u8 {
        self.byte(CLICK_OCCURRED_OFFSET)
    }

    /// `NumOfFingers` (byte 30).
    #[must_use]
    pub fn num_of_fingers(&self) -> u8 {
        self.byte(NUM_OF_FINGERS_OFFSET)
    }

    /// Whether the valid bytes hold the header and every finger record `NumOfFingers` declares.
    #[must_use]
    pub fn holds_declared_fingers(&self) -> bool {
        self.bytes.len() >= packet_len_for(self.num_of_fingers())
    }

    /// `Fingers[index]`. `index` must be below [`SPI_MAX_FINGERS`] to address a real finger; larger
    /// indexes read zeros.
    #[must_use]
    pub fn finger(&self, index: usize) -> SpiFinger {
        let base = FINGERS_OFFSET.saturating_add(index.saturating_mul(SPI_FINGER_LEN));
        let at = |off: usize| self.i16_at(base.saturating_add(off));
        SpiFinger {
            original_x: at(FINGER_ORIGINAL_X),
            original_y: at(FINGER_ORIGINAL_Y),
            x: at(FINGER_X),
            y: at(FINGER_Y),
            touch_major: at(FINGER_TOUCH_MAJOR),
            touch_minor: at(FINGER_TOUCH_MINOR),
            pressure: at(FINGER_PRESSURE),
        }
    }
}

/// Bytes a packet with `num_of_fingers` finger records occupies: the 46-byte header plus 30 bytes per
/// finger.
#[must_use]
pub const fn packet_len_for(num_of_fingers: u8) -> usize {
    SPI_PACKET_HEADER_LEN + SPI_FINGER_LEN * num_of_fingers as usize
}

/// The consistency check after the length gate (not in C): the lower read must hold every finger record
/// the header declares. C accepted a bare header that declared contacts, and reported them with zeroed or
/// stale finger data (SPI-B6, SPI-B9).
///
/// # Errors
/// Returns `STATUS_DEVICE_DATA_ERROR` if the packet is shorter than its declared contacts need.
pub fn check_declared_fingers(packet: &SpiPacket<'_>) -> Result<(), NtStatus> {
    if packet.holds_declared_fingers() {
        Ok(())
    } else {
        Err(STATUS_DEVICE_DATA_ERROR)
    }
}

/// The length gate (Input.c:199-213): `SpiRequestLength = (LONG)WdfRequestGetInformation(SpiRequest)`.
///
/// Returns `STATUS_DEVICE_DATA_ERROR` if it is below 46 (the C trace says "attempt to re-enable", but
/// nothing is done; preserved), otherwise the positive length. The `(LONG)` cast keeps the low 32 bits
/// as a signed value, so `0x1_0000_002E` passes as 46 and `0xFFFF_FFFF` fails as -1. The lower
/// `IoStatus.Status` is not consulted (preserved, SPI-B9).
///
/// # Errors
/// Returns `STATUS_DEVICE_DATA_ERROR` if the length is shorter than the packet header.
pub const fn lower_read_length(information: u64) -> Result<usize, NtStatus> {
    // (LONG)(ULONG_PTR): truncate to the low 32 bits, reinterpret as signed.
    let length = information as u32 as i32;
    if length < SPI_PACKET_HEADER_LEN as i32 {
        Err(STATUS_DEVICE_DATA_ERROR)
    } else {
        // length >= 46 > 0, so the conversion is lossless.
        Ok(length as usize)
    }
}

/// Clamps `value - minimum` into `0..=logical_max`.
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

/// Builds a tracked PTP input report from a packet, clamping coordinates to the trackpad's logical
/// span and maintaining persistent `ContactID`s (`0..=4`) plus 1-frame `TipSwitch = 0` liftoff
/// reports via `contacts`.
///
/// When `!selective.surface_report`, `contacts` is reset and no contacts are emitted.
#[must_use]
pub fn build_report(
    packet: &SpiPacket<'_>,
    trackpad: &SpiTrackpadInfo,
    scan_time: u16,
    selective: SelectiveReportBits,
    contacts: &mut ContactTracker,
) -> SpiPtpReport {
    let raw_count = if selective.surface_report {
        usize::from(packet.num_of_fingers()).min(SPI_MAX_FINGERS)
    } else {
        0
    };
    let x_span = i32::from(trackpad.x_max).saturating_sub(i32::from(trackpad.x_min));
    let y_span = i32::from(trackpad.y_max).saturating_sub(i32::from(trackpad.y_min));
    let mut decoded = [Contact::EMPTY; SPI_MAX_FINGERS];
    for (index, dst) in decoded.iter_mut().take(raw_count).enumerate() {
        let f = packet.finger(index);
        *dst = Contact {
            confidence: f.touch_major < CONFIDENCE_TOUCH_LIMIT
                && f.touch_minor < CONFIDENCE_TOUCH_LIMIT,
            tip_switch: f.pressure > 0,
            x: bounded_offset(i32::from(f.x), i32::from(trackpad.x_min), x_span),
            y: bounded_offset(
                i32::from(trackpad.y_max).saturating_sub(i32::from(f.y)),
                0,
                y_span,
            ),
            ..Contact::EMPTY
        };
    }
    let frame = report_input(
        contacts,
        ContactMatching::Position,
        decoded.get(..raw_count).unwrap_or_default(),
        packet.click_occurred(),
        selective.into(),
    );
    SpiPtpReport::from_frame(&frame, scan_time)
}

/// [`build_report`] encoded to the 30 wire bytes.
#[must_use]
pub fn report_bytes(
    packet: &SpiPacket<'_>,
    trackpad: &SpiTrackpadInfo,
    scan_time: u16,
    selective: SelectiveReportBits,
    contacts: &mut ContactTracker,
) -> [u8; SPI_PTP_REPORT_LEN] {
    build_report(packet, trackpad, scan_time, selective, contacts).to_bytes()
}

/// The valid prefix of a lower read buffer: the first `min(length, buffer.len())` bytes, where `length`
/// comes from [`lower_read_length`].
#[must_use]
pub fn valid_prefix(buffer: &[u8], length: usize) -> &[u8] {
    buffer.get(..length).unwrap_or(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_gate_truncates_like_long_cast() {
        assert_eq!(lower_read_length(45), Err(STATUS_DEVICE_DATA_ERROR));
        assert_eq!(lower_read_length(46), Ok(46));
        assert_eq!(lower_read_length(0x1_0000_002E), Ok(46));
        assert_eq!(
            lower_read_length(0x1_0000_002D),
            Err(STATUS_DEVICE_DATA_ERROR)
        );
        assert_eq!(
            lower_read_length(0xFFFF_FFFF),
            Err(STATUS_DEVICE_DATA_ERROR)
        );
        assert_eq!(
            lower_read_length(0x8000_0000),
            Err(STATUS_DEVICE_DATA_ERROR)
        );
        assert_eq!(lower_read_length(0x7FFF_FFFF), Ok(0x7FFF_FFFF));
        assert_eq!(lower_read_length(u64::MAX), Err(STATUS_DEVICE_DATA_ERROR));
    }
}

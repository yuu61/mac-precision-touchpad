//! `const fn` builders for the PTP report descriptors of all four drivers.
//!
//! Every report descriptor follows one template. Only these things vary:
//! - the contact shape inside each finger collection: a 9-byte contact with a 32-bit contact ID (`UsbUm`,
//!   `UsbKm`, `HidFilter`), or a 5-byte contact with a 3-bit contact ID (`SpiKm`);
//! - the touchpad TLC usage (Touch Pad 0x05, or Touch Screen 0x04 for `SpiKm`'s touchscreen variant);
//! - four 16-bit axis values ([`Axes`]);
//!
//! Compared to the legacy C macro templates, these builders make four intentional HID conformance fixes:
//! - the SPI contact ID's logical maximum is [`SPI_CONTACT_ID_LOGICAL_MAX`] (4) instead of 3, at
//!   [`SPI_CONTACT_ID_MAX_OFFSETS`];
//! - all five finger collections end with [`GLOBAL_RESET`] (retiring the legacy `_2` no-reset macro on
//!   fingers 3 and 5);
//! - [`TOUCHPAD_TLC_TAIL`] emits [`GLOBAL_RESET`] immediately after Scan Time so `Seconds / -4 / 65535`
//!   do not leak into subsequent fields and collections;
//! - [`WINDOWS_CONFIGURATION_TLC`] declares [`PTP_COLLECTION_WINDOWS`] (`0x03`) instead of `16` as the
//!   logical maximum of Input Mode (`0x52`).
//!
//! | Driver | Builder | Length |
//! |---|---|---|
//! | `UsbKm` (T2) | [`report_descriptor_u32_id`] | 511 |
//! | `HidFilter` (MT2) | [`report_descriptor_u32_id`] | 511 |
//!
//! Every function here is a `const fn`, so a core crate can write
//! `pub const REPORT_DESCRIPTOR: [u8; REPORT_DESCRIPTOR_U32_ID_LEN] = report_descriptor_u32_id(AXES);`.
//! All length arithmetic is checked at compile time.

use crate::bytes::put;
use crate::common::{
    BUTTON_SWITCH, PTP_COLLECTION_WINDOWS, PTP_MAX_CONTACT_POINTS, REPORTID_DEVICE_CAPS,
    REPORTID_FUNCSWITCH, REPORTID_MULTITOUCH, REPORTID_PTPHQA, REPORTID_REPORTMODE, SURFACE_SWITCH,
};
use crate::item::{
    BEGIN_COLLECTION, END_COLLECTION, FEATURE, INPUT, LOGICAL_MAXIMUM, LOGICAL_MAXIMUM_2,
    LOGICAL_MAXIMUM_3, LOGICAL_MINIMUM, PHYSICAL_MAXIMUM, PHYSICAL_MAXIMUM_2, PHYSICAL_MAXIMUM_3,
    REPORT_COUNT, REPORT_COUNT_2, REPORT_ID, REPORT_SIZE, UNIT, UNIT_2, UNIT_EXPONENT, USAGE,
    USAGE_PAGE, USAGE_PAGE_1,
};

pub use crate::bytes::concat;

/// The four per-family 16-bit values in each finger collection. They are encoded little-endian in the
/// order X logical max, X physical max, Y physical max, Y logical max, which is the order they appear in
/// the bytes. Logical and physical minimums are never set, so they are 0. The physical unit is cm with
/// exponent -2 (0.01 cm).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Axes {
    /// Logical Maximum of usage X (`26 lo hi` before `09 30`).
    pub x_logical_max: u16,
    /// Physical Maximum of usage X in 0.01 cm (`46 lo hi` after `09 30`).
    pub x_physical_max: u16,
    /// Physical Maximum of usage Y in 0.01 cm (`46 lo hi` before `09 31`).
    pub y_physical_max: u16,
    /// Logical Maximum of usage Y (`26 lo hi` before `09 31`).
    pub y_logical_max: u16,
}

/// Usage of the touchpad top-level collection (byte 3 of the descriptor).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TlcUsage {
    /// Digitizers page usage 0x05, Touch Pad. Used by every PTP descriptor.
    TouchPad = 0x05,
    /// Digitizers page usage 0x04, Touch Screen. Used only by `SpiKm`'s `*_TOUCHSCREEN_TLC` variants,
    /// selected by the `DesiredReportType` registry value.
    TouchScreen = 0x04,
}

impl TlcUsage {
    /// The usage byte (0x05 or 0x04).
    #[must_use]
    pub const fn usage(self) -> u8 {
        self as u8
    }
}

/// Length of the first part of a 9-byte-contact finger collection, up to and including the
/// 32-bit Contact Identifier.
pub const FINGER_HEAD_U32_ID_LEN: usize = 33;
/// Length of the first part of an SPI 5-byte-contact finger collection, up to and including the 3 padding
/// bits.
pub const FINGER_HEAD_SPI_LEN: usize = 30;
/// Length of the X/Y item block shared by both contact shapes ([`axis_items`]).
pub const AXIS_ITEMS_LEN: usize = 30;
/// Length of the `45 00 55 00 65 00` reset at the end of a `*_FINGER_COLLECTION_1`.
pub const GLOBAL_RESET_LEN: usize = 6;

/// Length of [`finger_collection_u32_id`] (9-byte contact with [`GLOBAL_RESET`]).
pub const FINGER_COLLECTION_U32_ID_LEN: usize =
    FINGER_HEAD_U32_ID_LEN + AXIS_ITEMS_LEN + GLOBAL_RESET_LEN + 1;
/// Length of [`finger_collection_spi`] (SPI 5-byte contact with [`GLOBAL_RESET`]).
pub const FINGER_COLLECTION_SPI_LEN: usize =
    FINGER_HEAD_SPI_LEN + AXIS_ITEMS_LEN + GLOBAL_RESET_LEN + 1;

/// Length of [`touchpad_tlc_header`].
pub const TOUCHPAD_TLC_HEADER_LEN: usize = 10;
/// Length of [`FINGER_SEPARATOR`].
pub const FINGER_SEPARATOR_LEN: usize = 4;
/// Length of [`TOUCHPAD_TLC_TAIL`].
pub const TOUCHPAD_TLC_TAIL_LEN: usize = 88;

/// Length of [`touchpad_tlc_u32_id`]: header, five reset-terminated finger collections with 4 separators,
/// and the tail.
pub const TOUCHPAD_TLC_U32_ID_LEN: usize = TOUCHPAD_TLC_HEADER_LEN
    + 5 * FINGER_COLLECTION_U32_ID_LEN
    + 4 * FINGER_SEPARATOR_LEN
    + TOUCHPAD_TLC_TAIL_LEN;
/// Length of [`touchpad_tlc_spi`].
pub const TOUCHPAD_TLC_SPI_LEN: usize = TOUCHPAD_TLC_HEADER_LEN
    + 5 * FINGER_COLLECTION_SPI_LEN
    + 4 * FINGER_SEPARATOR_LEN
    + TOUCHPAD_TLC_TAIL_LEN;

/// Length of [`WINDOWS_CONFIGURATION_TLC`].
pub const WINDOWS_CONFIGURATION_TLC_LEN: usize = 47;

/// Length of [`report_descriptor_u32_id`] (`UsbKm`, `HidFilter`).
pub const REPORT_DESCRIPTOR_U32_ID_LEN: usize =
    TOUCHPAD_TLC_U32_ID_LEN + WINDOWS_CONFIGURATION_TLC_LEN;
/// Length of [`report_descriptor_spi`] (`SpiKm`).
pub const REPORT_DESCRIPTOR_SPI_LEN: usize = TOUCHPAD_TLC_SPI_LEN + WINDOWS_CONFIGURATION_TLC_LEN;

const _: () = assert!(FINGER_COLLECTION_U32_ID_LEN == 70);
const _: () = assert!(FINGER_COLLECTION_SPI_LEN == 67);
const _: () = assert!(TOUCHPAD_TLC_U32_ID_LEN == 464);
const _: () = assert!(TOUCHPAD_TLC_SPI_LEN == 449);
const _: () = assert!(REPORT_DESCRIPTOR_U32_ID_LEN == 511);
const _: () = assert!(REPORT_DESCRIPTOR_SPI_LEN == 496);

/// Head of a 9-byte-contact finger collection, from `BEGIN_COLLECTION` to the 32-bit Contact
/// Identifier: Confidence:1, Tip Switch:1, 6 constant bits, Contact ID:32 with logical maximum
/// 0xFFFFFFFF.
#[rustfmt::skip]
pub const FINGER_HEAD_U32_ID: [u8; FINGER_HEAD_U32_ID_LEN] = [
    BEGIN_COLLECTION, 0x02, // Begin Collection: Logical
    LOGICAL_MAXIMUM, 0x01, // Logical Maximum: 1
    USAGE, 0x47, // Usage: Confidence
    USAGE, 0x42, // Usage: Tip switch
    REPORT_COUNT, 0x02, // Report Count: 2
    REPORT_SIZE, 0x01, // Report Size: 1
    INPUT, 0x02, // Input: (Data, Var, Abs)
    REPORT_SIZE, 0x01, // Report Size: 1
    REPORT_COUNT, 0x06, // Report Count: 6
    INPUT, 0x03, // Input: (Const, Var, Abs)
    REPORT_COUNT, 0x01, // Report Count: 1
    REPORT_SIZE, 0x20, // Report Size: 0x20 (4 bytes)
    LOGICAL_MAXIMUM_3, 0xff, 0xff, 0xff, 0xff, // Logical Maximum: 0xffffffff
    USAGE, 0x51, // Usage: Contact Identifier
    INPUT, 0x02, // Input: (Data, Var, Abs)
];

/// Logical maximum of the SPI 3-bit Contact Identifier: the highest contact slot index `SpiKm` reports
/// (4). The C descriptor declared 3, so the fifth contact's ID was out of range (P3 fix).
pub const SPI_CONTACT_ID_LOGICAL_MAX: u8 = PTP_MAX_CONTACT_POINTS - 1;

/// Offset of the Contact Identifier's logical-maximum value in [`FINGER_HEAD_SPI`].
pub const FINGER_HEAD_SPI_CONTACT_ID_MAX_OFFSET: usize = 19;

/// Offsets of the Contact Identifier's logical-maximum value in an SPI report descriptor, one per finger
/// collection (all five finger collections end with [`GLOBAL_RESET`]; see [`touchpad_tlc_spi`]).
pub const SPI_CONTACT_ID_MAX_OFFSETS: [usize; 5] = {
    let step = FINGER_COLLECTION_SPI_LEN + FINGER_SEPARATOR_LEN;
    let first = TOUCHPAD_TLC_HEADER_LEN + FINGER_HEAD_SPI_CONTACT_ID_MAX_OFFSET;
    [
        first,
        first + step,
        first + 2 * step,
        first + 3 * step,
        first + 4 * step,
    ]
};

/// Head of an SPI 5-byte-contact finger collection, from `BEGIN_COLLECTION` to the padding: Confidence:1,
/// Tip Switch:1, Contact ID:3 with logical maximum [`SPI_CONTACT_ID_LOGICAL_MAX`], and 3 constant bits.
#[rustfmt::skip]
pub const FINGER_HEAD_SPI: [u8; FINGER_HEAD_SPI_LEN] = [
    BEGIN_COLLECTION, 0x02, // Begin Collection: Logical
    LOGICAL_MAXIMUM, 0x01, // Logical Maximum: 1
    USAGE, 0x47, // Usage: Confidence
    USAGE, 0x42, // Usage: Tip switch
    REPORT_COUNT, 0x02, // Report Count: 2
    REPORT_SIZE, 0x01, // Report Size: 1
    INPUT, 0x02, // Input: (Data, Var, Abs)
    REPORT_COUNT, 0x01, // Report Count: 1
    REPORT_SIZE, 0x03, // Report Size: 3
    LOGICAL_MAXIMUM, SPI_CONTACT_ID_LOGICAL_MAX, // Logical Maximum: 4
    USAGE, 0x51, // Usage: Contact Identifier
    INPUT, 0x02, // Input: (Data, Var, Abs)
    REPORT_SIZE, 0x01, // Report Size: 1
    REPORT_COUNT, 0x03, // Report Count: 3
    INPUT, 0x03, // Input: (Const, Var, Abs)
];

const _: () = {
    assert!(FINGER_HEAD_SPI[FINGER_HEAD_SPI_CONTACT_ID_MAX_OFFSET - 1] == LOGICAL_MAXIMUM);
    assert!(FINGER_HEAD_SPI[FINGER_HEAD_SPI_CONTACT_ID_MAX_OFFSET] == SPI_CONTACT_ID_LOGICAL_MAX);
};

/// The `PHYSICAL_MAXIMUM 0, UNIT_EXPONENT 0, UNIT 0` reset that ends `*_FINGER_COLLECTION_1` but is missing
/// from `*_FINGER_COLLECTION_2`.
#[rustfmt::skip]
pub const GLOBAL_RESET: [u8; GLOBAL_RESET_LEN] = [
    PHYSICAL_MAXIMUM, 0x00, // Physical Maximum: 0
    UNIT_EXPONENT, 0x00, // Unit exponent: 0
    UNIT, 0x00, // Unit: None
];

/// The X/Y item block of a finger collection: Generic Desktop page, X and Y as 16-bit fields, unit cm with
/// exponent -2, and the four [`Axes`] values. Within this block the value bytes are at offsets 3 (X
/// logical), 14 (X physical), 21 (Y physical) and 24 (Y logical).
#[rustfmt::skip]
#[must_use]
pub const fn axis_items(axes: Axes) -> [u8; AXIS_ITEMS_LEN] {
    let [xl0, xl1] = axes.x_logical_max.to_le_bytes();
    let [xp0, xp1] = axes.x_physical_max.to_le_bytes();
    let [yp0, yp1] = axes.y_physical_max.to_le_bytes();
    let [yl0, yl1] = axes.y_logical_max.to_le_bytes();
    [
        USAGE_PAGE, 0x01, // Usage Page: Generic Desktop
        LOGICAL_MAXIMUM_2, xl0, xl1, // Logical Maximum: X
        REPORT_SIZE, 0x10, // Report Size: 0x10 (2 bytes)
        UNIT_EXPONENT, 0x0e, // Unit exponent: -2
        UNIT, 0x11, // Unit: SI Length (cm)
        USAGE, 0x30, // Usage: X
        PHYSICAL_MAXIMUM_2, xp0, xp1, // Physical Maximum: X
        REPORT_COUNT, 0x01, // Report count: 1
        INPUT, 0x02, // Input: (Data, Var, Abs)
        PHYSICAL_MAXIMUM_2, yp0, yp1, // Physical Maximum: Y
        LOGICAL_MAXIMUM_2, yl0, yl1, // Logical Maximum: Y
        USAGE, 0x31, // Usage: Y
        INPUT, 0x02, // Input: (Data, Var, Abs)
    ]
}

/// Builds one finger collection from its head, the axis block and an optional reset, then closes it with
/// `END_COLLECTION`. `N` must equal `head + 30 + reset + 1`; this is checked at compile time.
const fn finger_collection<const H: usize, const R: usize, const N: usize>(
    head: &[u8; H],
    axes: Axes,
    reset: &[u8; R],
) -> [u8; N] {
    const {
        assert!(
            H + AXIS_ITEMS_LEN + R + 1 == N,
            "finger collection length mismatch"
        );
    };
    let mut out = [0_u8; N];
    let remaining = put(&mut out, head);
    let remaining = put(remaining, &axis_items(axes));
    let remaining = put(remaining, reset);
    put(remaining, &[END_COLLECTION]);
    out
}

/// Finger collection with a 9-byte contact (70 bytes), ending with [`GLOBAL_RESET`].
#[must_use]
pub const fn finger_collection_u32_id(axes: Axes) -> [u8; FINGER_COLLECTION_U32_ID_LEN] {
    finger_collection(&FINGER_HEAD_U32_ID, axes, &GLOBAL_RESET)
}

/// Finger collection with an SPI 5-byte contact (67 bytes), ending with [`GLOBAL_RESET`].
#[must_use]
pub const fn finger_collection_spi(axes: Axes) -> [u8; FINGER_COLLECTION_SPI_LEN] {
    finger_collection(&FINGER_HEAD_SPI, axes, &GLOBAL_RESET)
}

/// The first 10 bytes of the touchpad TLC: Digitizer page, the [`TlcUsage`] (at byte 3), Application
/// collection, Report ID 0x05 and Usage Finger for the first finger collection.
#[rustfmt::skip]
#[must_use]
pub const fn touchpad_tlc_header(usage: TlcUsage) -> [u8; TOUCHPAD_TLC_HEADER_LEN] {
    [
        USAGE_PAGE, 0x0d, // Usage Page: Digitizer
        USAGE, usage.usage(), // Usage: Touch Pad (0x05) / Touch Screen (0x04)
        BEGIN_COLLECTION, 0x01, // Begin Collection: Application
        REPORT_ID, REPORTID_MULTITOUCH, // Report ID: Multi-touch
        USAGE, 0x22, // Usage: Finger
    ]
}

/// The 4 bytes before each finger collection after the first: `Usage Page: Digitizer, Usage: Finger`.
#[rustfmt::skip]
pub const FINGER_SEPARATOR: [u8; FINGER_SEPARATOR_LEN] = [
    USAGE_PAGE, 0x0d, // Usage Page: Digitizer
    USAGE, 0x22, // Usage: Finger
];

/// The end of the touchpad TLC after the fifth finger (88 bytes). It contains Scan Time (16-bit, because
/// the finger collections' `REPORT_SIZE 0x10` is still in effect; unit seconds, exponent -4), an explicit
/// [`GLOBAL_RESET`] so time units and `65535` physical maximum do not leak further, Contact Count (8-bit,
/// logical max 0x7F), Button 1 plus 7 padding bits, feature report 0x07 (Maximum Contacts, Pad Type),
/// vendor feature report 0x08 (256-byte HQA blob), and `END_COLLECTION`.
#[rustfmt::skip]
pub const TOUCHPAD_TLC_TAIL: [u8; TOUCHPAD_TLC_TAIL_LEN] = [
    USAGE_PAGE, 0x0d, // Usage Page: Digitizer
    UNIT_EXPONENT, 0x0c, // Unit exponent: -4
    UNIT_2, 0x01, 0x10, // Time: Second
    PHYSICAL_MAXIMUM_3, 0xff, 0xff, 0x00, 0x00,
    LOGICAL_MAXIMUM_3, 0xff, 0xff, 0x00, 0x00,
    USAGE, 0x56, // Usage: Scan Time
    INPUT, 0x02, // Input: (Data, Var, Abs)
    PHYSICAL_MAXIMUM, 0x00, // Physical Maximum: 0
    UNIT_EXPONENT, 0x00, // Unit exponent: 0
    UNIT, 0x00, // Unit: None
    USAGE, 0x54, // Usage: Contact Count
    LOGICAL_MAXIMUM, 0x7f,
    REPORT_SIZE, 0x08,
    INPUT, 0x02, // Input: (Data, Var, Abs)
    USAGE_PAGE, 0x09, // Usage Page: Button
    USAGE, 0x01, // Button 1
    LOGICAL_MAXIMUM, 0x01,
    REPORT_SIZE, 0x01,
    INPUT, 0x02, // Input: (Data, Var, Abs)
    REPORT_COUNT, 0x07,
    INPUT, 0x03, // Input: (Const, Var, Abs)
    USAGE_PAGE, 0x0d, // Usage Page: Digitizer
    REPORT_ID, REPORTID_DEVICE_CAPS,
    USAGE, 0x55, // Usage: Maximum Contacts
    USAGE, 0x59, // Usage: Touchpad Button Type
    LOGICAL_MINIMUM, 0x00,
    LOGICAL_MAXIMUM_2, 0xff, 0x00,
    REPORT_SIZE, 0x08,
    REPORT_COUNT, 0x02,
    FEATURE, 0x02,
    USAGE_PAGE_1, 0x00, 0xff, // Usage Page: Vendor defined 0xFF00
    REPORT_ID, REPORTID_PTPHQA,
    USAGE, 0xc5,
    LOGICAL_MINIMUM, 0x00,
    LOGICAL_MAXIMUM_2, 0xff, 0x00,
    REPORT_SIZE, 0x08,
    REPORT_COUNT_2, 0x00, 0x01, // Report Count: 256
    FEATURE, 0x02,
    END_COLLECTION, // End Collection (Application)
];

/// Assembles the touchpad TLC: header, five identical reset-terminated finger collections separated by
/// [`FINGER_SEPARATOR`], and [`TOUCHPAD_TLC_TAIL`].
const fn touchpad_tlc<const F: usize, const N: usize>(usage: TlcUsage, fc: &[u8; F]) -> [u8; N] {
    const {
        assert!(
            TOUCHPAD_TLC_HEADER_LEN + 5 * F + 4 * FINGER_SEPARATOR_LEN + TOUCHPAD_TLC_TAIL_LEN == N,
            "touchpad TLC length mismatch"
        );
    };
    let mut out = [0_u8; N];
    let rest = put(&mut out, &touchpad_tlc_header(usage));
    let rest = put(rest, fc); // finger 1
    let rest = put(rest, &FINGER_SEPARATOR);
    let rest = put(rest, fc); // finger 2
    let rest = put(rest, &FINGER_SEPARATOR);
    let rest = put(rest, fc); // finger 3
    let rest = put(rest, &FINGER_SEPARATOR);
    let rest = put(rest, fc); // finger 4
    let rest = put(rest, &FINGER_SEPARATOR);
    let rest = put(rest, fc); // finger 5
    put(rest, &TOUCHPAD_TLC_TAIL);
    out
}

/// The touchpad TLC with 9-byte contacts (464 bytes), e.g. `AAPL_WELLSPRING_3_PTP_TLC`,
/// `AAPL_WELLSPRING_T2_PTP_TLC` or `AAPL_MAGIC_TRACKPAD2_PTP_TLC`. Inside each finger collection the axis
/// value bytes are at offsets 36, 47, 54 and 57.
#[must_use]
pub const fn touchpad_tlc_u32_id(usage: TlcUsage, axes: Axes) -> [u8; TOUCHPAD_TLC_U32_ID_LEN] {
    touchpad_tlc(usage, &finger_collection_u32_id(axes))
}

/// The touchpad TLC with SPI 5-byte contacts (449 bytes), e.g. `AAPL_SPI_SERIES1_PTP_TLC` (with
/// [`TlcUsage::TouchPad`]) or `AAPL_SPI_SERIES1_TOUCHSCREEN_TLC` (with [`TlcUsage::TouchScreen`]). Inside
/// each finger collection the axis value bytes are at offsets 33, 44, 51 and 54.
#[must_use]
pub const fn touchpad_tlc_spi(usage: TlcUsage, axes: Axes) -> [u8; TOUCHPAD_TLC_SPI_LEN] {
    touchpad_tlc(usage, &finger_collection_spi(axes))
}

/// `AAPL_PTP_WINDOWS_CONFIGURATION_TLC` (47 bytes), identical in all four drivers. It holds the Digitizer
/// Configuration TLC with feature report 0x04 (Input Mode; logical max [`PTP_COLLECTION_WINDOWS`] = 3)
/// and feature report 0x06 (two 1-bit switches, Surface 0x57 then Button 0x58, plus 6 padding bits).
#[rustfmt::skip]
pub const WINDOWS_CONFIGURATION_TLC: [u8; WINDOWS_CONFIGURATION_TLC_LEN] = [
    USAGE_PAGE, 0x0d, // Usage Page: Digitizer
    USAGE, 0x0e, // Usage: Configuration
    BEGIN_COLLECTION, 0x01, // Begin Collection: Application
    REPORT_ID, REPORTID_REPORTMODE, // Report ID: Mode Selection
    USAGE, 0x22, // Usage: Finger
    BEGIN_COLLECTION, 0x02, // Begin Collection: Logical
    USAGE, 0x52, // Usage: Input Mode
    LOGICAL_MINIMUM, 0x00, // Logical Minimum: 0 (Mouse)
    LOGICAL_MAXIMUM, PTP_COLLECTION_WINDOWS, // Logical Maximum: 3 (Windows PTP)
    REPORT_SIZE, 0x08, // Report Size: 0x08
    REPORT_COUNT, 0x01, // Report Count: 0x01
    FEATURE, 0x02, // Feature: (Data, Var, Abs)
    END_COLLECTION, // End Collection
    BEGIN_COLLECTION, 0x00, // Begin Collection: Physical
    REPORT_ID, REPORTID_FUNCSWITCH, // Report ID: Function Switch
    USAGE, SURFACE_SWITCH, // Usage: 0x57 Surface Switch (bit 0)
    USAGE, BUTTON_SWITCH, // Usage: 0x58 Button Switch (bit 1)
    REPORT_SIZE, 0x01, // Report Size: 0x01
    REPORT_COUNT, 0x02, // Report Count: 0x02
    LOGICAL_MAXIMUM, 0x01, // Logical Maximum: 0x01
    FEATURE, 0x02, // Feature: (Data, Var, Abs)
    REPORT_COUNT, 0x06, // Report Count: 0x06
    FEATURE, 0x03, // Feature: (Const, Var, Abs)
    END_COLLECTION, // End Collection
    END_COLLECTION, // End Collection
];

/// Touchpad TLC (9-byte contacts, Touch Pad usage) followed by [`WINDOWS_CONFIGURATION_TLC`]: 511 bytes.
///
/// `UsbKm` and `HidFilter` use it.
#[must_use]
pub const fn report_descriptor_u32_id(axes: Axes) -> [u8; REPORT_DESCRIPTOR_U32_ID_LEN] {
    concat(
        &touchpad_tlc_u32_id(TlcUsage::TouchPad, axes),
        &WINDOWS_CONFIGURATION_TLC,
    )
}

/// Touchpad TLC (SPI 5-byte contacts, with the given usage), [`WINDOWS_CONFIGURATION_TLC`] (496 bytes).
///
/// `SpiKm` `AmtPtpSpiFamily{1,2,3a,3b}ReportDescriptor` (with [`TlcUsage::TouchPad`]) and
/// `...TouchscreenReportDescriptor` (with [`TlcUsage::TouchScreen`]) use it.
#[must_use]
pub const fn report_descriptor_spi(usage: TlcUsage, axes: Axes) -> [u8; REPORT_DESCRIPTOR_SPI_LEN] {
    concat(&touchpad_tlc_spi(usage, axes), &WINDOWS_CONFIGURATION_TLC)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AXES: Axes = Axes {
        x_logical_max: 0x1122,
        x_physical_max: 0x3344,
        y_physical_max: 0x5566,
        y_logical_max: 0x7788,
    };

    #[test]
    fn axis_value_offsets_match_spec() {
        let fc = finger_collection_u32_id(AXES);
        assert_eq!(&fc[36..38], &[0x22, 0x11]);
        assert_eq!(&fc[47..49], &[0x44, 0x33]);
        assert_eq!(&fc[54..56], &[0x66, 0x55]);
        assert_eq!(&fc[57..59], &[0x88, 0x77]);
        let fc = finger_collection_spi(AXES);
        assert_eq!(&fc[33..35], &[0x22, 0x11]);
        assert_eq!(&fc[44..46], &[0x44, 0x33]);
        assert_eq!(&fc[51..53], &[0x66, 0x55]);
        assert_eq!(&fc[54..56], &[0x88, 0x77]);
    }

    #[test]
    fn every_finger_collection_ends_with_global_reset() {
        let a = finger_collection_u32_id(AXES);
        assert_eq!(&a[63..69], &GLOBAL_RESET);
        assert_eq!(a[69], END_COLLECTION);
        let b = finger_collection_spi(AXES);
        assert_eq!(&b[60..66], &GLOBAL_RESET);
        assert_eq!(b[66], END_COLLECTION);
    }

    #[test]
    fn finger_collection_offsets_inside_tlc() {
        let t = touchpad_tlc_u32_id(TlcUsage::TouchPad, AXES);
        let fc = finger_collection_u32_id(AXES);
        for idx in 0..5 {
            let start = TOUCHPAD_TLC_HEADER_LEN
                + idx * (FINGER_COLLECTION_U32_ID_LEN + FINGER_SEPARATOR_LEN);
            assert_eq!(&t[start..start + FINGER_COLLECTION_U32_ID_LEN], &fc);
        }
        assert_eq!(&t[376..], &TOUCHPAD_TLC_TAIL);

        let t = touchpad_tlc_spi(TlcUsage::TouchScreen, AXES);
        let fc = finger_collection_spi(AXES);
        for idx in 0..5 {
            let start =
                TOUCHPAD_TLC_HEADER_LEN + idx * (FINGER_COLLECTION_SPI_LEN + FINGER_SEPARATOR_LEN);
            assert_eq!(&t[start..start + FINGER_COLLECTION_SPI_LEN], &fc);
        }
        assert_eq!(&t[361..], &TOUCHPAD_TLC_TAIL);
        assert_eq!(t[3], 0x04);
    }

    #[test]
    fn tlc_usage_byte() {
        assert_eq!(TlcUsage::TouchPad.usage(), 0x05);
        assert_eq!(TlcUsage::TouchScreen.usage(), 0x04);
        assert_eq!(touchpad_tlc_header(TlcUsage::TouchPad)[3], 0x05);
    }

    #[test]
    fn shared_tlc_bytes() {
        assert_eq!(
            WINDOWS_CONFIGURATION_TLC,
            [
                0x05, 0x0d, 0x09, 0x0e, 0xa1, 0x01, 0x85, 0x04, 0x09, 0x22, 0xa1, 0x02, 0x09, 0x52,
                0x15, 0x00, 0x25, 0x03, 0x75, 0x08, 0x95, 0x01, 0xb1, 0x02, 0xc0, 0xa1, 0x00, 0x85,
                0x06, 0x09, 0x57, 0x09, 0x58, 0x75, 0x01, 0x95, 0x02, 0x25, 0x01, 0xb1, 0x02, 0x95,
                0x06, 0xb1, 0x03, 0xc0, 0xc0
            ]
        );
    }

    #[test]
    fn collections_are_balanced() {
        // Walk the item stream of a full descriptor: every item must be well-formed and collections
        // must nest back to depth 0 at each TLC boundary.
        fn walk(d: &[u8]) -> usize {
            let mut i = 0;
            let mut depth: i32 = 0;
            let mut tlcs = 0;
            while i < d.len() {
                let prefix = d[i];
                let size = match prefix & 3 {
                    3 => 4,
                    s => s as usize,
                };
                match prefix {
                    BEGIN_COLLECTION => {
                        if depth == 0 {
                            tlcs += 1;
                        }
                        depth += 1;
                    }
                    END_COLLECTION => {
                        depth -= 1;
                        assert!(depth >= 0);
                    }
                    _ => {}
                }
                i += 1 + size;
            }
            assert_eq!(i, d.len());
            assert_eq!(depth, 0);
            tlcs
        }
        assert_eq!(walk(&report_descriptor_u32_id(AXES)), 2);
        assert_eq!(walk(&report_descriptor_spi(TlcUsage::TouchPad, AXES)), 2);
    }
}

//! USB model selection shared by enumeration, mode switching, input decoding and HID descriptors.

pub use crate::bcm5974::config::{Bcm5974Config, Bcm5974Param, TrackpadType};
use amtptp_core::hid::descriptor::{Axes, REPORT_DESCRIPTOR_U32_ID_LEN, report_descriptor_u32_id};

/// USB firmware behavior shared by each group of product IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Firmware {
    /// Original Wellspring or Magic Trackpad 2 mode recovery and tip detection.
    Legacy,
    /// T2 touch controller with an additional minor-axis tip threshold.
    T2,
}

/// Geometry, descriptor and contact threshold for a group of USB hardware IDs.
#[derive(Debug)]
pub struct UsbModel {
    /// Primary USB product ID.
    pub identification: u16,
    /// Other product IDs with the same input format and geometry.
    pub aliases: &'static [u16],
    /// Packet format and sensor bounds.
    pub profile: Bcm5974Config,
    /// Firmware-specific input behavior.
    pub firmware: Firmware,
    /// HID report descriptor built from this model's sensor bounds.
    pub descriptor: [u8; REPORT_DESCRIPTOR_U32_ID_LEN],
}

impl UsbModel {
    const fn new(
        id: u16,
        aliases: &'static [u16],
        format: TrackpadType,
        x: (i32, i32),
        y: (i32, i32),
        physical: (u16, u16),
        firmware: Firmware,
    ) -> Self {
        Self {
            identification: id,
            aliases,
            profile: Bcm5974Config {
                tp_type: format,
                x: Bcm5974Param { min: x.0, max: x.1 },
                y: Bcm5974Param { min: y.0, max: y.1 },
            },
            firmware,
            descriptor: report_descriptor_u32_id(Axes {
                x_logical_max: (x.1 - x.0) as u16,
                y_logical_max: (y.1 - y.0) as u16,
                x_physical_max: physical.0,
                y_physical_max: physical.1,
            }),
        }
    }

    /// Whether this row covers the USB product ID.
    #[must_use]
    pub fn matches(&self, pid: u16) -> bool {
        self.identification == pid || self.aliases.contains(&pid)
    }
}
use TrackpadType::{Type2, Type3, Type4, Type5};
/// Generic geometry used for the explicitly listed fallback devices.
pub const FALLBACK_CONFIG: UsbModel = UsbModel::new(
    0xffff,
    &[],
    Type4,
    (-10000, 10000),
    (-2000, 10000),
    (1300, 850),
    Firmware::T2,
);
/// First 13-inch T2 model and its HID descriptor.
pub const T2_13_CONFIG: UsbModel = UsbModel::new(
    0x027a,
    &[],
    Type4,
    (-6243, 6749),
    (-170, 7685),
    (1352, 836),
    Firmware::T2,
);
/// USB models, indexed once at `PrepareHardware`.
#[rustfmt::skip]
pub static CONFIG_TABLE: &[UsbModel] = &[
    FALLBACK_CONFIG,
    T2_13_CONFIG,
    UsbModel { identification: 0x027b, ..T2_13_CONFIG },
    UsbModel { identification: 0x027c, ..FALLBACK_CONFIG },
    UsbModel { identification: 0x027d, ..FALLBACK_CONFIG },
    UsbModel::new(0x0236, &[0x0237, 0x0238], Type2, (-4460, 5166), (-75, 6700), (1050, 760), Firmware::Legacy),
    UsbModel::new(0x0245, &[0x0246, 0x0247], Type2, (-4415, 5050), (-55, 6680), (1050, 810), Firmware::Legacy),
    UsbModel::new(0x024c, &[0x024d, 0x024e, 0x0249, 0x024a, 0x024b], Type2, (-4620, 5140), (-150, 6600), (1067, 762), Firmware::Legacy),
    UsbModel::new(0x0252, &[0x0253, 0x0254, 0x0262, 0x0263, 0x0264, 0x0259, 0x025a, 0x025b], Type2, (-4750, 5280), (-150, 6730), (1064, 773), Firmware::Legacy),
    UsbModel::new(0x0290, &[0x0291, 0x0292], Type3, (-4620, 5140), (-150, 6600), (1045, 750), Firmware::Legacy),
    UsbModel::new(0x0272, &[0x0273, 0x0274], Type4, (-4828, 5345), (-203, 6803), (1045, 750), Firmware::Legacy),
    UsbModel::new(0x0265, &[], Type5, (-3678, 3934), (-2479, 2586), (1600, 1149), Firmware::Legacy),
];
/// T2 PIDs using the generic geometry.
pub const FALLBACK_PIDS: [u16; 5] = [0x0277, 0x027e, 0x027f, 0x0280, 0x0340];

/// Index of the generic fallback row ([`FALLBACK_CONFIG`]) that unknown PIDs get.
pub const FALLBACK_CONFIG_INDEX: usize = 0;

/// Whether `product_id` is one of the [`FALLBACK_PIDS`].
#[must_use]
pub fn uses_listed_fallback(product_id: u16) -> bool {
    FALLBACK_PIDS.contains(&product_id)
}

/// `AmtPtpGetDeviceConfig`: the index of the row whose `identification` equals `product_id`, scanning
/// from row 0; [`FALLBACK_CONFIG_INDEX`] when no row matches (the C traces "Selected a generic fallback
/// configuration" in that case).
#[must_use]
pub fn config_index_for_pid(product_id: u16) -> usize {
    CONFIG_TABLE
        .iter()
        .position(|row| row.matches(product_id))
        .unwrap_or(FALLBACK_CONFIG_INDEX)
}

/// The row for `index`, or `None` if out of range.
#[must_use]
pub fn config_by_index(index: usize) -> Option<&'static UsbModel> {
    CONFIG_TABLE.get(index)
}

/// `AmtPtpGetDeviceConfig`: the row for `product_id` (never fails; unknown PIDs get the fallback row).
#[must_use]
pub fn config_for_pid(product_id: u16) -> &'static UsbModel {
    config_by_index(config_index_for_pid(product_id)).unwrap_or(&FALLBACK_CONFIG)
}

/// `transferLength` of `AmtPtpConfigContReaderForInterruptEndPoint` (Interrupt.c:30-47):
/// `HEADER_TYPEn + FSIZE_TYPEn * MAX_FINGERS` (526 for TYPE4). `size_t` arithmetic on positive constants.
#[must_use]
pub const fn reader_transfer_length(tp_type: TrackpadType) -> usize {
    tp_type.reader_transfer_length()
}

const _: () = assert!(reader_transfer_length(TrackpadType::Type4) == 526);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_and_fallback() {
        assert_eq!(config_index_for_pid(0x027A), 1);
        assert_eq!(config_index_for_pid(0x027B), 2);
        assert_eq!(config_index_for_pid(0x027C), 3);
        assert_eq!(config_index_for_pid(0x027D), 4);
        assert_eq!(config_index_for_pid(0xFFFF), 0);
        for pid in [0x0000, 0x0340, 0x8000] {
            assert_eq!(config_index_for_pid(pid), FALLBACK_CONFIG_INDEX);
            assert_eq!(config_for_pid(pid).identification, 0xFFFF);
        }
        assert_eq!(config_for_pid(0x027A).profile.x.min, -6243);
        assert_eq!(config_for_pid(0x027A).profile.y.max, 7685);
        assert!(config_by_index(CONFIG_TABLE.len()).is_none());
    }

    #[test]
    fn listed_fallback_pids_have_no_row() {
        for pid in FALLBACK_PIDS {
            assert_eq!(
                config_index_for_pid(pid),
                FALLBACK_CONFIG_INDEX,
                "{pid:04x}"
            );
            assert!(uses_listed_fallback(pid));
        }
        for row in &CONFIG_TABLE[1..] {
            assert!(!uses_listed_fallback(row.identification));
        }
    }

    #[test]
    fn reader_lengths() {
        assert_eq!(reader_transfer_length(TrackpadType::Type1), 474);
        assert_eq!(reader_transfer_length(TrackpadType::Type2), 478);
        assert_eq!(reader_transfer_length(TrackpadType::Type3), 486);
        assert_eq!(reader_transfer_length(TrackpadType::Type4), 526);
        assert_eq!(reader_transfer_length(TrackpadType::Type5), 156);
    }
}

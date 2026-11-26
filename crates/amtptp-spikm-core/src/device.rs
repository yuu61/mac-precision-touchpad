//! Device identification: `SpiTrackpadConfigTable`, the PID-to-family switch, `REPORT_TYPE`, and the
//! `PrepareHardware` logic that fills the device configuration (C `DEVICE_CONTEXT` fields `HidVendorID`,
//! `HidProductID`, `HidVersionNumber`, `TrackpadInfo` and `ReportType`).

use core::sync::atomic::{AtomicI16, AtomicU16, AtomicU32, Ordering};

use ptp_hid::hidport::{HID_DEVICE_ATTRIBUTES_LEN, HidDeviceAttributes};

use crate::status::{NtStatus, STATUS_NOT_FOUND, nt_success};

/// Apple's USB/SPI vendor ID (0x05AC), the `VendorId` of every config-table entry.
pub const APPLE_VENDOR_ID: u16 = 0x05ac;

/// `SPI_TRACKPAD_INFO` (Device.h): the coordinate range of one trackpad model.
///
/// Only `x_min` and `y_max` are used by the input path (spec §7.3); `x_max` and `y_min` are stored but
/// never read, as in C.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpiTrackpadInfo {
    /// `VendorId`.
    pub vendor_id: u16,
    /// `ProductId`.
    pub product_id: u16,
    /// `XMin`: subtracted from the raw X coordinate.
    pub x_min: i16,
    /// `XMax`: never read.
    pub x_max: i16,
    /// `YMin`: never read.
    pub y_min: i16,
    /// `YMax`: the raw Y coordinate is subtracted from it (Y is inverted).
    pub y_max: i16,
}

impl SpiTrackpadInfo {
    /// The all-zero value the KMDF-zeroed device context starts with.
    pub const ZERO: Self = Self {
        vendor_id: 0,
        product_id: 0,
        x_min: 0,
        x_max: 0,
        y_min: 0,
        y_max: 0,
    };

    const fn apple(product_id: u16, x_min: i16, x_max: i16, y_min: i16, y_max: i16) -> Self {
        Self {
            vendor_id: APPLE_VENDOR_ID,
            product_id,
            x_min,
            x_max,
            y_min,
            y_max,
        }
    }
}

/// Number of real entries in [`SPI_TRACKPAD_CONFIG_TABLE`].
pub const SPI_TRACKPAD_CONFIG_COUNT: usize = 9;

/// `SpiTrackpadConfigTable` (AppleDefinition.h:52-71) without its `{0}` terminator, in C order.
///
/// PID 0x0280 is bound by the INF but absent here; such a device fails `PrepareHardware` with
/// `STATUS_NOT_FOUND` (preserved known issue SPI-B17).
pub static SPI_TRACKPAD_CONFIG_TABLE: [SpiTrackpadInfo; SPI_TRACKPAD_CONFIG_COUNT] = [
    // MacBookPro11,1 / MacBookPro12,1
    SpiTrackpadInfo::apple(0x0272, -4750, 5280, -150, 6730),
    SpiTrackpadInfo::apple(0x0273, -4750, 5280, -150, 6730),
    // MacBook9
    SpiTrackpadInfo::apple(0x0275, -5087, 5579, -128, 6089),
    // MacBookPro14,1 / MacBookPro14,2
    SpiTrackpadInfo::apple(0x0276, -6243, 6749, -170, 7685),
    SpiTrackpadInfo::apple(0x0277, -6243, 6749, -170, 7685),
    // MacBookPro14,3
    SpiTrackpadInfo::apple(0x0278, -7456, 7976, -163, 9283),
    // MacBook10
    SpiTrackpadInfo::apple(0x0279, -5087, 5579, -128, 6089),
    // MacBookAir7,2 fallback
    SpiTrackpadInfo::apple(0x0290, -5087, 5579, -128, 6089),
    SpiTrackpadInfo::apple(0x0291, -5087, 5579, -128, 6089),
];

/// The `PrepareHardware` table lookup (Device.c:244-259): the first entry whose `VendorId` **and**
/// `ProductId` both match.
///
/// C walks the table until the `{0}` terminator. No real entry has `VendorId == 0`, so iterating the 9
/// entries gives the same result for every input, including `vendor_id == 0` (no match), without any
/// sentinel-based indexing.
#[must_use]
pub fn lookup_trackpad(vendor_id: u16, product_id: u16) -> Option<&'static SpiTrackpadInfo> {
    SPI_TRACKPAD_CONFIG_TABLE
        .iter()
        .find(|e| e.vendor_id == vendor_id && e.product_id == product_id)
}

/// The descriptor family selected by the `HidProductID` switches in `AmtPtpGetHidDescriptor` and
/// `AmtPtpGetReportDescriptor` (Hid.c:147-282, 403-475). The switch is independent of the config table,
/// but covers the same 9 PIDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// Series 1: PIDs 0x0275, 0x0279, 0x0290, 0x0291 (MacBook9/10, MacBookAir7,2 fallback).
    F1,
    /// Series 2: PIDs 0x0272, 0x0273 (MacBookPro11,1/12,1).
    F2,
    /// Series 3, 13-inch: PIDs 0x0276, 0x0277 (MacBookPro14,1/14,2).
    F3a,
    /// Series 3, 15-inch: PID 0x0278 (MacBookPro14,3).
    F3b,
}

/// The `HidProductID` switch: which descriptor family a PID uses, or `None` for the `default:` branch.
#[must_use]
pub const fn family_for_product(product_id: u16) -> Option<Family> {
    match product_id {
        0x0275 | 0x0279 | 0x0290 | 0x0291 => Some(Family::F1),
        0x0272 | 0x0273 => Some(Family::F2),
        0x0276 | 0x0277 => Some(Family::F3a),
        0x0278 => Some(Family::F3b),
        _ => None,
    }
}

/// `REPORT_TYPE` (Device.h:40-44), kept as the raw 32-bit enum value.
///
/// The driver only ever stores 0 or 1, but the value is raw so that any other value behaves like C:
/// `IOCTL_HID_GET_REPORT_DESCRIPTOR` answers `STATUS_NOT_FOUND`, while `IOCTL_HID_GET_DEVICE_DESCRIPTOR`
/// does not look at it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReportType(pub u32);

impl ReportType {
    /// `PrecisionTouchpad` (0): the Touch Pad descriptor. The zeroed context starts with it.
    pub const PRECISION_TOUCHPAD: Self = Self(0);
    /// `Touchscreen` (1): the Touch Screen descriptor variant.
    pub const TOUCHSCREEN: Self = Self(1);
    /// `InvalidReportType` (0x7fffffff): a sizing sentinel that is never stored.
    pub const INVALID: Self = Self(0x7fff_ffff);
}

/// The configuration `PrepareHardware` fills in: the C `DEVICE_CONTEXT` fields `HidVendorID`,
/// `HidProductID`, `HidVersionNumber`, `TrackpadInfo` and `ReportType`.
///
/// It is written only at `PASSIVE_LEVEL` in `PrepareHardware` and read by the IOCTL handlers and the read
/// completion routine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceConfig {
    /// `HidVendorID`: the real VID from the lower `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (never read by C).
    pub hid_vendor_id: u16,
    /// `HidProductID`: the real PID; selects the descriptors and is reported in the attributes.
    pub hid_product_id: u16,
    /// `HidVersionNumber`: the real version, reported in the attributes.
    pub hid_version_number: u16,
    /// `TrackpadInfo`: the matched config-table entry (all zero until a match).
    pub trackpad_info: SpiTrackpadInfo,
    /// `ReportType`: which report-descriptor variant to return.
    pub report_type: ReportType,
}

impl DeviceConfig {
    /// The zero-filled state KMDF gives a new device context.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hid_vendor_id: 0,
            hid_product_id: 0,
            hid_version_number: 0,
            trackpad_info: SpiTrackpadInfo::ZERO,
            report_type: ReportType::PRECISION_TOUCHPAD,
        }
    }

    /// `PrepareHardware` steps 1-3 (Device.c:210-265), after the synchronous lower
    /// `IOCTL_HID_GET_DEVICE_ATTRIBUTES` returned `lower_status` and filled `lower_attributes` (a
    /// zero-initialized 32-byte `HID_DEVICE_ATTRIBUTES`).
    ///
    /// - `!NT_SUCCESS(lower_status)` (warnings such as `STATUS_BUFFER_OVERFLOW` included): returns that
    ///   status and changes nothing.
    /// - Otherwise stores `HidVendorID`, `HidProductID` and `HidVersionNumber` **before** the lookup (so they
    ///   hold the lower values even on failure), then copies the matching table entry into
    ///   `trackpad_info`. No match returns `STATUS_NOT_FOUND` and leaves `trackpad_info` as it was.
    ///
    /// On `Err` the glue returns the status from `EvtDevicePrepareHardware` **without** touching the registry.
    ///
    /// # Errors
    /// Returns a failed `lower_status` or `STATUS_NOT_FOUND` for unsupported hardware.
    pub fn identify(
        &mut self,
        lower_status: NtStatus,
        lower_attributes: &[u8; HID_DEVICE_ATTRIBUTES_LEN],
    ) -> Result<(), NtStatus> {
        if !nt_success(lower_status) {
            return Err(lower_status);
        }
        let Some(attrs) = HidDeviceAttributes::from_bytes(lower_attributes) else {
            // Unreachable: the array is exactly HID_DEVICE_ATTRIBUTES_LEN bytes.
            return Err(STATUS_NOT_FOUND);
        };
        self.hid_vendor_id = attrs.vendor_id;
        self.hid_product_id = attrs.product_id;
        self.hid_version_number = attrs.version_number;
        match lookup_trackpad(attrs.vendor_id, attrs.product_id) {
            Some(info) => {
                self.trackpad_info = *info;
                Ok(())
            }
            None => Err(STATUS_NOT_FOUND),
        }
    }

    /// `PrepareHardware` step 4 (Device.c:268-303): applies the `DesiredReportType` value, if one was read
    /// (see [`desired_report_type_value`]).
    ///
    /// `Some(0)` selects [`ReportType::PRECISION_TOUCHPAD`], `Some(1)` [`ReportType::TOUCHSCREEN`]; any
    /// other value, and `None` (missing key/value, or a value larger than 4 bytes), leaves `report_type`
    /// unchanged. That keeps the previous value on a re-start of the same device (preserved behavior).
    /// `PrepareHardware` then returns `STATUS_SUCCESS` regardless.
    pub const fn apply_desired_report_type(&mut self, value: Option<u32>) {
        match value {
            Some(0) => self.report_type = ReportType::PRECISION_TOUCHPAD,
            Some(1) => self.report_type = ReportType::TOUCHSCREEN,
            // C: Status = STATUS_INVALID_PARAMETER, then discarded.
            _ => {}
        }
    }
}

/// The [`DeviceConfig`] as it lives in the device context: one atomic per field.
///
/// `PrepareHardware` writes the configuration at `PASSIVE_LEVEL`, and it runs again on every restart of the
/// same WDFDEVICE (stop/start, rebalance). The IOCTL handlers and the lower-read completion routine read
/// it at up to `DISPATCH_LEVEL`. They are not quiesced while `PrepareHardware` runs, because the default queue
/// is not power-managed and `D0Exit` does not cancel in-flight lower reads. C used plain fields, which is a
/// data race (SPI-B15). Here every field is its own atomic, so each field keeps C's single-variable
/// semantics (P2). Readers take one [`load`](Self::load) snapshot per request.
///
/// `Relaxed` ordering is enough: the values only need to be untorn per field, and the PnP start IRP
/// completing before hidclass sends its first IOCTL already orders the first write before every read.
#[derive(Debug, Default)]
pub struct SharedDeviceConfig {
    hid_vendor_id: AtomicU16,
    hid_product_id: AtomicU16,
    hid_version_number: AtomicU16,
    info_vendor_id: AtomicU16,
    info_product_id: AtomicU16,
    info_x_min: AtomicI16,
    info_x_max: AtomicI16,
    info_y_min: AtomicI16,
    info_y_max: AtomicI16,
    report_type: AtomicU32,
}

impl SharedDeviceConfig {
    /// The zero-filled state of a new device context ([`DeviceConfig::new`]).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            hid_vendor_id: AtomicU16::new(0),
            hid_product_id: AtomicU16::new(0),
            hid_version_number: AtomicU16::new(0),
            info_vendor_id: AtomicU16::new(0),
            info_product_id: AtomicU16::new(0),
            info_x_min: AtomicI16::new(0),
            info_x_max: AtomicI16::new(0),
            info_y_min: AtomicI16::new(0),
            info_y_max: AtomicI16::new(0),
            report_type: AtomicU32::new(ReportType::PRECISION_TOUCHPAD.0),
        }
    }

    /// A snapshot of every field (each one read atomically).
    pub fn load(&self) -> DeviceConfig {
        DeviceConfig {
            hid_vendor_id: self.hid_vendor_id.load(Ordering::Relaxed),
            hid_product_id: self.hid_product_id.load(Ordering::Relaxed),
            hid_version_number: self.hid_version_number.load(Ordering::Relaxed),
            trackpad_info: self.trackpad_info(),
            report_type: ReportType(self.report_type.load(Ordering::Relaxed)),
        }
    }

    /// `TrackpadInfo` only (what the lower-read completion routine reads).
    pub fn trackpad_info(&self) -> SpiTrackpadInfo {
        SpiTrackpadInfo {
            vendor_id: self.info_vendor_id.load(Ordering::Relaxed),
            product_id: self.info_product_id.load(Ordering::Relaxed),
            x_min: self.info_x_min.load(Ordering::Relaxed),
            x_max: self.info_x_max.load(Ordering::Relaxed),
            y_min: self.info_y_min.load(Ordering::Relaxed),
            y_max: self.info_y_max.load(Ordering::Relaxed),
        }
    }

    /// Stores every field of `config` (each one atomically). Only `PrepareHardware` calls this.
    pub fn store(&self, config: &DeviceConfig) {
        self.hid_vendor_id
            .store(config.hid_vendor_id, Ordering::Relaxed);
        self.hid_product_id
            .store(config.hid_product_id, Ordering::Relaxed);
        self.hid_version_number
            .store(config.hid_version_number, Ordering::Relaxed);
        let info = &config.trackpad_info;
        self.info_vendor_id.store(info.vendor_id, Ordering::Relaxed);
        self.info_product_id
            .store(info.product_id, Ordering::Relaxed);
        self.info_x_min.store(info.x_min, Ordering::Relaxed);
        self.info_x_max.store(info.x_max, Ordering::Relaxed);
        self.info_y_min.store(info.y_min, Ordering::Relaxed);
        self.info_y_max.store(info.y_max, Ordering::Relaxed);
        self.report_type
            .store(config.report_type.0, Ordering::Relaxed);
    }
}

/// Name of the registry value read from the driver's `Parameters` key (driver-wide, not per device).
pub const DESIRED_REPORT_TYPE_VALUE_NAME: &str = "DesiredReportType";
/// `ValueLength` passed to `WdfRegistryQueryValue`: `sizeof(ULONG)`.
pub const DESIRED_REPORT_TYPE_VALUE_LEN: usize = 4;
/// `KEY_READ` (0x20019), the access mask for `WdfDriverOpenParametersRegistryKey`.
pub const PARAMETERS_KEY_ACCESS: u32 = 0x0002_0019;

/// Interprets the `WdfRegistryQueryValue(key, "DesiredReportType", 4, &value, ...)` result.
///
/// `data` must be the 4-byte value buffer **zero-initialized before the query** (SPI-B24, P2: C left it
/// uninitialized, so a 1-3 byte value had garbage upper bytes). The value type and returned length are not
/// checked, as in C, so any value of up to 4 bytes is accepted. Returns `None` when the query failed
/// (`!NT_SUCCESS`, e.g. missing value or `STATUS_BUFFER_OVERFLOW` for a larger value). When the key could
/// not be opened, the glue passes `None` to [`DeviceConfig::apply_desired_report_type`] directly.
#[must_use]
pub const fn desired_report_type_value(
    query_status: NtStatus,
    data: [u8; DESIRED_REPORT_TYPE_VALUE_LEN],
) -> Option<u32> {
    if nt_success(query_status) {
        Some(u32::from_le_bytes(data))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_spec_section_8() {
        let pids: [u16; 9] = [
            0x0272, 0x0273, 0x0275, 0x0276, 0x0277, 0x0278, 0x0279, 0x0290, 0x0291,
        ];
        for (entry, pid) in SPI_TRACKPAD_CONFIG_TABLE.iter().zip(pids) {
            assert_eq!(entry.vendor_id, APPLE_VENDOR_ID);
            assert_eq!(entry.product_id, pid);
            assert!(family_for_product(pid).is_some());
        }
        assert_eq!(lookup_trackpad(0x05ac, 0x0280), None);
        assert_eq!(lookup_trackpad(0, 0), None);
        assert_eq!(family_for_product(0x0280), None);
    }

    #[test]
    fn identify_stores_ids_before_lookup() {
        let mut cfg = DeviceConfig::new();
        let mut attrs = [0_u8; 32];
        attrs[..10].copy_from_slice(&[0x20, 0, 0, 0, 0xac, 0x05, 0x80, 0x02, 0x02, 0x01]);
        assert_eq!(cfg.identify(0, &attrs), Err(STATUS_NOT_FOUND));
        assert_eq!(cfg.hid_product_id, 0x0280);
        assert_eq!(cfg.hid_version_number, 0x0102);
        assert_eq!(cfg.trackpad_info, SpiTrackpadInfo::ZERO);
    }

    #[test]
    fn shared_config_round_trips_every_field() {
        let shared = SharedDeviceConfig::new();
        assert_eq!(shared.load(), DeviceConfig::new());
        let cfg = DeviceConfig {
            hid_vendor_id: 0x05ac,
            hid_product_id: 0x0278,
            hid_version_number: 0x0102,
            trackpad_info: SpiTrackpadInfo::apple(0x0278, -7456, 7976, -163, 9283),
            report_type: ReportType::TOUCHSCREEN,
        };
        shared.store(&cfg);
        assert_eq!(shared.load(), cfg);
        assert_eq!(shared.trackpad_info(), cfg.trackpad_info);
        // A raw report type other than 0/1 survives the round trip (it selects no report descriptor).
        let odd = DeviceConfig {
            report_type: ReportType::INVALID,
            ..cfg
        };
        shared.store(&odd);
        assert_eq!(shared.load().report_type, ReportType::INVALID);
    }
}

//! The driver-owned part of `DEVICE_CONTEXT` (Device.h:6-29), with atomic fields.
//!
//! The C driver has no locking at all (UM-B22): the parallel default queue, the continuous-reader
//! completions (several pending reads) and the PnP/power callbacks read and write the context fields
//! concurrently. Plain fields would be a data race in Rust, so every field is an atomic accessed with
//! `Relaxed` ordering (P2: same per-field semantics as the C plain loads and stores, without UB).
//! Field-wise updates are kept: no handler reads and writes back the whole state, so concurrent updates
//! of unrelated fields cannot be lost.
//!
//! The context is zero-filled by WDF in C; [`DeviceState::new`] gives the same all-zero state.

use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU16, AtomicU32, Ordering};

use crate::config::{Bcm5974Config, config_row};

const RELAXED: Ordering = Ordering::Relaxed;

/// Marker in [`DeviceState`] for "no config row" (C: `DeviceInfo == NULL`).
const NO_ROW: u8 = u8::MAX;

/// The fields of the cached `USB_DEVICE_DESCRIPTOR` the driver uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsbDeviceDescriptor {
    /// `idVendor` (only traced; never checked, UM-B23).
    pub id_vendor: u16,
    /// `idProduct`: config lookup, descriptor switch, `HID_DEVICE_ATTRIBUTES.ProductID`.
    pub id_product: u16,
    /// `bcdDevice` (unused).
    pub bcd_device: u16,
    /// `iManufacturer`: string index for `HID_STRING_ID_IMANUFACTURER`.
    pub i_manufacturer: u8,
    /// `iProduct`: string index for `HID_STRING_ID_IPRODUCT`.
    pub i_product: u8,
    /// `iSerialNumber`: string index for `HID_STRING_ID_ISERIALNUMBER`.
    pub i_serial_number: u8,
}

/// Length of a `USB_DEVICE_DESCRIPTOR`.
pub const USB_DEVICE_DESCRIPTOR_LEN: usize = 18;

impl UsbDeviceDescriptor {
    /// Decodes the fields used by the driver from the 18-byte little-endian `USB_DEVICE_DESCRIPTOR`.
    pub const fn from_bytes(bytes: &[u8; USB_DEVICE_DESCRIPTOR_LEN]) -> Self {
        let [
            _,
            _,
            _,
            _,
            _,
            _,
            _,
            _,
            v0,
            v1,
            p0,
            p1,
            d0,
            d1,
            i_manufacturer,
            i_product,
            i_serial_number,
            _,
        ] = *bytes;
        Self {
            id_vendor: u16::from_le_bytes([v0, v1]),
            id_product: u16::from_le_bytes([p0, p1]),
            bcd_device: u16::from_le_bytes([d0, d1]),
            i_manufacturer,
            i_product,
            i_serial_number,
        }
    }
}

/// A plain copy of the mutable driver state, for tracing and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StateSnapshot {
    /// `IsWellspringModeOn`.
    pub wellspring_mode_on: bool,
    /// `IsSurfaceReportOn` (set from bit 1 of feature 0x06, UM-B14).
    pub surface_report_on: bool,
    /// `IsButtonReportOn` (set from bit 0 of feature 0x06, UM-B14; also gates D0Entry, UM-B10).
    pub button_report_on: bool,
    /// `PressureQualLevel` (dead state, UM-B15).
    pub pressure_qual_level: u8,
    /// `SgContactSizeQualLevel` (dead state, UM-B15).
    pub sg_contact_size_qual_level: u8,
    /// `MuContactSizeQualLevel` (dead state, UM-B15).
    pub mu_contact_size_qual_level: u8,
    /// `PerfCounter.QuadPart`: QPC value at the last D0Entry (ScanTime base, UM-B12).
    pub perf_counter: i64,
}

/// Driver state of one device (the non-WDF fields of `DEVICE_CONTEXT`, plus the B16 reader flag).
///
/// Embed one in the WDF device context and pass `&DeviceState` to every core entry point. All methods
/// take `&self`.
#[derive(Debug)]
pub struct DeviceState {
    config_row: AtomicU8,
    id_vendor: AtomicU16,
    id_product: AtomicU16,
    bcd_device: AtomicU16,
    i_manufacturer: AtomicU8,
    i_product: AtomicU8,
    i_serial_number: AtomicU8,
    usb_device_traits: AtomicU32,
    pressure_qual_level: AtomicU8,
    sg_contact_size_qual_level: AtomicU8,
    mu_contact_size_qual_level: AtomicU8,
    is_wellspring_mode_on: AtomicBool,
    is_surface_report_on: AtomicBool,
    is_button_report_on: AtomicBool,
    perf_counter: AtomicI64,
    reader_configured: AtomicBool,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceState {
    /// The zero-filled state of a freshly created WDF device context (no config row, all flags FALSE,
    /// all levels 0, `PerfCounter` 0, reader not configured).
    pub const fn new() -> Self {
        Self {
            config_row: AtomicU8::new(NO_ROW),
            id_vendor: AtomicU16::new(0),
            id_product: AtomicU16::new(0),
            bcd_device: AtomicU16::new(0),
            i_manufacturer: AtomicU8::new(0),
            i_product: AtomicU8::new(0),
            i_serial_number: AtomicU8::new(0),
            usb_device_traits: AtomicU32::new(0),
            pressure_qual_level: AtomicU8::new(0),
            sg_contact_size_qual_level: AtomicU8::new(0),
            mu_contact_size_qual_level: AtomicU8::new(0),
            is_wellspring_mode_on: AtomicBool::new(false),
            is_surface_report_on: AtomicBool::new(false),
            is_button_report_on: AtomicBool::new(false),
            perf_counter: AtomicI64::new(0),
            reader_configured: AtomicBool::new(false),
        }
    }

    // --- config row (DeviceInfo) -------------------------------------------------------------------

    /// `DeviceInfo`: the config-table row selected by the last PrepareHardware, or `None` (C: `NULL`).
    pub fn config(&self) -> Option<&'static Bcm5974Config> {
        config_row(self.config_row.load(RELAXED))
    }

    /// The selected config-table row index, or `None`.
    pub fn config_row(&self) -> Option<u8> {
        let row = self.config_row.load(RELAXED);
        config_row(row).map(|_| row)
    }

    /// Sets `DeviceInfo` (`None` = `NULL`).
    pub fn set_config_row(&self, row: Option<u8>) {
        self.config_row.store(row.unwrap_or(NO_ROW), RELAXED);
    }

    // --- cached device descriptor -------------------------------------------------------------------

    /// The cached device-descriptor fields.
    pub fn device_descriptor(&self) -> UsbDeviceDescriptor {
        UsbDeviceDescriptor {
            id_vendor: self.id_vendor.load(RELAXED),
            id_product: self.id_product.load(RELAXED),
            bcd_device: self.bcd_device.load(RELAXED),
            i_manufacturer: self.i_manufacturer.load(RELAXED),
            i_product: self.i_product.load(RELAXED),
            i_serial_number: self.i_serial_number.load(RELAXED),
        }
    }

    /// `DeviceDescriptor.idProduct`.
    pub fn id_product(&self) -> u16 {
        self.id_product.load(RELAXED)
    }

    /// Stores the device descriptor (`WdfUsbTargetDeviceGetDeviceDescriptor`, Device.c:166-169).
    pub fn set_device_descriptor(&self, descriptor: &UsbDeviceDescriptor) {
        self.id_vendor.store(descriptor.id_vendor, RELAXED);
        self.id_product.store(descriptor.id_product, RELAXED);
        self.bcd_device.store(descriptor.bcd_device, RELAXED);
        self.i_manufacturer
            .store(descriptor.i_manufacturer, RELAXED);
        self.i_product.store(descriptor.i_product, RELAXED);
        self.i_serial_number
            .store(descriptor.i_serial_number, RELAXED);
    }

    // --- traits ----------------------------------------------------------------------------------------

    /// `UsbDeviceTraits` (write-only in C; kept for tracing).
    pub fn usb_device_traits(&self) -> u32 {
        self.usb_device_traits.load(RELAXED)
    }

    /// Sets `UsbDeviceTraits`.
    pub fn set_usb_device_traits(&self, traits: u32) {
        self.usb_device_traits.store(traits, RELAXED);
    }

    // --- flags -----------------------------------------------------------------------------------------

    /// `IsWellspringModeOn`.
    pub fn is_wellspring_mode_on(&self) -> bool {
        self.is_wellspring_mode_on.load(RELAXED)
    }

    /// Sets `IsWellspringModeOn`.
    pub fn set_wellspring_mode_on(&self, on: bool) {
        self.is_wellspring_mode_on.store(on, RELAXED);
    }

    /// `IsSurfaceReportOn`.
    pub fn is_surface_report_on(&self) -> bool {
        self.is_surface_report_on.load(RELAXED)
    }

    /// Sets `IsSurfaceReportOn`.
    pub fn set_surface_report_on(&self, on: bool) {
        self.is_surface_report_on.store(on, RELAXED);
    }

    /// `IsButtonReportOn`.
    pub fn is_button_report_on(&self) -> bool {
        self.is_button_report_on.load(RELAXED)
    }

    /// Sets `IsButtonReportOn`.
    pub fn set_button_report_on(&self, on: bool) {
        self.is_button_report_on.store(on, RELAXED);
    }

    // --- qualification levels (UM-B15: stored and echoed only) ------------------------------------------

    /// `[PressureQualLevel, SgContactSizeQualLevel, MuContactSizeQualLevel]`.
    pub fn qual_levels(&self) -> [u8; 3] {
        [
            self.pressure_qual_level.load(RELAXED),
            self.sg_contact_size_qual_level.load(RELAXED),
            self.mu_contact_size_qual_level.load(RELAXED),
        ]
    }

    /// Sets `PressureQualLevel`, `SgContactSizeQualLevel`, `MuContactSizeQualLevel` (in the C statement
    /// order: Sg, Mu, Pressure; each store is independent).
    pub fn set_qual_levels(
        &self,
        pressure: u8,
        single_contact_size: u8,
        multiple_contact_size: u8,
    ) {
        self.sg_contact_size_qual_level
            .store(single_contact_size, RELAXED);
        self.mu_contact_size_qual_level
            .store(multiple_contact_size, RELAXED);
        self.pressure_qual_level.store(pressure, RELAXED);
    }

    // --- scan time base -------------------------------------------------------------------------------

    /// `PerfCounter.QuadPart`.
    pub fn perf_counter(&self) -> i64 {
        self.perf_counter.load(RELAXED)
    }

    /// Sets `PerfCounter.QuadPart`.
    pub fn set_perf_counter(&self, value: i64) {
        self.perf_counter.store(value, RELAXED);
    }

    // --- UM-B16 -----------------------------------------------------------------------------------------

    /// Whether the continuous reader has already been configured on the interrupt pipe (P2 fix for
    /// UM-B16: configure it once).
    pub fn reader_configured(&self) -> bool {
        self.reader_configured.load(RELAXED)
    }

    /// Sets the "reader configured" flag.
    pub fn set_reader_configured(&self, configured: bool) {
        self.reader_configured.store(configured, RELAXED);
    }

    // --- snapshots --------------------------------------------------------------------------------------

    /// A plain copy of the mutable fields.
    pub fn snapshot(&self) -> StateSnapshot {
        let [
            pressure_qual_level,
            sg_contact_size_qual_level,
            mu_contact_size_qual_level,
        ] = self.qual_levels();
        StateSnapshot {
            wellspring_mode_on: self.is_wellspring_mode_on(),
            surface_report_on: self.is_surface_report_on(),
            button_report_on: self.is_button_report_on(),
            pressure_qual_level,
            sg_contact_size_qual_level,
            mu_contact_size_qual_level,
            perf_counter: self.perf_counter(),
        }
    }

    /// Overwrites the mutable fields from a snapshot (tests, diagnostics).
    pub fn restore(&self, snapshot: &StateSnapshot) {
        self.set_wellspring_mode_on(snapshot.wellspring_mode_on);
        self.set_surface_report_on(snapshot.surface_report_on);
        self.set_button_report_on(snapshot.button_report_on);
        self.set_qual_levels(
            snapshot.pressure_qual_level,
            snapshot.sg_contact_size_qual_level,
            snapshot.mu_contact_size_qual_level,
        );
        self.set_perf_counter(snapshot.perf_counter);
    }
}

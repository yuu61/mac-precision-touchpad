//! The hardware-independent part of the C `DEVICE_CONTEXT` (Device.h:25-47).
//!
//! The C context has no locking: `AmtPtpEvtUsbInterruptPipeReadComplete` (≤ `DISPATCH_LEVEL`), the I/O
//! dispatcher and the PnP/power callbacks read and write plain fields concurrently. In Rust that is a data
//! race, so every field here is an atomic with `Relaxed` ordering (port policy P2: "atomics with the same
//! semantics"; on x64/ARM64 byte and aligned-qword accesses are what the C compiler emits anyway).
//!
//! WDF handles (`UsbDevice`, `UsbInterface`, `InterruptPipe`, `InputQueue`) stay in the driver's own
//! context. This struct records only whether the USB target exists and *which* pipe index is the interrupt
//! pipe; the glue maps the index to its `WDFUSBPIPE`.
//!
//! | C field | here |
//! |---|---|
//! | `UsbDevice != NULL` | [`DeviceState::usb_device_created`] |
//! | `DeviceDescriptor` (`idVendor`, `idProduct`, `bcdDevice`) | [`DeviceState::usb_device_ids`] |
//! | `UsbDeviceTraits` | [`DeviceState::usb_device_traits`] (dead in C: stored, never read) |
//! | `DeviceInfo` | [`DeviceState::config_index`] / [`DeviceState::config`] |
//! | `InterruptPipe` | [`DeviceState::interrupt_pipe`] (pipe index) |
//! | `IsWellspringModeOn` | [`DeviceState::wellspring_mode_on`] |
//! | `PtpReportTouch`, `PtpReportButton` | [`DeviceState::input_gates`] (always TRUE in the real driver) |
//! | Reporting clock | [`DeviceState::qpc_frequency`] plus the reporting-session origin |
//! | `PtpInputOn` | dropped: never read or written |

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};

use amtptp_core::hid::feature::{SelectiveReportBits, SelectiveReportState};
use amtptp_core::hid::scan_time::ScanClock;

use crate::infrastructure::usb::{
    CONFIG_TABLE, FALLBACK_CONFIG, TrackpadType, UsbModel, config_by_index,
};

const RELAXED: Ordering = Ordering::Relaxed;

/// The three fields of the cached `USB_DEVICE_DESCRIPTOR` the driver uses (`idVendor`, `idProduct`) or
/// keeps for tracing (`bcdDevice`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsbDeviceIds {
    /// `idVendor`. Reported as `HID_DEVICE_ATTRIBUTES.VendorID` (the real 0x05AC, not 0x8910).
    pub vendor_id: u16,
    /// `idProduct`. Selects the config row and is reported as `HID_DEVICE_ATTRIBUTES.ProductID`.
    pub product_id: u16,
    /// `bcdDevice`. Not used (the HID `VersionNumber` is the constant 1).
    pub bcd_device: u16,
}

/// `PtpReportTouch` / `PtpReportButton`: gates of contact and button parsing in the reader completion.
///
/// Both are set at device creation and at the end of every successful `PrepareHardware` (the power-on
/// state of selective reporting), and `SET_FEATURE` 0x06 sets them from the Surface and Button Switches
/// (P3 fix of spec B15: C never changed them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputGates {
    /// `PtpReportTouch`: parse contacts (and apply the more-than-5-fingers `DATA_ERROR` check).
    pub report_touch: bool,
    /// `PtpReportButton`: report the button byte. (C also gated the `D0Entry` mode switch on it; the port
    /// switches the mode unconditionally.)
    pub report_button: bool,
}

impl InputGates {
    /// Both gates on: the only state the real driver is ever in.
    pub const ON: Self = Self {
        report_touch: true,
        report_button: true,
    };
}

/// A plain copy of every [`DeviceState`] field, for tests, tracing and for restoring a state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceStateSnapshot {
    /// `UsbDevice != NULL`.
    pub usb_device_created: bool,
    /// Cached device-descriptor fields.
    pub ids: UsbDeviceIds,
    /// Index into [`CONFIG_TABLE`] (`None` = `DeviceInfo == NULL`, before the first `PrepareHardware`).
    pub config_index: Option<usize>,
    /// `UsbDeviceTraits`.
    pub usb_device_traits: u32,
    /// Index of the configured pipe stored in `InterruptPipe` (`None` = NULL).
    pub interrupt_pipe: Option<u8>,
    /// `IsWellspringModeOn`.
    pub wellspring_mode_on: bool,
    /// `PtpReportTouch`.
    pub report_touch: bool,
    /// `PtpReportButton`.
    pub report_button: bool,
    /// Cached QPC frequency in ticks per second.
    pub qpc_frequency: i64,
}

/// Shared per-device state. Place one in the WDF device context (`ptr.write(DeviceState::new())`) and
/// share `&DeviceState` with every callback; all methods take `&self`.
#[derive(Debug, Default)]
pub struct DeviceState {
    usb_device_created: AtomicBool,
    vendor_id: AtomicU16,
    product_id: AtomicU16,
    bcd_device: AtomicU16,
    /// 0 = `DeviceInfo == NULL`, otherwise row index + 1.
    config_slot: AtomicU8,
    usb_device_traits: AtomicU32,
    /// 0 = `InterruptPipe == NULL`, otherwise pipe index + 1.
    interrupt_pipe_slot: AtomicU16,
    wellspring_mode_on: AtomicBool,
    raw_requested: AtomicBool,
    reporting: SelectiveReportState,
    reader_failures: AtomicU32,
    pub(crate) scan_clock: ScanClock,
}

const _: () = assert!(CONFIG_TABLE.len() < u8::MAX as usize);

impl DeviceState {
    /// The all-zero state that `WdfDeviceCreate` gives the C context (WDF zero-initializes contexts).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            usb_device_created: AtomicBool::new(false),
            vendor_id: AtomicU16::new(0),
            product_id: AtomicU16::new(0),
            bcd_device: AtomicU16::new(0),
            config_slot: AtomicU8::new(0),
            usb_device_traits: AtomicU32::new(0),
            interrupt_pipe_slot: AtomicU16::new(0),
            wellspring_mode_on: AtomicBool::new(false),
            raw_requested: AtomicBool::new(false),
            reporting: SelectiveReportState::new(SelectiveReportBits::from_byte(0)),
            reader_failures: AtomicU32::new(0),
            scan_clock: ScanClock::new(),
        }
    }

    /// A state with the given field values (tests, or restoring a snapshot). The driver starts from
    /// [`DeviceState::new`] followed by [`DeviceState::on_device_create`].
    #[must_use]
    pub fn from_snapshot(s: DeviceStateSnapshot) -> Self {
        let state = Self::new();
        state.restore(s);
        state
    }

    /// Overwrites every field with the values of `s`.
    pub fn restore(&self, s: DeviceStateSnapshot) {
        self.usb_device_created.store(s.usb_device_created, RELAXED);
        self.set_usb_device_ids(s.ids);
        self.config_slot.store(
            s.config_index
                .filter(|&i| i < CONFIG_TABLE.len())
                // Fits: the table has fewer than 255 rows (asserted above).
                .map_or(0, |i| (i as u8).wrapping_add(1)),
            RELAXED,
        );
        self.usb_device_traits.store(s.usb_device_traits, RELAXED);
        self.set_interrupt_pipe(s.interrupt_pipe);
        self.wellspring_mode_on.store(s.wellspring_mode_on, RELAXED);
        self.request_raw(s.wellspring_mode_on);
        self.reporting.store(SelectiveReportBits {
            surface_report: s.report_touch,
            button_report: s.report_button,
        });
        self.scan_clock.restart(s.qpc_frequency);
    }

    /// A plain copy of every field.
    pub fn snapshot(&self) -> DeviceStateSnapshot {
        let reporting = self.reporting.load();
        DeviceStateSnapshot {
            usb_device_created: self.usb_device_created(),
            ids: self.usb_device_ids(),
            config_index: self.config_index(),
            usb_device_traits: self.usb_device_traits(),
            interrupt_pipe: self.interrupt_pipe(),
            wellspring_mode_on: self.wellspring_mode_on(),
            report_touch: reporting.surface_report,
            report_button: reporting.button_report,
            qpc_frequency: self.qpc_frequency(),
        }
    }

    /// `AmtPtpDeviceUsbKmCreateDevice` after a successful `WdfDeviceCreate` (Device.c:106-107):
    /// `PtpReportButton = TRUE; PtpReportTouch = TRUE`.
    pub fn on_device_create(&self) {
        self.set_input_gates_on();
    }

    /// Sets both input gates (`CreateDevice` and the end of `PrepareHardware`): selective reporting's
    /// power-on state.
    pub(crate) fn set_input_gates_on(&self) {
        self.set_selective_reporting(SelectiveReportBits::POWER_ON);
    }

    /// `SET_FEATURE` 0x06: `PtpReportTouch` = Surface Switch, `PtpReportButton` = Button Switch.
    pub(crate) fn set_selective_reporting(&self, bits: SelectiveReportBits) {
        self.reporting.store(bits);
    }

    /// Whether `WdfUsbTargetDeviceCreate` has succeeded (`UsbDevice != NULL`).
    pub fn usb_device_created(&self) -> bool {
        self.usb_device_created.load(RELAXED)
    }

    pub(crate) fn set_usb_device_created(&self) {
        self.usb_device_created.store(true, RELAXED);
    }

    /// The cached device-descriptor fields (all zero before the first `PrepareHardware`, which is what
    /// `IOCTL_HID_GET_DEVICE_ATTRIBUTES` then reports).
    pub fn usb_device_ids(&self) -> UsbDeviceIds {
        UsbDeviceIds {
            vendor_id: self.vendor_id.load(RELAXED),
            product_id: self.product_id.load(RELAXED),
            bcd_device: self.bcd_device.load(RELAXED),
        }
    }

    pub(crate) fn set_usb_device_ids(&self, ids: UsbDeviceIds) {
        self.vendor_id.store(ids.vendor_id, RELAXED);
        self.product_id.store(ids.product_id, RELAXED);
        self.bcd_device.store(ids.bcd_device, RELAXED);
    }

    /// `UsbDeviceTraits` (`WDF_USB_DEVICE_TRAIT_*`, 0 if `WdfUsbTargetDeviceRetrieveInformation` failed).
    /// The C driver stores it and never reads it; it is kept for tracing.
    pub fn usb_device_traits(&self) -> u32 {
        self.usb_device_traits.load(RELAXED)
    }

    pub(crate) fn set_usb_device_traits(&self, traits: u32) {
        self.usb_device_traits.store(traits, RELAXED);
    }

    /// Index of the selected config row, `None` before the first `PrepareHardware` (`DeviceInfo == NULL`).
    pub fn config_index(&self) -> Option<usize> {
        match self.config_slot.load(RELAXED) {
            0 => None,
            slot => Some(usize::from(slot) - 1),
        }
    }

    pub(crate) fn set_config_index(&self, index: usize) {
        let slot = if index < CONFIG_TABLE.len() {
            // Fits: the table has fewer than 255 rows (asserted above).
            (index as u8).wrapping_add(1)
        } else {
            0
        };
        self.config_slot.store(slot, RELAXED);
    }

    /// The selected config row (`DeviceInfo`).
    ///
    /// Before the first `PrepareHardware` the C pointer is NULL and every reader of it would crash; that
    /// cannot happen (the reader and the mode switch only run after `PrepareHardware`). This returns the
    /// fallback row in that case instead of failing.
    pub fn config(&self) -> &'static UsbModel {
        self.config_index()
            .and_then(config_by_index)
            .unwrap_or(&FALLBACK_CONFIG)
    }

    /// Index of the configured pipe stored as `InterruptPipe`, `None` for NULL.
    pub fn interrupt_pipe(&self) -> Option<u8> {
        match self.interrupt_pipe_slot.load(RELAXED) {
            0 => None,
            // slot = index + 1 <= 256, so the index fits in u8.
            slot => Some(slot.wrapping_sub(1) as u8),
        }
    }

    pub(crate) fn set_interrupt_pipe(&self, pipe: Option<u8>) {
        self.interrupt_pipe_slot
            .store(pipe.map_or(0, |p| u16::from(p) + 1), RELAXED);
    }

    /// `IsWellspringModeOn`: the last mode successfully applied by
    /// [`crate::usb::wellspring::set_wellspring_mode`].
    pub fn wellspring_mode_on(&self) -> bool {
        self.wellspring_mode_on.load(RELAXED)
    }

    pub(crate) fn request_raw(&self, requested: bool) {
        self.raw_requested.store(requested, RELAXED);
    }

    /// Devices with a mode register (both Legacy and T2) can recover raw mode if mouse packets
    /// arrive while raw mode is requested.
    pub fn can_recover_mode(&self) -> bool {
        self.raw_requested.load(RELAXED) && self.config().profile.tp_type != TrackpadType::Type3
    }

    /// Resets the consecutive continuous-reader failure counter (`D0Entry` and valid packets).
    pub fn reset_reader_failures(&self) {
        self.reader_failures.store(0, RELAXED);
    }

    /// Increments and returns the consecutive continuous-reader failure count (saturating).
    pub fn note_reader_failure(&self) -> u32 {
        let mut current = self.reader_failures.load(RELAXED);
        loop {
            let next = current.saturating_add(1);
            match self
                .reader_failures
                .compare_exchange_weak(current, next, RELAXED, RELAXED)
            {
                Ok(_) => return next,
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn set_wellspring_mode_on(&self, on: bool) {
        self.wellspring_mode_on.store(on, RELAXED);
    }

    /// `PtpReportTouch` / `PtpReportButton`.
    pub fn input_gates(&self) -> InputGates {
        let reporting = self.reporting.load();
        InputGates {
            report_touch: reporting.surface_report,
            report_button: reporting.button_report,
        }
    }

    /// Cached QPC frequency in ticks per second.
    pub fn qpc_frequency(&self) -> i64 {
        self.scan_clock.frequency()
    }

    pub(crate) fn set_qpc_frequency(&self, value: i64) {
        self.scan_clock.restart(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_zero_and_create_sets_gates() {
        let s = DeviceState::new();
        assert_eq!(s.snapshot(), DeviceStateSnapshot::default());
        assert_eq!(s.config().identification, 0xFFFF);
        s.on_device_create();
        assert_eq!(s.input_gates(), InputGates::ON);
    }

    #[test]
    fn snapshot_round_trip() {
        let snap = DeviceStateSnapshot {
            usb_device_created: true,
            ids: UsbDeviceIds {
                vendor_id: 0x05ac,
                product_id: 0x027a,
                bcd_device: 0x0100,
            },
            config_index: Some(1),
            usb_device_traits: 4,
            interrupt_pipe: Some(255),
            wellspring_mode_on: true,
            report_touch: false,
            report_button: true,
            qpc_frequency: -5,
        };
        let s = DeviceState::from_snapshot(snap);
        assert_eq!(s.snapshot(), snap);
        assert_eq!(s.config().identification, 0x027a);
        s.set_config_index(99);
        assert_eq!(s.config_index(), None);
        s.set_interrupt_pipe(Some(0));
        assert_eq!(s.interrupt_pipe(), Some(0));
    }
}

//! Bluetooth device state, power transitions and lower-read recovery policy.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU32, AtomicUsize, Ordering};

use amtptp_core::hid::feature::{SelectiveReportBits, SelectiveReportState};
use amtptp_core::hid::scan_time::ScanClock;
use amtptp_core::hid::status::NtStatus;

use crate::infrastructure::magic_trackpad::{
    Bcm5974Param, InputLayout, ModeSwitch, Transport, TransportParams,
};

/// Failed lower reads in a row that are re-issued at once by the work item, as C did (not in C: C had
/// no limit). Past this, a failed read waits for the 3 s recovery timer instead, so a lower driver that
/// fails every read immediately (a dropped link) cannot keep a worker thread in a zero-delay loop.
///
/// `HidBth` fails a read with `STATUS_DEVICE_DATA_ERROR` now and then during normal use. It is the read sent
/// right after the Bluetooth device's own HIDCLASS took a multi-finger packet (`docs/rust-port/hidfilter.md`,
/// "Competing reader on Bluetooth"). On hardware these failures came singly or in pairs between
/// successful reads. Waiting 3 s for each (as the first version of this fix did) left no read in flight
/// after two of them and froze input for 3 s. The limit is far above such runs.
pub const MAX_IMMEDIATE_READ_RETRIES: u32 = 8;

/// What the completion routine does with a finished lower read, decided before any upper request is
/// touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LowerReadClass {
    /// `VendorID` is not Apple: `WdfDeviceSetFailed(WdfDeviceFailedNoRestart)`. Checked first, as in C,
    /// so it also wins over a zero length (fixture `vid-1234-len0`).
    UnsupportedVendor,
    /// The read failed (P3 fix of HF-B7). C ignored the status, so a failed read with `Information == 0`
    /// took the [`Self::Empty`] path, whose work item issued the next read at once: a lower driver that
    /// fails every read (a Bluetooth link going down, a cancelled read) made a zero-delay loop between
    /// the work item and the completion. The port still re-issues at once, but only for
    /// [`crate::bluetooth::config::MAX_IMMEDIATE_READ_RETRIES`] failures in a row; then it waits for the 3 s
    /// recovery timer, while the device is in D0.
    Failed {
        /// The completion status.
        status: NtStatus,
    },
    /// A successful read of 0 bytes: enqueue the recovery work item, which issues a new read at
    /// `PASSIVE_LEVEL` (as in C).
    Empty,
    /// `Information` is larger than the read's buffer ([`crate::bluetooth::REPORT_BUFFER_SIZE`]), so it is not a packet
    /// length: start the 3 s recovery timer, as for [`Self::Malformed`] (P3 fix of HF-B29: C took the low
    /// 32 bits sign-extended, which made some of these values small valid lengths and others huge ones).
    Oversize,
    /// The first byte is not the transport's multitouch report ID ([`crate::bluetooth::multitouch_report_id`]): another
    /// report of the trackpad, e.g. a mouse-mode report after the trackpad restarted, or its battery
    /// status (P3 fix of HF-B8: C parsed such a packet as finger data when its length happened to fit,
    /// and otherwise stalled input for 3 s as a malformed packet). The packet is dropped; the recovery
    /// work item sends the mode switch again and issues the next read.
    OtherReport {
        /// The packet's first byte (0 for an empty buffer).
        report_id: u8,
    },
    /// `len < InputHeaderSize` or `(len - InputHeaderSize) % InputFingerSize != 0`: start the 3 s
    /// recovery timer. With `InputFingerSize == 0` C divides by zero; this port classifies it as
    /// malformed instead (HF-B18, port policy P2).
    Malformed,
    /// A parseable multitouch packet of `length` bytes (`hdr + 9n`, n >= 0, at most
    /// [`crate::bluetooth::REPORT_BUFFER_SIZE`]).
    Report {
        /// `Information`: the packet length.
        length: usize,
    },
}

/// The device-state change a `SET_FEATURE` asks for. The input report honours `PtpInputOn` and the two
/// selective-reporting switches (HF-B20).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureChange {
    /// Nothing changes (input mode other than 0 or 3).
    None,
    /// `PtpInputOn` = `ptp_input_on` (mode 3 = TRUE, mode 0 = FALSE).
    InputMode {
        /// New `PtpInputOn`.
        ptp_input_on: bool,
    },
    /// `PtpReportTouch` = bit 0 (Surface Switch), `PtpReportButton` = bit 1 (Button Switch) of report
    /// byte 1.
    SelectiveReporting {
        /// New `PtpReportButton`.
        button_report: bool,
        /// New `PtpReportTouch`.
        surface_report: bool,
    },
}

/// A plain copy of every [`DeviceState`] field, for tracing and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceSnapshot {
    /// `VendorID` (the real VID of the lower device).
    pub vendor_id: u16,
    /// `ProductID`.
    pub product_id: u16,
    /// `VersionNumber`.
    pub version_number: u16,
    /// `InputHeaderSize`.
    pub input_header_size: usize,
    /// `InputFingerSize`.
    pub input_finger_size: usize,
    /// `InputFingerDelta`.
    pub input_finger_delta: usize,
    /// `InputButtonDelta`.
    pub input_button_delta: usize,
    /// `X`.
    pub x: Bcm5974Param,
    /// `Y`.
    pub y: Bcm5974Param,
    /// `DeviceConfigured`.
    pub device_configured: bool,
    /// The data source is open: the glue has opened the vendor collection (`Col03`) that HIDCLASS
    /// delivers the multitouch reports on. It replaces C's `IsHidIoDetourCompleted` and
    /// `HidIoTarget != NULL` (the HIDCLASS detour is gone, `bth`).
    pub source_open: bool,
    /// `PtpInputOn`: written by `SET_FEATURE` 0x04 (`0x03` = TRUE, `0x00` = FALSE); gates PTP input
    /// reporting via [`DeviceState::effective_reporting`].
    pub ptp_input_on: bool,
    /// `PtpReportTouch`: the Surface Switch of `SET_FEATURE` 0x06; the input report honours it (P3 fix of
    /// HF-B20: C never read it).
    pub ptp_report_touch: bool,
    /// `PtpReportButton`: the Button Switch of `SET_FEATURE` 0x06; the input report honours it.
    pub ptp_report_button: bool,
    /// Cached QPC frequency in ticks per second.
    pub qpc_frequency: i64,
}

#[derive(Debug)]
struct AxisState {
    min: AtomicI32,
    max: AtomicI32,
}

impl AxisState {
    const fn new(p: Bcm5974Param) -> Self {
        Self {
            min: AtomicI32::new(p.min),
            max: AtomicI32::new(p.max),
        }
    }

    fn load(&self) -> Bcm5974Param {
        Bcm5974Param {
            min: self.min.load(Ordering::Relaxed),
            max: self.max.load(Ordering::Relaxed),
        }
    }

    fn store(&self, p: Bcm5974Param) {
        self.min.store(p.min, Ordering::Relaxed);
        self.max.store(p.max, Ordering::Relaxed);
    }
}

/// `DeviceConfigured` in `DeviceState::power`.
const POWER_CONFIGURED: u32 = 1;
/// The device is between `D0Entry` and `D0Exit` (not in C).
const POWER_IN_D0: u32 = 2;
/// One step of the epoch in the upper bits of `DeviceState::power`: every `D0Exit` advances it.
const POWER_EPOCH_STEP: u32 = 4;

/// Taken by the recovery timer before its mode switch (`recovery_timer_callback`): the power word
/// it saw, which must still hold when it marks the device configured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryTicket {
    power: u32,
}

/// The non-handle fields of the C `DEVICE_CONTEXT` (spec 3.1), shared by every callback.
///
/// The C driver reads and writes these fields without any lock from the parallel default queue, the
/// lower-read completion routine, the passive timer, the work item and the PnP/power callbacks. To keep
/// that behaviour without data races (port policy P2), every field is an atomic accessed with `Relaxed`
/// ordering, which is what the plain C loads and stores amount to. Put one `DeviceState` in the WDF
/// device context (initialize it with [`DeviceState::new`], which equals KMDF's zero-filled context).
///
/// `DeviceConfigured` shares one atomic word with whether the device is in D0 and an epoch that every
/// `D0Exit` advances (not in C), so the recovery timer can tell whether the device was stopped while its
/// mode switch ran.
///
/// The glue keeps the WDF handles (queues, timer, work item, lookaside list, I/O target) itself, and the
/// tracker of the lower reads in flight behind its own lock.
#[derive(Debug)]
pub struct DeviceState {
    vendor_id: AtomicU16,
    product_id: AtomicU16,
    version_number: AtomicU16,
    input_header_size: AtomicUsize,
    input_finger_size: AtomicUsize,
    input_finger_delta: AtomicUsize,
    input_button_delta: AtomicUsize,
    x: AxisState,
    y: AxisState,
    /// `DeviceConfigured` ([`POWER_CONFIGURED`]), in D0 ([`POWER_IN_D0`]) and the epoch.
    power: AtomicU32,
    /// See [`DeviceSnapshot::source_open`]. `Release` when the glue publishes the opened source,
    /// `Acquire` before using it.
    source_open: AtomicBool,
    ptp_input_on: AtomicBool,
    reporting: SelectiveReportState,
    /// Lower reads of the current epoch that failed in a row (not in C): any other completion resets it.
    read_failures: AtomicU32,
    /// A report other than the multitouch report arrived: the work item sends the mode switch again (not
    /// in C).
    mode_switch_wanted: AtomicBool,
    scan_clock: ScanClock,
}

impl Default for DeviceState {
    fn default() -> Self {
        Self::new()
    }
}

/// Work requested by the input state machine after a current-epoch read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadAction {
    /// Drop the completion while stopped.
    Drop,
    /// Reject an incompatible lower device.
    FailDevice,
    /// Reissue from the passive work item.
    Retry,
    /// Back off after repeated failures or malformed input.
    ArmTimer,
    /// Deliver a validated report of the given length.
    Deliver(usize),
}

impl DeviceState {
    /// Selects the transport layout and the report sent to enter multitouch mode.
    ///
    /// # Errors
    /// Returns the transport-selection status for an unsupported device.
    pub fn mode_switch_report(&self) -> Result<ModeSwitch, NtStatus> {
        let transport = Transport::from_ids(self.vendor_id(), self.product_id())?;
        self.apply_transport_params(&transport.params());
        Ok(transport.mode_switch())
    }

    /// Applies a classified read event and returns the work for the driver.
    pub fn on_read(&self, class: LowerReadClass) -> ReadAction {
        if !matches!(class, LowerReadClass::Failed { .. }) {
            self.note_read_completion();
        }
        match class {
            LowerReadClass::UnsupportedVendor => ReadAction::FailDevice,
            LowerReadClass::Failed { .. }
                if self.note_read_failure() <= MAX_IMMEDIATE_READ_RETRIES =>
            {
                ReadAction::Retry
            }
            LowerReadClass::Failed { .. } if !self.in_d0() => ReadAction::Drop,
            LowerReadClass::Failed { .. }
            | LowerReadClass::Oversize
            | LowerReadClass::Malformed => ReadAction::ArmTimer,
            LowerReadClass::Empty => ReadAction::Retry,
            LowerReadClass::OtherReport { .. } => {
                self.request_mode_switch();
                ReadAction::Retry
            }
            LowerReadClass::Report { length } => ReadAction::Deliver(length),
        }
    }

    /// Samples the current reporting session clock.
    pub fn sample_scan_time(&self, counter: i64) -> u16 {
        self.scan_clock.sample(counter)
    }

    /// All fields zero / FALSE except `ptp_input_on` and the two selective-reporting switches, which
    /// start on (`true` / [`SelectiveReportBits::POWER_ON`]) so PTP reports flow as soon as the
    /// multitouch mode switch completes unless the host explicitly switches to mouse mode (`0x04 = 0`)
    /// or disables switches (`0x06`).
    #[must_use]
    pub const fn new() -> Self {
        Self::with(&DeviceSnapshot {
            vendor_id: 0,
            product_id: 0,
            version_number: 0,
            input_header_size: 0,
            input_finger_size: 0,
            input_finger_delta: 0,
            input_button_delta: 0,
            x: Bcm5974Param { min: 0, max: 0 },
            y: Bcm5974Param { min: 0, max: 0 },
            device_configured: false,
            source_open: false,
            ptp_input_on: true,
            ptp_report_touch: SelectiveReportBits::POWER_ON.surface_report,
            ptp_report_button: SelectiveReportBits::POWER_ON.button_report,
            qpc_frequency: 0,
        })
    }

    /// Initializes device state from a snapshot and starts a fresh reporting clock. The device is not in
    /// D0 (call `d0_entry`).
    #[must_use]
    pub const fn with(s: &DeviceSnapshot) -> Self {
        Self {
            vendor_id: AtomicU16::new(s.vendor_id),
            product_id: AtomicU16::new(s.product_id),
            version_number: AtomicU16::new(s.version_number),
            input_header_size: AtomicUsize::new(s.input_header_size),
            input_finger_size: AtomicUsize::new(s.input_finger_size),
            input_finger_delta: AtomicUsize::new(s.input_finger_delta),
            input_button_delta: AtomicUsize::new(s.input_button_delta),
            x: AxisState::new(s.x),
            y: AxisState::new(s.y),
            power: AtomicU32::new(if s.device_configured {
                POWER_CONFIGURED
            } else {
                0
            }),
            source_open: AtomicBool::new(s.source_open),
            ptp_input_on: AtomicBool::new(s.ptp_input_on),
            reporting: SelectiveReportState::new(SelectiveReportBits {
                surface_report: s.ptp_report_touch,
                button_report: s.ptp_report_button,
            }),
            read_failures: AtomicU32::new(0),
            mode_switch_wanted: AtomicBool::new(false),
            scan_clock: ScanClock::with_frequency(s.qpc_frequency),
        }
    }

    /// A lower read failed: returns how many reads have now failed in a row (saturating).
    pub fn note_read_failure(&self) -> u32 {
        let mut current = self.read_failures.load(Ordering::Relaxed);
        loop {
            let next = current.saturating_add(1);
            match self.read_failures.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(actual) => current = actual,
            }
        }
    }

    /// A lower read completed without failing: the run of failures ends.
    pub fn note_read_completion(&self) {
        self.read_failures.store(0, Ordering::Relaxed);
    }

    /// Asks the work item to send the mode switch again.
    pub fn request_mode_switch(&self) {
        self.mode_switch_wanted.store(true, Ordering::Relaxed);
    }

    /// Takes a pending mode-switch request (the work item).
    pub fn take_mode_switch_request(&self) -> bool {
        self.mode_switch_wanted.swap(false, Ordering::Relaxed)
    }

    /// Reads every field (each with its own relaxed load).
    pub fn snapshot(&self) -> DeviceSnapshot {
        let reporting = self.reporting.load();
        DeviceSnapshot {
            vendor_id: self.vendor_id(),
            product_id: self.product_id(),
            version_number: self.version_number.load(Ordering::Relaxed),
            input_header_size: self.input_header_size.load(Ordering::Relaxed),
            input_finger_size: self.input_finger_size.load(Ordering::Relaxed),
            input_finger_delta: self.input_finger_delta.load(Ordering::Relaxed),
            input_button_delta: self.input_button_delta.load(Ordering::Relaxed),
            x: self.x.load(),
            y: self.y.load(),
            device_configured: self.device_configured(),
            source_open: self.source_open(),
            ptp_input_on: self.ptp_input_on.load(Ordering::Relaxed),
            ptp_report_touch: reporting.surface_report,
            ptp_report_button: reporting.button_report,
            qpc_frequency: self.qpc_frequency(),
        }
    }

    /// `VendorID`.
    pub fn vendor_id(&self) -> u16 {
        self.vendor_id.load(Ordering::Relaxed)
    }

    /// `ProductID`.
    pub fn product_id(&self) -> u16 {
        self.product_id.load(Ordering::Relaxed)
    }

    /// Stores `VendorID`, `ProductID` and `VersionNumber` (`SelfManagedIoInit` after the attributes query,
    /// Device.c:221-223; `PrepareHardware` with zeros).
    pub fn set_ids(&self, vendor_id: u16, product_id: u16, version_number: u16) {
        self.vendor_id.store(vendor_id, Ordering::Relaxed);
        self.product_id.store(product_id, Ordering::Relaxed);
        self.version_number.store(version_number, Ordering::Relaxed);
    }

    /// The fields the lower-read completion reads to parse a packet.
    pub fn input_layout(&self) -> InputLayout {
        InputLayout {
            header_size: self.input_header_size.load(Ordering::Relaxed),
            finger_size: self.input_finger_size.load(Ordering::Relaxed),
            finger_delta: self.input_finger_delta.load(Ordering::Relaxed),
            button_delta: self.input_button_delta.load(Ordering::Relaxed),
            x_min: self.x.min.load(Ordering::Relaxed),
            y_min: self.y.min.load(Ordering::Relaxed),
        }
    }

    /// Writes the transport parameters (`ConfigureMultiTouch`, before the send).
    pub fn apply_transport_params(&self, p: &TransportParams) {
        self.input_finger_size
            .store(p.finger_size, Ordering::Relaxed);
        self.input_header_size
            .store(p.header_size, Ordering::Relaxed);
        self.input_finger_delta
            .store(p.finger_delta, Ordering::Relaxed);
        self.input_button_delta
            .store(p.button_delta, Ordering::Relaxed);
        self.x.store(p.x);
        self.y.store(p.y);
    }

    /// `DeviceConfigured`: the mode switch is done and upper reads may trigger lower reads.
    pub fn device_configured(&self) -> bool {
        self.power.load(Ordering::SeqCst) & POWER_CONFIGURED != 0
    }

    /// Sets `DeviceConfigured`.
    pub fn set_device_configured(&self, configured: bool) {
        if configured {
            self.power.fetch_or(POWER_CONFIGURED, Ordering::SeqCst);
        } else {
            self.power.fetch_and(!POWER_CONFIGURED, Ordering::SeqCst);
        }
    }

    /// Whether the device is in D0 (between `d0_entry` and `d0_exit`).
    pub fn in_d0(&self) -> bool {
        self.power.load(Ordering::SeqCst) & POWER_IN_D0 != 0
    }

    /// `D0Entry`: the device is in D0.
    pub fn enter_d0(&self) {
        self.power.fetch_or(POWER_IN_D0, Ordering::SeqCst);
    }

    /// `D0Exit`: not in D0, not configured, next epoch.
    pub fn leave_d0(&self) {
        let mut power = self.power.load(Ordering::SeqCst);
        loop {
            let next = (power & !(POWER_CONFIGURED | POWER_IN_D0)).wrapping_add(POWER_EPOCH_STEP);
            match self
                .power
                .compare_exchange_weak(power, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return,
                Err(actual) => power = actual,
            }
        }
    }

    /// The recovery timer may work on the device only while it is in D0.
    pub fn begin_recovery(&self) -> Option<RecoveryTicket> {
        let power = self.power.load(Ordering::SeqCst);
        (power & POWER_IN_D0 != 0).then_some(RecoveryTicket { power })
    }

    /// Marks the device configured if nothing changed since `ticket` was taken (in particular, no
    /// `D0Exit`). Returns whether it did.
    pub fn finish_recovery(&self, ticket: RecoveryTicket) -> bool {
        self.power
            .compare_exchange(
                ticket.power,
                ticket.power | POWER_CONFIGURED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Whether the data source is open ([`DeviceSnapshot::source_open`]).
    pub fn source_open(&self) -> bool {
        self.source_open.load(Ordering::Acquire)
    }

    /// Records that the data source opened or closed.
    pub fn set_source_open(&self, open: bool) {
        self.source_open.store(open, Ordering::Release);
    }

    /// Cached QPC frequency in ticks per second.
    pub fn qpc_frequency(&self) -> i64 {
        self.scan_clock.frequency()
    }

    /// Caches the QPC frequency and starts a new input-reporting session.
    pub fn set_qpc_frequency(&self, value: i64) {
        self.scan_clock.restart(value);
    }

    /// The selective-reporting switches configured by `SET_FEATURE` 0x06.
    pub fn selective_reporting(&self) -> SelectiveReportBits {
        self.reporting.load()
    }

    /// Effective input gates combining `ptp_input_on` (`SET_FEATURE` 0x04) with
    /// [`Self::selective_reporting`] (`SET_FEATURE` 0x06).
    pub fn effective_reporting(&self) -> SelectiveReportBits {
        if self.ptp_input_on.load(Ordering::Relaxed) {
            self.selective_reporting()
        } else {
            SelectiveReportBits {
                surface_report: false,
                button_report: false,
            }
        }
    }

    /// Back to power-on enabled state (`ptp_input_on = true` and [`SelectiveReportBits::POWER_ON`];
    /// `PrepareHardware`).
    pub fn reset_selective_reporting(&self) {
        self.ptp_input_on.store(true, Ordering::Relaxed);
        self.apply_feature_change(FeatureChange::SelectiveReporting {
            button_report: SelectiveReportBits::POWER_ON.button_report,
            surface_report: SelectiveReportBits::POWER_ON.surface_report,
        });
    }

    /// Applies a decoded `SET_FEATURE` ([`crate::bluetooth::decode_set_feature`]).
    pub fn apply_feature_change(&self, change: FeatureChange) {
        match change {
            FeatureChange::None => {}
            FeatureChange::InputMode { ptp_input_on } => {
                self.ptp_input_on.store(ptp_input_on, Ordering::Relaxed);
            }
            FeatureChange::SelectiveReporting {
                button_report,
                surface_report,
            } => {
                self.reporting.store(SelectiveReportBits {
                    surface_report,
                    button_report,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests;

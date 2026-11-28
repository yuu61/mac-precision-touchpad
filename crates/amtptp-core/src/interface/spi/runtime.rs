//! Mutable per-device state shared between SPI PnP/power callbacks, IOCTL dispatch, recovery/retry
//! timers, and the lower-read completion routine.

use core::sync::atomic::{AtomicU32, Ordering};

pub use crate::domain::ContactTracker;
use crate::hid::input::SPI_PTP_REPORT_LEN;
pub use crate::hid::read_tracker::{
    LowerReadOrigin, MAX_IN_FLIGHT_READS, PostSendAction, ReadTicket, ReadTracker, SuspendedReads,
};
use crate::hid::scan_time::ScanClock;

use crate::spi::device::SpiTrackpadInfo;
use crate::spi::feature::PtpSettings;
use crate::spi::input::{
    SpiPacket, check_declared_fingers, lower_read_length, report_bytes, valid_prefix,
};
use crate::spi::status::{NtStatus, STATUS_CANCELLED, STATUS_SUCCESS, nt_success};

/// `PTP_AAPL_DEVICE_POWER_STATUS` (Device.h:46-50).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DeviceStatus {
    /// `D3` (0): created, or suspended (`SelfManagedIoSuspend` / `D0Exit`).
    D3 = 0,
    /// `D0ActiveAndConfigured` (1): multitouch `SET_FEATURE` succeeded; upstream reads issue lower reads.
    D0ActiveAndConfigured = 1,
    /// `D0ActiveAndUnconfigured` (2): after `D0Entry` until the mode switch succeeds.
    D0ActiveAndUnconfigured = 2,
}

impl DeviceStatus {
    /// Decodes the stored discriminant.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::D0ActiveAndConfigured,
            2 => Self::D0ActiveAndUnconfigured,
            _ => Self::D3,
        }
    }
}

/// `WDF_REL_TIMEOUT_IN_SEC(5)`: due time of the power-on mode-recovery timer (100 ns units, relative).
pub const RECOVERY_TIMER_DUE_TIME: i64 = -50_000_000;
/// `WDF_TIMER_CONFIG.Period` of one-shot timers: 0.
pub const RECOVERY_TIMER_PERIOD: u32 = 0;
/// Maximum timer-driven mode-recovery attempts before escalating to `WdfDeviceSetFailed`.
pub const MAX_RECOVERY_RETRIES: u32 = 3;
/// Initial backoff (`50 ms` relative, 100 ns units) for replenishing failed lower-read issues/completions.
pub const READ_RETRY_INITIAL_DUE_TIME: i64 = -500_000;
/// Maximum consecutive lower-read issue/completion failures before escalating to `WdfDeviceSetFailed`.
pub const MAX_READ_RETRIES: u32 = 6;
/// Completion status for every request `D0Exit` drains from `HidQueue` (Information 0).
pub const D0_EXIT_DRAIN_STATUS: NtStatus = STATUS_CANCELLED;

/// Decision of `SelfManagedIoInit` / `SelfManagedIoRestart`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfManagedIoOutcome {
    /// Return value of the callback: always `STATUS_SUCCESS`.
    pub status: NtStatus,
    /// `Some(due_time)` when the mode switch failed and the recovery timer should be armed.
    pub start_recovery_timer: Option<i64>,
    /// Whether the device is now configured and should issue [`DeviceRuntime::backlog_reads`].
    pub configured: bool,
}

/// Epoch snapshot taken before the recovery timer runs its synchronous mode switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryTicket {
    epoch: u32,
}

/// Action after a recovery-timer mode-switch attempt ([`DeviceRuntime::finish_recovery`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Mode switch succeeded; issue [`DeviceRuntime::backlog_reads`].
    Configured,
    /// Mode switch failed within budget; reschedule the recovery timer with `due_time`.
    Retry(i64),
    /// Mode switch failed beyond [`MAX_RECOVERY_RETRIES`]; call `WdfDeviceSetFailed(AttemptRestart)`.
    FailDevice,
    /// Device left D0 or was already configured by a restart while the mode switch ran.
    Ignored,
}

/// Action after a lower-read issue or lower-read completion failure while configured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplenishAction {
    /// Device is no longer configured; do nothing.
    None,
    /// Schedule the replenishment timer with `due_time` (exponential backoff).
    Retry(i64),
    /// Consecutive failures exceeded [`MAX_READ_RETRIES`]; call `WdfDeviceSetFailed(AttemptRestart)`.
    FailDevice,
}

/// Why [`DeviceRuntime::prepare_lower_read`] refused a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimRefused {
    /// Device is not `D0ActiveAndConfigured`.
    NotConfigured,
    /// All [`MAX_IN_FLIGHT_READS`] slots are occupied.
    TooManyInFlight,
}

/// `DeviceStatus`, reporting clock, PTP flags, and retry counters of one SPI trackpad.
#[derive(Debug)]
pub struct DeviceRuntime {
    status: AtomicU32,
    recovery_failures: AtomicU32,
    read_failures: AtomicU32,
    scan_clock: ScanClock,
    ptp: PtpSettings,
}

impl Default for DeviceRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceRuntime {
    /// Initial state at device creation (`D3`).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            status: AtomicU32::new(DeviceStatus::D3 as u32),
            recovery_failures: AtomicU32::new(0),
            read_failures: AtomicU32::new(0),
            scan_clock: ScanClock::new(),
            ptp: PtpSettings::new(),
        }
    }

    /// Current [`DeviceStatus`].
    pub fn device_status(&self) -> DeviceStatus {
        DeviceStatus::from_raw(self.status.load(Ordering::SeqCst))
    }

    fn set_status(&self, status: DeviceStatus) {
        self.status.store(status as u32, Ordering::SeqCst);
    }

    /// Cached QPC frequency in ticks per second.
    pub fn qpc_frequency(&self) -> i64 {
        self.scan_clock.frequency()
    }

    /// The PTP flags (`PtpInputOn`, `PtpReportButton`, `PtpReportTouch`).
    pub const fn ptp_settings(&self) -> &PtpSettings {
        &self.ptp
    }

    /// `D0Entry`: resets retry budgets, marks the device unconfigured, and initializes the clock.
    pub fn on_d0_entry(&self, qpc_frequency: i64) -> NtStatus {
        self.recovery_failures.store(0, Ordering::Relaxed);
        self.read_failures.store(0, Ordering::Relaxed);
        self.set_status(DeviceStatus::D0ActiveAndUnconfigured);
        self.scan_clock.restart(qpc_frequency);
        STATUS_SUCCESS
    }

    /// `SelfManagedIoInit` / `SelfManagedIoRestart` after running the synchronous mode switch.
    pub fn on_self_managed_io_init_or_restart<H, const N: usize, F: FnOnce() -> i64>(
        &self,
        _tracker: &mut ReadTracker<H, N>,
        set_state_status: NtStatus,
        qpc_frequency: F,
    ) -> SelfManagedIoOutcome {
        self.recovery_failures.store(0, Ordering::Relaxed);
        if nt_success(set_state_status) {
            self.configure(qpc_frequency);
            SelfManagedIoOutcome {
                status: STATUS_SUCCESS,
                start_recovery_timer: None,
                configured: true,
            }
        } else {
            SelfManagedIoOutcome {
                status: STATUS_SUCCESS,
                start_recovery_timer: Some(RECOVERY_TIMER_DUE_TIME),
                configured: false,
            }
        }
    }

    fn configure<F: FnOnce() -> i64>(&self, qpc_frequency: F) {
        self.recovery_failures.store(0, Ordering::Relaxed);
        self.read_failures.store(0, Ordering::Relaxed);
        self.scan_clock.restart(qpc_frequency());
        self.set_status(DeviceStatus::D0ActiveAndConfigured);
    }

    /// Captures the current epoch if the device is still waiting in `D0ActiveAndUnconfigured`.
    pub fn begin_recovery<H, const N: usize>(
        &self,
        tracker: &ReadTracker<H, N>,
    ) -> Option<RecoveryTicket> {
        (self.device_status() == DeviceStatus::D0ActiveAndUnconfigured).then_some(RecoveryTicket {
            epoch: tracker.epoch(),
        })
    }

    /// Completes a recovery-timer mode-switch attempt, rescheduling up to [`MAX_RECOVERY_RETRIES`]
    /// times and then escalating to [`RecoveryAction::FailDevice`].
    pub fn finish_recovery<H, const N: usize, F: FnOnce() -> i64>(
        &self,
        tracker: &mut ReadTracker<H, N>,
        ticket: RecoveryTicket,
        set_state_status: NtStatus,
        qpc_frequency: F,
    ) -> RecoveryAction {
        let still_waiting = ticket.epoch == tracker.epoch()
            && self.device_status() == DeviceStatus::D0ActiveAndUnconfigured;
        if !still_waiting {
            return RecoveryAction::Ignored;
        }
        if nt_success(set_state_status) {
            self.configure(qpc_frequency);
            return RecoveryAction::Configured;
        }
        let attempt = self
            .recovery_failures
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if attempt < MAX_RECOVERY_RETRIES {
            RecoveryAction::Retry(RECOVERY_TIMER_DUE_TIME)
        } else {
            RecoveryAction::FailDevice
        }
    }

    /// Number of lower reads needed to match `queued` pending upper reads.
    pub fn backlog_reads<H: Copy, const N: usize>(
        &self,
        tracker: &ReadTracker<H, N>,
        queued: usize,
    ) -> usize {
        if self.device_status() == DeviceStatus::D0ActiveAndConfigured {
            queued.saturating_sub(tracker.in_flight())
        } else {
            0
        }
    }

    /// Resets the consecutive lower-read failure counter after a successful send/completion.
    pub fn reset_read_failures(&self) {
        self.read_failures.store(0, Ordering::Relaxed);
    }

    /// Records a failed lower-read issue or failed lower-read completion (`!nt_success(status)`),
    /// returning exponential backoff or [`ReplenishAction::FailDevice`].
    #[must_use]
    pub fn note_lower_read_failure(&self) -> ReplenishAction {
        if self.device_status() != DeviceStatus::D0ActiveAndConfigured {
            return ReplenishAction::None;
        }
        let failures = self
            .read_failures
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if failures > MAX_READ_RETRIES {
            return ReplenishAction::FailDevice;
        }
        let shift = failures.saturating_sub(1).min(5);
        ReplenishAction::Retry(READ_RETRY_INITIAL_DUE_TIME.saturating_mul(1_i64 << shift))
    }

    /// `SelfManagedIoSuspend`: marks `D3`, advances epoch, and returns `Sent` reads to cancel.
    pub fn suspend<H: Copy, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
    ) -> SuspendedReads<H, N> {
        self.set_status(DeviceStatus::D3);
        tracker.suspend()
    }

    /// `D0Exit`: suspends if not already suspended and returns `STATUS_SUCCESS`.
    pub fn on_d0_exit<H: Copy, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
    ) -> (NtStatus, SuspendedReads<H, N>) {
        (STATUS_SUCCESS, self.suspend(tracker))
    }

    /// Lock-free hint before allocating a lower read on the `READ_REPORT` path.
    pub fn should_issue_lower_read(&self) -> bool {
        self.device_status() == DeviceStatus::D0ActiveAndConfigured
    }

    /// Reserves a slot in `Preparing` state before `WdfRequestSend`.
    ///
    /// # Errors
    /// Returns [`ClaimRefused`] if not configured or all slots are full.
    pub fn prepare_lower_read<H: Copy, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
        read: H,
    ) -> Result<ReadTicket, ClaimRefused> {
        if self.device_status() != DeviceStatus::D0ActiveAndConfigured {
            return Err(ClaimRefused::NotConfigured);
        }
        tracker.prepare(read).ok_or(ClaimRefused::TooManyInFlight)
    }

    /// Convenience single-step claim for synchronous host tests (`prepare` + `Sent`).
    ///
    /// # Errors
    /// Returns [`ClaimRefused`] if not configured or all slots are full.
    pub fn claim_lower_read<H: Copy + PartialEq, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
        read: H,
    ) -> Result<ReadTicket, ClaimRefused> {
        if self.device_status() != DeviceStatus::D0ActiveAndConfigured {
            return Err(ClaimRefused::NotConfigured);
        }
        tracker.claim(read).ok_or(ClaimRefused::TooManyInFlight)
    }

    /// Finalizes `WdfRequestSend` in `tracker`.
    #[must_use]
    pub fn complete_lower_read_send<H: Copy + PartialEq, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
        read: H,
        ticket: ReadTicket,
        sent: bool,
    ) -> PostSendAction {
        tracker.complete_send(read, ticket, sent)
    }

    /// Unregisters a completed lower read and reports whether its epoch is current.
    pub fn finish_lower_read<H: Copy + PartialEq, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
        read: H,
        ticket: ReadTicket,
    ) -> LowerReadOrigin {
        tracker.finish(read, ticket)
    }

    /// Validates and decodes a completed lower read (`NT_SUCCESS(lower_status)` already verified by
    /// the caller) into a 30-byte SPI PTP report.
    ///
    /// # Errors
    /// Returns `STATUS_DEVICE_DATA_ERROR` if `information` is outside `46..=1024` or shorter than
    /// the declared finger count.
    pub fn complete_lower_read<F: FnOnce() -> i64>(
        &self,
        trackpad: &SpiTrackpadInfo,
        contacts: &mut ContactTracker,
        information: u64,
        buffer: &[u8],
        query_counter: F,
    ) -> Result<[u8; SPI_PTP_REPORT_LEN], NtStatus> {
        let length = lower_read_length(information)?;
        let packet = SpiPacket::new(valid_prefix(buffer, length));
        check_declared_fingers(&packet)?;
        let scan_time = self.scan_clock.sample(query_counter());
        Ok(report_bytes(
            &packet,
            trackpad,
            scan_time,
            self.ptp.effective_reporting(),
            contacts,
        ))
    }

    /// `SET_FEATURE` (see [`PtpSettings::set_feature`]).
    pub fn set_feature(
        &self,
        report_id: u8,
        report_buffer_len: u32,
        report: Option<&[u8]>,
    ) -> NtStatus {
        self.ptp.set_feature(report_id, report_buffer_len, report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spi::status::STATUS_UNSUCCESSFUL;

    type Tracker = ReadTracker<u32, 4>;

    #[test]
    fn power_sequence() {
        let rt = DeviceRuntime::new();
        let mut tracker = Tracker::new();
        assert_eq!(rt.device_status(), DeviceStatus::D3);
        rt.on_d0_entry(10_000_000);
        assert_eq!(rt.device_status(), DeviceStatus::D0ActiveAndUnconfigured);
        assert!(!rt.should_issue_lower_read());
        let out = rt.on_self_managed_io_init_or_restart(&mut tracker, STATUS_UNSUCCESSFUL, || 1);
        assert_eq!(out.start_recovery_timer, Some(-50_000_000));
        assert_eq!(out.status, STATUS_SUCCESS);
        assert!(!out.configured);
        let ticket = rt.begin_recovery(&tracker).expect("waiting");
        assert_eq!(
            rt.finish_recovery(&mut tracker, ticket, STATUS_SUCCESS, || 7),
            RecoveryAction::Configured
        );
        assert_eq!(rt.qpc_frequency(), 7);
        assert!(rt.should_issue_lower_read());
        assert_eq!(rt.backlog_reads(&tracker, 3), 3);
        let (status, reads) = rt.on_d0_exit(&mut tracker);
        assert_eq!(status, STATUS_SUCCESS);
        assert_eq!(reads.iter().count(), 0);
        assert_eq!(rt.device_status(), DeviceStatus::D3);
    }

    #[test]
    fn claims_need_a_configured_device_and_a_free_slot() {
        let rt = DeviceRuntime::new();
        let mut tracker = Tracker::new();
        assert_eq!(
            rt.claim_lower_read(&mut tracker, 1),
            Err(ClaimRefused::NotConfigured)
        );
        rt.on_d0_entry(1);
        let _outcome = rt.on_self_managed_io_init_or_restart(&mut tracker, STATUS_SUCCESS, || 1);
        let tickets =
            [1, 2, 3, 4].map(|read| rt.claim_lower_read(&mut tracker, read).expect("free slot"));
        assert_eq!(
            rt.claim_lower_read(&mut tracker, 5),
            Err(ClaimRefused::TooManyInFlight)
        );
        assert_eq!(tracker.in_flight(), 4);
        assert_eq!(rt.backlog_reads(&tracker, 6), 2);
        assert_eq!(
            rt.finish_lower_read(&mut tracker, 2, tickets[1]),
            LowerReadOrigin::Current
        );
        let _reclaimed: ReadTicket = rt.claim_lower_read(&mut tracker, 5).expect("freed slot");
    }

    #[test]
    fn recovery_retries_then_escalates_and_ignores_stale_epoch() {
        let rt = DeviceRuntime::new();
        let mut tracker = Tracker::new();
        assert_eq!(rt.begin_recovery(&tracker), None);

        rt.on_d0_entry(1);
        let stale = rt.begin_recovery(&tracker).expect("unconfigured");
        let _cancelled = rt.suspend(&mut tracker);
        assert_eq!(
            rt.finish_recovery(&mut tracker, stale, STATUS_SUCCESS, || 1),
            RecoveryAction::Ignored
        );

        rt.on_d0_entry(1);
        for _ in 1..MAX_RECOVERY_RETRIES {
            let t = rt.begin_recovery(&tracker).expect("unconfigured");
            assert_eq!(
                rt.finish_recovery(&mut tracker, t, STATUS_UNSUCCESSFUL, || 1),
                RecoveryAction::Retry(RECOVERY_TIMER_DUE_TIME)
            );
        }
        let last = rt.begin_recovery(&tracker).expect("unconfigured");
        assert_eq!(
            rt.finish_recovery(&mut tracker, last, STATUS_UNSUCCESSFUL, || 1),
            RecoveryAction::FailDevice
        );
    }

    #[test]
    fn read_failure_replenish_backoff_and_escalation() {
        let rt = DeviceRuntime::new();
        let mut tracker = Tracker::new();
        assert_eq!(rt.note_lower_read_failure(), ReplenishAction::None);

        rt.on_d0_entry(1);
        let _outcome = rt.on_self_managed_io_init_or_restart(&mut tracker, STATUS_SUCCESS, || 1);
        assert_eq!(
            rt.note_lower_read_failure(),
            ReplenishAction::Retry(READ_RETRY_INITIAL_DUE_TIME)
        );
        assert_eq!(
            rt.note_lower_read_failure(),
            ReplenishAction::Retry(READ_RETRY_INITIAL_DUE_TIME * 2)
        );
        rt.reset_read_failures();
        for _ in 0..MAX_READ_RETRIES {
            assert!(matches!(
                rt.note_lower_read_failure(),
                ReplenishAction::Retry(_)
            ));
        }
        assert_eq!(rt.note_lower_read_failure(), ReplenishAction::FailDevice);
    }

    #[test]
    fn empty_lower_buffer_reads_as_zero_packet() {
        use crate::hid::common::REPORTID_MULTITOUCH;
        use crate::hid::input::SpiPtpReport;
        use crate::spi::device::SPI_MODELS;
        use crate::spi::status::STATUS_DEVICE_DATA_ERROR;

        let rt = DeviceRuntime::new();
        let tp = SPI_MODELS[0].trackpad;
        let mut contacts = ContactTracker::new();

        let mut queried = false;
        let short = rt.complete_lower_read(&tp, &mut contacts, 45, &[], || {
            queried = true;
            7
        });
        assert_eq!(short, Err(STATUS_DEVICE_DATA_ERROR));
        assert!(!queried);
        assert_eq!(
            rt.complete_lower_read(&tp, &mut contacts, 46, &[], || 0),
            Err(STATUS_DEVICE_DATA_ERROR)
        );
        let report = rt.complete_lower_read(&tp, &mut contacts, 46, &[0; 46], || 0);
        assert_eq!(report, Ok(SpiPtpReport::EMPTY.to_bytes()));
        assert_eq!(
            report.ok().and_then(|bytes| bytes.first().copied()),
            Some(REPORTID_MULTITOUCH)
        );
    }
}

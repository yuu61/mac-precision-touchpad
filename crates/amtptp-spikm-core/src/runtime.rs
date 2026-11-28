//! The mutable per-device state the driver shares between its PnP/power callbacks, the IOCTL dispatcher,
//! the recovery timer and the lower-read completion routine: the device status, the reporting clock, the
//! PTP flags and the bookkeeping of the lower reads in flight.
//!
//! # Stop and resume as one state transition
//!
//! The status moves `D3 -> D0ActiveAndUnconfigured` (`D0Entry`) `-> D0ActiveAndConfigured` (a successful
//! mode switch in `SelfManagedIoInit/Restart` or in the recovery timer) and back to `D3`
//! (`SelfManagedIoSuspend`, and again in `D0Exit`). The transitions that a concurrent callback can race
//! with go through a [`ReadTracker`], which the driver keeps behind one spin lock:
//!
//! - every lower read is registered with [`DeviceRuntime::claim_lower_read`] before it is sent. The claim
//!   fails unless the device is configured, so nothing new is sent once the device has been suspended;
//! - [`DeviceRuntime::suspend`] closes that gate, starts a new *epoch* and hands back the reads in flight,
//!   which the driver cancels. A read issued in an earlier epoch ([`LowerReadOrigin::Stale`]) is dropped
//!   when it completes, so it never consumes an upstream request queued after the resume;
//! - the recovery timer takes a [`RecoveryTicket`] before its mode switch and only configures the device
//!   if the epoch is unchanged afterwards, so a timer that fires after `D0Exit` or after a successful
//!   restart does nothing;
//! - every transition to configured is followed by [`DeviceRuntime::backlog_reads`]: one lower read for
//!   every upstream read that is already waiting in `HidQueue`.
//!
//! The status and the reporting clock are atomics, so lock-free readers (the `READ_REPORT` fast path) see
//! a consistent value; the status is only written while the driver holds the tracker lock.

use core::sync::atomic::{AtomicU32, Ordering};

pub use ptp_hid::contact_tracker::ContactTracker;
use ptp_hid::input::SPI_PTP_REPORT_LEN;
pub use ptp_hid::read_tracker::{
    LowerReadOrigin, MAX_IN_FLIGHT_READS, ReadTicket, ReadTracker, SuspendedReads,
};
use ptp_hid::scan_time::ScanClock;

use crate::device::SpiTrackpadInfo;
use crate::feature::PtpSettings;
use crate::input::{
    SpiPacket, check_declared_fingers, lower_read_length, report_bytes, valid_prefix,
};
use crate::status::{NtStatus, STATUS_CANCELLED, STATUS_SUCCESS, nt_success};

/// `PTP_AAPL_DEVICE_POWER_STATUS` (Device.h:46-50).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DeviceStatus {
    /// `D3` (0): created, or suspended (`SelfManagedIoSuspend` / `D0Exit`).
    D3 = 0,
    /// `D0ActiveAndConfigured` (1): the multitouch `SET_FEATURE` succeeded; upstream reads issue lower
    /// reads.
    D0ActiveAndConfigured = 1,
    /// `D0ActiveAndUnconfigured` (2): after `D0Entry` until the mode switch succeeds.
    D0ActiveAndUnconfigured = 2,
}

impl DeviceStatus {
    /// Decodes the stored value. Only the three discriminants are ever stored; anything else maps to
    /// `D3` (unreachable).
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::D0ActiveAndConfigured,
            2 => Self::D0ActiveAndUnconfigured,
            _ => Self::D3,
        }
    }
}

/// `WDF_REL_TIMEOUT_IN_SEC(5)`: due time of the one-shot power-on recovery timer, in 100 ns units,
/// relative (negative).
pub const RECOVERY_TIMER_DUE_TIME: i64 = -50_000_000;
/// `WDF_TIMER_CONFIG.Period` of the recovery timer: 0 (one-shot).
pub const RECOVERY_TIMER_PERIOD: u32 = 0;
/// Completion status for every request `D0Exit` drains from `HidQueue` (Information 0).
pub const D0_EXIT_DRAIN_STATUS: NtStatus = STATUS_CANCELLED;

/// What `SelfManagedIoInit` / `SelfManagedIoRestart` decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfManagedIoOutcome {
    /// Return value of the callback: always `STATUS_SUCCESS`.
    pub status: NtStatus,
    /// `Some(due_time)` when the mode switch failed: the glue calls `WdfTimerStart(PowerOnRecoveryTimer,
    /// due_time)`.
    pub start_recovery_timer: Option<i64>,
    /// The device is now configured: the glue issues [`DeviceRuntime::backlog_reads`] lower reads.
    pub configured: bool,
}

/// Taken by the recovery timer before its mode switch ([`DeviceRuntime::begin_recovery`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryTicket {
    epoch: u32,
}

/// Why [`DeviceRuntime::claim_lower_read`] refused a read. The driver deletes the read without sending it;
/// the upstream request stays queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimRefused {
    /// The device is not configured (unconfigured, or suspended).
    NotConfigured,
    /// [`MAX_IN_FLIGHT_READS`] reads are already in flight.
    TooManyInFlight,
}

/// `DeviceStatus`, the reporting clock and the PTP flags of one device.
#[derive(Debug)]
pub struct DeviceRuntime {
    status: AtomicU32,
    scan_clock: ScanClock,
    ptp: PtpSettings,
}

impl Default for DeviceRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceRuntime {
    /// Initial state at device creation: D3, no reporting clock origin, and all PTP flags FALSE.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            status: AtomicU32::new(DeviceStatus::D3 as u32),
            scan_clock: ScanClock::new(),
            ptp: PtpSettings::new(),
        }
    }

    /// `DeviceStatus`.
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

    /// `D0Entry`: marks the device unconfigured and initializes the reporting clock for D0.
    pub fn on_d0_entry(&self, qpc_frequency: i64) -> NtStatus {
        self.set_status(DeviceStatus::D0ActiveAndUnconfigured);
        self.scan_clock.restart(qpc_frequency);
        STATUS_SUCCESS
    }

    /// `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` (Device.c:397-427), registered for both
    /// `SelfManagedIoInit` and `SelfManagedIoRestart`, after the glue ran the mode switch
    /// ([`crate::wire::with_set_state_packet`], synchronous `IOCTL_HID_SET_FEATURE`) and got `set_state_status`.
    /// The framework never runs it concurrently with [`Self::suspend`].
    ///
    /// - Success: restart the reporting clock and mark the device configured. The glue then issues
    ///   [`Self::backlog_reads`] lower reads for the requests that are already queued (P3 fix of SPI-B13).
    /// - Failure: nothing changes; the outcome asks the glue to start the 5 s recovery timer.
    ///
    /// The callback always returns `STATUS_SUCCESS`.
    pub fn on_self_managed_io_init_or_restart<H, const N: usize, F: FnOnce() -> i64>(
        &self,
        _tracker: &mut ReadTracker<H, N>,
        set_state_status: NtStatus,
        qpc_frequency: F,
    ) -> SelfManagedIoOutcome {
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
        self.scan_clock.restart(qpc_frequency());
        self.set_status(DeviceStatus::D0ActiveAndConfigured);
    }

    /// Start of `AmtPtpPowerRecoveryTimerCallback` (Device.c:541-562, `PASSIVE_LEVEL` work item): the
    /// ticket the callback needs to configure the device, or `None` if the device is not waiting for a
    /// mode switch (suspended, or already configured). With `None` the glue does nothing, not even the mode
    /// switch (P3 fix of SPI-B12: the C timer also ran in D3 and after a successful restart).
    pub fn begin_recovery<H, const N: usize>(
        &self,
        tracker: &ReadTracker<H, N>,
    ) -> Option<RecoveryTicket> {
        (self.device_status() == DeviceStatus::D0ActiveAndUnconfigured).then_some(RecoveryTicket {
            epoch: tracker.epoch(),
        })
    }

    /// End of the recovery timer callback, after the glue ran the mode switch again and got
    /// `set_state_status`. On success the device is configured and the reporting clock restarted, unless
    /// it was suspended (or configured by a restart) while the mode switch ran; the glue then issues
    /// [`Self::backlog_reads`] lower reads (one per queued upstream read, P3 fix of SPI-B12, which issued
    /// exactly one). On failure nothing happens and nothing is rescheduled. Returns whether the device was
    /// configured.
    pub fn finish_recovery<H, const N: usize, F: FnOnce() -> i64>(
        &self,
        tracker: &mut ReadTracker<H, N>,
        ticket: RecoveryTicket,
        set_state_status: NtStatus,
        qpc_frequency: F,
    ) -> bool {
        let still_waiting = ticket.epoch == tracker.epoch()
            && self.device_status() == DeviceStatus::D0ActiveAndUnconfigured;
        if nt_success(set_state_status) && still_waiting {
            self.configure(qpc_frequency);
            true
        } else {
            false
        }
    }

    /// How many lower reads to issue right after the device became configured: one for every upstream
    /// read waiting in `HidQueue` (`queued`, from `WdfIoQueueGetState`) that no read of the current epoch
    /// serves yet. Read `queued` **after** the transition, so a `READ_REPORT` racing with it is counted
    /// at least once (counting it twice only issues a spare read, whose data is dropped).
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

    /// `SelfManagedIoSuspend` (and again `D0Exit`): `DeviceStatus = D3`, a new epoch, and the reads that
    /// were in flight, for the glue to cancel. The glue also stops the recovery timer. Idempotent: a second
    /// call only advances the epoch again and returns no reads (P3 fix of SPI-B12/B14: the C driver kept the
    /// timer and the in-flight reads alive across `D0Exit`).
    pub fn suspend<H: Copy, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
    ) -> SuspendedReads<H, N> {
        self.set_status(DeviceStatus::D3);
        tracker.suspend()
    }

    /// `AmtPtpEvtDeviceD0Exit` (Device.c:353-395): [`Self::suspend`] (normally a no-op, because
    /// `SelfManagedIoSuspend` ran first). The glue cancels the returned reads, retrieves every request
    /// from `HidQueue`, completes it with [`D0_EXIT_DRAIN_STATUS`] (Information 0) until the queue is empty,
    /// and returns `STATUS_SUCCESS`. The trackpad is not switched off (SPI-B14, preserved).
    pub fn on_d0_exit<H: Copy, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
    ) -> (NtStatus, SuspendedReads<H, N>) {
        (STATUS_SUCCESS, self.suspend(tracker))
    }

    /// `AmtPtpSpiInputRoutineWorker` (Input.c:4-36), after `WdfRequestForwardToIoQueue(HidQueue)`
    /// succeeded: whether to try one lower `IOCTL_HID_READ_REPORT` now (`DeviceStatus ==
    /// D0ActiveAndConfigured`). This is only a hint read without the lock; [`Self::claim_lower_read`]
    /// decides. If the forward failed, the glue completes the request with that status and does not call
    /// this.
    pub fn should_issue_lower_read(&self) -> bool {
        self.device_status() == DeviceStatus::D0ActiveAndConfigured
    }

    /// Registers lower read `read`, created and formatted but not yet sent. The glue stores the ticket in
    /// the read's context and then sends it; if the send fails it calls [`Self::finish_lower_read`] and
    /// deletes the read.
    ///
    /// # Errors
    /// [`ClaimRefused`] if the device is not configured or [`MAX_IN_FLIGHT_READS`] reads are in flight. The
    /// glue deletes the read without sending it.
    pub fn claim_lower_read<H: Copy, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
        read: H,
    ) -> Result<ReadTicket, ClaimRefused> {
        if self.device_status() != DeviceStatus::D0ActiveAndConfigured {
            return Err(ClaimRefused::NotConfigured);
        }
        tracker.claim(read).ok_or(ClaimRefused::TooManyInFlight)
    }

    /// Unregisters lower read `read` when it completes (or when its send failed) and tells whether it
    /// belongs to the current epoch. A read the last suspend handed out is no longer registered; its
    /// ticket is stale.
    pub fn finish_lower_read<H: Copy + PartialEq, const N: usize>(
        &self,
        tracker: &mut ReadTracker<H, N>,
        read: H,
        ticket: ReadTicket,
    ) -> LowerReadOrigin {
        tracker.finish(read, ticket)
    }

    /// The body of `AmtPtpRequestCompletionRoutine` (Input.c:157-321) for a [`LowerReadOrigin::Current`]
    /// read, after the glue retrieved the oldest request from `HidQueue` (if the queue is empty the glue
    /// drops the data and calls nothing here: SPI-B11, preserved).
    ///
    /// - `information`: `WdfRequestGetInformation(SpiRequest)`; see [`lower_read_length`]. Below 46
    ///   returns `Err(STATUS_DEVICE_DATA_ERROR)` without sampling the clock.
    /// - `buffer`: the lower read buffer. Only its first `min((LONG)information, buffer.len())` bytes are
    ///   read. They must hold every finger record `NumOfFingers` declares
    ///   ([`check_declared_fingers`]), else `Err(STATUS_DEVICE_DATA_ERROR)` without sampling the clock
    ///   (P3: C read missing fingers as 0 or as stale lookaside bytes). Pass only memory that is
    ///   initialized.
    /// - `contacts`: per-device [`ContactTracker`] maintaining stable `ContactID`s and liftoff frames.
    /// - `query_counter`: samples the QPC counter once after the length gate.
    ///
    /// On success the 30 report bytes are returned, honouring `PtpInputOn` and the selective-reporting
    /// switches ([`PtpSettings::effective_reporting`]).
    ///
    /// # Errors
    /// Returns `STATUS_DEVICE_DATA_ERROR` if the reported length is shorter than the packet header or
    /// than the finger records the header declares.
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
    use crate::status::STATUS_UNSUCCESSFUL;

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
        assert_eq!(rt.qpc_frequency(), 10_000_000);
        let ticket = rt
            .begin_recovery(&tracker)
            .expect("waiting for the mode switch");
        assert!(rt.finish_recovery(&mut tracker, ticket, STATUS_SUCCESS, || 7));
        assert_eq!(rt.qpc_frequency(), 7);
        assert!(rt.should_issue_lower_read());
        assert_eq!(rt.backlog_reads(&tracker, 3), 3);
        let (status, reads) = rt.on_d0_exit(&mut tracker);
        assert_eq!(status, STATUS_SUCCESS);
        assert_eq!(reads.iter().count(), 0);
        assert_eq!(rt.device_status(), DeviceStatus::D3);
        assert_eq!(rt.backlog_reads(&tracker, 3), 0);
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
        assert_eq!(
            rt.claim_lower_read(&mut tracker, 1),
            Err(ClaimRefused::NotConfigured)
        );
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
        assert_eq!(tracker.in_flight(), 3);
        let _ticket = rt
            .claim_lower_read(&mut tracker, 5)
            .expect("a slot is free again");
    }

    #[test]
    fn suspend_hands_out_the_reads_and_makes_them_stale() {
        let rt = DeviceRuntime::new();
        let mut tracker = Tracker::new();
        rt.on_d0_entry(1);
        let _outcome = rt.on_self_managed_io_init_or_restart(&mut tracker, STATUS_SUCCESS, || 1);
        let first = rt.claim_lower_read(&mut tracker, 10).expect("claim");
        let second = rt.claim_lower_read(&mut tracker, 11).expect("claim");
        let reads = rt.suspend(&mut tracker);
        assert!(reads.iter().eq([10, 11]));
        assert_eq!(tracker.in_flight(), 0);
        assert_eq!(
            rt.claim_lower_read(&mut tracker, 12),
            Err(ClaimRefused::NotConfigured)
        );
        // Resume: the old reads complete afterwards and are stale.
        rt.on_d0_entry(1);
        let _outcome = rt.on_self_managed_io_init_or_restart(&mut tracker, STATUS_SUCCESS, || 1);
        let third = rt.claim_lower_read(&mut tracker, 12).expect("claim");
        assert_eq!(
            rt.finish_lower_read(&mut tracker, 10, first),
            LowerReadOrigin::Stale
        );
        assert_eq!(
            rt.finish_lower_read(&mut tracker, 11, second),
            LowerReadOrigin::Stale
        );
        assert_eq!(tracker.in_flight(), 1);
        assert_eq!(
            rt.finish_lower_read(&mut tracker, 12, third),
            LowerReadOrigin::Current
        );
    }

    #[test]
    fn recovery_timer_only_configures_a_waiting_device() {
        let rt = DeviceRuntime::new();
        let mut tracker = Tracker::new();
        // Never in D0: nothing to recover.
        assert_eq!(rt.begin_recovery(&tracker), None);
        rt.on_d0_entry(1);
        let ticket = rt.begin_recovery(&tracker).expect("unconfigured");
        // Suspended while the mode switch ran.
        let _cancelled = rt.suspend(&mut tracker);
        assert!(!rt.finish_recovery(&mut tracker, ticket, STATUS_SUCCESS, || 1));
        assert_eq!(rt.device_status(), DeviceStatus::D3);
        // Resumed and configured by the restart before the stale callback finished.
        rt.on_d0_entry(1);
        let _outcome = rt.on_self_managed_io_init_or_restart(&mut tracker, STATUS_SUCCESS, || 1);
        assert!(!rt.finish_recovery(&mut tracker, ticket, STATUS_SUCCESS, || 1));
        assert_eq!(rt.begin_recovery(&tracker), None);
        // A failed mode switch changes nothing.
        let _cancelled = rt.suspend(&mut tracker);
        rt.on_d0_entry(1);
        let ticket = rt.begin_recovery(&tracker).expect("unconfigured");
        assert!(!rt.finish_recovery(&mut tracker, ticket, STATUS_UNSUCCESSFUL, || 1));
        assert_eq!(rt.device_status(), DeviceStatus::D0ActiveAndUnconfigured);
    }

    /// SPI-X12 (a): the glue passes an empty buffer when the completion parameters or the lookaside
    /// buffer are NULL (unreachable). It holds no header, so the frame is a data error.
    #[test]
    fn empty_lower_buffer_reads_as_zero_packet() {
        use crate::device::SPI_MODELS;
        use crate::status::STATUS_DEVICE_DATA_ERROR;
        use ptp_hid::common::REPORTID_MULTITOUCH;
        use ptp_hid::input::SpiPtpReport;

        let rt = DeviceRuntime::new();
        let tp = SPI_MODELS[0].trackpad;
        let mut contacts = ContactTracker::new();

        // Below 46: STATUS_DEVICE_DATA_ERROR, reporting clock not sampled.
        let mut queried = false;
        let short = rt.complete_lower_read(&tp, &mut contacts, 45, &[], || {
            queried = true;
            7
        });
        assert_eq!(short, Err(STATUS_DEVICE_DATA_ERROR));
        assert!(!queried);
        assert_eq!(rt.qpc_frequency(), 0);

        // No buffer at all: the header is missing, so the frame is rejected too.
        assert_eq!(
            rt.complete_lower_read(&tp, &mut contacts, 46, &[], || 0),
            Err(STATUS_DEVICE_DATA_ERROR)
        );

        // A zeroed 46-byte header: SUCCESS with the all-zero report (05, no contacts, ContactCount 0,
        // button 0); ScanTime starts at zero in this reporting session.
        let report = rt.complete_lower_read(&tp, &mut contacts, 46, &[0; 46], || 0);
        assert_eq!(report, Ok(SpiPtpReport::EMPTY.to_bytes()));
        assert_eq!(
            report.ok().and_then(|bytes| bytes.first().copied()),
            Some(REPORTID_MULTITOUCH)
        );
    }
}

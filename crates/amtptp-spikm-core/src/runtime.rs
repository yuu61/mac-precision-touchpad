//! The mutable per-device state the driver shares between its PnP/power callbacks, the IOCTL dispatcher
//! and the lower-read completion routine: C `DEVICE_CONTEXT.DeviceStatus`, `LastReportTime` and the
//! three PTP flags. C accessed them without synchronization (SPI-B15); here they are atomics with the
//! same single-variable semantics (P2).

use core::sync::atomic::{AtomicI64, AtomicU32, Ordering};

use ptp_hid::input::SPI_PTP_REPORT_LEN;

use crate::device::SpiTrackpadInfo;
use crate::feature::PtpSettings;
use crate::input::{SpiPacket, lower_read_length, report_bytes, scan_time, valid_prefix};
use crate::status::{NtStatus, STATUS_CANCELLED, STATUS_SUCCESS, nt_success};

/// `PTP_AAPL_DEVICE_POWER_STATUS` (Device.h:46-50).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DeviceStatus {
    /// `D3` (0): created, or after D0Exit.
    D3 = 0,
    /// `D0ActiveAndConfigured` (1): the multitouch SET_FEATURE succeeded; upstream reads issue lower
    /// reads.
    D0ActiveAndConfigured = 1,
    /// `D0ActiveAndUnconfigured` (2): after D0Entry until SetState succeeds.
    D0ActiveAndUnconfigured = 2,
}

impl DeviceStatus {
    /// Decodes the stored value. Only the three discriminants are ever stored; anything else maps to
    /// `D3` (unreachable).
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
/// Completion status for every request D0Exit drains from `HidQueue` (Information 0).
pub const D0_EXIT_DRAIN_STATUS: NtStatus = STATUS_CANCELLED;

/// What `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfManagedIoOutcome {
    /// Return value of the callback: always `STATUS_SUCCESS`.
    pub status: NtStatus,
    /// `Some(due_time)` when SetState failed: the glue calls `WdfTimerStart(PowerOnRecoveryTimer,
    /// due_time)` (re-arming a still pending timer, as KMDF does).
    pub start_recovery_timer: Option<i64>,
}

/// `DeviceStatus`, `LastReportTime` and the PTP flags of one device.
#[derive(Debug)]
pub struct DeviceRuntime {
    status: AtomicU32,
    last_report_time: AtomicI64,
    ptp: PtpSettings,
}

impl Default for DeviceRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceRuntime {
    /// The state after `AmtPtpDeviceSpiKmCreateDevice`: the zeroed context with `DeviceStatus = D3`,
    /// `LastReportTime = 0` and all PTP flags FALSE.
    pub const fn new() -> Self {
        Self {
            status: AtomicU32::new(DeviceStatus::D3 as u32),
            last_report_time: AtomicI64::new(0),
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

    /// `LastReportTime.QuadPart`.
    pub fn last_report_time(&self) -> i64 {
        self.last_report_time.load(Ordering::Relaxed)
    }

    /// The write-only PTP flags (`PtpInputOn`, `PtpReportButton`, `PtpReportTouch`).
    pub fn ptp_settings(&self) -> &PtpSettings {
        &self.ptp
    }

    /// `AmtPtpEvtDeviceD0Entry` (Device.c:319-351): `DeviceStatus = D0ActiveAndUnconfigured`, then
    /// `KeQueryPerformanceCounter(&LastReportTime)`.
    ///
    /// `qpc_frequency` is the value `KeQueryPerformanceCounter` writes through its **PerformanceFrequency**
    /// out-parameter: C passes `&LastReportTime` there and discards the returned counter (SPI-B8,
    /// preserved). The glue does the same: `KeQueryPerformanceCounter(&mut freq)` and passes `freq`.
    /// Returns `STATUS_SUCCESS`; no I/O.
    pub fn on_d0_entry(&self, qpc_frequency: i64) -> NtStatus {
        self.set_status(DeviceStatus::D0ActiveAndUnconfigured);
        self.last_report_time
            .store(qpc_frequency, Ordering::Relaxed);
        STATUS_SUCCESS
    }

    /// `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` (Device.c:397-427), registered for both
    /// SelfManagedIoInit and SelfManagedIoRestart, after the glue ran the mode switch
    /// ([`crate::wire::SetStatePacket`], synchronous `IOCTL_HID_SET_FEATURE`) and got `set_state_status`.
    ///
    /// - Success: `DeviceStatus = D0ActiveAndConfigured`, then `LastReportTime = qpc_frequency()` (the
    ///   frequency again, see [`Self::on_d0_entry`]). No lower read is issued, even if requests are already
    ///   queued (preserved, SPI-B13).
    /// - Failure: nothing changes; the outcome asks the glue to start the 5 s recovery timer.
    ///
    /// The callback always returns `STATUS_SUCCESS`.
    pub fn on_self_managed_io_init_or_restart(
        &self,
        set_state_status: NtStatus,
        qpc_frequency: impl FnOnce() -> i64,
    ) -> SelfManagedIoOutcome {
        if nt_success(set_state_status) {
            self.set_status(DeviceStatus::D0ActiveAndConfigured);
            self.last_report_time
                .store(qpc_frequency(), Ordering::Relaxed);
            SelfManagedIoOutcome {
                status: STATUS_SUCCESS,
                start_recovery_timer: None,
            }
        } else {
            SelfManagedIoOutcome {
                status: STATUS_SUCCESS,
                start_recovery_timer: Some(RECOVERY_TIMER_DUE_TIME),
            }
        }
    }

    /// `AmtPtpPowerRecoveryTimerCallback` (Device.c:541-562, PASSIVE_LEVEL work item), after the glue ran
    /// the mode switch again and got `set_state_status`.
    ///
    /// On success it calls `issue_lower_read` **exactly once** (one `AmtPtpSpiInputIssueRequest`, whatever
    /// the number of queued requests), **then** sets `DeviceStatus = D0ActiveAndConfigured`.
    /// `LastReportTime` is not refreshed. On failure nothing happens and nothing is rescheduled. This runs
    /// whatever the current `DeviceStatus` is, also in D3 (SPI-B12, preserved). Returns whether the read was
    /// issued.
    pub fn on_recovery_timer(
        &self,
        set_state_status: NtStatus,
        issue_lower_read: impl FnOnce(),
    ) -> bool {
        if nt_success(set_state_status) {
            issue_lower_read();
            self.set_status(DeviceStatus::D0ActiveAndConfigured);
            true
        } else {
            false
        }
    }

    /// `AmtPtpEvtDeviceD0Exit` (Device.c:353-395): `DeviceStatus = D3`. The glue then retrieves every
    /// request from `HidQueue` and completes it with [`D0_EXIT_DRAIN_STATUS`] (Information 0) until the
    /// queue is empty, and returns `STATUS_SUCCESS`. The recovery timer is not stopped, in-flight lower
    /// reads are not cancelled and the trackpad is not switched off (SPI-B12, SPI-B14, preserved).
    pub fn on_d0_exit(&self) -> NtStatus {
        self.set_status(DeviceStatus::D3);
        STATUS_SUCCESS
    }

    /// `AmtPtpSpiInputRoutineWorker` (Input.c:4-36), after `WdfRequestForwardToIoQueue(HidQueue)`
    /// succeeded: whether to issue one lower `IOCTL_HID_READ_REPORT` now (`DeviceStatus ==
    /// D0ActiveAndConfigured`). If the forward failed, the glue completes the request with that status
    /// and does not call this.
    pub fn should_issue_lower_read(&self) -> bool {
        self.device_status() == DeviceStatus::D0ActiveAndConfigured
    }

    /// The body of `AmtPtpRequestCompletionRoutine` (Input.c:157-321) after the glue retrieved the oldest
    /// request from `HidQueue` (if the queue is empty the glue drops the data and calls nothing here:
    /// SPI-B11, preserved).
    ///
    /// - `information`: `WdfRequestGetInformation(SpiRequest)`; see [`lower_read_length`]. Below 46
    ///   returns `Err(STATUS_DEVICE_DATA_ERROR)` without calling `qpc_frequency` or touching
    ///   `LastReportTime`.
    /// - `buffer`: the lower read buffer. Only its first `min((LONG)information, buffer.len())` bytes are
    ///   read; bytes past them read as 0 (P2 fix of SPI-B9). Pass only memory that is initialized.
    /// - `qpc_frequency`: called once, after the length gate; the glue returns what
    ///   `KeQueryPerformanceCounter(&x)` writes to `x` (the frequency, SPI-B8 preserved).
    ///
    /// On success `LastReportTime` holds the new value and the 30 report bytes are returned. The glue then
    /// does `WdfRequestRetrieveOutputMemory` + `WdfMemoryCopyFromBuffer(mem, 0, bytes, 30)` (either failing
    /// completes with that status, Information 0), `WdfRequestSetInformation(30)` and completes with
    /// `STATUS_SUCCESS`. On `Err(status)` it completes with `status`. Either way it then deletes the lower
    /// request and its memory (reading the memory handle **before** deleting the request: P2 fix of the
    /// use-after-free SPI-B10).
    pub fn complete_lower_read(
        &self,
        trackpad: &SpiTrackpadInfo,
        information: u64,
        buffer: &[u8],
        qpc_frequency: impl FnOnce() -> i64,
    ) -> Result<[u8; SPI_PTP_REPORT_LEN], NtStatus> {
        let length = lower_read_length(information)?;
        let packet = SpiPacket::new(valid_prefix(buffer, length));
        let current = qpc_frequency();
        // delta from the previous value, then store the current one (one atomic step instead of C's racy
        // load + store; identical when single-threaded).
        let last = self.last_report_time.swap(current, Ordering::Relaxed);
        Ok(report_bytes(&packet, trackpad, scan_time(current, last)))
    }

    /// SET_FEATURE (see [`PtpSettings::set_feature`]).
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

    #[test]
    fn power_sequence() {
        let rt = DeviceRuntime::new();
        assert_eq!(rt.device_status(), DeviceStatus::D3);
        rt.on_d0_entry(10_000_000);
        assert_eq!(rt.device_status(), DeviceStatus::D0ActiveAndUnconfigured);
        assert!(!rt.should_issue_lower_read());
        let out = rt.on_self_managed_io_init_or_restart(STATUS_UNSUCCESSFUL, || 1);
        assert_eq!(out.start_recovery_timer, Some(-50_000_000));
        assert_eq!(out.status, STATUS_SUCCESS);
        assert_eq!(rt.last_report_time(), 10_000_000);
        let mut issued = 0;
        assert!(rt.on_recovery_timer(STATUS_SUCCESS, || issued += 1));
        assert_eq!(issued, 1);
        assert!(rt.should_issue_lower_read());
        rt.on_d0_exit();
        assert_eq!(rt.device_status(), DeviceStatus::D3);
    }

    /// SPI-X12 (a): the glue passes an empty buffer when the completion parameters or the lookaside
    /// buffer are NULL (unreachable). Every packet byte then reads as 0.
    #[test]
    fn empty_lower_buffer_reads_as_zero_packet() {
        use crate::device::SPI_TRACKPAD_CONFIG_TABLE;
        use crate::status::STATUS_DEVICE_DATA_ERROR;
        use ptp_hid::common::REPORTID_MULTITOUCH;
        use ptp_hid::input::SpiPtpReport;

        let rt = DeviceRuntime::new();
        let tp = SPI_TRACKPAD_CONFIG_TABLE[0];

        // Below 46: STATUS_DEVICE_DATA_ERROR, QPC not queried, LastReportTime untouched.
        let mut queried = false;
        let short = rt.complete_lower_read(&tp, 45, &[], || {
            queried = true;
            7
        });
        assert_eq!(short, Err(STATUS_DEVICE_DATA_ERROR));
        assert!(!queried);
        assert_eq!(rt.last_report_time(), 0);

        // >= 46: SUCCESS with the all-zero report (05, no contacts, ContactCount 0, button 0); ScanTime
        // is computed as usual (0 here: same value as the stored LastReportTime).
        let report = rt.complete_lower_read(&tp, 46, &[], || 0);
        assert_eq!(report, Ok(SpiPtpReport::EMPTY.to_bytes()));
        assert_eq!(
            report.ok().and_then(|bytes| bytes.first().copied()),
            Some(REPORTID_MULTITOUCH)
        );
    }
}

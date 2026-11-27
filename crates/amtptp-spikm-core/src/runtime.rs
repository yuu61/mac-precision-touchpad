//! The mutable per-device state the driver shares between its PnP/power callbacks, the IOCTL dispatcher
//! and the lower-read completion routine: the device status, reporting clock, and three PTP flags.
//! They use atomic state so callbacks can access them concurrently (SPI-B15).

use core::sync::atomic::{AtomicU32, Ordering};

use ptp_hid::input::SPI_PTP_REPORT_LEN;
use ptp_hid::scan_time::ScanClock;

use crate::device::SpiTrackpadInfo;
use crate::feature::PtpSettings;
use crate::input::{SpiPacket, lower_read_length, report_bytes, valid_prefix};
use crate::status::{NtStatus, STATUS_CANCELLED, STATUS_SUCCESS, nt_success};

/// `PTP_AAPL_DEVICE_POWER_STATUS` (Device.h:46-50).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum DeviceStatus {
    /// `D3` (0): created, or after `D0Exit`.
    D3 = 0,
    /// `D0ActiveAndConfigured` (1): the multitouch `SET_FEATURE` succeeded; upstream reads issue lower
    /// reads.
    D0ActiveAndConfigured = 1,
    /// `D0ActiveAndUnconfigured` (2): after `D0Entry` until `SetState` succeeds.
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

/// What `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelfManagedIoOutcome {
    /// Return value of the callback: always `STATUS_SUCCESS`.
    pub status: NtStatus,
    /// `Some(due_time)` when `SetState` failed: the glue calls `WdfTimerStart(PowerOnRecoveryTimer,
    /// due_time)` (re-arming a still pending timer, as KMDF does).
    pub start_recovery_timer: Option<i64>,
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

    /// The write-only PTP flags (`PtpInputOn`, `PtpReportButton`, `PtpReportTouch`).
    pub const fn ptp_settings(&self) -> &PtpSettings {
        &self.ptp
    }

    /// Marks the device unconfigured and initializes the reporting clock for D0.
    pub fn on_d0_entry(&self, qpc_frequency: i64) -> NtStatus {
        self.set_status(DeviceStatus::D0ActiveAndUnconfigured);
        self.scan_clock.restart(qpc_frequency);
        STATUS_SUCCESS
    }

    /// `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` (Device.c:397-427), registered for both
    /// `SelfManagedIoInit` and `SelfManagedIoRestart`, after the glue ran the mode switch
    /// ([`crate::wire::SetStatePacket`], synchronous `IOCTL_HID_SET_FEATURE`) and got `set_state_status`.
    ///
    /// - Success: restart the reporting clock and mark the device configured. Already queued requests
    ///   still wait for a lower read (SPI-B13).
    /// - Failure: nothing changes; the outcome asks the glue to start the 5 s recovery timer.
    ///
    /// The callback always returns `STATUS_SUCCESS`.
    pub fn on_self_managed_io_init_or_restart<F: FnOnce() -> i64>(
        &self,
        set_state_status: NtStatus,
        qpc_frequency: F,
    ) -> SelfManagedIoOutcome {
        if nt_success(set_state_status) {
            self.scan_clock.restart(qpc_frequency());
            self.set_status(DeviceStatus::D0ActiveAndConfigured);
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

    /// `AmtPtpPowerRecoveryTimerCallback` (Device.c:541-562, `PASSIVE_LEVEL` work item), after the glue ran
    /// the mode switch again and got `set_state_status`.
    ///
    /// On success it calls `issue_lower_read` **exactly once** (one `AmtPtpSpiInputIssueRequest`, whatever
    /// the number of queued requests), **then** sets `DeviceStatus = D0ActiveAndConfigured`.
    /// the reporting clock is not refreshed. On failure nothing happens and nothing is rescheduled. This runs
    /// whatever the current `DeviceStatus` is, also in D3 (SPI-B12, preserved). Returns whether the read was
    /// issued.
    pub fn on_recovery_timer<F: FnOnce()>(
        &self,
        set_state_status: NtStatus,
        issue_lower_read: F,
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
    ///   returns `Err(STATUS_DEVICE_DATA_ERROR)` without sampling the clock.
    /// - `buffer`: the lower read buffer. Only its first `min((LONG)information, buffer.len())` bytes are
    ///   read; bytes past them read as 0 (P2 fix of SPI-B9). Pass only memory that is initialized.
    /// - `query_counter`: samples the QPC counter once after the length gate.
    ///
    /// On success the 30 report bytes are returned. The glue then
    /// does `WdfRequestRetrieveOutputMemory` + `WdfMemoryCopyFromBuffer(mem, 0, bytes, 30)` (either failing
    /// completes with that status, Information 0), `WdfRequestSetInformation(30)` and completes with
    /// `STATUS_SUCCESS`. On `Err(status)` it completes with `status`. Either way it then deletes the lower
    /// request and its memory (reading the memory handle **before** deleting the request: P2 fix of the
    /// use-after-free SPI-B10).
    ///
    /// # Errors
    /// Returns `STATUS_DEVICE_DATA_ERROR` if the reported length is shorter than the packet header.
    pub fn complete_lower_read<F: FnOnce() -> i64>(
        &self,
        trackpad: &SpiTrackpadInfo,
        information: u64,
        buffer: &[u8],
        query_counter: F,
    ) -> Result<[u8; SPI_PTP_REPORT_LEN], NtStatus> {
        let length = lower_read_length(information)?;
        let packet = SpiPacket::new(valid_prefix(buffer, length));
        let scan_time = self.scan_clock.sample(query_counter());
        Ok(report_bytes(&packet, trackpad, scan_time))
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
        assert_eq!(rt.qpc_frequency(), 10_000_000);
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

        // Below 46: STATUS_DEVICE_DATA_ERROR, reporting clock not sampled.
        let mut queried = false;
        let short = rt.complete_lower_read(&tp, 45, &[], || {
            queried = true;
            7
        });
        assert_eq!(short, Err(STATUS_DEVICE_DATA_ERROR));
        assert!(!queried);
        assert_eq!(rt.qpc_frequency(), 0);

        // >= 46: SUCCESS with the all-zero report (05, no contacts, ContactCount 0, button 0); ScanTime
        // starts at zero in this reporting session.
        let report = rt.complete_lower_read(&tp, 46, &[], || 0);
        assert_eq!(report, Ok(SpiPtpReport::EMPTY.to_bytes()));
        assert_eq!(
            report.ok().and_then(|bytes| bytes.first().copied()),
            Some(REPORTID_MULTITOUCH)
        );
    }
}

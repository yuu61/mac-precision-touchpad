//! PnP/power callbacks, the mode switch and the recovery timer (Device.c:104-443).

use super::Filter;
use super::pipeline::issue_backlog_reads;
use amtptp_usb_bt::bluetooth::config::CONFIGURE_RETRY_DUE_TIME;
use amtptp_usb_bt::bluetooth::{DeviceState, HID_PID_MAGIC_TRACKPAD_2, HID_VID_APPLE_BT};
use amtptp_usb_bt::bluetooth::{NtStatus, STATUS_CANCELLED, STATUS_SUCCESS, nt_success};

/// `PtpFilterPrepareHardware` (Device.c:104-130): clear `DeviceConfigured` and turn both
/// selective-reporting switches on (not in C: the state is not kept across a power reset). Returns
/// `STATUS_SUCCESS`.
///
/// VID, PID and version are kept (P3 fix of HF-B30). They identify the lower device, which a PnP stop
/// and restart (resource rebalance: `D0Exit`, `PrepareHardware`, `D0Entry`, `SelfManagedIoRestart`)
/// does not change, and only `SelfManagedIoInit` sets them. The C driver zeroed them here, so after a
/// rebalance the mode switch failed with `STATUS_NOT_SUPPORTED` forever while the restart reported
/// success. Transport parameters and the reporting clock are kept too.
pub(crate) fn prepare_hardware(state: &DeviceState) -> NtStatus {
    state.set_device_configured(false);
    state.reset_selective_reporting();
    STATUS_SUCCESS
}

/// `PtpFilterDeviceD0Entry` (Device.c:132-147): the device is in D0 (not in C, which did nothing). Only
/// then does the recovery timer work on it.
pub(crate) fn d0_entry(state: &DeviceState) -> NtStatus {
    state.enter_d0();
    STATUS_SUCCESS
}

/// `PtpFilterDeviceD0Exit` (Device.c:149-182):
/// 1. clear `DeviceConfigured` and leave D0: a recovery timer or work item that runs from now on does
///    nothing, one whose mode switch is running cannot mark the device configured, and the read tracker
///    refuses new reads;
/// 2. stop the recovery timer without waiting, and cancel the lower reads in flight in a new epoch, so
///    their completions are dropped (not in C: P3 fix of HF-B14, which left both alive across the power
///    transition; a read that completed after the resume served an upper read with stale data);
/// 3. complete every parked upper read with `STATUS_CANCELLED` until the queue reports an error.
///
/// Returns `STATUS_SUCCESS`.
pub(crate) fn d0_exit(io: &mut Filter<'_>, state: &DeviceState) -> NtStatus {
    state.leave_d0();
    io.stop_recovery_timer();
    let _cancelled = io.cancel_in_flight_reads();
    io.with_contacts(amtptp_core::domain::ContactTracker::reset);
    while let Ok(request) = io.retrieve_next_parked_read() {
        io.complete(request, STATUS_CANCELLED);
    }
    STATUS_SUCCESS
}

/// `PtpFilterSelfManagedIoInit` (Device.c:184-245), run once at the first start.
///
/// The C driver applied the HIDCLASS detour here and read VID, PID and version from the PDO through
/// it. The detour is gone (`bth`): the device's identity is static, the Bluetooth Magic Trackpad 2
/// (the only node the INF installs the filter on), and is set before the upper HIDCLASS asks for the
/// descriptors. The data source (the vendor collection) opens asynchronously; the glue reports it
/// with [`source_opened`], which configures the device.
///
/// If the source is already open: [`configure_multitouch`]; on failure arm the 2 s timer and return
/// SUCCESS with `DeviceConfigured` still FALSE; on success cache the QPC frequency, restart the
/// reporting clock, set `DeviceConfigured`, and issue a lower read for every upper read parked
/// meanwhile ([`issue_backlog_reads`]). Always returns `STATUS_SUCCESS`.
pub(crate) fn self_managed_io_init(io: &mut Filter<'_>, state: &DeviceState) -> NtStatus {
    state.set_ids(HID_VID_APPLE_BT, HID_PID_MAGIC_TRACKPAD_2, 0);
    if state.source_open() {
        configure_and_start(io, state);
    }
    STATUS_SUCCESS
}

/// `PtpFilterSelfManagedIoRestart` (Device.c:247-284), run on every return to D0 after the first:
/// - source open: [`configure_multitouch`]; on failure arm the 2 s timer and return **SUCCESS**
///   without touching `DeviceConfigured` (HF-B12); on success restart the reporting clock, set
///   `DeviceConfigured` and issue a lower read for every upper read parked while the device was
///   unconfigured ([`issue_backlog_reads`], not in C);
/// - source not open: nothing; [`source_opened`] configures the device later. (C, without the
///   detour, marked the device configured and returned `STATUS_INVALID_STATE_TRANSITION`, HF-B11.)
pub(crate) fn self_managed_io_restart(io: &mut Filter<'_>, state: &DeviceState) -> NtStatus {
    if state.source_open() {
        configure_and_start(io, state);
    }
    STATUS_SUCCESS
}

/// The mode switch, then the reads: the common end of init, restart and [`source_opened`].
fn configure_and_start(io: &mut Filter<'_>, state: &DeviceState) {
    if !nt_success(configure_multitouch(io, state)) {
        io.start_recovery_timer(CONFIGURE_RETRY_DUE_TIME);
        return;
    }
    mark_configured(io, state);
    issue_backlog_reads(io, state);
}

/// The glue opened the data source (the vendor collection): configure the device if it is in D0,
/// as the recovery timer would ([`recovery_timer_callback`]). Out of D0, `SelfManagedIoRestart`
/// configures it on the way back.
pub(crate) fn source_opened(io: &mut Filter<'_>, state: &DeviceState) {
    state.set_source_open(true);
    recovery_timer_callback(io, state);
}

/// The data source is going away (query-remove or removal of the vendor collection): the device is
/// no longer configured, the recovery timer stops, and the lower reads in flight are cancelled in a
/// new epoch, so their completions are dropped. Parked upper reads stay parked until the source opens
/// again. Call before closing the source.
pub(crate) fn source_closed(io: &mut Filter<'_>, state: &DeviceState) {
    state.set_source_open(false);
    state.set_device_configured(false);
    io.stop_recovery_timer();
    let _cancelled = io.cancel_in_flight_reads();
    io.with_contacts(amtptp_core::domain::ContactTracker::reset);
}

/// Caches the QPC frequency, resets tracked contacts, and starts a new reporting session.
fn mark_configured(io: &mut Filter<'_>, state: &DeviceState) {
    io.with_contacts(amtptp_core::domain::ContactTracker::reset);
    let frequency = io.query_performance_frequency();
    state.set_qpc_frequency(frequency);
    state.set_device_configured(true);
}

/// `PtpFilterRecoveryTimerCallback` (Device.c:424-443), armed after a failed mode switch (2 s) or a
/// failed lower-read send or malformed packet (3 s).
///
/// It only works on a device in D0 (P3 fix: the C callback ran in D3 too, and issued a lower read even
/// when its mode switch had failed, HF-B12):
/// - configured: issue the lower reads the parked upper reads are missing ([`issue_backlog_reads`]);
/// - source not open: nothing (it is configured when the source opens);
/// - otherwise [`configure_multitouch`]. On failure the timer is armed again while the device is still
///   in D0. On success the reporting clock restarts and, unless the device left D0 meanwhile, it is
///   marked configured and the backlog is issued.
pub(crate) fn recovery_timer_callback(io: &mut Filter<'_>, state: &DeviceState) {
    let Some(ticket) = state.begin_recovery() else {
        return;
    };
    if state.device_configured() {
        issue_backlog_reads(io, state);
        return;
    }
    if !state.source_open() {
        return;
    }
    if !nt_success(configure_multitouch(io, state)) {
        if state.in_d0() {
            io.start_recovery_timer(CONFIGURE_RETRY_DUE_TIME);
        }
        return;
    }
    let frequency = io.query_performance_frequency();
    state.set_qpc_frequency(frequency);
    if state.finish_recovery(ticket) {
        issue_backlog_reads(io, state);
    }
}

/// `PtpFilterConfigureMultiTouch` (Device.c:286-422): the "wellspring" mode switch.
///
/// Selects the report for the current transport, applies its input layout and returns
/// the status of the synchronous feature request to the HID collection.
pub(crate) fn configure_multitouch(io: &mut Filter<'_>, state: &DeviceState) -> NtStatus {
    match state.mode_switch_report() {
        Ok(report) => io.send_mode_report(report),
        Err(status) => status,
    }
}

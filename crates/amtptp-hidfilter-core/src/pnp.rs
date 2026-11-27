//! PnP/power callbacks, the mode switch and the recovery timer (Device.c:104-443).

use ptp_hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;

use crate::config::CONFIGURE_RETRY_DUE_TIME;
use crate::detour::{DetourIo, detour_hid_stack};
use crate::device::{DeviceState, Transport, XferBuffer};
use crate::io::FilterIo;
use crate::pipeline::issue_backlog_reads;
use crate::status::{
    NtStatus, STATUS_CANCELLED, STATUS_INVALID_STATE_TRANSITION, STATUS_SUCCESS,
    STATUS_UNSUCCESSFUL, nt_success,
};

/// `PtpFilterPrepareHardware` (Device.c:104-130): clear `DeviceConfigured` and turn both
/// selective-reporting switches on (not in C: the state is not kept across a power reset). Returns
/// `STATUS_SUCCESS`.
///
/// VID, PID and version are kept (P3 fix of HF-B30). They identify the lower device, which a PnP stop
/// and restart (resource rebalance: `D0Exit`, `PrepareHardware`, `D0Entry`, `SelfManagedIoRestart`)
/// does not change, and only `SelfManagedIoInit` queries them. The C driver zeroed them here, so after a
/// rebalance the mode switch failed with `STATUS_NOT_SUPPORTED` forever while the restart reported
/// success. Transport parameters and the reporting clock are kept too.
pub fn prepare_hardware(state: &DeviceState) -> NtStatus {
    state.set_device_configured(false);
    state.reset_selective_reporting();
    STATUS_SUCCESS
}

/// `PtpFilterDeviceD0Entry` (Device.c:132-147): the device is in D0 (not in C, which did nothing). Only
/// then does the recovery timer work on it.
pub fn d0_entry(state: &DeviceState) -> NtStatus {
    state.enter_d0();
    STATUS_SUCCESS
}

/// `PtpFilterDeviceD0Exit` (Device.c:149-182): clear `DeviceConfigured`, leave D0 (a recovery timer that
/// fires from now on does nothing, and one whose mode switch is running cannot mark the device
/// configured), then complete every parked upper read with `STATUS_CANCELLED` until the queue reports an
/// error. In-flight lower reads are left alone (HF-B14). Returns `STATUS_SUCCESS`.
pub fn d0_exit<P: FilterIo>(io: &mut P, state: &DeviceState) -> NtStatus {
    state.leave_d0();
    while let Ok(request) = io.retrieve_next_parked_read() {
        io.complete(request, STATUS_CANCELLED);
    }
    STATUS_SUCCESS
}

/// `PtpFilterSelfManagedIoInit` (Device.c:184-245), run once at the first start:
/// 1. apply the HIDCLASS detour ([`detour_hid_stack`]); its failure fails the start;
/// 2. query `HID_DEVICE_ATTRIBUTES` from the PDO; failure fails the start; store VID, PID, version;
/// 3. [`configure_multitouch`]; on failure arm the 2 s timer and return SUCCESS with
///    `DeviceConfigured` still FALSE;
/// 4. cache the QPC frequency, restart the reporting clock, set `DeviceConfigured`, and issue a lower
///    read for every upper read parked meanwhile ([`issue_backlog_reads`], not in C).
pub fn self_managed_io_init<P: FilterIo + DetourIo>(io: &mut P, state: &DeviceState) -> NtStatus {
    let status = detour_hid_stack(io, state);
    if !nt_success(status) {
        return status;
    }
    let mut attributes = [0_u8; HID_DEVICE_ATTRIBUTES_LEN];
    let status = io.query_device_attributes(&mut attributes);
    if !nt_success(status) {
        return status;
    }
    // HID_DEVICE_ATTRIBUTES: Size u32, VendorID, ProductID, VersionNumber (u16 LE each). The returned
    // size is not checked, as in C.
    let [_, _, _, _, v0, v1, p0, p1, n0, n1, ..] = attributes;
    state.set_ids(
        u16::from_le_bytes([v0, v1]),
        u16::from_le_bytes([p0, p1]),
        u16::from_le_bytes([n0, n1]),
    );
    if !nt_success(configure_multitouch(io, state)) {
        io.start_recovery_timer(CONFIGURE_RETRY_DUE_TIME);
        return STATUS_SUCCESS;
    }
    mark_configured(io, state);
    issue_backlog_reads(io, state);
    STATUS_SUCCESS
}

/// `PtpFilterSelfManagedIoRestart` (Device.c:247-284), run on every return to D0 after the first:
/// - detour done: [`configure_multitouch`]; on failure arm the 2 s timer and return **SUCCESS**
///   without touching `DeviceConfigured` (HF-B12); on success restart the reporting clock, set
///   `DeviceConfigured` and issue a lower read for every upper read parked while the device was
///   unconfigured ([`issue_backlog_reads`], not in C);
/// - detour not done: restart the reporting clock and set `DeviceConfigured`, but return
///   `STATUS_INVALID_STATE_TRANSITION` (HF-B11).
pub fn self_managed_io_restart<P: FilterIo>(io: &mut P, state: &DeviceState) -> NtStatus {
    if !state.detour_completed() {
        mark_configured(io, state);
        return STATUS_INVALID_STATE_TRANSITION;
    }
    if !nt_success(configure_multitouch(io, state)) {
        io.start_recovery_timer(CONFIGURE_RETRY_DUE_TIME);
        return STATUS_SUCCESS;
    }
    mark_configured(io, state);
    issue_backlog_reads(io, state);
    STATUS_SUCCESS
}

/// Caches the QPC frequency and starts a new reporting session.
fn mark_configured<P: FilterIo>(io: &mut P, state: &DeviceState) {
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
/// - detour not done: nothing (the timer is only armed after the detour);
/// - otherwise [`configure_multitouch`]. On failure the timer is armed again while the device is still
///   in D0. On success the reporting clock restarts and, unless the device left D0 meanwhile, it is
///   marked configured and the backlog is issued.
pub fn recovery_timer_callback<P: FilterIo>(io: &mut P, state: &DeviceState) {
    let Some(ticket) = state.begin_recovery() else {
        return;
    };
    if state.device_configured() {
        issue_backlog_reads(io, state);
        return;
    }
    if !state.detour_completed() {
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
/// 1. [`Transport::from_ids`] on the stored VID/PID; `STATUS_NOT_SUPPORTED` writes nothing.
/// 2. Write the transport parameters (before the send; they stay set if the send fails).
/// 3. Build the 32-byte [`XferBuffer`] (USB: id 0x02, `02 01 00 00`; BT: id 0xF1, `F1 02 01`).
/// 4. Create the request (failure: return it), preallocated memory over the buffer, format
///    `IOCTL_HID_SET_FEATURE`, `Irp->UserBuffer = buffer` (NULL IRP: `STATUS_UNSUCCESSFUL`), send
///    synchronously with no timeout, and return `WdfRequestGetStatus` whatever the send returned (P3
///    fix of HF-B13: C treated a send returning TRUE as success and dropped the lower driver's status).
/// 5. Delete the request on every path after it was created.
pub fn configure_multitouch<P: FilterIo>(io: &mut P, state: &DeviceState) -> NtStatus {
    let transport = match Transport::from_ids(state.vendor_id(), state.product_id()) {
        Ok(t) => t,
        Err(status) => return status,
    };
    state.apply_transport_params(&transport.params());
    let mut packet = XferBuffer::new(transport.mode_switch());
    packet.link_report_buffer();
    let request = match io.create_mode_switch_request() {
        Ok(r) => r,
        Err(status) => return status,
    };
    let status = send_mode_switch(io, request, &mut packet);
    io.delete_mode_switch_request(request);
    status
}

fn send_mode_switch<P: FilterIo>(
    io: &mut P,
    request: P::ModeSwitchRequest,
    packet: &mut XferBuffer,
) -> NtStatus {
    let memory = match io.create_mode_switch_memory(request, packet) {
        Ok(m) => m,
        Err(status) => return status,
    };
    let status = io.format_mode_switch(request, memory);
    if !nt_success(status) {
        return status;
    }
    if !io.set_mode_switch_user_buffer(request, packet) {
        return STATUS_UNSUCCESSFUL;
    }
    // TRUE: the request completed and its status is the lower driver's; FALSE: the framework could not
    // send it and the status says why.
    let _sent = io.send_mode_switch_synchronously(request);
    io.mode_switch_status(request)
}

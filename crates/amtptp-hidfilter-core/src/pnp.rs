//! PnP/power callbacks, the mode switch and the recovery timer (Device.c:104-443).

use ptp_hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;

use crate::config::CONFIGURE_RETRY_DUE_TIME;
use crate::detour::{DetourIo, detour_hid_stack};
use crate::device::{DeviceState, Transport, XferBuffer};
use crate::io::FilterIo;
use crate::pipeline::issue_lower_read;
use crate::status::{
    NtStatus, STATUS_CANCELLED, STATUS_INVALID_STATE_TRANSITION, STATUS_SUCCESS,
    STATUS_UNSUCCESSFUL, nt_success,
};

/// `PtpFilterPrepareHardware` (Device.c:104-130): zero VID, PID and version and clear
/// `DeviceConfigured`. Transport parameters and `LastReportTime` are kept.
///
/// Preserved (HF-B30): KMDF also calls this on the restart after a PnP stop, and nothing re-reads the
/// IDs afterwards, so the mode switch then fails forever and the first timer-issued read fails the
/// device. Returns `STATUS_SUCCESS`.
pub fn prepare_hardware(state: &DeviceState) -> NtStatus {
    state.set_ids(0, 0, 0);
    state.set_device_configured(false);
    STATUS_SUCCESS
}

/// `PtpFilterDeviceD0Entry` (Device.c:132-147): does nothing.
pub const fn d0_entry() -> NtStatus {
    STATUS_SUCCESS
}

/// `PtpFilterDeviceD0Exit` (Device.c:149-182): clear `DeviceConfigured`, then complete every parked
/// upper read with `STATUS_CANCELLED` until the queue reports an error. The timer, the work item and
/// in-flight lower reads are left alone (HF-B14). Returns `STATUS_SUCCESS`.
pub fn d0_exit<P: FilterIo>(io: &mut P, state: &DeviceState) -> NtStatus {
    state.set_device_configured(false);
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
/// 4. stamp `LastReportTime` with the QPC frequency (HF-B6) and set `DeviceConfigured`.
pub fn self_managed_io_init<P: FilterIo + DetourIo>(io: &mut P, state: &DeviceState) -> NtStatus {
    let status = detour_hid_stack(io, state);
    if !nt_success(status) {
        return status;
    }
    let mut attributes = [0u8; HID_DEVICE_ATTRIBUTES_LEN];
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
    STATUS_SUCCESS
}

/// `PtpFilterSelfManagedIoRestart` (Device.c:247-284), run on every return to D0 after the first and
/// by the recovery timer:
/// - detour done: [`configure_multitouch`]; on failure arm the 2 s timer and return **SUCCESS**
///   without touching `DeviceConfigured` (HF-B12);
/// - detour not done: status `STATUS_INVALID_STATE_TRANSITION`, but fall through (HF-B11);
/// - then stamp `LastReportTime`, set `DeviceConfigured = TRUE` and return the status.
pub fn self_managed_io_restart<P: FilterIo>(io: &mut P, state: &DeviceState) -> NtStatus {
    let status = if state.detour_completed() {
        if !nt_success(configure_multitouch(io, state)) {
            io.start_recovery_timer(CONFIGURE_RETRY_DUE_TIME);
            return STATUS_SUCCESS;
        }
        STATUS_SUCCESS
    } else {
        STATUS_INVALID_STATE_TRANSITION
    };
    mark_configured(io, state);
    status
}

/// `KeQueryPerformanceCounter(&ctx->LastReportTime)` (stores the frequency) and
/// `DeviceConfigured = TRUE`.
fn mark_configured<P: FilterIo>(io: &mut P, state: &DeviceState) {
    let frequency = io.query_performance_frequency();
    state.store_last_report_time(frequency);
    state.set_device_configured(true);
}

/// `PtpFilterRecoveryTimerCallback` (Device.c:424-443): [`self_managed_io_restart`], and if that
/// returns success (which includes a masked mode-switch failure, HF-B12) issue one lower read.
pub fn recovery_timer_callback<P: FilterIo>(io: &mut P, state: &DeviceState) {
    if nt_success(self_managed_io_restart(io, state)) {
        issue_lower_read(io, state);
    }
}

/// `PtpFilterConfigureMultiTouch` (Device.c:286-422): the "wellspring" mode switch.
///
/// 1. [`Transport::from_ids`] on the stored VID/PID; `STATUS_NOT_SUPPORTED` writes nothing.
/// 2. Write the transport parameters (before the send; they stay set if the send fails).
/// 3. Build the 32-byte [`XferBuffer`] (USB: id 0x02, `02 01 00 00`; BT: id 0xF1, `F1 02 01`).
/// 4. Create the request (failure: return it), preallocated memory over the buffer, format
///    `IOCTL_HID_SET_FEATURE`, `Irp->UserBuffer = buffer` (NULL IRP: `STATUS_UNSUCCESSFUL`), send
///    synchronously with no timeout. A send returning TRUE is SUCCESS whatever the request status
///    (HF-B13); FALSE returns `WdfRequestGetStatus`.
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
    if io.send_mode_switch_synchronously(request) {
        STATUS_SUCCESS
    } else {
        io.mode_switch_status(request)
    }
}

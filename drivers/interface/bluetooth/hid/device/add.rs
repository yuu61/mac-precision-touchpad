//! Framework object initialization for the HID filter.

use super::AddDevice;
use amtptp_usb_bt::bluetooth::{NtStatus, STATUS_UNSUCCESSFUL, nt_success};

/// `AddDevice` in C order. Returns the first failure.
///
/// Deviations (port policy P2; C would later use a NULL handle and bugcheck):
/// - a NULL WDM device object returns `STATUS_UNSUCCESSFUL` (C returns SUCCESS and creates nothing
///   more, HF-B17);
/// - a failure creating the lookaside list, the timer or the work item is returned at once (C logs it
///   and continues, HF-B16).
pub(crate) fn add_device(io: &mut AddDevice) -> NtStatus {
    io.fdo_init_set_filter();
    io.set_pnp_power_event_callbacks();
    let status = io.create_device();
    if !nt_success(status) {
        return status;
    }
    if !io.store_wdm_device_object() {
        return STATUS_UNSUCCESSFUL;
    }
    for step in [
        AddDevice::create_read_buffer_lookaside,
        AddDevice::create_recovery_timer,
        AddDevice::create_recovery_work_item,
        AddDevice::create_default_queue,
    ] {
        let status = step(io);
        if !nt_success(status) {
            return status;
        }
    }
    io.create_read_queue()
}

//! `PtpFilterEvtDeviceAdd` + `PtpFilterCreateDevice` + `PtpFilterIoQueueInitialize`
//! (Driver.c:45-66, Device.c:10-102, Queue.c:10-54): which object-creation failures fail `AddDevice`.

use crate::status::{NtStatus, STATUS_UNSUCCESSFUL, nt_success};

/// The object creations of `AddDevice`, one method per C call. The configuration of each object is in
/// [`crate::config`].
pub trait AddDeviceIo {
    /// `WdfFdoInitSetFilter(DeviceInit)`.
    fn fdo_init_set_filter(&mut self);
    /// `WdfDeviceInitSetPnpPowerEventCallbacks`: `PrepareHardware`, `D0Entry`, `D0Exit`,
    /// `SelfManagedIoInit`, `SelfManagedIoRestart` (nothing else).
    fn set_pnp_power_event_callbacks(&mut self);
    /// `WdfDeviceCreate` with the device context; on success the glue initializes the context
    /// (`DeviceState::new()`, `Device` handle).
    fn create_device(&mut self) -> NtStatus;
    /// `ctx->WdmDeviceObject = WdfDeviceWdmGetDeviceObject(device)`; false if NULL.
    fn store_wdm_device_object(&mut self) -> bool;
    /// `WdfDeviceCreateDeviceInterface(device, &GUID_DEVICEINTERFACE_AmtPtpHidFilter, NULL)`.
    fn create_device_interface(&mut self) -> NtStatus;
    /// `WdfLookasideListCreate` (1024 bytes, `NonPagedPoolNx`, tag 'LTPA', parent device: HF-B15).
    fn create_read_buffer_lookaside(&mut self) -> NtStatus;
    /// `WdfTimerCreate` (one-shot, passive, parent device, `AutomaticSerialization` FALSE: HF-B16).
    fn create_recovery_timer(&mut self) -> NtStatus;
    /// `WdfWorkItemCreate` (parent device, `AutomaticSerialization` FALSE: HF-B16).
    fn create_recovery_work_item(&mut self) -> NtStatus;
    /// `WdfIoQueueCreate` of the default parallel queue (with its `QUEUE_CONTEXT.Device`).
    fn create_default_queue(&mut self) -> NtStatus;
    /// `WdfIoQueueCreate` of the manual `HidReadQueue`.
    fn create_read_queue(&mut self) -> NtStatus;
}

/// `AddDevice` in C order. Returns the first failure.
///
/// Deviations (port policy P2; C would later use a NULL handle and bugcheck):
/// - a NULL WDM device object returns `STATUS_UNSUCCESSFUL` (C returns SUCCESS and creates nothing
///   more, HF-B17);
/// - a failure creating the lookaside list, the timer or the work item is returned at once (C logs it
///   and continues, HF-B16).
pub fn add_device<P: AddDeviceIo>(io: &mut P) -> NtStatus {
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
        P::create_device_interface,
        P::create_read_buffer_lookaside,
        P::create_recovery_timer,
        P::create_recovery_work_item,
        P::create_default_queue,
    ] {
        let status = step(io);
        if !nt_success(status) {
            return status;
        }
    }
    io.create_read_queue()
}

//! Device creation and the PnP/power, timer and work-item callbacks (`Driver.c:45-66`, `Device.c`,
//! `Queue.c:10-54`, `Input.c:30-37`).
//!
//! `evt_device_add` runs [`add_device`] with [`AddDevice`], which makes the C `WdfDeviceInit*` /
//! `Wdf*Create` calls in C order with the C configuration (`amtptp_hidfilter_core::config`), except
//! the recorded P2 deltas: the lookaside list is parented to the device (HF-B15), the timer and
//! work item use `AutomaticSerialization = FALSE` (HF-B16), and every creation failure fails
//! `AddDevice` (HF-B16, HF-B17).
//!
//! Registered PnP/power callbacks, as in C: `PrepareHardware`, `D0Entry`, `D0Exit`, `SelfManagedIoInit`,
//! `SelfManagedIoRestart`. Nothing else (no `ReleaseHardware`, no SelfManagedIoSuspend/Flush/Cleanup,
//! no power-policy settings, no file-object configuration, no device cleanup callback).

use core::ptr::{null, null_mut};

use amtptp_hidfilter_core::config::{
    DEVICE_INTERFACE_GUID, READ_BUFFER_LOOKASIDE_PARENT_IS_DEVICE, READ_BUFFER_POOL_TAG,
    READ_QUEUE_POWER_MANAGED, RECOVERY_TIMER_AUTOMATIC_SERIALIZATION, RECOVERY_TIMER_PERIOD,
    RECOVERY_WORK_ITEM_AUTOMATIC_SERIALIZATION,
};
use amtptp_hidfilter_core::{
    AddDeviceIo, NtStatus, REPORT_BUFFER_SIZE, STATUS_INVALID_DEVICE_STATE, add_device, d0_entry,
    d0_exit, prepare_hardware, recovery_timer_callback, self_managed_io_init,
    self_managed_io_restart, work_item_callback,
};
use tracelogging as tlg;
use wdf_support::handle::WdfHandle;
use wdf_support::hid as wdf_hid;
use wdf_support::init::{
    object_attributes_init, object_attributes_init_context_type, pnppower_event_callbacks_init,
    timer_config_init, workitem_config_init,
};
use wdf_support::ntstatus::NT_SUCCESS;
use wdk_sys::{
    _POOL_TYPE, _WDF_EXECUTION_LEVEL, BOOLEAN, GUID, NTSTATUS, PWDFDEVICE_INIT,
    WDF_NO_OBJECT_ATTRIBUTES, WDF_POWER_DEVICE_STATE, WDFCMRESLIST, WDFDEVICE, WDFDRIVER,
    WDFLOOKASIDE, WDFQUEUE, WDFTIMER, WDFWORKITEM, call_unsafe_wdf_function_binding,
};

use crate::context::{
    DEVICE_CONTEXT_TYPE_INFO, DeviceContext, QUEUE_CONTEXT_TYPE_INFO, QueueContext, device_context,
    device_get_context, queue_get_context,
};
use crate::io::Filter;
use crate::{KW_DEVICE, KW_DRIVER, PROVIDER, queue};

/// `GUID_DEVICEINTERFACE_AmtPtpHidFilter` `{FF969022-3111-4441-8F88-875440172C2E}` (Device.h:7-9).
static GUID_DEVICEINTERFACE_AMTPTP_HID_FILTER: GUID = GUID {
    Data1: DEVICE_INTERFACE_GUID.0,
    Data2: DEVICE_INTERFACE_GUID.1,
    Data3: DEVICE_INTERFACE_GUID.2,
    Data4: DEVICE_INTERFACE_GUID.3,
};

/// C `TRUE`/`FALSE` for BOOLEAN fields.
const fn boolean(value: bool) -> BOOLEAN {
    if value { 1 } else { 0 }
}

// The port's object configuration (P2 deltas from C, see the module docs).
const _: () = assert!(READ_BUFFER_LOOKASIDE_PARENT_IS_DEVICE);
const _: () = assert!(!READ_QUEUE_POWER_MANAGED);

/// `PtpFilterEvtDeviceAdd` (Driver.c:45-66) + `PtpFilterCreateDevice` (Device.c:10-102) +
/// `PtpFilterIoQueueInitialize` (Queue.c:10-54), at `PASSIVE_LEVEL`, in `PAGE` as in C.
#[unsafe(link_section = "PAGE")]
pub(crate) unsafe extern "C" fn evt_device_add(
    _driver: WDFDRIVER,
    device_init: PWDFDEVICE_INIT,
) -> NTSTATUS {
    wdk::paged_code!();
    let mut glue = AddDevice {
        device_init,
        device: null_mut(),
        context: null(),
    };
    let status = add_device(&mut glue);
    tlg::write_event!(
        PROVIDER,
        "DeviceAdd",
        level(Informational),
        keyword(KW_DRIVER),
        win_ntstatus("Status", &status)
    );
    status
}

/// [`AddDeviceIo`]: the object creations of `AddDevice`. Only used inside `evt_device_add`
/// (`PASSIVE_LEVEL`), before the device can receive any other callback.
struct AddDevice {
    /// The framework's `WDFDEVICE_INIT`; set to NULL by a successful `WdfDeviceCreate`.
    device_init: PWDFDEVICE_INIT,
    /// The created device (NULL before `create_device` succeeded).
    device: WDFDEVICE,
    /// Its initialized context (NULL before `create_device` succeeded).
    context: *const DeviceContext,
}

impl AddDevice {
    /// The device context, once `create_device` succeeded (the core calls the later steps only
    /// then, so `None` is unreachable).
    const fn context(&self) -> Option<&DeviceContext> {
        // SAFETY: `context` is NULL or points at the initialized context of the live device
        // created by `create_device`, which AddDevice is still building.
        unsafe { self.context.as_ref() }
    }

    fn trace_failure(step: &str, status: NtStatus) {
        tlg::write_event!(
            PROVIDER,
            "DeviceAddStepFailed",
            level(Error),
            keyword(KW_DEVICE),
            str8("Step", step),
            win_ntstatus("Status", &status)
        );
    }
}

impl AddDeviceIo for AddDevice {
    fn fdo_init_set_filter(&mut self) {
        // SAFETY: the framework's live WDFDEVICE_INIT of this EvtDriverDeviceAdd call. "We do not
        // own power control. In addition we do not own every I/O request." (Driver.c:56-59)
        unsafe { call_unsafe_wdf_function_binding!(WdfFdoInitSetFilter, self.device_init) };
    }

    fn set_pnp_power_event_callbacks(&mut self) {
        let mut callbacks = pnppower_event_callbacks_init();
        callbacks.EvtDevicePrepareHardware = Some(evt_device_prepare_hardware);
        callbacks.EvtDeviceD0Entry = Some(evt_device_d0_entry);
        callbacks.EvtDeviceD0Exit = Some(evt_device_d0_exit);
        callbacks.EvtDeviceSelfManagedIoInit = Some(evt_device_self_managed_io_init);
        callbacks.EvtDeviceSelfManagedIoRestart = Some(evt_device_self_managed_io_restart);
        // SAFETY: live WDFDEVICE_INIT; `callbacks` is initialized (WDF copies it).
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfDeviceInitSetPnpPowerEventCallbacks,
                self.device_init,
                &raw mut callbacks
            );
        }
    }

    fn create_device(&mut self) -> NtStatus {
        let mut attributes = object_attributes_init_context_type(&DEVICE_CONTEXT_TYPE_INFO);
        let mut device: WDFDEVICE = null_mut();
        // SAFETY: live WDFDEVICE_INIT (consumed on success); `attributes` names DEVICE_CONTEXT;
        // `device` is a valid out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfDeviceCreate,
                &raw mut self.device_init,
                &raw mut attributes,
                &raw mut device
            )
        };
        if !NT_SUCCESS(status) {
            Self::trace_failure("WdfDeviceCreate", status);
            return status;
        }
        // SAFETY: `device` was just created with DEVICE_CONTEXT_TYPE_INFO: its context exists, is
        // zero-filled and nothing references it yet. This is the only write that is not atomic.
        let context = unsafe {
            let context = device_get_context(device);
            context.write(DeviceContext::new(device));
            context
        };
        self.device = device;
        self.context = context;
        // The read tracker's lock (not in C), parented to the device. Its failure fails the device
        // creation step, like the C failures of this step.
        // SAFETY: PASSIVE_LEVEL; the context was just initialized and no callback can reach it yet;
        // the device outlives its child lock.
        let lock_status = unsafe { (*context).reads.create(device.cast()) };
        if !NT_SUCCESS(lock_status) {
            Self::trace_failure("WdfSpinLockCreate", lock_status);
            return lock_status;
        }
        status
    }

    fn store_wdm_device_object(&mut self) -> bool {
        let Some(ctx) = self.context() else {
            return false;
        };
        // SAFETY: live device.
        let object =
            unsafe { call_unsafe_wdf_function_binding!(WdfDeviceWdmGetDeviceObject, self.device) };
        ctx.set_wdm_device_object(object);
        if object.is_null() {
            tlg::write_event!(
                PROVIDER,
                "WdmDeviceObjectNull",
                level(Error),
                keyword(KW_DEVICE)
            );
        }
        !object.is_null()
    }

    fn create_device_interface(&mut self) -> NtStatus {
        // SAFETY: live device; the GUID is a 'static; no reference string (Device.c:52).
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfDeviceCreateDeviceInterface,
                self.device,
                &raw const GUID_DEVICEINTERFACE_AMTPTP_HID_FILTER,
                null()
            )
        };
        if !NT_SUCCESS(status) {
            Self::trace_failure("WdfDeviceCreateDeviceInterface", status);
        }
        status
    }

    fn create_read_buffer_lookaside(&mut self) -> NtStatus {
        let Some(ctx) = self.context() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        // C passes WDF_NO_OBJECT_ATTRIBUTES, which parents the list to the WDFDRIVER and leaks one
        // list per AddDevice (HF-B15). P2: parent it to the device.
        let mut attributes = object_attributes_init();
        attributes.ParentObject = self.device.cast();
        let mut lookaside: WDFLOOKASIDE = null_mut();
        // SAFETY: `attributes` initialized (parent = live device); 1024-byte NonPagedPoolNx
        // buffers tagged 'LTPA'; no memory attributes, as in C; `lookaside` is a valid out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfLookasideListCreate,
                &raw mut attributes,
                REPORT_BUFFER_SIZE,
                _POOL_TYPE::NonPagedPoolNx,
                WDF_NO_OBJECT_ATTRIBUTES,
                READ_BUFFER_POOL_TAG,
                &raw mut lookaside
            )
        };
        if !NT_SUCCESS(status) {
            Self::trace_failure("WdfLookasideListCreate", status);
            return status;
        }
        ctx.set_hid_read_buffer_lookaside(lookaside);
        status
    }

    fn create_recovery_timer(&mut self) -> NtStatus {
        let Some(ctx) = self.context() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        let mut config = timer_config_init(Some(evt_recovery_timer));
        config.Period = RECOVERY_TIMER_PERIOD;
        // C: TRUE. With the inherited sync scope None it has no effect, but with the device's
        // inherited Dispatch execution level it may make a passive timer fail to create
        // (STATUS_WDF_INCOMPATIBLE_EXECUTION_LEVEL). HF-B16.
        config.AutomaticSerialization = boolean(RECOVERY_TIMER_AUTOMATIC_SERIALIZATION);
        let mut attributes = object_attributes_init();
        attributes.ParentObject = self.device.cast();
        // A passive-level timer: its callback runs the synchronous mode switch.
        attributes.ExecutionLevel = _WDF_EXECUTION_LEVEL::WdfExecutionLevelPassive;
        let mut timer: WDFTIMER = null_mut();
        // SAFETY: `config` and `attributes` initialized (one-shot, parent = live device);
        // `timer` is a valid out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfTimerCreate,
                &raw mut config,
                &raw mut attributes,
                &raw mut timer
            )
        };
        if !NT_SUCCESS(status) {
            Self::trace_failure("WdfTimerCreate", status);
            return status;
        }
        ctx.set_recovery_timer(timer);
        status
    }

    fn create_recovery_work_item(&mut self) -> NtStatus {
        let Some(ctx) = self.context() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        let mut config = workitem_config_init(Some(evt_recovery_work_item));
        // C: TRUE (INIT default). No effect under sync scope None; FALSE avoids the execution-level
        // mismatch with the Dispatch-level parent (HF-B16).
        config.AutomaticSerialization = boolean(RECOVERY_WORK_ITEM_AUTOMATIC_SERIALIZATION);
        let mut attributes = object_attributes_init();
        attributes.ParentObject = self.device.cast();
        let mut work_item: WDFWORKITEM = null_mut();
        // SAFETY: `config` and `attributes` initialized (parent = live device); valid out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfWorkItemCreate,
                &raw mut config,
                &raw mut attributes,
                &raw mut work_item
            )
        };
        if !NT_SUCCESS(status) {
            Self::trace_failure("WdfWorkItemCreate", status);
            return status;
        }
        ctx.set_recovery_work_item(work_item);
        status
    }

    fn create_default_queue(&mut self) -> NtStatus {
        // Default parallel queue; PowerManaged = WdfUseDefault, which is "not power-managed" on a
        // filter device (Queue.c:28-41).
        let mut config = wdf_hid::default_queue_config(
            Some(queue::evt_io_internal_device_control),
            Some(queue::evt_io_stop),
        );
        let mut attributes = object_attributes_init_context_type(&QUEUE_CONTEXT_TYPE_INFO);
        let mut queue: WDFQUEUE = null_mut();
        // SAFETY: live device; `config` and `attributes` initialized; valid out pointer.
        let status = unsafe {
            wdf_hid::create_queue(
                self.device,
                &mut config,
                &raw mut attributes,
                &raw mut queue,
            )
        };
        if !NT_SUCCESS(status) {
            Self::trace_failure("WdfIoQueueCreate(default)", status);
            return status;
        }
        // SAFETY: `queue` was just created with QUEUE_CONTEXT_TYPE_INFO; its zero-filled context is
        // not referenced yet (the device is not started, so no request can be dispatched).
        unsafe {
            queue_get_context(queue).write(QueueContext {
                device: WdfHandle::new(self.device),
            });
        };
        status
    }

    fn create_read_queue(&mut self) -> NtStatus {
        let Some(ctx) = self.context() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        let mut config = wdf_hid::read_queue_config();
        let mut queue: WDFQUEUE = null_mut();
        // SAFETY: live device; `config` initialized; default attributes (parent = device); valid
        // out pointer.
        let status = unsafe {
            wdf_hid::create_queue(
                self.device,
                &mut config,
                WDF_NO_OBJECT_ATTRIBUTES,
                &raw mut queue,
            )
        };
        if !NT_SUCCESS(status) {
            Self::trace_failure("WdfIoQueueCreate(read)", status);
            return status;
        }
        ctx.set_hid_read_queue(queue);
        status
    }
}

/// `PtpFilterPrepareHardware` (Device.c:104-130): clears `DeviceConfigured`; the resource lists are
/// ignored. VID/PID/version are kept, so the restart after a PnP stop sends the mode switch again (P3
/// fix of HF-B30: C zeroed them).
unsafe extern "C" fn evt_device_prepare_hardware(
    device: WDFDEVICE,
    _resources_raw: WDFCMRESLIST,
    _resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    // SAFETY: PnP callback of a live device of this driver.
    let ctx = unsafe { device_context(device) };
    let status = prepare_hardware(&ctx.state);
    tlg::write_event!(
        PROVIDER,
        "PrepareHardware",
        level(Informational),
        keyword(KW_DEVICE)
    );
    status
}

/// `PtpFilterDeviceD0Entry` (Device.c:132-147): marks the device as in D0 (C did nothing), so the
/// recovery timer works on it again.
unsafe extern "C" fn evt_device_d0_entry(
    device: WDFDEVICE,
    previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    tlg::write_event!(
        PROVIDER,
        "D0Entry",
        level(Informational),
        keyword(KW_DEVICE),
        i32("PreviousState", &previous_state)
    );
    // SAFETY: power callback of a live device of this driver.
    let ctx = unsafe { device_context(device) };
    d0_entry(&ctx.state)
}

/// `PtpFilterDeviceD0Exit` (Device.c:149-182): clears `DeviceConfigured`, leaves D0 (a recovery timer
/// or work item that runs afterwards does nothing), stops the recovery timer, cancels the lower reads
/// in flight (their late completions are stale and dropped: P3 fix of HF-B14) and completes every
/// parked upper read with `STATUS_CANCELLED`.
unsafe extern "C" fn evt_device_d0_exit(
    device: WDFDEVICE,
    target_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    // SAFETY: power callback of a live device of this driver.
    let ctx = unsafe { device_context(device) };
    let status = d0_exit(&mut Filter::new(ctx), &ctx.state);
    tlg::write_event!(
        PROVIDER,
        "D0Exit",
        level(Informational),
        keyword(KW_DEVICE),
        i32("TargetState", &target_state)
    );
    status
}

/// Traces the identification state after a configuration attempt.
fn trace_configuration(event_is_init: bool, ctx: &DeviceContext, status: NtStatus) {
    let s = ctx.state.snapshot();
    if event_is_init {
        tlg::write_event!(
            PROVIDER,
            "SelfManagedIoInit",
            level(Informational),
            keyword(KW_DEVICE),
            win_ntstatus("Status", &status),
            u16_hex("VendorID", &s.vendor_id),
            u16_hex("ProductID", &s.product_id),
            u16_hex("VersionNumber", &s.version_number),
            bool8("DeviceConfigured", &s.device_configured),
            bool8("DetourCompleted", &s.detour_completed)
        );
    } else {
        tlg::write_event!(
            PROVIDER,
            "SelfManagedIoRestart",
            level(Informational),
            keyword(KW_DEVICE),
            win_ntstatus("Status", &status),
            u16_hex("VendorID", &s.vendor_id),
            u16_hex("ProductID", &s.product_id),
            bool8("DeviceConfigured", &s.device_configured)
        );
    }
}

/// `PtpFilterSelfManagedIoInit` (Device.c:184-245), once at the first start (`PASSIVE_LEVEL)`:
/// detour, synchronous attributes query, mode switch (failure: 2 s retry timer, SUCCESS), restart the
/// reporting clock, set `DeviceConfigured` and issue the lower reads parked upper reads are missing.
unsafe extern "C" fn evt_device_self_managed_io_init(device: WDFDEVICE) -> NTSTATUS {
    // SAFETY: PnP callback of a live device of this driver.
    let ctx = unsafe { device_context(device) };
    let status = self_managed_io_init(&mut Filter::new(ctx), &ctx.state);
    trace_configuration(true, ctx, status);
    status
}

/// `PtpFilterSelfManagedIoRestart` (Device.c:247-284), on every later return to D0 (`PASSIVE_LEVEL`).
/// HF-B11/B12 preserved (see the core); a successful mode switch issues the lower reads parked upper
/// reads are missing.
unsafe extern "C" fn evt_device_self_managed_io_restart(device: WDFDEVICE) -> NTSTATUS {
    // SAFETY: power callback of a live device of this driver.
    let ctx = unsafe { device_context(device) };
    let status = self_managed_io_restart(&mut Filter::new(ctx), &ctx.state);
    trace_configuration(false, ctx, status);
    status
}

/// `PtpFilterRecoveryTimerCallback` (Device.c:424-443): passive-level one-shot timer (its
/// `ExecutionLevel` is `WdfExecutionLevelPassive`), so the synchronous mode switch it triggers runs
/// at `PASSIVE_LEVEL` as in C. It does nothing unless the device is in D0 (see the core).
unsafe extern "C" fn evt_recovery_timer(timer: WDFTIMER) {
    // SAFETY: live timer of this driver, parented to the device.
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfTimerGetParentObject, timer) };
    // SAFETY: the timer's parent is our WDFDEVICE, alive while its child timer runs.
    let ctx = unsafe { device_context(device.cast()) };
    tlg::write_event!(
        PROVIDER,
        "RecoveryTimerFired",
        level(Informational),
        keyword(KW_DEVICE)
    );
    recovery_timer_callback(&mut Filter::new(ctx), &ctx.state);
}

/// `PtpFilterWorkItemCallback` (Input.c:30-37), `PASSIVE_LEVEL`: while the device is configured, issues
/// the lower reads parked upper reads are missing (C issued one read without checking).
unsafe extern "C" fn evt_recovery_work_item(work_item: WDFWORKITEM) {
    // SAFETY: live work item of this driver, parented to the device.
    let device =
        unsafe { call_unsafe_wdf_function_binding!(WdfWorkItemGetParentObject, work_item) };
    // SAFETY: the work item's parent is our WDFDEVICE, alive while its child work item runs.
    let ctx = unsafe { device_context(device.cast()) };
    work_item_callback(&mut Filter::new(ctx), &ctx.state);
}

//! Device creation and the PnP/power callbacks (Driver.c:98-143, Device.c).
//!
//! - `EvtDriverDeviceAdd`: `WdfFdoInitSetFilter`, then `AmtPtpDeviceUsbKmCreateDevice`. The C also calls
//!   `WdfPdoInitAllowForwardingRequestToParent`, a PDO-only API that KMDF rejects on a filter's
//!   `DeviceInit` (error trace + verifier break, no other effect); it is not called here (spec B12, P2).
//! - `CreateDevice`: PnP/power callbacks `PrepareHardware`, `D0Entry`, `D0Exit` only (nothing else is set: no
//!   power policy, file object, I/O type or request attributes, so a filter inherits them from the lower
//!   device object); `WdfDeviceCreate` with the `DEVICE_CONTEXT`; the context's initial state; the device
//!   interface; the queues. The first failure is returned (WDF deletes the device).
//! - `PrepareHardware` / `D0Entry` / `D0Exit`: the core's sequencing over [`UsbIo`].
//!
//! Not present, as in C: `ReleaseHardware`, `SelfManagedIo`*, `SurpriseRemoval`, QueryRemove/Stop, power policy
//! callbacks, idle/wake settings.

use amtptp_usbkm_core::lifecycle::{self, DEVICE_INTERFACE_GUID};
use tracelogging as tlg;
use wdf_support::init::{object_attributes_init_context_type, pnppower_event_callbacks_init};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE};
use wdk_sys::{
    GUID, NTSTATUS, PWDFDEVICE_INIT, WDF_POWER_DEVICE_STATE, WDFCMRESLIST, WDFDEVICE, WDFDRIVER,
    call_unsafe_wdf_function_binding,
};

use crate::PROVIDER;
use crate::context::{DEVICE_CONTEXT_TYPE_INFO, DeviceContext, device_get_context};
use crate::queue::queue_initialize;
use crate::trace::{KW_DRIVER, power_state_name};
use crate::usb::UsbIo;

/// `GUID_DEVINTERFACE_AmtPtpDeviceUsbKm` {4aa332cc-5777-4afd-aa4e-95387330612a} (Public.h:22-24).
static GUID_DEVINTERFACE_AMTPTPDEVICEUSBKM: GUID = GUID {
    Data1: DEVICE_INTERFACE_GUID.data1,
    Data2: DEVICE_INTERFACE_GUID.data2,
    Data3: DEVICE_INTERFACE_GUID.data3,
    Data4: DEVICE_INTERFACE_GUID.data4,
};

/// `AmtPtpDeviceUsbKmEvtDeviceAdd` (Driver.c:98-143). `PASSIVE_LEVEL`, `PAGE` as in C.
#[unsafe(link_section = "PAGE")]
pub(crate) unsafe extern "C" fn evt_device_add(
    _driver: WDFDRIVER,
    device_init: PWDFDEVICE_INIT,
) -> NTSTATUS {
    wdk::paged_code!();
    tlg::write_event!(
        PROVIDER,
        "EvtDeviceAdd",
        level(Informational),
        keyword(KW_DRIVER)
    );

    // SAFETY: `device_init` is the framework-provided WDFDEVICE_INIT of this callback, not yet consumed.
    unsafe { call_unsafe_wdf_function_binding!(WdfFdoInitSetFilter, device_init) };
    // B12 (P2): no WdfPdoInitAllowForwardingRequestToParent (PDO-only; rejected on this DeviceInit).

    // SAFETY: as above; create_device consumes `device_init` through WdfDeviceCreate.
    let status = unsafe { create_device(device_init) };
    if !NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "EvtDeviceAddFailed",
            level(Error),
            keyword(KW_DRIVER),
            win_ntstatus("Status", &status)
        );
    }
    status
}

/// `AmtPtpDeviceUsbKmCreateDevice` (Device.c:50-128). `PASSIVE_LEVEL`, `PAGE` as in C.
///
/// # Safety
/// `device_init` is the unconsumed `WDFDEVICE_INIT` of the current `EvtDriverDeviceAdd`.
#[unsafe(link_section = "PAGE")]
unsafe fn create_device(mut device_init: PWDFDEVICE_INIT) -> NTSTATUS {
    wdk::paged_code!();

    let mut callbacks = pnppower_event_callbacks_init();
    callbacks.EvtDevicePrepareHardware = Some(evt_device_prepare_hardware);
    callbacks.EvtDeviceD0Entry = Some(evt_device_d0_entry);
    callbacks.EvtDeviceD0Exit = Some(evt_device_d0_exit);
    // SAFETY: unconsumed `device_init` (caller contract); `callbacks` is initialized (Size set).
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetPnpPowerEventCallbacks,
            device_init,
            &raw mut callbacks
        );
    }

    let mut attributes = object_attributes_init_context_type(&DEVICE_CONTEXT_TYPE_INFO);
    let mut device: WDFDEVICE = core::ptr::null_mut();
    // SAFETY: WdfDeviceCreate consumes `device_init` (and sets it to NULL); `attributes` carries the
    // DEVICE_CONTEXT type info; `device` is a valid out-pointer.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreate,
            &raw mut device_init,
            &raw mut attributes,
            &raw mut device
        )
    };
    if !NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "WdfDeviceCreateFailed",
            level(Error),
            keyword(KW_DRIVER),
            win_ntstatus("Status", &status)
        );
        return status;
    }

    // SAFETY: `device` was just created with DEVICE_CONTEXT_TYPE_INFO, so its (zero-filled) context
    // exists; no callback can see it yet (PnP callbacks run after DeviceAdd returns, queue callbacks after
    // the queues below exist), so this single write initializes it before any shared reference is formed.
    let context = unsafe {
        let context = device_get_context(device);
        if context.is_null() {
            return STATUS_INVALID_DEVICE_STATE;
        }
        context.write(DeviceContext::new());
        &*context
    };
    // PtpReportButton = PtpReportTouch = TRUE (Device.c:106-107).
    context.state.on_device_create();

    // Not in C: the lock that serializes the mode switches (parent: the device).
    // SAFETY: PASSIVE_LEVEL; created once, before any callback can use the context; the device
    // outlives its child lock.
    let status = unsafe { context.mode_switch_lock.create(device.cast()) };
    if !NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "WdfWaitLockCreateFailed",
            level(Error),
            keyword(KW_DRIVER),
            win_ntstatus("Status", &status)
        );
        return status;
    }

    // SAFETY: PASSIVE_LEVEL; created once, before any callback can use the context; the device
    // outlives its child lock.
    let status = unsafe { context.contacts.create(device.cast()) };
    if !NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "WdfSpinLockCreateFailed",
            level(Error),
            keyword(KW_DRIVER),
            win_ntstatus("Status", &status)
        );
        return status;
    }

    // SAFETY: live device; the GUID is a 'static; NULL reference string as in C.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreateDeviceInterface,
            device,
            &raw const GUID_DEVINTERFACE_AMTPTPDEVICEUSBKM,
            core::ptr::null()
        )
    };
    if !NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "WdfDeviceCreateDeviceInterfaceFailed",
            level(Error),
            keyword(KW_DRIVER),
            win_ntstatus("Status", &status)
        );
        return status;
    }

    // SAFETY: live device whose context was initialized above; PASSIVE_LEVEL.
    unsafe { queue_initialize(device, context) }
}

/// `AmtPtpDeviceUsbKmEvtDevicePrepareHardware` (Device.c:130-272). `PASSIVE_LEVEL`, `PAGE` as in C. The
/// resource lists are unused (USB device).
#[unsafe(link_section = "PAGE")]
unsafe extern "C" fn evt_device_prepare_hardware(
    device: WDFDEVICE,
    _resources_raw: WDFCMRESLIST,
    _resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    wdk::paged_code!();
    // SAFETY: WDF passes this driver's live device.
    let Some(context) = (unsafe { DeviceContext::of(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: `device` owns `context`; PrepareHardware runs at PASSIVE_LEVEL.
    let mut io = unsafe { UsbIo::new(device, context) };
    let status = lifecycle::prepare_hardware(&context.state, &mut io);
    // SAFETY: live device context; spinlock created in `create_device`; no WDF calls inside.
    unsafe { context.contacts.with(amtptp_usbkm_core::input::ContactTracker::reset) };
    let config = context.state.config();
    let pipe = context.state.interrupt_pipe().map_or(-1, i32::from);
    tlg::write_event!(
        PROVIDER,
        "EvtDevicePrepareHardware",
        level(Informational),
        keyword(KW_DRIVER),
        win_ntstatus("Status", &status),
        i32_hex("ConfigIdentification", &i32::from(config.identification)),
        i32("InterruptPipe", &pipe)
    );
    status
}

/// `AmtPtpEvtDeviceD0Entry` (Device.c:275-359). `PASSIVE_LEVEL`, non-paged as in C.
///
/// Wellspring mode on (failure ignored, B11), `LastReportTime` = the QPC out-parameter (B1), start the
/// interrupt pipe target (its failure fails `D0Entry`).
unsafe extern "C" fn evt_device_d0_entry(
    device: WDFDEVICE,
    previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    tlg::write_event!(
        PROVIDER,
        "EvtDeviceD0Entry",
        level(Informational),
        keyword(KW_DRIVER),
        str8("PreviousState", power_state_name(previous_state))
    );
    // SAFETY: WDF passes this driver's live device.
    let Some(context) = (unsafe { DeviceContext::of(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: `device` owns `context`; D0Entry runs at PASSIVE_LEVEL.
    let mut io = unsafe { UsbIo::new(device, context) };
    // SAFETY: live device context; spinlock created in `create_device`; no WDF calls inside.
    unsafe { context.contacts.with(amtptp_usbkm_core::input::ContactTracker::reset) };
    let status = lifecycle::d0_entry(&context.state, &mut io);
    let wellspring = context.state.wellspring_mode_on();
    if NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "EvtDeviceD0EntryDone",
            level(Informational),
            keyword(KW_DRIVER),
            bool8("WellspringModeOn", &wellspring)
        );
    } else {
        tlg::write_event!(
            PROVIDER,
            "EvtDeviceD0EntryFailed",
            level(Error),
            keyword(KW_DRIVER),
            win_ntstatus("Status", &status),
            bool8("WellspringModeOn", &wellspring)
        );
    }
    status
}

/// `AmtPtpEvtDeviceD0Exit` (Device.c:361-416). `PASSIVE_LEVEL`, non-paged as in C.
///
/// Stop the interrupt pipe target (cancelling the reader's reads) and switch wellspring mode off, best
/// effort: a failed transfer is traced, and the callback returns `STATUS_SUCCESS` (P3 fix of B10: on a
/// surprise removal the transfer always fails, and a failed `D0Exit` is a failed power-down).
unsafe extern "C" fn evt_device_d0_exit(
    device: WDFDEVICE,
    target_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    tlg::write_event!(
        PROVIDER,
        "EvtDeviceD0Exit",
        level(Informational),
        keyword(KW_DRIVER),
        str8("TargetState", power_state_name(target_state))
    );
    // SAFETY: WDF passes this driver's live device.
    let Some(context) = (unsafe { DeviceContext::of(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: `device` owns `context`; D0Exit runs at PASSIVE_LEVEL.
    let mut io = unsafe { UsbIo::new(device, context) };
    let outcome = lifecycle::d0_exit(&context.state, &mut io);
    // SAFETY: live device context; spinlock created in `create_device`; no WDF calls inside.
    unsafe { context.contacts.with(amtptp_usbkm_core::input::ContactTracker::reset) };
    let mode_switch_status = outcome.mode_switch_status;
    if !NT_SUCCESS(mode_switch_status) {
        tlg::write_event!(
            PROVIDER,
            "CancelWellspringModeFailed",
            level(Warning),
            keyword(KW_DRIVER),
            str8("TargetState", power_state_name(target_state)),
            win_ntstatus("Status", &mode_switch_status)
        );
    }
    outcome.status
}

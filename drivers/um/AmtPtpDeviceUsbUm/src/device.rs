//! Device creation and the PnP/power callbacks (C: Device.c, `AmtPtpDeviceEvtDeviceAdd` in Driver.c).
//!
//! | C | Rust |
//! |---|---|
//! | `DEVICE_CONTEXT` / `DeviceGetContext` | [`DeviceContext`] / [`device_get_context`] (WDF name `"DEVICE_CONTEXT"`) |
//! | `AmtPtpDeviceEvtDeviceAdd` | [`evt_device_add`] |
//! | `AmtPtpCreateDevice` | [`create_device`] |
//! | `AmtPtpEvtDevicePrepareHardware` | [`evt_device_prepare_hardware`] → `pnp::prepare_hardware` |
//! | `AmtPtpEvtDeviceD0Entry` | [`evt_device_d0_entry`] → `pnp::d0_entry` |
//! | `AmtPtpEvtDeviceD0Exit` | [`evt_device_d0_exit`] → `pnp::d0_exit` |
//!
//! There is no `EvtDeviceReleaseHardware`, no self-managed I/O, no surprise-removal callback, no
//! power-policy settings (the driver is a filter, so not the power-policy owner), no file-object
//! configuration and no I/O type setting, exactly as in C.
//!
//! UMDF runs every callback at `PASSIVE_LEVEL` in the host process; nothing here is pageable.

use core::ptr::{self, null_mut};
use core::sync::atomic::{AtomicPtr, Ordering};

use amtptp_usbum_core::{DeviceState, pnp};
use wdf_support::declare_context_type;
use wdf_support::init::{
    device_pnp_capabilities_init, object_attributes_init_context_type,
    pnppower_event_callbacks_init,
};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE};
use wdk_sys::{
    _WDF_TRI_STATE, GUID, NTSTATUS, PWDFDEVICE_INIT, WDF_POWER_DEVICE_STATE, WDFCMRESLIST,
    WDFDEVICE, WDFDRIVER, WDFQUEUE, WDFQUEUE__, WDFUSBDEVICE, WDFUSBDEVICE__, WDFUSBINTERFACE,
    WDFUSBINTERFACE__, WDFUSBPIPE, WDFUSBPIPE__, call_unsafe_wdf_function_binding,
};

use crate::queue;
use crate::trace;
use crate::usb::{DeviceIo, UsbSetup};

/// The device context (C `DEVICE_CONTEXT`, Device.h:6-29), attached to the WDFDEVICE.
///
/// The WDF handles are `AtomicPtr`s and every other field lives in the core's [`DeviceState`] (atomics):
/// the C driver has no locking, and the parallel default queue, the continuous-reader completions and the
/// PnP/power callbacks all touch the context concurrently (UM-B22). Handles are published with `Release`
/// and read with `Acquire`.
///
/// The C fields `UsbDeviceTraits`, `DeviceDescriptor`, `DeviceInfo`, the three qualification levels, the
/// three flags and `PerfCounter` are in [`DeviceState`]. `reader_pipe` is new (UM-B16 fix).
pub(crate) struct DeviceContext {
    /// `UsbDevice`: created once by the first `PrepareHardware`, reused on restart.
    usb_device: AtomicPtr<WDFUSBDEVICE__>,
    /// `InterruptPipe`: the first interrupt pipe of interface 0. Keeps its value across a restart, as in C.
    interrupt_pipe: AtomicPtr<WDFUSBPIPE__>,
    /// `UsbInterface`: interface 0.
    usb_interface: AtomicPtr<WDFUSBINTERFACE__>,
    /// `InputQueue`: the manual queue of pending `IOCTL_HID_READ_REPORT` requests.
    input_queue: AtomicPtr<WDFQUEUE__>,
    /// The pipe the continuous reader was configured on (UM-B16: configure once per pipe).
    reader_pipe: AtomicPtr<WDFUSBPIPE__>,
    /// Everything else of `DEVICE_CONTEXT`.
    pub(crate) state: DeviceState,
}

// Concurrent callbacks (parallel queue, reader completions, PnP/power) share the context through
// `&DeviceContext`, so it must be `Sync` (all fields are atomics).
const _: fn() = || {
    const fn assert_sync<T: Sync>() {}
    assert_sync::<DeviceContext>();
};

// C: WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, DeviceGetContext). Same ContextName, so
// `!wdfkd.wdfobject` shows "DEVICE_CONTEXT" as for the C driver.
declare_context_type!(
    pub(crate) DeviceContext,
    DEVICE_CONTEXT_TYPE_INFO,
    device_get_context,
    name = "DEVICE_CONTEXT"
);

impl DeviceContext {
    /// The freshly created context: all handles NULL, the core state zero-filled as WDF does in C
    /// (`DeviceState::new`, whose "no config row" marker is not the all-zero bit pattern, so the context
    /// must be written explicitly after `WdfDeviceCreate`).
    const fn new() -> Self {
        Self {
            usb_device: AtomicPtr::new(null_mut()),
            interrupt_pipe: AtomicPtr::new(null_mut()),
            usb_interface: AtomicPtr::new(null_mut()),
            input_queue: AtomicPtr::new(null_mut()),
            reader_pipe: AtomicPtr::new(null_mut()),
            state: DeviceState::new(),
        }
    }

    /// `UsbDevice` (NULL before the first successful `PrepareHardware`).
    pub(crate) fn usb_device(&self) -> WDFUSBDEVICE {
        self.usb_device.load(Ordering::Acquire)
    }

    pub(crate) fn set_usb_device(&self, usb_device: WDFUSBDEVICE) {
        self.usb_device.store(usb_device, Ordering::Release);
    }

    /// `InterruptPipe`.
    pub(crate) fn interrupt_pipe(&self) -> WDFUSBPIPE {
        self.interrupt_pipe.load(Ordering::Acquire)
    }

    pub(crate) fn set_interrupt_pipe(&self, pipe: WDFUSBPIPE) {
        self.interrupt_pipe.store(pipe, Ordering::Release);
    }

    /// `UsbInterface`.
    pub(crate) fn usb_interface(&self) -> WDFUSBINTERFACE {
        self.usb_interface.load(Ordering::Acquire)
    }

    pub(crate) fn set_usb_interface(&self, usb_interface: WDFUSBINTERFACE) {
        self.usb_interface.store(usb_interface, Ordering::Release);
    }

    /// `InputQueue`.
    pub(crate) fn input_queue(&self) -> WDFQUEUE {
        self.input_queue.load(Ordering::Acquire)
    }

    pub(crate) fn set_input_queue(&self, queue: WDFQUEUE) {
        self.input_queue.store(queue, Ordering::Release);
    }

    /// The pipe the continuous reader is configured on (NULL before the first configuration).
    pub(crate) fn reader_pipe(&self) -> WDFUSBPIPE {
        self.reader_pipe.load(Ordering::Acquire)
    }

    pub(crate) fn set_reader_pipe(&self, pipe: WDFUSBPIPE) {
        self.reader_pipe.store(pipe, Ordering::Release);
    }
}

/// Returns the [`DeviceContext`] of `device`, or `None` if it has none (never the case for a device
/// created by [`create_device`]).
///
/// # Safety
/// `device` must be a live WDFDEVICE created by [`create_device`] (whose context was written with
/// [`DeviceContext::new`]): the callback's WDFDEVICE argument or `WdfIoQueueGetDevice` of one of its
/// queues. (A child object's handle would not do: WDF returns NULL for it, not the parent's context.)
/// The returned reference must not outlive the device; every caller is a WDF callback of the device or
/// of one of its queues, which the framework only calls while the device exists.
pub(crate) unsafe fn context<'a>(device: WDFDEVICE) -> Option<&'a DeviceContext> {
    // SAFETY: `device` is a live WDF object handle (caller contract).
    let context = unsafe { device_get_context(device) };
    // SAFETY: a non-null result points at the device's context, which create_device initialized with
    // `ptr.write(DeviceContext::new())` before any other callback of the device can run; it is only
    // accessed through shared references (all fields are atomics) and lives as long as the device.
    unsafe { context.cast_const().as_ref() }
}

/// `GUID_DEVINTERFACE_AmtPtpDevice` `{4a5064e5-7d39-41d1-a0e4-81097edce967}` (Trace.h:7-8).
static GUID_DEVINTERFACE_AMTPTP_DEVICE: GUID = {
    let (data1, data2, data3, data4) = pnp::GUID_DEVINTERFACE_AMTPTP_DEVICE;
    GUID {
        Data1: data1,
        Data2: data2,
        Data3: data3,
        Data4: data4,
    }
};

/// `AmtPtpDeviceEvtDeviceAdd` (Driver.c:108-141): `WdfFdoInitSetFilter`, then [`create_device`], whose
/// status is returned.
///
/// # Safety
/// Called by the framework with a valid `WDFDEVICE_INIT` it owns for the duration of the callback.
pub(crate) unsafe extern "C" fn evt_device_add(
    _driver: WDFDRIVER,
    device_init: PWDFDEVICE_INIT,
) -> NTSTATUS {
    // The HID minidriver sits below mshidumdf: in WDF terms a filter (default I/O passes down, the
    // queues are not power-managed, the driver is not the power-policy owner).
    // SAFETY: `device_init` is the framework's WDFDEVICE_INIT for this callback, not yet consumed.
    unsafe { call_unsafe_wdf_function_binding!(WdfFdoInitSetFilter, device_init) };

    // SAFETY: same `device_init`, still not consumed.
    unsafe { create_device(device_init) }
}

/// `AmtPtpCreateDevice` (Device.c:24-118).
///
/// 1. PnP/power callbacks `PrepareHardware`, `D0Entry`, `D0Exit` (nothing else).
/// 2. `WdfDeviceCreate` with the `DEVICE_CONTEXT` context type (no cleanup/destroy callbacks).
/// 3. Initialize the context.
/// 4. `SurpriseRemovalOK = WdfTrue` (all other capabilities `WdfUseDefault`).
/// 5. `WdfDeviceCreateDeviceInterface(GUID_DEVINTERFACE_AmtPtpDevice, NULL)`.
/// 6. Only if 5 succeeded: the queues ([`queue::initialize`]).
///
/// # Safety
/// `device_init` must be the framework's `WDFDEVICE_INIT` of the current `EvtDriverDeviceAdd`, not yet
/// consumed by `WdfDeviceCreate`.
unsafe fn create_device(mut device_init: PWDFDEVICE_INIT) -> NTSTATUS {
    let mut pnp_power_callbacks = pnppower_event_callbacks_init();
    pnp_power_callbacks.EvtDevicePrepareHardware = Some(evt_device_prepare_hardware);
    pnp_power_callbacks.EvtDeviceD0Entry = Some(evt_device_d0_entry);
    pnp_power_callbacks.EvtDeviceD0Exit = Some(evt_device_d0_exit);
    // SAFETY: valid, unconsumed WDFDEVICE_INIT (caller contract); the callbacks structure is
    // initialized by WDF_PNPPOWER_EVENT_CALLBACKS_INIT and is copied by the framework.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetPnpPowerEventCallbacks,
            device_init,
            &raw mut pnp_power_callbacks
        );
    }

    let mut device_attributes = object_attributes_init_context_type(&DEVICE_CONTEXT_TYPE_INFO);
    let mut device: WDFDEVICE = null_mut();
    // SAFETY: `device_init` is valid and unconsumed (WdfDeviceCreate consumes it and NULLs the local);
    // the attributes are initialized with the 'static context type info; `device` is an out pointer.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreate,
            &raw mut device_init,
            &raw mut device_attributes,
            &raw mut device
        )
    };
    if !NT_SUCCESS(status) {
        trace::pnp_failure("AmtPtpCreateDevice", "WdfDeviceCreate", status);
        return status;
    }

    // SAFETY: `device` was just created with DEVICE_CONTEXT_TYPE_INFO.
    let context_ptr = unsafe { device_get_context(device) };
    if context_ptr.is_null() {
        // Unreachable: the device was created with this context type.
        trace::pnp_failure(
            "AmtPtpCreateDevice",
            "DeviceGetContext",
            STATUS_INVALID_DEVICE_STATE,
        );
        return STATUS_INVALID_DEVICE_STATE;
    }
    // SAFETY: the context memory belongs to the new device, is zero-filled by WDF, suitably aligned
    // (checked by declare_context_type!) and not yet visible to any other callback (none of the
    // device's callbacks can run before EvtDriverDeviceAdd returns).
    unsafe { context_ptr.write(DeviceContext::new()) };
    // SAFETY: just initialized; the device lives at least until this callback returns.
    let Some(context) = (unsafe { context_ptr.cast_const().as_ref() }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };

    let mut pnp_caps = device_pnp_capabilities_init();
    pnp_caps.SurpriseRemovalOK = _WDF_TRI_STATE::WdfTrue;
    // SAFETY: valid device; initialized capabilities structure (copied by the framework).
    unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceSetPnpCapabilities, device, &raw mut pnp_caps);
    };

    // SAFETY: valid device; the GUID is a 'static; ReferenceString NULL (as in C).
    let mut status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreateDeviceInterface,
            device,
            &raw const GUID_DEVINTERFACE_AMTPTP_DEVICE,
            ptr::null()
        )
    };
    if NT_SUCCESS(status) {
        // SAFETY: `device` is the device just created; `context` is its context.
        status = unsafe { queue::initialize(device, context) };
    } else {
        trace::pnp_failure(
            "AmtPtpCreateDevice",
            "WdfDeviceCreateDeviceInterface",
            status,
        );
    }
    status
}

/// `AmtPtpEvtDevicePrepareHardware` (Device.c:120-253) → `pnp::prepare_hardware`.
///
/// The resource lists are unused (USB device). An unknown PID writes the C `DeviceIdentificationEvent`
/// (Subtype `DeviceNotFoundInRegistry`).
///
/// # Safety
/// Framework callback for a device created by [`create_device`].
unsafe extern "C" fn evt_device_prepare_hardware(
    device: WDFDEVICE,
    _resource_list: WDFCMRESLIST,
    _resource_list_translated: WDFCMRESLIST,
) -> NTSTATUS {
    // SAFETY: framework callback of our device.
    let Some(context) = (unsafe { context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    let mut usb = UsbSetup::new(device, context);
    let outcome = pnp::prepare_hardware(&context.state, &mut usb);
    if let Some(event) = outcome.event {
        trace::device_identification(event);
    }
    if NT_SUCCESS(outcome.status) {
        let descriptor = context.state.device_descriptor();
        trace::prepare_hardware_done(
            descriptor.id_vendor,
            descriptor.id_product,
            context.state.usb_device_traits(),
            usb.selected_endpoint_address(),
            // Handle value for the trace only.
            context.interrupt_pipe() as usize,
            outcome.configured_reader,
        );
    } else {
        trace::pnp_failure(
            "AmtPtpEvtDevicePrepareHardware",
            usb.failed_step(),
            outcome.status,
        );
    }
    outcome.status
}

/// `AmtPtpEvtDeviceD0Entry` (Device.c:498-585) → `pnp::d0_entry`: enable Wellspring mode (C only did
/// if `IsButtonReportOn || IsWellspringModeOn`, UM-B10; failure only traced), snapshot QPC into
/// `PerfCounter`, start the interrupt pipe's I/O target (its status is returned).
///
/// # Safety
/// Framework callback for a device created by [`create_device`].
unsafe extern "C" fn evt_device_d0_entry(
    device: WDFDEVICE,
    previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    // SAFETY: framework callback of our device.
    let Some(context) = (unsafe { context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    let before = context.state.snapshot();
    let mut io = DeviceIo::new(context);
    let outcome = pnp::d0_entry(
        &context.state,
        &mut io,
        wdf_support::time::query_performance_frequency,
    );
    if !NT_SUCCESS(outcome.mode_switch_status) {
        trace::mode_switch_failed("AmtPtpEvtDeviceD0Entry", true, outcome.mode_switch_status);
    }
    trace::state_changed("AmtPtpEvtDeviceD0Entry", &before, &context.state.snapshot());
    if !NT_SUCCESS(outcome.status) {
        trace::pnp_failure("AmtPtpEvtDeviceD0Entry", "WdfIoTargetStart", outcome.status);
    }
    trace::power_transition("AmtPtpEvtDeviceD0Entry", previous_state, outcome.status);
    outcome.status
}

/// `AmtPtpEvtDeviceD0Exit` (Device.c:587-642) → `pnp::d0_exit`: stop the interrupt pipe's I/O target
/// (cancel sent I/O) and disable Wellspring mode, best effort: a failed switch is traced and the callback
/// returns `STATUS_SUCCESS` (P3 fix of UM-B9).
///
/// # Safety
/// Framework callback for a device created by [`create_device`].
unsafe extern "C" fn evt_device_d0_exit(
    device: WDFDEVICE,
    target_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    // SAFETY: framework callback of our device.
    let Some(context) = (unsafe { context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    let before = context.state.snapshot();
    let mut io = DeviceIo::new(context);
    let outcome = pnp::d0_exit(&context.state, &mut io);
    if !NT_SUCCESS(outcome.mode_switch_status) {
        trace::mode_switch_failed("AmtPtpEvtDeviceD0Exit", false, outcome.mode_switch_status);
    }
    trace::state_changed("AmtPtpEvtDeviceD0Exit", &before, &context.state.snapshot());
    trace::power_transition("AmtPtpEvtDeviceD0Exit", target_state, outcome.status);
    outcome.status
}

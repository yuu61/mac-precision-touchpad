//! AmtPtpDeviceUsbUm: UMDF 2.15 Precision Touchpad driver for Apple USB trackpads (MacBook "Wellspring"
//! internal trackpads and the Magic Trackpad 2 over USB). Rust port of `src/AmtPtpDeviceUsbUm`.
//!
//! The DLL is loaded by WUDFHost below mshidumdf (the function driver, with HidClass above it) with
//! WUDFRd as a lower filter. It is a WDF filter (`WdfFdoInitSetFilter`) that receives the HID minidriver
//! IOCTLs as device-control requests, answers them with static HID descriptors, reads the USB interrupt
//! pipe with a continuous reader and turns Apple's raw multitouch packets into Precision Touchpad input
//! reports (report 0x05).
//!
//! This crate is only the WDF glue. Every decision (descriptor bytes, IOCTL routing and results,
//! feature reports, mode-switch transfers, packet parsing, PnP/power sequencing) is made by the
//! `amtptp_usbum_core` crate, whose `io` traits this crate implements with exactly the WDF DDIs the C
//! driver calls, in the same order.
//!
//! | C file | Rust module |
//! |---|---|
//! | Driver.c | this file ([`driver_entry`], driver cleanup) and `device` (`EvtDeviceAdd`) |
//! | Device.c | `device` (context, PnP/power callbacks), `usb` (USB target, pipes, control transfers) |
//! | Queue.c | `queue` |
//! | Hid.c | `amtptp_usbum_core::hid`, driven by `queue`; `request` (WDFREQUEST access) |
//! | InputInterrupt.c | `usb` (reader setup and callbacks) → `amtptp_usbum_core::input` |
//! | ModernTrace.h / WPP | `trace` |
//!
//! UMDF drivers are DLLs in WUDFHost.exe, so `std` is available, and every callback runs at
//! PASSIVE_LEVEL. A panic must never unwind across a WDF callback: `wdk_panic::install_panic_hook`
//! (first thing in `DriverEntry`) logs it with `OutputDebugStringA` and aborts the host process, and the
//! code avoids panicking operations (no indexing, no unwrap).
//!
//! Port decisions (P2 fixes, preserved C behavior) are recorded in `docs/rust-port/usbum.md`.

mod device;
mod queue;
mod request;
mod trace;
mod usb;

use wdf_support::init::{driver_config_init, object_attributes_init};
use wdf_support::ntstatus::NT_SUCCESS;
use wdk_sys::{
    NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, WDF_NO_HANDLE, WDFDRIVER, WDFOBJECT,
    call_unsafe_wdf_function_binding,
};

/// `DriverEntry` (Driver.c:12-78), called by `FxDriverEntryUm` (WdfDriverStubUm.lib, the export the
/// UMDF host calls) once the UMDF 2.15 runtime is bound.
///
/// 1. Register the TraceLogging provider (`DriverTraceInit`; WPP is not ported).
/// 2. `WDF_OBJECT_ATTRIBUTES_INIT` with `EvtCleanupCallback` = the driver cleanup callback.
/// 3. `WDF_DRIVER_CONFIG_INIT(&config, AmtPtpDeviceEvtDeviceAdd)`: no `EvtDriverUnload`, no init flags,
///    default pool tag.
/// 4. `WdfDriverCreate(DriverObject, RegistryPath, &attributes, &config, WDF_NO_HANDLE)`.
/// 5. On failure: the C `DriverFunctionalEvent` (CriticalFailure), unregister the provider, return the
///    status.
///
/// # Safety
/// Called once by the UMDF host with a valid driver object and registry path.
#[unsafe(export_name = "DriverEntry")]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    wdk_panic::install_panic_hook();

    // SAFETY: DriverEntry runs once per DLL load, so the provider is not yet registered. It is
    // unregistered on the failure path below, and otherwise by the driver-object cleanup callback, which
    // runs before the DLL unloads.
    unsafe { trace::register() };

    let mut attributes = object_attributes_init();
    attributes.EvtCleanupCallback = Some(evt_driver_context_cleanup);
    let mut config = driver_config_init(Some(device::evt_device_add));

    // SAFETY: `driver_object` and `registry_path` are the host's arguments; `attributes` and `config`
    // are initialized by the WDF INIT helpers; WDF_NO_HANDLE = do not return the WDFDRIVER handle.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver_object,
            registry_path,
            &raw mut attributes,
            &raw mut config,
            WDF_NO_HANDLE.cast::<WDFDRIVER>()
        )
    };
    if !NT_SUCCESS(status) {
        trace::driver_create_failed(status);
        // DriverTraceCleanup: the cleanup callback does not run when WdfDriverCreate fails.
        trace::unregister();
    }
    status
}

/// `AmtPtpDeviceEvtDriverContextCleanup` (Driver.c:143-158): `EvtCleanupCallback` of the WDFDRIVER, run
/// when the driver unloads → `DriverTraceCleanup` (unregister the TraceLogging provider; UM-B19: the C
/// `WPP_CLEANUP` of an undeclared identifier has no counterpart).
///
/// # Safety
/// Framework callback of the driver object.
unsafe extern "C" fn evt_driver_context_cleanup(_driver_object: WDFOBJECT) {
    trace::unregister();
}

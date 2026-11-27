//! `AmtPtpHidFilter`: KMDF 1.23 HID lower filter that turns the Magic Trackpad 2 HID collection
//! (Bluetooth; the USB transport is supported but no INF binds it) into a Windows Precision Touchpad.
//! Port of the C driver `src/AmtPtpHidFilter` (spec `hidfilter.md`).
//!
//! All hardware-independent logic (IOCTL routing, feature reports, descriptors, the read pipeline,
//! the mode switch, the PnP/power sequences, the detour's checks and `AddDevice`'s failure handling)
//! lives in `amtptp_hidfilter_core`, which owns the C call **sequences** and is replayed against the
//! C oracle fixtures. This crate is the thin WDF glue: every WDF callback is one call into the core,
//! and the core's I/O traits are implemented here with one WDF or kernel call per method.
//!
//! | C file | Rust module |
//! |---|---|
//! | `Driver.c` (`DriverEntry`, cleanup) | this file |
//! | `Driver.c` (`EvtDeviceAdd`), `Device.c`, `Queue.c` (queue creation) | [`device`] |
//! | `Queue.c` (dispatch, `EvtIoStop`) | [`queue`] |
//! | `Input.c`, `Hid.c` and the WDF calls of `Device.c` | [`io`] |
//! | `Detour.c`, `include/Hac.h` | [`hac`] (the only module that chases undocumented pointers) |
//! | `include/Device.h`, `include/Queue.h` (contexts) | [`context`] |
//!
//! Code placement mirrors the C `#pragma alloc_text`: `DriverEntry` in `INIT`; device-add and the
//! driver-object cleanup in `PAGE` (both `PASSIVE_LEVEL`). Everything else is non-paged, as in C
//! (I/O callbacks and the completion routine run at up to `DISPATCH_LEVEL`).
//!
//! Tracing: `TraceLogging` provider `AmtPtpHidFilterTraceProvider` with the C driver's WPP control
//! GUID; the keywords reuse the WPP flag bits (`TRACE_DRIVER` 0x1 ... `TRACE_INPUT` 0x10).
#![no_std]

extern crate wdk_panic; // #[panic_handler]: a panic is a bugcheck (0x52504E43).

mod context;
mod device;
mod hac;
mod io;
mod queue;

use tracelogging as tlg;
use wdf_support::init::{driver_config_init, object_attributes_init};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_SUCCESS};
use wdk_sys::{
    NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, WDF_NO_HANDLE, WDFDRIVER, WDFOBJECT,
    call_unsafe_wdf_function_binding,
};

// TraceLogging provider (replaces WPP). The id is the C driver's WPP control GUID
// (`include/Trace.h`), so existing trace-session tooling keeps the same provider GUID.
tlg::define_provider!(
    PROVIDER,
    "AmtPtpHidFilterTraceProvider",
    id("eeb34803-f3ec-4bb1-97ce-b42405d7f3c2")
);

/// Keyword of driver-level events (WPP flag `TRACE_DRIVER`).
const KW_DRIVER: u64 = 0x01;
/// Keyword of device, PnP/power, detour and mode-switch events (WPP flag `TRACE_DEVICE`).
const KW_DEVICE: u64 = 0x02;
/// Keyword of queue/dispatch events (WPP flag `TRACE_QUEUE`).
const KW_QUEUE: u64 = 0x04;
/// Keyword of HID request events (WPP flag `TRACE_HID`).
const KW_HID: u64 = 0x08;
/// Keyword of read-pipeline events (WPP flag `TRACE_INPUT`).
const KW_INPUT: u64 = 0x10;

/// `DriverEntry` (Driver.c:12-43), called by `FxDriverEntry` (WdfDriverEntry.lib, the image entry
/// point) after the KMDF runtime is bound.
///
/// `WdfDriverCreate` with `EvtDriverDeviceAdd` and, on the driver object, an `EvtCleanupCallback`;
/// no `EvtDriverUnload`, `DriverInitFlags = 0`, `DriverPoolTag = 0`, default sync scope and
/// execution level. A failure is returned unchanged.
///
/// # Safety
/// Called once by the I/O manager with a valid driver object and registry path.
#[unsafe(export_name = "DriverEntry")]
#[unsafe(link_section = "INIT")]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    // SAFETY: DriverEntry runs once per image load, so the provider is not yet registered and no
    // other thread uses it. It is unregistered on the failure path below, and otherwise by the
    // driver-object cleanup callback, which runs before the image unloads.
    unsafe { PROVIDER.register() };

    let mut attributes = object_attributes_init();
    attributes.EvtCleanupCallback = Some(evt_driver_context_cleanup);
    let mut config = driver_config_init(Some(device::evt_device_add));

    // SAFETY: `driver_object` and `registry_path` are the I/O manager's arguments; `attributes`
    // and `config` are initialized by the WDF INIT helpers; WDF_NO_HANDLE = do not return the
    // WDFDRIVER handle.
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
        tlg::write_event!(
            PROVIDER,
            "WdfDriverCreateFailed",
            level(Error),
            keyword(KW_DRIVER),
            win_ntstatus("Status", &status)
        );
        // The cleanup callback does not run when WdfDriverCreate fails (C: WPP_CLEANUP here).
        PROVIDER.unregister();
        return status;
    }
    tlg::write_event!(
        PROVIDER,
        "DriverInitialized",
        level(Informational),
        keyword(KW_DRIVER)
    );
    STATUS_SUCCESS
}

/// `PtpFilterEvtDriverContextCleanup` (Driver.c:68-80): `EvtCleanupCallback` of the WDF driver
/// object, at `PASSIVE_LEVEL` when the driver unloads (the C `WPP_CLEANUP` site).
///
/// The HIDCLASS detour is intentionally not reverted (HF-B26).
#[unsafe(link_section = "PAGE")]
unsafe extern "C" fn evt_driver_context_cleanup(_driver_object: WDFOBJECT) {
    wdk::paged_code!();
    tlg::write_event!(
        PROVIDER,
        "DriverContextCleanup",
        level(Informational),
        keyword(KW_DRIVER)
    );
    PROVIDER.unregister();
}

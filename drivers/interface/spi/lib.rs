//! `AmtPtpDeviceSpiKm`: KMDF Precision Touchpad driver for Apple SPI trackpads (C: `src/AmtPtpDeviceSpiKm`).
//!
//! A KMDF **lower filter** under `mshidkmdf.sys` on Apple's SPI HID child (`SPI\VID_05ac&PID_xxxx&MI_02`).
//! It answers the HID-minidriver internal IOCTLs itself, with a synthetic identity (VID 0x8910 and its own
//! Precision Touchpad report descriptor). It queues every upstream `IOCTL_HID_READ_REPORT` in `HidQueue`
//! and issues one lower read for each one that arrives while the device is `D0ActiveAndConfigured`; reads
//! queued while unconfigured get their lower reads when the mode switch succeeds. `SelfManagedIoSuspend`
//! stops the recovery timer and cancels the lower reads in flight; their completions no longer touch
//! `HidQueue`. It converts Apple's multitouch packet into a 30-byte PTP input report and switches the
//! trackpad into multitouch mode at every self-managed-I/O init/restart.
//!
//! This crate is the WDF glue only. Every decision that does not need WDF (descriptors, identification,
//! IOCTL routing, feature reports, the input conversion and the power/read state machine) lives in
//! [`amtptp_core::spi`], which is host-tested against fixtures recorded from the C driver.
//!
//! Modules, after the C files:
//! - this file: `DriverEntry`, the driver-object cleanup callback and `EvtDriverDeviceAdd` (Driver.c);
//! - [`device`]: the device context, device creation, the PnP/power callbacks, the mode switch and the
//!   power-on recovery timer (Device.c);
//! - [`queue`]: the queues and the internal-IOCTL dispatcher (Queue.c);
//! - [`hid`]: the HID IOCTL handlers (Hid.c);
//! - [`input`]: the read path: upstream reads, lower reads and their completion routine (Input.c).
//!
//! Code placement mirrors the C `#pragma alloc_text`: `DriverEntry` in `INIT`; device add, device
//! creation, queue creation and the driver cleanup in `PAGE` (all `PASSIVE_LEVEL` only). Everything that can
//! run at `DISPATCH_LEVEL`, and every PnP/power callback, is non-paged.
//!
//! Deliberate differences from the C driver are recorded in `docs/rust-port/spikm.md`.
#![no_std]

extern crate wdk_panic; // #[panic_handler]: a panic is a bugcheck (0x52504E43).

mod device;
mod hid;
mod input;
mod queue;

use tracelogging as tlg;
use wdf_support::init::{driver_config_init, object_attributes_init};
use wdf_support::ntstatus::NT_SUCCESS;
use wdk_sys::{
    NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, PWDFDEVICE_INIT, WDF_NO_HANDLE, WDFDRIVER,
    WDFOBJECT, call_unsafe_wdf_function_binding,
};

// TraceLogging provider (replaces WPP). The id is the C driver's WPP control GUID
// (`AmtPtpDeviceSpiKmTraceGuid`), so existing trace sessions keep using the same provider GUID.
tlg::define_provider!(
    PROVIDER,
    "AmtPtpDeviceSpiKmTraceProvider",
    id("9b319d80-fe25-41e4-80ef-7d8528f7f1b5")
);

mod keyword;

/// Driver entry point (C `DriverEntry`, Driver.c:26-98), called by `FxDriverEntry`
/// (WdfDriverEntry.lib, the image entry point) after the KMDF runtime is bound.
///
/// `WDF_OBJECT_ATTRIBUTES_INIT` + `EvtCleanupCallback`, `WDF_DRIVER_CONFIG_INIT(EvtDeviceAdd)` (no
/// `EvtDriverUnload`, no init flags, no pool tag), `WdfDriverCreate` without returning the handle.
///
/// # Safety
/// Called once by the I/O manager with a valid driver object and registry path.
#[unsafe(export_name = "DriverEntry")]
#[unsafe(link_section = "INIT")]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    // SAFETY: DriverEntry runs once per image load, so the provider is not yet
    // registered and no other thread uses it. It is unregistered on the failure
    // path below, and otherwise by the driver-object cleanup callback, which
    // runs before the image unloads. (C: WPP_INIT_TRACING.)
    unsafe { PROVIDER.register() };
    tlg::write_event!(
        PROVIDER,
        "DriverEntry",
        level(Informational),
        keyword(keyword::DRIVER)
    );

    let mut attributes = object_attributes_init();
    attributes.EvtCleanupCallback = Some(evt_driver_context_cleanup);
    let mut config = driver_config_init(Some(evt_device_add));

    // SAFETY: `driver_object` and `registry_path` are the I/O manager's
    // arguments; `attributes` and `config` are initialized by the WDF INIT
    // helpers; WDF_NO_HANDLE = do not return the WDFDRIVER handle.
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
            keyword(keyword::DRIVER),
            win_ntstatus("Status", &status)
        );
        // The cleanup callback does not run when WdfDriverCreate fails (C: WPP_CLEANUP here).
        PROVIDER.unregister();
    }
    status
}

/// `AmtPtpDeviceSpiKmEvtDriverContextCleanup` (Driver.c:151-179): the `EvtCleanupCallback` of the WDF
/// driver object. Runs at `PASSIVE_LEVEL` when the driver unloads (the C `WPP_CLEANUP` site).
#[unsafe(link_section = "PAGE")]
unsafe extern "C" fn evt_driver_context_cleanup(_driver_object: WDFOBJECT) {
    wdk::paged_code!();
    tlg::write_event!(
        PROVIDER,
        "DriverContextCleanup",
        level(Informational),
        keyword(keyword::DRIVER)
    );
    PROVIDER.unregister();
}

/// `AmtPtpDeviceSpiKmEvtDeviceAdd` (Driver.c:100-149), `PASSIVE_LEVEL`.
///
/// `WdfFdoInitSetFilter`, then [`device::create_device`]. The C code also called
/// `WdfPdoInitAllowForwardingRequestToParent`, a PDO-only DDI that has no effect on a filter FDO's
/// `WDFDEVICE_INIT` (it only logs an error and hits a verifier-only breakpoint); it is dropped (SPI-B20).
#[unsafe(link_section = "PAGE")]
unsafe extern "C" fn evt_device_add(_driver: WDFDRIVER, device_init: PWDFDEVICE_INIT) -> NTSTATUS {
    wdk::paged_code!();

    // SAFETY: `device_init` is the framework-provided WDFDEVICE_INIT of this
    // callback; WdfFdoInitSetFilter only marks it as a filter.
    unsafe { call_unsafe_wdf_function_binding!(WdfFdoInitSetFilter, device_init) };

    // SAFETY: `device_init` is still the unconsumed WDFDEVICE_INIT of this
    // EvtDriverDeviceAdd call, at PASSIVE_LEVEL.
    let status = unsafe { device::create_device(device_init) };
    tlg::write_event!(
        PROVIDER,
        "DeviceAdd",
        level(Informational),
        keyword(keyword::DRIVER),
        win_ntstatus("Status", &status)
    );
    status
}

//! `AmtPtpDeviceUsbKm`: KMDF Precision Touchpad driver for Apple T2 USB trackpads (C: `src/AmtPtpDeviceUsbKm`).
//!
//! A KMDF lower filter on interface 2 of the T2 keyboard/trackpad (`USB\VID_05AC&PID_027x&MI_02`) whose
//! function driver is `mshidkmdf`, so this filter is the real HID minidriver: it answers the HID internal
//! IOCTLs, reads the trackpad with a USB continuous reader and switches it into wellspring (raw multitouch)
//! mode. Porting spec: `usbkm.md`; port record: `docs/rust-port/usbkm.md`.
//!
//! This crate is the WDF glue only. Every hardware-independent decision (descriptors, IOCTL routing,
//! feature reports, packet parsing, PnP/power sequencing, wire bytes) is in `amtptp_usb_bt::usb`; the glue
//! performs the WDF calls the C code makes, in the C order.
//!
//! | C (file) | Rust |
//! |---|---|
//! | `DriverEntry`, `AmtPtpDeviceUsbKmEvtDriverContextCleanup` (Driver.c) | [`driver_entry`], `evt_driver_context_cleanup` |
//! | `AmtPtpDeviceUsbKmEvtDeviceAdd`, `AmtPtpDeviceUsbKmCreateDevice` (Driver.c, Device.c) | `device::evt_device_add`, `device::create_device` |
//! | `AmtPtpDeviceUsbKmEvtDevicePrepareHardware`, `AmtPtpEvtDeviceD0Entry`, `AmtPtpEvtDeviceD0Exit` (Device.c) | `device::evt_device_prepare_hardware`, `device::evt_device_d0_entry`, `device::evt_device_d0_exit` |
//! | `SelectInterruptInterface`, `AmtPtpSetWellspringMode`, `AmtPtpConfigContReaderForInterruptEndPoint` | core `lifecycle` / `wellspring` over `usb::UsbIo` |
//! | `AmtPtpDeviceUsbKmQueueInitialize`, `AmtPtpDeviceUsbKmEvtIoDeviceControl`, `AmtPtpDeviceUsbKmEvtIoStop` (Queue.c) | `queue::queue_initialize`, `queue::evt_io_internal_device_control`, `queue::evt_io_stop` |
//! | `AmtPtpGetHidDescriptor`, `AmtPtpGetDeviceAttribs`, `AmtPtpGetReportDescriptor`, `AmtPtpReportFeatures`, `AmtPtpSetFeatures`, `AmtPtpDispatchReadReportRequests` (Hid.c, Queue.c) | `queue::*` + core `hid` / `feature` |
//! | `AmtPtpEvtUsbInterruptPipeReadComplete`, `AmtPtpEvtUsbInterruptReadersFailed` (Interrupt.c) | `interrupt::evt_usb_interrupt_pipe_read_complete`, `interrupt::evt_usb_interrupt_readers_failed` |
//!
//! Code placement mirrors the C `#pragma alloc_text`: `DriverEntry` in `INIT`; device add, the driver
//! cleanup, queue initialization and `PrepareHardware` in `PAGE` (all `PASSIVE_LEVEL`). Everything else (I/O,
//! power and USB-completion code) is non-paged.
#![no_std]

extern crate wdk_panic; // #[panic_handler]: a panic is a bugcheck (0x52504E43).

mod context;
mod device;
mod interrupt;
mod queue;
mod trace;
mod usb;

use tracelogging as tlg;
use wdf_support::init::{driver_config_init, object_attributes_init};
use wdf_support::ntstatus::NT_SUCCESS;
use wdk_sys::{
    NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, WDF_NO_HANDLE, WDFDRIVER, WDFOBJECT,
    call_unsafe_wdf_function_binding,
};

use crate::trace::KW_DRIVER;

// TraceLogging provider (replaces WPP). The id is the C driver's WPP control GUID
// (AmtPtpDeviceUsbKmTraceGuid, Trace.h:20-31), so trace sessions keep the same provider GUID. Defined at
// the crate root so every module can use it (define_provider! symbols are module-private).
tlg::define_provider!(
    PROVIDER,
    "AmtPtpDeviceUsbKmTraceProvider",
    id("b409cf52-f41a-4193-98c2-bad95204203b")
);

/// Driver entry point (`DriverEntry`, Driver.c:27-96), called by `FxDriverEntry` (WdfDriverEntry.lib, the
/// image entry point) after the KMDF runtime is bound.
///
/// `WDF_OBJECT_ATTRIBUTES_INIT` + `EvtCleanupCallback`, `WDF_DRIVER_CONFIG_INIT(EvtDeviceAdd)` (no
/// `EvtDriverUnload`, `DriverInitFlags` 0, pool tag 0), `WdfDriverCreate(.., WDF_NO_HANDLE)`.
///
/// # Safety
/// Called once by the I/O manager with a valid driver object and registry path.
#[unsafe(export_name = "DriverEntry")]
#[unsafe(link_section = "INIT")]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    // WPP_INIT_TRACING.
    // SAFETY: DriverEntry runs once per image load, so the provider is not yet
    // registered and no other thread uses it. It is unregistered on the failure
    // path below, and otherwise by the driver-object cleanup callback, which
    // runs before the image unloads.
    unsafe { PROVIDER.register() };
    tlg::write_event!(
        PROVIDER,
        "DriverEntry",
        level(Informational),
        keyword(KW_DRIVER)
    );

    let mut attributes = object_attributes_init();
    attributes.EvtCleanupCallback = Some(evt_driver_context_cleanup);
    let mut config = driver_config_init(Some(device::evt_device_add));

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
            keyword(KW_DRIVER),
            win_ntstatus("Status", &status)
        );
        // WPP_CLEANUP(DriverObject): the cleanup callback does not run when
        // WdfDriverCreate fails.
        PROVIDER.unregister();
    }
    status
}

/// `AmtPtpDeviceUsbKmEvtDriverContextCleanup` (Driver.c:145-175): the `EvtCleanupCallback` of the WDF
/// driver object, at `PASSIVE_LEVEL` when the driver unloads. The C only calls `WPP_CLEANUP`.
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

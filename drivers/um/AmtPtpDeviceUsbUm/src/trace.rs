//! `TraceLogging` (the C `ModernTrace.h` provider) and the replacement for the WPP trace points.
//!
//! Provider: `"AmtPtpDeviceTraceProvider"`, `{871b1e2d-cc5a-4ade-b74e-6cf1004ef149}` (Driver.c:7-10), so
//! `AmtPtpDevice.wprp` keeps working. It is registered in `DriverEntry` and unregistered by the driver
//! object's cleanup callback, or in `DriverEntry` itself when `WdfDriverCreate` fails.
//!
//! The three events the C driver writes are reproduced exactly: same event name, same field names,
//! field order and field types (`TraceLoggingString` = `cstr8_cp1252`, `TraceLoggingUInt16` = `u16`,
//! `TraceLoggingInt32` = `i32`), and the C defaults of `TraceLoggingWrite` (level 5 = verbose,
//! keyword 0).
//!
//! WPP (`TraceEvents`, control GUID `{efc3ce99-...}`) has no Rust equivalent. Its meaningful trace points
//! (errors, PnP/power transitions, mode switches, input-path anomalies) are written as additional
//! `TraceLogging` events on the same provider, under event names the C driver never wrote with fields:
//!
//! | event | content |
//! |---|---|
//! | `DevicePnpEvent` | `EvtDeviceAdd` / queue creation / `PrepareHardware` (incl. the failing USB setup step: target creation, interface and pipe selection, reader configuration) / `D0Entry` / `D0Exit` results |
//! | `DeviceUsbOperationEvent` | failed control transfers, failed Wellspring-mode switches whose status C ignores (`D0Entry`) or returns (`D0Exit`), continuous-reader failures |
//! | `DeviceStateEvent` | changes of the driver state (Wellspring mode, selective reporting, qualification levels) |
//! | `DeviceIoctlEvent` | device-control requests that fail or are not supported |
//! | `InputDiagnosticsEvent` | read-completion anomalies (malformed packets / emergency reset, failed `READ_REPORT` completion, dropped frames) |
//!
//! (`DeviceUsbOperationEvent` and `InputDiagnosticsEvent` are names `ModernTrace.h` declares but never
//! uses.) All events use keyword 0, like the C events.

use amtptp_usbum_core::hid::TraceEvent;
use amtptp_usbum_core::input::ReadCompleteOutcome;
use amtptp_usbum_core::state::StateSnapshot;
use tracelogging as tlg;
use wdk_sys::NTSTATUS;

// TraceLogging provider of the C driver (Driver.c:7-10), same name and GUID.
tlg::define_provider!(
    PROVIDER,
    "AmtPtpDeviceTraceProvider",
    id("871b1e2d-cc5a-4ade-b74e-6cf1004ef149")
);

/// `DriverTraceInit`: `TraceLoggingRegister(g_hAmtPtpDeviceTraceProvider)`.
///
/// # Safety
/// Must not be called while the provider is registered; it must be paired with [`unregister`] before
/// the DLL unloads.
pub(crate) unsafe fn register() {
    // SAFETY: forwarded to the caller (not yet registered, unregistered before unload). The result is
    // ignored, as the C code ignores TraceLoggingRegister's.
    unsafe { PROVIDER.register() };
}

/// `DriverTraceCleanup`: `TraceLoggingUnregister(g_hAmtPtpDeviceTraceProvider)`.
pub(crate) fn unregister() {
    PROVIDER.unregister();
}

// ---------------------------------------------------------------------------------------------------
// The C events (exact).
// ---------------------------------------------------------------------------------------------------

/// Driver.c:59-66: `DriverFunctionalEvent` when `WdfDriverCreate` fails.
pub(crate) fn driver_create_failed(status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "DriverFunctionalEvent",
        keyword(0),
        cstr8_cp1252("Routine", "DriverEntry"),
        cstr8_cp1252("Context", "WdfDriverCreate"),
        i32("Status", &status),
        cstr8_cp1252("Subtype", "CriticalFailure"),
    );
}

/// Device.c:181-188 and Hid.c:168-174: `DeviceIdentificationEvent`.
pub(crate) fn device_identification(event: TraceEvent) {
    match event {
        TraceEvent::DeviceNotFound {
            id_product,
            id_vendor,
        } => {
            tlg::write_event!(
                PROVIDER,
                "DeviceIdentificationEvent",
                keyword(0),
                cstr8_cp1252("Routine", event.routine()),
                u16("idProduct", &id_product),
                u16("idVendor", &id_vendor),
                cstr8_cp1252("Subtype", event.subtype()),
            );
        }
        TraceEvent::HidDescriptorNotFound { id_product } => {
            tlg::write_event!(
                PROVIDER,
                "DeviceIdentificationEvent",
                keyword(0),
                cstr8_cp1252("Routine", event.routine()),
                u16("idProduct", &id_product),
                cstr8_cp1252("Subtype", event.subtype()),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Replacements for the WPP trace points.
// ---------------------------------------------------------------------------------------------------

/// `DevicePnpEvent` (level Error): a PnP/power/device-add step failed.
pub(crate) fn pnp_failure(routine: &str, context: &str, status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "DevicePnpEvent",
        level(Error),
        keyword(0),
        cstr8_cp1252("Routine", routine),
        cstr8_cp1252("Context", context),
        win_ntstatus("Status", &status),
    );
}

/// `DevicePnpEvent` (level Informational): a power transition (`D0Entry` coming from `power_state`,
/// `D0Exit` moving to `power_state`, a `WDF_POWER_DEVICE_STATE` value) and its result.
pub(crate) fn power_transition(routine: &str, power_state: i32, status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "DevicePnpEvent",
        level(Informational),
        keyword(0),
        cstr8_cp1252("Routine", routine),
        i32("PowerState", &power_state),
        win_ntstatus("Status", &status),
    );
}

/// `DevicePnpEvent` (level Informational): `PrepareHardware` succeeded.
pub(crate) fn prepare_hardware_done(
    id_vendor: u16,
    id_product: u16,
    traits: u32,
    endpoint_address: u8,
    pipe: usize,
    configured_reader: bool,
) {
    tlg::write_event!(
        PROVIDER,
        "DevicePnpEvent",
        level(Informational),
        keyword(0),
        cstr8_cp1252("Routine", "AmtPtpEvtDevicePrepareHardware"),
        u16("idVendor", &id_vendor),
        u16("idProduct", &id_product),
        u32_hex("UsbDeviceTraits", &traits),
        u8("InterruptEndpointAddress", &endpoint_address),
        pointer("InterruptPipe", &pipe),
        bool8("ContinuousReaderConfigured", &configured_reader),
    );
}

/// `DeviceUsbOperationEvent` (level Error): a control transfer failed (`setup` = the 8 setup bytes).
pub(crate) fn control_transfer_failed(setup: [u8; 8], status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "DeviceUsbOperationEvent",
        level(Error),
        keyword(0),
        cstr8_cp1252(
            "Routine",
            "WdfUsbTargetDeviceSendControlTransferSynchronously"
        ),
        binary("SetupPacket", &setup),
        win_ntstatus("Status", &status),
    );
}

/// `DeviceUsbOperationEvent` (level Warning): the continuous reader failed; the framework resets the
/// pipe and restarts it (`EvtUsbTargetPipeReadersFailed` returns TRUE).
pub(crate) fn readers_failed(status: NTSTATUS, usbd_status: i32) {
    tlg::write_event!(
        PROVIDER,
        "DeviceUsbOperationEvent",
        level(Warning),
        keyword(0),
        cstr8_cp1252("Routine", "AmtPtpEvtUsbInterruptReadersFailed"),
        win_ntstatus("Status", &status),
        i32_hex("UsbdStatus", &usbd_status),
    );
}

/// `DeviceUsbOperationEvent` (level Warning): a Wellspring-mode switch whose failure the C code only
/// traces (`D0Entry`, emergency reset).
pub(crate) fn mode_switch_failed(routine: &str, enable: bool, status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "DeviceUsbOperationEvent",
        level(Warning),
        keyword(0),
        cstr8_cp1252("Routine", routine),
        bool8("Enable", &enable),
        win_ntstatus("Status", &status),
    );
}

/// `DeviceStateEvent` (level Informational): the driver state (flags, qualification levels) changed in
/// `routine`. `PerfCounter` (reset by every `D0Entry`) is not compared.
pub(crate) fn state_changed(routine: &str, before: &StateSnapshot, after: &StateSnapshot) {
    let without_qpc_frequency = |snapshot: &StateSnapshot| StateSnapshot {
        qpc_frequency: 0,
        ..*snapshot
    };
    if without_qpc_frequency(before) == without_qpc_frequency(after) {
        return;
    }
    tlg::write_event!(
        PROVIDER,
        "DeviceStateEvent",
        level(Informational),
        keyword(0),
        cstr8_cp1252("Routine", routine),
        bool8("WellspringModeOn", &after.wellspring_mode_on),
        bool8("SurfaceReportOn", &after.surface_report_on),
        bool8("ButtonReportOn", &after.button_report_on),
        u8("PressureQualLevel", &after.pressure_qual_level),
        u8("SgContactSizeQualLevel", &after.sg_contact_size_qual_level),
        u8("MuContactSizeQualLevel", &after.mu_contact_size_qual_level),
    );
}

/// `DeviceIoctlEvent` (level Verbose): an IOCTL the driver does not support (completed with
/// `STATUS_NOT_SUPPORTED`, not forwarded).
pub(crate) fn ioctl_not_supported(io_control_code: u32) {
    tlg::write_event!(
        PROVIDER,
        "DeviceIoctlEvent",
        level(Verbose),
        keyword(0),
        u32_hex("IoControlCode", &io_control_code),
        cstr8_cp1252("Name", ptp_hid::ioctl::ioctl_name(io_control_code)),
    );
}

/// `DeviceIoctlEvent` (level Warning): a supported IOCTL completed with a failure status.
pub(crate) fn ioctl_failed(io_control_code: u32, status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "DeviceIoctlEvent",
        level(Warning),
        keyword(0),
        u32_hex("IoControlCode", &io_control_code),
        cstr8_cp1252("Name", ptp_hid::ioctl::ioctl_name(io_control_code)),
        win_ntstatus("Status", &status),
    );
}

/// `InputDiagnosticsEvent`: what a read completion did, for the outcomes worth tracing.
///
/// Emergency resets (UM-B11) and failed `READ_REPORT` completions (UM-B6) are warnings; frames dropped
/// because no `READ_REPORT` is pending or Wellspring mode is off are verbose (they can occur for every
/// packet). A normal completion is not traced.
pub(crate) fn read_complete(outcome: ReadCompleteOutcome, length: usize) {
    match outcome {
        ReadCompleteOutcome::EmergencyReset { status } => {
            tlg::write_event!(
                PROVIDER,
                "InputDiagnosticsEvent",
                level(Warning),
                keyword(0),
                cstr8_cp1252("Outcome", "MalformedPacketEmergencyReset"),
                usize("Length", &length),
                win_ntstatus("Status", &status),
            );
        }
        ReadCompleteOutcome::CompletedWithError { status } => {
            tlg::write_event!(
                PROVIDER,
                "InputDiagnosticsEvent",
                level(Warning),
                keyword(0),
                cstr8_cp1252("Outcome", "ReadReportCompletedWithError"),
                usize("Length", &length),
                win_ntstatus("Status", &status),
            );
        }
        ReadCompleteOutcome::NoPendingRequest { status } => {
            tlg::write_event!(
                PROVIDER,
                "InputDiagnosticsEvent",
                level(Verbose),
                keyword(0),
                cstr8_cp1252("Outcome", "NoPendingReadReport"),
                usize("Length", &length),
                win_ntstatus("Status", &status),
            );
        }
        ReadCompleteOutcome::DroppedModeOff => {
            tlg::write_event!(
                PROVIDER,
                "InputDiagnosticsEvent",
                level(Verbose),
                keyword(0),
                cstr8_cp1252("Outcome", "DroppedWellspringModeOff"),
                usize("Length", &length),
            );
        }
        ReadCompleteOutcome::DroppedUnsupported => {
            tlg::write_event!(
                PROVIDER,
                "InputDiagnosticsEvent",
                level(Verbose),
                keyword(0),
                cstr8_cp1252("Outcome", "DroppedUnsupportedType"),
                usize("Length", &length),
            );
        }
        ReadCompleteOutcome::DroppedNoConfig => {
            tlg::write_event!(
                PROVIDER,
                "InputDiagnosticsEvent",
                level(Error),
                keyword(0),
                cstr8_cp1252("Outcome", "DroppedNoDeviceConfig"),
                usize("Length", &length),
            );
        }
        ReadCompleteOutcome::Completed => {}
    }
}

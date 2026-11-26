//! The device context, device creation, the PnP/power callbacks, the mode switch and the power-on
//! recovery timer (C: Device.c).

use core::ptr::null_mut;

use amtptp_spikm_core::device::{
    DESIRED_REPORT_TYPE_VALUE_LEN, DESIRED_REPORT_TYPE_VALUE_NAME, PARAMETERS_KEY_ACCESS,
    SharedDeviceConfig, desired_report_type_value,
};
use amtptp_spikm_core::runtime::{D0_EXIT_DRAIN_STATUS, DeviceRuntime, RECOVERY_TIMER_PERIOD};
use amtptp_spikm_core::wire::{
    DEVICE_INTERFACE_GUID, LOWER_ATTRIBUTES_IOCTL, LOWER_ATTRIBUTES_LEN, LOWER_READ_BUFFER_LEN,
    NO_IO_TARGET_STATUS, POOL_TAG, SET_STATE_IOCTL, SET_STATE_PACKET_LEN, SetStatePacket,
};
use tracelogging as tlg;
use wdf_support::declare_context_type;
use wdf_support::init::{
    memory_descriptor_init_buffer, object_attributes_init, object_attributes_init_context_type,
    pnppower_event_callbacks_init, timer_config_init,
};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE, STATUS_SUCCESS};
use wdf_support::request::Request;
use wdk_sys::{
    _POOL_TYPE, _WDF_EXECUTION_LEVEL, GUID, LARGE_INTEGER, NTSTATUS, PWDFDEVICE_INIT, ULONG,
    UNICODE_STRING, WDF_NO_OBJECT_ATTRIBUTES, WDF_POWER_DEVICE_STATE, WDFCMRESLIST, WDFDEVICE,
    WDFIOTARGET, WDFKEY, WDFLOOKASIDE, WDFQUEUE, WDFREQUEST, WDFTIMER,
    call_unsafe_wdf_function_binding,
};

use crate::{PROVIDER, input, keyword, queue};

/// Port of the C `DEVICE_CONTEXT` (Device.h:56-84). Named `DEVICE_CONTEXT` for `!wdfkd`.
///
/// The four handles are written exactly once, inside [`create_device`], before the device can receive
/// any request or PnP/power callback; afterwards the context is only accessed through shared references.
/// All state that changes later is atomic ([`SharedDeviceConfig`], [`DeviceRuntime`]), so concurrent
/// callbacks at up to DISPATCH_LEVEL never race on plain fields (P2 for SPI-B15).
///
/// Dropped C fields (dead): `SpiDevice`, `InputPollThreadHandle`. `HidVendorID`, `HidProductID`,
/// `HidVersionNumber`, `TrackpadInfo` and `ReportType` are in `config`; `DeviceStatus`, `LastReportTime`,
/// `PtpInputOn`, `PtpReportButton` and `PtpReportTouch` are in `runtime`.
pub(crate) struct DeviceContext {
    /// `SpiTrackpadIoTarget`: the local (default) I/O target, i.e. the Apple SPI PDO below us.
    pub(crate) spi_trackpad_io_target: WDFIOTARGET,
    /// `HidQueue`: the manual queue holding the pending upstream `IOCTL_HID_READ_REPORT` requests.
    pub(crate) hid_queue: WDFQUEUE,
    /// `PowerOnRecoveryTimer`: the 5 s one-shot passive-level retry timer.
    pub(crate) power_on_recovery_timer: WDFTIMER,
    /// `HidReadBufferLookaside`: 1024-byte NonPagedPoolNx buffers for the lower reads.
    pub(crate) hid_read_buffer_lookaside: WDFLOOKASIDE,
    /// What PrepareHardware found out about the device.
    pub(crate) config: SharedDeviceConfig,
    /// `DeviceStatus`, `LastReportTime` and the PTP feature flags.
    pub(crate) runtime: DeviceRuntime,
}

impl DeviceContext {
    /// The state right after `WdfDeviceCreate`: null handles, the zero configuration and
    /// `DeviceStatus = D3` (C: zero-filled context, then `DeviceStatus = D3` at Device.c:134).
    const fn new() -> Self {
        Self {
            spi_trackpad_io_target: null_mut(),
            hid_queue: null_mut(),
            power_on_recovery_timer: null_mut(),
            hid_read_buffer_lookaside: null_mut(),
            config: SharedDeviceConfig::new(),
            runtime: DeviceRuntime::new(),
        }
    }
}

declare_context_type!(pub(crate) DeviceContext, DEVICE_CONTEXT_TYPE_INFO, device_get_context, name = "DEVICE_CONTEXT");

/// `DeviceGetContext(Device)` as a shared reference.
///
/// Returns `None` only if the object has no `DEVICE_CONTEXT`, which cannot happen for the devices this
/// driver creates (C checked it in PrepareHardware and SetState only; the port checks it everywhere
/// instead of dereferencing NULL).
///
/// # Safety
/// `device` is a live WDFDEVICE created by [`create_device`] (so its context was initialized there), and
/// the caller runs at IRQL <= DISPATCH_LEVEL.
pub(crate) unsafe fn device_context<'a>(device: WDFDEVICE) -> Option<&'a DeviceContext> {
    // SAFETY: valid live device handle (caller contract).
    let context = unsafe { device_get_context(device) };
    // SAFETY: a non-null context of this type was initialized with `write` in
    // `create_device` before the device was published, is never mutated through
    // a `&mut` afterwards, and lives as long as the device.
    unsafe { context.as_ref() }
}

/// `KeQueryPerformanceCounter(&x)` exactly as the C driver calls it: the variable is passed as the
/// optional **PerformanceFrequency** out-parameter and the returned counter is discarded, so this returns
/// the QPC frequency, a boot-time constant (SPI-B8, preserved: ScanTime is therefore always 0).
pub(crate) fn query_performance_counter_as_c() -> i64 {
    let mut frequency = LARGE_INTEGER { QuadPart: 0 };
    // SAFETY: `frequency` is a valid out-parameter; KeQueryPerformanceCounter
    // may be called at any IRQL.
    let _discarded_counter =
        unsafe { wdk_sys::ntddk::KeQueryPerformanceCounter(&raw mut frequency) };
    // SAFETY: `QuadPart` covers the whole 8-byte union, which was initialized
    // above and then written by the call.
    unsafe { frequency.QuadPart }
}

/// `GUID_DEVINTERFACE_AmtPtpDeviceSpiKm` {f271e39d-d211-4a6e-a439-63e51940d210} (Public.h:22-24).
static DEVICE_INTERFACE_CLASS_GUID: GUID = GUID {
    Data1: DEVICE_INTERFACE_GUID.data1,
    Data2: DEVICE_INTERFACE_GUID.data2,
    Data3: DEVICE_INTERFACE_GUID.data3,
    Data4: DEVICE_INTERFACE_GUID.data4,
};

/// `AmtPtpDeviceSpiKmCreateDevice` (Device.c:24-164), PASSIVE_LEVEL (`PAGE`, as in C).
///
/// In C order: PnP/power callbacks, `WdfDeviceCreate` (context `DEVICE_CONTEXT`), the lookaside list, the
/// recovery timer, the local I/O target (NULL: `STATUS_INVALID_DEVICE_STATE`), the device interface and,
/// only if that succeeded, the queues. Any failure returns its status (the framework then deletes the
/// device). Two deliberate differences: the lookaside list is parented to the device instead of the
/// driver (SPI-B21, the C list leaked per device until unload), and the timer has
/// `AutomaticSerialization = FALSE` (no effect under synchronization scope None; TRUE is a documented
/// invalid combination with a passive-level timer under a dispatch-level device).
///
/// No other `WdfDeviceInit*` setter is called, so the filter inherits the I/O type, `DO_POWER_PAGABLE`,
/// device type and characteristics from the lower device, as the C driver does.
///
/// # Safety
/// `device_init` is the unconsumed WDFDEVICE_INIT of the current `EvtDriverDeviceAdd` call.
#[unsafe(link_section = "PAGE")]
pub(crate) unsafe fn create_device(device_init: PWDFDEVICE_INIT) -> NTSTATUS {
    wdk::paged_code!();

    let mut device_init = device_init;
    let mut pnp_power_callbacks = pnppower_event_callbacks_init();
    pnp_power_callbacks.EvtDevicePrepareHardware = Some(evt_device_prepare_hardware);
    pnp_power_callbacks.EvtDeviceD0Entry = Some(evt_device_d0_entry);
    pnp_power_callbacks.EvtDeviceD0Exit = Some(evt_device_d0_exit);
    pnp_power_callbacks.EvtDeviceSelfManagedIoInit =
        Some(evt_device_self_managed_io_init_or_restart);
    pnp_power_callbacks.EvtDeviceSelfManagedIoRestart =
        Some(evt_device_self_managed_io_init_or_restart);
    // SAFETY: `device_init` is valid and unconsumed (caller contract); the
    // callbacks structure is initialized by WDF_PNPPOWER_EVENT_CALLBACKS_INIT.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceInitSetPnpPowerEventCallbacks,
            device_init,
            &raw mut pnp_power_callbacks
        );
    }

    let mut device_attributes = object_attributes_init_context_type(&DEVICE_CONTEXT_TYPE_INFO);
    let mut device: WDFDEVICE = null_mut();
    // SAFETY: `device_init` is valid and unconsumed (WdfDeviceCreate consumes
    // it); the attributes carry the DEVICE_CONTEXT type information.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreate,
            &raw mut device_init,
            &raw mut device_attributes,
            &raw mut device
        )
    };
    if !NT_SUCCESS(status) {
        trace_create_device_failure(b"WdfDeviceCreate", status);
        return status;
    }

    // SAFETY: `device` was just created with DEVICE_CONTEXT_TYPE_INFO.
    let context = unsafe { device_get_context(device) };
    if context.is_null() {
        // Unreachable: the device was created with this context type.
        return STATUS_INVALID_DEVICE_STATE;
    }
    // SAFETY: `context` is the zero-filled, suitably aligned DEVICE_CONTEXT
    // memory of the new device; nothing else can reach it before this
    // callback returns, so writing (and the raw field writes below) cannot race.
    unsafe { context.write(DeviceContext::new()) };

    // SAFETY: as above; `device` is the device this context belongs to.
    unsafe { initialize_device(device, context) }
}

/// The part of `AmtPtpDeviceSpiKmCreateDevice` after `WdfDeviceCreate` (Device.c:78-151).
///
/// # Safety
/// `device` was just created by [`create_device`], and `context` is its initialized context, not yet
/// reachable by any other callback (so the handle fields can be written through the raw pointer).
#[unsafe(link_section = "PAGE")]
unsafe fn initialize_device(device: WDFDEVICE, context: *mut DeviceContext) -> NTSTATUS {
    wdk::paged_code!();

    // Lookaside list: REPORT_BUFFER_SIZE (1024) bytes, NonPagedPoolNx, tag 'LTPA'. Parent = the device
    // (C: WDF_NO_OBJECT_ATTRIBUTES, i.e. the driver; SPI-B21). Memory objects: no attributes, as in C.
    let mut lookaside_attributes = object_attributes_init();
    lookaside_attributes.ParentObject = device.cast();
    // SAFETY: valid device; the attributes are initialized; the out-parameter
    // is the context field (C passes &pDeviceContext->HidReadBufferLookaside),
    // writable per the function contract.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfLookasideListCreate,
            &raw mut lookaside_attributes,
            LOWER_READ_BUFFER_LEN,
            _POOL_TYPE::NonPagedPoolNx,
            WDF_NO_OBJECT_ATTRIBUTES,
            POOL_TAG,
            &raw mut (*context).hid_read_buffer_lookaside
        )
    };
    if !NT_SUCCESS(status) {
        trace_create_device_failure(b"WdfLookasideListCreate", status);
        return status;
    }

    // Power-on recovery timer: one-shot (Period 0), passive-level callback, parent = the device.
    let mut timer_config = timer_config_init(Some(power_recovery_timer_callback));
    timer_config.Period = RECOVERY_TIMER_PERIOD;
    // C sets TRUE; FALSE is behaviorally identical here (device synchronization scope None) and avoids
    // the documented-invalid passive-timer/dispatch-device combination (spec §2.4).
    timer_config.AutomaticSerialization = 0;
    let mut timer_attributes = object_attributes_init();
    timer_attributes.ParentObject = device.cast();
    timer_attributes.ExecutionLevel = _WDF_EXECUTION_LEVEL::WdfExecutionLevelPassive;
    // SAFETY: initialized config and attributes; the out-parameter is the
    // context field, writable per the function contract.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfTimerCreate,
            &raw mut timer_config,
            &raw mut timer_attributes,
            &raw mut (*context).power_on_recovery_timer
        )
    };
    if !NT_SUCCESS(status) {
        trace_create_device_failure(b"WdfTimerCreate", status);
        return status;
    }

    // SAFETY: valid device handle.
    let io_target = unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetIoTarget, device) };
    // SAFETY: writable per the function contract.
    unsafe { (*context).spi_trackpad_io_target = io_target };
    if io_target.is_null() {
        trace_create_device_failure(b"WdfDeviceGetIoTarget", NO_IO_TARGET_STATUS);
        return NO_IO_TARGET_STATUS;
    }

    // DeviceStatus = D3: already set by DeviceContext::new().

    // SAFETY: valid device; the GUID is a 'static; no reference string (C: NULL).
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreateDeviceInterface,
            device,
            &raw const DEVICE_INTERFACE_CLASS_GUID,
            core::ptr::null()
        )
    };
    if !NT_SUCCESS(status) {
        trace_create_device_failure(b"WdfDeviceCreateDeviceInterface", status);
        return status;
    }

    // SAFETY: valid device; `hid_queue` is writable per the function contract.
    unsafe { queue::queue_initialize(device, &raw mut (*context).hid_queue) }
}

/// Error trace for the device-creation steps.
fn trace_create_device_failure(step: &[u8], status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "CreateDeviceFailed",
        level(Error),
        keyword(keyword::DRIVER),
        str8("Step", step),
        win_ntstatus("Status", &status)
    );
}

/// The 32-byte `HID_DEVICE_ATTRIBUTES` output buffer of the identification request, aligned like the C
/// struct (its first field is a `ULONG`).
#[repr(C, align(8))]
struct LowerAttributesBuffer([u8; LOWER_ATTRIBUTES_LEN]);

/// `LOWER_ATTRIBUTES_LEN` (32) as the `ULONG` length of a memory descriptor.
const LOWER_ATTRIBUTES_DESCRIPTOR_LEN: ULONG = {
    assert!(LOWER_ATTRIBUTES_LEN <= ULONG::MAX as usize);
    LOWER_ATTRIBUTES_LEN as ULONG
};

/// `AmtPtpEvtDevicePrepareHardware` (Device.c:166-317), PASSIVE_LEVEL (non-paged, as in C).
///
/// 1. Synchronous lower `IOCTL_HID_GET_DEVICE_ATTRIBUTES` into a zeroed 32-byte buffer (no timeout:
///    SPI-B23, preserved). Failure returns that status.
/// 2. Store VID/PID/version, then look the device up in the config table: no match returns
///    `STATUS_NOT_FOUND` (the IDs stay stored, as in C).
/// 3. Read `DesiredReportType` from the driver's Parameters key (zero-initialized value buffer: SPI-B24).
/// 4. Return `STATUS_SUCCESS` whatever the registry step did.
unsafe extern "C" fn evt_device_prepare_hardware(
    device: WDFDEVICE,
    _resource_list: WDFCMRESLIST,
    _resource_list_translated: WDFCMRESLIST,
) -> NTSTATUS {
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        // C: the (unreachable) `pDeviceContext == NULL` branch, Device.c:197-207.
        return STATUS_INVALID_DEVICE_STATE;
    };

    let mut attributes = LowerAttributesBuffer([0; LOWER_ATTRIBUTES_LEN]);
    let mut output = memory_descriptor_init_buffer(
        attributes.0.as_mut_ptr().cast(),
        LOWER_ATTRIBUTES_DESCRIPTOR_LEN,
    );
    // SAFETY: PASSIVE_LEVEL; valid local I/O target; `output` describes the
    // 32-byte local buffer, which outlives this synchronous call; no request,
    // input, options or bytes-returned (all NULL, as in C).
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoTargetSendInternalIoctlSynchronously,
            context.spi_trackpad_io_target,
            null_mut(),
            LOWER_ATTRIBUTES_IOCTL,
            null_mut(),
            &raw mut output,
            null_mut(),
            null_mut()
        )
    };

    let mut config = context.config.load();
    let identified = config.identify(status, &attributes.0);
    // C writes HidVendorID/HidProductID/HidVersionNumber before the table lookup, so they are stored
    // even when the lookup fails (identify leaves `config` unchanged when the lower request failed).
    context.config.store(&config);
    if let Err(status) = identified {
        tlg::write_event!(
            PROVIDER,
            "PrepareHardwareFailed",
            level(Error),
            keyword(keyword::DRIVER),
            win_ntstatus("Status", &status),
            u16_hex("VendorId", &config.hid_vendor_id),
            u16_hex("ProductId", &config.hid_product_id)
        );
        return status;
    }

    // SAFETY: valid device handle, PASSIVE_LEVEL.
    let desired_report_type = unsafe { query_desired_report_type(device) };
    config.apply_desired_report_type(desired_report_type);
    context.config.store(&config);

    tlg::write_event!(
        PROVIDER,
        "PrepareHardware",
        level(Informational),
        keyword(keyword::DRIVER),
        u16_hex("VendorId", &config.hid_vendor_id),
        u16_hex("ProductId", &config.hid_product_id),
        u16_hex("VersionNumber", &config.hid_version_number),
        u32("ReportType", &config.report_type.0)
    );
    STATUS_SUCCESS
}

/// `ASCII_NAME` as a NUL-terminated UTF-16 array of `N` code units (`N` = length + 1), for a
/// `DECLARE_CONST_UNICODE_STRING`. Evaluated only in a constant initializer, where a wrong `N` or a
/// non-ASCII byte is a compile-time error.
#[allow(clippy::indexing_slicing)] // const evaluation only: out-of-bounds fails the build, not the driver
const fn ascii_to_utf16z<const N: usize>(ascii_name: &str) -> [u16; N] {
    let bytes = ascii_name.as_bytes();
    assert!(
        bytes.len() + 1 == N,
        "N must be the name length plus the NUL"
    );
    let mut out = [0u16; N];
    let mut i = 0;
    while i < bytes.len() {
        assert!(bytes[i].is_ascii(), "ASCII names only");
        // ASCII byte -> the same UTF-16 code unit.
        out[i] = bytes[i] as u16;
        i += 1;
    }
    out
}

/// Number of UTF-16 code units of `L"DesiredReportType"` including the NUL.
const DESIRED_REPORT_TYPE_NAME_UNITS: usize = DESIRED_REPORT_TYPE_VALUE_NAME.len() + 1;
/// `L"DesiredReportType"`.
static DESIRED_REPORT_TYPE_NAME: [u16; DESIRED_REPORT_TYPE_NAME_UNITS] =
    ascii_to_utf16z(DESIRED_REPORT_TYPE_VALUE_NAME);
/// `DECLARE_CONST_UNICODE_STRING`: `Length` = bytes without the NUL, `MaximumLength` = with it.
const DESIRED_REPORT_TYPE_NAME_LENGTH: u16 = {
    let bytes = (DESIRED_REPORT_TYPE_NAME_UNITS - 1) * 2;
    assert!(bytes + 2 <= u16::MAX as usize);
    bytes as u16
};
/// `sizeof(ULONG)`, the `ValueLength` of the query.
const DESIRED_REPORT_TYPE_QUERY_LEN: ULONG = {
    assert!(DESIRED_REPORT_TYPE_VALUE_LEN == core::mem::size_of::<u32>());
    DESIRED_REPORT_TYPE_VALUE_LEN as ULONG
};

/// PrepareHardware step 4 (Device.c:268-303): `WdfDriverOpenParametersRegistryKey(KEY_READ)` on the
/// driver (`...\Services\AmtPtpDeviceSpiKm\Parameters`, driver-wide), `WdfRegistryQueryValue(
/// "DesiredReportType", 4 bytes)`, `WdfRegistryClose`. Returns the value when the query succeeded.
///
/// # Safety
/// `device` is a live WDFDEVICE; PASSIVE_LEVEL.
unsafe fn query_desired_report_type(device: WDFDEVICE) -> Option<u32> {
    // SAFETY: valid device handle.
    let driver = unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetDriver, device) };
    let mut key: WDFKEY = null_mut();
    // SAFETY: valid driver handle, PASSIVE_LEVEL, no key attributes, valid out-parameter.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverOpenParametersRegistryKey,
            driver,
            PARAMETERS_KEY_ACCESS,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut key
        )
    };
    if !NT_SUCCESS(status) {
        return None;
    }

    let name = UNICODE_STRING {
        Length: DESIRED_REPORT_TYPE_NAME_LENGTH,
        MaximumLength: DESIRED_REPORT_TYPE_NAME_LENGTH + 2,
        // WDF only reads the name (C: DECLARE_CONST_UNICODE_STRING).
        Buffer: DESIRED_REPORT_TYPE_NAME.as_ptr().cast_mut(),
    };
    // Zero-initialized (SPI-B24): a 1-3 byte value leaves defined zero upper bytes.
    let mut value: u32 = 0;
    let mut length: ULONG = 0;
    let mut value_type: ULONG = 0;
    // SAFETY: `key` is the open key; `name` points at a 'static UTF-16 string;
    // `value` is a 4-byte writable buffer matching ValueLength; the two
    // out-parameters are valid.
    let query_status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRegistryQueryValue,
            key,
            &raw const name,
            DESIRED_REPORT_TYPE_QUERY_LEN,
            (&raw mut value).cast(),
            &raw mut length,
            &raw mut value_type
        )
    };
    // SAFETY: `key` was opened above and is closed exactly once.
    unsafe { call_unsafe_wdf_function_binding!(WdfRegistryClose, key) };

    // The registry wrote raw little-endian bytes into `value`; the native byte order of the u32 is
    // those bytes again.
    desired_report_type_value(query_status, value.to_ne_bytes())
}

/// `AmtPtpEvtDeviceD0Entry` (Device.c:319-351): `DeviceStatus = D0ActiveAndUnconfigured` and
/// `LastReportTime` = the QPC frequency (see [`query_performance_counter_as_c`]). No I/O.
unsafe extern "C" fn evt_device_d0_entry(
    device: WDFDEVICE,
    previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    tlg::write_event!(
        PROVIDER,
        "D0Entry",
        level(Informational),
        keyword(keyword::DRIVER),
        i32("PreviousState", &previous_state)
    );
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    context
        .runtime
        .on_d0_entry(query_performance_counter_as_c())
}

/// `AmtPtpEvtDeviceD0Exit` (Device.c:353-395): `DeviceStatus = D3`, then every request still in
/// `HidQueue` is completed with `STATUS_CANCELLED` (Information 0). The recovery timer is not stopped,
/// in-flight lower reads are not cancelled and the trackpad is not switched off (SPI-B12/B14, preserved).
unsafe extern "C" fn evt_device_d0_exit(
    device: WDFDEVICE,
    target_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    let status = context.runtime.on_d0_exit();

    let mut drained: u32 = 0;
    loop {
        let mut request: WDFREQUEST = null_mut();
        // SAFETY: `hid_queue` is the device's manual queue (created in AddDevice).
        let retrieved = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoQueueRetrieveNextRequest,
                context.hid_queue,
                &raw mut request
            )
        };
        if !NT_SUCCESS(retrieved) || request.is_null() {
            // The queue is empty (STATUS_NO_MORE_ENTRIES): expected.
            break;
        }
        // SAFETY: the request was just retrieved from our manual queue, so the
        // driver owns it and nothing else completes it.
        let request = unsafe { Request::from_raw(request, D0_EXIT_DRAIN_STATUS) };
        request.complete(D0_EXIT_DRAIN_STATUS);
        drained = drained.wrapping_add(1);
    }

    tlg::write_event!(
        PROVIDER,
        "D0Exit",
        level(Informational),
        keyword(keyword::DRIVER),
        i32("TargetState", &target_state),
        u32("CancelledReads", &drained)
    );
    status
}

/// `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` (Device.c:397-427), registered for both
/// SelfManagedIoInit and SelfManagedIoRestart; PASSIVE_LEVEL.
///
/// Runs the mode switch. Success: `DeviceStatus = D0ActiveAndConfigured` and `LastReportTime` = the QPC
/// frequency; no lower read is issued for already-queued requests (SPI-B13, preserved). Failure: start the
/// 5 s recovery timer (re-arming it if it is still pending). Always `STATUS_SUCCESS`.
unsafe extern "C" fn evt_device_self_managed_io_init_or_restart(device: WDFDEVICE) -> NTSTATUS {
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: PASSIVE_LEVEL (SelfManagedIoInit/Restart).
    let set_state = unsafe { spi_set_state(context, true) };
    let outcome = context
        .runtime
        .on_self_managed_io_init_or_restart(set_state, query_performance_counter_as_c);
    if let Some(due_time) = outcome.start_recovery_timer {
        tlg::write_event!(
            PROVIDER,
            "SetStateFailedRetryIn5s",
            level(Error),
            keyword(keyword::DRIVER),
            win_ntstatus("Status", &set_state)
        );
        // SAFETY: the timer was created in AddDevice and lives as long as the
        // device. The "already queued" result is ignored, as in C.
        let _was_pending = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfTimerStart,
                context.power_on_recovery_timer,
                due_time
            )
        };
    }
    outcome.status
}

/// `AmtPtpSpiSetState(Device, DesiredState)` (Device.c:457-539): Apple's multitouch mode switch.
///
/// Sends HID `SET_FEATURE` report 2 (`02 01` to enable) as a synchronous internal
/// `IOCTL_HID_SET_FEATURE` with the 255-byte `HID_XFER_PACKET` buffer as **input**, no output, no send
/// options and no timeout (SPI-B23, preserved). The buffer holds a pointer to its own byte 16, so it is
/// filled in place here and stays at this stack address until the synchronous send has returned.
///
/// # Safety
/// PASSIVE_LEVEL (synchronous send). `context` belongs to a live device.
unsafe fn spi_set_state(context: &DeviceContext, desired_state: bool) -> NTSTATUS {
    let mut packet = SetStatePacket::zeroed();
    packet.fill(desired_state);
    let mut input =
        memory_descriptor_init_buffer(packet.as_mut_ptr().cast(), SET_STATE_PACKET_DESCRIPTOR_LEN);
    // SAFETY: PASSIVE_LEVEL (caller contract); valid local I/O target; `input`
    // describes `packet`, which is neither moved nor dropped before this
    // synchronous call returns (its self-pointer stays valid); everything else
    // NULL, as in C.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoTargetSendInternalIoctlSynchronously,
            context.spi_trackpad_io_target,
            null_mut(),
            SET_STATE_IOCTL,
            &raw mut input,
            null_mut(),
            null_mut(),
            null_mut()
        )
    };
    if NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "SetState",
            level(Informational),
            keyword(keyword::DRIVER),
            bool8("Enabled", &desired_state)
        );
    } else {
        tlg::write_event!(
            PROVIDER,
            "SetStateFailed",
            level(Error),
            keyword(keyword::DRIVER),
            bool8("Enabled", &desired_state),
            win_ntstatus("Status", &status)
        );
    }
    status
}

/// `SET_STATE_PACKET_LEN` (255) as the `ULONG` length of a memory descriptor.
const SET_STATE_PACKET_DESCRIPTOR_LEN: ULONG = {
    assert!(SET_STATE_PACKET_LEN <= ULONG::MAX as usize);
    SET_STATE_PACKET_LEN as ULONG
};

/// `AmtPtpPowerRecoveryTimerCallback` (Device.c:541-562): the passive-level timer callback (a framework
/// work item), 5 s after a failed SelfManagedIoInit/Restart.
///
/// Runs the mode switch again. Success: issue exactly one lower read, then
/// `DeviceStatus = D0ActiveAndConfigured` (`LastReportTime` is not refreshed). Failure: nothing, no
/// reschedule. It runs whatever the current power state is, also after D0Exit (SPI-B12, preserved).
unsafe extern "C" fn power_recovery_timer_callback(timer: WDFTIMER) {
    // SAFETY: `timer` is the framework's timer handle for this callback.
    let device: WDFDEVICE =
        unsafe { call_unsafe_wdf_function_binding!(WdfTimerGetParentObject, timer) }.cast();
    // SAFETY: the timer's parent is the device it was created for (AddDevice);
    // the callback runs at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return;
    };
    // SAFETY: passive-level timer callback.
    let status = unsafe { spi_set_state(context, true) };
    let issued = context.runtime.on_recovery_timer(status, || {
        // SAFETY: `device` is the live parent of the timer and `context` its context; IRQL <= DISPATCH.
        unsafe { input::spi_input_issue_request(device, context) }
    });
    tlg::write_event!(
        PROVIDER,
        "PowerRecoveryTimer",
        level(Informational),
        keyword(keyword::DRIVER),
        win_ntstatus("Status", &status),
        bool8("ReadIssued", &issued)
    );
}

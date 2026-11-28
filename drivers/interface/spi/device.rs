//! The device context, device creation, the PnP/power callbacks, the mode switch and the power-on
//! recovery timer (C: Device.c).

use core::ptr::null_mut;

use amtptp_core::spi::device::{
    DESIRED_REPORT_TYPE_VALUE_LEN, DESIRED_REPORT_TYPE_VALUE_NAME, PARAMETERS_KEY_ACCESS,
    SharedDeviceConfig, desired_report_type_value,
};
use amtptp_core::spi::runtime::{
    ContactTracker, D0_EXIT_DRAIN_STATUS, DeviceRuntime, MAX_IN_FLIGHT_READS,
    RECOVERY_TIMER_PERIOD, ReadTracker, SuspendedReads,
};
use amtptp_core::spi::wire::{
    LOWER_ATTRIBUTES_IOCTL, LOWER_ATTRIBUTES_LEN, LOWER_READ_BUFFER_LEN, NO_IO_TARGET_STATUS,
    POOL_TAG, SET_STATE_IOCTL, SET_STATE_PACKET_LEN, with_set_state_packet,
};
use tracelogging as tlg;
use wdf_support::declare_context_type;
use wdf_support::handle::WdfHandle;
use wdf_support::init::{
    memory_descriptor_init_buffer, object_attributes_init, object_attributes_init_context_type,
    pnppower_event_callbacks_init, sync_send_options, timer_config_init,
};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE, STATUS_SUCCESS};
use wdf_support::request::Request;
use wdf_support::sync::SpinLocked;
use wdf_support::time::query_performance_frequency;
use wdk_sys::{
    _POOL_TYPE, _WDF_EXECUTION_LEVEL, NTSTATUS, PWDFDEVICE_INIT, ULONG, UNICODE_STRING,
    WDF_NO_OBJECT_ATTRIBUTES, WDF_POWER_DEVICE_STATE, WDFCMRESLIST, WDFDEVICE, WDFIOTARGET__,
    WDFKEY, WDFLOOKASIDE, WDFLOOKASIDE__, WDFQUEUE, WDFQUEUE__, WDFREQUEST, WDFREQUEST__, WDFTIMER,
    WDFTIMER__, call_unsafe_wdf_function_binding,
};

use crate::{PROVIDER, input, keyword, queue};

/// A lower read request as the read tracker stores it.
pub(crate) type LowerRead = WdfHandle<WDFREQUEST__>;

/// Port of the C `DEVICE_CONTEXT` (Device.h:56-84). Named `DEVICE_CONTEXT` for `!wdfkd`.
///
/// The four handles and the read lock are created exactly once, inside [`create_device`], before the
/// device can receive any request or PnP/power callback; afterwards the context is only accessed through
/// shared references. All state that changes later is atomic ([`SharedDeviceConfig`], [`DeviceRuntime`])
/// or behind the read lock (`reads`), so concurrent callbacks at up to `DISPATCH_LEVEL` never race on
/// plain fields (P2 for SPI-B15). The handles are [`WdfHandle`]s, so the context is `Sync`, as
/// `declare_context_type!` requires.
///
/// Dropped C fields (dead): `SpiDevice`, `InputPollThreadHandle`. `HidVendorID`, `HidProductID`,
/// `HidVersionNumber`, `TrackpadInfo` and `ReportType` are in `config`; `DeviceStatus`, the reporting clock,
/// `PtpInputOn`, `PtpReportButton` and `PtpReportTouch` are in `runtime`.
pub(crate) struct DeviceContext {
    /// `SpiTrackpadIoTarget`: the local (default) I/O target, i.e. the Apple SPI PDO below us.
    pub(crate) spi_trackpad_io_target: WdfHandle<WDFIOTARGET__>,
    /// `HidQueue`: the manual queue holding the pending upstream `IOCTL_HID_READ_REPORT` requests.
    pub(crate) hid_queue: WdfHandle<WDFQUEUE__>,
    /// `PowerOnRecoveryTimer`: the 5 s one-shot passive-level retry timer.
    pub(crate) power_on_recovery_timer: WdfHandle<WDFTIMER__>,
    /// `HidReadBufferLookaside`: 1024-byte `NonPagedPoolNx` buffers for the lower reads.
    pub(crate) hid_read_buffer_lookaside: WdfHandle<WDFLOOKASIDE__>,
    /// What `PrepareHardware` found out about the device.
    pub(crate) config: SharedDeviceConfig,
    /// `DeviceStatus`, the reporting clock and the PTP feature flags.
    pub(crate) runtime: DeviceRuntime,
    /// The lower reads in flight, the current epoch, and the active contact tracker (see
    /// [`amtptp_core::spi::runtime`]). Every change of `DeviceStatus`, every claim of a lower
    /// read, and every contact-state update happens under this lock.
    reads: SpinLocked<(ReadTracker<LowerRead>, ContactTracker)>,
}

impl DeviceContext {
    /// The state right after `WdfDeviceCreate`: null handles, the zero configuration and
    /// `DeviceStatus = D3` (C: zero-filled context, then `DeviceStatus = D3` at Device.c:134).
    const fn new() -> Self {
        Self {
            spi_trackpad_io_target: WdfHandle::NULL,
            hid_queue: WdfHandle::NULL,
            power_on_recovery_timer: WdfHandle::NULL,
            hid_read_buffer_lookaside: WdfHandle::NULL,
            config: SharedDeviceConfig::new(),
            runtime: DeviceRuntime::new(),
            reads: SpinLocked::new((ReadTracker::new(), ContactTracker::new())),
        }
    }

    /// Runs `f` with the runtime and the read tracker while holding the read lock. `f` runs at
    /// `DISPATCH_LEVEL` and must not send, cancel or complete a request.
    ///
    /// # Safety
    /// The context was set up by [`create_device`] (so the lock exists); IRQL <= `DISPATCH_LEVEL`.
    pub(crate) unsafe fn with_reads<R, F>(&self, f: F) -> R
    where
        F: FnOnce(&DeviceRuntime, &mut ReadTracker<LowerRead>) -> R,
    {
        // SAFETY: the lock was created in `initialize_device` (caller contract); `f` only touches the
        // runtime and the tracker.
        unsafe { self.reads.with(|(reads, _)| f(&self.runtime, reads)) }
    }

    /// Runs `f` with the runtime and the contact tracker while holding the read lock. `f` runs at
    /// `DISPATCH_LEVEL` and must not send, cancel or complete a request.
    ///
    /// # Safety
    /// The context was set up by [`create_device`] (so the lock exists); IRQL <= `DISPATCH_LEVEL`.
    pub(crate) unsafe fn with_contacts<R, F>(&self, f: F) -> R
    where
        F: FnOnce(&DeviceRuntime, &mut ContactTracker) -> R,
    {
        // SAFETY: the lock was created in `initialize_device` (caller contract); `f` only touches the
        // runtime and the contact tracker.
        unsafe { self.reads.with(|(_, contacts)| f(&self.runtime, contacts)) }
    }
}

declare_context_type!(pub(crate) DeviceContext, DEVICE_CONTEXT_TYPE_INFO, device_get_context, name = "DEVICE_CONTEXT");

/// `DeviceGetContext(Device)` as a shared reference.
///
/// Returns `None` only if the object has no `DEVICE_CONTEXT`, which cannot happen for the devices this
/// driver creates (C checked it in `PrepareHardware` and `SetState` only; the port checks it everywhere
/// instead of dereferencing NULL).
///
/// # Safety
/// `device` is a live WDFDEVICE created by [`create_device`] (so its context was initialized there), and
/// the caller runs at IRQL <= `DISPATCH_LEVEL`.
pub(crate) unsafe fn device_context<'a>(device: WDFDEVICE) -> Option<&'a DeviceContext> {
    // SAFETY: valid live device handle (caller contract).
    let context = unsafe { device_get_context(device) };
    // SAFETY: a non-null context of this type was initialized with `write` in
    // `create_device` before the device was published, is never mutated through
    // a `&mut` afterwards, and lives as long as the device.
    unsafe { context.as_ref() }
}

/// `AmtPtpDeviceSpiKmCreateDevice` (Device.c:24-164), `PASSIVE_LEVEL` (`PAGE`, as in C).
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
/// `device_init` is the unconsumed `WDFDEVICE_INIT` of the current `EvtDriverDeviceAdd` call.
#[unsafe(link_section = "PAGE")]
pub(crate) unsafe fn create_device(mut device_init: PWDFDEVICE_INIT) -> NTSTATUS {
    wdk::paged_code!();

    let mut pnp_power_callbacks = pnppower_event_callbacks_init();
    pnp_power_callbacks.EvtDevicePrepareHardware = Some(evt_device_prepare_hardware);
    pnp_power_callbacks.EvtDeviceD0Entry = Some(evt_device_d0_entry);
    pnp_power_callbacks.EvtDeviceD0Exit = Some(evt_device_d0_exit);
    pnp_power_callbacks.EvtDeviceSelfManagedIoInit =
        Some(evt_device_self_managed_io_init_or_restart);
    pnp_power_callbacks.EvtDeviceSelfManagedIoRestart =
        Some(evt_device_self_managed_io_init_or_restart);
    pnp_power_callbacks.EvtDeviceSelfManagedIoSuspend = Some(evt_device_self_managed_io_suspend);
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
            (&raw mut (*context).hid_read_buffer_lookaside).cast::<WDFLOOKASIDE>()
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
            (&raw mut (*context).power_on_recovery_timer).cast::<WDFTIMER>()
        )
    };
    if !NT_SUCCESS(status) {
        trace_create_device_failure(b"WdfTimerCreate", status);
        return status;
    }

    // The read lock (parent = the device). Not in C: see `DeviceContext::reads`.
    // SAFETY: PASSIVE_LEVEL; the context is not reachable by any callback yet, and the device outlives
    // the lock (its child).
    let status = unsafe { (*context).reads.create(device.cast()) };
    if !NT_SUCCESS(status) {
        trace_create_device_failure(b"WdfSpinLockCreate", status);
        return status;
    }

    // SAFETY: valid device handle.
    let io_target = unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetIoTarget, device) };
    // SAFETY: writable per the function contract.
    unsafe { (*context).spi_trackpad_io_target = WdfHandle::new(io_target) };
    if io_target.is_null() {
        trace_create_device_failure(b"WdfDeviceGetIoTarget", NO_IO_TARGET_STATUS);
        return NO_IO_TARGET_STATUS;
    }

    // DeviceStatus = D3: already set by DeviceContext::new().

    // SAFETY: valid device; `hid_queue` is writable per the function contract.
    unsafe { queue::queue_initialize(device, (&raw mut (*context).hid_queue).cast::<WDFQUEUE>()) }
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

/// `AmtPtpEvtDevicePrepareHardware` (Device.c:166-317), `PASSIVE_LEVEL` (non-paged, as in C).
///
/// 1. Synchronous lower `IOCTL_HID_GET_DEVICE_ATTRIBUTES` into a zeroed 32-byte buffer (no timeout:
///    SPI-B23, preserved). Failure returns that status.
/// 2. Store VID/PID/version, then look the device up in the config table: no match returns
///    `STATUS_NOT_FOUND` (the IDs stay stored, as in C).
/// 3. Turn both selective-reporting switches on (not in C, which never used them).
/// 4. Read `DesiredReportType` from the driver's Parameters key (zero-initialized value buffer: SPI-B24).
/// 5. Return `STATUS_SUCCESS` whatever the registry step did.
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
    // Not in C (no options): bound the wait for a lower driver that never completes (B-3).
    let mut options = sync_send_options(0);
    // SAFETY: PASSIVE_LEVEL; valid local I/O target; `output` describes the
    // 32-byte local buffer, which outlives this synchronous call (also on a
    // timeout: the framework waits for the cancelled request); no request,
    // input or bytes-returned (NULL, as in C).
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoTargetSendInternalIoctlSynchronously,
            context.spi_trackpad_io_target.get(),
            null_mut(),
            LOWER_ATTRIBUTES_IOCTL,
            null_mut(),
            &raw mut output,
            &raw mut options,
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

    // Selective reporting is not kept across a (re)start: report contacts and the button again.
    context.runtime.ptp_settings().reset_selective_reporting();
    // SAFETY: live device context at PASSIVE_LEVEL.
    unsafe { context.with_contacts(|_, contacts| contacts.reset()) };

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
const fn ascii_to_utf16z<const N: usize>(ascii_name: &str) -> [u16; N] {
    let mut bytes = ascii_name.as_bytes();
    assert!(
        bytes.len() + 1 == N,
        "N must be the name length plus the NUL"
    );
    let mut out = [0_u16; N];
    let mut units = out.as_mut_slice();
    while let (Some((&byte, rest)), Some((unit, tail))) =
        (bytes.split_first(), units.split_first_mut())
    {
        assert!(byte.is_ascii(), "ASCII names only");
        // ASCII byte -> the same UTF-16 code unit.
        *unit = byte as u16;
        bytes = rest;
        units = tail;
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

/// `PrepareHardware` step 4 (Device.c:268-303): `WdfDriverOpenParametersRegistryKey(KEY_READ)` on the
/// driver (`...\Services\AmtPtpDeviceSpiKm\Parameters`, driver-wide), `WdfRegistryQueryValue(
/// "DesiredReportType", 4 bytes)`, `WdfRegistryClose`. Returns the value when the query succeeded.
///
/// # Safety
/// `device` is a live WDFDEVICE; `PASSIVE_LEVEL`.
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
    desired_report_type_value(query_status, value.to_le_bytes())
}

/// `AmtPtpEvtDeviceD0Entry` (Device.c:319-351): `DeviceStatus = D0ActiveAndUnconfigured` and
/// initializes the reporting clock with the cached QPC frequency. No device I/O.
unsafe extern "C" fn evt_device_d0_entry(
    device: WDFDEVICE,
    previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    // SAFETY: WDF supplies the live device to this D0 entry callback.
    let system_power_action =
        unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetSystemPowerAction, device) };
    tlg::write_event!(
        PROVIDER,
        "D0Entry",
        level(Informational),
        keyword(keyword::DRIVER),
        i32("PreviousState", &previous_state),
        i32("SystemPowerAction", &system_power_action)
    );
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: live device context at PASSIVE_LEVEL.
    unsafe { context.with_contacts(|_, contacts| contacts.reset()) };
    context.runtime.on_d0_entry(query_performance_frequency())
}

/// `EvtDeviceSelfManagedIoSuspend` (not in C): runs before every `D0Exit` (power-down, resource
/// rebalance, removal and surprise removal).
///
/// 1. `DeviceStatus = D3` and a new epoch, under the read lock: no lower read is sent from now on, and
///    the completion of every read sent before is stale (it leaves `HidQueue` alone).
/// 2. `WdfTimerStop(Wait = FALSE)`: a pending recovery timer does not fire. A callback that is already
///    running finds the device suspended and does nothing. It is not waited for, because its mode switch
///    has no timeout.
/// 3. `WdfRequestCancelSentRequest` on every read that was in flight. Their completions are not waited
///    for either; they are dropped whenever they arrive.
///
/// Together with the epoch check in the completion routine this fixes SPI-B12/B14 (P3).
unsafe extern "C" fn evt_device_self_managed_io_suspend(device: WDFDEVICE) -> NTSTATUS {
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: the context belongs to a live device; PASSIVE_LEVEL.
    let cancelled = unsafe { cancel_in_flight_reads(context, DeviceRuntime::suspend) };
    // SAFETY: the timer was created in AddDevice and lives as long as the device; Wait = FALSE is
    // allowed at any IRQL <= DISPATCH_LEVEL.
    let was_queued = unsafe {
        call_unsafe_wdf_function_binding!(WdfTimerStop, context.power_on_recovery_timer.get(), 0)
    };
    tlg::write_event!(
        PROVIDER,
        "SelfManagedIoSuspend",
        level(Informational),
        keyword(keyword::DRIVER),
        u32("CancelledLowerReads", &cancelled),
        bool8("TimerWasQueued", &(was_queued != 0))
    );
    STATUS_SUCCESS
}

/// Suspends the read pipeline with `suspend` (under the read lock), then cancels every read it handed
/// out. Returns how many reads were cancelled.
///
/// Each read is referenced under the lock: it is still registered there, so its completion routine has
/// not deleted it yet (the routine unregisters it under the same lock first). The reference keeps the
/// handle valid while it is cancelled outside the lock (cancelling can run the completion routine
/// synchronously, which takes the lock). A read that was claimed but not sent yet only gets its IRP's
/// cancel flag set; it is stale anyway.
///
/// # Safety
/// `context` belongs to a live device set up by [`create_device`]; IRQL <= `DISPATCH_LEVEL`.
unsafe fn cancel_in_flight_reads<F>(context: &DeviceContext, suspend: F) -> u32
where
    F: FnOnce(&DeviceRuntime, &mut ReadTracker<LowerRead>) -> SuspendedReads<LowerRead>,
{
    let mut held: [WDFREQUEST; MAX_IN_FLIGHT_READS] = [null_mut(); MAX_IN_FLIGHT_READS];
    let take = |(reads, contacts): &mut (ReadTracker<LowerRead>, ContactTracker)| {
        contacts.reset();
        let suspended = suspend(&context.runtime, reads);
        let mut count = 0_usize;
        for (slot, read) in held.iter_mut().zip(suspended.iter()) {
            // SAFETY: a registered lower read is alive (see above); NULL tag, as WdfObjectReference.
            unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfObjectReferenceActual,
                    read.get().cast(),
                    null_mut(),
                    0,
                    core::ptr::null()
                );
            }
            *slot = read.get();
            count += 1;
        }
        count
    };
    // SAFETY: caller contract; `take` only resets contacts and references objects (no callback can run from it).
    let count = unsafe { context.reads.with(take) };
    for &read in held.iter().take(count) {
        // SAFETY: referenced above, so the handle is valid even if the read has completed and been
        // deleted meanwhile; IRQL <= DISPATCH_LEVEL.
        unsafe {
            let _cancelled = call_unsafe_wdf_function_binding!(WdfRequestCancelSentRequest, read);
            call_unsafe_wdf_function_binding!(
                WdfObjectDereferenceActual,
                read.cast(),
                null_mut(),
                0,
                core::ptr::null()
            );
        }
    }
    // At most MAX_IN_FLIGHT_READS (32).
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// `AmtPtpEvtDeviceD0Exit` (Device.c:353-395): `DeviceStatus = D3` (again: `SelfManagedIoSuspend` already
/// suspended the read pipeline, so normally no read is left to cancel), then every request still in
/// `HidQueue` is completed with `STATUS_CANCELLED` (Information 0). The trackpad is not switched off
/// (SPI-B14, preserved).
unsafe extern "C" fn evt_device_d0_exit(
    device: WDFDEVICE,
    target_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    // SAFETY: WDF supplies the live device to this D0 exit callback.
    let system_power_action =
        unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetSystemPowerAction, device) };
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    let mut status = STATUS_SUCCESS;
    // SAFETY: the context belongs to a live device; PASSIVE_LEVEL.
    let _cancelled = unsafe {
        cancel_in_flight_reads(context, |runtime, reads| {
            let (d0_exit_status, suspended) = runtime.on_d0_exit(reads);
            status = d0_exit_status;
            suspended
        })
    };

    let mut drained: u32 = 0;
    loop {
        let mut request: WDFREQUEST = null_mut();
        // SAFETY: `hid_queue` is the device's manual queue (created in AddDevice).
        let retrieved = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoQueueRetrieveNextRequest,
                context.hid_queue.get(),
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
        i32("SystemPowerAction", &system_power_action),
        u32("CancelledReads", &drained)
    );
    status
}

/// `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` (Device.c:397-427), registered for both
/// `SelfManagedIoInit` and `SelfManagedIoRestart`; `PASSIVE_LEVEL`.
///
/// Runs the mode switch. Success: `DeviceStatus = D0ActiveAndConfigured`, a new reporting session, and
/// one lower read for every upstream read already waiting in `HidQueue` (P3 fix of SPI-B13). Failure:
/// start the 5 s recovery timer. Always `STATUS_SUCCESS`.
unsafe extern "C" fn evt_device_self_managed_io_init_or_restart(device: WDFDEVICE) -> NTSTATUS {
    // SAFETY: framework callback for one of our devices, at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return STATUS_INVALID_DEVICE_STATE;
    };
    // SAFETY: PASSIVE_LEVEL (SelfManagedIoInit/Restart).
    let set_state = unsafe { spi_set_state(context, true) };
    // SAFETY: the context belongs to a live device; PASSIVE_LEVEL.
    let outcome = unsafe {
        context.with_reads(|runtime, reads| {
            runtime.on_self_managed_io_init_or_restart(
                reads,
                set_state,
                query_performance_frequency,
            )
        })
    };
    if outcome.configured {
        // SAFETY: `device` is live and `context` its context; PASSIVE_LEVEL.
        let issued = unsafe { input::issue_backlog_reads(device, context) };
        trace_configured(b"SelfManagedIo", issued);
    }
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
                context.power_on_recovery_timer.get(),
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
/// `PASSIVE_LEVEL` (synchronous send). `context` belongs to a live device.
unsafe fn spi_set_state(context: &DeviceContext, desired_state: bool) -> NTSTATUS {
    let status = with_set_state_packet(desired_state, |packet| {
        let mut input = memory_descriptor_init_buffer(
            packet.as_ptr().cast_mut().cast(),
            SET_STATE_PACKET_DESCRIPTOR_LEN,
        );
        // Not in C (no options): bound the wait for a lower driver that never completes (B-3).
        let mut options = sync_send_options(0);
        // SAFETY: PASSIVE_LEVEL (caller contract); valid local I/O target; `input`
        // describes `packet`, which is neither moved nor dropped before this
        // synchronous call returns (its self-pointer stays valid; on a timeout the
        // framework waits for the cancelled request); request, output and
        // bytes-returned NULL, as in C.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetSendInternalIoctlSynchronously,
                context.spi_trackpad_io_target.get(),
                null_mut(),
                SET_STATE_IOCTL,
                &raw mut input,
                null_mut(),
                &raw mut options,
                null_mut()
            )
        }
    })
    .unwrap_or(wdf_support::ntstatus::STATUS_INVALID_DEVICE_STATE);
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
/// Only while the device is in D0 and still waiting for the mode switch (P3 fix of SPI-B12: the C
/// callback also ran in D3 and after a successful restart): run the mode switch again. Success, unless
/// the device was suspended meanwhile: `DeviceStatus = D0ActiveAndConfigured`, a new reporting session,
/// and one lower read for every queued upstream read. Failure: nothing, no reschedule.
unsafe extern "C" fn power_recovery_timer_callback(timer: WDFTIMER) {
    // SAFETY: `timer` is the framework's timer handle for this callback.
    let device: WDFDEVICE =
        unsafe { call_unsafe_wdf_function_binding!(WdfTimerGetParentObject, timer) }.cast();
    // SAFETY: the timer's parent is the device it was created for (AddDevice);
    // the callback runs at PASSIVE_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        return;
    };
    // SAFETY: the context belongs to a live device; PASSIVE_LEVEL.
    let Some(ticket) =
        (unsafe { context.with_reads(|runtime, reads| runtime.begin_recovery(reads)) })
    else {
        tlg::write_event!(
            PROVIDER,
            "PowerRecoveryTimerSkipped",
            level(Informational),
            keyword(keyword::DRIVER),
            u32("DeviceStatus", &(context.runtime.device_status() as u32))
        );
        return;
    };
    // SAFETY: passive-level timer callback.
    let status = unsafe { spi_set_state(context, true) };
    // SAFETY: as above.
    let configured = unsafe {
        context.with_reads(|runtime, reads| {
            runtime.finish_recovery(reads, ticket, status, query_performance_frequency)
        })
    };
    let issued = if configured {
        // SAFETY: `device` is the live parent of the timer and `context` its context; PASSIVE_LEVEL.
        unsafe { input::issue_backlog_reads(device, context) }
    } else {
        0
    };
    tlg::write_event!(
        PROVIDER,
        "PowerRecoveryTimer",
        level(Informational),
        keyword(keyword::DRIVER),
        win_ntstatus("Status", &status),
        bool8("Configured", &configured),
        u32("ReadsIssued", &issued)
    );
}

/// Trace for a transition to configured and the backlog reads issued for it.
fn trace_configured(source: &[u8], issued: u32) {
    tlg::write_event!(
        PROVIDER,
        "Configured",
        level(Informational),
        keyword(keyword::DRIVER),
        str8("Source", source),
        u32("BacklogReadsIssued", &issued)
    );
}

//! Device context, filter creation, and WDF object initialization.

use core::ptr::null_mut;

use amtptp_core::hid::delivery::CompletionQueue;
use amtptp_core::spi::device::SharedDeviceConfig;
use amtptp_core::spi::runtime::{
    ContactTracker, DeviceRuntime, RECOVERY_TIMER_PERIOD, ReadTracker, ReplenishAction,
};
use amtptp_core::spi::wire::{LOWER_READ_BUFFER_LEN, NO_IO_TARGET_STATUS, POOL_TAG};
use tracelogging as tlg;
use wdf_support::declare_context_type;
use wdf_support::handle::WdfHandle;
use wdf_support::init::{
    object_attributes_init, object_attributes_init_context_type, pnppower_event_callbacks_init,
    timer_config_init,
};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE};
use wdf_support::request::Request;
use wdf_support::sync::SpinLocked;
use wdk_sys::{
    _POOL_TYPE, _WDF_DEVICE_FAILED_ACTION, _WDF_EXECUTION_LEVEL, NTSTATUS, PWDFDEVICE_INIT,
    WDF_NO_OBJECT_ATTRIBUTES, WDFDEVICE, WDFIOTARGET__, WDFLOOKASIDE, WDFLOOKASIDE__, WDFQUEUE,
    WDFQUEUE__, WDFREQUEST__, WDFTIMER, WDFTIMER__, call_unsafe_wdf_function_binding,
};

use crate::{PROVIDER, keyword, queue};

mod identify;
mod power;

use identify::evt_device_prepare_hardware;
use power::{
    evt_device_d0_entry, evt_device_d0_exit, evt_device_self_managed_io_init_or_restart,
    evt_device_self_managed_io_suspend, power_recovery_timer_callback, read_retry_timer_callback,
};

/// A lower read request as the read tracker stores it.
pub(crate) type LowerRead = WdfHandle<WDFREQUEST__>;

/// Guarded pipeline state: lower reads, contact tracker, and serialized upper completions.
pub(crate) type SpiPipeline = (
    ReadTracker<LowerRead>,
    ContactTracker,
    CompletionQueue<(Request, NTSTATUS)>,
);

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
    /// One-shot passive-level backoff timer for replenishing failed lower-read issues/completions.
    pub(crate) read_retry_timer: WdfHandle<WDFTIMER__>,
    /// `HidReadBufferLookaside`: 1024-byte `NonPagedPoolNx` buffers for the lower reads.
    pub(crate) hid_read_buffer_lookaside: WdfHandle<WDFLOOKASIDE__>,
    /// What `PrepareHardware` found out about the device.
    pub(crate) config: SharedDeviceConfig,
    /// `DeviceStatus`, the reporting clock and the PTP feature flags.
    pub(crate) runtime: DeviceRuntime,
    /// Lower reads, active contact tracker, and FIFO completion serializer.
    pub(crate) reads: SpinLocked<SpiPipeline>,
}

impl DeviceContext {
    /// The state right after `WdfDeviceCreate`: null handles, the zero configuration and
    /// `DeviceStatus = D3` (C: zero-filled context, then `DeviceStatus = D3` at Device.c:134).
    const fn new() -> Self {
        Self {
            spi_trackpad_io_target: WdfHandle::NULL,
            hid_queue: WdfHandle::NULL,
            power_on_recovery_timer: WdfHandle::NULL,
            read_retry_timer: WdfHandle::NULL,
            hid_read_buffer_lookaside: WdfHandle::NULL,
            config: SharedDeviceConfig::new(),
            runtime: DeviceRuntime::new(),
            reads: SpinLocked::new((
                ReadTracker::new(),
                ContactTracker::new(),
                CompletionQueue::new(),
            )),
        }
    }

    /// Runs `f` with the runtime and the read tracker while holding the read lock.
    ///
    /// # Safety
    /// The context was set up by [`create_device`] (so the lock exists); IRQL <= `DISPATCH_LEVEL`.
    pub(crate) unsafe fn with_reads<R, F>(&self, f: F) -> R
    where
        F: FnOnce(&DeviceRuntime, &mut ReadTracker<LowerRead>) -> R,
    {
        // SAFETY: the lock was created in `initialize_device` (caller contract).
        unsafe { self.reads.with(|(reads, _, _)| f(&self.runtime, reads)) }
    }

    /// Runs `f` with the runtime and the contact tracker while holding the read lock.
    ///
    /// # Safety
    /// The context was set up by [`create_device`] (so the lock exists); IRQL <= `DISPATCH_LEVEL`.
    pub(crate) unsafe fn with_contacts<R, F>(&self, f: F) -> R
    where
        F: FnOnce(&DeviceRuntime, &mut ContactTracker) -> R,
    {
        // SAFETY: the lock was created in `initialize_device` (caller contract).
        unsafe {
            self.reads
                .with(|(_, contacts, _)| f(&self.runtime, contacts))
        }
    }

    /// Handles a lower-read issue or completion failure by arming `read_retry_timer` or escalating
    /// to `WdfDeviceSetFailed(AttemptRestart)`.
    ///
    /// # Safety
    /// `device` owns `self`; IRQL <= `DISPATCH_LEVEL`.
    pub(crate) unsafe fn handle_read_failure(&self, device: WDFDEVICE) {
        match self.runtime.note_lower_read_failure() {
            ReplenishAction::None => {}
            ReplenishAction::Retry(due_time) => {
                // SAFETY: `read_retry_timer` was created in `initialize_device`; callable <= DISPATCH.
                let _was_pending = unsafe {
                    call_unsafe_wdf_function_binding!(
                        WdfTimerStart,
                        self.read_retry_timer.get(),
                        due_time
                    )
                };
            }
            ReplenishAction::FailDevice => {
                tlg::write_event!(
                    PROVIDER,
                    "ReadReplenishEscalated",
                    level(Error),
                    keyword(keyword::DRIVER)
                );
                // SAFETY: `device` is live; `WdfDeviceSetFailed` is callable <= DISPATCH_LEVEL.
                unsafe {
                    call_unsafe_wdf_function_binding!(
                        WdfDeviceSetFailed,
                        device,
                        _WDF_DEVICE_FAILED_ACTION::WdfDeviceFailedAttemptRestart
                    );
                }
            }
        }
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

    let mut retry_config = timer_config_init(Some(read_retry_timer_callback));
    retry_config.Period = RECOVERY_TIMER_PERIOD;
    retry_config.AutomaticSerialization = 0;
    let mut retry_attributes = object_attributes_init();
    retry_attributes.ParentObject = device.cast();
    retry_attributes.ExecutionLevel = _WDF_EXECUTION_LEVEL::WdfExecutionLevelPassive;
    // SAFETY: initialized config/attributes; writable context field before publication.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfTimerCreate,
            &raw mut retry_config,
            &raw mut retry_attributes,
            (&raw mut (*context).read_retry_timer).cast::<WDFTIMER>()
        )
    };
    if !NT_SUCCESS(status) {
        trace_create_device_failure(b"WdfTimerCreate(ReadRetry)", status);
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

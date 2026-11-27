//! `AmtPtpBthFilter`: KMDF lower filter on the Magic Trackpad 2's Bluetooth HID node
//! (`BTHENUM\{00001124-...}_VID&0001004c_PID&0265`), below `HidBth`.
//!
//! The trackpad's SDP HID descriptor declares neither its multitouch report `0x31` nor its
//! mode-switch feature `0xF1`, so HIDCLASS drops the reports. This filter makes HIDCLASS deliver
//! them itself (see [`amtptp_hidfilter_core::bth`]):
//!
//! - [`sdp`]: `HidBth`'s SDP attribute search at start is answered with the vendor collection
//!   appended to the report descriptor. HIDCLASS then exposes it as `Col03`.
//! - [`interrupt`]: each `0x31` report on the HID interrupt channel is padded in place to the length
//!   declared for it, with its real length in the last byte, because HIDCLASS drops shorter reports.
//!
//! Every other request passes through unchanged (send-and-forget). The collection filter
//! (`AmtPtpHidFilter`) reads `Col03` like any HID client, so no reader competes with HIDCLASS.
//!
//! Tracing: `TraceLogging` provider `AmtPtpBthFilterTraceProvider`
//! (`849adcb8-3936-417a-a967-0f281ec3c07f`). Keywords: 0x1 driver/device, 0x2 SDP, 0x4 interrupt
//! channel; per-report events are `Verbose`.
#![no_std]

extern crate wdk_panic; // #[panic_handler]: a panic is a bugcheck (0x52504E43).

mod brb;
mod interrupt;
mod sdp;

use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tracelogging as tlg;
use wdf_support::declare_context_type;
use wdf_support::init::{
    driver_config_init, io_queue_config_init_default_queue, object_attributes_init,
    object_attributes_init_context_type, request_parameters_init,
};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_SUCCESS};
use wdf_support::request::Request;
use wdk_sys::{
    _WDF_IO_QUEUE_DISPATCH_TYPE, _WDF_TRI_STATE, NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT,
    PWDF_REQUEST_COMPLETION_PARAMS, PWDFDEVICE_INIT, ULONG, WDF_NO_HANDLE, WDF_NO_OBJECT_ATTRIBUTES,
    WDFCONTEXT, WDFDEVICE, WDFDRIVER, WDFIOTARGET, WDFOBJECT, WDFQUEUE, WDFREQUEST,
    call_unsafe_wdf_function_binding,
};

use crate::brb::{Brb, IOCTL_SUBMIT_BRB};

tlg::define_provider!(
    PROVIDER,
    "AmtPtpBthFilterTraceProvider",
    id("849adcb8-3936-417a-a967-0f281ec3c07f")
);

/// Keyword of driver and device events.
const KW_DEVICE: u64 = 0x1;
/// Keyword of SDP events.
const KW_SDP: u64 = 0x2;
/// Keyword of interrupt-channel events.
const KW_INTERRUPT: u64 = 0x4;

/// Number of interrupt channels tracked at once (one per connection; a reconnect opens a new one
/// before the old close is seen at worst).
const CHANNEL_SLOTS: usize = 4;

/// Device context: the open HID interrupt channels (`ChannelHandle` values, 0 = free slot).
pub(crate) struct DeviceContext {
    interrupt_channels: [AtomicU64; CHANNEL_SLOTS],
    /// The slot to overwrite when every slot is taken (a channel whose close was never seen).
    next_eviction: AtomicUsize,
}

declare_context_type!(pub(crate) DeviceContext, DEVICE_CONTEXT_TYPE_INFO, device_get_context, name = "AMTPTP_BTH_DEVICE_CONTEXT");

impl DeviceContext {
    /// Records an opened interrupt channel. When every slot is taken (closes that were never
    /// seen), it overwrites the slots in turn and returns `false`.
    pub(crate) fn add_interrupt_channel(&self, channel: u64) -> bool {
        if channel == 0 || self.is_interrupt_channel(channel) {
            return true;
        }
        let stored = self.interrupt_channels.iter().any(|slot| {
            slot.compare_exchange(0, channel, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        });
        if !stored {
            let victim = self.next_eviction.fetch_add(1, Ordering::Relaxed) % CHANNEL_SLOTS;
            if let Some(slot) = self.interrupt_channels.get(victim) {
                slot.store(channel, Ordering::Release);
            }
        }
        stored
    }

    /// Forgets a closed channel (no effect for other channels).
    pub(crate) fn remove_channel(&self, channel: u64) -> bool {
        channel != 0
            && self.interrupt_channels.iter().any(|slot| {
                slot.compare_exchange(channel, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
    }

    /// Whether `channel` is an open interrupt channel.
    pub(crate) fn is_interrupt_channel(&self, channel: u64) -> bool {
        channel != 0
            && self
                .interrupt_channels
                .iter()
                .any(|slot| slot.load(Ordering::Acquire) == channel)
    }
}

/// `DriverEntry`, called by `FxDriverEntry` once the KMDF runtime is bound.
///
/// # Safety
/// Called once by the I/O manager with a valid driver object and registry path.
#[unsafe(export_name = "DriverEntry")]
#[unsafe(link_section = "INIT")]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    // SAFETY: DriverEntry runs once per image load; the provider is unregistered on the failure
    // path below or by the driver-object cleanup callback.
    unsafe { PROVIDER.register() };
    let mut attributes = object_attributes_init();
    attributes.EvtCleanupCallback = Some(evt_driver_cleanup);
    let mut config = driver_config_init(Some(evt_device_add));
    // SAFETY: the I/O manager's arguments; initialized attributes and config; no handle returned.
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
        PROVIDER.unregister();
        return status;
    }
    tlg::write_event!(
        PROVIDER,
        "DriverInitialized",
        level(Informational),
        keyword(KW_DEVICE)
    );
    STATUS_SUCCESS
}

/// Driver-object cleanup: unregisters the provider before the image unloads.
unsafe extern "C" fn evt_driver_cleanup(_driver: WDFOBJECT) {
    tlg::write_event!(
        PROVIDER,
        "DriverContextCleanup",
        level(Informational),
        keyword(KW_DEVICE)
    );
    PROVIDER.unregister();
}

/// `EvtDriverDeviceAdd`: a filter device with a parallel, non-power-managed default queue for
/// `IRP_MJ_DEVICE_CONTROL` and `IRP_MJ_INTERNAL_DEVICE_CONTROL`. KMDF passes every other major
/// function of a filter device down unchanged.
#[unsafe(link_section = "PAGE")]
unsafe extern "C" fn evt_device_add(
    _driver: WDFDRIVER,
    mut device_init: PWDFDEVICE_INIT,
) -> NTSTATUS {
    wdk::paged_code!();
    // SAFETY: the framework's WDFDEVICE_INIT, before WdfDeviceCreate.
    unsafe { call_unsafe_wdf_function_binding!(WdfFdoInitSetFilter, device_init) };
    let mut attributes = object_attributes_init_context_type(&DEVICE_CONTEXT_TYPE_INFO);
    let mut device: WDFDEVICE = null_mut();
    // SAFETY: valid init pointer (consumed on success), attributes and out pointer.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDeviceCreate,
            &raw mut device_init,
            &raw mut attributes,
            &raw mut device
        )
    };
    if !NT_SUCCESS(status) {
        trace_failure("WdfDeviceCreate", status);
        return status;
    }
    // SAFETY: the device was just created with this context type; no callback runs before the
    // queue below exists.
    unsafe {
        device_get_context(device).write(DeviceContext {
            interrupt_channels: [const { AtomicU64::new(0) }; CHANNEL_SLOTS],
            next_eviction: AtomicUsize::new(0),
        });
    }
    let mut config =
        io_queue_config_init_default_queue(_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel);
    // HidBth sends BRBs during power transitions too: never hold a request.
    config.PowerManaged = _WDF_TRI_STATE::WdfFalse;
    config.EvtIoDeviceControl = Some(evt_io_device_control);
    config.EvtIoInternalDeviceControl = Some(evt_io_internal_device_control);
    // SAFETY: live device; initialized config; no attributes; the queue handle is not needed.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &raw mut config,
            WDF_NO_OBJECT_ATTRIBUTES,
            null_mut()
        )
    };
    if !NT_SUCCESS(status) {
        trace_failure("WdfIoQueueCreate", status);
        return status;
    }
    tlg::write_event!(PROVIDER, "DeviceAdd", level(Informational), keyword(KW_DEVICE));
    STATUS_SUCCESS
}

fn trace_failure(call: &str, status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "CallFailed",
        level(Error),
        keyword(KW_DEVICE),
        str8("Call", call),
        win_ntstatus("Status", &status)
    );
}

/// The device of a request presented by `queue`.
///
/// # Safety
/// `queue` is one of this driver's live queues.
unsafe fn queue_device(queue: WDFQUEUE) -> WDFDEVICE {
    // SAFETY: forwarded.
    unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue) }
}

/// `EvtIoDeviceControl`: `HidBth`'s SDP requests (`IoBuildDeviceIoControlRequest`,
/// `InternalDeviceIoControl = FALSE`).
unsafe extern "C" fn evt_io_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    // SAFETY: our queue.
    let device = unsafe { queue_device(queue) };
    // SAFETY: an SDP search request presented to this driver, and its device.
    if io_control_code == sdp::IOCTL_SDP_SERVICE_ATTRIBUTE_SEARCH && unsafe { sdp::serve(device, request) }
    {
        return;
    }
    // SAFETY: a request presented to this driver, not completed.
    unsafe { send_and_forget(device, request) };
}

/// `EvtIoInternalDeviceControl`: BRBs and the BTHENUM internal IOCTLs.
///
/// Only incoming transfers on an interrupt channel and channel opens get a completion routine;
/// everything else is sent and forgotten. (`output_buffer_length` aliases the BRB pointer's slot
/// for `METHOD_NEITHER` and is not a length.)
unsafe extern "C" fn evt_io_internal_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    // SAFETY: our queue.
    let device = unsafe { queue_device(queue) };
    if io_control_code == IOCTL_SUBMIT_BRB {
        // SAFETY: the BRB of a request this driver owns; used only below, before forwarding.
        let brb = unsafe { Brb::classify(request_brb(request)) };
        // SAFETY: the device's context, written in AddDevice.
        let ctx = unsafe { &*device_get_context(device) };
        // SAFETY: the BRB's request is still owned.
        if let Some(tag) = unsafe { interrupt::on_submit(ctx, brb) } {
            // SAFETY: a request presented to this driver, not completed.
            unsafe { forward_with_completion(device, request, tag) };
            return;
        }
    }
    // SAFETY: a request presented to this driver, not completed.
    unsafe { send_and_forget(device, request) };
}

/// The BRB of an `IOCTL_INTERNAL_BTH_SUBMIT_BRB` request (`Parameters.Others.Arg1`), or null.
///
/// # Safety
/// `request` is a live request owned by this driver.
pub(crate) unsafe fn request_brb(request: WDFREQUEST) -> *mut u8 {
    let mut params = request_parameters_init();
    // SAFETY: live request; initialized parameters structure.
    unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetParameters, request, &raw mut params) };
    // SAFETY: `Others` is the union member KMDF fills for internal device-control requests.
    unsafe { params.Parameters.Others.Arg1 }.cast::<u8>()
}

/// Sends `request` down unchanged and forgets it; completes it with the send status on failure.
///
/// # Safety
/// `request` was presented to this driver and is not completed; `device` is its device.
unsafe fn send_and_forget(device: WDFDEVICE, request: WDFREQUEST) {
    // SAFETY: our device's default local target.
    let target = unsafe { call_unsafe_wdf_function_binding!(WdfDeviceGetIoTarget, device) };
    // SAFETY: the request is owned (caller's contract); the fallback status is only used if the
    // guard were dropped without a terminal operation, which does not happen here.
    let request = unsafe { Request::from_raw(request, wdf_support::ntstatus::STATUS_UNSUCCESSFUL) };
    // SAFETY: `target` is live (the device's own target).
    if let Err(rejected) = unsafe { request.send_and_forget(target) } {
        trace_failure("WdfRequestSend", rejected.status);
        rejected.complete();
    }
}

/// Sends `request` down unchanged with [`evt_completion`]; completes it with the send status on
/// failure.
///
/// # Safety
/// As [`send_and_forget`].
unsafe fn forward_with_completion(device: WDFDEVICE, request: WDFREQUEST, tag: usize) {
    // SAFETY: owned request; our device's default local target; `tag` is a plain integer that the
    // completion routine never dereferences.
    let sent = unsafe {
        let target = call_unsafe_wdf_function_binding!(WdfDeviceGetIoTarget, device);
        call_unsafe_wdf_function_binding!(WdfRequestFormatRequestUsingCurrentType, request);
        call_unsafe_wdf_function_binding!(
            WdfRequestSetCompletionRoutine,
            request,
            Some(evt_completion),
            tag as *mut c_void as WDFCONTEXT
        );
        call_unsafe_wdf_function_binding!(WdfRequestSend, request, target, null_mut())
    };
    if sent == 0 {
        // SAFETY: the send failed, so the request is still ours; completed exactly once here.
        unsafe {
            let status = call_unsafe_wdf_function_binding!(WdfRequestGetStatus, request);
            trace_failure("WdfRequestSend", status);
            call_unsafe_wdf_function_binding!(WdfRequestComplete, request, status);
        }
    }
}

/// Completion routine of the requests [`interrupt::on_submit`] tagged: lets [`interrupt`] look at
/// (and possibly pad) the result, then completes the request with the lower driver's status and
/// information.
unsafe extern "C" fn evt_completion(
    request: WDFREQUEST,
    target: WDFIOTARGET,
    params: PWDF_REQUEST_COMPLETION_PARAMS,
    context: WDFCONTEXT,
) {
    // SAFETY: KMDF passes valid completion parameters.
    let (status, information) = unsafe {
        (
            (*params).IoStatus.__bindgen_anon_1.Status,
            (*params).IoStatus.Information,
        )
    };
    // SAFETY: the target is our device's local target; its device carries our context.
    let ctx = unsafe {
        let device = call_unsafe_wdf_function_binding!(WdfIoTargetGetDevice, target);
        &*device_get_context(device)
    };
    // SAFETY: the request is ours until completed below; its BRB stays valid until then.
    unsafe { interrupt::on_complete(ctx, Brb::classify(request_brb(request)), context as usize, status) };
    // SAFETY: completed exactly once, with the lower driver's result.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestCompleteWithInformation,
            request,
            status,
            information
        );
    }
}

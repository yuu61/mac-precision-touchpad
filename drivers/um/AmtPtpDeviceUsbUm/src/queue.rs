//! The I/O queues and the device-control dispatcher (C: Queue.c, Hid.c handlers).
//!
//! | C | Rust |
//! |---|---|
//! | `AmtPtpDeviceQueueInitialize` | [`initialize`] |
//! | `AmtPtpDeviceEvtIoDeviceControl` | [`evt_io_device_control`] → `hid::route_ioctl` + the `hid` handlers |
//! | `AmtPtpDeviceEvtIoStop` | [`evt_io_stop`] (no-op) |
//! | `AmtPtpDispatchReadReportRequests` | [`dispatch_read_report`] |
//! | `WdfIoQueueRetrieveNextRequest` on `InputQueue` (InputInterrupt.c) | [`InputQueue`] (`ReadQueue`) |
//!
//! Queues (exactly as in C):
//! - default queue: `WdfIoQueueDispatchParallel`, `EvtIoDeviceControl` + `EvtIoStop`, `PowerManaged`
//!   left at `WdfUseDefault` (the driver is a filter, so the queue is not power-managed),
//!   `WDF_NO_OBJECT_ATTRIBUTES`, handle not stored. Request types without a handler (read, write,
//!   internal device control) are passed down by the framework (filter rule).
//! - `InputQueue`: `WdfIoQueueDispatchManual`, `PowerManaged = WdfFalse`, no callbacks, stored in the
//!   device context; never purged by driver code (the framework purges it on removal and cancels the
//!   requests it holds).

use amtptp_usbum_core::hid::{self, IoctlOutcome, IoctlRoute};
use amtptp_usbum_core::io::ReadQueue;
use amtptp_usbum_core::status::NtStatus;
use wdf_support::init::{io_queue_config_init, io_queue_config_init_default_queue};
use wdf_support::ntstatus::{
    NT_SUCCESS, STATUS_INVALID_DEVICE_STATE, STATUS_NOT_SUPPORTED, STATUS_UNSUCCESSFUL,
};
use wdf_support::request::Request;
use wdk_sys::{
    _WDF_IO_QUEUE_DISPATCH_TYPE, _WDF_TRI_STATE, NTSTATUS, ULONG, ULONG_PTR,
    WDF_NO_OBJECT_ATTRIBUTES, WDFDEVICE, WDFQUEUE, WDFREQUEST, call_unsafe_wdf_function_binding,
};

use crate::device::{DeviceContext, context};
use crate::request::RequestIo;
use crate::trace;
use crate::usb::{DeviceIo, UsbStrings};

/// `AmtPtpDeviceQueueInitialize` (Queue.c:6-75): the default parallel queue, then the manual
/// `InputQueue`. Returns the first failure.
///
/// # Safety
/// `device` must be the live WDFDEVICE being created in `EvtDriverDeviceAdd`, `context` its context.
pub(crate) unsafe fn initialize(device: WDFDEVICE, context: &DeviceContext) -> NTSTATUS {
    let mut queue_config =
        io_queue_config_init_default_queue(_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel);
    queue_config.EvtIoDeviceControl = Some(evt_io_device_control);
    queue_config.EvtIoStop = Some(evt_io_stop);
    let mut queue: WDFQUEUE = core::ptr::null_mut();
    // SAFETY: valid device (caller contract); `queue_config` initialized; no attributes; out pointer.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &raw mut queue_config,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut queue
        )
    };
    if !NT_SUCCESS(status) {
        trace::pnp_failure(
            "AmtPtpDeviceQueueInitialize",
            "WdfIoQueueCreate (Primary)",
            status,
        );
        return status;
    }

    let mut queue_config =
        io_queue_config_init(_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchManual);
    queue_config.PowerManaged = _WDF_TRI_STATE::WdfFalse;
    let mut input_queue: WDFQUEUE = core::ptr::null_mut();
    // SAFETY: as above.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueCreate,
            device,
            &raw mut queue_config,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut input_queue
        )
    };
    if !NT_SUCCESS(status) {
        trace::pnp_failure(
            "AmtPtpDeviceQueueInitialize",
            "WdfIoQueueCreate (Input)",
            status,
        );
        return status;
    }
    context.set_input_queue(input_queue);
    status
}

/// `AmtPtpDeviceEvtIoDeviceControl` (Queue.c:118-213).
///
/// Routes with `hid::route_ioctl`. `READ_REPORT` is forwarded to `InputQueue` (pending; completed later by
/// the read completion). Every other request is completed here exactly once with the handler's status
/// and Information (`WdfRequestSetInformation` only where the C handler sets it, i.e. a non-zero value,
/// then `WdfRequestComplete`). Unsupported codes complete with `STATUS_NOT_SUPPORTED` and are **not**
/// forwarded.
///
/// # Safety
/// Framework callback of the default queue; `request` is presented to the driver.
unsafe extern "C" fn evt_io_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    // SAFETY: the framework presented `request` to this callback, so the driver owns it; it is wrapped
    // exactly once. Every path below completes or forwards it explicitly (the drop status is only a
    // safety net).
    let request = unsafe { Request::from_raw(request, STATUS_UNSUCCESSFUL) };
    // SAFETY: `queue` is the live default queue of our device.
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue) };
    // SAFETY: the queue's device was created by create_device.
    let Some(context) = (unsafe { context(device) }) else {
        request.complete(STATUS_INVALID_DEVICE_STATE);
        return;
    };

    let outcome = match hid::route_ioctl(io_control_code) {
        IoctlRoute::ReadReport => {
            dispatch_read_report(context, request);
            return;
        }
        IoctlRoute::GetDeviceDescriptor => {
            hid::get_hid_descriptor(&context.state, &mut RequestIo::new(&request))
        }
        IoctlRoute::GetReportDescriptor => {
            hid::get_report_descriptor(&context.state, &mut RequestIo::new(&request))
        }
        IoctlRoute::GetDeviceAttributes => {
            hid::get_device_attributes(&context.state, &mut RequestIo::new(&request))
        }
        IoctlRoute::GetString => hid::get_string(
            &context.state,
            &mut RequestIo::new(&request),
            &mut UsbStrings::new(context),
        ),
        IoctlRoute::GetFeature => hid::get_feature(&context.state, &mut RequestIo::new(&request)),
        IoctlRoute::SetFeature => {
            let before = context.state.snapshot();
            let outcome = hid::set_feature(
                &context.state,
                &mut RequestIo::new(&request),
                &mut DeviceIo::new(context),
            );
            trace::state_changed("AmtPtpSetFeatures", &before, &context.state.snapshot());
            outcome
        }
        IoctlRoute::NotSupported => {
            trace::ioctl_not_supported(io_control_code);
            IoctlOutcome::NOT_SUPPORTED
        }
    };

    if let Some(event) = outcome.event {
        trace::device_identification(event);
    }
    if !NT_SUCCESS(outcome.status) && outcome.status != STATUS_NOT_SUPPORTED {
        trace::ioctl_failed(io_control_code, outcome.status);
    }
    complete(request, outcome);
}

/// The dispatcher's `WdfRequestComplete(Request, status)`, preceded by the handler's
/// `WdfRequestSetInformation` when it set one (non-zero Information).
fn complete(request: Request, outcome: IoctlOutcome) {
    if outcome.information != 0 {
        // usize -> ULONG_PTR: same width on x64 and ARM64.
        request.set_information(outcome.information as ULONG_PTR);
    }
    request.complete(outcome.status);
}

/// `AmtPtpDispatchReadReportRequests` (Queue.c:260-302): `WdfRequestForwardToIoQueue(Request,
/// InputQueue)`. On success the request is pending in `InputQueue`; on failure it is completed with the
/// forward status (Information 0). No buffer-size check (as in C).
fn dispatch_read_report(context: &DeviceContext, request: Request) {
    let input_queue = context.input_queue();
    if input_queue.is_null() {
        // Unreachable: the queues exist before any request is presented.
        request.complete(STATUS_INVALID_DEVICE_STATE);
        return;
    }
    // SAFETY: `input_queue` is the live manual queue of the request's device.
    if let Err(rejected) = unsafe { request.forward_to_io_queue(input_queue) } {
        trace::ioctl_failed(ptp_hid::ioctl::IOCTL_HID_READ_REPORT, rejected.status);
        rejected.complete();
    }
}

/// `AmtPtpDeviceEvtIoStop` (Queue.c:215-258): a no-op, as in C — it neither acknowledges nor completes
/// the request. Every request the default queue presents is completed before `EvtIoDeviceControl`
/// returns (`READ_REPORT` is forwarded to the non-power-managed `InputQueue`), so the framework only has
/// to wait for a dispatch that is still running.
///
/// # Safety
/// Framework callback of the default queue.
const unsafe extern "C" fn evt_io_stop(
    _queue: WDFQUEUE,
    _request: WDFREQUEST,
    _action_flags: ULONG,
) {
}

/// [`ReadQueue`] over `InputQueue`: the pending `IOCTL_HID_READ_REPORT` requests.
pub(crate) struct InputQueue {
    queue: WDFQUEUE,
}

impl InputQueue {
    /// The adapter over the device's `InputQueue` handle.
    pub(crate) const fn new(queue: WDFQUEUE) -> Self {
        Self { queue }
    }
}

impl ReadQueue for InputQueue {
    type Request = RequestIo<Request>;

    /// `WdfIoQueueRetrieveNextRequest(InputQueue, &Request)`.
    fn retrieve_next_request(&mut self) -> Result<RequestIo<Request>, NtStatus> {
        if self.queue.is_null() {
            // Unreachable: the reader only runs after EvtDeviceAdd created the queue.
            return Err(STATUS_INVALID_DEVICE_STATE);
        }
        let mut request: WDFREQUEST = core::ptr::null_mut();
        // SAFETY: live manual queue of the device; `request` is an out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoQueueRetrieveNextRequest,
                self.queue,
                &raw mut request
            )
        };
        if !NT_SUCCESS(status) {
            return Err(status);
        }
        // SAFETY: a request retrieved from a manual queue belongs to the driver until it completes it;
        // it is wrapped exactly once. The core always completes it through `complete_request` (the drop
        // status is only a safety net).
        Ok(RequestIo::new(unsafe {
            Request::from_raw(request, STATUS_UNSUCCESSFUL)
        }))
    }

    /// `WdfRequestSetInformation(Request, information)` + `WdfRequestComplete(Request, status)`.
    fn complete_request(
        &mut self,
        request: RequestIo<Request>,
        status: NtStatus,
        information: usize,
    ) {
        let request = request.into_inner();
        // usize -> ULONG_PTR: same width on x64 and ARM64.
        request.set_information(information as ULONG_PTR);
        request.complete(status);
    }
}

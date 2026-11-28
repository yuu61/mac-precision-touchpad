//! Queues and the internal-IOCTL dispatcher (Queue.c, Hid.c).
//!
//! - Default queue: `WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(Parallel)`, `PowerManaged = WdfUseDefault`,
//!   inheriting execution level so `IOCTL_HID_READ_REPORT` and descriptor/feature-read IOCTLs run
//!   directly at up to `DISPATCH_LEVEL` without hopping to a worker thread.
//! - `PassiveQueue`: non-power-managed parallel queue with `WdfExecutionLevelPassive` for
//!   `IOCTL_HID_GET_STRING` and `IOCTL_HID_SET_FEATURE`, which issue synchronous USB control transfers.
//! - `InputQueue`: `WDF_IO_QUEUE_CONFIG_INIT(Manual)`, `PowerManaged = WdfFalse`, drained on `D0Exit`.

use amtptp_usb_bt::usb::lifecycle::io_stop;
use core::marker::PhantomData;
use core::ptr;
use core::slice;

use amtptp_core::hid::common::{REPORTID_FUNCSWITCH, REPORTID_REPORTMODE};
use amtptp_usb_bt::usb::feature::{self, GetFeaturePacket, HID_XFER_PACKET_LEN, SetFeaturePacket};
use amtptp_usb_bt::usb::hid::{self, HID_DEVICE_ATTRIBUTES_LEN, InternalIoctl};
use amtptp_usb_bt::usb::lifecycle::IoStopAction;
use amtptp_usb_bt::usb::output::OutputMemory;
use amtptp_usb_bt::usb::status::{Completion, NtStatus};
use tracelogging as tlg;
use wdf_support::hid as wdf_hid;
use wdf_support::init::{io_queue_config_init, object_attributes_init};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE, STATUS_NOT_SUPPORTED};
use wdf_support::request::Request;
use wdk_sys::{
    _WDF_EXECUTION_LEVEL, _WDF_IO_QUEUE_DISPATCH_TYPE, _WDF_TRI_STATE, HID_XFER_PACKET, NTSTATUS,
    ULONG, ULONG_PTR, WDF_NO_OBJECT_ATTRIBUTES, WDFDEVICE, WDFMEMORY, WDFQUEUE, WDFREQUEST,
    call_unsafe_wdf_function_binding,
};

use crate::PROVIDER;
use crate::context::DeviceContext;
use crate::trace::KW_QUEUE;
use crate::usb::UsbIo;

const _: () = assert!(size_of::<HID_XFER_PACKET>() == HID_XFER_PACKET_LEN);

/// Initializes the dispatch-capable default queue, the manual `InputQueue`, and the passive-level
/// control queue. Returns the first failure. `PASSIVE_LEVEL`, `PAGE`.
///
/// # Safety
/// `device` is the live device that owns `context`; `PASSIVE_LEVEL`.
#[unsafe(link_section = "PAGE")]
pub(crate) unsafe fn queue_initialize(device: WDFDEVICE, context: &DeviceContext) -> NTSTATUS {
    wdk::paged_code!();

    let mut config =
        wdf_hid::default_queue_config(Some(evt_io_internal_device_control), Some(evt_io_stop));
    let mut queue: WDFQUEUE = ptr::null_mut();
    // SAFETY: live device; `config` initialized; `WDF_NO_OBJECT_ATTRIBUTES` inherits execution level.
    let status = unsafe {
        wdf_hid::create_queue(device, &mut config, WDF_NO_OBJECT_ATTRIBUTES, &raw mut queue)
    };
    if !NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "WdfIoQueueCreateFailed",
            level(Error),
            keyword(KW_QUEUE),
            win_ntstatus("Status", &status)
        );
        return status;
    }

    let mut config = wdf_hid::read_queue_config();
    let mut input_queue: WDFQUEUE = ptr::null_mut();
    // SAFETY: as above.
    let status = unsafe {
        wdf_hid::create_queue(
            device,
            &mut config,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut input_queue,
        )
    };
    context.set_input_queue(input_queue);
    if !NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "InputQueueCreateFailed",
            level(Error),
            keyword(KW_QUEUE),
            win_ntstatus("Status", &status)
        );
        return status;
    }

    let mut passive_config =
        io_queue_config_init(_WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchParallel);
    passive_config.PowerManaged = _WDF_TRI_STATE::WdfFalse;
    passive_config.EvtIoInternalDeviceControl = Some(evt_io_passive_device_control);
    let mut passive_attrs = object_attributes_init();
    passive_attrs.ExecutionLevel = _WDF_EXECUTION_LEVEL::WdfExecutionLevelPassive;
    let mut passive_queue: WDFQUEUE = ptr::null_mut();
    // SAFETY: live device at `PASSIVE_LEVEL`; initialized config and passive execution attributes.
    let status = unsafe {
        wdf_hid::create_queue(
            device,
            &mut passive_config,
            &raw mut passive_attrs,
            &raw mut passive_queue,
        )
    };
    context.set_passive_queue(passive_queue);
    if !NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "PassiveQueueCreateFailed",
            level(Error),
            keyword(KW_QUEUE),
            win_ntstatus("Status", &status)
        );
    }
    status
}

/// Completes a request like the C dispatcher: `WdfRequestSetInformation(Request, information)` only when
/// the handler set it (the C calls it exactly on the paths with a non-zero value: 9, 493, 32, 50), then
/// `WdfRequestComplete(Request, status)`. On every other path the request keeps the Information it came
/// with, as in C.
pub(crate) fn complete(request: Request, completion: Completion) {
    if completion.information != 0 {
        // usize -> ULONG_PTR (u64) is lossless on the 64-bit targets.
        request.set_information(completion.information as ULONG_PTR);
    }
    request.complete(completion.status);
}

/// The `WDFMEMORY` of `WdfRequestRetrieveOutputMemory`, written with `WdfMemoryCopyFromBuffer` (so the
/// framework produces the copy status, as for the C). Valid while the request is not completed.
pub(crate) struct RequestOutputMemory<'r> {
    memory: WDFMEMORY,
    _request: PhantomData<&'r Request>,
}

impl OutputMemory for RequestOutputMemory<'_> {
    fn copy_from_buffer(&mut self, src: &[u8]) -> NtStatus {
        // SAFETY: `memory` is the output memory of a request that is still owned (borrowed for 'r, so not
        // completed); `src` is valid for `src.len()` bytes. The destination is hidclass's non-paged buffer,
        // so any IRQL <= DISPATCH_LEVEL is fine.
        unsafe { wdf_hid::copy_to_output_memory(self.memory, src) }
    }
}

/// `WdfRequestRetrieveOutputMemory(Request, &memory)`.
pub(crate) fn retrieve_output_memory(
    request: &Request,
) -> Result<RequestOutputMemory<'_>, NtStatus> {
    // SAFETY: the borrowed request remains live for the memory object's lifetime.
    let memory = unsafe { wdf_hid::retrieve_output_memory(request.as_raw()) }?;
    if memory.is_null() {
        // Impossible (success always returns a memory object); never complete as success without data.
        return Err(STATUS_INVALID_DEVICE_STATE);
    }
    Ok(RequestOutputMemory {
        memory,
        _request: PhantomData,
    })
}

/// `Result<RequestOutputMemory, NtStatus>` → the `Result<&mut M, NtStatus>` the core takes.
pub(crate) const fn as_output<'m, 'r>(
    output: &'m mut Result<RequestOutputMemory<'r>, NtStatus>,
) -> Result<&'m mut RequestOutputMemory<'r>, NtStatus> {
    match output {
        Ok(memory) => Ok(memory),
        Err(status) => Err(*status),
    }
}

/// `WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_DEVICE_ATTRIBUTES), &buffer, ..)` as a slice.
fn retrieve_attributes_buffer(request: &mut Request) -> Result<&mut [u8], NtStatus> {
    // SAFETY: this exclusively borrowed request is live and stays pending while the buffer is used.
    let (buffer, length) =
        unsafe { wdf_hid::retrieve_output_buffer(request.as_raw(), HID_DEVICE_ATTRIBUTES_LEN) }?;
    if buffer.is_null() {
        return Ok(&mut []);
    }
    // SAFETY: WDF returned the request's output buffer of `length` (>= 32) bytes, valid and exclusively
    // ours until the request is completed, which the exclusive borrow of `request` rules out while the
    // slice lives.
    Ok(unsafe { slice::from_raw_parts_mut(buffer, length) })
}

/// Returns a USB string in the NUL-terminated UTF-16 form required by HIDCLASS.
fn usb_string(request: &mut Request, context: &DeviceContext) -> Completion {
    use amtptp_core::hid::hidport::{
        HID_STRING_ID_IMANUFACTURER, HID_STRING_ID_IPRODUCT, HID_STRING_ID_ISERIALNUMBER,
    };
    let device = context.usb_device();
    if device.is_null() {
        return Completion::status(STATUS_INVALID_DEVICE_STATE);
    }
    // SAFETY: this is an internal device control request. HIDCLASS encodes
    // the ID and language in Type3InputBuffer itself.
    let value = unsafe {
        request
            .parameters()
            .Parameters
            .DeviceIoControl
            .Type3InputBuffer
    }
    .addr();
    let mut descriptor = wdk_sys::USB_DEVICE_DESCRIPTOR::default();
    // SAFETY: device is the live USB target and descriptor is writable.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfUsbTargetDeviceGetDeviceDescriptor,
            device,
            &raw mut descriptor
        );
    }
    let index = match value as u16 {
        HID_STRING_ID_IMANUFACTURER => descriptor.iManufacturer,
        HID_STRING_ID_IPRODUCT => descriptor.iProduct,
        HID_STRING_ID_ISERIALNUMBER => descriptor.iSerialNumber,
        _ => return Completion::status(STATUS_NOT_SUPPORTED),
    };
    if index == 0 {
        return Completion::status(STATUS_NOT_SUPPORTED);
    }
    // A USB string descriptor has an 8-bit byte length including its two-byte header.
    let mut units = [0_u16; 128];
    let mut length = 127_u16;
    let mut options = wdf_support::init::sync_send_options(0);
    // SAFETY: passive queue, valid target and synchronous borrowed buffers.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfUsbTargetDeviceQueryString,
            device,
            ptr::null_mut(),
            &raw mut options,
            units.as_mut_ptr(),
            &raw mut length,
            index,
            (value >> 16) as u16
        )
    };
    if !NT_SUCCESS(status) {
        return Completion::status(status);
    }
    let Some(text) = hid::terminated_usb_string(&units, length) else {
        return Completion::status(STATUS_INVALID_DEVICE_STATE);
    };
    let size = size_of_val(text);
    // SAFETY: text contains initialized UTF-16 code units on little-endian Windows and stays live during the copy.
    let bytes = unsafe { slice::from_raw_parts(text.as_ptr().cast::<u8>(), size) };
    match retrieve_output_memory(request) {
        Ok(mut memory) => {
            let status = memory.copy_from_buffer(bytes);
            if NT_SUCCESS(status) {
                Completion::success(size)
            } else {
                Completion::status(status)
            }
        }
        Err(status) => Completion::status(status),
    }
}

/// `GET_FEATURE` packet: the caller's report buffer as `&mut [u8]`; a NULL `reportBuffer` becomes an empty
/// slice (spec B16, P2: the C would write through NULL).
///
/// # Safety
/// As [`wdf_hid::read_xfer_packet`]; `reportBuffer` points at `reportBufferLen` writable bytes (hidclass's buffer).
unsafe fn get_feature_packet(request: &mut Request) -> Option<GetFeaturePacket<'_>> {
    // SAFETY: forwarded caller contract.
    let packet = unsafe { wdf_hid::read_xfer_packet(request.as_raw()) }?;
    let (buffer, length, report_id) = (
        packet.reportBuffer,
        packet.reportBufferLen as usize,
        packet.reportId,
    );
    let report_buffer: &mut [u8] = if buffer.is_null() {
        &mut []
    } else {
        // SAFETY: hidclass's report buffer of `length` bytes (caller contract), used only until the
        // request completes (the slice exclusively borrows `request`).
        unsafe { slice::from_raw_parts_mut(buffer, length) }
    };
    tlg::write_event!(
        PROVIDER,
        "GetFeature",
        level(Verbose),
        keyword(KW_QUEUE),
        u8_hex("ReportId", &report_id),
        usize("ReportBufferLen", &length)
    );
    Some(GetFeaturePacket {
        report_id,
        report_buffer,
    })
}

/// `SET_FEATURE` packet: the report as `&[u8]` (NULL `reportBuffer` → empty, spec B16).
///
/// # Safety
/// As [`wdf_hid::read_xfer_packet`]; `reportBuffer` points at `reportBufferLen` readable bytes.
unsafe fn set_feature_packet(request: &Request) -> Option<SetFeaturePacket<'_>> {
    // SAFETY: forwarded caller contract.
    let packet = unsafe { wdf_hid::read_xfer_packet(request.as_raw()) }?;
    let (buffer, length, report_id) = (
        packet.reportBuffer,
        packet.reportBufferLen as usize,
        packet.reportId,
    );
    let report_buffer: &[u8] = if buffer.is_null() {
        &[]
    } else {
        // SAFETY: hidclass's report buffer of `length` bytes (caller contract), used only until the
        // request completes (the slice borrows `request`).
        unsafe { slice::from_raw_parts(buffer.cast_const(), length) }
    };
    tlg::write_event!(
        PROVIDER,
        "SetFeature",
        level(Informational),
        keyword(KW_QUEUE),
        u8_hex("ReportId", &report_id),
        usize("ReportBufferLen", &length)
    );
    if report_id == REPORTID_REPORTMODE {
        let mode = feature::input_mode(report_buffer);
        tlg::write_event!(
            PROVIDER,
            "SetInputMode",
            level(Informational),
            keyword(KW_QUEUE),
            u8("InputMode", &mode)
        );
    }
    if report_id == REPORTID_FUNCSWITCH
        && let Some(bits) = feature::selective_report_bits(report_buffer)
    {
        // The core applies these switches to the input gates.
        tlg::write_event!(
            PROVIDER,
            "SelectiveReporting",
            level(Informational),
            keyword(KW_QUEUE),
            bool8("ButtonReport", &bits.button_report),
            bool8("SurfaceReport", &bits.surface_report)
        );
    }
    Some(SetFeaturePacket {
        report_id,
        report_buffer,
    })
}

/// `AmtPtpDeviceUsbKmEvtIoDeviceControl` (Queue.c:105-182), registered as the default queue's
/// `EvtIoInternalDeviceControl`. Any IRQL <= `DISPATCH_LEVEL` (`READ_REPORT` can arrive at `DISPATCH_LEVEL`);
/// non-paged. The two lengths are unused, as in C (the feature handlers read them from
/// `WdfRequestGetParameters`).
unsafe extern "C" fn evt_io_internal_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    // SAFETY: the framework presented `request` to this callback; the driver owns it and completes or
    // forwards it exactly once below. Drop fallback (unreachable): STATUS_NOT_SUPPORTED.
    let mut request = unsafe { Request::from_raw(request, STATUS_NOT_SUPPORTED) };
    // SAFETY: `queue` is the live default queue of this callback.
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue) };
    // SAFETY: the queue's device is this driver's live device.
    let Some(context) = (unsafe { DeviceContext::of(device) }) else {
        request.complete(STATUS_INVALID_DEVICE_STATE);
        return;
    };

    match hid::route_internal_ioctl(io_control_code) {
        InternalIoctl::GetDeviceDescriptor => {
            let completion = {
                let mut output = retrieve_output_memory(&request);
                hid::get_hid_descriptor_to(as_output(&mut output))
            };
            trace_failure("GetHidDescriptor", &completion);
            complete(request, completion);
        }
        InternalIoctl::GetDeviceAttributes => {
            let completion = hid::get_device_attributes(
                retrieve_attributes_buffer(&mut request),
                context.state.usb_device_ids(),
            );
            trace_failure("GetDeviceAttributes", &completion);
            complete(request, completion);
        }
        InternalIoctl::GetReportDescriptor => {
            let completion = {
                let mut output = retrieve_output_memory(&request);
                hid::get_report_descriptor_for_to(context.state.config(), as_output(&mut output))
            };
            trace_failure("GetReportDescriptor", &completion);
            complete(request, completion);
        }
        InternalIoctl::GetString | InternalIoctl::SetFeature => {
            forward_to_device_queue(request, context.passive_queue());
        }
        InternalIoctl::ReadReport => {
            forward_to_device_queue(request, context.input_queue());
        }
        InternalIoctl::GetFeature => {
            // AmtPtpReportFeatures (Hid.c:287-418).
            let parameters = request.parameters();
            // SAFETY: an internal device-control request, so `DeviceIoControl` is the active member.
            let output_length = unsafe { parameters.Parameters.DeviceIoControl.OutputBufferLength };
            let completion = feature::get_feature(
                output_length,
                // SAFETY: GET_FEATURE from hidclass; the core calls it only after the 16-byte check.
                || unsafe { get_feature_packet(&mut request) },
            );
            trace_failure("GetFeature", &completion);
            complete(request, completion);
        }
        InternalIoctl::NotSupported => {
            tlg::write_event!(
                PROVIDER,
                "UnsupportedInternalIoctl",
                level(Verbose),
                keyword(KW_QUEUE),
                u32_hex("IoControlCode", &io_control_code)
            );
            complete(request, hid::not_supported());
        }
    }
}

/// Forwards `request` to one of the device's auxiliary queues (`InputQueue` or `PassiveQueue`),
/// completing it on error.
fn forward_to_device_queue(request: Request, target_queue: WDFQUEUE) {
    if target_queue.is_null() {
        complete(request, Completion::status(STATUS_INVALID_DEVICE_STATE));
        return;
    }
    // SAFETY: `target_queue` belongs to the same `WDFDEVICE` as `request`.
    if let Err(rejected) = unsafe { request.forward_to_io_queue(target_queue) } {
        tlg::write_event!(
            PROVIDER,
            "WdfRequestForwardToIoQueueFailed",
            level(Error),
            keyword(KW_QUEUE),
            win_ntstatus("Status", &rejected.status)
        );
        let completion = hid::read_report_forward_failed(rejected.status);
        complete(rejected.request, completion);
    }
}

/// Passive-level dispatcher for `IOCTL_HID_GET_STRING` and `IOCTL_HID_SET_FEATURE`, which perform
/// synchronous USB control transfers.
unsafe extern "C" fn evt_io_passive_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    // SAFETY: framework-presented request owned by this callback and completed once below.
    let mut request = unsafe { Request::from_raw(request, STATUS_NOT_SUPPORTED) };
    // SAFETY: `queue` is the live `PassiveQueue` of this callback.
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue) };
    // SAFETY: `device` is this driver's live device.
    let Some(context) = (unsafe { DeviceContext::of(device) }) else {
        request.complete(STATUS_INVALID_DEVICE_STATE);
        return;
    };

    match hid::route_internal_ioctl(io_control_code) {
        InternalIoctl::GetString => {
            let completion = usb_string(&mut request, context);
            complete(request, completion);
        }
        InternalIoctl::SetFeature => {
            let parameters = request.parameters();
            // SAFETY: an internal device-control request, so `DeviceIoControl` is the active member.
            let input_length = unsafe { parameters.Parameters.DeviceIoControl.InputBufferLength };
            // SAFETY: `device` owns `context`; `PassiveQueue` guarantees `PASSIVE_LEVEL`.
            let mut io = unsafe { UsbIo::new(device, context) };
            let completion = feature::set_feature(
                input_length,
                // SAFETY: SET_FEATURE from hidclass; the core calls it only after the 16-byte check.
                || unsafe { set_feature_packet(&request) },
                &context.state,
                &mut io,
            );
            if !context.state.wellspring_mode_on() || !context.state.input_gates().report_touch {
                // SAFETY: live device context; spinlock created in `create_device`; no WDF calls.
                unsafe { context.reset_contacts() };
            }
            trace_failure("SetFeature", &completion);
            complete(request, completion);
        }
        InternalIoctl::GetDeviceDescriptor
        | InternalIoctl::GetDeviceAttributes
        | InternalIoctl::GetReportDescriptor
        | InternalIoctl::ReadReport
        | InternalIoctl::GetFeature
        | InternalIoctl::NotSupported => complete(request, hid::not_supported()),
    }
}

/// Traces a failed synchronous IOCTL completion.
fn trace_failure(handler: &str, completion: &Completion) {
    if !NT_SUCCESS(completion.status) {
        tlg::write_event!(
            PROVIDER,
            "InternalIoctlFailed",
            level(Warning),
            keyword(KW_QUEUE),
            str8("Handler", handler),
            win_ntstatus("Status", &completion.status)
        );
    }
}

/// `AmtPtpDeviceUsbKmEvtIoStop` (Queue.c:221-280): trace only. No `WdfRequestStopAcknowledge`, no
/// completion: every request of the default queue is completed or forwarded by the dispatcher before it
/// returns (a `SET_FEATURE` in a synchronous transfer completes when the transfer does).
unsafe extern "C" fn evt_io_stop(queue: WDFQUEUE, request: WDFREQUEST, action_flags: ULONG) {
    let IoStopAction::None = io_stop(action_flags);
    let queue = queue as usize;
    let request = request as usize;
    tlg::write_event!(
        PROVIDER,
        "EvtIoStop",
        level(Informational),
        keyword(KW_QUEUE),
        pointer("Queue", &queue),
        pointer("Request", &request),
        u32_hex("ActionFlags", &action_flags)
    );
}

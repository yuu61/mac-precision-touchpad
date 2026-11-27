//! The I/O queues and the internal-IOCTL dispatcher (C: Queue.c).

use core::ptr::null_mut;

use amtptp_spikm_core::ioctl::{InternalIoctl, NOT_SUPPORTED_STATUS, classify_internal_ioctl};
use tracelogging as tlg;
use wdf_support::hid as wdf_hid;
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE};
use wdf_support::request::Request;
use wdk_sys::{
    NTSTATUS, ULONG, WDF_NO_OBJECT_ATTRIBUTES, WDFDEVICE, WDFQUEUE, WDFREQUEST,
    call_unsafe_wdf_function_binding,
};

use crate::device::device_context;
use crate::{PROVIDER, hid, input, keyword};

/// `AmtPtpDeviceSpiKmQueueInitialize` (Queue.c:24-88), `PASSIVE_LEVEL` (`PAGE`, as in C).
///
/// 1. The default queue: parallel dispatch, `PowerManaged = WdfUseDefault` (not power-managed, because the
///    device is a filter), only `EvtIoInternalDeviceControl`. Every other request type is forwarded down
///    by the framework (filter rule). The C template's empty `EvtIoStop` is not registered: on a
///    non-power-managed queue it only runs at removal for driver-owned requests, and an `EvtIoStop` that
///    does not acknowledge behaves exactly like none (spec §2.7).
/// 2. `HidQueue`: manual dispatch, `PowerManaged = WdfFalse`, no callbacks; holds the pending upstream
///    `IOCTL_HID_READ_REPORT` requests. Its handle is written to `hid_queue`.
///
/// Neither queue has object attributes (the C `QUEUE_CONTEXT` was never used).
///
/// # Safety
/// `device` is the device being created in `AddDevice`; `hid_queue` points at its context's `hid_queue`
/// field, which nothing else accesses yet.
#[unsafe(link_section = "PAGE")]
pub(crate) unsafe fn queue_initialize(device: WDFDEVICE, hid_queue: *mut WDFQUEUE) -> NTSTATUS {
    wdk::paged_code!();

    let mut queue_config =
        wdf_hid::default_queue_config(Some(evt_io_internal_device_control), None);
    let mut queue: WDFQUEUE = null_mut();
    // SAFETY: valid device in AddDevice; initialized config; no attributes;
    // valid out-parameter (the handle is not kept, as in C).
    let status = unsafe {
        wdf_hid::create_queue(
            device,
            &mut queue_config,
            WDF_NO_OBJECT_ATTRIBUTES,
            &raw mut queue,
        )
    };
    if !NT_SUCCESS(status) {
        trace_queue_create_failure(b"Default", status);
        return status;
    }

    let mut queue_config = wdf_hid::read_queue_config();
    // SAFETY: as above; `hid_queue` is a writable out-parameter (caller contract).
    let status = unsafe {
        wdf_hid::create_queue(
            device,
            &mut queue_config,
            WDF_NO_OBJECT_ATTRIBUTES,
            hid_queue,
        )
    };
    if !NT_SUCCESS(status) {
        trace_queue_create_failure(b"HidQueue", status);
    }
    status
}

/// Error trace for a failed `WdfIoQueueCreate`.
fn trace_queue_create_failure(queue: &[u8], status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "QueueCreateFailed",
        level(Error),
        keyword(keyword::QUEUE),
        str8("Queue", queue),
        win_ntstatus("Status", &status)
    );
}

/// `AmtPtpDeviceSpiKmEvtIoInternalDeviceControl` (Queue.c:133-222): the HID-minidriver IOCTLs that
/// mshidkmdf sends down. Runs at IRQL <= `DISPATCH_LEVEL` (no synchronization scope, dispatch execution
/// level), so nothing reachable from here is pageable or blocks.
///
/// Every request is completed exactly once here or handed on exactly once: `IOCTL_HID_READ_REPORT` to
/// `HidQueue`, `IOCTL_HID_GET_STRING` to the lower driver. Unknown codes complete with
/// `STATUS_NOT_SUPPORTED` and are not forwarded (SPI-B16, preserved).
unsafe extern "C" fn evt_io_internal_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: ULONG,
) {
    // SAFETY: `request` was presented to this callback, so the driver owns it
    // and completes or hands it on exactly once below. The fallback status is
    // only used if a path forgot to (none does).
    let request = unsafe { Request::from_raw(request, STATUS_INVALID_DEVICE_STATE) };
    // SAFETY: `queue` is the framework's queue handle for this callback.
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue) };
    // SAFETY: the queue belongs to one of our devices; IRQL <= DISPATCH_LEVEL.
    let Some(context) = (unsafe { device_context(device) }) else {
        // Unreachable: every device of this driver has a DEVICE_CONTEXT.
        request.complete(STATUS_INVALID_DEVICE_STATE);
        return;
    };

    match classify_internal_ioctl(io_control_code) {
        InternalIoctl::GetDeviceDescriptor => {
            let status = hid::get_hid_descriptor(context, &request);
            request.complete(status);
        }
        InternalIoctl::GetDeviceAttributes => {
            let status = hid::get_device_attribs(context, &request);
            request.complete(status);
        }
        InternalIoctl::GetReportDescriptor => {
            let status = hid::get_report_descriptor(context, &request);
            request.complete(status);
        }
        InternalIoctl::GetString => hid::get_strings(context, request),
        InternalIoctl::ReadReport => {
            // SAFETY: `device` is the request's device and `context` its context; IRQL <= DISPATCH.
            unsafe { input::spi_input_routine_worker(device, context, request) };
        }
        InternalIoctl::GetFeature => {
            let status = hid::report_features(&request);
            request.complete(status);
        }
        InternalIoctl::SetFeature => {
            let status = hid::set_features(context, &request);
            request.complete(status);
        }
        InternalIoctl::NotSupported => {
            tlg::write_event!(
                PROVIDER,
                "IoctlNotSupported",
                level(Warning),
                keyword(keyword::QUEUE),
                u32_hex("IoControlCode", &io_control_code)
            );
            request.complete(NOT_SUPPORTED_STATUS);
        }
    }
}

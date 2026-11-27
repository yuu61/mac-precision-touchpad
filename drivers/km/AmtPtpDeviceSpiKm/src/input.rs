//! The read path (C: Input.c): upstream `IOCTL_HID_READ_REPORT` requests wait in `HidQueue`; each one
//! normally triggers one lower `IOCTL_HID_READ_REPORT` to the Apple PDO, and each lower completion turns
//! Apple's packet into a PTP input report for the **oldest** queued upstream request.
//!
//! Every lower read is registered in the device's read tracker before it is sent and unregistered when it
//! completes (see [`amtptp_spikm_core::runtime`]): the claim fails once the device is suspended, and a read
//! sent before the last suspend is dropped when it completes.
//!
//! Everything here runs at IRQL <= `DISPATCH_LEVEL` and is non-paged.

use core::ptr::{null, null_mut, write_bytes};

use amtptp_spikm_core::runtime::{ClaimRefused, LowerReadOrigin, ReadTicket};
use amtptp_spikm_core::wire::LOWER_READ_IOCTL;
use ptp_hid::input::SPI_PTP_REPORT_LEN;
use tracelogging as tlg;
use wdf_support::declare_context_type;
use wdf_support::init::object_attributes_init_context_type;
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_SUCCESS};
use wdf_support::request::Request;
use wdk_sys::{
    NTSTATUS, PWDF_REQUEST_COMPLETION_PARAMS, ULONG, ULONG_PTR, WDFCONTEXT, WDFDEVICE, WDFIOTARGET,
    WDFMEMORY, WDFOBJECT, WDFREQUEST, call_unsafe_wdf_function_binding,
};

use wdf_support::time::query_performance_counter;

use crate::device::DeviceContext;
use crate::{PROVIDER, keyword};

/// Port of the C `WORKER_REQUEST_CONTEXT` (Device.h:96-101), the context of every driver-created lower
/// read request. Named `WORKER_REQUEST_CONTEXT` for `!wdfkd`.
pub(crate) struct WorkerRequestContext {
    /// `DeviceContext`: the context of the device that issued the read (the request's parent).
    device_context: *const DeviceContext,
    /// `RequestMemory`: the lookaside buffer used as the read's output; deleted by the completion routine.
    request_memory: WDFMEMORY,
    /// The epoch the read was claimed in (not in C).
    ticket: ReadTicket,
}

declare_context_type!(
    pub(crate) WorkerRequestContext,
    WORKER_REQUEST_CONTEXT_TYPE_INFO,
    worker_request_get_context,
    name = "WORKER_REQUEST_CONTEXT"
);

/// `AmtPtpSpiInputRoutineWorker` (Input.c:4-36), `IOCTL_HID_READ_REPORT`.
///
/// `WdfRequestForwardToIoQueue(HidQueue)`; if that fails the request is completed with its status.
/// Otherwise, only when the device is `D0ActiveAndConfigured`, one lower read is issued. A read that
/// arrives while unconfigured waits in the queue until the device is configured, which issues a lower read
/// for it ([`issue_backlog_reads`]).
///
/// # Safety
/// `device` is the request's live device and `context` its context; IRQL <= `DISPATCH_LEVEL`.
pub(crate) unsafe fn spi_input_routine_worker(
    device: WDFDEVICE,
    context: &DeviceContext,
    request: Request,
) {
    // SAFETY: `hid_queue` is the device's manual queue, created in AddDevice.
    if let Err(rejected) = unsafe { request.forward_to_io_queue(context.hid_queue) } {
        tlg::write_event!(
            PROVIDER,
            "ForwardToHidQueueFailed",
            level(Error),
            keyword(keyword::DRIVER),
            win_ntstatus("Status", &rejected.status)
        );
        rejected.complete();
        return;
    }
    if context.runtime.should_issue_lower_read() {
        // SAFETY: forwarded caller contract.
        unsafe { spi_input_issue_request(device, context) };
    }
}

/// Deletes a WDF object the driver created (`WdfObjectDelete`).
///
/// # Safety
/// `object` is a live object created by this driver that is not used afterwards.
unsafe fn delete_object(object: WDFOBJECT) {
    // SAFETY: caller contract.
    unsafe { call_unsafe_wdf_function_binding!(WdfObjectDelete, object) };
}

/// Right after the device became configured: one lower read for every upstream read waiting in `HidQueue`
/// that no current read serves yet ([`amtptp_spikm_core::runtime::DeviceRuntime::backlog_reads`]; the queue
/// depth is read after the transition). Returns how many reads were attempted.
///
/// # Safety
/// `device` is a live device of this driver and `context` its context; IRQL <= `DISPATCH_LEVEL`.
pub(crate) unsafe fn issue_backlog_reads(device: WDFDEVICE, context: &DeviceContext) -> u32 {
    let mut queued: ULONG = 0;
    // SAFETY: `hid_queue` is the device's manual queue; DriverRequests is optional (NULL).
    let _state = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueGetState,
            context.hid_queue,
            &raw mut queued,
            null_mut()
        )
    };
    // SAFETY: caller contract; the closure only reads the tracker.
    let reads = unsafe {
        context.with_reads(|runtime, tracker| runtime.backlog_reads(tracker, queued as usize))
    };
    for _ in 0..reads {
        // SAFETY: forwarded caller contract.
        unsafe { spi_input_issue_request(device, context) };
    }
    // At most `queued`, a ULONG.
    u32::try_from(reads).unwrap_or(u32::MAX)
}

/// Trace for a lower read that could not be issued (the upstream request stays queued).
fn trace_issue_failure(step: &[u8], status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "LowerReadIssueFailed",
        level(Warning),
        keyword(keyword::DEVICE),
        str8("Step", step),
        win_ntstatus("Status", &status)
    );
}

/// `AmtPtpSpiInputIssueRequest` (Input.c:38-155): issues one asynchronous lower `IOCTL_HID_READ_REPORT`.
///
/// 1. `WdfRequestCreate` (context `WORKER_REQUEST_CONTEXT`, parent = the device, target = the local I/O
///    target).
/// 2. `WdfMemoryCreateFromLookaside` (1024 bytes, `NonPagedPoolNx`). The buffer is zeroed (P2: lookaside
///    buffers are not zero-initialized and may hold an earlier read; the completion routine only looks at
///    the first `Information` bytes anyway, so this changes nothing observable).
/// 3. `WdfIoTargetFormatRequestForInternalIoctl(IOCTL_HID_READ_REPORT, no input, output = the memory)`.
/// 4. `WdfRequestSetCompletionRoutine` with the context pointer.
/// 5. Claim the read in the read tracker (not in C): refused when the device is not configured (e.g. it
///    was suspended since the caller looked) or too many reads are in flight. Context = {device context,
///    memory, ticket}.
/// 6. `WdfRequestSend` (no options, no timeout); a failed send unregisters the read again.
///
/// Any failure deletes what was created (memory first, then the request, as in C) and returns silently:
/// the upstream request stays queued and is served by a later completion.
///
/// # Safety
/// `device` is a live device of this driver and `context` its context; IRQL <= `DISPATCH_LEVEL`.
pub(crate) unsafe fn spi_input_issue_request(device: WDFDEVICE, context: &DeviceContext) {
    let target: WDFIOTARGET = context.spi_trackpad_io_target;

    let mut attributes = object_attributes_init_context_type(&WORKER_REQUEST_CONTEXT_TYPE_INFO);
    attributes.ParentObject = device.cast();
    let mut lower_request: WDFREQUEST = null_mut();
    // SAFETY: initialized attributes; valid local I/O target; valid out-parameter.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestCreate,
            &raw mut attributes,
            target,
            &raw mut lower_request
        )
    };
    if !NT_SUCCESS(status) {
        trace_issue_failure(b"WdfRequestCreate", status);
        return;
    }

    let mut memory: WDFMEMORY = null_mut();
    // SAFETY: the lookaside list was created in AddDevice (parent: the device).
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfMemoryCreateFromLookaside,
            context.hid_read_buffer_lookaside,
            &raw mut memory
        )
    };
    if !NT_SUCCESS(status) {
        trace_issue_failure(b"WdfMemoryCreateFromLookaside", status);
        // SAFETY: created above, never sent.
        unsafe { delete_object(lower_request.cast()) };
        return;
    }

    let mut buffer_size: usize = 0;
    // SAFETY: `memory` was just created; valid size out-parameter.
    let buffer = unsafe {
        call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, memory, &raw mut buffer_size)
    };
    if !buffer.is_null() {
        // SAFETY: WdfMemoryGetBuffer returned this object's buffer of
        // `buffer_size` bytes, which nothing else uses before the send.
        unsafe { write_bytes(buffer.cast::<u8>(), 0, buffer_size) };
    }

    // SAFETY: `lower_request` was created with WORKER_REQUEST_CONTEXT_TYPE_INFO.
    let worker_context = unsafe { worker_request_get_context(lower_request) };
    if worker_context.is_null() {
        // Unreachable: the request was created with this context type.
        // SAFETY: both created above, never sent.
        unsafe {
            delete_object(memory.cast());
            delete_object(lower_request.cast());
        }
        return;
    }

    // SAFETY: valid target and request; output = the lookaside memory, whole
    // (offset NULL); no input.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoTargetFormatRequestForInternalIoctl,
            target,
            lower_request,
            LOWER_READ_IOCTL,
            null_mut(),
            null_mut(),
            memory,
            null_mut()
        )
    };
    if !NT_SUCCESS(status) {
        trace_issue_failure(b"WdfIoTargetFormatRequestForInternalIoctl", status);
        // SAFETY: both created above, never sent (C order: memory, then request).
        unsafe {
            delete_object(memory.cast());
            delete_object(lower_request.cast());
        }
        return;
    }

    // SAFETY: valid driver-created request; the routine's context is the
    // request's own WORKER_REQUEST_CONTEXT, which lives until the request is
    // deleted (in the completion routine).
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestSetCompletionRoutine,
            lower_request,
            Some(request_completion_routine),
            worker_context.cast()
        );
    }

    // SAFETY: caller contract; the closure only touches the tracker.
    let claim = unsafe {
        context.with_reads(|runtime, reads| runtime.claim_lower_read(reads, lower_request))
    };
    let ticket = match claim {
        Ok(ticket) => ticket,
        Err(refused) => {
            tlg::write_event!(
                PROVIDER,
                "LowerReadNotClaimed",
                level(Verbose),
                keyword(keyword::DEVICE),
                bool8("TooManyInFlight", &(refused == ClaimRefused::TooManyInFlight))
            );
            // SAFETY: both created above, never sent (C order: memory, then request).
            unsafe {
                delete_object(memory.cast());
                delete_object(lower_request.cast());
            }
            return;
        }
    };
    // SAFETY: the zero-filled, aligned context of the new request, not yet visible to the completion
    // routine (the request is not sent yet).
    unsafe {
        worker_context.write(WorkerRequestContext {
            device_context: context,
            request_memory: memory,
            ticket,
        });
    }

    // SAFETY: formatted request for this target; no send options (C: NULL).
    let sent = unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestSend, lower_request, target, null_mut())
    };
    if sent == 0 {
        tlg::write_event!(
            PROVIDER,
            "LowerReadSendFailed",
            level(Warning),
            keyword(keyword::DEVICE)
        );
        // SAFETY: caller contract; the closure only touches the tracker.
        let _origin = unsafe {
            context.with_reads(|runtime, reads| {
                runtime.finish_lower_read(reads, lower_request, ticket)
            })
        };
        // SAFETY: the send failed, so the completion routine will not run and
        // both objects are still ours (C order: memory, then request).
        unsafe {
            delete_object(memory.cast());
            delete_object(lower_request.cast());
        }
    }
}

/// `AmtPtpRequestCompletionRoutine` (Input.c:157-321): completion of a lower read, IRQL <= `DISPATCH_LEVEL`,
/// possibly on several CPUs at once.
///
/// 0. Unregister the read (not in C). A read sent before the last suspend is stale: its data is dropped
///    and `HidQueue` is not touched (P3 fix of SPI-B14), so it cannot complete a request queued after the
///    resume.
/// 1. Retrieve the oldest request from `HidQueue`. None: the data is dropped and no new read is issued
///    (SPI-B11, preserved).
/// 2. `(LONG)WdfRequestGetInformation(lower)` < 46: complete it with `STATUS_DEVICE_DATA_ERROR`; the lower
///    status is never checked (SPI-B9, preserved). Otherwise build the 30-byte report (zero-initialized:
///    SPI-B5; bytes past the lower length read as 0: SPI-B9c), then `WdfRequestRetrieveOutputMemory` +
///    `WdfMemoryCopyFromBuffer(30)` (a failure completes with that status), Information 30, SUCCESS.
/// 3. Cleanup: delete the lower request, then its memory. The memory handle is read from the request
///    context **before** the request (which owns the context) is deleted: the C code read it afterwards
///    (use-after-free, SPI-B10, P2).
unsafe extern "C" fn request_completion_routine(
    spi_request: WDFREQUEST,
    _target: WDFIOTARGET,
    params: PWDF_REQUEST_COMPLETION_PARAMS,
    context: WDFCONTEXT,
) {
    let worker_context = context.cast::<WorkerRequestContext>().cast_const();
    // SAFETY: `context` is the WORKER_REQUEST_CONTEXT of `spi_request`
    // (registered in spi_input_issue_request), alive until the request is
    // deleted below. Both fields are copied out now, before that delete.
    let (device_context, request_memory, ticket) = unsafe {
        match worker_context.as_ref() {
            Some(worker) => (worker.device_context, worker.request_memory, worker.ticket),
            // Unreachable: spi_input_issue_request never registers a NULL
            // context. The memory handle lives only in that context, so it
            // cannot be deleted here (accepted for an impossible state, SPI-X12).
            None => (null(), null_mut(), ReadTicket::default()),
        }
    };

    // SAFETY: the device context outlives every lower request (they are
    // children of the device); it is only accessed through shared references.
    if let Some(device) = unsafe { device_context.as_ref() } {
        // SAFETY: the context of a live device; completion routines run at IRQL <= DISPATCH_LEVEL. The
        // closure only touches the tracker.
        let origin = unsafe {
            device.with_reads(|runtime, reads| runtime.finish_lower_read(reads, spi_request, ticket))
        };
        if origin == LowerReadOrigin::Current {
            // SAFETY: `spi_request` is the completed lower request; `params` its
            // completion parameters, valid during this callback.
            unsafe { serve_oldest_read(device, spi_request, params) };
        } else {
            tlg::write_event!(
                PROVIDER,
                "StaleLowerReadDropped",
                level(Verbose),
                keyword(keyword::HID_INPUT)
            );
        }
    }

    // SAFETY: the lower request is ours and complete; it is not used again.
    unsafe { delete_object(spi_request.cast()) };
    if !request_memory.is_null() {
        // SAFETY: the lookaside memory created for this request; the request
        // that referenced it was deleted above and nothing else uses it.
        unsafe { delete_object(request_memory.cast()) };
    }
}

/// Steps 1-2 of [`request_completion_routine`].
///
/// # Safety
/// `context` is the issuing device's context, `spi_request` the completed lower read and `params` its
/// completion parameters; IRQL <= `DISPATCH_LEVEL`.
unsafe fn serve_oldest_read(
    context: &DeviceContext,
    spi_request: WDFREQUEST,
    params: PWDF_REQUEST_COMPLETION_PARAMS,
) {
    let mut ptp_request: WDFREQUEST = null_mut();
    // SAFETY: `hid_queue` is the device's manual queue; valid out-parameter.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueRetrieveNextRequest,
            context.hid_queue,
            &raw mut ptp_request
        )
    };
    if !NT_SUCCESS(status) || ptp_request.is_null() {
        tlg::write_event!(
            PROVIDER,
            "LowerReadDropped",
            level(Verbose),
            keyword(keyword::HID_INPUT),
            win_ntstatus("Status", &status)
        );
        return;
    }
    // SAFETY: just retrieved from our manual queue: the driver owns it and
    // completes it exactly once below.
    let ptp_request =
        unsafe { Request::from_raw(ptp_request, wdf_support::ntstatus::STATUS_DEVICE_DATA_ERROR) };

    // SAFETY: valid completed lower request.
    let information =
        unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetInformation, spi_request) };

    // The lower read's output memory (C: Params->Parameters.Ioctl.Output.Buffer).
    // SAFETY: `params` is valid during the completion routine; for an internal
    // IOCTL `Parameters.Ioctl` is the active member (plain handle read).
    let output_memory = unsafe {
        params
            .as_ref()
            .map_or(null_mut(), |p| p.Parameters.Ioctl.Output.Buffer)
    };
    let mut buffer_size: usize = 0;
    let buffer = if output_memory.is_null() {
        null_mut()
    } else {
        // SAFETY: the lookaside memory the request was formatted with.
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfMemoryGetBuffer,
                output_memory,
                &raw mut buffer_size
            )
        }
    };
    let packet: &[u8] = if buffer.is_null() {
        &[]
    } else {
        // SAFETY: the memory object's whole buffer: zeroed when the read was
        // issued, then written by the lower driver, which has completed the
        // read; nobody writes it while this shared view exists (it is deleted
        // only after this function returns).
        unsafe { core::slice::from_raw_parts(buffer.cast::<u8>().cast_const(), buffer_size) }
    };

    let trackpad = context.config.trackpad_info();
    // Lossless: ULONG_PTR is u64 on x64/ARM64.
    let result = context.runtime.complete_lower_read(
        &trackpad,
        information as u64,
        packet,
        query_performance_counter,
    );
    match result {
        Ok(report) => {
            let status = copy_report(&ptp_request, &report);
            ptp_request.complete(status);
        }
        Err(status) => {
            tlg::write_event!(
                PROVIDER,
                "LowerReadTooShort",
                level(Warning),
                keyword(keyword::HID_INPUT),
                u64("Information", &(information as u64)),
                win_ntstatus("Status", &status)
            );
            ptp_request.complete(status);
        }
    }
}

/// Input.c:266-306: `WdfRequestRetrieveOutputMemory`, `WdfMemoryCopyFromBuffer(mem, 0, report, 30)`,
/// `WdfRequestSetInformation(30)`; returns the status to complete with.
fn copy_report(request: &Request, report: &[u8; SPI_PTP_REPORT_LEN]) -> NTSTATUS {
    let mut memory: WDFMEMORY = null_mut();
    // SAFETY: the caller owns `request`; valid out-parameter.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestRetrieveOutputMemory,
            request.as_raw(),
            &raw mut memory
        )
    };
    if !NT_SUCCESS(status) {
        trace_report_copy_failure(b"WdfRequestRetrieveOutputMemory", status);
        return status;
    }
    // SAFETY: `memory` is the request's output memory; the source is the
    // 30-byte local report, which WDF only reads.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfMemoryCopyFromBuffer,
            memory,
            0,
            report.as_ptr().cast_mut().cast(),
            report.len()
        )
    };
    if !NT_SUCCESS(status) {
        trace_report_copy_failure(b"WdfMemoryCopyFromBuffer", status);
        return status;
    }
    // Lossless: 30.
    request.set_information(SPI_PTP_REPORT_LEN as ULONG_PTR);
    STATUS_SUCCESS
}

/// Error trace for [`copy_report`].
fn trace_report_copy_failure(step: &[u8], status: NTSTATUS) {
    tlg::write_event!(
        PROVIDER,
        "ReportCopyFailed",
        level(Error),
        keyword(keyword::HID_INPUT),
        str8("Step", step),
        win_ntstatus("Status", &status)
    );
}

//! SPI read path (`Input.c`): two-phase lower-read ownership (`prepare` / `complete_send`), lower
//! `NTSTATUS` and packet-length checks, replenishment retry on transient failures, host-synced
//! `ContactTracker` commit, and FIFO-serialized upper `WdfRequestComplete`.

use core::ptr::{null, null_mut, write_bytes};

use amtptp_core::hid::delivery::EnqueueOutcome;
use amtptp_core::hid::input::SPI_PTP_REPORT_LEN;
use amtptp_core::spi::runtime::{ClaimRefused, LowerReadOrigin, PostSendAction, ReadTicket};
use amtptp_core::spi::wire::LOWER_READ_IOCTL;
use tracelogging as tlg;
use wdf_support::declare_context_type;
use wdf_support::handle::{ContextPtr, WdfHandle};
use wdf_support::init::object_attributes_init_context_type;
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_DEVICE_DATA_ERROR, STATUS_SUCCESS};
use wdf_support::queue::{cancel_and_dereference_read, drain_completions, reference_read};
use wdf_support::request::Request;
use wdf_support::time::query_performance_counter;
use wdk_sys::{
    NTSTATUS, PWDF_REQUEST_COMPLETION_PARAMS, ULONG, ULONG_PTR, WDFCONTEXT, WDFDEVICE, WDFIOTARGET,
    WDFMEMORY, WDFMEMORY__, WDFOBJECT, WDFREQUEST, call_unsafe_wdf_function_binding,
};

use crate::device::DeviceContext;
use crate::{PROVIDER, keyword};

/// Context attached to every driver-created lower SPI read request.
pub(crate) struct WorkerRequestContext {
    device: WdfHandle<wdk_sys::WDFDEVICE__>,
    device_context: ContextPtr<DeviceContext>,
    request_memory: WdfHandle<WDFMEMORY__>,
    ticket: ReadTicket,
}

declare_context_type!(
    pub(crate) WorkerRequestContext,
    WORKER_REQUEST_CONTEXT_TYPE_INFO,
    worker_request_get_context,
    name = "WORKER_REQUEST_CONTEXT"
);

/// Parks an upstream `IOCTL_HID_READ_REPORT` in `HidQueue` and issues one lower read if configured.
///
/// # Safety
/// `device` is the request's live device and `context` its context; IRQL <= `DISPATCH_LEVEL`.
pub(crate) unsafe fn spi_input_routine_worker(
    device: WDFDEVICE,
    context: &DeviceContext,
    request: Request,
) {
    // SAFETY: `hid_queue` is the device's manual queue created in `AddDevice`.
    if let Err(rejected) = unsafe { request.forward_to_io_queue(context.hid_queue.get()) } {
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

/// Deletes a driver-created WDF object.
///
/// # Safety
/// `object` must be a live driver-owned WDF object not used afterwards.
unsafe fn delete_object(object: WDFOBJECT) {
    // SAFETY: caller contract.
    unsafe { call_unsafe_wdf_function_binding!(WdfObjectDelete, object) };
}

/// Issues one lower read per unserved upstream request waiting in `HidQueue`.
///
/// # Safety
/// `device` is a live device of this driver and `context` its context; IRQL <= `DISPATCH_LEVEL`.
pub(crate) unsafe fn issue_backlog_reads(device: WDFDEVICE, context: &DeviceContext) -> u32 {
    let mut queued: ULONG = 0;
    // SAFETY: `hid_queue` is the device's manual queue; `DriverRequests` is optional (NULL).
    let _state = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueGetState,
            context.hid_queue.get(),
            &raw mut queued,
            null_mut()
        )
    };
    // SAFETY: caller contract; closure only inspects `reads`.
    let reads = unsafe {
        context.with_reads(|runtime, tracker| runtime.backlog_reads(tracker, queued as usize))
    };
    for _ in 0..reads {
        // SAFETY: forwarded caller contract.
        unsafe { spi_input_issue_request(device, context) };
    }
    u32::try_from(reads).unwrap_or(u32::MAX)
}

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

/// Creates, formats, registers (`Preparing`), and sends one asynchronous lower read.
/// On allocation/format/send failure while configured, schedules replenishment via `handle_read_failure`.
///
/// # Safety
/// `device` is a live device of this driver and `context` its context; IRQL <= `DISPATCH_LEVEL`.
pub(crate) unsafe fn spi_input_issue_request(device: WDFDEVICE, context: &DeviceContext) {
    let target: WDFIOTARGET = context.spi_trackpad_io_target.get();

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
        // SAFETY: live device and context.
        unsafe { context.handle_read_failure(device) };
        return;
    }

    let mut memory: WDFMEMORY = null_mut();
    // SAFETY: lookaside list was created in `AddDevice`.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfMemoryCreateFromLookaside,
            context.hid_read_buffer_lookaside.get(),
            &raw mut memory
        )
    };
    if !NT_SUCCESS(status) {
        trace_issue_failure(b"WdfMemoryCreateFromLookaside", status);
        // SAFETY: created above, never sent; live device.
        unsafe {
            delete_object(lower_request.cast());
            context.handle_read_failure(device);
        }
        return;
    }

    let mut buffer_size: usize = 0;
    // SAFETY: `memory` was just created; valid size out-parameter.
    let buffer = unsafe {
        call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, memory, &raw mut buffer_size)
    };
    if !buffer.is_null() {
        // SAFETY: exclusive lookaside buffer before send.
        unsafe { write_bytes(buffer.cast::<u8>(), 0, buffer_size) };
    }

    // SAFETY: `lower_request` was created with `WORKER_REQUEST_CONTEXT_TYPE_INFO`.
    let worker_context = unsafe { worker_request_get_context(lower_request) };
    if worker_context.is_null() {
        // SAFETY: both created above, never sent.
        unsafe {
            delete_object(memory.cast());
            delete_object(lower_request.cast());
        }
        return;
    }

    // SAFETY: valid target, request, and output memory.
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
        // SAFETY: both created above, never sent; live device.
        unsafe {
            delete_object(memory.cast());
            delete_object(lower_request.cast());
            context.handle_read_failure(device);
        }
        return;
    }

    // SAFETY: valid driver-created request and request context pointer.
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestSetCompletionRoutine,
            lower_request,
            Some(request_completion_routine),
            worker_context.cast()
        );
    }

    let handle = WdfHandle::new(lower_request);
    // SAFETY: caller contract; reserves slot in `Preparing` state under lock.
    let prepared =
        unsafe { context.with_reads(|runtime, reads| runtime.prepare_lower_read(reads, handle)) };
    let ticket = match prepared {
        Ok(ticket) => ticket,
        Err(refused) => {
            tlg::write_event!(
                PROVIDER,
                "LowerReadNotClaimed",
                level(Verbose),
                keyword(keyword::DEVICE),
                bool8(
                    "TooManyInFlight",
                    &(refused == ClaimRefused::TooManyInFlight)
                )
            );
            // SAFETY: both created above, never sent.
            unsafe {
                delete_object(memory.cast());
                delete_object(lower_request.cast());
            }
            return;
        }
    };

    // SAFETY: request is not sent yet, so completion cannot race with this context initialization.
    unsafe {
        worker_context.write(WorkerRequestContext {
            device: WdfHandle::new(device),
            device_context: ContextPtr::new(context),
            request_memory: WdfHandle::new(memory),
            ticket,
        });
    }

    // SAFETY: formatted request for `target`; completion routine may run synchronously here.
    let sent = unsafe {
        call_unsafe_wdf_function_binding!(WdfRequestSend, lower_request, target, null_mut())
    } != 0;

    // SAFETY: updates tracker state under lock; if `CancelSent`, references `lower_request` before
    // releasing the lock so a concurrent completion cannot delete it before cancel runs.
    let post_send = unsafe {
        context.with_reads(|runtime, reads| {
            let action = runtime.complete_lower_read_send(reads, handle, ticket, sent);
            if action == PostSendAction::CancelSent {
                reference_read(lower_request);
            }
            action
        })
    };

    match post_send {
        PostSendAction::None => {}
        PostSendAction::CancelSent => {
            // SAFETY: referenced under lock above, and `WdfRequestSend` returned `true`.
            unsafe { cancel_and_dereference_read(lower_request) };
        }
        PostSendAction::DeleteUnsent => {
            tlg::write_event!(
                PROVIDER,
                "LowerReadSendFailed",
                level(Warning),
                keyword(keyword::DEVICE)
            );
            // SAFETY: send returned `false`, so completion will never run; live device.
            unsafe {
                delete_object(memory.cast());
                delete_object(lower_request.cast());
                context.handle_read_failure(device);
            }
        }
    }
}

/// Completion outcome staged under the single `reads` spinlock.
enum CompletionStep {
    Stale,
    LowerFailed(NTSTATUS),
    DroppedNoUpper(NTSTATUS),
    CompleteUpper {
        outcome: EnqueueOutcome<(Request, NTSTATUS)>,
        data_err: Option<(u64, NTSTATUS)>,
        copy_err: Option<(&'static [u8], NTSTATUS)>,
    },
}

/// Lower-read completion routine (`IRQL <= DISPATCH_LEVEL`).
unsafe extern "C" fn request_completion_routine(
    spi_request: WDFREQUEST,
    _target: WDFIOTARGET,
    params: PWDF_REQUEST_COMPLETION_PARAMS,
    context: WDFCONTEXT,
) {
    let worker_context = context.cast::<WorkerRequestContext>().cast_const();
    // SAFETY: `worker_context` lives until `spi_request` is deleted at the end of this function.
    let (device_handle, device_context, request_memory, ticket) = unsafe {
        match worker_context.as_ref() {
            Some(worker) => (
                worker.device.get(),
                worker.device_context.get(),
                worker.request_memory.get(),
                worker.ticket,
            ),
            None => (null_mut(), null(), null_mut(), ReadTicket::default()),
        }
    };

    // SAFETY: `device_context` outlives every child `spi_request`.
    if let Some(ctx) = unsafe { device_context.as_ref() } {
        // SAFETY: valid completed `spi_request` and completion `params`.
        let lower_status =
            unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetStatus, spi_request) };
        // SAFETY: valid completed `spi_request`.
        let information =
            unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetInformation, spi_request) };
        // SAFETY: `params` is valid during this callback.
        let packet = unsafe { lower_packet_slice(params) };
        let trackpad = ctx.config.trackpad_info();

        // SAFETY: single atomic critical section covering epoch check, upper-request retrieval,
        // staged `ContactTracker` update, output copy, and FIFO `CompletionQueue` enqueue.
        let step = unsafe {
            ctx.reads.with(|(reads, contacts, completions)| {
                let origin =
                    ctx.runtime
                        .finish_lower_read(reads, WdfHandle::new(spi_request), ticket);
                if origin == LowerReadOrigin::Stale {
                    return CompletionStep::Stale;
                }
                if !NT_SUCCESS(lower_status) {
                    return CompletionStep::LowerFailed(lower_status);
                }
                ctx.runtime.reset_read_failures();

                let mut raw_ptp: WDFREQUEST = null_mut();
                let q_status = call_unsafe_wdf_function_binding!(
                    WdfIoQueueRetrieveNextRequest,
                    ctx.hid_queue.get(),
                    &raw mut raw_ptp
                );
                if !NT_SUCCESS(q_status) || raw_ptp.is_null() {
                    return CompletionStep::DroppedNoUpper(q_status);
                }
                let ptp_request = Request::from_raw(raw_ptp, STATUS_DEVICE_DATA_ERROR);

                let mut staged = *contacts;
                match ctx.runtime.complete_lower_read(
                    &trackpad,
                    &mut staged,
                    information as u64,
                    packet,
                    query_performance_counter,
                ) {
                    Ok(report) => match copy_report(&ptp_request, &report) {
                        Ok(()) => {
                            *contacts = staged;
                            CompletionStep::CompleteUpper {
                                outcome: completions.enqueue((ptp_request, STATUS_SUCCESS)),
                                data_err: None,
                                copy_err: None,
                            }
                        }
                        Err((step_name, err)) => CompletionStep::CompleteUpper {
                            outcome: completions.enqueue((ptp_request, err)),
                            data_err: None,
                            copy_err: Some((step_name, err)),
                        },
                    },
                    Err(err) => CompletionStep::CompleteUpper {
                        outcome: completions.enqueue((ptp_request, err)),
                        data_err: Some((information as u64, err)),
                        copy_err: None,
                    },
                }
            })
        };

        match step {
            CompletionStep::Stale => {
                tlg::write_event!(
                    PROVIDER,
                    "StaleLowerReadDropped",
                    level(Verbose),
                    keyword(keyword::HID_INPUT)
                );
            }
            CompletionStep::LowerFailed(err) => {
                tlg::write_event!(
                    PROVIDER,
                    "LowerReadCompletedWithError",
                    level(Warning),
                    keyword(keyword::HID_INPUT),
                    win_ntstatus("Status", &err)
                );
                if !device_handle.is_null() {
                    // SAFETY: live device handle and context.
                    unsafe { ctx.handle_read_failure(device_handle) };
                }
            }
            CompletionStep::DroppedNoUpper(q_status) => {
                tlg::write_event!(
                    PROVIDER,
                    "LowerReadDropped",
                    level(Verbose),
                    keyword(keyword::HID_INPUT),
                    win_ntstatus("Status", &q_status)
                );
            }
            CompletionStep::CompleteUpper {
                outcome,
                data_err,
                copy_err,
            } => {
                if let Some((info, err)) = data_err {
                    tlg::write_event!(
                        PROVIDER,
                        "LowerReadTooShort",
                        level(Warning),
                        keyword(keyword::HID_INPUT),
                        u64("Information", &info),
                        win_ntstatus("Status", &err)
                    );
                }
                if let Some((step_name, err)) = copy_err {
                    trace_report_copy_failure(step_name, err);
                }
                // SAFETY: `ctx.reads` is initialized and NOT held here; IRQL <= DISPATCH_LEVEL.
                unsafe {
                    drain_completions(
                        &ctx.reads,
                        outcome,
                        |(_, _, q)| q,
                        |(req, st)| req.complete(st),
                    );
                }
            }
        }
    }

    // SAFETY: lower request is complete and no longer referenced; delete request then memory.
    unsafe { delete_object(spi_request.cast()) };
    if !request_memory.is_null() {
        // SAFETY: lookaside buffer created for `spi_request`, no longer referenced.
        unsafe { delete_object(request_memory.cast()) };
    }
}

/// Extracts the read-only lookaside buffer slice from `params`.
///
/// # Safety
/// `params` must be the valid completion parameters of a completed internal-IOCTL request whose
/// output buffer remains alive for the returned slice's lifetime.
unsafe fn lower_packet_slice<'a>(params: PWDF_REQUEST_COMPLETION_PARAMS) -> &'a [u8] {
    // SAFETY: caller guarantees `params` validity for an internal IOCTL completion.
    let output_memory = unsafe {
        params
            .as_ref()
            .map_or(null_mut(), |p| p.Parameters.Ioctl.Output.Buffer)
    };
    if output_memory.is_null() {
        return &[];
    }
    let mut buffer_size: usize = 0;
    // SAFETY: `output_memory` is the live lookaside memory formatted on the lower read.
    let buffer = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfMemoryGetBuffer,
            output_memory,
            &raw mut buffer_size
        )
    };
    if buffer.is_null() {
        &[]
    } else {
        // SAFETY: buffer of `buffer_size` bytes, written by lower driver before completion.
        unsafe { core::slice::from_raw_parts(buffer.cast::<u8>().cast_const(), buffer_size) }
    }
}

fn copy_report(
    request: &Request,
    report: &[u8; SPI_PTP_REPORT_LEN],
) -> Result<(), (&'static [u8], NTSTATUS)> {
    let mut memory: WDFMEMORY = null_mut();
    // SAFETY: caller owns `request`; `memory` is writable.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfRequestRetrieveOutputMemory,
            request.as_raw(),
            &raw mut memory
        )
    };
    if !NT_SUCCESS(status) {
        return Err((b"WdfRequestRetrieveOutputMemory", status));
    }
    // SAFETY: `memory` is valid output memory; `report` is 30 initialized stack bytes.
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
        return Err((b"WdfMemoryCopyFromBuffer", status));
    }
    request.set_information(SPI_PTP_REPORT_LEN as ULONG_PTR);
    Ok(())
}

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

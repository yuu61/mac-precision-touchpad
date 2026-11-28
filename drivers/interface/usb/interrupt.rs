//! Continuous-reader callbacks (`EvtUsbTargetPipeReadComplete`, `EvtUsbTargetPipeReadersFailed`).
//!
//! `evt_usb_interrupt_pipe_read_complete` runs at IRQL <= `DISPATCH_LEVEL`:
//! 1. Validate framing (and trigger mode recovery if raw mode received a mouse/other packet) before
//!    touching `InputQueue` or `ContactTracker`.
//! 2. Inside the `pipeline` spinlock, dequeue the oldest `READ_REPORT` from `InputQueue`, build and
//!    copy the 50-byte PTP report, commit `ContactTracker` only if the copy succeeded (so dropped
//!    frames when `InputQueue` is empty never desynchronize liftoffs from the host), and enqueue
//!    the completion into [`amtptp_core::hid::delivery::CompletionQueue`].
//! 3. Outside the spinlock, drain `WdfRequestComplete` in strict generation FIFO order.

use core::ptr;
use core::slice;
use core::sync::atomic::Ordering;

use amtptp_usb_bt::usb::input;
use amtptp_usb_bt::usb::lifecycle::{self, ReaderFailureAction};
use amtptp_usb_bt::usb::status::Completion;
use tracelogging as tlg;
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_UNSUCCESSFUL};
use wdf_support::queue::drain_completions;
use wdf_support::request::Request;
use wdf_support::time::query_performance_counter;
use wdk_sys::{
    _WDF_DEVICE_FAILED_ACTION, BOOLEAN, NTSTATUS, USBD_STATUS, WDFCONTEXT, WDFMEMORY, WDFREQUEST,
    WDFUSBPIPE, call_unsafe_wdf_function_binding,
};

use crate::PROVIDER;
use crate::context::DeviceContext;
use crate::queue::{as_output, complete, retrieve_output_memory};
use crate::trace::KW_INPUT;

/// `EvtUsbTargetPipeReadComplete` of the continuous reader. IRQL <= `DISPATCH_LEVEL`, non-paged.
pub(crate) unsafe extern "C" fn evt_usb_interrupt_pipe_read_complete(
    _pipe: WDFUSBPIPE,
    buffer: WDFMEMORY,
    num_bytes_transferred: usize,
    context: WDFCONTEXT,
) {
    // SAFETY: `context` is the `DeviceContext` pointer configured on the continuous reader, alive
    // until `D0Exit` stops the pipe before device deletion.
    let Some(context) = (unsafe { context.cast::<DeviceContext>().cast_const().as_ref() }) else {
        return;
    };

    let mut size: usize = 0;
    // SAFETY: `buffer` is the reader's live `WDFMEMORY` for the duration of this callback.
    let data =
        unsafe { call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, buffer, &raw mut size) };
    if data.is_null() {
        tlg::write_event!(
            PROVIDER,
            "ReaderBufferMissing",
            level(Informational),
            keyword(KW_INPUT)
        );
        return;
    }
    // SAFETY: WDF's reader buffer of `size` bytes, read-only during this callback.
    let whole = unsafe { slice::from_raw_parts(data.cast::<u8>().cast_const(), size) };
    let Some(packet) = whole.get(..num_bytes_transferred) else {
        tlg::write_event!(
            PROVIDER,
            "MalformedInput",
            level(Informational),
            keyword(KW_INPUT),
            usize("Length", &num_bytes_transferred)
        );
        return;
    };

    if input::needs_mode_recovery(&context.state, packet) {
        if context.recovery_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        // SAFETY: live device context owns the initialized pipeline lock and work item.
        unsafe {
            context.reset_contacts();
            call_unsafe_wdf_function_binding!(
                WdfWorkItemEnqueue,
                context.mode_recovery.load(Ordering::Acquire)
            );
        }
        return;
    }

    if !context.state.wellspring_mode_on() {
        // SAFETY: live device context; spinlock created in `create_device`.
        unsafe { context.reset_contacts() };
        return;
    }

    if !input::is_valid_packet(&context.state, packet) {
        tlg::write_event!(
            PROVIDER,
            "MalformedInput",
            level(Informational),
            keyword(KW_INPUT),
            usize("Length", &num_bytes_transferred)
        );
        return;
    }

    context.state.reset_reader_failures();
    let input_queue = context.input_queue();
    if input_queue.is_null() {
        return;
    }
    let counter = query_performance_counter();

    // SAFETY: `pipeline` spinlock is initialized; `WdfIoQueueRetrieveNextRequest` and
    // `WdfMemoryCopyFromBuffer` on non-paged HID buffers are allowed at `DISPATCH_LEVEL` and do
    // not invoke callbacks on this manual queue; `WdfRequestComplete` runs outside the lock below.
    let staged = unsafe {
        context.pipeline.with(|(contacts, completions)| {
            let mut raw_request: WDFREQUEST = ptr::null_mut();
            let status = call_unsafe_wdf_function_binding!(
                WdfIoQueueRetrieveNextRequest,
                input_queue,
                &raw mut raw_request
            );
            if !NT_SUCCESS(status) || raw_request.is_null() {
                return None;
            }
            let request = Request::from_raw(raw_request, STATUS_UNSUCCESSFUL);
            let mut output = retrieve_output_memory(&request);
            let retrieve_err = output.as_ref().err().copied();
            let Some((report, completion)) = input::deliver_packet_to(
                &context.state,
                contacts,
                packet,
                counter,
                as_output(&mut output),
            ) else {
                return Some((
                    completions.enqueue((request, Completion::status(STATUS_UNSUCCESSFUL))),
                    retrieve_err,
                    (0_u8, 0_u8, STATUS_UNSUCCESSFUL),
                ));
            };
            let contact_count = report.get(48).copied().unwrap_or(0);
            let button = report.get(49).copied().unwrap_or(0);
            let status = completion.status;
            Some((
                completions.enqueue((request, completion)),
                retrieve_err,
                (contact_count, button, status),
            ))
        })
    };

    let Some((outcome, retrieve_err, (contacts_count, button, status))) = staged else {
        tlg::write_event!(
            PROVIDER,
            "NoPendingReadReport",
            level(Verbose),
            keyword(KW_INPUT)
        );
        return;
    };

    if let Some(err) = retrieve_err {
        tlg::write_event!(
            PROVIDER,
            "ReadReportRetrieveOutputMemoryFailed",
            level(Error),
            keyword(KW_INPUT),
            win_ntstatus("Status", &err)
        );
    }
    if NT_SUCCESS(status) {
        tlg::write_event!(
            PROVIDER,
            "InputReport",
            level(Verbose),
            keyword(KW_INPUT),
            u8("ContactCount", &contacts_count),
            u8("Button", &button)
        );
    } else {
        tlg::write_event!(
            PROVIDER,
            "ReadReportCompletionFailed",
            level(Error),
            keyword(KW_INPUT),
            win_ntstatus("Status", &status)
        );
    }

    // SAFETY: `pipeline` lock is initialized and NOT held here; IRQL <= `DISPATCH_LEVEL`.
    unsafe {
        drain_completions(
            &context.pipeline,
            outcome,
            |(_, q)| q,
            |(req, comp)| {
                complete(req, comp);
            },
        );
    }
}

/// `EvtUsbTargetPipeReadersFailed`: bounds pipe-reset retries and escalates persistent hardware
/// faults to `WdfDeviceSetFailed(AttemptRestart)` instead of spinning forever.
pub(crate) unsafe extern "C" fn evt_usb_interrupt_readers_failed(
    pipe: WDFUSBPIPE,
    status: NTSTATUS,
    usbd_status: USBD_STATUS,
) -> BOOLEAN {
    tlg::write_event!(
        PROVIDER,
        "ReadersFailed",
        level(Warning),
        keyword(KW_INPUT),
        win_ntstatus("Status", &status),
        i32_hex("UsbdStatus", &usbd_status)
    );
    let target = crate::usb::pipe_io_target(pipe);
    if target.is_null() {
        return BOOLEAN::from(false);
    }
    // SAFETY: `pipe` (and its I/O target) is a live object owned by one of this driver's `WDFDEVICE`s.
    let device = unsafe { call_unsafe_wdf_function_binding!(WdfIoTargetGetDevice, target) };
    // SAFETY: `device` belongs to this driver; IRQL <= `DISPATCH_LEVEL`.
    let Some(context) = (unsafe { DeviceContext::of(device) }) else {
        return BOOLEAN::from(false);
    };

    match lifecycle::on_readers_failed(&context.state, status, usbd_status) {
        ReaderFailureAction::Restart => BOOLEAN::from(true),
        ReaderFailureAction::Stop => BOOLEAN::from(false),
        ReaderFailureAction::FailDevice => {
            tlg::write_event!(
                PROVIDER,
                "ReadersFailedEscalated",
                level(Error),
                keyword(KW_INPUT),
                win_ntstatus("Status", &status)
            );
            // SAFETY: `device` is live and `WdfDeviceSetFailed` is callable at <= `DISPATCH_LEVEL`.
            unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfDeviceSetFailed,
                    device,
                    _WDF_DEVICE_FAILED_ACTION::WdfDeviceFailedAttemptRestart
                );
            }
            BOOLEAN::from(false)
        }
    }
}

//! The continuous reader's callbacks (Interrupt.c:89-264).
//!
//! `AmtPtpEvtUsbInterruptPipeReadComplete` runs at IRQL <= `DISPATCH_LEVEL` for every packet of the
//! interrupt pipe: nothing reachable from here is paged, blocks or allocates.
//!
//! Order (C → here):
//! 1. length gate → [`input::process_packet`] returns `Discard`: drop the packet, the `InputQueue` is not
//!    touched;
//! 2. `WdfMemoryGetBuffer(Buffer)` (the C calls it after the gate; it has no side effect);
//! 3. `WdfIoQueueRetrieveNextRequest(InputQueue)`; none → the packet is lost;
//! 4. `WdfRequestRetrieveOutputMemory`; failure → C leaks the request (B6), here it is completed with
//!    that status (P2);
//! 5. `ScanTime` uses the current performance counter and the frequency cached at D0 entry;
//! 6. more than 5 fingers → `WdfRequestComplete(STATUS_DATA_ERROR)` (B8);
//! 7. `WdfMemoryCopyFromBuffer(mem, 0, report, 50)`; failure → C leaks the request (B7), here it is
//!    completed with the copy status (P2);
//! 8. `WdfRequestSetInformation(50)`, `WdfRequestComplete(status)`.

use core::ptr;
use core::slice;

use amtptp_usbkm_core::input::{self, PacketAction, ReadReportResult};
use amtptp_usbkm_core::lifecycle;
use tracelogging as tlg;
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_UNSUCCESSFUL};
use wdf_support::request::Request;
use wdf_support::time::query_performance_counter;
use wdk_sys::{
    BOOLEAN, NTSTATUS, USBD_STATUS, WDFCONTEXT, WDFMEMORY, WDFREQUEST, WDFUSBPIPE,
    call_unsafe_wdf_function_binding,
};

use crate::PROVIDER;
use crate::context::DeviceContext;
use crate::queue::{as_output, complete, retrieve_output_memory};
use crate::trace::KW_INPUT;

/// `AmtPtpEvtUsbInterruptPipeReadComplete` (Interrupt.c:89-250): `EvtUsbTargetPipeReadComplete` of the
/// continuous reader. `context` is the device context given at reader configuration. IRQL <=
/// `DISPATCH_LEVEL`, non-paged.
pub(crate) unsafe extern "C" fn evt_usb_interrupt_pipe_read_complete(
    _pipe: WDFUSBPIPE,
    buffer: WDFMEMORY,
    num_bytes_transferred: usize,
    context: WDFCONTEXT,
) {
    // SAFETY: `context` is the `DeviceContext` pointer passed to WDF_USB_CONTINUOUS_READER_CONFIG_INIT.
    // The context lives as long as the device, and the reader is stopped (D0Exit) before the device is
    // deleted; it is only accessed through shared references.
    let Some(context) = (unsafe { context.cast::<DeviceContext>().cast_const().as_ref() }) else {
        return;
    };

    let mut size: usize = 0;
    // SAFETY: `buffer` is the reader's live WDFMEMORY for the duration of this callback.
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
    // SAFETY: WDF's reader buffer of `size` bytes, valid and not written by anyone else during the
    // callback.
    let whole = unsafe { slice::from_raw_parts(data.cast::<u8>().cast_const(), size) };
    // N > buffer size cannot happen (N <= TransferLength = size); such a packet is treated as malformed.
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

    let counter = query_performance_counter();
    // SAFETY: live device context; spinlock created in `create_device`; IRQL <= DISPATCH_LEVEL;
    // pure packet decoding and contact tracking inside the lock.
    let action = unsafe {
        context
            .contacts
            .with(|contacts| input::process_packet(&context.state, contacts, packet, counter))
    };
    let result = match action {
        PacketAction::Discard => {
            tlg::write_event!(
                PROVIDER,
                "MalformedInput",
                level(Informational),
                keyword(KW_INPUT),
                usize("Length", &num_bytes_transferred)
            );
            return;
        }
        PacketAction::Deliver(result) => result,
    };

    let input_queue = context.input_queue();
    if input_queue.is_null() {
        return;
    }
    let mut raw_request: WDFREQUEST = ptr::null_mut();
    // SAFETY: `input_queue` is the device's live manual InputQueue; valid out-pointer; <= DISPATCH_LEVEL.
    let status = unsafe {
        call_unsafe_wdf_function_binding!(
            WdfIoQueueRetrieveNextRequest,
            input_queue,
            &raw mut raw_request
        )
    };
    if !NT_SUCCESS(status) || raw_request.is_null() {
        tlg::write_event!(
            PROVIDER,
            "NoPendingReadReport",
            level(Verbose),
            keyword(KW_INPUT)
        );
        return;
    }
    // SAFETY: WdfIoQueueRetrieveNextRequest handed this request to the driver (driver-owned, not
    // cancelable); it is completed exactly once below. Drop fallback (unreachable): STATUS_UNSUCCESSFUL.
    let request = unsafe { Request::from_raw(raw_request, STATUS_UNSUCCESSFUL) };

    let completion = {
        let mut output = retrieve_output_memory(&request);
        if let Err(status) = output {
            // B6 (P2): the C returns here without completing the request.
            tlg::write_event!(
                PROVIDER,
                "ReadReportRetrieveOutputMemoryFailed",
                level(Error),
                keyword(KW_INPUT),
                win_ntstatus("Status", &status)
            );
        }
        input::complete_read_report_to(&result, as_output(&mut output))
    };
    match result {
        ReadReportResult::DataError => {
            tlg::write_event!(
                PROVIDER,
                "ReadReportDataError",
                level(Error),
                keyword(KW_INPUT),
                usize("Length", &num_bytes_transferred),
                win_ntstatus("Status", &completion.status)
            );
        }
        ReadReportResult::Report(_) if !NT_SUCCESS(completion.status) => {
            // B7 (P2) or B6: the C leaves the request uncompleted.
            tlg::write_event!(
                PROVIDER,
                "ReadReportCompletionFailed",
                level(Error),
                keyword(KW_INPUT),
                win_ntstatus("Status", &completion.status)
            );
        }
        ReadReportResult::Report(report) => {
            let contacts = report.get(48).copied().unwrap_or(0);
            let button = report.get(49).copied().unwrap_or(0);
            tlg::write_event!(
                PROVIDER,
                "InputReport",
                level(Verbose),
                keyword(KW_INPUT),
                u8("ContactCount", &contacts),
                u8("Button", &button)
            );
        }
    }
    complete(request, completion);
}

/// `AmtPtpEvtUsbInterruptReadersFailed` (Interrupt.c:252-264): `EvtUsbTargetPipeReadersFailed`. Returns
/// TRUE ("reset the pipe and restart the readers"), whatever the status; the framework retries
/// indefinitely. IRQL <= `DISPATCH_LEVEL`.
pub(crate) unsafe extern "C" fn evt_usb_interrupt_readers_failed(
    _pipe: WDFUSBPIPE,
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
    BOOLEAN::from(lifecycle::readers_failed(status, usbd_status))
}

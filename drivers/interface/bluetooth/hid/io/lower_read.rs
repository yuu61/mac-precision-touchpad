//! WDF request and memory ownership for lower reads and their completions.

use core::ffi::c_void;
use core::ptr::null_mut;

use amtptp_usb_bt::bluetooth::bth::{PADDED_REPORT_LEN, PaddedReport, unpad_report};
use amtptp_usb_bt::bluetooth::{
    CompletionQueue, ContactTracker, DeviceState, EnqueueOutcome, LowerReadClass, NtStatus,
    PostSendAction, ReadTicket, ReadTracker, STATUS_INVALID_DEVICE_STATE,
};
use tracelogging as tlg;
use wdf_support::handle::{ContextPtr, WdfHandle};
use wdf_support::init::object_attributes_init_context_type;
use wdf_support::ntstatus::NT_SUCCESS;
use wdf_support::queue::{
    cancel_and_dereference_read, cancel_referenced_reads, collect_reads_to_cancel,
    drain_completions, reference_read,
};
use wdk_sys::{
    PWDF_REQUEST_COMPLETION_PARAMS, WDFCONTEXT, WDFIOTARGET, WDFMEMORY, WDFMEMORY_OFFSET,
    WDFREQUEST, call_unsafe_wdf_function_binding,
};

use super::{Filter, LowerReadOutcome, UpperRequest, complete_lower_read, object_delete};
use crate::context::{
    DeviceContext, LowerRead, QueuedCompletion, WORKER_REQUEST_CONTEXT_TYPE_INFO,
    WorkerRequestContext, worker_request_get_context,
};
use crate::{KW_DEVICE, KW_INPUT, PROVIDER};

#[expect(
    clippy::unused_self,
    reason = "Callbacks use the same operation receiver for live requests and for the recording backend."
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "The exclusive receiver limits the lifetime of borrowed framework request and report buffers."
)]
impl Filter<'_> {
    pub(super) fn create_lower_read(&mut self) -> Result<LowerRead, NtStatus> {
        let Some(target) = self.io_target() else {
            return Err(STATUS_INVALID_DEVICE_STATE);
        };
        let mut attributes = object_attributes_init_context_type(&WORKER_REQUEST_CONTEXT_TYPE_INFO);
        attributes.ParentObject = self.ctx.device().cast();
        let mut request: WDFREQUEST = null_mut();
        // SAFETY: `attributes` is initialized (context type, parent = this live device); `target`
        // is the default local I/O target; `request` is a valid out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestCreate,
                &raw mut attributes,
                target,
                &raw mut request
            )
        };
        if NT_SUCCESS(status) {
            Ok(WdfHandle::new(request))
        } else {
            // "This can fail for Bluetooth devices" (Input.c:59): the core arms the 3 s timer.
            tlg::write_event!(
                PROVIDER,
                "LowerReadCreateFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status)
            );
            Err(status)
        }
    }

    pub(super) fn create_lower_read_memory(
        &mut self,
        read: LowerRead,
    ) -> Result<WDFMEMORY, NtStatus> {
        let mut memory: WDFMEMORY = null_mut();
        // SAFETY: the lookaside list was created in AddDevice (parent = device, HF-B15) and lives
        // as long as the device; `memory` is a valid out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfMemoryCreateFromLookaside,
                self.ctx.hid_read_buffer_lookaside(),
                &raw mut memory
            )
        };
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "LowerReadMemoryFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status)
            );
            return Err(status);
        }
        // SAFETY: `read` was just created with WORKER_REQUEST_CONTEXT, so its (zero-filled)
        // context exists and nothing else references it yet (the request is not sent). The ticket
        // is written by `send_lower_read`.
        unsafe {
            worker_request_get_context(read.get()).write(WorkerRequestContext {
                device_context: ContextPtr::new(self.ctx),
                request_memory: WdfHandle::new(memory),
                ticket: ReadTicket::default(),
            });
        }
        Ok(memory)
    }

    pub(super) fn format_lower_read(&mut self, read: LowerRead, memory: WDFMEMORY) -> NtStatus {
        let Some(target) = self.io_target() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        // One padded report: HIDCLASS completes a read with as many whole reports as fit.
        let mut offset = WDFMEMORY_OFFSET {
            BufferOffset: 0,
            BufferLength: PADDED_REPORT_LEN,
        };
        // SAFETY: `read` is a driver-created request for `target`; `memory` is its 1024-byte
        // lookaside buffer, of which the first PADDED_REPORT_LEN bytes are read into; no device
        // offset.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetFormatRequestForRead,
                target,
                read.get(),
                memory,
                &raw mut offset,
                null_mut()
            )
        };
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "LowerReadFormatFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status)
            );
        }
        status
    }

    pub(super) fn set_lower_read_completion(&mut self, read: LowerRead) {
        let read = read.get();
        // SAFETY: `read` is live and carries WORKER_REQUEST_CONTEXT (initialized in
        // `create_lower_read_memory`); its address is the routine's context, as in C.
        unsafe {
            let context = worker_request_get_context(read);
            call_unsafe_wdf_function_binding!(
                WdfRequestSetCompletionRoutine,
                read,
                Some(evt_lower_read_completion),
                context.cast::<c_void>() as WDFCONTEXT
            );
        }
    }

    pub(super) fn send_lower_read(&mut self, read: LowerRead, ticket: ReadTicket) -> bool {
        let Some(target) = self.io_target() else {
            return false;
        };
        let read = read.get();
        // SAFETY: the request is not sent yet, so nothing else reads its context; the context was
        // initialized in `create_lower_read_memory`.
        unsafe { (*worker_request_get_context(read)).ticket = ticket };
        // SAFETY: `read` is formatted for `target` and has a completion routine; NULL options =
        // asynchronous send (Input.c:102). On TRUE the completion routine owns the request.
        let sent =
            unsafe { call_unsafe_wdf_function_binding!(WdfRequestSend, read, target, null_mut()) };
        if sent == 0 {
            tlg::write_event!(
                PROVIDER,
                "LowerReadSendFailed",
                level(Error),
                keyword(KW_DEVICE)
            );
        }
        sent != 0
    }

    pub(super) fn lower_read_buffer(&mut self, memory: WDFMEMORY) -> &[u8] {
        let mut size: usize = 0;
        // SAFETY: `memory` is the live lookaside buffer of the completed lower read (deleted by the
        // core only after the slice's last use); `size` is a valid out pointer.
        let buffer =
            unsafe { call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, memory, &raw mut size) };
        if buffer.is_null() {
            return &[];
        }
        // SAFETY: WDF memory of `size` bytes, written by the lower driver and no longer accessed by
        // it (the request has completed); read-only view.
        unsafe { core::slice::from_raw_parts(buffer.cast::<u8>(), size) }
    }

    pub(super) fn delete_lower_read(&mut self, read: LowerRead) {
        // SAFETY: driver-created request, not pending (never sent, or completed), and no longer in
        // the read tracker (the core unregisters it first).
        unsafe { object_delete(read.get()) };
    }

    pub(super) fn delete_lower_memory(&mut self, memory: WDFMEMORY) {
        // SAFETY: lookaside memory of a request that is not pending; deleted after the request
        // (HF-B10: its handle was read from the request context before the request was deleted).
        unsafe { object_delete(memory) };
    }

    pub(super) fn in_flight_reads(&mut self) -> usize {
        // SAFETY: spinlock created in `create_device`; IRQL <= DISPATCH_LEVEL; pure query.
        unsafe { self.ctx.pipeline.with(|p| p.reads.in_flight()) }
    }

    pub(super) fn reset_contacts(&mut self) {
        // SAFETY: spinlock created in `create_device`; IRQL <= DISPATCH_LEVEL; pure reset.
        unsafe { self.ctx.pipeline.with(|p| p.contacts.reset()) };
    }

    pub(super) fn prepare_lower_read(
        &mut self,
        state: &DeviceState,
        read: LowerRead,
    ) -> Option<ReadTicket> {
        // SAFETY: spinlock created in `create_device`; IRQL <= DISPATCH_LEVEL; pure tracker update.
        unsafe {
            self.ctx.pipeline.with(|p| {
                if state.device_configured() {
                    p.reads.prepare(read)
                } else {
                    None
                }
            })
        }
    }

    pub(super) fn complete_lower_read_send(
        &mut self,
        read: LowerRead,
        ticket: ReadTicket,
        sent: bool,
    ) -> PostSendAction {
        // SAFETY: spinlock created in `create_device`; if `CancelSent` is returned, the completion
        // routine has not yet unregistered or deleted `read`, so `reference_read` is valid under lock.
        let action = unsafe {
            self.ctx.pipeline.with(|p| {
                let action = p.reads.complete_send(read, ticket, sent);
                if action == PostSendAction::CancelSent {
                    reference_read(read.get());
                }
                action
            })
        };
        if action == PostSendAction::CancelSent {
            // SAFETY: outside `pipeline` lock at IRQL <= DISPATCH_LEVEL; `read` was referenced above
            // and `WdfRequestSend` succeeded.
            unsafe { cancel_and_dereference_read(read.get()) };
        }
        action
    }

    pub(super) const fn pack_completion(
        request: UpperRequest,
        status: NtStatus,
    ) -> QueuedCompletion {
        (WdfHandle::new(request), status)
    }

    pub(super) fn with_pipeline<R, F>(&mut self, f: F) -> R
    where
        F: FnOnce(
            &mut Self,
            &mut ReadTracker<LowerRead>,
            &mut ContactTracker,
            &mut CompletionQueue<QueuedCompletion>,
        ) -> R,
    {
        let mut inner = Self {
            ctx: self.ctx,
            trace_input_drop: self.trace_input_drop,
        };
        // SAFETY: `pipeline` spinlock was created in `create_device`; callers run at IRQL <= DISPATCH_LEVEL
        // and never call `WdfRequestSend`, `WdfRequestCancelSentRequest`, `WdfRequestComplete`, or re-enter `pipeline`.
        unsafe {
            self.ctx.pipeline.with(|p| {
                f(
                    &mut inner,
                    &mut p.reads,
                    &mut p.contacts,
                    &mut p.completions,
                )
            })
        }
    }

    pub(super) fn drain_upper_completions(&mut self, outcome: EnqueueOutcome<QueuedCompletion>) {
        // SAFETY: `pipeline` lock is initialized and NOT held here; IRQL <= DISPATCH_LEVEL.
        unsafe {
            drain_completions(
                &self.ctx.pipeline,
                outcome,
                |p| &mut p.completions,
                |(request, status)| self.complete(request.get(), status),
            );
        }
    }

    pub(super) fn cancel_in_flight_reads(&mut self) -> usize {
        // SAFETY: `pipeline` lock was created in `create_device`; `collect_reads_to_cancel` only
        // references live `Sent` lower reads protected by this lock.
        let (held, count) = unsafe {
            self.ctx.pipeline.with(|p| {
                p.contacts.reset();
                let suspended = p.reads.suspend();
                collect_reads_to_cancel(&suspended)
            })
        };
        // SAFETY: outside `pipeline` lock at IRQL <= DISPATCH_LEVEL; `held[..count]` were referenced above.
        unsafe { cancel_referenced_reads(&held, count) };
        count
    }
}

/// `PtpFilterInputRequestCompletionCallback` (Input.c:119-253): completion routine of every lower
/// `IOCTL_HID_READ_REPORT`, at IRQL <= `DISPATCH_LEVEL`.
///
/// Reads the device-context pointer, `RequestMemory` and the read's ticket from the request context
/// **before** anything is deleted (the C code reads `RequestMemory` after deleting the request: HF-B10,
/// P2), and the completion status (C ignored it: HF-B7) and `Information`, then hands over to the
/// core. The core unregisters the read from the tracker, handles it, and deletes the request and then
/// the memory on every path. After that call nothing here touches the request, its context or the
/// memory.
///
/// The packet buffer is read through `RequestMemory`; C calls `WdfMemoryGetBuffer` at entry on
/// `Params->Parameters.Ioctl.Output.Buffer`, which is the same WDFMEMORY (the one formatted as the
/// output buffer), with a NULL size.
unsafe extern "C" fn evt_lower_read_completion(
    request: WDFREQUEST,
    _target: WDFIOTARGET,
    _params: PWDF_REQUEST_COMPLETION_PARAMS,
    context: WDFCONTEXT,
) {
    let request_context = context.cast::<WorkerRequestContext>();
    // SAFETY: `context` is the WORKER_REQUEST_CONTEXT of `request`, initialized before the send
    // and alive until the request is deleted (below, inside the core).
    let (device_context, request_memory, ticket) = unsafe {
        (
            (*request_context).device_context.get(),
            (*request_context).request_memory.get(),
            (*request_context).ticket,
        )
    };
    // SAFETY: the completed request is still ours until we delete it.
    let status = unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetStatus, request) };
    // SAFETY: as above.
    let information =
        unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetInformation, request) };
    // ULONG_PTR -> usize: both 64-bit on x64/ARM64 (checked at compile time), lossless.
    let information = information as usize;
    // SAFETY: the request's own lookaside buffer, which it still owns.
    let information = unsafe { unpad(request_memory, status, information) };
    // SAFETY: the request is a child of the device whose context this is, so the device (and its
    // context) outlives this completion.
    let ctx = unsafe { &*device_context };
    let mut filter = Filter::for_read_completion(ctx);
    let outcome = complete_lower_read(
        &mut filter,
        &ctx.state,
        WdfHandle::new(request),
        request_memory,
        ticket,
        status,
        information,
    );
    trace_lower_read(ctx, outcome, status, information);
}

/// Turns a padded report read from the data source back into what the core expects
/// (`amtptp_usb_bt::bluetooth::bth`): a multitouch report keeps its bytes at the start of the buffer
/// and gets its real length; a mode-lost marker becomes a one-byte report of the original ID, which
/// the core treats as another report (it sends the mode switch again). Anything else is left as it
/// is (the core then sees a 140-byte packet that does not parse).
///
/// # Safety
/// `memory` is the completed read's live lookaside buffer.
unsafe fn unpad(memory: WDFMEMORY, status: NtStatus, information: usize) -> usize {
    if !NT_SUCCESS(status) || information != PADDED_REPORT_LEN {
        return information;
    }
    let mut size = 0_usize;
    // SAFETY: a live lookaside WDFMEMORY; valid size out pointer.
    let buffer =
        unsafe { call_unsafe_wdf_function_binding!(WdfMemoryGetBuffer, memory, &raw mut size) };
    if buffer.is_null() || size < PADDED_REPORT_LEN {
        return information;
    }
    // SAFETY: `size` bytes at `buffer` belong to the memory object, which nothing else touches
    // while its read is being completed.
    let report = unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u8>(), PADDED_REPORT_LEN) };
    match unpad_report(report) {
        Some(PaddedReport::Multitouch(raw)) => raw.len(),
        Some(PaddedReport::ModeLost(report_id)) => {
            if let Some(first) = report.first_mut() {
                *first = report_id;
            }
            1
        }
        None => information,
    }
}

/// Traces the outcomes of a lower read that are not the normal report path.
fn trace_lower_read(
    ctx: &DeviceContext,
    outcome: LowerReadOutcome,
    status: NtStatus,
    information: usize,
) {
    let LowerReadOutcome::Handled(class) = outcome else {
        tlg::write_event!(
            PROVIDER,
            "StaleLowerReadDropped",
            level(Verbose),
            keyword(KW_INPUT),
            win_ntstatus("Status", &status)
        );
        return;
    };
    match class {
        LowerReadClass::UnsupportedVendor => {
            let vendor_id = ctx.state.vendor_id();
            tlg::write_event!(
                PROVIDER,
                "UnsupportedDeviceInput",
                level(Error),
                keyword(KW_INPUT),
                u16_hex("VendorID", &vendor_id)
            );
        }
        LowerReadClass::Failed { status } => {
            tlg::write_event!(
                PROVIDER,
                "LowerReadFailed",
                level(Warning),
                keyword(KW_INPUT),
                win_ntstatus("Status", &status)
            );
        }
        LowerReadClass::Oversize | LowerReadClass::Malformed => {
            tlg::write_event!(
                PROVIDER,
                "MalformedInput",
                level(Informational),
                keyword(KW_INPUT),
                usize("Length", &information)
            );
        }
        LowerReadClass::OtherReport { report_id } => {
            tlg::write_event!(
                PROVIDER,
                "OtherInputReportDropped",
                level(Verbose),
                keyword(KW_INPUT),
                u8_hex("ReportId", &report_id),
                usize("Length", &information)
            );
        }
        LowerReadClass::Empty | LowerReadClass::Report { .. } => {}
    }
}

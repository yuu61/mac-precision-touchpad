//! [`FilterIo`] for the device: one WDF (or kernel) call per trait method, exactly the calls the C
//! code makes in `Input.c`, `Hid.c` and `Device.c` that the oracle records, plus the lower-read
//! completion routine. Pure accessors may differ from C without effect: `WdfMemoryGetBuffer` is
//! called only on the report path of the completion (C: at entry, with a NULL size), and
//! `GET/SET_FEATURE` take their lengths from the `EvtIoInternalDeviceControl` arguments instead of
//! `WdfRequestGetParameters` (the same IRP stack-location fields).
//!
//! The core decides *which* call happens *when* (it is replayed against the C oracle); this module
//! only has to make each call with the C arguments. No method adds its own buffer checks in front
//! of a framework call, so framework-originated statuses (e.g. `STATUS_BUFFER_TOO_SMALL` from
//! `WdfRequestRetrieveOutputMemory` / `WdfMemoryCopyFromBuffer`) are KMDF's, as in C.
//!
//! IRQL: everything here except the mode switch and the attributes query may run at
//! `DISPATCH_LEVEL` (parallel default queue, completion routine) and is non-paged. The two
//! synchronous sends are only reached from `EvtDeviceSelfManagedIoInit`/`Restart` and the
//! passive-level recovery timer, i.e. at `PASSIVE_LEVEL`, as in C.

use core::ffi::c_void;
use core::mem::size_of;
use core::ptr::null_mut;

use amtptp_hidfilter_core::{
    FailedAction, FeatureChange, FilterIo, HID_XFER_PACKET_SIZE, LowerReadClass,
    LowerReadOutcome, NtStatus, ReadTicket, ReadTracker, STATUS_INVALID_DEVICE_REQUEST,
    STATUS_INVALID_DEVICE_STATE, XferBuffer, XferPacket, XferPacketMut, complete_lower_read,
    decode_set_feature,
};
use ptp_hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;
use ptp_hid::read_tracker::MAX_IN_FLIGHT_READS;
use ptp_hid::ioctl::{
    IOCTL_HID_GET_DEVICE_ATTRIBUTES, IOCTL_HID_READ_REPORT, IOCTL_HID_SET_FEATURE,
};
use tracelogging as tlg;
use wdf_support::handle::{ContextPtr, WdfHandle};
use wdf_support::hid as wdf_hid;
use wdf_support::init::{
    memory_descriptor_init_buffer, object_attributes_init, object_attributes_init_context_type,
    request_send_options_init, sync_send_options,
};
use wdf_support::ntstatus::NT_SUCCESS;
use wdf_support::request::Request;
use wdf_support::wdm::{request_wdm_get_irp, set_irp_user_buffer};
use wdk_sys::{
    _WDF_DEVICE_FAILED_ACTION, _WDF_REQUEST_SEND_OPTIONS_FLAGS, HID_DEVICE_ATTRIBUTES,
    HID_XFER_PACKET, PWDF_REQUEST_COMPLETION_PARAMS, ULONG, ULONG_PTR,
    WDF_NO_OBJECT_ATTRIBUTES, WDFCONTEXT, WDFIOTARGET, WDFMEMORY, WDFREQUEST,
    call_unsafe_wdf_function_binding,
};

use crate::context::{
    DeviceContext, LowerRead, WORKER_REQUEST_CONTEXT_TYPE_INFO, WorkerRequestContext,
    worker_request_get_context,
};
use crate::{KW_DEVICE, KW_HID, KW_INPUT, PROVIDER};

const _: () = assert!(size_of::<HID_DEVICE_ATTRIBUTES>() == HID_DEVICE_ATTRIBUTES_LEN);
const _: () = assert!(size_of::<HID_XFER_PACKET>() == amtptp_hidfilter_core::HID_XFER_PACKET_LEN);
const _: () = assert!(size_of::<XferBuffer>() == HID_XFER_PACKET_SIZE);
const _: () = assert!(size_of::<ULONG_PTR>() == size_of::<usize>());

/// The device's framework operations, for one callback invocation. Holds only a shared reference
/// to the device context (its state is atomic or behind the read lock) and a tracing flag, so
/// independent instances can coexist, e.g. when a lower read completes synchronously inside
/// `WdfRequestSend` on the dispatch path.
pub(crate) struct Filter<'a> {
    pub(crate) ctx: &'a DeviceContext,
    /// Trace a failed `WdfIoQueueRetrieveNextRequest` as a dropped input packet (Input.c:176-180).
    /// Only the lower-read completion sets it: `D0Exit` drains `HidReadQueue` until that call fails,
    /// which is not an error there.
    trace_input_drop: bool,
}

impl<'a> Filter<'a> {
    pub(crate) const fn new(ctx: &'a DeviceContext) -> Self {
        Self {
            ctx,
            trace_input_drop: false,
        }
    }

    /// The instance of the lower-read completion routine (see `trace_input_drop`).
    pub(crate) const fn for_read_completion(ctx: &'a DeviceContext) -> Self {
        Self {
            ctx,
            trace_input_drop: true,
        }
    }

    /// `HidIoTarget`, or `None` before the detour set it. The core never issues a lower request
    /// before the detour (it checks `io_target_set`, HF-B25), so `None` is unreachable; it only
    /// keeps a NULL handle away from WDF (which would bugcheck).
    fn io_target(&self) -> Option<WDFIOTARGET> {
        let target = self.ctx.hid_io_target();
        if target.is_null() {
            tlg::write_event!(PROVIDER, "NoIoTarget", level(Error), keyword(KW_DEVICE));
            None
        } else {
            Some(target)
        }
    }
}

/// `WdfObjectDelete(object)`.
///
/// # Safety
/// `object` is a live WDF object this driver may delete (a driver-created request or memory).
unsafe fn object_delete<T>(object: *mut T) {
    // SAFETY: caller contract; WdfObjectDelete is callable at <= DISPATCH_LEVEL.
    unsafe { call_unsafe_wdf_function_binding!(WdfObjectDelete, object.cast::<c_void>()) };
}

impl FilterIo for Filter<'_> {
    type Request = WDFREQUEST;
    type Memory = WDFMEMORY;
    type LowerRead = LowerRead;
    type LowerMemory = WDFMEMORY;
    type ModeSwitchRequest = WDFREQUEST;
    type ModeSwitchMemory = WDFMEMORY;

    // ---- upper requests ------------------------------------------------------------------------

    fn complete(&mut self, request: WDFREQUEST, status: NtStatus) {
        tlg::write_event!(
            PROVIDER,
            "RequestCompleted",
            level(Verbose),
            keyword(KW_HID),
            pointer("Request", &(request as usize)),
            win_ntstatus("Status", &status)
        );
        // SAFETY: the core hands over a framework-delivered request it owns (presented to the
        // dispatch callback or retrieved from HidReadQueue) and calls `complete` at most once per
        // request, as its last use (verified by the fixture mock's double-completion assertion on
        // every record). `complete` consumes the guard, so it completes exactly once here.
        unsafe { Request::from_raw(request, status) }.complete(status);
    }

    fn set_information(&mut self, request: WDFREQUEST, information: usize) {
        // ULONG_PTR is u64 on x64/ARM64 (the only targets): lossless.
        let information = information as ULONG_PTR;
        // SAFETY: `request` is a live request owned by the core (not yet completed).
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestSetInformation, request, information);
        };
    }

    fn retrieve_output_memory(&mut self, request: WDFREQUEST) -> Result<WDFMEMORY, NtStatus> {
        // SAFETY: the dispatcher owns the live request; its output memory is used before completion.
        let output = unsafe { wdf_hid::retrieve_output_memory(request) };
        if let Err(status) = output {
            // Input.c:228 (also reached from the descriptor IOCTLs, where it is an error too).
            tlg::write_event!(
                PROVIDER,
                "RetrieveOutputMemoryFailed",
                level(Error),
                keyword(KW_INPUT),
                win_ntstatus("Status", &status)
            );
        }
        output
    }

    fn copy_to_memory(&mut self, memory: WDFMEMORY, source: &[u8]) -> NtStatus {
        // SAFETY: the memory belongs to the pending request and is writable. The source is
        // a descriptor or a locally produced report that cannot overlap it.
        let status = unsafe { wdf_hid::copy_to_output_memory(memory, source) };
        if !NT_SUCCESS(status) {
            // Input.c:236 (also reached from the descriptor IOCTLs, where it is an error too).
            tlg::write_event!(
                PROVIDER,
                "CopyToOutputMemoryFailed",
                level(Error),
                keyword(KW_INPUT),
                win_ntstatus("Status", &status)
            );
        }
        status
    }

    fn retrieve_output_buffer(
        &mut self,
        request: WDFREQUEST,
        minimum_length: usize,
    ) -> Result<&mut [u8], NtStatus> {
        // SAFETY: the dispatcher owns this live request and keeps it pending while the output is used.
        let (buffer, _) = unsafe { wdf_hid::retrieve_output_buffer(request, minimum_length) }?;
        if buffer.is_null() {
            // Defensive (P2): KMDF does not return success with a NULL buffer for a non-zero
            // minimum length; C would write through it.
            return Err(STATUS_INVALID_DEVICE_REQUEST);
        }
        // SAFETY: on success WDF guarantees an output buffer of at least `minimum_length` bytes
        // that belongs to the request until it is completed; the slice is used before completion
        // (it borrows `self`) and nothing else references that memory meanwhile.
        Ok(unsafe { core::slice::from_raw_parts_mut(buffer, minimum_length) })
    }

    fn get_feature_packet(&mut self, request: WDFREQUEST) -> Option<XferPacketMut<'_>> {
        // SAFETY: the core checks the IOCTL buffer size before requesting the live HIDCLASS packet.
        let packet = unsafe { wdf_hid::read_xfer_packet(request) }?;
        let (buffer, length, report_id) =
            (packet.reportBuffer, packet.reportBufferLen, packet.reportId);
        let report_buffer: &mut [u8] = if buffer.is_null() {
            &mut []
        } else {
            // SAFETY: `reportBuffer`/`reportBufferLen` describe the caller's report buffer, which
            // the C code trusts the same way (kernel caller, HIDCLASS). It stays valid until the
            // request is completed, after the slice's last use.
            unsafe { core::slice::from_raw_parts_mut(buffer, length as usize) }
        };
        // Hid.c:217, 240, 253: with the status of `RequestCompleted` this tells "buffer too small"
        // from "unsupported ID".
        let report_buffer_len = report_buffer.len();
        tlg::write_event!(
            PROVIDER,
            "GetFeature",
            level(Informational),
            keyword(KW_HID),
            u8_hex("ReportId", &report_id),
            usize("ReportBufferLen", &report_buffer_len)
        );
        Some(XferPacketMut {
            report_id,
            report_buffer,
        })
    }

    fn set_feature_packet(&mut self, request: WDFREQUEST) -> Option<XferPacket<'_>> {
        // SAFETY: the core checks the IOCTL buffer size before requesting the live HIDCLASS packet.
        let packet = unsafe { wdf_hid::read_xfer_packet(request) }?;
        let (buffer, length, report_id) =
            (packet.reportBuffer, packet.reportBufferLen, packet.reportId);
        let report_buffer: &[u8] = if buffer.is_null() {
            &[]
        } else {
            // SAFETY: as in `get_feature_packet` (read-only view).
            unsafe { core::slice::from_raw_parts(buffer, length as usize) }
        };
        trace_set_feature(report_id, report_buffer);
        Some(XferPacket {
            report_id,
            report_buffer,
        })
    }

    fn forward_to_read_queue(&mut self, request: WDFREQUEST) -> NtStatus {
        let queue = self.ctx.hid_read_queue();
        // SAFETY: live owned request; `queue` is this device's manual HidReadQueue (created in
        // AddDevice, lives as long as the device). On success the queue owns the request.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestForwardToIoQueue, request, queue)
        };
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "ForwardToReadQueueFailed",
                level(Error),
                keyword(KW_INPUT),
                win_ntstatus("Status", &status)
            );
        }
        status
    }

    fn retrieve_next_parked_read(&mut self) -> Result<WDFREQUEST, NtStatus> {
        let mut request: WDFREQUEST = null_mut();
        // SAFETY: HidReadQueue is a live manual queue of this device; `request` is a valid out
        // pointer. A retrieved request is owned by the core, which completes it.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoQueueRetrieveNextRequest,
                self.ctx.hid_read_queue(),
                &raw mut request
            )
        };
        if NT_SUCCESS(status) {
            Ok(request)
        } else {
            if self.trace_input_drop {
                // Input.c:176-180: no parked upper read, the packet is dropped.
                tlg::write_event!(
                    PROVIDER,
                    "InputDroppedNoParkedRead",
                    level(Error),
                    keyword(KW_INPUT),
                    win_ntstatus("Status", &status)
                );
            }
            Err(status)
        }
    }

    fn parked_read_count(&mut self) -> usize {
        let mut queued: ULONG = 0;
        // SAFETY: HidReadQueue is a live manual queue of this device; DriverRequests is optional (NULL).
        let _state = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoQueueGetState,
                self.ctx.hid_read_queue(),
                &raw mut queued,
                null_mut()
            )
        };
        queued as usize
    }

    fn format_using_current_type(&mut self, request: WDFREQUEST) {
        // SAFETY: live owned request.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestFormatRequestUsingCurrentType, request);
        };
    }

    fn send_and_forget(&mut self, request: WDFREQUEST) -> bool {
        let Some(target) = self.io_target() else {
            return false;
        };
        let mut options = request_send_options_init(
            // Small non-negative enum constant (0x8), as in the C macro use.
            _WDF_REQUEST_SEND_OPTIONS_FLAGS::WDF_REQUEST_SEND_OPTION_SEND_AND_FORGET as ULONG,
        );
        // SAFETY: live owned request formatted with the current type; `target` is the default
        // local I/O target; `options` is initialized. On TRUE the lower driver owns the request.
        let sent = unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestSend, request, target, &raw mut options)
        };
        sent != 0
    }

    fn request_status(&mut self, request: WDFREQUEST) -> NtStatus {
        // SAFETY: live owned request (the send failed, so it is still ours).
        unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetStatus, request) }
    }

    // ---- lower reads (Input.c:39-117) ----------------------------------------------------------

    fn create_lower_read(&mut self) -> Result<LowerRead, NtStatus> {
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

    fn create_lower_read_memory(&mut self, read: LowerRead) -> Result<WDFMEMORY, NtStatus> {
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

    fn format_lower_read(&mut self, read: LowerRead, memory: WDFMEMORY) -> NtStatus {
        let Some(target) = self.io_target() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        // SAFETY: `read` is a driver-created request for `target`; `memory` is its 1024-byte
        // lookaside buffer (output, METHOD_NEITHER: becomes Irp->UserBuffer); no input, no offsets.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetFormatRequestForInternalIoctl,
                target,
                read.get(),
                IOCTL_HID_READ_REPORT,
                null_mut(),
                null_mut(),
                memory,
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

    fn set_lower_read_completion(&mut self, read: LowerRead) {
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

    fn send_lower_read(&mut self, read: LowerRead, ticket: ReadTicket) -> bool {
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

    fn lower_read_buffer(&mut self, memory: WDFMEMORY) -> &[u8] {
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

    fn delete_lower_read(&mut self, read: LowerRead) {
        // SAFETY: driver-created request, not pending (never sent, or completed), and no longer in
        // the read tracker (the core unregisters it first).
        unsafe { object_delete(read.get()) };
    }

    fn delete_lower_memory(&mut self, memory: WDFMEMORY) {
        // SAFETY: lookaside memory of a request that is not pending; deleted after the request
        // (HF-B10: its handle was read from the request context before the request was deleted).
        unsafe { object_delete(memory) };
    }

    fn with_reads<R, F: FnOnce(&mut ReadTracker<LowerRead>) -> R>(&mut self, f: F) -> R {
        // SAFETY: the lock was created in `create_device` before any callback could run; every
        // caller runs at <= DISPATCH_LEVEL; `f` only touches the tracker and atomics (the core's
        // contract), so it neither re-enters the lock nor runs a callback of this driver.
        unsafe { self.ctx.reads.with(f) }
    }

    fn cancel_in_flight_reads(&mut self) -> usize {
        let mut held: [WDFREQUEST; MAX_IN_FLIGHT_READS] = [null_mut(); MAX_IN_FLIGHT_READS];
        let take = |reads: &mut ReadTracker<LowerRead>| {
            let suspended = reads.suspend();
            let mut count = 0_usize;
            for (slot, read) in held.iter_mut().zip(suspended.iter()) {
                // SAFETY: a registered lower read is alive: its completion routine unregisters it
                // under this lock before deleting it. The reference keeps the handle valid after the
                // lock is released. NULL tag, as WdfObjectReference.
                unsafe {
                    call_unsafe_wdf_function_binding!(
                        WdfObjectReferenceActual,
                        read.get().cast(),
                        null_mut(),
                        0,
                        core::ptr::null()
                    );
                }
                *slot = read.get();
                count += 1;
            }
            count
        };
        // SAFETY: as in `with_reads`; `take` only references objects (no callback can run from it).
        let count = unsafe { self.ctx.reads.with(take) };
        for &read in held.iter().take(count) {
            // SAFETY: referenced above, so the handle is valid even if the read has completed and
            // been deleted meanwhile; IRQL <= DISPATCH_LEVEL. Cancelling can run the completion
            // routine synchronously, which takes the read lock: the lock is not held here. A read
            // that was registered but not sent yet only gets its IRP's cancel flag set; its
            // completion is stale anyway.
            unsafe {
                let _cancelled =
                    call_unsafe_wdf_function_binding!(WdfRequestCancelSentRequest, read);
                call_unsafe_wdf_function_binding!(
                    WdfObjectDereferenceActual,
                    read.cast(),
                    null_mut(),
                    0,
                    core::ptr::null()
                );
            }
        }
        count
    }

    // ---- device-wide ---------------------------------------------------------------------------

    fn set_failed(&mut self, action: FailedAction) {
        let failed_action = match action {
            FailedAction::AttemptRestart => {
                _WDF_DEVICE_FAILED_ACTION::WdfDeviceFailedAttemptRestart
            }
            FailedAction::NoRestart => _WDF_DEVICE_FAILED_ACTION::WdfDeviceFailedNoRestart,
        };
        tlg::write_event!(
            PROVIDER,
            "DeviceSetFailed",
            level(Error),
            keyword(KW_INPUT),
            i32("Action", &failed_action)
        );
        // SAFETY: live device; WdfDeviceSetFailed is callable at <= DISPATCH_LEVEL.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfDeviceSetFailed, self.ctx.device(), failed_action);
        };
    }

    fn start_recovery_timer(&mut self, due_time: i64) {
        tlg::write_event!(
            PROVIDER,
            "RecoveryTimerStarted",
            level(Informational),
            keyword(KW_DEVICE),
            i64("DueTime", &due_time)
        );
        // SAFETY: the timer was created in AddDevice (parent = device); WdfTimerStart is callable at
        // <= DISPATCH_LEVEL. Its BOOLEAN (already queued) is ignored, as in C.
        let _was_queued = unsafe {
            call_unsafe_wdf_function_binding!(WdfTimerStart, self.ctx.recovery_timer(), due_time)
        };
    }

    fn stop_recovery_timer(&mut self) {
        // SAFETY: the timer was created in AddDevice (parent = device); Wait = FALSE is allowed at
        // <= DISPATCH_LEVEL. A callback that is already running is not waited for (its mode switch
        // can block up to SYNC_IO_TIMEOUT); it finds the device out of D0 and stops.
        let was_queued = unsafe {
            call_unsafe_wdf_function_binding!(WdfTimerStop, self.ctx.recovery_timer(), 0)
        };
        tlg::write_event!(
            PROVIDER,
            "RecoveryTimerStopped",
            level(Informational),
            keyword(KW_DEVICE),
            bool8("WasQueued", &(was_queued != 0))
        );
    }

    fn enqueue_recovery_work_item(&mut self) {
        // SAFETY: the work item was created in AddDevice (parent = device); WdfWorkItemEnqueue is
        // callable at <= DISPATCH_LEVEL (a no-op if already queued).
        unsafe {
            call_unsafe_wdf_function_binding!(WdfWorkItemEnqueue, self.ctx.recovery_work_item());
        };
    }

    fn query_performance_counter(&mut self) -> i64 {
        wdf_support::time::query_performance_counter()
    }

    fn query_performance_frequency(&mut self) -> i64 {
        wdf_support::time::query_performance_frequency()
    }

    fn query_device_attributes(
        &mut self,
        output: &mut [u8; HID_DEVICE_ATTRIBUTES_LEN],
    ) -> NtStatus {
        let Some(target) = self.io_target() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        // A zeroed, naturally aligned HID_DEVICE_ATTRIBUTES on the stack, as in C (Device.c:205).
        let mut attributes = HID_DEVICE_ATTRIBUTES::default();
        let mut descriptor = memory_descriptor_init_buffer(
            (&raw mut attributes).cast::<c_void>(),
            // 32, checked at compile time above.
            size_of::<HID_DEVICE_ATTRIBUTES>() as ULONG,
        );
        // C sends without options, so without a timeout (HF-B27); the port bounds the wait.
        let mut options = sync_send_options(0);
        // SAFETY: PASSIVE_LEVEL (EvtDeviceSelfManagedIoInit); `target` is the default local
        // target; `descriptor` describes `attributes`, which outlives the synchronous call (also on
        // a timeout: the framework waits for the cancelled request). No request, no input, no
        // BytesReturned (Device.c:212-215).
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetSendInternalIoctlSynchronously,
                target,
                null_mut(),
                IOCTL_HID_GET_DEVICE_ATTRIBUTES,
                null_mut(),
                &raw mut descriptor,
                &raw mut options,
                null_mut()
            )
        };
        // SAFETY: `attributes` is a plain 32-byte C struct without padding (checked above) and
        // `output` is 32 bytes; the regions do not overlap.
        unsafe {
            core::ptr::copy_nonoverlapping(
                (&raw const attributes).cast::<u8>(),
                output.as_mut_ptr(),
                HID_DEVICE_ATTRIBUTES_LEN,
            );
        }
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "DeviceAttributesQueryFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status)
            );
        }
        status
    }

    // ---- mode switch (Device.c:371-418) --------------------------------------------------------

    fn create_mode_switch_request(&mut self) -> Result<WDFREQUEST, NtStatus> {
        let Some(target) = self.io_target() else {
            return Err(STATUS_INVALID_DEVICE_STATE);
        };
        let mut request: WDFREQUEST = null_mut();
        // SAFETY: no attributes (WDF_NO_OBJECT_ATTRIBUTES, as in C); `target` is the default local
        // target; `request` is a valid out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfRequestCreate,
                WDF_NO_OBJECT_ATTRIBUTES,
                target,
                &raw mut request
            )
        };
        if NT_SUCCESS(status) {
            Ok(request)
        } else {
            Err(status)
        }
    }

    fn create_mode_switch_memory(
        &mut self,
        request: WDFREQUEST,
        packet: &mut XferBuffer,
    ) -> Result<WDFMEMORY, NtStatus> {
        let mut attributes = object_attributes_init();
        attributes.ParentObject = request.cast();
        let mut memory: WDFMEMORY = null_mut();
        // SAFETY: `packet` is the 32-byte mode-switch buffer, kept in place by the core until the
        // synchronous send has returned and the request (the memory's parent) has been deleted.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfMemoryCreatePreallocated,
                &raw mut attributes,
                packet.as_mut_ptr().cast::<c_void>(),
                HID_XFER_PACKET_SIZE,
                &raw mut memory
            )
        };
        if NT_SUCCESS(status) {
            Ok(memory)
        } else {
            Err(status)
        }
    }

    fn format_mode_switch(&mut self, request: WDFREQUEST, memory: WDFMEMORY) -> NtStatus {
        let Some(target) = self.io_target() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        // SAFETY: driver-created request for `target`; `memory` (32 bytes) is the input buffer;
        // no output, no offsets (Device.c:386-388).
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetFormatRequestForInternalIoctl,
                target,
                request,
                IOCTL_HID_SET_FEATURE,
                memory,
                null_mut(),
                null_mut(),
                null_mut()
            )
        }
    }

    fn set_mode_switch_user_buffer(
        &mut self,
        request: WDFREQUEST,
        packet: &mut XferBuffer,
    ) -> bool {
        // SAFETY: live driver-created request, formatted, not yet sent.
        let irp = unsafe { request_wdm_get_irp(request) };
        if irp.is_null() {
            return false;
        }
        // SAFETY: we own the unsent IRP; `packet` stays valid and in place until the synchronous
        // send returns (the HID minidriver reads the HID_XFER_PACKET from Irp->UserBuffer).
        unsafe { set_irp_user_buffer(irp, packet.as_mut_ptr().cast::<c_void>()) };
        true
    }

    fn send_mode_switch_synchronously(&mut self, request: WDFREQUEST) -> bool {
        let Some(target) = self.io_target() else {
            return false;
        };
        // C sends without a timeout (HF-B27); the port bounds the wait.
        let mut options = sync_send_options(
            // Small non-negative enum constant (0x2).
            _WDF_REQUEST_SEND_OPTIONS_FLAGS::WDF_REQUEST_SEND_OPTION_SYNCHRONOUS as ULONG,
        );
        // SAFETY: PASSIVE_LEVEL (SelfManagedIoInit/Restart, the passive-level timer or the work
        // item); formatted request for `target`; synchronous, so the mode-switch buffer outlives the
        // request (on a timeout the framework cancels the request and waits for it to complete).
        let sent = unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestSend, request, target, &raw mut options)
        };
        if sent != 0 {
            tlg::write_event!(
                PROVIDER,
                "MultitouchModeSet",
                level(Informational),
                keyword(KW_DEVICE)
            );
        } else {
            tlg::write_event!(
                PROVIDER,
                "MultitouchModeSendFailed",
                level(Error),
                keyword(KW_DEVICE)
            );
        }
        sent != 0
    }

    fn mode_switch_status(&mut self, request: WDFREQUEST) -> NtStatus {
        // SAFETY: live driver-created request (the send returned).
        unsafe { call_unsafe_wdf_function_binding!(WdfRequestGetStatus, request) }
    }

    fn delete_mode_switch_request(&mut self, request: WDFREQUEST) {
        // SAFETY: driver-created request, not pending (the synchronous send returned or it was
        // never sent); deleting it also deletes its child memory.
        unsafe { object_delete(request) };
    }
}

/// Traces a `SET_FEATURE` (Hid.c:298-336): the report, then the change the core will apply (decoded
/// the same way, from the same bytes) or the rejection. The core reaches `set_feature_packet` only
/// after its length and NULL `UserBuffer` checks, as in C.
fn trace_set_feature(report_id: u8, report_buffer: &[u8]) {
    let report_buffer_len = report_buffer.len();
    tlg::write_event!(
        PROVIDER,
        "SetFeature",
        level(Informational),
        keyword(KW_HID),
        u8_hex("ReportId", &report_id),
        usize("ReportBufferLen", &report_buffer_len)
    );
    match decode_set_feature(report_id, report_buffer) {
        Ok(FeatureChange::InputMode { ptp_input_on }) => {
            tlg::write_event!(
                PROVIDER,
                "SetInputMode",
                level(Informational),
                keyword(KW_HID),
                bool8("PtpInputOn", &ptp_input_on)
            );
        }
        Ok(FeatureChange::SelectiveReporting {
            button_report,
            surface_report,
        }) => {
            tlg::write_event!(
                PROVIDER,
                "SelectiveReporting",
                level(Informational),
                keyword(KW_HID),
                bool8("ButtonReport", &button_report),
                bool8("SurfaceReport", &surface_report)
            );
        }
        Ok(FeatureChange::None) => {}
        Err(status) => {
            tlg::write_event!(
                PROVIDER,
                "SetFeatureNotSupported",
                level(Informational),
                keyword(KW_HID),
                u8_hex("ReportId", &report_id),
                win_ntstatus("Status", &status)
            );
        }
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

//! Framework I/O for the Bluetooth HID collection.

use core::ffi::c_void;
use core::mem::size_of;
use core::ptr::null_mut;

use amtptp_hidfilter_core::bth::{PADDED_REPORT_LEN, PaddedReport, unpad_report};
use amtptp_hidfilter_core::{
    ContactTracker, FeatureChange, LowerReadClass, NtStatus, ReadTicket, ReadTracker,
    STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_DEVICE_STATE, XferPacket, XferPacketMut,
    decode_set_feature,
};
use ptp_hid::hidport::{
    HID_STRING_ID_IMANUFACTURER, HID_STRING_ID_IPRODUCT, HID_STRING_ID_ISERIALNUMBER,
};
use ptp_hid::ioctl::IOCTL_HID_SET_FEATURE;
use ptp_hid::read_tracker::MAX_IN_FLIGHT_READS;
use tracelogging as tlg;
use wdf_support::handle::{ContextPtr, WdfHandle};
use wdf_support::hid as wdf_hid;
use wdf_support::init::{
    object_attributes_init_context_type, request_parameters_init, sync_send_options,
};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_PARAMETER};
use wdf_support::request::Request;
use wdk_sys::{
    _WDF_DEVICE_FAILED_ACTION, HID_XFER_PACKET, PWDF_REQUEST_COMPLETION_PARAMS, ULONG, ULONG_PTR,
    WDFCONTEXT, WDFIOTARGET, WDFMEMORY, WDFMEMORY_OFFSET, WDFREQUEST,
    call_unsafe_wdf_function_binding,
};

use crate::context::{
    DeviceContext, LowerRead, WORKER_REQUEST_CONTEXT_TYPE_INFO, WorkerRequestContext,
    worker_request_get_context,
};
use crate::{KW_DEVICE, KW_HID, KW_INPUT, PROVIDER};

mod dispatch;
mod pipeline;
mod pnp;
pub(crate) use dispatch::dispatch_internal_ioctl;
pub(crate) use pipeline::{LowerReadOutcome, complete_lower_read, work_item_callback};
pub(crate) use pnp::{
    d0_entry, d0_exit, prepare_hardware, recovery_timer_callback, self_managed_io_init,
    self_managed_io_restart, source_closed, source_opened,
};

type UpperRequest = WDFREQUEST;
type ReadMemory = WDFMEMORY;
/// `WDF_DEVICE_FAILED_ACTION` for `WdfDeviceSetFailed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailedAction {
    /// `WdfDeviceFailedAttemptRestart`.
    AttemptRestart,
    /// `WdfDeviceFailedNoRestart`.
    NoRestart,
}

const _: () = assert!(size_of::<HID_XFER_PACKET>() == amtptp_hidfilter_core::HID_XFER_PACKET_LEN);
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

    /// The data source's target, or `None` while it is not open. The core only issues requests while
    /// the source is open, so `None` only appears in a race with its removal; it keeps a NULL handle
    /// away from WDF (which would bugcheck).
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

#[expect(
    clippy::unused_self,
    reason = "Callbacks use the same operation receiver for live requests and for the recording backend."
)]
#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "The exclusive receiver limits the lifetime of borrowed framework request and report buffers."
)]
impl Filter<'_> {
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

    fn copy_string(&mut self, request: WDFREQUEST) -> Result<usize, NtStatus> {
        let mut params = request_parameters_init();
        // SAFETY: live owned request; initialized parameters structure.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfRequestGetParameters, request, &raw mut params);
        }
        // SAFETY: an internal device-control request: KMDF fills `DeviceIoControl`, whose
        // Type3InputBuffer carries the string ID (low 16 bits) and language ID (high 16 bits).
        let id_and_language =
            unsafe { params.Parameters.DeviceIoControl.Type3InputBuffer } as usize;
        let id = (id_and_language & 0xFFFF) as u16;
        let mut units = [0_u16; crate::context::MAX_STRING_UNITS];
        // SAFETY: the spin lock was created in AddDevice; the closure does not call back into WDF.
        let found = unsafe {
            self.ctx.strings.with(|strings| {
                if !strings.known {
                    return Err(STATUS_INVALID_DEVICE_STATE);
                }
                let text = match id {
                    HID_STRING_ID_IMANUFACTURER => &strings.manufacturer,
                    HID_STRING_ID_IPRODUCT => &strings.product,
                    HID_STRING_ID_ISERIALNUMBER => &strings.serial_number,
                    _ => return Err(STATUS_INVALID_PARAMETER),
                };
                let text = text.as_slice();
                if let Some(dst) = units.get_mut(..text.len()) {
                    dst.copy_from_slice(text);
                }
                Ok(text.len())
            })
        }?;
        // The string and its NUL, little-endian UTF-16.
        let bytes = (found + 1) * 2;
        let output = self.retrieve_output_buffer(request, bytes)?;
        let output = output.get_mut(..bytes).ok_or(STATUS_INVALID_PARAMETER)?;
        let (pairs, _) = output.as_chunks_mut::<2>();
        for (pair, unit) in pairs
            .iter_mut()
            .zip(units.iter().take(found).chain(core::iter::once(&0)))
        {
            *pair = unit.to_le_bytes();
        }
        Ok(bytes)
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

    fn with_contacts<R, F: FnOnce(&mut ContactTracker) -> R>(&mut self, f: F) -> R {
        // SAFETY: the lock was created in `create_device` before any callback could run; every
        // caller runs at <= DISPATCH_LEVEL; `f` only touches the contact tracker.
        unsafe { self.ctx.contacts.with(f) }
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

    // ---- mode switch (Device.c:371-418) --------------------------------------------------------

    fn send_mode_report(&mut self, report: amtptp_hidfilter_core::ModeSwitch) -> NtStatus {
        let Some(target) = self.io_target() else {
            return STATUS_INVALID_DEVICE_STATE;
        };
        let mut input = wdf_support::init::memory_descriptor_init_buffer(
            report.payload.as_ptr().cast_mut().cast(),
            report.payload.len() as ULONG,
        );
        let mut options = sync_send_options(0);
        let started = wdf_support::time::query_performance_counter();
        // SAFETY: the class IOCTL reads the static report. The target is live,
        // and this synchronous call runs in a passive callback.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetSendIoctlSynchronously,
                target,
                null_mut(),
                IOCTL_HID_SET_FEATURE,
                &raw mut input,
                null_mut(),
                &raw mut options,
                null_mut()
            )
        };
        let elapsed_ms = elapsed_ms_since(started);
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "MultitouchModeSendFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status),
                u64("ElapsedMs", &elapsed_ms)
            );
            return status;
        }
        tlg::write_event!(
            PROVIDER,
            "MultitouchModeSet",
            level(Informational),
            keyword(KW_DEVICE),
            win_ntstatus("Status", &status),
            u64("ElapsedMs", &elapsed_ms)
        );
        status
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
/// (`amtptp_hidfilter_core::bth`): a multitouch report keeps its bytes at the start of the buffer
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

/// Milliseconds since the performance-counter value `since` (0 if the counter is unusable).
fn elapsed_ms_since(since: i64) -> u64 {
    let frequency = wdf_support::time::query_performance_frequency();
    let now = wdf_support::time::query_performance_counter();
    match (u64::try_from(now - since), u64::try_from(frequency)) {
        (Ok(ticks), Ok(frequency)) if frequency > 0 => ticks.saturating_mul(1000) / frequency,
        _ => 0,
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

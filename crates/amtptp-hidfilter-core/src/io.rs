//! The framework operations the driver's sequences need, as a trait the glue implements.
//!
//! Every method is one KMDF (or kernel) call of the C driver; the doc of each method names it. The
//! sequencing functions of this crate ([`crate::dispatch_internal_ioctl`],
//! [`crate::complete_lower_read`], [`crate::self_managed_io_init`], ...) call them in exactly the C
//! order (for the calls the oracle records; pure accessors such as `WdfMemoryGetBuffer` may be called
//! later or not at all), so the glue only translates each method into its WDF call. The host tests implement the trait
//! with the fixture harness's stub model and compare the recorded call sequence with the C oracle.
//!
//! A few methods have no C counterpart: the read tracker ([`FilterIo::with_reads`]), the cancellation of
//! the reads in flight and the timer stop at `D0Exit` (P3 fixes of HF-B7/HF-B14). The test model keeps
//! them out of the recorded C sequence and checks them separately.
//!
//! Handles are `Copy` associated types (WDF handles in the glue). None of the methods may panic.

use ptp_hid::hidport::HID_DEVICE_ATTRIBUTES_LEN;
use ptp_hid::read_tracker::{ReadTicket, ReadTracker};

use crate::device::XferBuffer;
use crate::status::NtStatus;

/// `WDF_DEVICE_FAILED_ACTION` for `WdfDeviceSetFailed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailedAction {
    /// `WdfDeviceFailedAttemptRestart`.
    AttemptRestart,
    /// `WdfDeviceFailedNoRestart`.
    NoRestart,
}

/// The `HID_XFER_PACKET` of an `IOCTL_HID_GET_FEATURE` request, writable.
#[derive(Debug)]
pub struct XferPacketMut<'a> {
    /// `reportId` (what the handler switches on).
    pub report_id: u8,
    /// The `reportBufferLen` bytes at `reportBuffer`. A NULL `reportBuffer` must be given as an empty
    /// slice.
    pub report_buffer: &'a mut [u8],
}

/// The `HID_XFER_PACKET` of an `IOCTL_HID_SET_FEATURE` request, read-only.
#[derive(Debug)]
pub struct XferPacket<'a> {
    /// `reportId` (what the handler switches on).
    pub report_id: u8,
    /// The `reportBufferLen` bytes at `reportBuffer`. A NULL `reportBuffer` must be given as an empty
    /// slice.
    pub report_buffer: &'a [u8],
}

/// Framework operations used by the device's I/O, PnP/power, timer and work-item paths.
///
/// One implementation per device (e.g. a struct holding the `WDFDEVICE` and a reference to its context)
/// serves every callback; per-call objects (requests, memory) are passed in as handles.
pub trait FilterIo {
    /// A framework-delivered request: presented to `EvtIoInternalDeviceControl` or retrieved from
    /// `HidReadQueue` (`WDFREQUEST`). Each one the core receives or retrieves is completed, forwarded or
    /// parked exactly once on every path.
    type Request: Copy;
    /// The output `WDFMEMORY` of an upper request.
    type Memory: Copy;
    /// A driver-created lower `IOCTL_HID_READ_REPORT` request (`WDFREQUEST` with the
    /// `WORKER_REQUEST_CONTEXT`), as the read tracker stores it.
    type LowerRead: Copy + PartialEq;
    /// The lookaside `WDFMEMORY` of a lower read (stored in its request context as `RequestMemory`).
    type LowerMemory: Copy;
    /// The driver-created mode-switch request (`WDFREQUEST`).
    type ModeSwitchRequest: Copy;
    /// The preallocated `WDFMEMORY` over the mode-switch [`XferBuffer`] (child of the request).
    type ModeSwitchMemory: Copy;

    // ---- upper requests ------------------------------------------------------------------------

    /// `WdfRequestComplete(request, status)`. Leaves `Information` as it is.
    fn complete(&mut self, request: Self::Request, status: NtStatus);
    /// `WdfRequestSetInformation(request, information)`.
    fn set_information(&mut self, request: Self::Request, information: usize);
    /// `WdfRequestRetrieveOutputMemory(request, &memory)`.
    ///
    /// # Errors
    /// Returns the WDF failure status if the request has no retrievable output memory.
    fn retrieve_output_memory(&mut self, request: Self::Request) -> Result<Self::Memory, NtStatus>;
    /// `WdfMemoryCopyFromBuffer(memory, 0, source, source.len())`.
    fn copy_to_memory(&mut self, memory: Self::Memory, source: &[u8]) -> NtStatus;
    /// `WdfRequestRetrieveOutputBuffer(request, minimum_length, &buffer, NULL)`; the slice has at least
    /// `minimum_length` bytes (the driver glue gives exactly `minimum_length`, because `Length` is NULL).
    ///
    /// # Errors
    /// Returns the WDF failure status if the buffer cannot be retrieved or is too short.
    fn retrieve_output_buffer(
        &mut self,
        request: Self::Request,
        minimum_length: usize,
    ) -> Result<&mut [u8], NtStatus>;
    /// `(PHID_XFER_PACKET)WdfRequestWdmGetIrp(request)->UserBuffer` for `GET_FEATURE`; `None` if NULL.
    fn get_feature_packet(&mut self, request: Self::Request) -> Option<XferPacketMut<'_>>;
    /// `(PHID_XFER_PACKET)WdfRequestWdmGetIrp(request)->UserBuffer` for `SET_FEATURE`; `None` if NULL.
    fn set_feature_packet(&mut self, request: Self::Request) -> Option<XferPacket<'_>>;
    /// `WdfRequestForwardToIoQueue(request, HidReadQueue)`.
    fn forward_to_read_queue(&mut self, request: Self::Request) -> NtStatus;
    /// `WdfIoQueueRetrieveNextRequest(HidReadQueue, &request)`.
    ///
    /// # Errors
    /// Returns the WDF failure status if the queue is empty or cannot supply a request.
    fn retrieve_next_parked_read(&mut self) -> Result<Self::Request, NtStatus>;
    /// `WdfIoQueueGetState(HidReadQueue, &QueueRequests, NULL)`: the number of parked upper reads (not
    /// in C; a pure query).
    fn parked_read_count(&mut self) -> usize;
    /// `WdfRequestFormatRequestUsingCurrentType(request)`.
    fn format_using_current_type(&mut self, request: Self::Request);
    /// `WdfRequestSend(request, HidIoTarget, SEND_AND_FORGET)`; true if sent (the request then belongs
    /// to the lower driver).
    fn send_and_forget(&mut self, request: Self::Request) -> bool;
    /// `WdfRequestGetStatus(request)`.
    fn request_status(&mut self, request: Self::Request) -> NtStatus;

    // ---- lower reads (Input.c:39-117) ----------------------------------------------------------

    /// `WdfRequestCreate(attributes{WORKER_REQUEST_CONTEXT, ParentObject = device}, HidIoTarget)`.
    ///
    /// # Errors
    /// Returns the WDF failure status if the request cannot be created.
    fn create_lower_read(&mut self) -> Result<Self::LowerRead, NtStatus>;
    /// `WdfMemoryCreateFromLookaside(HidReadBufferLookaside)`; on success also stores
    /// `DeviceContext` and `RequestMemory` in the request context.
    ///
    /// # Errors
    /// Returns the WDF failure status if the buffer cannot be allocated.
    fn create_lower_read_memory(
        &mut self,
        read: Self::LowerRead,
    ) -> Result<Self::LowerMemory, NtStatus>;
    /// `WdfIoTargetFormatRequestForInternalIoctl(HidIoTarget, read, IOCTL_HID_READ_REPORT, NULL, NULL,
    /// memory, NULL)`.
    fn format_lower_read(&mut self, read: Self::LowerRead, memory: Self::LowerMemory) -> NtStatus;
    /// `WdfRequestSetCompletionRoutine(read, <completion routine>, request context)`. The completion
    /// routine reads `RequestMemory` and the ticket from the context (before anything is deleted,
    /// HF-B10), and `WdfRequestGetStatus` / `WdfRequestGetInformation` from the request, and calls
    /// [`crate::complete_lower_read`].
    fn set_lower_read_completion(&mut self, read: Self::LowerRead);
    /// Stores `ticket` in the request context (not in C), then `WdfRequestSend(read, HidIoTarget, NULL)`
    /// (asynchronous); false if not sent.
    fn send_lower_read(&mut self, read: Self::LowerRead, ticket: ReadTicket) -> bool;
    /// The lower read's output buffer: `WdfMemoryGetBuffer(memory, &size)`, `size` bytes
    /// ([`crate::REPORT_BUFFER_SIZE`] for the lookaside buffer). C passes NULL for the size. Only
    /// [`crate::complete_lower_read`] calls this, for the report ID and on the report path.
    fn lower_read_buffer(&mut self, memory: Self::LowerMemory) -> &[u8];
    /// `WdfObjectDelete(read)`.
    fn delete_lower_read(&mut self, read: Self::LowerRead);
    /// `WdfObjectDelete(memory)`.
    fn delete_lower_memory(&mut self, memory: Self::LowerMemory);
    /// Runs `f` on the device's lower-read tracker while holding its lock (not in C). The glue keeps the
    /// tracker behind a `WDFSPINLOCK`: `f` runs at `DISPATCH_LEVEL` and only touches the tracker (and
    /// atomics), never a request.
    fn with_reads<R, F: FnOnce(&mut ReadTracker<Self::LowerRead>) -> R>(&mut self, f: F) -> R;
    /// `D0Exit` (not in C, P3 fix of HF-B14): under the tracker's lock, [`ReadTracker::suspend`] (a new
    /// epoch; the completion of every read sent before is stale) and `WdfObjectReference` each read it
    /// hands out; then, outside the lock, `WdfRequestCancelSentRequest` and `WdfObjectDereference` each
    /// one. Nothing waits for the cancelled reads. Returns how many reads were cancelled.
    fn cancel_in_flight_reads(&mut self) -> usize;

    // ---- device-wide ---------------------------------------------------------------------------

    /// `WdfDeviceSetFailed(device, action)`.
    fn set_failed(&mut self, action: FailedAction);
    /// `WdfTimerStart(HidTransportRecoveryTimer, due_time)` (relative, 100 ns units).
    fn start_recovery_timer(&mut self, due_time: i64);
    /// `WdfTimerStop(HidTransportRecoveryTimer, Wait = FALSE)` (not in C, P3 fix of HF-B14): a pending
    /// timer does not fire. A callback that is already running is not waited for (its mode switch can
    /// take up to the synchronous-I/O timeout); it finds the device out of D0 and stops.
    fn stop_recovery_timer(&mut self);
    /// `WdfWorkItemEnqueue(HidTransportRecoveryWorkItem)`.
    fn enqueue_recovery_work_item(&mut self);
    /// The current QPC counter in ticks.
    fn query_performance_counter(&mut self) -> i64;
    /// The QPC frequency in ticks per second.
    fn query_performance_frequency(&mut self) -> i64;
    /// `WdfIoTargetSendInternalIoctlSynchronously(HidIoTarget, NULL, IOCTL_HID_GET_DEVICE_ATTRIBUTES,
    /// NULL, &descriptor(output, 32), NULL, NULL)`: no timeout (HF-B27). `output` arrives zeroed.
    fn query_device_attributes(&mut self, output: &mut [u8; HID_DEVICE_ATTRIBUTES_LEN])
    -> NtStatus;

    // ---- mode switch (Device.c:371-418) --------------------------------------------------------

    /// `WdfRequestCreate(WDF_NO_OBJECT_ATTRIBUTES, HidIoTarget, &request)`.
    ///
    /// # Errors
    /// Returns the WDF failure status if the request cannot be created.
    fn create_mode_switch_request(&mut self) -> Result<Self::ModeSwitchRequest, NtStatus>;
    /// `WdfMemoryCreatePreallocated(attributes{ParentObject = request}, packet.as_mut_ptr(), 32)`.
    ///
    /// # Errors
    /// Returns the WDF failure status if the transfer packet cannot be wrapped in WDF memory.
    fn create_mode_switch_memory(
        &mut self,
        request: Self::ModeSwitchRequest,
        packet: &mut XferBuffer,
    ) -> Result<Self::ModeSwitchMemory, NtStatus>;
    /// `WdfIoTargetFormatRequestForInternalIoctl(HidIoTarget, request, IOCTL_HID_SET_FEATURE, memory,
    /// NULL, NULL, NULL)`.
    fn format_mode_switch(
        &mut self,
        request: Self::ModeSwitchRequest,
        memory: Self::ModeSwitchMemory,
    ) -> NtStatus;
    /// `irp = WdfRequestWdmGetIrp(request)`; if NULL return false, else `irp->UserBuffer =
    /// packet.as_mut_ptr()` and return true.
    fn set_mode_switch_user_buffer(
        &mut self,
        request: Self::ModeSwitchRequest,
        packet: &mut XferBuffer,
    ) -> bool;
    /// `WdfRequestSend(request, HidIoTarget, SYNCHRONOUS)` with no timeout (HF-B27); its BOOLEAN.
    fn send_mode_switch_synchronously(&mut self, request: Self::ModeSwitchRequest) -> bool;
    /// `WdfRequestGetStatus(request)`, after every send: the completion status when the send returned
    /// TRUE, the failure when it returned FALSE.
    fn mode_switch_status(&mut self, request: Self::ModeSwitchRequest) -> NtStatus;
    /// `WdfObjectDelete(request)` (also deletes the memory, its child).
    fn delete_mode_switch_request(&mut self, request: Self::ModeSwitchRequest);
}

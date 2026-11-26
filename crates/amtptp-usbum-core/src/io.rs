//! The framework operations the driver logic needs, as small traits the WDF glue implements.
//!
//! Each method stands for one (or one fixed pair of) WDF DDI call(s) of the C driver. The core decides
//! the order of the calls, the checks between them and the resulting status; the glue performs the call
//! and returns whatever the framework returned. In particular the core never invents a framework status
//! (for example the result of `WdfMemoryCopyFromBuffer` on a short buffer): those come back through these
//! traits.

use crate::status::NtStatus;
use crate::usb::SetupPacket;

/// A WDFREQUEST of the default queue (device control) or of the manual `READ_REPORT` queue, as seen by
/// the HID handlers.
///
/// Every method may be called more than once (WDF returns the same memory object each time).
pub trait HidRequest {
    /// `WdfRequestRetrieveInputMemory` + `WdfMemoryGetBuffer`: the input buffer, or the framework's
    /// failure status (WDF fails a zero-length buffer with `STATUS_BUFFER_TOO_SMALL`).
    ///
    /// # Errors
    /// Returns the WDF failure status if the input memory cannot be retrieved.
    fn input_memory(&mut self) -> Result<&[u8], NtStatus>;

    /// `WdfRequestRetrieveOutputMemory` + `WdfMemoryGetBuffer(.., &length)`: only the length.
    ///
    /// Used where the C code never touches the output bytes: `SET_FEATURE` takes the report ID from the
    /// output length (the mshidumdf convention; the buffer itself is not accessible), and the descriptor
    /// and `READ_REPORT` paths retrieve the memory before copying into it. The glue must not create a
    /// slice over the buffer here.
    ///
    /// # Errors
    /// Returns the WDF failure status if the output memory cannot be retrieved.
    fn output_memory_len(&mut self) -> Result<usize, NtStatus>;

    /// `WdfRequestRetrieveOutputMemory` + `WdfMemoryGetBuffer`: the writable output buffer (`GET_FEATURE`
    /// writes the report in place).
    ///
    /// # Errors
    /// Returns the WDF failure status if the output memory cannot be retrieved.
    fn output_memory(&mut self) -> Result<&mut [u8], NtStatus>;

    /// `WdfRequestRetrieveOutputMemory` + `WdfMemoryCopyFromBuffer(memory, 0, src, src.len())`. Returns the
    /// framework's status (WDF fails when `src` is longer than the buffer).
    ///
    /// # Errors
    /// Returns the WDF failure status if the copy cannot complete.
    fn copy_to_output_memory(&mut self, src: &[u8]) -> Result<(), NtStatus>;

    /// `WdfRequestRetrieveOutputBuffer(request, minimum_len, &buffer, &length)`: the whole output buffer
    /// (`length` bytes, at least `minimum_len`), or the framework's failure status.
    ///
    /// # Errors
    /// Returns the WDF failure status if the buffer cannot be retrieved or is too short.
    fn output_buffer(&mut self, minimum_len: usize) -> Result<&mut [u8], NtStatus>;
}

/// The manual `InputQueue` that holds pending `IOCTL_HID_READ_REPORT` requests.
pub trait ReadQueue {
    /// A request retrieved from the queue. The glue's type must make sure the request is completed even
    /// if it is dropped without [`ReadQueue::complete_request`] (it never is by this crate).
    type Request: HidRequest;

    /// `WdfIoQueueRetrieveNextRequest`. An empty queue gives the framework's status
    /// (`STATUS_NO_MORE_ENTRIES`).
    ///
    /// # Errors
    /// Returns the WDF failure status if the queue is empty or cannot supply a request.
    fn retrieve_next_request(&mut self) -> Result<Self::Request, NtStatus>;

    /// `WdfRequestSetInformation(request, information)` + `WdfRequestComplete(request, status)`.
    fn complete_request(&mut self, request: Self::Request, status: NtStatus, information: usize);
}

/// The USB default control pipe (`WdfUsbTargetDeviceSendControlTransferSynchronously` on the
/// WDFUSBDEVICE, no request object, no timeout options).
pub trait ControlPipe {
    /// Sends one control transfer synchronously and returns its status.
    ///
    /// `setup` is the 8-byte setup packet (its `w_length` is always 4 = `data.len()`). The data stage is
    /// `data`, described to WDF as a 4-byte buffer (`WDF_MEMORY_DESCRIPTOR_INIT_BUFFER`). For a
    /// device-to-host transfer the device's bytes land at the start of `data`; bytes it does not return
    /// stay as they were. The number of bytes transferred is not needed (the C driver never checks it).
    fn send_control_transfer(&mut self, setup: &SetupPacket, data: &mut [u8; 4]) -> NtStatus;
}

/// `WdfUsbTargetDeviceAllocAndQueryString` (`GET_STRING`).
pub trait UsbStringSource {
    /// The string memory: `NumCharacters * 2` bytes of UTF-16LE. The glue should delete the WDFMEMORY
    /// when this value is dropped (the C driver leaks it, UM-B7).
    type String: AsRef<[u8]>;

    /// Queries string descriptor `string_index` in language `language_id` from the device.
    ///
    /// # Errors
    /// Returns the WDF failure status if allocation or the USB descriptor query fails.
    fn alloc_and_query_string(
        &mut self,
        string_index: u8,
        language_id: u16,
    ) -> Result<Self::String, NtStatus>;
}

/// `WDF_USB_PIPE_TYPE` of a configured pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PipeType {
    /// `WdfUsbPipeTypeInvalid`.
    Invalid,
    /// `WdfUsbPipeTypeControl`.
    Control,
    /// `WdfUsbPipeTypeIsochronous`.
    Isochronous,
    /// `WdfUsbPipeTypeBulk`.
    Bulk,
    /// `WdfUsbPipeTypeInterrupt`.
    Interrupt,
}

/// The WDF USB objects `PrepareHardware` works with (WDFUSBDEVICE, interface 0, its pipes, the
/// continuous reader).
pub trait UsbTargetSetup {
    /// `if (ctx.UsbDevice == NULL) WdfUsbTargetDeviceCreate(..)`: creates the USB target device once and
    /// reuses it on later calls. Returns the create failure status.
    ///
    /// # Errors
    /// Returns the WDF failure status if the USB device cannot be created.
    fn create_usb_device_if_needed(&mut self) -> Result<(), NtStatus>;

    /// `WdfUsbTargetDeviceGetDeviceDescriptor`.
    fn device_descriptor(&mut self) -> crate::state::UsbDeviceDescriptor;

    /// `WDF_USB_DEVICE_INFORMATION_INIT` + `WdfUsbTargetDeviceRetrieveInformation`: the `Traits`.
    ///
    /// # Errors
    /// Returns the WDF failure status if the USB traits cannot be retrieved.
    fn retrieve_device_traits(&mut self) -> Result<u32, NtStatus>;

    /// `WdfUsbTargetDeviceGetInterface(UsbDevice, 0)` (stored as `UsbInterface`) and
    /// `WdfUsbInterfaceGetNumConfiguredPipes`. `None` when the interface is `NULL`.
    fn interface0_configured_pipes(&mut self) -> Option<u8>;

    /// `WdfUsbInterfaceGetConfiguredPipe(UsbInterface, index, &pipeInfo)` followed by
    /// `WdfUsbTargetPipeSetNoMaximumPacketSizeCheck(pipe)`; returns `pipeInfo.PipeType`.
    fn visit_configured_pipe(&mut self, index: u8) -> PipeType;

    /// `ctx.InterruptPipe = pipe` for the pipe last visited at `index`.
    fn select_interrupt_pipe(&mut self, index: u8);

    /// `ctx.InterruptPipe != NULL`. Note that the C handle survives a PnP stop/start.
    fn has_interrupt_pipe(&self) -> bool;

    /// `WDF_USB_CONTINUOUS_READER_CONFIG_INIT(&cfg, EvtUsbInterruptPipeReadComplete, ctx,
    /// transfer_length)`, `cfg.EvtUsbTargetPipeReadersFailed = EvtUsbInterruptReadersFailed` (returns
    /// [`crate::pnp::READERS_FAILED_RESULT`]), `NumPendingReads` 0 (framework default), no header/trailer,
    /// no buffer attributes; then `WdfUsbTargetPipeConfigContinuousReader(ctx.InterruptPipe, &cfg)`.
    fn configure_continuous_reader(&mut self, transfer_length: usize) -> NtStatus;
}

/// The I/O target of the interrupt pipe (`WdfUsbTargetPipeGetIoTarget(ctx.InterruptPipe)`).
pub trait InterruptPipeTarget {
    /// `WdfIoTargetStart`.
    fn start(&mut self) -> NtStatus;

    /// `WdfIoTargetStop(target, WdfIoTargetCancelSentIo)`.
    fn stop_cancel_sent_io(&mut self);
}

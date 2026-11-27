//! The WDF USB I/O target glue: the core's [`UsbTargetSetup`], [`ControlPipe`], [`InterruptPipeTarget`]
//! and [`UsbStringSource`] over the WDFUSBDEVICE / interface / pipes, and the continuous-reader
//! callbacks (C: Device.c:255-496, :644-727; InputInterrupt.c:6-226).
//!
//! | C | Rust |
//! |---|---|
//! | `SelectInterruptInterface` | `pnp::prepare_hardware` driving [`UsbSetup`] |
//! | `AmtPtpConfigContReaderForInterruptEndPoint` | [`UsbSetup::configure_continuous_reader`] |
//! | `WdfUsbTargetDeviceSendControlTransferSynchronously` in Get/SetWellspringMode | [`DeviceIo::send_control_transfer`] |
//! | `WdfIoTargetStart` / `WdfIoTargetStop` on the pipe target | [`DeviceIo`] (`InterruptPipeTarget`) |
//! | `WdfUsbTargetDeviceAllocAndQueryString` in `AmtPtpGetStrings` | [`UsbStrings`] |
//! | `AmtPtpEvtUsbInterruptPipeReadComplete` | [`evt_usb_interrupt_pipe_read_complete`] → `input::on_read_complete` |
//! | `AmtPtpEvtUsbInterruptReadersFailed` | [`evt_usb_interrupt_readers_failed`] |
//!
//! All USB I/O is synchronous where the C code is synchronous: the mode-switch control transfers run
//! without a timeout inside the `SET_FEATURE` dispatch, D0Entry/D0Exit and — for the emergency reset of
//! UM-B11 — inside the continuous-reader completion. In UMDF every callback runs at `PASSIVE_LEVEL` on a
//! host thread, so this is a timing risk (a hung device blocks that thread), not an IRQL one.

use core::ptr::{self, null_mut};

use amtptp_usbum_core::input;
use amtptp_usbum_core::io::{
    ControlPipe, InterruptPipeTarget, PipeType, UsbStringSource, UsbTargetSetup,
};
use amtptp_usbum_core::pnp;
use amtptp_usbum_core::state::UsbDeviceDescriptor;
use amtptp_usbum_core::status::NtStatus;
use amtptp_usbum_core::usb::{MODE_SWITCH_TRANSFER_LEN, SetupPacket};
use wdf_support::init::{
    memory_descriptor_init_buffer, usb_continuous_reader_config_init, usb_device_information_init,
    usb_pipe_information_init,
};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE};
use wdf_support::time::query_performance_counter;
use wdk_sys::{
    _WDF_IO_TARGET_SENT_IO_ACTION, _WDF_USB_PIPE_TYPE, BOOLEAN, NTSTATUS, ULONG,
    USB_DEVICE_DESCRIPTOR, USBD_STATUS, USHORT, WDF_NO_OBJECT_ATTRIBUTES,
    WDF_USB_CONTROL_SETUP_PACKET, WDFCONTEXT, WDFDEVICE, WDFIOTARGET, WDFMEMORY, WDFUSBDEVICE,
    WDFUSBPIPE, call_unsafe_wdf_function_binding,
};

use crate::device::DeviceContext;
use crate::queue::InputQueue;
use crate::request::{memory_buffer, slice_or_empty};
use crate::trace;

// ---------------------------------------------------------------------------------------------------
// PrepareHardware: USB target device, interface 0, pipes, continuous reader.
// ---------------------------------------------------------------------------------------------------

/// [`UsbTargetSetup`] for one `EvtDevicePrepareHardware` call.
pub(crate) struct UsbSetup<'a> {
    device: WDFDEVICE,
    context: &'a DeviceContext,
    /// The pipe last returned by `WdfUsbInterfaceGetConfiguredPipe`, with its index and endpoint.
    visited: Option<(u8, WDFUSBPIPE, u8)>,
    /// Endpoint address of the pipe selected by this call (0 if none was selected now).
    selected_endpoint_address: u8,
    /// The WDF step currently running, for the failure trace.
    step: &'static str,
}

impl<'a> UsbSetup<'a> {
    /// The adapter for `device` (whose context is `context`).
    pub(crate) const fn new(device: WDFDEVICE, context: &'a DeviceContext) -> Self {
        Self {
            device,
            context,
            visited: None,
            selected_endpoint_address: 0,
            step: "AmtPtpEvtDevicePrepareHardware",
        }
    }

    /// `EndpointAddress` of the interrupt pipe selected by this `PrepareHardware` (0 if none).
    pub(crate) const fn selected_endpoint_address(&self) -> u8 {
        self.selected_endpoint_address
    }

    /// The step that was running when `pnp::prepare_hardware` stopped (for the failure trace).
    pub(crate) const fn failed_step(&self) -> &'static str {
        self.step
    }
}

impl UsbTargetSetup for UsbSetup<'_> {
    fn create_usb_device_if_needed(&mut self) -> Result<(), NtStatus> {
        self.step = "WdfUsbTargetDeviceCreate";
        if !self.context.usb_device().is_null() {
            return Ok(());
        }
        let mut usb_device: WDFUSBDEVICE = null_mut();
        // SAFETY: `self.device` is the live WDFDEVICE of this PrepareHardware; no attributes (the USB
        // target is parented to the device, as in C); `usb_device` is an out pointer.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetDeviceCreate,
                self.device,
                WDF_NO_OBJECT_ATTRIBUTES,
                &raw mut usb_device
            )
        };
        if !NT_SUCCESS(status) {
            return Err(status);
        }
        self.context.set_usb_device(usb_device);
        Ok(())
    }

    fn device_descriptor(&mut self) -> UsbDeviceDescriptor {
        // A failure after this point (before the next step) is the config-table lookup.
        self.step = "AmtPtpGetDeviceConfig";
        let usb_device = self.context.usb_device();
        let mut descriptor = USB_DEVICE_DESCRIPTOR::default();
        if !usb_device.is_null() {
            // SAFETY: valid WDFUSBDEVICE (created above); `descriptor` is a writable 18-byte
            // USB_DEVICE_DESCRIPTOR.
            unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfUsbTargetDeviceGetDeviceDescriptor,
                    usb_device,
                    &raw mut descriptor
                );
            }
        }
        // USB_DEVICE_DESCRIPTOR is packed: copy every field by value (no references).
        let USB_DEVICE_DESCRIPTOR {
            idVendor,
            idProduct,
            bcdDevice,
            iManufacturer,
            iProduct,
            iSerialNumber,
            ..
        } = descriptor;
        UsbDeviceDescriptor {
            id_vendor: idVendor,
            id_product: idProduct,
            bcd_device: bcdDevice,
            i_manufacturer: iManufacturer,
            i_product: iProduct,
            i_serial_number: iSerialNumber,
        }
    }

    fn retrieve_device_traits(&mut self) -> Result<u32, NtStatus> {
        self.step = "WdfUsbTargetDeviceRetrieveInformation";
        let usb_device = self.context.usb_device();
        if usb_device.is_null() {
            return Err(STATUS_INVALID_DEVICE_STATE);
        }
        let mut information = usb_device_information_init();
        // SAFETY: valid WDFUSBDEVICE; `information` is initialized with its Size.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetDeviceRetrieveInformation,
                usb_device,
                &raw mut information
            )
        };
        if NT_SUCCESS(status) {
            Ok(information.Traits)
        } else {
            Err(status)
        }
    }

    fn interface0_configured_pipes(&mut self) -> Option<u8> {
        self.step = "WdfUsbTargetDeviceGetInterface";
        let usb_device = self.context.usb_device();
        if usb_device.is_null() {
            return None;
        }
        // SAFETY: valid WDFUSBDEVICE; interface index 0.
        let usb_interface = unsafe {
            call_unsafe_wdf_function_binding!(WdfUsbTargetDeviceGetInterface, usb_device, 0)
        };
        if usb_interface.is_null() {
            return None;
        }
        // SAFETY: valid WDFUSBINTERFACE just returned by the framework.
        let pipes = unsafe {
            call_unsafe_wdf_function_binding!(WdfUsbInterfaceGetNumConfiguredPipes, usb_interface)
        };
        self.context.set_usb_interface(usb_interface);
        Some(pipes)
    }

    fn visit_configured_pipe(&mut self, index: u8) -> PipeType {
        self.step = "SelectInterruptInterface";
        let usb_interface = self.context.usb_interface();
        if usb_interface.is_null() {
            return PipeType::Invalid;
        }
        let mut pipe_info = usb_pipe_information_init();
        // SAFETY: valid WDFUSBINTERFACE (stored by interface0_configured_pipes); `index` is below the
        // configured-pipe count; `pipe_info` is initialized with its Size.
        let pipe = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbInterfaceGetConfiguredPipe,
                usb_interface,
                index,
                &raw mut pipe_info
            )
        };
        if pipe.is_null() {
            // C would pass NULL to WdfUsbTargetPipeSetNoMaximumPacketSizeCheck; skip the pipe instead.
            self.visited = None;
            return PipeType::Invalid;
        }
        // Tell the framework that it is okay to read less than MaximumPacketSize (every visited pipe).
        // SAFETY: valid WDFUSBPIPE just returned by the framework.
        unsafe {
            call_unsafe_wdf_function_binding!(WdfUsbTargetPipeSetNoMaximumPacketSizeCheck, pipe);
        };
        self.visited = Some((index, pipe, pipe_info.EndpointAddress));
        match pipe_info.PipeType {
            _WDF_USB_PIPE_TYPE::WdfUsbPipeTypeControl => PipeType::Control,
            _WDF_USB_PIPE_TYPE::WdfUsbPipeTypeIsochronous => PipeType::Isochronous,
            _WDF_USB_PIPE_TYPE::WdfUsbPipeTypeBulk => PipeType::Bulk,
            _WDF_USB_PIPE_TYPE::WdfUsbPipeTypeInterrupt => PipeType::Interrupt,
            _ => PipeType::Invalid,
        }
    }

    fn select_interrupt_pipe(&mut self, index: u8) {
        let Some((visited_index, pipe, endpoint_address)) = self.visited else {
            return;
        };
        if visited_index != index {
            return;
        }
        self.context.set_interrupt_pipe(pipe);
        self.selected_endpoint_address = endpoint_address;
        // UM-B16 (P2): the reader is configured once per pipe object. On a PnP restart the framework
        // keeps the configuration and the pipe objects (the driver never calls SelectConfig), so the
        // pipe is the one the reader is already configured on and the core skips the configuration. If
        // a restart ever produced a different pipe object, configure the reader on the new pipe.
        if pipe != self.context.reader_pipe() {
            self.context.state.set_reader_configured(false);
        }
    }

    fn has_interrupt_pipe(&self) -> bool {
        !self.context.interrupt_pipe().is_null()
    }

    fn configure_continuous_reader(&mut self, transfer_length: usize) -> NtStatus {
        self.step = "WdfUsbTargetPipeConfigContinuousReader";
        let pipe = self.context.interrupt_pipe();
        if pipe.is_null() {
            return STATUS_INVALID_DEVICE_STATE;
        }
        // C: the completion context is the device context.
        let reader_context: WDFCONTEXT = ptr::from_ref(self.context).cast_mut().cast();
        let mut config = usb_continuous_reader_config_init(
            Some(evt_usb_interrupt_pipe_read_complete),
            reader_context,
            transfer_length,
        );
        config.EvtUsbTargetPipeReadersFailed = Some(evt_usb_interrupt_readers_failed);
        // NumPendingReads stays 0 (pnp::CONTINUOUS_READER_NUM_PENDING_READS, the framework default), no
        // header/trailer, no buffer attributes: completions can overlap and arrive out of order, as in C.
        // SAFETY: valid WDFUSBPIPE of this device; `config` is initialized; `reader_context` points at
        // the device context, which outlives the reader (the reader is a child of the device's USB
        // target and is stopped in D0Exit / on removal before the device is deleted).
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetPipeConfigContinuousReader,
                pipe,
                &raw mut config
            )
        };
        if NT_SUCCESS(status) {
            self.context.set_reader_pipe(pipe);
        }
        status
    }
}

// ---------------------------------------------------------------------------------------------------
// Control pipe and interrupt-pipe I/O target.
// ---------------------------------------------------------------------------------------------------

/// [`ControlPipe`] (the default control pipe of the WDFUSBDEVICE) and [`InterruptPipeTarget`] (the I/O
/// target of the interrupt pipe) for one callback.
pub(crate) struct DeviceIo<'a> {
    context: &'a DeviceContext,
}

impl<'a> DeviceIo<'a> {
    /// The adapter over `context`'s USB device and interrupt pipe.
    pub(crate) const fn new(context: &'a DeviceContext) -> Self {
        Self { context }
    }
}

impl ControlPipe for DeviceIo<'_> {
    /// `WdfUsbTargetDeviceSendControlTransferSynchronously(UsbDevice, WDF_NO_HANDLE, NULL, &setup,
    /// &memoryDescriptor, &cbTransferred)` with a 4-byte buffer descriptor (UM-B1: wLength 4).
    ///
    /// The setup packet is the core's 8 wire bytes (`bmRequestType` class/interface, `bRequest`,
    /// `wValue`, `wIndex`, `wLength` = 4); C builds the same bytes with `WDF_USB_CONTROL_SETUP_PACKET_INIT`
    /// + `Type = BmRequestClass` and lets the framework fill `wLength` from the 4-byte descriptor.
    fn send_control_transfer(
        &mut self,
        setup: &SetupPacket,
        data: &mut [u8; MODE_SWITCH_TRANSFER_LEN],
    ) -> NtStatus {
        let usb_device = self.context.usb_device();
        if usb_device.is_null() {
            // Unreachable after a successful PrepareHardware (C would hand NULL to WDF).
            return STATUS_INVALID_DEVICE_STATE;
        }
        let setup_bytes = setup.to_bytes();
        let mut packet = WDF_USB_CONTROL_SETUP_PACKET::default();
        packet.Generic.Bytes = setup_bytes;
        // Lossless: MODE_SWITCH_TRANSFER_LEN is 4.
        let mut descriptor = memory_descriptor_init_buffer(
            data.as_mut_ptr().cast(),
            MODE_SWITCH_TRANSFER_LEN as ULONG,
        );
        let mut bytes_transferred: ULONG = 0;
        // SAFETY: valid WDFUSBDEVICE; no request object and no send options (synchronous, no timeout,
        // as in C); `packet` and `descriptor` are initialized locals; the descriptor's buffer is the
        // caller's 4-byte array, which outlives this synchronous call; `bytes_transferred` is an out
        // pointer (its value is not used, as in C).
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetDeviceSendControlTransferSynchronously,
                usb_device,
                null_mut(),
                null_mut(),
                &raw mut packet,
                &raw mut descriptor,
                &raw mut bytes_transferred
            )
        };
        if !NT_SUCCESS(status) {
            trace::control_transfer_failed(setup_bytes, status);
        }
        status
    }
}

/// `WdfUsbTargetPipeGetIoTarget(Pipe)`: a `FORCEINLINE` in `wdfusb.h` (UMDF 2.15) that returns
/// `(WDFIOTARGET) Pipe` (a WDFUSBPIPE is an I/O target), so it is not in the WDF function table.
const fn pipe_io_target(pipe: WDFUSBPIPE) -> WDFIOTARGET {
    pipe.cast()
}

impl InterruptPipeTarget for DeviceIo<'_> {
    /// `WdfIoTargetStart(WdfUsbTargetPipeGetIoTarget(InterruptPipe))`.
    fn start(&mut self) -> NtStatus {
        let pipe = self.context.interrupt_pipe();
        if pipe.is_null() {
            // Unreachable: D0Entry only runs after a successful PrepareHardware.
            return STATUS_INVALID_DEVICE_STATE;
        }
        let target = pipe_io_target(pipe);
        // SAFETY: the pipe's I/O target, valid as long as the pipe.
        unsafe { call_unsafe_wdf_function_binding!(WdfIoTargetStart, target) }
    }

    /// `WdfIoTargetStop(WdfUsbTargetPipeGetIoTarget(InterruptPipe), WdfIoTargetCancelSentIo)`.
    fn stop_cancel_sent_io(&mut self) {
        let pipe = self.context.interrupt_pipe();
        if pipe.is_null() {
            return;
        }
        let target = pipe_io_target(pipe);
        // SAFETY: the pipe's I/O target. Called from D0Exit, never from the reader's own completion
        // routine (stopping from there would wait for itself).
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetStop,
                target,
                _WDF_IO_TARGET_SENT_IO_ACTION::WdfIoTargetCancelSentIo
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// GET_STRING.
// ---------------------------------------------------------------------------------------------------

/// [`UsbStringSource`] over the WDFUSBDEVICE.
pub(crate) struct UsbStrings {
    usb_device: WDFUSBDEVICE,
}

impl UsbStrings {
    /// The string source of `context`'s USB device (NULL before the first `PrepareHardware`).
    pub(crate) fn new(context: &DeviceContext) -> Self {
        Self {
            usb_device: context.usb_device(),
        }
    }
}

/// The WDFMEMORY returned by `WdfUsbTargetDeviceAllocAndQueryString`, deleted on drop (UM-B7: the C
/// driver leaks it until the USB target is deleted).
pub(crate) struct StringMemory {
    /// NULL for an empty string (WDF returns success without a memory object).
    memory: WDFMEMORY,
    /// `min(NumCharacters * 2, buffer length)` bytes of UTF-16LE at `bytes`.
    bytes: *const u8,
    len: usize,
}

impl AsRef<[u8]> for StringMemory {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: `bytes`/`len` describe (a prefix of) the buffer of `self.memory`, which stays alive
        // and unmodified until `self` is dropped; NULL/0 gives an empty slice.
        unsafe { slice_or_empty(self.bytes, self.len) }
    }
}

impl Drop for StringMemory {
    fn drop(&mut self) {
        if !self.memory.is_null() {
            // SAFETY: the driver owns this memory object (created by AllocAndQueryString without a
            // parent override) and deletes it exactly once; no slice into it outlives `self`.
            unsafe { call_unsafe_wdf_function_binding!(WdfObjectDelete, self.memory.cast()) };
        }
    }
}

impl UsbStringSource for UsbStrings {
    type String = StringMemory;

    /// `WdfUsbTargetDeviceAllocAndQueryString(UsbDevice, WDF_NO_OBJECT_ATTRIBUTES, &memHandle,
    /// &wcharCount, strIndex, (USHORT) languageId)`.
    fn alloc_and_query_string(
        &mut self,
        string_index: u8,
        language_id: u16,
    ) -> Result<StringMemory, NtStatus> {
        if self.usb_device.is_null() {
            // GET_STRING before the first PrepareHardware: C would hand NULL to WDF (P2 guard).
            return Err(STATUS_INVALID_DEVICE_STATE);
        }
        let mut memory: WDFMEMORY = null_mut();
        let mut characters: USHORT = 0;
        // SAFETY: valid WDFUSBDEVICE; out pointers to locals.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetDeviceAllocAndQueryString,
                self.usb_device,
                WDF_NO_OBJECT_ATTRIBUTES,
                &raw mut memory,
                &raw mut characters,
                string_index,
                language_id
            )
        };
        if !NT_SUCCESS(status) {
            return Err(status);
        }
        let mut string = StringMemory {
            memory,
            bytes: ptr::null(),
            len: 0,
        };
        if !memory.is_null() {
            // SAFETY: valid WDFMEMORY just created by the framework.
            let (bytes, size) = unsafe { memory_buffer(memory) };
            string.bytes = bytes.cast_const();
            string.len = size.min(usize::from(characters) * 2);
        }
        Ok(string)
    }
}

// ---------------------------------------------------------------------------------------------------
// Continuous-reader callbacks.
// ---------------------------------------------------------------------------------------------------

/// `AmtPtpEvtUsbInterruptPipeReadComplete` (InputInterrupt.c:82-211) → `input::on_read_complete`.
///
/// `Buffer` is the reader's WDFMEMORY; the packet is its first `NumBytesTransferred` bytes (never more
/// than the buffer holds). The malformed-length emergency reset (UM-B11) runs synchronously here, as in C.
///
/// # Safety
/// Called by the framework's continuous reader, configured in
/// [`UsbSetup::configure_continuous_reader`] with `Context` = the device context.
unsafe extern "C" fn evt_usb_interrupt_pipe_read_complete(
    _pipe: WDFUSBPIPE,
    buffer: WDFMEMORY,
    num_bytes_transferred: usize,
    context: WDFCONTEXT,
) {
    // SAFETY: the reader context is the device context (configure_continuous_reader), which lives as
    // long as the device; the framework only runs reader completions while the reader exists.
    let Some(context) = (unsafe { context.cast::<DeviceContext>().cast_const().as_ref() }) else {
        return;
    };
    let packet = if buffer.is_null() {
        &[][..]
    } else {
        // SAFETY: the reader's WDFMEMORY, valid for the duration of this callback.
        let (bytes, size) = unsafe { memory_buffer(buffer) };
        // SAFETY: the buffer holds `size` bytes, valid and not written by anyone else until this
        // callback returns; at most `size` bytes are exposed.
        unsafe { slice_or_empty(bytes.cast_const(), num_bytes_transferred.min(size)) }
    };
    let mut queue = InputQueue::new(context.input_queue());
    let mut io = DeviceIo::new(context);
    let before = context.state.snapshot();
    let outcome = input::on_read_complete(
        &context.state,
        packet,
        &mut queue,
        &mut io,
        query_performance_counter,
    );
    trace::read_complete(outcome, num_bytes_transferred);
    if let input::ReadCompleteOutcome::EmergencyReset { .. } = outcome {
        trace::state_changed("AmtPtpEmergResetDevice", &before, &context.state.snapshot());
    }
}

/// `AmtPtpEvtUsbInterruptReadersFailed` (InputInterrupt.c:213-226): returns TRUE, so the framework
/// resets the pipe and restarts the reader. (The Rust port also traces the failure.)
///
/// # Safety
/// Called by the framework's continuous reader.
unsafe extern "C" fn evt_usb_interrupt_readers_failed(
    _pipe: WDFUSBPIPE,
    status: NTSTATUS,
    usbd_status: USBD_STATUS,
) -> BOOLEAN {
    trace::readers_failed(status, usbd_status);
    BOOLEAN::from(pnp::READERS_FAILED_RESULT)
}

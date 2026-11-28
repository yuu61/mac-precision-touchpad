//! The WDF USB calls behind the core's [`UsbHardware`] and [`ControlTransfer`] traits: one WDF call per
//! method, in the order the core (= the C code) makes them.
//!
//! | core method | WDF call (C site) |
//! |---|---|
//! | `create_usb_device` | `WdfUsbTargetDeviceCreate(Device, WDF_NO_OBJECT_ATTRIBUTES, ..)` (Device.c:180) |
//! | `device_descriptor` | `WdfUsbTargetDeviceGetDeviceDescriptor` (Device.c:194) |
//! | `retrieve_device_traits` | `WDF_USB_DEVICE_INFORMATION_INIT` + `WdfUsbTargetDeviceRetrieveInformation` (Device.c:212) |
//! | `select_single_interface` | `..._SELECT_CONFIG_PARAMS_INIT_SINGLE_INTERFACE` + `WdfUsbTargetDeviceSelectConfig` (Device.c:437) |
//! | `configured_pipe` | `WDF_USB_PIPE_INFORMATION_INIT` + `WdfUsbInterfaceGetConfiguredPipe` (Device.c:458) |
//! | `set_no_maximum_packet_size_check` | `WdfUsbTargetPipeSetNoMaximumPacketSizeCheck` (Device.c:469) |
//! | `interrupt_pipe_selected` | the assignment `InterruptPipe = pipe` (Device.c:479) |
//! | `configure_continuous_reader` | `WDF_USB_CONTINUOUS_READER_CONFIG_INIT` + `WdfUsbTargetPipeConfigContinuousReader` (Interrupt.c:54-65) |
//! | `start_pipe` / `stop_pipe` | `WdfIoTargetStart` / `WdfIoTargetStop(.., WdfIoTargetCancelSentIo)` on `WdfUsbTargetPipeGetIoTarget` (Device.c:325, 383) |
//! | `query_performance_frequency` | Cache the QPC frequency at D0 entry |
//! | `control_transfer` | `WdfUsbTargetDeviceSendControlTransferSynchronously(UsbDevice, WDF_NO_HANDLE, NULL, ..)` (Device.c:566, 607) |
//!
//! IRQL: everything here is `PASSIVE_LEVEL` (`PrepareHardware`, `D0Entry`, `D0Exit`, and `SET_FEATURE`, which
//! hidclass sends at `PASSIVE_LEVEL`). None of it is in a `PAGE` section, because D0Entry/D0Exit and the
//! I/O path call it from non-paged code (as in C).

use core::ptr;

use amtptp_usbkm_core::lifecycle::{ContinuousReaderConfig, UsbHardware, UsbPipeInfo, UsbPipeType};
use amtptp_usbkm_core::state::UsbDeviceIds;
use amtptp_usbkm_core::status::NtStatus;
use amtptp_usbkm_core::wellspring::{
    ControlTransfer, ModeSwitch, UsbSetupPacket, WELLSPRING_DATA_LEN,
};
use tracelogging as tlg;
use wdf_support::init::{
    memory_descriptor_init_buffer, sync_send_options, usb_continuous_reader_config_init,
    usb_device_information_init, usb_device_select_config_params_init_single_interface,
    usb_pipe_information_init,
};
use wdf_support::ntstatus::{NT_SUCCESS, STATUS_INVALID_DEVICE_STATE};
use wdk_sys::{
    _WDF_IO_TARGET_SENT_IO_ACTION, _WDF_USB_DEVICE_TRAITS, _WDF_USB_PIPE_TYPE, ULONG,
    USB_DEVICE_DESCRIPTOR, WDF_NO_HANDLE, WDF_NO_OBJECT_ATTRIBUTES, WDF_USB_CONTROL_SETUP_PACKET,
    WDFDEVICE, WDFIOTARGET, WDFIOTARGET__, WDFREQUEST__, WDFUSBPIPE,
    call_unsafe_wdf_function_binding,
};

use crate::PROVIDER;
use crate::context::DeviceContext;
use crate::interrupt::{evt_usb_interrupt_pipe_read_complete, evt_usb_interrupt_readers_failed};
use crate::trace::KW_DEVICE;

// The memory descriptor length (`wLength`) fits the ULONG field.
const TRANSFER_LENGTH: ULONG = {
    assert!(WELLSPRING_DATA_LEN <= ULONG::MAX as usize);
    WELLSPRING_DATA_LEN as ULONG
};

/// The device's WDF USB objects for one PnP/power callback or one `SET_FEATURE` request.
pub(crate) struct UsbIo<'a> {
    device: WDFDEVICE,
    context: &'a DeviceContext,
    /// The pipe `WdfUsbInterfaceGetConfiguredPipe` returned last in the current selection (index, handle):
    /// the C local `pipe` of the `SelectInterruptInterface` loop.
    visited_pipe: Option<(u8, WDFUSBPIPE)>,
}

impl<'a> UsbIo<'a> {
    /// # Safety
    /// `device` is the live `WDFDEVICE` that owns `context`; the caller runs at `PASSIVE_LEVEL` whenever it
    /// calls a method that sends a synchronous request (control transfers, select config, target stop).
    pub(crate) const unsafe fn new(device: WDFDEVICE, context: &'a DeviceContext) -> Self {
        Self {
            device,
            context,
            visited_pipe: None,
        }
    }

    /// The `WDFUSBPIPE` stored as `InterruptPipe` for pipe `index` (the core only ever passes the index it
    /// selected), or NULL.
    fn pipe(&self, index: u8) -> WDFUSBPIPE {
        if self.context.state.interrupt_pipe() == Some(index) {
            self.context.interrupt_pipe()
        } else {
            ptr::null_mut()
        }
    }
}

impl ControlTransfer for UsbIo<'_> {
    fn serialized<R, F: FnOnce(&mut ModeSwitch<'_>) -> R>(&mut self, f: F) -> R {
        // SAFETY: DeviceAdd initialized the lock. All mode switches run at PASSIVE_LEVEL.
        // The device context and its nonpaged transfer buffer outlive the synchronous I/O.
        unsafe {
            self.context.mode_switch_lock.with(|buffer| {
                f(&mut |register, on| {
                    register.set_mode(buffer, on, |setup, data| self.control_transfer(setup, data))
                })
            })
        }
    }
}

impl UsbIo<'_> {
    fn control_transfer(
        &self,
        setup: &UsbSetupPacket,
        data: &mut [u8; WELLSPRING_DATA_LEN],
    ) -> NtStatus {
        let usb_device = self.context.usb_device();
        if usb_device.is_null() {
            // KM-X5 (P2): no USB target yet (SET_FEATURE before the first PrepareHardware). The C sends to a
            // NULL handle, which is a bugcheck.
            return STATUS_INVALID_DEVICE_STATE;
        }
        // The setup packet exactly as the C builds it (WDF_USB_CONTROL_SETUP_PACKET_INIT + Type = Class):
        // the core's bytes with wLength left 0. The framework derives wLength from the memory descriptor
        // (4 bytes), giving the wire bytes a1 01 02 03 02 00 04 00 / 21 09 02 03 02 00 04 00 whichever layer
        // fills it in, as for the C.
        let [
            bm_request_type,
            b_request,
            value_lo,
            value_hi,
            index_lo,
            index_hi,
            _,
            _,
        ] = setup.to_bytes();
        let mut packet = WDF_USB_CONTROL_SETUP_PACKET::default();
        packet.Generic.Bytes = [
            bm_request_type,
            b_request,
            value_lo,
            value_hi,
            index_lo,
            index_hi,
            0,
            0,
        ];
        // WDF_MEMORY_DESCRIPTOR_INIT_BUFFER over the 4-byte non-paged WDFMEMORY buffer.
        let mut descriptor =
            memory_descriptor_init_buffer(data.as_mut_ptr().cast(), TRANSFER_LENGTH);
        let mut transferred: ULONG = 0;
        // Not in C (no options, no timeout): a device that never answers would block the PnP/power
        // or dispatch thread forever (B-3).
        let mut options = sync_send_options(0);
        // SAFETY: `usb_device` is the live WDFUSBDEVICE of this device; WDF_NO_HANDLE request as in C;
        // `options` (SYNC_IO_TIMEOUT), `packet`, `descriptor` and `transferred` are valid for the
        // synchronous call, and `data` (the WDFMEMORY buffer) outlives it (on a timeout the framework
        // waits for the cancelled transfer). PASSIVE_LEVEL (UsbIo::new contract; KMDF checks the IRQL
        // only with its verifier on, otherwise a caller at DISPATCH_LEVEL blocks in the framework's
        // synchronous wait, which is expected to bugcheck, as for the C).
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetDeviceSendControlTransferSynchronously,
                usb_device,
                WDF_NO_HANDLE.cast::<WDFREQUEST__>(),
                &raw mut options,
                &raw mut packet,
                &raw mut descriptor,
                &raw mut transferred
            )
        };
        let bytes = setup.to_bytes();
        if NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "WellspringControlTransfer",
                level(Verbose),
                keyword(KW_DEVICE),
                binary("Setup", &bytes),
                u32("BytesTransferred", &transferred)
            );
        } else {
            // The C traces the read failure with cbTransferred and um_size, and the write failure.
            tlg::write_event!(
                PROVIDER,
                "WellspringControlTransferFailed",
                level(Error),
                keyword(KW_DEVICE),
                binary("Setup", &bytes),
                win_ntstatus("Status", &status),
                u32("BytesTransferred", &transferred)
            );
        }
        status
    }
}

impl UsbHardware for UsbIo<'_> {
    fn create_usb_device(&mut self) -> NtStatus {
        let mut usb_device = ptr::null_mut();
        // SAFETY: `self.device` is the live device (UsbIo::new contract); PASSIVE_LEVEL (PrepareHardware).
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetDeviceCreate,
                self.device,
                WDF_NO_OBJECT_ATTRIBUTES,
                &raw mut usb_device
            )
        };
        if NT_SUCCESS(status) {
            self.context.set_usb_device(usb_device);
        } else {
            tlg::write_event!(
                PROVIDER,
                "WdfUsbTargetDeviceCreateFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status)
            );
        }
        status
    }

    fn device_descriptor(&mut self) -> UsbDeviceIds {
        let usb_device = self.context.usb_device();
        let mut descriptor = USB_DEVICE_DESCRIPTOR::default();
        if !usb_device.is_null() {
            // SAFETY: live WDFUSBDEVICE (created before this is called); `descriptor` is a valid
            // out-buffer of the right type.
            unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfUsbTargetDeviceGetDeviceDescriptor,
                    usb_device,
                    &raw mut descriptor
                );
            }
        }
        // USB_DEVICE_DESCRIPTOR is packed: copy the fields out by value.
        let ids = UsbDeviceIds {
            vendor_id: descriptor.idVendor,
            product_id: descriptor.idProduct,
            bcd_device: descriptor.bcdDevice,
        };
        tlg::write_event!(
            PROVIDER,
            "UsbDeviceDescriptor",
            level(Informational),
            keyword(KW_DEVICE),
            u16_hex("VendorId", &ids.vendor_id),
            u16_hex("ProductId", &ids.product_id),
            u16_hex("BcdDevice", &ids.bcd_device)
        );
        ids
    }

    fn retrieve_device_traits(&mut self) -> Result<u32, NtStatus> {
        let mut information = usb_device_information_init();
        // SAFETY: live WDFUSBDEVICE; `information` is initialized (Size set). PASSIVE_LEVEL.
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetDeviceRetrieveInformation,
                self.context.usb_device(),
                &raw mut information
            )
        };
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "WdfUsbTargetDeviceRetrieveInformationFailed",
                level(Warning),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status)
            );
            return Err(status);
        }
        let traits = information.Traits;
        // The WDF_USB_DEVICE_TRAITS enum constants are small positive ints (1, 2, 4); `as u32` keeps them.
        let high_speed =
            traits & _WDF_USB_DEVICE_TRAITS::WDF_USB_DEVICE_TRAIT_AT_HIGH_SPEED as u32 != 0;
        let self_powered =
            traits & _WDF_USB_DEVICE_TRAITS::WDF_USB_DEVICE_TRAIT_SELF_POWERED as u32 != 0;
        let remote_wake =
            traits & _WDF_USB_DEVICE_TRAITS::WDF_USB_DEVICE_TRAIT_REMOTE_WAKE_CAPABLE as u32 != 0;
        tlg::write_event!(
            PROVIDER,
            "UsbDeviceTraits",
            level(Informational),
            keyword(KW_DEVICE),
            u32_hex("Traits", &traits),
            bool8("IsDeviceHighSpeed", &high_speed),
            bool8("IsDeviceSelfPowered", &self_powered),
            bool8("IsDeviceRemoteWakeable", &remote_wake)
        );
        Ok(traits)
    }

    fn select_single_interface(&mut self) -> Result<u8, NtStatus> {
        let mut params = usb_device_select_config_params_init_single_interface();
        // SAFETY: live WDFUSBDEVICE; `params` initialized for a single-interface selection;
        // WDF_NO_OBJECT_ATTRIBUTES as in C. PASSIVE_LEVEL (PrepareHardware).
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetDeviceSelectConfig,
                self.context.usb_device(),
                WDF_NO_OBJECT_ATTRIBUTES,
                &raw mut params
            )
        };
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "WdfUsbTargetDeviceSelectConfigFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status)
            );
            return Err(status);
        }
        // SAFETY: `Type` is WdfUsbTargetDeviceSelectConfigTypeSingleInterface, so `SingleInterface` is the
        // union member WDF filled in; plain data copy.
        let single = unsafe { params.Types.SingleInterface };
        self.context
            .set_usb_interface(single.ConfiguredUsbInterface);
        Ok(single.NumberConfiguredPipes)
    }

    fn configured_pipe(&mut self, index: u8) -> UsbPipeInfo {
        let mut information = usb_pipe_information_init();
        // SAFETY: the interface handle comes from the successful WdfUsbTargetDeviceSelectConfig just
        // before (the core calls this only after it); `index` < NumberConfiguredPipes; `information` is
        // initialized.
        let pipe = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbInterfaceGetConfiguredPipe,
                self.context.usb_interface(),
                index,
                &raw mut information
            )
        };
        self.visited_pipe = Some((index, pipe));
        let pipe_type = information.PipeType;
        tlg::write_event!(
            PROVIDER,
            "UsbPipe",
            level(Informational),
            keyword(KW_DEVICE),
            u8("Index", &index),
            i32("PipeType", &pipe_type),
            u8_hex("EndpointAddress", &information.EndpointAddress),
            u32("MaximumPacketSize", &information.MaximumPacketSize)
        );
        let pipe_type = match pipe_type {
            _WDF_USB_PIPE_TYPE::WdfUsbPipeTypeControl => UsbPipeType::Control,
            _WDF_USB_PIPE_TYPE::WdfUsbPipeTypeIsochronous => UsbPipeType::Isochronous,
            _WDF_USB_PIPE_TYPE::WdfUsbPipeTypeBulk => UsbPipeType::Bulk,
            _WDF_USB_PIPE_TYPE::WdfUsbPipeTypeInterrupt => UsbPipeType::Interrupt,
            _ => UsbPipeType::Invalid,
        };
        UsbPipeInfo {
            pipe_type,
            endpoint_address: information.EndpointAddress,
        }
    }

    fn set_no_maximum_packet_size_check(&mut self, index: u8) {
        if let Some((visited, pipe)) = self.visited_pipe
            && visited == index
            && !pipe.is_null()
        {
            // SAFETY: `pipe` was just returned by WdfUsbInterfaceGetConfiguredPipe for the current
            // selection, so it is a live WDFUSBPIPE.
            unsafe {
                call_unsafe_wdf_function_binding!(
                    WdfUsbTargetPipeSetNoMaximumPacketSizeCheck,
                    pipe
                );
            };
        }
    }

    fn interrupt_pipe_selected(&mut self, index: Option<u8>) {
        let pipe = match (index, self.visited_pipe) {
            (Some(index), Some((visited, pipe))) if visited == index => pipe,
            _ => ptr::null_mut(),
        };
        self.context.set_interrupt_pipe(pipe);
    }

    fn configure_continuous_reader(
        &mut self,
        index: u8,
        config: &ContinuousReaderConfig,
    ) -> NtStatus {
        let pipe = self.pipe(index);
        if pipe.is_null() {
            return STATUS_INVALID_DEVICE_STATE;
        }
        // WDF_USB_CONTINUOUS_READER_CONFIG_INIT(&cfg, ReadComplete, DeviceContext, TransferLength) plus
        // EvtUsbTargetPipeReadersFailed. The completion context is the device context, as in C.
        let mut reader = usb_continuous_reader_config_init(
            Some(evt_usb_interrupt_pipe_read_complete),
            ptr::from_ref(self.context).cast_mut().cast(),
            config.transfer_length,
        );
        reader.HeaderLength = config.header_length;
        reader.TrailerLength = config.trailer_length;
        reader.NumPendingReads = config.num_pending_reads;
        reader.EvtUsbTargetPipeReadersFailed = Some(evt_usb_interrupt_readers_failed);
        // SAFETY: `pipe` is the live interrupt pipe of the current selection; `reader` is initialized and
        // its context pointer (the device context) lives as long as the device, which outlives the
        // reader. PASSIVE_LEVEL (PrepareHardware).
        let status = unsafe {
            call_unsafe_wdf_function_binding!(
                WdfUsbTargetPipeConfigContinuousReader,
                pipe,
                &raw mut reader
            )
        };
        if !NT_SUCCESS(status) {
            // The core returns this status from PrepareHardware.
            tlg::write_event!(
                PROVIDER,
                "WdfUsbTargetPipeConfigContinuousReaderFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status),
                usize("TransferLength", &config.transfer_length)
            );
        }
        status
    }

    fn start_pipe(&mut self, index: u8) -> NtStatus {
        let pipe = self.pipe(index);
        if pipe.is_null() {
            return STATUS_INVALID_DEVICE_STATE;
        }
        // SAFETY: live interrupt pipe (a pipe is its own I/O target); PASSIVE_LEVEL (D0Entry).
        let status =
            unsafe { call_unsafe_wdf_function_binding!(WdfIoTargetStart, pipe_io_target(pipe)) };
        if !NT_SUCCESS(status) {
            tlg::write_event!(
                PROVIDER,
                "InterruptPipeStartFailed",
                level(Error),
                keyword(KW_DEVICE),
                win_ntstatus("Status", &status)
            );
        }
        status
    }

    fn stop_pipe(&mut self, index: u8) {
        let pipe = self.pipe(index);
        if pipe.is_null() {
            return;
        }
        // SAFETY: live interrupt pipe (a pipe is its own I/O target); WdfIoTargetCancelSentIo waits for the
        // reader's reads, which needs PASSIVE_LEVEL (D0Exit).
        unsafe {
            call_unsafe_wdf_function_binding!(
                WdfIoTargetStop,
                pipe_io_target(pipe),
                _WDF_IO_TARGET_SENT_IO_ACTION::WdfIoTargetCancelSentIo
            );
        }
    }

    fn query_performance_frequency(&mut self) -> i64 {
        wdf_support::time::query_performance_frequency()
    }
}

/// `WdfUsbTargetPipeGetIoTarget(Pipe)`: a `FORCEINLINE` in `wdfusb.h` (not in the WDF function table)
/// whose body is `return (WDFIOTARGET) Pipe;`.
const fn pipe_io_target(pipe: WDFUSBPIPE) -> WDFIOTARGET {
    pipe.cast::<WDFIOTARGET__>()
}

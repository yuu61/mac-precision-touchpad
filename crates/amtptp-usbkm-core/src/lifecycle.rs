//! PnP/power sequencing: `AmtPtpDeviceUsbKmEvtDevicePrepareHardware` (Device.c:130-272),
//! `SelectInterruptInterface` (Device.c:418-501), `AmtPtpConfigContReaderForInterruptEndPoint`
//! (Interrupt.c:14-87), `AmtPtpEvtDeviceD0Entry` / `AmtPtpEvtDeviceD0Exit` (Device.c:275-416), the
//! continuous reader's `EvtUsbTargetPipeReadersFailed` and the queue's `EvtIoStop`, plus the static
//! configuration the glue passes to WDF (queues, reader, device interface).
//!
//! The WDF calls are behind [`UsbHardware`]; the order of the calls, which failures are returned and
//! which are ignored, and every context update live here.

use crate::config::{config_index_for_pid, reader_transfer_length};
use crate::state::{DeviceState, UsbDeviceIds};
use crate::status::{NtStatus, STATUS_INVALID_DEVICE_STATE, STATUS_SUCCESS, nt_success};
use crate::wellspring::{ControlTransfer, set_wellspring_mode};

/// `WDF_USB_PIPE_TYPE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum UsbPipeType {
    /// `WdfUsbPipeTypeInvalid` (0).
    Invalid = 0,
    /// `WdfUsbPipeTypeControl` (1).
    Control = 1,
    /// `WdfUsbPipeTypeIsochronous` (2).
    Isochronous = 2,
    /// `WdfUsbPipeTypeBulk` (3).
    Bulk = 3,
    /// `WdfUsbPipeTypeInterrupt` (4).
    Interrupt = 4,
}

/// The fields of `WDF_USB_CONTINUOUS_READER_CONFIG` the C sets (via `WDF_USB_CONTINUOUS_READER_CONFIG_INIT`
/// plus `EvtUsbTargetPipeReadersFailed`). The glue adds the callbacks: `EvtUsbTargetPipeReadComplete`
/// (context = the device), and `EvtUsbTargetPipeReadersFailed` returning [`readers_failed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContinuousReaderConfig {
    /// `TransferLength`: 526 for TYPE4 (`HEADER_TYPE4 + FSIZE_TYPE4 * MAX_FINGERS`).
    pub transfer_length: usize,
    /// `HeaderLength`: 0.
    pub header_length: usize,
    /// `TrailerLength`: 0.
    pub trailer_length: usize,
    /// `NumPendingReads`: 0 = framework default.
    pub num_pending_reads: u8,
}

/// The WDF USB operations of the PnP/power callbacks (implemented by the glue). `PASSIVE_LEVEL`.
///
/// Pipes are identified by their index in the selected interface (`WdfUsbInterfaceGetConfiguredPipe`
/// index); the glue keeps the matching `WDFUSBPIPE` handles.
pub trait UsbHardware: ControlTransfer {
    /// `WdfUsbTargetDeviceCreate(Device, WDF_NO_OBJECT_ATTRIBUTES, &UsbDevice)`. Called only while no USB
    /// target exists ([`DeviceState::usb_device_created`] is false).
    fn create_usb_device(&mut self) -> NtStatus;

    /// `WdfUsbTargetDeviceGetDeviceDescriptor` (cannot fail).
    fn device_descriptor(&mut self) -> UsbDeviceIds;

    /// `WdfUsbTargetDeviceRetrieveInformation`: `Ok(Traits)` or the failure status (which the caller
    /// ignores; the C stores traits 0 and goes on).
    ///
    /// # Errors
    /// Returns the WDF failure status if the USB traits cannot be retrieved.
    fn retrieve_device_traits(&mut self) -> Result<u32, NtStatus>;

    /// `WdfUsbTargetDeviceSelectConfig` with `WDF_USB_DEVICE_SELECT_CONFIG_PARAMS_INIT_SINGLE_INTERFACE`.
    /// Stores `ConfiguredUsbInterface` and returns `NumberConfiguredPipes`, or the failure status.
    ///
    /// # Errors
    /// Returns the WDF failure status if the interface cannot be selected.
    fn select_single_interface(&mut self) -> Result<u8, NtStatus>;

    /// `WdfUsbInterfaceGetConfiguredPipe(UsbInterface, index, &PipeInfo)`: the pipe's `PipeType`.
    fn configured_pipe_type(&mut self, index: u8) -> UsbPipeType;

    /// `WdfUsbTargetPipeSetNoMaximumPacketSizeCheck` on pipe `index`.
    fn set_no_maximum_packet_size_check(&mut self, index: u8);

    /// The C assignment `InterruptPipe = pipe` (`Some(index)`, made right after
    /// [`UsbHardware::configured_pipe_type`] and [`UsbHardware::set_no_maximum_packet_size_check`] of that
    /// pipe), or the reset of `InterruptPipe` to NULL at the start of a selection (`None`, the B20 P2 fix).
    /// The glue keeps its `WDFUSBPIPE` handle in sync with [`DeviceState::interrupt_pipe`] here: the handle
    /// is the one `WdfUsbInterfaceGetConfiguredPipe` returned for `index`. Default: nothing.
    fn interrupt_pipe_selected(&mut self, _index: Option<u8>) {}

    /// `WdfUsbTargetPipeConfigContinuousReader` on pipe `index`. The caller ignores the status (spec B9):
    /// trace failures here.
    fn configure_continuous_reader(
        &mut self,
        index: u8,
        config: &ContinuousReaderConfig,
    ) -> NtStatus;

    /// `WdfIoTargetStart(WdfUsbTargetPipeGetIoTarget(pipe))`.
    fn start_pipe(&mut self, index: u8) -> NtStatus;

    /// `WdfIoTargetStop(WdfUsbTargetPipeGetIoTarget(pipe), WdfIoTargetCancelSentIo)`.
    fn stop_pipe(&mut self, index: u8);

    /// `KeQueryPerformanceCounter(&x)` and return `x.QuadPart`: the value written to the
    /// **out-parameter**, i.e. the QPC frequency (the counter, the return value, is discarded as in C).
    fn query_performance_counter_out(&mut self) -> i64;
}

/// `ContinuousReaderConfig` for the device's config row, or `None` for an unknown trackpad type (C:
/// `transferLength <= 0` → `STATUS_UNKNOWN_REVISION`, which is masked anyway; no table row has one).
pub fn continuous_reader_config(state: &DeviceState) -> Option<ContinuousReaderConfig> {
    let transfer_length = reader_transfer_length(state.config().profile.tp_type);
    if transfer_length == 0 {
        return None;
    }
    Some(ContinuousReaderConfig {
        transfer_length,
        header_length: 0,
        trailer_length: 0,
        num_pending_reads: 0,
    })
}

/// `SelectInterruptInterface`: selects the single interface, then walks the configured pipes in order,
/// calling `SetNoMaximumPacketSizeCheck` on **every visited pipe** (it must precede the reader
/// configuration: 526 is not a multiple of any realistic `MaxPacketSize`, spec B28), and takes the first
/// interrupt pipe regardless of direction (`tp_ep` is ignored, spec B20, preserved).
///
/// Returns the select-config failure, `STATUS_INVALID_DEVICE_STATE` if there is no interrupt pipe, or
/// success.
///
/// P2 (spec B20): `InterruptPipe` is reset before the selection. The C keeps the previous handle, which
/// the reselection has deleted, so a restart that finds no interrupt pipe would use a freed pipe; here it
/// fails with `STATUS_INVALID_DEVICE_STATE`.
pub fn select_interrupt_interface<H: UsbHardware + ?Sized>(
    state: &DeviceState,
    hw: &mut H,
) -> NtStatus {
    state.set_interrupt_pipe(None);
    hw.interrupt_pipe_selected(None);
    let pipes = match hw.select_single_interface() {
        Ok(pipes) => pipes,
        Err(status) => return status,
    };
    for index in 0..pipes {
        let pipe_type = hw.configured_pipe_type(index);
        hw.set_no_maximum_packet_size_check(index);
        if pipe_type == UsbPipeType::Interrupt {
            state.set_interrupt_pipe(Some(index));
            hw.interrupt_pipe_selected(Some(index));
            break;
        }
    }
    if state.interrupt_pipe().is_none() {
        return STATUS_INVALID_DEVICE_STATE;
    }
    STATUS_SUCCESS
}

/// `AmtPtpConfigContReaderForInterruptEndPoint`: configures the continuous reader on the interrupt pipe
/// and **always returns `STATUS_SUCCESS`** (spec B9, preserved: a failed configuration leaves the device
/// without input but `PrepareHardware` succeeds).
pub fn configure_continuous_reader<H: UsbHardware + ?Sized>(
    state: &DeviceState,
    hw: &mut H,
) -> NtStatus {
    if let (Some(config), Some(pipe)) = (continuous_reader_config(state), state.interrupt_pipe()) {
        // The status is masked, as in C (Interrupt.c:86). The glue traces it.
        let _masked = hw.configure_continuous_reader(pipe, &config);
    }
    STATUS_SUCCESS
}

/// `AmtPtpDeviceUsbKmEvtDevicePrepareHardware`.
///
/// 1. create the USB target once (a restart reuses it); failure → return it;
/// 2. read the device descriptor and select the config row by `idProduct` (unknown → fallback row);
/// 3. retrieve the USB traits (failure ignored, traits = 0);
/// 4. [`select_interrupt_interface`]; failure → return it;
/// 5. [`configure_continuous_reader`] (always success);
/// 6. `PtpReportButton = PtpReportTouch = TRUE`.
pub fn prepare_hardware<H: UsbHardware + ?Sized>(state: &DeviceState, hw: &mut H) -> NtStatus {
    if !state.usb_device_created() {
        let status = hw.create_usb_device();
        if !nt_success(status) {
            return status;
        }
        state.set_usb_device_created();
    }

    let ids = hw.device_descriptor();
    state.set_usb_device_ids(ids);
    // `DeviceInfo == NULL` → STATUS_INVALID_DEVICE_STATE is unreachable: the lookup never fails.
    state.set_config_index(config_index_for_pid(ids.product_id));

    let traits = hw.retrieve_device_traits().unwrap_or(0);
    state.set_usb_device_traits(traits);

    let status = select_interrupt_interface(state, hw);
    if !nt_success(status) {
        return status;
    }

    let status = configure_continuous_reader(state, hw);
    if !nt_success(status) {
        return status;
    }

    state.set_input_gates_on();
    status
}

/// `AmtPtpEvtDeviceD0Entry`.
///
/// 1. if `PtpReportButton || IsWellspringModeOn` (always true): wellspring mode on; a failure is ignored
///    (spec B11, preserved: the device then stays in mouse mode);
/// 2. `LastReportTime` = the `KeQueryPerformanceCounter` out-parameter (the frequency, spec B1);
/// 3. start the interrupt pipe's I/O target (starts the continuous reader) and return its status.
///
/// The mode switch happens before the reader starts. Without an interrupt pipe (impossible: KMDF calls
/// `D0Entry` only after a successful `PrepareHardware`, which guarantees one) the C would dereference NULL;
/// this returns `STATUS_INVALID_DEVICE_STATE` instead.
pub fn d0_entry<H: UsbHardware + ?Sized>(state: &DeviceState, hw: &mut H) -> NtStatus {
    let gates = state.input_gates();
    if gates.report_button || state.wellspring_mode_on() {
        let _ignored = set_wellspring_mode(state, hw, true);
    }
    state.set_last_report_time(hw.query_performance_counter_out());
    match state.interrupt_pipe() {
        Some(pipe) => hw.start_pipe(pipe),
        None => STATUS_INVALID_DEVICE_STATE,
    }
}

/// `AmtPtpEvtDeviceD0Exit`: stop the interrupt pipe (cancelling the reader's reads), then wellspring
/// mode off, and **return the mode-switch status** (spec B10, preserved: a failed OFF transfer fails the
/// power-down).
pub fn d0_exit<H: UsbHardware + ?Sized>(state: &DeviceState, hw: &mut H) -> NtStatus {
    if let Some(pipe) = state.interrupt_pipe() {
        hw.stop_pipe(pipe);
    }
    set_wellspring_mode(state, hw, false)
}

/// `AmtPtpEvtUsbInterruptReadersFailed`: always `TRUE` ("reset the pipe and restart the readers"),
/// whatever the status. Retries indefinitely.
#[must_use]
pub const fn readers_failed(_status: NtStatus, _usbd_status: i32) -> bool {
    true
}

/// What `AmtPtpDeviceUsbKmEvtIoStop` does with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoStopAction {
    /// Nothing: no `WdfRequestStopAcknowledge`, no completion (the C only traces). Requests of the
    /// default queue are never driver-owned after the dispatch callback returns, so there is nothing to
    /// stop.
    None,
}

/// `AmtPtpDeviceUsbKmEvtIoStop(Queue, Request, ActionFlags)`: trace only.
#[must_use]
pub const fn io_stop(_action_flags: u32) -> IoStopAction {
    IoStopAction::None
}

/// `WDF_IO_QUEUE_DISPATCH_TYPE` of a queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum QueueDispatch {
    /// `WdfIoQueueDispatchParallel` (2).
    Parallel = 2,
    /// `WdfIoQueueDispatchManual` (3).
    Manual = 3,
}

/// `WDF_TRI_STATE` of `WDF_IO_QUEUE_CONFIG.PowerManaged`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum PowerManaged {
    /// `WdfFalse` (0).
    False = 0,
    /// `WdfUseDefault` (2). For a filter (`WdfFdoInitSetFilter`) this means "not power-managed".
    UseDefault = 2,
}

/// The `WDF_IO_QUEUE_CONFIG` of one queue (Queue.c:24-103). Object attributes: none (parent = device).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueSpec {
    /// `DispatchType`.
    pub dispatch: QueueDispatch,
    /// `PowerManaged`.
    pub power_managed: PowerManaged,
    /// `DefaultQueue` (`WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE`).
    pub default_queue: bool,
    /// `EvtIoInternalDeviceControl` registered (the dispatcher, see [`crate::hid::route_internal_ioctl`]).
    pub internal_device_control: bool,
    /// `EvtIoStop` registered ([`io_stop`]).
    pub io_stop: bool,
}

/// The default queue: `WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&cfg, WdfIoQueueDispatchParallel)` with
/// `EvtIoInternalDeviceControl` and `EvtIoStop`. The handle is not kept.
pub const DEFAULT_QUEUE: QueueSpec = QueueSpec {
    dispatch: QueueDispatch::Parallel,
    power_managed: PowerManaged::UseDefault,
    default_queue: true,
    internal_device_control: true,
    io_stop: true,
};

/// The `InputQueue` for pended `READ_REPORTs`: `WDF_IO_QUEUE_CONFIG_INIT(&cfg, WdfIoQueueDispatchManual)`,
/// `PowerManaged = WdfFalse`, no callbacks. Never started, stopped or purged by the driver.
pub const INPUT_QUEUE: QueueSpec = QueueSpec {
    dispatch: QueueDispatch::Manual,
    power_managed: PowerManaged::False,
    default_queue: false,
    internal_device_control: false,
    io_stop: false,
};

/// A GUID in its `DEFINE_GUID` form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guid {
    /// `Data1`.
    pub data1: u32,
    /// `Data2`.
    pub data2: u16,
    /// `Data3`.
    pub data3: u16,
    /// `Data4`.
    pub data4: [u8; 8],
}

impl Guid {
    /// The 16 in-memory bytes (Data1..Data3 little-endian).
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 16] {
        let [a0, a1, a2, a3] = self.data1.to_le_bytes();
        let [b0, b1] = self.data2.to_le_bytes();
        let [c0, c1] = self.data3.to_le_bytes();
        let [d0, d1, d2, d3, d4, d5, d6, d7] = self.data4;
        [
            a0, a1, a2, a3, b0, b1, c0, c1, d0, d1, d2, d3, d4, d5, d6, d7,
        ]
    }
}

/// `GUID_DEVINTERFACE_AmtPtpDeviceUsbKm` {4aa332cc-5777-4afd-aa4e-95387330612a} (Public.h:22-24),
/// registered with `WdfDeviceCreateDeviceInterface(device, &guid, NULL)` after `WdfDeviceCreate`.
pub const DEVICE_INTERFACE_GUID: Guid = Guid {
    data1: 0x4aa3_32cc,
    data2: 0x5777,
    data3: 0x4afd,
    data4: [0xaa, 0x4e, 0x95, 0x38, 0x73, 0x30, 0x61, 0x2a],
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wellspring::{UsbSetupPacket, WELLSPRING_DATA_LEN};

    /// Pipes: Bulk, Interrupt, Interrupt. Records the selection hook and the pipe calls.
    struct PipeHw {
        pipe_types: [UsbPipeType; 3],
        log: [Option<(&'static str, Option<u8>)>; 16],
        len: usize,
        fail_select: bool,
    }

    impl PipeHw {
        fn new() -> Self {
            Self {
                pipe_types: [
                    UsbPipeType::Bulk,
                    UsbPipeType::Interrupt,
                    UsbPipeType::Interrupt,
                ],
                log: [None; 16],
                len: 0,
                fail_select: false,
            }
        }
        fn push(&mut self, name: &'static str, index: Option<u8>) {
            self.log[self.len] = Some((name, index));
            self.len += 1;
        }
        fn log(&self) -> &[Option<(&'static str, Option<u8>)>] {
            &self.log[..self.len]
        }
    }

    struct Buf([u8; WELLSPRING_DATA_LEN]);
    impl AsMut<[u8; WELLSPRING_DATA_LEN]> for Buf {
        fn as_mut(&mut self) -> &mut [u8; WELLSPRING_DATA_LEN] {
            &mut self.0
        }
    }

    impl ControlTransfer for PipeHw {
        type Buffer = Buf;
        fn allocate_transfer_buffer(&mut self) -> Result<Buf, NtStatus> {
            Ok(Buf([0; WELLSPRING_DATA_LEN]))
        }
        fn control_transfer(
            &mut self,
            _: &UsbSetupPacket,
            _: &mut [u8; WELLSPRING_DATA_LEN],
        ) -> NtStatus {
            STATUS_SUCCESS
        }
    }

    impl UsbHardware for PipeHw {
        fn create_usb_device(&mut self) -> NtStatus {
            STATUS_SUCCESS
        }
        fn device_descriptor(&mut self) -> UsbDeviceIds {
            UsbDeviceIds::default()
        }
        fn retrieve_device_traits(&mut self) -> Result<u32, NtStatus> {
            Ok(0)
        }
        fn select_single_interface(&mut self) -> Result<u8, NtStatus> {
            self.push("select", None);
            if self.fail_select { Err(-1) } else { Ok(3) }
        }
        fn configured_pipe_type(&mut self, index: u8) -> UsbPipeType {
            self.push("pipe_type", Some(index));
            self.pipe_types[usize::from(index)]
        }
        fn set_no_maximum_packet_size_check(&mut self, index: u8) {
            self.push("no_max", Some(index));
        }
        fn interrupt_pipe_selected(&mut self, index: Option<u8>) {
            self.push("selected", index);
        }
        fn configure_continuous_reader(
            &mut self,
            index: u8,
            _: &ContinuousReaderConfig,
        ) -> NtStatus {
            self.push("reader", Some(index));
            STATUS_SUCCESS
        }
        fn start_pipe(&mut self, _: u8) -> NtStatus {
            STATUS_SUCCESS
        }
        fn stop_pipe(&mut self, _: u8) {}
        fn query_performance_counter_out(&mut self) -> i64 {
            0
        }
    }

    #[test]
    fn interrupt_pipe_selected_hook_follows_the_c_assignment() {
        let state = DeviceState::new();
        let mut hw = PipeHw::new();
        assert_eq!(prepare_hardware(&state, &mut hw), STATUS_SUCCESS);
        assert_eq!(
            hw.log(),
            [
                Some(("selected", None)),
                Some(("select", None)),
                Some(("pipe_type", Some(0))),
                Some(("no_max", Some(0))),
                Some(("pipe_type", Some(1))),
                Some(("no_max", Some(1))),
                Some(("selected", Some(1))),
                Some(("reader", Some(1))),
            ]
        );
        assert_eq!(state.interrupt_pipe(), Some(1));

        // A failed reselection leaves no pipe (B20 fix): the hook saw the reset.
        let mut hw = PipeHw::new();
        hw.fail_select = true;
        assert_eq!(select_interrupt_interface(&state, &mut hw), -1);
        assert_eq!(hw.log(), [Some(("selected", None)), Some(("select", None))]);
        assert_eq!(state.interrupt_pipe(), None);
    }

    #[test]
    fn constants() {
        assert_eq!(
            DEVICE_INTERFACE_GUID.to_bytes(),
            [
                0xcc, 0x32, 0xa3, 0x4a, 0x77, 0x57, 0xfd, 0x4a, 0xaa, 0x4e, 0x95, 0x38, 0x73, 0x30,
                0x61, 0x2a
            ]
        );
        assert!(readers_failed(0, 0));
        assert_eq!(io_stop(1), IoStopAction::None);
        let cfg = continuous_reader_config(&DeviceState::new());
        assert_eq!(cfg.map(|c| c.transfer_length), Some(526));
    }
}

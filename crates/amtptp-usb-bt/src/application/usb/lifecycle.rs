//! PnP/power sequencing: `AmtPtpDeviceUsbKmEvtDevicePrepareHardware` (Device.c:130-272),
//! `SelectInterruptInterface` (Device.c:418-501), `AmtPtpConfigContReaderForInterruptEndPoint`
//! (Interrupt.c:14-87), `AmtPtpEvtDeviceD0Entry` / `AmtPtpEvtDeviceD0Exit` (Device.c:275-416), the
//! continuous reader's `EvtUsbTargetPipeReadersFailed` and the queue's `EvtIoStop`, plus the static
//! configuration the glue passes to WDF (queues and reader).
//!
//! The WDF calls are behind [`UsbHardware`]; the order of the calls, which failures are returned and
//! which are ignored, and every context update live here.

use amtptp_core::hid::status::{
    NtStatus, STATUS_CANCELLED, STATUS_DELETE_PENDING, STATUS_DEVICE_NOT_CONNECTED,
    STATUS_INVALID_DEVICE_STATE, STATUS_NO_SUCH_DEVICE, STATUS_SUCCESS, nt_success,
};

use super::state::{DeviceState, UsbDeviceIds};
use super::wellspring::{ControlTransfer, set_wellspring_mode};
use crate::infrastructure::usb::{config_index_for_pid, reader_transfer_length};

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

/// The fields of `WDF_USB_PIPE_INFORMATION` the pipe selection reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbPipeInfo {
    /// `PipeType`.
    pub pipe_type: UsbPipeType,
    /// `EndpointAddress`: bit 7 set for an IN (device-to-host) endpoint.
    pub endpoint_address: u8,
}

impl UsbPipeInfo {
    /// An interrupt IN pipe, the only kind a continuous reader can read from.
    #[must_use]
    pub const fn is_interrupt_in(self) -> bool {
        matches!(self.pipe_type, UsbPipeType::Interrupt) && self.endpoint_address & 0x80 != 0
    }
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

    /// `WdfUsbInterfaceGetConfiguredPipe(UsbInterface, index, &PipeInfo)`: the pipe's `PipeType` and
    /// `EndpointAddress`.
    fn configured_pipe(&mut self, index: u8) -> UsbPipeInfo;

    /// `WdfUsbTargetPipeSetNoMaximumPacketSizeCheck` on pipe `index`.
    fn set_no_maximum_packet_size_check(&mut self, index: u8);

    /// The C assignment `InterruptPipe = pipe` (`Some(index)`, made right after
    /// [`UsbHardware::configured_pipe`] and [`UsbHardware::set_no_maximum_packet_size_check`] of that
    /// pipe), or the reset of `InterruptPipe` to NULL at the start of a selection (`None`, the B20 P2 fix).
    /// The glue keeps its `WDFUSBPIPE` handle in sync with [`DeviceState::interrupt_pipe`] here: the handle
    /// is the one `WdfUsbInterfaceGetConfiguredPipe` returned for `index`. Default: nothing.
    fn interrupt_pipe_selected(&mut self, _index: Option<u8>) {}

    /// `WdfUsbTargetPipeConfigContinuousReader` on pipe `index`. A failure fails `PrepareHardware`.
    fn configure_continuous_reader(
        &mut self,
        index: u8,
        config: &ContinuousReaderConfig,
    ) -> NtStatus;

    /// `WdfIoTargetStart(WdfUsbTargetPipeGetIoTarget(pipe))`.
    fn start_pipe(&mut self, index: u8) -> NtStatus;

    /// `WdfIoTargetStop(WdfUsbTargetPipeGetIoTarget(pipe), WdfIoTargetCancelSentIo)`.
    fn stop_pipe(&mut self, index: u8);

    /// QPC frequency in ticks per second.
    fn query_performance_frequency(&mut self) -> i64;
}

/// `ContinuousReaderConfig` for the device's config row, or `None` for an unknown trackpad type.
/// Uses `num_pending_reads = 1` so interrupt packets arrive in hardware order.
pub fn continuous_reader_config(state: &DeviceState) -> Option<ContinuousReaderConfig> {
    let transfer_length = reader_transfer_length(state.config().profile.tp_type);
    if transfer_length == 0 {
        return None;
    }
    Some(ContinuousReaderConfig {
        transfer_length,
        header_length: 0,
        trailer_length: 0,
        num_pending_reads: 1,
    })
}

/// `SelectInterruptInterface`: selects the single interface, then walks the configured pipes in order,
/// calling `SetNoMaximumPacketSizeCheck` on **every visited pipe** (it must precede the reader
/// configuration: 526 is not a multiple of any realistic `MaxPacketSize`, spec B28), and takes the first
/// interrupt **IN** pipe (P3 fix: C took the first interrupt pipe whatever its direction, and a continuous
/// reader on an OUT pipe cannot work).
///
/// Returns the select-config failure, `STATUS_INVALID_DEVICE_STATE` if there is no interrupt IN pipe, or
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
        let pipe = hw.configured_pipe(index);
        hw.set_no_maximum_packet_size_check(index);
        if pipe.is_interrupt_in() {
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
/// and returns its status (P3 fix of spec B9: C always returned `STATUS_SUCCESS`, so a failed
/// configuration left a started device without input). Without a reader configuration or an interrupt
/// pipe (unreachable after [`select_interrupt_interface`] succeeded) it returns
/// `STATUS_INVALID_DEVICE_STATE`.
pub fn configure_continuous_reader<H: UsbHardware + ?Sized>(
    state: &DeviceState,
    hw: &mut H,
) -> NtStatus {
    match (continuous_reader_config(state), state.interrupt_pipe()) {
        (Some(config), Some(pipe)) => hw.configure_continuous_reader(pipe, &config),
        _ => STATUS_INVALID_DEVICE_STATE,
    }
}

/// `AmtPtpDeviceUsbKmEvtDevicePrepareHardware`.
///
/// 1. create the USB target once (a restart reuses it); failure → return it;
/// 2. read the device descriptor and select the config row by `idProduct` (unknown → fallback row);
/// 3. retrieve the USB traits (failure ignored, traits = 0);
/// 4. [`select_interrupt_interface`]; failure → return it;
/// 5. [`configure_continuous_reader`]; failure → return it;
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
/// 1. Switch wellspring multitouch mode on; if the mode switch fails, fail `D0Entry` with that
///    status instead of starting the pipe in mouse mode.
/// 2. Reset the reader failure counter, cache the QPC frequency, and restart the reporting clock.
/// 3. Start the interrupt pipe's I/O target (starts the continuous reader) and return its status.
pub fn d0_entry<H: UsbHardware + ?Sized>(state: &DeviceState, hw: &mut H) -> NtStatus {
    state.reset_reader_failures();
    let mode_status = hw.serialized(|hw| {
        state.request_raw(true);
        set_wellspring_mode(state, hw, true)
    });
    if !nt_success(mode_status) {
        return mode_status;
    }
    state.set_qpc_frequency(hw.query_performance_frequency());
    match state.interrupt_pipe() {
        Some(pipe) => hw.start_pipe(pipe),
        None => STATUS_INVALID_DEVICE_STATE,
    }
}

/// What `AmtPtpEvtDeviceD0Exit` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct D0ExitOutcome {
    /// The callback's return value: always `STATUS_SUCCESS`.
    pub status: NtStatus,
    /// Status of the wellspring-OFF transfer (C returned it from the callback), for the trace.
    pub mode_switch_status: NtStatus,
}

/// `AmtPtpEvtDeviceD0Exit`: stop the interrupt pipe (cancelling the reader's reads), then wellspring
/// mode off, best effort.
///
/// P3 fix of B10: C returned the mode-switch status, so a failed OFF transfer failed the power-down. On
/// a surprise removal (`D3Final`) the trackpad is already gone and the transfer always fails, and a failed
/// `D0Exit` makes the framework treat the power-down as a device failure. The switch is still attempted
/// on every target state (an orderly removal or sleep leaves the trackpad in mouse mode); its status is
/// only reported in [`D0ExitOutcome::mode_switch_status`].
pub fn d0_exit<H: UsbHardware + ?Sized>(state: &DeviceState, hw: &mut H) -> D0ExitOutcome {
    if let Some(pipe) = state.interrupt_pipe() {
        hw.stop_pipe(pipe);
    }
    D0ExitOutcome {
        status: STATUS_SUCCESS,
        mode_switch_status: hw.serialized(|hw| {
            state.request_raw(false);
            set_wellspring_mode(state, hw, false)
        }),
    }
}

/// Maximum consecutive continuous-reader pipe restarts before escalating to `WdfDeviceSetFailed`.
pub const MAX_READER_RESTARTS: u32 = 3;

/// Decision returned by [`on_readers_failed`] for `EvtUsbTargetPipeReadersFailed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderFailureAction {
    /// Return `TRUE` so WDF resets the pipe and restarts the continuous reader.
    Restart,
    /// Return `FALSE` quietly (e.g. cancelled during power-down or device removed).
    Stop,
    /// Return `FALSE` and call `WdfDeviceSetFailed(WdfDeviceFailedAttemptRestart)`.
    FailDevice,
}

/// Evaluates a continuous-reader failure against terminal removal/cancellation statuses and the
/// consecutive-restart budget ([`MAX_READER_RESTARTS`]).
#[must_use]
pub fn on_readers_failed(
    state: &DeviceState,
    status: NtStatus,
    _usbd_status: i32,
) -> ReaderFailureAction {
    if matches!(
        status,
        STATUS_CANCELLED
            | STATUS_DEVICE_NOT_CONNECTED
            | STATUS_NO_SUCH_DEVICE
            | STATUS_DELETE_PENDING
    ) {
        return ReaderFailureAction::Stop;
    }
    if state.note_reader_failure() <= MAX_READER_RESTARTS {
        ReaderFailureAction::Restart
    } else {
        ReaderFailureAction::FailDevice
    }
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
    /// `EvtIoInternalDeviceControl` registered (the dispatcher, see [`crate::usb::hid::route_internal_ioctl`]).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::usb::wellspring::ModeSwitch;

    const fn pipe(pipe_type: UsbPipeType, endpoint_address: u8) -> UsbPipeInfo {
        UsbPipeInfo {
            pipe_type,
            endpoint_address,
        }
    }

    /// Pipes: bulk IN, interrupt IN, interrupt IN. Records the selection hook and the pipe calls.
    struct PipeHw {
        pipes: [UsbPipeInfo; 3],
        log: [Option<(&'static str, Option<u8>)>; 32],
        len: usize,
        fail_select: bool,
        reader_status: NtStatus,
    }

    impl PipeHw {
        fn new() -> Self {
            Self::with_pipes([
                pipe(UsbPipeType::Bulk, 0x81),
                pipe(UsbPipeType::Interrupt, 0x83),
                pipe(UsbPipeType::Interrupt, 0x84),
            ])
        }
        const fn with_pipes(pipes: [UsbPipeInfo; 3]) -> Self {
            Self {
                pipes,
                log: [None; 32],
                len: 0,
                fail_select: false,
                reader_status: STATUS_SUCCESS,
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

    impl ControlTransfer for PipeHw {
        fn serialized<R, F: FnOnce(&mut ModeSwitch<'_>) -> R>(&mut self, f: F) -> R {
            f(&mut |_, _| STATUS_SUCCESS)
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
        fn configured_pipe(&mut self, index: u8) -> UsbPipeInfo {
            self.push("pipe_type", Some(index));
            self.pipes[usize::from(index)]
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
            self.reader_status
        }
        fn start_pipe(&mut self, _: u8) -> NtStatus {
            STATUS_SUCCESS
        }
        fn stop_pipe(&mut self, _: u8) {}
        fn query_performance_frequency(&mut self) -> i64 {
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

    /// Interrupt OUT first: the reader goes on the interrupt IN pipe after it.
    #[test]
    fn interrupt_out_pipe_is_skipped() {
        let state = DeviceState::new();
        let mut hw = PipeHw::with_pipes([
            pipe(UsbPipeType::Interrupt, 0x03),
            pipe(UsbPipeType::Interrupt, 0x83),
            pipe(UsbPipeType::Bulk, 0x02),
        ]);
        assert_eq!(prepare_hardware(&state, &mut hw), STATUS_SUCCESS);
        assert_eq!(state.interrupt_pipe(), Some(1));
        assert_eq!(hw.log().last(), Some(&Some(("reader", Some(1)))));
    }

    /// Only an interrupt OUT pipe: no reader can be configured, so `PrepareHardware` fails.
    #[test]
    fn interrupt_out_only_fails_prepare_hardware() {
        let state = DeviceState::new();
        let mut hw = PipeHw::with_pipes([
            pipe(UsbPipeType::Interrupt, 0x03),
            pipe(UsbPipeType::Bulk, 0x81),
            pipe(UsbPipeType::Control, 0x00),
        ]);
        assert_eq!(
            prepare_hardware(&state, &mut hw),
            STATUS_INVALID_DEVICE_STATE
        );
        assert_eq!(state.interrupt_pipe(), None);
        assert!(!hw.log().contains(&Some(("reader", Some(0)))));
    }

    /// The reader configuration fails after the IN pipe was selected: `PrepareHardware` returns that
    /// status and does not turn the input gates on.
    #[test]
    fn reader_configuration_failure_fails_prepare_hardware() {
        const STATUS_INSUFFICIENT_RESOURCES: NtStatus = 0xC000_009A_u32 as i32;
        let state = DeviceState::new();
        let mut hw = PipeHw::with_pipes([
            pipe(UsbPipeType::Interrupt, 0x03),
            pipe(UsbPipeType::Interrupt, 0x83),
            pipe(UsbPipeType::Bulk, 0x02),
        ]);
        hw.reader_status = STATUS_INSUFFICIENT_RESOURCES;
        let gates = state.input_gates();
        assert_eq!(
            prepare_hardware(&state, &mut hw),
            STATUS_INSUFFICIENT_RESOURCES
        );
        assert_eq!(state.interrupt_pipe(), Some(1));
        assert_eq!(state.input_gates(), gates);
    }

    /// A restart selects the configuration again (new pipe objects), so the reader is configured again
    /// and its status checked again.
    #[test]
    fn prepare_hardware_twice_configures_the_reader_twice() {
        let state = DeviceState::new();
        let mut hw = PipeHw::new();
        assert_eq!(prepare_hardware(&state, &mut hw), STATUS_SUCCESS);
        assert_eq!(prepare_hardware(&state, &mut hw), STATUS_SUCCESS);
        let readers = hw
            .log()
            .iter()
            .filter(|entry| matches!(entry, Some(("reader", Some(1)))))
            .count();
        assert_eq!(readers, 2);
    }

    #[test]
    fn constants_and_reader_failure_budget() {
        let state = DeviceState::new();
        assert_eq!(io_stop(1), IoStopAction::None);
        let cfg = continuous_reader_config(&state).expect("reader config");
        assert_eq!((cfg.transfer_length, cfg.num_pending_reads), (526, 1));

        assert_eq!(
            on_readers_failed(&state, STATUS_CANCELLED, 0),
            ReaderFailureAction::Stop
        );
        assert_eq!(
            on_readers_failed(&state, STATUS_DEVICE_NOT_CONNECTED, 0),
            ReaderFailureAction::Stop
        );
        for _ in 0..MAX_READER_RESTARTS {
            assert_eq!(
                on_readers_failed(&state, -1, 0),
                ReaderFailureAction::Restart
            );
        }
        assert_eq!(
            on_readers_failed(&state, -1, 0),
            ReaderFailureAction::FailDevice
        );
        state.reset_reader_failures();
        assert_eq!(
            on_readers_failed(&state, -1, 0),
            ReaderFailureAction::Restart
        );
    }
}

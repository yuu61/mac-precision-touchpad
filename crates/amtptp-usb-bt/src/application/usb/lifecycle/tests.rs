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
    fn configure_continuous_reader(&mut self, index: u8, _: &ContinuousReaderConfig) -> NtStatus {
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

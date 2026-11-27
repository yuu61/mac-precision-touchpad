//! PnP/power callbacks: `AmtPtpEvtDevicePrepareHardware` (with `SelectInterruptInterface` and the reader
//! setup), `AmtPtpEvtDeviceD0Entry`, `AmtPtpEvtDeviceD0Exit` (Device.c:120-253, :498-727;
//! InputInterrupt.c:6-80, :213-226) and the device-add constants.

use crate::config::lookup_config_row;
use crate::hid::TraceEvent;
use crate::io::{ControlPipe, InterruptPipeTarget, UsbTargetSetup};
use crate::state::DeviceState;
use crate::status::{
    NtStatus, STATUS_INVALID_DEVICE_STATE, STATUS_SUCCESS, STATUS_UNSUCCESSFUL, nt_success,
};
use crate::usb::set_wellspring_mode;

/// `GUID_DEVINTERFACE_AmtPtpDevice` `{4a5064e5-7d39-41d1-a0e4-81097edce967}` (Trace.h:7-8), registered with
/// `WdfDeviceCreateDeviceInterface(device, &guid, NULL)` in `EvtDeviceAdd`: `(Data1, Data2, Data3, Data4)`.
pub const GUID_DEVINTERFACE_AMTPTP_DEVICE: (u32, u16, u16, [u8; 8]) = (
    0x4a50_64e5,
    0x7d39,
    0x41d1,
    [0xa0, 0xe4, 0x81, 0x09, 0x7e, 0xdc, 0xe9, 0x67],
);

/// Return value of `AmtPtpEvtUsbInterruptReadersFailed`: always `TRUE` (the framework resets the pipe and
/// restarts the reader). The callback does nothing else.
pub const READERS_FAILED_RESULT: bool = true;

/// `NumPendingReads` of the continuous reader: 0 = framework default.
pub const CONTINUOUS_READER_NUM_PENDING_READS: u32 = 0;

/// Outcome of [`prepare_hardware`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrepareHardwareOutcome {
    /// The callback's return value.
    pub status: NtStatus,
    /// `TraceLogging` event to write ([`TraceEvent::DeviceNotFound`] for an unknown PID).
    pub event: Option<TraceEvent>,
    /// Whether this call configured the continuous reader (false on a restart, UM-B16).
    pub configured_reader: bool,
}

impl PrepareHardwareOutcome {
    const fn status(status: NtStatus) -> Self {
        Self {
            status,
            event: None,
            configured_reader: false,
        }
    }
}

/// `AmtPtpEvtDevicePrepareHardware`.
///
/// 1. Create the USB target device if not yet created (failure: that status).
/// 2. Cache the device descriptor; look up the config row by `idProduct` (UM-B3 bounded; unknown →
///    `STATUS_INVALID_DEVICE_STATE` + [`TraceEvent::DeviceNotFound`], `DeviceInfo = NULL`).
/// 3. Retrieve the USB traits (failure → traits 0; write-only state).
/// 4. `SelectInterruptInterface`: interface 0 (`NULL` → `STATUS_UNSUCCESSFUL`); visit configured pipes in
///    order (each gets `SetNoMaximumPacketSizeCheck`) until the first **interrupt IN** pipe (P3 fix of
///    UM-B21: C took the first interrupt pipe whatever its direction); none → `STATUS_INVALID_DEVICE_STATE`
///    (C also accepted the handle a previous start had selected).
/// 5. Configure the continuous reader with the row's transfer length — **only once** (UM-B16, P2 FIXED:
///    the C code reconfigures it on every `PrepareHardware`, which WDF rejects with
///    `STATUS_INVALID_DEVICE_STATE`, so the device never restarts after a PnP stop/start). Failure: that
///    status.
/// 6. `IsButtonReportOn = IsSurfaceReportOn = TRUE`; return success.
pub fn prepare_hardware<T: UsbTargetSetup>(
    state: &DeviceState,
    usb: &mut T,
) -> PrepareHardwareOutcome {
    if let Err(status) = usb.create_usb_device_if_needed() {
        return PrepareHardwareOutcome::status(status);
    }
    let descriptor = usb.device_descriptor();
    state.set_device_descriptor(&descriptor);
    let row = lookup_config_row(descriptor.id_product);
    state.set_config_row(row);
    let Some(cfg) = state.config() else {
        return PrepareHardwareOutcome {
            status: STATUS_INVALID_DEVICE_STATE,
            event: Some(TraceEvent::DeviceNotFound {
                id_product: descriptor.id_product,
                id_vendor: descriptor.id_vendor,
            }),
            configured_reader: false,
        };
    };
    state.set_usb_device_traits(usb.retrieve_device_traits().unwrap_or(0));

    let status = select_interrupt_interface(usb);
    if !nt_success(status) {
        return PrepareHardwareOutcome::status(status);
    }

    let configured_reader = if state.reader_configured() {
        false
    } else {
        let status = usb.configure_continuous_reader(cfg.profile.tp_type.reader_transfer_length());
        if !nt_success(status) {
            return PrepareHardwareOutcome::status(status);
        }
        state.set_reader_configured(true);
        true
    };

    state.set_button_report_on(true);
    state.set_surface_report_on(true);
    PrepareHardwareOutcome {
        status: STATUS_SUCCESS,
        event: None,
        configured_reader,
    }
}

/// `SelectInterruptInterface` (Device.c:644-727). `WdfUsbTargetDeviceSelectConfig` is never called (the
/// framework already selected the first configuration).
fn select_interrupt_interface<T: UsbTargetSetup>(usb: &mut T) -> NtStatus {
    let Some(pipe_count) = usb.interface0_configured_pipes() else {
        return STATUS_UNSUCCESSFUL;
    };
    for index in 0..pipe_count {
        if usb.visit_configured_pipe(index).is_interrupt_in() {
            usb.select_interrupt_pipe(index);
            return STATUS_SUCCESS;
        }
    }
    STATUS_INVALID_DEVICE_STATE
}

/// Outcome of [`d0_entry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct D0EntryOutcome {
    /// The callback's return value (the `WdfIoTargetStart` status).
    pub status: NtStatus,
    /// Status of `SetWellspringMode(TRUE)` if it ran (a failure is only traced).
    pub mode_switch_status: Option<NtStatus>,
}

/// `AmtPtpEvtDeviceD0Entry`.
///
/// 1. If `IsButtonReportOn || IsWellspringModeOn`: `SetWellspringMode(TRUE)`; a failure is ignored
///    (UM-B10, P3 PRESERVED gate).
/// 2. `Cache the QPC frequency and restart the reporting clock`.
/// 3. `WdfIoTargetStart` on the interrupt pipe; its status is the return value.
pub fn d0_entry<T, F>(
    state: &DeviceState,
    io: &mut T,
    query_performance_frequency: F,
) -> D0EntryOutcome
where
    T: ControlPipe + InterruptPipeTarget,
    F: FnOnce() -> i64,
{
    let mode_switch_status =
        (state.is_button_report_on() || state.is_wellspring_mode_on()).then(|| {
            match state.config() {
                Some(cfg) => set_wellspring_mode(state, cfg, io, true),
                // P2: C would dereference DeviceInfo == NULL; D0Entry never runs without a config row.
                None => STATUS_INVALID_DEVICE_STATE,
            }
        });
    state.set_qpc_frequency(query_performance_frequency());
    let status = io.start();
    D0EntryOutcome {
        status,
        mode_switch_status,
    }
}

/// `AmtPtpEvtDeviceD0Exit`: `WdfIoTargetStop(.., WdfIoTargetCancelSentIo)`, then
/// `SetWellspringMode(FALSE)`, whose status is **returned** (UM-B9, P3 PRESERVED: a failed transfer, e.g.
/// on surprise removal, fails `D0Exit`).
pub fn d0_exit<T>(state: &DeviceState, io: &mut T) -> NtStatus
where
    T: ControlPipe + InterruptPipeTarget,
{
    io.stop_cancel_sent_io();
    match state.config() {
        Some(cfg) => set_wellspring_mode(state, cfg, io, false),
        // P2: see d0_entry.
        None => STATUS_INVALID_DEVICE_STATE,
    }
}

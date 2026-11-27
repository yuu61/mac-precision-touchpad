//! Wellspring-mode control transfers (`AmtPtpGetWellspringMode`, `AmtPtpSetWellspringMode`,
//! `AmtPtpEmergResetDevice`, Device.c:255-496 and :758-802).
//!
//! UM-B1: the C code describes the transfer buffer with `sizeof(DeviceInfo->um_size)` = `sizeof(int)` =
//! **4** bytes, so `wLength` on the wire is 4 for every family (P1 PRESERVED). For TYPE4/5 the C buffer is
//! only `um_size` = 2 bytes, which the 4-byte transfer overruns (P2 FIXED): this module always uses one
//! zero-initialized 4-byte buffer for the GET and the following SET, so bytes 2..3 of a TYPE4/5 SET are
//! the device's GET bytes, or 0 where the device returned fewer than 4 bytes. TYPE1-3 allocate 8 bytes in
//! C, of which only the first 4 are ever on the wire, so the 4-byte buffer is wire-identical.
//!
//! UM-B5 (P2 FIXED): no memory object is allocated, so there is no `WdfObjectDelete(NULL)` path.
//!
//! TYPE3 (WS8) does no I/O: GET always reports "on", SET only stores the flag (UM-B4, P3 PRESERVED).

use bcm5974::usb::UsbMsg;

use crate::config::{TrackpadType, WellspringDevice};
use crate::io::ControlPipe;
use crate::state::DeviceState;
use crate::status::{NtStatus, STATUS_SUCCESS, nt_success};

pub use bcm5974::usb::{MODE_SWITCH_TRANSFER_LEN, UsbSetupPacket as SetupPacket};

/// The `GET_REPORT` setup packet of a row: `A1 01 <um_req_val> <um_req_idx> 04 00`.
#[must_use]
pub const fn wellspring_mode_read_setup(cfg: &WellspringDevice) -> SetupPacket {
    UsbMsg::for_type(cfg.profile.tp_type).read_setup()
}

/// The `SET_REPORT` setup packet of a row: `21 09 <um_req_val> <um_req_idx> 04 00`.
#[must_use]
pub const fn wellspring_mode_write_setup(cfg: &WellspringDevice) -> SetupPacket {
    UsbMsg::for_type(cfg.profile.tp_type).write_setup()
}

/// Sends the `GET_REPORT` into `buffer` and returns its status (Device.c:300-341 / :403-444).
fn read_mode_register<C: ControlPipe>(
    cfg: &WellspringDevice,
    control: &mut C,
    buffer: &mut [u8; MODE_SWITCH_TRANSFER_LEN],
) -> NtStatus {
    control.send_control_transfer(&wellspring_mode_read_setup(cfg), buffer)
}

/// `AmtPtpGetWellspringMode`: whether the device reports raw multitouch mode.
///
/// TYPE3 returns `Ok(true)` without I/O. Otherwise one `GET_REPORT` into a zeroed 4-byte buffer; on
/// success the result is `buffer[um_switch_idx] == um_switch_on`. A failed transfer returns its status.
/// `cbTransferred` is not checked (as in C).
///
/// # Errors
/// Returns the failure status of the USB control transfer.
pub fn get_wellspring_mode<C: ControlPipe>(
    cfg: &WellspringDevice,
    control: &mut C,
) -> Result<bool, NtStatus> {
    if cfg.profile.tp_type == TrackpadType::Type3 {
        return Ok(true);
    }
    let mut buffer = [0_u8; MODE_SWITCH_TRANSFER_LEN];
    let status = read_mode_register(cfg, control, &mut buffer);
    if !nt_success(status) {
        return Err(status);
    }
    // C: `unsigned char bit = buffer[idx]; bit == um_switch_on` (u8 promoted to int).
    Ok(UsbMsg::for_type(cfg.profile.tp_type).is_enabled(&buffer))
}

/// `AmtPtpSetWellspringMode`: read-modify-write of the device's mode register.
///
/// TYPE3: stores `enable` in `IsWellspringModeOn` and returns success (no I/O). Otherwise: `GET_REPORT` into
/// a zeroed 4-byte buffer (failure: return its status, flag unchanged); set `buffer[um_switch_idx]` to
/// `(u8) um_switch_on` or `(u8) um_switch_off`; `SET_REPORT` of the same 4 bytes (failure: return its status,
/// flag unchanged); then `IsWellspringModeOn = enable`.
pub fn set_wellspring_mode<C: ControlPipe>(
    state: &DeviceState,
    cfg: &WellspringDevice,
    control: &mut C,
    enable: bool,
) -> NtStatus {
    if cfg.profile.tp_type == TrackpadType::Type3 {
        state.set_wellspring_mode_on(enable);
        return STATUS_SUCCESS;
    }
    let mut buffer = [0_u8; MODE_SWITCH_TRANSFER_LEN];
    let status = read_mode_register(cfg, control, &mut buffer);
    if !nt_success(status) {
        return status;
    }
    UsbMsg::for_type(cfg.profile.tp_type).apply_mode(&mut buffer, enable);
    let status = control.send_control_transfer(&wellspring_mode_write_setup(cfg), &mut buffer);
    if !nt_success(status) {
        return status;
    }
    state.set_wellspring_mode_on(enable);
    status
}

/// Outcome of [`emergency_reset`]: the statuses of its two mode switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmergencyResetOutcome {
    /// Status of `SetWellspringMode(FALSE)` (only traced in C).
    pub disable_status: NtStatus,
    /// Status of `SetWellspringMode(TRUE)`: the function's return value.
    pub status: NtStatus,
}

/// `AmtPtpEmergResetDevice`: `SetWellspringMode(FALSE)` then `SetWellspringMode(TRUE)`; returns the
/// second status. Runs even when the host selected mouse mode (UM-B11, P3 PRESERVED).
pub fn emergency_reset<C: ControlPipe>(
    state: &DeviceState,
    cfg: &WellspringDevice,
    control: &mut C,
) -> EmergencyResetOutcome {
    let disable_status = set_wellspring_mode(state, cfg, control, false);
    let status = set_wellspring_mode(state, cfg, control, true);
    EmergencyResetOutcome {
        disable_status,
        status,
    }
}

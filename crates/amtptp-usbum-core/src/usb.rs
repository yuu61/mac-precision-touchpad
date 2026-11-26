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

use crate::config::{Bcm5974Config, TrackpadType};
use crate::io::ControlPipe;
use crate::state::DeviceState;
use crate::status::{NtStatus, STATUS_SUCCESS, nt_success};

/// `BCM5974_WELLSPRING_MODE_READ_REQUEST_ID`: HID class `GET_REPORT`.
pub const BCM5974_WELLSPRING_MODE_READ_REQUEST_ID: u8 = 1;
/// `BCM5974_WELLSPRING_MODE_WRITE_REQUEST_ID`: HID class `SET_REPORT`.
pub const BCM5974_WELLSPRING_MODE_WRITE_REQUEST_ID: u8 = 9;

/// Length of the mode-switch data stage on the wire: `sizeof(int)` (UM-B1).
pub const MODE_SWITCH_TRANSFER_LEN: usize = 4;

/// `bmRequestType` direction bit: device to host (`BmRequestDeviceToHost`).
pub const BM_REQUEST_DEVICE_TO_HOST: u8 = 0x80;
/// `bmRequestType` type field: class (`BmRequestClass` << 5).
pub const BM_REQUEST_CLASS: u8 = 0x20;
/// `bmRequestType` recipient field: interface (`BmRequestToInterface`).
pub const BM_REQUEST_TO_INTERFACE: u8 = 0x01;

/// An 8-byte USB setup packet.
///
/// It is what `WDF_USB_CONTROL_SETUP_PACKET_INIT` + `Packet.bm.Request.Type = BmRequestClass` produce,
/// with `wLength` filled in the way the framework does from the 4-byte memory descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupPacket {
    /// `bmRequestType`.
    pub bm_request_type: u8,
    /// `bRequest`.
    pub b_request: u8,
    /// `wValue`.
    pub w_value: u16,
    /// `wIndex`.
    pub w_index: u16,
    /// `wLength`.
    pub w_length: u16,
}

impl SetupPacket {
    /// The 8 bytes on the wire (little-endian fields).
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 8] {
        let [v0, v1] = self.w_value.to_le_bytes();
        let [i0, i1] = self.w_index.to_le_bytes();
        let [l0, l1] = self.w_length.to_le_bytes();
        [self.bm_request_type, self.b_request, v0, v1, i0, i1, l0, l1]
    }

    /// Whether this is a device-to-host (IN) transfer.
    #[must_use]
    pub const fn is_device_to_host(self) -> bool {
        self.bm_request_type & BM_REQUEST_DEVICE_TO_HOST != 0
    }
}

const fn mode_switch_setup(
    cfg: &Bcm5974Config,
    device_to_host: bool,
    b_request: u8,
) -> SetupPacket {
    SetupPacket {
        bm_request_type: if device_to_host {
            BM_REQUEST_DEVICE_TO_HOST
        } else {
            0
        } | BM_REQUEST_CLASS
            | BM_REQUEST_TO_INTERFACE,
        b_request,
        // C: `(USHORT) DeviceInfo->um_req_val` / `(USHORT) DeviceInfo->um_req_idx` (truncation).
        w_value: cfg.um.um_req_val as u16,
        w_index: cfg.um.um_req_idx as u16,
        // UM-B1: the memory descriptor is `sizeof(int)` bytes long.
        w_length: MODE_SWITCH_TRANSFER_LEN as u16,
    }
}

/// The `GET_REPORT` setup packet of a row: `A1 01 <um_req_val> <um_req_idx> 04 00`.
#[must_use]
pub const fn wellspring_mode_read_setup(cfg: &Bcm5974Config) -> SetupPacket {
    mode_switch_setup(cfg, true, BCM5974_WELLSPRING_MODE_READ_REQUEST_ID)
}

/// The `SET_REPORT` setup packet of a row: `21 09 <um_req_val> <um_req_idx> 04 00`.
#[must_use]
pub const fn wellspring_mode_write_setup(cfg: &Bcm5974Config) -> SetupPacket {
    mode_switch_setup(cfg, false, BCM5974_WELLSPRING_MODE_WRITE_REQUEST_ID)
}

/// `buffer[um_switch_idx]` as an index into the 4-byte transfer buffer.
fn switch_index(cfg: &Bcm5974Config) -> usize {
    // um_switch_idx is 0 or 1 in every row; a negative value would be a table bug and maps out of range
    // (the accessors below then treat the byte as absent).
    usize::try_from(cfg.um.um_switch_idx).unwrap_or(usize::MAX)
}

/// Sends the `GET_REPORT` into `buffer` and returns its status (Device.c:300-341 / :403-444).
fn read_mode_register<C: ControlPipe>(
    cfg: &Bcm5974Config,
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
    cfg: &Bcm5974Config,
    control: &mut C,
) -> Result<bool, NtStatus> {
    if cfg.tp_type == TrackpadType::Type3 {
        return Ok(true);
    }
    let mut buffer = [0_u8; MODE_SWITCH_TRANSFER_LEN];
    let status = read_mode_register(cfg, control, &mut buffer);
    if !nt_success(status) {
        return Err(status);
    }
    // C: `unsigned char bit = buffer[idx]; bit == um_switch_on` (u8 promoted to int).
    let bit = buffer.get(switch_index(cfg)).copied().unwrap_or(0);
    Ok(i32::from(bit) == cfg.um.um_switch_on)
}

/// `AmtPtpSetWellspringMode`: read-modify-write of the device's mode register.
///
/// TYPE3: stores `enable` in `IsWellspringModeOn` and returns success (no I/O). Otherwise: `GET_REPORT` into
/// a zeroed 4-byte buffer (failure: return its status, flag unchanged); set `buffer[um_switch_idx]` to
/// `(u8) um_switch_on` or `(u8) um_switch_off`; `SET_REPORT` of the same 4 bytes (failure: return its status,
/// flag unchanged); then `IsWellspringModeOn = enable`.
pub fn set_wellspring_mode<C: ControlPipe>(
    state: &DeviceState,
    cfg: &Bcm5974Config,
    control: &mut C,
    enable: bool,
) -> NtStatus {
    if cfg.tp_type == TrackpadType::Type3 {
        state.set_wellspring_mode_on(enable);
        return STATUS_SUCCESS;
    }
    let mut buffer = [0_u8; MODE_SWITCH_TRANSFER_LEN];
    let status = read_mode_register(cfg, control, &mut buffer);
    if !nt_success(status) {
        return status;
    }
    // C: `(unsigned char) um_switch_on` / `(unsigned char) um_switch_off` (truncation).
    let value = if enable {
        cfg.um.um_switch_on as u8
    } else {
        cfg.um.um_switch_off as u8
    };
    if let Some(byte) = buffer.get_mut(switch_index(cfg)) {
        *byte = value;
    }
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
    cfg: &Bcm5974Config,
    control: &mut C,
) -> EmergencyResetOutcome {
    let disable_status = set_wellspring_mode(state, cfg, control, false);
    let status = set_wellspring_mode(state, cfg, control, true);
    EmergencyResetOutcome {
        disable_status,
        status,
    }
}

//! The wellspring (raw multitouch) mode switch: `AmtPtpSetWellspringMode` (Device.c:503-639).
//!
//! Wire traffic, identical to the C driver (spec §6.3, fixtures `feature.txt` / `wire.txt`):
//!
//! 1. HID `GET_REPORT`, feature report 2, interface 2: setup `a1 01 02 03 02 00 04 00`, IN data stage of
//!    up to 4 bytes.
//! 2. byte `um_switch_idx` (1) of the buffer := `um_switch_on` (1) or `um_switch_off` (0).
//! 3. HID `SET_REPORT`: setup `21 09 02 03 02 00 04 00`, OUT data stage of exactly 4 bytes
//!    `[buf[0], mode, buf[2], buf[3]]`.
//! 4. On success of both transfers, `IsWellspringModeOn := on`.
//!
//! `wLength` is 4 because the C memory descriptor uses `sizeof(DeviceInfo->um_size)` (`sizeof(int)`)
//! instead of `um_size` (2). The wire bytes are preserved (P1); the C pool overrun/over-read that came
//! with it (a 2-byte allocation behind a 4-byte descriptor, spec B3) is fixed by allocating the full
//! [`WELLSPRING_DATA_LEN`] bytes, zeroed, from non-paged memory (P2, B3/B27). The `WdfObjectDelete(NULL)`
//! after a failed allocation (B4) is gone: a failed [`ControlTransfer::allocate_transfer_buffer`] just
//! returns its status. The number of bytes the device returns is not checked (B5, preserved).

use bcm5974::usb::UsbMsg;

use crate::config::{T2Device, TrackpadType};
use crate::state::DeviceState;
use crate::status::{NtStatus, STATUS_SUCCESS, nt_success};

pub use bcm5974::usb::{MODE_SWITCH_TRANSFER_LEN as WELLSPRING_DATA_LEN, UsbSetupPacket};

/// `POOL_TAG_PTP_CONTROL` = `'PTPC'` (Device.h:59), an MSVC multichar literal (0x50545043, shown as
/// "CPTP" in pool dumps). Tag of the transfer buffer allocation.
pub const POOL_TAG_PTP_CONTROL: u32 = u32::from_be_bytes(*b"PTPC");

/// `NonPagedPoolNx` (`POOL_TYPE` 512): the pool the Rust driver allocates the transfer buffer from. The
/// C driver used `PagedPool` (1) for a URB transfer buffer (spec B27, fixed).
pub const TRANSFER_BUFFER_POOL_TYPE: i32 = 512;

/// The `GET_REPORT` setup packet for the given device.
#[must_use]
pub const fn wellspring_read_setup(cfg: &T2Device) -> UsbSetupPacket {
    UsbMsg::for_type(cfg.profile.tp_type).read_setup()
}

/// The `SET_REPORT` setup packet for the given device.
#[must_use]
pub const fn wellspring_write_setup(cfg: &T2Device) -> UsbSetupPacket {
    UsbMsg::for_type(cfg.profile.tp_type).write_setup()
}

/// Updates the mode byte in the data returned by `GET_REPORT`.
pub fn apply_mode_byte(cfg: &T2Device, buf: &mut [u8; WELLSPRING_DATA_LEN], on: bool) {
    UsbMsg::for_type(cfg.profile.tp_type).apply_mode(buf, on);
}

/// Synchronous USB control transfers to the device (implemented by the glue over `WDFUSBDEVICE`).
///
/// `PASSIVE_LEVEL` only, like the C `AmtPtpSetWellspringMode`.
pub trait ControlTransfer {
    /// The transfer buffer. The glue's type wraps a `WDFMEMORY` and deletes it in `Drop`
    /// (`WdfObjectDelete`), which runs exactly once on every path of [`set_wellspring_mode`].
    type Buffer: AsMut<[u8; WELLSPRING_DATA_LEN]>;

    /// `WdfMemoryCreate(WDF_NO_OBJECT_ATTRIBUTES, NonPagedPoolNx, POOL_TAG_PTP_CONTROL,
    /// WELLSPRING_DATA_LEN, ..)`. The content does not need to be initialized: the caller zeroes it.
    ///
    /// # Errors
    /// Returns the WDF failure status if the transfer buffer cannot be allocated.
    fn allocate_transfer_buffer(&mut self) -> Result<Self::Buffer, NtStatus>;

    /// `WdfUsbTargetDeviceSendControlTransferSynchronously(UsbDevice, WDF_NO_HANDLE, NULL, setup,
    /// <buffer descriptor over all of data>, &cb)`: no request options, so no timeout.
    ///
    /// For a device-to-host `setup`, the device writes up to [`WELLSPRING_DATA_LEN`] bytes into `data`;
    /// the byte count is ignored (spec B5). Returns the transfer status.
    fn control_transfer(
        &mut self,
        setup: &UsbSetupPacket,
        data: &mut [u8; WELLSPRING_DATA_LEN],
    ) -> NtStatus;
}

/// `AmtPtpSetWellspringMode(ctx, on)`: switches the trackpad into (`on`) or out of wellspring mode.
///
/// Returns the first failing status (allocation, `GET_REPORT` or `SET_REPORT`), leaving
/// `IsWellspringModeOn` unchanged; on success sets it to `on` and returns `STATUS_SUCCESS`.
///
/// Callers (C): `D0Entry` with `on = true` (result ignored), `D0Exit` with `on = false` (result returned),
/// `SET_FEATURE` input mode 3 with `on = true` when the mode is off (result returned).
pub fn set_wellspring_mode<T: ControlTransfer + ?Sized>(
    state: &DeviceState,
    io: &mut T,
    on: bool,
) -> NtStatus {
    let cfg = state.config();

    // Device.c:525-528. No table row is TYPE3, so this never runs; kept to mirror the C.
    if matches!(cfg.profile.tp_type, TrackpadType::Type3) {
        state.set_wellspring_mode_on(on);
        return STATUS_SUCCESS;
    }

    let mut buffer = match io.allocate_transfer_buffer() {
        Ok(buffer) => buffer,
        // B4 fixed: nothing was allocated, so there is nothing to delete.
        Err(status) => return status,
    };
    let data = buffer.as_mut();
    // RtlZeroMemory. B3 fixed: all 4 bytes the descriptor covers are ours and zeroed.
    *data = [0; WELLSPRING_DATA_LEN];

    let status = io.control_transfer(&wellspring_read_setup(cfg), data);
    if !nt_success(status) {
        return status;
    }

    apply_mode_byte(cfg, data, on);

    let status = io.control_transfer(&wellspring_write_setup(cfg), data);
    if !nt_success(status) {
        return status;
    }

    state.set_wellspring_mode_on(on);
    drop(buffer); // cleanup: WdfObjectDelete(bufHandle)
    STATUS_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FALLBACK_CONFIG;

    #[test]
    fn setup_packets_match_spec() {
        assert_eq!(
            wellspring_read_setup(&FALLBACK_CONFIG).to_bytes(),
            [0xa1, 0x01, 0x02, 0x03, 0x02, 0x00, 0x04, 0x00]
        );
        assert_eq!(
            wellspring_write_setup(&FALLBACK_CONFIG).to_bytes(),
            [0x21, 0x09, 0x02, 0x03, 0x02, 0x00, 0x04, 0x00]
        );
        assert!(wellspring_read_setup(&FALLBACK_CONFIG).is_device_to_host());
        assert!(!wellspring_write_setup(&FALLBACK_CONFIG).is_device_to_host());
        assert_eq!(POOL_TAG_PTP_CONTROL, 0x5054_5043);
    }

    #[test]
    fn mode_byte() {
        let mut buf = [0x02, 0x00, 0x33, 0x44];
        apply_mode_byte(&FALLBACK_CONFIG, &mut buf, true);
        assert_eq!(buf, [0x02, 0x01, 0x33, 0x44]);
        apply_mode_byte(&FALLBACK_CONFIG, &mut buf, false);
        assert_eq!(buf, [0x02, 0x00, 0x33, 0x44]);
    }
}

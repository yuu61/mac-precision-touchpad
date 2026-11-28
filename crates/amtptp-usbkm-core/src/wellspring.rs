//! USB multitouch mode switching. Each device uses its own HID feature register.

pub use bcm5974::usb::UsbMsg;

use crate::config::{TrackpadType, UsbModel};
use crate::state::DeviceState;
use crate::status::{NtStatus, STATUS_SUCCESS, nt_success};

pub use bcm5974::usb::{MODE_SWITCH_TRANSFER_LEN as WELLSPRING_DATA_LEN, UsbSetupPacket};

/// The `GET_REPORT` setup packet for the given device.
#[must_use]
pub const fn wellspring_read_setup(cfg: &UsbModel) -> UsbSetupPacket {
    UsbMsg::for_type(cfg.profile.tp_type).read_setup()
}

/// The `SET_REPORT` setup packet for the given device.
#[must_use]
pub const fn wellspring_write_setup(cfg: &UsbModel) -> UsbSetupPacket {
    UsbMsg::for_type(cfg.profile.tp_type).write_setup()
}

/// Updates the mode byte in the data returned by `GET_REPORT`.
pub fn apply_mode_byte(cfg: &UsbModel, buf: &mut [u8; WELLSPRING_DATA_LEN], on: bool) {
    UsbMsg::for_type(cfg.profile.tp_type).apply_mode(buf, on);
}

/// A register update using the device's exclusively borrowed control buffer.
pub type ModeSwitch<'a> = dyn FnMut(UsbMsg, bool) -> NtStatus + 'a;

/// Runs the state check and any associated register updates under the device's mode lock.
pub trait ControlTransfer {
    /// Supplies a synchronous register operation at `PASSIVE_LEVEL` for the duration of the callback.
    fn serialized<R, F: FnOnce(&mut ModeSwitch<'_>) -> R>(&mut self, f: F) -> R;
}

/// Updates the software mode after a successful hardware transition. Type 3 is always in raw mode.
pub fn set_wellspring_mode(state: &DeviceState, switch: &mut ModeSwitch<'_>, on: bool) -> NtStatus {
    let format = state.config().profile.tp_type;
    if format != TrackpadType::Type3 {
        let status = switch(UsbMsg::for_type(format), on);
        if !nt_success(status) {
            return status;
        }
    }
    state.set_wellspring_mode_on(on);
    STATUS_SUCCESS
}

/// Restore raw packets after a malformed legacy frame. Recheck the requested mode under
/// the lock so a queued recovery cannot undo a mouse-mode request or power down.
pub fn recover_mode<T: ControlTransfer>(state: &DeviceState, io: &mut T) {
    io.serialized(|switch| {
        if state.can_recover_mode() {
            let _off = set_wellspring_mode(state, switch, false);
            let _on = set_wellspring_mode(state, switch, true);
        }
    });
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

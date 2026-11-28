//! USB class control transfers for switching Wellspring raw input mode.

use amtptp_core::hid::status::{NtStatus, nt_success};

use crate::bcm5974::config::TrackpadType;

/// Length of the USB data stage used by the drivers.
pub const MODE_SWITCH_TRANSFER_LEN: usize = 4;
/// HID class `GET_REPORT` request.
pub const GET_REPORT: u8 = 1;
/// HID class `SET_REPORT` request.
pub const SET_REPORT: u8 = 9;
/// Device-to-host HID class request for an interface.
pub const BM_REQUEST_D2H_CLASS_INTERFACE: u8 = 0xA1;
/// Host-to-device HID class request for an interface.
pub const BM_REQUEST_H2D_CLASS_INTERFACE: u8 = 0x21;

/// An 8-byte USB setup packet (`WDF_USB_CONTROL_SETUP_PACKET.Generic.Bytes`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbSetupPacket {
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

impl UsbSetupPacket {
    /// The 8 wire bytes (little-endian words).
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 8] {
        let [v0, v1] = self.w_value.to_le_bytes();
        let [i0, i1] = self.w_index.to_le_bytes();
        let [l0, l1] = self.w_length.to_le_bytes();
        [self.bm_request_type, self.b_request, v0, v1, i0, i1, l0, l1]
    }

    /// Whether the data stage is device-to-host (bit 7 of `bmRequestType`).
    #[must_use]
    pub const fn is_device_to_host(self) -> bool {
        self.bm_request_type & 0x80 != 0
    }
}

/// Control request and mode-byte parameters for a Wellspring packet format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbMsg {
    value: u16,
    index: u16,
    mode_offset: usize,
    mode_on: u8,
    mode_off: u8,
}

impl UsbMsg {
    /// Class control request parameters for this packet format.
    #[must_use]
    pub const fn for_type(format: TrackpadType) -> Self {
        match format {
            TrackpadType::Type1 | TrackpadType::Type2 | TrackpadType::Type3 => Self {
                value: 0x300,
                index: 0,
                mode_offset: 0,
                mode_on: 0x1,
                mode_off: 0x8,
            },
            TrackpadType::Type4 => Self {
                value: 0x302,
                index: 2,
                mode_offset: 1,
                mode_on: 0x1,
                mode_off: 0x0,
            },
            TrackpadType::Type5 => Self {
                value: 0x302,
                index: 1,
                mode_offset: 1,
                mode_on: 0x1,
                mode_off: 0x0,
            },
        }
    }

    /// Builds the `GET_REPORT` setup packet for this format.
    #[must_use]
    pub const fn read_setup(self) -> UsbSetupPacket {
        self.setup(BM_REQUEST_D2H_CLASS_INTERFACE, GET_REPORT)
    }

    /// Builds the `SET_REPORT` setup packet for this format.
    #[must_use]
    pub const fn write_setup(self) -> UsbSetupPacket {
        self.setup(BM_REQUEST_H2D_CLASS_INTERFACE, SET_REPORT)
    }

    const fn setup(self, bm_request_type: u8, b_request: u8) -> UsbSetupPacket {
        UsbSetupPacket {
            bm_request_type,
            b_request,
            w_value: self.value,
            w_index: self.index,
            w_length: MODE_SWITCH_TRANSFER_LEN as u16,
        }
    }

    /// Whether the device returned the mode byte for raw multitouch input.
    #[must_use]
    pub fn is_enabled(self, data: &[u8; MODE_SWITCH_TRANSFER_LEN]) -> bool {
        data.get(self.mode_offset) == Some(&self.mode_on)
    }

    /// Replaces the mode byte and preserves the other bytes returned by `GET_REPORT`.
    pub fn apply_mode(self, data: &mut [u8; MODE_SWITCH_TRANSFER_LEN], enabled: bool) {
        if let Some(byte) = data.get_mut(self.mode_offset) {
            *byte = if enabled { self.mode_on } else { self.mode_off };
        }
    }

    /// Reads the mode register into a zeroed transfer buffer, updates its mode byte,
    /// and writes it back. Returns the first failure or the final transfer status.
    pub fn set_mode<F>(
        self,
        data: &mut [u8; MODE_SWITCH_TRANSFER_LEN],
        enabled: bool,
        mut transfer: F,
    ) -> NtStatus
    where
        F: FnMut(&UsbSetupPacket, &mut [u8; MODE_SWITCH_TRANSFER_LEN]) -> NtStatus,
    {
        *data = [0; MODE_SWITCH_TRANSFER_LEN];
        let status = transfer(&self.read_setup(), data);
        if !nt_success(status) {
            return status;
        }

        self.apply_mode(data, enabled);
        transfer(&self.write_setup(), data)
    }
}

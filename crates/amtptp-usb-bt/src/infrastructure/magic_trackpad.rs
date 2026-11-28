//! Magic Trackpad 2 identification, transport parameters and input packet decoding.

use amtptp_core::domain::Contact;
use amtptp_core::hid::common::{MAX_FINGERS, PTP_CONTACT_SLOTS};
use amtptp_core::hid::status::{NtStatus, STATUS_NOT_SUPPORTED};

use crate::infrastructure::bcm5974::config::MAGIC_TRACKPAD_2;
pub use crate::infrastructure::bcm5974::config::{
    Bcm5974Param, MAGIC_TRACKPAD_2_PID as HID_PID_MAGIC_TRACKPAD_2,
    USB_VENDOR_ID_APPLE as HID_VID_APPLE_USB,
};

/// `HID_VID_APPLE_BT` (Device.h:15): Apple's Bluetooth vendor ID.
pub const HID_VID_APPLE_BT: u16 = 0x004C;
/// True for the two vendor IDs the driver accepts (0x05AC, 0x004C). Used by the mode switch
/// (Device.c:308) and by the lower-read completion (Input.c:156).
#[must_use]
pub const fn is_apple_vendor(vendor_id: u16) -> bool {
    vendor_id == HID_VID_APPLE_USB || vendor_id == HID_VID_APPLE_BT
}

/// The parameters `PtpFilterConfigureMultiTouch` writes into the device context (Device.c:322-354).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportParams {
    /// `InputHeaderSize`: raw-report bytes before finger 0 (USB 12, BT 4).
    pub header_size: usize,
    /// `InputFingerSize`: bytes per finger record (9, `FSIZE_TYPE5`).
    pub finger_size: usize,
    /// `InputFingerDelta`: extra offset to finger 0 (0, `FDELTA_TYPE5`).
    pub finger_delta: usize,
    /// `InputButtonDelta`: absolute offset of the button byte (1, `BOFFSET_TYPE5`).
    pub button_delta: usize,
    /// `X` = [-3678, 3934].
    pub x: Bcm5974Param,
    /// `Y` = [-2479, 2586].
    pub y: Bcm5974Param,
}

/// The Magic Trackpad 2 transport, chosen by vendor ID (Device.c:322 / 343).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// VID 0x05AC. No INF binds the filter to the USB trackpad (the USB Magic Trackpad 2 has its own
    /// driver, `AmtPtpDeviceUsbKm`); the transport is kept for the C behaviour.
    Usb,
    /// VID 0x004C (the shipping configuration).
    Bluetooth,
}

impl Transport {
    /// The support checks of `PtpFilterConfigureMultiTouch` (Device.c:308-317), in C order: an unknown
    /// vendor ID, then a product ID other than 0x0265, give `STATUS_NOT_SUPPORTED`. Neither failure
    /// writes any device-context field.
    ///
    /// # Errors
    /// Returns `STATUS_NOT_SUPPORTED` if the vendor or product ID has no supported configuration.
    pub const fn from_ids(vendor_id: u16, product_id: u16) -> Result<Self, NtStatus> {
        if !is_apple_vendor(vendor_id) {
            return Err(STATUS_NOT_SUPPORTED);
        }
        if product_id != HID_PID_MAGIC_TRACKPAD_2 {
            return Err(STATUS_NOT_SUPPORTED);
        }
        if vendor_id == HID_VID_APPLE_USB {
            Ok(Self::Usb)
        } else {
            Ok(Self::Bluetooth)
        }
    }

    /// The parsing parameters for this transport. They differ only in the header size.
    #[must_use]
    pub const fn params(self) -> TransportParams {
        let format = MAGIC_TRACKPAD_2.tp_type;
        TransportParams {
            header_size: match self {
                Self::Usb => format.header() as usize,
                Self::Bluetooth => 4, // HOFFSET_TYPE_BTH_5 = 2 * 2
            },
            finger_size: format.finger_size() as usize,
            finger_delta: format.delta() as usize,
            button_delta: format.button() as usize,
            x: MAGIC_TRACKPAD_2.x,
            y: MAGIC_TRACKPAD_2.y,
        }
    }

    /// The vendor `SET_FEATURE` that switches the trackpad into raw multitouch mode (Device.c:335-361).
    #[must_use]
    pub const fn mode_switch(self) -> ModeSwitch {
        match self {
            Self::Usb => ModeSwitch {
                report_id: 0x02,
                payload: &[0x02, 0x01, 0x00, 0x00],
            },
            Self::Bluetooth => ModeSwitch {
                report_id: 0xF1,
                payload: &[0xF1, 0x02, 0x01],
            },
        }
    }
}

/// One mode-switch feature report: `HID_XFER_PACKET.reportId` and the `reportBufferLen` bytes at
/// `reportBuffer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeSwitch {
    /// `reportId` (USB 0x02, BT 0xF1).
    pub report_id: u8,
    /// The report bytes; `reportBufferLen` is their length (USB `02 01 00 00`, BT `F1 02 01`).
    pub payload: &'static [u8],
}

/// `sizeof(HID_XFER_PACKET)` on LP64: 16 bytes (`reportBuffer`@0, `reportBufferLen`@8, `reportId`@12,
/// 3 padding bytes). `GET/SET_FEATURE` reject a transfer buffer shorter than this.
pub use amtptp_core::hid::hidport::HID_XFER_PACKET_LEN;
/// `offsetof(HID_XFER_PACKET, reportBuffer)`.
pub const XFER_REPORT_BUFFER_OFFSET: usize = 0;
/// `offsetof(HID_XFER_PACKET, reportBufferLen)`.
pub const XFER_REPORT_BUFFER_LEN_OFFSET: usize = 8;
/// `offsetof(HID_XFER_PACKET, reportId)`.
pub const XFER_REPORT_ID_OFFSET: usize = 12;
/// `HID_XFER_PACKET_SIZE` (Device.h:12): the 32-byte buffer that holds the mode-switch packet followed
/// by its report bytes. It is both the preallocated WDFMEMORY (input length 32) and `Irp->UserBuffer`.
pub const HID_XFER_PACKET_SIZE: usize = 32;

pub use crate::bcm5974::input::{MT2_FINGER_LEN as FINGER_RECORD_LEN, mt2_contact as parse_finger};

/// The device-context fields the completion routine reads (snapshot them with
/// [`crate::bluetooth::DeviceState::input_layout`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputLayout {
    /// `InputHeaderSize`.
    pub header_size: usize,
    /// `InputFingerSize` (the stride between finger records).
    pub finger_size: usize,
    /// `InputFingerDelta`.
    pub finger_delta: usize,
    /// `InputButtonDelta` (absolute offset of the button byte).
    pub button_delta: usize,
    /// `X.min`.
    pub x_min: i32,
    /// `Y.min`.
    pub y_min: i32,
}

impl InputLayout {
    fn finger_count(&self, length: usize) -> usize {
        length
            .checked_sub(self.header_size)
            .and_then(|payload| payload.checked_div(self.finger_size))
            .unwrap_or(0)
    }

    /// Whether the length contains a header and only complete finger records.
    pub(crate) fn holds_whole_records(&self, length: usize) -> bool {
        length
            .checked_sub(self.header_size)
            .is_some_and(|payload| payload.checked_rem(self.finger_size) == Some(0))
    }
}

/// The report ID of the Magic Trackpad 2's multitouch input report on the transport that `vendor_id`
/// identifies: 0x02 over USB (`TRACKPAD2_USB_REPORT_ID` in Linux hid-magicmouse) and 0x31 over
/// Bluetooth (`TRACKPAD2_BT_REPORT_ID`). Every edge packet of the C oracle starts with these bytes.
#[must_use]
pub const fn multitouch_report_id(vendor_id: u16) -> u8 {
    if vendor_id == HID_VID_APPLE_USB {
        0x02
    } else {
        0x31
    }
}

/// The number of complete finger records implied by the length, capped at five.
#[must_use]
pub fn contact_count(layout: &InputLayout, length: usize) -> u8 {
    layout.finger_count(length).min(PTP_CONTACT_SLOTS) as u8
}

/// Reads `buffer[base + offset]`, or 0 if that is outside `buffer` (port policy P2). With the 1024-byte
/// lookaside buffer every byte C reads is inside it.
fn byte_at(buffer: &[u8], base: Option<usize>, offset: usize) -> u8 {
    base.and_then(|b| b.checked_add(offset))
        .and_then(|i| buffer.get(i))
        .copied()
        .unwrap_or(0)
}

const MAX_RAW_MT2_FINGERS: usize = MAX_FINGERS as usize;

/// Decodes up to 16 raw Magic Trackpad 2 finger records and the button bit from `buffer`.
#[must_use]
pub fn decode_raw_packet(
    layout: &InputLayout,
    length: usize,
    buffer: &[u8],
) -> ([Contact; MAX_RAW_MT2_FINGERS], usize, bool) {
    let raw_count = layout.finger_count(length).min(MAX_RAW_MT2_FINGERS);
    let mut contacts = [Contact::EMPTY; MAX_RAW_MT2_FINGERS];
    let first = layout.header_size.checked_add(layout.finger_delta);
    for (i, slot) in contacts.iter_mut().take(raw_count).enumerate() {
        let base = first.and_then(|f| {
            let o = i.checked_mul(layout.finger_size)?;
            f.checked_add(o)
        });
        let mut record = [0_u8; FINGER_RECORD_LEN];
        for (j, byte) in record.iter_mut().enumerate() {
            *byte = byte_at(buffer, base, j);
        }
        *slot = parse_finger(&record, layout.x_min, layout.y_min);
    }
    let button_pressed = byte_at(buffer, Some(layout.button_delta), 0) & 1 != 0;
    (contacts, raw_count, button_pressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_checks_in_c_order() {
        assert_eq!(
            Transport::from_ids(0x004C, 0x0265),
            Ok(Transport::Bluetooth)
        );
        assert_eq!(Transport::from_ids(0x05AC, 0x0265), Ok(Transport::Usb));
        assert_eq!(
            Transport::from_ids(0x1234, 0x0265),
            Err(STATUS_NOT_SUPPORTED)
        );
        assert_eq!(
            Transport::from_ids(0x004C, 0x0266),
            Err(STATUS_NOT_SUPPORTED)
        );
        assert_eq!(Transport::Usb.params().header_size, 12);
        assert_eq!(Transport::Bluetooth.params().header_size, 4);
    }

    #[test]
    fn report_ids_per_transport() {
        assert_eq!(multitouch_report_id(0x05AC), 0x02);
        assert_eq!(multitouch_report_id(0x004C), 0x31);
    }

    #[test]
    fn minimum_signed_y_maps_to_top_of_logical_range() {
        let c = parse_finger(&[0, 0, 0, 2, 10, 10, 0, 0, 6], -3678, -2479);
        assert_eq!((c.x, c.y), (3678, 5065));
    }
}

//! Magic Trackpad transport state, input decoding and HID report handling.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

pub mod bth;
pub mod config;
mod descriptor;
mod device;
mod feature;
mod input;
mod status;

pub use descriptor::{
    HID_DESCRIPTOR, MT2_AXES, REPORT_DESCRIPTOR, device_attributes, hid_descriptor_for,
    report_descriptor_for,
};
pub use device::{
    Bcm5974Param, DEVICE_VERSION, DeviceSnapshot, DeviceState, HID_PID_MAGIC_TRACKPAD_2,
    HID_VID_APPLE_BT, HID_VID_APPLE_USB, HID_XFER_PACKET_LEN, HID_XFER_PACKET_SIZE, ModeSwitch,
    ReadAction, RecoveryTicket, Transport, TransportParams, XFER_REPORT_BUFFER_LEN_OFFSET,
    XFER_REPORT_BUFFER_OFFSET, XFER_REPORT_ID_OFFSET, is_apple_vendor,
};
pub use feature::{FeatureChange, check_xfer_length, decode_set_feature, get_feature_report};
pub use input::{
    FINGER_RECORD_LEN, InputLayout, LowerReadClass, REPORT_BUFFER_SIZE, build_report,
    classify_lower_read, contact_count, decode_raw_packet, finish_tracked_report,
    multitouch_report_id, parse_finger,
};
pub use ptp_hid::contact_tracker::ContactTracker;
pub use ptp_hid::hidport::{XferPacket, XferPacketMut};
pub use ptp_hid::ioctl::InternalIoctl;
pub use ptp_hid::read_tracker::{LowerReadOrigin, ReadTicket, ReadTracker, SuspendedReads};
pub use status::{
    NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_CANCELLED, STATUS_INVALID_BUFFER_SIZE,
    STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_DEVICE_STATE, STATUS_INVALID_STATE_TRANSITION,
    STATUS_NO_MORE_ENTRIES, STATUS_NOT_SUPPORTED, STATUS_SUCCESS, STATUS_UNSUCCESSFUL, nt_success,
};

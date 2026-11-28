//! Magic Trackpad transport state, input decoding and HID report handling.
pub use crate::application::bluetooth::{
    DeviceSnapshot, DeviceState, FeatureChange, LowerReadClass, ReadAction, RecoveryTicket,
};
pub use crate::infrastructure::bth;
pub use crate::infrastructure::magic_trackpad::{
    Bcm5974Param, FINGER_RECORD_LEN, HID_PID_MAGIC_TRACKPAD_2, HID_VID_APPLE_BT, HID_VID_APPLE_USB,
    HID_XFER_PACKET_LEN, HID_XFER_PACKET_SIZE, InputLayout, ModeSwitch, Transport, TransportParams,
    XFER_REPORT_BUFFER_LEN_OFFSET, XFER_REPORT_BUFFER_OFFSET, XFER_REPORT_ID_OFFSET, contact_count,
    decode_raw_packet, is_apple_vendor, multitouch_report_id, parse_finger,
};
pub mod config;
mod descriptor;
mod feature;
mod input;
mod status;

pub use amtptp_core::domain::ContactTracker;
pub use amtptp_core::hid::delivery::{CompletionQueue, EnqueueOutcome};
pub use amtptp_core::hid::hidport::{XferPacket, XferPacketMut};
pub use amtptp_core::hid::ioctl::InternalIoctl;
pub use amtptp_core::hid::read_tracker::{
    LowerReadOrigin, PostSendAction, ReadTicket, ReadTracker, SuspendedReads,
};
pub use descriptor::{
    DEVICE_VERSION, HID_DESCRIPTOR, MT2_AXES, REPORT_DESCRIPTOR, device_attributes,
    hid_descriptor_for, report_descriptor_for,
};
pub use feature::{check_xfer_length, decode_set_feature, get_feature_report};
pub use input::{REPORT_BUFFER_SIZE, build_report, classify_lower_read, finish_tracked_report};
pub use status::{
    NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_CANCELLED, STATUS_INVALID_BUFFER_SIZE,
    STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_DEVICE_STATE, STATUS_INVALID_STATE_TRANSITION,
    STATUS_NO_MORE_ENTRIES, STATUS_NOT_SUPPORTED, STATUS_SUCCESS, STATUS_UNSUCCESSFUL, nt_success,
};

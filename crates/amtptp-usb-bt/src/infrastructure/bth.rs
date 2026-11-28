//! The contract between the Bluetooth lower filter (`AmtPtpBthFilter`, below `HidBth`) and the
//! collection filter (`AmtPtpHidFilter`).
//!
//! The Magic Trackpad 2 sends its multitouch report `0x31` and accepts the mode-switch feature
//! `0xF1` without declaring either in its SDP HID descriptor. HIDCLASS drops undeclared reports, so
//! the original design read `HidBth` directly (the detour), competing with HIDCLASS for every packet.
//!
//! The lower filter instead declares both reports, so HIDCLASS itself delivers them:
//!
//! 1. [`augment_sdp_attribute_list`] appends [`VENDOR_TLC`] (a vendor top-level collection with input
//!    `0x31` and feature `0xF1`) to the report descriptor in the SDP attribute list that `HidBth`
//!    reads at start. It becomes the device's last collection (`Col03`); `Col01` and `Col02` keep
//!    their numbers.
//! 2. HIDCLASS walks a read in steps of the declared report length and drops a shorter remainder, so
//!    a variable-length report cannot be declared as it is. [`pad_interrupt_payload`] pads every
//!    `0x31` interrupt payload to [`PADDED_REPORT_LEN`] and stores the original length in the last
//!    byte; [`unpad_report`] reverses it on the reader's side.
//!
//! Everything here is pure; the drivers only move bytes.

/// Report ID of the multitouch input report (Bluetooth).
pub const MULTITOUCH_REPORT_ID: u8 = 0x31;
/// Report ID of the mode-switch feature report (Bluetooth): `F1 02 01`.
pub const MODE_SWITCH_REPORT_ID: u8 = 0xF1;

mod report;
mod sdp;

pub use report::{
    HIDP_DATA_INPUT, MOUSE_REPORT_ID, PADDED_PAYLOAD_LEN, PADDED_REPORT_LEN, PaddedReport,
    RAW_REPORT_MAX_LEN, pad_interrupt_payload, unpad_report,
};
pub use sdp::{
    SdpAugmentError, VENDOR_TLC, augment_sdp_attribute_list, descriptor_declares_report,
};

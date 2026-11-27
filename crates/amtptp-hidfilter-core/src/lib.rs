//! Hardware-independent logic of the `AmtPtpHidFilter` (KMDF, Bluetooth Magic Trackpad 2) driver.
//!
//! The C driver (`src/AmtPtpHidFilter`) is a KMDF lower filter below mshidkmdf on a HID collection of
//! the Magic Trackpad 2. It answers the HID minidriver IOCTLs with a synthetic Precision Touchpad
//! identity, handles the PTP feature reports in software, switches the trackpad into raw multitouch
//! mode with a vendor `SET_FEATURE`, and turns each raw Apple report into one 50-byte PTP input report.
//!
//! This crate holds all of that logic with the C driver's exact observable behaviour, plus the port policy
//! P2 safety fixes and the P3 fixes of its PnP/power handling: the IDs survive a PnP restart (HF-B30), a
//! mode switch whose lower request failed is a failure (HF-B13), the recovery timer and work item only
//! act on a running device, and a successful configuration issues the lower reads that parked upper
//! reads are missing. The read pipeline also reads the completion status, the report ID and the real
//! length of every lower read (HF-B7, HF-B8, HF-B29), and `D0Exit` cancels the reads in flight, whose
//! late completions are dropped (HF-B14). It has no WDK dependency: framework calls go through the [`FilterIo`],
//! [`DetourIo`] and [`AddDeviceIo`] traits, which the driver crate implements with one WDF call per
//! method. The host tests implement the same traits with the C oracle's stub model and replay the
//! fixtures recorded from the real C code.
//!
//! The driver has no device table (only VID/PID comparisons, [`Transport::from_ids`]) and sends no USB
//! or SPI traffic. Its only requests to the hardware are internal HID IOCTLs to the PDO:
//! `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (32-byte output), the mode-switch `IOCTL_HID_SET_FEATURE`
//! ([`XferBuffer`]) and `IOCTL_HID_READ_REPORT` (1024-byte output).
//!
//! # How the driver uses this
//!
//! Device context: one [`DeviceState`] (atomics; write [`DeviceState::new`] in the create path), a
//! [`ReadTracker`] behind a spin lock, plus the WDF handles (`HidReadQueue`, lookaside list, timer,
//! work item, I/O target, WDM device object).
//! Implement [`FilterIo`] + [`DetourIo`] on a small per-call struct (device handle + context
//! reference), and [`AddDeviceIo`] on a struct holding the `PWDFDEVICE_INIT`. Then every callback is a
//! one-liner:
//!
//! | WDF callback (C function) | call |
//! |---|---|
//! | `EvtDriverDeviceAdd` (`PtpFilterEvtDeviceAdd`) | [`add_device`] |
//! | `EvtDevicePrepareHardware` (`PtpFilterPrepareHardware`) | [`prepare_hardware`] |
//! | `EvtDeviceD0Entry` (`PtpFilterDeviceD0Entry`) | [`d0_entry`] |
//! | `EvtDeviceD0Exit` (`PtpFilterDeviceD0Exit`) | [`d0_exit`] |
//! | `EvtDeviceSelfManagedIoInit` (`PtpFilterSelfManagedIoInit`) | [`self_managed_io_init`] |
//! | `EvtDeviceSelfManagedIoRestart` (`PtpFilterSelfManagedIoRestart`) | [`self_managed_io_restart`] |
//! | `EvtTimerFunc` (`PtpFilterRecoveryTimerCallback`) | [`recovery_timer_callback`] |
//! | `EvtWorkItemFunc` (`PtpFilterWorkItemCallback`) | [`work_item_callback`] |
//! | `EvtIoInternalDeviceControl` (`FilterEvtIoIntDeviceControl`) | [`dispatch_internal_ioctl`] |
//! | lower-read completion routine (`PtpFilterInputRequestCompletionCallback`) | [`complete_lower_read`] |
//! | `EvtIoStop` (`FilterEvtIoStop`) | nothing (no-op, as in C) |
//!
//! Object configuration (timer, work item, queues, lookaside list, interface GUID, due times) is in
//! [`config`]. Only the glue touches raw memory: the detour's pointer chase ([`detour`] has the
//! offsets), `HID_XFER_PACKET` access for feature requests ([`XferPacket`], [`XferPacketMut`]) and the
//! mode-switch buffer ([`XferBuffer::as_mut_ptr`]).
//!
//! The pure pieces are public too, for tracing and tests: [`classify_lower_read`], [`build_report`],
//! [`parse_finger`], [`get_feature_report`], [`decode_set_feature`],
//! [`hid_descriptor_for`], [`report_descriptor_for`], [`device_attributes`], [`InternalIoctl`].

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

mod add_device;
pub mod config;
mod descriptor;
pub mod detour;
mod device;
mod dispatch;
mod feature;
mod input;
mod io;
mod pipeline;
mod pnp;
mod status;

pub use add_device::{AddDeviceIo, add_device};
pub use descriptor::{
    HID_DESCRIPTOR, MT2_AXES, REPORT_DESCRIPTOR, device_attributes, hid_descriptor_for,
    report_descriptor_for,
};
pub use detour::{DetourIo, client_id_matches, detour_hid_stack};
pub use device::{
    Bcm5974Param, DEVICE_VERSION, DeviceSnapshot, DeviceState, HID_PID_MAGIC_TRACKPAD_2, RecoveryTicket,
    HID_VID_APPLE_BT, HID_VID_APPLE_USB, HID_XFER_PACKET_LEN, HID_XFER_PACKET_SIZE, ModeSwitch,
    Transport, TransportParams, XFER_REPORT_BUFFER_LEN_OFFSET, XFER_REPORT_BUFFER_OFFSET,
    XFER_REPORT_ID_OFFSET, XferBuffer, is_apple_vendor,
};
pub use dispatch::{InternalIoctl, dispatch_internal_ioctl};
pub use feature::{FeatureChange, check_xfer_length, decode_set_feature, get_feature_report};
pub use input::{
    FINGER_RECORD_LEN, InputLayout, LowerReadClass, REPORT_BUFFER_SIZE, build_report,
    classify_lower_read, contact_count, multitouch_report_id, parse_finger,
};
pub use io::{FailedAction, FilterIo, XferPacket, XferPacketMut};
pub use pipeline::{
    LowerReadOutcome, complete_lower_read, issue_backlog_reads, issue_lower_read, work_item_callback,
};
pub use ptp_hid::read_tracker::{LowerReadOrigin, ReadTicket, ReadTracker, SuspendedReads};
pub use pnp::{
    configure_multitouch, d0_entry, d0_exit, prepare_hardware, recovery_timer_callback,
    self_managed_io_init, self_managed_io_restart,
};
pub use status::{
    NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_CANCELLED, STATUS_INVALID_BUFFER_SIZE,
    STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_DEVICE_STATE, STATUS_INVALID_STATE_TRANSITION,
    STATUS_NO_MORE_ENTRIES, STATUS_NOT_SUPPORTED, STATUS_SUCCESS, STATUS_UNSUCCESSFUL, nt_success,
};

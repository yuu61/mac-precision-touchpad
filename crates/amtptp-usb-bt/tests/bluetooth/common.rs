//! Shared fixture parsing and a mock of the framework that follows the C oracle's stub model
//! (`tests/bluetooth/fixtures/README.md` section 5). Every trait call is recorded in the fixtures' `ev` format.
#![allow(
    dead_code,
    reason = "Each integration test target uses a different subset of these helpers"
)]

use amtptp_usb_bt::bluetooth::NtStatus;

mod fixture;
mod mock;

pub use fixture::{
    Rec, Tally, ev_args, ev_name_result, events, load, num, parse_state, st, started, status,
    to_hex,
};
pub use mock::{Feature, Mock, Upper};

/// Information preset on every upper request (README 2.2).
pub const INFO_IN: usize = 23130;

// Fault statuses (README 5).
pub const RESOURCE_FAIL: NtStatus = 0xC000_009A_u32 as i32;
pub const FORMAT_FAIL: NtStatus = 0xC000_0184_u32 as i32;
pub const SEND_FAIL: NtStatus = 0xC000_009D_u32 as i32;
pub const EXEC_LEVEL_FAIL: NtStatus = 0xC020_0225_u32 as i32;
pub const TRUE_STATUS_FAIL: NtStatus = 0xC000_0001_u32 as i32;
const BUFFER_TOO_SMALL: NtStatus = 0xC000_0023_u32 as i32;
const NO_MORE_ENTRIES: NtStatus = 0x8000_001A_u32 as i32;

type Filter<'a> = Mock;
type UpperRequest = usize;
type LowerRead = usize;
type ReadMemory = ();
type QueuedCompletion = (usize, NtStatus);
/// `WDF_DEVICE_FAILED_ACTION` for `WdfDeviceSetFailed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailedAction {
    /// `WdfDeviceFailedAttemptRestart`.
    AttemptRestart,
    /// `WdfDeviceFailedNoRestart`.
    NoRestart,
}

#[path = "../../../../drivers/interface/bluetooth/hid/io/dispatch.rs"]
pub(crate) mod dispatch;
#[path = "../../../../drivers/interface/bluetooth/hid/io/pipeline.rs"]
pub(crate) mod pipeline;
#[path = "../../../../drivers/interface/bluetooth/hid/io/pnp.rs"]
pub(crate) mod pnp;

type AddDevice = Mock;
#[path = "../../../../drivers/interface/bluetooth/hid/device/add.rs"]
pub(crate) mod add;

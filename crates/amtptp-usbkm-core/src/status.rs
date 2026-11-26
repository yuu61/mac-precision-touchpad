//! NTSTATUS values the C driver produces, and the `(status, Information)` pair a request is completed
//! with.
//!
//! The core crate has no WDK dependency, so NTSTATUS is a plain `i32` here. It is the same type as
//! `wdk_sys::NTSTATUS`, so the glue passes these values straight to `WdfRequestCompleteWithInformation`.

/// `NTSTATUS` (a signed 32-bit value; the high bits encode severity).
pub type NtStatus = i32;

/// `(NTSTATUS)0x...L` from `ntstatus.h`: the bit pattern reinterpreted as `i32`.
const fn hex(value: u32) -> NtStatus {
    // Same bit pattern as the C cast `(NTSTATUS)0xC0000023L`.
    value as NtStatus
}

/// `STATUS_SUCCESS` (0x00000000).
pub const STATUS_SUCCESS: NtStatus = hex(0x0000_0000);
/// `STATUS_INVALID_DEVICE_REQUEST` (0xC0000010): GET/SET_FEATURE with a NULL `Irp->UserBuffer`.
pub const STATUS_INVALID_DEVICE_REQUEST: NtStatus = hex(0xC000_0010);
/// `STATUS_BUFFER_TOO_SMALL` (0xC0000023): the `HID_XFER_PACKET` length check of GET/SET_FEATURE, and the
/// status `WdfMemoryCopyFromBuffer` / `WdfRequestRetrieveOutputBuffer` return for a short output buffer.
pub const STATUS_BUFFER_TOO_SMALL: NtStatus = hex(0xC000_0023);
/// `STATUS_DATA_ERROR` (0xC000003E): READ_REPORT completed for a packet with more than 5 finger blocks.
pub const STATUS_DATA_ERROR: NtStatus = hex(0xC000_003E);
/// `STATUS_UNKNOWN_REVISION` (0xC0000058): unknown trackpad type in the reader configuration (dead, and
/// masked by spec B9).
pub const STATUS_UNKNOWN_REVISION: NtStatus = hex(0xC000_0058);
/// `STATUS_NOT_SUPPORTED` (0xC00000BB): unhandled IOCTLs, unknown feature report IDs, input mode 0.
pub const STATUS_NOT_SUPPORTED: NtStatus = hex(0xC000_00BB);
/// `STATUS_CANCELLED` (0xC0000120): what WDF completes still-queued READ_REPORTs with on cancel/purge.
pub const STATUS_CANCELLED: NtStatus = hex(0xC000_0120);
/// `STATUS_INVALID_DEVICE_STATE` (0xC0000184): no interrupt pipe found in PrepareHardware.
pub const STATUS_INVALID_DEVICE_STATE: NtStatus = hex(0xC000_0184);
/// `STATUS_INVALID_BUFFER_SIZE` (0xC0000206): feature report buffer too small (caps < 3, HQA < 257).
pub const STATUS_INVALID_BUFFER_SIZE: NtStatus = hex(0xC000_0206);

/// `NT_SUCCESS(status)`: `status >= 0`.
pub const fn nt_success(status: NtStatus) -> bool {
    status >= 0
}

/// How the dispatcher completes a request: `WdfRequestSetInformation(Request, information)` followed by
/// `WdfRequestComplete(Request, status)` (or `WdfRequestCompleteWithInformation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    /// Completion status.
    pub status: NtStatus,
    /// `IoStatus.Information` (bytes written).
    pub information: usize,
}

impl Completion {
    /// A completion with `Information = 0`, which is what every C handler leaves when it does not call
    /// `WdfRequestSetInformation`.
    pub const fn status(status: NtStatus) -> Self {
        Self {
            status,
            information: 0,
        }
    }

    /// `STATUS_SUCCESS` with `Information = information`.
    pub const fn success(information: usize) -> Self {
        Self {
            status: STATUS_SUCCESS,
            information,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_match_ntstatus_h() {
        assert_eq!(STATUS_INVALID_DEVICE_REQUEST as u32, 0xC000_0010);
        assert_eq!(STATUS_BUFFER_TOO_SMALL as u32, 0xC000_0023);
        assert_eq!(STATUS_DATA_ERROR as u32, 0xC000_003E);
        assert_eq!(STATUS_UNKNOWN_REVISION as u32, 0xC000_0058);
        assert_eq!(STATUS_NOT_SUPPORTED as u32, 0xC000_00BB);
        assert_eq!(STATUS_CANCELLED as u32, 0xC000_0120);
        assert_eq!(STATUS_INVALID_DEVICE_STATE as u32, 0xC000_0184);
        assert_eq!(STATUS_INVALID_BUFFER_SIZE as u32, 0xC000_0206);
        assert!(nt_success(STATUS_SUCCESS));
        assert!(!nt_success(STATUS_NOT_SUPPORTED));
        assert!(nt_success(0x0000_0103)); // STATUS_PENDING is a success code
    }
}

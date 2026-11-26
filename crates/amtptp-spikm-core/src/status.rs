//! The `NTSTATUS` values the SpiKm driver returns or tests, as plain `i32`s.
//!
//! This crate does not depend on wdk-sys, so it defines its own constants. `NtStatus` has the same
//! representation as `wdk_sys::NTSTATUS` (`i32`), so the glue can pass the values through unchanged.

/// `NTSTATUS`: a 32-bit status code, stored as `i32` like the C typedef `LONG`.
pub type NtStatus = i32;

/// `(NTSTATUS)0x...L` from `ntstatus.h`: the 32-bit pattern reinterpreted as `i32`, like the C cast.
const fn ntstatus(bits: u32) -> NtStatus {
    // Bit-pattern reinterpretation (u32 -> i32 keeps all 32 bits), exactly what the C cast does.
    bits as NtStatus
}

/// `STATUS_SUCCESS` (0x00000000).
pub const STATUS_SUCCESS: NtStatus = ntstatus(0x0000_0000);
/// `STATUS_BUFFER_OVERFLOW` (0x80000005). A warning, so `NT_SUCCESS` is false for it.
pub const STATUS_BUFFER_OVERFLOW: NtStatus = ntstatus(0x8000_0005);
/// `STATUS_NO_MORE_ENTRIES` (0x8000001A): what `WdfIoQueueRetrieveNextRequest` returns for an empty queue.
pub const STATUS_NO_MORE_ENTRIES: NtStatus = ntstatus(0x8000_001A);
/// `STATUS_UNSUCCESSFUL` (0xC0000001).
pub const STATUS_UNSUCCESSFUL: NtStatus = ntstatus(0xC000_0001);
/// `STATUS_INVALID_PARAMETER` (0xC000000D). Used only internally by the C PrepareHardware and then
/// discarded.
pub const STATUS_INVALID_PARAMETER: NtStatus = ntstatus(0xC000_000D);
/// `STATUS_INVALID_DEVICE_REQUEST` (0xC0000010).
pub const STATUS_INVALID_DEVICE_REQUEST: NtStatus = ntstatus(0xC000_0010);
/// `STATUS_BUFFER_TOO_SMALL` (0xC0000023).
pub const STATUS_BUFFER_TOO_SMALL: NtStatus = ntstatus(0xC000_0023);
/// `STATUS_OBJECT_NAME_NOT_FOUND` (0xC0000034): a missing registry key or value.
pub const STATUS_OBJECT_NAME_NOT_FOUND: NtStatus = ntstatus(0xC000_0034);
/// `STATUS_DEVICE_DATA_ERROR` (0xC000009C): a lower read shorter than the 46-byte packet header.
pub const STATUS_DEVICE_DATA_ERROR: NtStatus = ntstatus(0xC000_009C);
/// `STATUS_NOT_SUPPORTED` (0xC00000BB).
pub const STATUS_NOT_SUPPORTED: NtStatus = ntstatus(0xC000_00BB);
/// `STATUS_CANCELLED` (0xC0000120): D0Exit completes every queued `IOCTL_HID_READ_REPORT` with it.
pub const STATUS_CANCELLED: NtStatus = ntstatus(0xC000_0120);
/// `STATUS_INVALID_DEVICE_STATE` (0xC0000184).
pub const STATUS_INVALID_DEVICE_STATE: NtStatus = ntstatus(0xC000_0184);
/// `STATUS_INVALID_BUFFER_SIZE` (0xC0000206).
pub const STATUS_INVALID_BUFFER_SIZE: NtStatus = ntstatus(0xC000_0206);
/// `STATUS_NOT_FOUND` (0xC0000225).
pub const STATUS_NOT_FOUND: NtStatus = ntstatus(0xC000_0225);

/// `NT_SUCCESS(Status)`: true for success and informational codes (`Status >= 0`), false for warnings
/// (0x8xxxxxxx) and errors (0xCxxxxxxx).
pub const fn nt_success(status: NtStatus) -> bool {
    status >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_match_ntstatus_h() {
        assert_eq!(STATUS_SUCCESS as u32, 0);
        assert_eq!(STATUS_BUFFER_OVERFLOW as u32, 0x8000_0005);
        assert_eq!(STATUS_DEVICE_DATA_ERROR as u32, 0xC000_009C);
        assert_eq!(STATUS_NOT_FOUND as u32, 0xC000_0225);
    }

    #[test]
    fn nt_success_rejects_warnings() {
        assert!(nt_success(STATUS_SUCCESS));
        assert!(nt_success(0x0000_0103)); // STATUS_PENDING
        assert!(!nt_success(STATUS_BUFFER_OVERFLOW));
        assert!(!nt_success(STATUS_NOT_FOUND));
    }
}

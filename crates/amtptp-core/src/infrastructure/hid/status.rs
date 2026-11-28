//! WDK-independent NTSTATUS codes and request completion values shared by the driver cores.

/// A signed 32-bit Windows status code.
pub type NtStatus = i32;

/// Interprets the WDK status bit pattern as a signed value.
const fn status(bits: u32) -> NtStatus {
    bits as NtStatus
}

/// `STATUS_SUCCESS` (0x00000000).
pub const STATUS_SUCCESS: NtStatus = status(0x0000_0000);
/// `STATUS_BUFFER_OVERFLOW` (0x80000005).
pub const STATUS_BUFFER_OVERFLOW: NtStatus = status(0x8000_0005);
/// `STATUS_NO_MORE_ENTRIES` (0x8000001A).
pub const STATUS_NO_MORE_ENTRIES: NtStatus = status(0x8000_001A);
/// `STATUS_UNSUCCESSFUL` (0xC0000001).
pub const STATUS_UNSUCCESSFUL: NtStatus = status(0xC000_0001);
/// `STATUS_INVALID_PARAMETER` (0xC000000D).
pub const STATUS_INVALID_PARAMETER: NtStatus = status(0xC000_000D);
/// `STATUS_NO_SUCH_DEVICE` (0xC000000E).
pub const STATUS_NO_SUCH_DEVICE: NtStatus = status(0xC000_000E);
/// `STATUS_INVALID_DEVICE_REQUEST` (0xC0000010).
pub const STATUS_INVALID_DEVICE_REQUEST: NtStatus = status(0xC000_0010);
/// `STATUS_BUFFER_TOO_SMALL` (0xC0000023).
pub const STATUS_BUFFER_TOO_SMALL: NtStatus = status(0xC000_0023);
/// `STATUS_OBJECT_NAME_NOT_FOUND` (0xC0000034).
pub const STATUS_OBJECT_NAME_NOT_FOUND: NtStatus = status(0xC000_0034);
/// `STATUS_DATA_ERROR` (0xC000003E).
pub const STATUS_DATA_ERROR: NtStatus = status(0xC000_003E);
/// `STATUS_DELETE_PENDING` (0xC0000056).
pub const STATUS_DELETE_PENDING: NtStatus = status(0xC000_0056);
/// `STATUS_UNKNOWN_REVISION` (0xC0000058).
pub const STATUS_UNKNOWN_REVISION: NtStatus = status(0xC000_0058);
/// `STATUS_DEVICE_DATA_ERROR` (0xC000009C).
pub const STATUS_DEVICE_DATA_ERROR: NtStatus = status(0xC000_009C);
/// `STATUS_DEVICE_NOT_CONNECTED` (0xC000009D).
pub const STATUS_DEVICE_NOT_CONNECTED: NtStatus = status(0xC000_009D);
/// `STATUS_NOT_SUPPORTED` (0xC00000BB).
pub const STATUS_NOT_SUPPORTED: NtStatus = status(0xC000_00BB);
/// `STATUS_CANCELLED` (0xC0000120).
pub const STATUS_CANCELLED: NtStatus = status(0xC000_0120);
/// `STATUS_INVALID_DEVICE_STATE` (0xC0000184).
pub const STATUS_INVALID_DEVICE_STATE: NtStatus = status(0xC000_0184);
/// `STATUS_INVALID_BUFFER_SIZE` (0xC0000206).
pub const STATUS_INVALID_BUFFER_SIZE: NtStatus = status(0xC000_0206);
/// `STATUS_NOT_FOUND` (0xC0000225).
pub const STATUS_NOT_FOUND: NtStatus = status(0xC000_0225);
/// `STATUS_INVALID_STATE_TRANSITION` (0xC000A003).
pub const STATUS_INVALID_STATE_TRANSITION: NtStatus = status(0xC000_A003);

/// `NT_SUCCESS(status)`: `status >= 0`.
#[must_use]
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
    #[must_use]
    pub const fn status(status: NtStatus) -> Self {
        Self {
            status,
            information: 0,
        }
    }

    /// `STATUS_SUCCESS` with `Information = information`.
    #[must_use]
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
        assert_eq!(STATUS_SUCCESS as u32, 0x0000_0000);
        assert_eq!(STATUS_BUFFER_OVERFLOW as u32, 0x8000_0005);
        assert_eq!(STATUS_NO_MORE_ENTRIES as u32, 0x8000_001A);
        assert_eq!(STATUS_UNSUCCESSFUL as u32, 0xC000_0001);
        assert_eq!(STATUS_INVALID_PARAMETER as u32, 0xC000_000D);
        assert_eq!(STATUS_INVALID_DEVICE_REQUEST as u32, 0xC000_0010);
        assert_eq!(STATUS_BUFFER_TOO_SMALL as u32, 0xC000_0023);
        assert_eq!(STATUS_OBJECT_NAME_NOT_FOUND as u32, 0xC000_0034);
        assert_eq!(STATUS_DATA_ERROR as u32, 0xC000_003E);
        assert_eq!(STATUS_UNKNOWN_REVISION as u32, 0xC000_0058);
        assert_eq!(STATUS_DEVICE_DATA_ERROR as u32, 0xC000_009C);
        assert_eq!(STATUS_NOT_SUPPORTED as u32, 0xC000_00BB);
        assert_eq!(STATUS_CANCELLED as u32, 0xC000_0120);
        assert_eq!(STATUS_INVALID_DEVICE_STATE as u32, 0xC000_0184);
        assert_eq!(STATUS_INVALID_BUFFER_SIZE as u32, 0xC000_0206);
        assert_eq!(STATUS_NOT_FOUND as u32, 0xC000_0225);
        assert_eq!(STATUS_INVALID_STATE_TRANSITION as u32, 0xC000_A003);
    }

    #[test]
    fn nt_success_accepts_informational_codes_and_rejects_warnings_and_errors() {
        assert!(nt_success(STATUS_SUCCESS));
        assert!(nt_success(0x0000_0103)); // STATUS_PENDING
        assert!(!nt_success(STATUS_BUFFER_OVERFLOW));
        assert!(!nt_success(STATUS_NO_MORE_ENTRIES));
        assert!(!nt_success(STATUS_NOT_FOUND));
    }
}

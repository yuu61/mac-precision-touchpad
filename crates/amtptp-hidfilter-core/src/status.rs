//! The `NTSTATUS` values this driver returns, and `NT_SUCCESS`.
//!
//! The core crate has no WDK dependency, so it defines the handful of status codes it needs itself.
//! [`NtStatus`] is `i32`, the same representation as `wdk_sys::NTSTATUS`, so the glue passes values
//! through unchanged.

/// `NTSTATUS` (a C `LONG`).
pub type NtStatus = i32;

/// Reinterprets a 32-bit status code such as `0xC00000BB` as the signed `NTSTATUS` value C uses.
const fn status(code: u32) -> NtStatus {
    code as i32 // bit-for-bit reinterpretation, as in C
}

/// `STATUS_SUCCESS`.
pub const STATUS_SUCCESS: NtStatus = 0;
/// `STATUS_UNSUCCESSFUL` (0xC0000001): detour sanity checks, a NULL IRP for the mode switch, and the P2
/// fix for a NULL WDM device object in `AddDevice` (HF-B17).
pub const STATUS_UNSUCCESSFUL: NtStatus = status(0xC000_0001);
/// `STATUS_INVALID_DEVICE_REQUEST` (0xC0000010): `GET/SET_FEATURE` with `Irp->UserBuffer == NULL`.
pub const STATUS_INVALID_DEVICE_REQUEST: NtStatus = status(0xC000_0010);
/// `STATUS_BUFFER_TOO_SMALL` (0xC0000023): `GET/SET_FEATURE` with a transfer buffer shorter than
/// `sizeof(HID_XFER_PACKET)`. KMDF also returns it for short output buffers.
pub const STATUS_BUFFER_TOO_SMALL: NtStatus = status(0xC000_0023);
/// `STATUS_NOT_SUPPORTED` (0xC00000BB): unknown internal IOCTLs, unknown feature report IDs,
/// descriptors for a product ID other than 0x0265, and an unsupported device in the mode switch.
pub const STATUS_NOT_SUPPORTED: NtStatus = status(0xC000_00BB);
/// `STATUS_CANCELLED` (0xC0000120): parked upper reads drained in `D0Exit`.
pub const STATUS_CANCELLED: NtStatus = status(0xC000_0120);
/// `STATUS_INVALID_DEVICE_STATE` (0xC0000184): the P2 answer to `IOCTL_HID_GET_STRING` before the
/// detour has set the I/O target (HF-B25; C would send to a NULL target).
pub const STATUS_INVALID_DEVICE_STATE: NtStatus = status(0xC000_0184);
/// `STATUS_INVALID_BUFFER_SIZE` (0xC0000206): `GET_FEATURE` 0x07/0x08 with a report buffer that is too
/// short.
pub const STATUS_INVALID_BUFFER_SIZE: NtStatus = status(0xC000_0206);
/// `STATUS_INVALID_STATE_TRANSITION` (0xC000A003): `SelfManagedIoRestart` before the detour was applied
/// (HF-B11).
pub const STATUS_INVALID_STATE_TRANSITION: NtStatus = status(0xC000_A003);
/// `STATUS_NO_MORE_ENTRIES` (0x8000001A): what `WdfIoQueueRetrieveNextRequest` returns for an empty
/// queue. The core never produces it; it is here for glue and tests.
pub const STATUS_NO_MORE_ENTRIES: NtStatus = status(0x8000_001A);

/// `NT_SUCCESS(status)`: true for success and informational codes (status >= 0).
#[must_use]
pub const fn nt_success(status: NtStatus) -> bool {
    status >= 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_match_wdk() {
        assert_eq!(STATUS_NOT_SUPPORTED as u32, 0xC000_00BB);
        assert_eq!(STATUS_INVALID_STATE_TRANSITION as u32, 0xC000_A003);
        assert!(nt_success(STATUS_SUCCESS));
        assert!(!nt_success(STATUS_NO_MORE_ENTRIES));
        assert!(!nt_success(STATUS_CANCELLED));
    }
}

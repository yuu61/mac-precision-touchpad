//! `NTSTATUS` values used by the driver.
//!
//! The core crate has no WDK dependency, so it carries its own copies of the few status codes that the C
//! driver returns itself (`ntstatus.h` values). Statuses decided by the framework (for example the result
//! of `WdfRequestRetrieveOutputMemory`) are never invented here; they are passed in by the glue through
//! the traits in [`crate::io`].

/// An `NTSTATUS` (`LONG`).
pub type NtStatus = i32;

/// Reinterprets a `0xC000_xxxx`-style literal as the signed `NTSTATUS` (two's complement, as in C).
const fn nt(code: u32) -> NtStatus {
    // Bit-for-bit reinterpretation (C: `((NTSTATUS)0xC0000001L)`).
    code as i32
}

/// `STATUS_SUCCESS`.
pub const STATUS_SUCCESS: NtStatus = 0;
/// `STATUS_UNSUCCESSFUL` (0xC0000001): interface 0 missing in `SelectInterruptInterface`.
pub const STATUS_UNSUCCESSFUL: NtStatus = nt(0xC000_0001);
/// `STATUS_BUFFER_TOO_SMALL` (0xC0000023). Never produced by the driver logic itself; listed for the
/// glue and the tests (it is what WDF returns from `WdfRequestRetrieve*` for short or empty buffers).
pub const STATUS_BUFFER_TOO_SMALL: NtStatus = nt(0xC000_0023);
/// `STATUS_UNKNOWN_REVISION` (0xC0000058): the unreachable `transferLength <= 0` branch of the C reader
/// setup. Kept for completeness; no code path returns it.
pub const STATUS_UNKNOWN_REVISION: NtStatus = nt(0xC000_0058);
/// `STATUS_NOT_SUPPORTED` (0xC00000BB): unknown IOCTL, unknown `GET_FEATURE` report ID.
pub const STATUS_NOT_SUPPORTED: NtStatus = nt(0xC000_00BB);
/// `STATUS_INVALID_DEVICE_STATE` (0xC0000184): unknown PID, no interrupt pipe, no HID descriptor.
pub const STATUS_INVALID_DEVICE_STATE: NtStatus = nt(0xC000_0184);
/// `STATUS_INVALID_BUFFER_SIZE` (0xC0000206): short `GET_STRING` input, short `GET_FEATURE` input/report
/// buffer (checks made by the driver itself).
pub const STATUS_INVALID_BUFFER_SIZE: NtStatus = nt(0xC000_0206);
/// `STATUS_NO_MORE_ENTRIES` (0x8000001A): what `WdfIoQueueRetrieveNextRequest` returns on an empty
/// queue. Listed for the glue and the tests.
pub const STATUS_NO_MORE_ENTRIES: NtStatus = nt(0x8000_001A);

/// `NT_SUCCESS(status)`: true for success and informational statuses (`status >= 0`).
#[must_use]
pub const fn nt_success(status: NtStatus) -> bool {
    status >= 0
}

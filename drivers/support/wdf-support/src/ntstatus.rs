//! NTSTATUS values used by the AmtPtp drivers.
//!
//! wdk-sys 0.5.2 already defines every `STATUS_*` constant (hand-written in its
//! `constants.rs`, unconditionally for KMDF and UMDF), so nothing is defined
//! here. This module re-exports the set the four C drivers use (every
//! `STATUS_*` name in `src/**/*.c|h`, plus a few common ones) and checks the
//! values against `ntstatus.h` (SDK 10.0.28000.0) at compile time.
//!
//! `NT_SUCCESS`, `NT_INFORMATION`, `NT_WARNING`, `NT_ERROR` are `const fn`s in
//! wdk-sys and are re-exported too.

pub use wdk_sys::{
    NT_ERROR, NT_INFORMATION, NT_SUCCESS, NT_WARNING, NTSTATUS, STATUS_BUFFER_OVERFLOW,
    STATUS_BUFFER_TOO_SMALL, STATUS_CANCELLED, STATUS_DATA_ERROR, STATUS_DEVICE_DATA_ERROR,
    STATUS_DEVICE_NOT_CONNECTED, STATUS_INSUFFICIENT_RESOURCES, STATUS_INVALID_BUFFER_SIZE,
    STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_DEVICE_STATE, STATUS_INVALID_PARAMETER,
    STATUS_INVALID_STATE_TRANSITION, STATUS_IO_TIMEOUT, STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED,
    STATUS_NOT_SUPPORTED, STATUS_PENDING, STATUS_SUCCESS, STATUS_UNKNOWN_REVISION,
    STATUS_UNSUCCESSFUL,
};

/// `(NTSTATUS)0x...L` from `ntstatus.h`: the bit pattern reinterpreted as i32.
const fn hex(value: u32) -> NTSTATUS {
    // Same bit pattern as the C cast (NTSTATUS)0xC0000001L.
    value as NTSTATUS
}

const _: () = {
    assert!(STATUS_SUCCESS == hex(0x0000_0000));
    assert!(STATUS_PENDING == hex(0x0000_0103));
    assert!(STATUS_BUFFER_OVERFLOW == hex(0x8000_0005));
    assert!(STATUS_UNSUCCESSFUL == hex(0xC000_0001));
    assert!(STATUS_NOT_IMPLEMENTED == hex(0xC000_0002));
    assert!(STATUS_INVALID_PARAMETER == hex(0xC000_000D));
    assert!(STATUS_INVALID_DEVICE_REQUEST == hex(0xC000_0010));
    assert!(STATUS_BUFFER_TOO_SMALL == hex(0xC000_0023));
    assert!(STATUS_DATA_ERROR == hex(0xC000_003E));
    assert!(STATUS_UNKNOWN_REVISION == hex(0xC000_0058));
    assert!(STATUS_INSUFFICIENT_RESOURCES == hex(0xC000_009A));
    assert!(STATUS_DEVICE_DATA_ERROR == hex(0xC000_009C));
    assert!(STATUS_DEVICE_NOT_CONNECTED == hex(0xC000_009D));
    assert!(STATUS_IO_TIMEOUT == hex(0xC000_00B5));
    assert!(STATUS_NOT_SUPPORTED == hex(0xC000_00BB));
    assert!(STATUS_CANCELLED == hex(0xC000_0120));
    assert!(STATUS_INVALID_DEVICE_STATE == hex(0xC000_0184));
    assert!(STATUS_INVALID_BUFFER_SIZE == hex(0xC000_0206));
    assert!(STATUS_NOT_FOUND == hex(0xC000_0225));
    assert!(STATUS_INVALID_STATE_TRANSITION == hex(0xC000_A003));
};

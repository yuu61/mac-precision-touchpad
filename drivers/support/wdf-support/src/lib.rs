//! WDF glue shared by the AmtPtp drivers.
//!
//! Built against the KMDF 1.31 bindings in the driver workspace.
//!
//! Contents:
//! - [`init`]: Rust equivalents of the `FORCEINLINE` `WDF_*_INIT` helpers and
//!   `WDF_REL_TIMEOUT_IN_*` (bindgen does not emit inline functions, so wdk-sys
//!   has none of them). The initializers match the WDK for a fixed KMDF version.
//! - [`context`] + [`declare_context_type!`]: `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME`, for `Sync`
//!   context types only.
//! - [`handle`]: [`handle::WdfHandle`] and [`handle::ContextPtr`], the `Sync` forms of the WDF handles
//!   and context pointers a context stores.
//! - [`request`]: [`request::Request`], an owned `WDFREQUEST` that is completed,
//!   forwarded or explicitly released exactly once.
//! - [`sync`]: [`sync::SpinLocked`], a value guarded by a `WDFSPINLOCK`, and [`sync::WaitLock`], a
//!   `WDFWAITLOCK` for `PASSIVE_LEVEL` sequences.
//! - [`wdm`] (KMDF only): `WdfRequestWdmGetIrp`, `IoGetCurrentIrpStackLocation`,
//!   `Irp->UserBuffer`, `Type3InputBuffer`.
//! - [`ntstatus`]: the NTSTATUS values the C drivers use, re-exported from
//!   wdk-sys (which already defines all of them for KMDF) with
//!   compile-time checks against `ntstatus.h`.
//!
//! Everything here is `no_std` and allocation-free, so it is usable from the
//! kernel-mode drivers (which must not use `alloc`).
#![no_std]
#![deny(
    unsafe_op_in_unsafe_fn,
    missing_docs,
    clippy::undocumented_unsafe_blocks,
    clippy::missing_safety_doc,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

#[cfg(not(driver_model__driver_type = "KMDF"))]
compile_error!(
    "wdf-support must be built inside a KMDF driver workspace (drivers/km): \
     no [workspace.metadata.wdk.driver-model] was found"
);

pub mod context;
pub mod handle;
pub mod hid;
pub mod init;
pub mod ntstatus;
pub mod request;
pub mod sync;
pub mod time;
pub mod wdm;

#[doc(hidden)]
pub mod __private;

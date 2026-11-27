//! WDF glue shared by the AmtPtp drivers.
//!
//! This crate is compiled once per driver workspace, against that workspace's
//! `wdk-sys` bindings:
//!
//! | workspace     | framework | cfg set by `build.rs`                      |
//! |---------------|-----------|--------------------------------------------|
//! | `drivers/km`  | KMDF 1.23 | `driver_model__driver_type = "KMDF"`       |
//! | `drivers/um`  | UMDF 2.15 | `driver_model__driver_type = "UMDF"`       |
//!
//! Contents:
//! - [`init`]: Rust equivalents of the `FORCEINLINE` `WDF_*_INIT` helpers and
//!   `WDF_REL_TIMEOUT_IN_*` (bindgen does not emit inline functions, so wdk-sys
//!   has none of them). Bodies follow `Include/wdf/kmdf/1.23` and
//!   `Include/wdf/umdf/2.15` field by field.
//! - [`context`] + [`declare_context_type!`]: `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME`.
//! - [`request`]: [`request::Request`], an owned `WDFREQUEST` that is completed,
//!   forwarded or explicitly released exactly once.
//! - [`sync`] (KMDF only): [`sync::SpinLocked`], a value guarded by a `WDFSPINLOCK`.
//! - [`wdm`] (KMDF only): `WdfRequestWdmGetIrp`, `IoGetCurrentIrpStackLocation`,
//!   `Irp->UserBuffer`, `Type3InputBuffer`.
//! - [`ntstatus`]: the NTSTATUS values the C drivers use, re-exported from
//!   wdk-sys (which already defines all of them for KMDF and UMDF) with
//!   compile-time checks against `ntstatus.h`.
//!
//! Everything here is `no_std` and allocation-free, so it is usable from the
//! kernel-mode drivers (which must not use `alloc`) as well as from UMDF.
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

#[cfg(not(any(driver_model__driver_type = "KMDF", driver_model__driver_type = "UMDF")))]
compile_error!(
    "wdf-support must be built inside a KMDF or UMDF driver workspace (drivers/km or drivers/um): \
     no [workspace.metadata.wdk.driver-model] was found"
);

pub mod context;
#[cfg(driver_model__driver_type = "KMDF")]
pub mod hid;
pub mod init;
pub mod ntstatus;
pub mod request;
#[cfg(driver_model__driver_type = "KMDF")]
pub mod sync;
pub mod time;
#[cfg(driver_model__driver_type = "KMDF")]
pub mod wdm;

#[doc(hidden)]
pub mod __private;

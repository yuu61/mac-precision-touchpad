//! Emits `cfg(driver_model__driver_type = "KMDF" | "UMDF")` for this crate.
//!
//! `wdk_build::configure_wdk_library_build` walks up from `OUT_DIR` to the first
//! `Cargo.lock` (= the driver workspace that is building us: `drivers/km` or
//! `drivers/um`), reads its `[workspace.metadata.wdk.driver-model]` through
//! `cargo metadata`, and prints the same `cargo::rustc-cfg` / `rustc-check-cfg`
//! directives that wdk-sys itself gets. Built outside a driver workspace it only
//! emits the check-cfg list, and `src/lib.rs` then stops with a `compile_error!`.

fn main() -> Result<(), wdk_build::ConfigError> {
    wdk_build::configure_wdk_library_build()
}

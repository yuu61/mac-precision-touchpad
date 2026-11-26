//! Configures the KMDF link (WDK library paths, `/DRIVER /ENTRY:FxDriverEntry`,
//! `WdfDriverEntry.lib`, ...) from the workspace's `[workspace.metadata.wdk]`.
//! The C project has no `.rc` file, so the image carries no VERSIONINFO.

fn main() -> Result<(), wdk_build::ConfigError> {
    wdk_build::configure_wdk_binary_build()
}

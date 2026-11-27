//! Configures the KMDF link (WDK library paths, `/DRIVER /ENTRY:FxDriverEntry`,
//! `WdfDriverEntry.lib`, ...) from the workspace's `[workspace.metadata.wdk]`.
//! Compiles and links the driver VERSIONINFO resource.

fn main() -> Result<(), Box<dyn core::error::Error>> {
    wdk_build::configure_wdk_binary_build()?;
    rc_embed::compile_and_link("Resource.rc")?;
    Ok(())
}

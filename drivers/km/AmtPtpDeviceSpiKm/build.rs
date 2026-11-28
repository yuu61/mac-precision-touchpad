//! Configures the KMDF link (WDK library paths, `/DRIVER /ENTRY:FxDriverEntry`,
//! `WdfDriverEntry.lib`, ...) from the workspace's `[workspace.metadata.wdk]`.
//! Generates and links the driver VERSIONINFO resource.

fn main() -> Result<(), Box<dyn core::error::Error>> {
    wdk_build::configure_wdk_binary_build()?;
    rc_embed::embed_driver_version(
        "AmtPtpDeviceSpiKm.sys",
        "Apple Precision Touchpad SPI Kernel Mode Driver",
    )?;
    Ok(())
}

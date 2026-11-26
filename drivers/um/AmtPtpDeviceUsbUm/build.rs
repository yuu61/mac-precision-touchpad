//! Configures the UMDF link (WDK library paths, `WdfDriverStubUm.lib`, ...) from
//! the workspace's `[workspace.metadata.wdk]`, then compiles `Resource.rc`
//! (ported byte-for-byte from the C project, UTF-16LE) and links the
//! VERSIONINFO into the DLL.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    wdk_build::configure_wdk_binary_build()?;
    rc_embed::compile_and_link("Resource.rc")?;
    Ok(())
}

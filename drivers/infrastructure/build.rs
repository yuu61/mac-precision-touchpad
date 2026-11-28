//! Configures the KMDF link and embeds VERSIONINFO for the selected driver.

mod version;

fn main() -> Result<(), Box<dyn core::error::Error>> {
    let (filename, description) = match env!("CARGO_PKG_NAME") {
        "amtptp-device-usb-km" => (
            "AmtPtpDeviceUsbKm.sys",
            "Apple Precision Touchpad Kernel Mode Driver",
        ),
        "amtptp-device-spi-km" => (
            "AmtPtpDeviceSpiKm.sys",
            "Apple Precision Touchpad SPI Kernel Mode Driver",
        ),
        "amtptp-hid-filter" => (
            "AmtPtpHidFilter.sys",
            "Apple Precision Touchpad HID Filter Driver",
        ),
        "amtptp-bth-filter" => (
            "AmtPtpBthFilter.sys",
            "Apple Precision Touchpad Bluetooth Filter Driver",
        ),
        name => return Err(format!("unknown driver package {name}").into()),
    };
    println!("cargo::rerun-if-changed=Cargo.toml");
    wdk_build::configure_wdk_binary_build()?;
    version::embed_driver_version(filename, description)
}

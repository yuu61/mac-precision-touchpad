# Windows Precision Touchpad driver for Apple trackpads

[![Rust CI](https://github.com/yuu61/mac-precision-touchpad/actions/workflows/rust-ci.yml/badge.svg?branch=main)](https://github.com/yuu61/mac-precision-touchpad/actions/workflows/rust-ci.yml)

This Rust driver adds Windows Precision Touchpad support for Apple Magic Trackpad 2 and supported
Intel MacBook trackpads over USB, SPI, or Bluetooth. It targets Windows 10 21H2 (build 19044,
including LTSC 2021) and later on x64 and ARM64.

## Apple Boot Camp

Apple provides Windows support software for compatible Intel Macs. See [Apple's Boot Camp instructions](https://support.apple.com/en-us/102622).

## Issues

Report bugs and hardware support requests in [GitHub Issues](https://github.com/yuu61/mac-precision-touchpad/issues).
Include your Windows build, architecture, hardware ID, and connection type.

## See it in action (YouTube video)

[![Watch the video](https://img.youtube.com/vi/-GWlfw7omdo/hqdefault.jpg)](https://youtu.be/-GWlfw7omdo)

## Driver packages and installation

Build the Rust package for your architecture using the [driver build commands](#driver-build-and-tests).
The CI workflow builds unsigned packages for validation. Prepare the test machine for test signing,
then sign the local package before installation. [Microsoft's test-signing guide](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/test-signing) · [Package signing commands](docs/rust-port/packaging.md#signing)

1. Uninstall Trackpad++ if it is installed for the device.
2. Open an elevated terminal in the signed package directory and run
   `pnputil /add-driver .\AmtPtpDevice.inf /install`.
3. For a Bluetooth Magic Trackpad 2, pair it in Windows Settings.

To uninstall, run `pnputil /enum-drivers` and find the published `oemNNN.inf` for
`AmtPtpDevice.inf` by matching the provider and driver version. Then run
`pnputil /delete-driver oemNNN.inf /uninstall` in an elevated terminal.

The [upstream releases](https://github.com/imbushuo/mac-precision-touchpad/releases) and
[Chocolatey package](https://community.chocolatey.org/packages/mac-precision-touchpad) distribute
the original C driver.

## For developers

The drivers are written in Rust on top of [windows-drivers-rs](https://github.com/microsoft/windows-drivers-rs) (`wdk-sys` bindings to the Windows Driver Kit).

| Binary | Bus | Framework | Source |
|---|---|---|---|
| `AmtPtpDeviceUsbKm.sys` | USB (Wellspring MacBooks, T2, Magic Trackpad 2) | KMDF 1.31 | `drivers/km/AmtPtpDeviceUsbKm`, `crates/amtptp-usbkm-core` |
| `AmtPtpDeviceSpiKm.sys` | SPI | KMDF 1.31 | `drivers/km/AmtPtpDeviceSpiKm`, `crates/amtptp-spikm-core` |
| `AmtPtpHidFilter.sys` | Bluetooth (Magic Trackpad 2) | KMDF 1.31 | `drivers/km/AmtPtpHidFilter`, `crates/amtptp-hidfilter-core` |
| `AmtPtpBthFilter.sys` | Bluetooth (Magic Trackpad 2), below HidBth | KMDF 1.31 | `drivers/km/AmtPtpBthFilter`, `crates/amtptp-hidfilter-core` (`bth`) |

### Repository layout

| Directory | Contents |
|---|---|
| [`crates/`](crates/) | Hardware-independent `no_std` driver logic, shared Precision Touchpad HID code, and host tests with fixtures recorded from the C drivers. |
| [`drivers/`](drivers/) | WDK driver code. `km/` is the KMDF workspace. `support/` contains shared WDF glue and the Windows resource compiler helper. |
| [`packaging/`](packaging/) | INF templates and the PowerShell helper for creating CAB files. |
| [`tools/`](tools/) | The `xtask` build and packaging utility and the WPR profile in `tracing/` for all four drivers. |
| [`docs/rust-port/`](docs/rust-port/) | Rust port records and the driver packaging guide. |

### Driver build and tests

Use Rust stable with the MSVC target for each architecture you build, the Visual Studio C++ build
tools, the Windows SDK and WDK, and LLVM for bindgen. See the [build requirements](docs/rust-port/packaging.md#requirements).

```
cargo xtask test                                   # host tests and host/KMDF Clippy
cargo xtask build --arch all --release             # build the drivers for x64 and ARM64
cargo xtask package --arch all --release           # unsigned packages under target/package/
```

Each package includes one INF, four drivers, and a catalog. `--driver-version a.b.c.d` sets the
version shared by the INF and all four SYS files. See the [packaging guide](docs/rust-port/packaging.md) for signing and other options.

## Device support

The INF matches specific hardware IDs for Wellspring and T2 USB trackpads, Apple SPI trackpads, and
Magic Trackpad 2 (PID `0265`) over USB and Bluetooth. Check the device's hardware ID in Device
Manager against the [INF model list](packaging/AmtPtpDevice.inx#L29).

## Roadmap

- [x] Touchpad Device Power Management
- [x] Touchpad Protocol Stack Implementation
- [x] Touchpad Raw Input Output (in Driver Trace)
- [x] HID Protocol Implementation (PTP Touch)
- [x] HID Protocol Implementation (Configuration)
- [x] Device Test (Magic Trackpad 2)
- [x] Refine input experience (tip switch detection and pressure)
- [x] Refine input experience (gesture experience)
- [x] More Models
- [x] SPI
- [x] Bluetooth
- [ ] Defuzz
- [ ] Input sensitivity configuration

## Acknowledgements

**UPDATE 2021/05**: She got an M1 Mac now.

People familiar with me know that I don't use Apple MacBook (so development work of this driver occurs on a Surface Pro 4). People probably think it doesn't make sense that I started this project. The motivation behind this project origin from complaints from my girlfriend. Hearing about frequent complains about Dell XPS 13's touchpad (though it passed Precision Touchpad certification), I decided to start this project, so by the time she switches to MacBook Pro, she will have excellent touchpad experience out-of-box, even on Windows. This driver is made for you, but also for everyone.

I would like to thank projects like [magictrackpad2-dkms](https://github.com/robbi5/magictrackpad2-dkms) and [macbook12-spi-driver](https://github.com/cb22/macbook12-spi-driver) that inspire me. I would also like to thank my friends for providing me immense help on devices and testing.
 
## License

- The USB driver, the two Bluetooth filters, and `crates/bcm5974` are licensed under [GPLv2](LICENSE-GPL.md).
- SPI driver is licensed under [MIT](LICENSE-MIT.md).
- The shared building blocks (`crates/ptp-hid`, `drivers/support/wdf-support`, `drivers/support/rc-embed`) are licensed under [MIT](LICENSE-MIT.md).

See [LICENSE.md](LICENSE.md) for the mapping of source directories to licenses.


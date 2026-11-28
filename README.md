# Windows Precision Touchpad Implementation for Apple MacBook family/Magic Trackpad 2

This project implements Windows Precision Touchpad Protocol for Apple MacBook family/Magic Trackpad 2 on Windows 10. Both USB (traditional and T2), SPI and Bluetooth trackpads are supported.

## Donation?

A few people asked me why I removed donation. My current financial situation doesn't require me to accept donation for the budget of future development. If you are considering donation, please consider other meaningful projects and foundations, such as [Electronic Frontier Foundation](https://www.eff.org/).

## Official Driver?

Bootcamp 6.1.5 offers official driver for T2-based model and Magic Trackpad 2. You can also consider using the official driver if you have such Mac. If you have an older model (e.g. MacBooks before 2018/2019), this is the only implementation for these models (yet). 

## Future Plans and feature tracking

Use the [Azure DevOps Board](https://ligstd.visualstudio.com/Apple%20PTP%20Trackpad/_workitems/) to track feature and device support plans. Bug reports should go to Github issues.

## See it in action (YouTube video)

[![Watch the video](https://img.youtube.com/vi/-GWlfw7omdo/hqdefault.jpg)](https://youtu.be/-GWlfw7omdo)

## Converged Installation Guide

**IMPORTANT:** Given changes in Microsoft driver code signing policy, and the compliance need of EV certificate, CI auto builds beyond 2021/01/06 04:00AM EST will not be automatically signed with normal code signing certificate. These builds are still supposed via TestSigning, but not recommended for normal users. Proper WHQL and EV dual-signed packages will be released manually, which can be downloaded from the release page.

0. Make sure you uninstalled `Trackpad++` completely if you have previously does so
1. Go to the release tab in Github and download the newest version for your architecture
2. Right click `AmtPtpDevice.inf` and install it
3. If you have a Magic Trackpad 2 and want to use it in Bluetooth mode: manaully pair it in PC Settings.

**Note: it is unnecessary to enable test signing, or install the certificate manually. Doing so may cause problems in installation. See [this issue](https://github.com/imbushuo/mac-precision-touchpad/issues/228#issuecomment-538689587) for detailed explanation.**

## Also Uninstallation (extremely important for reinstallation `Trackpad++` and such)

See also [here](https://magicutilities.net/magic-trackpad/help/mac-precision-touchpad-driver-installed).

1. Go to device manager
2. Find the "Apple Precision Touch Device", "Apple Multi-touch Trackpad HID filter" and "Apple Multi-touch Auxiliary Services"
3. Right click "remove the device" and also check "uninstall driver"
4. Rescan devices

## Installation with Chocolatey

The drivers are available as a [Chocolatey package](https://chocolatey.org/packages/mac-precision-touchpad/). To install using [Chocolatey](https://chocolatey.org) run:

```
choco install mac-precision-touchpad
```

## For developers

The drivers are written in Rust on top of [windows-drivers-rs](https://github.com/microsoft/windows-drivers-rs) (`wdk-sys` bindings to the Windows Driver Kit).

| Binary | Bus | Framework | Source |
|---|---|---|---|
| `AmtPtpDeviceUsbKm.sys` | USB (Wellspring MacBooks, T2, Magic Trackpad 2) | KMDF 1.23 | `drivers/km/AmtPtpDeviceUsbKm`, `crates/amtptp-usbkm-core` |
| `AmtPtpDeviceSpiKm.sys` | SPI | KMDF 1.23 | `drivers/km/AmtPtpDeviceSpiKm`, `crates/amtptp-spikm-core` |
| `AmtPtpHidFilter.sys` | Bluetooth (Magic Trackpad 2) | KMDF 1.23 | `drivers/km/AmtPtpHidFilter`, `crates/amtptp-hidfilter-core` |
| `AmtPtpBthFilter.sys` | Bluetooth (Magic Trackpad 2), below HidBth | KMDF 1.23 | `drivers/km/AmtPtpBthFilter`, `crates/amtptp-hidfilter-core` (`bth`) |

### Repository layout

| Directory | Contents |
|---|---|
| [`crates/`](crates/) | Hardware-independent `no_std` driver logic, shared Precision Touchpad HID code, and host tests with fixtures recorded from the C drivers. |
| [`drivers/`](drivers/) | WDK driver code. `km/` is the KMDF workspace. `support/` contains shared WDF glue and the Windows resource compiler helper. |
| [`packaging/`](packaging/) | INF templates and the PowerShell helper for creating CAB files. |
| [`tools/`](tools/) | The `xtask` build and packaging utility and the WPR profile in `tracing/` for all four drivers. |
| [`docs/rust-port/`](docs/rust-port/) | Rust port records and the driver packaging guide. |

### Driver build and tests

The minimum OS is Windows 10 1709 (KMDF 1.23). The drivers target x64 and ARM64.

Requirements: Rust (stable, with the `aarch64-pc-windows-msvc` target for ARM64), Visual Studio with the C++ build tools (x64 and ARM64), the Windows Driver Kit, and LLVM (`winget install LLVM.LLVM`, used by bindgen).

```
cargo xtask test                                   # host unit tests + clippy for all workspaces
cargo xtask build --arch all --release             # build the drivers for x64 and ARM64
cargo xtask package --arch x64 --release           # assemble target/package/x64/AmtPtpDevice (unsigned)
cargo xtask package --arch arm64 --release --sign <signtool arguments>
```

See [docs/rust-port/packaging.md](docs/rust-port/packaging.md) for all options, signing, and the INF changes.

## Device support

There is a bring-up issue for certain MacBook Pro and MacBook Air. I am looking into it. If you have such device with Windows installed, please contact me so I can look into the issue remotely (as I don't have many devices).

- [x] Non-Retina MacBook 
- [x] Some non-Retina MacBook Pro (2011 and 2012)
- [x] MacBook Pro with Retina Display (MacBookPro 10,1, MacBookPro10,2 & MacBookPro11,1)
- [x] All recent MacBook Air (Please report if your model is not supported)
- [x] MacBook Pro with Retina Display (2013, 2014, 2015, 13-inch & 15-inch)
- [x] New MacBook (12-inch)
- [x] MacBook Pro 2015, 2016, 2017 (a few SPI devices are in work-in-progress state)
- [x] T2-based devices: MacBook Air 2018, MacBook Pro 2017/2018/2019/2020: Use default fallback, experience might not be optimal. Open a issue if you encountered dead touch regions.
- [x] Magic Trackpad 2 & 3 (USB)
- [x] Magic Trackpad 2 & 3 (Bluetooth connection)

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
- [x] KM Driver cross-cert
- [x] Bluetooth
- [ ] Defuzz
- [ ] Input sensitivity configuration

## Acknowledgements

**UPDATE 2021/05**: She got an M1 Mac now.

People familiar with me know that I don't use Apple MacBook (so development work of this driver occurs on a Surface Pro 4). People probably think it doesn't make sense that I started this project. The motivation behind this project origin from complaints from my girlfriend. Hearing about frequent complains about Dell XPS 13's touchpad (though it passed Precision Touchpad certification), I decided to start this project, so by the time she switches to MacBook Pro, she will have excellent touchpad experience out-of-box, even on Windows. This driver is made for you, but also for everyone.

I would like to thank projects like [magictrackpad2-dkms](https://github.com/robbi5/magictrackpad2-dkms) and [macbook12-spi-driver](https://github.com/cb22/macbook12-spi-driver) that inspire me. I would also like to thank my friends for providing me immense help on devices and testing.
 
## License

- USB drivers, the Magic Trackpad 2 HID filter and `crates/bcm5974` are licensed under [GPLv2](LICENSE-GPL.md).
- SPI driver is licensed under [MIT](LICENSE-MIT.md).
- The shared building blocks (`crates/ptp-hid`, `drivers/support/wdf-support`, `drivers/support/rc-embed`) are licensed under [MIT](LICENSE-MIT.md).

See [LICENSE.md](LICENSE.md) for the mapping of source directories to licenses.


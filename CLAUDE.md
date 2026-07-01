# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Windows driver package that implements the **Windows Precision Touchpad (PTP) HID protocol** for Apple trackpads (MacBook family internal trackpads and the Magic Trackpad 2/3), so Windows treats them as native precision touchpads. Each driver attaches as a **lower filter** on the existing HID/USB stack of the Apple device, intercepts the device's proprietary multitouch reports, and re-presents them to Windows as standard PTP HID reports.

Different Apple devices use different transports, so the package contains several drivers, one per transport, plus a settings app.

## Projects

`AmtPtpDriver.sln` contains all native projects; the C# settings app lives in a separate solution `AmtPtpDevice.Settings.sln`. Sources are under `src/`.

| Project | Type | License | Handles |
|---|---|---|---|
| `AmtPtpDeviceUsbKm` | KMDF kernel driver (`.sys`) | GPLv2 | T2-based MacBook USB trackpads (USB `MI_02`) |
| `AmtPtpDeviceUsbUm` | UMDF user-mode driver (`.dll`, `DynamicLibrary`) | GPLv2 | Traditional/older Mac USB trackpads + Magic Trackpad 2 over USB |
| `AmtPtpDeviceSpiKm` | KMDF kernel driver (`.sys`) | MIT | 2015–2017 MacBook/MacBook Pro internal **SPI** trackpads |
| `AmtPtpHidFilter` | KMDF kernel driver (`.sys`) | — | Magic Trackpad 2 over **Bluetooth** (HID transport) |
| `AmtPtpDeviceUniversalPkg` | `Utility` (no compile) | — | Packages all binaries + INF into the shippable driver package |
| `AmtPtpDevice.Settings` | UWP/C# app | — | User-facing settings; talks to drivers via the device interface GUIDs |

Note the license split: USB drivers are GPLv2, the SPI driver is MIT (see `LICENSE-GPL.md` / `LICENSE-MIT.md`).

## Architecture

**Common per-driver structure.** Every native driver is a WDF driver with the same file layout, so once you understand one you understand them all:

- `Driver.c` — `DriverEntry`, WPP trace init, `EvtDeviceAdd`. Drivers call `WdfFdoInitSetFilter` to attach as a *lower filter* — they don't own power or all I/O.
- `Device.c` — PnP/power callbacks, per-device context (`DEVICE_CONTEXT`).
- `Queue.c` — WDF I/O queues, including the queue that holds the pending PTP read request from the OS.
- `Hid.c` — the PTP side: serves the HID/PTP **report descriptor** and handles HID IOCTLs (device caps, input mode, etc.).
- `Input.c` / `Interrupt.c` / `InputInterrupt.c` — the device side: reads raw multitouch packets from the Apple device and translates them into PTP HID reports that complete the OS's pending read.

**The translation is the core of the project.** Raw Apple finger/contact data → confidence/palm filtering → PTP contact reports. See `AmtPtpDeviceSpiKm/Input.c` and the per-series HID descriptors in `AmtPtpDeviceSpiKm/HID/` (`SpiTrackpadSeries1/2/3.h`) for the SPI path; `AppleDefinition.h` describes the raw packet formats.

**`AmtPtpHidFilter` is special.** Bluetooth Magic Trackpad 2 exposes the touch data on a HID collection that Windows would otherwise route to the inbox HID stack. `Detour.c` reaches into the lower HID transport driver object to detour the Windows HID stack so this filter can claim the input. This is deliberately low-level WDM code; the intentional `_DRIVER_OBJECT::DriverExtension` accesses are annotated with `#pragma warning(suppress: 28175)`.

**Device binding.** Everything is wired together by a single INF: `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf`. It maps Apple USB/SPI/Bluetooth hardware IDs to the right driver as a `LowerFilters` entry, and declares the KMDF/UMDF service installs. The package targets **Windows 11 (build 22000) and newer only** — the inbox HID minidriver references (`MsHidKmdf.inf`, `MsHidUmdf.inf`, `WUDFRD.inf` via `Include`/`Needs`) used to resolve InfVerif error 2084 were introduced in build 22000. Don't reintroduce direct `AddService`/`ServiceBinary` for the inbox HID pass-through drivers; use the documented Include/Needs pattern.

**Settings app IPC.** `AmtPtpDevice.Settings` finds each driver through the `GUID_DEVINTERFACE_*` GUIDs in each project's `Public.h` and exchanges feature reports (e.g. `Mt2BatteryStatusReport`, `PtpUserModeConfReport` under `DataObjects/`).

## Building

The build environment is **installed and working** here: Visual Studio 2026 Community (`C:\Program Files\Microsoft Visual Studio\18\Community`) with the WDK/SDK at `10.0.28000.0`. Driver projects build with toolset `WindowsKernelModeDriver10.0` / `Universal` target platform. KM drivers use KMDF v1.23 (Bluetooth filter v1.15); the USB UM driver uses UMDF v2.15. (clang/IntelliSense "header not found" errors against WDK headers in editor tooling are still noise — the real compiler is MSVC via msbuild.)

`msbuild` is not on `PATH`; run from a VS Developer prompt or use the full path `"C:\Program Files\Microsoft Visual Studio\18\Community\MSBuild\Current\Bin\MSBuild.exe"`.

```sh
# Whole solution (pick a config × platform)
msbuild AmtPtpDriver.sln /p:Configuration=Debug /p:Platform=x64 /p:ApiValidator_Enable=false

# A single driver project
msbuild src/AmtPtpDeviceSpiKm/AmtPtpDeviceSpiKm.vcxproj /p:Configuration=Debug /p:Platform=x64 /p:ApiValidator_Enable=false
```

- **`/p:ApiValidator_Enable=false` is required to get a clean build in this environment.** Without it the driver still compiles, runs Code Analysis, and produces a test-signed `.sys`, but the final `ApiValidator.exe` post-build step crashes (`aitstatic` exits 193 / `ERROR_BAD_EXE_FORMAT`) and fails the whole build (MSB3721). That is a WDK tooling glitch, not a code defect — disabling ApiValidator yields a clean `exit 0`.
- Configurations: `Debug`, `Release`, `ReleaseSigned`. Platforms: `x64`, `ARM64`.
- `ReleaseSigned` is reserved for production/signing infrastructure; building it locally yields an effectively unsigned package — use `Release` for local release builds.
- The compiling `.vcxproj` files no longer pin a specific `WindowsTargetPlatformVersion`; they fall through to `$(LatestTargetPlatformVersion)` (resolves to `10.0.28000.0` here). The old dormant `BuildEnvironment=Github` override that pinned `10.0.19041.0` was removed during the Win11 modernization — **don't reintroduce a pre-22000 SDK pin**: build 22000 introduced the inbox HID sections the package INF `Needs`, so an older SDK would resurrect InfVerif error 2084.
- The settings app builds from its own solution: `msbuild AmtPtpDevice.Settings.sln`.

## Static analysis & warnings

Two shared MSBuild property sheets are imported by each compiling `.vcxproj` (via its `PropertySheets` ImportGroup, **before** `Microsoft.Cpp.targets`):

- `src/CompilerWarnings.props` — enforces `/W4` + `/WX` (warnings are errors). `/W4` is the deliberate ceiling: `/Wall` drowns in noise from WDK system headers. Per-project `DisableSpecificWarnings=4214` suppresses the WPP-generated non-int bitfield warning in `UsbKm`/`SpiKm`.
- `src/CodeAnalysis.props` — enables PREfast (`RunCodeAnalysis` + `EnablePREfast`) on every build. In a WDK environment the driver projects get the Microsoft Driver Recommended Rules (memory-safety, leaks, IRQL). `CodeAnalysisRuleSet` is intentionally left unset so the WDK integration picks the ruleset. PREfast findings are warnings (not `/WX`) and do not fail the build.

Both files carry detailed rationale in their comments — read them before changing warning policy. For driver certification, **CodeQL** is now the primary required path (SDV is removed; the legacy Code Analysis for Drivers is retiring). CodeQL is *required* for the three kernel-mode drivers (`UsbKm`, `SpiKm`, `HidFilter`); run it in a WDK build environment with `tools/Run-DriverCodeQL.ps1`, which builds a CodeQL database per KM driver, runs the must-fix suite, and generates a per-project DVL for HLK submission. (That script encodes the documented MS commands but hasn't been executed here — no CodeQL CLI in this environment.)

## Testing

There is no unit-test suite — these are kernel/user-mode drivers. Verification means deploying the package to a target machine with the Apple hardware and observing behavior, using **WPP software tracing** (each driver has a `Trace.h` and emits `TraceEvents(...)`; capture with TraceView/`tracelog` against the generated `.tmh` GUIDs).

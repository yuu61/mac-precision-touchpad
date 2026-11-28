# Building, packaging and signing the Rust drivers

The four drivers are built by Cargo and packaged by `cargo xtask` (`tools/xtask/`, std only, no external
dependencies). This replaces `AmtPtpDriver.sln`, the driver `.vcxproj` files and
`AmtPtpDeviceUniversalPkg.vcxproj`.

## Requirements

- Rust stable with the `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` targets
  (`rustup target add aarch64-pc-windows-msvc`).
- Visual Studio Build Tools (MSVC linker, x64 and ARM64).
- Windows SDK + WDK 10.0.28000 (or newer), including stampinf, Inf2Cat and InfVerif. An
  installed WDK and an eWDK prompt both work.
  - Kit root, in this order: `WDK_ROOT`; `WDKContentRoot`; `MicrosoftKitRoot\Windows Kits\
    <WDKKitVersion, default 10.0>`; the registry value
    `HKLM\SOFTWARE\Microsoft\Windows Kits\Installed Roots\KitsRoot10` (64-bit, then 32-bit
    view); `C:\Program Files (x86)\Windows Kits\10`. A `WDKContentRoot` or `MicrosoftKitRoot`
    that is not an existing directory is skipped with a warning.
  - Kit version, in this order: `WDK_VERSION`; `Version_Number` (only when `WDK_ROOT` is unset,
    because it describes the shell's kit); the highest `bin\10.*` that has `stampinf.exe`.
  - Which variable affects what:
    - `WDKContentRoot`, `MicrosoftKitRoot`, the registry and `Version_Number` are what
      wdk-build 0.6.0 uses (`detect_wdk_content_root`, `detect_windows_sdk_version`). They pick
      the kit for the driver build **and** for the packaging tools, so by default both use the
      same kit. An eWDK or NuGet shell, which sets `WDKContentRoot` and `Version_Number` and has
      no KitsRoot10, needs no extra settings.
    - `WDK_ROOT` / `WDK_VERSION` are xtask-only overrides. They pick stampinf, Inf2Cat,
      InfVerif and signtool, **not** the kit the binaries are compiled against; wdk-build never
      reads them.
    - xtask does not export `WDKContentRoot` / `Version_Number` to the driver builds to make
      `WDK_ROOT` steer them too. wdk-build emits no `rerun-if-env-changed`, so a changed value
      would leave stale wdk-sys bindings and link paths in `drivers/km/target`.
  - Tool locations in WDK 10.0.28000: stampinf is in `bin\<ver>\x64`, Inf2Cat only in
    `bin\<ver>\x86`, InfVerif in `Tools\<ver>\x64`, signtool in `bin\<ver>\x64`.
- LLVM (libclang) for wdk-sys's bindgen. When `LIBCLANG_PATH` is unset and
  `C:\Program Files\LLVM\bin` exists, xtask sets `LIBCLANG_PATH` to that directory for the driver
  builds. Set `LIBCLANG_PATH` yourself if LLVM is installed elsewhere. MSYS2 can put an
  unrelated libclang on PATH.

## Commands

Run these from the repository root. `.cargo/config.toml` defines the `cargo xtask` alias.

```text
cargo xtask build   [--arch x64|arm64|all] [--release]
cargo xtask package [--arch x64|arm64|all] [--release]
                    [--driver-version a.b.c.d] [--sign <signtool sign args...>]
cargo xtask test
cargo xtask help
```

- `--arch`: defaults to the host architecture. `all` means x64 and arm64.
- `--release`: uses the release profile. The default is the dev profile.
- `build`: runs `cargo build --target <triple> [--release]` in `drivers/km` (KMDF: UsbKm, SpiKm,
  HidFilter and BthFilter).
  - The binaries go to `drivers/km/target/<triple>/<debug|release>/<Name>.dll` and `.pdb`.
  - The target dir must stay inside each driver workspace, because wdk-build walks up from
    OUT_DIR to the first `Cargo.lock`. `RUSTFLAGS`-style variables would replace the workspace's
    `+crt-static` rustflags. So xtask removes `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`,
    `CARGO_BUILD_TARGET`, `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS` and `CARGO_BUILD_RUSTFLAGS`
    from the driver builds' environment.
- `package`: builds the KMDF workspace first. There is one package, `packaging/AmtPtpDevice.inx`
  with all four drivers (see [One INF](#one-inf)). Then, for each arch:
  1. It empties `target/package/<arch>/AmtPtpDevice/` and `target/package/<arch>/AmtPtpDevice.symbols/`,
     so no stale `.cat`, `.inf` or `.pdb` is left. It also removes, when they exist, the shared
     `target/package/<arch>/symbols/` directory and the standalone `AmtPtpHidFilter/` and
     `AmtPtpHidFilter.symbols/` package directories of older xtask versions.
  2. It copies the binaries. KMDF `<Name>.dll` becomes `<Name>.sys`, because the INF
     lists those filenames in its `[SourceDisksFiles]` section. The matching PDBs go to
     `target/package/<arch>/AmtPtpDevice.symbols/`.
  3. It writes `packaging/AmtPtpDevice.inx` as `AmtPtpDevice.inf` with CRLF line endings. The
     `.inx` is LF in git.
  4. It runs `stampinf -f <inf> -d * -a amd64|arm64 -c AmtPtpDevice.cat -v <version|*> -k 1.31`.
     The KMDF version comes from the workspace metadata in `drivers/km/Cargo.toml`.
  5. It checks the stamped file: no `$KEYWORD$` left, `DriverVer` and `CatalogFile` set, and every
     `KmdfLibraryVersion` value as requested.
  6. It runs `InfVerif /w <inf>` on the entire stamped INF. A Windows Driver validation error stops
     packaging before signing or catalog creation.
  7. With `--sign`, it signs the binaries.
  8. It runs `Inf2Cat /driver:<dir> /os:<list> /uselocaltime` and checks that the `.cat` exists.
     Inf2Cat writes the catalog name in lower case; xtask renames it to the `CatalogFile`
     spelling.
     - `/uselocaltime` is needed because `stampinf -d *` writes the local date, while Inf2Cat
       checks in UTC by default. East of UTC it would otherwise reject a fresh DriverVer as
       future-dated.
     - OS list for x64: `10_VB_X64,ServerFE_X64,10_CO_X64,10_NI_X64,10_GE_X64,Server2025_X64`.
     - OS list for arm64: `10_VB_ARM64,10_CO_ARM64,10_NI_ARM64,10_GE_ARM64`.
     - `10_VB` includes build 19044. The models decoration controls the exact minimum
       installable build. The ARM64 catalog targets client releases.
  9. With `--sign`, it signs the `.cat`.
  - Any failing step stops the run with a message naming the tool, its exit code and the full
    command line (with signtool's `/p` value redacted). The exit status is non-zero.
- `--driver-version a.b.c.d`: the DriverVer version, with each part from 0 to 65535. The default
  is stampinf's `*`, which is the current time `h.m.s.ms`. The DriverVer date is always the
  current date.
- `test`: runs these steps, all of them, then reports every failure:
  - `cargo test --workspace` in the host workspace (core crates and xtask);
  - Clippy on all host targets, including integration tests.
  - Clippy in the driver workspace via `clippy-driver` as the compiler wrapper.
    This also checks the shared `wdf-support` and `rc-embed` crates for KMDF.

  [`.cargo/config.toml`](../../.cargo/config.toml) enables the `all`, `pedantic`, `nursery`, and
  `restriction` groups and records the reasons for individual exceptions. The Cargo manifests
  enable the `cargo` group and check dependency versions at the workspace level.
  [`clippy.toml`](../../clippy.toml) configures the lints, including allowances for test
  assertions. Warnings fail `cargo xtask test`. The strict profile was verified with Clippy 1.100.
  Fix lint findings in the code. For an unavoidable local exception, use
  `#[expect(lint, reason = "...")]` where the lint reliably fires. Document recurring
  exceptions once in the shared configuration.

### Package layout

```text
target/package/<x64|arm64>/
  AmtPtpDevice/              AmtPtpDevice.inf  AmtPtpDevice.cat
                             AmtPtpDeviceUsbKm.sys
                             AmtPtpDeviceSpiKm.sys  AmtPtpHidFilter.sys
                             AmtPtpBthFilter.sys
  AmtPtpDevice.symbols/      AmtPtpDeviceUsbKm.pdb
                             AmtPtpDeviceSpiKm.pdb  AmtPtpHidFilter.pdb
                             AmtPtpBthFilter.pdb
```

- The package directory contains the INF, CAT and binaries. Pass the directory to
  `New-CabinetFile -ContentDirectory` after dot-sourcing [the CAB script](../../packaging/New-CabinetFile.ps1).
- The directory does not record the profile. A dev-profile `package` overwrites a release one.
  Use `--release` for anything you distribute.
- The `.symbols` directory is emptied together with the package, so its PDBs always match the
  package's binaries (every link stamps a new PDB GUID).
- The PDBs keep their cargo names. The debugger matches on the PDB GUID, not on the image name,
  so `AmtPtpDeviceUsbKm.pdb` serves `AmtPtpDeviceUsbKm.sys`.

### Signing

- The packages are unsigned by default. xtask never creates certificates.
- `--sign` passes **everything after it** to `signtool sign`, so `--sign` must be the last
  option. xtask adds `/fd sha256` when the arguments contain no `/fd`. The C build's ARM64 driver
  projects lacked it; see crosscut C3 and D.
- It signs the binaries before Inf2Cat and the catalog after it.
- Run `--sign` from PowerShell or cmd, or from Git Bash with `MSYS2_ARG_CONV_EXCL='*'`.
  Otherwise MSYS rewrites slash options such as `/f` and `/fd` into drive paths before xtask
  sees them.
- Examples:
  - Production, the old `ReleaseSigned` configuration:
    `cargo xtask package --arch all --release --sign /sha1 <thumbprint> /tr http://timestamp.digicert.com /td sha256`.
    Microsoft must attestation-sign or WHQL-sign a package for Windows 10 1607+ with Secure Boot.
    An EV signature from `--sign` is the input to that submission, not a replacement for it.
  - Test signing, the old Debug/Release `TestSign`, with a certificate you created yourself and
    imported into the certificate store:
    `cargo xtask package --release --sign /sha1 <thumbprint>`.
- Passwords: prefer `/sha1 <thumbprint>` with a store certificate over `/f <pfx> /p <password>`.
  xtask prints every command it runs and repeats it in error messages. For signtool it shows the
  argument after `/p` or `-p` (exact match, case-insensitive; `/ph`, `/p7`, `/p7co`, `/p7ce` are
  not secrets) as `<redacted>`. That only covers xtask's own output: with `/p` the password is
  still on the command lines of the cargo, xtask and signtool processes and in shell history.
  The MSBuild DriverSign flow took the certificate from a project property and passed no
  password.

## Framework version and OS floor

The four drivers target KMDF 1.31 starting at Windows 10 21H2 (build 19044). KMDF 1.31 was first
available in Windows 10 2004, which also introduced the Bluetooth filter's `ExAllocatePool2`
dependency. [KMDF version history](https://learn.microsoft.com/en-us/windows-hardware/drivers/wdf/kmdf-version-history)

Each architecture's INF has two models sections with the same
hardware IDs. Both bind every supported USB device to `AmtPtpDeviceUsbKm`, including Wellspring 8
and the USB Magic Trackpad 2.

- From build 19044 through 21999, USB, SPI and Bluetooth `Col01` install the inbox
  `mshidkmdf.sys` function driver using the package's legacy service declaration.
- Starting with build 22000, those three stacks use `Include = MsHidKmdf.inf` and the matching
  `MsHidKmdf.NT`, `.NT.HW` and `.NT.Services` sections. Windows supplies the function service, and
  this package installs the corresponding KMDF lower filter. Microsoft introduced the inbox INF
  in build 22000. [Microsoft KMDF HID sample](https://github.com/microsoft/Windows-driver-samples/blob/main/hid/vhidmini2/driver/kmdf/vhidmini.inx)

### Changes since KMDF 1.23

| Change | How it applies to this package |
| --- | --- |
| 1.25 introduced an optional lower runtime version for a driver built with a newer framework. Microsoft lists no further changes for 1.27 and 1.29. | 1.31 is both the target and the runtime requirement. The Rust WDF initializers can use the compile-time structure sizes. [WDF version targeting](https://learn.microsoft.com/en-us/windows-hardware/drivers/wdf/building-a-wdf-driver-for-multiple-versions-of-windows) |
| 1.25 added `WdfDriverOpenPersistentStateRegistryKey` for driver-wide state written at runtime. | The drivers keep their runtime state in WDF object contexts. [Persistent state key](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdfdriver/nf-wdfdriver-wdfdriveropenpersistentstateregistrykey) |
| 1.31 made `WdfDeviceGetSystemPowerAction` more accurate for the power policy owner. | USB, SPI and the Bluetooth `Col01` filter now record the framework's `POWER_ACTION` as `SystemPowerAction` in their D0 entry and exit ETW events. The filters use it for diagnostics. [Power action API](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdfdevice/nf-wdfdevice-wdfdevicegetsystempoweraction) |
| `WdfDeviceSetDeviceInterfaceStateEx` can control a device interface before PnP start. `WdfPdoInitRemovePowerDependencyOnParent` can decouple a child PDO. Directed PoFx depends on an appropriate power policy. | HIDCLASS publishes the HID interfaces. These drivers filter existing devices, while the underlying stacks own their PDOs and power policy. [Interface state](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdfdevice/nf-wdfdevice-wdfdevicesetdeviceinterfacestateex) · [Child PDO power](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdfpdo/nf-wdfpdo-wdfpdoinitremovepowerdependencyonparent) · [Directed PoFx](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/introduction-to-the-directed-power-management-framework) |
| Windows 10 1903 introduced declarative filter registration in INFs. | All four services use `AddFilter` with `FilterPosition = Lower`. The function drivers come from the same Microsoft services selected above. [AddFilter directive](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/inf-addfilter-directive) |

## One INF

`packaging/AmtPtpDevice.inx` is the only INF template. It installs all four KMDF drivers and binds AmtPtpHidFilter
to the Bluetooth Magic Trackpad 2 collections (`Col01` filter, `Col02` null device).

The package includes `AmtPtpBthFilter`, a lower
filter on the Bluetooth HID device node (`BTHENUM\{00001124-...}_VID&0001004c_PID&0265`). Its
`AmtPtpBthFilter_Device` sections pull in the inbox `hidbth.inf`/`input.inf` sections (`Include`/
`Needs`) and register the lower filter with `AddFilter`. The vendor collection it creates, `Col03`, is bound to the null
device. See [bthfilter.md](bthfilter.md#inf).

The port first also kept the standalone `packaging/AmtPtpHidFilter.inx` (the C
`src/AmtPtpHidFilter/AmtPtpHidFilter.inf`, never part of a release). It was removed on 2026-11-27
(step 1 of the structural improvement audit of the Rust port):

- it claimed the USB Magic Trackpad 2 (`USB\Vid_05ac&Pid_0265&MI_01`, and the filter on its HID
  collections) against the universal INF's UsbUm binding (crosscut C2);
- its USB hardware IDs hard-coded `REV_0855`, so they missed other firmware revisions such as
  `REV_0871`;
- the universal INF already contained the HidFilter install sections for the Bluetooth trackpad.

`cargo xtask package` therefore builds one package and has no `--inf` option any more. The INF
changes below that name HidFilter describe that removed template.

## Historical INF changes before USB consolidation

The following comparison and verification records describe the original port. Step 3 replaced both USB install paths with the single KMDF binding described above.

- `packaging/AmtPtpDevice.inx` is `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf` (and the removed
  `packaging/AmtPtpHidFilter.inx` was `src/AmtPtpHidFilter/AmtPtpHidFilter.inf`), with only the
  changes below. Line endings are LF, per `.gitattributes`.
- **Unchanged (P1):**
  - every hardware ID, including the duplicate USB `0273/0274/0290/0291&MI_02` bindings to both
    UsbKm and UsbUm and SPI `0280`, which has no device-table entry;
  - every install, service, `.Wdf`, AddReg and filter section;
  - every service name and flag;
  - every string;
  - the two-INF split, in which the universal and HidFilter INFs claim the MT2 USB device in
    conflicting ways (crosscut C2), until the HidFilter INF was removed (see [One INF](#one-inf)).
- `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf` and the models body of the `...17134` section
  were diffed mechanically. They are byte-identical apart from line endings.

| # | INF | Change | Why |
|---|---|---|---|
| 1 | both | `DriverVer` is left empty in the `.inx` and stamped by stampinf (`-d * -v <ver or *>`) | Required. It was stamped by the MSBuild StampInf task before. HidFilter's original `DriverVer= ; TODO: set DriverVer in stampinf property pages` line is kept verbatim; stampinf replaces the whole line. |
| 2 | universal | `UmdfLibraryVersion=2.15.0` becomes `UmdfLibraryVersion=$UMDFVERSION$` | Required. stampinf `-u 2.15.0` writes the same value back, so the stamped INF is unchanged here. The value now comes from `drivers/um/Cargo.toml`. `$KMDFVERSION$` (three wdfsects in universal, one in HidFilter) was already a keyword and is stamped `-k 1.23`. |
| 3 | universal | `[Manufacturer] %ManufacturerName%=Standard,NT$ARCH$` becomes `Standard,NT$ARCH$.10.0...16299,NT$ARCH$.10.0...17134`, with two models sections. `[Standard.NT$ARCH$.10.0...17134]` is the original `[Standard.NT$ARCH$]` body, byte-identical and in the same order. `[Standard.NT$ARCH$.10.0...16299]` is the same body minus the 27 `AmtPtpDeviceUsbUm_Install` lines. | InfVerif `/u` errors, see the text below. Without a decoration: `ERROR(1199) ... The syntax 'DIRID 13 (CopyFiles)' was introduced in OS version 10.0.16299, but DDInstall sections utilizing the syntax will install on earlier OS versions. Those DDInstall sections should be restricted to only install on 10.0.16299 or higher using a TargetOSVersion decoration.` With only `...16299`, this error remains: `ERROR(1199) ... The syntax 'DIRID 13 (UMDF)' was introduced in OS version 10.0.17134, but DDInstall sections utilizing the syntax will install on earlier OS versions. Those DDInstall sections should be restricted to only install on 10.0.17134 or higher using a TargetOSVersion decoration.` |
| 4 | HidFilter | `NT$ARCH$.10.0` becomes `NT$ARCH$.10.0...16299` in `[Manufacturer]` and in the models section name | InfVerif `/u` on the stamped original (`NTamd64.10.0`): `ERROR(1199) ... The syntax 'DIRID 13 (CopyFiles)' was introduced in OS version 10.0.16299, but DDInstall sections utilizing the syntax will install on earlier OS versions. ...`, exit code 1627. KMDF 1.23 already requires 1709. |
| 5 | universal | Comment lines (`;`) at `[Manufacturer]` and in the `...16299` section explain the split | Documentation only. INF comments have no install semantics. |

### Why change 3 has two decorations and not one

- Windows picks the single models section with the highest decoration that is less than or equal
  to the running build.
- On **10.0.17134+ (1803+)** the `...17134` section applies. It holds the complete original
  binding list, so install semantics there are exactly those of the C INF, including the duplicate
  IDs and their order: UsbKm lines first, then UsbUm.
- On **10.0.16299 to 17133 (1709 only)** the `...16299` section applies.
  - It has no UsbUm entries. Wellspring `MI_01`, MT2 USB `0265&MI_01` and USB `0272&MI_02` are
    not matched there.
  - The duplicate IDs `0273/0274/0290/0291&MI_02` bind only to UsbKm.
  - This is not a regression. Microsoft Learn says: "The UMDF platform will fail to load the main
    UMDF driver binaries from any location other than the %SystemRoot%\System32\Drivers\Umdf
    directory or, in Windows 10 1803 and later, a run from Driver Store location" ("Restricting
    the loading location of UMDF drivers").
  - The C INF installs UsbUm with `ServiceBinary=%13%\AmtPtpDeviceUsbUm.dll`, so on 1709 its UsbUm
    install could not load either.
- **Rejected alternative: a single `...17134` decoration.** It also passes InfVerif, but it would
  raise the floor of the three KMDF drivers (T2 USB, SPI, MT2 Bluetooth) from 1709 to 1803 for
  no technical reason.
- **Rejected alternative: moving UMDF to `12,UMDF`.** It would change the install location (P1).

### InfVerif mode in the original port

- The original packaging pipeline ran **`InfVerif /u` (Universal)**.
- The C projects all had `DriverTargetPlatform Universal`, which is what the MSBuild InfVerif
  step used.
- `/w` (Windows Driver, DCH) and `/h` (WHQL) would require an install-semantics change. Under
  those modes, InfVerif reports five `2084` errors (`mshidkmdf` from the HidFilter, UsbKm and
  SpiKm `.Services` sections, plus `WUDFRd` and `mshidumdf` from UsbUm's; the removed HidFilter INF
  added a sixth, its own `mshidkmdf`):

  ```text
  ERROR(2084) ... line N: Service binary 'C:\WINDOWS\System32\Drivers\mshidkmdf.sys' should reference a CopyFiles destination file.
  ERROR(2084) ... Service binary 'C:\WINDOWS\System32\drivers\WUDFRd.sys' ...
  ERROR(2084) ... Service binary 'C:\WINDOWS\System32\drivers\mshidumdf.sys' ...
  ```

  They come from the INF's own service-install sections for the inbox `mshidkmdf`, `mshidumdf`
  and `WUDFRd` services. Clearing them means replacing those sections with `Include`/`Needs` of
  the inbox INFs, or dropping them, which changes how the function driver and the reflector are
  installed. P1 forbids that.
- Under `/u` the same findings are `WARNING(2084)`. They are accepted, as they were for the C
  package. With changes 1 to 3, `/u` reports no errors.
- The current pipeline uses `/w` with the [build-specific install sections](#framework-version-and-os-floor).

## The old MSBuild / Azure DevOps pipeline no longer applies

- The C drivers were built by `AmtPtpDriver.sln` and the `.vcxproj` files, with MSBuild's
  StampInf, Inf2Cat and DriverSign tasks.
- Release builds came from an external Azure DevOps pipeline, the README build badge
  (`ligstd.visualstudio.com`, definitionId=22). It hooked into the vcxprojs through
  `$(BuildEnvironment)=='AzurePipeline'` and `$(PipelineReleaseID)`, the latter giving the
  `2021.<id>.1.1000` DriverVer.
- That pipeline cannot build the Rust drivers, and its badge no longer reflects this code. The
  in-repo CI is `.github/workflows/rust-ci.yml` (`.github/workflows/github-issue-sync.yml` only
  syncs issues):
  - host tests and Clippy, and Clippy (`cargo check` with `clippy-driver`) of the driver workspace
    for x64 and ARM64, scoped to the packages a change affects;
  - the **Driver package** job, for x64 and ARM64, whenever a driver, the INF template or xtask
    changes: `cargo xtask package --arch <arch> --release`, unsigned. It links the KMDF drivers,
    compiles the `Resource.rc` files and runs stampinf, `InfVerif /w` and Inf2Cat, which Clippy
    alone never exercises. The kit comes from the same NuGet packages as the Clippy jobs;
    `.github/scripts/rust-ci.ps1 -PackagingTools` also extracts the WDK's `bin` and `tools`
    directories (stampinf, Inf2Cat, InfVerif) from the x64 WDK package.
- The replacement is `cargo xtask package`.
  - `ReleaseSigned` becomes `--release --sign ...`.
  - The pipeline DriverVer becomes `--driver-version a.b.c.d`.
  - Debug/Release `TestSign` becomes `--sign` with a test certificate.
- The "For developers" section of README.md (KMDF 1.23/1.15, UMDF 2.15, "DDK 2004",
  `ReleaseSigned`) and the badge are updated in the integration phase, not here.

## Verification record

Done on 2026-11-26 with WDK 10.0.28000, Rust 1.100, and the stub drivers from the scaffolding
phase:

- `cargo xtask package --arch x64 --release` and `--arch arm64 --release`, each packaging both
  INFs, exited 0. A final `--arch all --release` run also exited 0. For all four packages:
  - stampinf stamped `DriverVer=<date>,<h.m.s.ms>`, `CatalogFile`, `NTamd64` / `NTarm64`,
    `KmdfLibraryVersion = 1.23` and `UmdfLibraryVersion=2.15.0`.
  - `InfVerif /u` reported no errors. The only findings were the `WARNING(2084)` lines above: five
    for AmtPtpDevice.inf (target OS `NT<arch>.10.0...17134`) and one for AmtPtpHidFilter.inf.
  - Inf2Cat reported `Signability test complete. Errors: None Warnings: None` and `Catalog
    generation complete.`
- `--driver-version 19.0.0.200` stamps `DriverVer = <date>,19.0.0.200`.
- `cargo xtask build --arch all --release` built all four workspace/arch combinations, exited 0,
  and printed the output directories.
- Inf2Cat exits with -2 when a file listed in the INF is missing, and xtask stops on any non-zero
  exit. InfVerif exits with 1627 when it reports errors.
- Diffing the stamped arm64 `AmtPtpDevice.inf` against the C INF shows only the changes in the
  table above, with `$ARCH$` replaced by `arm64`, plus the comment lines.
- PE headers: `.sys` files are machine 0x8664 or 0xAA64 with subsystem 1 (Native). The
  `AmtPtpDeviceUsbUm.dll` files are 0x8664 or 0xAA64 with subsystem 2 (Windows GUI DLL).
- `cargo xtask test`: host `cargo test --workspace` and the three clippy runs (`-D warnings`)
  passed.
- Packages are unsigned. Nothing was signed, test-signed, installed or loaded.

Review fixes, 2026-11-26 (FORCE_INTEGRITY, per-package symbols, `/p` redaction, eWDK discovery):

- `cargo test -p xtask` (21 tests, including the PE patch, `/p` redaction and discovery-order
  tests) and `cargo clippy -p xtask --all-targets -- -D warnings` passed.
- `cargo xtask package --arch x64 --release --inf all` exited 0; Inf2Cat reported no errors or
  warnings for either package. `llvm-readobj --file-headers`:
  - packaged `AmtPtpDeviceUsbUm.dll`: DllCharacteristics 0x160, CheckSum 0;
  - `drivers/um/target/x86_64-pc-windows-msvc/release/AmtPtpDeviceUsbUm.dll`: still 0x1E0;
  - packaged `.sys` files: unchanged, 0x1E0 with non-zero checksums.
- `cargo xtask package --arch arm64 --release --inf all`, run with only the eWDK variables
  `WDKContentRoot` and `Version_Number` set (pointing at the installed kit; no real eWDK was
  available), exited 0 and used that root: the printed root kept the variable's trailing `\`.
  The packaged ARM64 `AmtPtpDeviceUsbUm.dll` has 0x160.
- Both runs removed the old shared `symbols/` directory and wrote `AmtPtpDevice.symbols/`, with
  four PDBs, and `AmtPtpHidFilter.symbols/`, with one.
- `Version_Number=10.0.99999.0` fails before the build with "Version_Number=..., but ... has no
  stampinf.exe". A missing `WDKContentRoot` is skipped with a warning, and one without `bin\` is
  an error.
- The redaction was checked by unit test only. No `--sign` run was made, because no certificate
  exists and none is created.

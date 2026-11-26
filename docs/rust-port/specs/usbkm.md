<!-- Porting analysis of the C driver as of commit 2edb0f2, written before the Rust rewrite. File:line references point into that commit's `src/` tree, which has since been removed. -->

# AmtPtpDeviceUsbKm: porting spec (C/KMDF → Rust)

Scope: every file under `src/AmtPtpDeviceUsbKm/`, plus the sections of `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf` that install this binary. Repository HEAD `2edb0f2`. Its only change since `29d2778` is `.gitattributes`, and the scoped sources are byte-identical between the two commits.

Paths are relative to `src/AmtPtpDeviceUsbKm/` unless stated otherwise. Each `file:line` citation points at the C source.

Tags used throughout:

| Tag | Meaning |
|---|---|
| **[KEEP]** | Observable behavior. Replicate it to stay identical. |
| **[DECIDE]** | Looks like a bug. The current behavior is documented exactly. The port must replicate or fix it deliberately. |
| **[DROP]** | Dead code or no observable effect. Safe to omit. |
| **BUG** | Behavior that differs from what the code evidently intends. Flagged, not fixed. |
| **QUIRK** | Odd but harmless, or intentional. |

---

## 0. Overview

`AmtPtpDeviceUsbKm.sys` is a **KMDF 1.15 lower filter driver**. It sits on the USB composite-device child for interface 2 (`USB\VID_05AC&PID_027x&MI_02`) of the Apple T2 internal keyboard/trackpad. The function driver of that devnode is `mshidkmdf.sys` (the HID class pass-through), so this filter **acts as the real HID minidriver**:

- `mshidkmdf` forwards the HID minidriver IOCTLs down as `IRP_MJ_INTERNAL_DEVICE_CONTROL`. This driver answers them: HID descriptor, attributes, report descriptor, read report, get feature, set feature.
- **Every other internal IOCTL is completed with `STATUS_NOT_SUPPORTED`. None are forwarded down.**

On the hardware side it:

1. Selects the USB interface.
2. Opens the first interrupt pipe.
3. Runs a WDF continuous reader with 526-byte buffers.
4. Switches the trackpad into "wellspring" raw multitouch mode, using HID class GET_REPORT/SET_REPORT on feature report 0x02, interface 2.
5. Converts each raw packet into one Windows Precision Touchpad (PTP) input report (ID 0x05, 50 bytes), delivered by completing a pended `IOCTL_HID_READ_REPORT`.

It has no registry access, timers, work items, DPCs, locks, idle/selective-suspend or wake support.

```
  HID collection PDOs (created by hidclass)
  ──────────────────────────────────────────────
  FDO   : hidclass + mshidkmdf.sys        (INF: AddService flag 0x2 = function driver)
  ──────────────────────────────────────────────
  FiDO  : AmtPtpDeviceUsbKm.sys           (INF LowerFilters; WdfFdoInitSetFilter)   <== this driver
  ──────────────────────────────────────────────
  PDO   : usbccgp child  USB\VID_05AC&PID_xxxx&MI_02
```

The most important non-obvious facts for a porter are listed below. All are detailed later.

1. **ScanTime is always 0.**
   - `KeQueryPerformanceCounter(&x)` writes the **frequency** into `x`, not the counter (Device.c:319, Interrupt.c:177).
   - Both stored values are therefore the constant QPC frequency, so the delta is 0.
   - See §7.2 step 8.
2. **The HQA certification blob is not copied.**
   - `*blob = 0xfc, 0x28, ...;` is a comma expression, so only one byte is written (Hid.c:388).
   - See §5.5.
3. **The wellspring control transfers use wLength = 4**, not 2. The cause is `sizeof(int)` at Device.c:551, while the allocated buffer is only 2 bytes. See §6.3.
4. **More than 5 contacts makes every read fail** with `STATUS_DATA_ERROR` (Interrupt.c:186-194).
5. **Contact IDs are slot indices.** There is no finger tracking.
6. **GET_FEATURE and SET_FEATURE complete with Information = 0.**
7. **Selective reporting (report 0x06) is a no-op.**

---

## 1. Build facts

### 1.1 Project (`AmtPtpDeviceUsbKm.vcxproj`)

| Item | Value | Source |
|---|---|---|
| ProjectGuid | `{AB3E45E7-C524-47C1-9677-728BA2A19344}` | vcxproj:46 |
| TemplateGuid | `{8c0e3d8b-df43-455b-815a-4a0e72973bc6}` (VS "KMDF USB driver" template) | vcxproj:47 |
| RootNamespace / TargetName | `AmtPtpDeviceUsbKm` → **`AmtPtpDeviceUsbKm.sys`** | vcxproj:52; .rc OriginalFilename |
| Driver model | **KMDF** (`DriverType=KMDF`), `ConfigurationType=Driver` | vcxproj:61-96 |
| Toolset | `WindowsKernelModeDriver10.0` | vcxproj:62 etc. |
| DriverTargetPlatform | `Universal` | vcxproj:65 etc. |
| TargetVersion | `Windows10` | vcxproj:98-121 |
| KMDF version | **Not pinned** (no `KMDF_VERSION_MAJOR/MINOR`/`KmdfVersion`). The WDK default for TargetVersion=Windows10 is **1.15** (WDK `build/10.0.28000.0/WindowsDriver.KernelMode.Default.props:15-21`). It is injected as `KMDF_VERSION_MAJOR=1;KMDF_VERSION_MINOR=15` (`WindowsDriver.Common.targets:439`). The INF `KmdfLibraryVersion=$KMDFVERSION$` is stamped with the same value. All APIs used exist in KMDF ≤1.9. | — |
| SDK | `$(LatestTargetPlatformVersion)`; `10.0.19041.0` when `BuildEnvironment==Github` | vcxproj:55-60 |
| Configurations | Debug / Release / ReleaseSigned × x64 / ARM64. The Globals default `Platform=Win32` is vestigial; x86 was removed in commit 8e0cc7c. | vcxproj:3-28, 51 |
| UseDebugLibraries | true for Debug, false otherwise | vcxproj:98-121 |
| OutDir / IntDir | `$(SolutionDir)build\$(ProjectName)\$(Platform)\$(ConfigurationName)\`, `$(SolutionDir)intermediate\...` | vcxproj:130-169 |
| Include path | `IntDir` (for generated `.tmh`) ; `$(ProjectDir)include` ; defaults. `#include <Hid.h>` (Driver.h:28) resolves to `include/Hid.h` because of this ordering. | vcxproj:134 etc. |
| Custom preprocessor defines | **None.** Only WDK-injected ones (KMDF version, arch, `DBG=1` in Debug). The source tests `ALLOC_PRAGMA` (set by the WDK) for `alloc_text`. | — |
| Disabled warnings | `4214` (non-int bit-field types, used by `PTP_CONTACT` and the selective-report struct) | vcxproj:183 etc. |
| Extra link libs | `usbdex.lib; ntstrsafe.lib`. **Neither is referenced by the source** (no `USBD_*`, no `RtlString*`) **[DROP]**. Implicit WDK libs: KMDF stub (WdfDriverEntry/WdfLdr), ntoskrnl, hal, wmilib, BufferOverflowFastFailK, and WppRecorder (IFR enabled). | vcxproj:186 etc. |
| Signing | `FileDigestAlgorithm=sha256` on the **x64 configs only** (vcxproj:188-190, 203-205, 218-220). ARM64 configs omit it. `ReleaseSigned`: `ProductionCertificate=$(ProductionCertPath)`, `SignMode=ProductionSign` unless `BuildEnvironment==AzurePipeline` (then `Off`). TimeStamp server `http://timestamp.digicert.com`. | vcxproj:171-176 |
| Packaging | `FilesToPackage $(TargetPath)` (vcxproj:259). Consumed by `AmtPtpDeviceUniversalPkg.vcxproj:33` (ProjectReference). | — |
| Sources | `DebugUtils.c Device.c Driver.c Hid.c Interrupt.c Queue.c`; headers listed in the vcxproj: `Device.h Driver.h Public.h Queue.h resource.h Trace.h`; `Resource.rc`. The four `include/` headers (`AppleDefinition.h`, `Hid.h`, `hid/HidCommon.h`, `metadata/WellspringT2.h`) are **not** listed in the vcxproj. They are reached only through the `$(ProjectDir)include` include path. No `.inf`, `.wprp` or `.def` in this folder. `.vcxproj.filters` is IDE-only. | vcxproj:29-44, 261-263 |
| Encodings | `Resource.rc` is **UTF-16LE**. `Queue.c` contains one CP-1252 byte (a right single quote, Queue.c:276, comment only). Everything else is ASCII. | — |

**Code placement (`#pragma alloc_text`):**

- `INIT`: `DriverEntry` (Driver.c:21).
- `PAGE`: `AmtPtpDeviceUsbKmEvtDeviceAdd` (Driver.c:22), `AmtPtpDeviceUsbKmEvtDriverContextCleanup` (Driver.c:23), `AmtPtpDeviceUsbKmCreateDevice` (Device.c:21), `AmtPtpDeviceUsbKmEvtDevicePrepareHardware` (Device.c:22), `AmtPtpDeviceUsbKmQueueInitialize` (Queue.c:21).
- **Everything else is non-paged**, including all I/O, power and USB-completion code.
- `PAGED_CODE()` also appears in three non-paged functions, where it is an assert only: `AmtPtpEvtDeviceD0Exit` (Device.c:370), `SelectInterruptInterface` (Device.c:432), `AmtPtpReportFeatures` (Hid.c:300).

### 1.2 WPP tracing

| Item | Value | Source |
|---|---|---|
| Enabled | `WppEnabled=true`, `WppRecorderEnabled=true` (In-Flight Recorder), `WppKernelMode=true`, `WppScanConfigurationData=trace.h` | vcxproj:179-182 etc. |
| Control GUID | `AmtPtpDeviceUsbKmTraceGuid` = `{b409cf52-f41a-4193-98c2-bad95204203b}` | Trace.h:20-31 |
| Flags (bit order) | `TRACE_DRIVER`=0x1, `TRACE_DEVICE`=0x2, `TRACE_QUEUE`=0x4, `TRACE_INPUT`=0x8 | Trace.h:27-30 |
| Trace functions | `Trace{FLAGS=TRACE_DRIVER}(LEVEL, MSG, ...)` (never used), `TraceEvents(LEVEL, FLAGS, MSG, ...)` | Trace.h:58-61 |
| Level/flag filter macros | `WPP_FLAG_LEVEL_*`, `WPP_LEVEL_FLAGS_*`, `WPP_RECORDER_FLAGS_LEVEL_*` | Trace.h:33-52 |
| TMH includes | `driver.tmh` (Driver.c:18), `device.tmh` (Device.c:18), `hid.tmh` (Hid.c:2), `Interrupt.tmh` (Interrupt.c:4), `queue.tmh` (Queue.c:18). DebugUtils.c has none. | — |
| Init / cleanup | `WPP_INIT_TRACING(DriverObject, RegistryPath)` (Driver.c:65). `WPP_CLEANUP(DriverObject)` on the WdfDriverCreate failure path (Driver.c:89). `WPP_CLEANUP(WdfDriverWdmGetDriverObject(Driver))` in the driver-object cleanup callback (Driver.c:173). | — |

Tracing is not behavior. In Rust, replace it with TraceLogging, ETW or `DbgPrint`. Keeping the same provider GUID is optional; the payload format will differ.

### 1.3 Resource (`Resource.rc`, UTF-16LE; line numbers are of the decoded text)

- `LANGUAGE 9, 1` (rc:19).
- STRINGTABLE `IDS_APP_TITLE` (=103, resource.h:5) = `"Apple Precision Touchpad Driver"` (rc:51-54).
- `VS_VERSION_INFO` (rc:61-92):
  - `FILEVERSION 19,0,0,200` and `PRODUCTVERSION 19,0,0,200`.
  - `FILEFLAGSMASK 0x3F`. `FILEFLAGS 0x1` if `_DEBUG`, else `0x0`.
  - `FILEOS 0x40004` (VOS_NT_WINDOWS32).
  - `FILETYPE 0x1` (**VFT_APP**). QUIRK: a driver would normally use VFT_DRV=3.
  - `FILESUBTYPE 0`.
  - StringFileInfo block `"040904b0"`:
    - `CompanyName` "Bingxing Wang"
    - `FileDescription` "Apple Precision Touchpad Kernel Mode Driver"
    - `FileVersion` "19.0.0.200"
    - `InternalName` "AmtPtpDeviceUsbKm.sys"
    - `LegalCopyright` "Copyright (C) Bingxing Wang 2019"
    - `OriginalFilename` "AmtPtpDeviceUsbKm.sys"
    - `ProductName` "Apple Precision Touchpad Driver Component"
    - `ProductVersion` "19.0.0.200"
  - `VarFileInfo Translation 0x409, 1200`.
- The TEXTINCLUDE blocks (rc:21-44) are APSTUDIO-only. TEXTINCLUDE 1 names a non-existent `resource1.h`, which is harmless.

### 1.4 Device interface GUID

`GUID_DEVINTERFACE_AmtPtpDeviceUsbKm = {4aa332cc-5777-4afd-aa4e-95387330612a}` (Public.h:22-24) is registered in CreateDevice (Device.c:113-117).

**Nothing in the repository consumes it, and the driver has no handler for user IOCTLs.** A user-mode open of the interface symlink enters at the **top** of the devnode stack, which is the hidclass + mshidkmdf FDO, not this filter. hidclass does not send create, close or device-control down to its minidriver (MS Learn, "Minidrivers and the HID class driver"). So no user I/O on this interface ever reaches this filter, and the WDF filter auto-forward path (§4.1) is never exercised by it. The registration is kept for fidelity, since it creates a visible interface, but it is functionally **[DROP]-able**.

### 1.5 INF sections for this binary (`src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf`)

| Section / line | Content |
|---|---|
| `[Version]` 5-12 | `Class=HIDClass`, `ClassGuid={745a17a0-74d3-11d0-b6fe-00a0c90f57da}`, `Provider=%ManufacturerName%` ("Bingxing Wang"), `CatalogFile=AmtPtpDevice.cat`, `DriverVer=` (stamped), `PnpLockdown=1` |
| `[DestinationDirs]` 14-16 | `DefaultDestDir=13` (DriverStore run-from). `PtpKmDrivers_Dir` is not listed, so it uses the default 13. |
| `[SourceDisksNames]` 18-19 | `1 = %DiskName%,,,""` |
| `[SourceDisksFiles]` 22 | `AmtPtpDeviceUsbKm.sys = 1,,` |
| `[Manufacturer]` 27-28 | `%ManufacturerName%=Standard,NT$ARCH$` |
| `[Standard.NT$ARCH$]` 31-44 | `%AmtPtpDeviceUsbKm.DeviceDesc%=AmtPtpDeviceUsbKm_Device, USB\Vid_05ac&Pid_XXXX&MI_02` for **XXXX ∈ {0273, 0274, 0277, 027A, 027B, 027C, 027D, 027E, 027F, 0280, 0290, 0291, 0340}** |
| `[AmtPtpDeviceUsbKm_Device.NT]` 149-150 | `CopyFiles=PtpKmDrivers_Dir`. `[PtpKmDrivers_Dir]` (104-105) lists `AmtPtpDeviceUsbKm.sys`. |
| `[AmtPtpDeviceUsbKm_Device.NT.Services]` 152-154 | `AddService = AmtPtpDeviceUsbKm,, AmtPtpDeviceUsbKm_Service_Inst` (flags 0, so a filter service). `AddService = mshidkmdf, %SPSVCINST_ASSOCSERVICE% (0x2), mshidkmdf_Service_Inst` (**function driver = mshidkmdf**). |
| `[mshidkmdf_Service_Inst]` 114-118 | Type 1 (kernel), Start 3 (demand), ErrorControl 1, `%10%\System32\Drivers\mshidkmdf.sys` |
| `[AmtPtpDeviceUsbKm_Device.NT.HW]` 156-157 → `[AmtPtpDeviceUsbKm_AddReg]` 159-161 | `HKR,,FriendlyName,,%AmtPtpDeviceUsbKm.DeviceDesc%` and **`HKR,,"LowerFilters",0x00010008,"AmtPtpDeviceUsbKm"`** (REG_MULTI_SZ, append) |
| `[AmtPtpDeviceUsbKm_Service_Inst]` 164-169 | `DisplayName=%AmtPtpDeviceUsbKm.SVCDESC%` ("Apple USB Precision Touchpad Service"), `ServiceType=1`, `StartType=3`, `ErrorControl=1`, `ServiceBinary=%13%\AmtPtpDeviceUsbKm.sys`. **The service name `AmtPtpDeviceUsbKm` must equal the LowerFilters value and the KmdfService name.** |
| `[AmtPtpDeviceUsbKm_Device.NT.Wdf]` 171-172 | `KmdfService = AmtPtpDeviceUsbKm, AmtPtpDeviceUsbKm_wdfsect` |
| `[AmtPtpDeviceUsbKm_wdfsect]` 174-175 | `KmdfLibraryVersion = $KMDFVERSION$` (resolves to 1.15) |
| `[Strings]` 252-253 | `DiskName="AmtPtpDeviceUsbKm Installation Disk"`, `AmtPtpDeviceUsbKm.DeviceDesc="Apple USB Precision Touchpad Device (Kernel-mode)"` |

INF observations:

- **(a)** `0273, 0274, 0290, 0291` with `&MI_02` are **also** bound to `AmtPtpDeviceUsbUm_Install` at lines 69-72. The same INF has two install sections for an identical HWID, so which one wins is ambiguous. Flag only.
- **(b)** 9 of the 13 PIDs (`0273 0274 0277 027E 027F 0280 0290 0291 0340`) have no entry in `Bcm5974ConfigTable`. They get the generic fallback geometry (§8).
- **(c)** There is no UpperFilters entry. The filter only works under `mshidkmdf`, because it relies on receiving HID minidriver IOCTLs as internal device-control requests.
- If the Rust port changes its KMDF version (for example to 1.33 via wdk-sys), `KmdfLibraryVersion` and the minimum OS change too.

---

## 2. Entry points and WDF callbacks

### 2.1 Inventory

| Function | File:line | Kind / registered on | IRQL / section |
|---|---|---|---|
| `DriverEntry` | Driver.c:27-96 | `DRIVER_INITIALIZE` | PASSIVE, INIT |
| `AmtPtpDeviceUsbKmEvtDeviceAdd` | Driver.c:98-143 | `EVT_WDF_DRIVER_DEVICE_ADD`, from `WDF_DRIVER_CONFIG` | PASSIVE, PAGE |
| `AmtPtpDeviceUsbKmEvtDriverContextCleanup` | Driver.c:145-175 | `EVT_WDF_OBJECT_CONTEXT_CLEANUP` on **WDFDRIVER** attributes | PASSIVE, PAGE |
| `AmtPtpDeviceUsbKmCreateDevice` | Device.c:50-128 | helper (called from DeviceAdd) | PASSIVE, PAGE |
| `AmtPtpDeviceUsbKmEvtDevicePrepareHardware` | Device.c:130-272 | `EvtDevicePrepareHardware` (WDF_PNPPOWER_EVENT_CALLBACKS) | PASSIVE, PAGE |
| `AmtPtpEvtDeviceD0Entry` | Device.c:275-359 | `EvtDeviceD0Entry` | PASSIVE, non-paged |
| `AmtPtpEvtDeviceD0Exit` | Device.c:361-416 | `EvtDeviceD0Exit` | PASSIVE, non-paged |
| `AmtPtpGetDeviceConfig` (static) | Device.c:25-48 | helper | PASSIVE |
| `SelectInterruptInterface` | Device.c:418-501 | helper | PASSIVE |
| `AmtPtpSetWellspringMode` | Device.c:503-639 | helper (synchronous USB control transfers) | **PASSIVE only** |
| `AmtPtpDeviceUsbKmQueueInitialize` | Queue.c:24-103 | helper | PASSIVE, PAGE |
| `AmtPtpDeviceUsbKmEvtIoDeviceControl` | Queue.c:105-182 | **`EvtIoInternalDeviceControl`** of the default queue (Queue.c:66). The name says "DeviceControl"; the registration is *internal*. | ≤ DISPATCH (no constraint) |
| `AmtPtpDispatchReadReportRequests` | Queue.c:184-219 | helper | ≤ DISPATCH |
| `AmtPtpDeviceUsbKmEvtIoStop` | Queue.c:221-280 | `EvtIoStop` of the default queue (Queue.c:67) | ≤ DISPATCH |
| `AmtPtpGetHidDescriptor` | Hid.c:26-119 | helper (IOCTL handler) | annotated PASSIVE |
| `AmtPtpGetDeviceAttribs` | Hid.c:121-167 | helper | annotated PASSIVE |
| `AmtPtpGetReportDescriptor` | Hid.c:169-285 | helper | annotated PASSIVE |
| `AmtPtpReportFeatures` | Hid.c:287-418 | helper (GET_FEATURE) | annotated PASSIVE |
| `AmtPtpSetFeatures` | Hid.c:420-559 | helper (SET_FEATURE). It calls `AmtPtpSetWellspringMode`, so it **must** run at PASSIVE. | annotated PASSIVE |
| `AmtPtpConfigContReaderForInterruptEndPoint` | Interrupt.c:14-87 | helper | PASSIVE |
| `AmtPtpEvtUsbInterruptPipeReadComplete` | Interrupt.c:89-250 | `EVT_WDF_USB_READER_COMPLETION_ROUTINE` (continuous reader) | **≤ DISPATCH** |
| `AmtPtpEvtUsbInterruptReadersFailed` | Interrupt.c:252-264 | `EVT_WDF_USB_READERS_FAILED` | ≤ DISPATCH |
| `AmtRawToInteger` (static inline) | Interrupt.c:7-12 | `(int)(signed short)x` | any |
| `DbgDevicePowerString` | DebugUtils.c:3-29 | trace helper (power-state name) | any |
| `DbgIoControlGetString` | DebugUtils.c:31-68 | **never called [DROP]** | any |

**Not present (say "none" in the port):**

- `EvtDeviceReleaseHardware`, `EvtDeviceSelfManagedIo*`, `EvtDeviceSurpriseRemoval`, `EvtDeviceQueryRemove/Stop`.
- Power-policy callbacks (`ArmWake*`, etc.), `WdfDeviceAssignS0IdleSettings`, `WdfDeviceAssignSxWakeSettings`.
- Interrupt objects, DPCs, `WDFTIMER`/`KTIMER`, `WDFWORKITEM`, spin locks and wait locks.
- Registry access, WMI.
- `EvtIoRead`, `EvtIoWrite`, `EvtIoDeviceControl` (non-internal), `EvtIoDefault`, `EvtIoResume`, `EvtIoCanceledOnQueue`.
- `EvtDriverUnload` (WDF handles unload).

### 2.2 `DriverEntry` (Driver.c:27-96)

1. `WPP_INIT_TRACING(DriverObject, RegistryPath)`.
2. `WDF_OBJECT_ATTRIBUTES_INIT(&attributes)`, then `attributes.EvtCleanupCallback = AmtPtpDeviceUsbKmEvtDriverContextCleanup`.
   - Resolved fields: `Size`, `ExecutionLevel = WdfExecutionLevelInheritFromParent`, `SynchronizationScope = WdfSynchronizationScopeInheritFromParent`, no context type, no destroy callback.
3. `WDF_DRIVER_CONFIG_INIT(&config, AmtPtpDeviceUsbKmEvtDeviceAdd)`.
   - Resolved fields: `Size`, `EvtDriverDeviceAdd` set, `EvtDriverUnload = NULL`, `DriverInitFlags = 0` (PnP driver), `DriverPoolTag = 0` (framework default).
4. `status = WdfDriverCreate(DriverObject, RegistryPath, &attributes, &config, WDF_NO_HANDLE)`.
   - On failure: trace, `WPP_CLEANUP(DriverObject)`, then return status.
5. Return status.

### 2.3 `AmtPtpDeviceUsbKmEvtDeviceAdd` (Driver.c:98-143)

1. `WdfFdoInitSetFilter(DeviceInit)` (Driver.c:135). **[KEEP]** This makes the device a filter, which determines the queue power default and the auto-forwarding of unhandled request types (§4.1).
2. `WdfPdoInitAllowForwardingRequestToParent(DeviceInit)` (Driver.c:136). **BUG [DROP]**: this is a **PDO-only** API called on an FDO/filter `DeviceInit`. KMDF rejects it (an error trace, plus a break under the verifier). Otherwise it has no effect.
3. `return AmtPtpDeviceUsbKmCreateDevice(DeviceInit)`.

### 2.4 `AmtPtpDeviceUsbKmEvtDriverContextCleanup` (Driver.c:145-175)

Calls only `WPP_CLEANUP(WdfDriverWdmGetDriverObject((WDFDRIVER)DriverObject))`.

### 2.5 `AmtPtpDeviceUsbKmCreateDevice` (Device.c:50-128)

1. `WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&cb)`. Set:
   - `cb.EvtDevicePrepareHardware = AmtPtpDeviceUsbKmEvtDevicePrepareHardware`
   - `cb.EvtDeviceD0Entry = AmtPtpEvtDeviceD0Entry`
   - `cb.EvtDeviceD0Exit = AmtPtpEvtDeviceD0Exit`

   All other members are NULL. Then call `WdfDeviceInitSetPnpPowerEventCallbacks(DeviceInit, &cb)` (Device.c:81-85).
2. These are **not called**: `WdfDeviceInitSetPowerPolicyEventCallbacks`, `WdfDeviceInitSetIoType` (ignored for filters anyway), `WdfDeviceInitSetFileObjectConfig`, `WdfDeviceInitSetRequestAttributes`, `WdfDeviceInitSetPowerPolicyOwnership`, `WdfDeviceInitSetExclusive`, `WdfDeviceInitSetDeviceType`/characteristics, `WdfDeviceInitAssignSDDLString`.
   - So the file-object config is the framework default for a filter: create, cleanup and close are passed down.
   - The driver is **not** the power-policy owner; the FDO above is.
   - For a filter, the framework takes the I/O type, `DO_POWER_PAGABLE` and power-inrush settings from the next-lower device object (MS Learn, WdfFdoInitSetFilter remarks). The port must not set them explicitly.
3. `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, DEVICE_CONTEXT)` (Device.c:87).
   - The context type info comes from `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, DeviceGetContext)` (Device.h:54): name `"DEVICE_CONTEXT"`, `ContextSize = sizeof(DEVICE_CONTEXT)` = **80 bytes** on x64/ARM64.
   - There are no cleanup or destroy callbacks. ExecutionLevel and SynchronizationScope are both InheritFromParent, which resolves to **none**.
4. `WdfDeviceCreate(&DeviceInit, &attr, &device)`. The framework **zero-initializes** the context.
5. If that succeeded: set `ctx->PtpReportButton = TRUE` and `ctx->PtpReportTouch = TRUE` (Device.c:106-107).
6. `WdfDeviceCreateDeviceInterface(device, &GUID_DEVINTERFACE_AmtPtpDeviceUsbKm, NULL)` (Device.c:113-117).
7. If that succeeded: `status = AmtPtpDeviceUsbKmQueueInitialize(device)`.
8. Return status. Nothing is explicitly cleaned up on failure; WDF deletes the device when DeviceAdd fails.

### 2.6 `AmtPtpDeviceUsbKmEvtDevicePrepareHardware` (Device.c:130-272)

`ResourceList` and `ResourceListTranslated` are unused. Steps, in order:

1. If `ctx->UsbDevice == NULL`, call `WdfUsbTargetDeviceCreate(Device, WDF_NO_OBJECT_ATTRIBUTES, &ctx->UsbDevice)` (Device.c:180-191).
   - This is the non-`WithParameters` variant, so there is no USBD client contract version.
   - On failure, return status.
   - The target is created **only once**, so later restarts (for example a rebalance) reuse it.
2. `WdfUsbTargetDeviceGetDeviceDescriptor(ctx->UsbDevice, &ctx->DeviceDescriptor)` (Device.c:194-197).
3. `ctx->DeviceInfo = AmtPtpGetDeviceConfig(ctx->DeviceDescriptor)`. The 18-byte struct is passed by value (Device.c:200).
   - If the result is NULL: `STATUS_INVALID_DEVICE_STATE` (Device.c:201-206). **[DROP]**: this branch is unreachable because the function never returns NULL (§8).
4. `WDF_USB_DEVICE_INFORMATION_INIT(&info)`, then `WdfUsbTargetDeviceRetrieveInformation` (Device.c:212-247).
   - On success: trace HighSpeed, SelfPowered and RemoteWake from `info.Traits`, and store `ctx->UsbDeviceTraits = info.Traits`.
   - On failure: `ctx->UsbDeviceTraits = 0`, and the failure is **ignored** (status is overwritten by the next call).
   - `waitWakeEnable` is only traced. Traits are never read. **[DROP]**
5. `status = SelectInterruptInterface(Device)`. On failure, return.
6. `status = AmtPtpConfigContReaderForInterruptEndPoint(ctx)`. On failure, return. It never fails (BUG, §6.2).
7. `ctx->PtpReportButton = TRUE` and `ctx->PtpReportTouch = TRUE` (Device.c:266-267).
8. Return status (STATUS_SUCCESS).

### 2.7 `AmtPtpGetDeviceConfig` (Device.c:25-48)

```
id = descriptor.idProduct                       // USHORT, compared after promotion to int
for cfg in Bcm5974ConfigTable while cfg.identification != 0:
    if cfg.identification == id: return cfg
trace WARNING "Selected a generic fallback configuration"
return &Bcm5974ConfigTable[0]                   // identification 0xFFFF
```

The scan starts at index 0, which is the fallback entry 0xFFFF. It matches only a real PID of 0xFFFF.

### 2.8 `AmtPtpEvtDeviceD0Entry` (Device.c:275-359)

1. Trace the previous state with `DbgDevicePowerString`.
2. `if (ctx->PtpReportButton || ctx->IsWellspringModeOn)`. This is **always true**, because `PtpReportButton` is always TRUE.
   - Call `AmtPtpSetWellspringMode(ctx, TRUE)`.
   - A failure is only traced at WARNING level, and its status is **discarded**. **[KEEP]** (best-effort).
3. `KeQueryPerformanceCounter(&ctx->LastReportTime)` (Device.c:319). **BUG**: the argument is the *frequency* out-parameter, and the returned counter is discarded. So `LastReportTime` receives the **QPC frequency**. See §7.2 step 8.
4. `status = WdfIoTargetStart(WdfUsbTargetPipeGetIoTarget(ctx->InterruptPipe))` (Device.c:325). This starts the continuous reader.
   - On failure: trace, then `goto end`.
5. `isTargetStarted = TRUE`. The `end:` block calls `WdfIoTargetStop(..., WdfIoTargetCancelSentIo)` only when `!NT_SUCCESS(status) && isTargetStarted`. **That combination is impossible [DROP]** (Device.c:336-350).
6. Return status. A failure here makes WDF fail the power-up, and the device is torn down.

The ordering matters: the mode switch happens **before** the reader starts.

### 2.9 `AmtPtpEvtDeviceD0Exit` (Device.c:361-416)

1. Trace the target state.
2. `WdfIoTargetStop(WdfUsbTargetPipeGetIoTarget(ctx->InterruptPipe), WdfIoTargetCancelSentIo)`. This cancels the outstanding reader reads and waits for them.
3. `status = AmtPtpSetWellspringMode(ctx, FALSE)`. A failure is traced at WARNING level.
4. **`return status`** **[DECIDE]**.
   - D0Exit returns the wellspring-off result, so it fails whenever the control transfer fails. Example: a surprise-removed device during `WdfPowerDeviceD3Final`. This also applies to ordinary sleep and hibernate (D3, PrepareForHibernation) whenever the OFF transfer fails.
   - A non-success return from EvtDeviceD0Exit is treated by KMDF as a power-down failure.
   - Typical drivers return STATUS_SUCCESS here.
5. D0Exit runs for every target state, including `D3Final` (removal) and `PrepareForHibernation`.

The InputQueue is **not** power-managed. Pended `READ_REPORT` requests stay queued across Dx and are served after the next D0Entry.

### 2.10 `SelectInterruptInterface` (Device.c:418-501)

1. `WDF_USB_DEVICE_SELECT_CONFIG_PARAMS_INIT_SINGLE_INTERFACE(&p)`, which sets `Type = WdfUsbTargetDeviceSelectConfigTypeSingleInterface`. Then `WdfUsbTargetDeviceSelectConfig(ctx->UsbDevice, WDF_NO_OBJECT_ATTRIBUTES, &p)`.
   - On a usbccgp child this selects the function's only interface (bInterfaceNumber 2), alternate setting 0.
   - On failure, return.
2. `ctx->UsbInterface = p.Types.SingleInterface.ConfiguredUsbInterface`. `n = p.Types.SingleInterface.NumberConfiguredPipes` (UCHAR).
3. Pipe loop:

   ```
   for index in 0..n:
       WDF_USB_PIPE_INFORMATION_INIT(&pi)
       pipe = WdfUsbInterfaceGetConfiguredPipe(ctx->UsbInterface, index, &pi)
       WdfUsbTargetPipeSetNoMaximumPacketSizeCheck(pipe)      // applied to every pipe visited
       trace pi.PipeType
       if pi.PipeType == WdfUsbPipeTypeInterrupt: ctx->InterruptPipe = pipe; break
   ```

   - The **first interrupt pipe** wins.
   - **Direction is not checked.** If the first interrupt pipe were OUT, `WdfUsbTargetPipeConfigContinuousReader` would fail with `STATUS_INVALID_DEVICE_REQUEST` (invalid pipe type). B9 masks that failure, so the device would produce no input.
   - The config table's `tp_ep = 0x83` is **not used**.
   - **[KEEP] (order-dependent):** `WdfUsbTargetPipeSetNoMaximumPacketSizeCheck` must run on the pipe **before** the continuous reader is configured (§6.2). The reader's TransferLength of 526 = 2 × 263 is not a multiple of any realistic interrupt-endpoint MaxPacketSize (8/16/32/64/512/1024). The real endpoint's MaxPacketSize is not visible in the source. With the default check, `WdfUsbTargetPipeConfigContinuousReader` returns `STATUS_INVALID_BUFFER_SIZE` (MS Learn, WdfUsbTargetPipeConfigContinuousReader return values). B9 hides that error, so the result is silent zero input. A port that drops or reorders this call breaks input with no visible error.
4. If `ctx->InterruptPipe == NULL`, return `STATUS_INVALID_DEVICE_STATE`.

Edge case: `InterruptPipe` is not reset before the loop. On a second PrepareHardware (after a rebalance), a stale handle from the previous selection would pass the NULL check if no interrupt pipe were found. That is unlikely on real hardware. **[DECIDE]**: resetting it is harmless.

---

## 3. Device context (`DEVICE_CONTEXT`, Device.h:25-47)

The context is zero-initialized by WDF. Its size is **80 bytes** on x64/ARM64, with 8-byte alignment.

| Field | C type | x64 offset | Meaning | Written by | Read by |
|---|---|---|---|---|---|
| `UsbDevice` | `WDFUSBDEVICE` | 0 | USB target device | PrepareHardware (Device.c:181), once | SetWellspringMode, SelectInterruptInterface |
| `InterruptPipe` | `WDFUSBPIPE` | 8 | Continuous-reader pipe | SelectInterruptInterface (Device.c:479) | ConfigContReader (Interrupt.c:65), D0Entry (Device.c:325), D0Exit (Device.c:383) |
| `UsbInterface` | `WDFUSBINTERFACE` | 16 | Selected interface | SelectInterruptInterface (Device.c:449) | Pipe enumeration only |
| `InputQueue` | `WDFQUEUE` | 24 | Manual queue of pended READ_REPORTs | QueueInitialize (Queue.c:91) | DispatchReadReport (Queue.c:200), ReadComplete (Interrupt.c:141) |
| `DeviceDescriptor` | `USB_DEVICE_DESCRIPTOR` (18 B) | 32 | Cached USB device descriptor | PrepareHardware (Device.c:194-197) | GetDeviceConfig (`idProduct`), Hid.c switches (`idProduct`), GetDeviceAttribs (`idVendor`, `idProduct`) |
| `UsbDeviceTraits` | `ULONG` | 52 | `WDF_USB_DEVICE_TRAIT_*` | PrepareHardware (Device.c:243/246) | **never [DROP]** |
| `DeviceInfo` | `const struct BCM5974_CONFIG*` | 56 | Selected table row | PrepareHardware (Device.c:200) | SetWellspringMode, ConfigContReader, ReadComplete |
| `IsWellspringModeOn` | `BOOLEAN` | 64 | Last successfully applied mode | SetWellspringMode (Device.c:526, 627) | D0Entry (Device.c:296), SetFeatures (Hid.c:477) |
| `PtpInputOn` | `BOOLEAN` | 65 | — | **never** | **never [DROP]** |
| `PtpReportTouch` | `BOOLEAN` | 66 | Gate for contact parsing | TRUE at Device.c:107 and 267 only | ReadComplete (Interrupt.c:185). **Always TRUE.** |
| `PtpReportButton` | `BOOLEAN` | 67 | Gate for button parsing and the D0Entry mode switch | TRUE at Device.c:106 and 266 only | D0Entry (Device.c:296), ReadComplete (Interrupt.c:217). **Always TRUE.** |
| `LastReportTime` | `LARGE_INTEGER` | 72 | Intended last-report timestamp; actually holds the **QPC frequency** | D0Entry (Device.c:319) only | ReadComplete (Interrupt.c:178) |

**Locking: none.** There are no WDF locks, no sync scope and no interlocked operations. Concurrency points:

- `AmtPtpEvtUsbInterruptPipeReadComplete` runs at ≤ DISPATCH_LEVEL. With more than one outstanding reader read, these completions are not guaranteed to be serialized with each other or with the I/O dispatch (this is inferred; the driver sets no sync scope). It reads `DeviceInfo`, the two flags and `LastReportTime`, and dequeues from `InputQueue` (thread-safe WDF call).
- `AmtPtpSetFeatures` (I/O path) can write `IsWellspringModeOn` concurrently with D0Entry/D0Exit (PnP/power thread).
- All of these are byte or aligned-qword accesses, which are benign on x64/ARM64. In Rust, use `AtomicBool`/`AtomicI64` (Relaxed) or `UnsafeCell` fields in a `#[repr(C)]` context placed in WDF memory.

`QUEUE_CONTEXT` (Queue.h:23-29: `ULONG PrivateDeviceData`, with a `QueueGetContext` accessor) is declared but never attached to any queue. **[DROP]**

Framework-owned objects not in the context:

- The default queue handle, a local in QueueInitialize.
- The continuous-reader buffers: 526 bytes each, owned by the pipe, default buffer attributes (non-paged).
- The transient `WDFMEMORY` in SetWellspringMode.

---

## 4. I/O request handling

### 4.1 Queues (`AmtPtpDeviceUsbKmQueueInitialize`, Queue.c:24-103)

**Default queue** (Queue.c:64-79). `WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&cfg, WdfIoQueueDispatchParallel)` resolves to:

- `DispatchType = Parallel`, `PowerManaged = WdfUseDefault`, `DefaultQueue = TRUE`.
- `NumberOfPresentedRequests = (ULONG)-1` (unlimited, KMDF ≥ 1.9), `AllowZeroLengthRequests = FALSE`.
- `EvtIoInternalDeviceControl = AmtPtpDeviceUsbKmEvtIoDeviceControl` and `EvtIoStop = AmtPtpDeviceUsbKmEvtIoStop`. All other callbacks are NULL.
- Object attributes `WDF_NO_OBJECT_ATTRIBUTES` (parent = device).

The handle is not stored. On failure, the status is returned.

Two consequences of `WdfFdoInitSetFilter`:

- **Not power-managed.** MS Learn, WDF_IO_QUEUE_CONFIG: *"If set to WdfUseDefault, the framework handles power management for the queue unless the driver calls WdfFdoInitSetFilter."* So requests are delivered in any power state.
- **Unhandled request types are auto-forwarded** to the next-lower driver (MS Learn, "Creating I/O Queues" and the WdfFdoInitSetFilter remarks). That covers `IRP_MJ_CREATE/CLOSE/CLEANUP/READ/WRITE/DEVICE_CONTROL` and so on. This is the general WDF rule. In this stack, hidclass never sends create, close or device-control to its minidriver, so in practice only internal device-control, PnP and power IRPs reach this filter (§1.4).
- **Only `IRP_MJ_INTERNAL_DEVICE_CONTROL` is intercepted, and every such request is completed here. None are forwarded.**

**InputQueue** (Queue.c:84-100). `WDF_IO_QUEUE_CONFIG_INIT(&cfg, WdfIoQueueDispatchManual)`, then explicitly `PowerManaged = WdfFalse`. No callbacks, no attributes (parent = device). The handle goes to `ctx->InputQueue`. On failure, trace and return the status, which fails DeviceAdd.

- There is no `WdfDeviceConfigureRequestDispatching`.
- The InputQueue is **never** started, stopped, purged or drained by the driver.
- While queued, requests are cancelable by the framework: a requestor cancel completes them `STATUS_CANCELLED` with no driver callback.
- On device removal, WDF purges the queue.

**`AmtPtpDeviceUsbKmEvtIoStop`** (Queue.c:221-280) only traces. It neither acknowledges nor completes. Every request the driver receives from the default queue is either completed synchronously or forwarded to the manual queue, after which WDF owns it. So the callback never has driver-owned work. **[DROP]**, or keep it as a no-op.

### 4.2 Dispatcher (`AmtPtpDeviceUsbKmEvtIoDeviceControl`, Queue.c:105-182)

Signature: `(Queue, Request, OutputBufferLength, InputBufferLength, IoControlCode)`. **The two lengths are unused.**

```
device = WdfIoQueueGetDevice(Queue); pending = FALSE
switch IoControlCode { ... status = handler(...) }
if (!pending) WdfRequestComplete(Request, status)   // Information = whatever handler set (default 0)
```

IOCTL codes come from `CTL_CODE(FILE_DEVICE_KEYBOARD=0x0B, fn, method, FILE_ANY_ACCESS)`, defined in WDK `km/hidport.h:183-200` and `shared/hidclass.h:133-168`.

| IOCTL | Code | Method | Handler | Result |
|---|---|---|---|---|
| `IOCTL_HID_GET_DEVICE_DESCRIPTOR` | `0x000B0003` | NEITHER | `AmtPtpGetHidDescriptor` | §5.1 |
| `IOCTL_HID_GET_DEVICE_ATTRIBUTES` | `0x000B0027` | NEITHER | `AmtPtpGetDeviceAttribs` | §5.3 |
| `IOCTL_HID_GET_REPORT_DESCRIPTOR` | `0x000B0007` | NEITHER | `AmtPtpGetReportDescriptor` | §5.2 |
| `IOCTL_HID_READ_REPORT` | `0x000B000B` | NEITHER | `AmtPtpDispatchReadReportRequests` | pended, §4.3 |
| `IOCTL_HID_GET_FEATURE` | `0x000B0192` | OUT_DIRECT (nominal) | `AmtPtpReportFeatures` | §5.5 |
| `IOCTL_HID_SET_FEATURE` | `0x000B0191` | IN_DIRECT (nominal) | `AmtPtpSetFeatures` | §5.6 |
| `IOCTL_HID_GET_STRING` | `0x000B0013` | NEITHER | — | `STATUS_NOT_SUPPORTED` |
| `IOCTL_HID_WRITE_REPORT` | `0x000B000F` | NEITHER | — | `STATUS_NOT_SUPPORTED` |
| `IOCTL_UMDF_HID_SET_OUTPUT_REPORT` | `0x000B005B` | NEITHER | — | `STATUS_NOT_SUPPORTED` (UMDF code; never sent in KM) |
| `IOCTL_UMDF_HID_GET_INPUT_REPORT` | `0x000B005F` | NEITHER | — | `STATUS_NOT_SUPPORTED` (UMDF code) |
| `IOCTL_HID_ACTIVATE_DEVICE` | `0x000B001F` | NEITHER | — | `STATUS_NOT_SUPPORTED` |
| `IOCTL_HID_DEACTIVATE_DEVICE` | `0x000B0023` | NEITHER | — | `STATUS_NOT_SUPPORTED` |
| `IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST` | `0x000B002B` | NEITHER | — | `STATUS_NOT_SUPPORTED`, so HID selective suspend is unavailable |
| *any other* | e.g. `IOCTL_HID_GET_INPUT_REPORT 0x000B01A2`, `IOCTL_HID_SET_OUTPUT_REPORT 0x000B0195`, `IOCTL_HID_GET_INDEXED_STRING 0x000B01E2`, `IOCTL_GET_PHYSICAL_DESCRIPTOR 0x000B019A`, any `IOCTL_INTERNAL_USB_*` from above | | — | `STATUS_NOT_SUPPORTED` (0xC00000BB), Information 0, **not forwarded** |

The explicit cases at Queue.c:165-171 are identical to `default:`. A Rust `match` only needs the six handled codes plus `_ => STATUS_NOT_SUPPORTED`.

**Buffer conventions.** These are KMDF behaviors the Rust port must reproduce.

- **METHOD_NEITHER internal IOCTLs** (descriptor, attributes, report descriptor, read report). KMDF exposes `Irp->UserBuffer` as the request's *output* buffer, with length `IrpSp->Parameters.DeviceIoControl.OutputBufferLength`. So `WdfRequestRetrieveOutputMemory` and `WdfRequestRetrieveOutputBuffer` work. This is the standard KMDF HID-minidriver pattern.
- **GET_FEATURE / SET_FEATURE.** hidclass sends these as internal IOCTLs with `Irp->UserBuffer = PHID_XFER_PACKET` and no MDL, despite the nominal DIRECT method. So the driver **bypasses WDF buffer retrieval** and reads `WdfRequestWdmGetIrp(Request)->UserBuffer` directly (Hid.c:319, 455).
  - Lengths are taken from `WdfRequestGetParameters` → `Parameters.DeviceIoControl.OutputBufferLength` for GET and `.InputBufferLength` for SET.
- `HID_XFER_PACKET` (`shared/hidclass.h:180-184`), natural alignment, x64/ARM64:

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `reportBuffer` (`PUCHAR`) |
| 8 | 4 | `reportBufferLen` (`ULONG`) |
| 12 | 1 | `reportId` (`UCHAR`) |
| — | — | `sizeof = 16` |

### 4.3 READ_REPORT flow

1. **Enqueue.** `AmtPtpDispatchReadReportRequests` (Queue.c:184-219) calls `WdfRequestForwardToIoQueue(Request, ctx->InputQueue)`.
   - On success: `*Pending = TRUE`, so the dispatcher does not complete the request.
   - On failure: the failure status is returned and the dispatcher completes the request with it.
   - **No buffer-length validation at enqueue time.**
2. **Dequeue.** This happens only in the reader completion (Interrupt.c:140): one request per *valid* USB packet, FIFO, via `WdfIoQueueRetrieveNextRequest`. If none is queued, the USB packet is **discarded**. There is no input buffering or coalescing.
3. **Completion outcomes:**
   - `STATUS_SUCCESS`, `Information = 50` (normal).
   - `STATUS_DATA_ERROR` (0xC000003E), `Information = 0` (more than 5 contacts).
   - Never completed (**BUG leaks**, §10 B6/B7). Once `WdfIoQueueRetrieveNextRequest` hands a request to the driver, the driver owns it, and it is **not cancelable** by default (MS Learn, "Revise Canceled Request Logic"). So a requestor's cancel has no effect, and nothing ever completes the request. Each leak permanently consumes one of hidclass's outstanding ping-pong reads. Teardown is also expected to block. A purge of InputQueue waits for driver-owned requests (WdfIoQueuePurgeSynchronously "returns after all the unprocessed and driver-owned requests are completed or canceled"), and InputQueue has no EvtIoStop. So PnP remove or disable can hang. The hang is inferred from the documented purge semantics, not observed.
   - Or completed `STATUS_CANCELLED` by WDF on cancel or removal. This applies **only** to requests still sitting in InputQueue (framework-owned).

---

## 5. HID

### 5.1 HID descriptor (`AmtPtpT2DefaultHidDescriptor`, Hid.c:12-22)

The type is `HID_DESCRIPTOR` (WDK `km/hidport.h:126-144`, inside `#include <pshpack1.h>`, so **packed, 9 bytes**).

| Field | Value |
|---|---|
| `bLength` | `0x09` |
| `bDescriptorType` | `0x21` |
| `bcdHID` | `0x0100` |
| `bCountry` (the source comment says `bCountryCode`) | `0x00` |
| `bNumDescriptors` | `0x01` |
| `DescriptorList[0].bReportType` | `0x22` |
| `DescriptorList[0].wReportLength` | `sizeof(AmtPtpT2ReportDescriptor)` = **493 = 0x01ED** |

**Wire bytes: `09 21 00 01 00 01 22 ED 01`** **[KEEP]**

`AmtPtpGetHidDescriptor` (Hid.c:26-119):

1. `WdfRequestRetrieveOutputMemory(Request, &mem)`. On failure, return that status.
2. `switch (ctx->DeviceDescriptor.idProduct)`. The cases `027A/027B/027C/027D` and `default` are **byte-identical**; `default` only adds a WARNING trace. **[DROP]** the switch.
3. `WdfMemoryCopyFromBuffer(mem, 0, &AmtPtpT2DefaultHidDescriptor, bLength = 9)`. This fails (STATUS_BUFFER_TOO_SMALL) if the output buffer is under 9 bytes; that status is returned.
4. `WdfRequestSetInformation(Request, 9)`. Return STATUS_SUCCESS.

### 5.2 Report descriptor (`AmtPtpT2ReportDescriptor`, Hid.c:7-10)

The array is declared as `HID_REPORT_DESCRIPTOR AmtPtpT2ReportDescriptor[]`, where `HID_REPORT_DESCRIPTOR` is a `UCHAR` typedef (Hid.h:12). It is a **non-const global in `.data`**. Its contents are:

```
{ AAPL_WELLSPRING_T2_PTP_TLC            /* include/metadata/WellspringT2.h:84-138 */,
  AAPL_PTP_WINDOWS_CONFIGURATION_TLC    /* include/Hid.h:29-54 */ }
```

The item-tag macros come from `include/hid/HidCommon.h:14-39`, and the report IDs from HidCommon.h:3-9. **Total length: 493 bytes.** The full hex is in Appendix A; it was generated by compiling the real headers.

`AmtPtpGetReportDescriptor` (Hid.c:169-285):

1. `RetrieveOutputMemory`. On failure, return.
2. The identical-branch switch (**[DROP]**).
3. `szCopy = AmtPtpT2DefaultHidDescriptor.DescriptorList[0].wReportLength` (493).
   - If 0: `STATUS_INVALID_DEVICE_STATE` (**[DROP]**: it is a constant 493).
4. `WdfMemoryCopyFromBuffer(mem, 0, AmtPtpT2ReportDescriptor, 493)`. This fails if the output is under 493 bytes.
5. `SetInformation(493)`, then STATUS_SUCCESS.

**Layout map:**

| Offset | Len | Content |
|---|---|---|
| 0 | 10 | `05 0D` UsagePage Digitizer; `09 05` Usage Touch Pad; `A1 01` Collection(Application); `85 05` ReportID 5; `09 22` Usage Finger |
| 10 | 70 | Finger #1 = `AAPL_WELLSPRING_T2_PTP_FINGER_COLLECTION_1` (WellspringT2.h:5-44) |
| 80 | 4 | `05 0D 09 22` |
| 84 | 70 | Finger #2 = COLLECTION_1 |
| 154 | 4 | `05 0D 09 22` |
| 158 | 64 | Finger #3 = `..._FINGER_COLLECTION_2` (WellspringT2.h:46-82) |
| 222 | 4 | `05 0D 09 22` |
| 226 | 70 | Finger #4 = COLLECTION_1 |
| 296 | 4 | `05 0D 09 22` |
| 300 | 64 | Finger #5 = COLLECTION_2 |
| 364 | 82 | Tail: scan time, contact count, button, Feature 0x07 (caps), Feature 0x08 (HQA), `C0` end of the TLC |
| 446 | 47 | Configuration TLC: Feature 0x04 (input mode), Feature 0x06 (function switch) |

**Finger collection 1 (70 bytes), item by item:**

```
A1 02            Collection (Logical)
25 01            Logical Max 1
09 47            Usage Confidence
09 42            Usage Tip Switch
95 02 75 01 81 02   Count 2, Size 1, Input(Data,Var,Abs)      -> bit0 Confidence, bit1 Tip
75 01 95 06 81 03   Size 1, Count 6, Input(Const)             -> bits 2..7 padding
95 01 75 20      Count 1, Size 32
27 FF FF FF FF   Logical Max (4-byte) 0xFFFFFFFF
09 51 81 02      Usage Contact Identifier, Input           -> 4 bytes
05 01            UsagePage Generic Desktop
26 20 4E         Logical Max 20000
75 10            Size 16
55 0E            Unit Exponent -2
65 11            Unit cm (SI linear length)
09 30            Usage X
46 14 05         Physical Max 1300
95 01 81 02      Count 1, Input                              -> X u16
46 52 03         Physical Max 850
26 E0 2E         Logical Max 12000
09 31 81 02      Usage Y, Input                              -> Y u16
45 00 55 00 65 00   Physical Max 0, Unit Exp 0, Unit none   (reset tail; ABSENT in COLLECTION_2)
C0               End Collection
```

**Finger collection 2 (64 bytes)** is collection 1 **minus** the 6-byte reset tail `45 00 55 00 65 00`.

QUIRK: the comments at WellspringT2.h:74 and 77 claim physical maxima of 1045 and 750, but the bytes are `0x0514` (1300) and `0x0352` (850), identical to collection 1. **Port the bytes, not the comments.**

**Tail (offset 364-445):**

```
05 0D               UsagePage Digitizer
55 0C               Unit Exponent -4
66 01 10            Unit (2-byte) 0x1001 = seconds
47 FF FF 00 00      Physical Max 65535
27 FF FF 00 00      Logical Max 65535
09 56 81 02         Usage Scan Time, Input            -> u16 (Size 16 / Count 1 inherited)
09 54               Usage Contact Count
25 7F               Logical Max 127
75 08 81 02         Size 8, Input                     -> u8
05 09 09 01         UsagePage Button, Usage Button 1
25 01 75 01 81 02   Logical Max 1, Size 1, Input      -> bit0
95 07 81 03         Count 7, Input(Const)             -> 7 pad bits
05 0D               UsagePage Digitizer
85 07               Report ID 7 (DEVICE_CAPS)
09 55 09 59         Usage Contact Count Maximum, Usage Pad Type (button type)
15 00 26 FF 00      Logical Min 0, Logical Max 255
75 08 95 02 B1 02   Size 8, Count 2, Feature           -> 2 bytes
06 00 FF            UsagePage (2-byte) 0xFF00 vendor
85 08               Report ID 8 (PTPHQA)
09 C5               Usage 0xC5 (certification blob)
15 00 26 FF 00      Logical 0..255
75 08 96 00 01 B1 02   Size 8, Count 256, Feature      -> 256 bytes
C0                  End Collection (touch pad TLC)
```

**Configuration TLC (offset 446-492):**

```
05 0D 09 0E A1 01   Digitizer / Device Configuration / Collection(Application)
85 04               Report ID 4 (REPORTMODE)
09 22 A1 02         Usage Finger, Collection(Logical)
09 52               Usage Input Mode
15 00 25 10         Logical Min 0, Logical Max 16 (MAX_FINGERS; Hid.h:38)
75 08 95 01 B1 02   Size 8, Count 1, Feature          -> 1 byte
C0
A1 00               Collection(Physical)
85 06               Report ID 6 (FUNCSWITCH)
09 57 09 58         Usage Button Switch (0x57), Usage Surface Switch (0x58)
75 01 95 02 25 01 B1 02   Size 1, Count 2, Logical Max 1, Feature -> bit0 Button, bit1 Surface
95 06 B1 03         Count 6, Feature(Const)           -> padding
C0 C0
```

**Resulting reports.** Byte counts include the report ID byte.

| Report | Type | Bytes | Fields |
|---|---|---|---|
| 0x05 | Input | 50 | 5 × {flags u8, ContactID u32, X u16, Y u16} + ScanTime u16 + ContactCount u8 + button byte. See §7.3. |
| 0x07 | Feature | 3 | MaxContacts u8, ButtonType u8 |
| 0x08 | Feature | 257 | 256-byte blob (vendor page 0xFF00, usage 0xC5) |
| 0x04 | Feature | 2 | InputMode u8 |
| 0x06 | Feature | 2 | bit0 ButtonSwitch, bit1 SurfaceSwitch |

**Descriptor quirks.** Keep the bytes; these are listed so nobody "fixes" them by accident.

- The Contact ID Logical Max is encoded as 4-byte `0xFFFFFFFF`, which a strict parser reads as signed −1 (< Logical Min 0).
- Logical Minimum is never set in the input part, so it stays 0 (the initial global state).
- Contact Count and the button items inherit Unit = seconds, Unit Exponent −4 and Physical Max 65535 from Scan Time, because the globals are not reset. The same globals carry into Feature 0x07, Feature 0x08 and the whole configuration TLC (Features 0x04 and 0x06), because nothing after the tail resets them.
- Finger #3 uses COLLECTION_2, which lacks the reset tail. So finger #4's Confidence/Tip bits and Contact ID inherit Unit = cm, Unit Exponent −2 and Physical Max 850 from finger #3's Y item. Fingers #1, #2 and #5 do not have this quirk: #1 starts from the initial state, and #2 and #5 follow a COLLECTION_1 reset.
- Input Mode Logical Max is 16, though only 0 and 3 are meaningful.
- **[DROP]** `AAPL_PTP_USERMODE_CONFIGURATION_APP_TLC` (Hid.h:16-27; 23 bytes; Report ID 0x09, vendor page 0xFF00, 3 bytes) is defined but **not** included in the descriptor.

### 5.3 Device attributes (`AmtPtpGetDeviceAttribs`, Hid.c:121-167)

1. `WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_DEVICE_ATTRIBUTES) = 32, &p, NULL)`. On failure, return its status (for example STATUS_BUFFER_TOO_SMALL).
2. Fill the buffer. The layout is `HID_DEVICE_ATTRIBUTES` (`km/hidport.h:108-123`).

| Offset | Field | Value |
|---|---|---|
| 0 | `Size` (ULONG) | 32 |
| 4 | `VendorID` | `DeviceDescriptor.idVendor` (0x05AC) |
| 6 | `ProductID` | `DeviceDescriptor.idProduct` (the real PID, not the table entry) |
| 8 | `VersionNumber` | `DEVICE_VERSION` = **0x0001** (Hid.h:14). Constant, not `bcdDevice`. |
| 10..31 | `Reserved[11]` | **Not written.** Whatever was in the caller's buffer remains. |

3. `WdfRequestSetInformation(Request, 32)`. Return STATUS_SUCCESS.

### 5.4 Strings

**None.** `IOCTL_HID_GET_STRING` and `IOCTL_HID_GET_INDEXED_STRING` complete with `STATUS_NOT_SUPPORTED`.

### 5.5 GET_FEATURE (`AmtPtpReportFeatures`, Hid.c:287-418)

1. `WdfRequestGetParameters`. If `Parameters.DeviceIoControl.OutputBufferLength < sizeof(HID_XFER_PACKET)` (16), return **`STATUS_BUFFER_TOO_SMALL`** (0xC0000023).
2. `pkt = (PHID_XFER_PACKET) WdfRequestWdmGetIrp(Request)->UserBuffer`. If NULL, return **`STATUS_INVALID_DEVICE_REQUEST`** (0xC0000010).
3. `switch (pkt->reportId)`:
   - **0x07 (`REPORTID_DEVICE_CAPS`)** (Hid.c:328-367):
     - If `pkt->reportBufferLen < 3`, return **`STATUS_INVALID_BUFFER_SIZE`** (0xC0000206).
     - Otherwise write `reportBuffer[1] = 5` (`PTP_MAX_CONTACT_POINTS`, Hid.h:90), `reportBuffer[2] = 0` (`PTP_BUTTON_TYPE_CLICK_PAD`, Hid.h:91), then `reportBuffer[0] = 0x07`. Return SUCCESS.
   - **0x08 (`REPORTID_PTPHQA`)** (Hid.c:368-397):
     - If `reportBufferLen < 257`, return `STATUS_INVALID_BUFFER_SIZE`.
     - Otherwise execute `*certReport->CertificationBlob = DEFAULT_PTP_HQA_BLOB;` (Hid.c:388). **BUG [DECIDE]**: the macro is a comma-separated byte list, so the statement parses as `(blob[0] = 0xFC), 0x28, 0xFE, ...;`. **Only `reportBuffer[1] = 0xFC` is written.** `reportBuffer[2..256]` keep their prior contents (hidclass's buffer, usually zero).
     - Then `reportBuffer[0] = 0x08`. Return SUCCESS.
     - The intended 256-byte blob is in Hid.h:56-88 and Appendix C.
     - Identical behavior means writing exactly two bytes: `[0]=0x08`, `[1]=0xFC`.
   - **Any other ID**: return `STATUS_NOT_SUPPORTED`.
4. **`WdfRequestSetInformation` is never called, so the request completes with `Information = 0`** even on success. **[KEEP]/[DECIDE]**
5. There is no NULL check on `pkt->reportBuffer`. The writes go directly into the hidclass kernel buffer.

### 5.6 SET_FEATURE (`AmtPtpSetFeatures`, Hid.c:420-559)

1. If `Parameters.DeviceIoControl.InputBufferLength < 16`, return `STATUS_BUFFER_TOO_SMALL`.
2. `pkt = Irp->UserBuffer`. If NULL, return `STATUS_INVALID_DEVICE_REQUEST`.
3. `switch (pkt->reportId)`:
   - **0x04 (`REPORTID_REPORTMODE`)** (Hid.c:469-518). The report is read through `PTP_DEVICE_INPUT_MODE_REPORT` = `{ReportID u8, Mode u8}` (Hid.h:111-114), so `Mode = reportBuffer[1]`. **There is no `reportBufferLen` check.** Snapshot `wasOn = ctx->IsWellspringModeOn`.
     - `Mode == 0` (`PTP_COLLECTION_MOUSE`): return **`STATUS_NOT_SUPPORTED`**.
     - `Mode == 3` (`PTP_COLLECTION_WINDOWS`): if `!wasOn`, call `status = AmtPtpSetWellspringMode(ctx, TRUE)`, and on failure return that status. If it was already on, do nothing. Return SUCCESS.
     - **Any other Mode value**: no `default:` case, so **SUCCESS with no action**.
   - **0x06 (`REPORTID_FUNCSWITCH`)** (Hid.c:519-540): **only traces.**
     - It traces `ButtonReport` and `SurfaceReport` through `PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` = `{ReportID, DeviceMode, ButtonReport:1, SurfaceReport:1, Padding:6}` (Hid.h:116-125, pack 1, 3 bytes). It therefore reads bits 0 and 1 of **`reportBuffer[2]`**.
     - BUG (trace only): the descriptor puts those bits in `reportBuffer[1]`, and the report is only 2 bytes long.
     - **`PtpReportButton` and `PtpReportTouch` are NOT modified, so selective reporting is ignored.** Return SUCCESS. **[KEEP]** (no-op success)
   - **Any other ID**: return `STATUS_NOT_SUPPORTED`.
4. Information is never set, so it is 0.

IRQL: `AmtPtpSetWellspringMode` allocates paged pool and sends a synchronous USB transfer, so it requires PASSIVE_LEVEL. The queue imposes no execution level; the code relies on hidclass issuing SET_FEATURE at PASSIVE.

### 5.7 Not implemented

**None of the following exist:** latency mode, battery report, user-mode configuration report (0x09), mouse-collection fallback (input mode 0 is rejected), surface/button selective reporting (accepted but ignored), input reports via GET_INPUT_REPORT, and output reports.

---

## 6. Device I/O to the hardware

### 6.1 USB selection

See §2.10. In summary: a single-interface config selection, then the first `WdfUsbPipeTypeInterrupt` pipe, with `SetNoMaximumPacketSizeCheck` applied to every pipe visited. No alternate-setting changes and no pipe reset or abort by the driver. The only reset is the framework's own, after ReadersFailed.

### 6.2 Continuous reader (`AmtPtpConfigContReaderForInterruptEndPoint`, Interrupt.c:14-87)

`transferLength` by `tp_type` (Interrupt.c:30-47) is `HEADER_TYPEn + FSIZE_TYPEn * MAX_FINGERS(16)`:

| Type | Header | FSize | Transfer length |
|---|---|---|---|
| TYPE1 | 26 | 28 | 474 |
| TYPE2 | 30 | 28 | 478 |
| TYPE3 | 38 | 28 | 486 |
| **TYPE4** | **46** | **30** | **526** (the only type used) |
| TYPE5 | 12 | 9 | 156 |

- `if (transferLength <= 0)` (size_t, so effectively `== 0`) returns `STATUS_UNKNOWN_REVISION`. **[DROP]**: every enum value is covered.
- `WDF_USB_CONTINUOUS_READER_CONFIG_INIT(&cfg, AmtPtpEvtUsbInterruptPipeReadComplete, ctx, 526)` resolves to:
  - `Size`
  - `EvtUsbTargetPipeReadComplete`
  - `EvtUsbTargetPipeReadCompleteContext = ctx` (a raw `PDEVICE_CONTEXT`, passed back as `WDFCONTEXT`)
  - `TransferLength = 526`
  - `HeaderLength = 0`, `TrailerLength = 0`
  - **`NumPendingReads = 0`**, meaning the framework default. The open-source WDF sources use 2; the documentation only says "framework default". Nothing in this driver depends on the number.
  - `BufferAttributes = NULL`
  - `EvtUsbTargetPipeReadersFailed = AmtPtpEvtUsbInterruptReadersFailed` (set explicitly at Interrupt.c:61)
- `WdfUsbTargetPipeConfigContinuousReader(ctx->InterruptPipe, &cfg)`. A failure is traced, but **the function returns `STATUS_SUCCESS` unconditionally** (Interrupt.c:86). **BUG [DECIDE]**: PrepareHardware then succeeds, the target starts with no reader, and the device produces no input.
  - Documented failures this masks include `STATUS_INVALID_BUFFER_SIZE` and `STATUS_INVALID_DEVICE_REQUEST`. `STATUS_INVALID_BUFFER_SIZE` occurs because 526 is not a multiple of any realistic MaxPacketSize; it is avoided only because `SetNoMaximumPacketSizeCheck` was already called in §2.10, which is order-dependent. `STATUS_INVALID_DEVICE_REQUEST` occurs for a wrong pipe type or direction.
- `AmtPtpEvtUsbInterruptReadersFailed` (Interrupt.c:252-264) returns **TRUE**: "framework, reset the pipe and restart the readers". It retries indefinitely.
- The reader is started in D0Entry (`WdfIoTargetStart`) and stopped in D0Exit (`WdfIoTargetStop(..., WdfIoTargetCancelSentIo)`).

### 6.3 Wellspring mode switch (`AmtPtpSetWellspringMode`, Device.c:503-639)

Inputs: `ctx`, `on: BOOLEAN`. **PASSIVE_LEVEL only.**

| Step | Action |
|---|---|
| 0 | If `DeviceInfo->tp_type == TYPE3`: `ctx->IsWellspringModeOn = on`, return SUCCESS (Device.c:525-528). **[DROP]**: no TYPE3 rows exist. |
| 1 | `WdfMemoryCreate(WDF_NO_OBJECT_ATTRIBUTES, PagedPool, POOL_TAG_PTP_CONTROL, um_size = 2, &mem, &buf)`. The tag `'PTPC'` is an MSVC multichar literal = **0x50545043** (Device.h:59), which shows as "CPTP" in pool dumps. On failure, `goto cleanup`, which calls **`WdfObjectDelete(NULL)`**: **BUG**. NULL is an invalid handle, and WdfObjectDelete documents "A bug check occurs if the driver supplies an invalid object handle". Expect a 0x10D WDF_VIOLATION bug check in all builds, not just a verifier break. **PagedPool** is a second problem: this buffer becomes the URB `TransferBuffer` of a control transfer, which the URB contract expects to be resident. Fix freely by using NonPagedPoolNx; there is no observable change. |
| 2 | `RtlZeroMemory(buf, um_size = 2)`. |
| 3 | `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER(&md, buf, sizeof(DeviceInfo->um_size))`. **BUG [DECIDE]**: `sizeof(int)` = **4**, not `um_size` = 2 (Device.c:551). The descriptor covers 4 bytes of a 2-byte allocation. |
| 4 | **Read** setup packet (Device.c:554-564). `WDF_USB_CONTROL_SETUP_PACKET_INIT(Dir=DeviceToHost, Recipient=Interface, bRequest=1, wValue=um_req_val, wIndex=um_req_idx)` sets Type=Standard; the code then overrides `bm.Request.Type = BmRequestClass`. Result: **`A1 01 02 03 02 00 04 00`**: bmRequestType 0xA1 (D2H \| Class \| Interface), bRequest 0x01 (HID GET_REPORT, `BCM5974_WELLSPRING_MODE_READ_REQUEST_ID`), wValue 0x0302 (Feature report, ID 2), wIndex 0x0002 (interface 2), wLength 0x0004 (from the descriptor length). |
| 5 | `WdfUsbTargetDeviceSendControlTransferSynchronously(UsbDevice, WDF_NO_HANDLE, NULL /*no options → no timeout*/, &setup, &md, &cb)`. On failure, cleanup and return the status. **`cb` (bytes transferred) is never checked** (comment at Device.c:575-576). Because wLength is 4, the device may legally return up to **4 bytes into the 2-byte allocation**, a 2-byte pool **overrun on the read**. That is in addition to the over-read on the write in step 7. The trace prints the ULONG `cb` with `%llu` (format mismatch, trace only). |
| 6 | `buf[um_switch_idx = 1] = on ? um_switch_on (0x01) : um_switch_off (0x00)`. |
| 7 | **Write** setup packet (Device.c:595-605). Result: **`21 09 02 03 02 00 04 00`**: bmRequestType 0x21 (H2D \| Class \| Interface), bRequest 0x09 (HID SET_REPORT, `..._WRITE_REQUEST_ID`), wValue 0x0302, wIndex 2, wLength 4. Data stage: **4 bytes** `[buf[0], on?1:0, buf[2], buf[3]]`. `buf[0]` is whatever the device returned (expected `0x02`, the report ID). `buf[2..3]` lie **past the 2-byte allocation**: device-written if the read returned more than 2 bytes, otherwise uninitialized or adjacent memory. |
| 8 | Send synchronously, the same way as step 5. On failure, cleanup and return the status. `IsWellspringModeOn` is **unchanged**. |
| 9 | `ctx->IsWellspringModeOn = on`. |
| 10 | `cleanup:` `WdfObjectDelete(mem)`. Return the status. |

Porting guidance for steps 3 and 7:

- To reproduce the wire traffic safely, allocate **4 zeroed bytes** and use wLength = 4. Bytes 2-3 are then zero (or device-provided) instead of undefined.
- The alternative fix is wLength = 2, which matches Linux `bcm5974`.
- This is a **[DECIDE]**: the hardware evidently accepts the 4-byte form today.

Callers:

- D0Entry, with on=TRUE (errors ignored).
- D0Exit, with on=FALSE (its error is returned).
- SET_FEATURE report 0x04 with Mode=3, with on=TRUE, only when currently off.

### 6.4 Everything else

- **SPI / HID lower IOCTLs**: none. This is USB only. The other projects (SpiKm, UsbUm) are separate.
- **Timers, work items, DPCs**: none.
- **Selective suspend / idle**: none. There is no `WdfDeviceAssignS0IdleSettings`, and `IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST` is rejected.
- **Wake**: none. Remote-wake capability is only traced.

### 6.5 Lifecycle summary

| Phase | Actions (in order) |
|---|---|
| DeviceAdd | Mark as filter. Make the bogus PDO call. Set PnP/power callbacks. Create the device with an 80-byte context. Set flags = TRUE. Register the device interface. Create the default parallel queue (internal IOCTL + stop callbacks) and the manual InputQueue (not power-managed). |
| PrepareHardware | Create the USB target (first time only). Read the device descriptor. Choose the config row by PID. Retrieve USB info (traits). Select the single interface and find the interrupt pipe. Configure the continuous reader (526 B). Set flags = TRUE. |
| D0Entry | Wellspring ON (best-effort). `LastReportTime` = QPC frequency. Start the pipe I/O target; a failure fails D0Entry. |
| Running | Reader completions turn packets into READ_REPORT completions. SET_FEATURE 0x04/3 may re-enable wellspring mode. |
| D0Exit | Stop the pipe target (cancel sent I/O). Wellspring OFF. Return the OFF status. |
| Removal | D0Exit(D3Final). WDF purges InputQueue (STATUS_CANCELLED) and deletes objects. There is no ReleaseHardware. |

---

## 7. Input parsing

### 7.1 Raw packet format (TYPE4, the only format any table row uses)

| Region | Offset (bytes) | Notes |
|---|---|---|
| Header | 0 .. 45 | `tp_header = HEADER_TYPE4 = 23*2 = 46` (AppleDefinition.h:33) |
| Button | 31 | `tp_button = BUTTON_TYPE4 = 31` (AppleDefinition.h:40). Nonzero means clicked. |
| Finger block *i* | 46 + 30·i .. 75 + 30·i | `tp_fsize = FSIZE_TYPE4 = 15*2 = 30` (AppleDefinition.h:50) |
| Finger struct *i* | **48 + 30·i** .. 75 + 30·i (28 bytes) | `tp_delta = DELTA_TYPE4 = 2` (AppleDefinition.h:57). The struct exactly fills the rest of the block. |
| Max length | 526 = 46 + 16·30 | The reader buffer size |

`struct TRACKPAD_FINGER` (AppleDefinition.h:78-92) is `__declspec(align(2))`, made of 14 × `USHORT`, little-endian:

| Struct offset | Absolute offset (finger *i*) | Field | Used? | Interpretation in parser |
|---|---|---|---|---|
| +0 | 48+30i | `origin` | no | — |
| +2 | 50+30i | `abs_x` | **yes** | `i16` (via `AmtRawToInteger`: `(int)(signed short)`) |
| +4 | 52+30i | `abs_y` | **yes** | `i16` |
| +6 | 54+30i | `rel_x` | no | — |
| +8 | 56+30i | `rel_y` | no | — |
| +10 | 58+30i | `tool_major` | no | — |
| +12 | 60+30i | `tool_minor` | no | — |
| +14 | 62+30i | `orientation` | no | — |
| +16 | 64+30i | `touch_major` | **yes** | `i16` |
| +18 | 66+30i | `touch_minor` | **yes** | `i16` |
| +20 | 68+30i | `unused[2]` | no | — |
| +24 | 72+30i | `pressure` | no | — |
| +26 | 74+30i | `multi` | no | — |

### 7.2 Algorithm (`AmtPtpEvtUsbInterruptPipeReadComplete`, Interrupt.c:89-250), step by step

Notation: `N = NumBytesTransferred` (size_t), `buf` = reader buffer, `cfg = ctx->DeviceInfo`, `H = (size_t)(unsigned)cfg->tp_header = 46`, `F = (size_t)(unsigned)cfg->tp_fsize = 30`. `Context` is the `PDEVICE_CONTEXT` given at reader configuration; `Pipe` is unused.

1. **Length gate** (Interrupt.c:115-123). If `N < H || (N - H) % F != 0`, trace and **return**. The packet is dropped and no request is touched. Valid N values are 46, 76, 106, ..., 526.
2. `buf = WdfMemoryGetBuffer(Buffer, NULL)`. If NULL, return. **[DROP]**: it never happens.
3. `WdfIoQueueRetrieveNextRequest(ctx->InputQueue, &req)`. On failure (no pended read), return: **the packet is lost**.
4. `WdfRequestRetrieveOutputMemory(req, &mem)`. On failure, return **without completing `req`**. **BUG [DECIDE]**: the request leaks **permanently**. A driver-owned request from `RetrieveNextRequest` is not cancelable, so requestor cancel does nothing, and InputQueue teardown at removal is expected to block on it (§4.3).
5. Zero a 50-byte `PTP_REPORT`. Set `ReportID = 0x05` and `IsButtonClicked = 0`.
6. `raw_n = (N - H) / F`, the number of finger blocks (0..16).
7. `f_base = buf + H + cfg->tp_delta` = `buf + 48`.
8. **ScanTime** (Interrupt.c:177-183):

   ```
   KeQueryPerformanceCounter(&CurrentPerfCounter);   // return value (the counter) DISCARDED;
                                                    // CurrentPerfCounter := QPC FREQUENCY
   delta = (CurrentPerfCounter.QuadPart - ctx->LastReportTime.QuadPart) / 100;   // LONGLONG
   if (delta > 0xFF) delta = 0xFF;
   ScanTime = (USHORT)delta;
   ```

   `ctx->LastReportTime` also holds the QPC frequency (Device.c:319, same misuse). The frequency is constant for the boot session, so **`delta == 0` and ScanTime is always 0**. **BUG [DECIDE]**.
   - Identical behavior: ScanTime = 0.
   - Clearly intended: (now − last)/100 clamped to 255. Even the intent is wrong: the units are 100 QPC ticks while the descriptor declares 100 µs, `LastReportTime` would never be updated per report, and the field is clamped to 8 bits despite being 16 bits.
   - A correct PTP port would use 100 µs units, wrap at 65536 and update per report.
9. **Contacts** (Interrupt.c:185-215). `PtpReportTouch` is always TRUE.
   1. `if (raw_n >= 5) raw_n = 5;` (`PTP_MAX_CONTACT_POINTS`).
   2. `if (raw_n * F < (N - H))`, that is when the packet had **6 or more** fingers: **`WdfRequestComplete(req, STATUS_DATA_ERROR)`** with Information 0, then **return**. **[KEEP]/[DECIDE]**: with six or more fingers on the pad, every read fails, and hidclass sees a stream of failed reads.
   3. `ContactCount = (UCHAR)raw_n`.
   4. For each `i` in `0 .. raw_n`, with all arithmetic in C `int` (i32):

      ```
      p    = f_base + i*F
      ax   = (i32)(i16)LE16(p+2)      // abs_x
      ay   = (i32)(i16)LE16(p+4)      // abs_y
      tmaj = (i32)(i16)LE16(p+16)     // touch_major
      tmin = (i32)(i16)LE16(p+18)     // touch_minor

      X = (ax - cfg.x.min) > 0 ? (u16)(ax - cfg.x.min) : 0     // lower clamp only
      Y = (cfg.y.max - ay) > 0 ? (u16)(cfg.y.max - ay) : 0     // Y inverted; lower clamp only
      Contacts[i].ContactID  = (u32)(u8)i                       // slot index, NOT a tracking id
      Contacts[i].X = X ; Contacts[i].Y = Y
      Contacts[i].TipSwitch  = ((tmaj << 1) >= 200) || ((tmin << 1) >= 150)
      Contacts[i].Confidence = ((tmin << 1) > 0)
      ```

      - No upper clamp is applied. Descriptor logical maxima are X 20000 and Y 12000.
        - Fallback config: X ∈ [0, 42767] and Y ∈ [0, 42768] for arbitrary i16 input. Within the nominal sensor range, X ≤ 20000 and Y ≤ 12000.
        - 13-inch config (027A/027B): nominal X ≤ 12992 and Y ≤ 7855.
        - The `(u16)` cast never wraps, because the maximum is 32767 + 10000 < 65536.
      - TipSwitch equals `tmaj >= 100 || tmin >= 75` for non-negative values.
        - For negative values, C left-shift is UB; MSVC produces a negative product, so the result is false.
        - In Rust, use `tmaj * 2 >= 200`. There is no overflow, because |v| ≤ 32768.
      - Confidence equals `tmin > 0`.
      - The flags byte is `Confidence | (TipSwitch << 1)`; bits 2..7 are 0.
   5. Not used: `origin` (no persistent contact identity), `pressure`, `orientation`, `tool_*`, `rel_*`, `multi`.
      - No defuzz (`// TODO: Implement defuzz later`, Interrupt.c:207-208).
      - No palm rejection, no size or pressure qualification. The `*_QUALIFICATION_THRESHOLD` constants at AppleDefinition.h:142-147 are unused.
      - **No state is kept across reports.**
      - QUIRK: when finger slot 0 lifts, the next finger shifts into ContactID 0, so Windows sees an identity change.
10. **Button** (Interrupt.c:217-226). `PtpReportButton` is always TRUE. If `buf[cfg->tp_button] != 0` (byte 31), set `IsButtonClicked = 1`. Only the whole byte is tested.
11. `WdfMemoryCopyFromBuffer(mem, 0, &report, 50)`. On failure (output buffer under 50 bytes), return **without completing**. **BUG [DECIDE]**: a permanent, non-cancelable leak, the same as step 4.
12. `WdfRequestSetInformation(req, 50)`, then `WdfRequestComplete(req, STATUS_SUCCESS)`.

Each valid USB packet produces at most one report. A packet with N = 46 (no finger blocks) yields `ContactCount = 0` plus the button state.

### 7.3 Output report (`PTP_REPORT`, Hid.h:127-145): 50 bytes

`PTP_CONTACT` is `#pragma pack(1)`, 9 bytes. `PTP_REPORT` uses default packing, but its natural layout has no padding (ScanTime at 46 is 2-aligned), so `sizeof == 50`. This was verified by compiling the headers.

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | `ReportID` = 0x05 |
| 1 + 9k (k = 0..4) | 1 | flags: bit0 Confidence, bit1 TipSwitch (MSVC bitfields allocate from the LSB) |
| 2 + 9k | 4 | `ContactID` u32 LE (= k for k < ContactCount) |
| 6 + 9k | 2 | `X` u16 LE |
| 8 + 9k | 2 | `Y` u16 LE |
| 46 | 2 | `ScanTime` u16 LE (always 0, see §7.2) |
| 48 | 1 | `ContactCount` (0..5) |
| 49 | 1 | `IsButtonClicked` (0 or 1; matches descriptor bit0 + 7 pad bits) |

Slots with k ≥ ContactCount are all-zero.

### 7.4 Golden vectors

These were produced by compiling the arithmetic of Interrupt.c:115-226 against the project's real headers (gcc), with ScanTime = 0 as derived in §7.2.

**A. Header only (N = 46), button byte 31 = 0x01, any config.**

```
IN : 46 bytes, all 00 except [31]=01
OUT: 05 00×45 | 00 00 | 00 | 01            (ScanTime 0, ContactCount 0, button 1)
     05 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
     00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 01
```

**B. Two fingers (N = 106), fallback config** (PID 0xFFFF, 027C, 027D, or unknown):

- Finger 0: abs_x = −500 (`0C FE`), abs_y = 3000 (`B8 0B`), touch_major = 120, touch_minor = 50.
- Finger 1: abs_x = −12000 (`20 D1`), abs_y = 11000 (`F8 2A`), touch_major = 10, touch_minor = 0.

```
IN (106 bytes):
  00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
  00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 0c fe b8 0b 00 00 00 00 00 00 00 00 00 00
  78 00 32 00 00 00 00 00 00 00 00 00 00 00 00 00 20 d1 f8 2a 00 00 00 00 00 00 00 00 00 00 0a 00
  00 00 00 00 00 00 00 00 00 00
OUT (50 bytes):
  05 03 00 00 00 00 1c 25 58 1b 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00
  00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02 00
```

Contact 0: flags 0x03, ID 0, X = 9500 (0x251C), Y = 7000 (0x1B58). Contact 1: flags 0x00, ID 1, X = 0 (below min), Y = 0 (above max). ContactCount 2, button 0.

**B'. The same packet, 13-inch config (027A/027B).** Contact 0 gives X = −500 + 6243 = 5743 (0x166F) and Y = 7685 − 3000 = 4685 (0x124D).

```
  05 03 00 00 00 00 6f 16 4d 12 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00
  00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 02 00
```

**C. Six fingers (N = 226).** The pended request is completed with `STATUS_DATA_ERROR` (0xC000003E), Information 0.

**D. N = 47** (`(47 − 46) % 30 ≠ 0`). The packet is dropped, and the pended request stays queued.

---

## 8. Device support tables

### 8.1 `Bcm5974ConfigTable` (AppleDefinition.h:149-214)

`struct BCM5974_CONFIG` (AppleDefinition.h:102-125) is a sequence of `int` fields plus an `enum`, followed by five `struct BCM5974_PARAM {int snratio, min, max}` (AppleDefinition.h:95-99).

`DATAFORMAT(TYPE4)` (AppleDefinition.h:127-134) expands to:

```
tp_type=TYPE4, tp_header=46, tp_datalen=46+16*30=526, tp_button=31, tp_fsize=30, tp_delta=2,
um_size=2, um_req_val=0x302, um_req_idx=2, um_switch_idx=1, um_switch_on=0x1, um_switch_off=0x0
```

**Fields common to all 5 rows:**

| Field | Value |
|---|---|
| `caps` | `HAS_INTEGRATED_BUTTON` = 1 |
| `bt_ep` | 0 |
| `bt_datalen` | `sizeof(TRACKPAD_BUTTON_DATA)` = 4 |
| `tp_ep` | 0x83 |
| `tp_type` | TYPE4 (enum value 3) |
| `tp_header` | 46 |
| `tp_datalen` | 526 |
| `tp_button` | 31 |
| `tp_fsize` | 30 |
| `tp_delta` | 2 |
| `um_size` | 2 |
| `um_req_val` | 0x0302 |
| `um_req_idx` | 2 |
| `um_switch_idx` | 1 |
| `um_switch_on` | 1 |
| `um_switch_off` | 0 |
| `p` | {45, 0, 300} |
| `w` | {25, 0, 2048} |
| `o` | {10, −16384, 16384} |

**Rows that differ:**

| Idx | `identification` | Comment | `x` {sn, min, max} | `y` {sn, min, max} |
|---|---|---|---|---|
| 0 | `0xFFFF` (`USB_DEVICE_ID_DEFAULT_FALLBACK`) | "New device?" generic fallback | {250, −10000, 10000} | {250, −2000, 10000} |
| 1 | `0x027A` | 13 inch | {250, −6243, 6749} | {250, −170, 7685} |
| 2 | `0x027B` | 13 inch | {250, −6243, 6749} | {250, −170, 7685} |
| 3 | `0x027C` | 15 inch | {250, −10000, 10000} | {250, −2000, 10000} |
| 4 | `0x027D` | 15 inch | {250, −10000, 10000} | {250, −2000, 10000} |
| 5 | `0` | terminator `{0}` | — | — |

**Fields read at runtime:** `tp_type`, `tp_header`, `tp_fsize`, `tp_delta`, `tp_button`, `um_size`, `um_req_val`, `um_req_idx`, `um_switch_idx`, `um_switch_on`, `um_switch_off`, `x.min`, `y.max`.

**Every other field is [DROP]:** `caps`, `bt_ep`, `bt_datalen`, `tp_ep`, `tp_datalen`, `p`, `w`, `o`, all `snratio`, `x.max`, `y.min`.

A minimal Rust table is therefore `{pid, x_min: i32, y_max: i32}` plus TYPE4 constants.

### 8.2 Effective VID/PID mapping (INF ∩ table)

All devices use VID `0x05AC`, interface `MI_02`, format TYPE4, the 493-byte descriptor and the 9-byte HID descriptor. The HID attributes report the **real** PID.

| PID | INF line | Config row | X min | Y max |
|---|---|---|---|---|
| 0x0273 | 32 (also UsbUm line 69) | fallback (0) | −10000 | 10000 |
| 0x0274 | 33 (also UsbUm line 70) | fallback | −10000 | 10000 |
| 0x0277 | 34 | fallback | −10000 | 10000 |
| 0x027A | 35 | row 1 | −6243 | 7685 |
| 0x027B | 36 | row 2 | −6243 | 7685 |
| 0x027C | 37 | row 3 (same values as fallback) | −10000 | 10000 |
| 0x027D | 38 | row 4 (same values as fallback) | −10000 | 10000 |
| 0x027E | 39 | fallback | −10000 | 10000 |
| 0x027F | 40 | fallback | −10000 | 10000 |
| 0x0280 | 41 | fallback | −10000 | 10000 |
| 0x0290 | 42 (also UsbUm line 71) | fallback | −10000 | 10000 |
| 0x0291 | 43 (also UsbUm line 72) | fallback | −10000 | 10000 |
| 0x0340 | 44 | fallback | −10000 | 10000 |

### 8.3 Other constants in AppleDefinition.h (format types other than TYPE4 are [DROP])

| Type (enum value) | Header | Button offset | FSize | Delta | USBMSG (size, val, idx, sw_idx, on, off) |
|---|---|---|---|---|---|
| TYPE1 (0) | 26 | 0 | 28 | 0 | 8, 0x300, 0, 0, 1, 8 |
| TYPE2 (1) | 30 | 15 | 28 | 0 | 8, 0x300, 0, 0, 1, 8 |
| TYPE3 (2) | 38 | 23 | 28 | 0 | 8, 0x300, 0, 0, 1, 8 |
| **TYPE4 (3)** | **46** | **31** | **30** | **2** | **2, 0x302, 2, 1, 1, 0** |
| TYPE5 (4) | 12 | 1 | 9 | 0 | 2, 0x302, 1, 1, 1, 0 |

Sources: AppleDefinition.h:21-65. TYPE5's fsize of 9 is smaller than `sizeof(TRACKPAD_FINGER)` = 28, so the struct cast would over-read. It is dead.

Other constants:

- `USB_VENDOR_ID_APPLE` = 0x05AC (line 3); it is not compared at runtime.
- `MAX_FINGERS` = 16 (line 72).
- `MAX_FINGER_ORIENTATION` = 16384 (line 73).
- `BCM5974_MOUSE_SIZE` = 8 (line 75, unused).
- `SN_*` (lines 137-140, table only).
- Thresholds (lines 142-147, unused).
- `struct TRACKPAD_BUTTON_DATA` (lines 13-18, `sizeof` only).

---

## 9. Registry and configuration

**None.**

- No `WdfDriverOpenParametersRegistryKey`, `WdfDeviceOpenRegistryKey`, `WdfRegistry*` or `Zw*Key`. `RegistryPath` is only passed to `WPP_INIT_TRACING` and `WdfDriverCreate`.
- No user-mode configuration interface:
  - The device interface GUID is registered, but no user-mode IOCTL is handled. Opens of the interface land on the hidclass FDO at the top of the stack, which never passes create, close or device-control down to this filter (§1.4).
  - The vendor configuration TLC (Report 0x09, Hid.h:16-27) and `PTP_USERMODEAPP_CONF_REPORT` (Hid.h:147-152) are unused.
- The INF writes only `FriendlyName` and `LowerFilters`.
- No WMI, and no per-device parameters.

---

## 10. Error paths, edge cases, magic numbers, TODOs, dead code, bugs

### 10.1 Bugs and suspicious behavior (flagged, not fixed)

| ID | Location | Description | Tag |
|---|---|---|---|
| B1 | Device.c:319, Interrupt.c:177 | `KeQueryPerformanceCounter(&x)` stores the **frequency** in `x` and discards the counter, so **ScanTime is always 0**. The intended clamp-to-255 and /100 units are also wrong (§7.2 step 8). | DECIDE |
| B2 | Hid.c:388 + Hid.h:56-88 | The HQA blob assignment is a comma expression, so **only `reportBuffer[1] = 0xFC`** is written (§5.5). | DECIDE |
| B3 | Device.c:551 | `sizeof(um_size)` gives 4, so wLength = 4 on a 2-byte buffer. The GET_REPORT read may write up to 2 bytes **past** the allocation (pool overrun), and the SET_REPORT data stage sends 2 bytes from past the allocation (over-read) (§6.3). | DECIDE |
| B4 | Device.c:636 | `WdfObjectDelete(bufHandle)` runs with `bufHandle == NULL` when `WdfMemoryCreate` failed. That is an invalid handle, so expect a 0x10D WDF_VIOLATION bug check (documented "bug check occurs if the driver supplies an invalid object handle"). A low-memory failure therefore crashes the system. | fix freely |
| B5 | Device.c:575-587 | `cbTransferred` is not validated after GET_REPORT (acknowledged workaround). | KEEP |
| B6 | Interrupt.c:158-165 | `WdfRequestRetrieveOutputMemory` failure: the request was dequeued but is **never completed**. It is driver-owned and therefore **not cancelable**, so the leak is permanent. It consumes one hidclass read for good, and InputQueue teardown at removal is expected to block on it (§4.3). | DECIDE (fixing it is strongly advised) |
| B7 | Interrupt.c:236-243 | `WdfMemoryCopyFromBuffer` failure (output under 50 bytes): the request is **never completed**. The leak is permanent and non-cancelable, the same as B6. | DECIDE (fixing it is strongly advised) |
| B8 | Interrupt.c:186-194 | More than 5 fingers: the request is completed with `STATUS_DATA_ERROR`, and every multi-finger (6+) frame fails the read. | KEEP/DECIDE |
| B9 | Interrupt.c:86 | `AmtPtpConfigContReaderForInterruptEndPoint` always returns STATUS_SUCCESS, masking configuration failure (for example `STATUS_INVALID_BUFFER_SIZE` or `STATUS_INVALID_DEVICE_REQUEST`, §6.2) and `STATUS_UNKNOWN_REVISION`. | DECIDE |
| B10 | Device.c:395-415 | D0Exit returns the wellspring-OFF status. It fails on surprise removal or D3Final if the device is gone. It also fails on **ordinary** Dx transitions (sleep, hibernate) whenever the OFF control transfer fails, and KMDF then treats that power-down as failed. | DECIDE |
| B11 | Device.c:303-316 | D0Entry ignores a wellspring-ON failure: the device comes up in mouse mode and emits no valid packets. | KEEP |
| B12 | Driver.c:136 | `WdfPdoInitAllowForwardingRequestToParent` is called on a filter/FDO init (PDO-only API). | DROP |
| B13 | Hid.c:289-418, 420-559 | GET/SET_FEATURE never set Information (0 on success). | KEEP/DECIDE |
| B14 | Hid.c:476-511 | SET_FEATURE 0x04 has no `reportBufferLen` check and reads `reportBuffer[1]`. Mode values other than 0 or 3 succeed silently. | KEEP |
| B15 | Hid.c:526-533 + Hid.h:116-125 | The FUNCSWITCH struct has an extra `DeviceMode` byte, so the traced bits come from `reportBuffer[2]`, an out-of-report read. Selective reporting is ignored entirely. | KEEP (no-op) |
| B16 | Hid.c:346, 387 | GET_FEATURE does not check `reportBuffer != NULL`. | fix freely |
| B17 | Hid.c:153-158 | `HID_DEVICE_ATTRIBUTES.Reserved` is not zeroed. | fix freely (zeroing is invisible) |
| B18 | Interrupt.c:209 | ContactID is the slot index, so there is no contact identity across frames. | KEEP |
| B19 | Interrupt.c:202-205 | X and Y are not clamped to the descriptor logical maxima (20000/12000). | KEEP |
| B20 | Device.c:455-488 | `InterruptPipe` is not reset before re-selection on restart; the first interrupt pipe is taken regardless of direction; `tp_ep` is ignored. | KEEP |
| B21 | WellspringT2.h:74, 77 | The physical-max comments (1045/750) disagree with the bytes (1300/850). COLLECTION_2 omits the unit reset; Contact ID Logical Max is −1 signed. | KEEP bytes |
| B22 | Resource.rc:71 | `FILETYPE VFT_APP` for a `.sys`. | cosmetic |
| B23 | INF 33-43 vs 69-72 | Duplicate HWIDs (0273/0274/0290/0291 MI_02) are bound to both UsbKm and UsbUm. | flag |
| B24 | Hid.c:320, 456 | The only validation of `Irp->UserBuffer` is a NULL check. hidclass is trusted. | KEEP (KM-only caller) |
| B25 | Queue.c:184-219 | READ_REPORT output length is not validated at enqueue; it fails late (B6/B7). | DECIDE |
| B26 | Device.c:581-584 | Trace format `%llu` for a ULONG. | trace only |
| B27 | Device.c:530-537 | The wellspring control-transfer buffer is allocated from **PagedPool** and passed as a buffer-type memory descriptor, which becomes the URB TransferBuffer. URB buffers are expected to be resident. | fix freely (NonPagedPoolNx) |
| B28 | Device.c:469 vs Interrupt.c:64 | Hidden ordering dependency: `SetNoMaximumPacketSizeCheck` must precede `ConfigContinuousReader`. 526 is not a multiple of any realistic MaxPacketSize (8/16/32/64/512/1024), so without it the reader config fails with `STATUS_INVALID_BUFFER_SIZE`, and B9 hides the failure. | KEEP (order) |

### 10.2 Dead code (safe to drop)

| Item | Location |
|---|---|
| `PtpInputOn` field | Device.h:40 |
| `UsbDeviceTraits` and `waitWakeEnable` (stored or traced only) | Device.c:157, 231-246; Device.h:33 |
| `QUEUE_CONTEXT` / `QueueGetContext` | Queue.h:23-29 |
| `DbgIoControlGetString` | DebugUtils.c:31-68; Device.h:109-112 |
| `AmtPtpDeviceUsbKmEvtIoStop` body (trace only) | Queue.c:221-280 |
| Identical `idProduct` switch branches | Hid.c:57-110, 200-276 |
| `wReportLength == 0` checks | Hid.c:208-216, 246-254 |
| `DeviceInfo == NULL` check | Device.c:201-206 |
| `TouchBuffer == NULL` check | Interrupt.c:131-137 |
| `transferLength <= 0` check | Interrupt.c:49-52 |
| `isTargetStarted` cleanup | Device.c:283, 286, 336-350 |
| TYPE3 shortcut | Device.c:523-528 |
| TYPE1/2/3/5 constants and switch cases | AppleDefinition.h; Interrupt.c:32-46 |
| `PtpReportTouch` / `PtpReportButton` gates (always TRUE) | Interrupt.c:185, 217; Device.c:296. Keep the bodies, drop the conditions. |
| Explicit NOT_SUPPORTED cases | Queue.c:165-171 |
| Unused HID macros and structs: `AAPL_PTP_USERMODE_CONFIGURATION_APP_TLC`, `PTP_USERMODEAPP_CONF_REPORT`, `PTP_BUTTON_TYPE_PRESSURE_PAD`, `PTP_CONTACT_CONFIDENCE_BIT`, `PTP_CONTACT_TIPSWITCH_BIT`, `REPORTID_STANDARDMOUSE`, `REPORTID_UMAPP_CONF`, unused item tags in HidCommon.h | Hid.h:16-27, 92, 97-98, 147-152; HidCommon.h:3, 9 |
| Unused config fields and constants | AppleDefinition.h (see §8) |
| `Trace{FLAGS=TRACE_DRIVER}` WPP function | Trace.h:59 |
| `usbdex.lib`, `ntstrsafe.lib` link deps | vcxproj:186 etc. |
| `#ifndef _AAPL_HID_DESCRIPTOR_H_` guard inside a `.c` file | Hid.c:4-24 |

### 10.3 TODOs and comments that admit gaps

- Interrupt.c:207-208: `// Defuzz functions remain the same // TODO: Implement defuzz later`.
- Device.c:575-576: `Behavior mismatch: Actual device does not transfer bytes as expected (in length) So we do not check um_size as a temporary workaround.`
- WellspringT2.h:26 and 67: `/* Size is hard-coded at this moment */` (descriptor logical and physical ranges are not derived from the config table).
- Device.c:437: `// It is slightly different than UMDF`.

### 10.4 Magic numbers

| Value | Meaning | Location |
|---|---|---|
| 0x05 / 0x07 / 0x08 / 0x04 / 0x06 (0x02, 0x09 unused) | Report IDs | HidCommon.h:3-9 |
| 5 | `PTP_MAX_CONTACT_POINTS` (caps report and contact clamp) | Hid.h:90 |
| 0 | ButtonType click-pad | Hid.h:91 |
| 0 / 3 | Input mode Mouse / Windows PTP | Hid.h:94-95 |
| 16 | MAX_FINGERS (reader buffer sizing, Input Mode logical max) | AppleDefinition.h:72 |
| 200, 150 | Tip thresholds applied to `touch_major<<1` and `touch_minor<<1` | Interrupt.c:212 |
| 100, 0xFF | ScanTime divisor and clamp | Interrupt.c:178-181 |
| 1 / 9 | HID GET_REPORT / SET_REPORT bRequest | AppleDefinition.h:68-69 |
| 0x0302 / 2 / idx 1 / 1 / 0 | Wellspring wValue, wIndex, switch byte index, on, off | AppleDefinition.h:64 |
| 'PTPC' = 0x50545043 | Pool tag | Device.h:59 |
| 0x0001 | HID VersionNumber | Hid.h:14 |
| 9, 0x21, 0x0100, 0x22, 493 | HID descriptor fields | Hid.c:12-22 |

### 10.5 Edge cases

- **N == 46** (no fingers): a valid report with ContactCount 0. Button state is still reported.
- **Packet arrives while no READ_REPORT is pending**: silently dropped. There is no "latest state" cache, so a lift-off packet can be lost and Windows keeps a stale contact until the next packet.
- **READ_REPORT arrives in Dx**: it sits in the InputQueue (not power-managed) until input resumes after D0Entry.
- **Cancel of a pended READ_REPORT**: WDF completes it with `STATUS_CANCELLED`; there is no driver callback.
- **Concurrent reader completions** (with more than one pending read): completions are not guaranteed to be serialized, so two packets may be paired with requests in either order. The driver enforces no ordering.
- **SET_FEATURE input mode 3 while the device is in Dx**: the queue is not power-managed, so nothing in this driver gates the request. The synchronous control transfer is sent to the WDFUSBDEVICE target while the device is in Dx. The result depends on the lower USB stack and is **unverified**; failure is likely, and any error is returned to hidclass. MS Learn does not document KMDF stopping the USB device target on Dx (it documents only that local targets are started automatically), so do not assume the request is queued until D0. In practice, hidclass rarely issues SET_FEATURE in Dx, because there is no idle support, so Dx happens only during system sleep.
- **Restart after rebalance**: the USB target is reused; config, pipe and reader are re-established; flags are reset to TRUE; `IsWellspringModeOn` keeps its last value until D0Entry re-runs the switch.

---

## 11. Platform-specific and risky constructs for Rust

| Construct | Where | Rust guidance |
|---|---|---|
| **Comma-operator macro bug** | Hid.c:388 | A naive port copies 256 bytes. Decide explicitly (§5.5). |
| **Out-parameter misuse of `KeQueryPerformanceCounter`** | Device.c:319, Interrupt.c:177 | A naive port computes a real delta. Decide explicitly; identical behavior means ScanTime = 0. |
| **`sizeof(field)` instead of the field's value** | Device.c:551 | wLength = 4. Allocate ≥ 4 bytes zeroed if replicating, from **non-paged** pool (B27). |
| **Hidden call-order dependency** | Device.c:469 before Interrupt.c:64 | Call `WdfUsbTargetPipeSetNoMaximumPacketSizeCheck` on the chosen pipe before `WdfUsbTargetPipeConfigContinuousReader` (B28). Also make the Rust port propagate the reader-config status, or B9 keeps hiding this mistake. |
| **Driver-owned requests are not cancelable** | Interrupt.c:140-165, 229-243 | Every path after `WdfIoQueueRetrieveNextRequest` must complete the request. An early `return` is a permanent leak (B6/B7). In Rust, wrap the retrieved request in a guard that completes it on drop. |
| Bit-fields in a `UCHAR` unit (MSVC allocates from the LSB) | `PTP_CONTACT` (Hid.h:129-136), `PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` (Hid.h:118-124) | Manual bit ops: `flags = conf as u8 \| (tip as u8) << 1`. |
| `#pragma pack(1)` structs | `PTP_CONTACT` (9 B), `PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` (3 B), `HID_DESCRIPTOR` (9 B via pshpack1 in hidport.h) | Prefer serializing into `[u8; N]` with `to_le_bytes`. If using `#[repr(C, packed)]`, never take references to fields (unaligned u32/u16). |
| Default-packed struct with packed members | `PTP_REPORT` (50 B, Hid.h:139-145) | Same as above; assert `size_of == 50`. |
| `__declspec(align(2))` struct cast over a byte buffer | `struct TRACKPAD_FINGER` (AppleDefinition.h:78) cast at `f_base + i*fsize` (Interrupt.c:199) | Read `i16::from_le_bytes(buf[o..o+2])` at the absolute offsets in §7.1. Do not transmute. |
| Signed reinterpretation `(signed short)USHORT` | Interrupt.c:7-12 | `u16 as i16 as i32`. |
| Left shift of a possibly negative `int` (UB in C) | Interrupt.c:212-213 | Use multiplication (`v * 2`) in i32. MSVC semantics are equivalent. |
| Integer promotions | `USHORT idProduct == int identification` (Device.c:35); `int` coordinate math; `size_t` from `(unsigned int)int` (Interrupt.c:100-101); `LONGLONG / 100` truncation | Keep i32 for coordinate math; cast to u16 only after the `> 0` check. |
| MSVC multichar literal | `'PTPC'` (Device.h:59) | `u32::from_be_bytes(*b"PTPC")` = 0x50545043. |
| Comma-list macro composition of byte arrays | WellspringT2.h, Hid.h, HidCommon.h | Emit a `static REPORT_DESCRIPTOR: [u8; 493]` (Appendix A) and unit-test it against the C build. |
| Non-const global descriptor in `.data` | Hid.c:7 | Use `static` (read-only) in Rust; never mutated. |
| Raw IRP access | `WdfRequestWdmGetIrp(Request)->UserBuffer` cast to `HID_XFER_PACKET*`, then raw `reportBuffer` writes (Hid.c:319, 455) | `unsafe`: build a `&mut [u8]` from `(reportBuffer, reportBufferLen)` after the checks. Note that the C code writes before or without some length checks (B14/B15). |
| KMDF inline helpers (not exported) | `WDF_*_INIT`, `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE`, `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME` (generates `DeviceGetContext` via `WdfObjectGetTypedContextWorker`), `WDF_USB_CONTROL_SETUP_PACKET_INIT`, `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER`, `WDF_USB_CONTINUOUS_READER_CONFIG_INIT`, `WDF_USB_DEVICE_SELECT_CONFIG_PARAMS_INIT_SINGLE_INTERFACE`, `WDF_USB_PIPE_INFORMATION_INIT`, `WDF_USB_DEVICE_INFORMATION_INIT`, `WDF_REQUEST_PARAMETERS_INIT` | Re-implement each, zeroing the struct and setting `Size` (resolved field values are given in §2 and §4). The context type info is a `static WDF_OBJECT_CONTEXT_TYPE_INFO { Size, ContextName: "DEVICE_CONTEXT", ContextSize: 80, UniqueType: &self, EvtDriverGetUniqueContextType: NULL }`. |
| KMDF function-table dispatch | All `Wdf*` calls go through `WdfFunctions_01015[...]` | wdk-sys `call_unsafe_wdf_function_binding!`. Choose the KMDF minor version consistently with the INF. |
| `WDF_USB_CONTROL_SETUP_PACKET` union with bit-field `bm.Request {Recipient:2, Reserved:3, Type:2, Dir:1}` | Device.c:554-605 | Build the 8 raw bytes directly: `A1 01 02 03 02 00 04 00` and `21 09 02 03 02 00 04 00`. |
| `LARGE_INTEGER` union (`QuadPart`) | Device.h:45; Interrupt.c:108, 178 | Use `i64`. |
| `USB_DEVICE_DESCRIPTOR` passed by value | Device.c:26-29 | Pass `&` or just the `u16` PID. |
| IRQL contracts | ReadComplete ≤ DISPATCH: no paged code, no blocking, no paged allocation. SetWellspringMode PASSIVE only (PagedPool + synchronous transfer). The IOCTL dispatcher may run at DISPATCH for READ_REPORT. | Keep all I/O-path code in non-paged sections. Rust has no `alloc_text`; `#[link_section = "PAGE"]` is possible but unnecessary. Make everything non-paged except what you are sure of. |
| `PAGED_CODE()` in non-paged functions | Device.c:370, 432; Hid.c:300 | Optional debug IRQL asserts. |
| Callback ABI | All WDF callbacks | `extern "C"` / `unsafe extern "system"`; identical on x64 and ARM64. |
| WPP preprocessor tracing | All `TraceEvents` | No Rust equivalent; replace (§1.2). |
| Encodings | Resource.rc (UTF-16LE), Queue.c (CP-1252 byte) | Tooling only. |
| **Not present** | — | No inline assembly, no function-pointer patching or hooking, no undocumented kernel structures (only the documented `HID_XFER_PACKET`, `HID_DESCRIPTOR`, `HID_DEVICE_ATTRIBUTES`), no global mutable state besides the descriptor array, no floating point. |

---

## Appendix A: report descriptor, 493 bytes (`AmtPtpT2ReportDescriptor`, Hid.c:7-10)

Decimal offsets. Generated by compiling `{ AAPL_WELLSPRING_T2_PTP_TLC, AAPL_PTP_WINDOWS_CONFIGURATION_TLC }` from the project headers.

```
0000: 05 0D 09 05 A1 01 85 05 09 22 A1 02 25 01 09 47
0016: 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01
0032: 75 20 27 FF FF FF FF 09 51 81 02 05 01 26 20 4E
0048: 75 10 55 0E 65 11 09 30 46 14 05 95 01 81 02 46
0064: 52 03 26 E0 2E 09 31 81 02 45 00 55 00 65 00 C0
0080: 05 0D 09 22 A1 02 25 01 09 47 09 42 95 02 75 01
0096: 81 02 75 01 95 06 81 03 95 01 75 20 27 FF FF FF
0112: FF 09 51 81 02 05 01 26 20 4E 75 10 55 0E 65 11
0128: 09 30 46 14 05 95 01 81 02 46 52 03 26 E0 2E 09
0144: 31 81 02 45 00 55 00 65 00 C0 05 0D 09 22 A1 02
0160: 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06
0176: 81 03 95 01 75 20 27 FF FF FF FF 09 51 81 02 05
0192: 01 26 20 4E 75 10 55 0E 65 11 09 30 46 14 05 95
0208: 01 81 02 46 52 03 26 E0 2E 09 31 81 02 C0 05 0D
0224: 09 22 A1 02 25 01 09 47 09 42 95 02 75 01 81 02
0240: 75 01 95 06 81 03 95 01 75 20 27 FF FF FF FF 09
0256: 51 81 02 05 01 26 20 4E 75 10 55 0E 65 11 09 30
0272: 46 14 05 95 01 81 02 46 52 03 26 E0 2E 09 31 81
0288: 02 45 00 55 00 65 00 C0 05 0D 09 22 A1 02 25 01
0304: 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03
0320: 95 01 75 20 27 FF FF FF FF 09 51 81 02 05 01 26
0336: 20 4E 75 10 55 0E 65 11 09 30 46 14 05 95 01 81
0352: 02 46 52 03 26 E0 2E 09 31 81 02 C0 05 0D 55 0C
0368: 66 01 10 47 FF FF 00 00 27 FF FF 00 00 09 56 81
0384: 02 09 54 25 7F 75 08 81 02 05 09 09 01 25 01 75
0400: 01 81 02 95 07 81 03 05 0D 85 07 09 55 09 59 15
0416: 00 26 FF 00 75 08 95 02 B1 02 06 00 FF 85 08 09
0432: C5 15 00 26 FF 00 75 08 96 00 01 B1 02 C0 05 0D
0448: 09 0E A1 01 85 04 09 22 A1 02 09 52 15 00 25 10
0464: 75 08 95 01 B1 02 C0 A1 00 85 06 09 57 09 58 75
0480: 01 95 02 25 01 B1 02 95 06 B1 03 C0 C0
```

## Appendix B: HID descriptor (9 bytes)

```
09 21 00 01 00 01 22 ED 01
```

## Appendix C: intended HQA blob (`DEFAULT_PTP_HQA_BLOB`, Hid.h:56-88, 256 bytes)

Only byte 0 (`FC`) actually reaches the caller (B2).

```
0000: FC 28 FE 84 40 CB 9A 87 0D BE 57 3C B6 70 09 88
0016: 07 97 2D 2B E3 38 34 B6 6C ED B0 F7 E5 9C F6 C2
0032: 2E 84 1B E8 B4 51 78 43 1F 28 4B 7C 2D 53 AF FC
0048: 47 70 1B 59 6F 74 43 C4 F3 47 18 53 1A A2 A1 71
0064: C7 95 0E 31 55 21 D3 B5 1E E9 0C BA EC B8 89 19
0080: 3E B3 AF 75 81 9D 53 B9 41 57 F4 6D 39 25 29 7C
0096: 87 D9 B4 98 45 7D A7 26 9C 65 3B 85 68 89 D7 3B
0112: BD FF 14 67 F2 2B F0 2A 41 54 F0 FD 2C 66 7C F8
0128: C0 8F 33 13 03 F1 D3 C1 0B 89 D9 1B 62 CD 51 B7
0144: 80 B8 AF 3A 10 C1 8A 5B E8 8A 56 F0 8C AA FA 35
0160: E9 42 C4 D8 55 C3 38 CC 2B 53 5C 69 52 D5 C8 73
0176: 02 38 7C 73 B6 41 E7 FF 05 D8 2B 79 9A E2 34 60
0192: 8F A3 32 1F 09 78 62 BC 80 E3 0F BD 65 20 08 13
0208: C1 E2 EE 53 2D 86 7E A7 5A C5 D3 7D 98 BE 31 48
0224: 1F FB DA AF A2 A8 6A 89 D6 BF F2 D3 32 2A 9A E4
0240: CF 17 B7 B8 F4 E1 33 08 24 8B C4 43 A5 E5 24 C2
```

## Appendix D: NTSTATUS values used (WDK `shared/ntstatus.h`)

| Name | Value | Produced by |
|---|---|---|
| `STATUS_SUCCESS` | 0x00000000 | — |
| `STATUS_INVALID_DEVICE_REQUEST` | 0xC0000010 | GET/SET_FEATURE when `UserBuffer` is NULL |
| `STATUS_BUFFER_TOO_SMALL` | 0xC0000023 | GET/SET_FEATURE xfer-packet length check (also returned by WDF retrieve/copy helpers) |
| `STATUS_DATA_ERROR` | 0xC000003E | READ_REPORT with more than 5 fingers |
| `STATUS_UNKNOWN_REVISION` | 0xC0000058 | Dead branch in ConfigContReader (masked) |
| `STATUS_NOT_SUPPORTED` | 0xC00000BB | Unhandled IOCTLs, unknown feature IDs, input mode 0 |
| `STATUS_CANCELLED` | 0xC0000120 | WDF, for cancelled or purged queued reads |
| `STATUS_INVALID_DEVICE_STATE` | 0xC0000184 | No interrupt pipe; dead config/length checks |
| `STATUS_INVALID_BUFFER_SIZE` | 0xC0000206 | Feature report buffer too small (caps under 3, HQA under 257). Also what `ConfigContinuousReader` would return without the NoMaximumPacketSizeCheck call (masked by B9; B28). |

## Appendix E: rough Rust module map (suggestion, not behavior)

| Module | Contents |
|---|---|
| `driver.rs` | DriverEntry, DeviceAdd (filter), context type |
| `device.rs` | PrepareHardware, D0Entry/Exit, interface/pipe selection, wellspring switch (§6.3) |
| `queue.rs` | Default parallel queue (internal IOCTL), manual InputQueue, dispatcher table (§4.2) |
| `hid.rs` | `static HID_DESCRIPTOR: [u8; 9]`, `static REPORT_DESCRIPTOR: [u8; 493]`, attributes, GET/SET_FEATURE (§5) |
| `input.rs` | A pure, unit-testable `fn parse(packet: &[u8], cfg: &Cfg) -> ParseResult { Drop, DataError, Report([u8; 50]) }` plus the reader callback glue. Use the §7.4 golden vectors as tests. |
| `config.rs` | PID → `{x_min, y_max}` table (§8) |

## Verification addendum

This section records an adversarial re-check of this spec against every file in `src/AmtPtpDeviceUsbKm/` (all `.c`, `.h`, `include/**`, `.vcxproj` and the decoded `Resource.rc`) and against `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf`. The fixes below have also been made inline above.

**Checked mechanically, no change needed:**

- The 493-byte report descriptor (Appendix A) and the 256-byte HQA blob (Appendix C) are byte-identical to the project macros. They were compiled with gcc against the real headers and diffed.
- Sizes match: `PTP_REPORT` 50, `PTP_CONTACT` 9, selective-report struct 3, `BCM5974_CONFIG` 128, `TRACKPAD_FINGER` 28. Field offsets match: ScanTime 46, ContactCount 48, button 49. The `DEVICE_CONTEXT` layout is 80 bytes, and `USB_DEVICE_DESCRIPTOR` and `HID_DESCRIPTOR` are packed (pshpack1).
- All 6 `Bcm5974ConfigTable` rows match the values printed from the compiled table.
- Golden vectors A, B, B', C (DATA_ERROR) and D (drop) were reproduced by a C replica of Interrupt.c:115-226, and the outputs match.
- All IOCTL codes were recomputed from WDK 10.0.28000 `hidport.h`/`hidclass.h`. The NTSTATUS values, the KMDF 1.15 default and the file:line citations were spot-checked.

**Corrections made:**

1. Header: HEAD is `2edb0f2`, not `29d2778`. The only difference is `.gitattributes`.
2. §1.1 Sources: the `include/` headers are not listed in the vcxproj. They are reached only through the IncludePath.
3. §1.4, §4.1, §9: removed the claim that user `IRP_MJ_DEVICE_CONTROL` on the device interface is forwarded to the USB PDO. Opens land on the hidclass FDO at the top of the stack, and hidclass never sends create, close or device-control to its minidriver. The WDF auto-forward rule is kept as a general rule only.
4. §1.5: added `[SourceDisksNames]` (INF 18-19).
5. §2.5: for a filter, KMDF takes the IoType, DO_POWER_PAGABLE and inrush settings from the lower DO. Do not set them.
6. §2.10, §6.2, B28 and §11 (new): **order-dependent [KEEP]**. `SetNoMaximumPacketSizeCheck` must precede `ConfigContinuousReader`. 526 is not a multiple of any realistic MaxPacketSize (8/16/32/64/512/1024; the real endpoint's value is not in the source), so the reader config would otherwise return `STATUS_INVALID_BUFFER_SIZE`, which B9 hides, giving zero input. Also noted that an OUT interrupt pipe (direction is unchecked) yields `STATUS_INVALID_DEVICE_REQUEST`, which is also masked.
7. §4.3, §7.2 steps 4 and 11, B6, B7 and §11: corrected the leak semantics. A request retrieved with `WdfIoQueueRetrieveNextRequest` is driver-owned and **not cancelable**, so the leak is permanent and is not ended by a cancel. It consumes a hidclass read, and InputQueue teardown at removal is expected to block. That last point is inferred from the documented purge semantics. `STATUS_CANCELLED` applies only to requests still queued.
8. §5.2 descriptor quirks: finger #4 inherits cm, exp −2 and PhysMax 850 from finger #3 (COLLECTION_2 has no reset). The seconds/−4/65535 globals also carry into Features 0x07, 0x08, 0x04 and 0x06.
9. §6.3 step 1 and B4: `WdfObjectDelete(NULL)` is a documented bug check (expect 0x10D), not just a verifier break.
10. §6.3 step 1 and B27 (new): the control-transfer buffer comes from **PagedPool** and is used as a URB TransferBuffer. Fix freely with NonPagedPoolNx.
11. §6.3 step 5 and B3: the GET_REPORT read (wLength 4) can **overrun** the 2-byte allocation by 2 bytes. Previously only the write-side over-read was described.
12. §2.9 and B10: a D0Exit failure also applies to ordinary sleep and hibernate whenever the OFF transfer fails, not only D3Final or surprise removal.
13. §10.5: softened the "SET_FEATURE in Dx fails" claim. MS Learn does not document KMDF stopping the USB device target on Dx, so the outcome is unverified.
14. Appendix D: `STATUS_INVALID_BUFFER_SIZE` is also the masked reader-config error.

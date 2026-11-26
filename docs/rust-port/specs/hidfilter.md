<!-- Porting analysis of the C driver as of commit 2edb0f2, written before the Rust rewrite. File:line references point into that commit's `src/` tree, which has since been removed. -->

# AmtPtpHidFilter: porting specification for a Rust rewrite

Source: ``, branch `main`, HEAD `2edb0f2` (it only touches `.gitattributes`; the sources are identical to `29d2778`).
Driver: `src/AmtPtpHidFilter`, the KMDF lower filter that turns a Bluetooth (and, in the standalone INF, USB) **Magic Trackpad 2** HID collection into a Windows Precision Touchpad (PTP) device.

Every file under `src/AmtPtpHidFilter` was read in full:

* C sources: `Driver.c`, `Device.c`, `Detour.c`, `Diagnostics.c`, `Hid.c`, `Input.c`, `Queue.c`
* Headers in `include/`: `Driver.h`, `Device.h`, `Diagnostics.h`, `Hac.h`, `HidCommon.h`, `HidDevice.h`, `HidMiniport.h`, `Input.h`, `Queue.h`, `Trace.h`
* Headers in `include/Metadata/`: `MagicTrackpad2.h`, `StaticHidRegistry.h`, `WindowsHID.h`
* Project and install files: `AmtPtpHidFilter.vcxproj`, `AmtPtpHidFilter.vcxproj.filters`, `AmtPtpHidFilter.inf`
* Outside the directory: `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf`

The directory has no `.rc`, `.wprp`, `.def`, `.mc` or `.man` file. The repository's only `.wprp`, `src/AmtPtpDeviceUsbUm/AmtPtpDevice.wprp`, belongs to a different driver.

## Conventions

* `file:line` paths are relative to `src/AmtPtpHidFilter/` unless a path is given in full.
* **[CODE]** marks behaviour that this driver's source implements.
* **[FW]** marks behaviour of KMDF, the I/O manager, HIDCLASS, mshidkmdf or HidBth that the driver *relies on*. The Rust port gets it by calling the same framework APIs. It does not reimplement it. [FW] statements come from WDK headers and docs (Microsoft Learn, the Windows-Driver-Frameworks source), not from this repo.
* Flags:
  * **[BUG]**: looks wrong. It is only flagged here, not fixed.
  * **[DEAD]**: never executed or never read. It can be dropped.
  * **[QUIRK]**: odd but intentional or harmless.
  * **[RISK]**: dangerous for porting.
* All sizes and offsets assume LP64 (x64 and ARM64; x86 was removed in commit `8e0cc7c`). The struct sizes below were computed with ctypes using MSVC x64 alignment rules.

## Contents

| Section | Topic |
|---|---|
| 0 | Architecture overview |
| 1 | Build facts (project, WPP, INFs) |
| 2 | Entry points and WDF callbacks |
| 3 | Context structures |
| 4 | IOCTL handling |
| 5 | HID: descriptors, attributes, strings, feature reports |
| 6 | Device I/O to the hardware: detour, mode switch, reads, timer and work item, power |
| 7 | Input parsing |
| 8 | Device support tables |
| 9 | Registry and configuration |
| 10 | Error paths, magic numbers, TODOs, dead code, bugs |
| 11 | Rust porting risks |
| Appendix A | Report descriptor bytes, raw and decoded |
| Appendix B | HQA blob |
| Appendix C | Bug table with preservation guidance |
| Appendix D | Golden test vectors |
| Appendix E | Suggested Rust module split |

The short answer is in Appendix C. Most of the C driver's oddities are observable to Windows and must be preserved bit-for-bit: the Y off-by-one, the confidence threshold, the scan-time computation (because `KeQueryPerformanceCounter` is misused, ScanTime is effectively a constant 0; see 7.5 and B6, which is a "Decide" item), the single HQA byte and the global HIDCLASS patch. Only internal memory-safety defects should be fixed (B4, B10, B15, B17-B19, B25).

---

## 0. Architecture overview

### 0.1 Device stack (shipping configuration: Bluetooth)

```
  HIDCLASS FDO  (DriverObject = mshidkmdf.sys; mshidkmdf registered as HID minidriver)  <- function driver
        |   IOCTL_HID_* sent down as IRP_MJ_INTERNAL_DEVICE_CONTROL (mshidkmdf is a pass-through shim) [FW]
  AmtPtpHidFilter.sys  (KMDF device, WdfFdoInitSetFilter; installed as "LowerFilters")
        |   default local I/O target = WdfDeviceGetIoTarget(device)  (the next-lower device object)
  PDO   HID\{00001124-0000-1000-8000-00805f9b34fb}_VID&0001004c_PID&0265&Col01
        created by the *original* HIDCLASS instance of the trackpad; its DriverObject is the
        transport minidriver (HidBth.sys for BT; HidUsb.sys for the USB variant).
```

The node being filtered is collection **Col01** of the real trackpad's original HID device. A HIDCLASS collection PDO does not service minidriver IOCTLs such as `IOCTL_HID_READ_REPORT`, because HIDCLASS has replaced the minidriver's dispatch table. `Detour.c` writes the minidriver's original `IRP_MJ_INTERNAL_DEVICE_CONTROL` handler back into the minidriver's `DRIVER_OBJECT.MajorFunction`. HIDCLASS had saved that handler in its driver-object client extension. After the write, internal IOCTLs that this filter sends to the PDO reach HidBth (or HidUsb) directly. That gives the filter raw input reports and raw SET_FEATURE access to the device (INF comment `AmtPtpHidFilter.inf:38-41`, `Detour.c:67-72`).

Toward mshidkmdf and HIDCLASS, the filter behaves like a HID miniport. It:

* answers `GET_DEVICE_DESCRIPTOR`, `GET_DEVICE_ATTRIBUTES` and `GET_REPORT_DESCRIPTOR` with a synthetic PTP descriptor (vendor ID faked to 0x8910);
* forwards `GET_STRING` to the PDO;
* handles PTP feature reports in software;
* turns each raw Apple multitouch input report into one 50-byte PTP input report.

### 0.2 Lifecycle and data flow (normal path)

1. `DriverEntry` initialises WPP and creates the WDFDRIVER (`Driver.c:12-43`).
2. `EvtDeviceAdd` marks the device as a filter, then creates it along with its context, device interface, lookaside list, timer, work item and two queues (`Driver.c:45-66`, `Device.c:10-102`, `Queue.c:10-54`).
3. `EvtDevicePrepareHardware` resets the IDs and sets `DeviceConfigured = FALSE` (`Device.c:104-130`).
4. `EvtDeviceD0Entry` does nothing (`Device.c:132-147`).
5. `EvtDeviceSelfManagedIoInit` runs once, at first start (`Device.c:184-245`):
   * applies the detour and sets `HidIoTarget`;
   * sends `IOCTL_HID_GET_DEVICE_ATTRIBUTES` synchronously to the PDO and stores VID, PID and version;
   * calls `PtpFilterConfigureMultiTouch`, which sends a vendor SET_FEATURE to switch the trackpad into raw multitouch ("wellspring") mode;
   * calls `KeQueryPerformanceCounter(&ctx->LastReportTime)`, which stores the QPC **frequency** (not a counter value) in `LastReportTime` (see 7.5), and sets `DeviceConfigured = TRUE`.
6. HIDCLASS, above mshidkmdf, then queries the descriptors, attributes and strings, and posts continuous `IOCTL_HID_READ_REPORT` IRPs [FW].
7. Each upper `IOCTL_HID_READ_REPORT` is parked in the manual queue `HidReadQueue`. If `DeviceConfigured` is set, exactly one lower `IOCTL_HID_READ_REPORT` (1024-byte buffer) is sent to the PDO (`Input.c:6-28`, `Input.c:39-117`).
8. When a lower read completes, the raw packet is validated and parsed. One parked upper read is dequeued and completed with a 50-byte `PTP_REPORT` (`Input.c:119-253`). No new lower read is issued from here; the next one is triggered by the next upper read. The pipeline is 1:1.
9. On resume, `EvtDeviceSelfManagedIoRestart` re-sends the mode switch (`Device.c:247-284`). On power-down, `EvtDeviceD0Exit` clears `DeviceConfigured` and cancels the parked upper reads (`Device.c:149-182`).
10. Recovery uses two mechanisms:
    * The one-shot passive-level timer (2 s or 3 s) re-runs the "restart" path and issues a lower read (`Device.c:424-443`).
    * The work item re-issues a lower read after a zero-length completion (`Input.c:30-37`, `Input.c:163-166`).

---

## 1. Build facts

### 1.1 Project (`AmtPtpHidFilter.vcxproj`)

| Item | Value | Where |
|---|---|---|
| Project GUID | `{EE63C42B-F401-4F55-ADBB-14C16BD3B18C}` | vcxproj:30; `AmtPtpDriver.sln:25` |
| RootNamespace | `AmtPtpHidFilter` | vcxproj:36 |
| Driver model | **KMDF** kernel-mode: `ConfigurationType=Driver`, `DriverType=KMDF`, `PlatformToolset=WindowsKernelModeDriver10.0`, `DriverTargetPlatform=Universal`, `TargetVersion=Windows10` (identical in all 6 configurations) | vcxproj:40-87 |
| Configurations | Debug, Release and ReleaseSigned, each for x64 and ARM64. No Win32 configuration. `Platform` defaults to `Win32` (vcxproj:35), a leftover. | vcxproj:3-28 |
| SDK | `WindowsTargetPlatformVersion=$(LatestTargetPlatformVersion)` | vcxproj:37 |
| KMDF version | **Not pinned** in the project. The WDK default for `_NT_TARGET_VERSION >= WIN10` is **1.15** (installed WDK 10.0.28000, `WindowsDriver.KernelMode.Default.props:18`). The WDK passes `KMDF_VERSION_MAJOR=1;KMDF_VERSION_MINOR=15` to the compiler, and stampinf writes it into `$KMDFVERSION$`. README.md:53 confirms "Bluetooth driver uses KMDF Framework 1.15". Every API used exists in KMDF 1.9 or earlier; passive-level timers need 1.9. | [FW] |
| Output binary | `AmtPtpHidFilter.sys` (TargetName = ProjectName) in `$(SolutionDir)build\AmtPtpHidFilter\$(Platform)\$(ConfigurationName)\`. Intermediates go to `$(SolutionDir)intermediate\...`, which is also on the include path for the WPP `.tmh` files. | vcxproj:96-137 |
| Include path | `$(ProjectDir)include;<IntDir>;$(IncludePath)` | vcxproj:98 etc. |
| Language | C only (all `ClCompile` items are `.c`); `LanguageStandard_C=stdc17`. `LanguageStandard=stdcpplatest` is unused. | vcxproj:145-146 etc., 219-227 |
| Preprocessor defines | **None** specific to the project (`WppPreprocessorDefinitions` is empty). Only WDK defaults apply, such as the KMDF version and `DBG=1` in Debug. The source has no `#ifdef` on any build define except `ALLOC_PRAGMA`. | vcxproj:141-142 etc. |
| Code placement | `#pragma alloc_text`: `DriverEntry` goes to INIT (`Driver.c:7`). `PtpFilterEvtDeviceAdd` and `PtpFilterEvtDriverContextCleanup` go to PAGE (`Driver.c:8-9`), as do `PtpFilterCreateDevice` (`Device.c:7`) and `PtpFilterIoQueueInitialize` (`Queue.c:7`). Everything else is non-paged, although several of those functions call `PAGED_CODE()` (only a Debug-build IRQL assert). | |
| Linked libraries | No `AdditionalDependencies`. The KMDF toolset defaults apply [FW]: `WdfDriverEntry.lib`, `WdfLdr.lib`, `ntoskrnl.lib`, `hal.lib`, `wmilib.lib` and BufferOverflowK. With IFR, `WppRecorder.lib` is added too. | |
| Kernel imports used by code | `IoGetLowerDeviceObject` (declared by hand, `Hac.h:37-41`; it normally lives in ntifs.h), `ObDereferenceObject`, `KeQueryPerformanceCounter`, `strncmp`, `RtlZeroMemory`, the WPP and IFR runtime, and the WDF function table. | |
| Unused headers | `usb.h`, `usbdlib.h` and `wdfusb.h` are included (`Driver.h:6-8`), but no USB API is used. `initguid.h` is included before `Device.h`, so `DEFINE_GUID` emits the GUID in every translation unit (`Driver.h:9`). | |
| WPP | `WppEnabled=true`, `WppRecorderEnabled=true` (Inflight Recorder), `WppScanConfigurationData=include\Trace.h` | vcxproj:140-144 etc. |
| Signing | `DriverSign/FileDigestAlgorithm=sha256` is set on the three **x64** configurations only. The ARM64 configurations have no `DriverSign` element. `TimeStampServer=http://timestamp.digicert.com` is set on all. | vcxproj:181-183, 195-197, 209-211; 101 etc. |
| Packaging | `Inf Include="AmtPtpHidFilter.inf"` and `FilesToPackage $(TargetPath)`. The package project `src/AmtPtpDeviceUniversalPkg/AmtPtpDeviceUniversalPkg.vcxproj:39-41` references this project and ships `AmtPtpDevice.inf`. | vcxproj:213-218 |
| Resources | **No `.rc` file**, so there is no VERSIONINFO, version number or product name in the PE. The driver version exists only in INF `DriverVer`, which stampinf fills in (`AmtPtpHidFilter.inf:11` is empty). | |

### 1.2 WPP / tracing (`include/Trace.h`)

* Control GUID name `AmtPtpHidFilterTraceGuid`, GUID **`{eeb34803-f3ec-4bb1-97ce-b42405d7f3c2}`** (`Trace.h:8-13`).
* Flags, assigned sequentially by `WPP_DEFINE_BIT` (`Trace.h:15-19`):

  | Flag | Value |
  |---|---|
  | `TRACE_DRIVER` | 0x01 |
  | `TRACE_DEVICE` | 0x02 |
  | `TRACE_QUEUE` | 0x04 |
  | `TRACE_HID` | 0x08 |
  | `TRACE_INPUT` | 0x10 |

* A message is enabled when its flag is enabled and the control level is at least the message LEVEL (`Trace.h:22-33`). Recorder argument reordering is defined at `Trace.h:40-41`.
* Trace functions (`Trace.h:47-50`): `TraceEvents(LEVEL, FLAGS, MSG, ...)`, and `Trace(LEVEL, MSG, ...)` with FLAGS fixed to `TRACE_DRIVER`. `Trace` is never called.
* Lifetime:
  * `WPP_INIT_TRACING(DriverObject, RegistryPath)` runs first in `DriverEntry` (`Driver.c:25`).
  * `WPP_CLEANUP(DriverObject)` runs if `WdfDriverCreate` fails (`Driver.c:37`).
  * `WPP_CLEANUP(WdfDriverWdmGetDriverObject(driver))` runs in the driver-object cleanup callback (`Driver.c:79`).
* WPP format extensions used: `%!FUNC!` and `%!STATUS!`. `Detour.c` logs under `TRACE_DEVICE`.
* Rust port: WPP is a C preprocessor tool and cannot be used. Replace it with TraceLogging/ETW or `DbgPrintEx`. Tracing has no functional effect; dropping it changes no behaviour.

### 1.3 INF: shipping package `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf`

| Section / line | Content relevant to this binary |
|---|---|
| `[Version]` 5-12 | `Class=HIDClass`, `ClassGuid={745a17a0-74d3-11d0-b6fe-00a0c90f57da}`, `CatalogFile=AmtPtpDevice.cat`, `PnpLockdown=1`. `DriverVer` is empty and stamped later. |
| `[DestinationDirs]` 14-16 | `DefaultDestDir = 13` (DriverStore run-from-store) |
| `[SourceDisksFiles]` 25 | `AmtPtpHidFilter.sys = 1,,` |
| `[Manufacturer]` 27-28 | `%ManufacturerName%=Standard,NT$ARCH$` |
| `[Standard.NT$ARCH$]` 86-91 | USB MT2 `USB\Vid_05ac&Pid_0265&MI_01` goes to the *UMDF* driver (`AmtPtpDeviceUsbUm_Install`), **not to this filter**. BT `HID\{00001124-0000-1000-8000-00805f9b34fb}_VID&0001004c_PID&0265&Col01` goes to `AmtPtpHidFilter_MiniPortDevice`. BT `...&Col02` goes to `AmtPtpHidFilter_NullDevice`. |
| `[AmtPtpHidFilter_NullDevice]` 94-95 | empty |
| `[AmtPtpHidFilter_NullDevice.Services]` 97-98 | `AddService = ,2`: null service plus `SPSVCINST_ASSOCSERVICE`, so the collection runs raw with no driver. |
| `[FilterDriver_Payload]` 101-102 | `AmtPtpHidFilter.sys` |
| `[mshidkmdf_Service_Inst]` 114-118 | kernel driver, demand start, error normal, `%10%\System32\Drivers\mshidkmdf.sys` |
| `[AmtPtpHidFilter_MiniPortDevice.NT]` 121-122 | `CopyFiles=FilterDriver_Payload` |
| `[AmtPtpHidFilter_MiniPortDevice.NT.Services]` 124-126 | `AddService = AmtPtpHidFilter,, AmtPtpHidFilter_Service_Inst` (flags 0). `AddService = mshidkmdf, 0x2, mshidkmdf_Service_Inst`: mshidkmdf is the **function driver**. |
| `[AmtPtpHidFilter_MiniPortDevice.NT.HW]` 128-129 | `AddReg=AmtPtpHidFilter_AddReg` |
| `[AmtPtpHidFilter_AddReg]` 131-133 | `HKR,,FriendlyName,,"Apple Multi-touch Trackpad HID Filter"` and `HKR,,"LowerFilters",0x00010008,"AmtPtpHidFilter"`. The flags are `FLG_ADDREG_TYPE_MULTI_SZ` with `FLG_ADDREG_APPEND`. |
| `[AmtPtpHidFilter_MiniPortDevice.NT.Wdf]` 135-136 | `KmdfService = AmtPtpHidFilter, AmtPtpHidFilter_wdfsect` |
| `[AmtPtpHidFilter_Service_Inst]` 138-143 | `DisplayName="Apple Multi-touch Trackpad HID Filter Service"`, `ServiceType=1`, `StartType=3`, `ErrorControl=1`, `ServiceBinary=%13%\AmtPtpHidFilter.sys` |
| `[AmtPtpHidFilter_wdfsect]` 145-146 | `KmdfLibraryVersion = $KMDFVERSION$`, which becomes 1.15 (see 1.1) |
| `[Strings]` 247-262 | `ManufacturerName="Bingxing Wang"`; `AmtPtpHidFilter.DeviceDesc="Apple Multi-touch Trackpad HID Filter"`; `AmtPtpHidFilter.NullDeviceDesc="Apple Multi-touch Auxiliary Services"`; `AmtPtpHidFilter.SVCDESC` as above. `AmtPtpHidRootDevice.DeviceDesc` is defined but unused in this INF. |

The service name **`AmtPtpHidFilter`** must stay the same. It is the value written to `LowerFilters` and the value of `KmdfService`.

### 1.4 INF: standalone `src/AmtPtpHidFilter/AmtPtpHidFilter.inf` (built with the project)

The service, AddReg, Wdf and mshidkmdf sections are identical to 1.3; see lines 64-104. The differences are:

| Line | Difference |
|---|---|
| 10-11 | `CatalogFile=AmtPtpHidFilter.cat` (line 10); `DriverVer=` carries a TODO comment (line 11). |
| 17-19 | `[ControlFlags] ExcludeFromSelect = *` |
| 31-32 | `[Manufacturer] %ManufacturerName%=Standard,NT$ARCH$.10.0`, with the models section `[Standard.NT$ARCH$.10.0]` |
| 36 | **Migration**: `USB\Vid_05ac&Pid_0265&MI_01` goes to `StandardUsbHid_Device`. That section (52-62) includes `input.inf` and `Needs = HID_Inst.NT`, `.NT.HW` and `.NT.Services`, which puts the in-box HidUsb back on USB MT2 so its HID collections can be filtered. |
| 42-43 | Filter is bound to USB `HID\VID_05AC&PID_0265&REV_0855&MI_01&Col01` **and** to BT `...&Col01` |
| 45-47 | Null device for USB `...&Col02`, USB `...&Col03` and BT `...&Col02` |
| 109 | `DiskName = "Precision Touchpad for Apple Trackpad Installation Disk"` |

Only the standalone INF can bind the USB code path (`VendorID == 0x05AC`). The shipping package routes USB MT2 to the UMDF driver, so **the USB branch in this filter cannot be reached in the shipping package**. The Rust port should keep it for parity.

---

## 2. Entry points and WDF callbacks

### 2.1 Summary table

| Function | file:line | Registered as / on | IRQL [FW] | Section |
|---|---|---|---|---|
| `DriverEntry` | Driver.c:12-43 | DRIVER_INITIALIZE | PASSIVE | INIT |
| `PtpFilterEvtDeviceAdd` | Driver.c:45-66 | `WDF_DRIVER_CONFIG.EvtDriverDeviceAdd` | PASSIVE | PAGE |
| `PtpFilterEvtDriverContextCleanup` | Driver.c:68-80 | `EvtCleanupCallback` of the **WDFDRIVER** object attributes | PASSIVE | PAGE |
| `PtpFilterCreateDevice` (helper) | Device.c:10-102 | called from EvtDeviceAdd | PASSIVE | PAGE |
| `PtpFilterPrepareHardware` | Device.c:104-130 | `EvtDevicePrepareHardware` | PASSIVE | non-paged |
| `PtpFilterDeviceD0Entry` | Device.c:132-147 | `EvtDeviceD0Entry` | PASSIVE | non-paged |
| `PtpFilterDeviceD0Exit` | Device.c:149-182 | `EvtDeviceD0Exit` | PASSIVE | non-paged |
| `PtpFilterSelfManagedIoInit` | Device.c:184-245 | `EvtDeviceSelfManagedIoInit` | PASSIVE | non-paged |
| `PtpFilterSelfManagedIoRestart` | Device.c:247-284 | `EvtDeviceSelfManagedIoRestart`; **also called directly** from the timer callback | PASSIVE | non-paged |
| `PtpFilterConfigureMultiTouch` (helper) | Device.c:286-422 | called from Init, Restart and the timer path | PASSIVE (synchronous send) | non-paged |
| `PtpFilterRecoveryTimerCallback` | Device.c:424-443 | `EvtTimerFunc` of `HidTransportRecoveryTimer` | PASSIVE (passive-level timer) | non-paged |
| `PtpFilterDetourWindowsHIDStack` (helper) | Detour.c:6-86 | called from SelfManagedIoInit | PASSIVE | non-paged |
| `PtpFilterIoQueueInitialize` (helper) | Queue.c:10-54 | called from CreateDevice | PASSIVE | PAGE |
| `FilterEvtIoIntDeviceControl` | Queue.c:56-117 | `EvtIoInternalDeviceControl` of the **default** queue | <= DISPATCH | non-paged |
| `FilterEvtIoStop` | Queue.c:119-129 | `EvtIoStop` of the default queue (no-op) | <= DISPATCH | non-paged |
| `PtpFilterInputProcessRequest` (helper) | Input.c:6-28 | READ_REPORT handler | <= DISPATCH | non-paged |
| `PtpFilterWorkItemCallback` | Input.c:30-37 | `EvtWorkItemFunc` of `HidTransportRecoveryWorkItem` | PASSIVE | non-paged |
| `PtpFilterInputIssueTransportRequest` (helper) | Input.c:39-117 | called from ProcessRequest, the work item and the timer | <= DISPATCH | non-paged |
| `PtpFilterInputRequestCompletionCallback` | Input.c:119-253 | `EVT_WDF_REQUEST_COMPLETION_ROUTINE` set on each lower read request | <= DISPATCH | non-paged |
| `PtpFilterGetHidDescriptor`, `PtpFilterGetDeviceAttribs`, `PtpFilterGetReportDescriptor`, `PtpFilterGetStrings`, `PtpFilterGetHidFeatures`, `PtpFilterSetHidFeatures` (helpers) | Hid.c:6-345 | called from `FilterEvtIoIntDeviceControl` | <= DISPATCH | non-paged |
| `PtpFilterDiagnosticsIoControlGetString` | Diagnostics.c:141-181 | trace helper only | any | non-paged |
| `PtpFilterDiagnosticsInitializeContinuousRead`, `PtpFilterDiagnosticsInputIssueRequest`, `PtpFilterDiagnosticsRequestCompletionRoutine` | Diagnostics.c:6-139 | **[DEAD]** never called | n/a | |

The PnP and power callbacks run at PASSIVE. On a filter, KMDF copies `DO_POWER_PAGABLE` from the lower device object [FW].

The following callbacks are **not** registered, so the Rust port must not add them unless it intends to change behaviour:

* `EvtDriverUnload`
* any device `EvtCleanupCallback` or `EvtDestroyCallback`
* `EvtDeviceReleaseHardware`
* `EvtDeviceD0EntryPostInterruptsEnabled` and `EvtDeviceD0ExitPreInterruptsDisabled`
* `EvtDeviceSelfManagedIoSuspend`, `EvtDeviceSelfManagedIoFlush` and `EvtDeviceSelfManagedIoCleanup`
* `EvtDeviceSurpriseRemoval`
* `EvtDeviceQueryRemove` and `EvtDeviceQueryStop`
* file-object callbacks (`WdfDeviceInitSetFileObjectConfig` is not called)
* `EvtIoDeviceControl`, `EvtIoRead`, `EvtIoWrite` and `EvtIoDefault`
* `EvtIoCanceledOnQueue`
* `EvtIoResume`
* power-policy settings: no `WdfDeviceAssignS0IdleSettings` or `WdfDeviceAssignSxWakeSettings`, and no power-policy ownership change (a filter is not the power-policy owner by default)
* request-cancel routines

Because the device is a filter, KMDF forwards every request type that has no queue handler (create, close, read, write, `IRP_MJ_DEVICE_CONTROL`, PnP, power and so on) to the next-lower driver automatically [FW: WdfFdoInitSetFilter remarks].

### 2.2 `DriverEntry` (Driver.c:12-43)

1. `WPP_INIT_TRACING(DriverObject, RegistryPath)` (line 25).
2. Driver object attributes (lines 28-29): `WDF_OBJECT_ATTRIBUTES_INIT`, then `EvtCleanupCallback = PtpFilterEvtDriverContextCleanup`. No context, and the default sync scope and execution level are kept (KMDF driver defaults: `WdfSynchronizationScopeNone`, `WdfExecutionLevelDispatch` [FW]).
3. `WDF_DRIVER_CONFIG_INIT(&config, PtpFilterEvtDeviceAdd)` (line 32). That leaves `DriverInitFlags=0` (PnP driver), `DriverPoolTag=0` and no `EvtDriverUnload`.
4. `WdfDriverCreate(DriverObject, RegistryPath, &attributes, &config, WDF_NO_HANDLE)` (line 33). If it fails: trace, `WPP_CLEANUP(DriverObject)`, then return the status (lines 35-39).
5. Return `STATUS_SUCCESS` (line 42).

### 2.3 `PtpFilterEvtDeviceAdd` (Driver.c:45-66)

`WdfFdoInitSetFilter(DeviceInit)` (line 59), then `return PtpFilterCreateDevice(DeviceInit)` (line 62).

### 2.4 `PtpFilterCreateDevice` (Device.c:10-102)

The steps run in order:

1. **PnP/power callbacks** (lines 27-33). `WDF_PNPPOWER_EVENT_CALLBACKS_INIT`, then `EvtDevicePrepareHardware`, `EvtDeviceD0Entry`, `EvtDeviceD0Exit`, `EvtDeviceSelfManagedIoInit` and `EvtDeviceSelfManagedIoRestart` are set to the functions in 2.1. The structure goes to `WdfDeviceInitSetPnpPowerEventCallbacks`.
2. **Device** (lines 36-41). `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, DEVICE_CONTEXT)` sets no sync scope and no execution level, so both inherit the driver's (None and Dispatch). Then `WdfDeviceCreate(&DeviceInit, &attr, &device)`. On failure, `goto exit` returns the status.
3. **Context init** (lines 44-50):
   * `ctx->Device = device`
   * `ctx->WdmDeviceObject = WdfDeviceWdmGetDeviceObject(device)`. If this is NULL, `goto exit` returns the **current status, which is STATUS_SUCCESS** **[BUG]**. It cannot be NULL in practice.
4. **Device interface** (lines 52-56). `WdfDeviceCreateDeviceInterface(device, &GUID_DEVICEINTERFACE_AmtPtpHidFilter, NULL)`. The GUID is **`{FF969022-3111-4441-8F88-875440172C2E}`** (`Device.h:7-9`), with no reference string. On failure, `goto exit` returns the error. KMDF enables the interface when the device starts [FW]. No handler in this driver serves it: there is no `EvtIoDeviceControl` and no file-object config. **[DEAD]** functionally. Keep it if any tool enumerates it.
5. **Lookaside list** (lines 59-65). `WdfLookasideListCreate(WDF_NO_OBJECT_ATTRIBUTES, REPORT_BUFFER_SIZE = 1024, NonPagedPoolNx, WDF_NO_OBJECT_ATTRIBUTES, PTP_LIST_POOL_TAG, &ctx->HidReadBufferLookaside)`.
   * `PTP_LIST_POOL_TAG` is `'LTPA'`, a MSVC multi-char constant with value 0x4C545041, which appears in memory and poolmon as "APTL" (`Driver.h:32-33`).
   * A failure is **only logged**. The status is overwritten later and the handle stays NULL **[BUG]** (section 10).
   * Because no attributes are given, the parent is the **WDFDRIVER**, not the device [FW: WdfLookasideListCreate remarks]. The list is never deleted, so one lookaside object leaks per AddDevice until the driver unloads **[BUG, minor]**.
6. **Recovery timer** (lines 68-76).
   * `WDF_TIMER_CONFIG_INIT(&tc, PtpFilterRecoveryTimerCallback)` gives `Period=0` (one-shot), `TolerableDelay=0` and the default `UseHighResolutionTimer`. The code then sets `tc.AutomaticSerialization = TRUE` (INIT already sets it TRUE).
   * Attributes: `WDF_OBJECT_ATTRIBUTES_INIT`, `ParentObject = device`, **`ExecutionLevel = WdfExecutionLevelPassive`**, which makes it a passive-level timer (KMDF ≥ 1.9; Period must be 0).
   * `WdfTimerCreate(&tc, &attr, &ctx->HidTransportRecoveryTimer)`. A failure is only logged **[BUG]**.
   * [FW, two doc statements that conflict for this configuration] The docs (WDF_TIMER_CONFIG remarks) say both of the following:
     * "Setting AutomaticSerialization to TRUE has no effect if the parent object's synchronization scope is WdfSynchronizationScopeNone". The device inherits None from the driver.
     * For passive-level timers, AutomaticSerialization may be TRUE "only if the parent device's execution level is WdfExecutionLevelPassive". The device inherits **Dispatch**, and `WdfTimerCreate` lists `STATUS_WDF_INCOMPATIBLE_EXECUTION_LEVEL` for this mismatch.

     **It is not verified whether `WdfTimerCreate` succeeds in the C driver today.** The timer is started only on error paths (configure failure, transport failure, malformed input). A failed creation would leave the handle NULL, would go unnoticed on the happy path, and would bugcheck on the first retry.
   * **Rust instruction:** set `AutomaticSerialization = FALSE` on this timer. With sync scope None this behaves the same as the intended TRUE, and it removes any chance of `STATUS_WDF_INCOMPATIBLE_EXECUTION_LEVEL`.
7. **Recovery work item** (lines 79-85). `WDF_WORKITEM_CONFIG_INIT(&wc, PtpFilterWorkItemCallback)` sets `AutomaticSerialization=TRUE` [FW]. Attributes: `ParentObject = device`. `WdfWorkItemCreate(&wc, &attr, &ctx->HidTransportRecoveryWorkItem)`. A failure is only logged **[BUG]**.
   * [FW] The WDF_WORKITEM_CONFIG remarks say AutomaticSerialization "has no effect" with sync scope None, but also that "if AutomaticSerialization is TRUE, the parent object's execution level must be WdfExecutionLevelPassive". The device inherits Dispatch.
   * The work item is enqueued on the live path (every zero-length lower-read completion; commit `cf47d19`), so creation must succeed in practice, or the driver would bugcheck in normal use.
   * **Rust instruction:** set `AutomaticSerialization = FALSE`. That is behaviourally identical under sync scope None and removes the ambiguity.
8. **Initial state** (lines 88-91): `VendorID=0`, `ProductID=0`, `VersionNumber=0`, `DeviceConfigured=FALSE`. Every other field is zero because KMDF zero-fills context memory [FW]. That includes `IsHidIoDetourCompleted=FALSE`, `HidIoTarget=NULL`, `PtpInputOn`, `PtpReportTouch` and `PtpReportButton` (all FALSE), `LastReportTime=0`, `X` and `Y` all 0, and the four size fields all 0.
9. **Queues**: `status = PtpFilterIoQueueInitialize(device)` (lines 94-97). This status is what the function returns, so failures in steps 5-7 are lost.

### 2.5 `PtpFilterIoQueueInitialize` (Queue.c:10-54)

* **Default queue** (lines 28-41):
  * Config: `WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&qc, WdfIoQueueDispatchParallel)`. That gives `DefaultQueue=TRUE`, `PowerManaged=WdfUseDefault` and `NumberOfPresentedRequests=(ULONG)-1`. `EvtIoInternalDeviceControl = FilterEvtIoIntDeviceControl` and `EvtIoStop = FilterEvtIoStop` are then set.
  * Attributes: `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&qa, QUEUE_CONTEXT)`.
  * `WdfIoQueueCreate(Device, &qc, &qa, &queue)`. On failure, `goto exit` returns the status.
  * [FW] `WdfUseDefault` on a device that called `WdfFdoInitSetFilter` means the queue is **not power-managed** (WDF_IO_QUEUE_CONFIG docs).
  * Queue context: `Device = ctx->Device`, `DeviceIoTarget = ctx->HidIoTarget`. **[BUG/DEAD]** `HidIoTarget` is still NULL here because the detour sets it later, and `DeviceIoTarget` is never read anyway.
* **Read queue** (lines 44-49):
  * Config: `WDF_IO_QUEUE_CONFIG_INIT(&qc, WdfIoQueueDispatchManual)`, then explicitly `PowerManaged = WdfFalse`. No callbacks.
  * `WdfIoQueueCreate(Device, &qc, WDF_NO_OBJECT_ATTRIBUTES, &ctx->HidReadQueue)`. The parent defaults to the device [FW].
  * A failure is logged and its status returned (no goto).

### 2.6 PnP/power callbacks

* **`PtpFilterPrepareHardware`** (Device.c:104-130). Ignores both resource lists. Sets `VendorID`, `ProductID` and `VersionNumber` to 0 and `DeviceConfigured = FALSE` (lines 123-126). Returns `STATUS_SUCCESS`. [FW] KMDF calls it on **every** PnP start, including the restart after an `IRP_MN_STOP_DEVICE` (resource rebalance), while `EvtDeviceSelfManagedIoInit` runs only once per device object. After a stop/start the IDs therefore stay 0 and the device breaks permanently (**B30**, see 6.8).
* **`PtpFilterDeviceD0Entry`** (Device.c:132-147). No-op; returns `STATUS_SUCCESS`.
* **`PtpFilterDeviceD0Exit`** (Device.c:149-182):
  1. `DeviceConfigured = FALSE` (line 166).
  2. Loop: `WdfIoQueueRetrieveNextRequest(ctx->HidReadQueue, &r)`. On success, `WdfRequestComplete(r, STATUS_CANCELLED)`. The loop stops at the first failure, normally `STATUS_NO_MORE_ENTRIES` (lines 169-178).
  3. Always returns `STATUS_SUCCESS` (line 181).

  It does **not** stop the recovery timer, flush the work item, or cancel in-flight lower reads **[RISK]** (section 10).
* **`PtpFilterSelfManagedIoInit`** (Device.c:184-245). KMDF calls it once, at the first start [FW]:
  1. `PtpFilterDetourWindowsHIDStack(Device)` (line 198). If it fails, return that status, which fails the device start.
  2. Zero a `HID_DEVICE_ATTRIBUTES` (32 bytes) on the stack, then `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER(&md, &attrs, 32)` (lines 205-210).
  3. `WdfIoTargetSendInternalIoctlSynchronously(ctx->HidIoTarget, NULL, IOCTL_HID_GET_DEVICE_ATTRIBUTES, NULL /*in*/, &md /*out*/, NULL /*options: no timeout*/, NULL /*bytes returned*/)` (lines 212-215). For METHOD_NEITHER, KMDF puts the output pointer in `Irp->UserBuffer` and sets `OutputBufferLength=32` [FW]. If the call fails, return that status. The returned size is not checked.
  4. `ctx->VendorID`, `ProductID` and `VersionNumber` take the values from `attrs` (lines 221-223).
  5. `status = PtpFilterConfigureMultiTouch(Device)` (line 227). If it fails, set `status = STATUS_SUCCESS`, call `WdfTimerStart(timer, WDF_REL_TIMEOUT_IN_SEC(2))` (that is, -20,000,000 in 100 ns units) and `goto exit`. In that case `DeviceConfigured` stays FALSE and `LastReportTime` is not stamped (lines 228-234).
  6. `KeQueryPerformanceCounter(&ctx->LastReportTime)` (line 237). The argument is the `_Out_opt_ PerformanceFrequency` parameter and the return value (the counter) is discarded, so this stores the QPC **frequency** in `LastReportTime` (7.5).
  7. `DeviceConfigured = TRUE` (line 240) and return `STATUS_SUCCESS`.
* **`PtpFilterSelfManagedIoRestart`** (Device.c:247-284). KMDF calls it on return to D0 after a low-power state, and also on the restart that follows a PnP stop (rebalance) [FW]. In the stop/start case, `PrepareHardware` has already zeroed the IDs, so `ConfigureMultiTouch` fails with `STATUS_NOT_SUPPORTED` every time (B30). The timer also calls it (2.7).
  * If `IsHidIoDetourCompleted` is set (lines 260-269): `status = PtpFilterConfigureMultiTouch(Device)`. If that fails, the status is set to `STATUS_SUCCESS`, the timer is started for 2 s, and the function jumps to exit **without** stamping or setting `DeviceConfigured`.
  * Otherwise (lines 270-273): `status = STATUS_INVALID_STATE_TRANSITION`, and the code **falls through** (**[BUG]**).
  * Both successful configuration and the else branch then run `KeQueryPerformanceCounter(&ctx->LastReportTime)` (line 276), which stores the QPC frequency (7.5), and set `DeviceConfigured = TRUE` (line 279). They return `status`. In the else branch that means `DeviceConfigured` becomes TRUE even though an error is returned.

### 2.7 `PtpFilterRecoveryTimerCallback` (Device.c:424-443)

```
device = WdfTimerGetParentObject(Timer)
status = PtpFilterSelfManagedIoRestart(device)        // re-sends the mode switch; see 2.6
if NT_SUCCESS(status): PtpFilterInputIssueTransportRequest(device)   // issues ONE lower read
```

**[BUG]** `SelfManagedIoRestart` returns SUCCESS even when `ConfigureMultiTouch` failed, after re-arming the timer for 2 s. A failed reconfiguration therefore still issues a lower read, and another retry follows 2 s later. Each retry adds another lower read in flight. `deviceContext` is fetched at line 434 but not used.

### 2.8 `PtpFilterWorkItemCallback` (Input.c:30-37)

`PtpFilterInputIssueTransportRequest(WdfWorkItemGetParentObject(WorkItem))`. It does **not** check `DeviceConfigured`.

### 2.9 Queue callbacks

* `FilterEvtIoIntDeviceControl` (Queue.c:56-117): see section 4.
* `FilterEvtIoStop` (Queue.c:119-129): an empty body. It neither acknowledges nor completes anything. [FW] Because the queue is not power-managed, KMDF does not call it on Dx transitions. It can be called during removal for requests the driver still owns from this queue. In practice none exist: requests are either completed synchronously, forwarded with send-and-forget, or moved to `HidReadQueue`.

---

## 3. Context structures

### 3.1 `DEVICE_CONTEXT` (`Device.h:26-60`)

Accessor: `PtpFilterGetContext` (`Device.h:62`). Size is **152 bytes** on x64/ARM64. The layout is private, so the Rust port is free to change it. Offsets are listed for debugging comparison only.

| Off | Field | C type | Meaning | Written by | Read by |
|---|---|---|---|---|---|
| 0 | `WdmDeviceObject` | `PDEVICE_OBJECT` | Our own WDM DO | CreateDevice (Device.c:46) | Detour (Detour.c:22, 29) |
| 8 | `Device` | `WDFDEVICE` | Self handle | CreateDevice (45) | queue init (Queue.c:40); `WdfDeviceSetFailed` calls (Input.c:72, 95, 158, 229, 237); Diagnostics.c:138 |
| 16 | `HidReadQueue` | `WDFQUEUE` | Manual queue of parked upper READ_REPORT requests | Queue.c:46 | Input.c:16, 176; D0Exit (Device.c:171) |
| 24 | `VendorID` | `USHORT` | Real VID of the lower device: 0x05AC for USB, 0x004C for BT | Device.c:88, 123, 221 | ConfigureMultiTouch (308, 322, 343); completion check (Input.c:156) |
| 26 | `ProductID` | `USHORT` | Real PID (0x0265) | Device.c:89, 124, 222 | ConfigureMultiTouch (313); Hid.c:28, 81, 113 |
| 28 | `VersionNumber` | `USHORT` | Real version (0x0855 on USB per the INF hardware ID) | Device.c:90, 125, 223 | trace only (Device.c:224-225) |
| 32 | `InputHeaderSize` | `size_t` | Raw-report bytes before the first finger: 12 (USB) or 4 (BT) | Device.c:324 / 345 | Input.c:152, 169 |
| 40 | `InputFingerSize` | `size_t` | Bytes per finger record: 9 | Device.c:323 / 344 | Input.c:153, 169 |
| 48 | `InputFingerDelta` | `size_t` | Extra offset after the header to finger 0: 0 | Device.c:325 / 346 | Input.c:198 |
| 56 | `InputButtonDelta` | `size_t` | Absolute byte offset of the button byte in the raw report: 1 | Device.c:326 / 347 | Input.c:221 |
| 64 | `X` | `BCM5974_PARAM` {int snratio, min, max} | snratio 250, min -3678, max 3934 | Device.c:328-330 / 349-351 | only `X.min` is read (Input.c:207); **`snratio` and `max` are [DEAD]** |
| 76 | `Y` | `BCM5974_PARAM` | snratio 250, min -2479, max 2586 | Device.c:331-333 / 352-354 | only `Y.min` (Input.c:208) |
| 88 | `HidReadBufferLookaside` | `WDFLOOKASIDE` | 1024-byte NonPagedPoolNx buffers for lower reads | Device.c:61 | Input.c:67; Diagnostics.c:46 |
| 96 | `HidIoTarget` | `WDFIOTARGET` | Default (local) I/O target, i.e. the PDO | Detour.c:77 (only on success) | every lower send; Queue.c:41 (copied while NULL) |
| 104 | `IsHidIoDetourCompleted` | `BOOLEAN` | Detour applied | Detour.c:76 | Device.c:260 |
| 112 | `HidTransportRecoveryTimer` | `WDFTIMER` | One-shot passive retry timer | Device.c:73 | every `WdfTimerStart` |
| 120 | `HidTransportRecoveryWorkItem` | `WDFWORKITEM` | Re-issue a read at PASSIVE | Device.c:82 | Input.c:164 |
| 128 | `DeviceConfigured` | `BOOLEAN` | Mode switch done; upper reads may trigger lower reads | Device.c:91, 126, 166, 240, 279; Input.c:62, 106 | Input.c:25 |
| 136 | `LastReportTime` | `LARGE_INTEGER` | Intended as the QPC count at the last report. Because `KeQueryPerformanceCounter`'s pointer argument is the *frequency* output (7.5), it actually holds **0** (before the first stamp) or the **QPC frequency** (afterwards, always) | Device.c:237, 276; Input.c:190 | Input.c:188 |
| 144 | `PtpInputOn` | `BOOLEAN` | Input mode: TRUE means PTP (mode 3), FALSE means mouse (mode 0) | Hid.c:306, 313 | **never read [DEAD]** |
| 145 | `PtpReportTouch` | `BOOLEAN` | Selective reporting, surface bit | Hid.c:327 | **never read [DEAD]** |
| 146 | `PtpReportButton` | `BOOLEAN` | Selective reporting, button bit | Hid.c:326 | **never read [DEAD]** |

**Locking: there is none.** The driver takes no spin lock, wait lock or interlocked operation anywhere. `DeviceConfigured`, `LastReportTime` and `PtpInputOn` are touched concurrently from up to five contexts:

* the queue dispatch callback (≤ DISPATCH, parallel queue);
* the I/O completion routine, which may run concurrently for several in-flight lower reads;
* the passive timer callback;
* the work item;
* PnP/power callbacks.

The races are benign in C. In Rust, store `DeviceConfigured` as `AtomicBool` and `LastReportTime` as `AtomicI64` (relaxed ordering matches C), or protect them with a WDF spin lock. The read-modify-write race on `LastReportTime` has no observable effect, because every writer stores the same constant, the QPC frequency (7.5).

### 3.2 `WORKER_REQUEST_CONTEXT` (`Device.h:65-70`)

Accessor `WorkerRequestGetContext`; 16 bytes. Attached to every lower read request that the driver creates (Input.c:55; Diagnostics.c:35).

| Off | Field | Meaning |
|---|---|---|
| 0 | `PDEVICE_CONTEXT DeviceContext` | back pointer |
| 8 | `WDFMEMORY RequestMemory` | lookaside buffer used as the output buffer; deleted in the completion routine |

The same pointer is passed as the completion-routine `Context` (Input.c:100). The completion routine reads `RequestMemory` **after deleting the request that owns this context**, which is a use-after-free **[BUG]** (Input.c:246-248; also Diagnostics.c:132-138). Rust: read `RequestMemory` into a local before deleting the request.

### 3.3 `QUEUE_CONTEXT` (`Queue.h:13-18`)

Accessor `PtpFilterQueueGetContext`; 16 bytes.

* `Device` (WDFDEVICE): used by the dispatch callback to reach the device context.
* `DeviceIoTarget` (WDFIOTARGET): always NULL and never read **[DEAD]**.

### 3.4 `BCM5974_PARAM` (`Device.h:19-23`)

`{ int snratio; int min; int max; }`, 12 bytes. The type is named after the Linux bcm5974 driver.

---

## 4. IOCTL handling

### 4.1 Where requests arrive

Only `IRP_MJ_INTERNAL_DEVICE_CONTROL` is handled, by `FilterEvtIoIntDeviceControl` on the default parallel queue. That queue is not power-managed and its callbacks run at ≤ DISPATCH. Every other major function goes down the stack automatically (filter device) [FW]. **Every** internal IOCTL code reaches the handler, and unknown codes are *completed* with `STATUS_NOT_SUPPORTED`. They are **not** passed down.

The upper requester is mshidkmdf, which forwards IRPs it receives from HIDCLASS. HIDCLASS builds them per the HID minidriver contract [FW]:

* For METHOD_NEITHER minidriver IOCTLs, the output buffer is at `Irp->UserBuffer` and the input at `IrpSp->Parameters.DeviceIoControl.Type3InputBuffer`.
* For `IOCTL_HID_GET_FEATURE` and `IOCTL_HID_SET_FEATURE`, `Irp->UserBuffer` points to a `HID_XFER_PACKET`. HIDCLASS sets `OutputBufferLength = sizeof(HID_XFER_PACKET)` for GET and `InputBufferLength = sizeof(HID_XFER_PACKET)` for SET.

[FW] For `IRP_MJ_INTERNAL_DEVICE_CONTROL` with METHOD_NEITHER, KMDF's `WdfRequestRetrieveOutputBuffer` and `WdfRequestRetrieveOutputMemory` return `Irp->UserBuffer` with `OutputBufferLength`, treating it as a kernel-to-kernel request. The driver relies on this for the descriptor, attribute and read IOCTLs.

### 4.2 Dispatch table (Queue.c:76-110)

`requestPending` starts FALSE and `status` starts as `STATUS_UNSUCCESSFUL` (lines 67-68). After the switch, if `requestPending != TRUE`, the request is completed with `WdfRequestComplete(Request, status)` (lines 112-116). That call does not set Information; any Information set earlier by a helper is kept.

IOCTL numbers are `CTL_CODE(FILE_DEVICE_KEYBOARD=0x0B, fn, method, FILE_ANY_ACCESS)`, from the WDK `hidport.h:183-200` and `hidclass.h:133-168`. The values below were computed.

| IOCTL | Value | Method | Handler | Completion |
|---|---|---|---|---|
| `IOCTL_HID_GET_DEVICE_DESCRIPTOR` | 0x000B0003 | NEITHER | `PtpFilterGetHidDescriptor` (Hid.c:6-56) | sync; Information = 9 on success |
| `IOCTL_HID_GET_REPORT_DESCRIPTOR` | 0x000B0007 | NEITHER | `PtpFilterGetReportDescriptor` (Hid.c:91-140) | sync; Information = 493 |
| `IOCTL_HID_READ_REPORT` | 0x000B000B | NEITHER | `PtpFilterInputProcessRequest` (Input.c:6-28) | **pending**; parked in `HidReadQueue`, completed later with 50 bytes |
| `IOCTL_HID_WRITE_REPORT` | 0x000B000F | NEITHER | explicit case → `STATUS_NOT_SUPPORTED` | sync |
| `IOCTL_HID_GET_STRING` | 0x000B0013 | NEITHER | `PtpFilterGetStrings` (Hid.c:142-174) | **forwarded** send-and-forget to the PDO; the lower driver completes it |
| `IOCTL_HID_ACTIVATE_DEVICE` | 0x000B001F | NEITHER | explicit → `STATUS_NOT_SUPPORTED` | sync |
| `IOCTL_HID_DEACTIVATE_DEVICE` | 0x000B0023 | NEITHER | explicit → `STATUS_NOT_SUPPORTED` | sync |
| `IOCTL_HID_GET_DEVICE_ATTRIBUTES` | 0x000B0027 | NEITHER | `PtpFilterGetDeviceAttribs` (Hid.c:58-89) | sync; Information = 32 |
| `IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST` | 0x000B002B | NEITHER | explicit → `STATUS_NOT_SUPPORTED`, so HIDCLASS selective suspend is effectively refused | sync |
| `IOCTL_UMDF_HID_SET_OUTPUT_REPORT` | 0x000B005B | NEITHER | explicit → `STATUS_NOT_SUPPORTED` | sync |
| `IOCTL_UMDF_HID_GET_INPUT_REPORT` | 0x000B005F | NEITHER | explicit → `STATUS_NOT_SUPPORTED` | sync |
| `IOCTL_HID_SET_FEATURE` | 0x000B0191 | IN_DIRECT | `PtpFilterSetHidFeatures` (Hid.c:264-345) | sync; **Information not set** |
| `IOCTL_HID_GET_FEATURE` | 0x000B0192 | OUT_DIRECT | `PtpFilterGetHidFeatures` (Hid.c:176-262) | sync; **Information not set** |
| any other code, for example `IOCTL_HID_GET_INPUT_REPORT` 0x000B01A2, `IOCTL_HID_SET_OUTPUT_REPORT` 0x000B0195, `IOCTL_HID_GET_INDEXED_STRING` 0x000B01E2, `IOCTL_GET_PHYSICAL_DESCRIPTOR` 0x000B019A, `IOCTL_UMDF_HID_GET_FEATURE` 0x000B0057, `IOCTL_UMDF_HID_SET_FEATURE` 0x000B0053 | | | `default:` → `STATUS_NOT_SUPPORTED`, with a warning trace that includes the name from `PtpFilterDiagnosticsIoControlGetString` | sync |

The explicit `case` labels before `default` are cosmetic: they share the default body (Queue.c:100-109). The diagnostic name table (Diagnostics.c:146-180) maps the 15 codes above to their names; anything else is "IOCTL_UNKNOWN".

### 4.3 Per-IOCTL behaviour

#### `IOCTL_HID_GET_DEVICE_DESCRIPTOR` (Hid.c:6-56)

1. `WdfRequestRetrieveOutputMemory(Request, &mem)`. If it fails, return that status, for example `STATUS_BUFFER_TOO_SMALL` when the length is 0 [FW].
2. Switch on `ctx->ProductID`. Only `0x0265` selects a descriptor: `PtpDefaultHidDescriptorMagicTrackpad2`, size `bLength = 9`.
3. If a descriptor was selected: `WdfMemoryCopyFromBuffer(mem, 0, desc, 9)`, which fails with `STATUS_BUFFER_TOO_SMALL` if the output length is under 9 [FW]. Then `WdfRequestSetInformation(Request, 9)` and return SUCCESS.
4. Otherwise return `STATUS_NOT_SUPPORTED`. That includes `ProductID == 0`, which happens if the attribute query never ran, and also after any PnP stop/start, because `PrepareHardware` re-zeroes the IDs and nothing re-reads them (B30).

#### `IOCTL_HID_GET_REPORT_DESCRIPTOR` (Hid.c:91-140)

Same pattern. For PID 0x0265, the size is `PtpDefaultHidDescriptorMagicTrackpad2.DescriptorList[0].wReportLength` (= 493) and the source is `PtpReportDescriptorMagicTrackpad2`. Information is 493 on success. Any other PID gives `STATUS_NOT_SUPPORTED`.

#### `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (Hid.c:58-89)

`WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_DEVICE_ATTRIBUTES)=32, &p, NULL)`. If it fails, return the status. Otherwise:

* `p->Size = 32`
* `p->ProductID = ctx->ProductID` (the **real** PID, 0x0265)
* `p->VendorID = DEVICE_VID = 0x8910`. This is **fake** on purpose, to avoid a hardware-ID collision with the original HID device (`HidCommon.h:42`; comment at Hid.c:79-80; commit `dd364d6`).
* `p->VersionNumber = DEVICE_VERSION = 0x0100` (`HidCommon.h:43`)
* `Reserved[11]` is **not** zeroed.
* `WdfRequestSetInformation(Request, 32)`; return SUCCESS.

As a result, the new HIDCLASS instance enumerates children with IDs like `HID\VID_8910&PID_0265&REV_0100&Col0n` [FW].

#### `IOCTL_HID_GET_STRING` (Hid.c:142-174)

1. `WdfRequestFormatRequestUsingCurrentType(Request)`, which copies the current stack location to the next one unchanged: `Type3InputBuffer` (the string ID) and `UserBuffer` (the output).
2. `WDF_REQUEST_SEND_OPTIONS_INIT(&o, WDF_REQUEST_SEND_OPTION_SEND_AND_FORGET)`.
3. `sent = WdfRequestSend(Request, ctx->HidIoTarget, &o)`, then `*Pending = TRUE`.
4. If `!sent`: `status = WdfRequestGetStatus(Request)` and `*Pending = FALSE`, so the dispatcher completes the request with that error.

The real device's strings (manufacturer, product, serial) therefore come back from HidBth or HidUsb. There is no local string table. If `HidIoTarget` is NULL (detour not done), `WdfRequestSend` gets a NULL handle and the system bugchecks **[RISK]**. It cannot happen on the normal path, because HIDCLASS queries strings only after a successful start.

#### `IOCTL_HID_READ_REPORT` (Input.c:6-28)

1. `WdfRequestForwardToIoQueue(Request, ctx->HidReadQueue)`. If that fails, complete the request with that status and return. The dispatcher's `requestPending = TRUE` prevents a double completion.
2. If `ctx->DeviceConfigured == TRUE`, call `PtpFilterInputIssueTransportRequest(Device)` (section 6.5). Otherwise the request stays parked until recovery issues a lower read.

Requests parked in a manual queue are cancelable by the framework, which completes them with `STATUS_CANCELLED` [FW]. No `EvtIoCanceledOnQueue` is registered.

#### `IOCTL_HID_GET_FEATURE` and `IOCTL_HID_SET_FEATURE`

See 5.5.

### 4.4 Manual queue draining

`HidReadQueue` is drained in two places only:

* one request per lower-read completion (Input.c:176);
* the full drain with `STATUS_CANCELLED` in D0Exit (Device.c:169-178).

It is never purged or stopped explicitly, so its removal-time behaviour is whatever KMDF does [FW].

---

## 5. HID

### 5.1 Report descriptor

* Symbol: `static HID_REPORT_DESCRIPTOR PtpReportDescriptorMagicTrackpad2[]` (`include/Metadata/StaticHidRegistry.h:15-18`), with `HID_REPORT_DESCRIPTOR` = `UCHAR` (`StaticHidRegistry.h:10`).
* Content: `AAPL_MAGIC_TRACKPAD2_PTP_TLC` (`MagicTrackpad2.h:83-137`) followed by `AAPL_PTP_WINDOWS_CONFIGURATION_TLC` (`WindowsHID.h:29-54`). The item-tag macros come from `HidCommon.h:15-40`.
* **Total length: 493 bytes (0x01ED)**: 446 bytes of touchpad TLC and 47 bytes of configuration TLC. This was computed by expanding the macros and checked by hand.
  * `AAPL_MAGIC_TRACKPAD2_PTP_FINGER_COLLECTION_1` (`MagicTrackpad2.h:4-43`) is 70 bytes.
  * `..._COLLECTION_2` (`MagicTrackpad2.h:45-81`) is 64 bytes. It is identical to _1 minus the trailing `45 00 55 00 65 00` (Physical Max 0, Unit Exponent 0, Unit None).
  * The finger order is 1, 1, 2, 1, 2 (`MagicTrackpad2.h:89-101`).
* Appendix A has the byte-exact hex dump; it is authoritative.
* Because the arrays are `static` in a header, every translation unit gets its own copy. Rust needs one `static DESC: [u8; 493]`.

Reports the descriptor defines:

| Report ID | Type | Payload bytes (excluding ID) | Total with ID | Fields (in bit order, LSB first) |
|---|---|---|---|---|
| 0x05 `REPORTID_MULTITOUCH` | Input (Digitizer/Touch Pad 0x0D/0x05, Application) | 49 | **50** | 5 × finger {Confidence:1, TipSwitch:1, pad:6, ContactID:32 (LogMax `27 FF FF FF FF`), X:16 (LogMax 7612, PhysMax 1600, unit cm, exp -2), Y:16 (LogMax 5065, PhysMax 1149)}, then ScanTime:16 (unit s, exp -4, Log/PhysMax 0xFFFF), ContactCount:8 (LogMax 127), Button1:1, pad:7 |
| 0x07 `REPORTID_DEVICE_CAPS` | Feature (inside the Touch Pad TLC) | 2 | **3** | MaximumContacts (0x55):8, PadType/ButtonType (0x59):8 |
| 0x08 `REPORTID_PTPHQA` | Feature (vendor page 0xFF00, usage 0xC5) | 256 | **257** | certification blob |
| 0x04 `REPORTID_REPORTMODE` | Feature (Digitizer/Configuration 0x0D/0x0E TLC, Finger logical collection) | 1 | **2** | Input Mode (0x52):8, LogMax **16** (`MAX_FINGERS`, `WindowsHID.h:4`) |
| 0x06 `REPORTID_FUNCSWITCH` | Feature (Physical collection) | 1 | **2** | Button Switch (0x57):1, Surface Switch (0x58):1, pad:6 |

The input report layout matches `PTP_REPORT` (`HidMiniport.h:44-50`, `#pragma pack(1)`): 1 + 5×9 + 2 + 1 + 1 = **50 bytes**. Offsets are ID@0; Contact[i]@1+9i; ScanTime@46; ContactCount@48; IsButtonClicked@49.

Descriptor quirks. All must be preserved byte-for-byte; none needs code.

* No Logical Minimum is ever set in the touchpad TLC before the feature section, so it defaults to 0.
* ContactID LogMax is `0xFFFFFFFF`, which is -1 when read as a signed value.
* Finger collections 3 and 5 (`_COLLECTION_2`) do not reset Physical Max, Unit or Unit Exponent. Global items survive collection boundaries, so the cm unit, exponent -2 and PhysMax 1149 left by finger 3 carry into the Confidence, Tip and ContactID fields of **finger 4 only**. Finger 4 is a `_COLLECTION_1`, which resets them, so finger 5 starts clean. What finger 5 leaves behind is overwritten before the next main item: the ScanTime prelude sets Unit Exponent, Unit and Physical Max explicitly (`55 0C 66 01 10 47 FF FF 00 00`), so nothing reaches ScanTime.
* The ScanTime globals then persist to the **end of the descriptor**: Unit 0x1001 (seconds), exponent -4 and PhysMax 65535 apply to ContactCount, Button 1 and its padding, the 0x07 and 0x08 features, and the whole configuration TLC (the 0x04 Input Mode and the 0x06 switches). Logical Minimum stays at its default 0 until the `15 00` at 0x019F.
* ScanTime is 16 bits because `REPORT_SIZE 0x10` from the last finger is still in effect.

Defined but **not included** in any descriptor **[DEAD]**: `AAPL_PTP_USERMODE_CONFIGURATION_APP_TLC` (`WindowsHID.h:16-27`). It is a vendor TLC 0xFF00/0x01 with feature report 0x09 (`REPORTID_UMAPP_CONF`) of 3 bytes, 23 bytes in total. Its bytes are in Appendix A.3 for reference.

### 5.2 HID descriptor

Symbol `static HID_DESCRIPTOR PtpDefaultHidDescriptorMagicTrackpad2` (`StaticHidRegistry.h:20-30`). `HID_DESCRIPTOR` is `#pragma pack(1)` in the WDK `hidport.h:126-144`, 9 bytes:

| Byte | Field | Value |
|---|---|---|
| 0 | bLength | 0x09 |
| 1 | bDescriptorType | 0x21 |
| 2-3 | bcdHID | 0x0100 (LE `00 01`) |
| 4 | bCountry | 0x00 |
| 5 | bNumDescriptors | 0x01 |
| 6 | DescriptorList[0].bReportType | 0x22 |
| 7-8 | DescriptorList[0].wReportLength | sizeof(report descriptor) = 493 = 0x01ED (LE `ED 01`) |

Exact bytes: **`09 21 00 01 00 01 22 ED 01`**.

### 5.3 Device attributes

See 4.3. The response is `Size=32`, `VendorID=0x8910`, `ProductID=<real PID>`, `VersionNumber=0x0100`, `Reserved` untouched.

### 5.4 Strings

The driver has no string table. `IOCTL_HID_GET_STRING` is forwarded to the PDO (4.3). `IOCTL_HID_GET_INDEXED_STRING` is not supported.

### 5.5 Feature reports

Shared preamble. For GET (Hid.c:192-205):

1. `WdfRequestGetParameters`.
2. If `Parameters.DeviceIoControl.OutputBufferLength < sizeof(HID_XFER_PACKET)` (16), return `STATUS_BUFFER_TOO_SMALL`.
3. `hidContent = (PHID_XFER_PACKET) WdfRequestWdmGetIrp(Request)->UserBuffer`. If it is NULL, return `STATUS_INVALID_DEVICE_REQUEST`.

SET uses `InputBufferLength` in place of `OutputBufferLength` (Hid.c:279-292).

`HID_XFER_PACKET` (WDK `hidclass.h:180-184`, natural alignment) is 16 bytes on LP64:

* `PUCHAR reportBuffer` @0
* `ULONG reportBufferLen` @8
* `UCHAR reportId` @12
* 3 padding bytes

The switch is on `reportId`. `reportBuffer` and `reportBufferLen` are trusted without validation. They are kernel pointers from HIDCLASS, and `reportBuffer[0]` is the report ID byte [FW].

**GET_FEATURE** (`PtpFilterGetHidFeatures`, Hid.c:176-262). **Information is never set**, so it stays at the IRP's incoming value, normally 0. Preserve that.

| reportId | Size check | Action | Status |
|---|---|---|---|
| 0x07 DEVICE_CAPS (lines 209-230) | `reportBufferLen < sizeof(PTP_DEVICE_CAPS_FEATURE_REPORT)=3` → `STATUS_INVALID_BUFFER_SIZE` | `buf[1] = MaximumContactPoints = PTP_MAX_CONTACT_POINTS = 5`; `buf[2] = ButtonType = PTP_BUTTON_TYPE_CLICK_PAD = 0`; `buf[0] = ReportID = 0x07`. Writes exactly 3 bytes: `07 05 00`. | SUCCESS |
| 0x08 PTPHQA (lines 231-250) | `reportBufferLen < sizeof(PTP_DEVICE_HQA_CERTIFICATION_REPORT)=257` → `STATUS_INVALID_BUFFER_SIZE` | **[BUG]** `*certReport->CertificationBlob = DEFAULT_PTP_HQA_BLOB;` expands to `blob[0] = 0xfc, 0x28, 0xfe, ...;`. The **comma operator** binds looser than `=`, so only `buf[1] = 0xFC` is written. The other 255 literals are evaluated and discarded. Then `buf[0] = 0x08`. **`buf[2..257]` are left exactly as the caller supplied them.** | SUCCESS |
| anything else, including 0x04, 0x06, 0x09 and 0x05 | none | none | `STATUS_NOT_SUPPORTED` |

**SET_FEATURE** (`PtpFilterSetHidFeatures`, Hid.c:264-345). **Information is never set.** `reportBufferLen` is **never checked**: the code reads `reportBuffer[1]` regardless.

| reportId | Action | Status |
|---|---|---|
| 0x04 REPORTMODE (lines 296-320) | `mode = reportBuffer[1]` (`PTP_DEVICE_INPUT_MODE_REPORT.Mode`, `HidMiniport.h:20-23`). If `mode == 0` (`PTP_COLLECTION_MOUSE`), set `PtpInputOn = FALSE`. If `mode == 3` (`PTP_COLLECTION_WINDOWS`), set `PtpInputOn = TRUE`. Any other value changes nothing. | SUCCESS for every mode value |
| 0x06 FUNCSWITCH (lines 321-333) | `b = reportBuffer[1]`; `PtpReportButton = b & 1` (bitfield `ButtonReport:1`, the LSB); `PtpReportTouch = (b >> 1) & 1` (`SurfaceReport:1`). This follows MSVC's LSB-first bit-field layout (`HidMiniport.h:26-31`). | SUCCESS |
| anything else, including 0x07, 0x08 and 0x09 | none | `STATUS_NOT_SUPPORTED` |

**[QUIRK/DEAD]** `PtpInputOn`, `PtpReportTouch` and `PtpReportButton` are never read. Input mode and selective reporting have **no effect**: the driver emits PTP touch and button reports in every mode, including mouse mode 0 and with both switches off.

Not implemented: battery report, latency mode, the user-mode configuration report (0x09), output reports, and GET_INPUT_REPORT.

---

## 6. Device I/O to the hardware

### 6.1 What is *not* used

* **USB**: no USB pipes or interface selection, no continuous reader, no `WdfUsbTarget*` calls and no control transfers. The USB headers are included but unused.
* **SPI and ACPI**: nothing.
* Resources are not used; PrepareHardware ignores the resource lists.
* No interrupts or DPCs.
* No remote I/O targets. Every request goes to the **default local I/O target**, `WdfDeviceGetIoTarget(device)`, stored in `ctx->HidIoTarget` (Detour.c:77). That target is the next-lower device object: the HID collection PDO, owned by the HidBth or HidUsb driver object.
* Every lower request is an `IRP_MJ_INTERNAL_DEVICE_CONTROL` with a HID minidriver IOCTL. After the detour, HidBth or HidUsb services these directly, as if HIDCLASS had sent them [FW].

### 6.2 Detour of HIDCLASS (`Detour.c:6-86`, structures in `Hac.h`)

This is the single most dangerous construct in the driver; see section 11. The steps, exactly:

1. If `ctx->WdmDeviceObject == NULL` or `ctx->WdmDeviceObject->DriverObject == NULL`, return `STATUS_UNSUCCESSFUL` (lines 22-26).
2. `lower = IoGetLowerDeviceObject(ctx->WdmDeviceObject)` (line 29). The returned object is **referenced**. If it is NULL, return `STATUS_UNSUCCESSFUL` (lines 30-34).
3. `drv = lower->DriverObject`. If it is NULL, fail with `STATUS_UNSUCCESSFUL` and `ObDereferenceObject(lower)` (lines 36-41).
4. If `drv->DriverExtension == NULL`, fail the same way (lines 44-48).
5. `cli = ((DRIVER_EXTENSION_EXT*)drv->DriverExtension)->IoClientExtension` (line 51). If it is NULL, fail (lines 52-56). Hand-declared layout (`Hac.h:25-31`) on LP64:
   * `DriverObject`@0, `AddDevice`@8, `Count`@16 (u32 plus 4 bytes padding), `ServiceKeyName`@24 (UNICODE_STRING, 16 bytes), **`IoClientExtension`@0x28**.
   * This mirrors the undocumented internal `DRIVER_EXTENSION.ClientDriverExtension` [RISK].
6. `strncmp("HIDCLASS", (char*)cli->ClientIdentificationAddress, sizeof("HIDCLASS") = 9)`. If the result is not 0, fail (lines 58-62).
   * `IO_CLIENT_EXTENSION` (`Hac.h:19-23`): `NextExtension`@0, `ClientIdentificationAddress`@8, 16 bytes in total.
   * The comparison covers all 9 bytes including the NUL. HIDCLASS calls `IoAllocateDriverObjectExtension(minidriver, (PVOID)"HIDCLASS"-literal, ...)` [FW, inferred], so the client ID address points at that string.
   * Only the **first** client extension is examined; `NextExtension` is never walked.
   * Up to 9 bytes at an arbitrary client-ID address are read without validation [RISK]. `strncmp` stops at the first mismatching byte (or after the matching NUL), so a non-HIDCLASS client ID may be read for as little as 1 byte. A Rust port should compare byte by byte and stop at the first mismatch, so that it touches exactly the same memory.
7. `cls = (HIDCLASS_DRIVER_EXTENSION*)(cli + 1)`, i.e. `cli + 16 bytes` (line 64). The hand-declared layout (`Hac.h:7-17`) on LP64:
   * `MinidriverObject`@0, `RegistryPath`@8, `DeviceExtensionSize`@24
   * **`MajorFunction[28]`@32**
   * `AddDevice`@256, `DriverUnload`@264, `ReferenceCount`@272, `ListEntry`@280, `DevicesArePolled`@296; total size 304
   * `IRP_MJ_MAXIMUM_FUNCTION` = 0x1B (WDK `wdm.h`), so the array has 28 entries.
8. **Patch** (line 72): `drv->MajorFunction[IRP_MJ_INTERNAL_DEVICE_CONTROL /*0x0F*/] = cls->MajorFunction[0x0F]`.
   * The source slot is at `cli + 16 + 32 + 15*8` = **`cli + 0xA8`**.
   * It is a plain pointer store: not atomic, not interlocked, no memory barrier, and nothing saves the previous value (HIDCLASS's dispatch routine) [RISK].
   * The patch is never reverted, not on device removal and not on driver unload.
   * The value written is the minidriver's own original routine, not a function in this driver, so nothing dangles after unload.
   * The patch is **global**: it affects every device object of HidBth (or HidUsb) system-wide. Keyboards, mice and other HID devices on the same transport lose HIDCLASS handling of `IRP_MJ_INTERNAL_DEVICE_CONTROL` [RISK].
   * HIDCLASS also hooks other major functions (comment at lines 67-70). Only 0x0F is patched.
9. `ctx->IsHidIoDetourCompleted = TRUE`; `ctx->HidIoTarget = WdfDeviceGetIoTarget(Device)` (lines 76-77).
10. `ObDereferenceObject(lower)` (lines 80-82); return `STATUS_SUCCESS`.

The patch runs on every first start of every filter instance. It is idempotent.

Why it works [FW, inferred from the HID minidriver model]: HIDCLASS collection PDOs carry a device extension whose public `HID_DEVICE_EXTENSION` header (`PhysicalDeviceObject`, `NextDeviceObject`, `MiniDeviceExtension`) points at the parent FDO's minidriver data. The minidriver's internal IOCTL routine can therefore service an IRP sent to the PDO. The Rust port must reproduce this byte-for-byte to keep working. Treat it as an unsafe, version-fragile hack and document it as such.

### 6.3 Self-identification: `IOCTL_HID_GET_DEVICE_ATTRIBUTES` to the PDO

See 2.6 step 3. It is synchronous with no timeout, and the output buffer is a stack `HID_DEVICE_ATTRIBUTES` in `Irp->UserBuffer`. Expected values:

| Transport | VendorID | ProductID | VersionNumber |
|---|---|---|---|
| BT | 0x004C | 0x0265 | not documented in the repo |
| USB | 0x05AC | 0x0265 | 0x0855, per the INF hardware ID `REV_0855` |

### 6.4 Mode switch: `PtpFilterConfigureMultiTouch` (Device.c:286-422)

1. If `VendorID ∉ {0x05AC, 0x004C}`, return `STATUS_NOT_SUPPORTED` (lines 308-312). If `ProductID != 0x0265`, return `STATUS_NOT_SUPPORTED` (lines 313-317). Neither case touches the size fields.
2. Zero a 32-byte stack buffer `hidPacketBuffer[HID_XFER_PACKET_SIZE=32]` (`Device.h:12`; line 319) and cast it to `PHID_XFER_PACKET` (line 320). The buffer is not guaranteed to be 8-byte aligned [RISK].
3. Per-transport parameters and packet:

| Field | USB (VID 0x05AC), lines 322-342 | BT (VID 0x004C), lines 343-362 |
|---|---|---|
| `InputFingerSize` | `FSIZE_TYPE5` = 9 | 9 |
| `InputHeaderSize` | `HOFFSET_TYPE_USB_5` = 6*2 = **12** | `HOFFSET_TYPE_BTH_5` = 2*2 = **4** |
| `InputFingerDelta` | `FDELTA_TYPE5` = 0 | 0 |
| `InputButtonDelta` | `BOFFSET_TYPE5` = 1 | 1 |
| `X` {snratio, min, max} | {250, -3678, 3934} | {250, -3678, 3934} |
| `Y` {snratio, min, max} | {250, -2479, 2586} | {250, -2479, 2586} |
| `packet.reportId` | 0x02 | 0xF1 |
| `packet.reportBufferLen` | 4 | 3 |
| `packet.reportBuffer` | `(PUCHAR)packet + sizeof(HID_XFER_PACKET)` = buffer + **16** | same |
| payload bytes at buffer+16 | **`02 01 00 00`** | **`F1 02 01`** |

   The size fields are written **before** the send. They stay set even if the send fails. The final `else` ("Unrecognized device detected", Device.c:363-368) cannot be reached, because step 1 already rejects other VIDs **[DEAD]**.
4. `WdfRequestCreate(WDF_NO_OBJECT_ATTRIBUTES, ctx->HidIoTarget, &req)` (line 372). If it fails, return the status. That path skips cleanup, and `configRequest` is never assigned.
5. `WdfMemoryCreatePreallocated(attr{ParentObject=req}, packet, 32, &mem)` (lines 378-380). If it fails, `goto cleanup` (lines 381-384): `WdfObjectDelete(req)`, then return that error.
6. `WdfIoTargetFormatRequestForInternalIoctl(target, req, IOCTL_HID_SET_FEATURE /*0x000B0191, METHOD_IN_DIRECT*/, mem /*input*/, NULL, NULL /*no output*/, NULL)` (lines 386-388). The framework places the 32-byte input buffer according to its METHOD_IN_DIRECT rules for internal IOCTLs, with `InputBufferLength=32` [FW]. If formatting fails, `goto cleanup` (lines 389-392): the request (and `mem`, its child) is deleted and the error is returned.
7. `irp = WdfRequestWdmGetIrp(req)`. If it is NULL, return `STATUS_UNSUCCESSFUL` (395-400). **`irp->UserBuffer = packet`** (line 403). The HID minidriver reads the `HID_XFER_PACKET` from `Irp->UserBuffer` [FW], which is why this manual step is needed (comment at lines 371, 394, 402).
8. `WDF_REQUEST_SEND_OPTIONS_INIT(&o, WDF_REQUEST_SEND_OPTION_SYNCHRONOUS)` (line 405). There is **no timeout** [RISK: may block start or resume indefinitely]. `WdfRequestSend(req, target, &o)`:
   * If it returns FALSE: `status = WdfRequestGetStatus(req)`.
   * If it returns TRUE: `status = STATUS_SUCCESS` **without checking `WdfRequestGetStatus`** (lines 406-413).
   * [FW] In the KMDF source, the synchronous path returns TRUE when the request was submitted (the `SubmitSent` action), regardless of its completion status. A SET_FEATURE that the device or HidBth fails is therefore treated as success **[BUG]**. If exact parity matters, verify this against the KMDF build in use.
9. Cleanup: `WdfObjectDelete(req)` (lines 415-418), which also deletes `mem` as its child. Return `status`.

The two packets are the Apple "wellspring"/multitouch enable feature reports. The BT packet `F1 02 01` matches what Linux `hid-magicmouse` is commonly cited as sending for MT2. The USB packet here is 4 bytes (`02 01 00 00`, reportBufferLen 4). Its equivalence to the Linux USB value cannot be verified from this repository. Port these bytes and lengths exactly as the C code has them.

### 6.5 Lower read path: `PtpFilterInputIssueTransportRequest` (Input.c:39-117)

Each call issues exactly one asynchronous read:

1. `WdfRequestCreate(attr{context WORKER_REQUEST_CONTEXT, ParentObject=Device}, ctx->HidIoTarget, &req)` (lines 55-57). If it fails: set `DeviceConfigured = FALSE`, call `WdfTimerStart(timer, WDF_REL_TIMEOUT_IN_SEC(3))` (-30,000,000), and return (lines 58-65). The comment says this "can fail for Bluetooth devices".
2. `WdfMemoryCreateFromLookaside(ctx->HidReadBufferLookaside, &mem)` (line 67). The buffer is 1024 bytes. If it fails: `WdfObjectDelete(req)`, `WdfDeviceSetFailed(ctx->Device, WdfDeviceFailedAttemptRestart)`, return (lines 68-74).
3. Request context: `DeviceContext = ctx`, `RequestMemory = mem` (lines 78-80).
4. `WdfIoTargetFormatRequestForInternalIoctl(target, req, IOCTL_HID_READ_REPORT /*0x000B000B, NEITHER*/, NULL, NULL, mem /*output*/, NULL)` (lines 81-82). [FW] The IRP gets `Irp->UserBuffer = buffer` and `OutputBufferLength = 1024`. If formatting fails: delete `mem` and `req`, then `WdfDeviceSetFailed(..., WdfDeviceFailedAttemptRestart)`, and return (lines 83-97).
5. `WdfRequestSetCompletionRoutine(req, PtpFilterInputRequestCompletionCallback, requestContext)` (line 100).
6. `WdfRequestSend(req, target, NULL)`, asynchronous with no options (line 102). If it fails: set `DeviceConfigured = FALSE`, start the timer for 3 s, and delete `mem` then `req` (lines 103-116).

Pipeline invariants [CODE]:

* One lower read is issued per upper `READ_REPORT` that arrives while `DeviceConfigured` is TRUE.
* One more is issued per timer firing where the restart path reports success.
* One more is issued per zero-length completion, via the work item. This is not strictly per completion: `WdfWorkItemEnqueue` on an already-queued item is a no-op. If several zero-length completions land before the work item runs, only one read is restored. Likewise, several malformed packets inside one 3 s window, or several `WdfRequestCreate`/send failures, restart only one chain, because `WdfTimerStart` on a queued timer only resets its due time. The number of lower reads in flight can therefore shrink permanently, until new upper READ_REPORTs (while `DeviceConfigured`) replace them.
* A successful completion does **not** issue another read (comment at Input.c:251-252).
* If a lower read completes while `HidReadQueue` is empty, the packet is dropped and that read chain ends.

There is no fixed number of pending reads and no continuous reader. [FW] HIDCLASS keeps a small number of "ping-pong" reads pending, so in steady state the number of lower reads in flight roughly equals the number of HIDCLASS ping-pong IRPs.

### 6.6 Completion: `PtpFilterInputRequestCompletionCallback` (Input.c:119-253)

The full parsing algorithm is in section 7. The control flow:

```
ctx          = requestContext->DeviceContext
len          = (size_t)(LONG) WdfRequestGetInformation(Request)        // line 150
buf          = WdfMemoryGetBuffer(Params->Parameters.Ioctl.Output.Buffer, NULL)   // line 151
hdr, fsz     = ctx->InputHeaderSize, ctx->InputFingerSize
// NOTE: Params->IoStatus.Status / WdfRequestGetStatus(Request) is NEVER examined.   [BUG]
if ctx->VendorID not in {0x05AC, 0x004C}:                              // 156-160
    WdfDeviceSetFailed(ctx->Device, WdfDeviceFailedNoRestart); goto cleanup
if len == 0:                                                           // 163-166 ("<= 0" on size_t)
    WdfWorkItemEnqueue(ctx->HidTransportRecoveryWorkItem); goto cleanup  // re-issue a read at PASSIVE
if len < ctx->InputHeaderSize || (len - ctx->InputHeaderSize) % ctx->InputFingerSize != 0:   // 169-173
    WdfTimerStart(timer, WDF_REL_TIMEOUT_IN_SEC(3)); goto cleanup     // "malformed": reconfigure in 3 s
if !NT_SUCCESS(WdfIoQueueRetrieveNextRequest(ctx->HidReadQueue, &ptpReq)):   // 176-180
    goto cleanup                                                       // no parked upper read: drop packet
build PTP_REPORT (section 7)                                           // 182-223
if !NT_SUCCESS(WdfRequestRetrieveOutputMemory(ptpReq, &m)):            // 225-231
    WdfDeviceSetFailed(..., AttemptRestart); goto cleanup              // [BUG] ptpReq never completed
if !NT_SUCCESS(WdfMemoryCopyFromBuffer(m, 0, &report, 50)):            // 233-239
    WdfDeviceSetFailed(..., AttemptRestart); goto cleanup              // [BUG] ptpReq never completed
WdfRequestSetInformation(ptpReq, 50); WdfRequestComplete(ptpReq, STATUS_SUCCESS)   // 241-242
cleanup:
WdfObjectDelete(Request)                                               // 246
if requestContext->RequestMemory != NULL: WdfObjectDelete(it)          // 247-249  [BUG] use-after-free of context
```

Consequences:

* **No report-ID check.** Any raw report whose length is `hdr + 9n` (n ≥ 0) is parsed as touch data. A BT report of exactly 4 bytes gives n = 0: a PTP report with ContactCount 0 and the button bit taken from `buf[1]`.
* Any other length, for example a battery or status report, counts as "malformed". That read chain stalls until the 3 s timer runs `SelfManagedIoRestart` (re-sending the mode switch) and issues one new read.
* A failed lower read normally completes with Information 0, so it is re-issued immediately via the work item. If the lower device keeps failing fast, for example during removal or Dx, this becomes an unbounded retry loop [RISK].
* `len` is computed as `(size_t)(LONG)info`. Information values of 2^31 or more would sign-extend to huge sizes; harmless in practice. Rust: `info as u32 as i32 as isize as usize`.
* **Division by zero [BUG, latent]**: if `InputFingerSize == 0` (the device never went through `ConfigureMultiTouch`'s VID/PID branch), the `%` at line 169 faults. Check 0 catches non-Apple VIDs. An Apple VID with a PID other than 0x0265 leaves the sizes at 0 and would reach the `%` if a lower read were ever issued, for example from the timer. Rust's `%` by zero panics, so guard it and document the guard as a deviation.

### 6.7 Timers and work items

| Object | Config | Started or enqueued from | Due time | Effect |
|---|---|---|---|---|
| `HidTransportRecoveryTimer` | one-shot, passive, parent = device (Device.c:68-76) | SelfManagedIoInit configure failure (Device.c:232) | 2 s | Restart, then possibly a read |
| | | SelfManagedIoRestart configure failure (Device.c:266) | 2 s | |
| | | `WdfRequestCreate` failure in IssueTransportRequest (Input.c:63) | 3 s | |
| | | `WdfRequestSend` failure in IssueTransportRequest (Input.c:107) | 3 s | |
| | | malformed input length (Input.c:171) | 3 s | |
| | | **[DEAD]** Diagnostics.c:42, 51 (200 µs, i.e. -2000) and :94 (50 µs, i.e. -500) | | |
| `HidTransportRecoveryWorkItem` | default config, parent = device (Device.c:79-85) | zero-length lower read (Input.c:164) | immediately (PASSIVE worker) | `PtpFilterInputIssueTransportRequest` |

`WdfTimerStart` on an already-queued timer resets its due time, and `WdfWorkItemEnqueue` on an already-queued work item is a no-op [FW]. **Nothing ever calls `WdfTimerStop`.** The timer can fire after D0Exit (Dx) or during removal; it is deleted with the device. When it fires in Dx it sends SET_FEATURE to a device in low power, sets `DeviceConfigured = TRUE` and issues a read [RISK/BUG].

### 6.8 Power transitions

* **D0 entry**: no action. Configuration happens in SelfManagedIoInit (first start) or SelfManagedIoRestart (every later D0), which both send the mode-switch SET_FEATURE.
* **D0 exit**: clears `DeviceConfigured` and cancels the parked upper reads (`STATUS_CANCELLED`). In-flight lower reads, the timer and the work item are left alone. The local I/O target's state across Dx is managed by KMDF [FW]; the driver never calls `WdfIoTargetStart`, `WdfIoTargetStop` or `WdfIoTargetPurge`.
* **Selective suspend / idle / wake**: none. The filter is not the power-policy owner, and `IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST` is refused with `STATUS_NOT_SUPPORTED`.
* **PnP stop / restart (resource rebalance) [B30]**: the sequence is [FW] stop, then `PrepareHardware`, `D0Entry`, `SelfManagedIoRestart`; `SelfManagedIoInit` is *not* re-run. The steps:
  1. `PrepareHardware` zeroes VID, PID and version (Device.c:123-125).
  2. `SelfManagedIoRestart` calls `ConfigureMultiTouch`, which fails with `STATUS_NOT_SUPPORTED`. That failure is masked as SUCCESS, the 2 s timer is armed, and `DeviceConfigured` stays FALSE.
  3. Each timer firing repeats step 2 and still issues one lower read (B12). The retry never ends.
  4. The first of those reads to complete hits the VID check (Input.c:156-160) and calls `WdfDeviceSetFailed(WdfDeviceFailedNoRestart)`, which tears the device down for good.

  In the meantime, GET_DEVICE/REPORT_DESCRIPTOR return `STATUS_NOT_SUPPORTED` and GET_DEVICE_ATTRIBUTES reports PID 0. A HID collection PDO has no hardware resources, so a rebalance is rare. Power transitions (Dx to D0) do **not** call `PrepareHardware` and are unaffected.
* **Surprise removal / remove**: no handlers. Cleanup is entirely framework-driven through the device's child objects (timer, work item, queues). The lookaside list is a driver child (2.4).

---

## 7. Input parsing

### 7.1 Supported families

There is one family. The C code calls it "type 5" (Magic Trackpad), after the Linux bcm5974/magicmouse naming. It comes in two transports that differ only in header size and mode-switch packet:

| Parameter | USB 05AC:0265 | BT 004C:0265 | Source |
|---|---|---|---|
| header bytes before finger 0 | 12 | 4 | HidDevice.h:9-10 via Device.c:324/345 |
| finger record size | 9 | 9 | HidDevice.h:24 |
| extra finger delta | 0 | 0 | HidDevice.h:34 |
| button byte offset (absolute from buffer start) | 1 | 1 | HidDevice.h:17 |
| X.min / X.max | -3678 / 3934 | same | Device.c:329-330 / 350-351 |
| Y.min / Y.max | -2479 / 2586 | same | Device.c:332-333 / 353-354 |
| snratio | 250 (unused) | 250 (unused) | |
| max contacts reported | 5 | 5 | WindowsHID.h:6 |

The descriptor logical maxima are X.max − X.min = 7612 (`26 BC 1D`) and Y.max − Y.min = 5065 (`26 C9 13`). They are hard-coded in the descriptor, not computed. Physical maxima are 1600 and 1149, in units of 0.01 cm.

**[DEAD]** constants for the other Apple families (`HidDevice.h`): `HOFFSET_TYPE_USB_1..4` (26, 30, 38, 46), `BOFFSET_TYPE1..4` (0, 15, 23, 31), `FSIZE_TYPE1..4` (28, 28, 28, 30), `FDELTA_TYPE1..4` (0, 0, 0, 2), `HAS_INTEGRATED_BUTTON`, `MAX_FINGER_ORIENTATION` (16384), `TRACKPAD_BUTTON_DATA` (41-46) and `TRACKPAD_FINGER` (50-64, 28 bytes; it is only used as an intermediate pointer type at Input.c:199).

### 7.2 Raw finger record: `TRACKPAD_FINGER_TYPE5` (HidDevice.h:67-85)

The record is 9 bytes, packed (`pshpack2`, but every member is a byte). The bit-fields use MSVC's LSB-first layout.

| Byte | C field | Use |
|---|---|---|
| 0 | `AbsoluteX` | X bits 0-7 |
| 1 | `AbsoluteXY` | bits 0-4: X bits 8-12; bits 5-7: Y bits 0-2 |
| 2 | `AbsoluteY[0]` | Y bits 3-10 |
| 3 | `AbsoluteY[1]` | bits 0-1: Y bits 11-12; bits 2-7 ignored |
| 4 | `TouchMajor` | TipSwitch source |
| 5 | `TouchMinor` | Confidence source |
| 6 | `Size` | unused |
| 7 | `Pressure` | unused |
| 8 | `OrientationAndOrigin`: union of `{Id:4 (bits 0-3), Orientation:4 (bits 4-7)}` and `RawValue` | ContactID = low nibble |

Viewed as a little-endian u32 over bytes 0-3: X13 = bits 0-12 and Y13 = bits 13-25, both two's-complement 13-bit values. Bits 26-31 are ignored.

### 7.3 Algorithm (Input.c:182-223), exact arithmetic

Inputs: `buf[0..len)`, plus `hdr`, `fsz = 9`, `fdelta = 0` and `bdelta = 1` from the context.

1. `report.ReportID = 0x05` and `report.IsButtonClicked = 0` (lines 183-184). **No other bytes are initialised** (`PTP_REPORT ptpOutputReport;` on the stack, line 132). See 7.6.
2. Scan time (lines 187-190): see 7.5. In practice this is a constant **0**; the only exception is 0xFF while `LastReportTime` has never been stamped.
3. `raw_n = (len - hdr) / 9` (size_t division). If `raw_n >= 5`, set it to 5. `report.ContactCount = (u8)raw_n` (lines 194-196). Fingers past 5 are ignored: the first 5 in packet order are taken.
4. For `i in 0..raw_n` (lines 197-218), with `f = buf + hdr + fdelta + i*9`:
   * `tmp_x: u16 = read_u16_le(f[0..2]) & 0x1FFF` (line 202). This is an unaligned u16 read through `USHORT*`.
   * `tmp_y: u32 = read_u32_le(f[0..4])` (line 203). This is an unaligned u32 read; the `(INT)` cast followed by assignment to `unsigned int` is a no-op.
   * `x: i32 = (i16)(tmp_x << 3) >> 3` (line 205). `tmp_x << 3` is at most 0xFFF8 and fits in an int. Casting to `SHORT` wraps (MSVC), and `>> 3` on a negative value is arithmetic (MSVC). The result is **sign-extend-13(tmp_x)**, in the range -4096..4095.
   * `y: i32 = (-(i32)(tmp_y << 6)) >> 19` (line 206). The precedence is: `tmp_y << 6` (u32, the top 6 bits are discarded), then the cast to i32 (wrap), then **unary minus first** (wrapping, so `i32::MIN` stays `i32::MIN`), then **arithmetic shift right by 19**.
     * Let `v = (tmp_y << 6) as i32 = Y13*2^19 + r`, where `r = (tmp_y & 0x1FFF) << 6`, in the range 0..2^19-1.
     * Then `y = floor((-v) / 2^19)`, which is `-Y13` when `r == 0` (the X field bits are all 0) and **`-Y13 - 1` otherwise**. That is an off-by-one that depends on the X bits **[QUIRK, preserve]**.
     * Special case: `Y13 = -4096` with X bits 0 gives `v = i32::MIN`. Negation wraps to `i32::MIN`, and the result is -4096, not +4096.
     * Rust: `let y = ((tmp_y << 6) as i32).wrapping_neg() >> 19;`
     * The negation flips the axis: the device's Y increases upward, PTP's increases downward.
   * Offset and lower clamp (lines 207-208):

     ```
     x = (x - X.min) > 0 ? (x - X.min) : 0
     y = (y - Y.min) > 0 ? (y - Y.min) : 0
     ```

     With the constants, `x' = max(x + 3678, 0)` and `y' = max(y + 2479, 0)`. There is **no upper clamp**: x' can reach 4095 + 3678 = 7773 (above LogMax 7612) and y' can reach 4095 + 2479 = 6574 (above LogMax 5065). y peaks at 4095, either with Y13 = -4095 and X bits 0, or with Y13 = -4096 and X bits nonzero. The results are stored truncated to `USHORT` (lines 211-212), which is lossless because they are non-negative and below 65536.
   * `Contacts[i].ContactID = f[8] & 0x0F` (line 210). The ULONG ID is in the range 0..15; there is no remapping or tracking.
   * `Contacts[i].TipSwitch = (((i16)f[4]) << 1) > 0`, which is **`f[4] != 0`** (line 213).
   * `Contacts[i].Confidence = (((i16)f[5]) << 1) < 345 && (((i16)f[5]) << 1) < 345` (line 217). The same operand is tested twice **[BUG: probably meant TouchMajor && TouchMinor]**. The result is **`f[5] <= 172`**. This is the only "palm rejection".
   * The 6 padding bits of the contact's flag byte are **not written** (bit-field assignments), so they keep stack garbage.
5. Button (lines 221-223): if `buf[bdelta=1] & 1`, then `IsButtonClicked = 1`. This uses the absolute offset, not the header-relative one.
6. The completion copies all 50 bytes of `report` (section 6.6).

Pressure (`f[7]`), Size (`f[6]`), Orientation (high nibble of `f[8]`) and the header bytes other than `buf[1]` bit 0 are ignored. Rejection and confidence logic beyond step 4 does not exist. No state is kept across reports except `LastReportTime`, and after the first stamp that is a constant (the QPC frequency, 7.5).

Rust reference (behaviour-exact):

```rust
const X_MIN: i32 = -3678;
const Y_MIN: i32 = -2479;

/// One 9-byte type-5 finger -> (flags byte, contact id, x, y).
/// flags bit0 = Confidence, bit1 = TipSwitch, bits 2..7 = 0 (C leaves them as garbage; see 7.6).
fn parse_finger(f: &[u8; 9]) -> (u8, u32, u16, u16) {
    let tmp_x = u16::from_le_bytes([f[0], f[1]]) & 0x1FFF;
    let tmp_y = u32::from_le_bytes([f[0], f[1], f[2], f[3]]);
    let x = (((tmp_x << 3) as i16) >> 3) as i32;
    let y = ((tmp_y << 6) as i32).wrapping_neg() >> 19;
    let x = if x - X_MIN > 0 { x - X_MIN } else { 0 };
    let y = if y - Y_MIN > 0 { y - Y_MIN } else { 0 };
    let conf = ((f[5] as i32) << 1) < 345;   // f[5] <= 172
    let tip  = ((f[4] as i32) << 1) > 0;     // f[4] != 0
    ((conf as u8) | ((tip as u8) << 1), (f[8] & 0x0F) as u32, x as u16, y as u16)
}
```

### 7.4 Output report: `PTP_REPORT` (HidMiniport.h:33-50), `#pragma pack(1)`, 50 bytes

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | ReportID = 0x05 |
| 1 + 9i (i = 0..4) | 1 | `PTP_CONTACT` flags: bit0 Confidence, bit1 TipSwitch, bits 2-7 padding (MSVC LSB-first bit-fields) |
| 2 + 9i | 4 | ContactID (u32 LE) |
| 6 + 9i | 2 | X (u16 LE) |
| 8 + 9i | 2 | Y (u16 LE) |
| 46 | 2 | ScanTime (u16 LE) |
| 48 | 1 | ContactCount |
| 49 | 1 | IsButtonClicked (0 or 1) |

Rust: serialise into `[u8; 50]` by hand rather than relying on `#[repr(C, packed)]` bit-fields. The completed request's Information is 50 and its status is `STATUS_SUCCESS`.

### 7.5 Scan time (Input.c:186-190)

**Corrected in verification.** The WDK prototype is `LARGE_INTEGER KeQueryPerformanceCounter(_Out_opt_ PLARGE_INTEGER PerformanceFrequency)` (`km/wdm.h`). The counter is the **return value**, and the pointer argument receives the **frequency**. Every call in this driver passes a pointer and throws the return value away:

* `KeQueryPerformanceCounter(&ctx->LastReportTime)` at Device.c:237 and 276 stores the QPC frequency in `LastReportTime`.
* `KeQueryPerformanceCounter(&currentTSC)` at Input.c:187 stores the QPC frequency in `currentTSC`. The counter value is discarded.

What the code actually computes:

```
currentTSC = QPC_frequency                          // constant for the whole boot session
delta = (currentTSC - ctx->LastReportTime) / 100    // LONGLONG, C truncating division
ScanTime = (delta >= 0xFF) ? 0xFF : (USHORT)delta   // saturating compare as written
ctx->LastReportTime = currentTSC                    // = QPC_frequency
```

Observable result **[BUG, observable]**:

* **ScanTime is 0 in every report** once `LastReportTime` holds the frequency. That is the case after any successful configuration (Init step 6 or Restart, including the else branch and the timer path) and after the first emitted report.
* The only non-zero value comes from a report emitted while `LastReportTime` is still its initial 0. That requires `ConfigureMultiTouch` to have failed in Init and in every Restart since, while a timer-issued read still delivered a valid-length packet. In that case `delta = freq / 100`, which is at least 255 for any frequency of 25,500 Hz or more (all real systems: 10 MHz is typical on x64, and ARM64 uses its generic-timer frequency). That one report gets **0xFF**, and every later report gets 0.
* The "100us unit" comment and PTP's expectation (a free-running 100 µs counter that wraps at 0xFFFF) are not met at all. Windows today receives a constant ScanTime of 0, and the driver ships and works with that.
* A negative delta cannot occur, and the read-modify-write race between parallel completions has no effect: every writer stores the same constant. `PrepareHardware` does not reset `LastReportTime`.

Rust, mirroring the C behaviour exactly (B6 "Exact" option):

```rust
let mut freq: i64 = 0;
// return value (the counter) intentionally discarded, as in C
unsafe { KeQueryPerformanceCounter(&mut freq as *mut i64 as PLARGE_INTEGER) };
let last = ctx.last_report_time.swap(freq, Ordering::Relaxed);
let delta = (freq - last) / 100;
let scan_time: u16 = if delta >= 0xFF { 0xFF } else { delta as u16 };
// The config paths store the frequency the same way: call it into a local i64
// (return value discarded), then ctx.last_report_time.store(freq, Ordering::Relaxed).
```

B6 is a **Decide** item: either keep the constant 0, or emit a real 100 µs counter. Do not switch silently; see Appendix C.

### 7.6 Uninitialised bytes [BUG, cannot be reproduced exactly]

`ptpOutputReport` is a stack variable that is never zeroed. For `i >= raw_n`, `Contacts[i]` (9 bytes each) contains stack garbage. So do the 6 padding bits of every written contact's flag byte. All 50 bytes are copied to the upper buffer, which leaks kernel stack bytes. A consumer that honours ContactCount ignores them. **Recommendation:** zero-initialise the report in Rust. That is an observable but semantically irrelevant deviation (Appendix C).

### 7.7 Contact count limit

The maximum is 5 (`PTP_MAX_CONTACT_POINTS`). It is also reported in feature 0x07. `MAX_FINGERS = 16` (defined in both `HidDevice.h:37` and `WindowsHID.h:4`) is used only as the Input Mode logical max in the descriptor.

---

## 8. Device support tables

The driver has **no lookup table**; support is decided by hard-coded comparisons:

| Check | Accepted | Location | Result otherwise |
|---|---|---|---|
| VendorID (configure) | 0x05AC `HID_VID_APPLE_USB` or 0x004C `HID_VID_APPLE_BT` (`Device.h:14-15`) | Device.c:308 | `STATUS_NOT_SUPPORTED`, then the 2 s retry loop forever |
| ProductID (configure) | 0x0265 `HID_PID_MAGIC_TRACKPAD_2` (`Device.h:16`) | Device.c:313 | same |
| VendorID (input) | 0x05AC or 0x004C | Input.c:156 | `WdfDeviceSetFailed(NoRestart)` |
| ProductID (descriptors) | 0x0265 | Hid.c:28-36, 113-120 | `STATUS_NOT_SUPPORTED` |
| Transport selection | VID 0x05AC → USB parameters; VID 0x004C → BT parameters | Device.c:322 / 343 | |

INF bindings (PnP IDs):

| Hardware ID | Package INF (AmtPtpDevice.inf) | Standalone INF (AmtPtpHidFilter.inf) |
|---|---|---|
| `HID\{00001124-0000-1000-8000-00805f9b34fb}_VID&0001004c_PID&0265&Col01` | filter (line 90) | filter (line 43) |
| `HID\{00001124-...}_VID&0001004c_PID&0265&Col02` | null (line 91) | null (line 47) |
| `HID\VID_05AC&PID_0265&REV_0855&MI_01&Col01` | not bound | filter (line 42) |
| `HID\VID_05AC&PID_0265&REV_0855&MI_01&Col02` / `&Col03` | not bound | null (lines 45-46) |
| `USB\Vid_05ac&Pid_0265&MI_01` | UMDF driver AmtPtpDeviceUsbUm (line 88) | in-box HID (input.inf HID_Inst), migration (line 36) |

In the BT hardware ID, `{00001124-0000-1000-8000-00805f9b34fb}` is the Bluetooth HID service UUID. `VID&0001004c` means vendor-ID source 0x0001 (Bluetooth SIG) with VID 0x004C (Apple).

README.md:69-70 claims "Magic Trackpad 2 & 3 (Bluetooth connection)", but the code knows no PID other than 0x0265. An MT3 only works if it presents PID 0x0265 (unverified). The Rust port must not add PIDs unless that is intended.

---

## 9. Registry, configuration and user-mode interfaces

* **The driver code reads no registry values.** There are no calls to `WdfDeviceOpenRegistryKey`, `WdfDriverOpenParametersRegistryKey`, `IoOpenDeviceRegistryKey` or `RtlQueryRegistryValues`. `RegistryPath` is used only by `WdfDriverCreate` and `WPP_INIT_TRACING`. WPP/IFR may read its own settings under the service key [FW].
* Registry values written by the **INF**: `HKR,,FriendlyName` and `HKR,,LowerFilters` (MULTI_SZ, append) on the filtered device's hardware key, plus the standard service keys for `AmtPtpHidFilter` and `mshidkmdf` (section 1.3).
* **User-mode configuration interface: none.**
  * The device interface `{FF969022-3111-4441-8F88-875440172C2E}` is registered, but this driver has no handler for create, ioctl, read or write on it (2.4).
  * The user-mode configuration TLC (report 0x09, `REPORTID_UMAPP_CONF`, `WindowsHID.h:16-27`) is defined but not in the descriptor.
  * Feature report 0x09 is not handled: GET and SET both return `STATUS_NOT_SUPPORTED`.
* All tuning constants are compile-time: the axis ranges, the confidence threshold 345, the timeouts and the buffer size.

---

## 10. Error paths, edge cases, magic numbers, TODOs, dead code, suspected bugs

### 10.1 Error-path summary

| Where | Condition | Action |
|---|---|---|
| DriverEntry | `WdfDriverCreate` fails | WPP cleanup; return the error |
| CreateDevice | `WdfDeviceCreate` or `WdfDeviceCreateDeviceInterface` fails | return the error (the device is not created) |
| CreateDevice | WDM DO is NULL | return **SUCCESS** [BUG] |
| CreateDevice | lookaside, timer or work item creation fails | log only; the handle stays NULL; the status is overwritten [BUG] |
| QueueInit | default queue fails | return the error |
| QueueInit | read queue fails | return the error (`HidReadQueue` is NULL) |
| SelfManagedIoInit | detour fails | return the error, so start fails |
| SelfManagedIoInit | attributes IOCTL fails | return the error, so start fails |
| SelfManagedIoInit / Restart | ConfigureMultiTouch fails | return SUCCESS; timer 2 s; `DeviceConfigured` unchanged (Init: stays FALSE) |
| Restart | detour not done | `DeviceConfigured=TRUE`, `LastReportTime` set to the QPC frequency (7.5), return `STATUS_INVALID_STATE_TRANSITION` [BUG] |
| ConfigureMultiTouch | `WdfRequestCreate` fails | return the error (no cleanup is needed) |
| ConfigureMultiTouch | `WdfMemoryCreatePreallocated` fails, format fails, or the IRP is NULL | delete the request; return the error (`STATUS_UNSUCCESSFUL` for the NULL IRP) |
| ConfigureMultiTouch | `WdfRequestSend` returns FALSE | `WdfRequestGetStatus`; delete the request; return it |
| Restart after a PnP stop/start | `PrepareHardware` zeroed the IDs, so configure returns `STATUS_NOT_SUPPORTED` | masked as SUCCESS; 2 s timer forever; the first completed timer-issued read calls `WdfDeviceSetFailed(NoRestart)` [B30] |
| Timer | Restart returns success (including a masked configure failure) | issue one lower read |
| IssueTransportRequest | `WdfRequestCreate` fails | `DeviceConfigured=FALSE`; timer 3 s |
| IssueTransportRequest | lookaside allocation fails | delete the request; `WdfDeviceSetFailed(AttemptRestart)` |
| IssueTransportRequest | format fails | delete both; `WdfDeviceSetFailed(AttemptRestart)` |
| IssueTransportRequest | send fails | `DeviceConfigured=FALSE`; timer 3 s; delete both |
| Completion | VID is not Apple | `WdfDeviceSetFailed(NoRestart)` |
| Completion | len == 0 (including any failed read) | enqueue the work item, which issues a read immediately |
| Completion | len < hdr, or (len-hdr) % 9 != 0 | timer 3 s; no read is issued |
| Completion | no parked upper read | drop the packet; no read is issued |
| Completion | retrieving or copying to the upper buffer fails | `WdfDeviceSetFailed(AttemptRestart)`; **the upper request is leaked, never completed** [BUG] |
| ProcessRequest | forward to the queue fails | complete the upper request with that error |
| GetStrings | send fails | complete with `WdfRequestGetStatus` |
| Get/SetFeature | buffer < 16 / UserBuffer NULL / unknown ID / reportBufferLen too small (GET only) | `STATUS_BUFFER_TOO_SMALL` / `STATUS_INVALID_DEVICE_REQUEST` / `STATUS_NOT_SUPPORTED` / `STATUS_INVALID_BUFFER_SIZE` |
| Descriptor/attribute IOCTLs | retrieving or copying the output fails | that status; Information not set |
| Descriptor IOCTLs | PID != 0x0265 | `STATUS_NOT_SUPPORTED` |
| Any other internal IOCTL | | `STATUS_NOT_SUPPORTED` |

### 10.2 Magic numbers (all of them)

| Value | Meaning | Location |
|---|---|---|
| 1024 | lower read buffer size (`REPORT_BUFFER_SIZE`) | Driver.h:33 |
| `'LTPA'` = 0x4C545041 | pool tag of the lookaside buffers | Driver.h:32 |
| 32 | `HID_XFER_PACKET_SIZE`, the stack buffer for the mode switch | Device.h:12 |
| 0x05AC, 0x004C, 0x0265 | Apple USB VID, Apple BT VID, MT2 PID | Device.h:14-16 |
| 0x8910, 0x0100 | fake VID and version reported upward | HidCommon.h:42-43 |
| 0x02 + `02 01 00 00` (len 4) | USB mode-switch feature | Device.c:335-341 |
| 0xF1 + `F1 02 01` (len 3) | BT mode-switch feature | Device.c:356-361 |
| 12 / 4 | USB / BT header size | HidDevice.h:9-10 |
| 9 | finger record size | HidDevice.h:24 |
| 1 | button byte offset | HidDevice.h:17 |
| -3678 / 3934 / -2479 / 2586 / 250 | axis min/max and snratio | Device.c:328-333, 349-354 |
| 0x1FFF, `<<3 >>3`, `<<6`, `>>19` | 13-bit coordinate extraction | Input.c:202-206 |
| 345 (that is, minor ≤ 172) | confidence threshold. The comment cites the Microsoft "25 mm" guidance and "Or maybe I used the wrong unit? IDK" | Input.c:214-217 |
| 100, 0xFF | scan-time divisor and saturation | Input.c:188-189 |
| 5 | max contacts | WindowsHID.h:6 |
| 0 / 3 | input mode: mouse / PTP | WindowsHID.h:10-11 |
| 0 | button type: click pad | WindowsHID.h:7 |
| 16 | `MAX_FINGERS`, the Input Mode LogMax in the descriptor | WindowsHID.h:4 (also HidDevice.h:37) |
| 2 s, 3 s | retry timer due times | Device.c:232, 266; Input.c:63, 107, 171 |
| 200 µs, 50 µs | dead diagnostics timer due times | Diagnostics.c:42, 51, 94 |
| report IDs 0x05, 0x07, 0x08, 0x04, 0x06, (0x09, 0x02 unused) | | HidCommon.h:4-10 |
| usages 0x57 / 0x58 | button / surface switch | HidCommon.h:12-13 |
| `{FF969022-3111-4441-8F88-875440172C2E}` | device interface | Device.h:7-9 |
| `{eeb34803-f3ec-4bb1-97ce-b42405d7f3c2}` | WPP control GUID | Trace.h:13 |
| "HIDCLASS" (9 bytes with NUL) | client extension ID | Hac.h:33 |
| 0x0F, 0x1B | `IRP_MJ_INTERNAL_DEVICE_CONTROL`, `IRP_MJ_MAXIMUM_FUNCTION` | WDK wdm.h |

### 10.3 TODOs and comments of note

* `AmtPtpHidFilter.inf:11`: `DriverVer= ; TODO: set DriverVer in stampinf property pages`.
* `Detour.c:71`: links to a private Azure DevOps wiki page ("Hijack-HIDCLASS") that is not available.
* `Input.c:59-60`: "This can fail for Bluetooth devices. ... 3 second timer". `Input.c:69, 84`: "tbh if you fail here ... request a restart".
* `Input.c:214-216`: uncertainty about the confidence threshold unit.
* `Diagnostics.c:93`: the trace text mentions `AmtPtpSpiInputRoutineWorker`, a copy-paste from the SPI driver.
* `Device.c:307`: "So far in this prototype, we support Magic Trackpad 2 in USB (05AC:0265) or Bluetooth mode (004c:0265)".

### 10.4 Dead code and unused declarations (can be dropped)

* `Diagnostics.c:6-139` (`PtpFilterDiagnosticsInitializeContinuousRead`, `...InputIssueRequest`, `...RequestCompletionRoutine`): never called. The completion routine also has a use-after-free (lines 132-138). Keep `PtpFilterDiagnosticsIoControlGetString` only if trace names are wanted.
* `QUEUE_CONTEXT.DeviceIoTarget` (always NULL).
* `BCM5974_PARAM.snratio` and `.max`.
* `PtpInputOn`, `PtpReportTouch` and `PtpReportButton` are written but never read. Keep them as write-only state if parity of internal state matters; there is no external effect.
* `HID_USB_SERVICE_NAME` (Hac.h:34).
* `REPORTID_STANDARDMOUSE` (0x02) and `REPORTID_UMAPP_CONF` (0x09; used only by the unused TLC).
* `AAPL_PTP_USERMODE_CONFIGURATION_APP_TLC`.
* `PTP_BUTTON_TYPE_PRESSURE_PAD`, `PTP_CONTACT_CONFIDENCE_BIT` and `PTP_CONTACT_TIPSWITCH_BIT`.
* `HAS_INTEGRATED_BUTTON` and `MAX_FINGER_ORIENTATION`.
* The type 1-4 constants (7.1), `TRACKPAD_FINGER` and `TRACKPAD_BUTTON_DATA`.
* Unused HID item macros: `USAGE_MINIMUM`, `USAGE_MAXIMUM` and `PHYSICAL_MINIMUM`. They are harmless.
* `FilterEvtIoStop` is a no-op. Registering it changes nothing observable, apart from KMDF's bookkeeping during removal.
* The device interface (functionally dead; keep it for parity).
* `Trace(...)` macro variant (never used).
* `MAX_FINGERS` is defined twice with the same value.

### 10.5 Suspected bugs (flag only; summarised with porting guidance in Appendix C)

B1 HQA comma operator · B2 Y off-by-one from the X bits · B3 duplicated confidence operand · B4 uninitialised report bytes · B5 no upper coordinate clamp · B6 ScanTime always 0 (`KeQueryPerformanceCounter` argument is the frequency output) · B7 lower-read status ignored · B8 no report-ID filter; any other length stalls for 3 s · B9 upper request leaked on copy failure · B10 request-context use-after-free · B11 Restart else-branch sets Configured · B12 masked configure failure still issues a read · B13 SET_FEATURE completion status ignored · B14 timer not stopped on D0Exit · B15 lookaside leak (parented to the driver) · B16 object-creation failures swallowed · B17 NULL WDM DO returns success · B18 latent divide-by-zero · B19 queue context target is always NULL · B20 input mode and selective reporting ignored · B21 GET/SET_FEATURE Information never set · B22 attributes `Reserved` not zeroed · B23 SET_FEATURE ignores reportBufferLen · B24 possible hot retry loop on failing reads · B25 GET_STRING with a NULL target (unreachable) · B26 global, never-reverted HIDCLASS patch · B27 synchronous sends without timeout · B28 device interface registered with no handler · B29 `(size_t)(LONG)` sign extension of Information · B30 PnP stop/start zeroes the IDs, leading to an endless configure retry and then `SetFailed(NoRestart)`.

---

## 11. Platform-specific and risky constructs for a Rust port

1. **Undocumented kernel structures and function-pointer patching** (Hac.h, Detour.c).
   * `DRIVER_EXTENSION` beyond the documented fields (`IoClientExtension` @0x28), `IO_CLIENT_EXTENSION`, and HIDCLASS's private `HIDCLASS_DRIVER_EXTENSION` (`MajorFunction` @+0x20 after the 16-byte client header).
   * Rust needs `#[repr(C)]` mirrors with **compile-time offset asserts**: `IoClientExtension` @40, `ClientIdentificationAddress` @8, `MajorFunction` @32 in the HIDCLASS extension, slot 0x0F at +0xA8 from the client extension.
   * Use raw pointer reads and writes: `core::ptr::read_volatile` and `write_volatile`, or `AtomicPtr::store` for the dispatch slot.
   * The function-pointer type is `PDRIVER_DISPATCH = Option<unsafe extern "C" fn(*mut DEVICE_OBJECT, *mut IRP) -> NTSTATUS>` ("system" and "C" are the same ABI on x64/ARM64).
   * The patch is a write into another driver's `DRIVER_OBJECT`. It is global, not reverted, and depends on the Windows build. Keep the same sanity checks in the same order.
2. **`IoGetLowerDeviceObject`** is declared by hand because it lives in ntifs.h (Hac.h:37-41). wdk-sys's ntddk bindings may not include it, so declare an `extern "system"` import from ntoskrnl. Every successful call must be paired with `ObfDereferenceObject`.
3. **`strncmp` on an untrusted pointer** (Detour.c:58). Reimplement it as a compare of at most 9 bytes through a raw pointer that stops at the first mismatch, which is exactly `strncmp`'s behaviour. Like the C code, it can fault if the client ID address is not readable.
4. **Bit-fields** with MSVC LSB-first layout:
   * `PTP_CONTACT` flags (Confidence bit 0, TipSwitch bit 1);
   * `PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` (Button bit 0, Surface bit 1);
   * `TRACKPAD_FINGER_TYPE5.OrientationAndOrigin` (Id = low nibble).

   Rust has no bit-fields, so use explicit masks.
5. **Packed and unaligned data**:
   * `PTP_REPORT` and the feature structs are `pack(1)`;
   * `HID_DESCRIPTOR` is `pack(1)` (9 bytes);
   * `TRACKPAD_FINGER_TYPE5` is `pack(2)`, all bytes;
   * fingers sit at offset `hdr + 9i`, so the `USHORT*` and `unsigned int*` loads at Input.c:202-203 are **unaligned**.

   Use `from_le_bytes` on slices, and serialise output into byte arrays. Avoid references into `#[repr(packed)]` fields.
6. **C integer semantics that Rust must replicate explicitly**:
   * `(SHORT)` narrowing wraps;
   * `>>` on negative values is arithmetic;
   * negating `INT_MIN` is UB in C but wraps in MSVC, so use `wrapping_neg`;
   * `(USHORT)` truncation of a LONGLONG;
   * `size_t` modulo and division;
   * `(size_t)(LONG)ULONG_PTR` sign extension;
   * C division truncates toward zero, and so does Rust's `/` on `i64`.

   Rust debug builds panic on overflow, so use `wrapping_*` or `as` casts exactly as in 7.3. Kernel builds must never panic: guard `% 0` (B18) and slice bounds (`len` ≤ 1024 is guaranteed by the buffer size; still use `get()`).
7. **The comma-operator bug** (Hid.c:245, B1). A literal port of "copy the HQA blob" would *change* behaviour. Decide explicitly whether to preserve or fix it (Appendix C).
8. **Manual IRP editing**: `WdfRequestWdmGetIrp(...)->UserBuffer` is read for GET/SET_FEATURE (Hid.c:200, 287) and **written** for the mode switch (Device.c:403). Rust needs the `IRP` struct layout from wdk-sys (`UserBuffer` field) and must keep the 32-byte packet buffer pinned for the whole synchronous send. Use `#[repr(C, align(8))] struct XferBuf { pkt: HID_XFER_PACKET, payload: [u8; 16] }`. The packet's `reportBuffer` points *into the same buffer* (self-referential), and the C version's `UCHAR[32]` is not guaranteed to be 8-aligned.
9. **WDF macro helpers have no Rust equivalent and must be reimplemented with exact defaults**:
   * `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME` becomes a static `WDF_OBJECT_CONTEXT_TYPE_INFO` with `ContextName`, `ContextSize` and `UniqueType = &self`. The accessor is `WdfObjectGetTypedContextWorker`.
   * `WDF_OBJECT_ATTRIBUTES_INIT` sets `ExecutionLevel` and `SynchronizationScope` to InheritFromParent.
   * `WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE` sets `PowerManaged=WdfUseDefault`, `DefaultQueue=TRUE` and `NumberOfPresentedRequests=-1` for Parallel.
   * `WDF_IO_QUEUE_CONFIG_INIT`.
   * `WDF_TIMER_CONFIG_INIT` sets `Period=0` and `AutomaticSerialization=TRUE`. The Rust port should override it to FALSE (2.4 step 6).
   * `WDF_WORKITEM_CONFIG_INIT` sets `AutomaticSerialization=TRUE`. The Rust port should override it to FALSE (2.4 step 7).
   * `WDF_REQUEST_SEND_OPTIONS_INIT`, `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER` and `WDF_PNPPOWER_EVENT_CALLBACKS_INIT`.
   * `WDF_REL_TIMEOUT_IN_SEC(n)` is `-(n * 10_000_000)` and `WDF_REL_TIMEOUT_IN_US(n)` is `-(n * 10)`.
   * Every `Size` field must match the KMDF 1.15 structure sizes the driver binds to.
10. **Concurrency without locks**. The C code races on `DeviceConfigured` and `LastReportTime` (3.1). The `LastReportTime` race has no effect, because every writer stores the QPC frequency (7.5). Rust's aliasing rules make unsynchronised `&mut` to the context UB. Use atomics or interior mutability with `UnsafeCell` and volatile access. Callbacks run concurrently at ≤ DISPATCH: the queue is parallel, the completion routine runs for several reads, and the timer and work item run at PASSIVE.
11. **IRQL and paging**. The completion routine, dispatch callback and IssueTransportRequest may run at DISPATCH and must live in non-paged code. The C code pages only DriverEntry (INIT), DeviceAdd, the cleanup callback, CreateDevice and QueueInitialize. Rust/LLVM cannot easily place functions in PAGE sections, so keep everything non-paged; that is behaviourally equivalent. `PAGED_CODE()` asserts can become debug IRQL checks.
12. **Use-after-free in the completion path** (B10). Port it with the read moved before the delete.
13. **Pool tag** `'LTPA'`: an MSVC multi-character literal, equal to `u32::from_le_bytes(*b"APTL")` = 0x4C545041.
14. **Static data duplicated per translation unit**: the `static` descriptor arrays in headers. Use one `static` in Rust. Generate the 493 bytes from Appendix A, ideally from a structured builder with a length assert of 493 and a test that compares against the dump.
15. **WPP** cannot be ported. Replace it (1.2).
16. **Synchronous sends without timeouts** (Device.c:212, 405) run at PASSIVE in PnP callbacks and can block a PnP/power IRP indefinitely. Preserve or add a timeout deliberately.
17. **`KeQueryPerformanceCounter`** returns the counter as a `LARGE_INTEGER`, and its only parameter is the `_Out_opt_` **frequency**. All three calls in this driver (Device.c:237, Device.c:276, Input.c:187) pass a non-NULL pointer and **discard the return value**, so they read the frequency, never the counter. A faithful port must do the same (7.5, B6). A port that writes `let now = KeQueryPerformanceCounter(null_mut())` *changes* behaviour.
18. **No `EvtDriverUnload`**. KMDF supplies the unload routine. A Rust driver framework that requires an unload callback must keep it a no-op; the detour is intentionally *not* undone.

---

## Appendix A: report descriptor bytes (authoritative)

`PtpReportDescriptorMagicTrackpad2` (`StaticHidRegistry.h:15-18`) is 493 bytes (0x1ED). It was generated by expanding the macros in `HidCommon.h`, `WindowsHID.h` and `MagicTrackpad2.h`, and checked by a manual byte count: 70 bytes for finger collection type 1, 64 for type 2, 446 for the touchpad TLC and 47 for the configuration TLC.

### A.1 Raw dump

```
0000: 05 0D 09 05 A1 01 85 05 09 22 A1 02 25 01 09 47
0010: 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01
0020: 75 20 27 FF FF FF FF 09 51 81 02 05 01 26 BC 1D
0030: 75 10 55 0E 65 11 09 30 46 40 06 95 01 81 02 46
0040: 7D 04 26 C9 13 09 31 81 02 45 00 55 00 65 00 C0
0050: 05 0D 09 22 A1 02 25 01 09 47 09 42 95 02 75 01
0060: 81 02 75 01 95 06 81 03 95 01 75 20 27 FF FF FF
0070: FF 09 51 81 02 05 01 26 BC 1D 75 10 55 0E 65 11
0080: 09 30 46 40 06 95 01 81 02 46 7D 04 26 C9 13 09
0090: 31 81 02 45 00 55 00 65 00 C0 05 0D 09 22 A1 02
00A0: 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06
00B0: 81 03 95 01 75 20 27 FF FF FF FF 09 51 81 02 05
00C0: 01 26 BC 1D 75 10 55 0E 65 11 09 30 46 40 06 95
00D0: 01 81 02 46 7D 04 26 C9 13 09 31 81 02 C0 05 0D
00E0: 09 22 A1 02 25 01 09 47 09 42 95 02 75 01 81 02
00F0: 75 01 95 06 81 03 95 01 75 20 27 FF FF FF FF 09
0100: 51 81 02 05 01 26 BC 1D 75 10 55 0E 65 11 09 30
0110: 46 40 06 95 01 81 02 46 7D 04 26 C9 13 09 31 81
0120: 02 45 00 55 00 65 00 C0 05 0D 09 22 A1 02 25 01
0130: 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03
0140: 95 01 75 20 27 FF FF FF FF 09 51 81 02 05 01 26
0150: BC 1D 75 10 55 0E 65 11 09 30 46 40 06 95 01 81
0160: 02 46 7D 04 26 C9 13 09 31 81 02 C0 05 0D 55 0C
0170: 66 01 10 47 FF FF 00 00 27 FF FF 00 00 09 56 81
0180: 02 09 54 25 7F 75 08 81 02 05 09 09 01 25 01 75
0190: 01 81 02 95 07 81 03 05 0D 85 07 09 55 09 59 15
01A0: 00 26 FF 00 75 08 95 02 B1 02 06 00 FF 85 08 09
01B0: C5 15 00 26 FF 00 75 08 96 00 01 B1 02 C0 05 0D
01C0: 09 0E A1 01 85 04 09 22 A1 02 09 52 15 00 25 10
01D0: 75 08 95 01 B1 02 C0 A1 00 85 06 09 57 09 58 75
01E0: 01 95 02 25 01 B1 02 95 06 B1 03 C0 C0
```

### A.2 Item-by-item decode (offset, bytes, item)

Offsets 0x0000-0x01BD hold the touchpad TLC (`MagicTrackpad2.h:83-137`); offsets 0x01BE-0x01EC hold the configuration TLC (`WindowsHID.h:29-54`). Physical maximum, unit and unit exponent are global items, so they persist across collection boundaries until they are set again: from finger 3 into finger 4's first three fields only, and from the ScanTime prelude (0x016E-0x0177) to the end of the descriptor, including the configuration TLC (see 5.1).

```
0000  05 0D           Usage Page (0xD)
0002  09 05           Usage (0x5)
0004  A1 01           Collection (Application)
0006  85 05             Report ID (0x5)
0008  09 22             Usage (0x22)
000A  A1 02             Collection (Logical)
000C  25 01               Logical Maximum (1)
000E  09 47               Usage (0x47)
0010  09 42               Usage (0x42)
0012  95 02               Report Count (0x2)
0014  75 01               Report Size (0x1)
0016  81 02               Input (Data, Var, Abs)
0018  75 01               Report Size (0x1)
001A  95 06               Report Count (0x6)
001C  81 03               Input (Const, Var, Abs)
001E  95 01               Report Count (0x1)
0020  75 20               Report Size (0x20)
0022  27 FF FF FF FF      Logical Maximum (-1 = 0xFFFFFFFF unsigned)
0027  09 51               Usage (0x51)
0029  81 02               Input (Data, Var, Abs)
002B  05 01               Usage Page (0x1)
002D  26 BC 1D            Logical Maximum (7612)
0030  75 10               Report Size (0x10)
0032  55 0E               Unit Exponent (-2)
0034  65 11               Unit (0x11)
0036  09 30               Usage (0x30)
0038  46 40 06            Physical Maximum (1600)
003B  95 01               Report Count (0x1)
003D  81 02               Input (Data, Var, Abs)
003F  46 7D 04            Physical Maximum (1149)
0042  26 C9 13            Logical Maximum (5065)
0045  09 31               Usage (0x31)
0047  81 02               Input (Data, Var, Abs)
0049  45 00               Physical Maximum (0)
004B  55 00               Unit Exponent (0)
004D  65 00               Unit (0x0)
004F  C0                End Collection
0050  05 0D             Usage Page (0xD)
0052  09 22             Usage (0x22)
0054  A1 02             Collection (Logical)
0056  25 01               Logical Maximum (1)
0058  09 47               Usage (0x47)
005A  09 42               Usage (0x42)
005C  95 02               Report Count (0x2)
005E  75 01               Report Size (0x1)
0060  81 02               Input (Data, Var, Abs)
0062  75 01               Report Size (0x1)
0064  95 06               Report Count (0x6)
0066  81 03               Input (Const, Var, Abs)
0068  95 01               Report Count (0x1)
006A  75 20               Report Size (0x20)
006C  27 FF FF FF FF      Logical Maximum (-1 = 0xFFFFFFFF unsigned)
0071  09 51               Usage (0x51)
0073  81 02               Input (Data, Var, Abs)
0075  05 01               Usage Page (0x1)
0077  26 BC 1D            Logical Maximum (7612)
007A  75 10               Report Size (0x10)
007C  55 0E               Unit Exponent (-2)
007E  65 11               Unit (0x11)
0080  09 30               Usage (0x30)
0082  46 40 06            Physical Maximum (1600)
0085  95 01               Report Count (0x1)
0087  81 02               Input (Data, Var, Abs)
0089  46 7D 04            Physical Maximum (1149)
008C  26 C9 13            Logical Maximum (5065)
008F  09 31               Usage (0x31)
0091  81 02               Input (Data, Var, Abs)
0093  45 00               Physical Maximum (0)
0095  55 00               Unit Exponent (0)
0097  65 00               Unit (0x0)
0099  C0                End Collection
009A  05 0D             Usage Page (0xD)
009C  09 22             Usage (0x22)
009E  A1 02             Collection (Logical)
00A0  25 01               Logical Maximum (1)
00A2  09 47               Usage (0x47)
00A4  09 42               Usage (0x42)
00A6  95 02               Report Count (0x2)
00A8  75 01               Report Size (0x1)
00AA  81 02               Input (Data, Var, Abs)
00AC  75 01               Report Size (0x1)
00AE  95 06               Report Count (0x6)
00B0  81 03               Input (Const, Var, Abs)
00B2  95 01               Report Count (0x1)
00B4  75 20               Report Size (0x20)
00B6  27 FF FF FF FF      Logical Maximum (-1 = 0xFFFFFFFF unsigned)
00BB  09 51               Usage (0x51)
00BD  81 02               Input (Data, Var, Abs)
00BF  05 01               Usage Page (0x1)
00C1  26 BC 1D            Logical Maximum (7612)
00C4  75 10               Report Size (0x10)
00C6  55 0E               Unit Exponent (-2)
00C8  65 11               Unit (0x11)
00CA  09 30               Usage (0x30)
00CC  46 40 06            Physical Maximum (1600)
00CF  95 01               Report Count (0x1)
00D1  81 02               Input (Data, Var, Abs)
00D3  46 7D 04            Physical Maximum (1149)
00D6  26 C9 13            Logical Maximum (5065)
00D9  09 31               Usage (0x31)
00DB  81 02               Input (Data, Var, Abs)
00DD  C0                End Collection
00DE  05 0D             Usage Page (0xD)
00E0  09 22             Usage (0x22)
00E2  A1 02             Collection (Logical)
00E4  25 01               Logical Maximum (1)
00E6  09 47               Usage (0x47)
00E8  09 42               Usage (0x42)
00EA  95 02               Report Count (0x2)
00EC  75 01               Report Size (0x1)
00EE  81 02               Input (Data, Var, Abs)
00F0  75 01               Report Size (0x1)
00F2  95 06               Report Count (0x6)
00F4  81 03               Input (Const, Var, Abs)
00F6  95 01               Report Count (0x1)
00F8  75 20               Report Size (0x20)
00FA  27 FF FF FF FF      Logical Maximum (-1 = 0xFFFFFFFF unsigned)
00FF  09 51               Usage (0x51)
0101  81 02               Input (Data, Var, Abs)
0103  05 01               Usage Page (0x1)
0105  26 BC 1D            Logical Maximum (7612)
0108  75 10               Report Size (0x10)
010A  55 0E               Unit Exponent (-2)
010C  65 11               Unit (0x11)
010E  09 30               Usage (0x30)
0110  46 40 06            Physical Maximum (1600)
0113  95 01               Report Count (0x1)
0115  81 02               Input (Data, Var, Abs)
0117  46 7D 04            Physical Maximum (1149)
011A  26 C9 13            Logical Maximum (5065)
011D  09 31               Usage (0x31)
011F  81 02               Input (Data, Var, Abs)
0121  45 00               Physical Maximum (0)
0123  55 00               Unit Exponent (0)
0125  65 00               Unit (0x0)
0127  C0                End Collection
0128  05 0D             Usage Page (0xD)
012A  09 22             Usage (0x22)
012C  A1 02             Collection (Logical)
012E  25 01               Logical Maximum (1)
0130  09 47               Usage (0x47)
0132  09 42               Usage (0x42)
0134  95 02               Report Count (0x2)
0136  75 01               Report Size (0x1)
0138  81 02               Input (Data, Var, Abs)
013A  75 01               Report Size (0x1)
013C  95 06               Report Count (0x6)
013E  81 03               Input (Const, Var, Abs)
0140  95 01               Report Count (0x1)
0142  75 20               Report Size (0x20)
0144  27 FF FF FF FF      Logical Maximum (-1 = 0xFFFFFFFF unsigned)
0149  09 51               Usage (0x51)
014B  81 02               Input (Data, Var, Abs)
014D  05 01               Usage Page (0x1)
014F  26 BC 1D            Logical Maximum (7612)
0152  75 10               Report Size (0x10)
0154  55 0E               Unit Exponent (-2)
0156  65 11               Unit (0x11)
0158  09 30               Usage (0x30)
015A  46 40 06            Physical Maximum (1600)
015D  95 01               Report Count (0x1)
015F  81 02               Input (Data, Var, Abs)
0161  46 7D 04            Physical Maximum (1149)
0164  26 C9 13            Logical Maximum (5065)
0167  09 31               Usage (0x31)
0169  81 02               Input (Data, Var, Abs)
016B  C0                End Collection
016C  05 0D             Usage Page (0xD)
016E  55 0C             Unit Exponent (-4)
0170  66 01 10          Unit (0x1001)
0173  47 FF FF 00 00    Physical Maximum (65535)
0178  27 FF FF 00 00    Logical Maximum (65535)
017D  09 56             Usage (0x56)
017F  81 02             Input (Data, Var, Abs)
0181  09 54             Usage (0x54)
0183  25 7F             Logical Maximum (127)
0185  75 08             Report Size (0x8)
0187  81 02             Input (Data, Var, Abs)
0189  05 09             Usage Page (0x9)
018B  09 01             Usage (0x1)
018D  25 01             Logical Maximum (1)
018F  75 01             Report Size (0x1)
0191  81 02             Input (Data, Var, Abs)
0193  95 07             Report Count (0x7)
0195  81 03             Input (Const, Var, Abs)
0197  05 0D             Usage Page (0xD)
0199  85 07             Report ID (0x7)
019B  09 55             Usage (0x55)
019D  09 59             Usage (0x59)
019F  15 00             Logical Minimum (0)
01A1  26 FF 00          Logical Maximum (255)
01A4  75 08             Report Size (0x8)
01A6  95 02             Report Count (0x2)
01A8  B1 02             Feature (Data, Var, Abs)
01AA  06 00 FF          Usage Page (0xFF00)
01AD  85 08             Report ID (0x8)
01AF  09 C5             Usage (0xC5)
01B1  15 00             Logical Minimum (0)
01B3  26 FF 00          Logical Maximum (255)
01B6  75 08             Report Size (0x8)
01B8  96 00 01          Report Count (0x100)
01BB  B1 02             Feature (Data, Var, Abs)
01BD  C0              End Collection
01BE  05 0D           Usage Page (0xD)
01C0  09 0E           Usage (0xE)
01C2  A1 01           Collection (Application)
01C4  85 04             Report ID (0x4)
01C6  09 22             Usage (0x22)
01C8  A1 02             Collection (Logical)
01CA  09 52               Usage (0x52)
01CC  15 00               Logical Minimum (0)
01CE  25 10               Logical Maximum (16)
01D0  75 08               Report Size (0x8)
01D2  95 01               Report Count (0x1)
01D4  B1 02               Feature (Data, Var, Abs)
01D6  C0                End Collection
01D7  A1 00             Collection (Physical)
01D9  85 06               Report ID (0x6)
01DB  09 57               Usage (0x57)
01DD  09 58               Usage (0x58)
01DF  75 01               Report Size (0x1)
01E1  95 02               Report Count (0x2)
01E3  25 01               Logical Maximum (1)
01E5  B1 02               Feature (Data, Var, Abs)
01E7  95 06               Report Count (0x6)
01E9  B1 03               Feature (Const, Var, Abs)
01EB  C0                End Collection
01EC  C0              End Collection
```

### A.3 Unused user-mode configuration TLC (`AAPL_PTP_USERMODE_CONFIGURATION_APP_TLC`, `WindowsHID.h:16-27`), 23 bytes, NOT part of the descriptor [DEAD]

```
0000  06 00 FF        Usage Page (0xFF00)
0003  09 01           Usage (0x1)
0005  A1 01           Collection (Application)
0007  85 09             Report ID (0x9)
0009  09 01             Usage (0x1)
000B  15 00             Logical Minimum (0)
000D  26 FF 00          Logical Maximum (255)
0010  75 08             Report Size (0x8)
0012  95 03             Report Count (0x3)
0014  B1 02             Feature (Data, Var, Abs)
0016  C0              End Collection
```

### A.4 HID descriptor (`PtpDefaultHidDescriptorMagicTrackpad2`, `StaticHidRegistry.h:20-30`), 9 bytes

```
09 21 00 01 00 01 22 ED 01
```

---

## Appendix B: PTP HQA certification blob (`DEFAULT_PTP_HQA_BLOB`, `WindowsHID.h:56-88`), 256 bytes

Because of bug B1 (`Hid.c:245`), the current driver writes **only the first byte (0xFC)** of this blob into the GET_FEATURE 0x08 response. The full blob is listed for a port that chooses to fix B1. It is the widely circulated sample PTP HQA blob.

```
FC 28 FE 84 40 CB 9A 87 0D BE 57 3C B6 70 09 88
07 97 2D 2B E3 38 34 B6 6C ED B0 F7 E5 9C F6 C2
2E 84 1B E8 B4 51 78 43 1F 28 4B 7C 2D 53 AF FC
47 70 1B 59 6F 74 43 C4 F3 47 18 53 1A A2 A1 71
C7 95 0E 31 55 21 D3 B5 1E E9 0C BA EC B8 89 19
3E B3 AF 75 81 9D 53 B9 41 57 F4 6D 39 25 29 7C
87 D9 B4 98 45 7D A7 26 9C 65 3B 85 68 89 D7 3B
BD FF 14 67 F2 2B F0 2A 41 54 F0 FD 2C 66 7C F8
C0 8F 33 13 03 F1 D3 C1 0B 89 D9 1B 62 CD 51 B7
80 B8 AF 3A 10 C1 8A 5B E8 8A 56 F0 8C AA FA 35
E9 42 C4 D8 55 C3 38 CC 2B 53 5C 69 52 D5 C8 73
02 38 7C 73 B6 41 E7 FF 05 D8 2B 79 9A E2 34 60
8F A3 32 1F 09 78 62 BC 80 E3 0F BD 65 20 08 13
C1 E2 EE 53 2D 86 7E A7 5A C5 D3 7D 98 BE 31 48
1F FB DA AF A2 A8 6A 89 D6 BF F2 D3 32 2A 9A E4
CF 17 B7 B8 F4 E1 33 08 24 8B C4 43 A5 E5 24 C2
```

---

## Appendix C: bug and quirk table, with porting recommendations

Preservation classes:

* **Exact**: reproduce bit-for-bit, because it is visible to Windows or the user.
* **Observable**: keep the externally visible effect; the internals may differ.
* **Fix**: no observable change, or the C behaviour is UB or unreproducible.
* **Decide**: a product decision is needed; the default is the "Recommended" column.

| # | Item | Location | Class | Recommended Rust behaviour |
|---|---|---|---|---|
| B1 | HQA blob: comma operator writes only `blob[0]=0xFC`; the rest is left untouched | Hid.c:245 | Decide | Preserve (write `buf[0]=0x08`, `buf[1]=0xFC`, nothing else). Optionally fix by copying all 256 bytes, behind a flag. |
| B2 | Y off-by-one when the X bits are nonzero; `INT_MIN` wrap | Input.c:206 | Exact | `((tmp_y << 6) as i32).wrapping_neg() >> 19` |
| B3 | Confidence tests TouchMinor twice | Input.c:217 | Exact | `conf = f[5] <= 172` |
| B4 | Uninitialised report bytes (contacts ≥ n, padding bits) | Input.c:132 | Fix | Zero-initialise all 50 bytes |
| B5 | No upper clamp (x up to 7773, y up to 6574) | Input.c:207-208 | Exact | Lower clamp only |
| B6 | `KeQueryPerformanceCounter(&x)` writes the **frequency** into `x` and the counter return value is discarded (Device.c:237, 276; Input.c:187). `ScanTime = (freq - LastReportTime)/100`, saturating at 0xFF, so it is **always 0**, except 0xFF for a report emitted while `LastReportTime` has never been stamped (7.5) | Device.c:237, 276; Input.c:187-190 | Decide | **Exact** (the default, and what Windows sees today): mirror the call pattern literally (pass `&mut`, discard the return), which yields 0, or 0xFF on the never-stamped first report. **Fix** (only as a deliberate product change): emit a real counter delta in 100 µs units (`counter * 10_000 / freq`), free-running and wrapping at 0xFFFF as PTP specifies. Do not switch silently. |
| B7 | Lower-read completion status ignored; only Information is used | Input.c:150 | Exact | Ignore the status the same way |
| B8 | No report-ID filter; any length other than hdr+9n means 3 s timer and the chain stops | Input.c:169-173 | Exact | Same |
| B9 | Upper request leaked when output retrieve or copy fails, plus `SetFailed(AttemptRestart)` | Input.c:225-239 | Observable | Preserve `SetFailed`. Completing the request with the error status is an acceptable deviation, because the device is restarting anyway; note it if done. |
| B10 | `RequestMemory` read after the request (its context owner) was deleted | Input.c:246-248 | Fix | Read the handle first, then delete the request, then the memory |
| B11 | Restart else-branch sets `DeviceConfigured=TRUE` and returns `STATUS_INVALID_STATE_TRANSITION` | Device.c:270-279 | Exact | Same |
| B12 | Configure failure returns SUCCESS, so the timer still issues a read and re-arms 2 s | Device.c:262-267, 437-442 | Exact | Same |
| B13 | Synchronous SET_FEATURE: TRUE from `WdfRequestSend` is treated as success; the completion status is unchecked | Device.c:406-413 | Exact | Same: success iff `WdfRequestSend` returns TRUE |
| B14 | Timer and work item not stopped or flushed on D0Exit | Device.c:149-182 | Exact | Same; do not add `WdfTimerStop` |
| B15 | Lookaside parented to the WDFDRIVER, so it leaks per device | Device.c:59-62 | Fix | Parent it to the device |
| B16 | Failures creating the lookaside, timer or work item are logged only; NULL handles later bugcheck | Device.c:63-85 | Decide | **First** set `AutomaticSerialization = FALSE` on the timer and the work item (2.4 steps 6-7), so creation cannot fail with `STATUS_WDF_INCOMPATIBLE_EXECUTION_LEVEL`. Only then return the error from DeviceAdd on creation failure; after the change this deviates only on allocation failure. Without that change, fail-fast could stop the Rust driver loading wherever the C driver's timer creation silently fails today (unverified, see 2.4 step 6). |
| B17 | NULL WDM DO returns `STATUS_SUCCESS` | Device.c:47-50 | Fix | Unreachable; treat as an error or assert |
| B18 | `% InputFingerSize` with size 0 | Input.c:169 | Fix | Guard: if fsize == 0, treat as malformed (3 s timer). Document as a deviation. |
| B19 | `QUEUE_CONTEXT.DeviceIoTarget` always NULL | Queue.c:41 | Fix | Drop the field |
| B20 | Input mode and selective reporting stored but never applied | Hid.c:296-333 | Exact | Store but ignore |
| B21 | GET/SET_FEATURE never set Information | Hid.c:176-345 | Exact | Do not set Information |
| B22 | `HID_DEVICE_ATTRIBUTES.Reserved` not written | Hid.c:78-83 | Exact | Write only Size, VID, PID and Version |
| B23 | SET_FEATURE reads `reportBuffer[1]` without checking `reportBufferLen` | Hid.c:300, 325 | Exact | Same; the caller is HIDCLASS |
| B24 | A failing lower read with Information 0 is re-issued immediately via the work item (possible hot loop) | Input.c:163-166 | Exact | Same |
| B25 | GET_STRING forwarded to a possibly NULL target | Hid.c:163 | Fix | Unreachable; if NULL, complete with `STATUS_INVALID_DEVICE_STATE` or similar, and document it |
| B26 | HIDCLASS `MajorFunction[0x0F]` patch is global and never reverted | Detour.c:72 | Exact | Required for the driver to work |
| B27 | Synchronous lower IOCTLs have no timeout | Device.c:212, 405 | Exact | Same; adding a timeout would be a deliberate change |
| B28 | Device interface registered with no handler | Device.c:52 | Exact | Keep the registration |
| B29 | `responseLength = (size_t)(LONG)Information` | Input.c:150 | Exact | `info as u32 as i32 as isize as usize` |
| B30 | After a PnP stop/start, `PrepareHardware` zeroes VID, PID and version, and `SelfManagedIoInit` (the only code that reads them) is not re-run [FW]. `SelfManagedIoRestart` then gets `STATUS_NOT_SUPPORTED` from configure, masks it, and re-arms the 2 s timer forever. Each firing issues a read (B12), and the first read to complete calls `WdfDeviceSetFailed(NoRestart)` at the VID check. The descriptor IOCTLs return `STATUS_NOT_SUPPORTED` meanwhile. | Device.c:123-125, 260-267; Input.c:156-160 | Decide | Exact parity keeps it. Recommended fix, documented as a deviation: do not zero the IDs in `PrepareHardware` (or re-send `IOCTL_HID_GET_DEVICE_ATTRIBUTES` in Restart when VID == 0). The path is rare, because a HID collection PDO has no resources to rebalance. |

---

## Appendix D: golden test vectors

The vectors were computed with a Python emulator of the C arithmetic: MSVC int16 narrowing, arithmetic shifts, int32 wrap. They are for unit tests of the pure parsing layer.

### D.1 Single finger (`parse_finger`, 9 raw bytes; X.min = -3678, Y.min = -2479)

"X13" and "Y13" are the signed 13-bit fields encoded into bytes 0-3 (u32 LE: X13 in bits 0-12, Y13 in bits 13-25).

| Case | Raw bytes (hex) | x | y | ContactID | Tip | Conf |
|---|---|---|---|---|---|---|
| X13=0, Y13=0 | `00 00 00 00 0a 0a 00 00 01` | 3678 | 2479 | 1 | 1 | 1 |
| X13=100, Y13=200 (X bits ≠ 0, so y is off by one) | `64 00 19 00 0a 0a 00 00 02` | 3778 | **2278** | 2 | 1 | 1 |
| X13=0, Y13=200 (X bits = 0, exact) | `00 00 19 00 0a 0a 00 00 03` | 3678 | **2279** | 3 | 1 | 1 |
| X13=-3700 (x clamps to 0), Y13=-3000 | `8c 11 89 02 0a 0a 00 00 04` | 0 | 5478 (> 5065, no upper clamp) | 4 | 1 | 1 |
| X13=4095, Y13=-4096 | `ff 0f 00 02 0a 0a 00 00 05` | 7773 (> 7612) | 6574 | 5 | 1 | 1 |
| X13=0, Y13=-4096 (`INT_MIN` negation wraps) | `00 00 00 02 0a 0a 00 00 06` | 3678 | **0** (a non-wrapping port gives 6575) | 6 | 1 | 1 |
| TouchMinor=172 | `0a 40 01 00 01 ac 00 00 07` | 3688 | 2468 | 7 | 1 | **1** |
| TouchMinor=173 | `0a 40 01 00 01 ad 00 00 08` | 3688 | 2468 | 8 | 1 | **0** |
| TouchMajor=0 | `0a 40 01 00 00 00 00 00 09` | 3688 | 2468 | 9 | **0** | 1 |
| byte 8 = 0xAF (orientation 0xA, ID 0xF) | `0a 40 01 00 05 05 00 00 af` | 3688 | 2468 | **15** | 1 | 1 |

Bits 26-31 of the u32 (the top 6 bits of byte 3), bytes 6-7 and the high nibble of byte 8 must not affect the result.

### D.2 Whole BT packet (header 4), 6 fingers truncated to 5, button set

Raw, 58 bytes, so (58-4)/9 = 6 fingers. Finger i has X13 = 100i, Y13 = 50i, major = minor = 0x14 and ID = i:

```
31 01 00 00
00 00 00 00 14 14 00 00 00
64 40 06 00 14 14 00 00 01
c8 80 0c 00 14 14 00 00 02
2c c1 12 00 14 14 00 00 03
90 01 19 00 14 14 00 00 04
f4 41 1f 00 14 14 00 00 05
```

Expected `PTP_REPORT` (50 bytes). ScanTime (bytes 46-47) is `00 00` in steady state, i.e. whenever `LastReportTime` already holds the QPC frequency (7.5). It is `ff 00` only if `LastReportTime` was still 0.

```
05
03 00 00 00 00 5e 0e af 09
03 01 00 00 00 c2 0e 7c 09
03 02 00 00 00 26 0f 4a 09
03 03 00 00 00 8a 0f 18 09
03 04 00 00 00 ee 0f e6 08
00 00 05 01
```

The sixth finger (ID 5) is dropped. `buf[1] = 0x01`, so the button byte is 1.

### D.3 Length classification (BT header 4, finger 9; USB header 12)

| Transport | Information | Outcome |
|---|---|---|
| any | 0 | work item enqueued (immediate re-read); no upper completion |
| BT | 3 | malformed: 3 s timer; no re-read |
| BT | 4 | valid, ContactCount 0, button from `buf[1]` bit 0 |
| BT | 12 | malformed ((12-4) % 9 = 8) |
| BT | 13 | 1 contact |
| BT | 58 | 6 fingers, 5 reported |
| USB | 12 | valid, 0 contacts |
| USB | 21 | 1 contact |
| USB | 11 | malformed (< 12) |

### D.4 Scan time

**Driver-level behaviour** (corrected; see 7.5). The value fed in as "now" is the QPC *frequency*, not a counter:

| `LastReportTime` before the report | QPC frequency | ScanTime emitted | `LastReportTime` after |
|---|---|---|---|
| = frequency (any report after a successful configure, or after the first report) | any | **0** | frequency |
| 0 (never stamped) | 10,000,000 | **0xFF** | frequency |
| 0 (never stamped) | 25,500 | 0xFF (boundary) | frequency |
| 0 (never stamped) | 25,499 | 254 (theoretical; no real QPC is this slow) | frequency |

**Formula-level helper only.** `scan_time(delta) = delta/100 saturating at 0xFF`, with `(USHORT)` truncation for negatives. These rows exercise the arithmetic in isolation. In the driver, `delta` is always `freq - freq = 0`, or `freq - 0`:

| `delta_ticks` | ScanTime |
|---|---|
| 0 | 0 |
| 99 | 0 |
| 2550 | 25 |
| 25499 | 254 |
| 25500 | 255 |
| 10,000,000 | 255 |
| -50 | 0 (truncation toward zero) |
| -100 | 0xFFFF (`(USHORT)(-1)`) |

### D.5 Feature requests

NTSTATUS values used below: `STATUS_NOT_SUPPORTED` 0xC00000BB, `STATUS_BUFFER_TOO_SMALL` 0xC0000023, `STATUS_INVALID_DEVICE_REQUEST` 0xC0000010, `STATUS_INVALID_BUFFER_SIZE` 0xC0000206, `STATUS_CANCELLED` 0xC0000120, `STATUS_INVALID_STATE_TRANSITION` 0xC000A003.

| Request | Input | Result |
|---|---|---|
| GET_FEATURE | OutputBufferLength 8 (< 16) | `STATUS_BUFFER_TOO_SMALL` |
| GET_FEATURE | `Irp->UserBuffer == NULL` | `STATUS_INVALID_DEVICE_REQUEST` |
| GET_FEATURE | id 0x07, buffer of length 3 | buffer becomes `07 05 00`; SUCCESS; Information unchanged |
| GET_FEATURE | id 0x07, length 2 | `STATUS_INVALID_BUFFER_SIZE`; buffer untouched |
| GET_FEATURE | id 0x08, length 257, prefilled with `AA` | buffer becomes `08 FC AA AA ... AA`; SUCCESS |
| GET_FEATURE | id 0x08, length 256 | `STATUS_INVALID_BUFFER_SIZE` |
| GET_FEATURE | id 0x04 / 0x06 / 0x09 | `STATUS_NOT_SUPPORTED` |
| SET_FEATURE | id 0x04, `04 03` | `PtpInputOn = TRUE`; SUCCESS |
| SET_FEATURE | id 0x04, `04 00` | `PtpInputOn = FALSE`; SUCCESS |
| SET_FEATURE | id 0x04, `04 05` | no change; SUCCESS |
| SET_FEATURE | id 0x06, `06 03` | Button = 1, Touch = 1; SUCCESS |
| SET_FEATURE | id 0x06, `06 02` | Button = 0, Touch = 1; SUCCESS |
| SET_FEATURE | id 0x07 / 0x08 | `STATUS_NOT_SUPPORTED` |

### D.6 Other synchronous IOCTL outputs

| IOCTL | Output |
|---|---|
| `GET_DEVICE_DESCRIPTOR` (PID 0x0265) | `09 21 00 01 00 01 22 ED 01`; Information 9 |
| `GET_REPORT_DESCRIPTOR` (PID 0x0265) | the 493 bytes of Appendix A.1; Information 493 |
| `GET_DEVICE_ATTRIBUTES` (PID 0x0265) | `20 00 00 00 10 89 65 02 00 01` followed by `Reserved[22 bytes]` left untouched; Information 32 |
| `GET_DEVICE_DESCRIPTOR` / `GET_REPORT_DESCRIPTOR` (PID ≠ 0x0265) | `STATUS_NOT_SUPPORTED` |

The mode-switch packets sent to the device, as `HID_XFER_PACKET` with the payload at +16:

| Transport | reportId | reportBufferLen | Payload |
|---|---|---|---|
| USB | 0x02 | 4 | `02 01 00 00` |
| BT | 0xF1 | 3 | `F1 02 01` |

---

## Appendix E: suggested Rust module split

This is non-normative.

* `descriptor.rs`: `REPORT_DESC: [u8; 493]` and `HID_DESC: [u8; 9]`, with tests against Appendix A.
* `parse.rs`: a pure `no_std` module with `parse_finger`, `build_report(raw, hdr, now, &mut last) -> [u8; 50]` and `classify_len`. For C parity, `now` is the value written through `KeQueryPerformanceCounter`'s frequency pointer, i.e. the QPC frequency, not the counter (7.5). Unit-tested on the host with Appendix D.
* `detour.rs`: `#[repr(C)]` mirrors of `Hac.h`, with `const _: () = assert!(offset_of!(...) == ...)` checks, and the patch routine.
* `device.rs`, `queue.rs`, `input.rs`, `hid.rs`: the WDF glue, mirroring the C files one-to-one so the `file:line` citations in this spec stay useful.

---

## Verification addendum

An adversarial verification pass re-read every file under `src/AmtPtpHidFilter`, both INFs and the WDK 10.0.28000 headers (`km/wdm.h`, `km/hidport.h`, `shared/hidclass.h`). The pass re-expanded the descriptor and HQA macros with a script (all 493 bytes and all 256 bytes match Appendix A and B), recomputed every IOCTL code, and re-derived the struct offsets and the D.1 and D.2 finger vectors (all correct). The following were changed inline:

1. **ScanTime / `KeQueryPerformanceCounter` (major).** The pointer argument is `_Out_opt_ PerformanceFrequency`, and the counter is the discarded return value. `LastReportTime` and `currentTSC` therefore hold the QPC frequency, and **ScanTime is always 0**. The only exception is 0xFF for a report emitted before any stamp. The previous text ("raw QPC delta, 10 µs units", "frequency passed as NULL", the parallel-completion race) was wrong. Corrected in: the intro sentence, 0.2 step 5, 2.6 (Init step 6 and Restart), 3.1 (the `LastReportTime` row and the race note), 7.3 step 2, 7.3 closing note, 7.5 (rewritten, with a Rust mirror), 10.1, 10.5, 11.10, 11.17, B6 (now **Decide**), D.2 (`ss ss` becomes `00 00`), D.4 (rewritten) and Appendix E.
2. **Descriptor global-item carry-over (5.1, A.2).** The cm unit and PhysMax 1149 from finger 3 reach only **finger 4's** Confidence, Tip and ContactID, not finger 5's, and nothing reaches ScanTime. Also new: the ScanTime prelude's Unit 0x1001, exponent -4 and PhysMax 65535 persist through ContactCount, Button, features 0x07 and 0x08, and the whole configuration TLC (0x04, 0x06).
3. **New bug B30 (2.6, 4.3, 6.8, 10.1, 10.5, Appendix C).** After a PnP stop/start, `PrepareHardware` zeroes the IDs and `SelfManagedIoInit` is not re-run [FW]. Restart's configure then fails with `NOT_SUPPORTED`, and the masked 2 s retry runs forever. Each timer firing issues a read, and the first read to complete calls `WdfDeviceSetFailed(NoRestart)` at the VID check. The descriptor IOCTLs return `NOT_SUPPORTED` meanwhile.
4. **6.4 missing error paths:** a failure in `WdfMemoryCreatePreallocated` or in format goes to `cleanup` (delete the request, return the error). The unreachable final `else` (Device.c:363-368) is marked [DEAD]. The 10.1 rows for ConfigureMultiTouch were added.
5. **6.4 Linux claim softened.** The USB packet is 4 bytes (`02 01 00 00`) and cannot be verified against Linux from this repo; port the C bytes exactly.
6. **6.2 / 11.3 `strncmp`.** It reads *up to* 9 bytes and stops at the first mismatch, so a port should mirror that to touch the same memory.
7. **10.5 B-list** now includes B28-B30, which match Appendix C.
8. **6.5 pipeline invariants.** A zero-length completion does not always restore a read. `WdfWorkItemEnqueue` and `WdfTimerStart` on objects that are already queued coalesce, so the number of lower reads in flight can shrink permanently, until new upper reads replace them.
9. **Minor:** HEAD is `2edb0f2` (only `.gitattributes` changed since `29d2778`). In the standalone INF, `DriverVer` is on line 11, not 10.

Checked and found correct (no change): `DEVICE_CONTEXT` offsets and size 152; the `Hac.h` offsets (0x28, +16, +32, slot +0xA8, size 304); the `HID_XFER_PACKET` layout; every IOCTL value; the Y-axis arithmetic including the `INT_MIN` wrap; the confidence and tip thresholds; the HQA comma-operator effect; GET/SET_FEATURE status codes and the fact that they never set Information; the INF bindings and line numbers; the vcxproj facts; the commit references (`8e0cc7c`, `dd364d6`, `cf47d19`; `29d2778` touched only README, so no MT3 PID exists in code). B13's hedge about KMDF synchronous-send semantics stays unverified on purpose.

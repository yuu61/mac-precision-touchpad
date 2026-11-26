<!-- Porting analysis of the C driver as of commit 2edb0f2, written before the Rust rewrite. File:line references point into that commit's `src/` tree, which has since been removed. -->

# AmtPtpDeviceUsbUm: porting spec for the Rust rewrite

Source: `src/AmtPtpDeviceUsbUm` (git `main` @ 29d2778; current HEAD 2edb0f2 changes only `.gitattributes`, so the sources are identical).
Scope: the UMDF 2 USB driver for older MacBook USB trackpads ("Wellspring" / bcm5974 family) and the Magic Trackpad 2 over USB.

Every file under the directory was read in full:

- C sources: `Driver.c`, `Device.c`, `Hid.c`, `InputInterrupt.c`, `Queue.c`
- Headers in `include/`: `AppleDefinition.h`, `Device.h`, `Driver.h`, `Hid.h`, `HidCommon.h`, `ModernTrace.h`, `Queue.h`, `resource.h`, `StaticHidRegistry.h`, `Trace.h`, `DeviceFamily/*.h`
- Build and packaging: `MagicTrackpad2PtpDevice.vcxproj`, `.filters`, `Resource.rc` (UTF-16LE), `AmtPtpDevice.wprp`
- Also read: `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf` and its `.vcxproj`

**Citations.** `File.c:NN` means a path relative to `src/AmtPtpDeviceUsbUm/`, and `include/` is omitted for headers. INF citations are `INF:NN`, meaning `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf`.

**Verified numbers.** Values marked "(computed)" were produced by compiling the project's own headers with gcc and stub types, or by a script over the expanded bytes. They are not hand-counted. The scratch harness was not kept in the repository (calc.c: descriptors and struct sizes; gold.c: per-finger vectors; frame.c: TYPE4 frame; ctx.c: context layout; parse.py, sha.py).

**Bug IDs.** Bugs are numbered **B1..Bn** and collected in section 10. Each bug states:

- the current behavior, which is what an identical port must do
- the probable intent
- "Port decision: engineer's call" unless noted

## 0. Architecture in one paragraph

The driver is a UMDF 2 DLL loaded by WUDFHost. The device stack is assembled by INF:178-217:

- The function driver (FDO) is `mshidumdf.sys` plus HidClass. INF:185 marks it as the service with `0x2` (SPSVCINST_ASSOCSERVICE).
- `WUDFRd.sys` is a **lower filter** (INF:186, INF:204). It reflects I/O into this DLL.
- The DLL calls `WdfFdoInitSetFilter` (Driver.c:128). This is the standard pattern for UMDF HID minidrivers.

HidClass sends HID minidriver IOCTLs. mshidumdf passes them down, and the DLL receives them as ordinary **device-control** requests through `EvtIoDeviceControl` (Queue.c:29). It answers with:

- static HID descriptors
- reads from a USB interrupt IN pipe through a WDF continuous reader
- translation of Apple's raw multitouch packets into Windows Precision Touchpad (PTP) input reports (report ID 0x05)

Enabling raw multitouch ("Wellspring mode") requires a HID class GET_REPORT/SET_REPORT pair on the control pipe.

---

## 1. Build facts

### 1.1 Project / binary
| Item | Value | Cite |
|---|---|---|
| Project file | `MagicTrackpad2PtpDevice.vcxproj` (RootNamespace `MagicTrackpad2PtpDevice`) | vcxproj:61 |
| ProjectName → TargetName | `AmtPtpDeviceUsbUm` → output **`AmtPtpDeviceUsbUm.dll`** | vcxproj:62; INF:23, INF:108, INF:200 |
| ProjectGuid | `{87EFA31B-25EB-4944-A30A-300171BFFF57}` | vcxproj:55 |
| Driver model | **UMDF**: `PlatformToolset=WindowsUserModeDriver10.0`, `UMDF_VERSION_MAJOR=2`, `ConfigurationType=DynamicLibrary` | vcxproj:72-119 |
| UMDF minor | Not set in the vcxproj. The WDK default for `TargetVersion=Windows10` is **15** (`WindowsDriver.UserMode.Default.props:22`, WDK 10.0.28000). That gives UMDF **2.15**, consistent with INF `UmdfLibraryVersion=2.15.0` (INF:199). | — |
| TargetVersion / DriverTargetPlatform | `Windows10` / `Universal` | vcxproj:73,78 etc. |
| Configurations | `Debug`, `Release`, `ReleaseSigned` × `x64`, `ARM64`. There is no Win32 config, though the Platform fallback says `Win32` (vcxproj:60); x86 was removed in commit 8e0cc7c. | vcxproj:3-28 |
| SDK | `$(LatestTargetPlatformVersion)`. With `BuildEnvironment=Github` it is `10.0.19041.0`. | vcxproj:65-70 |
| OutDir / IntDir | `$(SolutionDir)build\$(ProjectName)\$(Platform)\$(ConfigurationName)\` / `$(SolutionDir)intermediate\$(ProjectName)\...` | vcxproj:130-166 |
| IncludePath | `$(DDK_INC_PATH);$(SolutionDir)intermediate\$(Platform)\$(ConfigurationName)\;$(ProjectDir)include;$(IncludePath)` | vcxproj:131 |
| Compiled sources | `Device.c Driver.c Hid.c InputInterrupt.c Queue.c` (C, not C++) | vcxproj:30-34 |
| Resource | `Resource.rc` | vcxproj:232 |
| Signing | `FileDigestAlgorithm=sha256` (x64 configs). ReleaseSigned uses `ProductionSign` with `$(ProductionCertPath)`; it is `Off` when `BuildEnvironment=AzurePipeline`. Timestamp server is `http://timestamp.digicert.com`. | vcxproj:168-204 |
| Packaging | `FilesToPackage Include="$(TargetPath)"` | vcxproj:228-230 |

### 1.2 Preprocessor defines and libraries

**Defines.** The vcxproj sets no `PreprocessorDefinitions`. The toolset supplies:

- `UMDF_VERSION_MAJOR=2` and `UMDF_VERSION_MINOR=15`
- `_DEBUG`/`DBG` in Debug
- WPP defines

`INPUT_CONTENT_TRACE` is **never defined**, so the per-contact traces at InputInterrupt.c:312-319, 341-356, 490-497 and 526-540 are compiled out.

**Libraries.** The vcxproj lists no `AdditionalDependencies`. The UMDF 2 toolset links:

- `WdfDriverStubUm.lib` (WDK `Lib/wdf/umdf/<arch>/2.15/WdfDriverStubUm.lib`). It supplies the exported UMDF entry and calls `DriverEntry`.
- `wpprecorderum.lib`, because `WppRecorderEnabled=true`
- default Win32 import libraries:
  - kernel32 for `QueryPerformanceCounter`
  - advapi32 for the ETW calls behind TraceLogging

For Rust, use the `windows-drivers-rs` UMDF configuration, or link `WdfDriverStubUm.lib` manually and export `DriverEntry` with the UMDF signature. **Verify that the stub's export name matches the toolchain you use.**

### 1.3 Tracing
**WPP tracing** (vcxproj:177-179, 186-189, 196-199, 207-209, 214-216, 221-223):

- `WppEnabled=true`, `WppRecorderEnabled=true` (in-flight recorder)
- `WppScanConfigurationData=include\trace.h`
- Control GUID `AmtPtpDriverTraceGuid` = `{efc3ce99-43ff-4b59-afe4-c856e1afd8b0}` (Trace.h:16-25)
- Flag bits, in order: `AMTPTPDRIVER_ALL_INFO`=0x1, `TRACE_DRIVER`=0x2, `TRACE_DEVICE`=0x4, `TRACE_QUEUE`=0x8, `TRACE_INPUT`=0x10
- Function `TraceEvents(LEVEL, FLAGS, MSG, ...)` (Trace.h:46). It is enabled when the flag is enabled and the level is ≥ the requested level (Trace.h:27-38).
- The second WPP function, `Trace{FLAG=MYDRIVER_ALL_INFO}` (Trace.h:45), names a flag that does not exist. It is never used. **Dead.**
- `.tmh` includes: `driver.tmh`, `device.tmh`, `Hid.tmh`, `InputInterrupt.tmh`, `queue.tmh` (each `.c:4/5`).

**TraceLogging (ETW manifest-free)** (Driver.c:7-10, ModernTrace.h:12-28):

- Provider name `"AmtPtpDeviceTraceProvider"`, GUID `{871B1E2D-CC5A-4ADE-B74E-6CF1004EF149}`, handle `g_hAmtPtpDeviceTraceProvider`.
- Event names:
  - `"DriverFunctionalEvent"`
  - `"DeviceIdentificationEvent"`
  - `"DeviceUsbOperationEvent"` (unused)
  - `"InputDiagnosticsEvent"` (unused)
- Field `"Subtype"` takes one of `"CriticalFailure"`, `"DeviceNotFoundInRegistry"`, `"DeviceDescriptorNotFoundInRegistry"`.

Emitted events:

| Where | Event | Fields |
|---|---|---|
| Driver.c:59-66 (WdfDriverCreate failed) | DriverFunctionalEvent | Routine=`"DriverEntry"`, Context=`"WdfDriverCreate"`, Status=i32, Subtype=`"CriticalFailure"` |
| Device.c:181-188 (PID not in table) | DeviceIdentificationEvent | Routine=`"AmtPtpEvtDevicePrepareHardware"`, idProduct u16, idVendor u16, Subtype=`"DeviceNotFoundInRegistry"` |
| Hid.c:168-174 (no HID descriptor for PID) | DeviceIdentificationEvent | Routine=`"AmtPtpGetHidDescriptor"`, idProduct, Subtype=`"DeviceDescriptorNotFoundInRegistry"` |
| Hid.c:379-385 | same, Routine=`"AmtPtpGetReportDescriptor"` | **Dead code**, see 10.3 |

**WPR profile** (`AmtPtpDevice.wprp`): collector `EventCollector_AmtPtpDeviceTraceProvider` with BufferSize 64 and 4 buffers, for the provider GUID above. Profiles: Verbose.File, Light.File, Verbose.Memory, Light.Memory (wprp:3-28). It is not compiled; it is listed as `None` (vcxproj:235).

**Rust.** Tracing is not behavioral. Map it to `tracing` or ETW TraceLogging, keeping the provider GUID if existing WPR profiles must keep working.

### 1.4 Resource (Resource.rc, UTF-16LE)
- `STRINGTABLE`: `IDS_APP_TITLE` (103, resource.h:5) = `"Apple Precision Touchpad Driver"` (rc:51-54)
- `VERSIONINFO` (rc:61-92):
  - FILEVERSION and PRODUCTVERSION `18,1,2,200`
  - FILEFLAGSMASK `0x3f`; FILEFLAGS `0x1` with `_DEBUG`, else `0`
  - FILEOS `0x40004` (VOS_NT_WINDOWS32)
  - **FILETYPE `0x1` (VFT_APP), although the binary is a DLL** (cosmetic)
- StringFileInfo `040904b0`:
  - CompanyName "Bingxing Wang"
  - FileDescription "Apple Precision Touchpad User Mode Driver"
  - FileVersion "18.1.2.200"
  - InternalName **"AmtPtpDevice.dll"**
  - LegalCopyright "Copyright (C) Bingxing Wang 2018"
  - OriginalFilename **"AmtPtpDevice.dll"**. This does not match the real `AmtPtpDeviceUsbUm.dll` (cosmetic).
  - ProductName "Apple Precision Touchpad Driver Component"
  - ProductVersion "18.1.2.200"
- `VarFileInfo` Translation `0x409, 1200`
- `TEXTINCLUDE 1` names `resource1.h` (APSTUDIO only, irrelevant)

### 1.5 INF sections that reference this binary (`AmtPtpDevice.inf`)
- `[Version]` (INF:5-12): `Signature="$WINDOWS NT$"`, `Provider=%ManufacturerName%`, `Class=HIDClass`, `ClassGuid={745a17a0-74d3-11d0-b6fe-00a0c90f57da}`, `CatalogFile=AmtPtpDevice.cat`, `PnpLockdown=1`, `DriverVer=` (stamped by the package project: `2021.$(PipelineReleaseID).1.1000` on Azure, `*` elsewhere; UniversalPkg.vcxproj:138-143).
- `[DestinationDirs]`: `DefaultDestDir=13`, `PtpUmDrivers_Dir=13` (DriverStore run-from-store) (INF:14-16).
- `[SourceDisksFiles]`: `AmtPtpDeviceUsbUm.dll = 1,,` (INF:23).
- Models `[Standard.NT$ARCH$]` → section `AmtPtpDeviceUsbUm_Install`. Hardware IDs are listed in section 8.2 (INF:47-72, INF:88).
- `[AmtPtpDeviceUsbUm_Install.NT]`: `CopyFiles=PtpUmDrivers_Dir`. `[PtpUmDrivers_Dir]` contains `AmtPtpDeviceUsbUm.dll` (INF:107-108, 178-179).
- `[AmtPtpDeviceUsbUm_Install.NT.hw]` → `AddReg=AmtPtpDeviceUsbUm_AddReg` (INF:181-182):
  - `HKR,,FriendlyName,,"Apple USB Precision Touchpad Device (User-mode)"`
  - `HKR,,"LowerFilters",0x00010008,"WUDFRd"` (MULTI_SZ | APPEND) (INF:202-204)
- `[AmtPtpDeviceUsbUm_Install.NT.Services]` (INF:184-186):
  - `AddService=mshidumdf, 0x000001fa, MSHIDUMDF_ServiceInstall`. The flags are ASSOCSERVICE plus all NOCLOBBER bits, which makes mshidumdf the function driver. The section name matches `[mshidumdf_ServiceInstall]` case-insensitively: kernel driver, demand start, `%12%\mshidumdf.sys` (INF:206-210).
  - `AddService=WUDFRd, 0x000001f8, WUDFRD_ServiceInstall`: kernel driver, demand start, `%12%\WUDFRd.sys` (INF:212-217).
- `[AmtPtpDeviceUsbUm_Install.NT.Wdf]` (INF:188-196):
  - `UmdfDispatcher=NativeUSB`: USB I/O is handled natively by the reflector (WUDFRd), **not** by WinUSB. This value requires UMDF ≥ 2.15 (MS Learn, "Specifying WDF Directives in INF Files"; `WinUsb` is the pre-2.15 value). The INF has no `Include/Needs=WINUSB`, consistent with this. **Load-bearing**: the port must keep using WDF USB I/O targets and UMDF ≥ 2.15.
  - `UmdfService=AmtPtpDeviceUsbUm,AmtPtpDeviceUsbUm_Install`
  - `UmdfServiceOrder=AmtPtpDeviceUsbUm`
  - `UmdfKernelModeClientPolicy=AllowKernelModeClients`: **load-bearing**, because HidClass and mshidumdf are kernel clients.
  - `UmdfFileObjectPolicy=AllowNullAndUnknownFileObjects`: **load-bearing**, because mshidumdf sends requests without a file object.
  - `UmdfMethodNeitherAction=Copy`: **load-bearing**. All HID IOCTLs are METHOD_NEITHER (section 4.1). With `Copy`, the reflector copies the buffers, and that is the only reason `WdfRequestRetrieveInputMemory`/`OutputMemory` work in this driver. The driver never touches `Irp->UserBuffer` or `Type3InputBuffer`.
  - `UmdfFsContextUsePolicy=CanUseFsContext2`
  - `UmdfHostPriority=PriorityHigh`
- `[AmtPtpDeviceUsbUm_Install]`: `UmdfLibraryVersion=2.15.0`, `ServiceBinary=%13%\AmtPtpDeviceUsbUm.dll` (INF:198-200).
- `[Strings]`: `AmtPtpDeviceUsbUm.DeviceDesc="Apple USB Precision Touchpad Device (User-mode)"`, `ManufacturerName="Bingxing Wang"`, `WudfRdDisplayName="Windows Driver Foundation - User-mode Driver Framework Reflector"` (INF:247-262).
- Package project: `AmtPtpDeviceUniversalPkg.vcxproj` references this project by GUID `{87efa31b-...}` (UniversalPkg.vcxproj:36-38).

---

## 2. Entry points and WDF callbacks

None of the following exist in the driver:

- registry reads
- timers
- work items
- spin locks or wait locks
- an `EvtDeviceReleaseHardware`
- self-managed I/O
- power-policy settings: no `WdfDeviceAssignS0IdleSettings`, no `WdfDeviceAssignSxWakeSettings`, so no idle or selective suspend. Because of `WdfFdoInitSetFilter` (and no `WdfDeviceInitSetPowerPolicyOwnership(TRUE)`), the driver is not the WDF power-policy owner, so a port could not simply add these calls.
- a file-object config: no `WdfDeviceInitSetFileObjectConfig`
- an I/O type setting (filter drivers inherit it)
- an explicit synchronization scope or execution level: every attributes block is default, meaning `WdfSynchronizationScopeInheritFromParent` and `WdfExecutionLevelInheritFromParent`, which give no framework serialization
- request contexts
- a queue context: `QUEUE_CONTEXT` is declared at Queue.h:9-15 but never attached, so it is dead

| # | Function | File:line | Registered on / how | Config |
|---|---|---|---|---|
| E1 | `DriverEntry(PDRIVER_OBJECT, PUNICODE_STRING)` | Driver.c:12-78 | Called by the UMDF stub | See 2.1 |
| E2 | `AmtPtpDeviceEvtDriverContextCleanup(WDFOBJECT)` | Driver.c:143-158 | `WDFDRIVER` attributes `.EvtCleanupCallback` (Driver.c:38-39) | Calls `DriverTraceCleanup` |
| E3 | `AmtPtpDeviceEvtDeviceAdd(WDFDRIVER, PWDFDEVICE_INIT)` | Driver.c:108-141 | `WDF_DRIVER_CONFIG.EvtDriverDeviceAdd` (Driver.c:41-43) | Calls `WdfFdoInitSetFilter` (Driver.c:128), then `AmtPtpCreateDevice` (Driver.c:130) and returns its status |
| E4 | `AmtPtpEvtDevicePrepareHardware` | Device.c:120-253 | `WDF_PNPPOWER_EVENT_CALLBACKS.EvtDevicePrepareHardware` (Device.c:51) | — |
| E5 | `AmtPtpEvtDeviceD0Entry` | Device.c:498-585 | `.EvtDeviceD0Entry` (Device.c:52) | — |
| E6 | `AmtPtpEvtDeviceD0Exit` | Device.c:587-642 | `.EvtDeviceD0Exit` (Device.c:53) | — |
| E7 | `AmtPtpDeviceEvtIoDeviceControl` | Queue.c:118-213 | Default queue `.EvtIoDeviceControl` (Queue.c:29) | — |
| E8 | `AmtPtpDeviceEvtIoStop` | Queue.c:215-258 | Default queue `.EvtIoStop` (Queue.c:30) | **No-op**: it only traces and neither acknowledges nor completes |
| E9 | `AmtPtpEvtUsbInterruptPipeReadComplete(WDFUSBPIPE, WDFMEMORY, size_t, WDFCONTEXT)` | InputInterrupt.c:82-211 | `WDF_USB_CONTINUOUS_READER_CONFIG` completion (InputInterrupt.c:47-52); context = `PDEVICE_CONTEXT` | — |
| E10 | `AmtPtpEvtUsbInterruptReadersFailed(WDFUSBPIPE, NTSTATUS, USBD_STATUS)` | InputInterrupt.c:213-226 | `.EvtUsbTargetPipeReadersFailed` (InputInterrupt.c:54) | Always returns `TRUE`, which tells the framework to reset the pipe and restart the reader. It does not log. |

### 2.1 DriverEntry (Driver.c:12-78)
1. `DriverTraceInit` (Driver.c:80-94) runs `WPP_INIT_TRACING(DriverObject, RegistryPath)` and then `TraceLoggingRegister(g_hAmtPtpDeviceTraceProvider)`.
2. `WDF_OBJECT_ATTRIBUTES_INIT(&attributes); attributes.EvtCleanupCallback = AmtPtpDeviceEvtDriverContextCleanup` (Driver.c:38-39).
3. `WDF_DRIVER_CONFIG_INIT(&config, AmtPtpDeviceEvtDeviceAdd)`. `DriverInitFlags=0`, there is no `EvtDriverUnload`, and the pool tag is default.
4. `WdfDriverCreate(DriverObject, RegistryPath, &attributes, &config, WDF_NO_HANDLE)` (Driver.c:45-50).
5. On failure, emit the TraceLogging event (Driver.c:59-66), call `DriverTraceCleanup(DriverObject)` (Driver.c:67), and return the status. On success, return `STATUS_SUCCESS`.

`DriverTraceCleanup` (Driver.c:96-106) runs `TraceLoggingUnregister`, then `WPP_CLEANUP(WdfDriverWdmGetDriverObject((WDFDRIVER) Driver))`. **`Driver` is an undeclared identifier.** This compiles only because UMDF's `WPP_CLEANUP` macro discards its argument; it calls `WppCleanupUm()`, as the comment at Driver.c:104 says. See B19.

Driver.c:67 passes a `PDRIVER_OBJECT` where a `WDFOBJECT` is expected. This is harmless because the argument is unused.

### 2.2 AmtPtpCreateDevice (Device.c:24-118), called from EvtDeviceAdd
1. Initialize `WDF_PNPPOWER_EVENT_CALLBACKS` and set PrepareHardware, D0Entry and D0Exit (Device.c:48-54). Nothing else is set (no ReleaseHardware, no SelfManagedIo*, no SurpriseRemoval).
2. `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&deviceAttributes, DEVICE_CONTEXT)` (Device.c:57). There are no cleanup or destroy callbacks. The context is **zero-filled by WDF**.
3. `WdfDeviceCreate(&DeviceInit, &deviceAttributes, &device)`. On failure, return the status.
4. `WDF_DEVICE_PNP_CAPABILITIES_INIT`, then `SurpriseRemovalOK = WdfTrue`, then `WdfDeviceSetPnpCapabilities` (Device.c:87-92). All other capabilities stay `WdfUseDefault`.
5. `WdfDeviceCreateDeviceInterface(device, &GUID_DEVINTERFACE_AmtPtpDevice, NULL)`.
   - The GUID is `{4a5064e5-7d39-41d1-a0e4-81097edce967}` (Trace.h:7-8).
   - No code path handles anything special for this interface. Any IOCTL that arrives through it hits the same dispatcher.
6. If step 5 succeeded, call `AmtPtpDeviceQueueInitialize(device)` (Queue.c:6-75). Return the final status.

### 2.3 Queues (Queue.c:6-75)

**Default queue** (Queue.c:24-37):

- `WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&cfg, WdfIoQueueDispatchParallel)`
- `EvtIoDeviceControl = AmtPtpDeviceEvtIoDeviceControl`, `EvtIoStop = AmtPtpDeviceEvtIoStop`
- `PowerManaged` is left at `WdfUseDefault`. Because the driver called `WdfFdoInitSetFilter`, the queue is **not power-managed** (MS Learn, WDF_IO_QUEUE_CONFIG.PowerManaged).
- Attributes: `WDF_NO_OBJECT_ATTRIBUTES`
- The handle is not stored.
- Parallel dispatch means unlimited concurrent presented requests (`NumberOfPresentedRequests=-1`).
- Request types with no handler (read, write, internal device control) are forwarded to the lower driver by the framework, per the filter-driver rules. Driver code is not involved.

**InputQueue** (Queue.c:53-61):

- `WDF_IO_QUEUE_CONFIG_INIT(&cfg, WdfIoQueueDispatchManual)`, `PowerManaged = WdfFalse`, no callbacks (no `EvtIoCanceledOnQueue`)
- Stored in `DEVICE_CONTEXT.InputQueue`
- While a request sits in it, cancellation is handled by the framework, which completes it with `STATUS_CANCELLED`.
- **Never purged or drained by driver code.** Because it is not power-managed, pending reads stay queued across Dx transitions. On removal the framework purges it.

### 2.4 AmtPtpEvtDevicePrepareHardware (Device.c:120-253)
Resource lists are unused. The steps, in order:

1. If `ctx.UsbDevice == NULL`, call `WdfUsbTargetDeviceCreate(Device, WDF_NO_OBJECT_ATTRIBUTES, &ctx.UsbDevice)`. On failure, return the status (Device.c:148-163). On restart it is reused.
2. `WdfUsbTargetDeviceGetDeviceDescriptor(ctx.UsbDevice, &ctx.DeviceDescriptor)` (Device.c:166-169).
3. `ctx.DeviceInfo = AmtPtpGetDeviceConfig(descriptor)` (Device.c:6-22). This is a linear scan of `Bcm5974ConfigTable` that matches `idProduct` against `ansi`, `iso` or `jis`. **idVendor is not checked.**
   - The loop condition is `cfg->ansi != 0`, but the table **has no sentinel entry**. See B3.
   - If nothing matches, return `STATUS_INVALID_DEVICE_STATE` and emit TraceLogging (Device.c:171-191). The outer `if (NT_SUCCESS(status))` at :171 is always true.
4. `WDF_USB_DEVICE_INFORMATION_INIT`, then `WdfUsbTargetDeviceRetrieveInformation` (Device.c:197-231).
   - On success, `ctx.UsbDeviceTraits = deviceInfo.Traits`. Otherwise it is 0.
   - `waitWakeEnable = Traits & WDF_USB_DEVICE_TRAIT_REMOTE_WAKE_CAPABLE` is computed and only traced. **Dead.**
   - `UsbDeviceTraits` is never read afterwards. **Dead state.**
5. `SelectInterruptInterface(Device)` (Device.c:644-727; see 6.1). On failure, return.
6. `AmtPtpConfigContReaderForInterruptEndPoint(ctx)` (InputInterrupt.c:6-80; see 6.2). On failure, return.
7. `ctx.IsButtonReportOn = TRUE; ctx.IsSurfaceReportOn = TRUE` (Device.c:248-249). Both are reset on **every** PrepareHardware.
8. Return `status`, which is `STATUS_SUCCESS` here.

Restart caveat: there is no ReleaseHardware. On a second PrepareHardware (stop/start, rebalance), `ctx.InterruptPipe` keeps its old handle, and `WdfUsbTargetPipeConfigContinuousReader` is called again on the pipe. The driver never calls `WdfUsbTargetDeviceSelectConfig`, so the pipe object (and its reader) survives. Open-source WDF (`src/framework/shared/targets/usb/fxusbpipe.cpp`, `FxUsbPipe::InitContinuousReader`) returns `STATUS_INVALID_DEVICE_STATE` when `m_Reader != NULL`. So the second PrepareHardware is expected to **fail**, and the device does not restart after a PnP stop/start. See B16.

### 2.5 AmtPtpEvtDeviceD0Entry (Device.c:498-585)
1. If `ctx.IsButtonReportOn || ctx.IsWellspringModeOn` (Device.c:519), call `AmtPtpSetWellspringMode(ctx, TRUE)`. A failure only produces a warning; the callback continues.
   - Right after PrepareHardware, `IsButtonReportOn` is always TRUE, so mode is always enabled on the first D0.
   - After a SET_FEATURE that cleared bit 0, a later D0Entry does **not** re-enable mode, provided D0Exit's `SetWellspringMode(FALSE)` succeeded and cleared `IsWellspringModeOn`. If that call failed, the flag is still TRUE and D0Entry does re-enable. See B10.
2. `QueryPerformanceCounter(&ctx.PerfCounter)` (Device.c:542-544). This is the **only** place the scan-time base is set (B12).
3. `WdfIoTargetStart(WdfUsbTargetPipeGetIoTarget(ctx.InterruptPipe))` (Device.c:550). On failure, return that status, which makes the device fail.
4. The cleanup at Device.c:565-576 (`if (!NT_SUCCESS) && isTargetStarted → WdfIoTargetStop`) is **dead code**. `isTargetStarted` becomes TRUE only on success.
5. Return the status. Note that a SetWellspringMode failure is not reflected in it.

### 2.6 AmtPtpEvtDeviceD0Exit (Device.c:587-642)
1. `WdfIoTargetStop(pipe target, WdfIoTargetCancelSentIo)` (Device.c:609-612). This cancels the reader's in-flight reads.
2. `status = AmtPtpSetWellspringMode(ctx, FALSE)` (Device.c:621-624). A failure is traced as a warning.
3. **Return `status`**, the mode-switch status (Device.c:641).
   - On surprise removal or a failed transfer, D0Exit reports failure to the framework. Per "Reporting Device Failures", the framework treats this as a device failure.
   - For TYPE3 (WS8), this always succeeds because no I/O is sent.
   - See B9.
4. `PAGED_CODE()` at :596 is a no-op in UMDF.

---

## 3. Device context (`DEVICE_CONTEXT`, Device.h:6-29; accessor `DeviceGetContext`, Device.h:36)

The context is zero-initialized by WDF. Offsets are for x64 (computed); the size is 96 bytes. The context is attached to the WDFDEVICE.

**There is no locking anywhere.** These callbacks race on the context fields with no synchronization:

- the parallel default queue (many IOCTLs at once)
- the continuous-reader completions (framework default NumPendingReads > 1, so completions can overlap)
- PnP/power callbacks

| Off | Field | C type | Meaning | Writers | Readers |
|---|---|---|---|---|---|
| 0 | `UsbDevice` | WDFUSBDEVICE | USB target | PrepareHardware (Device.c:149-151), once | Get/SetWellspringMode, GetStrings, SelectInterruptInterface |
| 8 | `InterruptPipe` | WDFUSBPIPE | First interrupt pipe of interface 0 | SelectInterruptInterface (Device.c:705) | D0Entry/D0Exit (start/stop), ConfigContReader |
| 16 | `UsbInterface` | WDFUSBINTERFACE | Interface 0 | SelectInterruptInterface (Device.c:682) | SelectInterruptInterface only |
| 24 | `InputQueue` | WDFQUEUE | Manual queue of pending `IOCTL_HID_READ_REPORT` requests | QueueInitialize (Queue.c:60) | DispatchReadReport (forward), both Service* routines (retrieve) |
| 32 | `DeviceDescriptor` | USB_DEVICE_DESCRIPTOR (18 B) | Cached device descriptor | PrepareHardware (Device.c:166-169) | Config lookup; HID descriptor switch on `idProduct`; attributes `ProductID`; string indices `iManufacturer`/`iProduct`/`iSerialNumber` |
| 56 | `DeviceInfo` | `const struct BCM5974_CONFIG*` | Row in `Bcm5974ConfigTable` | PrepareHardware (Device.c:173) | Every I/O path |
| 64 | `UsbDeviceTraits` | ULONG | USB traits | PrepareHardware (Device.c:228/230) | **nobody (dead)** |
| 68 | `PressureQualLevel` | UCHAR | User-mode "pressure qualification" | SET_FEATURE 0x09 (Hid.c:1086) | GET_FEATURE 0x09 (Hid.c:862) only. **No effect on input** (B15). |
| 69 | `SgContactSizeQualLevel` | UCHAR | "single-contact size qualification" | Hid.c:1084 | Hid.c:861 only |
| 70 | `MuContactSizeQualLevel` | UCHAR | "multi-contact size qualification" | Hid.c:1085 | Hid.c:860 only |
| 72 | `IsWellspringModeOn` | BOOL (i32) | Driver's belief that raw multitouch mode is on | SetWellspringMode, on success only (Device.c:383, :484) | D0Entry gate (:519), ReadComplete gate (InputInterrupt.c:132) |
| 76 | `IsSurfaceReportOn` | BOOL | Selective reporting: contacts | PrepareHardware (=TRUE), SET_FEATURE 0x06 (Hid.c:1052, from **bit 1**) | Service* (InputInterrupt.c:306, :485) |
| 80 | `IsButtonReportOn` | BOOL | Selective reporting: button. **Also gates mode enable in D0Entry.** | PrepareHardware (=TRUE), SET_FEATURE 0x06 (Hid.c:1051, from **bit 0**) | Service* (:361, :545), D0Entry (Device.c:519) |
| 88 | `PerfCounter` | LARGE_INTEGER (i64) | QPC value at the last D0Entry | D0Entry (Device.c:542-544) | Service* scan time (:280, :475) |

Pool tag `POOL_TAG_PTP_CONTROL 'PTPC'` (Device.h:41) is used only for the `WdfMemoryCreate` calls in Get/SetWellspringMode. The pool type is ignored in UMDF.

---

## 4. IOCTL handling

### 4.1 Codes

All codes are `HID_CTL_CODE(id) = CTL_CODE(FILE_DEVICE_KEYBOARD=0x0B, id, METHOD_NEITHER=3, FILE_ANY_ACCESS=0)`, which works out to `0x000B0003 | (id<<2)`. Source: WDK `hidclass.h:133`, `hidport.h:183-200`. The values were computed. The driver defines **no custom IOCTLs**.

| IOCTL | id | Value | Handler | Result |
|---|---|---|---|---|
| IOCTL_HID_GET_DEVICE_DESCRIPTOR | 0 | 0x000B0003 | `AmtPtpGetHidDescriptor` (Hid.c:7-186) | 4.3 |
| IOCTL_HID_GET_REPORT_DESCRIPTOR | 1 | 0x000B0007 | `AmtPtpGetReportDescriptor` (Hid.c:245-397) | 4.5 |
| IOCTL_HID_READ_REPORT | 2 | 0x000B000B | `AmtPtpDispatchReadReportRequests` (Queue.c:260-302) | 4.7 |
| IOCTL_HID_WRITE_REPORT | 3 | 0x000B000F | — | `STATUS_NOT_SUPPORTED` |
| IOCTL_HID_GET_STRING | 4 | 0x000B0013 | `AmtPtpGetStrings` (Hid.c:399-541) | 4.6 |
| IOCTL_HID_ACTIVATE_DEVICE | 7 | 0x000B001F | — | `STATUS_NOT_SUPPORTED` |
| IOCTL_HID_DEACTIVATE_DEVICE | 8 | 0x000B0023 | — | `STATUS_NOT_SUPPORTED` |
| IOCTL_HID_GET_DEVICE_ATTRIBUTES | 9 | 0x000B0027 | `AmtPtpGetDeviceAttribs` (Hid.c:188-243) | 4.4 |
| IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST | 10 | 0x000B002B | — | `STATUS_NOT_SUPPORTED`. HidClass therefore never idles the device; there is no selective suspend. |
| IOCTL_UMDF_HID_SET_FEATURE | 20 | 0x000B0053 | `AmtPtpSetFeatures` (Hid.c:897-1130) | 5.6 |
| IOCTL_UMDF_HID_GET_FEATURE | 21 | 0x000B0057 | `AmtPtpReportFeatures` (Hid.c:711-895) | 5.5 |
| IOCTL_UMDF_HID_SET_OUTPUT_REPORT | 22 | 0x000B005B | — | `STATUS_NOT_SUPPORTED` |
| IOCTL_UMDF_HID_GET_INPUT_REPORT | 23 | 0x000B005F | — | `STATUS_NOT_SUPPORTED` |
| anything else (e.g. IOCTL_UMDF_GET_PHYSICAL_DESCRIPTOR = 0x000B0063) | — | — | default | `STATUS_NOT_SUPPORTED` |

### 4.2 Dispatcher (Queue.c:118-213)

- `device = WdfIoQueueGetDevice(Queue)`.
- A `switch` maps each code to its handler. Every unsupported code completes with `STATUS_NOT_SUPPORTED` and is **not forwarded** to the lower target (Queue.c:188-202).
- `DbgIoControlGetString` (Queue.c:77-116) exists only for traces.
- After the switch, unless the handler set `requestPending = TRUE` (only READ_REPORT does), it calls `WdfRequestComplete(Request, status)` (Queue.c:205-210). Information was set by the handler with `WdfRequestSetInformation`, or stays 0.
- `OutputBufferLength` and `InputBufferLength` are only traced. Handlers call `WdfRequestRetrieve*` themselves.

### 4.3 IOCTL_HID_GET_DEVICE_DESCRIPTOR (Hid.c:7-186)

1. Call `WdfRequestRetrieveOutputMemory`. On failure, return that status.
2. Switch on `ctx.DeviceDescriptor.idProduct` (Hid.c:42-137) to pick one of six `HID_DESCRIPTOR` globals (section 5.1). The PID-to-descriptor map is in section 8.3.
3. If a descriptor was found, run `WdfMemoryCopyFromBuffer(mem, 0, desc, desc.bLength /*9*/)`:
   - If the buffer is under 9 bytes, the copy fails. Return that status (STATUS_BUFFER_TOO_SMALL) with Information 0.
   - Otherwise Information = 9 and the status is SUCCESS.
4. If no descriptor matched, emit TraceLogging and return `STATUS_INVALID_DEVICE_STATE`.

The bytes written are always `09 21 00 01 00 01 22 04 02`. This is the packed `HID_DESCRIPTOR` (hidport.h:126-144, `pshpack1`):

| Bytes | Field | Value |
|---|---|---|
| `09` | bLength | 9 |
| `21` | bDescriptorType | 0x21 |
| `00 01` | bcdHID | 0x0100 |
| `00` | bCountry | 0 |
| `01` | bNumDescriptors | 1 |
| `22` | bReportType | 0x22 |
| `04 02` | wReportLength | 516 |

All six report descriptors are 516 bytes, so all six HID descriptors are identical.

### 4.4 IOCTL_HID_GET_DEVICE_ATTRIBUTES (Hid.c:188-243)

`WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_DEVICE_ATTRIBUTES)=32, &p, NULL)`. On failure, return that status.

The output is the 32-byte `HID_DEVICE_ATTRIBUTES` (hidport.h:108-124):

| Offset | Field | Value | Note |
|---|---|---|---|
| 0 | `Size` (u32) | 32 | |
| 4 | `VendorID` (u16) | **`DEVICE_VID` = 0x8910** (Hid.h:20) | Deliberately *not* 0x05AC (comment Hid.c:225-226). This avoids conflict with in-box Apple drivers. |
| 6 | `ProductID` (u16) | the real `idProduct` | |
| 8 | `VersionNumber` (u16) | `DEVICE_VERSION` = 0x0001 (Hid.h:19) | |
| 10..31 | `Reserved[11]` | **not written** | Left as whatever the buffer held |

Information = 32. Status is SUCCESS.

### 4.5 IOCTL_HID_GET_REPORT_DESCRIPTOR (Hid.c:245-397)

1. `WdfRequestRetrieveOutputMemory`.
2. Same PID switch (Hid.c:280-339), selecting `AmtPtp{3,5,6,7a,8,Mt2}ReportDescriptor` with length `...DescriptorList[0].wReportLength` = 516.
3. If found, copy 516 bytes:
   - On failure, return the copy status (Hid.c:349-357).
   - Otherwise Information = 516, SUCCESS.
4. If the PID is unknown, `szHidDescriptor` stays 0, so the `else if (szHidDescriptor == 0)` branch (Hid.c:364-372) returns `STATUS_INVALID_DEVICE_STATE`. The final `else` with TraceLogging (Hid.c:373-388) is **unreachable**.

### 4.6 IOCTL_HID_GET_STRING (Hid.c:399-541): **broken, see B7**

1. `WdfRequestRetrieveInputMemory`, then `WdfMemoryGetBuffer`. If the input length is under 4, return `STATUS_INVALID_BUFFER_SIZE`.
2. Read `v = *(u32*)input`. Then `stringId = v & 0xFFFF` and `languageId = v >> 16` (Hid.c:462-464).
3. Map `stringId` to a USB string index:

   | stringId | Index |
   |---|---|
   | `HID_STRING_ID_IMANUFACTURER` (14) | `iManufacturer` |
   | `HID_STRING_ID_IPRODUCT` (15) | `iProduct` |
   | `HID_STRING_ID_ISERIALNUMBER` (16) | `iSerialNumber` |
   | anything else | **return the current status, which is STATUS_SUCCESS, with Information 0** (Hid.c:478-484) |

4. `WdfUsbTargetDeviceAllocAndQueryString(UsbDevice, WDF_NO_OBJECT_ATTRIBUTES, &memHandle, &wcharCount, strIndex, (USHORT)languageId)` sends a GET_DESCRIPTOR(STRING) to the device. On failure, return that status.
5. `WdfRequestRetrieveOutputBuffer(Request, wcharCount*2, &pStringBuffer, &actualSize)`. On failure (output too small), return that status, typically STATUS_BUFFER_TOO_SMALL.
6. **Bug.** `WdfMemoryCopyToBuffer(memHandle, 0, &pStringBuffer, actualSize)` (Hid.c:522-527):
   - It copies into the **address of the local pointer variable**, not the output buffer.
   - Its return value is ignored.
   - Result: nothing reaches the caller.
   - `actualSize` is the output buffer length, which is at least `wcharCount*2`. If it is larger than the string memory, the copy fails on the source-range check, and the failure is ignored.
   - If it is equal, `actualSize` bytes are written over the 8-byte stack slot. That is a stack overrun whenever the string is longer than 4 WCHARs.
7. Information = `actualSize`, the **full output buffer length**. Status is SUCCESS.
8. `memHandle` is never deleted. It leaks until its WDF parent is destroyed.

### 4.7 IOCTL_HID_READ_REPORT (Queue.c:260-302)

- `WdfRequestForwardToIoQueue(Request, ctx.InputQueue)`.
  - On failure, return that status. The dispatcher completes the request with it.
  - On success, set `*Pending = TRUE`. The dispatcher does not complete it.
- The request is then completed later by `AmtPtpServiceTouchInputInterrupt*` (section 7) when an interrupt packet arrives. Information = 50 and the status is SUCCESS.
- There is no timeout.
- If no input arrives, the request stays queued indefinitely. That includes:
  - while not in Wellspring mode
  - while selective reporting is off
  - for TYPE1 devices
- Queued requests survive D0Exit because the queue is not power-managed.
- Read output-buffer size is never validated up front. A buffer under 50 bytes makes `WdfMemoryCopyFromBuffer` fail, and the request is then **never completed** (B6).

---

## 5. HID: descriptors, attributes, feature reports

### 5.1 Report descriptor symbols (all defined in StaticHidRegistry.h, included only by Hid.c:4)

| Symbol | File:line | Composition | Length (computed) | SHA-256 (computed) |
|---|---|---|---|---|
| `AmtPtp3ReportDescriptor` | StaticHidRegistry.h:6-10 | `AAPL_WELLSPRING_3_PTP_TLC` (Wellspring3.h:85-139) + `AAPL_PTP_WINDOWS_CONFIGURATION_TLC` (Hid.h:35-60) + `AAPL_PTP_USERMODE_CONFIGURATION_APP_TLC` (Hid.h:22-33) | 516 | a94733137748c90e10982b8e472b2f82f4e1fac862b32b83ee67e7629aed214c |
| `AmtPtp5ReportDescriptor` | :12-16 | WS5 TLC (Wellspring5.h) + same two | 516 | 3b57036a6f16393ef1be451395fc4eec774f0630d798288a762b5e38133285ca |
| `AmtPtp6ReportDescriptor` | :18-22 | WS6 TLC (Wellspring6.h) + same | 516 | 83e1b906e55b42957a3a504d36219004ffcb83065ed2fd2d6d035bb14485d007 |
| `AmtPtp7aReportDescriptor` | :24-28 | WS7A TLC (Wellspring7A.h) + same | 516 | 9a509e4407b52d68371a8e49b999cc7c55e268c5e4e5d75437c8ba4c34e21703 |
| `AmtPtp8ReportDescriptor` | :30-34 | WS8 TLC (Wellspring8.h) + same | 516 | 13d94c5f486b201ef4d7919d2f955415560799cc78b12524b874bce6dee7724a |
| `AmtPtpMt2ReportDescriptor` | :36-40 | MT2 TLC (WellspringMt2.h:84-138) + same | 516 | c803f5c305a07a33d24e518be786ade6338bad844105657e64c582ba7fa13f25 |
| `AmtPtp{3,5,6,7a,8,Mt2}DefaultHidDescriptor` | :42-112 | `{0x09,0x21,0x0100,0x00,0x01,{0x22, sizeof(desc)}}` | 9 each | — |

Sub-block lengths (computed):

| Block | Bytes | Descriptor offsets |
|---|---|---|
| PTP TLC (any family) | 446 | 0..445 |
| Windows configuration TLC | 47 | 446..492 |
| User-mode TLC | 23 | 493..515 |

Within the PTP TLC (`*_FINGER_COLLECTION_1` is 70 bytes, `_2` is 64 bytes):

| Bytes | Content |
|---|---|
| 0..9 | `05 0d 09 05 a1 01 85 05 09 22` |
| 10..79 | finger 1, FC1 |
| 80..83 | `05 0d 09 22` |
| 84..153 | finger 2, FC1 |
| 154..157 | `05 0d 09 22` |
| 158..221 | finger 3, FC2 |
| 222..225 | `05 0d 09 22` |
| 226..295 | finger 4, FC1 |
| 296..299 | `05 0d 09 22` |
| 300..363 | finger 5, FC2 |
| 364..445 | report-level tail |

FC2 omits the trailing `45 00 55 00 65 00` (PhysMax 0, UnitExp 0, Unit none). Global state therefore carries over, but only in one place:

- Finger 3 (FC2) is followed by finger 4. Finger 4's Confidence, TipSwitch and ContactID items inherit Unit=cm, UnitExp=-2 and PhysMax = finger 3's Y physical maximum (for example 760 for WS3).
- Finger 4 is FC1, which resets these values, so finger 5 inherits nothing.
- The state left by finger 5 (FC2) is overwritten by the report-level tail, which sets `55 0c`, `66 01 10` and `47 ff ff 00 00` explicitly before Scan Time.

Global items also persist past the end of the collection. Contact Count, the button bits, the feature reports 0x07/0x08 and the later TLCs (0x04/0x06/0x09) all inherit Unit=seconds, UnitExp=-4 and PhysMax=65535 from the tail. **Reproduce verbatim.**

**Only these bytes differ between families** (computed diff). They are the four 2-byte little-endian values of each finger collection:

| Item byte (value bytes) for fingers 1..5 | Item | WS3 | WS5 | WS6 | WS7A | WS8 | MT2 |
|---|---|---|---|---|---|---|---|
| 45, 119, 193, 261, 335 (value at +1,+2) | `26` Logical Max X | 9626 (`9a 25`) | 9465 (`f9 24`) | 9760 (`20 26`) | 10030 (`2e 27`) | 9760 (`20 26`) | 7612 (`bc 1d`) |
| 56, 130, 204, 272, 346 | `46` Physical Max X (0.01 cm) | 1050 (`1a 04`) | 1050 | 1067 (`2b 04`) | 1064 (`28 04`) | 1045 (`15 04`) | 1600 (`40 06`) |
| 63, 137, 211, 279, 353 | `46` Physical Max Y | 760 (`f8 02`) | 810 (`2a 03`) | 762 (`fa 02`) | 773 (`05 03`) | 750 (`ee 02`) | 1149 (`7d 04`) |
| 66, 140, 214, 282, 356 | `26` Logical Max Y | 6775 (`77 1a`) | 6735 (`4f 1a`) | 6750 (`5e 1a`) | 6880 (`e0 1a`) | 6750 (`5e 1a`) | 5065 (`c9 13`) |

Rust: store one 516-byte template plus a per-family 4×u16 patch table, or six literal arrays. Verify against the SHA-256 values above.

Full byte dump of `AmtPtp3ReportDescriptor` (computed by expanding the macros; other families differ only at the offsets above):
```
05 0d 09 05 a1 01 85 05 09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01
75 20 27 ff ff ff ff 09 51 81 02 05 01 26 9a 25 75 10 55 0e 65 11 09 30 46 1a 04 95 01 81 02 46
f8 02 26 77 1a 09 31 81 02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02 25 01 09 47 09 42 95 02 75 01
81 02 75 01 95 06 81 03 95 01 75 20 27 ff ff ff ff 09 51 81 02 05 01 26 9a 25 75 10 55 0e 65 11
09 30 46 1a 04 95 01 81 02 46 f8 02 26 77 1a 09 31 81 02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02
25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01 75 20 27 ff ff ff ff 09 51 81 02 05
01 26 9a 25 75 10 55 0e 65 11 09 30 46 1a 04 95 01 81 02 46 f8 02 26 77 1a 09 31 81 02 c0 05 0d
09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01 75 20 27 ff ff ff ff 09
51 81 02 05 01 26 9a 25 75 10 55 0e 65 11 09 30 46 1a 04 95 01 81 02 46 f8 02 26 77 1a 09 31 81
02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03
95 01 75 20 27 ff ff ff ff 09 51 81 02 05 01 26 9a 25 75 10 55 0e 65 11 09 30 46 1a 04 95 01 81
02 46 f8 02 26 77 1a 09 31 81 02 c0 05 0d 55 0c 66 01 10 47 ff ff 00 00 27 ff ff 00 00 09 56 81
02 09 54 25 7f 75 08 81 02 05 09 09 01 25 01 75 01 81 02 95 07 81 03 05 0d 85 07 09 55 09 59 15
00 26 ff 00 75 08 95 02 b1 02 06 00 ff 85 08 09 c5 15 00 26 ff 00 75 08 96 00 01 b1 02 c0 05 0d
09 0e a1 01 85 04 09 22 a1 02 09 52 15 00 25 10 75 08 95 01 b1 02 c0 a1 00 85 06 09 57 09 58 75
01 95 02 25 01 b1 02 95 06 b1 03 c0 c0 06 00 ff 09 01 a1 01 85 09 09 01 15 00 26 ff 00 75 08 95
03 b1 02 c0
```

### 5.2 Decoded meaning (reports declared)
Computed from the item stream:

| Report ID | Kind | Bytes including ID | Usage context | Layout |
|---|---|---|---|---|
| 0x05 `REPORTID_MULTITOUCH` | Input | **50** | Digitizer / Touch Pad TLC (0x0D/0x05) | See 7.1 |
| 0x07 `REPORTID_DEVICE_CAPS` | Feature | 3 | Contact Count Maximum (0x55), Pad Type (0x59), 8 bits each | `[id, maxContacts, buttonType]` |
| 0x08 `REPORTID_PTPHQA` | Feature | 257 | Vendor page 0xFF00, usage 0xC5, 256 × 8-bit | `[id, blob[256]]` |
| 0x04 `REPORTID_REPORTMODE` | Feature | 2 | Digitizer Configuration TLC (0x0D/0x0E), Input Mode 0x52, logical max 16 (`MAX_FINGERS`, an oddity) | `[id, mode]` |
| 0x06 `REPORTID_FUNCSWITCH` | Feature | 2 | Usages listed in order `0x57` then `0x58`; 2×1 bit + 6 constant bits | `[id, bit0=usage 0x57, bit1=usage 0x58]` |
| 0x09 `REPORTID_UMAPP_CONF` | Feature | 4 | Vendor page 0xFF00, usage 0x01 TLC; 3 × 8-bit | `[id, pressure, single, multi]` |

Report ID 0x02 (`REPORTID_STANDARDMOUSE`, HidCommon.h:3) is **not declared and is unused**. `HID_AAPL_MOUSE_REPORT` and `HID_INPUT_REPORT` (Hid.h:106-118) are dead.

**Usage naming bug (B14).** HidCommon.h:11-12 defines `BUTTON_SWITCH 0x57` and `SURFACE_SWITCH 0x58`. In the HID Usage Tables and the MS PTP spec ("Selective Reporting Feature Report"), **0x57 is Surface Switch and 0x58 is Button Switch**. The descriptor bytes (`09 57 09 58`) are standard, so bit 0 is Surface Switch and bit 1 is Button Switch. The driver's struct, however, treats bit 0 as `ButtonReport` and bit 1 as `SurfaceReport` (Hid.h:137-142, Hid.c:1051-1052). The semantics are therefore **swapped**. It is invisible when Windows sets both bits equal.

### 5.3 Report constants (Hid.h:96-104, HidCommon.h:3-12)

- `PTP_MAX_CONTACT_POINTS=5`
- `PTP_BUTTON_TYPE_CLICK_PAD=0`; `PTP_BUTTON_TYPE_PRESSURE_PAD=1` is unused
- `PTP_COLLECTION_MOUSE=0`, `PTP_COLLECTION_WINDOWS=3`
- `PTP_CONTACT_CONFIDENCE_BIT=1` and `PTP_CONTACT_TIPSWITCH_BIT=2` are unused

### 5.4 Feature-report transport (UMDF HID conventions)

**GET** (`RequestGetHidXferPacketToReadFromDevice`, Hid.c:543-630):

- The report ID is `input[0]`. The input must be at least 1 byte, or the status is `STATUS_INVALID_BUFFER_SIZE`.
- The report buffer is the **output** buffer, with its full length.

**SET** (`RequestGetHidXferPacketToWriteToDevice`, Hid.c:632-709):

- The report ID is `(u8) OutputBufferLength`. This is the mshidumdf convention described at Hid.c:640-651; the output buffer itself is not readable.
- The report buffer is the **input** buffer, with its full length.

Only `RetrieveInput/OutputMemory` failures are errors. There are no other checks.

### 5.5 GET_FEATURE (`AmtPtpReportFeatures`, Hid.c:711-895)

If packet retrieval fails, return that status.

| reportId | Min len | Writes | Information | Status |
|---|---|---|---|---|
| 0x07 | 3 | `[0]=0x07, [1]=5, [2]=0` (click pad) | 3 | SUCCESS |
| 0x08 | 257 | `[0]=0x08`, **`[1]=0xFC` only** (B2). Bytes 2..256 are left untouched. | 257 | SUCCESS |
| 0x09 | 4 | `[0]=0x09, [1]=PressureQualLevel, [2]=SgContactSizeQualLevel, [3]=MuContactSizeQualLevel` | 4 | SUCCESS |
| other | — | — | 0 | `STATUS_NOT_SUPPORTED` |

If the length is too short, return `STATUS_INVALID_BUFFER_SIZE` with Information 0.

The intended HQA blob is `DEFAULT_PTP_HQA_BLOB` (Hid.h:62-94): 256 bytes, starting `fc 28 fe 84` and ending `a5 e5 24 c2`, SHA-256 b57b851d567808906f61a0122273da05c972140f1007355390aaf5dda8a072af (computed). Hid.c:822 writes `*certReport->CertificationBlob = DEFAULT_PTP_HQA_BLOB;`. Through the comma operator, that assigns only the first element.

### 5.6 SET_FEATURE (`AmtPtpSetFeatures`, Hid.c:897-1130)

**Always returns `STATUS_SUCCESS`** (Hid.c:1128), even when the packet retrieval fails, a mode switch fails, or the report ID is unsupported (B8). None of the cases check the input length; they read bytes 1..3 regardless.

**0x04 REPORTMODE** (Hid.c:935-1041). The mode is `buf[1]`.

1. Call `AmtPtpGetWellspringMode(ctx, &on)`, which does device I/O except for TYPE3. On failure, jump to exit with Information 0, still returning SUCCESS.
2. Act on the mode:

   | `buf[1]` | Condition | Action |
   |---|---|---|
   | 0 (mouse) | `on` | `AmtPtpSetWellspringMode(ctx, FALSE)`. On failure, jump to exit. |
   | 3 (PTP) | `!on` | `AmtPtpSetWellspringMode(ctx, TRUE)`. On failure, jump to exit. |
   | any other | — | Nothing |

3. On the normal path, Information = 2.

The decision uses the **device-reported** state, not `ctx.IsWellspringModeOn`. For TYPE3 (WS8), `Get` always reports TRUE, so a PTP request never sets the flag again after a mouse request. See B4.

**0x06 FUNCSWITCH** (Hid.c:1042-1073):

- `ctx.IsButtonReportOn = buf[1] & 1`
- `ctx.IsSurfaceReportOn = (buf[1] >> 1) & 1`
- Information = 2. No device I/O.

**0x09 UMAPP_CONF** (Hid.c:1074-1110):

- `PressureQualLevel = buf[1]`, `SgContactSizeQualLevel = buf[2]`, `MuContactSizeQualLevel = buf[3]`
- Information = 4

**Other IDs**: Information 0, return SUCCESS. The internal status is `STATUS_NOT_SUPPORTED` but it is discarded.

No feature reports exist for latency mode, battery, or the PTP "button press threshold". The battery input report 0x90 that the Settings app expects is not implemented (section 9).

---

## 6. Device I/O to the hardware

The driver uses nothing but USB through WDF USB targets:

- no SPI
- no lower HID IOCTLs
- no timers, DPCs or work items
- no idle or selective suspend
- no wake

### 6.1 Interface / pipe selection (`SelectInterruptInterface`, Device.c:644-727)

1. `WDF_USB_DEVICE_SELECT_CONFIG_PARAMS_INIT_SINGLE_INTERFACE(&configParams)` is filled in but **`WdfUsbTargetDeviceSelectConfig` is never called**. `configParams` is used only as scratch, so it is dead.
   - In UMDF, the framework has already selected the first configuration with default alternate settings (MS Learn, "How to use the continuous reader…").
   - The driver is loaded on a composite child (`MI_01` or `MI_02`), so "interface 0" here means the first interface that the child function exposes.
2. `usbInterface = WdfUsbTargetDeviceGetInterface(UsbDevice, 0)`. If it is NULL, return `STATUS_UNSUCCESSFUL`.
3. Set `ctx.UsbInterface = usbInterface` and `n = WdfUsbInterfaceGetNumConfiguredPipes(usbInterface)`.
4. For `index` from 0 to n-1:
   - `pipe = WdfUsbInterfaceGetConfiguredPipe(iface, index, &pipeInfo)`
   - `WdfUsbTargetPipeSetNoMaximumPacketSizeCheck(pipe)`. This applies to every pipe visited, up to and including the chosen one.
   - If `pipeInfo.PipeType == WdfUsbPipeTypeInterrupt`, set `ctx.InterruptPipe = pipe` and stop.
   - **Direction is not checked.** An interrupt OUT pipe enumerated first would be chosen.
   - The config table's `tp_ep` (0x81 or 0x83) is **not used**.
5. If `ctx.InterruptPipe == NULL`, return `STATUS_INVALID_DEVICE_STATE`. On a restart the old handle is still non-NULL, and the following `ConfigContinuousReader` on it is expected to fail with `STATUS_INVALID_DEVICE_STATE` (B16).

### 6.2 Continuous reader (`AmtPtpConfigContReaderForInterruptEndPoint`, InputInterrupt.c:6-80)

`transferLength = HEADER_TYPEn + FSIZE_TYPEn * MAX_FINGERS` (switch at :23-40), with `MAX_FINGERS=16` (AppleDefinition.h:122):

| Type | Header | Finger size | Transfer length | Same as `tp_datalen` |
|---|---|---|---|---|
| TYPE1 | 26 | 28 | **474** | yes |
| TYPE2 | 30 | 28 | **478** | yes |
| TYPE3 | 38 | 28 | **486** | yes |
| TYPE4 | 46 | 30 | **526** | yes |
| TYPE5 | 12 | 9 | **156** | yes |

The `transferLength <= 0` check (:42-45) returns `STATUS_UNKNOWN_REVISION`. It is unreachable, because every enum value has a case.

`WDF_USB_CONTINUOUS_READER_CONFIG_INIT(&cfg, AmtPtpEvtUsbInterruptPipeReadComplete, ctx /*context*/, transferLength)` sets:

- HeaderLength 0, TrailerLength 0
- **NumPendingReads 0**, meaning the framework default (more than 1). Completions can therefore overlap and arrive out of order.
- BufferAttributes NULL

`cfg.EvtUsbTargetPipeReadersFailed = AmtPtpEvtUsbInterruptReadersFailed`. It returns TRUE, so the framework resets the pipe and restarts.

`WdfUsbTargetPipeConfigContinuousReader(ctx.InterruptPipe, &cfg)`. On failure, return the status. Otherwise return SUCCESS.

The reader is started in D0Entry (`WdfIoTargetStart`, Device.c:550) and stopped in D0Exit (`WdfIoTargetStop(..., WdfIoTargetCancelSentIo)`, Device.c:609-612).

### 6.3 Wellspring-mode control transfers (Device.c:255-496)

Constants (AppleDefinition.h:111-119):

- `BCM5974_WELLSPRING_MODE_READ_REQUEST_ID = 1` (HID GET_REPORT)
- `BCM5974_WELLSPRING_MODE_WRITE_REQUEST_ID = 9` (HID SET_REPORT)

The per-type values come from `USBMSG_TYPEn` = `um_size, um_req_val, um_req_idx, um_switch_idx, um_switch_on, um_switch_off`:

| Type | um_size | wValue (um_req_val) | wIndex (um_req_idx) | switch_idx | on | off |
|---|---|---|---|---|---|---|
| TYPE1/2/3 | 8 | 0x0300 (Feature, ID 0) | 0 | 0 | 0x01 | 0x08 |
| TYPE4 | 2 | 0x0302 (Feature, ID 2) | 2 | 1 | 0x01 | 0x00 |
| TYPE5 | 2 | 0x0302 (Feature, ID 2) | 1 | 1 | 0x01 | 0x00 |

How the setup packet is built:

- `WDF_USB_CONTROL_SETUP_PACKET_INIT` (WDK wdfusb.h:199-219) zeroes the packet and sets Dir, **Type=Standard** and Recipient, plus bRequest, wValue and wIndex.
- It **does not set wLength**. The framework fills wLength from the memory descriptor length.
- The driver then overrides `Type = BmRequestClass` (Device.c:318, :421, :462).

**Bug B1.** The memory descriptor length is `sizeof(DeviceContext->DeviceInfo->um_size)`, which is `sizeof(int)` = **4** (Device.c:305, :408). It is *not* `um_size`. So **wLength on the wire is 4 for every family**:

- TYPE1-3 allocate 8 bytes, of which only 4 are used.
- TYPE4/5 allocate only 2 bytes (`WdfMemoryCreate(..., um_size, ...)`). The 4-byte descriptor then **overruns the 2-byte allocation**: the read can write 4 bytes, and the write sends 2 bytes from past the allocation.

The comments at Device.c:329-330 and :432-433 ("Actual device does not transfer bytes as expected…") are probably a symptom of this.

Literal setup packets as sent (8 bytes, little-endian fields):

| Operation | Type | Bytes |
|---|---|---|
| GET (read state) | TYPE1/2 (TYPE3 sends nothing) | `A1 01 00 03 00 00 04 00` |
| GET | TYPE4 (WS9, T2) | `A1 01 02 03 02 00 04 00` |
| GET | TYPE5 (MT2) | `A1 01 02 03 01 00 04 00` |
| SET (write state) | TYPE1/2 | `21 09 00 03 00 00 04 00` + 4 data bytes |
| SET | TYPE4 | `21 09 02 03 02 00 04 00` + 4 data bytes |
| SET | TYPE5 | `21 09 02 03 01 00 04 00` + 4 data bytes |

`bmRequestType` 0xA1 is IN | Class | Interface; 0x21 is OUT | Class | Interface. The transfers are sent with `WdfUsbTargetDeviceSendControlTransferSynchronously(UsbDevice, WDF_NO_HANDLE, NULL /*no timeout options*/, ...)`. That means a synchronous call with no timeout on the default control pipe.

**wIndex caveat.** TYPE1/2 use wIndex 0 while the INF binds them on `MI_01`. Whether the reflector's native USB path (NativeUSB, not WinUSB) or usbccgp rewrites the interface number for interface-recipient requests is platform behavior. Verify it on hardware before "fixing" it.

#### `AmtPtpGetWellspringMode(ctx, &on)` (Device.c:255-357)

1. If `tp_type == TYPE3`, set `*on = TRUE` and return SUCCESS. There is no I/O (:279-282).
2. `WdfMemoryCreate(NO_ATTR, PagedPool, 'PTPC', um_size, &h, &buf)`. On failure, jump to cleanup.
3. Zero `um_size` bytes. Build the descriptor over `buf` with length 4.
4. Send the GET packet above.
   - On failure, trace (including the uninitialized `cbTransferred`) and jump to cleanup.
   - `cbTransferred` is **never checked**.
5. `*on = (buf[um_switch_idx] == (u8)um_switch_on)` (:344-345).
6. Cleanup: `WdfObjectDelete(h)`. **When `h` is NULL because the allocation failed, this calls `WdfObjectDelete(NULL)` (B5).** Return the status.

#### `AmtPtpSetWellspringMode(ctx, enable)` (Device.c:359-496)

1. If `tp_type == TYPE3`, set `ctx.IsWellspringModeOn = enable` and return SUCCESS. There is no I/O (:382-385).
2. Allocate, zero, and send the GET exactly as above. On failure, jump to cleanup **without changing the flag**.
3. `buf[um_switch_idx] = enable ? um_switch_on : um_switch_off` (:447-449). Every other byte keeps the value the device returned; read-modify-write.
4. Send the SET packet with the same 4-byte descriptor (:452-471). On failure, jump to cleanup without changing the flag.
5. `ctx.IsWellspringModeOn = enable` (:484).
6. Cleanup: `WdfObjectDelete(h)` (B5 applies). Return the status.

#### `AmtPtpEmergResetDevice(ctx)` (Device.c:758-802)

1. `SetWellspringMode(FALSE)`, with errors traced.
2. Then `SetWellspringMode(TRUE)`.
3. Return the status of the **second** call only.

It is called only from the reader completion, on a malformed packet length (7.4 step 1). It does synchronous control I/O **inside the continuous-reader completion callback**. It runs *before* the `IsWellspringModeOn` check, so it **forces Wellspring mode ON even if the host asked for mouse mode** (B11).

### 6.4 Power-transition summary

| Event | Actions (in order) |
|---|---|
| PrepareHardware | Create the USB target once, cache the descriptor, look up config, record traits, select the pipe, configure the reader, set both selective-report flags TRUE. On a second PrepareHardware (PnP stop/start) the reader configuration is expected to fail (B16). |
| D0Entry | If `IsButtonReportOn` or `IsWellspringModeOn`, run SetWellspringMode(TRUE) (failure ignored). Snapshot QPC into `PerfCounter`. Start the pipe target (failure fails D0). |
| D0Exit | Stop the pipe target (cancel sent I/O). SetWellspringMode(FALSE). Return its status. |
| Surprise removal | Framework calls D0Exit. The SET fails, D0Exit returns an error (B9). `SurpriseRemovalOK=TRUE` suppresses the UI. |
| Remove / ReleaseHardware | No driver callback. WDF deletes the objects. |
| Idle / selective suspend / wake | None |

---

## 7. Input parsing

### 7.1 Output: `PTP_REPORT` wire layout (Hid.h:145-162). 50 bytes (computed); identical to input report 0x05.

`PTP_CONTACT` is `#pragma pack(1)` and 9 bytes. `PTP_REPORT` is not packed, but its natural layout has no padding (ScanTime lands at the even offset 46).

| Offset | Size | Field | Value written |
|---|---|---|---|
| 0 | 1 | ReportID | `0x05` |
| 1 + 9·i, i=0..4 | 1 | bit0 `Confidence`, bit1 `TipSwitch`, bits 2-7 `Padding` | Padding is **never written** (uninitialized in C) |
| 2 + 9·i | 4 | `ContactID` (u32 LE) | |
| 6 + 9·i | 2 | `X` (u16 LE) | |
| 8 + 9·i | 2 | `Y` (u16 LE) | |
| 46 | 2 | `ScanTime` (u16 LE) | Units of 100 µs per the descriptor: `55 0c` (exponent -4), `66 01 10` (seconds) |
| 48 | 1 | `ContactCount` | |
| 49 | 1 | `IsButtonClicked` | 0 or 1; the descriptor declares bit 0 = Button 1 and bits 1-7 = constant |

**Uninitialized data (B13).** `PtpReport` is a stack local that is never zeroed. Only these are always assigned: ReportID, IsButtonClicked, ScanTime, and (when the surface is on) ContactCount plus contacts `0..n-1`. So:

- contact slots `n..4` are stack garbage
- the padding bits are garbage
- **with `IsSurfaceReportOn == FALSE`, ContactCount and every contact are garbage**

The Rust port must zero-initialize. This is a deliberate deviation; the bytes that HidClass consumes are unchanged whenever ContactCount is valid.

### 7.2 Raw packet formats (AppleDefinition.h:62-163)

Packets are little-endian (le16 fields).

| Type | Families | Header bytes `tp_header` | Button byte offset `tp_button` | Finger stride `tp_fsize` | Finger offset from header end `tp_delta` | Fingers start at |
|---|---|---|---|---|---|---|
| TYPE1 | WS1, WS2 | 26 (13×u16) | 0 | 28 | 0 | 26 |
| TYPE2 | WS3, WS4, WS4A, WS5, WS5A, WS6, WS6A, WS7, WS7A | 30 (15×u16) | 15 | 28 | 0 | 30 |
| TYPE3 | WS8 | 38 (19×u16) | 23 | 28 | 0 | 38 |
| TYPE4 | WS9, T2 | 46 (23×u16) | 31 | 30 (15×u16) | 2 | 48 |
| TYPE5 | MT2 | 12 (6×u16) | 1 | 9 | 0 | 12 |

`struct TRACKPAD_FINGER` (AppleDefinition.h:128-142) is the finger layout for TYPE1-4. The C type is `__declspec(align(2))`, 28 bytes. All fields are u16 LE, interpreted as **i16** through `AmtRawToInteger` (InputInterrupt.c:591-597: `(signed short)x` widened to int):

| Offset | Field | Used? |
|---|---|---|
| 0 | origin | traced only |
| 2 | abs_x | **yes** |
| 4 | abs_y | **yes** |
| 6 | rel_x | no |
| 8 | rel_y | no |
| 10 | tool_major | no |
| 12 | tool_minor | no |
| 14 | orientation | no |
| 16 | touch_major | **yes** |
| 18 | touch_minor | **yes** |
| 20 | unused[2] | no |
| 24 | pressure | no |
| 26 | multi | no |

**TYPE4 slot framing.** The stride is 30, and the 30-byte slots start at `46 + 30i`, right after the header. The first 2 bytes of each slot are a prefix, which `tp_delta = 2` skips. The 28-byte `TRACKPAD_FINGER` then occupies `48+30i .. 76+30i`, so it ends exactly at the slot end. The code reads only finger bytes `+2..+20` (abs_x through touch_minor), so it never reads past `len`.

**Port hazard.** Do **not** slice `tp_fsize` bytes starting at `h + tp_delta + i*tp_fsize`. For the last TYPE4 finger that range ends 2 bytes past the packet; in the 106-byte frame vector of 7.6, finger 1 starts at 78, and 78+30 = 108 > 106. Instead, slice 28 bytes (or just the 20 bytes used), or index the fields directly. TYPE2/3/5 have `tp_delta = 0`, so a full-stride slice is safe for them.

`struct TRACKPAD_FINGER_TYPE5` (AppleDefinition.h:145-163) is a 9-byte stride. Under MSVC, `align(2)` makes `sizeof` 10, but the code always strides by `tp_fsize=9`. Bytes:

| Byte | Field | Content |
|---|---|---|
| 0 | AbsoluteX | X low 8 bits |
| 1 | AbsoluteXY | bits 0-4 = X high 5 bits; bits 5-7 = Y low 3 bits |
| 2-3 | AbsoluteY | Y bits 3..12 (spread across the u32 view, see 7.6) |
| 4 | TouchMajor | |
| 5 | TouchMinor | |
| 6 | Size | unused |
| 7 | Pressure | unused |
| 8 | union | `Id` = bits 0-3 (MSVC LSB-first bitfield); `Orientation` = bits 4-7 (unused) |

The button-interface struct `TRACKPAD_BUTTON_DATA` (AppleDefinition.h:63-68) is used only for `sizeof` = 4 → `bt_datalen`, which is itself unused.

### 7.3 Device constants used by parsing (`Bcm5974ConfigTable`, AppleDefinition.h:221-418)

Only `x.min` and `y.max` (TYPE2-4), or `x.min` and `y.min` (TYPE5), are used. `p`, `w`, `o`, `x.max` (TYPE2-4) and `y.max` (TYPE5) are unused. The full table is in section 8.1.

### 7.4 Read-completion entry (`AmtPtpEvtUsbInterruptPipeReadComplete`, InputInterrupt.c:82-211)

`len = NumBytesTransferred`, `h = tp_header`, `f = tp_fsize`. `device = WdfObjectContextGetObject(ctx)` is computed but unused.

1. **Length validation** (:108). If `len < h || (len - h) % f != 0`, call `AmtPtpEmergResetDevice(ctx)` (6.3) and **return**. A zero-length packet triggers this.
2. If `!ctx.IsWellspringModeOn`, warn and **return**. The packet is dropped, and pending reads stay queued.
3. Dispatch on `tp_type`:
   - TYPE1: "not yet supported", drop.
   - TYPE2, TYPE3, TYPE4: `AmtPtpServiceTouchInputInterrupt(ctx, WdfMemoryGetBuffer(Buffer, NULL), len)`.
   - TYPE5: `AmtPtpServiceTouchInputInterruptType5(...)`.
   - A failure status is only traced.

### 7.5 Wellspring TYPE2/3/4 (`AmtPtpServiceTouchInputInterrupt`, InputInterrupt.c:228-406)

```text
report.ReportID = 0x05; report.IsButtonClicked = 0                     // :257-258
req = WdfIoQueueRetrieveNextRequest(ctx.InputQueue)                     // :261
  if fail: return (frame DROPPED; no buffering of input)               // :266-273
qpc = QueryPerformanceCounter()                                         // :275
delta: i64 = (qpc - ctx.PerfCounter) / 100      // C signed division, truncates toward 0
if delta > 0xFF { delta = 0xFF }                // comment says "two bytes" but clamps to 0xFF (B12)
report.ScanTime = delta as u16                                          // :287
mem = WdfRequestRetrieveOutputMemory(req)
  if fail: return status   // REQUEST LEAKED: retrieved but never completed (B6)  :290-303
if ctx.IsSurfaceReportOn {                                              // :306
    n = (len - h) / f;  if n >= 5 { n = 5 }                             // :308-309
    report.ContactCount = n as u8
    for i in 0..n {
        base = h + tp_delta + i*f
        ax  = i16::from_le(buf[base+2 .. base+4])  as i32
        ay  = i16::from_le(buf[base+4 .. base+6])  as i32
        maj = i16::from_le(buf[base+16.. base+18]) as i32
        min = i16::from_le(buf[base+18.. base+20]) as i32
        X = if ax - x_min > 0 { (ax - x_min) as u16 } else { 0 }      // :328-329  (no upper clamp)
        Y = if y_max - ay > 0 { (y_max - ay) as u16 } else { 0 }      // :330-331  (Y inverted; no upper clamp)
        contact[i].ContactID = i as u32                                 // :335  slot index, NOT a tracking ID
        contact[i].TipSwitch  = (maj << 1) >= 200    // i.e. maj >= 100   :338
        contact[i].Confidence = (min << 1) > 0       // i.e. min > 0      :339
    }
}
if ctx.IsButtonReportOn && buf[tp_button] != 0 { report.IsButtonClicked = 1 }   // :361-366
st = WdfMemoryCopyFromBuffer(mem, 0, &report, 50)
  if fail (output buffer < 50): return st   // REQUEST LEAKED (B6)        :376-384
WdfRequestSetInformation(req, 50); WdfRequestComplete(req, STATUS_SUCCESS)    // :387-396
```

Arithmetic notes:

- All arithmetic is `int`. `(ax - x_min)` is at most 32767+4828 = 37595, so the cast to u16 never wraps. Likewise `y_max - ay` is at most 6803+32768 = 39571.
- Values above the descriptor's logical maximum are passed through unchanged; there is no clamping.
- `maj << 1` on a negative int is UB in C. MSVC gives two's complement, which is just `2*maj`. Use `maj * 2` in Rust; with i16 inputs it cannot overflow i32.
- `buf[tp_button]` lies inside the header, which step 1 guarantees is present.

### 7.6 Magic Trackpad 2 TYPE5 (`AmtPtpServiceTouchInputInterruptType5`, InputInterrupt.c:408-589)

The flow is the same as 7.5, with these differences:

- The output memory is retrieved *before* QPC (:456-482). This has no observable effect.
- The finger decode is different (:500-541):

```text
base = 12 + i*9                                  // tp_delta = 0
le16 = u16::from_le(buf[base..base+2])           // C: *(USHORT*)f  (unaligned for odd i)
le32 = u32::from_le(buf[base..base+4])           // C: *(UINT*)f    (unaligned)
// :506-510, EXACT C semantics:
x: i32 = (((le16 & 0x1fff) << 3) as u16 as i16 as i32) >> 3     // 13-bit sign extend of bits 0..12
y: i32 = (((le32 << 6) as i32).wrapping_neg()) >> 19            // NEGATE FIRST, then arithmetic >> 19
x = if x - x_min > 0 { x - x_min } else { 0 }    // x_min = -3678   :512
y = if y - y_min > 0 { y - y_min } else { 0 }    // y_min = -2479   :513
contact[i].ContactID  = (buf[base+8] & 0x0F) as u32                    // :515  device-supplied ID
contact[i].X = x as u16; contact[i].Y = y as u16                         // :516-517
contact[i].TipSwitch  = (buf[base+4] as i32 * 2) > 0      // TouchMajor != 0          :518
contact[i].Confidence = (buf[base+5] as i32 * 2) < 345    // TouchMinor <= 172        :523-524
button: buf[1] != 0
```

**Do not "simplify" the Y decode.** Because negation comes before the shift:

- Let `F` be the signed 13-bit field at bits 13..25 of `le32`.
- Let `L = le32 & 0x1FFF`, the raw X bits.
- Then `y = -F - (L != 0 ? 1 : 0)`, **except** when `F == -4096 && L == 0`.

So Y is off by one whenever the raw X field is nonzero.

**The exception.** When `F == -4096 && L == 0`, `le32 << 6 == 0x80000000`. Negating that is INT_MIN negation: UB in C, and MSVC/gcc wrap. The literal expression therefore yields **y = -4096**, while the closed form yields +4096. After the y_min offset this is Y = 0 (clamped) versus Y = 6575, a behavioral difference (see the 7th golden row).

Implement the literal expression with `wrapping_neg`, not the closed form. AmtRawToInteger on a u8 is just zero-extension.

**B17.** Confidence has a duplicated condition, `TouchMinor<<1 < 345 && TouchMinor<<1 < 345` (:523-524). The intent was probably TouchMajor plus TouchMinor. The comment mentions the "25mm" spec.

Golden vectors (computed by compiling the exact C expressions with gcc; MSVC semantics match). Input is the 9 finger bytes; x_min=-3678, y_min=-2479:

| Finger bytes 0..8 | x (pre-offset) | y (pre-offset) | X | Y | ID | Tip | Conf |
|---|---|---|---|---|---|---|---|
| `00 00 00 00 10 20 00 00 13` | 0 | 0 | 3678 | 2479 | 3 | 1 | 1 |
| `01 00 00 00 10 ac 00 00 02` | 1 | **-1** | 3679 | 2478 | 2 | 1 | 1 (0xAC = 172) |
| `34 12 56 00 00 ad 00 00 5f` | -3532 | -689 | 146 | 1790 | 15 | 0 | 0 (0xAD = 173) |
| `ff ff ff 03 30 40 00 00 01` | -1 | 0 | 3677 | 2479 | 1 | 1 | 1 |
| `62 f1 9a 02 20 30 00 00 04` | -3742 | 2856 | **0** (clamped) | 5335 | 4 | 1 | 1 |
| `00 e0 ff 01 01 01 00 00 00` | 0 | -4095 | 3678 | **0** (clamped) | 0 | 1 | 1 |
| `00 00 00 02 01 01 00 00 00` | 0 | **-4096** (INT_MIN wrap; closed form would give +4096) | 3678 | **0** (clamped) | 0 | 1 | 1 |

TYPE2-4 golden vectors (WS3: x_min=-4460, y_max=6700):

| abs_x | abs_y | touch_major | touch_minor | X | Y | Tip | Conf |
|---|---|---|---|---|---|---|---|
| 0 | 0 | 100 | 1 | 4460 | 6700 | 1 | 1 |
| -4460 | 6700 | 99 | 0 | 0 | 0 | 0 | 0 |
| -5000 | 7000 | -5 | -1 | 0 | 0 | 0 | 0 |
| 5166 | -75 | 150 | 30 | 9626 | 6775 | 1 | 1 |
| 32767 | -32768 | 16384 | 2 | 37227 | 39468 | 1 | 1 |

**Frame-level golden vector, TYPE4 / WS9** (computed with the real `TRACKPAD_FINGER` and `PTP_REPORT` structs from the headers, in `hidcalc_usbum/frame.c`). This checks the `h + tp_delta + i*fsize` offsets, the button byte and the 50-byte serialization.

- Config: x_min=-4828, y_max=6803.
- The packet length is 106 = 46 + 2×30. Finger 0 starts at byte 48, finger 1 at byte 78, and the button byte is 31.
- `PtpReport` was zeroed, per B13, and ScanTime was forced to 0xFF.

Raw packet (all other bytes 0):

| Byte(s) | Value | Meaning |
|---|---|---|
| [31] | 01 | button |
| [50..51] | 64 00 | f0.abs_x = 100 |
| [52..53] | c8 00 | f0.abs_y = 200 |
| [64..65] | 78 00 | f0.touch_major = 120 |
| [66..67] | 28 00 | f0.touch_minor = 40 |
| [80..81] | 78 ec | f1.abs_x = -5000 |
| [82..83] | 58 1b | f1.abs_y = 7000 |
| [94..95] | 32 00 | f1.touch_major = 50 |
| [96..97] | 00 00 | f1.touch_minor = 0 |

Output report (50 bytes):
```
05 03 00 00 00 00 40 13 cb 19 00 01 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 ff 00 02 01
```

Reading the output:

| Bytes | Meaning |
|---|---|
| 0 | ReportID 0x05 |
| 1..9 | Contact 0: flags 0x03 (Conf=1, Tip=1), ID 0, X=4928 (`40 13`), Y=6603 (`cb 19`) |
| 10..18 | Contact 1: flags 0x00, ID 1, X=0, Y=0 (both clamped) |
| 19..45 | Slots 2-4, zero |
| 46..47 | ScanTime `ff 00` |
| 48 | ContactCount 2 |
| 49 | Button 1 |

### 7.7 Behaviors that do NOT exist

Port these as non-features unless you decide otherwise:

- **No contact tracking.** TYPE2-4 IDs are slot indices 0..4, so the ID of a given finger changes when an earlier slot lifts.
- **No synthesized lift-off.** A slot that the device still sends but whose `touch_major` is under 100 (or MT2 `TouchMajor == 0`) is reported with Tip=0. A finger that vanishes from the packet is simply absent from the next frame.
- **No palm rejection** beyond the Confidence rules above.
- **No pressure** use, and no defuzz (TODO at InputInterrupt.c:334).
- **No use of the qualification levels** (B15).
- **No per-frame state.** The only state across reports is `PerfCounter`, which is set at D0Entry and never advanced.
- **No synthesis of mouse reports** in mouse mode.
- **No queueing of frames** when no read request is pending; the frame is dropped.
- **Contact count:** at most 5. Extra fingers are truncated by slot order and are not reported as excess.

**ScanTime (B12).** The value is `min((QPC_now - QPC_at_D0Entry)/100, 255)`. The QPC frequency is never queried, so the unit depends on the platform. Within a few milliseconds of D0Entry it saturates at **255 and stays there**. The PTP host therefore sees a constant scan time. The MS spec instead requires a free-running 100 µs counter that rolls over at 0xFFFF (see B12).

---

## 8. Device support tables

### 8.1 `Bcm5974ConfigTable` (AppleDefinition.h:221-418), as built (computed from the compiled table)

Columns that are identical for every row:

- `bt_datalen = 4` (`sizeof(TRACKPAD_BUTTON_DATA)`)
- `p = {45, 0, 300}`, except WS1/WS2, which use `{45, 0, 256}`
- `w = {25, 0, 2048}`
- `o = {10, -16384, 16384}`
- every x/y `snratio = 250`

`p`, `w`, `o`, `caps`, `bt_ep`, `bt_datalen`, `tp_ep`, `tp_datalen` and the snratios are **never read** by driver logic.

| Idx | Family (comment) | ansi / iso / jis | caps | bt_ep | tp_ep | Type | hdr | datalen | button | fsize | delta | um (size,val,idx,sw_idx,on,off) | x.min..x.max | y.min..y.max |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 0 | WS1 MacBookAir | 0x0223/0x0224/0x0225 | 0 | 0x84 | 0x81 | TYPE1 | 26 | 474 | 0 | 28 | 0 | 8,0x300,0,0,1,8 | -4824..5342 | -172..5820 |
| 1 | WS2 MacBookPro Penryn | 0x0230/0x0231/0x0232 | 0 | 0x84 | 0x81 | TYPE1 | 26 | 474 | 0 | 28 | 0 | 8,0x300,0,0,1,8 | -4824..4824 | -172..4290 |
| 2 | WS3 MacBook5,1 | 0x0236/0x0237/0x0238 | 1 | 0x84 | 0x81 | TYPE2 | 30 | 478 | 15 | 28 | 0 | 8,0x300,0,0,1,8 | -4460..5166 | -75..6700 |
| 3 | WS4 MacBookAir3,2 | 0x023f/0x0240/0x0241 | 1 | 0x84 | 0x81 | TYPE2 | 30 | 478 | 15 | 28 | 0 | 8,0x300,0,0,1,8 | -4620..5140 | -150..6600 |
| 4 | WS4A MacBookAir3,1 | 0x0242/0x0243/0x0244 | 1 | 0x84 | 0x81 | TYPE2 | 30 | 478 | 15 | 28 | 0 | 8,0x300,0,0,1,8 | -4616..5112 | -142..5234 |
| 5 | WS5 MacBook8 | 0x0245/0x0246/0x0247 | 1 | 0x84 | 0x81 | TYPE2 | 30 | 478 | 15 | 28 | 0 | 8,0x300,0,0,1,8 | -4415..5050 | -55..6680 |
| 6 | WS6 MacBookAir4,2 | 0x024c/0x024d/0x024e | 1 | 0x84 | 0x81 | TYPE2 | 30 | 478 | 15 | 28 | 0 | 8,0x300,0,0,1,8 | -4620..5140 | -150..6600 |
| 7 | WS5A MacBook8,2 | 0x0252/0x0253/0x0254 | 1 | 0x84 | 0x81 | TYPE2 | 30 | 478 | 15 | 28 | 0 | 8,0x300,0,0,1,8 | -4750..5280 | -150..6730 |
| 8 | WS6A MacBookAir4,1 | 0x0249/0x024a/0x024b | 1 | 0x84 | 0x81 | TYPE2 | 30 | 478 | 15 | 28 | 0 | 8,0x300,0,0,1,8 | -4620..5140 | -150..6600 |
| 9 | WS7 MacBookPro10,1 | 0x0262/0x0263/0x0264 | 1 | 0x84 | 0x81 | TYPE2 | 30 | 478 | 15 | 28 | 0 | 8,0x300,0,0,1,8 | -4750..5280 | -150..6730 |
| 10 | WS7A MacBookPro10,2 | 0x0259/0x025a/0x025b | 1 | 0x84 | 0x81 | TYPE2 | 30 | 478 | 15 | 28 | 0 | 8,0x300,0,0,1,8 | -4750..5280 | -150..6730 |
| 11 | WS8 MacBookAir6,2 | 0x0290/0x0291/0x0292 | 1 | 0 | 0x83 | TYPE3 | 38 | 486 | 23 | 28 | 0 | 8,0x300,0,0,1,8 (unused, no I/O for TYPE3) | -4620..5140 | -150..6600 |
| 12 | WS9 MacBookPro12,1 | 0x0272/0x0273/0x0274 | 1 | 0 | 0x83 | TYPE4 | 46 | 526 | 31 | 30 | 2 | 2,0x302,2,1,1,0 | -4828..5345 | -203..6803 |
| 13 | Magic Trackpad 2 | 0x0265 ×3 | 1 | 0 | 0x83 | TYPE5 | 12 | 156 | 1 | 9 | 0 | 2,0x302,1,1,1,0 | -3678..3934 | -2479..2586 |
| 14 | Apple T2 | 0x027d ×3 | 1 | 0 | 0x83 | TYPE4 | 46 | 526 | 31 | 30 | 2 | 2,0x302,2,1,1,0 | -4828..5345 | -203..6803 (comment AppleDefinition.h:412: "This is incorrect; check actual data later") |

The array has **15 rows and no `{0}` sentinel**, yet the lookup loop stops on `ansi == 0` (B3).

The PID macros are at AppleDefinition.h:3-60. `USB_VENDOR_ID_APPLE 0x05ac` is defined but unused, so the vendor is never checked. Enum ordinals are TYPE1=0 … TYPE5=4 (AppleDefinition.h:71-77). The macro `DATAFORMAT(type)` (AppleDefinition.h:198-205) expands to `type, HEADER_type, HEADER_type + 16*FSIZE_type, BUTTON_type, FSIZE_type, DELTA_type, USBMSG_type` (6 values).

### 8.2 Hardware IDs bound to this binary (INF)
| INF line | Hardware ID |
|---|---|
| 47-49 | `USB\Vid_05ac&Pid_0236/0237/0238&MI_01` |
| 50-52 | `...Pid_0245/0246/0247&MI_01` |
| 53-55 | `...Pid_0249/024a/024b&MI_01` |
| 56-58 | `...Pid_024c/024d/024e&MI_01` |
| 59-61 | `...Pid_0252/0253/0254&MI_01` |
| 62-64 | `...Pid_0259/025a/025b&MI_01` |
| 65-67 | `...Pid_0262/0263/0264&MI_01` |
| 68-70 | `...Pid_0272/0273/0274&MI_02` |
| 71-72 | `...Pid_0290/0291&MI_02` |
| 88 | `USB\Vid_05ac&Pid_0265&MI_01` (comment INF:87: "last few versions where Magic Trackpad 2 uses this old implementation") |

**Conflict.** `Pid_0273&MI_02` and `Pid_0274&MI_02` (INF:32-33), and `Pid_0290&MI_02` and `Pid_0291&MI_02` (INF:42-43), are *also* bound to `AmtPtpDeviceUsbKm_Device`. These are identical hardware IDs in the same INF, so which driver wins is decided by Windows ranking or section order, not by intent (B20).

### 8.3 PID → HID descriptor (Hid.c:42-137 and 280-339, identical switches)
| PIDs | Descriptor |
|---|---|
| 0x0236-0x0238 (WS3) | `AmtPtp3*` |
| 0x0245-0x0247 (WS5), 0x0252-0x0254 (WS5A) | `AmtPtp5*` |
| 0x024c-0x024e (WS6), 0x0249-0x024b (WS6A) | `AmtPtp6*` |
| 0x0262-0x0264 (WS7), 0x0259-0x025b (WS7A) | `AmtPtp7a*` |
| 0x0290-0x0292 (WS8), 0x0272-0x0274 (WS9) | `AmtPtp8*` |
| 0x0265 (MT2) | `AmtPtpMt2*` |
| anything else | `STATUS_INVALID_DEVICE_STATE` |

### 8.4 Reconciliation (effective status)

Range checks for TYPE2-4: output X spans `0..x.max-x.min` and output Y spans `0..y.max-y.min`. For MT2, Y spans `0..y.max-y.min`. These are compared with the descriptor's logical maximums.

| PID(s) | Table row / type | HID descriptor | INF | Effective status |
|---|---|---|---|---|
| 0223-0225 WS1, 0230-0232 WS2 | 0, 1 / TYPE1 | none | not bound | Non-functional. GET_DEVICE_DESCRIPTOR fails, and input is "not supported" anyway (InputInterrupt.c:145-153). |
| 0236-0238 WS3 | 2 / TYPE2 | AmtPtp3 (9626 × 6775) | MI_01 | OK. Range 9626 × 6775 matches. |
| 023f-0241 WS4, 0242-0244 WS4A | 3, 4 / TYPE2 | **none** | **not bound** | Non-functional. Only reachable by forced install. |
| 0245-0247 WS5 | 5 / TYPE2 | AmtPtp5 (9465 × 6735) | MI_01 | OK, matches. |
| 0252-0254 WS5A | 7 / TYPE2 | AmtPtp5 (9465 × 6735) | MI_01 | **Range mismatch**: output up to 10030 × 6880 exceeds the logical maximum (B18). |
| 024c-024e WS6, 0249-024b WS6A | 6, 8 / TYPE2 | AmtPtp6 (9760 × 6750) | MI_01 | OK, matches. |
| 0262-0264 WS7, 0259-025b WS7A | 9, 10 / TYPE2 | AmtPtp7a (10030 × 6880) | MI_01 | OK, matches. |
| 0290-0291 WS8 | 11 / TYPE3 | AmtPtp8 (9760 × 6750) | MI_02, **duplicated with UsbKm** | OK if UsbUm wins. No mode I/O. B4 applies. |
| 0292 WS8 JIS | 11 / TYPE3 | AmtPtp8 | **not bound** | Would work if force-installed. |
| 0272 WS9 | 12 / TYPE4 | AmtPtp8 (9760 × 6750) | MI_02 | **Range mismatch**: up to 10173 × 7006 (B18). |
| 0273-0274 WS9 | 12 / TYPE4 | AmtPtp8 | MI_02, **duplicated with UsbKm** | As 0272, plus the binding ambiguity. |
| 0265 MT2 | 13 / TYPE5 | AmtPtpMt2 (7612 × 5065) | MI_01 | OK, matches (3934+3678 = 7612; 2586+2479 = 5065). |
| 027d T2 | 14 / TYPE4 | **none** | bound only to UsbKm (INF:38) | Non-functional under this driver. |

---

## 9. Registry, configuration, user-mode interfaces

- **Registry reads: none.** There are no `WdfDriverOpenParametersRegistryKey`, `WdfDeviceOpenRegistryKey` or `WdfFdoInitQueryProperty` calls. The INF writes only `FriendlyName` and `LowerFilters` (INF:202-204).
- **Persistent settings: none.**
  - The qualification levels are 0 only when the WDFDEVICE (and its zeroed context) is created in EvtDeviceAdd. Nothing resets them at PrepareHardware or D0Entry, so they **survive** PnP stop/start and D-state transitions. They are lost when the device is removed or the driver unloads.
  - The selective-report flags reset to TRUE at every PrepareHardware.
  - `IsWellspringModeOn` is not reset at PrepareHardware either. It carries across a stop/start and changes only through SetWellspringMode.
- **Device interface.** `GUID_DEVINTERFACE_AmtPtpDevice` `{4a5064e5-7d39-41d1-a0e4-81097edce967}` is registered (Device.c:98-102), with no reference string. No driver-defined IOCTLs exist for it.
- **User-mode configuration channel: HID feature report 0x09** on the vendor TLC (Usage Page 0xFF00, Usage 0x01; Hid.h:22-33). The 4-byte layout is `[0x09, PressureQualificationLevel, SingleContactSizeQualificationLevel, MultipleContactSizeQualificationLevel]` (`PTP_USERMODEAPP_CONF_REPORT`, Hid.h:164-169). GET and SET are described in 5.5 and 5.6. Values are stored and echoed back but have **no effect** (B15).
- **Consumer**: `src/AmtPtpDevice.Settings` (UWP).
  - `MainPage.xaml.cs:58` selects `HidDevice.GetDeviceSelector(0xff00, 0x0001, 0x05ac, 0x0265)`.
  - It GETs 0x09 (:95) and SETs `[0x09, p, s, m]` (:126-134).
  - This driver reports **VendorID 0x8910** (Hid.c:227). The selector therefore **does not match** devices served by this driver; it presumably targets the Bluetooth filter path.
  - The app also reads battery input report **0x90** on usage 0xFF00/0x0014 (:59, :68). This driver does **not** declare or implement it.
- **Unused constants**: `PRESSURE_QUALIFICATION_THRESHOLD 2`, `SIZE_QUALIFICATION_THRESHOLD 9`, `SIZE_MU_LOWER_THRESHOLD 5`, `PRESSURE_MU_QUALIFICATION_THRESHOLD_TOTAL 15` and `SIZE_MU_QUALIFICATION_THRESHOLD_TOTAL 25` (AppleDefinition.h:213-218) are never referenced in this driver. They suggest an intended qualification algorithm that was never implemented here.

---

## 10. Error paths, edge cases, magic numbers, TODOs, dead code, bugs

### 10.1 Bugs and suspicious behavior

For each bug, an identical port must do what "Current" says. The port decision is the engineer's call unless stated otherwise.

**B1. Control-transfer length is `sizeof(int)`.** (Device.c:302-306, :405-409)
- Current:
  - wLength is 4 for every family.
  - TYPE4/5 have a 2-byte allocation, which the 4-byte transfer overruns.
  - The write echoes bytes the device returned. For TYPE4/5, bytes 2-3 come from past the allocation unless the device returned 4 bytes.
  - `cbTransferred` is never checked.
- Probable intent: `um_size` (8 or 2), as in Linux bcm5974.
- Port: a 4-byte wLength with a 4-byte buffer reproduces the wire behavior without the overrun. Test on hardware before switching to `um_size`.

**B2. HQA blob is not returned.** (Hid.c:822)
- Current: the comma operator writes only `report[1] = 0xFC`. Bytes 2..256 are untouched, and Information is 257.
- Probable intent: copy all 256 bytes of `DEFAULT_PTP_HQA_BLOB`.
- Port: either is defensible. Exact parity requires leaving bytes 2..256 as received from the caller.

**B3. Config table has no sentinel.** (Device.c:15; AppleDefinition.h:221-418)
- Current: for an unknown PID, the loop reads past the 15-row array. That is UB: it may crash, falsely match, or terminate on a stray zero.
- Probable intent: return "not found".
- Port: bounded iteration. This is the only sane option.

**B4. TYPE3 (WS8) gets stuck OFF.** (Device.c:279-282, 382-385; Hid.c:964-1027; InputInterrupt.c:132)
- Current:
  - Input mode 0 sets the flag FALSE through Set(FALSE).
  - A later mode 3 sees Get() == TRUE, does nothing, and the flag stays FALSE. All input is dropped until a D0 cycle.
  - D0Entry re-enables only if `IsButtonReportOn` is set.
- Probable intent: track the flag.
- Port decision needed.

**B5. `WdfObjectDelete(NULL)`.** (Device.c:354, :493)
- Current: reached when `WdfMemoryCreate` fails. A WDF verifier break or crash.
- Port: guard it.

**B6. Leaked read requests.** (InputInterrupt.c:295-303, 376-384, 460-468, 559-567)
- Current: after `WdfIoQueueRetrieveNextRequest` succeeds, a failure in `RetrieveOutputMemory` or `CopyFromBuffer` jumps to exit without completing the request. One trigger is a read buffer under 50 bytes. The request is orphaned and hangs device removal.
- Probable intent: complete it with an error.
- Port: completing with the error status is recommended.

**B7. GET_STRING is broken.** (Hid.c:478-484, 505-532)
- Current:
  - It copies into `&pStringBuffer` (stack) instead of the output buffer.
  - It reports Information = full output length with SUCCESS.
  - An unknown string ID returns SUCCESS with 0 bytes.
  - `memHandle` leaks.
- Probable intent: copy `wcharCount*2` bytes into the output buffer.
- Port: identical behavior is memory corruption. Implement the intent and document the deviation.

**B8. SET_FEATURE always returns success, with no length checks.** (Hid.c:1128; reads at :964, :1051-1052, :1084-1086)
- Current: every error, including unsupported IDs, returns SUCCESS. It reads `buf[1..3]` without bounds checks.
- Port: Rust must bounds-check. Choose a behavior for short buffers, for example treating the report as unsupported while still returning SUCCESS for parity.

**B9. D0Exit returns the mode-switch status.** (Device.c:621-641)
- Current: fails on surprise removal and USB errors, which the framework treats as a device failure.
- Probable intent: best-effort, return SUCCESS.

**B10. D0Entry gates mode on `IsButtonReportOn`.** (Device.c:519)
- Current: if the host turned off bit 0 (B14: that is really Surface) and a D0 cycle follows, Wellspring mode is not re-enabled. When D0Exit's Set(FALSE) succeeded and cleared the flag, input stops until a mode-3 SET_FEATURE arrives. When it failed, the flag stays TRUE and D0Entry re-enables.
- Probable intent: unconditional enable, or `IsWellspringModeOn`-before-D0Exit.

**B11. Emergency reset runs inside the reader callback and overrides the host's mode.** (InputInterrupt.c:108-130)
- Current:
  - Any packet whose length is not `header + k*fsize` triggers Set(FALSE) then Set(TRUE), as synchronous control I/O inside the completion.
  - This happens even when the host selected mouse mode, and even for TYPE3, where it only toggles the flag.
  - In mouse mode, the device's mouse-format packets are likely malformed by this rule, so the driver forces PTP mode back on.
- Port decision needed. Consider moving it to a work item.

**B12. ScanTime is effectively constant.** (Device.c:542-544; InputInterrupt.c:280-287, 475-482)
- Current: the base is set once per D0Entry. The QPC delta is divided by 100 without knowing the frequency, then clamped to 255 (not 65535). It saturates within milliseconds.
- Probable intent (MS PTP spec, "Scan Time"): a free-running relative digitizer time in 100 µs units that rolls over at 16 bits. The host uses the deltas between frames. For example: `((QPC_now - base) * 10_000 / QueryPerformanceFrequency) as u16`, with wrapping. It should not be a per-frame delta, and it should not be clamped.

**B13. `PtpReport` is uninitialized.** (InputInterrupt.c:239, :420)
- Current: stack garbage in unused contact slots and padding bits. With surface reporting off, the ContactCount and all contacts are garbage.
- Port: **zero-initialize** (deliberate deviation).

**B14. Selective-reporting bits are swapped.** (HidCommon.h:11-12; Hid.h:137-142; Hid.c:1051-1052)
- Current: bit 0 (HID usage 0x57, Surface Switch) is stored as ButtonReport, and bit 1 (0x58, Button Switch) as SurfaceReport.
- Probable intent: per the HID Usage Tables.
- Port decision needed. It is invisible when both bits are equal.

**B15. Qualification levels are dead state.** (Hid.c:860-862, 1084-1086)
- Current: stored and echoed back, never applied.

**B16. No `EvtDeviceReleaseHardware`.** (Device.c:148, :714; InputInterrupt.c:57)
- Current:
  - On PnP stop/start, `InterruptPipe` keeps its stale handle, so the NULL check is meaningless.
  - The continuous reader is reconfigured on a pipe that already has one. The configuration is never reselected, so the pipe object survives. Open-source WDF `FxUsbPipe::InitContinuousReader` (`src/framework/shared/targets/usb/fxusbpipe.cpp`) returns `STATUS_INVALID_DEVICE_STATE` when `m_Reader != NULL`. **PrepareHardware is therefore expected to fail on restart, and the device stays stopped (yellow bang) after a PnP stop/start or rebalance.** This has not been verified on hardware.
  - `UsbDevice` is reused.
- Port: configure the reader **once**, for example behind a "reader configured" flag in the context, and skip it on later PrepareHardware calls. Merely adding a ReleaseHardware does not help, because nothing short of a SelectConfig un-configures a pipe's reader.

**B17. Duplicated MT2 Confidence term.** (InputInterrupt.c:523-524)
- Current: `TouchMinor*2 < 345` twice.
- Probable intent: Major and Minor.

**B18. Logical-range mismatches and no upper clamp.** (Hid.c:57-73 and 108-124 versus AppleDefinition.h:313-325 and 378-390)
- Current: WS5A uses the WS5 descriptor and WS9 uses the WS8 descriptor, so the output exceeds the logical maximum. TYPE2-4 X/Y can reach 37595/39571 on extreme raw values; there is no clamp.
- MT2 (TYPE5) has no upper clamp either. X is at most 4095+3678 = 7773, above the logical maximum 7612. Y is at most 4095+2479 = 6574, above the logical maximum 5065.
- Port decision: keep parity, or clamp.

**B19. Undeclared `Driver` identifier.** (Driver.c:105)
- Current: compiles only because UMDF `WPP_CLEANUP` drops its argument.
- Port: not applicable in Rust; just unregister tracing.

**B20. Packaging inconsistencies.** (INF:32-33, 42-43 vs 69-72; Hid.c:227 vs Settings MainPage.xaml.cs:58; Resource.rc OriginalFilename/FILETYPE)
- Duplicate hardware IDs with UsbKm.
- VID 0x8910 means the Settings app cannot find the device.
- The resource names `AmtPtpDevice.dll` and declares FILETYPE APP.

**B21. Pipe selection ignores direction and `tp_ep`.** (Device.c:688-709)
- Current: takes the first interrupt pipe of interface 0.
- Port: keep, but assert it is IN.

**B22. No synchronization.** (whole driver)
- Current:
  - SET_FEATURE (parallel queue) and EmergReset (reader callback, with multiple pending reads) can run SetWellspringMode concurrently, interleaving read-modify-write control transfers.
  - Because the default queue is not power-managed, SET_FEATURE can do control I/O while the device is in Dx.
  - BOOL flags are read and written non-atomically.
- Port: Rust must use atomics or a lock anyway. A per-device mutex around mode switching is recommended.

**B23. Vendor ID is not checked in the config lookup.** (Device.c:12-21)
- Current: matches on PID only. Mitigated by the INF binding to VID 05AC.

**B24. TYPE1 is unsupported.** (InputInterrupt.c:145-153)
- Current: WS1/WS2 have config rows, but input is dropped and reads never complete. There is also no descriptor and no INF binding.

### 10.2 Other edge cases and error paths
- **Read with no pending request.** The frame is dropped silently: `WdfIoQueueRetrieveNextRequest` fails, typically with `STATUS_NO_MORE_ENTRIES`, and the status is only traced.
- **Read pending forever.** This happens when the mode is off, the type is TYPE1, or the packets are malformed (each malformed packet triggers a reset). The request is completed only by framework cancellation or purge.
- **`len == header` exactly.** This is a valid frame: 0 contacts, ContactCount 0, button still evaluated.
- **More than 5 finger slots.** Truncated to the first 5. A frame can carry up to 16 slots (the transfer length).
- **Device state vs. flag.** `IsWellspringModeOn` changes only after a successful SET (or unconditionally for TYPE3). Mode-switch failures leave the flag unchanged and are otherwise ignored, except in D0Exit.
- **Unknown input-mode values** (not 0 or 3): accepted, no action, Information 2.
- **PrepareHardware failure** (unknown PID, no interrupt pipe, reader config failure): the device fails to start.
- **D0Entry failure**: only when `WdfIoTargetStart` fails.
- **Status codes used**:
  - `STATUS_INVALID_DEVICE_STATE`: unknown PID, no interrupt pipe, no descriptor
  - `STATUS_UNSUCCESSFUL`: no interface 0
  - `STATUS_UNKNOWN_REVISION`: unreachable
  - `STATUS_INVALID_BUFFER_SIZE`: short GET_STRING input, short feature input/output
  - `STATUS_NOT_SUPPORTED`: unknown IOCTL, unknown GET_FEATURE ID
  - pass-through WDF statuses

### 10.3 Dead or unused code (safe to drop in the port)
- Device.c:171: `if (NT_SUCCESS(status))` is always true.
- Device.c:216-223 and :228-230: `waitWakeEnable`, `UsbDeviceTraits` (write-only).
- Device.c:662, :679-680: `configParams` (select-config is never called).
- Device.c:565-576: D0Entry stop-on-failure block is unreachable.
- Hid.c:373-388: unreachable `else` in GetReportDescriptor.
- InputInterrupt.c:42-45: `transferLength <= 0` is unreachable.
- InputInterrupt.c:104: `device` is unused.
- InputInterrupt.c:312-319, 341-356, 490-497, 526-540: `INPUT_CONTENT_TRACE` blocks are never compiled.
- InputInterrupt.c:145-153: the TYPE1 branch is effectively dead (no descriptor, no INF).
- Queue.c:215-258: `EvtIoStop` is a no-op. It could be omitted, or kept as a no-op for parity.
- Queue.h:9-15: `QUEUE_CONTEXT` / `QueueGetContext`.
- Hid.h:98, 103-104, 106-118: `PTP_BUTTON_TYPE_PRESSURE_PAD`, the contact bit constants, `HID_AAPL_MOUSE_REPORT`, `HID_INPUT_REPORT`.
- HidCommon.h:3, 17-18, 23: `REPORTID_STANDARDMOUSE`, `USAGE_MINIMUM`, `USAGE_MAXIMUM`, `PHYSICAL_MINIMUM`.
- AppleDefinition.h:
  - :3 `USB_VENDOR_ID_APPLE`
  - :94 `HAS_INTEGRATED_BUTTON` (only stored in `caps`)
  - :125 `BCM5974_MOUSE_SIZE`
  - :208-211 SN_* (table only)
  - :213-218 qualification thresholds
  - config fields `caps`, `bt_ep`, `bt_datalen`, `tp_ep`, `tp_datalen`, `p`, `w`, `o`, `snratio`
- Trace.h:45: WPP `Trace{FLAG=MYDRIVER_ALL_INFO}` names a non-existent flag.
- ModernTrace.h:22-23: unused event names.
- `DbgDevicePowerString` (Device.c:729-756) and `DbgIoControlGetString` (Queue.c:77-116): trace-only.

### 10.4 Magic numbers
| Value | Meaning | Cite |
|---|---|---|
| 0x8910 | Fake HID VendorID | Hid.h:20 |
| 0x0001 | HID VersionNumber | Hid.h:19 |
| 5 | Max reported contacts and feature 0x07 value | Hid.h:96, InputInterrupt.c:309/487 |
| 0 | Button type (click pad) | Hid.h:97 |
| 16 | `MAX_FINGERS`: transfer sizing; Input Mode logical maximum in the descriptor | AppleDefinition.h:122; Hid.h:44 |
| 200 (=2×100) | Wellspring TipSwitch threshold on touch_major | InputInterrupt.c:338 |
| 0 | Wellspring Confidence threshold on touch_minor (> 0) | InputInterrupt.c:339 |
| 345 (=2×172.5) | MT2 Confidence threshold on TouchMinor | InputInterrupt.c:523 |
| 0x1fff, <<3, >>3, <<6, >>19 | MT2 coordinate bitfield extraction | InputInterrupt.c:506-510 |
| /100, 0xFF | ScanTime scaling and clamp | InputInterrupt.c:280-284, 475-479 |
| 1 / 9 | HID GET_REPORT / SET_REPORT bRequest | AppleDefinition.h:118-119 |
| 0x0300 / 0x0302 | wValue (Feature report ID 0 / 2) | AppleDefinition.h:111-115 |
| 0x01 / 0x08 / 0x00 | Mode-switch on/off bytes | AppleDefinition.h:111-115 |
| 'PTPC' | Pool tag | Device.h:41 |
| 0xFC | First byte of the HQA blob, the only one sent | Hid.h:63 |

### 10.5 TODOs and comments that matter
- InputInterrupt.c:333-334: "Defuzz functions remain the same / TODO: Implement defuzz later".
- InputInterrupt.c:520-522: the 25 mm palm threshold was "raised a bit higher… Or maybe I used the wrong unit? IDK".
- Device.c:329-330 and :432-433: "Actual device does not transfer bytes as expected… we do not check um_size as a temporary workaround" (see B1).
- AppleDefinition.h:412: the T2 width parameters are "incorrect; check actual data later".
- Wellspring*.h:26-27: "Size is hard-coded… designed for MacBookAir 7,2" (WS3 file) / "MacBookPro 11,1" (WS7A file). These are copy-paste comments.
- INF:87: MT2 USB "will be a last few versions… old implementation".

---

## 11. Risky constructs for the Rust port

1. **Packed and bitfield structs.**
   - `PTP_CONTACT` is `pack(1)` and 9 bytes, with `Confidence` at bit 0 and `TipSwitch` at bit 1 of byte 0 (MSVC allocates bitfields LSB-first).
   - `PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` is `pack(1)`: bit 0 `ButtonReport`, bit 1 `SurfaceReport`.
   - The `TRACKPAD_FINGER_TYPE5` union: `Id` = low nibble of byte 8.
   - Recommendation: do **not** use `#[repr(packed)]` structs with references. Serialize `PTP_REPORT` into a `[u8; 50]` by hand at the offsets in 7.1, and parse input with `from_le_bytes` on slices.
   - **Slice bounds for TYPE4.** Finger `i` starts at `h + tp_delta + i*tp_fsize`, but only 28 bytes (20 bytes used) belong to it. A `tp_fsize`-byte slice from there overruns `len` by 2 on the last finger and panics in Rust (see 7.2).
2. **Unaligned and type-punned loads.**
   - InputInterrupt.c:325 casts `u8*` to `TRACKPAD_FINGER*` (even offsets).
   - InputInterrupt.c:506-507 reads `*(USHORT*)` and `*(UINT*)` at `12 + 9i`, which is odd for odd `i`.
   - Strict aliasing and alignment are both violated in C. Rust: slice plus `from_le_bytes` only.
3. **Signed-arithmetic UB and implementation-defined behavior.**
   - Left shift of a negative int (`AmtRawToInteger(x) << 1`).
   - `(SHORT)(u16 << 3)` narrowing.
   - Negating INT_MIN (`-(INT)(tmp_y << 6)`).
   - Arithmetic right shift of a negative value.
   - The MSVC results are given in 7.5 and 7.6. Use `wrapping_neg`, `as i16`, `>>` on i32, and `*2` instead of `<<1`. Validate against the golden vectors.
4. **Macros with side effects.**
   - `DEFAULT_PTP_HQA_BLOB` used as an rvalue triggers the comma operator (B2).
   - `DATAFORMAT(type)` and `USBMSG_TYPEn` are token-pasting multi-initializer macros. Expand them to explicit struct literals; section 8.1 lists the expanded values.
   - The HID descriptor macros are pure byte lists. Use a const array and check it against the SHA-256 values.
   - `sizeof(DeviceInfo->um_size)` is `sizeof(int)` (B1).
5. **`__declspec(align(2))`** on `TRACKPAD_FINGER` (28 bytes) and `TRACKPAD_FINGER_TYPE5`, where MSVC's `sizeof` is 10 but the stride is 9. Never use `size_of` for the stride; use `tp_fsize`.
6. **Global data defined in headers.**
   - `StaticHidRegistry.h` defines **non-const** global arrays and structs. It is included only by Hid.c.
   - `Bcm5974ConfigTable` is `static const` in a header included by every TU, so each TU gets a copy.
   - In Rust, use `static`/`const` with no mutability.
7. **No inline asm, no function-pointer patching, no undocumented Windows structures.** The Apple packet formats are reverse-engineered, inherited from Linux `bcm5974`. Only these are used:
   - public WDF/UMDF DDIs
   - `hidport.h` structures (`HID_DESCRIPTOR`, packed, 9 bytes; `HID_DEVICE_ATTRIBUTES`, 32 bytes; `HID_XFER_PACKET`, local only)
   - `USB_DEVICE_DESCRIPTOR` (18 bytes)
8. **WDF from Rust.**
   - All `Wdf*` calls go through the version-bound function table (`WdfFunctions_02015` plus `WdfDriverGlobals`), which the C headers hide behind macros. Rust must bind against **UMDF 2.15** (the INF pins `UmdfLibraryVersion=2.15.0`), for example with windows-drivers-rs `wdk-sys` in UMDF mode and `call_unsafe_wdf_function_binding!`.
   - Every `WDF_*_INIT` helper is a C inline function that must be reimplemented with the exact `Size` values from the 2.15 headers:
     - `WDF_OBJECT_ATTRIBUTES_INIT` (ExecutionLevel and SynchronizationScope = InheritFromParent)
     - `WDF_DEVICE_PNP_CAPABILITIES_INIT` (all WdfUseDefault, Address and UINumber = -1)
     - `WDF_IO_QUEUE_CONFIG_INIT[_DEFAULT_QUEUE]` (PowerManaged = WdfUseDefault; parallel → NumberOfPresentedRequests = -1)
     - `WDF_USB_CONTINUOUS_READER_CONFIG_INIT`, `WDF_USB_CONTROL_SETUP_PACKET_INIT` (Type = Standard, then overridden to Class)
     - `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER`, `WDF_USB_DEVICE_INFORMATION_INIT`, `WDF_USB_PIPE_INFORMATION_INIT`, `WDF_PNPPOWER_EVENT_CALLBACKS_INIT`, `WDF_DRIVER_CONFIG_INIT`
   - `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME` generates a `WDF_OBJECT_CONTEXT_TYPE_INFO` static (name `"DEVICE_CONTEXT"`, size 96 on x64). Rust needs an equivalent static with a stable address.
9. **Concurrency.**
   - The parallel queue and the continuous reader (more than 1 pending read; completions may overlap and arrive out of order) touch the context with no locks (B22).
   - Rust cannot express the C data race safely. Use `AtomicBool`/`AtomicU8` (Relaxed reproduces C semantics) and a `Mutex` or WDF wait lock around SetWellspringMode.
   - Consider `NumPendingReads = 1` to guarantee in-order frames. That is a deviation.
10. **Blocking in callbacks.** Synchronous control transfers with no timeout run inside:
    - the reader completion (EmergReset)
    - the I/O dispatch (SET_FEATURE)
    - D0Entry/D0Exit
11. **FFI hygiene.**
    - All callbacks are `extern "C"`.
    - Use `panic = "abort"`, or `catch_unwind` at every callback boundary.
    - The `WDFCONTEXT` passed to the reader is a raw `*mut DEVICE_CONTEXT`. It is valid while the device exists, because the reader is stopped in D0Exit.
12. **Entry point.** UMDF 2 DLLs are entered through the stub `WdfDriverStubUm.lib`, which calls `DriverEntry(PDRIVER_OBJECT, PUNICODE_STRING)`. In UM these are opaque framework-supplied pointers. The Rust crate must link the 2.15 stub, or replicate its exports, and be a `cdylib`.
13. **Tracing.** WPP has no Rust equivalent. TraceLogging maps to Microsoft's `tracelogging` crate with the same provider name and GUID (`{871B1E2D-CC5A-4ADE-B74E-6CF1004EF149}`), so that `AmtPtpDevice.wprp` keeps working.

---

## 12. Appendix: porting checklist (behavioral parity)
- [ ] Bounded PID lookup over the 15 rows (8.1). Reject unknown PIDs with `STATUS_INVALID_DEVICE_STATE` in PrepareHardware.
- [ ] Six 516-byte descriptors matching the SHA-256 values (5.1). A 9-byte HID descriptor `09 21 00 01 00 01 22 04 02`.
- [ ] Attributes `{32, 0x8910, idProduct, 1}`.
- [ ] Feature handling per 5.5 and 5.6, including SET always returning SUCCESS (B8) and the Information values.
- [ ] Mode switch per 6.3, with the exact setup bytes and wLength 4 (B1 decision).
- [ ] D0Entry/D0Exit order per 2.5 and 2.6.
- [ ] Reader: interrupt pipe = first interrupt pipe of interface 0; transfer lengths 474/478/486/526/156. Configure the reader only once per pipe (B16).
- [ ] Frame validation and EmergReset (7.4), the mode gate, and the per-type parse (7.5 and 7.6) against the golden vectors.
- [ ] 50-byte report, zero-initialized (B13), completed with Information 50.
- [ ] Decide on B2, B4, B6, B7, B9-B12, B14, B16-B18, B22, and record each decision.

---

## Verification addendum

An independent adversarial pass re-read every file under `src/AmtPtpDeviceUsbUm` and `AmtPtpDevice.inf` in full. It recomputed everything with a fresh harness (a scratch harness, not kept: `desc.c` expanded the real headers, `gold.c` ran the literal C expressions).

**Confirmed unchanged:**

- all six descriptor SHA-256 values and the WS3 byte dump
- the per-family patch offsets and values
- the HQA blob SHA
- the 15 config-table rows
- the IOCTL values (checked against WDK 10.0.28000 `hidport.h:183-215`)
- the struct sizes and offsets (PTP_REPORT 50, ScanTime @46; caps 3; HQA 257; mode 2; selective 2; umapp 4; TRACKPAD_FINGER 28)
- all MT2, TYPE2-4 and TYPE4-frame golden vectors
- the default `UMDF_VERSION_MINOR=15` (`WindowsDriver.UserMode.Default.props:22`)
- `WdfMemoryCopyFromBuffer` → `STATUS_BUFFER_TOO_SMALL` (MS Learn)

**Corrections made inline:**

1. **1.5 `UmdfDispatcher=NativeUSB`.** It was described as "USB I/O goes through WinUSB". It actually means the reflector handles USB I/O natively (UMDF ≥ 2.15; `WinUsb` is the older value). The same fix was applied to the 6.3 wIndex caveat. Source: MS Learn, "Specifying WDF Directives in INF Files".
2. **7.2 TYPE4 slot framing.** "The last 2 bytes of each slot are ignored" was wrong. Slots start at `46+30i`, the 2-byte prefix is skipped by `tp_delta`, and the 28-byte finger ends at the slot end. Added a port hazard here and in 11.1: a `tp_fsize` slice from `h+delta+i*f` overruns `len` by 2 bytes on the last finger. Example: the 106-byte frame vector, where finger 1 at 78 gives 78+30 = 108.
3. **7.6 MT2 Y closed form.** `y = -F - (L!=0)` is wrong for `F=-4096, L=0` (`le32<<6 == 0x80000000`). The literal C gives -4096, so Y=0; the formula gives +4096, so Y=6575. Added the exception and a 7th golden row `00 00 00 02 01 01 00 00 00 → y=-4096, X=3678, Y=0, ID 0, Tip 1, Conf 1` (computed by `gold.c`).
4. **B16, and the 2.4 restart caveat, 6.1 step 5, the 6.4 table and the 12 checklist.** "Reconfiguring the reader is untested" was replaced with the WDF source fact. `FxUsbPipe::InitContinuousReader` (`src/framework/shared/targets/usb/fxusbpipe.cpp`) returns `STATUS_INVALID_DEVICE_STATE` if `m_Reader != NULL`. The driver never reselects the configuration, so the second PrepareHardware is expected to fail after a PnP stop/start. Port: configure the reader once. A ReleaseHardware alone does not fix this.
5. **5.1 FC2 global-state carry-over.** "Fingers 4 and 5 inherit" was inaccurate. Only finger 4's Confidence/Tip/ContactID items inherit (cm, exp -2, PhysMax = finger-3 Y). Finger 5 inherits nothing, because FC1 resets. The tail re-sets the unit, exponent and physical max. Also noted that Contact Count, the button bits, the features and the later TLCs inherit seconds, -4 and 65535 from the tail. The bytes are unchanged.
6. **9 Persistent settings.** The qualification levels are zeroed only at EvtDeviceAdd, not "at every device start". They survive a PnP stop/start and D-transitions. Also noted that `IsWellspringModeOn` is not reset by PrepareHardware.
7. **2.5 and B10.** D0Exit clears `IsWellspringModeOn` only if its `Set(FALSE)` succeeds. If it fails, the flag stays TRUE and D0Entry re-enables the mode.
8. **B18.** Added the MT2 no-upper-clamp case: X ≤ 7773 > 7612 and Y ≤ 6574 > 5065.
9. **Section 2.** Added that as a filter (`WdfFdoInitSetFilter`) the driver is not the WDF power-policy owner, so S0Idle/SxWake settings cannot simply be added in a port.
10. **1.5 `[Version]`.** Added `Signature` and `Provider`.
11. **Header.** Noted that HEAD 2edb0f2 differs from 29d2778 only in `.gitattributes`.

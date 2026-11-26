<!-- Porting analysis of the C driver as of commit 2edb0f2, written before the Rust rewrite. File:line references point into that commit's `src/` tree, which has since been removed. -->

# AmtPtpDeviceSpiKm — Porting Specification (C/KMDF → Rust)

Source root: `src/AmtPtpDeviceSpiKm/` (all paths below are relative to it unless prefixed).
INF: `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf`.

Every file in the directory was read in full: `Driver.c/.h`, `Device.c/.h`, `Queue.c/.h`, `Hid.c/.h`,
`HidCommon.h`, `Input.c/.h`, `AppleDefinition.h`, `Public.h`, `Trace.h`,
`HID/SpiTrackpadSeries1.h`, `HID/SpiTrackpadSeries2.h`, `HID/SpiTrackpadSeries3.h`,
`AmtPtpDeviceSpiKm.vcxproj`, `AmtPtpDeviceSpiKm.vcxproj.filters`.
There is **no `.rc`, no `.wprp`, and no `.inf`** inside the directory. The INF is shared, in the package project.

Numeric facts such as struct offsets and sizes, report-descriptor bytes and lengths, and IOCTL codes were
not worked out by hand. They come from compiling the project's own headers with `gcc -mms-bitfields`, which
uses the MSVC bitfield and layout rules, together with WDK 10.0.28000 `hidport.h`/`hidclass.h`. The
SHA-256 values in §5 let you check a Rust byte table against the C one.

## Architecture in one paragraph

The driver is a **KMDF lower filter** installed under **`mshidkmdf.sys`**, Microsoft's HID pass-through
minidriver, which is the function driver and service for the devnode. That devnode is the HID child
`SPI\VID_05ac&PID_xxxx&MI_02`, enumerated by Apple's Boot Camp SPI driver. The stack looks like this:

```
HIDClass (hidclass.sys)
  └─ mshidkmdf.sys (FDO, HID minidriver; forwards IOCTL_HID_* as IRP_MJ_INTERNAL_DEVICE_CONTROL)
       └─ AmtPtpDeviceSpiKm.sys   (THIS DRIVER, KMDF lower filter, WdfFdoInitSetFilter)
            └─ Apple SPI PDO (Boot Camp SPI HID driver; itself speaks the HID-minidriver IOCTL dialect)
```

The driver does the following:

- It intercepts the HID-minidriver internal IOCTLs.
- It **replaces** the Apple device's HID identity. It reports a synthetic VID `0x8910` and its own HID
  report descriptor, a Windows Precision Touchpad (PTP) descriptor, or a touchscreen variant.
- It turns each upstream `IOCTL_HID_READ_REPORT` into one downstream `IOCTL_HID_READ_REPORT` to the
  Apple PDO.
- It converts Apple's raw multitouch packet into a 30-byte PTP input report (report ID 5).
- It answers the PTP feature reports: device caps (7) and HQA blob (8) on GET, input mode (4) and
  function switch (6) on SET.
- It puts the trackpad into multitouch mode by sending a HID SET_FEATURE (report ID 2, payload `02 01`)
  downstream at every self-managed-I/O init or restart. If that fails, it retries once after 5 s.

There is no USB, no continuous reader, no work item of its own, no idle or wake support, no user-mode
configuration path and no contact tracking.

---

## 1. Build facts

| Item | Value | Source |
|---|---|---|
| Driver model | KMDF, kernel mode (`<DriverType>KMDF</DriverType>`, `<ConfigurationType>Driver</ConfigurationType>`) | vcxproj:71-72 (repeated per config 67-132) |
| KMDF version | 1.23 (`KMDF_VERSION_MAJOR=1`, `KMDF_VERSION_MINOR=23`). The runtime needs Wdf01000.sys ≥ 1.23, i.e. Windows 10 1709+ | vcxproj:75-76 |
| `_NT_TARGET_VERSION` | `0xA000004` (NTDDI_WIN10_RS3, Windows 10 1709) | vcxproj:74 |
| TargetVersion | Windows10 | vcxproj:68 |
| DriverTargetPlatform | Universal | vcxproj:73 |
| PlatformToolset | WindowsKernelModeDriver10.0 | vcxproj:70 |
| Windows SDK | `$(LatestTargetPlatformVersion)`; `10.0.19041.0` when `BuildEnvironment==Github` | vcxproj:60-65 |
| Platforms / configs | x64 and ARM64 × {Debug, Release, ReleaseSigned}. No x86 (removed in commit 8e0cc7c) | vcxproj:3-28 |
| Output binary | `AmtPtpDeviceSpiKm.sys` (no TargetName override, so it equals ProjectName), in `$(SolutionDir)build\AmtPtpDeviceSpiKm\<Platform>\<Config>\` | vcxproj:57,150 |
| Intermediate dir | `$(SolutionDir)intermediate\AmtPtpDeviceSpiKm\<Platform>\<Config>\` (also added to IncludePath for `.tmh`) | vcxproj:151-152 |
| Project GUID | `{FC08B706-5661-47FA-A840-053B06125750}` | vcxproj:51 |
| Compiled sources | Device.c, Driver.c, Hid.c, Input.c, Queue.c | vcxproj:29-35 |
| Preprocessor defines | **None added by the project.** Only WDK defaults apply: `KMDF_VERSION_MAJOR/MINOR`, `_AMD64_`/`_ARM64_`, `DBG=1` in Debug (from UseDebugLibraries), `_WIN64`, etc. | vcxproj (no `<PreprocessorDefinitions>`) |
| Disabled warnings | `4214` (nonstandard bit-field type, needed for the `UCHAR` bitfields in Hid.h) | vcxproj:194 etc. |
| Linked libs | **None added.** WDK KMDF defaults only (WdfLdr/WdfDriverEntry stub, ntoskrnl, hal, BufferOverflowFastFailK…) | vcxproj (no `<AdditionalDependencies>`) |
| WPP | `WppEnabled=true`, `WppRecorderEnabled=true` (IFR), `WppKernelMode=true`, `WppScanConfigurationData=trace.h`. Each .c includes `<file>.tmh` | vcxproj:188-250; Driver.c:18, Device.c:18, Queue.c:18, Hid.c:4, Input.c:2 |
| WPP control GUID | `{9b319d80-fe25-41e4-80ef-7d8528f7f1b5}`, name `AmtPtpDeviceSpiKmTraceGuid`. Flags: `TRACE_DRIVER`=0x1, `TRACE_DEVICE`=0x2, `TRACE_QUEUE`=0x4, `TRACE_HID_INPUT`=0x8 | Trace.h:20-31 |
| WPP functions | `Trace{FLAGS=TRACE_DRIVER}(LEVEL,MSG,...)`, `TraceEvents(LEVEL,FLAGS,MSG,...)`. Level gate is `WPP_CONTROL(...).Level >= lvl` | Trace.h:33-61 |
| Debug prints | `KdPrintEx((DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, ...))`, a no-op unless DBG | Driver.c:67,89,94; Device.c:229 |
| Signing | DriverSign `FileDigestAlgorithm=sha256` on the **x64** configs only (not the ARM64 ones in this project). ReleaseSigned uses `SignMode=ProductionSign` with `$(ProductionCertPath)`, or `Off` on AzurePipeline. Timestamp server `http://timestamp.digicert.com` | vcxproj:141-147,196-198 |
| Packaging | `<FilesToPackage Include="$(TargetPath)"/>`. Referenced by `AmtPtpDeviceUniversalPkg.vcxproj:30-32` | vcxproj:251-253 |
| Resource / version info | **No `.rc` file**, so the binary has no VERSIONINFO resource. Version is carried only by INF `DriverVer`, which is stamped: `2021.$(PipelineReleaseID).1.1000` on AzurePipeline, otherwise `*` (build time) | AmtPtpDeviceUniversalPkg.vcxproj:138-143 |
| Code sections | `INIT`: DriverEntry. `PAGE`: AmtPtpDeviceSpiKmEvtDeviceAdd, AmtPtpDeviceSpiKmEvtDriverContextCleanup, AmtPtpDeviceSpiKmCreateDevice, AmtPtpDeviceSpiKmQueueInitialize. Everything else is non-paged (Hid.c, Input.c and the rest of Device.c have **no** `alloc_text`) | Driver.c:20-24; Device.c:20-22; Queue.c:20-22 |
| Pool tag | `PTP_LIST_POOL_TAG 'LTPA'`. The MSVC multi-char literal is `0x4C545041`, which pool tools display as **"APTL"**. In Rust: `u32::from_le_bytes(*b"APTL")` | Driver.h:43 |

### 1.1 INF sections that reference this binary (`AmtPtpDevice.inf`)

| Section / line | Content |
|---|---|
| `[Version]` 5-12 | `Class=HIDClass`, `ClassGuid={745a17a0-74d3-11d0-b6fe-00a0c90f57da}`, `Provider=%ManufacturerName%` ("Bingxing Wang"), `CatalogFile=AmtPtpDevice.cat`, `DriverVer=` (stamped), `PnpLockdown=1` |
| `[DestinationDirs]` 14-16 | `DefaultDestDir=13` (driver store run-from). `SpiKmDrivers_Dir` is not listed, so it uses 13 |
| `[SourceDisksFiles]` 24 | `AmtPtpDeviceSpiKm.sys = 1,,` |
| `[Standard.NT$ARCH$]` 74-84 | Hardware IDs, all mapped to install section `AmtPtpDeviceSpiKm_Device`: `SPI\VID_05ac&PID_0272&MI_02`, `…0273…`, `…0275…`, `…0276…`, `…0277…`, `…0278…`, `…0279…`, **`…0280…`**, `…0290…`, `…0291…` |
| `[SpiKmDrivers_Dir]` 110-111 | `AmtPtpDeviceSpiKm.sys` |
| `[mshidkmdf_Service_Inst]` 114-118 | ServiceType=1 (kernel), StartType=3 (demand), ErrorControl=1, `ServiceBinary=%10%\System32\Drivers\mshidkmdf.sys` |
| `[AmtPtpDeviceSpiKm_Device.NT]` 220-221 | `CopyFiles=SpiKmDrivers_Dir` |
| `[AmtPtpDeviceSpiKm_Device.NT.Services]` 223-225 | `AddService = AmtPtpDeviceSpiKm,, AmtPtpDeviceSpiKm_Service_Inst` (no flags, so it is not the function driver). `AddService = mshidkmdf, %SPSVCINST_ASSOCSERVICE% (0x2), mshidkmdf_Service_Inst`, which makes **mshidkmdf the function driver** |
| `[AmtPtpDeviceSpiKm_Device.NT.HW]` 227-228 | `AddReg = AmtPtpDeviceSpiKm_Device.NT.AddReg` |
| `[AmtPtpDeviceSpiKm_Device.NT.AddReg]` 230-231 | `HKR,,"LowerFilters",0x00010008,"AmtPtpDeviceSpiKm"` (`FLG_ADDREG_TYPE_MULTI_SZ \| FLG_ADDREG_APPEND`), so the driver is a **device lower filter**. No FriendlyName is set, unlike the USB sections |
| `[AmtPtpDeviceSpiKm_Service_Inst]` 233-239 | DisplayName `"Apple SPI Precision Touchpad Driver Service"`, ServiceType=1, StartType=3, ErrorControl=1, `ServiceBinary=%13%\AmtPtpDeviceSpiKm.sys`, `LoadOrderGroup=Extended Base` |
| `[AmtPtpDeviceSpiKm_Device.NT.Wdf]` 241-242 | `KmdfService = AmtPtpDeviceSpiKm, AmtPtpDeviceSpiKm_wdfsect` |
| `[AmtPtpDeviceSpiKm_wdfsect]` 244-245 | `KmdfLibraryVersion = $KMDFVERSION$`, which stampinf turns into 1.23 |
| `[Strings]` 255-256 | `AmtPtpDeviceSpiKm.DeviceDesc="Apple SPI Precision Touchpad Device"`, `AmtPtpDeviceSpiKm.SVCDESC="Apple SPI Precision Touchpad Driver Service"` |

The service name must stay **`AmtPtpDeviceSpiKm`**. The registry Parameters key (§9) is derived from it.

---

## 2. Entry points and WDF callbacks (registration and config values)

### 2.1 `DriverEntry` — Driver.c:26-98 (section INIT)
1. `WPP_INIT_TRACING(DriverObject, RegistryPath)` (64).
2. `WDF_OBJECT_ATTRIBUTES_INIT(&attributes)`, `attributes.EvtCleanupCallback = AmtPtpDeviceSpiKmEvtDriverContextCleanup` (73-74). There is no context type.
3. `WDF_DRIVER_CONFIG_INIT(&config, AmtPtpDeviceSpiKmEvtDeviceAdd)` (76-78). Defaults: `EvtDriverUnload=NULL`, `DriverInitFlags=0`, `DriverPoolTag=0`.
4. `WdfDriverCreate(DriverObject, RegistryPath, &attributes, &config, WDF_NO_HANDLE)` (80-85).
5. On failure, `WPP_CLEANUP(DriverObject)` and return the status (87-92). Otherwise return STATUS_SUCCESS.

Driver object defaults that every child inherits: `SynchronizationScope=None`, `ExecutionLevel=Dispatch`. The callbacks below can therefore run at IRQL ≤ DISPATCH_LEVEL, with no framework serialization.

### 2.2 `AmtPtpDeviceSpiKmEvtDriverContextCleanup` — Driver.c:151-179 (PAGE)
Registered as `EvtCleanupCallback` on the **WDFDRIVER** object. It calls only `WPP_CLEANUP(WdfDriverWdmGetDriverObject(Driver))`.

### 2.3 `AmtPtpDeviceSpiKmEvtDeviceAdd` — Driver.c:100-149 (PAGE)
Registered as `EvtDriverDeviceAdd`.
1. `WdfFdoInitSetFilter(DeviceInit)` (141). The device is a filter: request types with no queue callback are auto-forwarded down, and default queues with `PowerManaged=WdfUseDefault` become **non-power-managed** (verified on Microsoft Learn).
2. `WdfPdoInitAllowForwardingRequestToParent(DeviceInit)` (142). **DEAD / INCORRECT.** This is a PDO-only DDI called on a filter-FDO DeviceInit. Per the docs it belongs only before creating a child PDO. **Verified in the open-source WDF** (`microsoft/Windows-Driver-Frameworks`, `src/framework/shared/core/fxdeviceinitapi.cpp`): on a non-PDO init it logs a TRACE_LEVEL_ERROR "Not a PWDFDEVICE_INIT for a PDO", calls `FxVerifierDbgBreakPoint` (breaks only when the WDF verifier is on and a debugger is attached) and returns without changing any state. It has no functional effect for a filter: this driver never calls `WdfRequestForwardToParentDeviceIoQueue`. **Drop it in Rust.**
3. `return AmtPtpDeviceSpiKmCreateDevice(DeviceInit)` (144).

### 2.4 `AmtPtpDeviceSpiKmCreateDevice` — Device.c:24-164 (PAGE)
1. `WDF_PNPPOWER_EVENT_CALLBACKS` (47-55) registers:
   - `EvtDevicePrepareHardware = AmtPtpEvtDevicePrepareHardware`
   - `EvtDeviceD0Entry = AmtPtpEvtDeviceD0Entry`
   - `EvtDeviceD0Exit = AmtPtpEvtDeviceD0Exit`
   - `EvtDeviceSelfManagedIoInit = AmtPtpEvtDeviceSelfManagedIoInitOrRestart`
   - `EvtDeviceSelfManagedIoRestart = AmtPtpEvtDeviceSelfManagedIoInitOrRestart` (same function)
   - **Not registered:** ReleaseHardware, D0EntryPostInterruptsEnabled, SelfManagedIoSuspend/Flush/Cleanup, SurpriseRemoval, QueryRemove/Stop, UsageNotification.
2. **Not called:** `WdfDeviceInitSetPowerPolicyEventCallbacks`, `WdfDeviceAssignS0IdleSettings`, `WdfDeviceAssignSxWakeSettings`, `WdfDeviceInitSetFileObjectConfig` (no file object config), `WdfDeviceInitSetIoType` (inherited from lower, as for a filter), `WdfDeviceInitSetPowerPolicyOwnership`, `WdfDeviceInitSetExclusive`, `WdfDeviceInitSetPowerPageable`/`NotPageable`/`SetPowerInrush`, `WdfDeviceInitSetDeviceType`, `WdfDeviceInitSetCharacteristics`.
   Because the device is a filter, KMDF copies these from the attached lower device at `WdfDeviceCreate` (verified in WDF source `src/framework/shared/core/km/fxdevicekm.cpp`, `if (m_Filter)` branch): the buffer method (`DO_BUFFERED_IO`/`DO_DIRECT_IO`), **`DO_POWER_PAGABLE | DO_POWER_INRUSH`**, `DeviceType` and `Characteristics`. The Rust port must not call any of the setters above, so that it inherits the same values.
3. `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&DeviceAttributes, DEVICE_CONTEXT)` (58). SyncScope and ExecutionLevel are inherited, i.e. None/Dispatch. `WdfDeviceCreate` (60).
4. `pDeviceContext->SpiDevice = Device` (78).
5. `WdfLookasideListCreate(WDF_NO_OBJECT_ATTRIBUTES, REPORT_BUFFER_SIZE=1024, NonPagedPoolNx, WDF_NO_OBJECT_ATTRIBUTES, 'LTPA', &HidReadBufferLookaside)` (83-90). **Parent defaults to the WDFDRIVER, not the device** (verified on Learn), so each device instance leaks one lookaside list until driver unload. In Rust, parent it to the device; the change is harmless.
6. Timer (105-110):
   - `WDF_TIMER_CONFIG_INIT(&TimerConfig, AmtPtpPowerRecoveryTimerCallback)` gives `Period=0`, so it is one-shot.
   - `TimerConfig.AutomaticSerialization = TRUE`. This has **no effect** because the device sync scope is None (verified on Learn).
   - Learn also states that a passive-level timer may set `AutomaticSerialization=TRUE` only if the parent device's execution level is Passive. The C code violates that rule, yet `WdfTimerCreate` succeeds. Verified in WDF source (`fxtimer.cpp`, `_GetEffectiveLock`): with sync scope None no callback lock is found, so the `STATUS_WDF_INCOMPATIBLE_EXECUTION_LEVEL` path is never taken.
   - **Rust: set `AutomaticSerialization=FALSE`.** Behavior is identical (there is no sync scope), and it avoids the documented-invalid combination.
   - Attributes: `ParentObject=Device`, `ExecutionLevel=WdfExecutionLevelPassive`. This is a passive-level timer whose callback runs in a framework work item at PASSIVE_LEVEL.
   - The handle is stored in `PowerOnRecoveryTimer`.
7. `SpiTrackpadIoTarget = WdfDeviceGetIoTarget(Device)` (124). This is the local (default) I/O target, i.e. the next-lower device, which is the Apple PDO. If it is NULL, return `STATUS_INVALID_DEVICE_STATE` (125-129).
8. `DeviceStatus = D3` (134).
9. `WdfDeviceCreateDeviceInterface(Device, &GUID_DEVINTERFACE_AmtPtpDeviceSpiKm, NULL)` (140-144). GUID `{f271e39d-d211-4a6e-a439-63e51940d210}` (Public.h:22-24). This is effectively **dead**: nothing in the driver handles create or IOCTL from user mode, and an open goes to the top of the stack (hidclass). See §9.
10. If that succeeds, `AmtPtpDeviceSpiKmQueueInitialize(Device)` (151).

Error-path quirks:
- If `WdfDeviceCreate` fails, the code still reaches `exit` with that status.
- On a lookaside or timer failure it jumps to `exit`.
- If `WdfDeviceCreateDeviceInterface` fails, its status is returned and queues are not created.

### 2.5 `AmtPtpDeviceSpiKmQueueInitialize` — Queue.c:24-88 (PAGE)
- **Default queue** (44-57):
  - `WDF_IO_QUEUE_CONFIG_INIT_DEFAULT_QUEUE(&QueueConfig, WdfIoQueueDispatchParallel)` gives `DefaultQueue=TRUE`, `PowerManaged=WdfUseDefault`, which is **not power-managed** because the device is a filter. `NumberOfPresentedRequests=-1` (unlimited) and `AllowZeroLengthRequests=FALSE`.
  - `EvtIoInternalDeviceControl = AmtPtpDeviceSpiKmEvtIoInternalDeviceControl`
  - `EvtIoStop = AmtPtpDeviceSpiKmEvtIoStop`
  - No EvtIoRead/Write/DeviceControl/Default. Those request types are auto-forwarded to the lower driver by the filter rule; so are create, close and cleanup.
  - Object attributes: `WDF_NO_OBJECT_ATTRIBUTES` (the `QUEUE_CONTEXT` declared in Queue.h:23-29 is **never used**). The local `Queue` handle is discarded.
- **Manual queue `HidQueue`** (68-76): `WDF_IO_QUEUE_CONFIG_INIT(&QueueConfig, WdfIoQueueDispatchManual)`, `PowerManaged=WdfFalse`, no callbacks, `WDF_NO_OBJECT_ATTRIBUTES`. It holds upstream `IOCTL_HID_READ_REPORT` requests. Queued requests are cancelable by the framework automatically.

### 2.6 `AmtPtpDeviceSpiKmEvtIoInternalDeviceControl` — Queue.c:133-222 (non-paged; IRQL ≤ DISPATCH)
Dispatch table is in §4. If the handler did not mark the request pending (`RequestPending != TRUE`), the code calls `WdfRequestComplete(Request, Status)` (213-219). Information is whatever the handler set, normally 0 otherwise.

### 2.7 `AmtPtpDeviceSpiKmEvtIoStop` — Queue.c:224-274
An empty body; it is template boilerplate. Because the default queue is not power-managed, it is only invoked at removal for driver-owned requests from that queue. READ requests have been moved to `HidQueue`, and GET_STRING requests are send-and-forget, so effectively there is nothing to do. **Can be omitted** (dead code).

### 2.8 `AmtPtpEvtDevicePrepareHardware` — Device.c:166-317 (PASSIVE)
Steps:
0. If `DeviceGetContext(Device) == NULL`, return `STATUS_INVALID_DEVICE_STATE` (197-207). This branch is unreachable, so it is dead.
1. Send `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (`0x000B0027`) **synchronously** to the lower target.
   - Output: a zeroed local `HID_DEVICE_ATTRIBUTES`, 32 bytes, passed via `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER`. No input. No send options, so **no timeout** (210-225).
   - On failure, return that status and the device fails to start.
2. Store the result in context fields `HidVendorID`, `HidProductID`, `HidVersionNumber` (239-241).
3. Linear search of `SpiTrackpadConfigTable` (AppleDefinition.h:52-71) until `VendorId==0`, matching **both** VID and PID. On a match, copy all six fields into `TrackpadInfo` (244-259). With no match, return **`STATUS_NOT_FOUND`** and the device fails to start (261-265).
4. Registry: see §9 (268-303).
5. Always return `STATUS_SUCCESS` after the registry step (306).

### 2.9 `AmtPtpEvtDeviceD0Entry` — Device.c:319-351
Sets `DeviceStatus = D0ActiveAndUnconfigured` (339) and calls `KeQueryPerformanceCounter(&LastReportTime)` (342), then returns STATUS_SUCCESS. **Caution:** the argument of `KeQueryPerformanceCounter` is the optional *`PerformanceFrequency`* out-parameter (Learn: `LARGE_INTEGER KeQueryPerformanceCounter([out, optional] PLARGE_INTEGER PerformanceFrequency)`). The counter value is the return value, and the code discards it. So this stores the **QPC frequency** (a boot-time constant, typically 10,000,000), not a timestamp. See §7.3 step 5. It does no I/O. `PreviousState` is used only for tracing via `DbgDevicePowerString`.

### 2.10 `AmtPtpEvtDeviceD0Exit` — Device.c:353-395
Steps:
1. Set `DeviceStatus = D3` (371).
2. Loop on `WdfIoQueueRetrieveNextRequest(HidQueue)`, completing each request with **`STATUS_CANCELLED`** (Information 0), until retrieval fails (374-383).
3. Return STATUS_SUCCESS.

It does **not**:
- stop `PowerOnRecoveryTimer`;
- cancel in-flight lower reads;
- send `SetState(FALSE)`.

### 2.11 `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` — Device.c:397-427 (PASSIVE)
Registered for both SelfManagedIoInit (first start) and SelfManagedIoRestart (every return to D0 after a low-power state). It runs after D0Entry.
- It calls `AmtPtpSpiSetState(Device, TRUE)` (408).
- On failure, it traces, sets `Status = STATUS_SUCCESS` and calls `WdfTimerStart(PowerOnRecoveryTimer, WDF_REL_TIMEOUT_IN_SEC(5))`, which is `-50,000,000` (100 ns units, relative). `DeviceStatus` stays `D0ActiveAndUnconfigured` (409-416).
- On success, it sets `DeviceStatus = D0ActiveAndConfigured` and calls `KeQueryPerformanceCounter(&LastReportTime)` (418-422). As in §2.9, that writes the QPC **frequency** into `LastReportTime`.
- It always returns STATUS_SUCCESS.
- It **issues no lower read**, even if requests are already sitting in `HidQueue` (see §6.4).
- History: commit `e92581b` "SPI: workaround parent device initialization delay from S3 wakeup" introduced this retry.

### 2.12 `AmtPtpPowerRecoveryTimerCallback` — Device.c:541-562 (PASSIVE, framework work item)
1. `Device = WdfTimerGetParentObject(Timer)`.
2. `AmtPtpSpiSetState(Device, TRUE)`.
3. On success, call `AmtPtpSpiInputIssueRequest(Device)` (**exactly one** lower read), **then** set `DeviceStatus = D0ActiveAndConfigured` (556-559). `LastReportTime` is **not** refreshed.
4. On failure, **nothing**. There is no rescheduling, so the device stays unconfigured until the next D0 transition. `STATE_SWITCH_MAX_RETRIES 5` (Driver.h:48) is defined but **unused**.

The timer is not stopped on D0Exit. If D0Exit happens within the 5 s window, the callback can run while the device is in D3.
- KMDF does **not** stop the local I/O target on D0Exit; it stops it only on remove. The WDF power and PnP state machines (`powerstatemachine.cpp`, `pnpstatemachine.cpp`, `fxpkgfdo.cpp`) contain no target stop on power-down. So the SET_FEATURE really is delivered to the Apple PDO, which is itself powering down.
- If the lower driver succeeds, the callback issues one lower read and sets `Configured` while the device is in D3. If it fails, nothing happens.

The timer state also survives power cycles:
- If SMIO-restart fails again while an earlier timer is still pending, `WdfTimerStart` re-queues the same timer with a new due time, so the callback runs only once.
- If SMIO-restart **succeeds** while an earlier timer is pending, the stale timer still fires later. It sends an extra SET_FEATURE and issues one extra lower read. That read is dropped if `HidQueue` is empty; otherwise it serves the oldest queued request.

These are bugs; §10 lists them (B12).

### 2.13 `AmtPtpSpiSetState(Device, DesiredState)` — Device.c:457-539
Described byte-exactly in §6.2. It is called only with `TRUE`; the `FALSE` path is **never used** (dead). It also has an unreachable `DeviceGetContext == NULL` → `STATUS_INVALID_DEVICE_STATE` branch (471-481), which is dead.

### 2.14 Helper / debug-only functions
- `DbgDevicePowerString` — Device.c:429-455. Maps `WDF_POWER_DEVICE_STATE` to a string, used for tracing only.
- `DbgIoControlGetString` — Queue.c:90-131. Maps IOCTL codes to strings, used for tracing only. It includes `IOCTL_UMDF_*` names even though this is KMDF.

### 2.15 Request completion routine `AmtPtpRequestCompletionRoutine` — Input.c:157-321
Set via `WdfRequestSetCompletionRoutine` on each driver-created lower read (Input.c:127-131). The context argument is a pointer to that request's `WORKER_REQUEST_CONTEXT`. It runs at IRQL ≤ DISPATCH_LEVEL. See §7.

### 2.16 Context type sizes (not ABI-relevant)
- `DEVICE_CONTEXT` is about 96 bytes on x64.
- `WORKER_REQUEST_CONTEXT` is 16 bytes: two pointers.
- `QUEUE_CONTEXT` is 4 bytes and unused.

These are WDF context blobs and are never exposed outside the driver. Rust can lay them out freely.

---

## 3. Context structures

### 3.1 `DEVICE_CONTEXT` — Device.h:56-84 (accessor `DeviceGetContext`, Device.h:91)

KMDF zero-fills the context at creation, so every field starts at 0/NULL/FALSE.

| Field | C type | Meaning | Writers | Readers | Locking |
|---|---|---|---|---|---|
| `SpiDevice` | WDFDEVICE | Self handle | CreateDevice (Device.c:78) | **nobody** (dead) | — |
| `SpiTrackpadIoTarget` | WDFIOTARGET | Local I/O target (the lower Apple PDO) | CreateDevice (124) | PrepareHardware (218), SetState (502), GetStrings (Hid.c:566), IssueRequest (Input.c:57,98,135) | none needed (write-once) |
| `DeviceStatus` | `PTP_AAPL_DEVICE_POWER_STATUS` (enum, 4 bytes) | `D3=0`, `D0ActiveAndConfigured=1`, `D0ActiveAndUnconfigured=2` (Device.h:46-50) | CreateDevice (134, D3), D0Entry (339, Unconfigured), D0Exit (371, D3), SMIO init/restart (420, Configured), timer cb (558, Configured) | InputRoutineWorker (Input.c:33) | **none** (racy, see §10) |
| `InputPollThreadHandle` | HANDLE | — | nobody | nobody | **dead field** |
| `HidQueue` | WDFQUEUE | Manual queue of pending upstream READ_REPORTs | QueueInitialize (Queue.c:75) | D0Exit (376), Worker (Input.c:16), Completion (Input.c:187) | framework-internal |
| `HidVendorID` | USHORT | Real VID from lower attributes (0x05AC) | PrepareHardware (239) | **nobody** (dead) | — |
| `HidProductID` | USHORT | Real PID from lower attributes | PrepareHardware (240) | GetHidDescriptor (Hid.c:147), GetDeviceAttribs (346), GetReportDescriptor (403) | none |
| `HidVersionNumber` | USHORT | Real version from lower | PrepareHardware (241) | GetDeviceAttribs (347) | none |
| `TrackpadInfo` | `SPI_TRACKPAD_INFO` {u16 VendorId, u16 ProductId, i16 XMin, i16 XMax, i16 YMin, i16 YMax} (Device.h:21-28) | Coordinate range for the matched PID | PrepareHardware (249-254) | Completion (Input.c:232-235: XMin, YMax only) | none |
| `ReportType` | `REPORT_TYPE` enum (Device.h:40-44): `PrecisionTouchpad=0`, `Touchscreen=1`, `InvalidReportType=0x7fffffff` (a sentinel that forces a 32-bit enum; never used) | Which report descriptor to return | PrepareHardware (291/294) | GetReportDescriptor (Hid.c:412-467) | none |
| `PtpInputOn` | BOOLEAN | Set by input-mode feature (mode 3 → TRUE, 0 → FALSE) | SetFeatures (Hid.c:787,799) | **nobody** (write-only, no effect) | none |
| `PtpReportTouch` | BOOLEAN | Surface switch | SetFeatures (Hid.c:821) | **nobody** | none |
| `PtpReportButton` | BOOLEAN | Button switch | SetFeatures (Hid.c:820) | **nobody** | none |
| `LastReportTime` | LARGE_INTEGER (only `.QuadPart`, an i64, is used) | *Intended* to be the QPC value of the last report. *Actually* it always holds the QPC **frequency**: every writer passes it as `KeQueryPerformanceCounter`'s frequency out-parameter (§2.9, §7.3) | D0Entry (342), SMIO (421), Completion (Input.c:216-221) | Completion (Input.c:220) | **none**. This is formally a race, but every writer stores the same constant (§10) |
| `PowerOnRecoveryTimer` | WDFTIMER | 5 s one-shot retry timer | CreateDevice (110) | SMIO (414) | — |
| `HidReadBufferLookaside` | WDFLOOKASIDE | 1024-byte NonPagedPoolNx buffers for lower reads | CreateDevice (89) | IssueRequest (Input.c:74) | framework-internal |

### 3.2 `WORKER_REQUEST_CONTEXT` — Device.h:96-101 (accessor `WorkerRequestGetContext`)
This is the context on each **driver-created lower read request**.

| Field | Type | Meaning |
|---|---|---|
| `DeviceContext` | `PDEVICE_CONTEXT` | Back-pointer to the device context (Input.c:93) |
| `RequestMemory` | WDFMEMORY | The lookaside buffer used as the read's output (Input.c:94). It is deleted in the completion routine |

### 3.3 Unused types (dead; drop them)
- `QUEUE_CONTEXT`/`QueueGetContext` (Queue.h:23-29).
- `PTP_AAPL_MAPPING` + `MAPPING_MAX 10` (Device.h:32-38).
- `PTP_CONTACT_RAW`, `PTP_CONTACT_RAW_LL`, `enum CONTACT_STATE` (Hid.h:142-165).
- `PTP_USERMODEAPP_CONF_REPORT` (Hid.h:175-180).
- `DEVICE_VERSION` (Hid.h:16).
- `PTP_BUTTON_TYPE_PRESSURE_PAD`, `PTP_CONTACT_CONFIDENCE_BIT`, `PTP_CONTACT_TIPSWITCH_BIT` (Hid.h:95,100-101).
- `STATE_SWITCH_MAX_RETRIES` (Driver.h:48).
- `SPI_TRACKPAD_PACKET` fields other than `ClickOccurred`, `NumOfFingers` and `Fingers` (see §7.1).

---

## 4. IOCTL handling

All requests arrive as **IRP_MJ_INTERNAL_DEVICE_CONTROL** from mshidkmdf. They are dispatched in `AmtPtpDeviceSpiKmEvtIoInternalDeviceControl` (Queue.c:150-211). Codes are `CTL_CODE(FILE_DEVICE_KEYBOARD=0x0B, fn, method, FILE_ANY_ACCESS)`.

| IOCTL | Value | Method | Handler | Result |
|---|---|---|---|---|
| `IOCTL_HID_GET_DEVICE_DESCRIPTOR` | `0x000B0003` | NEITHER | `AmtPtpGetHidDescriptor` (Hid.c:107-304) | 9-byte HID descriptor, Info=9 |
| `IOCTL_HID_GET_REPORT_DESCRIPTOR` | `0x000B0007` | NEITHER | `AmtPtpGetReportDescriptor` (Hid.c:364-531) | 501-byte report descriptor, Info=501 |
| `IOCTL_HID_READ_REPORT` | `0x000B000B` | NEITHER | `AmtPtpSpiInputRoutineWorker` (Input.c:4-36) | pended, completed later with 30-byte report |
| `IOCTL_HID_GET_STRING` | `0x000B0013` | NEITHER | `AmtPtpGetStrings` (Hid.c:533-579) | **forwarded** to lower (send-and-forget) |
| `IOCTL_HID_GET_DEVICE_ATTRIBUTES` | `0x000B0027` | NEITHER | `AmtPtpGetDeviceAttribs` (Hid.c:306-362) | 32-byte attrs, Info=32 |
| `IOCTL_HID_GET_FEATURE` | `0x000B0192` | OUT_DIRECT | `AmtPtpReportFeatures` (Hid.c:581-725) | see §5.4 |
| `IOCTL_HID_SET_FEATURE` | `0x000B0191` | IN_DIRECT | `AmtPtpSetFeatures` (Hid.c:727-861) | see §5.5 |
| `IOCTL_HID_WRITE_REPORT` | `0x000B000F` | NEITHER | — | `STATUS_NOT_SUPPORTED` (0xC00000BB) |
| `IOCTL_HID_ACTIVATE_DEVICE` | `0x000B001F` | NEITHER | — | `STATUS_NOT_SUPPORTED` |
| `IOCTL_HID_DEACTIVATE_DEVICE` | `0x000B0023` | NEITHER | — | `STATUS_NOT_SUPPORTED` |
| `IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST` | `0x000B002B` | NEITHER | — | `STATUS_NOT_SUPPORTED`, so there is **no selective suspend** |
| `IOCTL_UMDF_HID_SET_OUTPUT_REPORT` | `0x000B005B` | NEITHER | — | `STATUS_NOT_SUPPORTED` |
| `IOCTL_UMDF_HID_GET_INPUT_REPORT` | `0x000B005F` | NEITHER | — | `STATUS_NOT_SUPPORTED` |
| *any other internal IOCTL* (e.g. `IOCTL_HID_GET_INPUT_REPORT` 0x000B01A2, `IOCTL_HID_SET_OUTPUT_REPORT` 0x000B0195, `IOCTL_HID_GET_INDEXED_STRING` 0x000B01E2, `IOCTL_UMDF_HID_GET/SET_FEATURE` 0x000B0057/0x000B0053) | — | — | default branch | `STATUS_NOT_SUPPORTED`. It is **not forwarded**, which is unusual for a filter. Only GET_STRING is forwarded |

The constants `IOCTL_UMDF_HID_GET_FEATURE` and `IOCTL_UMDF_HID_SET_FEATURE` appear only in `DbgIoControlGetString`. Non-internal request types are never seen: IRP_MJ_DEVICE_CONTROL, READ, WRITE, CREATE and CLOSE all have no queue callback, so the framework **auto-forwards** them to the lower driver under the filter rule (verified on Learn).

### 4.1 `IOCTL_HID_GET_DEVICE_DESCRIPTOR` — Hid.c:107-304
1. `WdfRequestRetrieveOutputMemory(Request, &mem)`. For an internal METHOD_NEITHER IOCTL, KMDF maps this to `Irp->UserBuffer` with length = OutputBufferLength. On failure, return that status (129-144).
2. Switch on `HidProductID`:
   - `0x0275, 0x0279, 0x0290, 0x0291` → Family 1
   - `0x0272, 0x0273` → Family 2
   - `0x0276, 0x0277` → Family 3A
   - `0x0278` → Family 3B
   - default → `DescriptorFound=FALSE`
3. Call `WdfMemoryCopyFromBuffer(mem, 0, &FamilyXDefaultHidDescriptor, bLength=9)`. If the output is shorter than 9 bytes, this fails with `STATUS_BUFFER_TOO_SMALL`, which is returned immediately.
4. If found, Information = 9 and status SUCCESS. Otherwise `STATUS_NOT_FOUND` (0xC0000225), Info 0. The not-found case is unreachable in practice, because PrepareHardware rejects PIDs outside the table and the table has the same 9 PIDs.

### 4.2 `IOCTL_HID_GET_DEVICE_ATTRIBUTES` — Hid.c:306-362
1. `WdfRequestRetrieveOutputBuffer(Request, sizeof(HID_DEVICE_ATTRIBUTES)=32, &p, NULL)`. On failure (e.g. `STATUS_BUFFER_TOO_SMALL`), complete with that status.
2. Write the fields:
   - `Size = 32`
   - `VendorID = DEVICE_VID = 0x8910` (AppleDefinition.h:4). This is deliberately fake, to avoid a collision with the USB/BT drivers (comment Hid.c:343-344; commit dd364d6).
   - `ProductID = HidProductID`
   - `VersionNumber = HidVersionNumber`
   - `Reserved[11]` is **not written**. Rust should zero it; the change is harmless.
3. Information = 32, status SUCCESS.

`HID_DEVICE_ATTRIBUTES` layout (hidport.h): `u32 Size; u16 VendorID; u16 ProductID; u16 VersionNumber; u16 Reserved[11];` gives offsets 0,4,6,8,10 and 32 bytes in total.

### 4.3 `IOCTL_HID_GET_REPORT_DESCRIPTOR` — Hid.c:364-531
1. `WdfRequestRetrieveOutputMemory`; on failure, return its status.
2. Choose by `HidProductID` (same family mapping as 4.1) **and** `ReportType`:
   - PrecisionTouchpad (0) → `AmtPtpSpiFamilyXReportDescriptor`
   - Touchscreen (1) → `AmtPtpSpiFamilyXTouchscreenReportDescriptor`

   `CopiedSize = FamilyXDefaultHidDescriptor.DescriptorList[0].wReportLength`, which is `sizeof(PTP variant)` = **501** for every family and both variants.
3. If no descriptor was chosen, return `STATUS_NOT_FOUND`. If `CopiedSize==0`, return `STATUS_INVALID_DEVICE_STATE` (unreachable).
4. `WdfMemoryCopyFromBuffer(mem, 0, desc, 501)`. If the buffer is too small, return `STATUS_BUFFER_TOO_SMALL`.
5. Information = 501, SUCCESS.

### 4.4 `IOCTL_HID_GET_STRING` — Hid.c:533-579
1. `WdfRequestFormatRequestUsingCurrentType(Request)`.
2. `WDF_REQUEST_SEND_OPTIONS_INIT(&opts, WDF_REQUEST_SEND_OPTION_SEND_AND_FORGET)`.
3. `WdfRequestSend(Request, SpiTrackpadIoTarget, &opts)`.
4. Set `*Pending = TRUE`. If the send failed, `Status = WdfRequestGetStatus(Request)` and `*Pending = FALSE`, so the dispatcher completes the request with that status.

The content is not inspected. Manufacturer, product and serial strings therefore come from Apple's lower driver, while the VID is faked (0x8910).

### 4.5 `IOCTL_HID_READ_REPORT` — Input.c:4-36
1. `WdfRequestForwardToIoQueue(Request, HidQueue)`. On failure, `WdfRequestComplete(Request, status)` and return.
2. If `DeviceStatus == D0ActiveAndConfigured`, call `AmtPtpSpiInputIssueRequest(Device)` (§6.3).
3. The dispatcher **always** treats this as pending (Queue.c:182).

The upstream output buffer is not touched here. It is filled in the completion routine (§7) with 30 bytes, Information=30.

### 4.6 GET/SET_FEATURE buffer handling (METHOD_NEITHER-style HID_XFER_PACKET)
- `HID_XFER_PACKET` (hidclass.h:180-184), x64/ARM64 layout: `{ *mut u8 reportBuffer @0; u32 reportBufferLen @8; u8 reportId @12; pad }`, **sizeof = 16**.
- The packet pointer is read **raw from `WdfRequestWdmGetIrp(Request)->UserBuffer`** (Hid.c:614, 759). This is the HID minidriver contract: hidclass places the `HID_XFER_PACKET*` (a kernel pointer) in `Irp->UserBuffer`, regardless of the METHOD bits.
- GET checks `Parameters.DeviceIoControl.OutputBufferLength >= 16`, otherwise `STATUS_BUFFER_TOO_SMALL` (Hid.c:608-612).
- SET checks `InputBufferLength >= 16`, otherwise `STATUS_BUFFER_TOO_SMALL` (Hid.c:753-757).
- If `UserBuffer == NULL`, the result is `STATUS_INVALID_DEVICE_REQUEST` (0xC0000010).
- The switch is on `pHidPacket->reportId`. The handler writes or reads `pHidPacket->reportBuffer`, whose byte 0 is the report ID by HID convention.
- **Neither GET nor SET calls `WdfRequestSetInformation`**, so Information stays at its initial value (0). Replicate this.

### 4.7 Manual queue draining
`HidQueue` is drained in two places only:
- by D0Exit, which completes all queued requests with `STATUS_CANCELLED`;
- one-at-a-time by the lower-read completion routine (FIFO `WdfIoQueueRetrieveNextRequest`).

The framework also cancels queued requests itself if the issuer (hidclass) cancels them. Nothing purges the queue on surprise removal; the framework purges it on remove.

---

## 5. HID

### 5.1 Report descriptors (symbols, lengths, bytes)

All eight descriptors are **501 bytes (0x01F5)**. They are composed as `[TLC] + AAPL_PTP_WINDOWS_CONFIGURATION_TLC (47 B) + AAPL_PTP_USERMODE_CONFIGURATION_APP_TLC (23 B)`. The family TLC is 431 B and contains five finger collections: FC1, FC1, FC2, FC1, FC2, where FC1 = 67 B and FC2 = 61 B.

| Symbol (Hid.c) | Line | TLC macro (file:line) | SHA-256 of the 501 bytes |
|---|---|---|---|
| `AmtPtpSpiFamily1ReportDescriptor` | 9-13 | `AAPL_SPI_SERIES1_PTP_TLC` (HID/SpiTrackpadSeries1.h:83-137) | `21a4d11df641c918bbe5b9718c7b054f6d399b21d4296b81bd3e9f85cc617cd3` |
| `AmtPtpSpiFamily1TouchscreenReportDescriptor` | 15-19 | `AAPL_SPI_SERIES1_TOUCHSCREEN_TLC` (Series1.h:139-193) | `868c9d50ac9ca620a578e717daf8dfb215086e83e8bf175851b712a6f809b1aa` |
| `AmtPtpSpiFamily2ReportDescriptor` | 33-37 | `AAPL_SPI_SERIES2_PTP_TLC` (Series2.h:83-137) | `5dab7a973382bbada6cedb4c47e0f1d7bc2d72d263ed5fd86545f96f2f8c9a3f` |
| `AmtPtpSpiFamily2TouchscreenReportDescriptor` | 39-43 | `AAPL_SPI_SERIES2_TOUCHSCREEN_TLC` (Series2.h:139-193) | `d4d5f0b8146fa01c0421653fff153cee5ff73679576546a1e8378f44cf479bbe` |
| `AmtPtpSpiFamily3aReportDescriptor` | 57-61 | `AAPL_SPI_SERIES3_13_PTP_TLC` (Series3.h:83-137) | `fb0db27b1cdf967200ffd68b01005a15ffcebfd0e2786f8dba7bd163b324fcf9` |
| `AmtPtpSpiFamily3aTouchscreenReportDescriptor` | 63-67 | `AAPL_SPI_SERIES3_13_TOUCHSCREEN_TLC` (Series3.h:139-193) | `28d6043da39c1cc339b20f49f20059f5bf91c32573ca74c25b9fa865e01582bf` |
| `AmtPtpSpiFamily3bReportDescriptor` | 81-85 | `AAPL_SPI_SERIES3_15_PTP_TLC` (Series3.h:271-325) | `31cd97cce4b5bfafc67bb490a1b3d2f2cf0d573c03e811181e2e7b424490361b` |
| `AmtPtpSpiFamily3bTouchscreenReportDescriptor` | 87-91 | `AAPL_SPI_SERIES3_15_TOUCHSCREEN_TLC` (Series3.h:327-381) | `976e5046059319cf83acb73000237753a9d2e74e52f5f73da65b5f74b16359d7` |

Shared TLC macros: `AAPL_PTP_WINDOWS_CONFIGURATION_TLC` (Hid.h:32-57) and `AAPL_PTP_USERMODE_CONFIGURATION_APP_TLC` (Hid.h:19-30). The item-tag constants are in HidCommon.h:14-39.

The C arrays are **non-const writable globals** of type `UCHAR`. In Rust use `static [u8; 501]`.

**Full bytes of Family 1 (PTP)** are in Appendix A. The other seven differ from F1 **only** at the offsets below, so a Rust table can be built from F1 plus these patches, then checked against the SHA-256 values above.

- **Touchscreen variant of any family**: offset `[3]` changes `0x05 → 0x04` (Usage: Touch Pad → Touch Screen). Nothing else changes. The input report format is identical.
- **F1 → F2**: at each of the 5 finger collections, the bases are FC offsets 10, 81, 152, 217, 288. Patch at base+33,+34 (X LogMax), +44 (X PhysMax lo), +51,+52 (Y PhysMax), +54,+55 (Y LogMax). Concretely: `[43]aa→2e [44]29→27 [54]5e→28 [61]c7→05 [62]02→03 [64]af→e0 [65]1e→1a`, and the same at `+71`, `+142`, `+207`, `+278`, i.e. offsets `{114,115,125,132,133,135,136}`, `{185,186,196,203,204,206,207}`, `{250,251,261,268,269,271,272}`, `{321,322,332,339,340,342,343}`.
- **F1 → F3A**: `[43]aa→c0 [44]29→32 [54]5e→48 [55]04→05 [61]c7→44 [62]02→03` (Y LogMax unchanged at 0x1EAF), repeated at +71, +142, +207, +278.
- **F1 → F3B**: `[43]aa→48 [44]29→3c [54]5e→30 [55]04→06 [61]c7→e0 [62]02→03 [64]af→e6 [65]1e→24`, repeated at +71, +142, +207, +278.

Per-family coordinate items:

| Family | X LogMax | X PhysMax (cm×100) | Y PhysMax | Y LogMax |
|---|---|---|---|---|
| 1 (Series1) | 10666 (0x29AA) | 1118 (0x045E) | 711 (0x02C7) | 7855 (0x1EAF) **(mismatch; see §8/§10)** |
| 2 (Series2) | 10030 (0x272E) | 1064 (0x0428) | 773 (0x0305) | 6880 (0x1AE0) |
| 3A (Series3_13) | 12992 (0x32C0) | 1352 (0x0548) | 836 (0x0344) | 7855 (0x1EAF) |
| 3B (Series3_15) | 15432 (0x3C48) | 1584 (0x0630) | 992 (0x03E0) | 9446 (0x24E6) |

Unit for X/Y is `UNIT 0x11` (SI linear, length cm), `UNIT_EXPONENT 0x0E` (−2). Logical minimum and physical minimum are never set, so they default to 0.

### 5.2 Descriptor semantics: reports defined

| Report ID | Type | TLC | Payload after ID | Driver handling |
|---|---|---|---|---|
| **0x05** `REPORTID_MULTITOUCH` | Input | Digitizer Touch Pad (0x0D/0x05), or Touch Screen (0x04) | 5 × [1 byte: bit0 Confidence (0x47), bit1 Tip Switch (0x42), bits2-4 Contact Identifier (0x51, **LogMax 3**), bits5-7 const] [X u16] [Y u16], then Scan Time u16 (0x56, Unit seconds exp −4, LogMax 0xFFFF), Contact Count u8 (0x54, LogMax 0x7F), Button 1 (Button page 0x09, 1 bit) + 7 const bits. 29 bytes, **30 with ID** | produced in §7 |
| **0x07** `REPORTID_DEVICE_CAPS` | Feature | same TLC | Maximum Contacts (0x55) u8, Touchpad Button Type (0x59) u8. 3 bytes with ID | GET only |
| **0x08** `REPORTID_PTPHQA` | Feature | same TLC (Vendor page 0xFF00, usage 0xC5) | 256 bytes. **257 with ID** | GET only |
| **0x04** `REPORTID_REPORTMODE` | Feature | Digitizer Configuration (0x0D/0x0E), logical collection under Usage Finger (0x22) | Input Mode (0x52) u8, LogMin 0, LogMax `MAX_FINGERS`=16 (sic). 2 bytes with ID | SET only |
| **0x06** `REPORTID_FUNCSWITCH` | Feature | same Configuration TLC, physical collection | bit0 Button Switch (0x57), bit1 Surface Switch (0x58), 6 const bits. 2 bytes with ID | SET only |
| **0x09** `REPORTID_UMAPP_CONF` | Feature | Vendor 0xFF00 usage 0x01 application collection | 3 × u8 (LogMax 255). 4 bytes with ID | **NOT handled**: GET and SET both return `STATUS_NOT_SUPPORTED` |

Report ID 0x02 (`REPORTID_STANDARDMOUSE`, HidCommon.h:3) is **not** declared in our descriptors. It is used only **downstream** as Apple's feature report (§6.2).

**N/A:**
- There is **no latency-mode feature report** (no PTP Latency Mode usage, no handler).
- There is **no battery report**.
- There is **no mouse-collection fallback report**. Input mode 0 (mouse) is accepted, but nothing changes (§5.5).
- There are **no output reports**: WRITE_REPORT and SET_OUTPUT_REPORT give NOT_SUPPORTED.

Descriptor quirks. Preserve them byte-exactly; do not "clean up".
- `AAPL_SPI_SERIES*_PTP_FINGER_COLLECTION_2` (e.g. Series1.h:47-81) lacks the trailing `PHYSICAL_MAXIMUM 0 / UNIT_EXPONENT 0 / UNIT 0` reset (`45 00 55 00 65 00`) that FC1 has (Series1.h:41-43).
  - After finger 3, unit = cm, exponent = −2 and PhysMax = Y-phys stay in effect for finger 4's Confidence, Tip, ContactID and padding bits.
  - After finger 5, the Scan Time block overrides all three, so there is no further effect.
- HID global items persist across collections and top-level collections, because only Push/Pop resets them. After Scan Time, the globals `UNIT=seconds`, `UNIT_EXPONENT=−4` and `PHYSICAL_MAXIMUM=65535` are never reset. They stay in effect for Contact Count, Button, features 7 and 8, **and** for the Configuration TLC (reports 4 and 6) and the UM TLC (report 9). `LOGICAL_MINIMUM` stays 0 throughout.
- The Contact Identifier logical max is 3, but the driver emits IDs 0..4 (§7). This is a bug; §10 lists it.
- The Input Mode logical maximum is `MAX_FINGERS` (16), which is semantically odd.

### 5.3 HID descriptor (9 bytes) — `AmtPtpSpiFamily{1,2,3a,3b}DefaultHidDescriptor` (Hid.c:21-31, 45-55, 69-79, 93-103)
`HID_DESCRIPTOR` is packed (`pshpack1`, hidport.h:126-144), size 9:
```
bLength=0x09, bDescriptorType=0x21, bcdHID=0x0100, bCountry=0x00, bNumDescriptors=0x01,
DescriptorList[0] = { bReportType=0x22, wReportLength=sizeof(FamilyXReportDescriptor)=0x01F5 }
bytes: 09 21 00 01 00 01 22 F5 01
```
All four families produce identical bytes. Note that `wReportLength` sits at **unaligned offset 7**.

### 5.4 GET_FEATURE (`AmtPtpReportFeatures`, Hid.c:581-725)
Run the validation in §4.6 first, then switch on `reportId`:
- **7 (DEVICE_CAPS)** (623-668):
  1. If `reportBufferLen < 3` (`sizeof(PTP_DEVICE_CAPS_FEATURE_REPORT)`, Hid.h:103-107), return `STATUS_INVALID_BUFFER_SIZE` (0xC0000206).
  2. Write `reportBuffer[1] = PTP_MAX_CONTACT_POINTS = 5`, `reportBuffer[2] = PTP_BUTTON_TYPE_CLICK_PAD = 0`, `reportBuffer[0] = 7`.
  3. SUCCESS, Information not set.
- **8 (PTPHQA)** (669-702):
  1. If `reportBufferLen < 257` (`sizeof(PTP_DEVICE_HQA_CERTIFICATION_REPORT)`, Hid.h:109-112), return `STATUS_INVALID_BUFFER_SIZE`.
  2. `*certReport->CertificationBlob = DEFAULT_PTP_HQA_BLOB;` (692). **BUG (comma operator):** this parses as `(blob[0] = 0xFC), 0x28, 0xFE, …;`, so **only `reportBuffer[1] = 0xFC` is written**. Bytes `[2..257)` keep whatever the caller's buffer contained, normally zero.
  3. Write `reportBuffer[0] = 8`. SUCCESS, Information not set.

  The same bug exists in the sibling drivers (`AmtPtpDeviceUsbKm/Hid.c:388`, `AmtPtpDeviceUsbUm/Hid.c:822`, `AmtPtpHidFilter/Hid.c:245`). The intended blob is in Hid.h:59-91: 256 bytes, first `fc 28 fe 84`, last `a5 e5 24 c2`, SHA-256 `b57b851d567808906f61a0122273da05c972140f1007355390aaf5dda8a072af`. **Porting decision required:**
  - exact behavior: write only 0xFC;
  - intended behavior: copy all 256 bytes.

  Copying the full blob is what Windows' PTP HQA check expects. Whichever option is chosen, flag it in the PR.
- **default**, including 9, 4 and 6: `STATUS_NOT_SUPPORTED`.

### 5.5 SET_FEATURE (`AmtPtpSetFeatures`, Hid.c:727-861)
Run the validation in §4.6 first. **`reportBufferLen` is never checked.**
- **4 (REPORTMODE)** (768-810):
  - Read `Mode = reportBuffer[1]` (`PTP_DEVICE_INPUT_MODE_REPORT{u8 ReportID; u8 Mode}`, Hid.h:114-117).
  - `0` (`PTP_COLLECTION_MOUSE`) sets `PtpInputOn = FALSE`. `3` (`PTP_COLLECTION_WINDOWS`) sets `PtpInputOn = TRUE`. Any other value is ignored.
  - Always SUCCESS.
- **6 (FUNCSWITCH)** (811-838):
  - The code uses `PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` (Hid.h:119-128, packed): `{u8 ReportID; u8 DeviceMode; u8 ButtonReport:1, SurfaceReport:1, Padding:6}`, 3 bytes.
  - It sets `PtpReportButton = reportBuffer[2] & 1` and `PtpReportTouch = (reportBuffer[2] >> 1) & 1`.
  - **BUG:** the descriptor's report 6 is 2 bytes (ID + 1 bitfield byte), so the bits are really at `reportBuffer[1]`. The struct has a spurious `DeviceMode` byte, so the code reads `reportBuffer[2]`, one byte past a 2-byte report: an out-of-bounds read of the caller's buffer.
  - Always SUCCESS.
- **default**: `STATUS_NOT_SUPPORTED`.

**Consequence:** `PtpInputOn`, `PtpReportButton` and `PtpReportTouch` are **never read anywhere**. Input mode and selective reporting have **no effect**: touch and button data are always reported once the device is configured. To keep behavior identical, Rust can keep these as write-only fields or drop them. If the FUNCSWITCH read is kept, read byte 1 or guard by `reportBufferLen`; the value is unused either way.

### 5.6 Strings
These are not produced by this driver. `IOCTL_HID_GET_STRING` is forwarded verbatim (§4.4). The string IDs (`HID_STRING_ID_IMANUFACTURER=14`, `IPRODUCT=15`, `ISERIALNUMBER=16`) are interpreted by the lower driver.

---

## 6. Device I/O to the hardware

### 6.1 Things that do not exist here (N/A)
- **USB** pipes, interfaces, continuous reader and control transfers: none. In particular there is no "wellspring mode" setup packet.
- **SPI** bus access: none directly. Hardware I/O is exactly **three kinds of internal IOCTLs** sent to the local I/O target (the Apple PDO):
  1. `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (synchronous, PrepareHardware)
  2. `IOCTL_HID_SET_FEATURE` (synchronous, SetState)
  3. `IOCTL_HID_READ_REPORT` (asynchronous, one per upstream read)

  None of them uses a timeout. In addition, `IOCTL_HID_GET_STRING` requests are forwarded.
- **Selective suspend / idle / wake**: none. Idle notification gets `STATUS_NOT_SUPPORTED`, and there is no S0Idle or SxWake setting.
- **Work items**: none of the driver's own. The passive-level timer is executed by the framework in a work item.
- **Interrupts, DMA, register access**: none. The resource lists in PrepareHardware are ignored.

### 6.2 Mode switch: `AmtPtpSpiSetState(Device, TRUE)` — Device.c:457-539
This is the only "configure the hardware" action. It enables multitouch reporting on Apple's SPI trackpad. Procedure:
1. `UCHAR HidPacketBuffer[255]` (`HID_XFER_PACKET_SIZE`, AppleDefinition.h:50) on the **stack**, zeroed.
2. Treat the start of the buffer as a `HID_XFER_PACKET`:
   - `reportId = HID_REPORTID_MOUSE = 2` (AppleDefinition.h:49)
   - `reportBufferLen = sizeof(SPI_SET_FEATURE) = 2`
   - `reportBuffer = HidPacketBuffer + sizeof(HID_XFER_PACKET)` (= +16 on x64/ARM64)
3. `SPI_SET_FEATURE` (AppleDefinition.h:44-47) is `{u8 BusLocation; u8 Status}`: `BusLocation = 2` (this is really the report-ID byte), `Status = DesiredState ? 1 : 0`.
4. Resulting 255-byte input buffer on x64/ARM64:

   | Offset | Bytes | Meaning |
   |---|---|---|
   | 0..8 | `&HidPacketBuffer[16]` (kernel VA, LE) | `reportBuffer` — **self-pointer into the same buffer** |
   | 8..12 | `02 00 00 00` | `reportBufferLen = 2` |
   | 12 | `02` | `reportId = 2` |
   | 13..16 | `00 00 00` | padding |
   | 16 | `02` | report byte 0 (report ID / "BusLocation") |
   | 17 | `01` (`00` for FALSE) | report byte 1 (enable) |
   | 18..255 | `00…` | zero |

5. `WdfIoTargetSendInternalIoctlSynchronously(SpiTrackpadIoTarget, NULL, IOCTL_HID_SET_FEATURE (0x000B0191), &InputDescriptor{buffer, 255}, NULL, NULL, NULL)`. There is no output, no send options and **no timeout**. The comment at line 500 is "Will non-internal IOCTL work?".
6. The status of the send is returned.

In Rust, use a `#[repr(C, align(8))]` buffer object and keep it **pinned and alive until the synchronous send returns**, because it contains a pointer to itself. Do not rely on stack-array alignment, which C does here (UCHAR array cast to `PHID_XFER_PACKET`). Use the **same KMDF DDI** (`WdfIoTargetSendInternalIoctlSynchronously` with an input memory descriptor of length 255).

**IRP placement, verified in WDF source** (`src/framework/shared/targets/general/km/fxiotargetkm.cpp`, `FxIoTarget::FormatIoctlRequest`):
- `IOCTL_HID_SET_FEATURE` is METHOD_IN_DIRECT. KMDF sets `Irp->AssociatedIrp.SystemBuffer` to **the caller's 255-byte buffer itself**; there is no copy, so the self-pointer at offset 0 points into the same memory. `InputBufferLength` is 255 and `OutputBufferLength` is 0. There is no output MDL, and `Irp->UserBuffer` is not set.
- So Apple's lower driver receives the `HID_XFER_PACKET` via **SystemBuffer**. That is not the hidclass→minidriver `Irp->UserBuffer` convention that this driver itself relies on upstream (§4.6).
- The METHOD_NEITHER sends work differently. GET_DEVICE_ATTRIBUTES (§2.8) and READ_REPORT (§6.3) put the output buffer in `Irp->UserBuffer` and the input, here NULL, in `Parameters.DeviceIoControl.Type3InputBuffer`.

If the Rust port ever hand-builds these IRPs instead of using the DDI, it must reproduce this placement exactly.

### 6.3 Lower read: `AmtPtpSpiInputIssueRequest` — Input.c:38-155 (IRQL ≤ DISPATCH)
1. `WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attr, WORKER_REQUEST_CONTEXT)`, `attr.ParentObject = Device`, then `WdfRequestCreate(&attr, SpiTrackpadIoTarget, &req)`. On failure, return silently (52-71).
2. `WdfMemoryCreateFromLookaside(HidReadBufferLookaside, &mem)` gives a 1024-byte NonPagedPoolNx buffer that is **not zeroed** and may hold data from an earlier read. On failure, delete `req` and return (73-89).
3. Context: `{DeviceContext = pDeviceContext, RequestMemory = mem}` (92-94).
4. `WdfIoTargetFormatRequestForInternalIoctl(target, req, IOCTL_HID_READ_REPORT (0x000B000B), NULL, NULL, mem, NULL)`, i.e. output = the whole 1024 bytes and no input. On failure, delete `mem` and `req` (97-125).
5. `WdfRequestSetCompletionRoutine(req, AmtPtpRequestCompletionRoutine, ctx)` (127-131).
6. `WdfRequestSend(req, target, NULL)` sends asynchronously with no options and no timeout. If it returns FALSE, delete `mem` and `req`. The upstream request already in `HidQueue` stays queued and is served by some later completion (139-154).

### 6.4 Read pairing and backlog (behavior to preserve or consciously change)
- There is **one lower read per upstream READ_REPORT**, issued only if `DeviceStatus == D0ActiveAndConfigured` at the moment the upstream read arrives (Input.c:33).
- Each lower completion serves the **oldest** queued upstream read (FIFO). It is not necessarily the one that triggered it.
- Reads that arrive while the device is unconfigured issue **no** lower read. SMIO-init/restart success issues none either. Recovery-timer success issues **exactly one**.

  So with N reads queued during the unconfigured period, a backlog of N−1 requests persists. The system stays live because each completion makes hidclass resubmit, which issues a new lower read. Each report is still delivered to the oldest pending request.
- If a lower read completes while `HidQueue` is empty (the upstream read was cancelled, or D0Exit drained it), the data is **dropped** and no replacement read is issued (Input.c:187-197).
- D0Exit does not cancel in-flight lower reads. The local I/O target's power behavior is the KMDF default, and the lower driver normally completes or cancels its own reads.

### 6.5 Power transitions summary

| Event | Actions |
|---|---|
| PrepareHardware | lower GET_DEVICE_ATTRIBUTES, table lookup, registry read |
| D0Entry | `DeviceStatus=Unconfigured`; `LastReportTime=QPC frequency` (sic, §2.9) |
| SelfManagedIoInit / Restart | `SetState(TRUE)`. On success, `Configured` + `LastReportTime=QPC frequency`. On failure, a 5 s one-shot timer |
| Timer (5 s) | `SetState(TRUE)`. On success, issue 1 lower read, then `Configured`. On failure, nothing more |
| D0Exit | `DeviceStatus=D3`; complete all `HidQueue` requests with `STATUS_CANCELLED` |
| ReleaseHardware / Surprise removal | not handled (framework defaults) |

---

## 7. Input parsing (Apple packet → PTP report)

### 7.1 Raw packet layout: `SPI_TRACKPAD_PACKET` (AppleDefinition.h:25-42)
This is undocumented Apple format. All multibyte fields are little-endian. The struct is naturally aligned with **no padding**: sizeof = 346, header = 46 bytes.

| Offset | Size | Field | Used? |
|---|---|---|---|
| 0 | 1 | `PacketType` | no |
| 1 | 1 | `ClickOccurred` | **yes** → button |
| 2 | 5 | `Reserved0` | no |
| 7 | 1 | `IsFinger` | no |
| 8 | 16 | `Reserved1` | no |
| 24 | 1 | `FingerDataLength` | no |
| 25 | 5 | `Reserved2` | no |
| 30 | 1 | `NumOfFingers` | **yes** |
| 31 | 1 | `ClickOccurred2` | no |
| 32 | 1 | `State1` | no |
| 33 | 1 | `State2` | no |
| 34 | 1 | `State3` | no |
| 35 | 1 | `Padding` | no |
| 36 | 10 | `Reserved3` | no |
| 46 | 10×30 | `Fingers[SPI_TRACKPAD_MAX_FINGERS=10]` | first ≤5 |

Finger i is at `46 + 30*i`. `SPI_TRACKPAD_FINGER` (AppleDefinition.h:6-23) is 15 × `i16` (SHORT, **signed**), 30 bytes:

| Off | Field | Used? |
|---|---|---|
| 0 | OriginalX | trace only |
| 2 | OriginalY | trace only |
| 4 | **X** | yes |
| 6 | **Y** | yes |
| 8 | HorizontalAccel | no |
| 10 | VerticalAccel | no |
| 12 | ToolMajor | no |
| 14 | ToolMinor | no |
| 16 | Orientation | no |
| 18 | **TouchMajor** | yes (confidence) |
| 20 | **TouchMinor** | yes (confidence) |
| 22 | Rsvd1 | no |
| 24 | Rsvd2 | no |
| 26 | **Pressure** | yes (tip switch) |
| 28 | Rsvd3 | no |

There is only **one packet format for all families**. Families differ only in `TrackpadInfo` (XMin/YMax) and in the report descriptor.

### 7.2 Output: `PTP_REPORT` (Hid.h:167-173) — 30 bytes, no padding
`PTP_CONTACT` is `#pragma pack(1)` (Hid.h:130-140), 5 bytes:
- byte 0 bits, LSB-first per MSVC (verified): bit0 `Confidence`, bit1 `TipSwitch`, bits2-4 `ContactID` (3 bits), bits5-7 `Padding`;
- `X` u16 @1;
- `Y` u16 @3.

`PTP_REPORT` is declared **outside** the pack(1) region but has no padding in practice: `ReportID` u8 @0, `Contacts[5]` @1..26, `ScanTime` u16 @26, `ContactCount` u8 @28, `IsButtonClicked` u8 @29, sizeof 30. This matches report 0x05 in the descriptor.

### 7.3 Algorithm: `AmtPtpRequestCompletionRoutine` (Input.c:157-321)
Inputs: the lower request `SpiRequest`, `Params`, and `Context = &WORKER_REQUEST_CONTEXT`. The routine runs at IRQL ≤ DISPATCH_LEVEL and may run concurrently on several CPUs if several lower reads are in flight.

1. `ctx = Context`, `dev = ctx->DeviceContext` (182-183).
2. `WdfIoQueueRetrieveNextRequest(dev->HidQueue, &PtpRequest)`. On failure (the queue is empty), go to **cleanup** without completing anything; the data is discarded (187-197).
3. `len = (LONG)WdfRequestGetInformation(SpiRequest)`, the bytes returned by the lower driver. `pkt = WdfMemoryGetBuffer(Params->Parameters.Ioctl.Output.Buffer, NULL)` (199-200).
   - `Params->IoStatus.Status` is **never checked**. A cancelled or failed lower read normally has Information=0 and falls into the next step.
4. If `len < 46`, set `Status = STATUS_DEVICE_DATA_ERROR` (0xC000009C) and go to **exit**, which completes `PtpRequest` with it, Information unchanged (0). The trace says "Attempt to re-enable the device", but **no re-enable happens** (203-213).
   - There is **no further length check**: fingers are read up to `46 + 30*min(NumOfFingers,5)` bytes regardless of `len`. The read stays inside the 1024-byte buffer, but it may consume stale lookaside bytes.
5. Scan time (216-221). **The C source is `KeQueryPerformanceCounter(&CurrentCounter);`, which passes the frequency out-parameter and discards the returned counter:**
   ```
   (void)KeQueryPerformanceCounter(&cur)            // cur := QPC FREQUENCY F (constant since boot), NOT a timestamp
   delta = (cur - dev->LastReportTime) / 100        // i64, truncating division
   dev->LastReportTime = cur                        // = F again
   ```
   `LastReportTime` was also set to F by D0Entry, and by SMIO on success. D0Entry always runs before any lower read can be issued, so at every completion `cur == LastReportTime == F` and **`delta == 0`, always**. (The KMDF context is zeroed, but no completion can happen before D0Entry.)
6. Report header (224-226). `PtpReport` is an **uninitialized stack variable** (173).
   ```
   ReportID        = 0x05
   ContactCount    = pkt[30]  (NumOfFingers, raw u8, NOT clamped)
   IsButtonClicked = pkt[1]   (ClickOccurred, raw u8, whole byte copied; bit0 = Button1, bits1-7 land in the descriptor's const padding)
   ```
7. Contacts (228-255):
   ```
   n = min(NumOfFingers, 5)               // u8
   for i in 0..n:
       f = Fingers[i]
       Contacts[i].ContactID  = i (low 3 bits)          // index, NOT a tracking ID; 0..4
       dx = (i32)f.X - (i32)XMin                         // int promotion, no overflow
       Contacts[i].X          = dx > 0 ? (u16)dx : 0     // no upper clamp
       dy = (i32)YMax - (i32)f.Y
       Contacts[i].Y          = dy > 0 ? (u16)dy : 0     // Y is INVERTED; no upper clamp
       Contacts[i].TipSwitch  = (f.Pressure > 0) ? 1 : 0          // signed compare
       Contacts[i].Confidence = (f.TouchMajor < 2500 && f.TouchMinor < 2500) ? 1 : 0   // signed compare; negatives => confident
   ```
   - The bitfields are assigned individually, which is a read-modify-write on an uninitialized byte, so the `Padding` bits 5-7 hold **stack garbage**.
   - Contacts `[n..5)` are **entirely uninitialized** stack garbage.
   - XMax and YMin are never used.
   - The magic numbers are `5`, the contact cap (= `PTP_MAX_CONTACT_POINTS`, used here as a literal at Input.c:228), and `2500`, the palm/size threshold, compared in raw Apple units. That threshold is the **only palm rejection**: it clears Confidence and nothing else.
8. Scan time store (257-264):
   ```
   ScanTime = (delta >= 0xFF) ? 0xFF : (u16)delta
   ```
   - Because `delta` is always 0 (step 5), **the shipped driver always reports `ScanTime = 0x0000`**. The cap and the cast below are dead in practice.
   - The cap is **0xFF**, not 0xFFFF. A negative `delta` would convert as `(u16)(i64)` and wrap. That cannot happen now, because every writer stores the same constant.
   - Had the counter been read correctly (`cur = KeQueryPerformanceCounter(NULL)`), `delta` would be in units of 10 µs at the usual 10 MHz QPC. The value would then saturate at 2.55 ms, so it would be 255 on nearly every report.
   - The PTP spec expects a monotonically increasing 100 µs counter. This is a bug; §10 lists it as B8.
   - **Porting decision required:**
     - **Faithful:** always emit `ScanTime = 0`. No QPC read is needed, although keeping the `LastReportTime` writes is harmless.
     - **Intended:** a real u16 counter in 100 µs units, e.g. `((counter as u128 * 10_000) / freq as u128) as u16`, wrapping. Use u128 or delta-based math: `i64` overflows after about 2.9 years of uptime at 10 MHz. Read the frequency once, ideally cache it at start, and use the return value of `KeQueryPerformanceCounter`.

     Flag the choice in the PR.
9. Copy out (266-306):
   - `WdfRequestRetrieveOutputMemory(PtpRequest)`. On failure, go to exit with that status.
   - `WdfMemoryCopyFromBuffer(mem, 0, &PtpReport, 30)`. If the buffer is under 30 bytes, `STATUS_BUFFER_TOO_SMALL` and go to exit.
   - `WdfRequestSetInformation(PtpRequest, 30)`.
10. **exit:** `WdfRequestComplete(PtpRequest, Status)` (308-312).
11. **cleanup** (314-320):
    ```
    WdfObjectDelete(SpiRequest);              // frees SpiRequest AND its context
    if (RequestContext->RequestMemory != NULL)  // <-- USE-AFTER-FREE: RequestContext is SpiRequest's context
        WdfObjectDelete(RequestContext->RequestMemory);
    ```
    In Rust, read `RequestMemory` **before** deleting the request, then delete the request, then delete the memory. That returns the buffer to the lookaside list.

**State kept across reports:** only `LastReportTime`, and it is effectively constant (the QPC frequency), so in practice **no state** carries from one report to the next. There is no contact history, no ID mapping, no defuzz (the `PTP_CONTACT_RAW*` types are unused) and no button debounce.

Contact ID assignment is **positional**: when finger 0 lifts, the remaining fingers shift down and change IDs. A faithful port preserves this. Contact count is limited to 5 contacts **filled**, but `ContactCount` is reported raw: it can be 6..10, and up to 255 if the packet is malformed.

### 7.4 Worked example (Family 2, PID 0x0272: XMin=−4750, YMax=6730)
The raw finger has X=100, Y=2000, Pressure=30, TouchMajor=800, TouchMinor=600.
- X = 100+4750 = **4850**
- Y = 6730−2000 = **4730**
- Tip = 1, Conf = 1, ID = 0

Contact byte 0 = `0b000_000_1_1` = `0x03` (plus garbage bits 5-7), followed by `F2 12 7A 12`.

---

## 8. Device support table

`SpiTrackpadConfigTable` (AppleDefinition.h:52-71) is `static const` in a header, so each TU gets its own copy; this is harmless. It is terminated by `{0}`. The family column is the mapping in Hid.c:147-282 and 403-475.

| VID | PID | Model (comment) | XMin | XMax | YMin | YMax | X span | Y span | Family / descriptor | Descriptor X LogMax / Y LogMax | Match? |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 0x05AC | 0x0272 | MacBookPro11,1 / 12,1 | −4750 | 5280 | −150 | 6730 | 10030 | 6880 | 2 | 10030 / 6880 | ✓ |
| 0x05AC | 0x0273 | MacBookPro11,1 / 12,1 | −4750 | 5280 | −150 | 6730 | 10030 | 6880 | 2 | 10030 / 6880 | ✓ |
| 0x05AC | 0x0275 | MacBook9 | −5087 | 5579 | −128 | 6089 | 10666 | **6217** | 1 | 10666 / **7855** | **Y mismatch** |
| 0x05AC | 0x0276 | MacBookPro14,1 / 14,2 | −6243 | 6749 | −170 | 7685 | 12992 | 7855 | 3A | 12992 / 7855 | ✓ |
| 0x05AC | 0x0277 | MacBookPro14,1 / 14,2 | −6243 | 6749 | −170 | 7685 | 12992 | 7855 | 3A | 12992 / 7855 | ✓ |
| 0x05AC | 0x0278 | MacBookPro14,3 | −7456 | 7976 | −163 | 9283 | 15432 | 9446 | 3B | 15432 / 9446 | ✓ |
| 0x05AC | 0x0279 | MacBook10 | −5087 | 5579 | −128 | 6089 | 10666 | **6217** | 1 | 10666 / **7855** | **Y mismatch** |
| 0x05AC | 0x0290 | MacBookAir7,2 fallback | −5087 | 5579 | −128 | 6089 | 10666 | **6217** | 1 | 10666 / **7855** | **Y mismatch** |
| 0x05AC | 0x0291 | MacBookAir7,2 fallback | −5087 | 5579 | −128 | 6089 | 10666 | **6217** | 1 | 10666 / **7855** | **Y mismatch** |

Notes:
- **INF lists `SPI\VID_05ac&PID_0280&MI_02`** (INF:82), but the table has no 0x0280. PrepareHardware returns `STATUS_NOT_FOUND`, so such a device would install and then fail to start (Code 10).
- The Family 1 Y logical max of 7855 (Series1.h:38,77) appears to be copied from Series 3A. Reported Y spans only 0..6217, about 79 % of the declared logical range, so the host's physical scaling (711 cm·100) is off for these models. Treat it as a bug; §10 lists it. Preserve it unless the port explicitly fixes it.
- File comments are inconsistent with the table: Series1.h:1 "MacBook8, 9, 10", Series2.h:1 "MacBookPro 10, 11", Hid.c:212 "MacBookPro 13, 14 (13-inch)". The table above is authoritative.

---

## 9. Registry and configuration

- **`DesiredReportType`** (REG_DWORD expected). It is read in PrepareHardware (Device.c:267-306), via `WdfDriverOpenParametersRegistryKey(Driver, KEY_READ)`, from `HKLM\SYSTEM\CurrentControlSet\Services\AmtPtpDeviceSpiKm\Parameters`. This is **driver-wide, not per device**.
  - `WdfRegistryQueryValue(key, L"DesiredReportType", sizeof(ULONG)=4, &v, &Length, &ValueType)`. `ValueType` and `Length` are **not checked**, so any value type of **≤ 4 bytes** is accepted. Values larger than 4 bytes give STATUS_BUFFER_OVERFLOW and are ignored.
  - **Uninitialized read:** the locals are `ULONG DesiredReportTypeValue, Length, ValueType = 0;` (Device.c:184), so only `ValueType` is initialized. A value of 1–3 bytes, such as a short REG_BINARY or a REG_SZ `""` (2 bytes), succeeds but fills only the low bytes, leaving the rest as stack garbage. Accept or ignore is then effectively random. **Rust: zero-initialize `v`.** That matches C for every well-formed REG_DWORD (new item B24).
  - `0` → `ReportType=PrecisionTouchpad`, `1` → `Touchscreen`, other → ignored (local status INVALID_PARAMETER, then discarded).
  - If the key or value is missing, `ReportType` is **unchanged**: 0 on first start because the context is zeroed, or the previous value on a re-start of the same WDFDEVICE.
  - The key is closed with `WdfRegistryClose`. The function returns STATUS_SUCCESS regardless.
  - The INF does not create this value, and nothing else in the repository writes it.
  - Effect: it only changes descriptor byte 3 (Touch Pad vs Touch Screen usage). The input report bytes are identical.
- **User-mode configuration interface: none functional.**
  - Feature report 9 (`REPORTID_UMAPP_CONF`) is declared in the descriptor (Hid.h:19-30), but GET and SET return `STATUS_NOT_SUPPORTED`. The USB UM driver does implement it (`AmtPtpDeviceUsbUm/Hid.c:837,1074`).
  - The device interface `{f271e39d-d211-4a6e-a439-63e51940d210}` is registered, but there is no create or IOCTL handler. As a lower filter, I/O on an interface handle enters at the top of the stack (hidclass) anyway. It is effectively dead; keep it only for compatibility.

---

## 10. Error paths, edge cases, magic numbers, dead code, suspected bugs

### 10.1 Magic numbers

| Value | Where | Meaning |
|---|---|---|
| `0x8910` | AppleDefinition.h:4 (`DEVICE_VID`), Hid.c:345 | fake VendorID reported upward |
| `2` | AppleDefinition.h:49, Device.c:491,497 | Apple feature report ID and report byte 0 |
| `255` | AppleDefinition.h:50, Device.c:465,488 | SetState xfer buffer size |
| `1024` | Hid.h:15 (`REPORT_BUFFER_SIZE`), Device.c:85 | lower read buffer size |
| `46` | Input.c:203 | minimum packet length = header size |
| `5` | Input.c:228; Hid.h:93 | max contacts filled / caps report |
| `2500` | Input.c:242-243 | TouchMajor/TouchMinor confidence threshold |
| `100` | Input.c:220 | QPC delta divisor (assumes a frequency). Inert: delta is always 0 (§7.3) |
| `0xFF` | Input.c:257-259 | ScanTime cap. Inert: ScanTime is always 0 (§7.3) |
| `5 s` | Device.c:414 | recovery delay |
| `16` | Hid.h:17 (`MAX_FINGERS`) | Input Mode logical max in the descriptor |
| `'LTPA'` | Driver.h:43 | pool tag |

### 10.2 Status codes returned
`STATUS_NOT_SUPPORTED` 0xC00000BB · `STATUS_NOT_FOUND` 0xC0000225 · `STATUS_INVALID_DEVICE_STATE` 0xC0000184 · `STATUS_BUFFER_TOO_SMALL` 0xC0000023 · `STATUS_INVALID_BUFFER_SIZE` 0xC0000206 · `STATUS_INVALID_DEVICE_REQUEST` 0xC0000010 · `STATUS_DEVICE_DATA_ERROR` 0xC000009C · `STATUS_CANCELLED` 0xC0000120 · (internal only) `STATUS_INVALID_PARAMETER` 0xC000000D.

### 10.3 Suspected bugs (flagged, NOT fixed)

| # | Location | Issue |
|---|---|---|
| B1 | Hid.c:692 | HQA blob: the comma operator writes only byte 0xFC (§5.4) |
| B2 | Hid.c:819-821 + Hid.h:121-127 | FUNCSWITCH reads byte 2 of a 2-byte report (OOB). The struct has a spurious `DeviceMode` byte |
| B3 | Hid.c:787-821; Device.h:73-75 | Input-mode and selective-report flags are written but never read, so they have no effect |
| B4 | Hid.c:608-850 | GET/SET_FEATURE never set Information. SET never validates `reportBufferLen` |
| B5 | Input.c:173,228-255 | `PtpReport` is uninitialized: unused contacts and padding bits leak stack bytes to user mode via HID. **The one intentional divergence: Rust must zero the report.** It is observationally irrelevant to hosts honoring ContactCount, but it closes an info leak |
| B6 | Input.c:225 | `ContactCount` is the raw `NumOfFingers`, unclamped (can exceed 5, and the descriptor LogMax 127) |
| B7 | Input.c:231 | ContactID is the array index (no tracking), and ID 4 exceeds the descriptor LogMax 3 |
| B8 | Input.c:216-264; Device.c:342,421 | ScanTime: `KeQueryPerformanceCounter(&x)` writes the **frequency** into `x` and the counter is discarded, so `delta` is always 0 and **ScanTime is always 0**. The `/100` and the 0xFF cap would be wrong even if the counter were read. It is not a running counter. Porting decision is in §7.3 step 8 |
| B9 | Input.c:199-213 | Lower `IoStatus.Status` is ignored. Only `len >= 46` is checked; finger bytes beyond `len` are read from stale lookaside memory. The "re-enable" log has no action |
| B10 | Input.c:317-319 | **Use-after-free**: the request context is read after `WdfObjectDelete(SpiRequest)` |
| B11 | Input.c:187-197 | A lower completion with no queued upstream request drops the data and issues no new read |
| B12 | Device.c:414, 541-562 | Recovery is a single retry, and it issues only one lower read for N queued requests. The timer is not stopped on D0Exit. It may fire in D3: the local target is not stopped on D0Exit, so SET_FEATURE reaches the lower device, and on success the callback sets Configured. A stale pending timer also fires after a later successful SMIO-restart (§2.12). `STATE_SWITCH_MAX_RETRIES` is unused |
| B13 | Device.c:397-427 | SMIO restart success does not issue reads for already-queued requests (usually none after D0Exit drained them) |
| B14 | Device.c:353-395 | D0Exit does not power the trackpad off (`SetState(FALSE)` is never called) and does not cancel in-flight lower reads |
| B15 | Device.c:339,371,420,558; Input.c:33,220-221 | `DeviceStatus` and `LastReportTime` are accessed without a lock or atomics from DISPATCH completions, passive callbacks and the timer: a data race. For `LastReportTime` the race has no observable effect, because every writer stores the same constant (B8). In Rust, use `AtomicU32`/`AtomicI64` (or a spin lock) to get defined behavior while keeping the semantics |
| B16 | Queue.c:202-210 | Unknown internal IOCTLs are completed with NOT_SUPPORTED instead of forwarded (ACTIVATE/DEACTIVATE/idle and the rest) |
| B17 | INF:82 | PID 0x0280 is installable but not in the table, so it fails to start |
| B18 | Series1.h:38,77 | Family 1 Y LogMax 7855 ≠ table span 6217 |
| B19 | Input.c:232-235 | No upper clamp of X/Y to the logical max. Readings above XMax or below YMin produce values beyond the descriptor LogMax |
| B20 | Driver.c:142 | `WdfPdoInitAllowForwardingRequestToParent` on a filter-FDO init. It is a PDO-only DDI. The WDF source shows it logs an error, hits a verifier-only breakpoint and returns with no effect (§2.3) |
| B21 | Device.c:83-90 | Lookaside list parented to the driver, so it leaks per device instance until unload |
| B22 | Hid.c:120,377,546,594,739 | `PAGED_CODE()` in functions that are not in a PAGE section and can be reached at DISPATCH. It is misleading (asserts only in DBG). **Nothing on the IOCTL or completion paths may be pageable or block in Rust** |
| B23 | Device.c:217, 501 | Synchronous lower IOCTLs have no timeout; a hung lower driver hangs start or resume |
| B24 | Device.c:184,277-299 | `DesiredReportTypeValue` is uninitialized. A registry value of 1–3 bytes is accepted, and its upper bytes are stack garbage (§9). Rust: zero-init |
| B25 | Hid.h:183-224; Queue.h:36 | `_IRQL_requires_(PASSIVE_LEVEL)` is on every Hid.c IOCTL handler and on `DbgIoControlGetString`, yet all of them are called from `EvtIoInternalDeviceControl`, which runs at ≤ DISPATCH (sync scope None, exec level Dispatch). The annotation is wrong, like the `PAGED_CODE()` calls in B22 |

### 10.4 Dead code (safe to drop)
- `SpiDevice`, `HidVendorID`, `InputPollThreadHandle`.
- `QUEUE_CONTEXT`, `PTP_AAPL_MAPPING`/`MAPPING_MAX`, `PTP_CONTACT_RAW`/`_LL`/`CONTACT_STATE`, `PTP_USERMODEAPP_CONF_REPORT`.
- `DEVICE_VERSION`, `PTP_BUTTON_TYPE_PRESSURE_PAD`, `PTP_CONTACT_*_BIT`, `STATE_SWITCH_MAX_RETRIES`, `REPORTID_STANDARDMOUSE` (descriptor-unused).
- `EvtIoStop` body.
- The `SetState(FALSE)` path.
- The `DeviceGetContext == NULL` branches in PrepareHardware (Device.c:197-207) and SetState (Device.c:471-481).
- `WdfPdoInitAllowForwardingRequestToParent`.
- `DbgIoControlGetString`/`DbgDevicePowerString` (trace-only).
- `KdPrintEx` (DBG-only).
- The unused `SPI_TRACKPAD_PACKET`/`FINGER` fields.
- The `#ifndef __AAPL_HID_DESCRIPTOR_H__` guard in Hid.c:6-105, a meaningless include-guard in a .c file.
- The device interface, which is functionally dead but harmless to keep.

There are no TODO or FIXME comments in the directory. The only semi-TODO is the comment "Will non-internal IOCTL work?" (Device.c:500).

---

## 11. Platform-specific and risky constructs for the Rust port

| Construct | Where | Rust guidance |
|---|---|---|
| **Bitfields** (`UCHAR x:1`, MSVC LSB-first) | `PTP_CONTACT` Hid.h:132-139; `PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` Hid.h:121-127 | Pack manually: `b0 = conf \| tip<<1 \| (id&7)<<2`. Verified layout: Confidence=0x01, Tip=0x02, ID=7 gives 0x1C |
| **Packed structs** | `PTP_CONTACT` (pack 1, 5 B), `PTP_DEVICE_SELECTIVE_…` (3 B), `HID_DESCRIPTOR` (pshpack1, 9 B, u16 at offset 7) | Serialize into `[u8; N]` with `to_le_bytes`; avoid references to packed fields |
| **Struct with natural layout used as wire format** | `PTP_REPORT` (30 B), `SPI_TRACKPAD_PACKET`/`FINGER` (346/30 B), `HID_DEVICE_ATTRIBUTES` (32 B), `HID_XFER_PACKET` (16 B, pointer-bearing) | Parse the input with explicit offset reads (`i16::from_le_bytes`); never transmute an unchecked-length buffer |
| **Raw `Irp->UserBuffer` deref** (METHOD_NEITHER contract) | Hid.c:614,759 | `unsafe`; validate non-null, the length ≥16 (`size_of::<HID_XFER_PACKET>()`), and ideally `reportBufferLen` before touching `reportBuffer` |
| **Self-referential stack buffer** | Device.c:465-509 | Use a heap or pinned aligned struct; it must outlive the synchronous send |
| **Alignment assumption** | `UCHAR[255]` cast to `HID_XFER_PACKET*` (Device.c:484) | `#[repr(C, align(8))]` |
| **Comma-operator assignment** | Hid.c:692 | Decide between exact and intended behavior (§5.4) |
| **Uninitialized stack data sent to the host** | Input.c:173 | Zero-init (intentional divergence) |
| **Use-after-free** | Input.c:317-319 | Reorder deletes |
| **Unsynchronized shared state** | `DeviceStatus`, `LastReportTime` | Atomics |
| **LARGE_INTEGER union** | Device.h:78; Input.c:176-221 | Only `.QuadPart` is used, so use `i64`. **Misuse:** C passes the variable as `KeQueryPerformanceCounter`'s *frequency* out-parameter and discards the returned counter (B8). A faithful port therefore emits ScanTime 0. If the intended behavior is chosen, use the **return value** of `KeQueryPerformanceCounter` |
| **Enum sizing sentinel** | `REPORT_TYPE.InvalidReportType = 0x7fffffff` (Device.h:43) | `#[repr(u32)]`/plain `u32` |
| **Multi-char literal pool tag** | Driver.h:43 | `u32::from_le_bytes(*b"APTL")` = 0x4C545041 |
| **Static table in a header** | AppleDefinition.h:52 (`static const`, duplicated per TU) | Single `const`/`static` |
| **Macro-built byte tables** | HID descriptors (Hid.c:9-103 via Series*.h/Hid.h macros) | Generate `static [u8;501]` and verify SHA-256 (§5.1). Watch the `LOGICAL_MAXIMUM_3` (0x27) and `PHYSICAL_MAXIMUM_3` (0x47) **4-byte** items and `REPORT_COUNT_2` (0x96) 2-byte item |
| **Signed/unsigned arithmetic** | Input.c:232-243 | Promote to `i32` before subtracting; compare signed; cast with `as u16` only when > 0 |
| **WDF inline-init macros** | `WDF_*_INIT`, `WDF_DECLARE_CONTEXT_TYPE_WITH_NAME`, `WDF_REL_TIMEOUT_IN_SEC` | Re-implement (wdk-sys / windows-drivers-rs provide `WDF_OBJECT_CONTEXT_TYPE_INFO`; set `Size` fields correctly for KMDF 1.23 or higher). `WDF_TIMER_CONFIG_INIT` defaults AutomaticSerialization=TRUE |
| **WPP tracing / IFR** | Trace.h; all `TraceEvents` | No WPP in Rust. Use TraceLogging/ETW or DbgPrint. The control GUID and flags are in §1 if log compatibility matters |
| **Paged/INIT code sections** | `alloc_text` (§1) | Rust: keep everything non-paged, which is safe. DISPATCH-reachable paths: EvtIoInternalDeviceControl, InputRoutineWorker, IssueRequest, CompletionRoutine, GetStrings |
| **Undocumented Apple structures** | `SPI_TRACKPAD_PACKET`, `SPI_SET_FEATURE` (report 2, `[02 01]`) | Reverse-engineered; keep the exact offsets |
| **KMDF version** | 1.23 | Configure the Rust driver to target KMDF ≥ 1.23 (e.g. `[package.metadata.wdk.driver-model] driver-type="KMDF", kmdf-version-major=1, target-kmdf-version-minor=23` or newer) |

**Not present:**
- inline asm;
- function-pointer patching or hooking;
- unions other than LARGE_INTEGER;
- floating point;
- dynamic allocation other than WDF lookaside/request objects;
- threads (`InputPollThreadHandle` is unused);
- spin locks or mutexes (the absence is the problem; see B15).

---

## Appendix A — Family 1 PTP report descriptor, full 501 bytes (`AmtPtpSpiFamily1ReportDescriptor`)

Regions:

| Offsets | Content |
|---|---|
| `0x000-0x009` | TLC header (`05 0D 09 05 A1 01 85 05 09 22`) |
| `0x00A-0x04C` | FC1 #1 |
| `0x04D-0x050` | `05 0D 09 22` |
| `0x051-0x093` | FC1 #2 |
| `0x094-0x097` | `05 0D 09 22` |
| `0x098-0x0D4` | FC2 #3 |
| `0x0D5-0x0D8` | `05 0D 09 22` |
| `0x0D9-0x11B` | FC1 #4 |
| `0x11C-0x11F` | `05 0D 09 22` |
| `0x120-0x15C` | FC2 #5 |
| `0x15D-0x1AE` | Scan time / count / button / features 7 and 8, end of TLC |
| `0x1AF-0x1DD` | Windows configuration TLC (47 B) |
| `0x1DE-0x1F4` | User-mode configuration TLC (23 B) |

```
0000: 05 0D 09 05 A1 01 85 05 09 22 A1 02 25 01 09 47
0010: 09 42 95 02 75 01 81 02 95 01 75 03 25 03 09 51
0020: 81 02 75 01 95 03 81 03 05 01 26 AA 29 75 10 55
0030: 0E 65 11 09 30 46 5E 04 95 01 81 02 46 C7 02 26
0040: AF 1E 09 31 81 02 45 00 55 00 65 00 C0 05 0D 09
0050: 22 A1 02 25 01 09 47 09 42 95 02 75 01 81 02 95
0060: 01 75 03 25 03 09 51 81 02 75 01 95 03 81 03 05
0070: 01 26 AA 29 75 10 55 0E 65 11 09 30 46 5E 04 95
0080: 01 81 02 46 C7 02 26 AF 1E 09 31 81 02 45 00 55
0090: 00 65 00 C0 05 0D 09 22 A1 02 25 01 09 47 09 42
00A0: 95 02 75 01 81 02 95 01 75 03 25 03 09 51 81 02
00B0: 75 01 95 03 81 03 05 01 26 AA 29 75 10 55 0E 65
00C0: 11 09 30 46 5E 04 95 01 81 02 46 C7 02 26 AF 1E
00D0: 09 31 81 02 C0 05 0D 09 22 A1 02 25 01 09 47 09
00E0: 42 95 02 75 01 81 02 95 01 75 03 25 03 09 51 81
00F0: 02 75 01 95 03 81 03 05 01 26 AA 29 75 10 55 0E
0100: 65 11 09 30 46 5E 04 95 01 81 02 46 C7 02 26 AF
0110: 1E 09 31 81 02 45 00 55 00 65 00 C0 05 0D 09 22
0120: A1 02 25 01 09 47 09 42 95 02 75 01 81 02 95 01
0130: 75 03 25 03 09 51 81 02 75 01 95 03 81 03 05 01
0140: 26 AA 29 75 10 55 0E 65 11 09 30 46 5E 04 95 01
0150: 81 02 46 C7 02 26 AF 1E 09 31 81 02 C0 05 0D 55
0160: 0C 66 01 10 47 FF FF 00 00 27 FF FF 00 00 09 56
0170: 81 02 09 54 25 7F 75 08 81 02 05 09 09 01 25 01
0180: 75 01 81 02 95 07 81 03 05 0D 85 07 09 55 09 59
0190: 15 00 26 FF 00 75 08 95 02 B1 02 06 00 FF 85 08
01A0: 09 C5 15 00 26 FF 00 75 08 96 00 01 B1 02 C0 05
01B0: 0D 09 0E A1 01 85 04 09 22 A1 02 09 52 15 00 25
01C0: 10 75 08 95 01 B1 02 C0 A1 00 85 06 09 57 09 58
01D0: 75 01 95 02 25 01 B1 02 95 06 B1 03 C0 C0 06 00
01E0: FF 09 01 A1 01 85 09 09 01 15 00 26 FF 00 75 08
01F0: 95 03 B1 02 C0
```

## Appendix B — Rust port checklist

1. Filter device: `WdfFdoInitSetFilter`. Context = DEVICE_CONTEXT (with atomics for `DeviceStatus` and `LastReportTime`).
2. PnP callbacks: PrepareHardware, D0Entry, D0Exit, SelfManagedIoInit and SelfManagedIoRestart (the same function).
3. Lookaside: 1024 B, NonPagedPoolNx, tag "APTL". Parent it to the device; this is a deviation from C (B21).
4. Passive one-shot timer, parent = device, 5 s.
5. Device interface `{f271e39d-d211-4a6e-a439-63e51940d210}` (optional / compat).
6. Default parallel queue (non-power-managed by filter default) with only EvtIoInternalDeviceControl. Manual non-power-managed `HidQueue`.
7. IOCTL table exactly as §4. Unknown codes complete with NOT_SUPPORTED.
8. Descriptors: 8 × 501 B tables matching the SHA-256 values, and the 9-B HID descriptor `09 21 00 01 00 01 22 F5 01`.
9. Attributes: VID 0x8910, real PID and version. Information 32.
10. Feature GET 7 → `[07 05 00]`. GET 8: decide on B1. SET 4/6 accepted and ignored. Information not set.
11. SetState: `{reportId=2, len=2, buf=[02 01]}` in a 255-B input buffer via synchronous internal IOCTL 0x000B0191.
12. Read path: 1:1 lower reads, FIFO completion, conversion as in §7.3, report zero-initialized (B5), cleanup order fixed (B10). **ScanTime: the faithful value is constant 0 (B8).** Decide between faithful and a real 100 µs counter, and flag it.
12a. Do not call any power-pageable, device-type or characteristics setter: the filter inherits them from the lower device (§2.4). Zero-init the `DesiredReportType` read buffer (B24).
13. Recovery: single retry after 5 s, then one lower read, then Configured. Keep or improve (B12), and document the choice.
14. Optionally fix the remaining §10.3 items in a separate, clearly labelled change, not in the 1:1 port.

---

## Verification addendum

This spec was re-verified against every file in `src/AmtPtpDeviceSpiKm/` and against `AmtPtpDevice.inf`.

**Checks that passed with no change:**
- The 8 descriptors, recompiled independently with `gcc -mms-bitfields` from the project headers. All are 501 B, and all eight SHA-256 values match §5.1.
- The HQA blob SHA-256.
- Appendix A, byte for byte.
- Every F1→F2/F3A/F3B/touchscreen patch offset in §5.1.
- Struct layouts: `PTP_REPORT` 30 B (ScanTime@26, Count@28, Button@29), `PTP_CONTACT` 5 B (conf+tip+ID4 = 0x13), `PTP_DEVICE_SELECTIVE_…` 3 B, `HID_XFER_PACKET` 16 B, `SPI_TRACKPAD_PACKET` 346 B (NumOfFingers@30, Fingers@46).
- All IOCTL codes and status values.
- The device table and its spans.
- The INF hardware IDs, including 0x0280, and the INF sections.
- Commit references e92581b, dd364d6 and 8e0cc7c.
- The sibling HQA bug line numbers.

**Corrections made inline:**
1. **ScanTime (major; changes runtime behavior).** The source calls `KeQueryPerformanceCounter(&x)`, not `(NULL)`. The argument is the *frequency* out-parameter (confirmed on Learn), and the counter is discarded. So `LastReportTime` and `CurrentCounter` always hold the QPC frequency, `delta` is always 0, and **ScanTime is always 0x0000**. It is not "almost always 255", and the negative-wrap case cannot occur.
   - Fixed in §2.9, §2.11, §3.1, §6.5, §7.3 (steps 5 and 8, plus the state note), §10.1, B8, B15, §11 and Appendix B #12.
   - §7.3 step 8 now records the porting decision: faithful constant 0, or the intended 100 µs counter.
2. **§6.2 SET_FEATURE IRP placement** is no longer "unverified". WDF source `FxIoTarget::FormatIoctlRequest` shows the following:
   - METHOD_IN_DIRECT puts the caller's 255-B buffer **in place** (no copy) into `Irp->AssociatedIrp.SystemBuffer`. InputBufferLength is 255, there is no MDL, and UserBuffer is not set.
   - METHOD_NEITHER sends put the output in `UserBuffer` and the input in `Type3InputBuffer`.
3. **§9 / new B24:** `DesiredReportTypeValue` is uninitialized (Device.c:184). Registry values of 1–3 bytes are accepted, and their upper bytes are garbage. Rust must zero-init. "Any 4-byte type" is corrected to "any type of ≤ 4 bytes".
4. **§2.4 (PnP/power config):** KMDF filters inherit the following from the lower device (WDF `fxdevicekm.cpp`):
   - `DO_POWER_PAGABLE|DO_POWER_INRUSH`
   - DeviceType
   - Characteristics
   - the buffer method

   The port must not call the corresponding WdfDeviceInit setters.
5. **§2.12 / B12:**
   - KMDF does not appear to stop the local I/O target on D0Exit. Learn documents an automatic stop only on remove, and the WDF power, PnP and FDO state-machine sources contain no target stop on power-down. So a timer firing in D3 sends SET_FEATURE down. `Configured` is set only if the lower driver succeeds; the earlier text implied it always is.
   - Added the stale-pending-timer behavior after a successful SMIO-restart.
   - Added the `WdfTimerStart` re-arm behavior.
6. **§2.3 / B20:** `WdfPdoInitAllowForwardingRequestToParent` on an FDO init is verified in WDF source: it logs an error, hits a verifier-only breakpoint and returns with no effect.
7. **§2.4 timer:** the reason `AutomaticSerialization=TRUE` does not fail is now verified (`_GetEffectiveLock` finds no lock under sync scope None).
8. **§5.2 descriptor quirks:**
   - The global items (s, −4, PhysMax 65535) persist through **all** remaining reports, including the Configuration and UM TLCs, not only through feature 7.
   - The missing reset in FC2 affects finger 4's bit fields, not "the next item".
9. **Unreachable `DeviceGetContext == NULL` → `STATUS_INVALID_DEVICE_STATE` branches** in PrepareHardware and SetState are now listed (§2.8, §2.13, §10.4).
10. **New B25:** `_IRQL_requires_(PASSIVE_LEVEL)` on the Hid.c handlers and `DbgIoControlGetString`, which are reachable at DISPATCH.

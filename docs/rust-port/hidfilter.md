# AmtPtpHidFilter: Rust port record

This file records the port of `src/AmtPtpHidFilter` (KMDF lower filter, Bluetooth Magic Trackpad 2) to Rust:

* `crates/amtptp-hidfilter-core`: all hardware-independent logic, host-tested against the C oracle fixtures.
* `drivers/km/AmtPtpHidFilter`: the KMDF 1.23 glue.

Bug IDs are those of the porting spec ([`specs/hidfilter.md`](specs/hidfilter.md), Appendix C). They are written `HF-Bn` here and in the code. The policy follows the [behavior policy](README.md#behavior-policy):

* **P1/P3 PRESERVED**: observable behaviour and logic bugs are kept, as a "preserved known issue".
* **P2 FIXED**: undefined behaviour, memory-unsafety, leaks and hangs are fixed.

## How the core is used

* The core owns the C call **sequences**: dispatcher, read pipeline, PnP/power callbacks, mode switch, detour checks and AddDevice.
* The framework calls go through three traits: `FilterIo`, `DetourIo` and `AddDeviceIo`. Each trait method is exactly one WDF or kernel call.
* The tests implement those traits with the oracle's stub model (fixtures README section 5). They compare the recorded call sequence with the C `ev` strings, record by record.
* The glue implements the traits and turns each WDF callback into one call. The crate-level rustdoc has the table of callbacks and core functions.
* The device state shared by the callbacks is `DeviceState`, which holds atomics with relaxed ordering.

The driver has **no device table** and sends **no USB or SPI bytes**. Its only lower requests are these internal HID IOCTLs to the PDO:

* `GET_DEVICE_ATTRIBUTES`, with 32 bytes of output;
* the mode-switch `SET_FEATURE`, a 32-byte `HID_XFER_PACKET` buffer (`XferBuffer`);
* `READ_REPORT`, with 1024 bytes of output.

## Decisions per spec bug

| ID | Decision | What the Rust code does |
|---|---|---|
| HF-B1 | PRESERVED (P1) | GET_FEATURE 0x08 writes only `08 FC` (comma-operator bug) via `ptp_hid::feature::write_hqa_report_as_c`; the rest of the caller's buffer is untouched. |
| HF-B2 | PRESERVED (P1) | `parse_finger`: `y = ((u32 << 6) as i32).wrapping_neg() >> 19`. That gives the off-by-one when the X bits are non-zero. INT_MIN wraps as on MSVC (port policy P2 wrapping op, 15 `ub=int_min_neg` records). |
| HF-B3 | PRESERVED (P1) | Confidence = `(TouchMinor << 1) < 345` (TouchMinor tested twice), i.e. byte 5 <= 172. |
| HF-B4 | FIXED (P2) | The 50-byte report is built zero-initialised (`PtpReport::EMPTY`). Unused contacts and the padding bits are 0 instead of kernel stack bytes. |
| HF-B5 | PRESERVED (P1) | Lower clamp only, no upper clamp: x up to 7773, y up to 6574. |
| HF-B6 | PRESERVED (P1) | `FilterIo::query_performance_frequency` returns what `KeQueryPerformanceCounter(&x)` writes, which is the frequency. `scan_time(freq, swap(last, freq))` is therefore 0, or 0xFF on the never-stamped first report. |
| HF-B7 | PRESERVED (P1) | The lower-read completion status is never read; only `Information` counts. |
| HF-B8 | PRESERVED (P1) | There is no report-ID filter. Any length `hdr + 9n` is parsed. Any other length starts the 3 s timer and ends that read chain. |
| HF-B9 | FIXED (P2) | Retrieve-output or copy failure: `WdfDeviceSetFailed(AttemptRestart)` is still issued. Then the upper request is **completed** with that failure status. C leaks it. |
| HF-B10 | FIXED (P2, glue) | The glue reads `RequestMemory` from the request context before `complete_lower_read`. The core then deletes the request, then the memory. |
| HF-B11 | PRESERVED (P3) | Restart before the detour returns `STATUS_INVALID_STATE_TRANSITION`, but still stamps `LastReportTime` and sets `DeviceConfigured`. |
| HF-B12 | PRESERVED (P3) for Init/Restart, FIXED (P3) for the timer | A failed mode switch in Init or Restart returns SUCCESS and arms the 2 s timer. The timer no longer issues a lower read after a failed switch; it retries while the device is in D0. |
| HF-B13 | FIXED (P3) | The mode switch returns `WdfRequestGetStatus` after every synchronous send, so a lower failure behind a TRUE send is a failure. |
| HF-B14 | FIXED (P3) by gating | D0Exit still does not stop the timer or flush the work item, but it leaves D0 and advances an epoch (`DeviceState`), so a later timer or work item does nothing and a running timer cannot mark the device configured. |
| HF-B15 | FIXED (P2 leak, glue) | The lookaside list is parented to the device (`config::READ_BUFFER_LOOKASIDE_PARENT_IS_DEVICE`); C parents it to the driver. |
| HF-B16 | FIXED (P2) | The timer and the work item use `AutomaticSerialization = FALSE` (`config`), which has the same effect under sync scope None. AddDevice returns the creation failure of the lookaside list, timer or work item immediately. C continues and would later use a NULL handle. |
| HF-B17 | FIXED (P2) | A NULL WDM device object makes AddDevice return `STATUS_UNSUCCESSFUL`. C returns SUCCESS with a half-built device. |
| HF-B18 | FIXED (P2) | `InputFingerSize == 0` (Apple VID with a PID other than 0x0265, read issued by the timer) is classified Malformed, which starts the 3 s timer. C divides by zero. |
| HF-B19 | FIXED (dead field) | `QUEUE_CONTEXT.DeviceIoTarget` (always NULL) is dropped. |
| HF-B20 | FIXED (P3) for selective reporting | `PtpReportTouch` (Surface Switch, bit 0) and `PtpReportButton` (Button Switch, bit 1) start on, are restored by PrepareHardware, and gate the input report. `PtpInputOn` is still write-only. |
| HF-B21 | PRESERVED (P1) | GET/SET_FEATURE never set `Information`. |
| HF-B22 | PRESERVED (P1) | `HID_DEVICE_ATTRIBUTES.Reserved` is not written. |
| HF-B23 | PRESERVED for `reportBufferLen >= 2`; FIXED (P2/P3) below | SET_FEATURE still ignores `reportBufferLen` otherwise. If byte 1 lies outside the report buffer, 0x04 reads it as 0 (mouse mode, P2) and 0x06 changes nothing (P3). |
| HF-B24 | PRESERVED (P3) | A zero-length completion enqueues the work item, which re-reads at once (possible hot loop). |
| HF-B25 | FIXED (P2, unreachable) | Without an I/O target, GET_STRING completes with `STATUS_INVALID_DEVICE_STATE` and `issue_lower_read` sends nothing. C uses a NULL handle. |
| HF-B26 | PRESERVED (P1) | The global `MajorFunction[0x0F]` patch is never reverted. The checks, their order and the `strncmp` semantics are in `detour_hid_stack` and `client_id_matches`. |
| HF-B27 | PRESERVED (P1) | Synchronous attribute query and mode switch without timeout. |
| HF-B28 | PRESERVED (P1) | The device interface `{FF969022-3111-4441-8F88-875440172C2E}` is registered with no handler (`config::DEVICE_INTERFACE_GUID`). |
| HF-B29 | PRESERVED (P1) | `response_length(info) = info as u32 as i32 as isize as usize`, the `(size_t)(LONG)` sign extension. |
| HF-B30 | FIXED (P3) | PrepareHardware keeps VID, PID and version (only SelfManagedIoInit queries them), so the restart after a PnP stop sends the mode switch again. Successful configurations issue the lower reads parked upper reads are missing. |

### Other P2 items, without a spec bug ID

* **Data races.** The C context fields are read and written without locks. The port keeps them as relaxed atomics in `DeviceState`.
* **Signed overflow.** The INT subtractions (`x - X.min`) and the LONGLONG ScanTime subtraction use `wrapping_*`.
* **Unaligned access.** The unaligned `USHORT*` and `UINT*` finger loads became explicit little-endian decoding.
* **Out-of-bounds reads.** Finger and button bytes outside the buffer read as 0. That never happens with the 1024-byte buffer: every input record was replayed with the tail filled with 0x00 and again with 0xFF, with identical results.
* **Alignment.** The mode-switch packet buffer is `#[repr(C, align(8))]`. The C `UCHAR[32]` is not guaranteed to be 8-aligned.

## Fixture coverage (`crates/amtptp-hidfilter-core/tests/fixtures`)

Every record is processed, except the 3 named skips in `lifecycle.txt`. Each test asserts its per-file total and the count per category.

| File | Records | Processed | Skipped | Notes |
|---|---:|---:|---:|---|
| `descriptors.txt` | 99 | 99 | 0 | See below. |
| `ioctl.txt` | 519 | 519 | 0 | 28 named or probe codes have full `ev`. The 491 sweep codes complete with `NOT_SUPPORTED` as the only call. |
| `feature.txt` | 1588 | 1588 | 0 | 1068 are exact. The 516 successful SET 0x06 records with byte 1 assert the P3 bit order (Surface bit 0, Button bit 1: the C switches exchanged). Of the 4 `oob=1` records, the two 0x04 ones assert the HF-B23 fix (byte 1 read as 0) and the two 0x06 ones assert that nothing changes (P3). C read a stale `03` there. |
| `input_bt.txt` | 1653 | 1653 | 0 | See below. |
| `input_usb.txt` | 1661 | 1661 | 0 | Same as BT; the act counts are 1384 / 219 / 24 / 23 / 9, plus 1 + 1 HF-B9. 6 records are `ub=int_min_neg`. |
| `wire.txt` | 65 | 65 | 0 | 21 are exact and 22 fault-injected, each with `WdfRequestGetStatus` after a TRUE mode-switch send (HF-B13). 22 assert a P3 fix derived from the record: 6 TRUE sends with a failed lower status, 12 timer records (gated to D0, no read after a failed switch) and 4 PnP restarts (IDs kept, so they equal the matching resume plus both selective-reporting switches on). `lower_reads_pending` is checked cumulatively per configuration. `tests/power.rs` covers the stop/resume sequences the C oracle got wrong. |
| `lifecycle.txt` | 53 | 50 | 3 | See below. |

`descriptors.txt`:

* 3 static records. 55 keys are checked against the port. 35 keys describe private C layout and are not mirrored: the `DEVICE_CONTEXT`, `WORKER_REQUEST_CONTEXT` and `QUEUE_CONTEXT` offsets and sizes, `sizeof(BCM5974_PARAM)`, and 5 `HIDCLASS_DRIVER_EXTENSION` members the port never touches.
* 58 records have a result decided by the driver.
* 38 records have a status decided by KMDF. That status is `0xC0000023`, from the stub model, which the test mock implements.

`input_bt.txt`:

* complete 1342, timer3s 239, setfailed_norestart 30, noupper 26, workitem 14.
* 1 outmem_fail and 1 copy_fail assert the HF-B9 fix: the upper request is completed with `0xC0000023` and `Information` is untouched.
* `out` is compared exactly, because the port zero-initialises (HF-B4), and the uninitialised C bits are 0 in `out`.
* 9 records are `ub=int_min_neg`.
* `rand` records are checked against the README 3.2 `ev` sequence for their `act`.

`lifecycle.txt`:

* 50 records are processed:
  * 38 are exact;
  * 3 assert P3 fixes: `prepare-hardware-after-start` keeps the IDs (HF-B30), and `workitem-run-unconfigured` and `timer-without-detour` do nothing;
  * 5 AddDevice records are exact;
  * 4 AddDevice records assert the HF-B16/HF-B17 fixes: an early failure return, or `STATUS_UNSUCCESSFUL`.
* The object-creation arguments of `device-add-ok` are checked against `config`, with the HF-B15 and HF-B16 deltas.
* **Skipped**: 2 `DriverEntry` records and 1 driver-cleanup record. They are glue-only: `WdfDriverCreate` passes its status through, and the provider registers and unregisters. There is no core logic, and the driver crate implements them.

Cases excluded by the fixture harness itself (README section 6) are covered as follows:

* the HF-B18 divide by zero is covered by the unit test `classification_order`;
* NULL-handle sends (HF-B25) are guarded and have no fixture;
* the spec's Appendix D golden vectors are in `tests/golden.rs`.

## Driver crate (`drivers/km/AmtPtpHidFilter`, KMDF 1.23)

The glue makes every WDF and kernel call of the C driver that the oracle records, with the C arguments, and nothing else.
Pure accessors have no effect and are not recorded (fixtures README section 1). They differ from C in two places:

* The lower-read completion calls `WdfMemoryGetBuffer` only on the report path, after `WdfIoQueueRetrieveNextRequest` and `KeQueryPerformanceCounter`. It passes `RequestMemory`, which is the same WDFMEMORY as `Params->Parameters.Ioctl.Output.Buffer`, and a non-NULL size pointer. C calls it at entry, on every completion, with NULL (Input.c:151).
* GET/SET_FEATURE take `OutputBufferLength`/`InputBufferLength` from the `EvtIoInternalDeviceControl` arguments instead of calling `WdfRequestGetParameters` (Hid.c:192-194, 279-281). Both read the same IRP stack-location fields.

`WdfRequestRetrieveOutputBuffer` (attributes) is called with `Length = NULL`, so the glue hands the core exactly `minimum_length` (32) bytes; the core writes only within them (the host mock hands it the whole output buffer, which the trait allows: "at least `minimum_length`").

The core decides which call happens, and when. Each WDF callback is one call into the core.

| Module | C origin | Contents |
|---|---|---|
| `lib.rs` | `Driver.c` | `DriverEntry` (`INIT`), the driver-object cleanup callback (`PAGE`), the TraceLogging provider |
| `device.rs` | `Driver.c`, `Device.c`, `Queue.c:10-54`, `Input.c:30-37` | `evt_device_add` (`PAGE`) with the `AddDeviceIo` impl; the PnP/power, timer and work-item callbacks |
| `queue.rs` | `Queue.c:56-129` | `EvtIoInternalDeviceControl` and the no-op `EvtIoStop` |
| `io.rs` | `Input.c`, `Hid.c`, the WDF calls of `Device.c` | `Filter`, the `FilterIo` impl (one WDF/kernel call per method); the lower-read completion routine |
| `hac.rs` | `Detour.c`, `include/Hac.h` | the `#[repr(C)]` mirrors of `Hac.h` and the `DetourIo` impl. This is the only module that reads undocumented structures or writes into another driver's `DRIVER_OBJECT`. |
| `context.rs` | `include/Device.h`, `include/Queue.h` | `DEVICE_CONTEXT`, `QUEUE_CONTEXT` and `WORKER_REQUEST_CONTEXT`, registered under the C names for `!wdfkd` |

### C function to Rust function

| C | Rust |
|---|---|
| `DriverEntry` | `lib.rs::driver_entry` |
| `PtpFilterEvtDriverContextCleanup` | `lib.rs::evt_driver_context_cleanup` |
| `PtpFilterEvtDeviceAdd`, `PtpFilterCreateDevice`, `PtpFilterIoQueueInitialize` | `device.rs::evt_device_add`, then `core::add_device` with `device.rs::AddDevice` |
| `PtpFilterPrepareHardware`, `D0Entry`, `D0Exit` | `device.rs::evt_device_{prepare_hardware,d0_entry,d0_exit}`, then `core::{prepare_hardware,d0_entry,d0_exit}` |
| `PtpFilterSelfManagedIoInit` / `Restart` | `device.rs::evt_device_self_managed_io_{init,restart}`, then the core functions of the same name |
| `PtpFilterDetourWindowsHIDStack` | `core::detour_hid_stack`, with `hac.rs` (`impl DetourIo for Filter`) |
| `PtpFilterConfigureMultiTouch` | `core::configure_multitouch`, with the mode-switch methods of `io.rs` |
| `PtpFilterRecoveryTimerCallback` | `device.rs::evt_recovery_timer`, then `core::recovery_timer_callback` |
| `PtpFilterWorkItemCallback` | `device.rs::evt_recovery_work_item`, then `core::work_item_callback` |
| `FilterEvtIoIntDeviceControl` and the `Hid.c` handlers | `queue.rs::evt_io_internal_device_control`, then `core::dispatch_internal_ioctl` |
| `FilterEvtIoStop` | `queue.rs::evt_io_stop` (empty, registered as in C) |
| `PtpFilterInputProcessRequest`, `PtpFilterInputIssueTransportRequest` | inside `core::dispatch_internal_ioctl`; `core::issue_lower_read` |
| `PtpFilterInputRequestCompletionCallback` | `io.rs::evt_lower_read_completion`, then `core::complete_lower_read` |
| `PtpFilterDiagnostics*` | dropped; the three dead routines are never called. Trace names come from `ptp_hid::ioctl::ioctl_name`. |

### Configuration, unchanged from C

* The device is a filter (`WdfFdoInitSetFilter`) with five PnP/power callbacks: PrepareHardware, D0Entry, D0Exit, SelfManagedIoInit and SelfManagedIoRestart. Nothing else is registered: no ReleaseHardware, no power policy, no file object, no device cleanup and no `EvtDriverUnload`.
* The device interface `{FF969022-3111-4441-8F88-875440172C2E}` has no reference string.
* The lookaside list holds 1024-byte `NonPagedPoolNx` buffers with tag `'LTPA'` and no memory attributes.
* The timer is one-shot and passive-level (`ExecutionLevel = WdfExecutionLevelPassive`), with parent = device. Because it is passive-level, the synchronous mode switch it triggers runs at PASSIVE_LEVEL, as in C.
* The work item's parent is the device.
* The default queue is parallel, with `PowerManaged = WdfUseDefault` (not power-managed on a filter). It has `EvtIoInternalDeviceControl` and an empty `EvtIoStop`.
* `HidReadQueue` is manual, with `PowerManaged = WdfFalse` and no attributes.
* Lower read: `WdfRequestCreate` with `WORKER_REQUEST_CONTEXT` and parent = device, `WdfMemoryCreateFromLookaside`, `WdfIoTargetFormatRequestForInternalIoctl(READ_REPORT, out = memory)`, a completion routine whose context is the request context, and an asynchronous `WdfRequestSend` with NULL options.
* Mode switch: `WdfRequestCreate(WDF_NO_OBJECT_ATTRIBUTES)`, then `WdfMemoryCreatePreallocated` (parent = request, 32 bytes), then format `SET_FEATURE` with input = memory, then `Irp->UserBuffer = packet`, then `WdfRequestSend(SYNCHRONOUS)` with no timeout, then `WdfObjectDelete`.
* Attribute query: `WdfIoTargetSendInternalIoctlSynchronously` into a zeroed, aligned `HID_DEVICE_ATTRIBUTES` on the stack, with no options and no BytesReturned.
* Feature IOCTLs read `WdfRequestWdmGetIrp(Request)->UserBuffer` (a NULL IRP is treated as a NULL `UserBuffer`; see Driver-side P2 fixes).
* Descriptor IOCTLs use `WdfRequestRetrieveOutputMemory` plus `WdfMemoryCopyFromBuffer`. Attributes use `WdfRequestRetrieveOutputBuffer(32, Length = NULL)`. So the framework, not the glue, produces the error statuses there.
* `KeQueryPerformanceCounter(&x)` is called with a non-NULL pointer and its return value is discarded. `x` is the QPC frequency (HF-B6, preserved).
* Code placement: `DriverEntry` is in `INIT`; device-add and driver cleanup are in `PAGE`, with `wdk::paged_code!()`, a debug IRQL assert like `PAGED_CODE()`. Everything else is in `.text`. That includes the PnP callbacks, which C also left non-paged, and everything reachable at DISPATCH_LEVEL (dispatch, completion routine, read issue).

### Driver-side P2 fixes

| Item | C | Rust |
|---|---|---|
| HF-B10 | The completion routine reads `RequestMemory` from the context of the request it has just deleted. | The routine copies the device-context pointer and `RequestMemory` into locals before calling the core. The core deletes the request, then the memory. Afterwards the routine touches neither, nor the request context. |
| HF-B15 | The lookaside list is created with `WDF_NO_OBJECT_ATTRIBUTES`, so its parent is the WDFDRIVER, and one list leaks per AddDevice. | `ParentObject = device`. |
| HF-B16 | The timer and the work item use `AutomaticSerialization = TRUE`. A creation failure is only logged. | `FALSE` on both. That has no effect under the inherited sync scope None, and it removes the possible `STATUS_WDF_INCOMPATIBLE_EXECUTION_LEVEL` against the Dispatch-level device. Every creation failure fails AddDevice. |
| HF-B17 | A NULL WDM device object returns SUCCESS. | `STATUS_UNSUCCESSFUL`. |
| Data races | Context fields are plain, shared without locks. | `DeviceState` uses relaxed atomics. The handles are `AtomicPtr`: `Relaxed` for the handles stored once in AddDevice, and `Release`/`Acquire` for `HidIoTarget`, which the detour stores. No `&mut` to a context is formed after its initializing `ptr.write`. |
| NULL client ID (no spec ID) | `strncmp("HIDCLASS", NULL, 9)` dereferences NULL. | A NULL `ClientIdentificationAddress` is a mismatch, so the detour returns `STATUS_UNSUCCESSFUL`. |
| NULL `reportBuffer` in a feature packet (no spec ID) | C writes or reads through it. | It becomes an empty slice. GET 0x07/0x08 then returns `STATUS_INVALID_BUFFER_SIZE`. SET 0x04/0x06 reads byte 1 as 0 and returns `STATUS_SUCCESS`, as in HF-B23. |
| NULL IRP for a feature request (no spec ID, defensive, unreachable) | `WdfRequestWdmGetIrp(Request)->UserBuffer` (Hid.c:200, Hid.c:287) dereferences the returned IRP without checking it, so a NULL IRP would be a NULL dereference. | `wdf_support::wdm::request_user_buffer` returns NULL when there is no IRP. GET/SET_FEATURE then treat it as a NULL `UserBuffer` and complete with `STATUS_INVALID_DEVICE_REQUEST`, after the same 16-byte length check. A request presented to `EvtIoInternalDeviceControl` always carries its IRP, so this cannot happen. |
| Alignment | The `UCHAR[32]` mode-switch buffer is not guaranteed to be 8-aligned. | `XferBuffer` is `align(8)`. The attribute query uses a real `HID_DEVICE_ATTRIBUTES` local, not the core's `[u8; 32]`. `HID_XFER_PACKET` fields are read with `read_unaligned`. |
| NULL handles to WDF (defensive, unreachable) | HF-B25: a NULL `HidIoTarget` is sent to WDF, which bugchecks. The lower-read completion dereferences the `WdfMemoryGetBuffer` result without a check (Input.c:151, 202-203, 221). | Every glue method that needs `HidIoTarget` checks it. A NULL target gives `STATUS_INVALID_DEVICE_STATE`, or "not sent", and nothing reaches WDF. The core never gets there: it checks `io_target_set` first. `WdfRequestRetrieveOutputBuffer` succeeding with a NULL buffer gives `STATUS_INVALID_DEVICE_REQUEST` instead of a write through NULL. A NULL `WdfMemoryGetBuffer` result in the lower-read completion (`io.rs::lower_read_buffer`) becomes an empty slice. Every finger and button byte then reads as 0, and the parked upper read is completed with that report. This is unreachable: a live lookaside WDFMEMORY always has its buffer, because `WdfMemoryCreateFromLookaside` fails otherwise and the read is never sent. |
| HF-B9 (core) | The upper request leaks. | Completed with the failure status. `WdfDeviceSetFailed` is still called. |

Exactly-once completion:

* The core completes, parks or forwards every upper request exactly once. That is the fixture mock's double-completion assertion, checked on every record.
* The glue's `complete` goes through `wdf_support::request::Request::from_raw(..).complete(status)`.
* Driver-created requests (lower reads, the mode switch) are deleted, never completed.

### Preserved driver-side issues (P1/P3)

* **HF-B26.** The global HIDCLASS `MajorFunction[0x0F]` patch is a single unsynchronized pointer store (`write_volatile`) and is never reverted.
* **HF-B27.** The attribute query and the mode switch are synchronous with no timeout. They run only at PASSIVE_LEVEL: from SelfManagedIoInit/Restart, and from the passive-level recovery timer. A hung lower driver therefore blocks start, resume or the timer's work-item thread indefinitely, as in C.
* **HF-B14.** D0Exit stops neither the timer nor the work item; no `WdfTimerStop` was added.
* **HF-B28.** The device interface is registered with no handler.
* **`EvtIoStop`** is registered and empty. It neither acknowledges nor completes.
* **KMDF version.** The C project bound the WDK default, KMDF 1.15. The KM workspace binds KMDF 1.23 for all three KM drivers (packaging decision, see `packaging.md`). The behaviour this driver relies on exists in 1.9 and later.

### Not verified (no device test on this machine)

* That the framework statuses KMDF 1.23 produces on the `wdf=`/stub-model paths match KMDF 1.15, for example `STATUS_BUFFER_TOO_SMALL` for a short output buffer. The glue makes the same calls, so they should match.
* HF-B13: whether a synchronous `WdfRequestSend` returns TRUE when the request completes with an error status.
* That a passive-level timer with `AutomaticSerialization = FALSE` creates under a Dispatch-level parent. The WDF documentation says it should, but this was not run on a device.
* The detour offsets against the running Windows build. The port uses the same hard-coded LP64 offsets as C.

### Tracing

* Provider `AmtPtpHidFilterTraceProvider`, id `eeb34803-f3ec-4bb1-97ce-b42405d7f3c2` (the C WPP control GUID).
* Keywords: 0x01 driver, 0x02 device, 0x04 queue, 0x08 HID, 0x10 input. These are the WPP flag bits.
* Events:
  * Informational: driver init and cleanup; DeviceAdd with its status; PrepareHardware; D0Entry/D0Exit with the power state; SelfManagedIoInit/Restart with status, IDs and `DeviceConfigured`; DetourPatched; MultitouchModeSet; RecoveryTimerStarted (due time) and RecoveryTimerFired; MalformedInput (response length, Input.c:170); GetFeature and SetFeature (report ID and `reportBufferLen`); SetInputMode (`PtpInputOn`) and SelectiveReporting (button and surface bits as the core decodes them; nothing reads them, HF-B20) for SET_FEATURE 0x04/0x06; SetFeatureNotSupported (report ID and status). The completion routine classifies the read with `core::classify_lower_read` before calling the core, only to trace it.
  * Error: each failed AddDevice step; each NULL in the detour chain; the attribute query and mode-switch send failures; lower-read create/memory/format/send failures; DeviceSetFailed; UnsupportedDeviceInput (VendorID, Input.c:157); InputDroppedNoParkedRead (status, Input.c:178; only from the completion routine, not from the D0Exit drain); RetrieveOutputMemoryFailed and CopyToOutputMemoryFailed (status, Input.c:228/236, also on the descriptor IOCTLs). Together these name the cause of each DeviceSetFailed(AttemptRestart).
  * Warning: unsupported IOCTLs (code and name).
  * Verbose: every request completion (handle and status).

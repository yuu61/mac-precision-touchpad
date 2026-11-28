# AmtPtpHidFilter: Rust port record

The WDF driver now owns request dispatch and power callbacks. The core returns input actions and report bytes. The implementation map below describes the original port.

This file records the port of `src/AmtPtpHidFilter` (KMDF lower filter, Bluetooth Magic Trackpad 2) to Rust:

* `crates/amtptp-usb-bt/src/interface/bluetooth`: Bluetooth read classification and HID entry points.
* `crates/amtptp-usb-bt/src/application/bluetooth.rs`: device state, power transitions and read recovery.
* `crates/amtptp-usb-bt/src/infrastructure/magic_trackpad.rs`: Magic Trackpad 2 transport layouts and packet decoding.
* `drivers/interface/bluetooth/hid`: the KMDF 1.31 glue.

The C driver read the trackpad through a patch of HIDCLASS's dispatch table (the detour). The port
replaced it (2026-11-27) with a *data source*: a vendor collection that the new lower filter
`AmtPtpBthFilter` declares below `HidBth` ([bthfilter.md](bthfilter.md)), and that this filter reads
like any HID client. See [The data source](#the-data-source-replaces-the-hidclass-detour); the
records below that mention the detour describe the C behaviour and the earlier port.

Bug IDs are those of the porting spec ([`specs/hidfilter.md`](specs/hidfilter.md), Appendix C). They are written `HF-Bn` here and in the code. The policy follows the [behavior policy](README.md#behavior-policy):

* **P1/P3 PRESERVED**: observable behaviour and logic bugs are kept, as a "preserved known issue".
* **P2 FIXED**: undefined behaviour, memory-unsafety, leaks and hangs are fixed.

## How the core is used

* The core owns the C call **sequences**: dispatcher, read pipeline, PnP/power callbacks, mode switch, the data source's open and close, and AddDevice.
* The framework calls go through two traits: `FilterIo` and `AddDeviceIo`. Each trait method is exactly one WDF or kernel call. (A third, `DetourIo`, went with the detour.)
* The tests implement those traits with the oracle's stub model (fixtures README section 5). They compare the recorded call sequence with the C `ev` strings, record by record.
* The glue implements the traits and turns each WDF callback into one call. The crate-level rustdoc has the table of callbacks and core functions.
* The device state shared by the callbacks is `DeviceState`, which holds atomics with relaxed ordering.

The driver has **no device table** and sends **no USB or SPI bytes**. Its only lower requests go to the data source (`Col03`, opened as a remote I/O target):

* `IRP_MJ_READ` of one padded report (140 bytes) into a 1024-byte lookaside buffer;
* the mode switch as the class `IOCTL_HID_SET_FEATURE`, input = the report `F1 02 01` (the C driver sent the minidriver IOCTL with a 32-byte `HID_XFER_PACKET` to the PDO);
* `IOCTL_HID_GET_MANUFACTURER_STRING`, `…_PRODUCT_STRING` and `…_SERIALNUMBER_STRING` once when the source opens, to answer `IOCTL_HID_GET_STRING`.

C also sent `IOCTL_HID_GET_DEVICE_ATTRIBUTES` to the PDO for VID, PID and version; the port's identity is static (Bluetooth `004C:0265`).

## The data source (replaces the HIDCLASS detour)

The C driver patched `HidBth`'s `IRP_MJ_INTERNAL_DEVICE_CONTROL` slot with the routine HIDCLASS had saved, so that minidriver IOCTLs sent to the `Col01` PDO reached `HidBth`. That made it a second reader of `HidBth` beside the trackpad's own HIDCLASS, which lost about 3% of the reports (see [Competing reader](#competing-reader-on-bluetooth-fixed-by-the-data-source)). The patch was global (every Bluetooth HID device) and relied on undocumented HIDCLASS structures.

The port reads the trackpad as an ordinary HID client instead:

* `AmtPtpBthFilter` (below `HidBth`) appends a vendor collection with input `0x31` and feature `0xF1` to the SDP descriptor and pads each `0x31` report to its declared length (140 bytes, the real length in the last byte); a mouse report becomes a *mode-lost marker*. HIDCLASS then exposes the collection as `Col03`, a sibling of `Col01`. Details in [bthfilter.md](bthfilter.md); the format is `amtptp_usb_bt::bluetooth::bth`.
* `source.rs` (glue) registers for `GUID_DEVINTERFACE_HID` arrivals in `SelfManagedIoInit`, opens the matching `Col03` link from a work item (read/write, shared; never from the notification callback), checks that its parent device node is this collection's parent, reads its strings and calls the core's `source_opened`. Query-remove calls `source_closed` and closes the target (so the Bluetooth device can be disabled or updated); a cancelled removal reopens it; `SelfManagedIoCleanup` unregisters and closes.
* The core: `SelfManagedIoInit` sets the static identity and configures only if the source is already open; `source_opened` configures the device like the recovery timer (mode switch, then the backlog of reads); `source_closed` unconfigures it, stops the timer and cancels the reads in flight (their completions are stale). `SelfManagedIoRestart` before the source opened does nothing. `DeviceState.source_open` replaces C's `IsHidIoDetourCompleted` and `HidIoTarget != NULL`.
* The completion routine un-pads before the core sees the read: a multitouch report keeps its bytes and gets its real length; a marker becomes a one-byte report of the original ID, which the core treats as another report (it sends the mode switch again, HF-B8).

## Decisions per spec bug

| ID | Decision | What the Rust code does |
|---|---|---|
| HF-B1 | FIXED (PR #7) | GET_FEATURE 0x08 writes all 257 bytes, the report ID and the 256-byte certification blob (`ptp_hid::feature::write_hqa_report`). C wrote only `08 FC` (comma-operator bug). Information stays unset (HF-B21). |
| HF-B2 | FIXED (Step 2) | The MT2 decoder sign-extends Y before inversion. X bits do not affect Y. |
| HF-B3 | PRESERVED (P1) | Confidence = `(TouchMinor << 1) < 345` (TouchMinor tested twice), i.e. byte 5 <= 172. |
| HF-B4 | FIXED (P2 / Step 2) | The 50-byte report is built zero-initialised (`PtpReport::EMPTY`). `ContactTracker` tracks MT2 hardware contact IDs (`0..=15`), deduplicates within a frame, latches `Confidence = 0` palms, and synthesises 1-frame `TipSwitch = 0` liftoff reports. The clean report descriptor is `511` bytes (`GLOBAL_RESET` after all five fingers and `Scan Time`, `Input Mode` logical maximum `3`). |
| HF-B5 | FIXED (Step 2) | Report coordinates stay within the descriptor limits of 7612 on X and 5065 on Y. |
| HF-B6 | FIXED (PR #7) | `ptp_hid::scan_time::ScanClock` reports the time since the first report of the session in 100 µs units: the first report is 0 and the 16-bit value wraps every 6.5536 s. Every successful configuration (data source opened, Init/Restart, recovery timer) caches the frequency (`FilterIo::query_performance_frequency`) and starts a new session. The completion samples `FilterIo::query_performance_counter`, the return value of `KeQueryPerformanceCounter(NULL)`. |
| HF-B7 | FIXED (P3) | The completion status is read (`WdfRequestGetStatus`). A failed read is `LowerReadClass::Failed`: the work item re-issues it at once, as C did, until `config::MAX_IMMEDIATE_READ_RETRIES` (8) reads have failed in a row; after that the 3 s recovery timer, while the device is in D0. Any other completion resets the count. C took a failed read with `Information == 0` as an empty read with no limit, so a lower driver that failed every read (Bluetooth link down) made a zero-delay loop. The first version of this fix sent every failed read to the timer. On hardware, with the detour, HidBth failed a read with `STATUS_DEVICE_DATA_ERROR` now and then during normal use (see "Competing reader on Bluetooth" below). Two such failures in a row left no read in flight and froze input for 3 s. With the data source, no read failed in the hardware measurement. |
| HF-B8 | FIXED (P3) | Byte 0 must be the transport's multitouch report ID (`multitouch_report_id`: USB 0x02, Bluetooth 0x31, as in Linux hid-magicmouse and every C edge packet). Another report is `LowerReadClass::OtherReport`: it is dropped, and the work item sends the mode switch again (if the device is configured, in D0 and its source is open) and issues the next read. C parsed it as fingers when its length was `hdr + 9n`, and otherwise stalled input for 3 s as malformed. The mode switch is new: when the trackpad's power switch restarts it while the HID device stays (no PnP events), it comes back in mouse mode (hardware: 8-byte reports with ID 0x02), and C never switched it back, so the cursor stayed dead until the device was re-added. Once on hardware, a 4-byte packet starting with 0x8C also triggered the switch; it was probably the tail of a report split by the competing reader below. The extra switch is harmless (SUCCESS, 23 ms). |
| HF-B9 | FIXED (P2) | Retrieve-output or copy failure: `WdfDeviceSetFailed(AttemptRestart)` is still issued. Then the upper request is **completed** with that failure status. C leaks it. |
| HF-B10 | FIXED (P2, glue) | The glue reads `RequestMemory` from the request context before `complete_lower_read`. The core then deletes the request, then the memory. |
| HF-B11 | CHANGED (data source) | C: a restart before the detour returned `STATUS_INVALID_STATE_TRANSITION`, but still stamped `LastReportTime` and set `DeviceConfigured` (preserved until the detour was removed). Now a restart before the source opened returns SUCCESS and does nothing; `source_opened` configures the device. |
| HF-B12 | PRESERVED (P3) for Init/Restart, FIXED (P3) for the timer | A failed mode switch in Init or Restart returns SUCCESS and arms the 2 s timer. The timer no longer issues a lower read after a failed switch; it retries while the device is in D0. |
| HF-B13 | FIXED (P3) | The mode switch returns `WdfRequestGetStatus` after every synchronous send, so a lower failure behind a TRUE send is a failure. Seen on hardware (Bluetooth): the synchronous send returned TRUE for switches that completed with `STATUS_CANCELLED`, `STATUS_DEVICE_BUSY` and `STATUS_DEVICE_NOT_CONNECTED`. |
| HF-B14 | FIXED (P3) | D0Exit leaves D0 and advances an epoch (`DeviceState`), so a later timer or work item does nothing and a running timer cannot mark the device configured. It also stops the recovery timer (`Wait = FALSE`) and cancels the lower reads in flight: every lower read is registered in a `ptp_hid::read_tracker::ReadTracker` (behind a `WDFSPINLOCK` in the glue) before it is sent, D0Exit suspends the tracker (new epoch) and cancels what it hands out, and a read of an earlier epoch is dropped when it completes (`LowerReadOutcome::Stale`) instead of serving an upper read parked after the resume. A registration after D0Exit is refused. |
| HF-B15 | FIXED (P2 leak, glue) | The lookaside list is parented to the device (`config::READ_BUFFER_LOOKASIDE_PARENT_IS_DEVICE`); C parents it to the driver. |
| HF-B16 | FIXED (P2) | The timer and the work item use `AutomaticSerialization = FALSE` (`config`), which has the same effect under sync scope None. AddDevice returns the creation failure of the lookaside list, timer or work item immediately. C continues and would later use a NULL handle. |
| HF-B17 | FIXED (P2) | A NULL WDM device object makes AddDevice return `STATUS_UNSUCCESSFUL`. C returns SUCCESS with a half-built device. |
| HF-B18 | FIXED (P2) | `InputFingerSize == 0` (Apple VID with a PID other than 0x0265, read issued by the timer) is classified Malformed, which starts the 3 s timer. C divides by zero. |
| HF-B19 | FIXED (dead field) | `QUEUE_CONTEXT.DeviceIoTarget` (always NULL) is dropped. |
| HF-B20 | FIXED (Step 2) | `PtpInputOn` (`SET_FEATURE` 0x04), `PtpReportTouch` (Surface Switch, bit 0) and `PtpReportButton` (Button Switch, bit 1) start on, are restored by PrepareHardware, and gate the input report (`DeviceState::effective_reporting`). Clearing `PtpInputOn` or `PtpReportTouch` resets `ContactTracker`. |
| HF-B21 | PRESERVED (P1) | GET/SET_FEATURE never set `Information`. |
| HF-B22 | FIXED (Step 2) | The attributes response zeroes all 22 reserved bytes. |
| HF-B23 | PRESERVED for `reportBufferLen >= 2`; FIXED (P2/P3) below | SET_FEATURE still ignores `reportBufferLen` otherwise. If byte 1 lies outside the report buffer, 0x04 reads it as 0 (mouse mode, P2) and 0x06 changes nothing (P3). |
| HF-B24 | PRESERVED (P3) for successful reads | A successful zero-length completion still enqueues the work item, which re-reads at once. Failed reads are limited separately (HF-B7), so the loop needs a lower driver that keeps completing reads successfully with no data. |
| HF-B25 | FIXED (P2) | Without an open source `issue_lower_read` sends nothing, and GET_STRING completes with `STATUS_INVALID_DEVICE_STATE` until the source's strings are known. C used a NULL handle before the detour. |
| HF-B26 | REMOVED (data source) | C patched HIDCLASS's `MajorFunction[0x0F]` globally and never reverted it (the port kept it until 2026-11-27). The detour is gone: no undocumented structure is read and no driver object is written. |
| HF-B27 | FIXED (P3) | The synchronous mode switch is sent with a timeout (`wdf_support::init::SYNC_IO_TIMEOUT`, 5 s), and so are the string queries when the source opens. A timed-out mode switch fails like any other and the 2 s recovery timer retries it; on a timeout the framework cancels the request and waits for it to complete, so the stack transfer buffer stays valid. (The attribute query it also bounded is gone.) |
| HF-B28 | PRESERVED (P1) | The device interface `{FF969022-3111-4441-8F88-875440172C2E}` is registered with no handler (`config::DEVICE_INTERFACE_GUID`). |
| HF-B29 | FIXED (P3) | `Information` is the packet length as is. A value above the 1024-byte buffer is `LowerReadClass::Oversize` and arms the 3 s timer like a malformed packet. C took the low 32 bits sign-extended, so some such values were small valid lengths and others huge ones. |
| HF-B30 | FIXED (P3) | PrepareHardware keeps VID, PID and version (only SelfManagedIoInit sets them, now statically), so the restart after a PnP stop sends the mode switch again. Successful configurations issue the lower reads parked upper reads are missing. |

### Other P2 items, without a spec bug ID

* **Data races.** The C context fields are read and written without locks. The port keeps them as relaxed atomics in `DeviceState`.
* **Signed overflow.** The INT subtractions (`x - X.min`) use `wrapping_*`. The ScanTime subtraction saturates (`ScanClock`, PR #7).
* **Unaligned access.** The unaligned `USHORT*` and `UINT*` finger loads became explicit little-endian decoding.
* **Out-of-bounds reads.** Finger and button bytes outside the buffer read as 0. That never happens with the 1024-byte buffer: every input record was replayed with the tail filled with 0x00 and again with 0xFF, with identical results.
* **Alignment.** The mode-switch packet buffer is `#[repr(C, align(8))]`. The C `UCHAR[32]` is not guaranteed to be 8-aligned.

## Fixture coverage (`crates/amtptp-usb-bt/tests/bluetooth/fixtures`)

Every record is processed, except the 3 named skips in `lifecycle.txt` and the records of the removed detour and attribute query (13 in `lifecycle.txt`, 7 in `wire.txt`), which are counted by name. Each test asserts its per-file total and the count per category.

| File | Records | Processed | Skipped | Notes |
|---|---:|---:|---:|---|
| `descriptors.txt` | 99 | 99 | 0 | See below. |
| `ioctl.txt` | 519 | 519 | 0 | 27 named or probe codes have full `ev`. `GET_STRING` (C: forwarded to the PDO) is answered from the source's strings (`copy_string`). The 491 sweep codes complete with `NOT_SUPPORTED` as the only call. |
| `feature.txt` | 1588 | 1588 | 0 | 1068 are exact. The 516 successful SET 0x06 records with byte 1 assert the P3 bit order (Surface bit 0, Button bit 1: the C switches exchanged). Of the 4 `oob=1` records, the two 0x04 ones assert the HF-B23 fix (byte 1 read as 0) and the two 0x06 ones assert that nothing changes (P3). C read a stale `03` there. |
| `input_bt.txt` | 1653 | 1653 | 0 | See below. |
| `input_usb.txt` | 1661 | 1661 | 0 | Same as BT; the act counts are 1202 / 209 / 24 / 19 / 8, plus 1 + 1 HF-B9, 55 HF-B29 and 142 HF-B8. 6 records are `ub=int_min_neg`. |
| `wire.txt` | 65 | 58 | 7 | 16 are exact and 16 fault-injected, each with `WdfRequestGetStatus` after a TRUE mode-switch send (HF-B13). 20 assert a P3 fix derived from the record: 4 TRUE sends with a failed lower status, 12 timer records (gated to D0, no read after a failed switch) and 4 PnP restarts (IDs kept, so they equal the matching resume plus both selective-reporting switches on). Data source: the 5 Bluetooth start records run as init + `source_opened` and expect the C sequence without the detour and the attribute query (version 0); the 7 starts of other identities are counted as removed (the identity is static); `restart-without-detour` does nothing. `lower_reads_pending` is checked cumulatively per configuration. `tests/power.rs` covers the stop/resume sequences the C oracle got wrong and the data source's open/close. |
| `lifecycle.txt` | 53 | 37 | 16 | See below. |

`descriptors.txt`:

* 3 static records. 49 keys are checked against the port. 41 keys are not mirrored: 35 describe private C layout (the `DEVICE_CONTEXT`, `WORKER_REQUEST_CONTEXT` and `QUEUE_CONTEXT` offsets and sizes, `sizeof(BCM5974_PARAM)`, and 5 `HIDCLASS_DRIVER_EXTENSION` members), and 6 the removed detour's structures and major-function indexes.
* 58 records have a result decided by the driver.
* 38 records have a status decided by KMDF. That status is `0xC0000023`, from the stub model, which the test mock implements.

`input_bt.txt`:

* complete 1197, timer3s 226, setfailed_norestart 30, noupper 26, workitem 13.
* 1 outmem_fail and 1 copy_fail assert the HF-B9 fix: the upper request is completed with `0xC0000023` and `Information` is untouched.
* Every record completes successfully (C never read the status). 47 records assert the HF-B29 fix (`Information` above 1024: the 3 s timer instead of the C outcome) and 112 the HF-B8 fix (byte 0 not 0x31: the work item instead). The C act of these 159 records was complete 145, timer3s 13, workitem 1 (`Information` 0x1_0000_0000, an empty read for C).
* `out` is compared exactly, because the port zero-initialises (HF-B4), and the uninitialised C bits are 0 in `out`.
* 9 records are `ub=int_min_neg`.
* `rand` records are checked against the README 3.2 `ev` sequence for their `act`.

`lifecycle.txt`:

* 37 records are processed:
  * 12 are exact;
  * 3 assert P3 fixes: `prepare-hardware-after-start` keeps the IDs (HF-B30), and `workitem-run-unconfigured` and `timer-without-detour` do nothing;
  * 11 `SelfManagedIoInit` records (C: the detour variants and the attribute-query failure) assert the new start: SUCCESS, no I/O, the static identity, the device waiting for the source;
  * 2 `GET_STRING` records assert the answer from the source's strings (none yet: `STATUS_INVALID_DEVICE_STATE`);
  * 5 AddDevice records are exact;
  * 4 AddDevice records assert the HF-B16/HF-B17 fixes: an early failure return, or `STATUS_UNSUCCESSFUL`.
* **Removed**: the 13 `PtpFilterDetourWindowsHIDStack` records (the detour is gone).
* The object-creation arguments of `device-add-ok` are checked against `config`, with the HF-B15 and HF-B16 deltas.
* **Skipped**: 2 `DriverEntry` records and 1 driver-cleanup record. They are glue-only: `WdfDriverCreate` passes its status through, and the provider registers and unregisters. There is no core logic, and the driver crate implements them.

Cases excluded by the fixture harness itself (README section 6) are covered as follows:

* the HF-B18 divide by zero is covered by the unit test `classification_order`;
* NULL-handle sends (HF-B25) are guarded and have no fixture;
* the spec's Appendix D golden vectors are in `tests/golden.rs`.

## Driver crate (`drivers/interface/bluetooth/hid`, KMDF 1.31)

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
| `source.rs` | (not in C; replaces `Detour.c`, `include/Hac.h`) | the data source: interface notification, opening `Col03` from a work item, the parent check, the strings, the remote target's removal callbacks |
| `context.rs` | `include/Device.h`, `include/Queue.h` | `DEVICE_CONTEXT`, `QUEUE_CONTEXT` and `WORKER_REQUEST_CONTEXT`, registered under the C names for `!wdfkd` |

### C function to Rust function

| C | Rust |
|---|---|
| `DriverEntry` | `lib.rs::driver_entry` |
| `PtpFilterEvtDriverContextCleanup` | `lib.rs::evt_driver_context_cleanup` |
| `PtpFilterEvtDeviceAdd`, `PtpFilterCreateDevice`, `PtpFilterIoQueueInitialize` | `device.rs::evt_device_add`, then `core::add_device` with `device.rs::AddDevice` |
| `PtpFilterPrepareHardware`, `D0Entry`, `D0Exit` | `device.rs::evt_device_{prepare_hardware,d0_entry,d0_exit}`, then `core::{prepare_hardware,d0_entry,d0_exit}` |
| `PtpFilterSelfManagedIoInit` / `Restart` | `device.rs::evt_device_self_managed_io_{init,restart}`, then the core functions of the same name |
| `PtpFilterDetourWindowsHIDStack` | removed; `source.rs` and `core::{source_opened, source_closed}` replace it |
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
* Mode switch: `WdfRequestCreate(WDF_NO_OBJECT_ATTRIBUTES)` for the source, then `WdfMemoryCreatePreallocated` (parent = request) over the report the packet carries (`XferBuffer::report_mut`: `F1 02 01`), then `WdfIoTargetFormatRequestForIoctl(IOCTL_HID_SET_FEATURE)` with input = memory, then `WdfRequestSend(SYNCHRONOUS)` with `wdf_support::init::SYNC_IO_TIMEOUT` (C: no timeout), then `WdfObjectDelete`. C formatted the minidriver IOCTL over the whole 32-byte packet and set `Irp->UserBuffer`.
* Lower read: as C, but `WdfIoTargetFormatRequestForRead` for the source, the first 140 bytes of the lookaside buffer.
* No attribute query (C: `WdfIoTargetSendInternalIoctlSynchronously(GET_DEVICE_ATTRIBUTES)` to the PDO).
* GET_STRING: answered from the strings read when the source opened (C: forwarded to the PDO with send-and-forget).
* Feature IOCTLs read `WdfRequestWdmGetIrp(Request)->UserBuffer` (a NULL IRP is treated as a NULL `UserBuffer`; see Driver-side P2 fixes).
* Descriptor IOCTLs use `WdfRequestRetrieveOutputMemory` plus `WdfMemoryCopyFromBuffer`. Attributes use `WdfRequestRetrieveOutputBuffer(32, Length = NULL)`. So the framework, not the glue, produces the error statuses there.
* `KeQueryPerformanceCounter(NULL)` gives the counter for ScanTime, and `KeQueryPerformanceCounter(&x)` the frequency when the device is configured (`wdf_support::time`, HF-B6 fixed in PR #7).
* Code placement: `DriverEntry` is in `INIT`; device-add and driver cleanup are in `PAGE`, with `wdk::paged_code!()`, a debug IRQL assert like `PAGED_CODE()`. Everything else is in `.text`. That includes the PnP callbacks, which C also left non-paged, and everything reachable at DISPATCH_LEVEL (dispatch, completion routine, read issue).

### Driver-side P2 fixes

| Item | C | Rust |
|---|---|---|
| HF-B10 | The completion routine reads `RequestMemory` from the context of the request it has just deleted. | The routine copies the device-context pointer and `RequestMemory` into locals before calling the core. The core deletes the request, then the memory. Afterwards the routine touches neither, nor the request context. |
| HF-B15 | The lookaside list is created with `WDF_NO_OBJECT_ATTRIBUTES`, so its parent is the WDFDRIVER, and one list leaks per AddDevice. | `ParentObject = device`. |
| HF-B16 | The timer and the work item use `AutomaticSerialization = TRUE`. A creation failure is only logged. | `FALSE` on both. That has no effect under the inherited sync scope None, and it removes the possible `STATUS_WDF_INCOMPATIBLE_EXECUTION_LEVEL` against the Dispatch-level device. Every creation failure fails AddDevice. |
| HF-B17 | A NULL WDM device object returns SUCCESS. | `STATUS_UNSUCCESSFUL`. |
| Data races | Context fields are plain, shared without locks. | `DeviceState` uses relaxed atomics. The handles are `AtomicPtr`: `Relaxed` for the handles stored once in AddDevice, and `Release`/`Acquire` for `HidIoTarget`, which the data source stores and clears (the C detour stored it once). No `&mut` to a context is formed after its initializing `ptr.write`. |
| NULL `reportBuffer` in a feature packet (no spec ID) | C writes or reads through it. | It becomes an empty slice. GET 0x07/0x08 then returns `STATUS_INVALID_BUFFER_SIZE`. SET 0x04/0x06 reads byte 1 as 0 and returns `STATUS_SUCCESS`, as in HF-B23. |
| NULL IRP for a feature request (no spec ID, defensive, unreachable) | `WdfRequestWdmGetIrp(Request)->UserBuffer` (Hid.c:200, Hid.c:287) dereferences the returned IRP without checking it, so a NULL IRP would be a NULL dereference. | `wdf_support::wdm::request_user_buffer` returns NULL when there is no IRP. GET/SET_FEATURE then treat it as a NULL `UserBuffer` and complete with `STATUS_INVALID_DEVICE_REQUEST`, after the same 16-byte length check. A request presented to `EvtIoInternalDeviceControl` always carries its IRP, so this cannot happen. |
| Alignment | The `UCHAR[32]` mode-switch buffer is not guaranteed to be 8-aligned. | `XferBuffer` is `align(8)`. The attribute query uses a real `HID_DEVICE_ATTRIBUTES` local, not the core's `[u8; 32]`. `HID_XFER_PACKET` fields are read with `read_unaligned`. |
| NULL handles to WDF (defensive, unreachable) | HF-B25: a NULL `HidIoTarget` is sent to WDF, which bugchecks (the port's target is the data source's; it is NULL while the source is closed). The lower-read completion dereferences the `WdfMemoryGetBuffer` result without a check (Input.c:151, 202-203, 221). | Every glue method that needs `HidIoTarget` checks it. A NULL target gives `STATUS_INVALID_DEVICE_STATE`, or "not sent", and nothing reaches WDF. The core never gets there: it checks `io_target_set` first. `WdfRequestRetrieveOutputBuffer` succeeding with a NULL buffer gives `STATUS_INVALID_DEVICE_REQUEST` instead of a write through NULL. A NULL `WdfMemoryGetBuffer` result in the lower-read completion (`io.rs::lower_read_buffer`) becomes an empty slice. Every finger and button byte then reads as 0, and the parked upper read is completed with that report. This is unreachable: a live lookaside WDFMEMORY always has its buffer, because `WdfMemoryCreateFromLookaside` fails otherwise and the read is never sent. |
| HF-B9 (core) | The upper request leaks. | Completed with the failure status. `WdfDeviceSetFailed` is still called. |

Exactly-once completion:

* The core completes, parks or forwards every upper request exactly once. That is the fixture mock's double-completion assertion, checked on every record.
* The glue's `complete` goes through `wdf_support::request::Request::from_raw(..).complete(status)`.
* Driver-created requests (lower reads, the mode switch) are deleted, never completed.

### Preserved driver-side issues (P1/P3)

* **HF-B28.** The device interface is registered with no handler.
* **`EvtIoStop`** is registered and empty. It neither acknowledges nor completes.
* **KMDF version.** The C project bound the WDK default, KMDF 1.15. The Rust workspace targets KMDF 1.31 for all four drivers. See [the framework version and OS floor](packaging.md#framework-version-and-os-floor).

### Competing reader on Bluetooth (fixed by the data source)

The Bluetooth HID device itself (`BTHENUM\{00001124-...}`, HidBth under HIDCLASS) has its own HIDCLASS, and that HIDCLASS keeps its own reads pending at HidBth. Through the detour, the filter read HidBth at the same time. HidBth gives each input packet to the oldest pending read, so a packet that landed in HIDCLASS's read never reached the filter, and HIDCLASS dropped it, because report 0x31 is not in the SDP descriptor.

Measured with a Magic Trackpad 2 during multi-finger gestures (2026-11-27):

| Driver | Interrupt packets | Did not reach the filter | Read failures (`DATA_ERROR`) |
|---|---:|---:|---:|
| C (release 2021.3979.1.1000), detour | 2514 | 86 (3.4%) | not traced |
| Rust port, detour | 2125 | 74 (3.5%) | yes (HF-B7) |
| Rust port, detour (with a trace filter below HidBth) | 2247 | 60 (2.7%) | 27 |
| Rust port, data source | 879 | **0** | **0** |

What happened, as the trace filter below HidBth showed: HIDCLASS's reads were 8 bytes (the largest report in the SDP descriptor), so HidBth gave each one a 9-byte transfer. A multitouch packet (13 bytes or more) filled it and the rest went to the next read; HIDCLASS dropped both pieces, and a rest that landed in the filter's read failed it with `STATUS_DEVICE_DATA_ERROR`. HIDCLASS's error handling then backed off for about a second (`HidpPingpongBackoffTimerDpc`), which spread the losses out.

With the data source no reader competes: HIDCLASS is the only reader of HidBth, its reads are 140 bytes (the declared length of `0x31`), and it hands every report to this filter's handle on `Col03`.

The traces used: `logman` with the filter providers, BTHPORT `{8a1f9517-3a8c-4a9e-a018-4f17a200f277}` level 5 (event 402 carries the HCI packet heads), and HIDCLASS `{6465DA78-E7A0-4F39-B084-8F53C7C30DC6}` (event 3: per-device read counters).

### Not verified (no device test on this machine)

* That KMDF 1.31 returns the expected framework statuses on the `wdf=`/stub-model paths, for example `STATUS_BUFFER_TOO_SMALL` for a short output buffer.
* That a passive-level timer with `AutomaticSerialization = FALSE` creates under a Dispatch-level parent. The WDF documentation says it should, but this was not run on a device.
* Sleep and resume with the data source.

### Tracing

* Provider `AmtPtpHidFilterTraceProvider`, id `eeb34803-f3ec-4bb1-97ce-b42405d7f3c2` (the C WPP control GUID).
* Keywords: 0x01 driver, 0x02 device, 0x04 queue, 0x08 HID, 0x10 input. These are the WPP flag bits.
* Events:
  * Informational: driver init and cleanup; DeviceAdd with its status; PrepareHardware; D0Entry/D0Exit with the power state; SelfManagedIoInit/Restart with status, IDs, `DeviceConfigured` and `SourceOpen`; SourceOpened, SourceReopened, SourceQueryRemove, SourceRemoved, SourceClosed; MultitouchModeSet (status and milliseconds taken; a TRUE send can still carry a failure, HF-B13); RecoveryTimerStarted (due time) and RecoveryTimerFired; MalformedInput (response length, Input.c:170); GetFeature and SetFeature (report ID and `reportBufferLen`); SetInputMode (`PtpInputOn`) and SelectiveReporting (button and surface bits as the core decodes them, HF-B20) for SET_FEATURE 0x04/0x06; SetFeatureNotSupported (report ID and status). The completion routine classifies the read with `core::classify_lower_read` before calling the core, only to trace it.
  * Error: each failed AddDevice step; SourceFailed (step and status of opening the source); the mode-switch send failures; lower-read create/memory/format/send failures; DeviceSetFailed; UnsupportedDeviceInput (VendorID, Input.c:157); InputDroppedNoParkedRead (status, Input.c:178; only from the completion routine, not from the D0Exit drain); RetrieveOutputMemoryFailed and CopyToOutputMemoryFailed (status, Input.c:228/236, also on the descriptor IOCTLs). Together these name the cause of each DeviceSetFailed(AttemptRestart).
  * Warning: unsupported IOCTLs (code and name); SourceOfAnotherDevice (a `Col03` of another parent); SourceStringFailed.
  * Verbose: every request completion (handle and status).

# AmtPtpDeviceSpiKm: Rust port record

The SPI report descriptor now has 496 bytes. The settings collection and its vendor report have been removed. The figures below describe the original port.

The C driver is `src/AmtPtpDeviceSpiKm` at commit 2edb0f2. The Rust port has two parts:

- `crates/amtptp-spikm-core` (MIT, `no_std`, depends only on `ptp-hid`) holds all the logic that does not need WDF.
- `drivers/km/AmtPtpDeviceSpiKm` is the KMDF 1.23 glue.

The porting spec is [`specs/spikm.md`](specs/spikm.md); read its verification addendum too. Bug IDs `SPI-Bn` are the spec's `Bn` (§10.3).
The policy terms come from the [behavior policy](README.md#behavior-policy):

- **P1/P3 PRESERVED**: the behavior is kept as is, because it is observable or is a logic bug.
- **P2 FIXED**: the behavior was undefined, memory-unsafe, a leak or a hang, and the port fixes it in the way closest to what C intended.

## Decisions per spec bug

| ID | Decision | What the Rust code does |
|---|---|---|
| SPI-B1 | FIXED (PR #7) bytes, PRESERVED (P1) Information | GET_FEATURE 0x08 writes all 257 bytes, the report ID and the 256-byte certification blob (`ptp_hid::feature::HQA_REPORT`). Information stays 0 (SPI-B4). |
| SPI-B2 | FIXED (P3) | SET_FEATURE 0x06 reads byte **1**, the descriptor's 2-byte layout (`ptp_hid::feature::SelectiveReportBits`). A report without byte 1 changes nothing. C read byte 2, past the real report. |
| SPI-B3 | FIXED (Step 2) | `PtpInputOn` (`SET_FEATURE` 0x04), `PtpReportTouch` (Surface Switch, bit 0) and `PtpReportButton` (Button Switch, bit 1) start on, are restored by every PrepareHardware, and gate the contacts and the button of the input report (`PtpSettings::effective_reporting`, `input::build_report`). Clearing `PtpInputOn` or `PtpReportTouch` resets `ContactTracker`. |
| SPI-B4 | PRESERVED (P1/P3) | GET/SET_FEATURE never set Information, so it stays 0 (`feature::FEATURE_INFORMATION`). SET does not validate `reportBufferLen`. Bytes past it read as 0 (see the P2 extras). |
| SPI-B5 | FIXED (P2) | The input report is zero-initialized (`SpiPtpReport::EMPTY`). Unused contacts and the padding bits 5-7 are 0 and no longer carry stack bytes. |
| SPI-B6 | FIXED (P3) | `ContactCount` is `min(NumOfFingers, 5)`. A lower read that does not hold every finger record `NumOfFingers` declares (`46 + 30 * n` bytes) completes the upstream read with `STATUS_DEVICE_DATA_ERROR` (`input::check_declared_fingers`). |
| SPI-B7 | FIXED (Step 2) | `ptp_hid::contact_tracker::ContactTracker` assigns persistent slot IDs `0..=4` by nearest-neighbor matching, synthesizes 1-frame `TipSwitch = 0` liftoff reports, and latches `Confidence = 0` palms. The SPI descriptors declare a contact-ID logical maximum of 4 (`SPI_CONTACT_ID_LOGICAL_MAX`), include `GLOBAL_RESET` after all five finger collections and `Scan Time`, and set `Input Mode` logical maximum to 3 (`519` bytes). |
| SPI-B8 | FIXED (PR #7) | `ptp_hid::scan_time::ScanClock` reports the time since the first report of the session in 100 µs units: the first report is 0 and the 16-bit value wraps every 6.5536 s. D0Entry and every successful SelfManagedIo init, restart or recovery cache the QPC frequency and start a new session (`runtime::DeviceRuntime`). The read path samples the counter, the return value of `KeQueryPerformanceCounter(NULL)`, once after the length gates. |
| SPI-B9a | PRESERVED (P3) | The lower `IoStatus.Status` is ignored. Only `(LONG)Information >= 46` is checked (`input::lower_read_length`). |
| SPI-B9b | PRESERVED (P3) | When the length is below 46, the request completes with `STATUS_DEVICE_DATA_ERROR`. The "re-enable" that C traces is never performed. |
| SPI-B9c | FIXED (P2) | Packet bytes at or past `min((LONG)Information, 1024)` read as 0 (`input::SpiPacket`). C read stale or uninitialized lookaside bytes there. The fixtures compare `out_tail0` on `b9=1` records. |
| SPI-B10 | FIXED (P2), required of the glue (stage 2) | The use-after-free is removed. In the completion routine the glue reads the memory handle first, then deletes the lower request, then the memory. |
| SPI-B11 | PRESERVED (P1) | A lower completion that finds `HidQueue` empty drops the data. No QPC call, no clock sample and no new read follow. |
| SPI-B12 | FIXED (P3) | `SelfManagedIoSuspend` stops the recovery timer. The timer callback works only on a device in D0 that still waits for the mode switch (`runtime::DeviceRuntime::begin_recovery`), and only marks it configured if no suspend happened meanwhile (`finish_recovery`). Success issues one lower read per queued request. |
| SPI-B13 | FIXED (P3) | Every transition to configured (SelfManagedIoInit/Restart or the timer) issues a lower read for each upstream read already in `HidQueue` (`runtime::DeviceRuntime::backlog_reads`). |
| SPI-B14 | FIXED (P3) for reads, PRESERVED for the mode | `SelfManagedIoSuspend` starts a new epoch and cancels the lower reads in flight (`runtime::ReadTracker`, behind a spin lock in the glue). A read from an earlier epoch is dropped when it completes and never consumes a request queued after the resume. D0Exit still does not send `SetState(FALSE)`. |
| SPI-B15 | FIXED (P2) | `DeviceStatus`, the ScanTime clock (`ScanClock`, which replaced `LastReportTime` in PR #7) and the PTP flags are atomics (`runtime::DeviceRuntime`, `feature::PtpSettings`). Their single-variable semantics are the same as before. |
| SPI-B16 | PRESERVED (P1) | Unknown internal IOCTLs complete with `STATUS_NOT_SUPPORTED` and are not forwarded (`ioctl::classify_internal_ioctl`). |
| SPI-B17 | RESOLVED | The INF no longer binds 0x0280 to this driver (it is a USB KMDF binding). `device::SPI_MODELS` is the only PID list; the `xtask` INF test checks that every SPI binding resolves to it. |
| SPI-B18 | FIXED (Step 2) | The Family 1 Y logical maximum is 6217, matching the table's `6089 - (-128)` span (`descriptors::AXES_F1`). |
| SPI-B19 | FIXED (Step 2) | X and Y are clamped to `0..=(max - min)` (`input::build_report`). |
| SPI-B20 | FIXED, required of the glue (stage 2) | `WdfPdoInitAllowForwardingRequestToParent`, a PDO-only DDI called on an FDO init, is dropped. It had no functional effect. |
| SPI-B21 | FIXED (P2), required of the glue (stage 2) | The lookaside list is parented to the device instead of the driver, which ends the per-device leak. |
| SPI-B22 | FIXED, required of the glue (stage 2) | Nothing on the IOCTL, completion or timer paths is pageable. `PAGED_CODE()` is not ported. |
| SPI-B23 | FIXED (P3) | Both synchronous lower IOCTLs are sent with a timeout (`wdf_support::init::SYNC_IO_TIMEOUT`, 5 s, the USB 2.0 bound for a control request with a data stage). A mode switch that times out fails like any other, and the 5 s recovery timer retries it; on a timeout the framework cancels the request and still waits for it to complete. |
| SPI-B24 | FIXED (P2) | The `DesiredReportType` value buffer is zero-initialized before the query (`device::desired_report_type_value`). A value of 1-3 bytes is therefore read with zero upper bytes. |
| SPI-B25 | FIXED (N/A) | The wrong `_IRQL_requires_(PASSIVE_LEVEL)` annotations have no counterpart in Rust. The handlers are DISPATCH-safe and non-pageable. |

### Other P2 fixes (no spec bug ID)

| ID | What the Rust code does |
|---|---|
| SPI-X1 | When the length check passes but `reportBuffer` is NULL, GET_FEATURE 0x07/0x08 returns `STATUS_INVALID_DEVICE_REQUEST`. C dereferenced NULL here. |
| SPI-X2 | SET_FEATURE byte rule: a byte at or past `reportBufferLen` reads as 0. For 0x04 with `reportBufferLen < 2` that means mode 0, so `PtpInputOn` becomes FALSE; C read past the report. A byte inside the length with a NULL buffer gives `STATUS_INVALID_DEVICE_REQUEST`. |
| SPI-X3 | The ScanTime subtraction saturates (`ScanClock`, PR #7). C's signed overflow was undefined behavior. |
| SPI-X4 | The mode-switch buffer is an 8-byte-aligned `#[repr(C, align(8))]` struct (`wire::SetStatePacket`). C cast an unaligned `UCHAR[255]`. The bytes, including the self-pointer to offset 16, are the same as in C. |
| SPI-X5 | The config-table lookup iterates the 9 entries and does not rely on a sentinel. The result is identical for every input. |
| SPI-X6 | All packet fields are decoded as explicit little-endian `i16`/`u8` reads, not through packed or unaligned struct access. |

### Kept as C (worth knowing)

- **`HID_DEVICE_ATTRIBUTES.Reserved`** is zeroed in the 32-byte attributes response (Step 2).
- **Identity**:
  - VID 0x8910.
  - The real PID and version, for any PID.
  - Information is 32 for the attributes, 9 for the HID descriptor and 519 for the report descriptor.
- **`ReportType`**:
  - It is a raw `u32`. Values other than 0 and 1 give `STATUS_NOT_FOUND` for the report descriptor only.
  - A missing value, missing key or an unknown value keeps the previous `ReportType` across restarts of the same device.
- **PrepareHardware**:
  - It stores VID, PID and version before the table lookup.
  - A warning status from the lower `GET_DEVICE_ATTRIBUTES`, for example `0x80000005`, fails the start.
  - The registry is not read after `STATUS_NOT_FOUND`.
- **Check order in GET/SET_FEATURE**: the IOCTL buffer length (< 16 gives `STATUS_BUFFER_TOO_SMALL`) is checked before the NULL `UserBuffer` (`STATUS_INVALID_DEVICE_REQUEST`).
- **Descriptor IOCTLs**: the output memory is retrieved before the PID switch. A zero-length buffer therefore gives `STATUS_BUFFER_TOO_SMALL` even for an unknown PID.
- **Glue items that stay as in the spec**:
  - The timer's `AutomaticSerialization` is FALSE. It has no behavioral difference under sync scope None.
  - `EvtIoStop` is omitted.
  - The device interface `{f271e39d-d211-4a6e-a439-63e51940d210}` is kept.

## Fixture coverage (`crates/amtptp-spikm-core/tests/`)

Every record of every fixture file is processed. Each test asserts the exact record count and a per-bucket count. **No record is skipped.**

Some records have a result decided by the stub KMDF (a `wdf=` entry). For those, the test reproduces the README's KMDF model and still asserts the driver's half: that no bytes were written, the `LastReportTime` value, the issue decision, and so on. They are counted under a named "framework" bucket.

| File | Records | Buckets (count) |
|---|---|---|
| `input_f1.txt` | 1242 | report delivered 834; length gate `c000009c` 103; frame shorter than its declared fingers `c000009c` 302 (P3, includes the 94 `b9=1` records); framework `RetrieveOutputMemory` (outlen 0) 1; framework `MemoryCopyFromBuffer` (outlen < 30) 2. Delivered reports are compared with `ContactCount` clamped to 5 (P3). |
| `input_f2.txt` | 1174 | 756 / 89 / 326 / 1 / 2; `b9=1` 97 |
| `input_f3a.txt` | 1174 | 778 / 92 / 301 / 1 / 2; `b9=1` 101 |
| `input_f3b.txt` | 1140 | 701 / 100 / 336 / 1 / 2; `b9=1` 92 |
| `feature.txt` | 572 | GET 278: success 12, driver error 266. SET 294: success 21, driver error 260, SET 0x06 decoded from byte 1 13 (P3, includes the 2 `oob=1` records). None has `wdf`. |
| `descriptors.txt` | 85 | driver success 63; `STATUS_NOT_FOUND` from injected states 10; framework output retrieval (outlen 0 or < 32) 7; framework copy too small 5 |
| `ioctl.txt` | 76 | not supported 64; GET_STRING forwarded 2; framework GET_STRING send failure 4; READ_REPORT queued 4; framework forward-to-queue failure 2 |
| `wire.txt` | 46 | PrepareHardware 28; PrepareHardware twice 10; mode switch + SMIO/timer 3; lower read issued 1; framework lower-read creation/format/send failure 4 |
| `power.txt` | 100 | 12 sequences: D0Entry 15, SMIO init/restart 15, timer 5, READ_REPORT 31, lower completion 22, D0Exit 11, framework cancellation 1. 7 sequences (56 records) are replayed with `SelfManagedIoSuspend` before each D0Exit; the 5 that recorded SPI-B12/B13 (44 records) are rewritten as tests of the fixed behavior in `fixture_power.rs`. |
| `setup.txt` | 12 | success with constants asserted 2; driver status no-I/O-target `c0000184` 1; framework-injected failures 7 (AddDevice/DriverEntry sequence; the WDF call order is checked in stage 2); no driver logic (cleanup, empty `EvtIoStop`) 2 |

Records that differ from the raw C output because of a P2 fix, and how the tests handle them:

- **`undef`** (SPI-B5): the test compares with `out`, which is the zero-painted run and equals a zero-initialized report. It also asserts that every bit flagged in `undef` is 0, and that `undef` follows the README rule on all edge records.
- **`b9=1`** (SPI-B9c): the test compares with `out_tail0`. As a cross-check, parsing the whole 1024-byte buffer the way C does reproduces `out`.
- **`oob=1`** (SPI-B2): C read byte 2, past `reportBufferLen`. The port reads byte 1 (P3), so these records are checked against the byte-1 rule instead of the C flags.

The README's excluded inputs are covered by unit tests instead:

- SET 0x04/0x06 with a short `reportBufferLen`;
- a NULL `reportBuffer`;
- ScanTime overflow (now `scan_time_advances_and_wraps_in_spi_reports`, which also checks the 16-bit wrap).

`tests/golden.rs` checks the spec's own vectors:

- the 8 descriptor SHA-256 values;
- the HQA blob SHA-256;
- Appendix A and the family patches;
- the §7.4 worked example;
- the §6.2 buffer;
- the bitfield encodings;
- the pool tag.

## Driver glue (stage 2): `drivers/km/AmtPtpDeviceSpiKm`

The KMDF 1.23 driver is a thin layer over the core crate. Its WDF call sequence was compared, path by path, with the `ops` field of the C oracle fixtures:

- `setup.txt` (adddevice);
- `wire.txt` (`prep-*`, `setstate-*` and `lower-read-*`);
- `power.txt`;
- `ioctl.txt`.

The calls, their arguments and their order are the same, apart from the differences listed below. This section resolves the items marked "required of the glue" and "open" in the table above.

### C function to Rust function

| C (file) | Rust (`src/...`) | Core calls |
|---|---|---|
| `DriverEntry`, `AmtPtpDeviceSpiKmEvtDriverContextCleanup`, `AmtPtpDeviceSpiKmEvtDeviceAdd` (Driver.c) | `lib.rs`: `driver_entry` (INIT); `evt_driver_context_cleanup` and `evt_device_add` (PAGE) | - |
| `AmtPtpDeviceSpiKmCreateDevice` (Device.c) | `device.rs`: `create_device` + `initialize_device` (PAGE) | `wire` constants |
| `AmtPtpEvtDevicePrepareHardware` | `device.rs`: `evt_device_prepare_hardware`, `query_desired_report_type` | `DeviceConfig::identify`, `desired_report_type_value`, `apply_desired_report_type`, `SharedDeviceConfig` |
| `AmtPtpEvtDeviceD0Entry` / `D0Exit` | `device.rs`: `evt_device_d0_entry` / `evt_device_d0_exit` | `on_d0_entry` / `on_d0_exit` |
| (not in C) `EvtDeviceSelfManagedIoSuspend` | `device.rs`: `evt_device_self_managed_io_suspend`, `cancel_in_flight_reads` | `suspend` |
| `AmtPtpEvtDeviceSelfManagedIoInitOrRestart` | `device.rs`: `evt_device_self_managed_io_init_or_restart` | `on_self_managed_io_init_or_restart`, `backlog_reads` |
| `AmtPtpSpiSetState` | `device.rs`: `spi_set_state` | `SetStatePacket::fill` |
| `AmtPtpPowerRecoveryTimerCallback` | `device.rs`: `power_recovery_timer_callback` | `begin_recovery`, `finish_recovery`, `backlog_reads` |
| `AmtPtpDeviceSpiKmQueueInitialize`, `...EvtIoInternalDeviceControl` (Queue.c) | `queue.rs`: `queue_initialize` (PAGE), `evt_io_internal_device_control` | `classify_internal_ioctl` |
| `AmtPtpGetHidDescriptor`, `...GetReportDescriptor`, `...GetDeviceAttribs`, `...GetStrings`, `...ReportFeatures`, `...SetFeatures` (Hid.c) | `hid.rs`: `get_hid_descriptor` and `get_report_descriptor` (both through `copy_descriptor`), `get_device_attribs`, `get_strings`, `report_features`, `set_features` | `hid_descriptor_for`, `report_descriptor_for`, `device_attributes`, `check_xfer_packet`, `get_feature`, `DeviceRuntime::set_feature` |
| `AmtPtpSpiInputRoutineWorker`, `...InputIssueRequest`, `AmtPtpRequestCompletionRoutine` (Input.c) | `input.rs`: `spi_input_routine_worker`, `spi_input_issue_request`, `issue_backlog_reads`, `request_completion_routine` (with `serve_oldest_read` and `copy_report`) | `should_issue_lower_read`, `claim_lower_read`, `finish_lower_read`, `complete_lower_read` |
| `AmtPtpDeviceSpiKmEvtIoStop`, `DbgDevicePowerString`, `DbgIoControlGetString` | not ported (dead or trace-only) | - |

The two contexts keep their C names for `!wdfkd`:

- `DEVICE_CONTEXT` is `device::DeviceContext`;
- `WORKER_REQUEST_CONTEXT` is `input::WorkerRequestContext`.

Their sizes differ from C. That is fine because they are WDF context blobs, not an ABI.

### Driver-side fixes (P2) and deliberate differences

| ID | What the driver does | Visible in fixture `ops`? |
|---|---|---|
| SPI-B10 | The completion routine copies `RequestMemory` and `DeviceContext` out of the worker context **before** `WdfObjectDelete(lower request)`, then deletes the memory. C read the context after deleting its owner. | No (the delete order `Delete(l); Delete(m)` is unchanged) |
| SPI-B20 | `WdfPdoInitAllowForwardingRequestToParent` is not called. It is a PDO-only DDI and has no effect on a filter FDO. | Yes (AddDevice makes one call fewer) |
| SPI-B21 | The lookaside list has `ParentObject = device`. It is deleted with the device instead of leaking until driver unload. Memory attributes are still NULL. | Yes (`attrs` is set instead of null) |
| SPI-B22 / SPI-B25 | `PAGE` holds only what C put there and only PASSIVE-level code: device add, device and queue creation, and the driver cleanup. DriverEntry is in `INIT`. These are all non-paged: the IOCTL handlers, the read path, the completion routine, the timer callback and every PnP/power callback. The `PAGED_CODE()` calls of the Hid.c handlers are not ported. | No |
| SPI-X7 (extends SPI-B15) | `HidProductID`, `HidVersionNumber`, `TrackpadInfo` and `ReportType` live in `core::device::SharedDeviceConfig`, one atomic per field. In C they were plain fields, and access to them was a data race: PrepareHardware runs again on every restart of the same device, while the non-power-managed default queue keeps dispatching IOCTLs and lower reads may still complete. Readers take one snapshot per request. | No |
| SPI-X8 | Each lookaside buffer is zeroed (`WdfMemoryGetBuffer` getter + memset) right after `WdfMemoryCreateFromLookaside`, before the lower read is sent. Lookaside memory is not zero-initialized, and the completion routine forms a byte slice over the buffer; zeroing means that slice never covers uninitialized memory. The core reads only the first `(LONG)Information` bytes (SPI-B9c), so the change is not observable. | No (only a getter is added) |
| SPI-X9 | Caller memory is only copied through raw pointers, never referenced. The `HID_XFER_PACKET` is read as an unaligned 16-byte value after the C length and NULL checks. GET writes `07 05 00` or the 257-byte HQA report and nothing else. SET copies `min(reportBufferLen, 3)` bytes into a local. The attributes request writes 32 bytes with `Reserved` zeroed. | No |
| SPI-X10 | Every callback checks for a missing `DEVICE_CONTEXT` instead of dereferencing NULL. C checked only in PrepareHardware and SetState, where it returned `STATUS_INVALID_DEVICE_STATE`. PrepareHardware keeps that status. The other callbacks return or complete with it, and the timer and completion callbacks skip their work. The case is unreachable, because every device of this driver is created with that context. | No |
| SPI-X11 | The 32-byte `GET_DEVICE_ATTRIBUTES` output buffer and the 255-byte mode-switch packet are 8-byte-aligned stack locals. C used an aligned struct for the first and an unaligned `UCHAR[255]` for the second. The packet points to itself, so it is filled in place and not moved until the synchronous send returns. | No (same bytes, see `setstate-*`) |
| SPI-X12 | The remaining defensive NULL checks. None of these cases can happen, and C would dereference NULL in each. (a) `input.rs` `serve_oldest_read`: if `Params`, `Params->Parameters.Ioctl.Output.Buffer` or the `WdfMemoryGetBuffer` result is NULL, the packet is empty and every byte reads as 0. With `(LONG)Information >= 46` the oldest READ_REPORT then completes with SUCCESS and an all-zero report: `05`, no contacts, ContactCount 0, button 0, ScanTime computed as usual. Below 46 it completes with `STATUS_DEVICE_DATA_ERROR`, as usual (C: Input.c:200, 225). KMDF always passes the completion parameters, and they carry the lookaside memory the read was formatted with. (b) `input.rs` `request_completion_routine`: with a NULL worker context, no upstream read is served and the lower request is deleted. The lookaside memory is not deleted, because its handle exists only in that context; the leak is accepted for an impossible state (C: Input.c:183, 318). `spi_input_issue_request` never registers a NULL context, so this branch is dead by construction. (c) `hid.rs` `get_device_attribs`: a NULL buffer after a successful `WdfRequestRetrieveOutputBuffer` completes with `STATUS_INVALID_DEVICE_REQUEST` (C: Hid.c:342). (d) `wdf_support::wdm::request_user_buffer`, used by `hid.rs` `read_xfer_packet`: a request without an IRP is treated as a NULL `UserBuffer`, so GET/SET_FEATURE complete with `STATUS_INVALID_DEVICE_REQUEST` after the 16-byte length check (C: Hid.c:614, 759). This is the same entry as in hidfilter.md. A request presented to `EvtIoInternalDeviceControl` always carries its IRP. | No |
| (spec §2.4) | The recovery timer has `AutomaticSerialization = FALSE` instead of TRUE. Under synchronization scope None the behavior is the same, and FALSE avoids the documented-invalid combination of a passive timer and a dispatch-level parent. | Yes (`autoserial=0`) |
| (spec §2.7) | The empty template `EvtIoStop` is not registered. On a non-power-managed queue it would run only at removal, for driver-owned requests, and an `EvtIoStop` that does not acknowledge behaves exactly like none. | Yes (`IoStop=0`) |
| - | D0Entry reads the QPC frequency before it stores `DeviceStatus`, while C stored the status first. Nothing can observe the order of these two writes. | No |

Every request that is presented to the dispatcher or retrieved from `HidQueue` goes through `wdf_support::request::Request`. It is handled exactly once:

- completed;
- forwarded to `HidQueue`; or
- sent and forgotten (GET_STRING).

When the forward or the send fails, the request is completed with that status, as in C.

Lower reads that the driver creates itself are never wrapped in `Request`. They are deleted instead:

- when an issue step fails, the memory is deleted first and then the request, as in C;
- otherwise they are deleted after completion.

### Preserved known issues (driver side)

- **SPI-B23** (fixed): the two synchronous lower IOCTLs are sent with `wdf_support::init::SYNC_IO_TIMEOUT` (5 s):
  - `GET_DEVICE_ATTRIBUTES` in PrepareHardware;
  - the mode-switch `SET_FEATURE` in SelfManagedIoInit/Restart and in the recovery timer.

  Without it a lower driver that never completes them hung device start or resume (on the PnP/power thread), or the timer's work item. A slow S3 wake that exceeds the timeout fails that attempt only: the 5 s recovery timer (commit e92581b) retries the mode switch. If the lower driver ignores the cancellation, the framework keeps waiting for its completion and returns its real status. Both sends run at PASSIVE_LEVEL, as in C.
- **SPI-B12/B13/B14 in the glue**:
  - `SelfManagedIoSuspend` suspends the read tracker under its `WDFSPINLOCK`, references the reads it hands out, stops the timer with `Wait = FALSE`, then cancels and dereferences each read. Nothing waits for the cancelled reads; their completions are stale and are dropped. D0Exit does not send `SetState(FALSE)`.
  - The timer callback takes a recovery ticket first and does nothing (not even the mode switch) unless the device is in D0 and unconfigured.
  - Every transition to configured reads the `HidQueue` depth (`WdfIoQueueGetState`) and issues the missing lower reads.
- **SPI-B16**: unknown internal IOCTLs complete with `STATUS_NOT_SUPPORTED` and are not forwarded. Non-internal request types have no queue callback, so the framework forwards them down (filter rule), as in C.
- **Identity and inheritance**: the PnP/power callback setter is the only `WdfDeviceInitSet*` call. The filter therefore inherits these from the lower device:
  - the I/O type;
  - `DO_POWER_PAGABLE`/`INRUSH`;
  - the device type;
  - the characteristics.

  The device interface `{f271e39d-d211-4a6e-a439-63e51940d210}` is still registered.

### Tracing

The driver uses the TraceLogging provider `AmtPtpDeviceSpiKmTraceProvider`, id `{9b319d80-fe25-41e4-80ef-7d8528f7f1b5}`, which is the C WPP control GUID.

Keywords are the WPP flag bits:

| Keyword | Area |
|---|---|
| 0x1 | driver |
| 0x2 | device (lower reads) |
| 0x4 | queue |
| 0x8 | HID input |

Events:

- driver and device setup: `DriverEntry`, `WdfDriverCreateFailed`, `DriverContextCleanup`, `DeviceAdd`, `CreateDeviceFailed`, `QueueCreateFailed`;
- PrepareHardware: `PrepareHardware`, `PrepareHardwareFailed`;
- power: `D0Entry`, `D0Exit` (with the number of cancelled reads);
- mode switch: `SetState`, `SetStateFailed`, `SetStateFailedRetryIn5s`, `PowerRecoveryTimer`;
- IOCTLs: `IoctlNotSupported`, `HidRequestFailed`, `GetStringForwardFailed`, `GetFeatureFailed`, `SetFeature`;
- read path: `ForwardToHidQueueFailed`, `LowerReadIssueFailed`, `LowerReadSendFailed`, `LowerReadDropped`, `LowerReadTooShort`, `ReportCopyFailed`.

Nothing is traced for a report that is delivered.

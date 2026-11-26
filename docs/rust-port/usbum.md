# AmtPtpDeviceUsbUm: Rust port record

The C driver is `src/AmtPtpDeviceUsbUm` at commit 2edb0f2. The Rust port has two parts:

- `crates/amtptp-usbum-core` (GPL-2.0-only, `no_std`, depends only on `ptp-hid`) holds all the logic that does not need WDF.
- `drivers/um/AmtPtpDeviceUsbUm` is the UMDF 2.15 glue.

The porting spec is [`specs/usbum.md`](specs/usbum.md); read its verification addendum too. Bug IDs `UM-Bn` are the spec's `Bn` (§10.1).
The policy terms come from the [behavior policy](README.md#behavior-policy):

- **P1/P3 PRESERVED**: the behavior is kept as is, because it is observable or is a logic bug.
- **P2 FIXED**: the behavior was undefined, memory-unsafe, a leak or a hang, and the port fixes it in the way closest to what C intended.

## Decisions per spec bug

| ID | Decision | What the Rust code does |
|---|---|---|
| UM-B1 | PRESERVED (P1) wire, FIXED (P2) buffer | `wLength` stays 4 for every family (`usb::MODE_SWITCH_TRANSFER_LEN`, `usb::wellspring_mode_read_setup`/`_write_setup`). The GET and the following SET share one zeroed 4-byte buffer (`usb::set_wellspring_mode`). TYPE4/5 no longer overrun a 2-byte allocation. Where the device answers fewer than 4 bytes, SET payload bytes 2-3 are `00`; in C they were heap garbage (fixture `umask`). `cbTransferred` is still not checked. |
| UM-B2 | PRESERVED (P3) | GET_FEATURE 0x08 writes only `08 FC` (`ptp_hid::feature::write_hqa_report_as_c`). Bytes 2..256 are untouched and Information is 257 (`hid::get_feature`). |
| UM-B3 | FIXED (P2) | `config::lookup_config_row` iterates the 15 rows and returns `None` for an unknown PID. `pnp::prepare_hardware` then sets `DeviceInfo = NULL` and returns `STATUS_INVALID_DEVICE_STATE`. It also reports `TraceEvent::DeviceNotFound` (DeviceIdentificationEvent / DeviceNotFoundInRegistry). |
| UM-B4 | PRESERVED (P3) | For TYPE3, `get_wellspring_mode` always reports on, and `set_wellspring_mode` only stores the flag. Mode 0 followed by mode 3 therefore leaves WS8 off until the next D0 cycle (fixture sequence `b4-ws8`). |
| UM-B5 | FIXED (P2) | Nothing is allocated for the mode switch: the core passes a 4-byte buffer to `io::ControlPipe`, so there is no `WdfObjectDelete(NULL)` path. The glue describes that buffer with `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER`. |
| UM-B6 | FIXED (P2) | When `RetrieveOutputMemory` or the 50-byte copy fails after a READ_REPORT was retrieved, that request is completed with the failing status and Information 0 (`input::ReadCompleteOutcome::CompletedWithError`). C leaked it, which hung removal. There are 90 fixture records (`done=l`). |
| UM-B7 | FIXED (P2) | GET_STRING copies the `wcharCount*2` UTF-16LE bytes to the start of the caller's output buffer (`hid::get_string`); C copied them into its own stack variable. The rest of the buffer is untouched. Information stays the full output buffer length and the status stays SUCCESS (P1). An unknown string ID still returns SUCCESS with Information 0. The glue must delete the string memory (`io::UsbStringSource::String` drop), which ends the leak. |
| UM-B8 | PRESERVED (P3) status, FIXED (P2) reads | SET_FEATURE always completes with `STATUS_SUCCESS`, including on retrieval failures, unsupported IDs and failed transfers (`hid::set_feature`). Report bytes 1..3 that lie past the input length read as 0 (the port policy rule "treat as 0"). So a 1-byte 0x04 report means mode 0, a 1-byte 0x06 report clears both flags, and a short 0x09 report zero-fills the missing levels. |
| UM-B9 | PRESERVED (P3) | `pnp::d0_exit` returns the status of `SetWellspringMode(FALSE)`. On surprise removal it can therefore fail D0Exit. |
| UM-B10 | PRESERVED (P3) | `pnp::d0_entry` switches the mode on only if `IsButtonReportOn \|\| IsWellspringModeOn`. A failure there is ignored and reported in `D0EntryOutcome::mode_switch_status`. |
| UM-B11 | PRESERVED (P3) | A packet with a bad length runs `usb::emergency_reset` (Set FALSE, then Set TRUE) synchronously inside the read completion, even in mouse mode and even for TYPE3 (flag only). It is not moved to a work item. |
| UM-B12 | PRESERVED (P1/P3) | `input::scan_time` is `min((qpc - PerfCounter)/100, 0xFF) as u16`, with truncating division, no lower clamp and 16-bit truncation of negative deltas. `PerfCounter` is set only in D0Entry. The subtraction wraps, giving the MSVC result instead of signed-overflow UB. |
| UM-B13 | FIXED (P2) | The input report is zero-initialized (`input::build_report` starts from `PtpReport::EMPTY`). Unused slots, padding bits and, with the surface off, ContactCount and every contact are 0. The fixtures' `out` is the run with the stack poisoned to `0x00`, so the records compare directly. |
| UM-B14 | PRESERVED (P3) | SET_FEATURE 0x06 sets bit 0 → `IsButtonReportOn` and bit 1 → `IsSurfaceReportOn`, so the HID usages are swapped. |
| UM-B15 | PRESERVED (P3) | The qualification levels are stored and echoed back (GET/SET 0x09) but never applied. They live in `DeviceState` and are not reset at PrepareHardware or D0. |
| UM-B16 | FIXED (P2) | The continuous reader is configured once. `DeviceState::reader_configured` is set after the first success, and later PrepareHardware calls still select the pipe but skip `WdfUsbTargetPipeConfigContinuousReader`. A PnP stop/start now succeeds (16 fixture records `*-prep2`); C failed with `STATUS_INVALID_DEVICE_STATE`. |
| UM-B17 | PRESERVED (P3) | MT2 Confidence = `TouchMinor*2 < 345`. The duplicated C term has no effect, so it is written once. |
| UM-B18 | PRESERVED (P3) | WS5A uses the WS5 descriptor and WS9 the WS8 one (`descriptors::HidRegistry::for_pid`). X/Y have no upper clamp. |
| UM-B19 | FIXED (N/A) | There is no counterpart to the undeclared `Driver` in WPP_CLEANUP. The glue only unregisters the TraceLogging provider. |
| UM-B20 | PRESERVED (P3) | VendorID stays 0x8910 (`descriptors::device_attributes`). The duplicate INF hardware IDs and the resource names are packaging matters, outside the core. |
| UM-B21 | PRESERVED (P3) | `pnp::prepare_hardware` takes the first interrupt pipe of interface 0 whatever its direction (fixture `prep-intout-then-intin` selects pipe 0). `tp_ep` is unused. The spec's suggested IN assertion was not added, because it would change behavior. |
| UM-B22 | FIXED (P2) | All per-device fields are atomics (`state::DeviceState`), accessed with `Relaxed` ordering. They are updated field by field as in C, so no update is lost. Mode switches are **not** serialized (see the open questions). |
| UM-B23 | PRESERVED (P3) | Lookup matches on PID only; the vendor ID is never compared (fixture `variant=vid*`). |
| UM-B24 | PRESERVED (P3) | Well-formed TYPE1 packets are dropped (`ReadCompleteOutcome::DroppedUnsupported`) and reads stay pending. TYPE1 rows have no descriptor. |

### Other P2 fixes (no spec bug ID)

| ID | What the Rust code does |
|---|---|
| UM-X1 | `DeviceInfo == NULL` is guarded everywhere it is dereferenced. This is unreachable after a successful PrepareHardware. SET 0x04 returns SUCCESS with Information 0, the read completion drops the packet (`DroppedNoConfig`), and the D0 mode switch returns `STATUS_INVALID_DEVICE_STATE`. |
| UM-X2 | A TYPE4 finger reads only its fields (+2..+20) from `h + delta + i*f`. A full 30-byte stride would run 2 bytes past the packet on the last finger. |
| UM-X3 | The C UB in the MT2 decode and in the Tip/Confidence math is resolved the MSVC way. `-(INT)(tmp_y << 6)` uses `wrapping_neg`, so the INT_MIN case gives y = -4096 and Y = 0. `x << 1` on a negative int is computed as `* 2`. |
| UM-X4 | Packed, unaligned and type-punned loads (`*(USHORT*)`, `*(UINT*)` at odd offsets, `TRACKPAD_FINGER*` casts) are replaced by explicit little-endian byte decoding. |

## Fixture coverage (`crates/amtptp-usbum-core/tests/fixtures_replay.rs`)

Every record of every file is replayed through the core with the fake framework and device of `tests/common/mod.rs`, which follows the fixture README model. Record counts are asserted per file, and so is the size of every group below.

The "fake-decided" column counts records whose final failing status was produced by the fake framework or device model and only passed through by the core. These are asserted against the fakes, so they verify the core's call order and pass-through, not the framework status itself. Examples are `0xc0000023` from `Retrieve*`, `0xc0000206` from `WdfMemoryCopyFromBuffer`, string-query failures, `xs`-scripted transfer failures, the `WdfUsbTargetDeviceCreate` failure and the `IoTargetStart` failure. The driver's own checks are not counted: GET_FEATURE/GET_STRING `0xc0000206` length checks, `0xc0000184` and `0xc00000bb`.

| File | Records | Processed | Asserting P2-fixed behavior | Fake-decided | Skipped |
|---|---|---|---|---|---|
| devices.txt | 44 | 44 | - | 0 | - |
| descriptors.txt | 898 | 898 | 35 B7 (string copied to the caller); the 24 `inj=1` records run unchanged | 372 | - |
| ioctl.txt | 24 | 24 | - | 3 | - |
| feature.txt | 1050 | 1050 | 8 B1 (`umask` payload bytes are `00`) | 20 | - |
| sequences.txt (7 sequences) | 48 | 48 | - | 1 | - |
| wire.txt | 259 | 254 | 16 B16 (restart succeeds), 16 B1 (`umask`) | 20 | 5 glue-only: `driverentry-fail`, `driverentry-ok`, `driver-cleanup`, `deviceadd-devif-fail`, `iostop` |
| input_ws1/ws2 | 340/70 | all | - | 0 | - |
| input_ws3/ws4/ws4a/ws5/ws5a | 1128/287/287/287/287 | all | B6: 14/5/5/5/5 | = B6 | - |
| input_ws6/ws6a/ws7/ws7a/t2 | 120/120/120/120/121 | all | B6: 1 each | = B6 | - |
| input_ws8 | 1118 | all | B6: 20 | = B6 | - |
| input_ws9 | 1129 | all | B6: 16, B1: 6 | 16 | - |
| input_mt2 | 1135 | all | B6: 15, B1: 8 | 15 | - |
| packets_t1..t5 | 300/1000/1000/1000/1000 | all | data only: index, `len == bytes`, and all 5400 `pkt=` references resolve | - | - |

Notes:

- Records with `undef` (B13) compare `out` directly. It is the zero-poison run, which is the zero-initialized report.
- The `deviceadd` record checks the device interface GUID constant, and `readersfailed` checks `pnp::READERS_FAILED_RESULT`.
- Requests other than the head of the READ_REPORT queue are asserted untouched.
- `calls=mc<n>,del` is not asserted. It is the C `WdfMemoryCreate` around each mode switch, which the port does not need (UM-B1/B5). All other framework calls in `calls=` are asserted, as are `qpcn` (the QPC call count and order) and `strq`.
- Statuses decided by the framework (`0xc0000023`, `0xc0000206`, scripted transfer failures, `IoTargetStart` results) come from the fakes, following the README's WDF model. The core only passes them through.

The README's excluded cases are covered by unit tests in `tests/golden.rs`:

| README exclusion | Test |
|---|---|
| 1 unknown PID (B3) | `unknown_pid_fails_prepare_hardware`, `config_lookup_is_bounded` |
| 2 short SET_FEATURE input (B8) | `short_set_feature_reads_missing_bytes_as_zero` |
| 3 GET_STRING with output = wcharCount*2 > 8 (B7) | `get_string_edge_cases` |
| 4 empty device string | `get_string_edge_cases` |
| 5 `WdfMemoryCreate` failure (B5) | not applicable (no allocation) |
| 6 TYPE4/5 short responses (B1) | fixture `umask` records + `type4_short_response_sends_zero_padding` |
| 7 `qpc - perf` overflow | `scan_time_matches_c` (wrapping) |
| 8 concurrency (B22) | not testable on the host; atomics by construction |

`tests/golden.rs` also checks the spec's golden vectors: the SHA-256 of all six report descriptors, the WS3 byte dump, the 7 MT2 and 5 TYPE2-4 contact vectors, the TYPE4 106-byte frame, the setup packets, the IOCTL routing table and the B16 restart.

## Requirements on the glue (stage 2)

- `ControlPipe`: `WdfUsbTargetDeviceSendControlTransferSynchronously(UsbDevice, NULL, NULL, &packet, &desc, &cbTransferred)`, as in C (`cbTransferred` is not checked, UM-B1). Copy `SetupPacket::to_bytes()` into `packet.Generic.Bytes`, and build `desc` with `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER` over the core's 4-byte buffer.
- `ReadQueue::Request` must complete on drop. The core always calls `complete_request` once it has retrieved a request.
- `UsbStringSource::String` deletes its WDFMEMORY on drop (UM-B7 leak).
- READ_REPORT is forwarded with `WdfRequestForwardToIoQueue`. On failure, complete with that status.
- The default queue is parallel and the InputQueue is manual with `PowerManaged = WdfFalse`. `EvtIoStop` is a no-op, and there is no `EvtDeviceReleaseHardware`. The reader uses `NumPendingReads = 0`.

## Open questions

1. **UM-B22 locking.** *Resolved in stage 2: no lock, recorded as P3 in the WDF glue section below.* C never serializes `SetWellspringMode`, which runs from SET_FEATURE (parallel queue), the emergency reset (reader completion) and D0. The core is memory-safe without a lock. A WDF wait lock around the core's mode-switching entry points would avoid interleaved read-modify-write transfers, but it changes timing. The glue author decides and records the choice.
2. **UM-B7 Information.** It is kept as the full output length, as in C. The alternative is `wcharCount*2`. HidClass does not depend on it today. The HID minidriver contract describes a NUL-terminated wide string, but the fix writes `wcharCount*2` bytes with no terminator; the rest of the buffer keeps whatever it held. If hardware shows garbage after strings, terminating the string is the fix.
3. **Read ordering.** `NumPendingReads = 0` (framework default, more than one) lets reader completions overlap and arrive out of order, as in C. `input::on_read_complete` is reentrant: each call has its own report buffer and only touches the atomics.

## WDF glue (stage 2: `drivers/um/AmtPtpDeviceUsbUm`)

The glue implements the core's `io` traits with the WDF DDIs the C driver calls, in the same order, and adds nothing the C driver does not have. It has no lower-target forwarding, no timers, work items, lookaside lists or registry reads, no `EvtDeviceReleaseHardware`, no self-managed I/O, no power policy, no file-object configuration and no I/O type setting. UMDF runs every callback at PASSIVE_LEVEL in WUDFHost, so nothing is placed in a PAGE section and IRQL is not a concern. Blocking is: see "Preserved" below.

### C function → Rust function

| C | Rust (`drivers/um/AmtPtpDeviceUsbUm/src`) |
|---|---|
| `DriverEntry`, `DriverTraceInit`, `DriverTraceCleanup` | `lib.rs::driver_entry`, `trace::register` / `trace::unregister` |
| `AmtPtpDeviceEvtDriverContextCleanup` | `lib.rs::evt_driver_context_cleanup` |
| `AmtPtpDeviceEvtDeviceAdd`, `AmtPtpCreateDevice` | `device::evt_device_add`, `device::create_device` |
| `DEVICE_CONTEXT`, `DeviceGetContext` | `device::DeviceContext` (WDF context name `"DEVICE_CONTEXT"`), `device::device_get_context` / `device::context` |
| `AmtPtpEvtDevicePrepareHardware`, `SelectInterruptInterface`, `AmtPtpConfigContReaderForInterruptEndPoint` | `device::evt_device_prepare_hardware` → `pnp::prepare_hardware` over `usb::UsbSetup` |
| `AmtPtpEvtDeviceD0Entry`, `AmtPtpEvtDeviceD0Exit` | `device::evt_device_d0_entry` / `evt_device_d0_exit` → `pnp::d0_entry` / `d0_exit` over `usb::DeviceIo` |
| `AmtPtpGet/SetWellspringMode`, `AmtPtpEmergResetDevice` (the transfers) | `usb::DeviceIo` (`ControlPipe`), driven by `amtptp_usbum_core::usb` |
| `AmtPtpDeviceQueueInitialize` | `queue::initialize` |
| `AmtPtpDeviceEvtIoDeviceControl`, `AmtPtpDispatchReadReportRequests`, `AmtPtpDeviceEvtIoStop` | `queue::evt_io_device_control`, `queue::dispatch_read_report`, `queue::evt_io_stop` |
| Hid.c handlers, `RequestGetHidXferPacketTo*` | `amtptp_usbum_core::hid::*` over `request::RequestIo` (`HidRequest`) and `usb::UsbStrings` (`UsbStringSource`) |
| `AmtPtpEvtUsbInterruptPipeReadComplete`, `AmtPtpServiceTouchInputInterrupt[Type5]` | `usb::evt_usb_interrupt_pipe_read_complete` → `input::on_read_complete` over `queue::InputQueue` (`ReadQueue`) |
| `AmtPtpEvtUsbInterruptReadersFailed` | `usb::evt_usb_interrupt_readers_failed` (returns `pnp::READERS_FAILED_RESULT`) |

### Driver-side decisions

| ID | Decision | What the glue does |
|---|---|---|
| UM-B22 | PRESERVED (P3), with the field races FIXED (P2) by the core's atomics | **No lock** around the mode switches. This answers open question 1. The P2 part of B22 is the data races on context fields: the core's atomics fix them, the WDF handles in the context are `AtomicPtr` (Release/Acquire), and each mode switch uses its own stack buffer, so there is no memory UB. SET_FEATURE, the emergency reset and D0 can still interleave their GET/SET transfers on the device, as in C. That interleaving is a logic race, so P3 applies. A lock would also risk deadlock: D0Exit's `WdfIoTargetStop(CancelSentIo)` waits for a running reader completion, which may be inside the emergency reset. It would also make read completions wait behind SET_FEATURE transfers. |
| UM-B7 (glue) | FIXED (P2) | The `WdfUsbTargetDeviceAllocAndQueryString` memory is deleted when the handler is done (`usb::StringMemory` drop). Only `min(wcharCount*2, buffer length)` bytes are exposed. A successful query that returns no memory object (an empty string, fixture README exclusion 4) gives an empty string: no `WdfMemoryGetBuffer` and no delete. C called `WdfMemoryCopyToBuffer(NULL, …)`. |
| UM-B16 (glue) | FIXED (P2) | The reader is configured once **per pipe object**. The context remembers the pipe the reader was configured on (`reader_pipe`). If a later PrepareHardware selects a different pipe object, the glue clears `DeviceState::reader_configured` so the core configures the new pipe. A restart that keeps the pipe (the expected case, because SelectConfig is never called) skips the configuration, as in the core. |
| UM-B1/B5 (glue) | wire PRESERVED (P1), buffer FIXED (P2) | Each transfer is `WdfUsbTargetDeviceSendControlTransferSynchronously(UsbDevice, NULL, NULL, &packet, &descriptor, &cbTransferred)`. The descriptor is a `WDF_MEMORY_DESCRIPTOR_INIT_BUFFER` over the core's 4-byte stack buffer; there is no `WdfMemoryCreate(PagedPool, 'PTPC')` and no `WdfObjectDelete`. The packet is `SetupPacket::to_bytes()` in `WDF_USB_CONTROL_SETUP_PACKET.Generic.Bytes`. C builds the same `bmRequestType`/`bRequest`/`wValue`/`wIndex` with `WDF_USB_CONTROL_SETUP_PACKET_INIT` + `Type = BmRequestClass` and leaves `wLength` 0. UMDF ignores the caller's `wLength` and sends the descriptor length (Windows-Driver-Frameworks `fxusbdeviceum.cpp`, `StoreAndReferenceMemory`: `SetupPacket.Length = TransferBufferLength`), so the wire bytes are identical. |
| UM-G1 | FIXED (P2) | NULL-handle guards. Each one is unreachable after a successful start, except GET_STRING before the first PrepareHardware. A NULL `UsbDevice` in GET_STRING or in a control transfer gives `STATUS_INVALID_DEVICE_STATE`. A NULL `InterruptPipe` in D0Entry gives `STATUS_INVALID_DEVICE_STATE`, and D0Exit then skips the stop. A NULL `InputQueue` completes READ_REPORT with `STATUS_INVALID_DEVICE_STATE`, and the reader finds no request. A missing context also gives `STATUS_INVALID_DEVICE_STATE`. In every one of these cases C would pass NULL to WDF, which crashes the host or breaks into the verifier. |
| UM-G2 | FIXED (P2) | If `WdfUsbInterfaceGetConfiguredPipe` returns NULL, that pipe is skipped and treated as an invalid type. C would call `WdfUsbTargetPipeSetNoMaximumPacketSizeCheck(NULL)`. |
| UM-G3 | FIXED (P2) | The reader packet is the first `min(NumBytesTransferred, WdfMemoryGetBuffer length)` bytes of the reader memory. Rust only builds a slice from a non-NULL pointer with a non-zero length (`request::slice_or_empty`). |
| UM-G4 | not observable | The context keeps the C WDF name `"DEVICE_CONTEXT"` for `!wdfkd`. Its layout and size differ from the 96-byte C struct: it holds the core's `DeviceState`, the handles and `reader_pipe`. |
| UM-G5 | PRESERVED (P1) | `WdfRequestRetrieveInputMemory` / `WdfRequestRetrieveOutputMemory` are called at most once per request, and their handles are cached (`request::RequestIo`). Every C path therefore makes the same retrieve calls. Framework statuses come from WDF itself: `WdfMemoryCopyFromBuffer` for descriptors and READ_REPORT, `WdfRequestRetrieveOutputBuffer` for attributes and strings. The glue never checks lengths in their place. The only extra calls are `WdfMemoryGetBuffer`, which has no side effects. |
| UM-G6 | PRESERVED (P1) | `WdfRequestSetInformation` is called only when the handler set a non-zero Information, then `WdfRequestComplete`. READ_REPORT completions call `SetInformation(50)` + `Complete(SUCCESS)`; the UM-B6 error path calls `SetInformation(0)` + `Complete(status)`. Every request goes through `wdf_support::request::Request`, so it is completed or forwarded exactly once. |
| UM-G7 | tracing | WPP is not ported. The three C TraceLogging events are exact: names, field names, order and types (`TraceLoggingString` → `cstr8_cp1252`), level 5 and keyword 0. The WPP error and state points become new events on the same provider: `DevicePnpEvent`, `DeviceUsbOperationEvent`, `DeviceStateEvent`, `DeviceIoctlEvent` and `InputDiagnosticsEvent`, all with keyword 0. The readers-failed callback now traces its status; C was silent. |
| UM-G8 | FIXED (build) | The UM workspace uses the default panic strategy (unwind), not abort. With panic=abort, wdk-sys's stub C++ EH handlers (returning ExceptionContinueExecution) displaced vcruntime's real `__CxxFrameHandler3` for DriverEntry and the static UCRT exit/onexit/ptd functions, so an SEH exception escaping a callee would loop instead of reaching the host's crash path as in C. Panics still abort in the `wdk_panic` hook before any unwinding. |

### Preserved C behavior in the glue (P1/P3)

- The order of `EvtDeviceAdd` calls is the same as in C:
  1. `WdfFdoInitSetFilter`
  2. `WdfDeviceInitSetPnpPowerEventCallbacks`, with only PrepareHardware, D0Entry and D0Exit
  3. `WdfDeviceCreate`
  4. `SurpriseRemovalOK = WdfTrue`
  5. `WdfDeviceCreateDeviceInterface(GUID, NULL)`
  6. Only if step 5 succeeds, the queues.
- The queues match C. The default queue is parallel, `PowerManaged = WdfUseDefault` (not power-managed, because the driver is a filter), and has `EvtIoDeviceControl` + `EvtIoStop`. The `InputQueue` is manual with `PowerManaged = WdfFalse`. Both use `WDF_NO_OBJECT_ATTRIBUTES`.
- `EvtIoStop` is a no-op. Unsupported IOCTLs complete with `STATUS_NOT_SUPPORTED` and are not forwarded. READ_REPORT is forwarded to `InputQueue` without a size check, and a failed forward completes the request with the forward status.
- The continuous reader matches C: `NumPendingReads = 0`, no header or trailer, no buffer attributes, completion context = the device context, and readers-failed returns TRUE.
- **Synchronous control transfers without a timeout** still run in the SET_FEATURE dispatch, in D0Entry and D0Exit, and inside the continuous-reader completion (the emergency reset, UM-B11). SET_FEATURE can also do control I/O while the device is in Dx, because the default queue is not power-managed. In UMDF these are timing risks, not IRQL risks: a hung device blocks that host thread. They are kept as in C.
- `QueryPerformanceCounter` comes from kernel32, and its BOOL result is ignored, as in C.

### To verify on hardware

1. **UM-B16 pipe identity.** The glue assumes a PnP stop/start returns the same `WDFUSBPIPE`. If it does not, the reader is configured on the new pipe. `DevicePnpEvent` from PrepareHardware traces the pipe handle, the endpoint address and whether the reader was configured.
2. **UM-B7 GET_STRING Information.** Information is still the full output length and no NUL terminator is written (open question 2).
3. **Statuses decided by the framework.** These come from the real framework now. On a live UMDF host, check them against the fixtures' modelled `0xc0000206` / `0xc0000023` (see the fixture README).

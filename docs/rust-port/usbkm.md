# AmtPtpDeviceUsbKm: Rust port record

The C driver is `src/AmtPtpDeviceUsbKm` at commit 2edb0f2. The Rust port has two parts:

- `crates/amtptp-usbkm-core` (GPL-2.0-only, `no_std`, depends only on `ptp-hid`) holds all the logic that does not
  need WDF. Its crate-level rustdoc ("How the driver uses this") is the contract for the glue.
- `drivers/km/AmtPtpDeviceUsbKm` is the KMDF 1.23 glue (stage 2). Entries below marked *glue* are decisions the
  glue has to implement; the core provides the constants or functions for them.

The porting spec is [`specs/usbkm.md`](specs/usbkm.md), including its verification addendum. Bug IDs `KM-Bn` are the spec's `Bn` (§10.1).
The policy terms come from the [behavior policy](README.md#behavior-policy):

- **PRESERVED (P1/P3)**: the behavior is kept as is, because it is observable or is a logic bug.
- **FIXED (P2)**: the behavior was undefined, memory-unsafe, a leak or a hang. The port fixes it in the way closest
  to what the C intended.

## Decisions per spec bug

| ID | Decision | What the Rust code does |
|---|---|---|
| KM-B1 | PRESERVED (P1/P3) | `input::scan_time_as_c(perf_out, LastReportTime)` = `trunc((a-b)/100)`. The result is clamped above at 0xFF, is not clamped below, and is cast to `u16`. D0Entry stores and the read path passes the value `KeQueryPerformanceCounter` writes to its **out-parameter** (the frequency, via `UsbHardware::query_performance_counter_out`), so ScanTime is always 0. The subtraction wraps (MSVC result instead of signed-overflow UB). |
| KM-B2 | PRESERVED (P3) | GET_FEATURE 0x08 writes only `[0]=08 [1]=FC` (`ptp_hid::feature::write_hqa_report_as_c`). Bytes 2..256 are untouched. |
| KM-B3 | PRESERVED (P1) wire, FIXED (P2) buffer | Both setup packets keep `wLength = 4` (`wellspring::WELLSPRING_DATA_LEN`). The SET_REPORT data stage is `[buf0, mode, buf2, buf3]`. The buffer is a full 4 bytes, zeroed by `set_wellspring_mode`, so there is no pool overrun on the read and no over-read on the write. Bytes the device does not return are `00`. |
| KM-B4 | FIXED (P2) | `ControlTransfer::allocate_transfer_buffer` returns `Err(status)` and `set_wellspring_mode` returns that status. There is no `WdfObjectDelete(NULL)`: the buffer type's `Drop` deletes only a real allocation. |
| KM-B5 | PRESERVED (P3) | The GET_REPORT byte count is not checked. |
| KM-B6 | FIXED (P2) | When `WdfRequestRetrieveOutputMemory` fails for a dequeued READ_REPORT, `input::complete_read_report(.., Err(status))` completes it with that status and Information 0. In C it leaked, non-cancelable, and removal hung. This failure takes precedence over DATA_ERROR, as in the C order. |
| KM-B7 | FIXED (P2) | When the output buffer is shorter than 50 bytes, the request is completed with `STATUS_BUFFER_TOO_SMALL` (the `WdfMemoryCopyFromBuffer` status) and nothing is written. In C it leaked. DATA_ERROR takes precedence, as in the C order. |
| KM-B8 | PRESERVED (P1) | More than 5 finger blocks give `ReadReportResult::DataError`, which completes with `STATUS_DATA_ERROR` and Information 0. The check sits inside the `PtpReportTouch` gate. |
| KM-B9 | FIXED (P3) | `lifecycle::configure_continuous_reader` returns the `WdfUsbTargetPipeConfigContinuousReader` status, and PrepareHardware fails with it. |
| KM-B10 | FIXED (P3) | `lifecycle::d0_exit` switches wellspring mode off best effort and always returns `STATUS_SUCCESS`; the OFF status is only traced (`D0ExitOutcome::mode_switch_status`). C returned it, so a surprise removal (where the transfer always fails) or a failed transfer at sleep failed the power-down. |
| KM-B11 | PRESERVED (P1) status, FIXED (P3) condition | `lifecycle::d0_entry` ignores the wellspring-ON status. The switch runs before the reader starts, now unconditionally: C's condition `PtpReportButton \|\| IsWellspringModeOn` was always true while SET_FEATURE 0x06 was ignored, and would otherwise leave the device in mouse mode after a resume with the button switch off. |
| KM-B12 | FIXED (P2) *glue* | `WdfPdoInitAllowForwardingRequestToParent` (a PDO-only API, rejected by KMDF on a filter `DeviceInit`, with a verifier break) is not called. It has no other effect. The wire test asserts the DeviceAdd call list without it. |
| KM-B13 | PRESERVED (P1) | GET/SET_FEATURE complete with Information 0 (`feature::get_feature` / `set_feature` return `Completion::status`). |
| KM-B14 | PRESERVED (P3) values, FIXED (P2) read | Mode values other than 0 and 3 succeed with no action. `feature::input_mode` reads byte 1 only when it lies inside `reportBufferLen`; otherwise it uses 0, giving `STATUS_NOT_SUPPORTED` and no transfer. C read past the report (fixture tag `ub_oob_read`). |
| KM-B15 | FIXED (P3) | SET_FEATURE 0x06 sets `PtpReportTouch` from bit 0 (Surface Switch) and `PtpReportButton` from bit 1 (Button Switch) of byte 1 (`feature::selective_report_bits`); the reader completion honours them. A report without byte 1 changes nothing. |
| KM-B16 | FIXED (P2) *glue* | A NULL `reportBuffer` is passed to the core as an empty slice. GET 0x07/0x08 then returns `STATUS_INVALID_BUFFER_SIZE`, which is the C result whenever C does not crash. SET 0x04 treats it as mode 0. |
| KM-B17 | FIXED (Step 2) | `get_device_attributes` writes the complete 32-byte structure with zeroed reserved fields. |
| KM-B18 | PRESERVED (P1) | ContactID = slot index (`u32::from(i as u8)`). |
| KM-B19 | PRESERVED (P1) | X/Y are clamped below at 0 only; Y is inverted (`y.max - abs_y`). |
| KM-B20 | FIXED (P2) stale handle, FIXED (P3) selection | `lifecycle::select_interrupt_interface` resets `InterruptPipe` before the selection and takes the first interrupt **IN** pipe. A restart that finds none fails with `STATUS_INVALID_DEVICE_STATE`; C kept the deleted pipe handle, a use-after-free, and took the first interrupt pipe whatever its direction. |
| KM-B21 | PRESERVED (P1) | The descriptor bytes are reproduced exactly (`hid::REPORT_DESCRIPTOR` = `ptp_hid::descriptor::report_descriptor_u32_id(T2_AXES)`): physical maxima 1300/850, COLLECTION_2 without the unit reset, and Contact ID Logical Max `27 FF FF FF FF`. |
| KM-B22 | PRESERVED (P3) *glue* | `FILETYPE VFT_APP` in the version resource is a driver-crate resource matter, not core logic. |
| KM-B23 | PRESERVED (P1) *packaging* | Duplicate INF hardware IDs are an INF matter (`packaging/AmtPtpDevice.inx`). |
| KM-B24 | PRESERVED (P1) *glue* | `Irp->UserBuffer` is checked only for NULL (`feature::*` closure returns `None`). The glue builds the report slice from `(reportBuffer, reportBufferLen)` and trusts hidclass. |
| KM-B25 | PRESERVED (P3) | READ_REPORT is forwarded to the InputQueue without a length check (`hid::InternalIoctl::ReadReport`). A short buffer fails at dequeue: with B6/B7 fixed, it is completed with `STATUS_BUFFER_TOO_SMALL` instead of leaking. |
| KM-B26 | N/A | WPP is replaced by TraceLogging, so the format mismatch no longer exists. |
| KM-B27 | FIXED (P2) *glue* | The transfer buffer comes from non-paged memory (`wellspring::TRANSFER_BUFFER_POOL_TYPE` = NonPagedPoolNx, tag `POOL_TAG_PTP_CONTROL` = `'PTPC'`), because it is a URB transfer buffer. |
| KM-B28 | PRESERVED (P1) | `select_interrupt_interface` calls `SetNoMaximumPacketSizeCheck` on every visited pipe. `prepare_hardware` always runs it before `configure_continuous_reader`. |

### Other P2 fixes (no spec bug ID)

| ID | What the Rust code does |
|---|---|
| KM-X1 | Data races on plain context fields (reader completion at DISPATCH vs. I/O vs. PnP) are removed. Every field of `state::DeviceState` is an atomic with `Relaxed` ordering, and each is written field by field as in C. |
| KM-X2 | `Bcm5974ConfigTable` has no sentinel. `config::config_index_for_pid` is a bounds-checked scan with fallback row 0, giving the same results as C. |
| KM-X3 | `touch_major << 1` / `touch_minor << 1` on negative ints (UB) are computed as `wrapping_mul(2)`, which is the MSVC result. Packed or unaligned `TRACKPAD_FINGER` loads are replaced by explicit LE decoding of the needed fields only. |
| KM-X4 | Unreachable NULL dereferences are guarded. Before the first PrepareHardware, `DeviceState::config()` returns the fallback row (C: `DeviceInfo == NULL`). `d0_entry` without an interrupt pipe returns `STATUS_INVALID_DEVICE_STATE`, and `d0_exit` skips the stop (C: `WdfUsbTargetPipeGetIoTarget(NULL)`). |

### Dropped dead code (spec §10.2)

These are dropped: `PtpInputOn`, `QUEUE_CONTEXT`, `DbgIoControlGetString`, the identical per-PID switch branches, the
`wReportLength == 0` and `DeviceInfo == NULL` checks, `isTargetStarted`, and the TYPE1/2/3/5 parsing.

These are kept: the TYPE3 shortcut in `set_wellspring_mode` and the `STATUS_UNKNOWN_REVISION` path. Both are cheap
and unreachable. `UsbDeviceTraits` is stored for tracing.

## Fixture coverage

Each fixture file has one test (`tests/descriptors.rs`, `ioctl.rs`, `feature.rs`, `input.rs`, `wire.rs`). Each test
puts every record into exactly one named bucket and asserts both the per-file total and every bucket count. The
fake WDF USB side is `tests/common/fake.rs`, which follows the fixture README's harness model. `tests/golden.rs`
holds the spec's golden vectors (§7.4 A/B/B′/C/D, §6.3 wire bytes, §5.3/5.5/5.6, Appendices A/B), typed in from the
spec.

| File | Records | Processed | Of which P2-fixed behavior is asserted | Skipped |
|---|---|---|---|---|
| descriptors.txt | 173 | 172 | (18 use the modeled framework status for zero-length / <32-byte output retrieval) | 1 |
| ioctl.txt | 61 | 61 | - | 0 |
| feature.txt | 140 | 140 | 4 `ub_oob_read` (2 differ from C), 2 `c_bugcheck_B4` (no `obj_delete:null`) | 0 |
| input_027a.txt | 1134 | 1134 | 2 `c_leak_B6`, 2 `c_leak_B7` (completed `0xc0000023`) | 0 |
| input_027b.txt | 1134 | 1134 | same | 0 |
| input_027c.txt | 1134 | 1134 | same | 0 |
| input_027d.txt | 1134 | 1134 | same | 0 |
| input_ffff.txt | 1145 | 1145 | same | 0 |
| wire.txt | 240 | 240 | 2 `c_bugcheck_B4` steps, 1 `c_stale_handle_B20` step (`0xc0000184`, pipe -1, no reader config) | 0 |

Details:

- **descriptors.txt**: the only skipped record is `kind=layout device_context=80 ...` (reason
  `c_memory_layout`). It holds the field offsets of the C `DEVICE_CONTEXT`, and the Rust context is a different type
  that nothing outside the driver sees. The other `layout` record is asserted key by key, so an unknown key fails.
  The sentinel `config_row idx=5` is asserted as all-zero, at index `CONFIG_TABLE.len()`. `consts.paged_pool=1`
  documents the C PagedPool. The port asserts NonPagedPoolNx (B27).
- **input_*.txt**:
  - The `kind=defaults` line is consumed as the defaults of the 1000 random records (bucket `defaults_line`).
  - The 22 `unreachable_state` records (gates off, `LastReportTime != frequency`) are **asserted**, not skipped. The
    core keeps the gates and the ScanTime arithmetic as inputs.
  - Field exception, bucket `driver/no_request` (2 per file): there is no request, so `out` is not a driver
    result. The test asserts that it is a buffer the driver never touched: either the previous case's `out`
    (`req-none`, a 3-contact report for a 2-finger packet) or the untouched prefill (`req-none-6fingers`). `res`,
    `completed` and the queue depths are compared.
  - The `ops` of the 27 records per file that carry it are not compared. That is the WDF call order of the reader
    callback, which the glue owns: it is documented in `input`'s module docs, and the order-relevant outcomes are
    asserted via `res`.
- **wire.txt**:
  - PrepareHardware (37 + 1 B20 + 2 P3: `pipes-int-out-first` takes the interrupt IN pipe 1, and
    `fail-cont-reader-masked` returns the reader failure), D0Entry (45 + 1 B4) and D0Exit (24 + 1 B4 + 2 B10: the OFF transfer failed, the port returns `STATUS_SUCCESS` and
    reports the C return value as `mode_switch_status`) run through
    `lifecycle::*`. Each
    asserts the status, `ctx_out`, the wire bytes and the normalized WDF call list (`fake::normalize_ops`). That
    list includes `mem_create`, `ctl_xfer` setup/dir/mdlen and `obj_delete`.
  - The 42 `driver_entry` and 41 `device_add` steps are WDF configuration the glue performs. For these the test
    asserts the injected framework status, the core constants (`DEFAULT_QUEUE`, `INPUT_QUEUE`,
    `DEVICE_INTERFACE_GUID`) against the `queuecreate`/`devif` ops, the call order without the B12 call, and the
    initial context.
  - The 7 cleanup steps assert "no WDF call, no context change".
  - The 37 `harness_*` / `note` lines are directives for the fake and are consumed (buckets `harness_directive`,
    `harness_note`).
  - `ctx_out.iface` and `inputq` are glue-side handles, tracked by the fake. `ptpinputon` is always 0 (the field is
    dropped).
- **feature.txt**:
  - The input gates are compared with the P3 rule: a successful SET 0x06 with byte 1 sets `touch` from bit 0 and
    `button` from bit 1 (C never changed them).
  - Each `ub_oob_read` record is asserted with its P2 result (`0xc00000bb`, no transfer). It is also replayed with
    `reportBufferLen` = 2; that replay must match the C record exactly, which shows the only difference is the
    out-of-report byte.
  - The `c_pool_overrun_B3` records are asserted as written. The harness allocation already is the P2 model:
    4 zeroed bytes.

## Open questions for the glue (stage 2)

These were all resolved in stage 2; see "Driver (stage 2)" below. The reader order and the DeviceAdd sequence are
as listed, `GET_DEVICE_ATTRIBUTES` reads `usb_device_ids()`, the concurrency is preserved, and the masked reader
status is traced in `UsbIo::configure_continuous_reader`.

- The order inside the reader completion is: `process_packet` (pure), then `WdfIoQueueRetrieveNextRequest`, then
  output memory, then `complete_read_report`. Skipping the dequeue on `PacketAction::Discard` is required: a
  bad-length packet must leave the pended request queued.
- The core cannot express the DeviceAdd sequence (WDF owns `DeviceInit` and the context memory). The glue must
  follow the crate docs: `WdfFdoInitSetFilter`, PnP callbacks (prepare, D0Entry, D0Exit only), `WdfDeviceCreate`,
  `ptr.write(DeviceState::new())` plus `on_device_create()`, device interface, default queue, then InputQueue,
  returning the first failure.
- `GET_DEVICE_ATTRIBUTES` before PrepareHardware must report VID/PID 0 (fixture `attr-before-prepare`). The glue
  must read `DeviceState::usb_device_ids()` (zero-initialized), not an `Option` that fails.
- SET_FEATURE input mode 3 and D0Entry/D0Exit run `set_wellspring_mode` inside `ControlTransfer::serialized`, which
  the glue implements with a `WDFWAITLOCK` (`DeviceContext::mode_switch_lock`, not in C). The check of the mode flag
  and the transfer pair are one step, so two switches never interleave. D0Exit stops the interrupt pipe before it
  takes the lock.

## Driver (stage 2): `drivers/km/AmtPtpDeviceUsbKm`

The KMDF 1.23 glue. It makes the WDF calls the C code makes, in the C order, and leaves every decision to the core.

| Module | C function | Rust |
|---|---|---|
| `lib.rs` | `DriverEntry`, `AmtPtpDeviceUsbKmEvtDriverContextCleanup` | `driver_entry` (`INIT`), `evt_driver_context_cleanup` (`PAGE`) |
| `device.rs` | `AmtPtpDeviceUsbKmEvtDeviceAdd`, `AmtPtpDeviceUsbKmCreateDevice` | `evt_device_add`, `create_device` (both `PAGE`) |
| `device.rs` | `AmtPtpDeviceUsbKmEvtDevicePrepareHardware` | `evt_device_prepare_hardware` (`PAGE`) → `lifecycle::prepare_hardware` |
| `device.rs` | `AmtPtpEvtDeviceD0Entry`, `AmtPtpEvtDeviceD0Exit` | `evt_device_d0_entry`, `evt_device_d0_exit` (non-paged) → `lifecycle::d0_entry` / `d0_exit` |
| `usb.rs` | `SelectInterruptInterface`, `AmtPtpConfigContReaderForInterruptEndPoint`, `AmtPtpSetWellspringMode` (WDF calls) | `UsbIo` (`UsbHardware` + `ControlTransfer`), `TransferBuffer` |
| `queue.rs` | `AmtPtpDeviceUsbKmQueueInitialize` | `queue_initialize` (`PAGE`), configs built from `lifecycle::DEFAULT_QUEUE` / `INPUT_QUEUE` |
| `queue.rs` | `AmtPtpDeviceUsbKmEvtIoDeviceControl` + `AmtPtpGetHidDescriptor`, `AmtPtpGetDeviceAttribs`, `AmtPtpGetReportDescriptor`, `AmtPtpDispatchReadReportRequests`, `AmtPtpReportFeatures`, `AmtPtpSetFeatures` | `evt_io_internal_device_control` → `hid::route_internal_ioctl` + `hid::*` / `feature::*` |
| `queue.rs` | `AmtPtpDeviceUsbKmEvtIoStop` | `evt_io_stop` (registered, trace only) |
| `interrupt.rs` | `AmtPtpEvtUsbInterruptPipeReadComplete`, `AmtPtpEvtUsbInterruptReadersFailed` | `evt_usb_interrupt_pipe_read_complete`, `evt_usb_interrupt_readers_failed` |
| `context.rs` | `DEVICE_CONTEXT` / `DeviceGetContext` | `DeviceContext` (WDF context name `"DEVICE_CONTEXT"`) / `DeviceContext::of` |
| `trace.rs` | WPP (`Trace.h`), `DbgDevicePowerString` | TraceLogging provider `AmtPtpDeviceUsbKmTraceProvider` {b409cf52-f41a-4193-98c2-bad95204203b}, keywords = the WPP flag bits, `power_state_name` |

### WDF call fidelity

- **Completion.** `queue::complete` calls `WdfRequestSetInformation` only on the paths where the C calls it: 9, 493,
  32 and 50 bytes, which are exactly the non-zero `Completion::information` values. It then calls
  `WdfRequestComplete(status)`. Every other path keeps the Information the IRP arrived with, as in C. The driver does
  not use `WdfRequestCompleteWithInformation(status, 0)`, which would overwrite that value.
- **Framework copies.** The descriptors and the READ_REPORT go through `WdfRequestRetrieveOutputMemory` and
  `WdfMemoryCopyFromBuffer`, so the copy status is the framework's own. The attributes go through
  `WdfRequestRetrieveOutputBuffer(Request, 32, ..)`. The core decides the precedence (`complete_read_report_to`);
  the WDFMEMORY wrapper `queue::RequestOutputMemory` does the copy.
- **Wellspring transfers.** The setup packet handed to WDF is the one the C builds
  (`WDF_USB_CONTROL_SETUP_PACKET_INIT` plus `Type = Class`). It is the core's `UsbSetupPacket::to_bytes` with
  wLength left 0 (fixture `setup_raw`), placed in `WDF_USB_CONTROL_SETUP_PACKET.Generic.Bytes`.
  - It is sent with `WdfUsbTargetDeviceSendControlTransferSynchronously(UsbDevice, WDF_NO_HANDLE, NULL, ..)` over
    a 4-byte buffer memory descriptor.
  - The framework derives wLength from that descriptor: the fixture `setup` gives wLength 4 on the wire. Whichever
    layer fills wLength in, the result is the C's.
- **Pipe handles.** The `WDFUSBPIPE` handles are the ones `WdfUsbInterfaceGetConfiguredPipe` returned during the
  selection, with no extra lookups. The core hook `UsbHardware::interrupt_pipe_selected` stores the handle exactly
  where the C assigns `InterruptPipe` and resets it at every reselection (B20). `WdfUsbTargetPipeGetIoTarget` is a
  `FORCEINLINE` cast in `wdfusb.h`, and is reproduced as one (`usb::pipe_io_target`).
- **Reader.** `KeQueryPerformanceCounter(&x)` is called before the length gate rather than after the dequeue.
  Only the out-parameter (the frequency) is used. That has no observable effect, and ScanTime stays 0 (B1).
  `WdfMemoryGetBuffer` is called before the gate. Its size bounds the packet slice, so a `NumBytesTransferred`
  beyond the buffer is dropped as malformed; that is impossible, because N ≤ TransferLength.
- **Code placement** mirrors `#pragma alloc_text`: `DriverEntry` in `INIT`, and device add, driver cleanup, queue
  init and PrepareHardware in `PAGE`. This was checked in the release map file. D0Entry, D0Exit, the dispatcher,
  EvtIoStop and both reader callbacks are in `.text`.

### Driver-side decisions

| ID | Decision | What the driver does |
|---|---|---|
| KM-B12 | FIXED (P2) | `WdfPdoInitAllowForwardingRequestToParent` is not called. `evt_device_add` calls only `WdfFdoInitSetFilter` and then CreateDevice. |
| KM-B16 / KM-B24 | FIXED (P2) / PRESERVED | `queue::xfer_packet` reads `Irp->UserBuffer` only after the core's 16-byte check, and NULL becomes `None`. The report slice is `(reportBuffer, reportBufferLen)`, and a NULL `reportBuffer` becomes a literal empty slice, never `from_raw_parts(NULL, ..)`. hidclass is trusted for the length. |
| KM-B27 / KM-B3 / KM-B4 | FIXED (P2) | `UsbIo::allocate_transfer_buffer` calls `WdfMemoryCreate(WDF_NO_OBJECT_ATTRIBUTES, NonPagedPoolNx, 'PTPC', 4, ..)`. `TransferBuffer::drop` calls `WdfObjectDelete` once, and only for a real allocation. |
| KM-B9 | FIXED (P3) | `UsbIo::configure_continuous_reader` traces a failed `WdfUsbTargetPipeConfigContinuousReader` (`WdfUsbTargetPipeConfigContinuousReaderFailed`) and returns the status; the core returns it from PrepareHardware. |
| KM-B6 / KM-B7 | FIXED (P2) | The reader completes a dequeued READ_REPORT on every path: the retrieval status, `STATUS_DATA_ERROR`, or the `WdfMemoryCopyFromBuffer` status. Each failure is traced. The `Request` guard's drop fallback (`STATUS_UNSUCCESSFUL`) is unreachable. |
| KM-X5 | FIXED (P2) | `UsbIo::control_transfer` returns `STATUS_INVALID_DEVICE_STATE` when no `WDFUSBDEVICE` exists yet. `queue::retrieve_output_memory` maps a (framework-impossible) success with a NULL WDFMEMORY to `STATUS_INVALID_DEVICE_STATE`, so a READ_REPORT is never completed as success without data. That only happens for a SET_FEATURE input mode 3 before the first PrepareHardware, which hidclass never sends. The C would send on a NULL handle, a bugcheck. The other unreachable NULLs are guarded the same way: a missing context, a missing InputQueue in the dispatcher (`STATUS_INVALID_DEVICE_STATE`) or in the reader (packet dropped), and a missing pipe handle (start fails with `STATUS_INVALID_DEVICE_STATE`, stop is skipped). |
| KM-X6 | FIXED (P2) | The WDF handles in the context (`UsbDevice`, `InterruptPipe`, `UsbInterface`, `InputQueue`) are `AtomicPtr` (Relaxed), like the core state (X1). The context is written once with `ptr.write` right after `WdfDeviceCreate`, and only shared references exist after that. |
| KM-X7 | FIXED (P2) | NULL IRP for a feature request (defensive, unreachable). C dereferences `WdfRequestWdmGetIrp(Request)->UserBuffer` without a check (Hid.c:319, Hid.c:455). `wdf_support::wdm::request_user_buffer` returns NULL when the request has no IRP, so `queue::xfer_packet` yields `None` and GET/SET_FEATURE complete with `STATUS_INVALID_DEVICE_REQUEST` after the same 16-byte length check, exactly like a NULL `UserBuffer`. A request presented through `EvtIoInternalDeviceControl` always has an IRP, so this cannot happen in practice. |
| EvtIoStop | PRESERVED | Registered on the default queue (the fixtures check that the callback exists). It only traces: no `WdfRequestStopAcknowledge`, no completion. |
| Device interface | PRESERVED | `WdfDeviceCreateDeviceInterface(device, {4aa332cc-5777-4afd-aa4e-95387330612a}, NULL)` after the context init, before the queues. |

### Preserved risks (documented, not changed)

- **SET_FEATURE runs synchronous USB transfers in the I/O callback.** The default queue has no execution-level
  constraint, so input mode 3 switches wellspring mode with two synchronous control transfers from
  `EvtIoInternalDeviceControl`. As in C, this relies on hidclass sending SET_FEATURE at PASSIVE_LEVEL.
  - Above PASSIVE_LEVEL, the open-source KMDF checks the IRQL at the entry of the synchronous send
    (`FxVerifierCheckIrqlLevel`) only when KMDF verifier is on (`FxVerifierOn`). The call then fails with
    `STATUS_INVALID_DEVICE_REQUEST`, and that status is returned to hidclass. Without the verifier the check
    returns success, the request is sent, and the framework blocks in its synchronous wait
    (`FxIoTarget::SubmitSync`). That wait is illegal at DISPATCH_LEVEL and is expected to bugcheck; at
    APC_LEVEL it works. The C has no guard either: it fails even earlier at DISPATCH, in its PagedPool
    `WdfMemoryCreate` and `RtlZeroMemory`. This is expected from the framework sources only: the MS Learn page
    of this DDI documents only "IRQL PASSIVE_LEVEL". It has not been observed.
  - No IRQL guard is added. A `KeGetCurrentIrql() > PASSIVE_LEVEL` check would also fail APC_LEVEL calls,
    which work in the C (P1), and hidclass never sends SET_FEATURE above PASSIVE_LEVEL.
  - Every allocation on this path is non-paged, and the code is in `.text`.
- **Control transfers have a timeout.** `UsbIo::control_transfer` sends with `wdf_support::init::SYNC_IO_TIMEOUT` (5 s, the USB 2.0 bound for
  a control request with a data stage; C: no options). A trackpad that never answers fails the switch instead of
  blocking D0Entry, D0Exit or the SET_FEATURE dispatch forever.
- **Imports.** `KeQueryPerformanceCounter` is imported from `HAL.dll`. In WDK 10.0.28000 only `hal.lib` exports
  it (`ntoskrnl.lib` does not), so a WDK-built C driver imports it from `HAL.dll` as well.

### Core API added for the glue

These additions are backward compatible: the slice-based functions and the existing tests are unchanged.

- `output::OutputMemory`: the request output buffer as written by `WdfMemoryCopyFromBuffer`. The `[u8]`
  implementation is the documented model, `STATUS_BUFFER_TOO_SMALL` with nothing written.
- `hid::get_hid_descriptor_to`, `hid::get_report_descriptor_to`, `input::complete_read_report_to`: generic over
  `OutputMemory`. The old slice functions now delegate to them, and the success status is the copy status, as in
  C.
- `lifecycle::UsbHardware::interrupt_pipe_selected(Option<u8>)`: a default no-op hook, called with `None` at the
  start of every selection and with `Some(index)` where the C assigns `InterruptPipe`. It is pinned by a unit test.

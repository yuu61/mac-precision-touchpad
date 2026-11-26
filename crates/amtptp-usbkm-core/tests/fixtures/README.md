# AmtPtpDeviceUsbKm oracle fixtures

These files are ground truth for `amtptp-usbkm-core` tests. Each record was produced by **executing the original C
driver code** (`src/AmtPtpDeviceUsbKm`) on the host against a recording fake of KMDF 1.15. No value was derived
by reading the C code. Everything that is not C behavior (fake-framework statuses, the device's control-transfer
responses, the QPC values) is listed under "Harness model" below.

## Provenance

- C sources: `src/AmtPtpDeviceUsbKm/*.c` and `include/**` at commit `2edb0f2` (unmodified working tree).
- **All six translation units were compiled unmodified**: `DebugUtils.c Device.c Driver.c Hid.c Interrupt.c
  Queue.c`. No function was copied or edited. Each `.c` was compiled from its original path, and the project
  headers (`Driver.h`, `Device.h`, `include/Hid.h`, `include/AppleDefinition.h`, ...) were included as-is. Struct
  layouts, bitfields, packing, the 493-byte descriptor array and `Bcm5974ConfigTable` therefore come from the C
  compiler.
- Compiler: clang 23.1.6, `--target=x86_64-pc-windows-msvc` (MSVC ABI and bitfield layout), `-O1`, with these
  defines: `-D_WIN64 -D_AMD64_ -DAMD64 -D_WIN32_WINNT=0x0A00 -DWINVER=0x0A00 -DWINNT=1 -DNTDDI_VERSION=0xA000010
  -DDEPRECATE_DDK_FUNCTIONS=1 -DMSC_NOOPT -DKMDF_VERSION_MAJOR=1 -DKMDF_VERSION_MINOR=15 -D_KERNEL_MODE=1`.
  - Include path: the real WDK 10.0.28000.0 `km/crt`, `km`, `shared` headers and the KMDF 1.15 headers.
  - Link: lld-link against the VS 18 (MSVC 14.52) and UCRT 10.0.28000 libs.
- WPP: the `*.tmh` files are empty. A forced include defines `TraceEvents(...)`, `WPP_INIT_TRACING(...)` and
  `WPP_CLEANUP(...)` as no-ops whose arguments are **not evaluated**, which is the same as WPP with the provider
  disabled. As a result `DbgDevicePowerString` and the FUNCSWITCH trace read of `reportBuffer[2]` never run.
- Fake KMDF: the driver calls WDF only through the KMDF 1.15 header inlines, which dispatch via
  `WdfFunctions_01015[]`. The harness supplies that 444-entry table. Every slot the driver does not use aborts
  the run. `__imp_KeQueryPerformanceCounter` is stubbed. The fake is compiled in a translation unit that
  includes the project's `Driver.h`, so it reads and writes `DEVICE_CONTEXT`, `HID_XFER_PACKET`,
  `USB_DEVICE_DESCRIPTOR` and the other structs with the C layouts.
- The harness sources are not committed. They are kept outside the repository (stubs.c, main.c, hx.h,
  build_*.sh). The run is deterministic: two runs produce byte-identical files. The PRNG is splitmix64 with seed
  `0x5EED0000 + pid` per family. The inputs are recorded in full, so replaying the fixtures does not need the PRNG.

## Common format

- One record per line, LF line endings, ASCII. Fields are separated by a single space.
- Split each field at its **first** `=`. Some values contain further `=`, `:`, `,`, `|` or `;`.
- Byte strings are lowercase hex with no separators. `-` means an empty byte string or "none".
- `status` is an NTSTATUS written as `0x%08x`. `info` is the request's `IoStatus.Information`, in decimal.
- `tags` is a comma list, or `-` when empty. A Rust test should treat these tags as follows:

| tag | meaning |
|---|---|
| `golden_A` `golden_B` `golden_Bprime` `golden_C` `golden_D` | the spec §7.4 golden vector with that name |
| `unreachable_state` | Driver state the real driver never has. `PtpReportTouch` and `PtpReportButton` are always TRUE, and `LastReportTime` always equals the QPC frequency. These records pin the C arithmetic. A port that drops the dead gates may **skip** them. |
| `scantime` | ScanTime arithmetic with `LastReportTime != frequency`. Also tagged `unreachable_state`. |
| `c_leak_B6`, `c_leak_B7` | C never completes the READ_REPORT (spec B6/B7). The record holds `completed=0 owned=1` and the status of the failing call is visible in `ops`. port policy P2 says Rust completes the request with that error-path status, `0xc0000023` here. |
| `c_bugcheck_B4` | `WdfMemoryCreate` failed. C then calls `WdfObjectDelete(NULL)`, which is a bugcheck on a real system. The fake logs `obj_delete:null` and continues. The Rust port (P2) skips the delete and returns the same status. |
| `c_pool_overrun_B3` | The device returned more than 2 bytes into C's 2-byte allocation, which overruns the pool. The harness allocation is at least 16 bytes and zero-filled, which is exactly the P2 fix of allocating the full wLength (4) zeroed. Rust must reproduce these records as written. |
| `ub_oob_read` | SET_FEATURE 0x04 with `reportBufferLen < 2`. C reads `reportBuffer[1]` past the declared length. The harness buffer physically holds that byte, so C acted on it. P2 says "treat as 0", so Mode=0 gives `0xc00000bb`. The Rust result therefore **differs** from these records. Skip them, or assert the P2 behavior instead. |
| `c_stale_handle_B20` | The second PrepareHardware finds no interrupt pipe but keeps the stale `InterruptPipe` handle. See wire.txt. |
| `beyond_transfer_len` | N=556, which is more than the 526-byte reader buffer and impossible on real hardware. Included only to show that N=46+30k with k>5 gives DATA_ERROR. |
| `fallback_pid` | INF-only or unknown PID that maps to table row 0 (0xFFFF). |

### NTSTATUS values seen

`0x00000000` SUCCESS, `0xc0000010` INVALID_DEVICE_REQUEST, `0xc0000023` BUFFER_TOO_SMALL, `0xc000003e`
DATA_ERROR, `0xc00000bb` NOT_SUPPORTED, `0xc0000184` INVALID_DEVICE_STATE, `0xc0000206` INVALID_BUFFER_SIZE,
`0x8000001a` NO_MORE_ENTRIES (appears in `ops` only).

There are two origins for a status.

**C-code literals:** SUCCESS, NOT_SUPPORTED, DATA_ERROR, INVALID_DEVICE_STATE, INVALID_BUFFER_SIZE,
INVALID_DEVICE_REQUEST, and the BUFFER_TOO_SMALL of the GET/SET_FEATURE `HID_XFER_PACKET` length check.

**Fake framework,** modeled on the MS Learn documentation:
- `WdfRequestRetrieveOutputMemory`: BUFFER_TOO_SMALL when the output length is 0.
- `WdfRequestRetrieveOutputBuffer`: BUFFER_TOO_SMALL when the length is 0 or less than the minimum.
- `WdfMemoryCopyFromBuffer`: BUFFER_TOO_SMALL when offset+length exceeds the buffer.
- `WdfIoQueueRetrieveNextRequest` on an empty queue: NO_MORE_ENTRIES.

Any status named in an `inject=` field, or in a `harness_inject` step, is an arbitrary failure the harness forced
on a WDF call. The C code only propagates it or ignores it.

## Harness model (not C behavior)

- **Device:** `USB_DEVICE_DESCRIPTOR`, 18 bytes:
  `12 01 00 02 00 00 00 40 <vid le> <pid le> <bcd le> 01 02 03 01`.
  - The defaults are VID 0x05AC and bcdDevice 0x0100, with traits `0x4` (AT_HIGH_SPEED). The `traits-all`
    scenario uses `0x7`.
  - There is one pipe: interrupt IN, EP 0x83, MaxPacketSize 64.
- **GET_REPORT response:** the fake device answers the wellspring GET_REPORT with `02 00`, 2 bytes, unless a
  `ctl_read_response`/`harness_ctl_read_response` says otherwise. At most `mdlen` bytes are delivered.
- **Allocation fill:** `WdfMemoryCreate` hands out a zero-filled block of at least 16 bytes. So bytes 2..3 of the
  4-byte SET_REPORT data stage are 0 unless the device wrote them. That matches P2's "allocate the full wLength
  zeroed". In C those bytes lie past the 2-byte allocation (spec B3).
- **wLength:** `setup` is the wire setup packet. Its wLength is filled from the memory-descriptor length, which is
  `sizeof(int)` = 4, the way the KMDF formatter does it. `setup_raw` is the packet exactly as the C code built it
  (`WDF_USB_CONTROL_SETUP_PACKET_INIT` leaves wLength 0). **The Rust builder must emit the `setup` bytes itself**,
  e.g. `a1 01 02 03 02 00 04 00`.
- **QPC:** `KeQueryPerformanceCounter(p)` stores `qpc_freq` = 10,000,000 in `*p`. Its return value, a counter
  starting at 1,000,000,000 and growing by 1234 per call, is discarded by the C code. The `wire.txt`
  `qpc-freq-*` scenarios use other frequencies. The real driver stores the *frequency* (spec B1), so
  `LastReportTime` = frequency and ScanTime = 0 in every reachable state.
- **READ_REPORT buffers:** for METHOD_NEITHER, `Irp->UserBuffer` equals the output buffer. Request output
  buffers are prefilled (`prefill`) so that untouched bytes are visible. The harness asserts that nothing is
  written past `outlen`.
- **Reader buffer:** each packet is replayed twice. Bytes past N in the reader buffer are 0xEE in one replay and
  0x11 in the other. The harness aborts if any result differs, so no output depends on bytes beyond N.
- **Requests:** each scenario starts from a fresh framework (`hx_reset`), so request ids (`req=`) restart at 1.

## Files

| file | records | content |
|---|---|---|
| `descriptors.txt` | 173 | report/HID descriptor bytes, C layouts, constants, IOCTL codes, config table, PID→row mapping, HID descriptor / report descriptor / attributes IOCTL outputs |
| `ioctl.txt` | 61 | dispatcher routing for every HID/USB internal IOCTL code + unknown codes, READ_REPORT pend and forward failure, FIFO service, EvtIoStop, ReadersFailed |
| `feature.txt` | 140 | GET_FEATURE / SET_FEATURE through the real dispatcher, incl. wire bytes of the mode switch |
| `wire.txt` | 240 | lifecycle steps (DriverEntry, DeviceAdd, PrepareHardware, D0Entry, D0Exit, cleanup) with ops + control-transfer wire bytes, failure injection |
| `input_ffff.txt` | 1145 (incl. 1 `kind=defaults` line) | reader completion → PTP report, config row 0 (0xFFFF fallback; also used by 0273 0274 0277 027E 027F 0280 0290 0291 0340 and unknown PIDs) |
| `input_027a.txt` `input_027b.txt` | 1134 each (incl. defaults line) | rows 1/2 (13", x.min −6243, y.max 7685) |
| `input_027c.txt` `input_027d.txt` | 1134 each (incl. defaults line) | rows 3/4 (15", x.min −10000, y.max 10000) |

### input_<row>.txt

These records come from `AmtPtpEvtUsbInterruptPipeReadComplete`, invoked through the continuous-reader callback
and context the driver registered. Every record starts from a full DriverEntry → DeviceAdd → PrepareHardware(pid)
→ D0Entry(D3Final) bring-up. Then the record's driver state is set, `npend` READ_REPORTs go through the real
dispatcher into the InputQueue, and the packet is delivered.

**Line 1** is `kind=defaults ...`. It gives the fixed context of the random records (`case=<row>-rand-*`):

| field | value |
|---|---|
| `outlen` | 50 |
| `prefill` | a5 |
| `npend` | 1 |
| `qpc_freq` | 10000000 |
| `st_in` | `wsm:1,touch:1,button:1,lrt:10000000` |
| `qdepth_in` | 1 |
| `st_out` | equal to `st_in` (asserted) |
| `owned` | 0 |

Random records carry only `case pid n in res completed status info out qdepth_out`.

**Edge records** (`case=<row>-<name>`) carry these fields:

| field | meaning |
|---|---|
| `pid` | USB idProduct given to PrepareHardware. It selects the config row. |
| `n` | `NumBytesTransferred` |
| `in` | the n packet bytes |
| `outlen` | output length of the first pended READ_REPORT |
| `prefill` | prefill byte of its buffer |
| `npend` | number of READ_REPORTs pended before the packet (0, 1 or 2) |
| `qpc_freq` | value KeQueryPerformanceCounter writes to its out-parameter |
| `st_in`, `st_out` | driver state before and after (mapping below) |
| `qdepth_in`, `qdepth_out` | InputQueue depth before and after |
| `res` | the outcome (values below) |
| `completed` | 1 if `WdfRequestComplete` was called on the first request |
| `owned` | 1 if the driver retrieved the request and never completed it |
| `status`, `info` | completion status and Information. Both are 0 when not completed. |
| `out` | the first request's whole output buffer (`outlen` bytes) after the call. `unchanged` means the buffer still equals the prefill (random records only). |
| `ops` | the WDF call log, on selected records |

`res` takes these values:

| value | meaning |
|---|---|
| `report` | completed SUCCESS, info 50 |
| `data_error` | completed `0xc000003e`, info 0, buffer untouched |
| `drop_len` | length gate failed. No request was touched and it stays queued. |
| `no_request` | the queue was empty and the packet was discarded |
| `leak_outmem` | RetrieveOutputMemory failed. The request was not completed. |
| `leak_copy` | CopyFromBuffer failed. The request was not completed. |

Within one record, `npend=2` shows FIFO order: only the first request is served, and the second stays queued.

State fields map to `DEVICE_CONTEXT` members as follows:

| field | C member |
|---|---|
| `wsm` | `IsWellspringModeOn` |
| `touch` | `PtpReportTouch` |
| `button` | `PtpReportButton` |
| `lrt` | `LastReportTime.QuadPart` |

The function also reads `DeviceInfo` (selected by `pid`) and `InputQueue`. It reads nothing else and writes no
context field, as `st_out` == `st_in` shows everywhere.

The output report is 50 bytes:
- `[0]=05`.
- 5 × 9-byte contacts at offset 1+9k:
  - flags: bit0 Confidence, bit1 TipSwitch
  - ContactID u32 LE
  - X u16 LE
  - Y u16 LE
- ScanTime u16 LE @46.
- ContactCount @48.
- button @49.

The edge groups exist in every `input_<row>.txt`:

| group | cases |
|---|---|
| header and button | `hdr-*`, `btn-*`, `golden-a` |
| golden B/B′ | `golden-b` |
| finger counts | `fingers-1..5`, plus `fingers-6 7 10 16 17` (DATA_ERROR) |
| length gate | `len-*`: 0..525, every invalid kind |
| X sweep | `x-*`: ±32768, x.min±1, x.max+1, ... |
| Y sweep | `y-*`: y.max±1, ... |
| sign-bit raw values | `raw-*` |
| tip/confidence thresholds | `tip-<major>-<minor>` |
| per-slot flags | `slots-flags` |
| ignored fields | `ignored-fields-*`: origin, rel, tool, orientation, pressure, multi, delta bytes 46..47, header bytes |
| slot 0 empty | `slot0-empty` |
| request handling | `req-*`: none, FIFO, outlen 0/1/49/50/51/64, combined with a DATA_ERROR packet and with a bad length |
| driver state | `state-*` |
| ScanTime | `scantime-lrt-*` |

The `ffff` file also contains `pid<xxxx>-golden-b` for the fallback PIDs.

Random corpus, 1000 records per row:
- ~76% are valid lengths with k≤5 blocks. Three quarters of those carry plausible values: coordinates biased to
  x.min/x.max/y.min/y.max ±3 and to i16 extremes, touch values biased to the thresholds 100/75/1 and to
  0/±1/±32768. The rest are fully random bytes.
- ~11% have k=6..16, which gives DATA_ERROR.
- ~13% have invalid lengths.

Header bytes are random throughout, because the parser checks only byte 31 (button). The case suffix tells the
kind: `plaus`, `rand`, `many` or `badlen`.

C UB that is kept, because it is deterministic: `touch_major << 1` and `touch_minor << 1` on negative values. The
results equal `v * 2` in i32, which cannot overflow because |v| ≤ 32768. clang folds `(v<<1) > 0` to `v > 0`, and
MSVC's `shl` gives the same answers. The Rust port should use `v * 2`.

### descriptors.txt (`kind=`)

| kind | fields |
|---|---|
| `report_descriptor` | `len` and `bytes` of the compiled global `AmtPtpT2ReportDescriptor` |
| `hid_descriptor` | `len` and `bytes` of the compiled global `AmtPtpT2DefaultHidDescriptor` |
| `layout` | `sizeof` and `FIELD_OFFSET` values from the compiler |
| `consts` | `pool_tag` ('PTPC' multichar), `paged_pool` (POOL_TYPE value of PagedPool), `reader_transfer_len` (HEADER_TYPE4+FSIZE_TYPE4*16), `devif_guid_mem` (the interface GUID as its 16 in-memory bytes) |
| `ioctl_code` | IOCTL values computed by the WDK macros |
| `config_row` | every row of `Bcm5974ConfigTable`, including the `{0}` terminator. `p` `w` `x` `y` `o` are `snratio,min,max`. |
| `config_select` | the row PrepareHardware stored in `DeviceInfo` for a given PID. `cfg` = `DeviceInfo->identification`. |
| `ioctl` | One request through the real dispatcher after bring-up to D0. Fields: `vid pid bcd` (descriptor), `prepared` (0 = only DeviceAdd ran, so `DeviceDescriptor` is still zero), `ioctl`, `name`, `outlen`, `inlen`, `prefill`, `completed`, `status`, `info`, `qdepth_*`, `out` (the whole output buffer after the call), `ops`. |

### ioctl.txt (`kind=`)

| kind | content |
|---|---|
| `route` | Same fields as `ioctl`. The GET/SET_FEATURE routes use a valid `HID_XFER_PACKET` with report id 0x55. `read-report-fwd-fail-*` records have `inject=fwd_to_queue:<st>`. |
| `fifo` | Three READ_REPORTs (outlen 50) are pended, then four 1-finger packets arrive. Per request: `reqN=completed:..,status:..,info:..,queued:..` and `outN` (50 bytes). |
| `io_stop` | `EvtIoStop` with the given `action_flags` on a queued request. `ops=-` means no WDF call: nothing is completed or acknowledged. |
| `readers_failed` | `EvtUsbTargetPipeReadersFailed(status, usbd_status)` → `ret` (1 = TRUE, i.e. "reset and restart"). |

### feature.txt (`kind=get_feature|set_feature`)

The request goes through the real dispatcher with `Irp->UserBuffer` = `HID_XFER_PACKET`. When `userbuffer=null`,
UserBuffer is NULL.

| field | meaning |
|---|---|
| `outlen`, `inlen` | `Parameters.DeviceIoControl.Output/InputBufferLength` |
| `report_id` | `HID_XFER_PACKET.reportId` |
| `report_buffer_len` | `reportBufferLen` |
| `buf_actual` | real size of the `reportBuffer` allocation |
| `buf_in`, `buf_out` | the `reportBuffer` allocation before and after the call (`buf_actual` bytes) |
| `inject` | failure injection or device response (`ctl_read_response:<hex>`) |
| `st_in`, `st_out` | driver state, as in the input files. `st_in.wsm` is set before the call. |
| `wire` | control transfers caused by the request (format below) |
| `ops` | the WDF call log |

`info` is always 0: GET_FEATURE and SET_FEATURE never set Information (spec B13).

### wire.txt

Each line is one step of one scenario.

| field | meaning |
|---|---|
| `scenario` | scenario name |
| `step` | 1-based step number |
| `call` | one of the calls below |
| `arg` | the call's argument |
| `status` | the callback's return value, `-` for void calls and harness steps |
| `ctx_out` | `DEVICE_CONTEXT` after the step (mapping below) |
| `wire` | control transfers during the step |
| `ops` | the WDF call log |

`call` takes these values:

| value | meaning |
|---|---|
| `driver_entry` | DriverEntry |
| `device_add` | EvtDriverDeviceAdd |
| `prepare_hardware` | EvtDevicePrepareHardware |
| `d0_entry` | EvtDeviceD0Entry. `arg` = previous `WDF_POWER_DEVICE_STATE`: 4=D3, 5=D3Final, 6=PrepareForHibernation |
| `d0_exit` | EvtDeviceD0Exit. `arg` = target state, same values |
| `driver_context_cleanup` | the WDFDRIVER cleanup callback |
| `harness_*` | a fake-hardware change before the next step: `harness_inject <point>:<status>`, `harness_pipes <type>/<ep>,...` (type 1 Control, 2 Isochronous, 3 Bulk, 4 Interrupt), `harness_descriptor`, `harness_ctl_read_response` |
| `note` | carries `tags:` for the scenario |

`ctx_out` fields map to the context as follows:

| field | C |
|---|---|
| `vid` `pid` `bcd` | `DeviceDescriptor.idVendor` / `idProduct` / `bcdDevice` |
| `cfg` | `DeviceInfo->identification` (`none` = NULL) |
| `wsm` | `IsWellspringModeOn` |
| `ptpinputon` | `PtpInputOn` |
| `touch` | `PtpReportTouch` |
| `button` | `PtpReportButton` |
| `lrt` | `LastReportTime.QuadPart` |
| `traits` | `UsbDeviceTraits` |
| `usbdev` `iface` `inputq` | `UsbDevice` / `UsbInterface` / `InputQueue` non-NULL |
| `pipe` | index of the configured pipe stored in `InterruptPipe` (-1 = NULL) |

`ctx_out=none` means no context exists yet.

Scenarios:

| scenario | content |
|---|---|
| `std-<pid>` | full cycle: start, D3, D0, hibernate, D0, D3Final, cleanup |
| `qpc-freq-*` | other QPC frequencies |
| `fail-*` | DriverEntry, DeviceAdd and PrepareHardware failures, incl. the masked reader-config failure (B9) and retrieve-info failure (traits = 0) |
| `pipes-*` | pipe selection: first interrupt pipe wins, direction unchecked, NoMaximumPacketSizeCheck on every visited pipe, INVALID_DEVICE_STATE with no interrupt pipe |
| `rebalance-*` | second PrepareHardware: `usbdev_create` is not repeated, the descriptor is re-read |
| `d0entry-*` | D0Entry mode-switch failures (ignored), IoTargetStart failure (returned) and device-response variants |
| `d0exit-*` | D0Exit returns the wellspring-OFF status (B10) |

`rebalance-stale-pipe` keeps the stale handle. The harness reuses pipe objects by index, so the stale handle is
reported as `pipe:0`. On real KMDF the old pipe object was deleted by the reselection, so this is a
use-after-free. P2/[DECIDE]: resetting `InterruptPipe` before the loop yields `0xc0000184` instead.

### Formats shared by feature.txt and wire.txt

**`wire`** is `-` when there were no transfers. Otherwise it is a `;`-joined list of
`<in|out>:<setup 8 bytes>:<data>:<status>`.
- For `in` transfers, `data` is what the device returned.
- For `out` transfers, `data` is what was sent: `mdlen` = 4 bytes.
- A failed `in` has data `-`.

**`ops`** is a `|`-joined list of `name[:k=v,...]` entries, one per WDF call, in call order. An optional `st=`
gives the status the fake returned. The names are:

| name | fields |
|---|---|
| `drvcreate` | `WDF_DRIVER_CONFIG` and attributes fields. Enum values: exec/sync 1 = InheritFromParent. |
| `fdo_init_set_filter` | — |
| `pdo_init_allow_fwd_to_parent` | — |
| `set_pnp_power_cb` | `others`=1 if any callback besides prepare/d0entry/d0exit is set |
| `devcreate` | context type name and size |
| `devif` | interface GUID, as memory bytes |
| `queuecreate` | dispatch 2=Parallel, 3=Manual; power 0=WdfFalse, 2=WdfUseDefault; `npresented`; callback presence |
| `fwd_to_queue` | `queue=1` is InputQueue |
| `retrieve_next` | — |
| `retrieve_outmem` | — |
| `retrieve_outbuf` | — |
| `mem_copy_from` | — |
| `set_info` | — |
| `complete` | — |
| `get_params` | — |
| `wdm_get_irp` | — |
| `usbdev_create` | — |
| `usbdev_get_desc` | — |
| `usbdev_retrieve_info` | — |
| `usbdev_select_config` | `type=2` = SingleInterface |
| `usbif_get_pipe` | — |
| `pipe_no_max_pkt_check` | — |
| `pipe_config_cont_reader` | `WDF_USB_CONTINUOUS_READER_CONFIG` fields: `len`=TransferLength, `npending`=NumPendingReads, callback identities |
| `iotarget_start` | — |
| `iotarget_stop` | `action=1` = WdfIoTargetCancelSentIo |
| `mem_create` | `pool=1` PagedPool, `tag`, `size` |
| `obj_delete` | `memory`, or `null` = `WdfObjectDelete(NULL)` |
| `ctl_xfer` | `setup_raw`, `setup`, `mdlen`, `dir`, `alloc_size` (the C-requested size), `data`, `cb`, `st` |
| `qpc` | `freq_out` = value stored to the out-parameter; `ret` = discarded return value |

## Excluded cases (UB in C, not in any fixture)

- GET_FEATURE 0x07 or 0x08 and SET_FEATURE 0x04 with `reportBuffer == NULL` and a large enough
  `reportBufferLen`. That is a NULL write or read (B16). A garbage non-NULL `HID_XFER_PACKET` pointer is also
  excluded.
- ScanTime with `LastReportTime` values that make `freq - LastReportTime` overflow `LONGLONG`, e.g.
  `LastReportTime = INT64_MIN`. Signed overflow is UB, and these values are unreachable anyway.
- SET_REPORT data bytes 2..3 when the pool block past C's 2-byte allocation holds garbage. C sends whatever is
  there. The fixtures use the zero-filled (P2) model only.
- Reader completions with N > 526 (the reader TransferLength) cannot occur on hardware and are not generated,
  except `fingers-17` (N=556, tag `beyond_transfer_len`). That case ends in DATA_ERROR without reading past N.
- Kept but tagged, not excluded: `ub_oob_read`, `c_pool_overrun_B3`, `c_bugcheck_B4`, `c_leak_B6`/`B7`,
  `c_stale_handle_B20`.

## Spec check (`usbkm.md`, incl. Verification addendum)

An independent Python model of spec §7.2, with ScanTime = `trunc((freq-lrt)/100)` clamped above at 255 and then
cast to u16, was run against all 5676 input records, edge and random. It found **0 mismatches**.

These outputs were diffed byte for byte against the spec and match:
- the golden vectors A, B, B′, C and D
- Appendix A (report descriptor, 493 bytes)
- Appendix B (HID descriptor)
- the §3 context offsets, which give an 80-byte context
- §4.1 queue configuration
- §5.3 attributes, with Reserved untouched
- §5.5 GET_FEATURE: caps `07 05 00`; HQA writes only `[0]=08 [1]=fc`; Information 0
- §5.6 SET_FEATURE semantics
- §6.2 reader config: 526 bytes, NumPendingReads 0
- §6.3 wire bytes: `a1 01 02 03 02 00 04 00` and `21 09 02 03 02 00 04 00` with data `02 01 00 00` (ON) and
  `02 00 00 00` (OFF)
- the D0Entry and D0Exit status rules

**No spec statement was found to be wrong.**

The spec leaves out one detail that these records now pin. When `LastReportTime > frequency`, C computes a
negative `PerfCounterDelta`. The clamp is signed, so the negative value passes through, and the `(USHORT)` cast
wraps it: `+100` gives ScanTime 0xFFFF, and `+250` gives 0xFFFE. This state is unreachable in the real driver.

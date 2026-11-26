# AmtPtpDeviceUsbUm oracle fixtures

These files record the behavior of the original C driver `src/AmtPtpDeviceUsbUm`, captured by executing it.
They are the ground truth that the `amtptp-usbum-core` tests replay. They were not derived from reading the code
or the spec. The C code ran against a recording fake of the UMDF 2.15 DDI table, described below.

## Provenance

- **Sources:** `src/AmtPtpDeviceUsbUm/{Device,Driver,Hid,InputInterrupt,Queue}.c` and `include/*`, at git commit
  `2edb0f2`. That commit is identical to `29d2778` for these files.
- **Compiler:** clang 23.1.6 (`C:/Program Files/LLVM/bin/clang.exe`), target `x86_64-pc-windows-msvc`. This gives the
  MSVC ABI, so struct layout, `#pragma pack`, bit-field allocation and `__declspec(align)` are the MSVC ones.
- **Compiler flags:**
  `-O0 -g -fwrapv -fno-strict-aliasing -w -DUMDF_VERSION_MAJOR=2 -DUMDF_VERSION_MINOR=15 -D_WIN32_WINNT=0x0A00 -DWINVER=0x0A00`.
  `-fwrapv` makes signed overflow wrap, as MSVC code generation does. This matters for `INT_MIN` negation in the MT2
  Y decode, and for `<<1` on negative values.
- **Headers:**
  - the real SDK/WDK 10.0.28000.0 headers (`um`, `shared`, and `km` via `-idirafter` for `hidport.h`)
  - the UMDF 2.15 headers (`Include/wdf/umdf/2.15`)
  - the project's own `include/` directory, so every struct, bit-field and table comes from the project headers
- **Linking:** the objects are linked with MSVC 14.52 (VS 18) libs into a host harness, together with a recording
  fake (`fake.c`) and the generators (`harness.c`). These harness sources are not committed. They live in the
  session scratch directory `work/oracle-usbum/`.
- **Compiled unmodified: all five `.c` files, every function.** No function was copied or edited.
  The only substitutions are made in the preprocessor, through a forced-include header (`-include pre.h`), and
  they affect only non-WDF services:
  - `QueryPerformanceCounter` is redirected to a deterministic harness clock. Each call returns the record's `qpc`.
  - `TraceLoggingWrite`, `TraceLoggingRegister` and `TraceLoggingUnregister` are redirected to recorders. Only the
    event name is recorded. The provider definition stays real.
  - WPP `TraceEvents`, `WPP_INIT_TRACING` and `WPP_CLEANUP` become no-ops, and the generated `*.tmh` files are empty
    stubs. This matches a build with tracing off. `WPP_CLEANUP` discarding its argument is also why the undeclared
    `Driver` in `Driver.c:105` (B19) compiles.
- **WDF calls:** every WDF call goes through `WdfFunctions_02015[...]`. The fake fills all 257 slots with a trap, then
  implements only the 34 DDIs the driver actually calls. No trap was hit.
- **Determinism:** the harness is deterministic. Two runs produce byte-identical files. Random data comes from
  splitmix64. Packet corpus `t<N>` uses seed `0xA5A50000 + N`. The PRNG also chooses the per-record state.
- **Overflow guard:** every buffer handed to the driver is placed directly before a `PAGE_NOACCESS` guard page. This
  covers input packets and request input/output buffers. Any over-read in a recorded case would have crashed the
  harness. One example this guards: TYPE4 reads its last finger up to `+20`, inside a `46+30k` packet.

### Framework model

The statuses that come from WDF were not produced by executing UMDF. They come from this fake, which mirrors the
open-source WDF code (microsoft/Windows-Driver-Frameworks):

| DDI | Modelled behavior | WDF source |
|---|---|---|
| `WdfRequestRetrieveInputMemory`, `WdfRequestRetrieveOutputMemory` | Length 0 returns `STATUS_BUFFER_TOO_SMALL` (0xc0000023) | `FxRequest::GetMemoryObject` / `GetDeviceControlOutputMemoryObject` |
| `WdfRequestRetrieveOutputBuffer` | Length 0, or length < required, returns `STATUS_BUFFER_TOO_SMALL` | — |
| `WdfMemoryCopyFromBuffer` | Count > destination length returns **`STATUS_INVALID_BUFFER_SIZE` (0xc0000206)** | `IFxMemory::_CopyPtrToPtr` |
| `WdfMemoryCopyToBuffer` | Count > source length returns `STATUS_BUFFER_TOO_SMALL` | `IFxMemory::_CopyPtrToPtr` |
| `WdfUsbTargetPipeConfigContinuousReader` | A second call on the same pipe returns `STATUS_INVALID_DEVICE_STATE` (0xc0000184) | `FxUsbPipe::InitContinuousReader` |
| `WdfUsbTargetDeviceSendControlTransferSynchronously` | wLength on the wire is the memory-descriptor length. bmRequestType, bRequest, wValue and wIndex come from the driver's packet. | UM `FormatControlRequest` |
| `WdfUsbTargetDeviceAllocAndQueryString` | The memory object is `numChars*2` bytes | — |
| `WdfIoQueueRetrieveNextRequest` on an empty queue | Returns `STATUS_NO_MORE_ENTRIES` | — |
| `WdfRequestForwardToIoQueue` | Always succeeds | — |

Object contexts are zero-filled.

**Device model (USB control pipe):** the device holds an 8-byte "mode register", written as `dev=<16 hex>/<resp>`.

- A GET (IN) transfer returns the first `min(resp, wLength, 8)` register bytes. It writes them to the start of the
  driver buffer and leaves the rest of the buffer untouched.
- A successful SET (OUT) transfer stores the `wLength` payload bytes at the register start.
- `xs=` scripts the status of the successive control transfers within one record. A missing entry, or `-`, means
  success. A failed IN returns no data.
- Strings for GET_STRING are defined for indices 1 to 4:

  | Index | String |
  |---|---|
  | 1 | `"Apple Inc."` (10 WCHARs) |
  | 2 | `"Apple Internal Keyboard / Trackpad"` (34 WCHARs) |
  | 3 | `"ABCD"` (4 WCHARs) |
  | 4 | `"AB"` (2 WCHARs) |

  Any other index fails with `STATUS_UNSUCCESSFUL` (0xc0000001).
- The device descriptor uses idVendor 0x05ac, the record's idProduct, bcdDevice 0x0105, iManufacturer 1, iProduct 2
  and iSerialNumber 3. Records with `vid=`/`bcd=` fields state the values they used; some vary idVendor and
  bcdDevice.
- In `dev2=` only the 8 register bytes are given. The response length does not change.

## Common format

- One record per line, LF line endings, ASCII. Fields are separated by single spaces and written as `key=value`.
  Keys are unique within a line and match `[a-z0-9_]+`. **Split each field at the first `=`**, because some values
  contain `=` themselves (for example `calls=ConfigContinuousReader(0)=0x00000000`). Values never contain spaces.
- Byte strings are lowercase hex with no separators. `-` means an empty string or "none".
- Statuses are `0x%08x` (the NTSTATUS as u32). Wire statuses inside `wire=` have no `0x`.
- Integers are decimal. `perf` and `qpc` are signed i64.
- `case=` is unique within a file. `seq=` with `step=` gives the order for multi-step records.
- Output buffers are pre-filled with `0xa5`. `out=-` means the buffer is unchanged: every byte is still `0xa5`.
  Otherwise `out=` is the full output buffer after the call.
- In input files the READ_REPORT buffers are pre-filled with `0xee`. The harness asserts that bytes from 50 onward
  are never written.

### State fields → C members (`DEVICE_CONTEXT`, Device.h)

| field | C member |
|---|---|
| `wsm` | `IsWellspringModeOn` (BOOL) |
| `surf` | `IsSurfaceReportOn` |
| `btn` | `IsButtonReportOn` |
| `pq` / `sq` / `mq` | `PressureQualLevel` / `SgContactSizeQualLevel` / `MuContactSizeQualLevel` (UCHAR) |
| `perf` | `PerfCounter.QuadPart` |
| `pid` | `DeviceDescriptor.idProduct` |
| `row` / `fam` | `DeviceInfo` (row of `Bcm5974ConfigTable`, see devices.txt) |
| `pipe` | index of `InterruptPipe` among the configured pipes |
| `traits` | `UsbDeviceTraits` |
| `usbdev` | `UsbDevice != NULL` |

The suffix `2` (`wsm2`, `perf2`, `dev2`, …) marks the value after the call. Without the suffix, the field is the
value before the call.

### `wire=` (control transfers made during the call, in order, comma-separated)

- `in/<setup 8 bytes>/<data the device returned>/<status>`
- `out/<setup 8 bytes>/<payload sent>/<status>[/<umask>]`

The setup is the 8 bytes on the wire: bmRequestType, bRequest, wValue LE, wIndex LE, wLength LE. wLength is
always 4 (B1).

`umask` appears only when a payload byte is undefined in C. Those bytes are shown as `00`, and `umask` marks them
`ff`.

This happens for TYPE4/TYPE5. The buffer is a 2-byte `WdfMemoryCreate(um_size)`, but it is sent with a 4-byte
descriptor. When the device returned fewer than 4 bytes, payload bytes 2 and 3 come from past the allocation (heap
garbage). The P2 fix allocates a zeroed 4-byte buffer, which gives `00`. The register stores the shown bytes.

## Files

### devices.txt (44)

One record per distinct PID in `Bcm5974ConfigTable` (`variant=ansi|iso|jis`). Three more records,
`variant=vid<idVendor>`, use idVendor 0x1234/0x0000/0xffff and bcdDevice 0x0000/0xffff. They still match by PID
(B23). It is captured after
`EvtDevicePrepareHardware`, from `ctx->DeviceInfo` (the driver TU's own copy of the table) and the
continuous-reader config the driver passed.

| Field | Content |
|---|---|
| `row` | table index |
| `ansi iso jis caps bt_ep bt_datalen tp_ep type header datalen button fsize delta um_size um_req_val um_req_idx um_switch_idx um_switch_on um_switch_off` | the `struct BCM5974_CONFIG` members |
| `p w x y o` | `snratio,min,max` |
| `reader_len` | `TransferLength` |
| `reader_npending` | `NumPendingReads` |
| `reader_hdr`, `reader_trl` | Header and Trailer lengths |
| `reader_ctx_is_devctx` | whether the completion context is the device context |
| `reader_failed_cb` | `EvtUsbTargetPipeReadersFailed` is set |
| `reader_bufattr` | whether buffer attributes are set |
| state after (`…2`) | the flags after PrepareHardware |

### descriptors.txt (898)

These records are IOCTLs sent through the real dispatcher (`AmtPtpDeviceEvtIoDeviceControl`) on a started device:

- `IOCTL_HID_GET_DEVICE_DESCRIPTOR`, with output lengths 0, 1, 8, 9, 10 and 64, plus one record with a 4-byte input
  buffer
- `IOCTL_HID_GET_REPORT_DESCRIPTOR`, with output lengths 0, 1, 515 and 516, plus 520 for the ansi PID
- `IOCTL_HID_GET_DEVICE_ATTRIBUTES`, with output lengths 0, 1, 31, 32, 33 and 40

They run for every PID of every table row. Rows without a descriptor return 0xc0000184.

The `vid<v>-bcd<b>-*` records vary idVendor and bcdDevice. VendorID stays 0x8910 and VersionNumber stays 1.

Two further groups:

- `inj=1` records: idProduct values that cannot pass PrepareHardware are written straight into
  `ctx->DeviceDescriptor` (see the B3 exclusion below).
- `IOCTL_HID_GET_STRING` (MT2 device): input lengths 0 to 3; ids 14, 15, 16, 0, 1, 13, 17 and 0xffff; languages
  0x0409, 0 and 0xffff; and three descriptor index sets, `str-d0` (1,2,3), `str-d1` (1,2,4) and `str-d2` (0,0,0). It
  also includes a query failure and output lengths below, equal to and above `wcharCount*2`.

| Field | Content |
|---|---|
| `ioctl` | the IOCTL code |
| `vid` / `bcd` | the device descriptor's idVendor and bcdDevice |
| `inlen` / `in` | input buffer |
| `outlen` | output buffer length |
| `cpl` | completion count (always 1) |
| `status` / `info` | completion status and Information |
| `out` | the output buffer after the call |
| `tl` | TraceLogging event names |
| `strq` | string queries sent to the device (`<index>/<langid>`) |
| `calls` | `ctb<n>=<status>[:stack]` for `WdfMemoryCopyToBuffer` |

In `calls`, `:stack` marks a copy that succeeded into the driver's own stack variable `&pStringBuffer` (B7). The
fake logs that write and skips it, because it is not observable.

### ioctl.txt (24)

Dispatcher routing for the HID minidriver codes (function ids 0 to 10 and 20 to 25) plus 7 other codes. Each request has a 4-byte input `07000000` and an
8-byte output.

| Field | Meaning |
|---|---|
| `cpl` | completion count |
| `status` | completion status, or `-` when not completed |
| `info` | Information |
| `queued` | 1 when the request sits in the manual InputQueue (READ_REPORT is forwarded, not completed) |
| `inputq` | InputQueue depth |

### feature.txt (1050)

`IOCTL_UMDF_HID_GET_FEATURE` (0x000b0057) and `IOCTL_UMDF_HID_SET_FEATURE` (0x000b0053), through the dispatcher.

**GET**

- The report ID is `in[0]`, and `out` is the report buffer. Report IDs 7, 8 and 9 run at lengths
  `0, 1, need-1, need, need+1, 300`, on the five types.
- Unhandled IDs 0 to 255 run on WS3. The qualification levels are randomised for ID 9.

**SET**

- The report ID is `(u8)outlen`. The input buffer is the report, and the output buffer is never written (asserted).
- ID 6 sweeps all 256 values of byte 1. ID 9 has 40 triples.
- Unhandled IDs are reached through `outlen`. Two out-of-range values are included: 0x105 becomes 5, and 0x100
  becomes 0.
- Retrieve failures are covered: `outlen=0`, and `inlen=0`.
- ID 4 runs for every row:
  - modes 0, 3, 1, 2, 4, 16 and 255
  - device register on or off, and ctx `wsm` 0 or 1
  - all of the above for the WS1, WS3, WS8, WS9 and MT2 rows; the remaining rows get modes 0, 3 and 1, with `wsm`
    equal to the device state
  - on the WS1, WS3, WS8, WS9 and MT2 rows, modes 0 and 3 also run with these device scripts: GET failure
    (`getfail`), SET failure (`setfail`), and a 2-byte device response (`resp2`)

**Fields**

| Field | Meaning |
|---|---|
| `wsm` … `mq` | state before; `…2` gives the state after |
| `dev`, `xs`, `dev2` | device model, see above |
| `wire` | control transfers during the call |
| `calls` | `mc<n>` = `WdfMemoryCreate(PagedPool,'PTPC',n)`; `del` = `WdfObjectDelete` of that memory |

### wire.txt (259)

Lifecycle records. `seq` and `step` order the steps, and `op` names the callback:

- `DriverEntry`: success, and the `WdfDriverCreate` failure path with its TraceLogging event and cleanup
- `EvtDriverContextCleanup`
- `EvtDeviceAdd`: filter, context size, PnP callbacks, PnP capabilities, device interface GUID and both queue
  configurations (`q0`/`q1` enum values). Also the device-interface failure.
- `PrepareHardware`, `D0Entry` and `D0Exit`

Every row runs the cycle prep → D0Entry → D0Exit → D0Entry → D0Exit → prep2. `prep2` is the restart; reconfiguring
the reader fails (B16).

The device-model variants run for the WS1, WS3, WS8, WS9 and MT2 rows: `on`, `resp0`/`resp1`/`resp2`/`resp3`/`resp8`,
`getfail`, `setfail`, and the gate cases (`btn`/`wsm` combinations for the D0Entry check). The remaining rows run
`on` and `setfail` only; they share `tp_type` and `um_*` with one of those five.

Further records cover:

- pipe-selection variants: interface NULL, no pipes, bulk only, bulk then interrupt, interrupt OUT before IN, and
  control/iso/interrupt
- a `RetrieveInformation` failure, a `UsbTargetDeviceCreate` failure and an `IoTargetStart` failure
- the return value of `EvtUsbTargetPipeReadersFailed`
- `EvtIoStop`, which leaves the request untouched

| Field | Content |
|---|---|
| `prev` / `target` | `WDF_POWER_DEVICE_STATE` value |
| `qpc` | the QPC value that D0Entry reads, so `perf2` is `qpc` |
| `status` | the callback's return value |
| `nomaxpkt` | per visited pipe, whether `WdfUsbTargetPipeSetNoMaximumPacketSizeCheck` was called |
| `calls` | framework calls, e.g. `IoTargetStop(pipe0,action=1)`. `1` is `WdfIoTargetCancelSentIo`. |
| `qpcn` | number of QPC calls |

### sequences.txt (48)

Multi-step behaviors on one device. The state carries over between steps. `op` is one of `Input`, `SetFeature`,
`GetFeature`, `D0Entry` or `D0Exit`.

| Sequence | What it shows |
|---|---|
| B4 on WS8 | A mode-0 request turns mode off, and a later mode-3 request leaves it off |
| `mode-WS3`, `mode-MT2`, `mode-WS9` | The mode tracks the device; B11: a malformed packet in mouse mode forces Wellspring mode back on |
| B10 | Selective-report bit 0 cleared, then a D0 cycle; also with D0Exit's SET failing |
| `qual` | Qualification levels survive a D0 cycle |

`Input` steps use one 50-byte READ_REPORT.

- `out` holds the report. Never-written bytes read as `00`, as with zero-initialization.
- `done` is the fate of that read: `c` = completed, `q` = still queued, `l` = retrieved but never completed.

### input_<fam>.txt (15 files) and packets_t<N>.txt (5 corpora)

Each record:

1. starts a fresh device for the row's **ansi PID** (EvtDeviceAdd → PrepareHardware → D0Entry)
2. writes the record's state into the context
3. queues the READ_REPORTs through the dispatcher
4. calls the driver's continuous-reader completion (`AmtPtpEvtUsbInterruptPipeReadComplete`) with the packet as a
   `WDFMEMORY` and `NumBytesTransferred = len`

| Field | Meaning |
|---|---|
| `len` | `NumBytesTransferred` |
| `in` | packet bytes (edge cases) |
| `pkt=t<N>:<i>` | random cases: the bytes are `in=` of that line in `packets_t<N>.txt`. N = tp_type + 1, so t1 = TYPE1 … t5 = TYPE5. A given index is the same packet for every row of that type. |
| `st` | three digits `<wsm><surf><btn>` before the call |
| `perf` | `PerfCounter` |
| `qpc` | the value `QueryPerformanceCounter` returns during the call |
| `req` | output lengths of the pending READ_REPORT requests, FIFO, or `-` |
| `dev`, `xs` | present only when the call made control transfers |
| `wire` | control transfers during the call (the emergency reset of B11) |
| `dev2` | present only when the call made control transfers |
| `st2` | flags after the call (`perf` never changes in this path; asserted) |
| `qpcn` | QPC calls |
| `done` | fate of the head request: `c`, `l`, `q` or `-` (no request pending) |
| `err` | with `done=l`, the WDF status of the failed step: `0xc0000023` = RetrieveOutputMemory on a 0-byte buffer; `0xc0000206` = CopyFromBuffer into a buffer under 50 bytes. P2 completes the request with this status (B6). |
| `out` | with `done=c`, the 50 report bytes written to the request. Undefined bits are zeroed. |
| `undef` | edge records in the `-len`, `-button`, `-fingers` and `-req` groups: bit mask of the report bits that C never wrote (B13) |
| `rem` | requests still queued afterwards |

- With `done=c`, the completion is always `STATUS_SUCCESS` with Information 50 (asserted for every record).
- Requests other than the head are never touched (asserted).

**How `undef` was measured.** Every input case ran twice. Before each run, the driver's own `PtpReport` stack
bytes 1 to 48 were poisoned with `0x00` and then with `0xff`. The poison was applied inside the fake
`WdfIoQueueRetrieveNextRequest`, at the offset of `PtpReport` from the caller frame, learned once. The harness
verifies that address in every `WdfMemoryCopyFromBuffer`. Bytes 0 and 49 are assigned before that call. Bits that
differ between the two runs are undefined.

Everything else must be identical between the two runs (asserted). `out` is the `0x00` run, which equals the
zero-initialized report of the Rust port.

**Closed form of the undefined bits.** The defined bits are:

- bytes 0, 46, 47 and 49 (ReportID, ScanTime, IsButtonClicked)
- if `surf`: byte 48 (ContactCount), and for each parsed contact `i < n`, bits 0 to 1 of byte `1+9i` and bytes
  `2+9i ..= 9+9i`

Here `n = min((len-header)/fsize, 5)`. Everything else is undefined: unused contact slots, flag padding bits 2 to 7,
and, with `surf=0`, ContactCount and all contacts. This closed form was asserted for **all 5105 completed records**.

**Edge groups (full set: WS3, WS8, WS9, MT2; TYPE1 rows: lengths, mode, request and reset only):**

| Group | Cases |
|---|---|
| `-len<N>` | lengths 0, 1, h-1, h, h+1, h+f-1, h+f, h+f+1, h+2f-1, h+2f, h+5f, h+6f, h+7f, max-1, max, h+2 and h+f+2 |
| `-coord<k>` | every abs_x in {xmin-1, xmin, xmin+1, 0, -1, xmax, xmax+1, 32767, -32768} × every abs_y in {ymax+1, ymax, ymax-1, 0, -1, ymin, 32767, -32768}, five per packet |
| `-tipconf<k>` | touch_major {99, 100, 101, 0, -1, -100, 32767, -32768, 16384, 199, 200} × touch_minor {0, 1, -1, 32767, -32768, 2} |
| `-button<v>-btn<b>-surf<s>` | button byte 00/01/80/ff × both flags |
| `-wsm0-*` | mode-flag cases |
| `-fingers<N>` | 6, 7, 8 and 16 slots; slots ≥5 are extreme and ignored |
| `-req<k>` | buffer lengths 0, 49, 1, 50, 51, 64, and two or three pending |
| `-noreq` | no request pending |
| `-scan<k>` | perf/qpc pairs: 0, 99, 100, 25599, 25600, negative deltas, ±i64 extremes without overflow |
| `-hdr-*` | header 0xff or random |
| `-fing-ff` | all finger bytes 0xff |
| `-specgold*` | spec §7.6 TYPE2-4 and MT2 golden vectors |
| `-specframe` | spec §7.6 TYPE4 106-byte frame (WS9, T2) |
| `-reset-<model>-wsm<w>` | malformed length with device models on, off, resp0/1/2/3/8, get/set failures and all failures |
| MT2 extras | `-xy*` X/Y bit-field boundaries, including the INT_MIN case and bits 26 to 31 set; `-touch*` TouchMajor/TouchMinor around 0/1 and 172/173; `-id*` contact-ID nibble |

**Random records (`-r<NNNN>`):**

- About 12% of packets have a malformed length, and plausible coordinates are biased toward the thresholds.
- About 17% of records vary the state: surf/btn/wsm, missing or short buffers, two pending requests.
- QPC deltas include ±25500/25600/25700, ±100, ±99, 0 and large values.

**Coverage per family:**

| family (row) | type | edge | random | note |
|---|---|---|---|---|
| WS1 (0) | TYPE1 | 40 | 300 | TYPE1 is never parsed: valid packets stay queued; malformed ones reset |
| WS2 (1) | TYPE1 | 20 | 50 | same parameters as WS1 |
| WS3 (2) | TYPE2 | 128 | 1000 | representative TYPE2 |
| WS4 (3), WS4A (4), WS5 (5), WS5A (7) | TYPE2 | 87 each | 200 each | distinct x.min/y.max; scan/req/reset groups trimmed (code and `um_*` identical to WS3) |
| WS6 (6), WS6A (8) | TYPE2 | 70 each | 50 each | parameters identical to WS4 |
| WS7 (9), WS7A (10) | TYPE2 | 70 each | 50 each | parameters identical to WS5A |
| WS8 (11) | TYPE3 | 118 | 1000 | no mode I/O |
| WS9 (12) | TYPE4 | 129 | 1000 | |
| T2 (14) | TYPE4 | 71 | 50 | parameters identical to WS9 |
| MT2 (13) | TYPE5 | 135 | 1000 | |

The random counts for rows whose parse parameters duplicate another row were reduced to stay near the ~3 MB
budget. Those rows still get the coordinate, tip/confidence, length, button and golden edge sets, which prove the
row-to-parameter mapping.

**Record totals:**

| File | Records |
|---|---|
| `input_*` | 6669 |
| `packets_t1` | 300 |
| `packets_t2` | 1000 |
| `packets_t3` | 1000 |
| `packets_t4` | 1000 |
| `packets_t5` | 1000 |

## Excluded cases (undefined behavior in C; the Rust port fixes them per the P2 rule in docs/rust-port/README.md)

1. **Unknown idProduct in PrepareHardware (B3).** `AmtPtpGetDeviceConfig` walks past the 15-row table, which has
   no sentinel. No record exists for it, so the `DeviceNotFoundInRegistry` TraceLogging event is not captured. The
   descriptor IOCTLs for unknown PIDs are covered by context injection instead (`inj=1`).
2. **SET_FEATURE with an input buffer shorter than the bytes read.** ID 4 and ID 6 read byte 1; ID 9 reads bytes 1
   to 3. These are out-of-bounds reads (B8).
3. **GET_STRING with `OutputBufferLength == wcharCount*2 > 8`.** `WdfMemoryCopyToBuffer` then succeeds and writes
   more than 8 bytes over the 8-byte stack variable `&pStringBuffer` (B7). Equal lengths of 8 bytes or less are
   included; that write lands only on the dead pointer variable.
4. **GET_STRING for a device string of length 0.** WDF returns success with a NULL `WDFMEMORY`, and the driver
   then calls `WdfMemoryCopyToBuffer(NULL, …)`.
5. **`WdfMemoryCreate` failure in Get/SetWellspringMode.** It leads to `WdfObjectDelete(NULL)` (B5), a framework
   bugcheck or verifier break.
6. **TYPE4/5 short device responses.** These are recorded, but payload bytes 2 to 3 are heap garbage in C. They
   appear as `00` with `umask` (see `wire=`). The 2-byte allocation is always overrun by the 4-byte transfer (B1);
   all such records assume the P2 fix of a 4-byte buffer.
7. **`qpc - perf` overflowing i64.** Every fixture keeps the difference below 2^63 in magnitude.
8. **Concurrency (B22)** is not modelled. Every call is sequential.

SPI and lower HID IOCTLs do not exist in this driver.

## Spec statements found wrong when executing the code

These concern the porting spec `docs/rust-port/specs/usbum.md`. Every parse formula, golden vector, setup packet, descriptor SHA-256 and
B-bug behavior in the spec matched the execution. An independent Python re-implementation of spec §7.5, §7.6, §5.5
and §6.3 reproduces all 6669 input records, all 3970 control transfers and all 379 GET_FEATURE records exactly.
The differences are all status codes decided by the framework, not by the driver. They are judged against the
open-source WDF model, so the Rust core will not produce them itself; the framework will:

1. **§4.3, §4.5, §4.7 and B6.** An output buffer that is too small for the descriptor or report copy gives
   **`STATUS_INVALID_BUFFER_SIZE` (0xc0000206)**, not `STATUS_BUFFER_TOO_SMALL`. `WdfMemoryCopyFromBuffer` checks the
   destination in `_CopyPtrToPtr`. A 0-byte output buffer gives `STATUS_BUFFER_TOO_SMALL` (0xc0000023) earlier, from
   `RetrieveOutputMemory`. For READ_REPORT the B6 leak happens with either status.

   MS Learn's `WdfMemoryCopyFromBuffer` page lists both codes, but its wording is garbled: it assigns
   `STATUS_BUFFER_TOO_SMALL` to "destination larger than the count" and `STATUS_INVALID_BUFFER_SIZE` to "offset
   too large". The open-source implementation was followed instead. This has not been verified on a live UMDF host.
2. **§5.4 and §5.5, GET_FEATURE.** A 0-byte input buffer gives `STATUS_BUFFER_TOO_SMALL` from
   `WdfRequestRetrieveInputMemory`, not `STATUS_INVALID_BUFFER_SIZE`. The driver's own `inputBufferLength < 1` check
   is unreachable. A 0-byte output buffer likewise gives `STATUS_BUFFER_TOO_SMALL`, not the "too short"
   `STATUS_INVALID_BUFFER_SIZE`.
3. **§4.6, GET_STRING.** Input length 0 gives `STATUS_BUFFER_TOO_SMALL`. Only lengths 1 to 3 reach the driver's
   `STATUS_INVALID_BUFFER_SIZE`.

Note, not an error: spec §7.6 says that retrieving output memory before QPC has no observable effect. That holds
for HidClass. The harness still sees it in `qpcn`: when READ_REPORT has a 0-byte buffer, TYPE5 makes 0 QPC calls and
TYPE2-4 make 1.

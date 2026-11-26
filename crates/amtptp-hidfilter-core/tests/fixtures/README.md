# AmtPtpHidFilter behaviour fixtures (C oracle)

These files record what the **original C driver** `src/AmtPtpHidFilter` actually does. They were
produced by compiling and running the real C sources on the host, not by reading them. The Rust
port (`amtptp-hidfilter-core` and the `AmtPtpHidFilter` driver crate) replays them as ground truth.
The C sources are deleted after the port, so this file is the only record of how they were produced.

| File | Records | Content |
|---|---:|---|
| `descriptors.txt` | 99 | HID descriptor, report descriptor and device-attributes IOCTLs for 8 device configurations and 4 buffer sizes each; static facts and struct layouts |
| `ioctl.txt` | 519 | `FilterEvtIoIntDeviceControl` routing for 28 named or probe codes plus every `CTL_CODE(0x0B, 0..127, 0..3, 0)` |
| `feature.txt` | 1588 | `IOCTL_HID_GET_FEATURE` / `IOCTL_HID_SET_FEATURE`: all 256 report IDs both ways, every SET payload byte for 0x04 and 0x06, size and NULL edges |
| `input_bt.txt` | 1653 | the lower-read completion (input parsing) for Bluetooth MT2 (VID 0x004C, header 4): 453 edge cases and 1200 pseudo-random packets |
| `input_usb.txt` | 1661 | the same for USB MT2 (VID 0x05AC, header 12): 461 edge cases and 1200 pseudo-random packets |
| `wire.txt` | 65 | the mode-switch SET_FEATURE sent to the device, plus the start, resume, timer and PnP-restart sequences that send it, with fault injection |
| `lifecycle.txt` | 53 | WDF glue: DriverEntry, AddDevice (with faults), PrepareHardware, D0Entry/D0Exit, detour variants, the read-issue path and its faults, the work item, READ_REPORT and GET_STRING dispatch, and pipeline FIFO order |

Total size is about 2.4 MB. The harness is deterministic: two runs give byte-identical files.

## 1. Provenance

* **Sources.** `src/AmtPtpHidFilter` at commit `2edb0f2` (the sources are identical to `29d2778`).
* **Compiled unmodified.** All seven C files were compiled **unmodified**: `Driver.c`, `Device.c`, `Detour.c`, `Diagnostics.c`, `Hid.c`, `Input.c` and `Queue.c`. No function was copied or edited.
* **Headers.** They come from the project's own `include/` (so struct layouts, bit-fields and `#pragma pack` are the C compiler's), WDK 10.0.28000.0 (`km`, `km/crt`, `shared`) and KMDF **1.15** (`Include/wdf/kmdf/1.15`), the same as the real build.
* **Only substitution: WPP.** The generated `*.tmh` files were replaced by a stub that defines `TraceEvents`, `Trace`, `WPP_INIT_TRACING` and `WPP_CLEANUP` as `((void)0)`. Tracing has no functional effect. Trace arguments are therefore not evaluated, including the `PtpFilterDiagnosticsIoControlGetString` calls in `Queue.c`.
* **Compiler.** LLVM clang 23.1.6 with these flags:

  ```
  --target=x86_64-pc-windows-msvc -O0 -fwrapv -std=c17 -fms-extensions -fms-compatibility
  -D_AMD64_ -DAMD64 -D_WIN64 -D_KERNEL_MODE -DKMDF_VERSION_MAJOR=1 -DKMDF_VERSION_MINOR=15
  -DNTDDI_VERSION=0x0A000000 -D_WIN32_WINNT=0x0A00 -DWINVER=0x0A00 -DWINNT=1 -DPOOL_NX_OPTIN=0 -nostdinc
  -I src/AmtPtpHidFilter/include -I <WDK>/km -I <WDK>/km/crt -I <WDK>/shared -I <WDK>/wdf/kmdf/1.15
  ```

  * The MSVC target gives MSVC bit-field allocation and struct layout. The `layout` record in `descriptors.txt` prints every offset. They all match spec section 3 and `Hac.h`.
  * `-fwrapv` makes the one signed overflow in the parser wrap the way MSVC's `neg` instruction does: `-(INT)(tmp_y << 6)` with the value `INT_MIN`, see `ub=int_min_neg`.
  * `-O0` keeps every read of the uninitialised `PTP_REPORT` a real memory read, so stack poisoning can detect it (section 3.4).
* **Linking.** The harness was linked with `lld-link` against the VS 18 MSVC 14.52.37025 `libcmt` and the UCRT, as a host x64 console program.
* **Harness.** Not committed; it lives in the session scratch directory. It supplies the following:
  * **A fake KMDF 1.15 function table.** `WdfFunctions_01015` has 444 slots, plus `WdfDriverGlobals`. Every slot that is not implemented traps and aborts the run, so no framework call can go unrecorded.
  * **Stubs** for the 39 KMDF functions the driver uses. Every call that creates, configures, sends, completes, queues, deletes or fails something is recorded as an event (`ev`), with its arguments.
    Pure accessors are not recorded: `WdfObjectGetTypedContextWorker`, `WdfTimerGetParentObject`, `WdfWorkItemGetParentObject`, `WdfRequestGetInformation`, `WdfMemoryGetBuffer`, `WdfRequestGetParameters`, and `WdfRequestWdmGetIrp` unless it returns NULL.
  * **The three kernel imports.** `KeQueryPerformanceCounter`, `IoGetLowerDeviceObject` and `ObfDereferenceObject` are supplied as `__imp_` pointers.
  * **A fake WDM world for the detour:**
    * a lower `DEVICE_OBJECT`/`DRIVER_OBJECT` whose `MajorFunction[i]` holds a sentinel meaning "HIDCLASS dispatch *i*";
    * a `DRIVER_EXTENSION_EXT` whose `IoClientExtension` points to an `IO_CLIENT_EXTENSION` with the client ID "HIDCLASS";
    * immediately after it, a `HIDCLASS_DRIVER_EXTENSION` whose `MajorFunction[i]` holds the sentinel "minidriver *i*".
* **Sanity checks.** The harness also cross-checks the spec (`specs/hidfilter.md`) but never derives a fixture value from it; see section 6.

## 2. Line format (all files)

* ASCII text with LF line endings. Lines that start with `#` are comments. Every other line is one record.
* A record is a set of tokens separated by single spaces. Each token is `key=value`.
  * **Split each token at the first `=`.** Values such as `ev` contain more `=` signs.
  * Values never contain whitespace. Keys are unique within a record.
  * The first token is always `case=<id>`, and the ID is unique within its file.
* **Byte strings** are lowercase hex with no separators. `-` means zero bytes.
* **Numbers:**
  * A value with `0x` is hex; any other number is decimal.
  * NTSTATUS values are `0x%08x`.
  * `last_in`, `last_out`, `freq`, `xmin` and `ymin` are signed decimal, matching their C types.
* `-` means "not applicable" (for example `status=-` when nothing was completed).
* `ev` is the `|`-separated, ordered list of the framework and kernel calls the C code made during the operation (every recorded call; the pure accessors listed in section 1 are omitted). Each entry has the form `Name(arg=value,...)` or `Name(...)=result`. Object names used in events:
  * `device`, `driver`;
  * `target`: the default local I/O target, which is the PDO;
  * `timer`, `workitem`, `lookaside`;
  * `queue_default`, `queue_read`: `HidReadQueue`;
  * `upperN`: the N-th upper request created in the scenario, counting from 1;
  * `upper_mem`: the WDFMEMORY returned for an upper request's output buffer;
  * `lower_read`, `lower_mem`: the lower READ_REPORT request and its lookaside buffer;
  * `cfg_req`, `cfg_mem`: the mode-switch request and its preallocated memory.
* Timer due times are in 100 ns units and negative, because they are relative: `-20000000` is 2 s and `-30000000` is 3 s.

### 2.1 Compound device state (`st_in` / `st_out`)

The value is a comma-separated list of `name:value` pairs:

```
vid:0x004c,pid:0x0265,ver:0x0100,hdr:4,fsz:9,fdelta:0,bdelta:1,x:250/-3678/3934,y:250/-2479/2586,cfg:1,detour:1,target:1,inp_on:0,touch:0,btn:0,last:10000000
```

| name | `DEVICE_CONTEXT` member (Device.h) |
|---|---|
| `vid` / `pid` / `ver` | `VendorID` / `ProductID` / `VersionNumber` |
| `hdr` / `fsz` / `fdelta` / `bdelta` | `InputHeaderSize` / `InputFingerSize` / `InputFingerDelta` / `InputButtonDelta` |
| `x` / `y` | `X` / `Y` as `snratio/min/max` (`BCM5974_PARAM`) |
| `cfg` | `DeviceConfigured` |
| `detour` | `IsHidIoDetourCompleted` |
| `target` | `HidIoTarget != NULL` (it is always the default I/O target when set) |
| `inp_on` / `touch` / `btn` | `PtpInputOn` / `PtpReportTouch` / `PtpReportButton` |
| `last` | `LastReportTime.QuadPart` |

The input files use flat keys for the members the completion routine reads (section 3.1).

### 2.2 Upper requests

Every upper request presented to `FilterEvtIoIntDeviceControl` starts with `Irp->IoStatus.Information = 23130` (0x5a5a, shown as `info_in=23130`). When `info` is still `23130` after completion, the C code did not set Information. This is spec B21, and it applies to GET/SET_FEATURE and to every error path.

## 3. `input_bt.txt`, `input_usb.txt`: input parsing

Each record runs **`PtpFilterInputRequestCompletionCallback`** (Input.c) on one completed lower `IOCTL_HID_READ_REPORT`. The scenario has these steps:

1. Start the device through the real `DriverEntry`, `PtpFilterEvtDeviceAdd`, `PrepareHardware`, `D0Entry` and `SelfManagedIoInit`. The lower PDO answers the attributes query with 004C:0265 ver 0x0100 for BT, or 05AC:0265 ver 0x0855 for USB.
2. Set the state fields below.
3. Park at most one upper READ_REPORT in `HidReadQueue`.
4. Issue one lower read with the real `PtpFilterInputIssueTransportRequest`.
5. Complete that lower read with `in` and `rinfo`, which calls the C completion routine.

### 3.1 Fields

| key | meaning |
|---|---|
| `fam` | `BT` or `USB` |
| `kind` | `edge` (hand-picked) or `rand` (pseudo-random) |
| `vid` | `DEVICE_CONTEXT.VendorID` when the completion runs. The device's real VID, except in `vid-*` edge cases and about 2% of random cases, where it is injected to exercise the VID check. |
| `hdr`, `fsz`, `fdelta`, `bdelta` | `InputHeaderSize`, `InputFingerSize`, `InputFingerDelta`, `InputButtonDelta`, exactly as set by the real `PtpFilterConfigureMultiTouch` (BT 4/9/0/1, USB 12/9/0/1). They are never varied. |
| `xmin`, `ymin` | `X.min`, `Y.min` (-3678 and -2479, set by ConfigureMultiTouch). These are the only members of X and Y that the completion reads. |
| `last_in` | `LastReportTime.QuadPart` before the call |
| `freq` | the value `KeQueryPerformanceCounter(&currentTSC)` writes through its `PerformanceFrequency` pointer. The C code discards the return value (the counter); see spec 7.5. |
| `rinfo` | the lower request's `IoStatus.Information` (ULONG_PTR, hex), as returned by `WdfRequestGetInformation`. C uses `(size_t)(LONG)rinfo` (B29), so values with bit 31 set or above 2^32 are meaningful test inputs. |
| `upper` | `OutputBufferLength` of the single upper READ_REPORT parked in `HidReadQueue`, or `none` if the queue is empty. Its buffer is prefilled with `0xEE` and its Information preset to 23130. |
| `in` | the bytes at the start of the 1024-byte lookaside read buffer. **The C code reads nothing beyond `in`.** Each case was run with the rest of the buffer set to 0x00 and again to 0xFF, and the results were identical. `len(in)` normally equals `rinfo`. When `rinfo` is huge or truncated, `in` holds as many bytes as the C code reads. |
| `act` | which path the completion took (table below) |
| `status`, `info` | the completion status and Information of the upper request (`-` if it was not completed) |
| `out` | the 50 bytes the C code copied into the upper request (the `PTP_REPORT`), with **uninitialised bits set to 0**. A Rust port that zero-initialises the report (port policy P2, spec B4) must produce exactly `out`. If `upper > 50`, bytes 50 and later of the upper buffer stayed `0xEE` (verified). |
| `undef` | a 50-byte bit mask of bits the C code left **uninitialised**: stack garbage in the real driver, leaked to HIDCLASS. They are always contacts with index ≥ ContactCount (all 9 bytes) and bits 2..7 of each written contact flag byte (`0xfc`). Compare with `(rust ^ out) & !undef == 0`, or, for a zero-initialising port, `rust == out`. |
| `last_out` | `LastReportTime.QuadPart` after the call. It equals `freq` on every path that reaches `KeQueryPerformanceCounter` (`complete`, `outmem_fail`, `copy_fail`); otherwise it is unchanged. |
| `ub` | present only as `ub=int_min_neg`: some parsed finger has `(u32 & 0x03FFFFFF) == 0x02000000`, so `-(INT)(tmp_y << 6)` negates `INT_MIN`. That is UB in ISO C. MSVC and this harness (`-fwrapv`) wrap, giving y = -4096 and then 0 after the offset (spec D.1 row 6). Port with `wrapping_neg` (port policy P2). |
| `ev` | present on `kind=edge` records only (section 2) |

Every other `DEVICE_CONTEXT` member is **unchanged** by the completion; this was verified for every record. The completion never reads `DeviceConfigured`, `PtpInputOn`, `PtpReportTouch` or `PtpReportButton`.

### 3.2 `act` values and their exact framework sequences

The sequences were asserted for every record. `rand` records omit `ev` because the `act` code determines it exactly.

| act | condition in C | exact `ev` |
|---|---|---|
| `complete` | report built and delivered | `WdfIoQueueRetrieveNextRequest(queue=queue_read)=0x00000000:upperN` `KeQueryPerformanceCounter(freq_ptr=nonnull)` `WdfRequestRetrieveOutputMemory(req=upperN)=0x00000000` `WdfMemoryCopyFromBuffer(mem=upper_mem,off=0,len=50)=0x00000000` `WdfRequestSetInformation(req=upperN,info=50)` `WdfRequestComplete(req=upperN,status=0x00000000,info=50)` `WdfObjectDelete(obj=lower_read)` `WdfObjectDelete(obj=lower_mem)` |
| `workitem` | `(size_t)(LONG)rinfo == 0` | `WdfWorkItemEnqueue(item=workitem)` then the two deletes |
| `timer3s` | length `< hdr`, or `(len-hdr) % fsz != 0` | `WdfTimerStart(timer=timer,due=-30000000)` then the two deletes |
| `noupper` | `HidReadQueue` empty | `WdfIoQueueRetrieveNextRequest(queue=queue_read)=0x8000001a` then the two deletes (the packet is dropped) |
| `setfailed_norestart` | `VendorID` not 0x05AC or 0x004C. This is checked **first**, before the length checks. | `WdfDeviceSetFailed(dev=device,action=no_restart)` then the two deletes |
| `outmem_fail` | `WdfRequestRetrieveOutputMemory` fails (stub model: `OutputBufferLength == 0`) | RetrieveNext OK, KeQPC, RetrieveOutputMemory `=0xc0000023`, `WdfDeviceSetFailed(...attempt_restart)`, the two deletes. The upper request is **never completed** (B9). |
| `copy_fail` | `WdfMemoryCopyFromBuffer` fails (stub model: `OutputBufferLength < 50`) | as `outmem_fail`, but the failing call is CopyFromBuffer. The upper request is never completed (B9). |

The two deletes are always `lower_read`, then `lower_mem`. C reads `RequestMemory` from the request context *after* deleting the request (B10). The harness keeps the memory valid, so these fixtures show the intended order.

### 3.3 Coverage

The edge cases are the same for both families. Case IDs are prefixed with `BT-` or `USB-`.

* **`d1-1` … `d1-10`.** Spec D.1 single-finger vectors, plus a `-ignored-bits` variant of each. The variant sets bits 26-31, bytes 6 and 7, and the high nibble of byte 8, and must give the same output.
* **`d2-six-fingers-button`.** Spec D.2: six fingers are truncated to five, and the button is set.
* **`fingers-0` … `fingers-8`.** 0 to 8 finger records; more than five are truncated.
* **`xy-x*-y*`.** 11 × 17 X13/Y13 values: -4096, -4095, around the clamp at -3678 and -2479, -1, 0, 1, around the descriptor maxima, and 4095. The X field bits are non-zero.
* **`y-xbits0-*`.** Every Y13 with X13 = 0 (exact y), with the top 6 bits 0 or 63. This set includes `INT_MIN`.
* **`tipconf-maj*-min*`.** TouchMajor in {0, 1, 0x7f, 0x80, 0xff} × TouchMinor in {0, 1, 0x7f, 0x80, 171, 172, 173, 174, 0xff}. This covers every branch of the tip formula (`f[4] != 0`) and the confidence formula (`f[5] <= 172`).
* **`id-byte8-*`.** ContactID is the low nibble of byte 8.
* **`button-*-n0|n1`.** `buf[1]` bit 0 sets the button, with 0 and 1 fingers.
* **`header-byte0-*`.** Wrong or right report-type bytes. They are ignored, because there is no report-ID check (B8).
* **`len-0` … `len-(hdr+64)`.** Every length up to seven fingers plus 1 (spec D.3).
* **`rinfo-*`.** Information values with bit 31 set, above 2^32, 0x7fffffff, and 1024 and above. Some of these parse, for example BT `0x80000008` and `0xfffffffd`, and USB `0x80000007` and `0xfffffffc` (B29).
* **`scantime-0` … `scantime-22`.** `last_in` and `freq` combinations:
  * the steady state 0;
  * never stamped: 0xff, 254, 25, 0;
  * negative deltas, giving `(USHORT)` truncation, 0xffff and 0xfeff;
  * other frequencies.
* **`no-parked-upper`, `upper-outlen{0,49,50,64}`.**
* **`vid-{0000,1234,05ad,004d,8910}`** and **`vid-1234-len0`.** These show the VID check runs before the length check. VID 0000 is the state after a PnP stop/start (B30).

**Random cases (`rand-0000` … `rand-1199`).**

* **PRNG.** splitmix64 with seed `0x48494446494C5452 ^ VID`. The packets are stored in full, so a test does not need to regenerate them.
* **Length.**
  * 85%: a valid length with 0 to 8 fingers.
  * 8%: an invalid length.
  * 3%: `len + k·2^32`.
  * 3%: bit 31 set, sometimes with high bits too.
  * 1%: `rinfo = 0`.
* **Header byte 0** is plausible (0x31 for BT, 0x02 for USB) 90% of the time.
* **Fingers.** A mix of fully random values, edge X/Y values, X bits forced to 0, TouchMajor = 0 (20%) and TouchMinor in 168..177 (33%).
* **State.**
  * `freq` is one of 10e6, 19.2e6, 24e6, 3579545, 25500 and 25499.
  * `last_in` is `freq` 70% of the time, 0 15% of the time, `freq ± 1e6` 10%, and a random 64-bit value 5% (with no overflow in `freq - last`).
* **Upper request and VID.** 2% have no parked upper request, and 2% have an injected non-Apple VID.

The `act` counts are:

| family | complete | timer3s | setfailed_norestart | noupper | workitem | outmem_fail | copy_fail |
|---|---:|---:|---:|---:|---:|---:|---:|
| BT | 1342 | 239 | 30 | 26 | 14 | 1 | 1 |
| USB | 1384 | 219 | 24 | 23 | 9 | 1 | 1 |

### 3.4 How the uninitialised bytes were measured

Each case ran three times:

| Run | Stack below the completion routine's caller | Rest of the read buffer |
|---|---|---|
| A | filled with 0x00 | 0x00 |
| B | filled with 0xFF | 0x00 |
| C | filled with 0x00 | 0xFF |

* `out` is the result of run A.
* `undef = A xor B`. It was asserted to equal exactly the expected uninitialised set described in section 3.1.
* A and C were asserted identical in every observable.
* Events and state were asserted identical across all three runs.

## 4. Other files

### 4.1 `descriptors.txt`

* **`case=facts`**: the C compiler's struct sizes, `PTP_LIST_POOL_TAG`, `REPORT_BUFFER_SIZE`, `HID_XFER_PACKET_SIZE`, and every IOCTL code value from the WDK headers.
* **`case=hqa_blob_reference`**: `DEFAULT_PTP_HQA_BLOB` expanded properly as an array initialiser (256 bytes). This is **not** what GET_FEATURE 0x08 returns (B1); the reference is there for a port that chooses to fix B1.
* **`case=layout`**: `FIELD_OFFSET` of every member of `DEVICE_CONTEXT`, `WORKER_REQUEST_CONTEXT`, `QUEUE_CONTEXT`, `HID_XFER_PACKET`, `HID_DEVICE_ATTRIBUTES`, `PTP_REPORT`, `PTP_CONTACT` and the `Hac.h` structs, plus more sizes.
* **`desc-<cfg>-<what>-len<N>`**: one IOCTL sent through the real dispatcher.
  * **Configurations** (`attr_vid`, `attr_pid` and `attr_ver` are what the PDO answered `IOCTL_HID_GET_DEVICE_ATTRIBUTES` with during `SelfManagedIoInit`):
    * `bt` (004C:0265) and `usb` (05AC:0265:0855);
    * `bt_pid0266`, `nonapple_pid0265` (1234:0265), `bt_pid0000` and `bt_pidffff`;
    * `bt_after_prepare_hw`: PrepareHardware re-zeroed the IDs (B30);
    * `added_not_started`: `SelfManagedIoInit` never ran.
  * `<what>`: `hid_descriptor` (0x000B0003), `report_descriptor` (0x000B0007) or `attributes` (0x000B0027).
  * **Fields:**
    * `outlen` is the OutputBufferLength.
    * `out` is the **whole** caller buffer afterwards. It was prefilled with `0xCC`, which shows exactly which bytes C wrote; for example, `Reserved` in the attributes stays `0xCC` (B22).
    * `status`, `info` and `ev` are as in section 2.
  * **Results:**
    * The descriptors are served whenever `ProductID == 0x0265`, regardless of VID. `nonapple_pid0265` gets them even though the mode switch was rejected.
    * Attributes always report VID 0x8910, the real PID, and version 0x0100.

### 4.2 `ioctl.txt`

One record per IOCTL code on a started BT device. The fields are:

| key | meaning |
|---|---|
| `code` | the IOCTL code |
| `name` | the WDK name, for known codes only |
| `outlen`, `inlen` | the buffer lengths sent |
| `disp` | `completed`, `queued` (parked in `HidReadQueue`) or `forwarded` (sent to `target` with send-and-forget) |
| `status`, `info` | the completion status and Information |
| `out_changed` | whether any byte of the 1024-byte UserBuffer (prefilled `0xCC`) changed |
| `ev` | named and probe codes only |

The inputs per code are:

* GET_FEATURE: a packet with rid 7 and `rblen` 3;
* SET_FEATURE: a packet with rid 4 and payload `04 03`;
* READ_REPORT: outlen 50;
* GET_STRING: outlen 128, with Type3InputBuffer pointing to ULONG 0x0409000E;
* GET_REPORT_DESCRIPTOR: outlen 1024;
* anything else: outlen 64.

Every code other than the 7 handled ones completes with `0xc00000bb` and Information unchanged.

### 4.3 `feature.txt`

| key | meaning |
|---|---|
| `op` | `get` or `set` |
| `ioctl` | 0x000B0192 or 0x000B0191 |
| `outlen`, `inlen` | the `WDF_REQUEST_PARAMETERS` DeviceIoControl lengths. GET checks `outlen` and SET checks `inlen`, against 16. |
| `ub` | `1` if `Irp->UserBuffer` points to a `HID_XFER_PACKET`, `0` if it is NULL |
| `rid` | `HID_XFER_PACKET.reportId`. This is the value that is switched on; `reportBuffer[0]` is never consulted. |
| `rblen` | `reportBufferLen` |
| `buf_in`, `buf_out` | the physical bytes at `reportBuffer` before and after. The length is the allocated size, which is at least `rblen`, except where `oob=1`. |
| `inp_on_in/out`, `touch_in/out`, `btn_in/out` | `PtpInputOn`, `PtpReportTouch`, `PtpReportButton` before and after |
| `status`, `info` | `info` is always 23130 (B21) |
| `oob=1` | SET with `rblen < 2`. C reads `reportBuffer[1]` anyway (B23). The physical buffer held the 2 bytes shown, so the record shows what C did; port policy P2 lets a port treat such a read as 0, so a test may skip these records. |

For every feature record, the only framework call was the completion. Feature requests never produce hardware traffic.

Coverage:

* `get-rid00`…`get-ridff` (rblen 8, buffer `a0..a7`);
* `get-caps-rblen*` and `get-hqa-rblen*` size edges;
* `get-*-hidclass-style`;
* `get-outlen*` and `get-userbuffer-null`;
* `set-rid00`…`set-ridff` (payload `[rid,00]`, state 1/0/1);
* `set-mode-XX-from{0,1}` and `set-funcswitch-XX-from{0,1}` for every XX;
* `set-inlen*` and `set-userbuffer-null`;
* `set-*-oob`;
* `set-rid04-buf0-06` / `set-rid06-buf0-04` (the packet's `reportId` wins over `buffer[0]`);
* the `d5-*` spec D.5 rows.

### 4.4 `wire.txt`: bytes sent to the hardware

This driver sends **no USB control transfers and no SPI requests**. Every request to the hardware is an internal HID IOCTL to the PDO, serviced by HidBth or HidUsb:

* `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (0x000B0027), with a 32-byte output buffer, during `SelfManagedIoInit`;
* the mode switch, `IOCTL_HID_SET_FEATURE` (0x000B0191), with a `HID_XFER_PACKET` in `Irp->UserBuffer`;
* `IOCTL_HID_READ_REPORT` (0x000B000B), with a 1024-byte output buffer, as seen in the `ev` of the input and lifecycle records.

The mode-switch fields are:

| key | meaning |
|---|---|
| `sent` | the number of mode-switch sends in the operation |
| `send_ioctl` | the IOCTL code of the send |
| `send_in_len` | size of the WDFMEMORY formatted as the input buffer (32) |
| `send_in_is_userbuffer` | `1`: the same buffer was also put in `Irp->UserBuffer` |
| `send_flags` | `WDF_REQUEST_SEND_OPTIONS.Flags`; 0x2 is SYNCHRONOUS, with no timeout |
| `send_timeout` | the send timeout from the same options (0 here) |
| `xfer_id` / `xfer_len` | `reportId` / `reportBufferLen` |
| `rb_off` | `reportBuffer - packet`, which is 16 |
| `payload` | the `xfer_len` bytes at `reportBuffer`: `f10201` for BT, `02010000` for USB |
| `raw32` | the whole 32-byte stack packet. Bytes 0..7 are the self-referential `reportBuffer` pointer and are **zeroed in the fixture**. Bytes 13..15 are padding that `RtlZeroMemory` cleared. |

Records:

* **`start-<cfg>`** runs `SelfManagedIoInit`. **`resume-<cfg>`** runs D0Exit, then D0Entry, then `SelfManagedIoRestart`. **`timer-<cfg>`** runs `PtpFilterRecoveryTimerCallback`, fired explicitly. **`pnp-restart-<cfg>`** runs D0Exit, then PrepareHardware, then D0Entry, then `SelfManagedIoRestart` (B30).
  * The configurations are `bt`, `usb`, `nonapple` and `bt_pid0266`.
  * `lower_reads_pending` is cumulative within one configuration's sequence.
* **`restart-without-detour`** (B11): returns `0xc000a003`, yet `cfg` becomes 1 and `last` is stamped.
* **`{start,restart,timer}-{bt,usb}-<fault>`**: a failing mode switch is masked as SUCCESS and arms the 2 s timer. The timer still issues a read (B12). With `send_true_status_fail`, the send returns TRUE with a failing request status, and C reports success (B13).
* **`cfg-VVVV-PPPP`** and **`cfg-<vid>-<fault>`**: `PtpFilterConfigureMultiTouch` called directly with injected `VendorID` and `ProductID`.
  * Before each call, the size, X and Y fields are set to the sentinel 77. `st_out` therefore shows which fields C writes: all of them, before the send, even when the send fails. When the VID or PID is rejected, it writes none of them.

### 4.5 `lifecycle.txt`

| key | meaning |
|---|---|
| `op` | the entry point called |
| `ret` | its NTSTATUS, or `-` for VOID |
| `st_in`, `st_out`, `ev` | as in section 2 |

Some records carry extra fields:

| key | meaning |
|---|---|
| `patched` | the slots of the lower `DRIVER_OBJECT.MajorFunction[]` that changed. `slot15=mini15@cli+0xa8` means `MajorFunction[15]` (`IRP_MJ_INTERNAL_DEVICE_CONTROL`) now holds the value read from `HIDCLASS_DRIVER_EXTENSION.MajorFunction[15]`, at `IO_CLIENT_EXTENSION + 0xA8`. `none` means nothing changed. |
| `lower_refs` | references taken by `IoGetLowerDeviceObject` minus `ObfDereferenceObject` calls; it is always 0, which means balanced |
| `client_id` | the 16 bytes at `ClientIdentificationAddress` |
| `readq_depth`, `lower_reads_pending` | queue counts after the operation |
| `upperN=completed:<status>:<info>` | the outcome of each upper request |

* **AddDevice faults.** The device, interface or queue failing returns an error. Lookaside, timer or work-item creation failing returns **SUCCESS** (B16). A NULL WDM device object returns SUCCESS and creates nothing more (B17).
* **Detour variants:**
  * NULL own driver object, lower device, lower driver object, driver extension or client extension;
  * client IDs `hidclass`, `HIDCLAS`, `HIDCLASSX` (no NUL in byte 9, so it is rejected), empty and `XIDCLASS`;
  * `HIDCLASS\0junk` (accepted);
  * a second detour (idempotent).

## 5. Stub model: what comes from the harness, not from C

Fields not listed here come from executing the C code.

* **`WdfRequestRetrieveOutputMemory`**: fails with `0xc0000023` (STATUS_BUFFER_TOO_SMALL) if OutputBufferLength is 0. Otherwise it returns `Irp->UserBuffer` with size OutputBufferLength.
* **`WdfRequestRetrieveOutputBuffer`**: fails with `0xc0000023` if the length is 0 or below the minimum.
* **`WdfMemoryCopyFromBuffer`**: fails with `0xc0000023` if `offset + length > size`.

  These three only matter for `outlen` 0 or short in `descriptors.txt`, and for `upper-outlen{0,49}` and `outmem_fail`/`copy_fail` in the input files. Those cases document how C reacts to such a failure; the status itself is KMDF's.
* **`WdfIoTargetSendInternalIoctlSynchronously(GET_DEVICE_ATTRIBUTES)`**: fills `Size=32` and VID, PID and version from `attr_*`, or returns an injected status.
* **`WdfRequestSend`**: returns the injected value. For the synchronous mode switch, the request's status is consulted only by C's `WdfRequestGetStatus` on the FALSE path.
* **`WdfTimerStart` and `WdfWorkItemEnqueue`** are only recorded. The timer and the work item run only when a record calls their callback explicitly.
* **`WdfIoQueueRetrieveNextRequest`**: FIFO. It returns `0x8000001a` (STATUS_NO_MORE_ENTRIES) when the queue is empty.
* **`KeQueryPerformanceCounter(p)`**: writes `freq` through `p`, which is 10000000 unless the record says otherwise. It returns a counter (123456789, plus 1000 per call), and C never uses that return value.
* **`WdfObjectDelete`**: marks the object deleted but never frees it (see B10).
* **Fault statuses** are arbitrary but realistic:

  | fault | status |
  |---|---|
  | resource failures | `0xc000009a` |
  | format failures, forward failures, GET_STRING send failures | `0xc0000184` |
  | the attributes IOCTL and a FALSE mode-switch send | `0xc000009d` |
  | timer or work-item creation | `0xc0200225` (STATUS_WDF_INCOMPATIBLE_EXECUTION_LEVEL) |
  | the request status after a TRUE send | `0xc0000001` |

* **Single-threaded execution.** The data races in the C code (spec 3.1) are not exercised.

## 6. Excluded or flagged undefined behaviour

| # | Case | Treatment |
|---|---|---|
| 1 | `% InputFingerSize` with `InputFingerSize == 0` (B18) | **Excluded.** This is reachable in C when the VID is Apple but the PID is not 0x0265: ConfigureMultiTouch rejects the PID before it sets the sizes, but the timer still issues a read (see `wire.txt timer-bt_pid0266`, `lower_reads_pending=1`). Completing that read with a length > 0 divides by zero and bugchecks. port policy P2: guard it. |
| 2 | Uninitialised `PTP_REPORT` bytes (B4) | Not excluded. They are represented by the `undef` mask, and `out` holds them as 0. |
| 3 | `INT_MIN` negation in the Y formula | Included and flagged `ub=int_min_neg`. MSVC wraps, and the harness uses `-fwrapv`. |
| 4 | SET_FEATURE reading `reportBuffer[1]` when `reportBufferLen < 2` (B23) | Included and flagged `oob=1`. A test may skip these records. |
| 5 | GET_FEATURE where `reportBufferLen` is larger than the real allocation: C trusts the length and would write past the buffer | Excluded. Every fixture buffer is at least `reportBufferLen` bytes. |
| 6 | A `HID_XFER_PACKET` with an invalid `reportBuffer` while `UserBuffer` is non-NULL | Excluded; C dereferences it. |
| 7 | Any send to `HidIoTarget == NULL` (GET_STRING or READ_REPORT before the detour, B25) | Excluded. The harness aborts on a NULL handle. |
| 8 | Reading `RequestMemory` from the context of a deleted request (B10) | The stub keeps the memory alive. The fixtures show the intended deletion order. |
| 9 | Using a NULL lookaside, timer or work-item handle after a failed creation (B16), or a NULL `HidReadQueue` after AddDevice with a NULL WDM device object | Not executed. Those AddDevice records stop at AddDevice. |
| 10 | Unaligned `USHORT*` and `unsigned int*` loads in the parser | Benign on x64. The fixtures equal explicit little-endian decoding. |
| 11 | Out-of-bounds input reads | **None exist** for the parse path: each record was rerun with the buffer tail changed, with identical results. |

## 7. Spec cross-check (`specs/hidfilter.md`, including its Verification addendum)

The harness compared the executed C behaviour with these spec items:

* A.1: all 493 bytes, parsed from the spec file itself;
* A.4/D.6: the HID descriptor, attributes and mode-switch packets;
* D.1: all 10 rows; D.2; D.3 (every length class); the D.4 driver-level rows; D.5: all rows;
* section 3: every offset and the size of 152;
* the `Hac.h` offsets (0x28, +16, +32, slot +0xA8, size 304);
* the HID_XFER_PACKET layout and every IOCTL value;
* the section 7.3 Rust reference model, run against all 3314 input records, ignoring `undef` bits.

All 114 golden checks passed, and the section 7.3 model had **0 mismatches**. **No spec statement was found to be wrong.**

The spec's descriptions of the following were also confirmed by execution:

* B11, B12, B13 and B30;
* the order of checks in the completion routine: the VID check before the length checks;
* the detour's NULL and ID checks and its reference balance;
* the AddDevice error-status handling (B16, B17);
* the D0Exit drain;
* send-and-forget forwarding of GET_STRING.

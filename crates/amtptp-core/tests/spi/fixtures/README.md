# AmtPtpDeviceSpiKm behavior fixtures (C oracle)

These files record what the **real C driver** `src/AmtPtpDeviceSpiKm` does. They were produced by compiling and
running its sources, not by reading them. The Rust port (`amtptp_core::spi` and the SpiKm driver crate) replays them
as ground truth. Do not edit them by hand.

## Provenance

- C sources: `src/AmtPtpDeviceSpiKm/{Driver,Device,Queue,Hid,Input}.c` and all headers, at commit `2edb0f2`.
  The directory was last changed in `8e0cc7c` and the working tree had no changes.
- **All five `.c` files were compiled unmodified.** No function was copied or edited. The only replaced inputs
  are the WPP-generated `*.tmh` headers, which do not exist without the WPP preprocessor. The stub `.tmh` define
  `TraceEvents(...)`, `Trace(...)`, `WPP_INIT_TRACING` and `WPP_CLEANUP` as `((void)0)`. Trace-only helpers such as
  `DbgIoControlGetString` and `DbgDevicePowerString` are therefore never called. `KdPrintEx` is a no-op because
  `DBG` is not defined, as in the Release config.
- Compiler: clang 23.1.6, `--target=x86_64-pc-windows-msvc`. This gives the MSVC ABI, so struct layout, bitfields
  and `#pragma pack` are MSVC's.
- Headers: WDK 10.0.28000.0 `km/crt`, `km` and `shared` (`ntddk.h`, `wdm.h`, `hidport.h`, `hidclass.h`), plus the
  KMDF 1.23 headers.
- Driver flags: `-c -O0 -w -Wno-error=incompatible-pointer-types -Wno-error=int-conversion -D_WIN64 -D_AMD64_
  -DAMD64 -D_WIN32_WINNT=0x0A00 -DWINVER=0x0A00 -DWINNT=1 -DNTDDI_VERSION=0xA000004 -DDEPRECATE_DDK_FUNCTIONS=1
  -DMSC_NOOPT -DKMDF_VERSION_MAJOR=1 -DKMDF_VERSION_MINOR=23 -D_KERNEL_MODE=1`. The `-Wno-error` flags are needed
  because `Hid.c:326` passes `PHID_DEVICE_ATTRIBUTES*` as `PVOID*`, which MSVC only warns about.
- Linking: the code was linked with lld against the MSVC 14.52 / UCRT user-mode libraries into a host test
  program (the harness).
- The harness runs the driver through its real entry points:
  - `DriverEntry`, then the captured `EvtDriverDeviceAdd`;
  - the captured PnP/power callbacks, the default queue's `EvtIoInternalDeviceControl` and the timer callback;
  - the completion routine that the driver registered on its own lower requests.

  Every WDF call goes through `WdfFunctions_01023[...]`. The harness fills that table with recording stubs, and any
  slot the driver should not use traps. `KeQueryPerformanceCounter` is reached through its `__imp_` pointer.
- The harness sources (`harness.c`, `scenarios.inc`, `io.c`, `build.sh`) live only in the session scratch
  directory. They are not committed.
- Output is deterministic: two runs gave byte-identical files.

## Line format

- One record per line, with LF line endings. Fields are separated by single spaces. Each field is `key=value`;
  split it at the first `=`. Values never contain spaces.
- Byte strings are lowercase hex with no separators. `-` means empty, none or not applicable.
- `status`, `lowst`, `lower_st` and `code` are 8 hex digits of the 32-bit value (e.g. `c000009c`). `pid` is 4 hex
  digits.
- `len`, `info`, `lrt_*` and `qpcf` are decimal. `len` is the full 64-bit `ULONG_PTR`; `lrt_*` and `qpcf` are
  signed i64.
- **Prefill rule.** Every buffer owned by the caller is filled before the call with `PRE(i) = (0xA0 + i) mod 256`.
  This covers upstream output buffers, feature report buffers and the `GET_DEVICE_ATTRIBUTES` output. Any byte
  still equal to `PRE(i)` afterwards was not written, as far as the fixture can tell. The rule is what makes "HQA
  writes only byte 1" and "`Reserved[]` is left untouched" visible.
- `completed`:
  - `1`: the driver called `WdfRequestComplete`;
  - `queued`: the request is waiting in `HidQueue`;
  - `fwd`: it was sent to the lower target with send-and-forget, so the driver no longer owns it;
  - `0`: none of these.
- `status` is the completion status, or `-` if the request was not completed.
- `info` is the request's Information at completion. It starts at 0 and only `WdfRequestSetInformation` changes it.
- `infoset` is the number of `WdfRequestSetInformation` calls. GET/SET_FEATURE never call it, so `infoset=0` there.
- `wdf` is a comma list of `WdfFunction:status` entries, one for each WDF stub that returned a failure during the
  record. `WdfRequestSend:false` means the send returned FALSE. `-` means none failed.
  - A status listed in `wdf` was **produced by the stub's model of KMDF** (see below), not by driver code. The
    driver only propagated it.
  - Statuses not listed there come from the driver itself, e.g. `c0000225`, `c0000206`, `c00000bb`, `c000009c`,
    `c0000010`, `c0000120`.
- `ops` is the ordered log of the WDF and kernel calls made by driver code during the record, separated by `;`.
  - Each entry is `Name(args)=result`. Pure getters are not logged: `GetInformation`, `GetParameters`,
    `WdmGetIrp`, `MemoryGetBuffer`, `RetrieveOutput*`, `MemoryCopyFromBuffer`, `IoQueueGetDevice`,
    `DeviceGetDriver` and `TimerGetParentObject`.
  - Object labels:
    - `u<n>`: upstream requests created by the harness;
    - `l<n>`: lower requests the driver creates itself;
    - `m<n>`: lookaside memory objects;
    - `dev`: the device;
    - `lower`: the local I/O target, i.e. the Apple PDO;
    - `hidq` / `defq`: the manual `HidQueue` and the default queue;
    - also `timer`, `lookaside`, `key`, `devinit` and `driver`.
  - `KeQueryPerformanceCounter(pfreq=nonnull)` shows the call passed a non-NULL *PerformanceFrequency*
    out-parameter (B8).
- WDF enum values in `ops`:
  - ExecutionLevel: 1 = InheritFromParent, 2 = Passive, 3 = Dispatch.
  - SynchronizationScope: 1 = InheritFromParent.
  - DispatchType: 2 = Parallel, 3 = Manual.
  - PowerManaged: 0 = WdfFalse, 1 = WdfTrue, 2 = WdfUseDefault.

## KMDF behavior modeled by the stubs (not executed C code)

| DDI | Model |
|---|---|
| `WdfRequestRetrieveOutputMemory` | Output memory is the request's output buffer (`Irp->UserBuffer` for internal METHOD_NEITHER), with length OutputBufferLength. A length of 0 gives `STATUS_BUFFER_TOO_SMALL` (`c0000023`) |
| `WdfRequestRetrieveOutputBuffer(min)` | `c0000023` if the length is 0 or less than `min` |
| `WdfMemoryCopyFromBuffer` | offset > size gives `c0000206`; offset + count > size gives `c0000023`, and nothing is copied |
| `WdfIoQueueRetrieveNextRequest` | FIFO; an empty queue gives `STATUS_NO_MORE_ENTRIES` (`8000001a`) |
| `WdfRegistryQueryValue` | A missing value gives `c0000034`. Data longer than ValueLength gives `STATUS_BUFFER_OVERFLOW` (`80000005`). Otherwise the data is copied and its length and type reported. Type is not checked, matching the API |
| `WdfDriverOpenParametersRegistryKey` | `c0000034` for "nokey", otherwise success |
| `WdfObjectDelete` | marks the object deleted but does not free it, so the C use-after-free (B10) reads stale but valid memory. The delete order is in `ops` |
| `WdfTimerStart` | returns TRUE if the timer was already armed |
| `WdfRequestSend` | Driver-created requests go to an in-flight list that the harness completes later. Forwarded GET_STRING requests count as owned by the lower driver |
| Object contexts | zero-filled at creation, as KMDF does |
| `KeQueryPerformanceCounter(p)` | if `p != NULL`, writes `qpcf` (the frequency, default 10,000,000) to `*p`. It returns an incrementing counter (`qpcc`) that never equals the frequency |

Failure injection (`G.*` knobs in the harness) simulates failing DDIs. The `wdf` field names the DDI and the
injected status.

## Files

| File | Records | Content |
|---|---|---|
| `setup.txt` | 12 | `DriverEntry`, `EvtDriverDeviceAdd` (device, lookaside, timer, interface, queue configuration), failure paths, driver cleanup, `EvtIoStop` |
| `descriptors.txt` | 85 | `GET_DEVICE_DESCRIPTOR` / `GET_REPORT_DESCRIPTOR` / `GET_DEVICE_ATTRIBUTES` for all 9 PIDs × ReportType 0/1, versions, buffer sizes, injected unreachable states |
| `ioctl.txt` | 76 | IOCTL routing: unsupported codes (named and 40 pseudo-random), GET_STRING forwarding, READ_REPORT dispatch per DeviceStatus |
| `feature.txt` | 572 | GET_FEATURE / SET_FEATURE: validation, report IDs 7/8/4/6 edge cases, every other report ID 0..255 for both GET and SET |
| `wire.txt` | 46 | PrepareHardware (lower `GET_DEVICE_ATTRIBUTES`, table lookup, registry), the mode-switch SET_FEATURE buffer, lower READ_REPORT issue and its failure paths |
| `power.txt` | 100 | 12 event sequences: D0Entry / SMIO init/restart / recovery timer / reads / lower completions / D0Exit / cancellation |
| `input_f1.txt` | 1242 | Family 1 (PIDs 0275, 0279, 0290, 0291): edge cases + 1000 pseudo-random packets |
| `input_f2.txt` | 1174 | Family 2 (PIDs 0272, 0273) |
| `input_f3a.txt` | 1174 | Family 3A (PIDs 0276, 0277) |
| `input_f3b.txt` | 1140 | Family 3B (PID 0278) |

### input_*.txt (lower READ_REPORT completion → PTP input report)

Each record runs the real path:

1. Preconditions: the device was brought up through AddDevice → PrepareHardware (lower attributes VID 05ac, `pid`,
   version 0x0102) → D0Entry → SelfManagedIoInit (SET_FEATURE succeeds). `DeviceStatus` is then 1, Configured.
2. The harness sets `LastReportTime = lrt_in`.
3. An upstream `IOCTL_HID_READ_REPORT` with an `outlen`-byte prefilled output buffer is dispatched through
   `EvtIoInternalDeviceControl`. The driver forwards it to `HidQueue` and issues one lower read.
4. The harness completes that lower read:
   - the 1024-byte lookaside buffer becomes `in` followed by zeros up to 1024 bytes;
   - `WdfRequestGetInformation(lower)` returns `len`;
   - `Params->IoStatus.Status` is `lowst`.

   Then it calls the driver's `AmtPtpRequestCompletionRoutine`.

| Field | Meaning (C struct member) |
|---|---|
| `case` | `<pid>-<edge name>` or `<family>-rand-<k>` |
| `pid` | `DEVICE_CONTEXT.HidProductID` (not read by the completion routine; identifies the config entry) |
| `xmin`, `ymax` | `TrackpadInfo.XMin`, `TrackpadInfo.YMax`, the only `TrackpadInfo` fields the routine reads, as set by the real PrepareHardware from `SpiTrackpadConfigTable` |
| `lrt_in` / `lrt_out` | `LastReportTime.QuadPart` before and after |
| `qpcf` | value `KeQueryPerformanceCounter(&CurrentCounter)` writes into `CurrentCounter` (the frequency out-parameter) |
| `outlen` | upstream OutputBufferLength (`Irp->UserBuffer` size) |
| `lowst` | lower `IoStatus.Status` (ignored by C) |
| `len` | lower Information (C uses `(LONG)` of it) |
| `in` | lower buffer prefix; every byte the C code can read is inside it, or is a zero of the tail |
| `status`, `info`, `infoset` | upstream completion |
| `out` | upstream output buffer after completion (`outlen` bytes). Bytes 0..30 = `PTP_REPORT` when `status=00000000` |
| `undef` | (edge records only) bit mask over `out` of bits the C code leaves **uninitialized** (see below) |
| `b9`, `out_tail0` | present only on B9 records; see below |
| `qpc_calls`, `qpcc` | number of `KeQueryPerformanceCounter` calls; `qpcc` (edge records only) is the discarded counter value |
| `ops` | completion-routine calls only; present on the edge records of the first PID of each family (0275, 0272, 0276, 0278) |

`DeviceStatus` is 1 before and after every input record (asserted). The upstream request is always completed
exactly once, and the lower request and its memory are always deleted (asserted).

**B9 records: finger bytes read past `len`.** Let `L = (LONG)len`. When `L >= 46`, the C code reads
`46 + 30*min(in[30],5)` bytes regardless of `L`. In 384 records (94 in F1, 97 in F2, 101 in F3A, 92 in F3B) a
byte in `in[L ..]` that the C code reads is non-zero. Those records carry two extra fields:

- `b9=1`;
- `out_tail0`, the output of a **third run of the real C code** with the lower buffer zeroed from offset `L` on.
  This is the "treat bytes past the valid length as 0" reading of port policy P2. Status, Information and
  `LastReportTime` are identical in both runs.

Which field to compare depends on the port's decision:

- If the port feeds the parser the whole lower buffer, as C does, compare `out`.
- If the port only sees the first `L` bytes and treats the rest as 0, compare `out_tail0` where `b9=1`, and `out`
  everywhere else.

Records without `b9` give the same result under both choices.

**Uninitialized report bytes (B5).** `PtpReport` is an uninitialized stack local. The harness ran every input
twice, with the stack below the routine painted 0x00 and then 0xFF. The driver objects are built with `-O0`, so
the garbage comes from stack memory.

- `out` is the 0x00 run. This equals a **zero-initialized** report, which is what port policy P2 requires of the Rust
  port. It was cross-checked: every `out` is byte-identical to a build of `Input.c` with
  `-ftrivial-auto-var-init=zero`.
- `undef` = run(0x00) XOR run(0xFF), per bit.
- For **every** record the harness asserted that `undef` is exactly:
  - for used contacts `i < min(NumOfFingers, 5)`: bits 5-7 of byte `1+5i` (the `Padding` bits); and
  - for contacts `min(NumOfFingers, 5)..5`: all 5 bytes.

  Nothing else is undefined. On random records `undef` is omitted because it follows this rule.

**Edge cases.** These cases run for the first PID of each family:

- `len` 0, 1 and 45;
- an n=0 header;
- button byte values;
- the spec §7.4 example;
- X and Y at XMin/XMax/YMin/YMax ±1, 0, ±32767/−32768 and −1;
- Pressure −32768..32767;
- TouchMajor/TouchMinor around 2500 and with signed extremes;
- tip/confidence combinations;
- NumOfFingers 0..11, 16, 127, 128 and 255;
- all five contacts at the extremes;
- Information variants, including `(LONG)` truncation: `0x10000002E` → 46 passes, `0xFFFFFFFF` → −1 fails;
- fingers read beyond `len`;
- an ignored `lowst`;
- `outlen` 0, 1, 29, 30, 31 and 64;
- ScanTime arithmetic: `lrt_in = qpcf − d` for many `d`, plus `qpcf` ∈ {0, 1, 3579545, 24000000, i64::MAX};
- ignored header and finger fields set to 0xFF.

The other PIDs of each family get a reduced set: the length gate, the example, X/Y bounds, n=0..6, the extremes
and the combos.

**Random cases.** There are 1000 per family, rotating through the family's PIDs.

- Generator: SplitMix64 with seed `0x5350494B4D000000 + family_index` (F1=0, F2=1, F3A=2, F3B=3).
- 15 % are fully random bytes, with a random `len`.
- 85 % are plausible packets:
  - `PacketType` 2;
  - NumOfFingers mostly 0..5, sometimes 6..10 or 0..255;
  - coordinates within the table range ±600;
  - pressure −5..300;
  - touch 0..3600;
  - occasional full-range values;
  - `len` mostly the packet size, sometimes shorter or random.
- About 1/6 of records use `lrt_in ≠ qpcf`, and about 1/20 a random `lowst`.

The observed ScanTime always equals `d = (qpcf − lrt_in) / 100` (C truncation toward zero), then
`d >= 255 ? 255 : (u16)d`. In the shipped driver `lrt_in == qpcf` always holds, so ScanTime is 0.

### descriptors.txt

Fields: `case code pid rt ver inj outlen completed status info infoset out wdf ops`.

- `pid`, `rt` and `ver` are `HidProductID`, `ReportType` and `HidVersionNumber`. They were set by the real
  PrepareHardware, except where `inj` says otherwise.
- `inj=pid` or `inj=rt` marks a value written directly into the context. These states are unreachable in
  practice; they show the `c0000225` (STATUS_NOT_FOUND) paths.
- `out` is the whole output buffer after the call. Unwritten bytes keep `PRE(i)`.

All 18 report descriptors (9 PIDs × rt 0/1) match the spec §5.1 SHA-256 values and
`crates/amtptp-core/tests/hid/fixtures/spi.txt`. The HID descriptor is `09210001000122f501` for every PID.

### ioctl.txt

Fields: `case code outlen inlen ds_in q_in completed status info infoset q_out lower_reads chg wdf ops`.

- `ds_in` is the DeviceStatus before dispatch.
- `q_in` and `q_out` are the `HidQueue` depth.
- `lower_reads` is the number of lower READ_REPORTs issued.
- `chg` lists `offset:value` pairs of output bytes that differ from `PRE`.

### feature.txt

Fields: `case op code outlen|inlen ub rid blen rb rbuf pio_in prb_in prt_in completed status info infoset chg
pio_out prb_out prt_out [oob=1] wdf ops`.

- `outlen|inlen` is `Parameters.DeviceIoControl.OutputBufferLength` (GET) or `InputBufferLength` (SET).
- `ub=0` means `Irp->UserBuffer == NULL`. Otherwise it points to a `HID_XFER_PACKET`.
- `rid`, `blen` and `rb` give `reportId`, `reportBufferLen` and whether `reportBuffer` is non-NULL.
- `rbuf` is the report buffer's initial bytes as actually allocated.
- `chg` lists the report-buffer bytes that changed, as `offset:newvalue`.
- `pio`, `prb` and `prt` are `PtpInputOn`, `PtpReportButton` and `PtpReportTouch`. Nothing reads them, so they
  are unobservable.

### wire.txt

- `prep-*` records cover PrepareHardware on a fresh device.
  - Inputs: `lower_attr`, the 32 bytes the lower `GET_DEVICE_ATTRIBUTES` returns; `lower_st`, the status of that
    send; `reg`, the DesiredReportType registry state; and `rt_in`.
  - Outputs: `status`, plus the context after the call:
    - `hid_vid`, `hid_pid`, `hid_ver`: `HidVendorID`, `HidProductID`, `HidVersionNumber`;
    - `ti`: `TrackpadInfo` VendorId, ProductId, XMin, XMax, YMin, YMax;
    - `rt_out`: `ReportType`;
    - `ds_out`: `DeviceStatus`.
  - `prep-restart-*` run PrepareHardware twice on the same device. `reg1` / `status1` / `rt_in` describe the first
    run. The first run sets `ReportType=1`, which tells "value ignored" (`rt_out` stays 1) apart from "set to 0".
    DWORD 2, 0x100, 0x101 and 0xFFFFFFFF, an 8-byte QWORD, a missing value and a missing key are all ignored.
    DWORD 0 and BINARY(4) 0 set 0; BINARY(4) 2 is ignored.
- `setstate-*` records cover the mode switch.
  - `setfeature_in` is the exact 255-byte input buffer of the synchronous internal `IOCTL_HID_SET_FEATURE`
    (`000b0191`). It is laid out as a `HID_XFER_PACKET` followed by the report `02 01`.
  - Bytes 0..8 hold `reportBuffer`, a pointer into the same buffer. They are emitted as the **offset** from the
    buffer start (`1000000000000000` = +16), and `ops` says `relptr=0>16`.
- `lower-read-*` records cover the lower READ_REPORT that is formatted and sent for one upstream read, and its
  failure paths. In those failure paths the upstream request stays queued.

### power.txt

Fields: `seq step ev ret in_* out_*` plus, when a request was completed during the step,
`completed_req` / `completed_out`, then `wdf ops`.

- Snapshot keys:
  - `ds`: `DeviceStatus`;
  - `lrt`: `LastReportTime`;
  - `hidq`: labels queued in `HidQueue`, FIFO;
  - `inflight`: lower reads sent and not yet completed;
  - `timer`: `armed:due` of `PowerOnRecoveryTimer`, as seen by the harness. `due=-50000000` is 5 s relative.
- `LowerReadComplete` uses `pkt1`: n=1, click=1, X=100, Y=2000, P=30, TouchMajor=800, TouchMinor=600, 76 bytes.
  `completed_out` uses the 0x00 stack paint.
- `FrameworkCancelsQueued` means the harness removed the oldest queued request, as a framework cancellation
  would.

## Excluded inputs (C undefined behavior; the Rust port fixes these per port policy P2)

- **Uninitialized `PtpReport` bytes (B5).** These were not excluded but replaced by zero (`out`) and flagged in
  `undef`.
- **`LastReportTime` values where `qpcf − LastReportTime` overflows i64** (signed overflow), e.g.
  `lrt = i64::MIN` with `qpcf ≥ 0`. They were never generated. The extremes used are
  `lrt_in = qpcf − i64::MAX` and `lrt_in = i64::MAX`.
- **SET_FEATURE report 4 with `reportBufferLen` < 2**: C reads `reportBuffer[1]`. **Report 6 with
  `reportBufferLen` < 2**: C reads `reportBuffer[2]`.
  - Exception: report 6 with `reportBufferLen = 2`. This is the real-world request (B2). It is recorded with
    `oob=1`, a 3-byte allocation and byte 2 = 0, i.e. "treat as 0". The value has no observable effect.
- **GET/SET_FEATURE with `reportBuffer == NULL` and a `reportBufferLen` that passes the size check** (NULL
  dereference). NULL with `reportBufferLen=0` is included (`*-rbnull`).
- **`Irp->UserBuffer` that does not point to a valid `HID_XFER_PACKET`**, and report buffers smaller than the C
  code writes. GET 7 writes 3 bytes and GET 8 writes 2 bytes; every allocation is at least that.
- **Registry `DesiredReportType` values of 0–3 bytes (B24)**: `DesiredReportTypeValue` is read partly
  uninitialized.
- **Concurrency (B15)**, such as completions racing on several CPUs. The harness is single-threaded.
- **Use-after-free (B10)**: not undefined in the harness, because deletes are deferred. The C delete order is in
  `ops`: `WdfObjectDelete(l)` then `WdfObjectDelete(m)`.

Not UB, but noted: the C code reads finger bytes beyond `len` (B9) but inside the 1024-byte buffer. These cases are
included. The bytes beyond `in` are zero, and `out_tail0` gives the tail-zeroed alternative (see input_*.txt).

## Spec check (`spikm.md` incl. Verification addendum)

No statement in the spec was contradicted by executing the C code. These checks were made:

- Report descriptors: SHA-256 and the HID fixture.
- HID descriptor bytes; attributes with VID bytes `10 89` and `Reserved` untouched.
- §7.4 worked example: contact bytes `03 f2 12 7a 12`.
- §5.4 caps `07 05 00`; HQA writes only `[0]=08 [1]=fc`.
- §6.2 SET_FEATURE buffer.
- §4 IOCTL table and statuses.
- §2.9–2.12 power behavior: the timer fires in D3 and sets Configured; a stale timer runs after a successful
  restart; the recovery timer issues exactly one lower read; reads queued before SMIO stay queued.
- §9 registry rules.
- `sizeof(DEVICE_CONTEXT)=96` and `WORKER_REQUEST_CONTEXT=16`.
- An independent Python model of §7.3, using a zero-initialized report, reproduced the status, Information and
  output bytes of all 4730 input records exactly, and the `out_tail0` of all 384 B9 records.

Behaviors the spec leaves implicit, confirmed here:

- Information is narrowed with `(LONG)`. `len=0x10000002E` passes as 46. `0x80000000`, `0xFFFFFFFF` and
  `0xFFFFFFFFFFFFFFFF` are negative and give `c000009c`.
- PrepareHardware stores `HidVendorID`, `HidProductID` and `HidVersionNumber` before the table lookup. On
  `c0000225` they hold the lower values, while `TrackpadInfo` stays zero.
- A lower `GET_DEVICE_ATTRIBUTES` that returns a warning status (`80000005`) also fails PrepareHardware, because
  the check is `!NT_SUCCESS`.
- GET_STRING: if `WdfRequestSend` fails and `WdfRequestGetStatus` returns `00000000`, the request is completed
  with STATUS_SUCCESS.
- GET/SET_FEATURE check the buffer length before checking `UserBuffer` for NULL. Length 15 with NULL gives
  `c0000023`.
- `GET_DEVICE_DESCRIPTOR` does not depend on `ReportType`. Only `GET_REPORT_DESCRIPTOR` does: an invalid
  ReportType gives `c0000225`.

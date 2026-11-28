# AmtPtpBthFilter: the Bluetooth lower filter

`AmtPtpBthFilter` is a KMDF lower filter on the Magic Trackpad 2's Bluetooth HID device node
(`BTHENUM\{00001124-0000-1000-8000-00805f9b34fb}_VID&0001004c_PID&0265`), below `HidBth`. It has no
C counterpart: it replaces the HIDCLASS detour of `AmtPtpHidFilter` (see [hidfilter.md](hidfilter.md),
"The data source").

* `crates/amtptp-usb-bt/src/infrastructure/bth.rs`: the pure part, shared with `AmtPtpHidFilter` (host-tested).
* `drivers/interface/bluetooth/bth`: the KMDF glue (`lib.rs`, `brb.rs`, `sdp.rs`, `interrupt.rs`).

## Why

The trackpad sends its multitouch report `0x31` and accepts the mode-switch feature `0xF1` without
declaring either in its SDP HID descriptor (135 bytes: input `0x02` and `0x90`, feature `0x55`).
HIDCLASS drops undeclared reports, so the C driver read `HidBth` directly through a patch of
HIDCLASS's dispatch table. That made it a second reader of `HidBth` beside the trackpad's own
HIDCLASS, and about 3% of the reports went to HIDCLASS and were lost ([hidfilter.md](hidfilter.md),
"Competing reader on Bluetooth").

This filter makes HIDCLASS deliver the reports itself, so `AmtPtpHidFilter` can read them like any
HID client.

## What it does

### 1. The SDP descriptor

`HidBth` reads the HID service's SDP attributes once per start (`HidBthInitDevice`), through its
lower device: `IOCTL_INTERNAL_BTHENUM_GET_DEVINFO`, `IOCTL_INTERNAL_BTHENUM_GET_ENUMINFO`,
`IOCTL_BTH_SDP_CONNECT` (with `SDP_CONNECT_CACHE`, so bthport answers from its cache), then
`IOCTL_BTH_SDP_SERVICE_ATTRIBUTE_SEARCH` twice (a 9-byte buffer for `requiredSize`, then the full
one), then `IOCTL_BTH_SDP_DISCONNECT`. A reconnect after a power cycle does not repeat it.

For each `SERVICE_ATTRIBUTE_SEARCH` at `PASSIVE_LEVEL`, the filter (`sdp.rs`):

1. sends the same search down with its own 4 KB buffer;
2. appends [`VENDOR_TLC`] to the report descriptor in the returned stream
   ([`augment_sdp_attribute_list`]); every enclosing SDP element gets its new length, and a length field
   only widens when it must;
3. answers `HidBth` itself, the way the stack does: the full stream when it fits (`Information` = 8 +
   size), else `requiredSize` only (`responseSize` 0, `Information` = the buffer size).

So both calls report the same exact size (Magic Trackpad 2: 271 bytes become 310). Anything
unexpected (another IRQL, a failed or oversized search, a stream it cannot parse, a descriptor that
already declares `0x31` or `0xF1`) forwards the original request unchanged instead.

The response is an SDP `AttributeLists` (a sequence of attribute lists, one per record); a cached
record (`BTHPORT\Parameters\Devices\<addr>\CachedServices`) is a single attribute list. The parser
handles both.

The vendor collection is appended last, so HIDCLASS exposes it as `Col03` and `Col01`/`Col02` keep
their hardware IDs:

```
06 01 FF  09 31  A1 01          Usage Page (Vendor FF01), Usage 0x31, Collection (Application)
  85 31  15 00  26 FF 00        Report ID 0x31, Logical 0..255
  35 00  45 00  65 00  55 00    reset the physical range, unit and exponent the earlier collections set
  75 08  95 8B  09 01  81 02    139 data bytes, Input
  85 F1  95 02  09 02  B1 02    Report ID 0xF1, 2 data bytes, Feature
C0
```

### 2. The interrupt channel

HIDCLASS walks each read in steps of the declared report length and drops a shorter remainder
(`HidpInterruptReadComplete`), so a report whose length depends on the number of fingers (4 + 9n
bytes) cannot be declared as it is. The filter pads it (`interrupt.rs`, [`pad_interrupt_payload`]):

| Payload | Becomes |
|---|---|
| `A1 31 <raw report>`, raw ≤ 139 bytes | `A1 31 <raw report> 00… <raw length>`, 141 bytes |
| `A1 02 <mouse report>` (the trackpad is in mouse mode) | `A1 31 02 00… 00` (length 0): a *mode-lost marker* |
| anything else (`0x90` battery, …) | unchanged |

`AmtPtpHidFilter` reverses it ([`unpad_report`]): a multitouch report gets its real length back; a
marker becomes a one-byte report of the original ID, which makes it send the mode switch again (the
recovery after the trackpad's power switch, PR #10). The battery report still reaches `Col02`.

A transfer is padded only when all of these hold:

* it is on a HID interrupt channel: the filter records the channel handle when an open
  (`BRB_L2CA_OPEN_CHANNEL`, `…_RESPONSE`, or the enhanced variants) completes with PSM `0x13`, and
  forgets it on `BRB_L2CA_CLOSE_CHANNEL`. On completion, the `Psm`/`Response` union holds the PSM for
  an outgoing open (start) and for an accepted incoming one (reconnect after a power cycle);
* its buffer was exactly 141 bytes when it was submitted: `HidBth` sizes each interrupt transfer as
  the upper read plus one byte, and HIDCLASS reads the declared report length (140);
* it succeeded, and nothing of the L2CAP packet was left over (`RemainingBufferSize` 0).

### 3. Everything else

Every other request, BRBs included (also the undocumented types `0xC101`/`0xC102` that `HidBth`
sends), goes down unchanged with send-and-forget. Only channel opens and the qualifying interrupt
transfers get a completion routine.

## HidBth facts this relies on (measured, 2026-11-27)

Measured with a trace-only lower filter at the same position (a PoC, not kept) and the BTHPORT HCI
trace, on Windows 11 build 26300:

* The SDP requests and all BRBs go through `HID_DEVICE_EXTENSION.NextDeviceObject`, i.e. through a
  lower filter of the BTHENUM node (also seen statically in `HidBthGetSdpRecord` and
  `HidBthSendBrbAsync`).
* `HidBth` has no buffer of its own for the interrupt channel: each `IOCTL_HID_READ_REPORT` becomes
  one `BRB_L2CA_ACL_TRANSFER` (IN, `ACL_SHORT_TRANSFER_OK`) of the read's size plus one byte.
* A packet larger than the transfer completes it with `RemainingBufferSize` > 0, and the rest goes to
  the next IN transfer. With the original descriptor HIDCLASS read 8 bytes, so every multitouch packet
  was split; that was where the C driver's `STATUS_DEVICE_DATA_ERROR` came from (all 27 failures in
  one run were a continuation fragment landing in the filter's read).
* The filter position saw the start of 2247 of the 2248 interrupt PDUs the HCI trace recorded.

## BRB layout

wdk-sys has no `bthddi.h` bindings. `brb.rs` writes out the offsets and sizes it uses; they were
measured by compiling the WDK 10.0.28000 `bthddi.h` with `offsetof`/`sizeof` for x64 and ARM64, which
agree (`BRB_HEADER` 112 bytes; `_BRB_L2CA_ACL_TRANSFER` 168 with `BufferSize` @132, `Buffer` @136,
`BufferMDL` @144, `RemainingBufferSize` @160; the open structures 368 and 480 with `ChannelHandle`
@112 and the PSM @120; `_BRB_L2CA_CLOSE_CHANNEL` 128 with `ChannelHandle` @120). Compile-time
assertions keep each field inside its structure, and a field is read only after `BRB_HEADER.Length`
shows that the BRB covers the structure of its type. The build refuses other targets.

The transfer buffer is reached through `Buffer` or, without one, through an MDL that already has a
system address (`MDL_MAPPED_TO_SYSTEM_VA` or `MDL_SOURCE_IS_NONPAGED_POOL`); the filter never maps one.

## INF

`AmtPtpDevice.inx` binds the BTHENUM hardware ID to `AmtPtpBthFilter_Device`, which installs the inbox
function driver through `Include = hidbth.inf, input.inf` and `Needs = HIDBTH_Inst.NT, HID_Inst.NT`
(`.NT.HW` and `.NT.Services` likewise), registers `AmtPtpBthFilter` using `AddFilter`, and binds `Col03` to the
null device. On Windows 11 the separate `Col01` installation gets its function driver from
`MsHidKmdf.inf`. See [the OS-specific INF sections](packaging.md#framework-version-and-os-floor).

## Hardware verification (2026-11-27, Magic Trackpad 2 `REV_0871`, Windows 11 26300)

* `Col03` appears as page `0xFF01`, usage `0x31`, input 140 bytes, feature 3 bytes.
* Padded reports read from `Col03`: raw lengths 13/22/31/40 (one to four fingers), none malformed.
  The trackpad never sent a 4-byte report: lifting is reported per finger.
* With `AmtPtpHidFilter` reading `Col03` (no detour): in 20 s of one- to three-finger gestures, the
  interrupt PDUs on the air, the reports this filter padded and the reports `AmtPtpHidFilter`
  delivered were equal in every second (879 each); no read failed.
* Power switch: after the reconnect, four mouse reports arrived as markers; `AmtPtpHidFilter` sent
  the mode switch again and multitouch resumed about 0.1 s later.
* Disabling and re-enabling `Col03`: `AmtPtpHidFilter` closed the source on the query-remove (its
  two reads in flight were cancelled), reopened it on arrival and sent the mode switch (success).
* After a reboot (no detour patch left in memory from earlier builds): input works.

Not verified: sleep and resume, ARM64 on hardware.

## Tracing

Provider `AmtPtpBthFilterTraceProvider`, id `849adcb8-3936-417a-a967-0f281ec3c07f`. Keywords: 0x1
driver and device, 0x2 SDP, 0x4 interrupt channel.

* Informational: `DriverInitialized`, `DriverContextCleanup`, `DeviceAdd`, `SdpAugmented` (original
  and augmented size, the output buffer length and `Information`), `InterruptChannelOpened` and
  `InterruptChannelClosed` (channel handle).
* Warning: `SdpPassThrough` (reason and status), `PadBufferUnavailable`.
* Error: `CallFailed` (WDF call and status).
* Verbose, one per report: `ReportPadded` (bytes received), `PadSkipped` (bytes received and left over).

[`VENDOR_TLC`]: ../../crates/amtptp-usb-bt/src/infrastructure/bth.rs
[`augment_sdp_attribute_list`]: ../../crates/amtptp-usb-bt/src/infrastructure/bth.rs
[`pad_interrupt_payload`]: ../../crates/amtptp-usb-bt/src/infrastructure/bth.rs
[`unpad_report`]: ../../crates/amtptp-usb-bt/src/infrastructure/bth.rs

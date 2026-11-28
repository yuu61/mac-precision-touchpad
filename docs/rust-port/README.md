# Rust port records

USB devices now share the KMDF driver. The settings app and its vendor report have been retired. The records below document the earlier port. See the [current packaging guide](packaging.md).

The four drivers were rewritten from C (commit 2edb0f2) to Rust. These documents record how
the port was done and what changed.

| Document | Content |
|---|---|
| [usbum.md](usbum.md) | AmtPtpDeviceUsbUm (UMDF): decisions per C defect, fixture coverage, WDF glue mapping |
| [usbkm.md](usbkm.md) | AmtPtpDeviceUsbKm (KMDF, T2): same |
| [spikm.md](spikm.md) | AmtPtpDeviceSpiKm (KMDF, SPI): same |
| [hidfilter.md](hidfilter.md) | AmtPtpHidFilter (KMDF, Bluetooth Magic Trackpad 2): same |
| [bthfilter.md](bthfilter.md) | AmtPtpBthFilter (KMDF, new: lower filter below HidBth that declares and pads the Magic Trackpad 2 reports, replacing the HIDCLASS detour) |
| [packaging.md](packaging.md) | Building, packaging and signing; INF changes; framework versions and OS floor |
| [specs/](specs/) | Porting analyses of the C drivers (`usbum.md`, `usbkm.md`, `spikm.md`, `hidfilter.md`) and of the code shared between them (`crosscut.md`). Bug IDs such as `UM-B7` or `HF-B15` refer to the `Bn` items in these analyses. File:line references point into the C tree at commit 2edb0f2. |

## Behavior policy

The rewrite is meant to be behavior-preserving. Every C defect listed in the analyses was
classified with one of these rules, and the classification is recorded in the per-driver
documents:

- **P1 PRESERVE** everything observable outside the driver: report and HID descriptor bytes,
  device attributes (including VendorID 0x8910 where the C driver reports it), input report
  bytes with every quirk (coordinate formulas, clamps, tip/confidence thresholds, contact IDs,
  ScanTime exactly as the C code computes it), feature-report semantics, NTSTATUS values and
  Information sizes, IOCTL routing, USB/SPI wire bytes, mode-switch sequences, timers, PnP and
  power behavior, queue configuration, registry reads and INF bindings.
- **P2 FIX** anything that was undefined behavior, memory-unsafe, a resource leak or a hang:
  out-of-bounds reads and writes, uninitialized bytes sent to the host (reports are now
  zero-initialized), use-after-free, writes into a stack copy instead of the caller's buffer,
  buffer overruns, requests that were retrieved but never completed, `WdfObjectDelete(NULL)`,
  paged pool used for transfer buffers, data races on plain fields (now atomics). The fix chosen
  is the behavior the C code evidently intended and closest to what it observably did.
- **P3 KEEP** other logic defects as they are (for example the selective-reporting bit order,
  SET_FEATURE always returning success in UsbUm, ScanTime being 0, the HQA certification report
  carrying only its first blob byte, VendorID 0x8910 not matching what the settings app looks
  for). These are listed as "preserved known issues" so they can be fixed deliberately later.

Later pull requests fixed several P1 and P3 items on purpose: ScanTime and the full HQA report
(PR #7), the selective-reporting bit order (PR #8), the UsbUm feature-report statuses, contact IDs
with liftoff reports, confidence, coordinate clamps and the HID descriptors (PR #13 and #14). The
per-driver tables mark each such item `FIXED`; the items still marked `PRESERVED`, such as
VendorID 0x8910, are unchanged.

## How the port was verified

- The hardware-independent logic lives in the `no_std` crates under `crates/`. Their tests replay
  fixtures recorded by compiling the original, unmodified C sources against a recording fake of
  KMDF/UMDF and executing them (`crates/*/tests/*/fixtures/README.md` describes each fixture set).
  Every record is replayed; records whose C result is undefined behavior or a P2 defect are
  checked against the fixed behavior instead and counted explicitly.
- The WDF glue under `drivers/` is not covered by those fixtures. It was reviewed against the C
  sources function by function, with every request path (completed or forwarded exactly once)
  and every callback (IRQL, code section) accounted for.
- AmtPtpHidFilter and AmtPtpBthFilter have been run on hardware (Bluetooth Magic Trackpad 2,
  x64); see their records. AmtPtpDeviceUsbUm has been run with the same trackpad over USB.
  AmtPtpDeviceUsbKm and AmtPtpDeviceSpiKm have not been run on hardware.

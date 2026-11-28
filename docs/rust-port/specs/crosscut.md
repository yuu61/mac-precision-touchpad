<!-- Porting analysis of the C driver as of commit 2edb0f2, written before the Rust rewrite. File:line references point into that commit's `src/` tree, which has since been removed. -->

# Cross-cutting spec for the C to Rust driver rewrite

Repository: `` at `main` (HEAD 29d2778). All `file:line` citations are relative to the repository root unless noted otherwise.

Scope: four Windows drivers, the universal INF, packaging, CI, and the contract with the C# settings app. The C# app (`src/AmtPtpDevice.Settings`) stays as it is.

Method:
- I diffed every duplicated header textually (`diff --strip-trailing-cr`).
- I also expanded every HID report descriptor to real bytes. To do this I compiled each driver's own headers with MinGW gcc against a stub `hidport.h` and dumped the arrays and struct sizes.
- The byte dumps (`um.txt`, `km.txt`, `spi.txt`, `flt.txt`, plus `hqa.txt`) are kept as golden fixtures in `crates/amtptp-core/tests/hid/fixtures/`; they are the host-test oracle for "byte-for-byte". The harness sources were not kept.

---

## A. Duplicated or near-duplicated code across the four drivers

### A1. `HidCommon.h`: 4 copies, report IDs and HID short-item tags

| Copy | Lines | Difference from UsbUm copy |
|---|---|---|
| `src/AmtPtpDeviceUsbUm/include/HidCommon.h` | 1-39 | (baseline) |
| `src/AmtPtpDeviceSpiKm/HidCommon.h` | 1-39 | identical |
| `src/AmtPtpDeviceUsbKm/include/hid/HidCommon.h` | 1-39 | identical |
| `src/AmtPtpHidFilter/include/HidCommon.h` | 1-43 | adds a comment at :1, `DEVICE_VID 0x8910` at :42, `DEVICE_VERSION 0x100` at :43 |

Contents:
- Report IDs:
  - `REPORTID_STANDARDMOUSE` 0x02
  - `REPORTID_MULTITOUCH` 0x05
  - `REPORTID_REPORTMODE` 0x04
  - `REPORTID_PTPHQA` 0x08
  - `REPORTID_FUNCSWITCH` 0x06
  - `REPORTID_DEVICE_CAPS` 0x07
  - `REPORTID_UMAPP_CONF` 0x09
- Usages: `BUTTON_SWITCH` 0x57 and `SURFACE_SWITCH` 0x58.
- HID item prefix bytes, for example `USAGE_PAGE` 0x05, `LOGICAL_MAXIMUM_3` 0x27, `REPORT_COUNT_2` 0x96, `FEATURE` 0xb1, `END_COLLECTION` 0xc0. These are at UsbUm HidCommon.h:3-39.

The other "device identity" constants are scattered and inconsistent:

| Constant | UsbUm | UsbKm | SpiKm | HidFilter |
|---|---|---|---|---|
| `DEVICE_VID` 0x8910 | Hid.h:20 | (absent) | AppleDefinition.h:4 | HidCommon.h:42 |
| `DEVICE_VERSION` | 0x01 (Hid.h:19) | 0x01 (include/Hid.h:14) | 0x01 (Hid.h:16), not used for attributes | 0x100 (HidCommon.h:43) |
| `MAX_FINGERS` 16 | AppleDefinition.h:122 | AppleDefinition.h:72 | Hid.h:17 | WindowsHID.h:4 and HidDevice.h:37 (defined twice) |

### A2. PTP top-level collections and the HQA blob: byte-identical in all 4 drivers

| Item | UsbUm `include/Hid.h` | UsbKm `include/Hid.h` | SpiKm `Hid.h` | HidFilter `include/Metadata/WindowsHID.h` |
|---|---|---|---|---|
| `AAPL_PTP_USERMODE_CONFIGURATION_APP_TLC` (23 B) | 22-33 | 16-27 | 19-30 | 16-27 |
| `AAPL_PTP_WINDOWS_CONFIGURATION_TLC` (47 B) | 35-60 | 29-54 | 32-57 | 29-54 |
| `DEFAULT_PTP_HQA_BLOB` (256 B) | 62-94 | 56-88 | 59-91 | 56-88 |
| `PTP_MAX_CONTACT_POINTS` 5, `PTP_BUTTON_TYPE_*`, `PTP_COLLECTION_MOUSE` 0, `PTP_COLLECTION_WINDOWS` 3 | 96-104 | 90-98 | 93-101 | 6-14 |

The expanded bytes are verified identical by `cmp.py`, with the result "WinCfg identical in SPI / T2: True".

- UMAPP TLC: `06 00 ff 09 01 a1 01 85 09 09 01 15 00 26 ff 00 75 08 95 03 b1 02 c0`
- Windows configuration TLC: `05 0d 09 0e a1 01 85 04 09 22 a1 02 09 52 15 00 25 10 75 08 95 01 b1 02 c0 a1 00 85 06 09 57 09 58 75 01 95 02 25 01 b1 02 95 06 b1 03 c0 c0`
  - The `25 10` in it is `MAX_FINGERS`, not `PTP_MAX_CONTACT_POINTS`.

### A3. Report structs: same names, different layouts

The sizes below were measured with gcc using MS bitfield layout. All four drivers use 1-byte packing for these structs.

| Struct | UsbUm `include/Hid.h` | UsbKm `include/Hid.h` | SpiKm `Hid.h` | HidFilter `include/HidMiniport.h` |
|---|---|---|---|---|
| `PTP_DEVICE_CAPS_FEATURE_REPORT` | 120-124, 3 B | 100-104, 3 B | 103-107, 3 B | 7-11, 3 B |
| `PTP_DEVICE_HQA_CERTIFICATION_REPORT` | 126-129, 257 B | 106-109 | 109-112 | 14-17 |
| `PTP_DEVICE_INPUT_MODE_REPORT` | 131-134, 2 B | 111-114 | 114-117 | 20-23 |
| `PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` | 136-143, **2 B** (id, bits) | 116-125, **3 B** (id, `DeviceMode` :120, bits) | 119-128, **3 B** (`DeviceMode` :123) | 26-31, **2 B** |
| `PTP_CONTACT` | 145-154, **9 B** (flags u8, `ContactID` u32, X u16, Y u16) | 127-137, 9 B | 130-140, **5 B** (`ContactID:3` bits in byte 0 at :135, X, Y) | 34-41, 9 B |
| `PTP_REPORT` | 156-162, **50 B** | 139-145, 50 B | 167-173, **30 B** | 44-50, 50 B (whole struct packed) |
| `PTP_USERMODEAPP_CONF_REPORT` | 164-169, 4 B | 147-152, 4 B | 175-180, 4 B | (absent) |

Structs that exist in only one driver:
- UsbUm: `HID_AAPL_MOUSE_REPORT` and `HID_INPUT_REPORT` (Hid.h:106-118).
- SpiKm: `CONTACT_STATE`, `PTP_CONTACT_RAW` and `PTP_CONTACT_RAW_LL` (Hid.h:142-165), and `REPORT_BUFFER_SIZE` 1024 (Hid.h:15).

Byte layout of input report 0x05 for the 9-byte-contact drivers (UsbUm, UsbKm, HidFilter):
- `[0]` is 0x05.
- Each contact `i` (0..4) starts at `b = 1 + 9i`:
  - `[b]` bit 0 is Confidence and bit 1 is TipSwitch.
  - `[b+1..b+5]` is ContactID as u32 LE.
  - `[b+5..b+7]` is X as u16 LE.
  - `[b+7..b+9]` is Y as u16 LE.
- `[46..48]` is ScanTime as u16 LE.
- `[48]` is ContactCount.
- `[49]` bit 0 is the button.

For SPI the contact is 5 bytes:
- `[b]` bit 0 is Confidence, bit 1 is TipSwitch, and bits 2-4 are ContactID.
- X and Y follow as u16 LE.
- ScanTime is at `[26]`, ContactCount at `[28]` and the button at `[29]`.

This matches the descriptors in A4: the 16-bit scan time works because the global `REPORT_SIZE 0x10` set inside the finger collections carries over to it.

### A4. Per-family report descriptors

All finger-collection headers are one template. Apart from contact shape and TLC usage, only four 16-bit values differ.

Copies of the template:
- UsbUm `include/DeviceFamily/{Wellspring3,5,6,7A,8,Mt2}.h`, each 138-139 lines.
- UsbKm `include/metadata/WellspringT2.h`.
- HidFilter `include/Metadata/MagicTrackpad2.h`. Its bytes are identical to UsbUm `WellspringMt2.h`; the textual diff is only the comment at :1 and a missing `#include`.
- SpiKm `HID/SpiTrackpadSeries{1,2,3}.h`. Series 3 holds two variants, 13" at :7-192 and 15" at :195-380.

Each header defines:
- `*_FINGER_COLLECTION_1`, which ends with a reset of `45 00 55 00 65 00` (PhysMax 0, UnitExp 0, Unit 0).
- `*_FINGER_COLLECTION_2`, which has no reset and is 6 bytes shorter.
- `*_PTP_TLC`, which places fingers in the order 1, 1, 2, 1, 2 and then appends the shared tail: scan time, contact count, button, caps feature 0x07 and HQA feature 0x08.
- SPI only: `*_TOUCHSCREEN_TLC`. It differs from the PTP TLC in one byte at offset 3 (usage `09 05` Touch Pad becomes `09 04` Touch Screen).

**Parameters, all little-endian u16.** The file:line column gives the X logical, X physical, Y physical and Y logical lines of collection 1. Collection 2 repeats them about 41 lines later.

| Family (driver) | X logical max | X physical max | Y physical max | Y logical max | file:line |
|---|---|---|---|---|---|
| Wellspring3 (UsbUm) | 9626 `9a 25` | 1050 `1a 04` | 760 `f8 02` | 6775 `77 1a` | Wellspring3.h:29,34,37,38 |
| Wellspring5/5A (UsbUm) | 9465 `f9 24` | 1050 `1a 04` | 810 `2a 03` | 6735 `4f 1a` | Wellspring5.h:29,34,37,38 |
| Wellspring6/6A (UsbUm) | 9760 `20 26` | 1067 `2b 04` | 762 `fa 02` | 6750 `5e 1a` | Wellspring6.h:29,34,37,38 |
| Wellspring7/7A (UsbUm) | 10030 `2e 27` | 1064 `28 04` | 773 `05 03` | 6880 `e0 1a` | Wellspring7A.h:29,34,37,38 |
| Wellspring8/9 (UsbUm) | 9760 `20 26` | 1045 `15 04` | 750 `ee 02` | 6750 `5e 1a` | Wellspring8.h:29,34,37,38 |
| MT2 (UsbUm and HidFilter) | 7612 `bc 1d` | 1600 `40 06` | 1149 `7d 04` | 5065 `c9 13` | WellspringMt2.h:28,33,36,37 |
| T2 (UsbKm) | 20000 `20 4e` | 1300 `14 05` | 850 `52 03` | 12000 `e0 2e` | WellspringT2.h:28,33,36,37 |
| SPI series 1 | 10666 `aa 29` | 1118 `5e 04` | 711 `c7 02` | 7855 `af 1e` | SpiTrackpadSeries1.h:29,34,37,38 |
| SPI series 2 | 10030 `2e 27` | 1064 `28 04` | 773 `05 03` | 6880 `e0 1a` | SpiTrackpadSeries2.h:29,34,37,38 |
| SPI series 3, 13" | 12992 `c0 32` | 1352 `48 05` | 836 `44 03` | 7855 `af 1e` | SpiTrackpadSeries3.h:29,34,37,38 |
| SPI series 3, 15" | 15432 `48 3c` | 1584 `30 06` | 992 `e0 03` | 9446 `e6 24` | SpiTrackpadSeries3.h:217,222,225,226 |

Comments that disagree with the bytes:
- `WellspringT2.h:20,61` says "Report Size: 0x10 (2 bytes)" but the byte is `0x20`.
- `:74` says 1045 but the bytes `14 05` are 1300.
- `:77` says 750 but `52 03` is 850.
- **The bytes are authoritative.** Port the bytes, not the comments.

**Contact shape: the second structural difference.**
- USB, T2 and MT2 use 1 byte of confidence and tip bits plus 6 bits of padding, then a 32-bit contact ID with `LOGICAL_MAXIMUM_3 ff ff ff ff`, then X and Y. That is 9 bytes. See WellspringMt2.h:7-23.
- SPI uses confidence and tip, then a 3-bit contact ID with `25 03`, then 3 bits of padding, then X and Y. That is 5 bytes. See SpiTrackpadSeries1.h:9-23.

**Segment layout, verified by `seg.py`.**

| Template | Header | Finger collections at (offset, length) | Separators | Shared tail | WinCfg | UMAPP |
|---|---|---|---|---|---|---|
| 9-byte contact | 10 B | (10,70) (84,70) (158,64) (226,70) (300,64) | `05 0d 09 22` | 82 B (364-445) | 47 B | 23 B |
| SPI 5-byte contact | 10 B | (10,67) (81,67) (152,61) (217,67) (288,61) | `05 0d 09 22` | 82 B (349-430) | 47 B | 23 B |

Patch offsets inside each finger collection:
- 9-byte template: X logical at +36, X physical at +47, Y physical at +54, Y logical at +57.
- SPI template: +33, +44, +51 and +54.

**Composition per driver, measured.**

| Driver | Report descriptor | Length | HID_DESCRIPTOR (9 B) | Defined at |
|---|---|---|---|---|
| UsbUm, all 6 families | PTP + WinCfg + UMAPP | 516 | `09 21 00 01 00 01 22 04 02` | include/StaticHidRegistry.h:6-112 |
| UsbKm (T2) | PTP + WinCfg (no UMAPP) | 493 | `... 22 ed 01` | Hid.c:7-22 |
| SpiKm, 4 families × {touchpad, touchscreen} | PTP or TS + WinCfg + UMAPP | 501 | `... 22 f5 01` | Hid.c:9-103 |
| HidFilter (MT2) | PTP + WinCfg (no UMAPP) | 493 | `... 22 ed 01` | include/Metadata/StaticHidRegistry.h:15-30 |

HidFilter's 493 bytes equal the first 493 bytes of UsbUm's MT2 descriptor, with 0 differing positions.

How each driver selects a descriptor:
- UsbUm switches on USB idProduct at Hid.c:42-135 and Hid.c:280-340:
  - W3 is 0236-0238.
  - W5/5A is 0245-0247 and 0252-0254.
  - W6/6A is 024c-024e and 0249-024b.
  - W7/7A is 0262-0264 and 0259-025b.
  - W8 and W9 are 0290-0292 and 0272-0274.
  - MT2 is 0265.
- SpiKm switches on HID PID at Hid.c:403-470:
  - Series 1 is 0275, 0279, 0290 and 0291.
  - Series 2 is 0272 and 0273.
  - Series 3a is 0276 and 0277.
  - Series 3b is 0278.
  - Registry value `DesiredReportType` (Device.c:183) picks touchpad or touchscreen.
- UsbKm always uses the T2 descriptor.
- HidFilter handles MT2 only (Hid.c:28-35, 113-119).

### A5. `AppleDefinition.h` (bcm5974 tables), 2.5 copies

| Copy | Content |
|---|---|
| `src/AmtPtpDeviceUsbUm/include/AppleDefinition.h` (417 lines) | Full Wellspring PID list (:3-60). `TRACKPAD_BUTTON_DATA`, `TRACKPAD_TYPE`, `HEADER_/BUTTON_/FSIZE_/DELTA_/USBMSG_TYPE1..5`, `TRACKPAD_FINGER` (28 B), `TRACKPAD_FINGER_TYPE5` (9 B, :145-163), `BCM5974_CONFIG` with `ansi, iso, jis` (:174), `DATAFORMAT`, and `SN_*`. `*_QUALIFICATION_THRESHOLD` (:213-218) is unused anywhere. `Bcm5974ConfigTable` has 15 entries (:221-418), **with no `{0}` sentinel**. |
| `src/AmtPtpDeviceUsbKm/include/AppleDefinition.h` (214 lines) | Same type and macro set, without `TRACKPAD_FINGER_TYPE5` and without the Wellspring PIDs. Adds T2 PIDs 027a-027d and `0xffff` fallback (:6-10). `BCM5974_CONFIG` uses a single `identification` field (:103). Its table has the fallback first and ends with a `{0}` sentinel (:149-214). |
| `src/AmtPtpHidFilter/include/HidDevice.h` (86 lines) | Renamed copies: `HOFFSET_TYPE_USB_1..5`, the new `HOFFSET_TYPE_BTH_5 = 4` (:10), `BOFFSET_*`, `FDELTA_*`, `FSIZE_*`, `TRACKPAD_BUTTON_DATA`, `TRACKPAD_FINGER`, and `TRACKPAD_FINGER_TYPE5` with a named union (:67-85). The MT2 limits (-3678..3934, -2479..2586, snratio 250) are hard-coded at Device.c:322-354, the same values as UsbUm AppleDefinition.h:400-401. |
| `src/AmtPtpDeviceSpiKm/AppleDefinition.h` (71 lines) | Different content and not a copy: `SPI_TRACKPAD_FINGER` (30 B), `SPI_TRACKPAD_PACKET` (346 B), `SPI_SET_FEATURE`, and the `SpiTrackpadConfigTable` VID/PID/min/max list with a `{0}` sentinel (:52-71). |

Differences between the UsbUm and UsbKm tables:
- The UsbUm T2 entry 0x027d uses x -4828..5345 and y -203..6803 (:404-417).
- The UsbKm 0x027d entry uses x -10000..10000 and y -2000..10000 (:199-210).
- All other shared fields match: TYPE4, tp_ep 0x83, and the USBMSG values.

### A6. Duplicated logic, not only headers

- **`AmtRawToInteger`**, a `(signed short)` cast, is defined twice: UsbUm InputInterrupt.c:592-597 and UsbKm Interrupt.c:7-12. It is declared again in UsbUm Device.h:196.
- **TYPE5 (MT2) decode** is duplicated in UsbUm InputInterrupt.c:500-548 and HidFilter Input.c:194-223:
  - X is `(SHORT)((u16 & 0x1fff) << 3) >> 3`.
  - Y is `-(INT)(u32 << 6) >> 19`.
  - Both are clamped at min.
  - ContactID is `Id:4`.
  - Tip is `TouchMajor << 1 > 0`.
  - Confidence is `TouchMinor << 1 < 345 && <same>`, a duplicated condition in both copies.
  - The button logic differs:
    - UsbUm requires `IsButtonReportOn && Buffer[tp_button] != 0` (InputInterrupt.c:545-548).
    - HidFilter uses `responseBuffer[InputButtonDelta] & 1` and ignores `PtpReportButton` (Input.c:221).
- **TYPE1-4 decode** is duplicated in UsbUm InputInterrupt.c:322-364 and UsbKm Interrupt.c:195-220:
  - X is `abs_x - x.min`.
  - Y is `y.max - abs_y`.
  - ContactID is the index `i`.
  - Tip differs: UsbUm uses `major << 1 >= 200`, while UsbKm uses `major << 1 >= 200 || minor << 1 >= 150` (Interrupt.c:212).
- **ScanTime** is (perf-counter delta / 100) clamped to 0xFF in three places: UsbUm InputInterrupt.c:275-287 and 470-482, and HidFilter Input.c:187-190.
- **Wellspring mode switch** (USB class control transfer: read request 1, write request 9, `um_req_val`, `um_req_idx`, buffer `[um_switch_idx] = on/off`):
  - UsbUm Device.c:257-357 for get and :361-496 for set.
  - UsbKm Device.c:505-639, which has set only and tracks the state in `IsWellspringModeOn`.
- **Config lookup**:
  - UsbUm Device.c:15 loops `for (cfg = table; cfg->ansi; ++cfg)` over a table with **no sentinel**, which is an out-of-bounds read when the PID is unknown.
  - UsbKm Device.c:34-47 has a sentinel and returns the fallback entry `[0]`.
- **Get-feature for reports 0x07 and 0x08** is the same code in all 4 drivers: UsbUm Hid.c:750-836, UsbKm Hid.c:328-397, SpiKm Hid.c:623-702 and HidFilter Hid.c:209-250.
- **HID attributes**:

| Driver | VendorID | ProductID | VersionNumber | Cite |
|---|---|---|---|---|
| UsbUm | 0x8910 | USB idProduct | 0x01 | Hid.c:223-228 |
| UsbKm | USB idVendor (0x05AC) | USB idProduct | 0x01 | Hid.c:154-156 |
| SpiKm | 0x8910 | SPI HID PID | underlying device's version | Hid.c:345-347 |
| HidFilter | 0x8910 | underlying PID (0x0265) | 0x100 | Hid.c:81-83 |

### A7. Behavior differences the port must decide explicitly

Do not silently copy these, and do not silently fix them.

1. **HQA blob comma-operator bug.** `*certReport->CertificationBlob = DEFAULT_PTP_HQA_BLOB;` writes **only byte 0 (0xfc)**. The other 255 bytes keep whatever was in the output buffer. I verified this with `hqa.c`: the output was `08 fc aa aa` on an 0xAA-filled buffer, and gcc emitted 255 "comma" warnings. It occurs at UsbUm Hid.c:822, UsbKm Hid.c:388, SpiKm Hid.c:692 and HidFilter Hid.c:245.
   - Fixing it, by sending the full 256-byte blob, changes what Windows sees for feature report 0x08.
2. **`PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` (report 0x06).**
   - The descriptor defines a 1-byte payload: bit 0 is button and bit 1 is surface.
   - UsbUm (Hid.c:1049-1052) and HidFilter (Hid.c:321-327) read byte 1, which is correct.
   - SpiKm (Hid.c:819-821) and UsbKm (Hid.c:526) read **byte 2**, because of the extra `DeviceMode` field. That byte is outside the 2-byte report. UsbKm only traces the values and never stores them.
3. **UsbUm `AmtPtpSetFeatures` always returns STATUS_SUCCESS** (Hid.c:1128), even for an unsupported ID or a failed USB transfer. It also does no length check before reading reports 0x04, 0x06 and 0x09 (Hid.c:943, 1049, 1081).
4. **Report 0x04 with Mode=0 (mouse) is handled differently in each driver:**
   - UsbUm turns Wellspring mode off (Hid.c:966-995).
   - UsbKm returns STATUS_NOT_SUPPORTED (Hid.c:481-489).
   - SpiKm sets `PtpInputOn=FALSE` (Hid.c:779-788).
   - HidFilter sets `PtpInputOn` to FALSE or TRUE, the same as SpiKm (Hid.c:301-314).
5. **UsbUm config table has no sentinel** (item 6 of A6, the config lookup).
6. **SpiKm advertises the UMAPP TLC but rejects report 0x09** (Hid.c:703-712 for get and 839-848 for set).
7. **The UMAPP values are never used.** `PressureQualLevel`, `SgContactSizeQualLevel` and `MuContactSizeQualLevel` are written and read only in UsbUm Hid.c (860-862, 1084-1086). They are zero-initialized by WDF, with no initializer in Device.c.

### A8. Proposed Rust crate split

Licensing constrains the split:
- LICENSE.md:5-6 puts UsbUm and UsbKm under GPLv2 and SpiKm under MIT.
- HidFilter is not mentioned and needs a decision.
- The bcm5974 tables and decode are on the GPL side, and HidFilter copies them.
- So do not put them in the crate that SpiKm depends on.

**`ptp-hid`: `no_std`, no WDK, MIT-compatible, host-testable**
- Report-ID and HID item-tag consts from A1.
- `HQA_BLOB: [u8; 256]` and `ptp_caps(max_contacts=5, button_type=0) -> [u8; 3]`.
- `const fn` descriptor builders:
  - `ptp_tlc(shape: ContactShape::{U32Id9, Spi3Bit5}, usage: TlcUsage::{TouchPad=0x05, TouchScreen=0x04}, axes: Axes{xl, xp, yp, yl}) -> [u8; 446 | 431]`
  - `WINDOWS_CONFIG_TLC: [u8; 47]`
  - `UMAPP_TLC: [u8; 23]`
  - `compose(...)`
  - `hid_descriptor(len) -> [u8; 9]` producing `09 21 00 01 00 01 22 lo hi`
- One `Axes` constant per family, taken from the table in A4.
- Explicit LE byte encoders and decoders rather than bitfield structs:
  - `PtpReport9::to_bytes() -> [u8; 50]`
  - `PtpReportSpi::to_bytes() -> [u8; 30]`
  - parsers for input mode `[u8; 2]`, selective mode and UMAPP `[u8; 4]`
  - `const` size asserts: 9, 5, 50, 30, 2, 3, 4 and 257.
- Golden tests against `um.txt`, `km.txt`, `spi.txt` and `flt.txt`. Together they cover 6 + 1 + 8 + 1 = 16 descriptors.

**`bcm5974`: `no_std`, GPL-2.0**
- Both config tables, each returning `Option` instead of relying on a sentinel. UsbKm keeps its fallback.
- `DATAFORMAT`-derived constants.
- `TrackpadFinger` (28 B) and `TrackpadFingerType5` (9 B) parsers.
- TYPE5 and TYPE1-4 coordinate decode, with the tip and confidence thresholds as parameters so each driver keeps its current behavior.
- A Wellspring mode-switch buffer helper.
- BT header size 4 and USB header size 12 for TYPE5.

**SPI-only `no_std` module in SpiKm**
- `SPI_TRACKPAD_PACKET` and finger parsing.
- `SpiTrackpadConfigTable`.

**Stays per-driver (WDK-bound)**
- WDF objects, queues and IOCTL dispatch:
  - UMDF uses `IOCTL_UMDF_HID_GET/SET_FEATURE`, at UsbUm Queue.c:176-186. It takes the report ID from input byte 0 for get (Hid.c:567-600) and from the **output buffer length** for set (Hid.c:640-681).
  - KMDF takes `HID_XFER_PACKET` from `Irp->UserBuffer`, at SpiKm Hid.c:753-764 and HidFilter Hid.c:194-205 and 280-292.
- USB pipes and control transfers.
- The SPI IO target and the `DesiredReportType` registry read.
- HidFilter's HIDCLASS patching (Detour.c, include/Hac.h).
- Perf-counter timing.
- WPP control GUIDs:
  - UsbUm efc3ce99-... (include/Trace.h:13)
  - UsbKm b409cf52-... (Trace.h:20)
  - SpiKm 9b319d80-... (Trace.h:20)
  - HidFilter eeb34803-... (include/Trace.h:8)
  - The TraceLogging provider 871B1E2D-... is used by `AmtPtpDevice.wprp:8` (include/ModernTrace.h:9).
- Device interface GUIDs (Public.h:24, HidFilter include/Device.h:8).
- Version resources: UsbUm Resource.rc is UTF-16 with FILEVERSION 18.1.2.200 and `OriginalFilename "AmtPtpDevice.dll"`; UsbKm is 19.0.0.200. SpiKm and HidFilter have no .rc file.

---

## B. Contract between the settings app and the drivers

### B1. What the app does

- **Input or settings device**
  - `HidDevice.GetDeviceSelector(0xff00, 0x0001, 0x05ac, 0x0265)` (`src/AmtPtpDevice.Settings/MainPage.xaml.cs:58`), opened ReadWrite on the first `Added` event (`Comm/UsbHidDeviceAccessSubscription.cs:61,80-85`).
- **Battery device**
  - `GetDeviceSelector(0xff00, 0x0014, 0x05ac, 0x0265)` (MainPage.xaml.cs:59).
- **Manifest**
  - `<Device Id="vidpid:05ac 0265">` with `usage:FF00 0001` and `usage:FF00 0014` (`Package.appxmanifest:27-34`, Device at :28, Function at :30 and :32).
- **Get feature report 0x09** (MainPage.xaml.cs:95-99), marshalled into `PtpUserModeConfReport` (`DataObjects/PtpUserModeConfReport.cs:9-14`).
  - 4 bytes: `[0]` ReportId=0x09, `[1]` PressureQualificationLevel, `[2]` SingleContactSizeQualificationLevel, `[3]` MultipleContactSizeQualificationLevel.
  - The values populate sliders at :103-105.
- **Send feature report 0x09** (MainPage.xaml.cs:126-134).
  - It writes exactly `09 P S M`, 4 bytes.
  - It sends on every slider `ValueChanged` (:143-156), but only after the initial get has succeeded (:120).
- **Get input report 0x90** on the battery device (MainPage.xaml.cs:68-72), marshalled into `Mt2BatteryStatusReport` (`DataObjects/Mt2BatteryStatusReport.cs:5-9`).
  - 3 bytes: `[0]` ReportId, `[1]` BatteryFlags, `[2]` ChargeStatus. ChargeStatus is shown as "%" at :75.

### B2. The driver side, byte-for-byte

- **Descriptor:** the 23-byte UMAPP TLC in A2. It is usage page 0xFF00 and usage 0x01, Feature, report ID 9, 3 × 8 bits, logical range 0..255, so the report length is 4.
  - It is present only in UsbUm (all families) and SpiKm.
  - UsbKm leaves it out (Hid.c:7-10), and so does HidFilter (StaticHidRegistry.h:15-18).
- **Get 0x09: only UsbUm implements it** (Hid.c:837-875).
  - It requires `reportBufferLen >= 4`; otherwise it returns STATUS_INVALID_BUFFER_SIZE.
  - It writes `09, PressureQualLevel, SgContactSizeQualLevel, MuContactSizeQualLevel` from `include/Device.h:19-21`.
  - It sets Information to 4.
- **Set 0x09: only UsbUm implements it** (Hid.c:1074-1110).
  - It has no length check.
  - It stores bytes `[1]`, `[2]` and `[3]` and sets Information to 4.
  - It returns STATUS_SUCCESS regardless of outcome (:1128).
- **SpiKm** returns STATUS_NOT_SUPPORTED for 0x09 (Hid.c:703-712 and 839-848). **UsbKm and HidFilter** have no case for it.
- **Battery collection 0xFF00/0x0014 and input report 0x90: no driver in this repo implements them.**
  - A grep of `src` for `0x90` and `battery` finds nothing.
  - So they are presumably served by the MT2's own firmware HID collection, through inbox HidUsb or HidBth. This is unverified.
  - Note that the INFs bind these MT2 collections to `NullDevice`:
    - Universal INF:91 binds BT `Col02`.
    - `AmtPtpHidFilter.inf:45-47` binds USB `Col02`/`Col03` and BT `Col02`.
  - That may hide the battery collection.

### B3. The contract is currently broken, and the decision is open

- **The two sides disagree on VID.** The app filters on VID 0x05AC (MainPage.xaml.cs:58, manifest :28).
- **Since commit dd364d6 (2021-06-02, "HID: update device ID to prevent some ID collision"), UsbUm reports `VendorID = 0x8910`** (Hid.c:225-227), and so do SpiKm and HidFilter. That commit added `DEVICE_VID` in 4 places.
  - HIDClass builds each TLC's hardware ID `HID\VID_v(4)&PID_d(4)...` from the minidriver's `IOCTL_HID_GET_DEVICE_ATTRIBUTES`. Source: Microsoft Learn, "HIDClass hardware IDs for top-level collections" and "IOCTL_HID_GET_DEVICE_ATTRIBUTES".
  - So UsbUm's vendor TLC (Col03) is expected to enumerate as `HID\VID_8910&PID_0265...`.
  - Neither the app's AQS filter nor its `vidpid:05ac 0265` capability would then match. I could not confirm directly that `System.DeviceInterface.Hid.VendorId` comes from the same attributes; this needs checking on hardware.
- **UsbKm reports the real VID 0x05AC**, but its PIDs are T2 PIDs, not 0x0265, and it has no UMAPP TLC or handler.
- **Over Bluetooth the native VID is 0x004C** (HidFilter `include/Device.h:15`, universal INF:90 `_VID&0001004c`). The app's 05AC filter cannot match BT collections in any case.
- **The app's code has not changed since 98f3af1 (2019-12-22).**
- **Open decision:**
  - (a) Keep 0x8910. This is byte-identical with today's drivers, and the unchanged app does not find the device.
  - (b) Report 0x05AC from the Rust UsbUm. The app could work again, but the collision that dd364d6 fixed comes back.
  - The requirement that the app stays unchanged rules out (c), changing the app's selector and manifest.
- **What must be preserved byte-for-byte whatever is decided:**
  - the UMAPP TLC bytes
  - report ID 0x09
  - the 4-byte layout `id, pressure, single, multi` and the get/set round-trip semantics
  - the PID passthrough (UsbUm reports the USB idProduct)

---

## C. Packaging, CI and documentation

### C1. Universal INF: `src/AmtPtpDeviceUniversalPkg/AmtPtpDevice.inf`

**Version section**
- Class HIDClass, `CatalogFile = AmtPtpDevice.cat` (:10).
- `DriverVer =` is empty and filled by stampinf (:11). `PnpLockdown = 1` (:12).

**Binary names are fixed by `[SourceDisksFiles]` (:22-25)**
- `AmtPtpDeviceUsbKm.sys`, `AmtPtpDeviceUsbUm.dll`, `AmtPtpDeviceSpiKm.sys`, `AmtPtpHidFilter.sys`.
- The package must contain these four files flat, next to the INF.
- All are copied to DriverStore run-from-store, `DestinationDirs` 13 (:14-16).
- The C UsbUm binary gets its name from `<ProjectName>AmtPtpDeviceUsbUm</ProjectName>` (MagicTrackpad2PtpDevice.vcxproj:62). Its .rc still says `AmtPtpDevice.dll`.

**`$ARCH$`** in `[Manufacturer] Standard,NT$ARCH$` (:28,30) is substituted by stampinf `-a`.

**Hardware ID bindings**

| Driver | Hardware IDs | INF lines |
|---|---|---|
| UsbKm | `USB\Vid_05ac&Pid_{0273,0274,0277,027A-027F,0280,0290,0291,0340}&MI_02` | :32-44 |
| UsbUm | Wellspring `MI_01` | :47-67 |
| UsbUm | `{0272,0273,0274,0290,0291}&MI_02` | :68-72 |
| UsbUm | MT2 USB `USB\Vid_05ac&Pid_0265&MI_01` | :88 |
| SpiKm | `SPI\VID_05ac&PID_{0272,0273,0275,0276,0277,0278,0279,0280,0290,0291}&MI_02` (no 027A-027F) | :75-84 |
| HidFilter | MT2 BT `HID\{00001124-...}_VID&0001004c_PID&0265&Col01` | :90 |
| NullDevice | MT2 BT `Col02` | :91 |

- **The same IDs are bound twice:** USB `0273`, `0274`, `0290` and `0291` `MI_02` go to both UsbKm (:33-34, 42-43) and UsbUm (:69-72). Rank and date decide which one wins.
- SPI `0280` (:82) has no entry in `SpiTrackpadConfigTable` or the descriptor switch.

**Install model**
- **KMDF drivers are lower filters under `mshidkmdf`, the function driver:**
  - `LowerFilters` at :133, :161 and :231.
  - mshidkmdf added with `SPSVCINST_ASSOCSERVICE` at :126, :154 and :225.
  - So the Rust KMDF binaries must still be HID minidrivers loaded as a lower filter, with `WdfFdoInitSetFilter`.
  - SpiKm also sets `LoadOrderGroup = Extended Base` (:239).
- **UMDF:**
  - `mshidumdf` is the function service (:185); `WUDFRd` is a lower filter (:204).
  - `UmdfDispatcher=NativeUSB`, `UmdfService=AmtPtpDeviceUsbUm` and the policy directives are at :188-196.
  - **`UmdfLibraryVersion=2.15.0` is hard-coded** (:199). `ServiceBinary=%13%\AmtPtpDeviceUsbUm.dll` (:200).
- **`KmdfLibraryVersion = $KMDFVERSION$` appears in three wdfsects:** :146 (HidFilter), :175 (UsbKm) and :245 (SpiKm).
  - Stampinf `-k` substitutes it, from the Inf item's `KmdfVersionNumber`, which is `$(KMDF_VERSION_MAJOR).$(KMDF_VERSION_MINOR)` of the project that owns the INF. Source: Microsoft Learn, "Stampinf task" and "Using INX Files".
  - The owning project is `AmtPtpDeviceUniversalPkg.vcxproj`. It sets no KMDF version, so it uses the WDK default. I could not determine the exact value without a build.
  - Only SpiKm pins KMDF 1.23 (AmtPtpDeviceSpiKm.vcxproj:75-76 and the same in each config through :131). UsbKm and HidFilter use the default.
  - **The Rust build must choose one KMDF version that satisfies all three KM binaries** and substitute it itself, either by running stampinf `-k` or by writing a literal.
  - Check which KMDF version the Rust WDK bindings target; do not assume it.

### C2. HidFilter's own INF: `src/AmtPtpHidFilter/AmtPtpHidFilter.inf` (built by AmtPtpHidFilter.vcxproj:214)

- It is a separate package: `CatalogFile=AmtPtpHidFilter.cat` (:10), `NT$ARCH$.10.0` (:32-34) and `$KMDFVERSION$` at :104.
- Its bindings differ from the universal INF:
  - MT2 USB `MI_01` migrates to inbox `input.inf` HID through `StandardUsbHid_Device` (:36, 52-62).
  - HidFilter binds the USB HID collection `HID\VID_05AC&PID_0265&REV_0855&MI_01&Col01` (:42) and BT Col01 (:43).
  - NullDevice gets USB Col02/Col03 and BT Col02 (:45-47).
- So the MT2 USB is claimed in **two conflicting ways**: UsbUm on raw USB in the universal INF, and inbox HID plus HidFilter in this INF.
- Decide whether the Rust build ships, drops or merges this INF.

### C3. Build, catalog and signing (MSBuild today)

- **Pkg project** (`AmtPtpDeviceUniversalPkg.vcxproj`):
  - ProjectReferences to all 4 drivers (:30-41) and `<Inf Include="AmtPtpDevice.inf"/>` (:44).
  - `DriverType Package` and `ConfigurationType Utility` (:66-67 and the same per config).
  - This is where Inf2Cat produces `AmtPtpDevice.cat`.
- **Signing modes:**
  - Debug and Release use `SignMode TestSign` (:119-126).
  - ReleaseSigned uses `ProductionSign` with `$(ProductionCertPath)` (:128-133), unless `$(BuildEnvironment)=='AzurePipeline'`, in which case it is `Off`.
  - The timestamp server is `http://timestamp.digicert.com`.
  - `DriverSign/FileDigestAlgorithm sha256` applies unconditionally in the Pkg (:134-137).
  - In UsbUm, UsbKm, SpiKm and HidFilter it is set **only in x64 ItemDefinitionGroups**: UsbUm .vcxproj:182,192,202; UsbKm :189,204,219; SpiKm :197,209,221; HidFilter :182,196,210.
  - **HidFilter has no ReleaseSigned SignMode or ProductionCertificate block at all.**
- **DriverVer stamping:** `TimeStamp` is `2021.$(PipelineReleaseID).1.1000` on AzurePipeline, otherwise `*` (:138-143).
- **SDK:** `WindowsTargetPlatformVersion` is `$(LatestTargetPlatformVersion)`. When `$(BuildEnvironment)=='Github'` it becomes `10.0.19041.0` (Pkg :58-60, UsbUm :68-70, UsbKm :58-60, SpiKm :63-65). HidFilter has no Github override.
- **Output paths:** `OutDir = $(SolutionDir)build\$(ProjectName)\$(Platform)\$(ConfigurationName)\` and `IntDir = $(SolutionDir)intermediate\...` in every vcxproj. UsbUm also puts `intermediate\$(Platform)\$(ConfigurationName)` on its include path for the WPP `.tmh` files (UsbUm vcxproj IncludePath lines).
- **What the Rust pipeline must reproduce:**
  - build the 4 binaries
  - stampinf for `$ARCH$`, `$KMDFVERSION$` and DriverVer
  - Inf2Cat
  - signtool with `/fd sha256` and the digicert timestamp
  - test-sign or production-sign selection

### C4. `es/New-CabinetFile.ps1`

- It defines one function, `New-CabinetFile` (:39-137). Nothing invokes it.
- **Parameters:**
  - `-Name`, the CAB file name.
  - `-ContentDirectory`. It uses non-recursive `Get-ChildItem` (:104), so it takes a flat file list.
  - `-DestinationDir`, which becomes the DDF `DestinationDir` inside the CAB (:93).
  - `-DestinationPath`, the output directory. It defaults to the current directory (:66) and is typed `string[]`.
  - `-NoClobber`.
- **What it does:**
  - It writes a MakeCAB DDF (MSZIP, size limits 0, :77-93) to `<DestinationPath>\<Name>.ddf` as ASCII.
  - It runs the hard-coded `C:\Windows\system32\makecab.exe /F` (:119).
  - **It throws on `ERROR:` lines only when `-Verbose` is set** (:122-127). Without `-Verbose`, a makecab failure is silent.
  - It deletes the DDF and returns the CAB's FileInfo.
- It was added for CI CAB creation in f0d3a81 (2020-02-22). The usual purpose is a hardware-dashboard submission CAB, which is an inference.
- **The file carries an Authenticode signature block** (:140-324, timestamped 2021-07-17, from commit ba97f8f "Use signed version"). Any edit invalidates the signature.
- It does not depend on the C build. **Recommendation: leave it untouched.** The Rust package layout only has to give it a flat directory: INF, CAT and the 4 binaries.

### C5. CI files

- **`.github/workflows/github-issue-sync.yml`** is the only workflow. It syncs GitHub issues to Azure DevOps (:1-27) and references no build outputs, .sln or .vcxproj.
- **There is no in-repo build CI.** `.github/workflows/main.yml` was deleted in 5f6a851 (2019-08-28).
- The real build is the external Azure DevOps pipeline. README:3 has its badge (`ligstd.visualstudio.com`, definitionId=22).
- The repo hooks into that pipeline through `$(BuildEnvironment)=='AzurePipeline'` and `$(PipelineReleaseID)` in the vcxprojs (C3).
- **Deleting the .sln and .vcxproj files will break that external pipeline.** It cannot be updated from this repo.
- The `'Github'` BuildEnvironment branch is dead, because no workflow sets it.

### C6. `AmtPtpDriver.sln`

- It lists the 5 projects (:17-26).
- Its `contrib` solution folder refers to `App Interface.md` (:10), which was deleted in dd2b7de (2019). This reference is stale.
- The file can be deleted whole once the Rust build replaces it.
- **Keep `AmtPtpDevice.Settings.sln`.**

### C7. README.md lines tied to the C build

| Line(s) | Content | What changes |
|---|---|---|
| :3 | Azure DevOps build badge | breaks with the pipeline |
| :25 | CI auto-build signing note | update |
| :29 | "Right click `AmtPtpDevice.inf`" | keep the INF name or update this |
| :39 | Device Manager names | the real INF strings are at INF:253-262 and already differ |
| :51-55 "For developers" | KMDF 1.23 for SPI/T2 (only SpiKm actually pins it); KMDF 1.15 for BT, which does not match the HidFilter vcxproj, where nothing is pinned; UMDF 2.15; "DDK 2004"; the `ReleaseSigned` configuration | all must be rewritten for cargo and the WDK toolchain |
| :68-70 | Device list claims MT3, added in 29d2778 | no MT3 PID exists in either INF; only 0x0265 |
| :99-100 and LICENSE.md:5-6 | Per-project licenses | must be restated per Rust crate (A8). HidFilter's license is unstated |

### C8. `.gitignore` and `.gitattributes`

- **`.gitignore`**
  - The C and VS build outputs are covered by :13-24: `[Dd]ebug/`, `[Rr]elease/`, `[Xx]64/`, `[Xx]86/`, `[Bb]uild/`, `bld/`, `[Bb]in/`, `[Oo]bj/`, `intermediate/`.
  - Keep the VS and UWP entries: `.vs/` (:27), `*.user` (:6), `AppPackages/` (:171), `[Bb]in/` and `[Oo]bj/`. The C# app still needs them.
  - **There is no `target/` entry.** Add `/target/`. Cargo.lock should be committed because these are binaries.
  - **Traps for a Rust layout:**
    - `[Bb]in/` (:22) ignores cargo's `src/bin/`.
    - `[Bb]uild/` (:20) ignores any directory named `build`.
    - `[Xx]64/` (:18) ignores directories named x64.
    - `*.pdb` (:55) is fine.
- **`.gitattributes`**
  - `* text=auto eol=lf` (:4), but the working-tree files are CRLF. That is a pre-existing inconsistency.
  - The linguist overrides for `*.c` and `*.h` (:6-7) should change to `*.rs` or be removed.
  - The UTF-16 `.rc` files are detected as binary.

---

## D. Architectures and configurations

**AmtPtpDriver.sln**
- Solution platforms are `{Debug, Release, ReleaseSigned} × {x64, ARM64}` (AmtPtpDriver.sln:28-35).
- All 5 projects are mapped Build and Deploy for all 6 combinations (:37-126).

**Driver vcxprojs**
- Each vcxproj declares the same 6 `ProjectConfiguration`s:
  - UsbUm MagicTrackpad2PtpDevice.vcxproj:4-27
  - UsbKm :4-27
  - SpiKm :4-27
  - HidFilter :4-27
  - Pkg :4-27
- `TargetVersion Windows10` and `DriverTargetPlatform Universal` throughout.
- UsbUm builds as a DLL with `UMDF_VERSION_MAJOR 2` (:76-78).
- **x86 was removed in 8e0cc7c (2021-10-02).** There is no 32-bit configuration.
- Rust targets: `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` for all 4 drivers. UsbUm becomes a UMDF cdylib; the other three are KMDF `.sys`.
- Map ReleaseSigned to a release profile plus a production-sign step.

**ARM64 signing gap**
- The ARM64 ItemDefinitionGroups of the 4 driver projects have no `FileDigestAlgorithm sha256` (see C3).
- Only the Pkg project sets it for all platforms.
- The Rust pipeline should use `/fd sha256` everywhere.

**Settings app, unchanged**
- `AmtPtpDevice.Settings.sln:11-18` has `{Debug, Release} × {ARM, x64, x86}`. ARM is 32-bit, and **there is no ARM64**.
- The csproj default platform is x86 (AmtPtpDevice.Settings.csproj:6), with UAP target and min version 10.0.15063.0 (:13-15).
- This is independent of the driver architectures. An ARM64 Windows install would run the app's x86 or ARM build under emulation; I have not verified this.

---

## Appendix: golden descriptor bytes

These dumps are copied from the fixtures. `um.txt` contains all 6 UsbUm families, `spi.txt` all 8 SPI variants, and `km.txt` and `flt.txt` contain T2 and HidFilter MT2.

UsbUm MT2, 516 bytes. HidFilter is the first 493 bytes. The T2 descriptor is this template plus the T2 patches from A4, also 493 bytes:
```
05 0d 09 05 a1 01 85 05 09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01
75 20 27 ff ff ff ff 09 51 81 02 05 01 26 bc 1d 75 10 55 0e 65 11 09 30 46 40 06 95 01 81 02 46
7d 04 26 c9 13 09 31 81 02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02 25 01 09 47 09 42 95 02 75 01
81 02 75 01 95 06 81 03 95 01 75 20 27 ff ff ff ff 09 51 81 02 05 01 26 bc 1d 75 10 55 0e 65 11
09 30 46 40 06 95 01 81 02 46 7d 04 26 c9 13 09 31 81 02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02
25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01 75 20 27 ff ff ff ff 09 51 81 02 05
01 26 bc 1d 75 10 55 0e 65 11 09 30 46 40 06 95 01 81 02 46 7d 04 26 c9 13 09 31 81 02 c0 05 0d
09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03 95 01 75 20 27 ff ff ff ff 09
51 81 02 05 01 26 bc 1d 75 10 55 0e 65 11 09 30 46 40 06 95 01 81 02 46 7d 04 26 c9 13 09 31 81
02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 75 01 95 06 81 03
95 01 75 20 27 ff ff ff ff 09 51 81 02 05 01 26 bc 1d 75 10 55 0e 65 11 09 30 46 40 06 95 01 81
02 46 7d 04 26 c9 13 09 31 81 02 c0 05 0d 55 0c 66 01 10 47 ff ff 00 00 27 ff ff 00 00 09 56 81
02 09 54 25 7f 75 08 81 02 05 09 09 01 25 01 75 01 81 02 95 07 81 03 05 0d 85 07 09 55 09 59 15
00 26 ff 00 75 08 95 02 b1 02 06 00 ff 85 08 09 c5 15 00 26 ff 00 75 08 96 00 01 b1 02 c0 05 0d
09 0e a1 01 85 04 09 22 a1 02 09 52 15 00 25 10 75 08 95 01 b1 02 c0 a1 00 85 06 09 57 09 58 75
01 95 02 25 01 b1 02 95 06 b1 03 c0 c0 06 00 ff 09 01 a1 01 85 09 09 01 15 00 26 ff 00 75 08 95
03 b1 02 c0
```

SPI series 1 touchpad, 501 bytes. The touchscreen variant differs only at byte 3 (`05` becomes `04`):
```
05 0d 09 05 a1 01 85 05 09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 95 01 75 03 25 03 09 51
81 02 75 01 95 03 81 03 05 01 26 aa 29 75 10 55 0e 65 11 09 30 46 5e 04 95 01 81 02 46 c7 02 26
af 1e 09 31 81 02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02 25 01 09 47 09 42 95 02 75 01 81 02 95
01 75 03 25 03 09 51 81 02 75 01 95 03 81 03 05 01 26 aa 29 75 10 55 0e 65 11 09 30 46 5e 04 95
01 81 02 46 c7 02 26 af 1e 09 31 81 02 45 00 55 00 65 00 c0 05 0d 09 22 a1 02 25 01 09 47 09 42
95 02 75 01 81 02 95 01 75 03 25 03 09 51 81 02 75 01 95 03 81 03 05 01 26 aa 29 75 10 55 0e 65
11 09 30 46 5e 04 95 01 81 02 46 c7 02 26 af 1e 09 31 81 02 c0 05 0d 09 22 a1 02 25 01 09 47 09
42 95 02 75 01 81 02 95 01 75 03 25 03 09 51 81 02 75 01 95 03 81 03 05 01 26 aa 29 75 10 55 0e
65 11 09 30 46 5e 04 95 01 81 02 46 c7 02 26 af 1e 09 31 81 02 45 00 55 00 65 00 c0 05 0d 09 22
a1 02 25 01 09 47 09 42 95 02 75 01 81 02 95 01 75 03 25 03 09 51 81 02 75 01 95 03 81 03 05 01
26 aa 29 75 10 55 0e 65 11 09 30 46 5e 04 95 01 81 02 46 c7 02 26 af 1e 09 31 81 02 c0 05 0d 55
0c 66 01 10 47 ff ff 00 00 27 ff ff 00 00 09 56 81 02 09 54 25 7f 75 08 81 02 05 09 09 01 25 01
75 01 81 02 95 07 81 03 05 0d 85 07 09 55 09 59 15 00 26 ff 00 75 08 95 02 b1 02 06 00 ff 85 08
09 c5 15 00 26 ff 00 75 08 96 00 01 b1 02 c0 05 0d 09 0e a1 01 85 04 09 22 a1 02 09 52 15 00 25
10 75 08 95 01 b1 02 c0 a1 00 85 06 09 57 09 58 75 01 95 02 25 01 b1 02 95 06 b1 03 c0 c0 06 00
ff 09 01 a1 01 85 09 09 01 15 00 26 ff 00 75 08 95 03 b1 02 c0
```

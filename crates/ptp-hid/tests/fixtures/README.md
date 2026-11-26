# ptp-hid golden fixtures

These files are the byte-for-byte oracle for the `ptp-hid` descriptor builders and the HQA blob. They were
generated from the C driver headers at commit `2edb0f2`. That is the last commit with the C sources under
`src/`; it differs from `29d2778` only in `.gitattributes`. Do not edit them by hand.

| File | Contents | Source in the C tree |
|---|---|---|
| `um.txt` | AmtPtpDeviceUsbUm: `AmtPtp{3,5,6,7a,8,Mt2}ReportDescriptor`, 516 bytes each | `src/AmtPtpDeviceUsbUm/include/StaticHidRegistry.h` |
| `km.txt` | AmtPtpDeviceUsbKm: `AmtPtpT2ReportDescriptor`, 493 bytes | `src/AmtPtpDeviceUsbKm/Hid.c` |
| `spi.txt` | AmtPtpDeviceSpiKm: `S1`, `S1T`, `S2`, `S2T`, `S3a`, `S3aT`, `S3b`, `S3bT`, 501 bytes each (`T` = touchscreen variant) | `src/AmtPtpDeviceSpiKm/Hid.c` (`AmtPtpSpiFamily{1,2,3a,3b}[Touchscreen]ReportDescriptor`) |
| `flt.txt` | AmtPtpHidFilter: `PtpReportDescriptorMagicTrackpad2`, 493 bytes | `src/AmtPtpHidFilter/include/Metadata/StaticHidRegistry.h` |
| `hqa.txt` | `DEFAULT_PTP_HQA_BLOB`, 256 bytes (SHA-256 `b57b851d567808906f61a0122273da05c972140f1007355390aaf5dda8a072af`) | `src/AmtPtpDeviceUsbUm/include/Hid.h`; identical in all four drivers |

## Format

- `<name> len=<n>` starts an entry. Hex bytes follow, whitespace-separated, until the next entry or `sizeof` line.
- `sizeof ...` lines record C struct sizes measured by the same harness, e.g. `SEL=2` in UsbUm but `SEL=3` in
  UsbKm/SpiKm (`PTP_DEVICE_SELECTIVE_REPORT_MODE_REPORT` has an extra `DeviceMode` byte there).

## How they were produced

The descriptor dumps (`um.txt`, `km.txt`, `spi.txt`, `flt.txt`) came from small C programs (`um.c`, `km.c`, `spi.c`, `flt.c`).
Each program included the driver's own headers against a stub `hidport.h`. It was compiled with MinGW gcc using MS
bitfield layout and printed `sizeof(array)` and the bytes. `hqa.txt` was extracted mechanically from the
`DEFAULT_PTP_HQA_BLOB` macro in all four drivers' headers. All four copies were byte-identical, and the SHA-256 above
was checked. The line endings were normalized to LF.

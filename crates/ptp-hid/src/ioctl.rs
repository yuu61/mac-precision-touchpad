//! HID IOCTL codes.
//!
//! bindgen in wdk-sys 0.5.2 cannot evaluate the function-like `CTL_CODE` / `HID_CTL_CODE` macros, so no
//! `IOCTL_HID_*` constants are generated. They are computed here with [`ctl_code`]. Every value is checked
//! at compile time against the table verified with a C program against the 10.0.28000.0 WDK headers.
//!
//! Sources: `km/hidport.h` (`HID_CTL_CODE` = `METHOD_NEITHER`, the minidriver IOCTLs and the
//! `IOCTL_UMDF_*` codes) and `shared/hidclass.h` (`HID_IN_CTL_CODE` / `HID_OUT_CTL_CODE` =
//! `METHOD_IN_DIRECT` / `METHOD_OUT_DIRECT`).
//!
//! Which driver uses which code:
//! - All four dispatch on the 9 `hidport.h` minidriver codes and on `IOCTL_UMDF_HID_GET_INPUT_REPORT`,
//!   `IOCTL_UMDF_HID_SET_OUTPUT_REPORT`, mostly to complete them with `STATUS_NOT_SUPPORTED`.
//! - `UsbUm` (UMDF) handles `IOCTL_UMDF_HID_GET_FEATURE` / `IOCTL_UMDF_HID_SET_FEATURE`.
//! - The three KMDF drivers handle `IOCTL_HID_GET_FEATURE` / `IOCTL_HID_SET_FEATURE`.
//! - `SpiKm` and `HidFilter` also *send* `IOCTL_HID_GET_DEVICE_ATTRIBUTES`, `IOCTL_HID_SET_FEATURE` and
//!   `IOCTL_HID_READ_REPORT` down their stacks.

/// `FILE_DEVICE_KEYBOARD`: the device type of every HID IOCTL.
pub const FILE_DEVICE_KEYBOARD: u32 = 0x0000_000B;
/// `METHOD_BUFFERED`.
pub const METHOD_BUFFERED: u32 = 0;
/// `METHOD_IN_DIRECT`.
pub const METHOD_IN_DIRECT: u32 = 1;
/// `METHOD_OUT_DIRECT`.
pub const METHOD_OUT_DIRECT: u32 = 2;
/// `METHOD_NEITHER`.
pub const METHOD_NEITHER: u32 = 3;
/// `FILE_ANY_ACCESS`.
pub const FILE_ANY_ACCESS: u32 = 0;

/// `CTL_CODE(DeviceType, Function, Method, Access)` from `devioctl.h`:
/// `(DeviceType << 16) | (Access << 14) | (Function << 2) | Method`.
#[must_use]
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

/// `HID_CTL_CODE(id)` (hidport.h): `CTL_CODE(FILE_DEVICE_KEYBOARD, id, METHOD_NEITHER, FILE_ANY_ACCESS)`.
#[must_use]
pub const fn hid_ctl_code(id: u32) -> u32 {
    ctl_code(FILE_DEVICE_KEYBOARD, id, METHOD_NEITHER, FILE_ANY_ACCESS)
}

/// `HID_BUFFER_CTL_CODE(id)` (hidclass.h): `CTL_CODE(FILE_DEVICE_KEYBOARD, id, METHOD_BUFFERED, FILE_ANY_ACCESS)`.
#[must_use]
pub const fn hid_buffer_ctl_code(id: u32) -> u32 {
    ctl_code(FILE_DEVICE_KEYBOARD, id, METHOD_BUFFERED, FILE_ANY_ACCESS)
}

/// `HID_IN_CTL_CODE(id)` (hidclass.h): `CTL_CODE(FILE_DEVICE_KEYBOARD, id, METHOD_IN_DIRECT, FILE_ANY_ACCESS)`.
#[must_use]
pub const fn hid_in_ctl_code(id: u32) -> u32 {
    ctl_code(FILE_DEVICE_KEYBOARD, id, METHOD_IN_DIRECT, FILE_ANY_ACCESS)
}

/// `HID_OUT_CTL_CODE(id)` (hidclass.h): `CTL_CODE(FILE_DEVICE_KEYBOARD, id, METHOD_OUT_DIRECT, FILE_ANY_ACCESS)`.
#[must_use]
pub const fn hid_out_ctl_code(id: u32) -> u32 {
    ctl_code(FILE_DEVICE_KEYBOARD, id, METHOD_OUT_DIRECT, FILE_ANY_ACCESS)
}

// ---- km/hidport.h: minidriver IOCTLs (METHOD_NEITHER) ----

/// `IOCTL_HID_GET_DEVICE_DESCRIPTOR` (0x000B0003): return the 9-byte `HID_DESCRIPTOR`.
pub const IOCTL_HID_GET_DEVICE_DESCRIPTOR: u32 = hid_ctl_code(0);
/// `IOCTL_HID_GET_REPORT_DESCRIPTOR` (0x000B0007): return the report descriptor.
pub const IOCTL_HID_GET_REPORT_DESCRIPTOR: u32 = hid_ctl_code(1);
/// `IOCTL_HID_READ_REPORT` (0x000B000B): read one input report (pended until data arrives).
pub const IOCTL_HID_READ_REPORT: u32 = hid_ctl_code(2);
/// `IOCTL_HID_WRITE_REPORT` (0x000B000F): write an output report. No driver supports it.
pub const IOCTL_HID_WRITE_REPORT: u32 = hid_ctl_code(3);
/// `IOCTL_HID_GET_STRING` (0x000B0013): get a device string (`UsbUm` queries USB; SpiKm/HidFilter forward).
pub const IOCTL_HID_GET_STRING: u32 = hid_ctl_code(4);
/// `IOCTL_HID_ACTIVATE_DEVICE` (0x000B001F). No driver supports it.
pub const IOCTL_HID_ACTIVATE_DEVICE: u32 = hid_ctl_code(7);
/// `IOCTL_HID_DEACTIVATE_DEVICE` (0x000B0023). No driver supports it.
pub const IOCTL_HID_DEACTIVATE_DEVICE: u32 = hid_ctl_code(8);
/// `IOCTL_HID_GET_DEVICE_ATTRIBUTES` (0x000B0027): return the 32-byte `HID_DEVICE_ATTRIBUTES`.
pub const IOCTL_HID_GET_DEVICE_ATTRIBUTES: u32 = hid_ctl_code(9);
/// `IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST` (0x000B002B). No driver supports it, so there is no HID
/// selective suspend.
pub const IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST: u32 = hid_ctl_code(10);

// ---- km/hidport.h: UMDF HID minidriver IOCTLs (METHOD_NEITHER) ----

/// `IOCTL_UMDF_HID_SET_FEATURE` (0x000B0053): UMDF set-feature (`UsbUm`).
pub const IOCTL_UMDF_HID_SET_FEATURE: u32 = hid_ctl_code(20);
/// `IOCTL_UMDF_HID_GET_FEATURE` (0x000B0057): UMDF get-feature (`UsbUm`).
pub const IOCTL_UMDF_HID_GET_FEATURE: u32 = hid_ctl_code(21);
/// `IOCTL_UMDF_HID_SET_OUTPUT_REPORT` (0x000B005B). No driver supports it.
pub const IOCTL_UMDF_HID_SET_OUTPUT_REPORT: u32 = hid_ctl_code(22);
/// `IOCTL_UMDF_HID_GET_INPUT_REPORT` (0x000B005F). No driver supports it.
pub const IOCTL_UMDF_HID_GET_INPUT_REPORT: u32 = hid_ctl_code(23);
/// `IOCTL_UMDF_GET_PHYSICAL_DESCRIPTOR` (0x000B0063). No driver handles it; it falls into the default
/// `STATUS_NOT_SUPPORTED` branch.
pub const IOCTL_UMDF_GET_PHYSICAL_DESCRIPTOR: u32 = hid_ctl_code(24);

// ---- shared/hidclass.h ----

/// `IOCTL_HID_SET_FEATURE` (0x000B0191, `METHOD_IN_DIRECT`): KMDF set-feature with a `HID_XFER_PACKET` in
/// `Irp->UserBuffer`. `SpiKm` and `HidFilter` also send it down to switch the device into multitouch mode.
pub const IOCTL_HID_SET_FEATURE: u32 = hid_in_ctl_code(100);
/// `IOCTL_HID_SET_OUTPUT_REPORT` (0x000B0195, `METHOD_IN_DIRECT`). No driver handles it.
pub const IOCTL_HID_SET_OUTPUT_REPORT: u32 = hid_in_ctl_code(101);
/// `IOCTL_HID_GET_FEATURE` (0x000B0192, `METHOD_OUT_DIRECT`): KMDF get-feature with a `HID_XFER_PACKET` in
/// `Irp->UserBuffer`.
pub const IOCTL_HID_GET_FEATURE: u32 = hid_out_ctl_code(100);
/// `IOCTL_HID_GET_INPUT_REPORT` (0x000B01A2, `METHOD_OUT_DIRECT`). No driver handles it.
pub const IOCTL_HID_GET_INPUT_REPORT: u32 = hid_out_ctl_code(104);
/// `IOCTL_HID_GET_INDEXED_STRING` (0x000B01E2, `METHOD_OUT_DIRECT`). No driver handles it.
pub const IOCTL_HID_GET_INDEXED_STRING: u32 = hid_out_ctl_code(120);

// Compile-time cross-check against the values printed by the C compiler from the 10.0.28000.0 headers
// (windows-drivers-rs spec §2, "IOCTL constants").
const _: () = assert!(IOCTL_HID_GET_DEVICE_DESCRIPTOR == 0x000B_0003);
const _: () = assert!(IOCTL_HID_GET_REPORT_DESCRIPTOR == 0x000B_0007);
const _: () = assert!(IOCTL_HID_READ_REPORT == 0x000B_000B);
const _: () = assert!(IOCTL_HID_WRITE_REPORT == 0x000B_000F);
const _: () = assert!(IOCTL_HID_GET_STRING == 0x000B_0013);
const _: () = assert!(IOCTL_HID_ACTIVATE_DEVICE == 0x000B_001F);
const _: () = assert!(IOCTL_HID_DEACTIVATE_DEVICE == 0x000B_0023);
const _: () = assert!(IOCTL_HID_GET_DEVICE_ATTRIBUTES == 0x000B_0027);
const _: () = assert!(IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST == 0x000B_002B);
const _: () = assert!(IOCTL_UMDF_HID_SET_FEATURE == 0x000B_0053);
const _: () = assert!(IOCTL_UMDF_HID_GET_FEATURE == 0x000B_0057);
const _: () = assert!(IOCTL_UMDF_HID_SET_OUTPUT_REPORT == 0x000B_005B);
const _: () = assert!(IOCTL_UMDF_HID_GET_INPUT_REPORT == 0x000B_005F);
const _: () = assert!(IOCTL_UMDF_GET_PHYSICAL_DESCRIPTOR == 0x000B_0063);
const _: () = assert!(IOCTL_HID_SET_FEATURE == 0x000B_0191);
const _: () = assert!(IOCTL_HID_GET_FEATURE == 0x000B_0192);
const _: () = assert!(IOCTL_HID_SET_OUTPUT_REPORT == 0x000B_0195);
const _: () = assert!(IOCTL_HID_GET_INPUT_REPORT == 0x000B_01A2);
const _: () = assert!(IOCTL_HID_GET_INDEXED_STRING == 0x000B_01E2);

/// Returns the symbolic name of an IOCTL code for tracing, or `"IOCTL_UNKNOWN"`.
///
/// This replaces the four C `DbgIoControlGetString` / `PtpFilterDiagnosticsIoControlGetString` tables,
/// which are trace-only. It knows every code defined in this module, which is a superset of each C table.
#[must_use]
pub const fn ioctl_name(code: u32) -> &'static str {
    match code {
        IOCTL_HID_GET_DEVICE_DESCRIPTOR => "IOCTL_HID_GET_DEVICE_DESCRIPTOR",
        IOCTL_HID_GET_REPORT_DESCRIPTOR => "IOCTL_HID_GET_REPORT_DESCRIPTOR",
        IOCTL_HID_READ_REPORT => "IOCTL_HID_READ_REPORT",
        IOCTL_HID_WRITE_REPORT => "IOCTL_HID_WRITE_REPORT",
        IOCTL_HID_GET_STRING => "IOCTL_HID_GET_STRING",
        IOCTL_HID_ACTIVATE_DEVICE => "IOCTL_HID_ACTIVATE_DEVICE",
        IOCTL_HID_DEACTIVATE_DEVICE => "IOCTL_HID_DEACTIVATE_DEVICE",
        IOCTL_HID_GET_DEVICE_ATTRIBUTES => "IOCTL_HID_GET_DEVICE_ATTRIBUTES",
        IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST => "IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST",
        IOCTL_UMDF_HID_SET_FEATURE => "IOCTL_UMDF_HID_SET_FEATURE",
        IOCTL_UMDF_HID_GET_FEATURE => "IOCTL_UMDF_HID_GET_FEATURE",
        IOCTL_UMDF_HID_SET_OUTPUT_REPORT => "IOCTL_UMDF_HID_SET_OUTPUT_REPORT",
        IOCTL_UMDF_HID_GET_INPUT_REPORT => "IOCTL_UMDF_HID_GET_INPUT_REPORT",
        IOCTL_UMDF_GET_PHYSICAL_DESCRIPTOR => "IOCTL_UMDF_GET_PHYSICAL_DESCRIPTOR",
        IOCTL_HID_SET_FEATURE => "IOCTL_HID_SET_FEATURE",
        IOCTL_HID_GET_FEATURE => "IOCTL_HID_GET_FEATURE",
        IOCTL_HID_SET_OUTPUT_REPORT => "IOCTL_HID_SET_OUTPUT_REPORT",
        IOCTL_HID_GET_INPUT_REPORT => "IOCTL_HID_GET_INPUT_REPORT",
        IOCTL_HID_GET_INDEXED_STRING => "IOCTL_HID_GET_INDEXED_STRING",
        _ => "IOCTL_UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctl_code_matches_devioctl_formula() {
        assert_eq!(
            ctl_code(0x22, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS),
            0x0022_2000
        );
        // FILE_READ_ACCESS = 1 lands in bits 14-15.
        assert_eq!(
            ctl_code(FILE_DEVICE_KEYBOARD, 0, METHOD_NEITHER, 1),
            0x000B_4003
        );
        assert_eq!(hid_buffer_ctl_code(104), 0x000B_01A0);
    }

    #[test]
    fn method_bits_match_header_families() {
        assert_eq!(IOCTL_HID_READ_REPORT & 3, METHOD_NEITHER);
        assert_eq!(IOCTL_UMDF_HID_GET_FEATURE & 3, METHOD_NEITHER);
        assert_eq!(IOCTL_HID_SET_FEATURE & 3, METHOD_IN_DIRECT);
        assert_eq!(IOCTL_HID_GET_FEATURE & 3, METHOD_OUT_DIRECT);
    }

    #[test]
    fn names_round_trip_and_unknown() {
        let all = [
            IOCTL_HID_GET_DEVICE_DESCRIPTOR,
            IOCTL_HID_GET_REPORT_DESCRIPTOR,
            IOCTL_HID_READ_REPORT,
            IOCTL_HID_WRITE_REPORT,
            IOCTL_HID_GET_STRING,
            IOCTL_HID_ACTIVATE_DEVICE,
            IOCTL_HID_DEACTIVATE_DEVICE,
            IOCTL_HID_GET_DEVICE_ATTRIBUTES,
            IOCTL_HID_SEND_IDLE_NOTIFICATION_REQUEST,
            IOCTL_UMDF_HID_SET_FEATURE,
            IOCTL_UMDF_HID_GET_FEATURE,
            IOCTL_UMDF_HID_SET_OUTPUT_REPORT,
            IOCTL_UMDF_HID_GET_INPUT_REPORT,
            IOCTL_UMDF_GET_PHYSICAL_DESCRIPTOR,
            IOCTL_HID_SET_FEATURE,
            IOCTL_HID_GET_FEATURE,
            IOCTL_HID_SET_OUTPUT_REPORT,
            IOCTL_HID_GET_INPUT_REPORT,
            IOCTL_HID_GET_INDEXED_STRING,
        ];
        for (i, a) in all.iter().enumerate() {
            assert!(ioctl_name(*a).starts_with("IOCTL_"));
            assert_ne!(ioctl_name(*a), "IOCTL_UNKNOWN");
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "duplicate IOCTL code");
            }
        }
        assert_eq!(ioctl_name(IOCTL_HID_GET_FEATURE), "IOCTL_HID_GET_FEATURE");
        assert_eq!(ioctl_name(0), "IOCTL_UNKNOWN");
        assert_eq!(ioctl_name(0x000B_019A), "IOCTL_UNKNOWN");
    }
}

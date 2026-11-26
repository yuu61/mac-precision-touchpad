//! `Bcm5974ConfigTable` (AppleDefinition.h:149-214) and the PID lookup of `AmtPtpGetDeviceConfig`
//! (Device.c:25-48).
//!
//! The C table is terminated by a `{0}` sentinel row and the lookup always succeeds: it scans from row 0
//! (the 0xFFFF "new device?" fallback) and returns row 0 when no row matches. Here the table has no
//! sentinel; the scan is bounds-checked and falls back to [`FALLBACK_CONFIG_INDEX`].
//!
//! All five rows use the TYPE4 data format. The rows differ only in `x` and `y`; the parser reads
//! `x.min` and `y.max`. Every other field is kept so the table can be checked against the compiled C
//! table, but it is never read at runtime (spec §8.1 marks them DROP).

/// `enum TRACKPAD_TYPE` (AppleDefinition.h:21-27). Only [`TrackpadType::Type4`] is used by any row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum TrackpadType {
    /// `TYPE1`, plain trackpad.
    Type1 = 0,
    /// `TYPE2`, button integrated in the trackpad.
    Type2 = 1,
    /// `TYPE3`, additional header fields since June 2013.
    Type3 = 2,
    /// `TYPE4`, additional header field for pressure data. The T2 format.
    Type4 = 3,
    /// `TYPE5`, Magic Trackpad 2 format.
    Type5 = 4,
}

impl TrackpadType {
    /// `HEADER_TYPEn` in bytes (AppleDefinition.h:30-34).
    pub const fn header(self) -> i32 {
        match self {
            Self::Type1 => 26,
            Self::Type2 => 30,
            Self::Type3 => 38,
            Self::Type4 => 46,
            Self::Type5 => 12,
        }
    }

    /// `FSIZE_TYPEn`, the size of one finger block in bytes (AppleDefinition.h:47-51).
    pub const fn finger_size(self) -> i32 {
        match self {
            Self::Type1 | Self::Type2 | Self::Type3 => 28,
            Self::Type4 => 30,
            Self::Type5 => 9,
        }
    }

    /// `BUTTON_TYPEn`, the offset of the button byte (AppleDefinition.h:37-41).
    pub const fn button(self) -> i32 {
        match self {
            Self::Type1 => 0,
            Self::Type2 => 15,
            Self::Type3 => 23,
            Self::Type4 => 31,
            Self::Type5 => 1,
        }
    }

    /// `DELTA_TYPEn`, the offset from a finger block to its `TRACKPAD_FINGER` struct (AppleDefinition.h:54-58).
    pub const fn delta(self) -> i32 {
        match self {
            Self::Type4 => 2,
            Self::Type1 | Self::Type2 | Self::Type3 | Self::Type5 => 0,
        }
    }
}

/// `MAX_FINGERS` (AppleDefinition.h:72): finger blocks per packet the reader buffer is sized for.
pub const MAX_FINGERS: usize = 16;
/// `MAX_FINGER_ORIENTATION` (AppleDefinition.h:73).
pub const MAX_FINGER_ORIENTATION: i32 = 16384;
/// `USB_VENDOR_ID_APPLE` (AppleDefinition.h:3). Not compared at runtime.
pub const USB_VENDOR_ID_APPLE: u16 = 0x05AC;
/// `USB_DEVICE_ID_DEFAULT_FALLBACK` (AppleDefinition.h:10): `identification` of row 0.
pub const USB_DEVICE_ID_DEFAULT_FALLBACK: u16 = 0xFFFF;
/// `USB_DEVICE_ID_APPLE_T2_7A` (13 inch).
pub const USB_DEVICE_ID_APPLE_T2_7A: u16 = 0x027A;
/// `USB_DEVICE_ID_APPLE_T2_7B` (13 inch).
pub const USB_DEVICE_ID_APPLE_T2_7B: u16 = 0x027B;
/// `USB_DEVICE_ID_APPLE_T2_7C` (15 inch).
pub const USB_DEVICE_ID_APPLE_T2_7C: u16 = 0x027C;
/// `USB_DEVICE_ID_APPLE_T2_7D` (15 inch).
pub const USB_DEVICE_ID_APPLE_T2_7D: u16 = 0x027D;

/// `HAS_INTEGRATED_BUTTON` (AppleDefinition.h:44).
pub const HAS_INTEGRATED_BUTTON: i32 = 1;
/// `sizeof(struct TRACKPAD_BUTTON_DATA)` (AppleDefinition.h:13-18).
pub const TRACKPAD_BUTTON_DATA_LEN: i32 = 4;
/// `SN_PRESSURE`.
pub const SN_PRESSURE: i32 = 45;
/// `SN_WIDTH`.
pub const SN_WIDTH: i32 = 25;
/// `SN_COORD`.
pub const SN_COORD: i32 = 250;
/// `SN_ORIENT`.
pub const SN_ORIENT: i32 = 10;

/// `struct BCM5974_PARAM` (AppleDefinition.h:95-99).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Bcm5974Param {
    /// Signal-to-noise ratio.
    pub snratio: i32,
    /// Device minimum reading.
    pub min: i32,
    /// Device maximum reading.
    pub max: i32,
}

/// `struct BCM5974_CONFIG` (AppleDefinition.h:102-125), field for field, with the C `int` types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Bcm5974Config {
    /// The USB product ID of this row (compared with `idProduct` promoted to `int`).
    pub identification: i32,
    /// Device capability bitmask. Unused.
    pub caps: i32,
    /// Endpoint of the button interface. Unused.
    pub bt_ep: i32,
    /// Data length of the button interface. Unused.
    pub bt_datalen: i32,
    /// Endpoint of the trackpad interface (0x83). Unused: the first interrupt pipe is taken instead.
    pub tp_ep: i32,
    /// Data format.
    pub tp_type: TrackpadType,
    /// Bytes in the header block.
    pub tp_header: i32,
    /// Data length of the trackpad interface. Unused (the reader length is computed from `tp_type`).
    pub tp_datalen: i32,
    /// Offset of the button byte.
    pub tp_button: i32,
    /// Bytes in a single finger block.
    pub tp_fsize: i32,
    /// Offset from a finger block to its finger struct.
    pub tp_delta: i32,
    /// USB control message length the C allocates (2). The transfers use 4, see
    /// [`crate::wellspring::WELLSPRING_DATA_LEN`].
    pub um_size: i32,
    /// USB control message `wValue` (0x0302: feature report 2).
    pub um_req_val: i32,
    /// USB control message `wIndex` (2: interface 2).
    pub um_req_idx: i32,
    /// Index of the mode byte in the control message data.
    pub um_switch_idx: i32,
    /// Mode byte value for "wellspring mode on".
    pub um_switch_on: i32,
    /// Mode byte value for "wellspring mode off".
    pub um_switch_off: i32,
    /// Finger pressure limits. Unused.
    pub p: Bcm5974Param,
    /// Finger width limits. Unused.
    pub w: Bcm5974Param,
    /// Horizontal limits; `x.min` is read by the parser.
    pub x: Bcm5974Param,
    /// Vertical limits; `y.max` is read by the parser.
    pub y: Bcm5974Param,
    /// Orientation limits. Unused.
    pub o: Bcm5974Param,
}

const fn param(snratio: i32, min: i32, max: i32) -> Bcm5974Param {
    Bcm5974Param { snratio, min, max }
}

/// One TYPE4 row: `{ id, HAS_INTEGRATED_BUTTON, 0, sizeof(TRACKPAD_BUTTON_DATA), 0x83, DATAFORMAT(TYPE4),
/// p, w, x, y, o }`.
const fn type4_row(identification: u16, x: Bcm5974Param, y: Bcm5974Param) -> Bcm5974Config {
    let t = TrackpadType::Type4;
    Bcm5974Config {
        identification: identification as i32, // u16 -> int, value preserving
        caps: HAS_INTEGRATED_BUTTON,
        bt_ep: 0,
        bt_datalen: TRACKPAD_BUTTON_DATA_LEN,
        tp_ep: 0x83,
        tp_type: t,
        tp_header: t.header(),
        tp_datalen: t.header() + (MAX_FINGERS as i32) * t.finger_size(), // 46 + 16 * 30 = 526
        tp_button: t.button(),
        tp_fsize: t.finger_size(),
        tp_delta: t.delta(),
        // USBMSG_TYPE4 = 2, 0x302, 2, 1, 0x1, 0x0
        um_size: 2,
        um_req_val: 0x302,
        um_req_idx: 2,
        um_switch_idx: 1,
        um_switch_on: 0x1,
        um_switch_off: 0x0,
        p: param(SN_PRESSURE, 0, 300),
        w: param(SN_WIDTH, 0, 2048),
        x,
        y,
        o: param(SN_ORIENT, -MAX_FINGER_ORIENTATION, MAX_FINGER_ORIENTATION),
    }
}

/// Row 0 of the table: the "New device?" generic fallback (identification 0xFFFF).
pub const FALLBACK_CONFIG: Bcm5974Config = type4_row(
    USB_DEVICE_ID_DEFAULT_FALLBACK,
    param(SN_COORD, -10000, 10000),
    param(SN_COORD, -2000, 10000),
);

/// `Bcm5974ConfigTable` without its `{0}` sentinel.
pub static CONFIG_TABLE: [Bcm5974Config; 5] = [
    // "New device?" generic fallback.
    FALLBACK_CONFIG,
    // 13 inch.
    type4_row(
        USB_DEVICE_ID_APPLE_T2_7A,
        param(SN_COORD, -6243, 6749),
        param(SN_COORD, -170, 7685),
    ),
    type4_row(
        USB_DEVICE_ID_APPLE_T2_7B,
        param(SN_COORD, -6243, 6749),
        param(SN_COORD, -170, 7685),
    ),
    // 15 inch.
    type4_row(
        USB_DEVICE_ID_APPLE_T2_7C,
        param(SN_COORD, -10000, 10000),
        param(SN_COORD, -2000, 10000),
    ),
    type4_row(
        USB_DEVICE_ID_APPLE_T2_7D,
        param(SN_COORD, -10000, 10000),
        param(SN_COORD, -2000, 10000),
    ),
];

/// Index of the generic fallback row ([`FALLBACK_CONFIG`]) that unknown PIDs get.
pub const FALLBACK_CONFIG_INDEX: usize = 0;

/// `AmtPtpGetDeviceConfig`: the index of the row whose `identification` equals `product_id`, scanning
/// from row 0; [`FALLBACK_CONFIG_INDEX`] when no row matches (the C traces "Selected a generic fallback
/// configuration" in that case).
pub fn config_index_for_pid(product_id: u16) -> usize {
    // C compares `int identification` with the USHORT `idProduct` promoted to int.
    let id = i32::from(product_id);
    CONFIG_TABLE
        .iter()
        .position(|row| row.identification == id)
        .unwrap_or(FALLBACK_CONFIG_INDEX)
}

/// The row for `index`, or `None` if out of range.
pub fn config_by_index(index: usize) -> Option<&'static Bcm5974Config> {
    CONFIG_TABLE.get(index)
}

/// `AmtPtpGetDeviceConfig`: the row for `product_id` (never fails; unknown PIDs get the fallback row).
pub fn config_for_pid(product_id: u16) -> &'static Bcm5974Config {
    config_by_index(config_index_for_pid(product_id)).unwrap_or(&FALLBACK_CONFIG)
}

/// `transferLength` of `AmtPtpConfigContReaderForInterruptEndPoint` (Interrupt.c:30-47):
/// `HEADER_TYPEn + FSIZE_TYPEn * MAX_FINGERS` (526 for TYPE4). `size_t` arithmetic on positive constants.
pub const fn reader_transfer_length(tp_type: TrackpadType) -> usize {
    // Both constants are small positive ints, so `as usize` is value preserving.
    tp_type.header() as usize + tp_type.finger_size() as usize * MAX_FINGERS
}

// sizeof(struct BCM5974_CONFIG) == 128 in the C build (descriptors.txt `layout`).
const _: () = assert!(core::mem::size_of::<Bcm5974Config>() == 128);
const _: () = assert!(reader_transfer_length(TrackpadType::Type4) == 526);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_and_fallback() {
        assert_eq!(config_index_for_pid(0x027A), 1);
        assert_eq!(config_index_for_pid(0x027B), 2);
        assert_eq!(config_index_for_pid(0x027C), 3);
        assert_eq!(config_index_for_pid(0x027D), 4);
        assert_eq!(config_index_for_pid(0xFFFF), 0);
        for pid in [0x0000, 0x0273, 0x0274, 0x0340, 0x8000] {
            assert_eq!(config_index_for_pid(pid), FALLBACK_CONFIG_INDEX);
            assert_eq!(config_for_pid(pid).identification, 0xFFFF);
        }
        assert_eq!(config_for_pid(0x027A).x.min, -6243);
        assert_eq!(config_for_pid(0x027A).y.max, 7685);
        assert!(config_by_index(5).is_none());
    }

    #[test]
    fn reader_lengths() {
        assert_eq!(reader_transfer_length(TrackpadType::Type1), 474);
        assert_eq!(reader_transfer_length(TrackpadType::Type2), 478);
        assert_eq!(reader_transfer_length(TrackpadType::Type3), 486);
        assert_eq!(reader_transfer_length(TrackpadType::Type4), 526);
        assert_eq!(reader_transfer_length(TrackpadType::Type5), 156);
    }
}

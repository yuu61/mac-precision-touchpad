//! `Bcm5974ConfigTable` (AppleDefinition.h:221-418) and the PID lookup (`AmtPtpGetDeviceConfig`,
//! Device.c:6-22).
//!
//! Every field of `struct BCM5974_CONFIG` is a C `int`, so they are stored as `i32` here, exactly as the C
//! table holds them (the `DATAFORMAT(type)` and `USBMSG_TYPEn` macros are expanded by the private row
//! constructor). Conversions to `usize` mirror the C `(unsigned int)` casts and live in the accessor
//! methods.
//!
//! UM-B3 (P2 FIXED): the C table has 15 rows and no `{0}` sentinel, but the lookup loops `while
//! (cfg->ansi)`. For an unknown PID the C code reads past the array (UB). [`lookup_config_row`] iterates
//! the 15 rows only and returns `None` ("not found"), which is what the C code evidently intended.
//! UM-B23 (P3 PRESERVED): the vendor ID is not checked; only `idProduct` is matched.

use ptp_hid::common::MAX_FINGERS;

/// `USB_VENDOR_ID_APPLE`. Defined by the C headers but never compared (UM-B23).
pub const USB_VENDOR_ID_APPLE: u16 = 0x05ac;

/// `HAS_INTEGRATED_BUTTON`: the only capability bit (stored in `caps`, never read).
pub const HAS_INTEGRATED_BUTTON: i32 = 1;

/// `sizeof(struct TRACKPAD_BUTTON_DATA)`: the `bt_datalen` of every row (never read).
pub const TRACKPAD_BUTTON_DATA_LEN: i32 = 4;

/// `MAX_FINGER_ORIENTATION` (orientation limits, never read).
pub const MAX_FINGER_ORIENTATION: i32 = 16384;

/// `SN_PRESSURE` (pressure signal-to-noise ratio, never read).
pub const SN_PRESSURE: i32 = 45;
/// `SN_WIDTH` (width signal-to-noise ratio, never read).
pub const SN_WIDTH: i32 = 25;
/// `SN_COORD` (coordinate signal-to-noise ratio, never read).
pub const SN_COORD: i32 = 250;
/// `SN_ORIENT` (orientation signal-to-noise ratio, never read).
pub const SN_ORIENT: i32 = 10;

/// `enum TRACKPAD_TYPE` (AppleDefinition.h:71-77). Ordinals as in C: TYPE1 = 0 ... TYPE5 = 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TrackpadType {
    /// `TYPE1`: plain trackpad (WS1, WS2). Input is never parsed (UM-B24).
    Type1 = 0,
    /// `TYPE2`: button integrated in trackpad (WS3..WS7A).
    Type2 = 1,
    /// `TYPE3`: additional header fields since June 2013 (WS8). No mode-switch I/O.
    Type3 = 2,
    /// `TYPE4`: additional header field for pressure data (WS9, T2).
    Type4 = 3,
    /// `TYPE5`: Magic Trackpad 2 format.
    Type5 = 4,
}

impl TrackpadType {
    /// `HEADER_TYPEn`: bytes before the first finger block.
    pub const fn header(self) -> i32 {
        // `n * sizeof(USHORT)`
        match self {
            Self::Type1 => 13 * 2,
            Self::Type2 => 15 * 2,
            Self::Type3 => 19 * 2,
            Self::Type4 => 23 * 2,
            Self::Type5 => 6 * 2,
        }
    }

    /// `BUTTON_TYPEn`: offset of the button byte.
    pub const fn button(self) -> i32 {
        match self {
            Self::Type1 => 0,
            Self::Type2 => 15,
            Self::Type3 => 23,
            Self::Type4 => 31,
            Self::Type5 => 1,
        }
    }

    /// `FSIZE_TYPEn`: bytes per finger block (the stride).
    pub const fn fsize(self) -> i32 {
        match self {
            Self::Type1 | Self::Type2 | Self::Type3 => 14 * 2,
            Self::Type4 => 15 * 2,
            Self::Type5 => 9,
        }
    }

    /// `DELTA_TYPEn`: offset from the header end to the finger struct inside a finger block.
    pub const fn delta(self) -> i32 {
        match self {
            Self::Type4 => 2,
            _ => 0,
        }
    }

    /// `USBMSG_TYPEn`: the mode-switch control-message parameters.
    pub const fn usb_msg(self) -> UsbMsg {
        match self {
            Self::Type1 | Self::Type2 | Self::Type3 => UsbMsg {
                um_size: 8,
                um_req_val: 0x300,
                um_req_idx: 0,
                um_switch_idx: 0,
                um_switch_on: 0x1,
                um_switch_off: 0x8,
            },
            Self::Type4 => UsbMsg {
                um_size: 2,
                um_req_val: 0x302,
                um_req_idx: 2,
                um_switch_idx: 1,
                um_switch_on: 0x1,
                um_switch_off: 0x0,
            },
            Self::Type5 => UsbMsg {
                um_size: 2,
                um_req_val: 0x302,
                um_req_idx: 1,
                um_switch_idx: 1,
                um_switch_on: 0x1,
                um_switch_off: 0x0,
            },
        }
    }

    /// Continuous-reader transfer length of `AmtPtpConfigContReaderForInterruptEndPoint`
    /// (InputInterrupt.c:23-40): `HEADER_TYPEn + FSIZE_TYPEn * MAX_FINGERS` = 474/478/486/526/156. It equals
    /// `tp_datalen` for every type.
    pub const fn reader_transfer_length(self) -> usize {
        // All operands are small positive constants; the C computes this in `size_t`.
        (self.header() + self.fsize() * MAX_FINGERS as i32) as usize
    }

    /// The C enumerator name (`"TYPE1"` ... `"TYPE5"`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Type1 => "TYPE1",
            Self::Type2 => "TYPE2",
            Self::Type3 => "TYPE3",
            Self::Type4 => "TYPE4",
            Self::Type5 => "TYPE5",
        }
    }
}

/// The six `USBMSG_TYPEn` values of a row (`um_size` ... `um_switch_off`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbMsg {
    /// `um_size`: the C allocation size (8 or 2). Not the wire length: see [`crate::usb`] (UM-B1).
    pub um_size: i32,
    /// `um_req_val`: wValue (Feature report, ID 0 or 2).
    pub um_req_val: i32,
    /// `um_req_idx`: wIndex.
    pub um_req_idx: i32,
    /// `um_switch_idx`: index of the mode byte in the transfer buffer.
    pub um_switch_idx: i32,
    /// `um_switch_on`: mode byte value for "Wellspring mode on".
    pub um_switch_on: i32,
    /// `um_switch_off`: mode byte value for "Wellspring mode off".
    pub um_switch_off: i32,
}

/// `struct BCM5974_PARAM`: `{snratio, min, max}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bcm5974Param {
    /// Signal-to-noise ratio (never read).
    pub snratio: i32,
    /// Device minimum reading.
    pub min: i32,
    /// Device maximum reading.
    pub max: i32,
}

/// Row names, in table order. The names match the C comments and the oracle fixtures (`fam=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// Row 0: MacBookAir, "wellspring".
    Ws1,
    /// Row 1: MacBookPro Penryn, "wellspring2".
    Ws2,
    /// Row 2: MacBook5,1, "wellspring3".
    Ws3,
    /// Row 3: MacBookAir3,2.
    Ws4,
    /// Row 4: MacBookAir3,1.
    Ws4a,
    /// Row 5: MacBook8.
    Ws5,
    /// Row 6: MacBookAir4,2.
    Ws6,
    /// Row 7: MacBook8,2.
    Ws5a,
    /// Row 8: MacBookAir4,1.
    Ws6a,
    /// Row 9: MacBookPro10,1.
    Ws7,
    /// Row 10: MacBookPro10,2.
    Ws7a,
    /// Row 11: MacBookAir6,2.
    Ws8,
    /// Row 12: MacBookPro12,1.
    Ws9,
    /// Row 13: Magic Trackpad 2.
    Mt2,
    /// Row 14: Apple T2.
    T2,
}

impl Family {
    /// Short name as used by the fixtures (`"WS1"` ... `"T2"`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ws1 => "WS1",
            Self::Ws2 => "WS2",
            Self::Ws3 => "WS3",
            Self::Ws4 => "WS4",
            Self::Ws4a => "WS4A",
            Self::Ws5 => "WS5",
            Self::Ws6 => "WS6",
            Self::Ws5a => "WS5A",
            Self::Ws6a => "WS6A",
            Self::Ws7 => "WS7",
            Self::Ws7a => "WS7A",
            Self::Ws8 => "WS8",
            Self::Ws9 => "WS9",
            Self::Mt2 => "MT2",
            Self::T2 => "T2",
        }
    }
}

/// `struct BCM5974_CONFIG` (AppleDefinition.h:174-196), plus the family name of the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bcm5974Config {
    /// Row name (not part of the C struct).
    pub family: Family,
    /// Product ID, ANSI layout.
    pub ansi: i32,
    /// Product ID, ISO layout.
    pub iso: i32,
    /// Product ID, JIS layout.
    pub jis: i32,
    /// Capability bitmask (never read).
    pub caps: i32,
    /// Button interface endpoint (never read).
    pub bt_ep: i32,
    /// Button interface data length (never read).
    pub bt_datalen: i32,
    /// Trackpad interface endpoint (never read: the pipe is chosen by type, UM-B21).
    pub tp_ep: i32,
    /// Packet format.
    pub tp_type: TrackpadType,
    /// Bytes in the header block.
    pub tp_header: i32,
    /// Data length of the trackpad interface (never read; equals the reader transfer length).
    pub tp_datalen: i32,
    /// Offset of the button byte.
    pub tp_button: i32,
    /// Bytes in a single finger block.
    pub tp_fsize: i32,
    /// Offset from the header to the finger struct.
    pub tp_delta: i32,
    /// Mode-switch control-message parameters.
    pub um: UsbMsg,
    /// Finger pressure limits (never read).
    pub p: Bcm5974Param,
    /// Finger width limits (never read).
    pub w: Bcm5974Param,
    /// Horizontal limits (`x.min` is used by TYPE2-5).
    pub x: Bcm5974Param,
    /// Vertical limits (`y.max` is used by TYPE2-4, `y.min` by TYPE5).
    pub y: Bcm5974Param,
    /// Orientation limits (never read).
    pub o: Bcm5974Param,
}

const fn param(snratio: i32, min: i32, max: i32) -> Bcm5974Param {
    Bcm5974Param { snratio, min, max }
}

impl Bcm5974Config {
    /// Builds a row the way the C initializer does: `ansi, iso, jis, caps, bt_ep, bt_datalen, tp_ep,
    /// DATAFORMAT(type), p, w, x, y, o`, with `bt_datalen = sizeof(TRACKPAD_BUTTON_DATA)`, `w = {SN_WIDTH, 0,
    /// 2048}` and `o = {SN_ORIENT, -MAX_FINGER_ORIENTATION, MAX_FINGER_ORIENTATION}` as in every C row.
    #[allow(clippy::too_many_arguments)]
    const fn new(
        family: Family,
        pids: (i32, i32, i32),
        caps: i32,
        bt_ep: i32,
        tp_ep: i32,
        tp_type: TrackpadType,
        p_max: i32,
        x: (i32, i32),
        y: (i32, i32),
    ) -> Self {
        Self {
            family,
            ansi: pids.0,
            iso: pids.1,
            jis: pids.2,
            caps,
            bt_ep,
            bt_datalen: TRACKPAD_BUTTON_DATA_LEN,
            tp_ep,
            tp_type,
            tp_header: tp_type.header(),
            tp_datalen: tp_type.header() + MAX_FINGERS as i32 * tp_type.fsize(),
            tp_button: tp_type.button(),
            tp_fsize: tp_type.fsize(),
            tp_delta: tp_type.delta(),
            um: tp_type.usb_msg(),
            p: param(SN_PRESSURE, 0, p_max),
            w: param(SN_WIDTH, 0, 2048),
            x: param(SN_COORD, x.0, x.1),
            y: param(SN_COORD, y.0, y.1),
            o: param(SN_ORIENT, -MAX_FINGER_ORIENTATION, MAX_FINGER_ORIENTATION),
        }
    }

    /// `(unsigned int) tp_header` as `size_t` (InputInterrupt.c:106).
    pub const fn header_len(&self) -> usize {
        // C: `size_t headerSize = (unsigned int) tp_header;` (non-negative constant).
        self.tp_header as u32 as usize
    }

    /// `(unsigned int) tp_fsize` as `size_t` (InputInterrupt.c:107).
    pub const fn finger_len(&self) -> usize {
        // C: `size_t fingerprintSize = (unsigned int) tp_fsize;`
        self.tp_fsize as u32 as usize
    }

    /// `tp_delta` as a pointer offset (`f_base = Buffer + headerSize + tp_delta`).
    pub const fn delta_len(&self) -> usize {
        // Non-negative constant (0 or 2); the C adds the int to a pointer.
        self.tp_delta as u32 as usize
    }

    /// `tp_button` as a buffer index (`Buffer[tp_button]`).
    pub const fn button_offset(&self) -> usize {
        // Non-negative constant; the C indexes with the int.
        self.tp_button as u32 as usize
    }

    /// Whether `pid` is one of this row's three product IDs (`cfg->ansi == id || cfg->iso == id ||
    /// cfg->jis == id`, the USHORT promoted to int).
    pub const fn matches(&self, pid: u16) -> bool {
        let id = pid as i32; // USHORT -> int promotion
        self.ansi == id || self.iso == id || self.jis == id
    }

    /// The ANSI product ID as a USB `idProduct`.
    pub const fn ansi_pid(&self) -> u16 {
        // Every table PID fits in 16 bits.
        self.ansi as u16
    }
}

const T1: TrackpadType = TrackpadType::Type1;
const T2: TrackpadType = TrackpadType::Type2;
const T3: TrackpadType = TrackpadType::Type3;
const T4: TrackpadType = TrackpadType::Type4;
const T5: TrackpadType = TrackpadType::Type5;
const HIB: i32 = HAS_INTEGRATED_BUTTON;

/// Number of rows in `Bcm5974ConfigTable`.
pub const CONFIG_TABLE_LEN: usize = 15;

/// `Bcm5974ConfigTable`, in C order (row index = position).
#[rustfmt::skip]
pub static CONFIG_TABLE: [Bcm5974Config; CONFIG_TABLE_LEN] = [
    Bcm5974Config::new(Family::Ws1, (0x0223, 0x0224, 0x0225), 0, 0x84, 0x81, T1, 256, (-4824, 5342), (-172, 5820)),
    Bcm5974Config::new(Family::Ws2, (0x0230, 0x0231, 0x0232), 0, 0x84, 0x81, T1, 256, (-4824, 4824), (-172, 4290)),
    Bcm5974Config::new(Family::Ws3, (0x0236, 0x0237, 0x0238), HIB, 0x84, 0x81, T2, 300, (-4460, 5166), (-75, 6700)),
    Bcm5974Config::new(Family::Ws4, (0x023f, 0x0240, 0x0241), HIB, 0x84, 0x81, T2, 300, (-4620, 5140), (-150, 6600)),
    Bcm5974Config::new(Family::Ws4a, (0x0242, 0x0243, 0x0244), HIB, 0x84, 0x81, T2, 300, (-4616, 5112), (-142, 5234)),
    Bcm5974Config::new(Family::Ws5, (0x0245, 0x0246, 0x0247), HIB, 0x84, 0x81, T2, 300, (-4415, 5050), (-55, 6680)),
    Bcm5974Config::new(Family::Ws6, (0x024c, 0x024d, 0x024e), HIB, 0x84, 0x81, T2, 300, (-4620, 5140), (-150, 6600)),
    Bcm5974Config::new(Family::Ws5a, (0x0252, 0x0253, 0x0254), HIB, 0x84, 0x81, T2, 300, (-4750, 5280), (-150, 6730)),
    Bcm5974Config::new(Family::Ws6a, (0x0249, 0x024a, 0x024b), HIB, 0x84, 0x81, T2, 300, (-4620, 5140), (-150, 6600)),
    Bcm5974Config::new(Family::Ws7, (0x0262, 0x0263, 0x0264), HIB, 0x84, 0x81, T2, 300, (-4750, 5280), (-150, 6730)),
    Bcm5974Config::new(Family::Ws7a, (0x0259, 0x025a, 0x025b), HIB, 0x84, 0x81, T2, 300, (-4750, 5280), (-150, 6730)),
    Bcm5974Config::new(Family::Ws8, (0x0290, 0x0291, 0x0292), HIB, 0, 0x83, T3, 300, (-4620, 5140), (-150, 6600)),
    Bcm5974Config::new(Family::Ws9, (0x0272, 0x0273, 0x0274), HIB, 0, 0x83, T4, 300, (-4828, 5345), (-203, 6803)),
    Bcm5974Config::new(Family::Mt2, (0x0265, 0x0265, 0x0265), HIB, 0, 0x83, T5, 300, (-3678, 3934), (-2479, 2586)),
    // C comment on the width limits of this row: "This is incorrect; check actual data later".
    Bcm5974Config::new(Family::T2, (0x027d, 0x027d, 0x027d), HIB, 0, 0x83, T4, 300, (-4828, 5345), (-203, 6803)),
];

/// `AmtPtpGetDeviceConfig` with bounded iteration (UM-B3, P2): the index of the first row whose
/// `ansi`, `iso` or `jis` equals `pid`, or `None` when no row matches. The vendor ID is not checked
/// (UM-B23, P3).
pub fn lookup_config_row(pid: u16) -> Option<u8> {
    CONFIG_TABLE
        .iter()
        .position(|cfg| cfg.matches(pid))
        // CONFIG_TABLE_LEN is 15, so the index always fits in a u8.
        .and_then(|row| u8::try_from(row).ok())
}

/// `AmtPtpGetDeviceConfig`: the table row for `pid`, or `None` (C: `NULL`).
pub fn lookup_config(pid: u16) -> Option<&'static Bcm5974Config> {
    lookup_config_row(pid).and_then(config_row)
}

/// The table row at `row`, or `None` if `row` is out of range.
pub fn config_row(row: u8) -> Option<&'static Bcm5974Config> {
    CONFIG_TABLE.get(usize::from(row))
}

//! Apple trackpad packet formats and USB device geometry.

use ptp_hid::common::MAX_FINGERS;

/// Apple's USB vendor ID.
pub const USB_VENDOR_ID_APPLE: u16 = 0x05AC;
/// Magic Trackpad 2 USB product ID (also used in its Bluetooth identification).
pub const MAGIC_TRACKPAD_2_PID: u16 = 0x0265;

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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub const fn finger_size(self) -> i32 {
        match self {
            Self::Type1 | Self::Type2 | Self::Type3 => 14 * 2,
            Self::Type4 => 15 * 2,
            Self::Type5 => 9,
        }
    }

    /// `DELTA_TYPEn`: offset from the header end to the finger struct inside a finger block.
    #[must_use]
    pub const fn delta(self) -> i32 {
        match self {
            Self::Type4 => 2,
            Self::Type1 | Self::Type2 | Self::Type3 | Self::Type5 => 0,
        }
    }

    /// Continuous-reader transfer length of `AmtPtpConfigContReaderForInterruptEndPoint`
    /// (InputInterrupt.c:23-40): `HEADER_TYPEn + FSIZE_TYPEn * MAX_FINGERS` = 474/478/486/526/156.
    #[must_use]
    pub const fn reader_transfer_length(self) -> usize {
        // All operands are small positive constants; the C computes this in `size_t`.
        (self.header() + self.finger_size() * MAX_FINGERS as i32) as usize
    }

    /// The C enumerator name (`"TYPE1"` ... `"TYPE5"`).
    #[must_use]
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

/// Raw coordinate limits for one axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bcm5974Param {
    /// Device minimum reading.
    pub min: i32,
    /// Device maximum reading.
    pub max: i32,
}

/// Input packet format and raw coordinate geometry for an Apple trackpad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bcm5974Config {
    /// Packet format.
    pub tp_type: TrackpadType,
    /// Horizontal limits.
    pub x: Bcm5974Param,
    /// Vertical limits.
    pub y: Bcm5974Param,
}

impl Bcm5974Config {
    const fn new(
        tp_type: TrackpadType,
        ((x_min, x_max), (y_min, y_max)): ((i32, i32), (i32, i32)),
    ) -> Self {
        Self {
            tp_type,
            x: Bcm5974Param {
                min: x_min,
                max: x_max,
            },
            y: Bcm5974Param {
                min: y_min,
                max: y_max,
            },
        }
    }
}

/// The geometry common to USB and Bluetooth Magic Trackpad 2.
pub const MAGIC_TRACKPAD_2: Bcm5974Config =
    Bcm5974Config::new(TrackpadType::Type5, ((-3678, 3934), (-2479, 2586)));

/// Row names, in table order. The names match the C comments and the oracle fixtures (`fam=`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    /// Row 0: `MacBookAir`, "wellspring".
    Ws1,
    /// Row 1: `MacBookPro` Penryn, "wellspring2".
    Ws2,
    /// Row 2: MacBook5,1, "wellspring3".
    Ws3,
    /// Row 3: MacBookAir3,2.
    Ws4,
    /// Row 4: MacBookAir3,1.
    Ws4a,
    /// Row 5: `MacBook8`.
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
    #[must_use]
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

/// Device table row for the UMDF driver's Wellspring and Magic Trackpad families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WellspringDevice {
    /// Family used to choose the HID descriptor.
    pub family: Family,
    /// ANSI, ISO and JIS product IDs.
    pub product_ids: (u16, u16, u16),
    /// Input geometry and packet format.
    pub profile: Bcm5974Config,
}

impl WellspringDevice {
    const fn new(family: Family, product_ids: (u16, u16, u16), profile: Bcm5974Config) -> Self {
        Self {
            family,
            product_ids,
            profile,
        }
    }

    /// Whether the product ID matches one of the keyboard layouts.
    #[must_use]
    pub const fn matches(&self, pid: u16) -> bool {
        let (ansi, iso, jis) = self.product_ids;
        ansi == pid || iso == pid || jis == pid
    }

    /// The ANSI product ID used to select the report descriptor.
    #[must_use]
    pub const fn ansi_pid(&self) -> u16 {
        self.product_ids.0
    }
}

const T1: TrackpadType = TrackpadType::Type1;
const T2: TrackpadType = TrackpadType::Type2;
const T3: TrackpadType = TrackpadType::Type3;
const T4: TrackpadType = TrackpadType::Type4;
const WS4_GEOMETRY: Bcm5974Config = Bcm5974Config::new(T2, ((-4620, 5140), (-150, 6600)));
const WS5A_GEOMETRY: Bcm5974Config = Bcm5974Config::new(T2, ((-4750, 5280), (-150, 6730)));
const WS9_GEOMETRY: Bcm5974Config = Bcm5974Config::new(T4, ((-4828, 5345), (-203, 6803)));

/// Legacy USB table in the original lookup order.
#[rustfmt::skip]
pub static WELLSPRING_DEVICES: [WellspringDevice; 15] = [
    WellspringDevice::new(Family::Ws1, (0x0223, 0x0224, 0x0225), Bcm5974Config::new(T1, ((-4824, 5342), (-172, 5820)))),
    WellspringDevice::new(Family::Ws2, (0x0230, 0x0231, 0x0232), Bcm5974Config::new(T1, ((-4824, 4824), (-172, 4290)))),
    WellspringDevice::new(Family::Ws3, (0x0236, 0x0237, 0x0238), Bcm5974Config::new(T2, ((-4460, 5166), (-75, 6700)))),
    WellspringDevice::new(Family::Ws4, (0x023f, 0x0240, 0x0241), WS4_GEOMETRY),
    WellspringDevice::new(Family::Ws4a, (0x0242, 0x0243, 0x0244), Bcm5974Config::new(T2, ((-4616, 5112), (-142, 5234)))),
    WellspringDevice::new(Family::Ws5, (0x0245, 0x0246, 0x0247), Bcm5974Config::new(T2, ((-4415, 5050), (-55, 6680)))),
    WellspringDevice::new(Family::Ws6, (0x024c, 0x024d, 0x024e), WS4_GEOMETRY),
    WellspringDevice::new(Family::Ws5a, (0x0252, 0x0253, 0x0254), WS5A_GEOMETRY),
    WellspringDevice::new(Family::Ws6a, (0x0249, 0x024a, 0x024b), WS4_GEOMETRY),
    WellspringDevice::new(Family::Ws7, (0x0262, 0x0263, 0x0264), WS5A_GEOMETRY),
    WellspringDevice::new(Family::Ws7a, (0x0259, 0x025a, 0x025b), WS5A_GEOMETRY),
    WellspringDevice::new(Family::Ws8, (0x0290, 0x0291, 0x0292), Bcm5974Config::new(T3, ((-4620, 5140), (-150, 6600)))),
    WellspringDevice::new(Family::Ws9, (0x0272, 0x0273, 0x0274), WS9_GEOMETRY),
    WellspringDevice::new(Family::Mt2, (MAGIC_TRACKPAD_2_PID, MAGIC_TRACKPAD_2_PID, MAGIC_TRACKPAD_2_PID), MAGIC_TRACKPAD_2),
    // UMDF used the WS9 geometry for 0x027D. Its historical report fixtures rely on it.
    WellspringDevice::new(Family::T2, (0x027d, 0x027d, 0x027d), WS9_GEOMETRY),
];

/// Device table row for the KMDF USB driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct T2Device {
    /// USB product ID (0xFFFF for the fallback).
    pub identification: u16,
    /// Input geometry and packet format.
    pub profile: Bcm5974Config,
}

const T2_13_INCH: Bcm5974Config = Bcm5974Config::new(T4, ((-6243, 6749), (-170, 7685)));
const T2_15_INCH: Bcm5974Config = Bcm5974Config::new(T4, ((-10000, 10000), (-2000, 10000)));

/// Generic TYPE4 geometry used by the KMDF driver for unlisted products.
pub const T2_FALLBACK: T2Device = T2Device {
    identification: 0xFFFF,
    profile: T2_15_INCH,
};

/// KMDF USB table in the original lookup order, with the fallback first.
#[rustfmt::skip]
pub static T2_DEVICES: [T2Device; 5] = [
    T2_FALLBACK,
    T2Device { identification: 0x027A, profile: T2_13_INCH },
    T2Device { identification: 0x027B, profile: T2_13_INCH },
    T2Device { identification: 0x027C, profile: T2_15_INCH },
    T2Device { identification: 0x027D, profile: T2_15_INCH },
];

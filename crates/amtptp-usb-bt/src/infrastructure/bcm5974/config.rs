//! Apple trackpad packet formats and USB device geometry.

use amtptp_core::hid::common::MAX_FINGERS;

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

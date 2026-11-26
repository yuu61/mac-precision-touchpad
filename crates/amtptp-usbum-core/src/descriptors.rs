//! HID descriptors and device attributes (`StaticHidRegistry.h`, Hid.c:7-397).
//!
//! The six 516-byte report descriptors are built at compile time from the shared [`ptp_hid`] builders with
//! this driver's per-family axis values (spec 5.1; byte-identical to the C arrays, verified against the
//! oracle fixtures). All six HID descriptors are the same 9 bytes `09 21 00 01 00 01 22 04 02`.
//!
//! The PID-to-descriptor switch (Hid.c:42-137 and :280-339) is **not** the config table: WS1/WS2/WS4/WS4A
//! and T2 have config rows but no descriptor, and WS5A uses the WS5 descriptor and WS9 the WS8 one
//! (UM-B18, P3 PRESERVED).

use ptp_hid::common::DEVICE_VID;
use ptp_hid::descriptor::{
    Axes, REPORT_DESCRIPTOR_U32_ID_UMAPP_LEN, report_descriptor_u32_id_umapp,
};
use ptp_hid::hidport::{HID_DESCRIPTOR_LEN, HID_DESCRIPTOR_U32_ID_UMAPP, HidDeviceAttributes};

/// Length of every UsbUm report descriptor (`DescriptorList[0].wReportLength` = 516).
pub const REPORT_DESCRIPTOR_LEN: usize = REPORT_DESCRIPTOR_U32_ID_UMAPP_LEN;

/// `DEVICE_VERSION` (Hid.h:19): `HID_DEVICE_ATTRIBUTES.VersionNumber`.
pub const DEVICE_VERSION: u16 = 0x0001;

/// `AmtPtp{3,5,6,7a,8,Mt2}DefaultHidDescriptor`: identical for all six families because every report
/// descriptor is 516 bytes: `09 21 00 01 00 01 22 04 02`.
pub const HID_DESCRIPTOR: [u8; HID_DESCRIPTOR_LEN] = HID_DESCRIPTOR_U32_ID_UMAPP;

/// Wellspring3.h axis values.
pub const AXES_WS3: Axes = Axes {
    x_logical_max: 9626,
    x_physical_max: 1050,
    y_physical_max: 760,
    y_logical_max: 6775,
};
/// Wellspring5.h axis values (also used for WS5A, UM-B18).
pub const AXES_WS5: Axes = Axes {
    x_logical_max: 9465,
    x_physical_max: 1050,
    y_physical_max: 810,
    y_logical_max: 6735,
};
/// Wellspring6.h axis values (WS6 and WS6A).
pub const AXES_WS6: Axes = Axes {
    x_logical_max: 9760,
    x_physical_max: 1067,
    y_physical_max: 762,
    y_logical_max: 6750,
};
/// Wellspring7A.h axis values (WS7 and WS7A).
pub const AXES_WS7A: Axes = Axes {
    x_logical_max: 10030,
    x_physical_max: 1064,
    y_physical_max: 773,
    y_logical_max: 6880,
};
/// Wellspring8.h axis values (WS8, and WS9, UM-B18).
pub const AXES_WS8: Axes = Axes {
    x_logical_max: 9760,
    x_physical_max: 1045,
    y_physical_max: 750,
    y_logical_max: 6750,
};
/// WellspringMt2.h axis values (Magic Trackpad 2).
pub const AXES_MT2: Axes = Axes {
    x_logical_max: 7612,
    x_physical_max: 1600,
    y_physical_max: 1149,
    y_logical_max: 5065,
};

/// `AmtPtp3ReportDescriptor` (SHA-256 a9473313...).
pub static REPORT_DESCRIPTOR_WS3: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_u32_id_umapp(AXES_WS3);
/// `AmtPtp5ReportDescriptor` (SHA-256 3b57036a...).
pub static REPORT_DESCRIPTOR_WS5: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_u32_id_umapp(AXES_WS5);
/// `AmtPtp6ReportDescriptor` (SHA-256 83e1b906...).
pub static REPORT_DESCRIPTOR_WS6: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_u32_id_umapp(AXES_WS6);
/// `AmtPtp7aReportDescriptor` (SHA-256 9a509e44...).
pub static REPORT_DESCRIPTOR_WS7A: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_u32_id_umapp(AXES_WS7A);
/// `AmtPtp8ReportDescriptor` (SHA-256 13d94c5f...).
pub static REPORT_DESCRIPTOR_WS8: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_u32_id_umapp(AXES_WS8);
/// `AmtPtpMt2ReportDescriptor` (SHA-256 c803f5c3...).
pub static REPORT_DESCRIPTOR_MT2: [u8; REPORT_DESCRIPTOR_LEN] =
    report_descriptor_u32_id_umapp(AXES_MT2);

/// The six static HID registries of `StaticHidRegistry.h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HidRegistry {
    /// `AmtPtp3*`: WS3.
    Ws3,
    /// `AmtPtp5*`: WS5 and WS5A.
    Ws5,
    /// `AmtPtp6*`: WS6 and WS6A.
    Ws6,
    /// `AmtPtp7a*`: WS7 and WS7A.
    Ws7a,
    /// `AmtPtp8*`: WS8 and WS9.
    Ws8,
    /// `AmtPtpMt2*`: Magic Trackpad 2.
    Mt2,
}

impl HidRegistry {
    /// The PID switch of `AmtPtpGetHidDescriptor` / `AmtPtpGetReportDescriptor` (both identical; spec 8.3).
    /// `None` for every other PID (the C handlers then return `STATUS_INVALID_DEVICE_STATE`).
    #[allow(clippy::manual_range_patterns)] // keep the C case groups (family, then "A" variant) visible
    pub const fn for_pid(pid: u16) -> Option<Self> {
        match pid {
            0x0236..=0x0238 => Some(Self::Ws3),
            0x0245..=0x0247 | 0x0252..=0x0254 => Some(Self::Ws5),
            0x024c..=0x024e | 0x0249..=0x024b => Some(Self::Ws6),
            0x0262..=0x0264 | 0x0259..=0x025b => Some(Self::Ws7a),
            0x0290..=0x0292 | 0x0272..=0x0274 => Some(Self::Ws8),
            0x0265 => Some(Self::Mt2),
            _ => None,
        }
    }

    /// The 516-byte report descriptor.
    pub const fn report_descriptor(self) -> &'static [u8; REPORT_DESCRIPTOR_LEN] {
        match self {
            Self::Ws3 => &REPORT_DESCRIPTOR_WS3,
            Self::Ws5 => &REPORT_DESCRIPTOR_WS5,
            Self::Ws6 => &REPORT_DESCRIPTOR_WS6,
            Self::Ws7a => &REPORT_DESCRIPTOR_WS7A,
            Self::Ws8 => &REPORT_DESCRIPTOR_WS8,
            Self::Mt2 => &REPORT_DESCRIPTOR_MT2,
        }
    }

    /// The 9-byte HID descriptor (the same for every registry).
    pub const fn hid_descriptor(self) -> &'static [u8; HID_DESCRIPTOR_LEN] {
        &HID_DESCRIPTOR
    }

    /// The axis values of this registry's finger collections.
    pub const fn axes(self) -> Axes {
        match self {
            Self::Ws3 => AXES_WS3,
            Self::Ws5 => AXES_WS5,
            Self::Ws6 => AXES_WS6,
            Self::Ws7a => AXES_WS7A,
            Self::Ws8 => AXES_WS8,
            Self::Mt2 => AXES_MT2,
        }
    }

    /// The C symbol prefix (`"AmtPtp3"` ... `"AmtPtpMt2"`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ws3 => "AmtPtp3",
            Self::Ws5 => "AmtPtp5",
            Self::Ws6 => "AmtPtp6",
            Self::Ws7a => "AmtPtp7a",
            Self::Ws8 => "AmtPtp8",
            Self::Mt2 => "AmtPtpMt2",
        }
    }
}

/// The `HID_DEVICE_ATTRIBUTES` the driver reports (Hid.c:219-228): VendorID [`DEVICE_VID`] (0x8910,
/// deliberately not 0x05AC, P3), ProductID = the USB `idProduct`, VersionNumber [`DEVICE_VERSION`].
pub const fn device_attributes(id_product: u16) -> HidDeviceAttributes {
    HidDeviceAttributes {
        vendor_id: DEVICE_VID,
        product_id: id_product,
        version_number: DEVICE_VERSION,
    }
}

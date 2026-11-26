//! Report IDs, PTP constants and usages shared by all four drivers.
//!
//! These come from `HidCommon.h`, which is identical in all four drivers apart from comments and `HidFilter`'s
//! extra `DEVICE_VID`/`DEVICE_VERSION`, and from the `PTP_*` block of each driver's `Hid.h` (`HidFilter`:
//! `Metadata/WindowsHID.h`), which is also identical.
//!
//! The per-driver `DEVICE_VERSION` is not here: it is 0x01 in UsbUm/UsbKm/SpiKm and 0x100 in `HidFilter`, and
//! `SpiKm` does not even use it. Each core crate defines its own.

/// `REPORTID_STANDARDMOUSE`. It is not declared in any of our report descriptors. `SpiKm` uses 0x02 only as
/// the ID of Apple's own downstream feature report.
pub const REPORTID_STANDARDMOUSE: u8 = 0x02;
/// `REPORTID_MULTITOUCH`: the PTP input report (contacts, scan time, contact count, button).
pub const REPORTID_MULTITOUCH: u8 = 0x05;
/// `REPORTID_REPORTMODE`: the input-mode feature report in the Windows configuration TLC.
pub const REPORTID_REPORTMODE: u8 = 0x04;
/// `REPORTID_PTPHQA`: the 257-byte PTP certification (HQA) feature report.
pub const REPORTID_PTPHQA: u8 = 0x08;
/// `REPORTID_FUNCSWITCH`: the selective-reporting (button/surface switch) feature report.
pub const REPORTID_FUNCSWITCH: u8 = 0x06;
/// `REPORTID_DEVICE_CAPS`: the device-capabilities feature report (maximum contacts, pad type).
pub const REPORTID_DEVICE_CAPS: u8 = 0x07;
/// `REPORTID_UMAPP_CONF`: the user-mode settings-app configuration feature report.
pub const REPORTID_UMAPP_CONF: u8 = 0x09;

/// `BUTTON_SWITCH` (HidCommon.h), used as the first usage of report 0x06.
///
/// Preserved naming bug: in the HID Usage Tables, Digitizers page 0x57 is *Surface Switch*. The C struct
/// and this crate nevertheless call bit 0 of report 0x06 "button report". See
/// [`crate::feature::SelectiveReportBits`].
pub const BUTTON_SWITCH: u8 = 0x57;
/// `SURFACE_SWITCH` (HidCommon.h), used as the second usage of report 0x06.
///
/// Preserved naming bug: in the HID Usage Tables, Digitizers page 0x58 is *Button Switch*.
pub const SURFACE_SWITCH: u8 = 0x58;

/// `MAX_FINGERS` (16). In the descriptors it appears only as the logical maximum of the Input Mode usage
/// in the Windows configuration TLC (`25 10`). It is not the contact limit; that is
/// [`PTP_MAX_CONTACT_POINTS`].
pub const MAX_FINGERS: u8 = 16;

/// `PTP_MAX_CONTACT_POINTS` (5): the number of contact slots in input report 0x05 and the value reported in
/// feature report 0x07.
pub const PTP_MAX_CONTACT_POINTS: u8 = 5;
/// [`PTP_MAX_CONTACT_POINTS`] as a `usize` for array lengths.
pub const PTP_CONTACT_SLOTS: usize = PTP_MAX_CONTACT_POINTS as usize;

/// `PTP_BUTTON_TYPE_CLICK_PAD` (0): the pad type every driver reports in feature report 0x07.
pub const PTP_BUTTON_TYPE_CLICK_PAD: u8 = 0;
/// `PTP_BUTTON_TYPE_PRESSURE_PAD` (1). No driver uses it.
pub const PTP_BUTTON_TYPE_PRESSURE_PAD: u8 = 1;

/// `PTP_COLLECTION_MOUSE` (0): the input-mode value for "mouse collection" in feature report 0x04.
pub const PTP_COLLECTION_MOUSE: u8 = 0;
/// `PTP_COLLECTION_WINDOWS` (3): the input-mode value for "PTP collection" in feature report 0x04.
pub const PTP_COLLECTION_WINDOWS: u8 = 3;

/// `PTP_CONTACT_CONFIDENCE_BIT` (1): the mask of the Confidence bit in a contact's flag byte.
pub const PTP_CONTACT_CONFIDENCE_BIT: u8 = 1;
/// `PTP_CONTACT_TIPSWITCH_BIT` (2): the mask of the Tip Switch bit in a contact's flag byte.
pub const PTP_CONTACT_TIPSWITCH_BIT: u8 = 2;

/// `DEVICE_VID` (0x8910): the substitute vendor ID that `UsbUm`, `SpiKm` and `HidFilter` report in
/// `HID_DEVICE_ATTRIBUTES`. It avoids a hardware-ID collision with Apple's own HID collections (commit
/// dd364d6). `UsbKm` reports the real USB idVendor instead.
pub const DEVICE_VID: u16 = 0x8910;

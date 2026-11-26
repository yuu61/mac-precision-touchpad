//! HID short-item prefix bytes, with the names used in `HidCommon.h` (identical in all four drivers).
//!
//! Each constant is a complete prefix byte: tag, type and size code. The C suffixes encode the data
//! size. No suffix means 1 data byte, `_1`/`_2` mean 2 data bytes (size code 2), and `_3` means 4 data
//! bytes (size code 3). For example, `LOGICAL_MAXIMUM_3, 0xff, 0xff, 0xff, 0xff` is Logical Maximum with
//! a 32-bit value.

/// Usage Page, 1 data byte.
pub const USAGE_PAGE: u8 = 0x05;
/// Usage Page, 2 data bytes (for example `06 00 ff` = vendor-defined page 0xFF00).
pub const USAGE_PAGE_1: u8 = 0x06;
/// Usage, 1 data byte.
pub const USAGE: u8 = 0x09;
/// Usage Minimum, 1 data byte.
pub const USAGE_MINIMUM: u8 = 0x19;
/// Usage Maximum, 1 data byte.
pub const USAGE_MAXIMUM: u8 = 0x29;
/// Logical Minimum, 1 data byte.
pub const LOGICAL_MINIMUM: u8 = 0x15;
/// Logical Maximum, 1 data byte.
pub const LOGICAL_MAXIMUM: u8 = 0x25;
/// Logical Maximum, 2 data bytes.
pub const LOGICAL_MAXIMUM_2: u8 = 0x26;
/// Logical Maximum, 4 data bytes.
pub const LOGICAL_MAXIMUM_3: u8 = 0x27;
/// Physical Minimum, 1 data byte.
pub const PHYSICAL_MINIMUM: u8 = 0x35;
/// Physical Maximum, 1 data byte.
pub const PHYSICAL_MAXIMUM: u8 = 0x45;
/// Physical Maximum, 2 data bytes.
pub const PHYSICAL_MAXIMUM_2: u8 = 0x46;
/// Physical Maximum, 4 data bytes.
pub const PHYSICAL_MAXIMUM_3: u8 = 0x47;
/// Unit Exponent, 1 data byte.
pub const UNIT_EXPONENT: u8 = 0x55;
/// Unit, 1 data byte.
pub const UNIT: u8 = 0x65;
/// Unit, 2 data bytes.
pub const UNIT_2: u8 = 0x66;
/// Report ID, 1 data byte.
pub const REPORT_ID: u8 = 0x85;
/// Report Count, 1 data byte.
pub const REPORT_COUNT: u8 = 0x95;
/// Report Count, 2 data bytes.
pub const REPORT_COUNT_2: u8 = 0x96;
/// Report Size (in bits), 1 data byte.
pub const REPORT_SIZE: u8 = 0x75;
/// Input main item, 1 data byte.
pub const INPUT: u8 = 0x81;
/// Feature main item, 1 data byte.
pub const FEATURE: u8 = 0xb1;
/// Collection (begin), 1 data byte: 0x00 physical, 0x01 application, 0x02 logical.
pub const BEGIN_COLLECTION: u8 = 0xa1;
/// End Collection, no data.
pub const END_COLLECTION: u8 = 0xc0;

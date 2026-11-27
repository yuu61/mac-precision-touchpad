//! PID lookup using the legacy Wellspring and Magic Trackpad device table.

pub use bcm5974::config::{
    Family, TrackpadType, WELLSPRING_DEVICES as CONFIG_TABLE, WellspringDevice,
};

/// `AmtPtpGetDeviceConfig` with bounded iteration (UM-B3, P2): the index of the first row whose
/// `ansi`, `iso` or `jis` equals `pid`, or `None` when no row matches. The vendor ID is not checked
/// (UM-B23, P3).
#[must_use]
pub fn lookup_config_row(pid: u16) -> Option<u8> {
    let row = CONFIG_TABLE.iter().position(|cfg| cfg.matches(pid))?;
    u8::try_from(row).ok()
}

/// `AmtPtpGetDeviceConfig`: the table row for `pid`, or `None` (C: `NULL`).
pub fn lookup_config(pid: u16) -> Option<&'static WellspringDevice> {
    lookup_config_row(pid).and_then(config_row)
}

/// The table row at `row`, or `None` if `row` is out of range.
#[must_use]
pub fn config_row(row: u8) -> Option<&'static WellspringDevice> {
    CONFIG_TABLE.get(usize::from(row))
}

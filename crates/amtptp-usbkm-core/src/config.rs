//! PID lookup using the KMDF USB device table and its generic TYPE4 fallback.

pub use bcm5974::config::{
    T2_DEVICES as CONFIG_TABLE, T2_FALLBACK as FALLBACK_CONFIG, T2Device, TrackpadType,
};

/// Index of the generic fallback row ([`FALLBACK_CONFIG`]) that unknown PIDs get.
pub const FALLBACK_CONFIG_INDEX: usize = 0;

/// The PIDs the INF binds to this driver that have no row of their own and therefore run on the generic
/// TYPE4 fallback geometry ([`FALLBACK_CONFIG`]). Listing them makes the fallback a decision: the `xtask`
/// INF test fails for a new binding that is neither a table row nor listed here, and for an entry here
/// that the INF no longer binds.
///
/// 0x0273, 0x0274, 0x0290 and 0x0291 are bound only by the Windows 10 1709 models section (1803 and later
/// give them to the UMDF driver). 0x0290 and 0x0291 are Wellspring 8 trackpads, whose packets are TYPE3,
/// so the TYPE4 fallback cannot parse them correctly there.
pub const FALLBACK_PIDS: [u16; 9] = [
    0x0273, 0x0274, 0x0277, 0x027E, 0x027F, 0x0280, 0x0290, 0x0291, 0x0340,
];

/// Whether `product_id` is one of the [`FALLBACK_PIDS`].
#[must_use]
pub fn uses_listed_fallback(product_id: u16) -> bool {
    FALLBACK_PIDS.contains(&product_id)
}

/// `AmtPtpGetDeviceConfig`: the index of the row whose `identification` equals `product_id`, scanning
/// from row 0; [`FALLBACK_CONFIG_INDEX`] when no row matches (the C traces "Selected a generic fallback
/// configuration" in that case).
#[must_use]
pub fn config_index_for_pid(product_id: u16) -> usize {
    CONFIG_TABLE
        .iter()
        .position(|row| row.identification == product_id)
        .unwrap_or(FALLBACK_CONFIG_INDEX)
}

/// The row for `index`, or `None` if out of range.
#[must_use]
pub fn config_by_index(index: usize) -> Option<&'static T2Device> {
    CONFIG_TABLE.get(index)
}

/// `AmtPtpGetDeviceConfig`: the row for `product_id` (never fails; unknown PIDs get the fallback row).
#[must_use]
pub fn config_for_pid(product_id: u16) -> &'static T2Device {
    config_by_index(config_index_for_pid(product_id)).unwrap_or(&FALLBACK_CONFIG)
}

/// `transferLength` of `AmtPtpConfigContReaderForInterruptEndPoint` (Interrupt.c:30-47):
/// `HEADER_TYPEn + FSIZE_TYPEn * MAX_FINGERS` (526 for TYPE4). `size_t` arithmetic on positive constants.
#[must_use]
pub const fn reader_transfer_length(tp_type: TrackpadType) -> usize {
    tp_type.reader_transfer_length()
}

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
        assert_eq!(config_for_pid(0x027A).profile.x.min, -6243);
        assert_eq!(config_for_pid(0x027A).profile.y.max, 7685);
        assert!(config_by_index(5).is_none());
    }

    #[test]
    fn listed_fallback_pids_have_no_row() {
        for pid in FALLBACK_PIDS {
            assert_eq!(config_index_for_pid(pid), FALLBACK_CONFIG_INDEX, "{pid:04x}");
            assert!(uses_listed_fallback(pid));
        }
        for row in &CONFIG_TABLE[1..] {
            assert!(!uses_listed_fallback(row.identification));
        }
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

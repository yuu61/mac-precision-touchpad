//! PID lookup using the KMDF USB device table and its generic TYPE4 fallback.

pub use bcm5974::config::{
    T2_DEVICES as CONFIG_TABLE, T2_FALLBACK as FALLBACK_CONFIG, T2Device, TrackpadType,
};

/// Index of the generic fallback row ([`FALLBACK_CONFIG`]) that unknown PIDs get.
pub const FALLBACK_CONFIG_INDEX: usize = 0;

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
    fn reader_lengths() {
        assert_eq!(reader_transfer_length(TrackpadType::Type1), 474);
        assert_eq!(reader_transfer_length(TrackpadType::Type2), 478);
        assert_eq!(reader_transfer_length(TrackpadType::Type3), 486);
        assert_eq!(reader_transfer_length(TrackpadType::Type4), 526);
        assert_eq!(reader_transfer_length(TrackpadType::Type5), 156);
    }
}

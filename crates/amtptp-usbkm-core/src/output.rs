//! The request output buffer as the C code writes it: through `WdfMemoryCopyFromBuffer` on the `WDFMEMORY`
//! that `WdfRequestRetrieveOutputMemory` returned.
//!
//! The C copies the HID descriptor, the report descriptor and the 50-byte input report with
//! `WdfMemoryCopyFromBuffer(mem, 0, src, len)`. The glue implements [`OutputMemory`] on that `WDFMEMORY`, so
//! the framework produces the copy status itself (P1: framework-originated statuses stay the framework's).
//! The `[u8]` implementation models the documented behavior for host tests: `STATUS_BUFFER_TOO_SMALL` and
//! nothing written when `src` does not fit; otherwise `src` is copied to the start and the rest is
//! untouched.

use crate::status::{NtStatus, STATUS_BUFFER_TOO_SMALL, STATUS_SUCCESS};

/// A destination for `WdfMemoryCopyFromBuffer(DestinationMemory, 0, src, src.len())`.
pub trait OutputMemory {
    /// Copies `src` to offset 0 and returns the `WdfMemoryCopyFromBuffer` status. On failure nothing is
    /// written.
    fn copy_from_buffer(&mut self, src: &[u8]) -> NtStatus;
}

impl OutputMemory for [u8] {
    fn copy_from_buffer(&mut self, src: &[u8]) -> NtStatus {
        match self.get_mut(..src.len()) {
            Some(head) => {
                head.copy_from_slice(src);
                STATUS_SUCCESS
            }
            None => STATUS_BUFFER_TOO_SMALL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_model() {
        let mut buf = [0xccu8; 4];
        assert_eq!(buf[..].copy_from_buffer(&[1, 2]), STATUS_SUCCESS);
        assert_eq!(buf, [1, 2, 0xcc, 0xcc]);
        assert_eq!(buf[..].copy_from_buffer(&[9; 5]), STATUS_BUFFER_TOO_SMALL);
        assert_eq!(buf, [1, 2, 0xcc, 0xcc]);
        let empty: &mut [u8] = &mut [];
        assert_eq!(empty.copy_from_buffer(&[]), STATUS_SUCCESS);
    }
}

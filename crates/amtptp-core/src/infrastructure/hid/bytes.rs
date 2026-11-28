//! Panic-free byte-copy helpers that work in `const` context.

use crate::hid::BufferTooSmall;

/// Copies `src` to the front of `dst` and returns the remaining tail of `dst`.
///
/// If `dst` is shorter than `src`, nothing is written and an empty slice is returned. Every caller in
/// this crate writes into an array whose length is checked at compile time against the sum of the parts.
/// The short case is therefore unreachable, and this function never needs to panic.
pub(crate) const fn put<'a>(dst: &'a mut [u8], src: &[u8]) -> &'a mut [u8] {
    match dst.split_at_mut_checked(src.len()) {
        Some((head, tail)) => {
            head.copy_from_slice(src);
            tail
        }
        None => &mut [],
    }
}

/// Copies the `N`-byte array at the front of `src` and returns the copy and the rest of `src`.
/// Returns `None` if `src` is shorter than `N`.
pub(crate) const fn take<const N: usize>(src: &[u8]) -> Option<([u8; N], &[u8])> {
    match src.split_first_chunk::<N>() {
        Some((head, tail)) => Some((*head, tail)),
        None => None,
    }
}

/// Concatenates two byte arrays into a new array of length `N`.
///
/// `N` must equal `A + B`. That is checked at compile time with an inline `const` assertion, so a wrong
/// length is a build error, never a runtime panic. Usable in `const` and `static` initializers, e.g.
/// `const D: [u8; 5] = concat(&[1, 2], &[3, 4, 5]);`.
#[must_use]
pub const fn concat<const A: usize, const B: usize, const N: usize>(
    a: &[u8; A],
    b: &[u8; B],
) -> [u8; N] {
    const {
        assert!(
            A + B == N,
            "concat: output length must equal the sum of the input lengths"
        );
    };
    let mut out = [0_u8; N];
    let rest = put(&mut out, a);
    put(rest, b);
    out
}

/// Copies `src` to the start of the caller's buffer `dst` and leaves every byte after it untouched.
///
/// Returns [`BufferTooSmall`] and writes nothing if `dst` is shorter than `src`.
pub(crate) fn write_prefix(dst: &mut [u8], src: &[u8]) -> Result<(), BufferTooSmall> {
    match dst.get_mut(..src.len()) {
        Some(head) => {
            head.copy_from_slice(src);
            Ok(())
        }
        None => Err(BufferTooSmall),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_writes_prefix_and_returns_tail() {
        let mut buf = [0_u8; 4];
        let rest = put(&mut buf, &[1, 2]);
        assert_eq!(rest.len(), 2);
        let rest = put(rest, &[3, 4]);
        assert_eq!(rest, []);
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn put_too_short_writes_nothing() {
        let mut buf = [9_u8; 2];
        let rest = put(&mut buf, &[1, 2, 3]);
        assert_eq!(rest, []);
        assert_eq!(buf, [9, 9]);
    }

    #[test]
    fn take_splits_or_fails() {
        assert_eq!(take::<2>(&[1, 2, 3]), Some(([1, 2], &[3_u8][..])));
        assert_eq!(take::<3>(&[1, 2]), None);
    }

    #[test]
    fn write_prefix_leaves_tail_untouched() {
        let mut buf = [0xAA_u8; 4];
        assert_eq!(write_prefix(&mut buf, &[1, 2]), Ok(()));
        assert_eq!(buf, [1, 2, 0xAA, 0xAA]);
        let mut short = [0xAA_u8; 1];
        assert_eq!(write_prefix(&mut short, &[1, 2]), Err(BufferTooSmall));
        assert_eq!(short, [0xAA]);
    }

    #[test]
    fn concat_in_const_context() {
        const C: [u8; 5] = concat(&[1, 2], &[3, 4, 5]);
        assert_eq!(C, [1, 2, 3, 4, 5]);
    }
}

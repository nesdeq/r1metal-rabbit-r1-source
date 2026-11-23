//! Numbers read out of bytes, bytes compared, and a C string's bytes.

pub(crate) fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

pub(crate) fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*bytes.get(at..)?.first_chunk()?))
}

pub(crate) fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(*bytes.get(at..)?.first_chunk()?))
}

pub(crate) fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(*bytes.get(at..)?.first_chunk()?))
}

pub(crate) fn ne32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(*bytes.get(at..)?.first_chunk()?))
}

pub(crate) fn ne64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(*bytes.get(at..)?.first_chunk()?))
}

// Takes the same time wherever the bytes differ: it compares secrets.
pub(crate) fn same(one: &[u8], other: &[u8]) -> bool {
    one.len() == other.len() && one.iter().zip(other).fold(0, |differing, (a, b)| differing | (a ^ b)) == 0
}

/// The bytes of a C string in a field of its own: up to its first zero.
pub fn until_zero(field: &[u8]) -> &[u8] {
    &field[..field.iter().position(|&byte| byte == 0).unwrap_or(field.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn until_zero_stops_at_zero() {
        let names = (until_zero(b"On\0\0junk"), until_zero(b"whole"), until_zero(b"\0"));
        assert_eq!(names, (&b"On"[..], &b"whole"[..], &b""[..]));
    }

    #[test]
    fn reads_numbers_within_bounds() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        assert_eq!((le16(&bytes, 1), be16(&bytes, 1)), (Some(0x0302), Some(0x0203)));
        assert_eq!((le32(&bytes, 5), be32(&bytes, 5)), (Some(0x0908_0706), Some(0x0607_0809)));
        assert_eq!(
            (ne32(&bytes, 0), ne64(&bytes, 1)),
            (Some(u32::from_ne_bytes([1, 2, 3, 4])), Some(u64::from_ne_bytes([2, 3, 4, 5, 6, 7, 8, 9])))
        );
        assert_eq!((le16(&bytes, 8), be16(&bytes, 9)), (None, None));
        assert_eq!(
            (le32(&bytes, 6), be32(&bytes, usize::MAX), ne32(&bytes, 6), ne64(&bytes, 2)),
            (None, None, None, None)
        );
    }

    #[test]
    fn same_compares_length_and_bytes() {
        assert!(same(&[1, 2, 3], &[1, 2, 3]) && !same(&[1, 2, 3], &[1, 2, 4]) && !same(&[1, 2], &[1, 2, 3]));
    }
}

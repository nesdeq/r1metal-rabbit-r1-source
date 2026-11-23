//! The vbmeta that r1metal flashes, and the flags any vbmeta holds.

/// A vbmeta with both disable flags and no descriptors, so the bootloader has no boot image to check.
pub const OPEN: &[u8] = include_bytes!("../assets/vbmeta-open.img");
const MAGIC: &[u8; 4] = b"AVB0";
// Byte offset of the big endian flags word in AvbVBMetaImageHeader, and the header's size.
const FLAGS_OFFSET: usize = 120;
const HEADER_SIZE: usize = 256;

/// The flags word of a vbmeta; none for what is none.
pub fn flags(image: &[u8]) -> Option<u32> {
    if image.len() < HEADER_SIZE || &image[..MAGIC.len()] != MAGIC {
        return None;
    }
    Some(u32::from_be_bytes(image[FLAGS_OFFSET..FLAGS_OFFSET + 4].try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAG_HASHTREE_DISABLED: u32 = 1 << 0;
    const FLAG_VERIFICATION_DISABLED: u32 = 1 << 1;
    // Size of vbmeta_a and vbmeta_b in the stock partition table (MT6765_Android_scatter.txt).
    const PARTITION_SIZE: usize = 0x80_0000;

    #[test]
    fn open_vbmeta_disables_verification_and_hashtrees() {
        assert_eq!(flags(OPEN), Some(FLAG_HASHTREE_DISABLED | FLAG_VERIFICATION_DISABLED));
    }

    #[test]
    fn open_vbmeta_fits_its_partition() {
        assert!(OPEN.len() <= PARTITION_SIZE);
    }

    #[test]
    fn refuses_other_blobs() {
        assert_eq!(flags(&[0u8; HEADER_SIZE]), None);
        assert_eq!(flags(b"AVB0"), None);
    }

    #[test]
    #[ignore = "needs the stock firmware under stock/"]
    fn stock_vbmeta_has_verification_on() {
        assert_eq!(flags(&std::fs::read(crate::stock("vbmeta.img")).unwrap()), Some(0));
    }
}

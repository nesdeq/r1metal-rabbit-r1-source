const CRC_POLYNOMIAL: u32 = 0xedb8_8320;
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut byte = 0;
    while byte < table.len() {
        let (mut crc, mut step) = (byte as u32, 0);
        while step < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ CRC_POLYNOMIAL } else { crc >> 1 };
            step += 1;
        }
        table[byte] = crc;
        byte += 1;
    }
    table
};
#[cfg(test)]
const FNV_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
#[cfg(test)]
const FNV_PRIME: u64 = 0x100_0000_01b3;

pub(crate) fn crc32_from(seed: u32, bytes: &[u8]) -> u32 {
    bytes.iter().fold(seed, |crc, &byte| CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8))
}

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    !crc32_from(!0, bytes)
}

#[cfg(test)]
pub(crate) fn fnv_from<'a>(hash: u64, bytes: impl IntoIterator<Item = &'a u8>) -> u64 {
    bytes.into_iter().fold(hash, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME))
}

#[cfg(test)]
pub(crate) fn fnv<'a>(bytes: impl IntoIterator<Item = &'a u8>) -> u64 {
    fnv_from(FNV_BASIS, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_zlib() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!((crc32(b""), crc32_from(0, b"")), (0, 0));
    }
}

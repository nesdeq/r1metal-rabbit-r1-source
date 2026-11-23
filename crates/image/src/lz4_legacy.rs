//! LZ4 legacy frame format, the one `lz4 -l` writes and the kernel's unlz4 reads.

use crate::invalid;
use std::io;

const MAGIC: u32 = 0x184C_2102;
// Uncompressed bytes per block, fixed by the format.
const BLOCK: usize = 8 << 20;

pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut out = MAGIC.to_le_bytes().to_vec();
    for chunk in data.chunks(BLOCK) {
        let block = lz4_flex::block::compress(chunk);
        out.extend_from_slice(&(block.len() as u32).to_le_bytes());
        out.extend_from_slice(&block);
    }
    out
}

fn take_u32(data: &mut &[u8]) -> Option<u32> {
    let (head, rest) = data.split_first_chunk::<4>()?;
    *data = rest;
    Some(u32::from_le_bytes(*head))
}

pub fn decompress(mut data: &[u8]) -> io::Result<Vec<u8>> {
    if take_u32(&mut data) != Some(MAGIC) {
        return Err(invalid("not an LZ4 legacy stream"));
    }
    let truncated = || invalid("LZ4 legacy stream ends inside a block");
    let mut out = Vec::new();
    while !data.is_empty() {
        let size = take_u32(&mut data).ok_or_else(truncated)?;
        if size == MAGIC {
            continue;
        }
        let (block, rest) = data.split_at_checked(size as usize).ok_or_else(truncated)?;
        // A block is decoded where it ends up, in the room a whole block takes at the output's end.
        let start = out.len();
        out.resize(start + BLOCK, 0);
        let decoded = lz4_flex::block::decompress_into(block, &mut out[start..])
            .map_err(|e| invalid(&format!("LZ4 block is corrupt: {e}")))?;
        out.truncate(start + decoded);
        data = rest;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_across_blocks() {
        let data: Vec<u8> = (0..BLOCK + 12345).map(|i| (i % 251) as u8).collect();
        let packed = compress(&data);
        assert_eq!(&packed[..4], &[0x02, 0x21, 0x4C, 0x18]);
        assert!(decompress(&packed).unwrap() == data);
    }

    #[test]
    fn rejects_other_formats_and_cut_streams() {
        let refusal = |bytes: &[u8]| decompress(bytes).unwrap_err().to_string();
        assert_eq!(refusal(b"\x1f\x8b\x08\x00"), "not an LZ4 legacy stream");
        let packed = compress(&[7u8; 4096]);
        assert_eq!(refusal(&packed[..packed.len() - 1]), "LZ4 legacy stream ends inside a block");
    }

    #[test]
    #[ignore = "needs the stock ramdisk and its cpio unpacked beside the stock firmware into unpacked/"]
    fn decodes_stock_ramdisk() {
        let packed = std::fs::read(crate::unpacked("boot/ramdisk")).unwrap();
        let reference = std::fs::read(crate::unpacked("ramdisk.cpio")).unwrap();
        assert!(decompress(&packed).unwrap() == reference);
    }
}

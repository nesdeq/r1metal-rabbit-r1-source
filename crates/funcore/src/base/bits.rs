use alloc::vec::Vec;

pub(crate) const H264_ESCAPE: u8 = 3;

pub(crate) const BYTE_BITS: usize = u8::BITS as usize;

#[derive(Default)]
pub(crate) struct BitWriter {
    bytes: Vec<u8>,
    pending: u64,
    pending_bits: u32,
}

impl BitWriter {
    pub(crate) fn with_bytes(bytes: Vec<u8>) -> BitWriter {
        BitWriter { bytes, pending: 0, pending_bits: 0 }
    }

    pub(crate) fn put(&mut self, value: u32, count: u32) {
        self.pending = self.pending << count | u64::from(value) & ((1 << count) - 1);
        self.pending_bits += count;
        while self.pending_bits >= u8::BITS {
            self.pending_bits -= u8::BITS;
            self.bytes.push((self.pending >> self.pending_bits) as u8);
        }
    }

    pub(crate) fn bit_count(&self) -> usize {
        self.bytes.len() * BYTE_BITS + self.pending_bits as usize
    }

    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.pending_bits != 0 {
            self.put(0, u8::BITS - self.pending_bits);
        }
        self.bytes
    }
}

#[derive(Clone)]
pub(crate) struct BitReader<'a> {
    pub(crate) bytes: &'a [u8],
    bit: usize,
    skips_escapes: bool,
}

impl<'a> BitReader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> BitReader<'a> {
        BitReader { bytes, bit: 0, skips_escapes: false }
    }

    pub(crate) const fn with_escapes(bytes: &'a [u8]) -> BitReader<'a> {
        BitReader { bytes, bit: 0, skips_escapes: true }
    }

    fn is_at_escape(&self) -> bool {
        if !self.skips_escapes || !self.bit.is_multiple_of(BYTE_BITS) {
            return false;
        }
        let byte = self.bit / BYTE_BITS;
        byte >= 2 && self.bytes[byte - 2..].starts_with(&[0, 0, H264_ESCAPE])
    }

    // Position of the next bit, past an escape byte at it.
    pub(crate) fn position(&self) -> usize {
        self.bit + if self.is_at_escape() { BYTE_BITS } else { 0 }
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.bytes.len() * BYTE_BITS - self.bit
    }

    pub(crate) fn bit(&mut self) -> Option<u32> {
        self.bit = self.position();
        let byte = *self.bytes.get(self.bit / BYTE_BITS)?;
        let shift = BYTE_BITS - 1 - self.bit % BYTE_BITS;
        self.bit += 1;
        Some(u32::from(byte >> shift & 1))
    }

    pub(crate) fn flag(&mut self) -> Option<bool> {
        Some(self.bit()? == 1)
    }

    // The next `count` bits, up to 32, highest first; none, reading nothing, if fewer are left.
    pub(crate) fn take(&mut self, count: u32) -> Option<u32> {
        let start = self.clone();
        let mut value = 0;
        for _ in 0..count {
            let Some(bit) = self.bit() else {
                *self = start;
                return None;
            };
            value = value << 1 | bit;
        }
        Some(value)
    }

    // Passes over the next `count` bits; none, passing over nothing, if fewer are left.
    pub(crate) fn skip(&mut self, count: usize) -> Option<()> {
        let start = self.clone();
        for _ in 0..count {
            if self.bit().is_none() {
                *self = start;
                return None;
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn writer_appends_and_pads() {
        let mut writer = BitWriter::with_bytes(vec![0xEE]);
        writer.put(0b101, 3);
        writer.put(0, 1);
        writer.put(0xABCD_1234, 32);
        assert_eq!(writer.bit_count(), 8 + 36);
        writer.put(0b1111_0001, 1);
        writer.put(0, 0);
        assert_eq!(writer.finish(), [0xEE, 0b1010_1010, 0xBC, 0xD1, 0x23, 0b0100_1000]);
        let mut whole = BitWriter::default();
        whole.put(0xAB, 8);
        assert_eq!((whole.bit_count(), whole.finish()), (8, vec![0xAB]));
        assert!(BitWriter::default().finish().is_empty());
    }

    #[test]
    fn reader_round_trip() {
        let mut writer = BitWriter::default();
        writer.put(0b101, 3);
        writer.put(0, 1);
        writer.put(0xABCD_1234, 32);
        writer.put(1, 1);
        let bytes = writer.finish();
        let mut reader = BitReader::new(&bytes);
        assert_eq!(
            (reader.take(3), reader.take(1), reader.take(32), reader.bit(), reader.remaining()),
            (Some(5), Some(0), Some(0xABCD_1234), Some(1), 3)
        );
        assert_eq!(reader.take(4), None);
        assert_eq!((reader.take(3), reader.take(0), reader.bit(), reader.flag()), (Some(0), Some(0), None, None));
        let mut flags = BitReader::new(&[0b1000_0000]);
        assert_eq!((flags.flag(), flags.flag()), (Some(true), Some(false)), "a bit as whether it is set");
    }

    #[test]
    fn reader_skips_escape() {
        let bytes = [0, 0, H264_ESCAPE, 0xFF];
        let mut reader = BitReader::with_escapes(&bytes);
        assert_eq!(reader.take(16), Some(0));
        assert_eq!(reader.take(8), Some(0xFF));
    }

    #[test]
    fn reader_skips_bits() {
        let bytes = [0, 0, H264_ESCAPE, 0xA5, 0x3C];
        let mut reader = BitReader::with_escapes(&bytes);
        assert_eq!((reader.skip(20), reader.take(4)), (Some(()), Some(0x5)), "past an escape byte as a read is");
        assert_eq!((reader.skip(9), reader.remaining()), (None, 8), "more than is left, and nothing is passed over");
        assert_eq!((reader.skip(0), reader.skip(8), reader.bit()), (Some(()), Some(()), None));
        let mut plain = BitReader::new(&bytes);
        assert_eq!((plain.skip(16), plain.take(8)), (Some(()), Some(u32::from(H264_ESCAPE))));
    }
}

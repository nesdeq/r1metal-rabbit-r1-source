use alloc::vec::Vec;

#[derive(Default)]
pub(super) struct Bits {
    bytes: Vec<u8>,
    buffer: u64,
    count: u32,
}

impl Bits {
    pub(super) fn add(&mut self, value: u64, count: u32) {
        self.buffer |= (value & ((1u64 << count) - 1)) << self.count;
        self.count += count;
        while self.count >= 8 {
            self.bytes.push(self.buffer as u8);
            (self.buffer, self.count) = (self.buffer >> 8, self.count - 8);
        }
    }

    pub(super) fn into_bytes(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.bytes.push(self.buffer as u8);
        }
        self.bytes
    }

    pub(super) fn close(mut self) -> Vec<u8> {
        self.add(1, 1);
        self.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_bits_first_and_end_mark() {
        let mut bits = Bits::default();
        bits.add(0b101, 3);
        bits.add(0xFFFF_FFFF_0000_0001, 1);
        bits.add(0x1234_5678, 32);
        assert_eq!(
            bits.into_bytes(),
            [0x8D, 0x67, 0x45, 0x23, 0x01],
            "what does not fill its last byte is padded with nothing"
        );
        let mut bits = Bits::default();
        bits.add(0, 4);
        bits.add(1, 4);
        bits.add(1, 2);
        bits.add(1, 1);
        assert_eq!(bits.close(), [0b0001_0000, 0b0000_1101], "RFC 8878, 4.2.2: the literals 0145, written backwards");
        assert_eq!(Bits::default().close(), [1], "a stream of nothing is its mark");
    }
}

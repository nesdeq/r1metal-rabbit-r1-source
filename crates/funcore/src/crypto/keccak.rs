const STATE_BITS: usize = 1600;
const ROW: usize = 5;
const LANES: usize = ROW * ROW;
const SHA3_SUFFIX: u8 = 0x06;
const SHAKE_SUFFIX: u8 = 0x1F;
const LAST_PAD_BIT: u8 = 0x80;
const ROUND_CONSTANTS: [u64; 24] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_8082,
    0x8000_0000_0000_808A,
    0x8000_0000_8000_8000,
    0x0000_0000_0000_808B,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8009,
    0x0000_0000_0000_008A,
    0x0000_0000_0000_0088,
    0x0000_0000_8000_8009,
    0x0000_0000_8000_000A,
    0x0000_0000_8000_808B,
    0x8000_0000_0000_008B,
    0x8000_0000_0000_8089,
    0x8000_0000_0000_8003,
    0x8000_0000_0000_8002,
    0x8000_0000_0000_0080,
    0x0000_0000_0000_800A,
    0x8000_0000_8000_000A,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8080,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8008,
];
const RHO_OFFSETS: [u32; 24] =
    [1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44];
const PI_LANES: [usize; 24] = [10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1];

fn theta(lanes: &mut [u64; LANES]) {
    let parity = |at: usize| (0..ROW).fold(0, |parity, row| parity ^ lanes[row * ROW + at]);
    let parities: [u64; ROW] = core::array::from_fn(parity);
    for at in 0..ROW {
        let mix = parities[(at + ROW - 1) % ROW] ^ parities[(at + 1) % ROW].rotate_left(1);
        (0..ROW).for_each(|row| lanes[row * ROW + at] ^= mix);
    }
}

fn rho_pi(lanes: &mut [u64; LANES]) {
    let mut last = lanes[1];
    for (offset, lane) in RHO_OFFSETS.iter().zip(PI_LANES) {
        let next = lanes[lane];
        lanes[lane] = last.rotate_left(*offset);
        last = next;
    }
}

fn chi(lanes: &mut [u64; LANES]) {
    for row in lanes.as_chunks_mut::<ROW>().0 {
        let before = *row;
        for at in 0..ROW {
            row[at] = before[at] ^ (!before[(at + 1) % ROW] & before[(at + 2) % ROW]);
        }
    }
}

fn permute(lanes: &mut [u64; LANES]) {
    for constant in ROUND_CONSTANTS {
        theta(lanes);
        rho_pi(lanes);
        chi(lanes);
        lanes[0] ^= constant;
    }
}

#[derive(Clone)]
pub(crate) struct Sponge {
    lanes: [u64; LANES],
    rate: usize,
    at: usize,
    suffix: u8,
    is_squeezing: bool,
}

impl Sponge {
    fn new(bits: usize, suffix: u8) -> Sponge {
        let rate = (STATE_BITS - 2 * bits) / 8;
        Sponge { lanes: [0; LANES], rate, at: 0, suffix, is_squeezing: false }
    }

    pub(crate) fn shake128() -> Sponge {
        Sponge::new(128, SHAKE_SUFFIX)
    }

    fn xor(&mut self, at: usize, byte: u8) {
        self.lanes[at / 8] ^= u64::from(byte) << (8 * (at % 8));
    }

    pub(crate) fn absorb(&mut self, bytes: &[u8]) {
        debug_assert!(!self.is_squeezing, "a sponge absorbs before it is squeezed");
        for &byte in bytes {
            self.xor(self.at, byte);
            self.at += 1;
            if self.at == self.rate {
                permute(&mut self.lanes);
                self.at = 0;
            }
        }
    }

    pub(crate) fn squeeze(&mut self, out: &mut [u8]) {
        if !self.is_squeezing {
            self.xor(self.at, self.suffix);
            self.xor(self.rate - 1, LAST_PAD_BIT);
            permute(&mut self.lanes);
            self.at = 0;
            self.is_squeezing = true;
        }
        for byte in out {
            if self.at == self.rate {
                permute(&mut self.lanes);
                self.at = 0;
            }
            *byte = (self.lanes[self.at / 8] >> (8 * (self.at % 8))) as u8;
            self.at += 1;
        }
    }
}

fn digest_into(mut sponge: Sponge, parts: &[&[u8]], out: &mut [u8]) {
    parts.iter().for_each(|part| sponge.absorb(part));
    sponge.squeeze(out);
}

fn digest<const N: usize>(sponge: Sponge, parts: &[&[u8]]) -> [u8; N] {
    let mut out = [0u8; N];
    digest_into(sponge, parts, &mut out);
    out
}

pub(crate) fn sha3_256(parts: &[&[u8]]) -> [u8; 32] {
    digest(Sponge::new(256, SHA3_SUFFIX), parts)
}

pub(crate) fn sha3_512(parts: &[&[u8]]) -> [u8; 64] {
    digest(Sponge::new(512, SHA3_SUFFIX), parts)
}

pub(crate) fn shake256(parts: &[&[u8]], out: &mut [u8]) {
    digest_into(Sponge::new(256, SHAKE_SUFFIX), parts, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::code::hex;
    use alloc::vec;
    use alloc::vec::Vec;

    fn ramp(length: u8) -> Vec<u8> {
        (0..length).collect()
    }

    #[test]
    fn sha3_fips_202_vectors() {
        let long = ramp(200);
        assert_eq!(hex(&sha3_256(&[])), "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a");
        assert_eq!(hex(&sha3_256(&[b"abc"])), "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532");
        assert_eq!(hex(&sha3_256(&[&long])), "5f728f63bf5ee48c77f453c0490398fa645b8d4c4e56be9a41cfec344d6ca899");
        assert_eq!(
            hex(&sha3_512(&[b"abc"])),
            "b751850b1a57168a5693cd924b6b096e08f621827444f70d884f5d0240d2712e10e116e9192af3c91a7ec57647e3934057340b4cf408d5a56592f8274eec53f0"
        );
        assert_eq!(
            hex(&sha3_512(&[&long])),
            "ea5d05f19348dd589793354793a15f37a73b4c0bb4e750b9a00757dfce2f8b65a64191bb9b137de00feef6474cfd47abf7880efbc51614a5715df12cfe0caee3"
        );
    }

    #[test]
    fn shake_fips_202_vectors() {
        let long = ramp(200);
        let mut out = [0u8; 32];
        let mut sponge = Sponge::shake128();
        sponge.absorb(b"abc");
        sponge.squeeze(&mut out);
        assert_eq!(hex(&out), "5881092dd818bf5cf8a3ddb793fbcba74097d5c526a6d35f97b83351940f2cc8");

        let mut sponge = Sponge::shake128();
        sponge.absorb(&long);
        let mut stream = vec![0u8; 504];
        stream.chunks_mut(7).for_each(|piece| sponge.squeeze(piece));
        assert_eq!(hex(&stream[..32]), "0c4234ca1e31801ae606f8b8d8e0665c66f42a21d601c2681858a92c79ad5d69");
        assert_eq!(hex(&stream[472..]), "18b8846cd7ea7157edf67f032326273a8348b334043eaadccff17512e2dca94f");

        let mut out = [0u8; 64];
        shake256(&[b"a", b"bc"], &mut out);
        assert_eq!(
            hex(&out),
            "483366601360a8771c6863080cc4114d8db44530f8f1e1ee4f94ea37e78b5739d5a15bef186a5386c75744c0527e1faa9f8726e462a12a4feb06bd8801e751e4"
        );
        let mut out = vec![0u8; 300];
        shake256(&[&long[..100], &long[100..]], &mut out);
        assert_eq!(hex(&out[268..]), "c53c23e716c670c4db23c67901358ae64f3f0ccedfa05b29e84e1a11a635bfe7");
    }
}

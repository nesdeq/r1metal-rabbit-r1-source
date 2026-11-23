use crate::crypto::SHA256_BYTES;

pub(crate) const HASH_BYTES: usize = SHA256_BYTES;
const BLOCK: usize = 64;
const ROUNDS: usize = 10;
const FANOUT_AND_DEPTH: u32 = 0x0101_0000;
const INNER_PAD: u8 = 0x36;
const OUTER_PAD: u8 = 0x5C;
const FIRST: [u32; 8] =
    [0x6A09_E667, 0xBB67_AE85, 0x3C6E_F372, 0xA54F_F53A, 0x510E_527F, 0x9B05_688C, 0x1F83_D9AB, 0x5BE0_CD19];
const SIGMA: [[usize; 16]; ROUNDS] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
    [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
    [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
    [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
    [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
    [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
    [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
    [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
];
const MIXES: [[usize; 4]; 8] = [
    [0, 4, 8, 12],
    [1, 5, 9, 13],
    [2, 6, 10, 14],
    [3, 7, 11, 15],
    [0, 5, 10, 15],
    [1, 6, 11, 12],
    [2, 7, 8, 13],
    [3, 4, 9, 14],
];

fn mix(v: &mut [u32; 16], [a, b, c, d]: [usize; 4], x: u32, y: u32) {
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
    v[d] = (v[d] ^ v[a]).rotate_right(16);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(12);
    v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
    v[d] = (v[d] ^ v[a]).rotate_right(8);
    v[c] = v[c].wrapping_add(v[d]);
    v[b] = (v[b] ^ v[c]).rotate_right(7);
}

fn compress(hash: &mut [u32; 8], block: &[u8; BLOCK], count: u64, is_last: bool) {
    let words: [u32; 16] =
        core::array::from_fn(|at| u32::from_le_bytes(*block[4 * at..].first_chunk().expect("four of sixty-four")));
    let mut v: [u32; 16] = core::array::from_fn(|at| if at < 8 { hash[at] } else { FIRST[at - 8] });
    v[12] ^= count as u32;
    v[13] ^= (count >> u32::BITS) as u32;
    if is_last {
        v[14] = !v[14];
    }
    for order in SIGMA {
        for (at, places) in MIXES.into_iter().enumerate() {
            mix(&mut v, places, words[order[2 * at]], words[order[2 * at + 1]]);
        }
    }
    for at in 0..8 {
        hash[at] ^= v[at] ^ v[at + 8];
    }
}

// BLAKE2s (RFC 7693) of a message given in pieces, plain or under a key of 32 bytes at most.
struct Hasher {
    hash: [u32; 8],
    block: [u8; BLOCK],
    held: usize,
    count: u64,
}

impl Hasher {
    fn new(key: &[u8], length: usize) -> Hasher {
        let mut hash = FIRST;
        hash[0] ^= FANOUT_AND_DEPTH ^ (key.len() as u32) << 8 ^ length as u32;
        let mut hasher = Hasher { hash, block: [0; BLOCK], held: 0, count: 0 };
        if !key.is_empty() {
            hasher.block[..key.len()].copy_from_slice(key);
            hasher.held = BLOCK;
        }
        hasher
    }

    fn update(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            // A full block is compressed only once more follows: the last one is marked as such.
            if self.held == BLOCK {
                self.count += BLOCK as u64;
                compress(&mut self.hash, &self.block, self.count, false);
                self.held = 0;
            }
            let taken = bytes.len().min(BLOCK - self.held);
            self.block[self.held..self.held + taken].copy_from_slice(&bytes[..taken]);
            (self.held, bytes) = (self.held + taken, &bytes[taken..]);
        }
    }

    fn finish<const N: usize>(mut self) -> [u8; N] {
        self.count += self.held as u64;
        self.block[self.held..].fill(0);
        compress(&mut self.hash, &self.block, self.count, true);
        let mut bytes = [0u8; HASH_BYTES];
        bytes.as_chunks_mut::<4>().0.iter_mut().zip(self.hash).for_each(|(bytes, word)| *bytes = word.to_le_bytes());
        core::array::from_fn(|at| bytes[at])
    }
}

pub(crate) fn hash(parts: &[&[u8]]) -> [u8; HASH_BYTES] {
    keyed(&[], parts)
}

// The hash of `N` bytes under a key of 32 bytes at most, which BLAKE2s takes in its first block.
pub(crate) fn keyed<const N: usize>(key: &[u8], parts: &[&[u8]]) -> [u8; N] {
    let mut hasher = Hasher::new(key, N);
    parts.iter().for_each(|part| hasher.update(part));
    hasher.finish()
}

// HMAC (RFC 2104) over the plain hash, as Noise asks of every hash.
pub(crate) fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; HASH_BYTES] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..HASH_BYTES].copy_from_slice(&hash(&[key]));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Hasher::new(&[], HASH_BYTES);
    inner.update(&block.map(|byte| byte ^ INNER_PAD));
    parts.iter().for_each(|part| inner.update(part));
    hash(&[&block.map(|byte| byte ^ OUTER_PAD), &inner.finish::<HASH_BYTES>()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::code::hex;
    use alloc::vec::Vec;

    fn ramp(length: u8) -> Vec<u8> {
        (0..length).collect()
    }

    #[test]
    fn rfc_7693_and_block_edges() {
        assert_eq!(
            hex(&hash(&[b"abc"])),
            "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982",
            "RFC 7693, appendix B"
        );
        assert_eq!(hex(&hash(&[])), "69217a3079908094e11121d042354a7c1f55b6482ca1a51e1b250dfd1ed0eef9");
        let long = ramp(200);
        assert_eq!(
            [hex(&hash(&[&long[..64]])), hex(&hash(&[&long[..65]])), hex(&hash(&[&long]))],
            [
                "56f34e8b96557e90c1f24b52d0c89d51086acf1b00f634cf1dde9233b8eaaa3e",
                "1b53ee94aaf34e4b159d48de352c7f0661d0a40edff95a0b1639b4090e974472",
                "6d244e1a06ce4ef578dd0f63aff0936706735119ca9c8d22d86c801414ab9741",
            ],
            "a whole block, one byte more, and four blocks, as Python's hashlib has them"
        );
        assert_eq!(
            hash(&[&long[..3], &[], &long[3..64], &long[64..131], &long[131..]]),
            hash(&[&long]),
            "in pieces it is the same hash"
        );
    }

    #[test]
    fn keyed_hashes() {
        let (key, long): ([u8; 32], _) = (core::array::from_fn(|at| at as u8), ramp(200));
        assert_eq!(hex(&keyed::<16>(&key, &[&long])), "42a6f8ff590af21b512e9e088257aa34");
        assert_eq!(hex(&keyed::<16>(&key, &[])), "9536f9b267655743dee97b8a670f9f53", "the key's block is the last");
        assert_eq!(
            hex(&keyed::<32>(&key, &[b"a", b"bc"])),
            "a281f725754969a702f6fe36fc591b7def866e4b70173ece402fc01c064d6b65"
        );
    }

    #[test]
    fn hmac_over_the_hash() {
        let long = ramp(200);
        assert_eq!(
            hex(&hmac(b"key", &[b"The quick ", b"brown fox"])),
            "8717096fc5e7ac53d11d990acd4380c9ddd7bd8da5d8b5bc29855e6c98a866a4"
        );
        assert_eq!(
            hex(&hmac(&long[..100], &[&long])),
            "c4c95271905c6f69c1dd7c6fbc0cb3735457a660ccc2c4370f28825e7abef5af",
            "a key longer than a block is hashed first"
        );
    }
}

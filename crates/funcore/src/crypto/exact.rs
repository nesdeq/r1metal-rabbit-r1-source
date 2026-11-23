use alloc::vec;
use alloc::vec::Vec;

const SHA256_ROUNDS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];
const SHA256_FIRST: [u32; 8] =
    [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
const SHA1_FIRST: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
const SHA1_STAGES: [u32; 4] = [0x5a827999, 0x6ed9eba1, 0x8f1bbcdc, 0xca62c1d6];
const MD5_FIRST: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
const MD5_ROUNDS: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501, 0x698098d8,
    0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340,
    0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87,
    0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
    0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039,
    0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92,
    0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
    0xeb86d391,
];
const MD5_SHIFTS: [[u32; 4]; 4] = [[7, 12, 17, 22], [5, 9, 14, 20], [4, 11, 16, 23], [6, 10, 15, 21]];
// The bytes of a block of MD5, SHA-1 and SHA-256, which an HMAC over any of them pads its key to.
const BLOCK: usize = 64;

// The message filled up to whole blocks, its length in bits at the end as the hash writes it.
fn to_blocks(parts: &[&[u8]], length: fn(u64) -> [u8; 8]) -> Vec<[u8; BLOCK]> {
    let mut message = parts.concat();
    let bits = (message.len() as u64) * 8;
    message.push(0x80);
    while message.len() % BLOCK != 56 {
        message.push(0);
    }
    message.extend(length(bits));
    message.as_chunks::<BLOCK>().0.to_vec()
}

pub(crate) fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = SHA256_FIRST;
    for block in to_blocks(parts, u64::to_be_bytes) {
        let mut w = [0u32; 64];
        for at in 0..64 {
            w[at] = if at < 16 {
                u32::from_be_bytes(block[4 * at..4 * at + 4].try_into().unwrap())
            } else {
                let s0 = w[at - 15].rotate_right(7) ^ w[at - 15].rotate_right(18) ^ (w[at - 15] >> 3);
                let s1 = w[at - 2].rotate_right(17) ^ w[at - 2].rotate_right(19) ^ (w[at - 2] >> 10);
                w[at - 16].wrapping_add(s0).wrapping_add(w[at - 7]).wrapping_add(s1)
            };
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = hash;
        for at in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let first = h.wrapping_add(s1).wrapping_add(choice).wrapping_add(SHA256_ROUNDS[at]).wrapping_add(w[at]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let second = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(first), c, b, a, first.wrapping_add(second));
        }
        hash.iter_mut().zip([a, b, c, d, e, f, g, h]).for_each(|(word, sum)| *word = word.wrapping_add(sum));
    }
    let mut out = [0u8; 32];
    out.as_chunks_mut::<4>().0.iter_mut().zip(hash).for_each(|(bytes, word)| *bytes = word.to_be_bytes());
    out
}

// RFC 1321: words and the length lowest byte first.
pub(crate) fn md5(parts: &[&[u8]]) -> [u8; 16] {
    let mut hash = MD5_FIRST;
    for block in to_blocks(parts, u64::to_le_bytes) {
        let words: [u32; 16] =
            core::array::from_fn(|at| u32::from_le_bytes(block[4 * at..4 * at + 4].try_into().unwrap()));
        let [mut a, mut b, mut c, mut d] = hash;
        for at in 0..64 {
            let (mixed, word) = match at / 16 {
                0 => ((b & c) | (!b & d), at),
                1 => ((b & d) | (c & !d), (5 * at + 1) % 16),
                2 => (b ^ c ^ d, (3 * at + 5) % 16),
                _ => (c ^ (b | !d), (7 * at) % 16),
            };
            let sum = a.wrapping_add(mixed).wrapping_add(MD5_ROUNDS[at]).wrapping_add(words[word]);
            (a, b, c, d) = (d, b.wrapping_add(sum.rotate_left(MD5_SHIFTS[at / 16][at % 4])), b, c);
        }
        hash.iter_mut().zip([a, b, c, d]).for_each(|(word, sum)| *word = word.wrapping_add(sum));
    }
    let mut out = [0u8; 16];
    out.as_chunks_mut::<4>().0.iter_mut().zip(hash).for_each(|(bytes, word)| *bytes = word.to_le_bytes());
    out
}

pub(crate) fn sha1(parts: &[&[u8]]) -> [u8; 20] {
    let mut hash = SHA1_FIRST;
    for block in to_blocks(parts, u64::to_be_bytes) {
        let mut w = [0u32; 80];
        for at in 0..80 {
            w[at] = if at < 16 {
                u32::from_be_bytes(block[4 * at..4 * at + 4].try_into().unwrap())
            } else {
                (w[at - 3] ^ w[at - 8] ^ w[at - 14] ^ w[at - 16]).rotate_left(1)
            };
        }
        let [mut a, mut b, mut c, mut d, mut e] = hash;
        for at in 0..80 {
            let f = match at / 20 {
                0 => (b & c) | (!b & d),
                2 => (b & c) | (b & d) | (c & d),
                _ => b ^ c ^ d,
            };
            let next =
                a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(SHA1_STAGES[at / 20]).wrapping_add(w[at]);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, next);
        }
        hash.iter_mut().zip([a, b, c, d, e]).for_each(|(word, sum)| *word = word.wrapping_add(sum));
    }
    let mut out = [0u8; 20];
    out.as_chunks_mut::<4>().0.iter_mut().zip(hash).for_each(|(bytes, word)| *bytes = word.to_be_bytes());
    out
}

const SHA512_ROUNDS: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];
const SHA512_FIRST: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];

pub(crate) fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut message = parts.concat();
    let bits = (message.len() as u128) * 8;
    message.push(0x80);
    while message.len() % 128 != 112 {
        message.push(0);
    }
    message.extend(bits.to_be_bytes());
    let mut hash = SHA512_FIRST;
    for block in message.as_chunks::<128>().0 {
        let mut w = [0u64; 80];
        for at in 0..80 {
            w[at] = if at < 16 {
                u64::from_be_bytes(block[8 * at..8 * at + 8].try_into().unwrap())
            } else {
                let s0 = w[at - 15].rotate_right(1) ^ w[at - 15].rotate_right(8) ^ (w[at - 15] >> 7);
                let s1 = w[at - 2].rotate_right(19) ^ w[at - 2].rotate_right(61) ^ (w[at - 2] >> 6);
                w[at - 16].wrapping_add(s0).wrapping_add(w[at - 7]).wrapping_add(s1)
            };
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = hash;
        for at in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let choice = (e & f) ^ (!e & g);
            let first = h.wrapping_add(s1).wrapping_add(choice).wrapping_add(SHA512_ROUNDS[at]).wrapping_add(w[at]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let second = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(first), c, b, a, first.wrapping_add(second));
        }
        hash.iter_mut().zip([a, b, c, d, e, f, g, h]).for_each(|(word, sum)| *word = word.wrapping_add(sum));
    }
    let mut out = [0u8; 64];
    out.as_chunks_mut::<8>().0.iter_mut().zip(hash).for_each(|(bytes, word)| *bytes = word.to_be_bytes());
    out
}

// RFC 2104 over a hash of 64-byte blocks.
pub(crate) fn hmac<const N: usize>(hash: fn(&[&[u8]]) -> [u8; N], key: &[u8], parts: &[&[u8]]) -> [u8; N] {
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..N].copy_from_slice(&hash(&[key]));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let inner_key = block.map(|byte| byte ^ 0x36);
    let mut inner: Vec<&[u8]> = vec![&inner_key];
    inner.extend_from_slice(parts);
    let outer_key = block.map(|byte| byte ^ 0x5c);
    hash(&[&outer_key, &hash(&inner)])
}

// RFC 5869 over HMAC-SHA256; false for more than 255 blocks.
pub(crate) fn hkdf(salt: &[u8], secret: &[u8], info: &[&[u8]], out: &mut [u8]) -> bool {
    let key = hmac(sha256, salt, &[secret]);
    let mut block = Vec::new();
    for (counter, chunk) in out.chunks_mut(32).enumerate() {
        let Ok(count) = u8::try_from(counter + 1) else { return false };
        let mut parts: Vec<&[u8]> = vec![&block];
        parts.extend_from_slice(info);
        let count = [count];
        parts.push(&count);
        let next = hmac(sha256, &key, &parts);
        chunk.copy_from_slice(&next[..chunk.len()]);
        block = next.to_vec();
    }
    true
}

fn field_product(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0;
    while b != 0 {
        if b & 1 == 1 {
            product ^= a;
        }
        a = (a << 1) ^ if a & 0x80 == 0 { 0 } else { 0x1b };
        b >>= 1;
    }
    product
}

fn substitute(byte: u8) -> u8 {
    let (mut inverse, mut power, mut exponent) = (1u8, byte, 254u32);
    while exponent > 0 {
        if exponent & 1 == 1 {
            inverse = field_product(inverse, power);
        }
        power = field_product(power, power);
        exponent >>= 1;
    }
    let rotate = |by: u32| inverse.rotate_left(by);
    inverse ^ rotate(1) ^ rotate(2) ^ rotate(3) ^ rotate(4) ^ 0x63
}

// A block under AES with a key of 16 or of 32 bytes (FIPS 197).
pub(crate) fn aes(key: &[u8], block: &[u8; 16]) -> [u8; 16] {
    let key_words = key.len() / 4;
    let rounds = key_words + 6;
    let mut words: Vec<[u8; 4]> = key.as_chunks::<4>().0.to_vec();
    let mut constant = 1u8;
    while words.len() < 4 * (rounds + 1) {
        let mut word = words[words.len() - 1];
        if words.len().is_multiple_of(key_words) {
            word = [substitute(word[1]) ^ constant, substitute(word[2]), substitute(word[3]), substitute(word[0])];
            constant = field_product(constant, 2);
        } else if key_words == 8 && words.len() % 8 == 4 {
            word = word.map(substitute);
        }
        let previous = words[words.len() - key_words];
        words.push(core::array::from_fn(|at| word[at] ^ previous[at]));
    }
    let add_round_key = |state: &mut [u8; 16], round: usize| {
        for (at, byte) in state.iter_mut().enumerate() {
            *byte ^= words[4 * round + at / 4][at % 4];
        }
    };
    let mut state = *block;
    add_round_key(&mut state, 0);
    for round in 1..=rounds {
        let bytes = state.map(substitute);
        state = core::array::from_fn(|at| bytes[at % 4 + 4 * ((at / 4 + at % 4) % 4)]);
        if round < rounds {
            for column in state.as_chunks_mut::<4>().0 {
                let [a, b, c, d] = *column;
                let double = |byte| field_product(byte, 2);
                *column = [
                    double(a) ^ double(b) ^ b ^ c ^ d,
                    a ^ double(b) ^ double(c) ^ c ^ d,
                    a ^ b ^ double(c) ^ double(d) ^ d,
                    double(a) ^ a ^ b ^ c ^ double(d),
                ];
            }
        }
        add_round_key(&mut state, round);
    }
    state
}

// GHASH's product in GF(2^128), the bits taken highest first (NIST SP 800-38D, 6.3).
fn gf_product(x: u128, y: u128) -> u128 {
    let (mut product, mut shifted) = (0u128, y);
    for bit in (0..u128::BITS).rev() {
        if x >> bit & 1 == 1 {
            product ^= shifted;
        }
        shifted = if shifted & 1 == 1 { shifted >> 1 ^ GCM_REDUCTION } else { shifted >> 1 };
    }
    product
}

const GCM_REDUCTION: u128 = 0xe1 << 120;

// GHASH over the associated data and the ciphertext, each filled to whole blocks, then their lengths in bits (SP 800-38D, 6.4 and 7.1).
fn ghash(subkey: u128, extra: &[u8], cipher: &[u8]) -> u128 {
    let mut sum = 0u128;
    for part in [extra, cipher] {
        for piece in part.chunks(16) {
            let mut block = [0u8; 16];
            block[..piece.len()].copy_from_slice(piece);
            sum = gf_product(sum ^ u128::from_be_bytes(block), subkey);
        }
    }
    let lengths = ((extra.len() as u128 * 8) << 64) | (cipher.len() as u128 * 8);
    gf_product(sum ^ lengths, subkey)
}

// The counter block of a 12-byte nonce: the nonce, then the 32-bit count (SP 800-38D, 7.1).
fn counter_block(nonce: &[u8; 12], count: u32) -> [u8; 16] {
    let mut block = [0u8; 16];
    block[..12].copy_from_slice(nonce);
    block[12..].copy_from_slice(&count.to_be_bytes());
    block
}

// AES in counter mode in place, the counter block counted on as one 128-bit number (NIST SP 800-38A, 6.5).
pub(crate) fn ctr(key: &[u8], counter: &[u8; 16], data: &mut [u8]) {
    let mut block = u128::from_be_bytes(*counter);
    for piece in data.chunks_mut(16) {
        piece.iter_mut().zip(aes(key, &block.to_be_bytes())).for_each(|(byte, mask)| *byte ^= mask);
        block = block.wrapping_add(1);
    }
}

// GCM's tag takes the count 1, its data the counts from 2 on, whose 32 bits no plaintext GCM allows carries past (SP 800-38D, 5.2.1.1 and 7.1).
fn gcm_tag(key: &[u8], nonce: &[u8; 12], extra: &[u8], cipher: &[u8]) -> [u8; 16] {
    let subkey = u128::from_be_bytes(aes(key, &[0; 16]));
    (ghash(subkey, extra, cipher) ^ u128::from_be_bytes(aes(key, &counter_block(nonce, 1)))).to_be_bytes()
}

// AES-GCM with a 12-byte nonce and a 16-byte tag, a key of 16 or 32 bytes, in place: the tag.
pub(crate) fn gcm_seal(key: &[u8], nonce: &[u8; 12], extra: &[u8], data: &mut [u8]) -> [u8; 16] {
    ctr(key, &counter_block(nonce, 2), data);
    gcm_tag(key, nonce, extra, data)
}

// In place over the data and the tag behind it: false, the data left as it came, for what was not sealed so.
pub(crate) fn gcm_open(key: &[u8], nonce: &[u8; 12], extra: &[u8], sealed: &mut [u8]) -> bool {
    let Some(length) = sealed.len().checked_sub(16) else {
        return false;
    };
    let (data, tag) = sealed.split_at_mut(length);
    if gcm_tag(key, nonce, extra, data) != *tag {
        return false;
    }
    ctr(key, &counter_block(nonce, 2), data);
    true
}

#[test]
fn standards_vectors() {
    use crate::base::code::hex;
    let long = [b'a'; 1000];
    let two_blocks: &[u8] = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    assert_eq!(
        [hex(&sha256(&[b"abc"])), hex(&sha256(&[&two_blocks[..20], &two_blocks[20..]])), hex(&sha256(&[]))],
        [
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ],
        "FIPS 180, in parts too"
    );
    assert_eq!(
        hex(&sha256(&[&long])),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3",
        "sixteen blocks"
    );
    assert_eq!(
        [hex(&sha1(&[b"abc"])), hex(&sha1(&[two_blocks]))],
        ["a9993e364706816aba3e25717850c26c9cd0d89d", "84983e441c3bd26ebaae4aa1f95129e5e54670f1"],
        "FIPS 180"
    );
    let two_long_blocks: &[u8] = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
    assert_eq!(
        [hex(&sha512(&[b"abc"])), hex(&sha512(&[&two_long_blocks[..50], &two_long_blocks[50..]])), hex(&sha512(&[]))],
        [
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909",
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e",
        ],
        "FIPS 180, SHA-512, in parts too"
    );
    assert_eq!(
        hex(&hmac(sha256, &[0x0b; 20], &[b"Hi ", b"There"])),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
        "RFC 4231, 4.2"
    );
    assert_eq!(
        hex(&hmac(sha256, &[0xaa; 131], &[b"Test Using Larger Than Block-Size Key - Hash Key First"])),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54",
        "RFC 4231, 4.7: a key longer than a block"
    );
    assert_eq!(
        hex(&hmac(sha1, &[0x0b; 20], &[b"Hi There"])),
        "b617318655057264e28bc0b6fb378c8ef146be00",
        "RFC 2202, 3, its first case"
    );
    let digits = b"12345678901234567890123456789012345678901234567890123456789012345678901234567890";
    assert_eq!(
        [
            hex(&md5(&[])),
            hex(&md5(&[b"a"])),
            hex(&md5(&[b"abc"])),
            hex(&md5(&[b"message ", b"digest"])),
            hex(&md5(&[b"abcdefghijklmnopqrstuvwxyz"])),
            hex(&md5(&[b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"])),
            hex(&md5(&[digits])),
        ],
        [
            "d41d8cd98f00b204e9800998ecf8427e",
            "0cc175b9c0f1b6a831c399e269772661",
            "900150983cd24fb0d6963f7d28e17f72",
            "f96b697d7cb7938d525a2f31aaf161d0",
            "c3fcd3d76192e4007dfb496cca67e13b",
            "d174ab98d277d9f5a5611c2c9f419d9f",
            "57edf4a22be3c955ac49da2e2107b67a",
        ],
        "RFC 1321, A.5, in parts too"
    );
    assert_eq!(
        hex(&hmac(md5, &[0x0b; 16], &[b"Hi There"])),
        "9294727a3638bb1c13f48ef8158bfc9d",
        "RFC 2202, 2, its first case"
    );
    let key: [u8; 32] = core::array::from_fn(|at| at as u8);
    let plain = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    assert_eq!(hex(&aes(&key, &plain)), "8ea2b7ca516745bfeafc49904b496089", "FIPS 197, C.3");
    assert_eq!(hex(&aes(&key[..16], &plain)), "69c4e0d86a7b0430d8cdb78070b4c55a", "FIPS 197, C.1");
    let unhex = |text: &str| crate::base::code::unhex(text).unwrap();
    let feffe = "feffe9928665731c6d6a8f9467308308";
    let long_key = [feffe, feffe].concat();
    let plain = "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255";
    let extra = "feedfacedeadbeeffeedfacedeadbeefabaddad2";
    let nonce = "cafebabefacedbaddecaf888";
    let cipher = "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091473f5985";
    let long_cipher = "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662898015ad";
    let zeros = "00000000000000000000000000000000";
    let long_zeros = [zeros, zeros].concat();
    let (short_plain, zero_nonce) = (&plain[..120], &zeros[..24]);
    for (case, key, plain, extra, nonce, cipher, tag) in [
        ("1", zeros, "", "", zero_nonce, "", "58e2fccefa7e3061367f1d57a4e7455a"),
        ("2", zeros, zeros, "", zero_nonce, "0388dace60b6a392f328c2b971b2fe78", "ab6e47d42cec13bdf53a67b21257bddf"),
        ("3", feffe, plain, "", nonce, cipher, "4d5c2af327cd64a62cf35abd2ba6fab4"),
        ("4", feffe, short_plain, extra, nonce, &cipher[..120], "5bc94fbc3221a5db94fae95ae7121a47"),
        ("13", long_zeros.as_str(), "", "", zero_nonce, "", "530f8afbc74536b9a963b4f1c4cb738b"),
        (
            "14",
            long_zeros.as_str(),
            zeros,
            "",
            zero_nonce,
            "cea7403d4d606b6e074ec5d3baf39d18",
            "d0d1c8a799996bf0265b98b5d48ab919",
        ),
        ("15", long_key.as_str(), plain, "", nonce, long_cipher, "b094dac5d93471bdec1a502270e3cc6c"),
        ("16", long_key.as_str(), short_plain, extra, nonce, &long_cipher[..120], "76fc6ece0f4e1768cddf8853bb2d551b"),
    ] {
        let (key, extra, nonce) = (unhex(key), unhex(extra), unhex(nonce).try_into().unwrap());
        let mut data = unhex(plain);
        let sealed_tag = gcm_seal(&key, &nonce, &extra, &mut data);
        assert_eq!(
            (hex(&data), hex(&sealed_tag)),
            (cipher.into(), tag.into()),
            "the GCM submission's test case {case}"
        );
        data.extend(sealed_tag);
        let mut forged = data.clone();
        assert!(gcm_open(&key, &nonce, &extra, &mut data), "test case {case} opens");
        assert_eq!(hex(&data[..data.len() - 16]), plain, "test case {case} back to its plaintext");
        *forged.last_mut().unwrap() ^= 1;
        let kept = forged.clone();
        assert!(!gcm_open(&key, &nonce, &extra, &mut forged) && forged == kept, "test case {case}, its tag changed");
    }
}

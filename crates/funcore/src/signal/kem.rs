//! ML-KEM-768 in two halves for the post-quantum ratchet, and round 3 Kyber1024 over the library's ML-KEM-1024.

use super::{Crypto, Lattice};
use crate::crypto::keccak;
use crate::crypto::keccak::Sponge;
use alloc::vec::Vec;

const Q: u16 = 3329;
const RANK: usize = 3;
const U_BITS: u32 = 10;
const V_BITS: u32 = 4;
/// The bytes of an encapsulation key's `t̂`.
pub const T_BYTES: usize = 384 * RANK;
/// The bytes of an encapsulation key's `ρ`.
pub const SEED_BYTES: usize = 32;
// The bytes of an encapsulation key's header: `ρ` and the key's hash.
pub(super) const HEADER_BYTES: usize = 64;
pub(super) const FIRST_BYTES: usize = 32 * U_BITS as usize * RANK;
pub(super) const SECOND_BYTES: usize = 32 * V_BITS as usize;
/// ML-KEM-768's decapsulation key, as FIPS 203 lays it out: the private vector, `t̂`, `ρ`, the key's hash, `z`.
pub const PRIVATE_BYTES: usize = 2400;

type Polynomial = [u16; 256];

const fn reverse_bits(index: usize) -> usize {
    let mut out = 0;
    let mut bit = 0;
    while bit < 7 {
        out |= ((index >> bit) & 1) << (6 - bit);
        bit += 1;
    }
    out
}

const fn powers_of_zeta(times: usize, plus: usize) -> [u16; 128] {
    let mut powers = [0u16; 128];
    let mut index = 0;
    while index < 128 {
        let mut power = 1u32;
        let mut at = 0;
        while at < times * reverse_bits(index) + plus {
            power = power * 17 % Q as u32;
            at += 1;
        }
        powers[index] = power as u16;
        index += 1;
    }
    powers
}

const ZETAS: [u16; 128] = powers_of_zeta(1, 0);
const GAMMAS: [u16; 128] = powers_of_zeta(2, 1);

// Every `%` below is by the constant Q, which the compiler turns into a multiplication: no division instruction, whose time could tell a secret, is used.
fn product(a: u16, b: u16) -> u16 {
    (u32::from(a) * u32::from(b) % u32::from(Q)) as u16
}

fn sum(a: u16, b: u16) -> u16 {
    (a + b) % Q
}

fn difference(a: u16, b: u16) -> u16 {
    (a + Q - b) % Q
}

fn ntt(mut f: Polynomial) -> Polynomial {
    let mut index = 1;
    let mut length = 128;
    while length >= 2 {
        for start in (0..256).step_by(2 * length) {
            let zeta = ZETAS[index];
            index += 1;
            for at in start..start + length {
                let t = product(zeta, f[at + length]);
                f[at + length] = difference(f[at], t);
                f[at] = sum(f[at], t);
            }
        }
        length /= 2;
    }
    f
}

fn inverse_ntt(mut f: Polynomial) -> Polynomial {
    let mut index = 127;
    let mut length = 2;
    while length <= 128 {
        for start in (0..256).step_by(2 * length) {
            let zeta = ZETAS[index];
            index -= 1;
            for at in start..start + length {
                let t = f[at];
                f[at] = sum(t, f[at + length]);
                f[at + length] = product(zeta, difference(f[at + length], t));
            }
        }
        length *= 2;
    }
    f.map(|coefficient| product(coefficient, 3303))
}

fn multiply(f: &Polynomial, g: &Polynomial) -> Polynomial {
    let mut h = [0u16; 256];
    for pair in 0..128 {
        let (a0, a1, b0, b1) = (f[2 * pair], f[2 * pair + 1], g[2 * pair], g[2 * pair + 1]);
        h[2 * pair] = sum(product(a0, b0), product(product(a1, b1), GAMMAS[pair]));
        h[2 * pair + 1] = sum(product(a0, b1), product(a1, b0));
    }
    h
}

fn add(f: &Polynomial, g: &Polynomial) -> Polynomial {
    core::array::from_fn(|at| sum(f[at], g[at]))
}

fn sample(seed: &[u8; SEED_BYTES], first: u8, second: u8) -> Polynomial {
    let mut sponge = Sponge::shake128();
    sponge.absorb(seed);
    sponge.absorb(&[first, second]);
    let mut f = [0u16; 256];
    let mut count = 0;
    while count < 256 {
        let mut bytes = [0u8; 3];
        sponge.squeeze(&mut bytes);
        let d1 = u16::from(bytes[0]) | (u16::from(bytes[1] & 0x0F) << 8);
        let d2 = u16::from(bytes[1] >> 4) | (u16::from(bytes[2]) << 4);
        for candidate in [d1, d2] {
            if candidate < Q && count < 256 {
                f[count] = candidate;
                count += 1;
            }
        }
    }
    f
}

fn binomial(seed: &[u8; 32], counter: u8) -> Polynomial {
    let mut bytes = [0u8; 128];
    keccak::shake256(&[seed, &[counter]], &mut bytes);
    core::array::from_fn(|at| {
        let nibble = (bytes[at / 2] >> (4 * (at % 2))) & 0x0F;
        let (plus, minus) = ((nibble & 1) + ((nibble >> 1) & 1), ((nibble >> 2) & 1) + ((nibble >> 3) & 1));
        difference(u16::from(plus), u16::from(minus))
    })
}

fn compress(x: u16, bits: u32) -> u16 {
    const RECIPROCAL: u64 = (1u64 << 48).div_ceil(Q as u64);
    let scaled = (u64::from(x) << bits) + u64::from(Q / 2);
    (((scaled * RECIPROCAL) >> 48) & ((1 << bits) - 1)) as u16
}

fn decompress(y: u16, bits: u32) -> u16 {
    ((u32::from(y) * u32::from(Q) + (1 << (bits - 1))) >> bits) as u16
}

fn encode(f: &Polynomial, bits: u32, out: &mut Vec<u8>) {
    let (mut buffer, mut count) = (0u32, 0u32);
    for &coefficient in f {
        buffer |= u32::from(coefficient) << count;
        count += bits;
        while count >= 8 {
            out.push(buffer as u8);
            buffer >>= 8;
            count -= 8;
        }
    }
}

fn decode_twelve_bits(bytes: &[u8]) -> Option<Polynomial> {
    let mut f = [0u16; 256];
    for (pair, group) in bytes.as_chunks::<3>().0.iter().enumerate() {
        f[2 * pair] = u16::from(group[0]) | (u16::from(group[1] & 0x0F) << 8);
        f[2 * pair + 1] = u16::from(group[1] >> 4) | (u16::from(group[2]) << 4);
    }
    f.iter().all(|&coefficient| coefficient < Q).then_some(f)
}

/// The first half's input to the second: the message `m` and the header, from which every secret is derived again.
#[derive(Clone, PartialEq, Eq)]
pub struct FirstHalf {
    pub(super) message: [u8; 32],
    pub(super) header: [u8; HEADER_BYTES],
}

fn hash_g(message: &[u8; 32], key_hash: &[u8]) -> ([u8; 32], [u8; 32]) {
    let both = keccak::sha3_512(&[message, key_hash]);
    (both[..32].try_into().expect("half of 64"), both[32..].try_into().expect("half of 64"))
}

fn noise(r: &[u8; 32]) -> ([Polynomial; RANK], [Polynomial; RANK], Polynomial) {
    let y: [Polynomial; RANK] = core::array::from_fn(|at| ntt(binomial(r, at as u8)));
    let e1: [Polynomial; RANK] = core::array::from_fn(|at| binomial(r, (RANK + at) as u8));
    (y, e1, binomial(r, 2 * RANK as u8))
}

/// The first half of an ML-KEM-768 ciphertext, from the key's header and the random message, and the secret the whole ciphertext carries (FIPS 203 Algorithms 14 and 17, without `t̂`).
pub fn encapsulate_first(header: &[u8; HEADER_BYTES], message: &[u8; 32]) -> (Vec<u8>, [u8; 32], FirstHalf) {
    let seed: &[u8; SEED_BYTES] = header[..SEED_BYTES].try_into().expect("ρ first");
    let (secret, r) = hash_g(message, &header[SEED_BYTES..]);
    let (y, e1, _) = noise(&r);
    let mut first = Vec::with_capacity(FIRST_BYTES);
    for (row, e1) in e1.iter().enumerate() {
        let mut column = [0u16; 256];
        for (at, y) in y.iter().enumerate() {
            column = add(&column, &multiply(&sample(seed, row as u8, at as u8), y));
        }
        let u = add(&inverse_ntt(column), e1);
        encode(&u.map(|coefficient| compress(coefficient, U_BITS)), U_BITS, &mut first);
    }
    (first, secret, FirstHalf { message: *message, header: *header })
}

/// The second half, once `t̂` has come; none if it does not hash with `ρ` to the header's hash or a coefficient is not below q.
pub fn encapsulate_second(first: &FirstHalf, t: &[u8]) -> Option<Vec<u8>> {
    if !t_matches(&first.header, t) {
        return None;
    }
    let (_, r) = hash_g(&first.message, &first.header[SEED_BYTES..]);
    let (y, _, e2) = noise(&r);
    let mut v = [0u16; 256];
    for (at, y) in y.iter().enumerate() {
        v = add(&v, &multiply(&decode_twelve_bits(&t[384 * at..384 * (at + 1)])?, y));
    }
    let mu: Polynomial = core::array::from_fn(|at| decompress(u16::from((first.message[at / 8] >> (at % 8)) & 1), 1));
    let v = add(&add(&inverse_ntt(v), &e2), &mu);
    let mut second = Vec::with_capacity(SECOND_BYTES);
    encode(&v.map(|coefficient| compress(coefficient, V_BITS)), V_BITS, &mut second);
    Some(second)
}

/// An ML-KEM-768 key pair from its two seeds, `d` and `z` (FIPS 203 Algorithms 13 and 16): the decapsulation key and the encapsulation key, as FIPS 203 lays them out.
pub fn generate(d: &[u8; 32], z: &[u8; 32]) -> (Vec<u8>, Vec<u8>) {
    let both = keccak::sha3_512(&[d, &[RANK as u8]]);
    let (seed, noise_seed): (&[u8; 32], &[u8; 32]) =
        (both[..32].try_into().expect("half of 64"), both[32..].try_into().expect("half of 64"));
    let s: [Polynomial; RANK] = core::array::from_fn(|at| ntt(binomial(noise_seed, at as u8)));
    let mut public = Vec::with_capacity(T_BYTES + SEED_BYTES);
    for row in 0..RANK {
        let mut t = ntt(binomial(noise_seed, (RANK + row) as u8));
        for (at, s) in s.iter().enumerate() {
            t = add(&t, &multiply(&sample(seed, at as u8, row as u8), s));
        }
        encode(&t, 12, &mut public);
    }
    public.extend_from_slice(seed);
    let mut private = Vec::with_capacity(PRIVATE_BYTES);
    s.iter().for_each(|s| encode(s, 12, &mut private));
    private.extend_from_slice(&public);
    private.extend_from_slice(&keccak::sha3_256(&[&public]));
    private.extend_from_slice(z);
    (private, public)
}

pub(super) fn t_matches(header: &[u8; HEADER_BYTES], t: &[u8]) -> bool {
    t.len() == T_BYTES
        && keccak::sha3_256(&[t, &header[..SEED_BYTES]])[..] == header[SEED_BYTES..]
        && t.as_chunks::<384>().0.iter().all(|part| decode_twelve_bits(part).is_some())
}

/// The header of an ML-KEM-768 encapsulation key: `ρ` and the hash of the key.
pub fn header_of(public: &[u8]) -> [u8; HEADER_BYTES] {
    let mut header = [0u8; HEADER_BYTES];
    header[..SEED_BYTES].copy_from_slice(&public[T_BYTES..]);
    header[SEED_BYTES..].copy_from_slice(&keccak::sha3_256(&[public]));
    header
}

/// `t̂` out of a decapsulation key, where FIPS 203 keeps the encapsulation key.
pub fn t_of(private: &[u8]) -> &[u8] {
    &private[T_BYTES..2 * T_BYTES]
}

fn kyber_secret(mlkem: &[u8; 32], ciphertext: &[u8]) -> [u8; 32] {
    let mut secret = [0u8; 32];
    keccak::shake256(&[mlkem, &keccak::sha3_256(&[ciphertext])], &mut secret);
    secret
}

/// A ciphertext to a Kyber1024 public key of round 3 and the secret it carries.
pub fn kyber_encapsulate(crypto: &impl Crypto, public: &[u8]) -> Option<(Vec<u8>, [u8; 32])> {
    let (ciphertext, secret) = crypto.encapsulate(public)?;
    let secret = kyber_secret(&secret, &ciphertext);
    Some((ciphertext, secret))
}

/// The secret a Kyber1024 ciphertext of round 3 carries to its private key.
pub fn kyber_decapsulate(crypto: &impl Crypto, private: &[u8], ciphertext: &[u8]) -> Option<[u8; 32]> {
    Some(kyber_secret(&crypto.decapsulate(Lattice::Mlkem1024, private, ciphertext)?, ciphertext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::code::hex;
    use crate::base::code::unhex;

    const D: &str = "e582b7d75e6c80b05ae392a1fc9f7153b12390fd99930368cc67a768baebc8a0";
    const Z: &str = "1cdacb8740c0b87c4a379575f187b367cbfa3b300bf591b109f79816e9cbe8f0";
    const EK_HASH: &str = "81e66ef5a7a221619f6a64039cc369843e10df5c859f6959cc3fd8e5272330fd";
    const DK_HASH: &str = "be81068c104cd6cf8efd800b294f4a15bb8a8050993fd54a2cc428841ef6ca44";
    const EK: &str = "9c6839354086919bc7b8a547ff18b523075fd1706856400952489e4cd13ed8b17a817a3076e0947b860ec9558bd9d3c94df5418a7a7301763999c09996715ce32aacfef767a44969925949cc9b8120cb78953bcf6fb8282427cb3f4cbbc50ab23274493a8ba7e5c09088160fda731b55980867912bac53125b27073bb502afac5e74973e0f3601dc50ce08cc7f3fa44ce331a43f9673be745295e693077264a8e238f0a912c0a59c44d561e2a8bd831541fe869b3110b051100db3517f87510ecad8651ce4998bb648002ba41a5351fa2a4ed94a58cb707214a59f8876176cf82fceb99364971c7c0896e8b62c0544463ff101983c2045981ccd9173eb33ba18483ed915bd11ea950db3a8787c105263242e3a64c748b8b1a5947880c5bf3896e08b4b9916a39fd70c2ff8bd041a2b18e251d16a276e38100a613ea96a86cb3652dcc83f2d31cd9f8b79e42541a66752d191436cf5b5d8350ee7835cdb122855392c1da5c33fda12ad1028f2806861a2ab9c63785fd11e220346322c250936ab819254734a8dc9241971ac90cda6696adb90d1e327dc977841aba38324c8fb2092784942003a0808e5546d034592f03bacc19e3138c9070ccea80a933d536c49e4771091af8a70207d474574c64b9f1c8e47224174061117e98c18897a7b1b3c4478c6558c321a89a84b6c508947cb61c89204fbca43bcaa1d688f768b02e50babebf0c5fb8962b2355973c4193fa92e0ce4b9f8473e154828c986bf63e1cd985c6f0462c9b4374594d226c5e99b8a844978db8dd2839e27138aacf75e917635cec4b407a54f184c22f62981b9f4552fb334ff2b7ff68b470fd61b993c32581cc0f39881a5a6a769a60297f4b0579acbf52a8abb143885886245ca2636e20498959e4f2857ab96706e6409c4164ba88c7193e568e4446120215a6d8499c6b4bc559628a9f481c7db51e2e10df1f4339a647b4ccc0405b79f70a90ca9a8ad07789222ba6e66c3aff42b61baa637af15126c88b2e210a7cad9cb08cbb5123a7f9bb4ad63daaea6b43f9aaba561212823cb898a676c967b2d64f4b6333439f8993bd7b19687248926e077a9700431a59157b9668ee022b6d31343f15c08f14488877dfb30a552c06f96a16f354b9a3f19090b32a043b43256ba5b1088b56a4125588c8746d4099ea21c4f33ad8c5c7c32286dee2ca76fc2651d57aee9d847824ac609b5bfd5c87316c2afe0e153d6326af89987ecb70cf0a0a77fa000e0e0acc2685e2317429c3a7513f42873cb98f669703ae9146919226db58d6c2b93b6f50c7cdab677b505945a0f2523886c1b82a6a7cc7eba9251096d88360d7ef6473acbcd359a0d20414b95c209b2413b1483a3a061386377b28e3896bcd8c5e2692681cc803e1694efb6ce353082ffca4079ea9bb7b2025bcb68990466ed2803ab11516d80325dc4cde3da9e020653e084c45f837c88711df3aa6075741c1283121c8b1c691013969a62b2563e556206cf0810848691c7c408d018617a7300ad943e189a9b44512ef13b669831392f1329e2ac7c623aa044223079139b7f1c213e93682ed86165468377d40884310163a5539f27745e667adf506896ba933b002e4d50b2497ecff09d0bbca4f7e6f9db9e10c643d23701bd6385e163cf71c1e919a6e20a";
    const M: &str = "4e77596168711e913965d8175ac3bd76aab08b7f9385a02ae883cf6c6e17dd81";
    const C_HASH: &str = "684da17d93e27520cb78ae6169c020d73ef770e6fa89589cc7d653a76a8b1751";
    const K: &str = "79d74f6c6c2d916bec47bd828fd9b67295a37f54927fab1263c0d122f1c6f1ed";

    fn bytes32(text: &str) -> [u8; 32] {
        unhex(text).unwrap().try_into().unwrap()
    }

    #[test]
    fn acvp_vectors() {
        let (private, public) = generate(&bytes32(D), &bytes32(Z));
        assert_eq!(
            (hex(&keccak::sha3_256(&[&public])), hex(&keccak::sha3_256(&[&private]))),
            (EK_HASH.into(), DK_HASH.into()),
            "ML-KEM.KeyGen_internal(d, z): the encapsulation key and the decapsulation key"
        );
        let public = unhex(EK).unwrap();
        let (first, secret, half) = encapsulate_first(&header_of(&public), &bytes32(M));
        let second = encapsulate_second(&half, &public[..T_BYTES]).unwrap();
        assert_eq!(
            (hex(&keccak::sha3_256(&[&first, &second])), hex(&secret)),
            (C_HASH.into(), K.into()),
            "ML-KEM.Encaps_internal(ek, m), made in two halves: the ciphertext and the secret"
        );
    }

    #[test]
    fn ntt_round_trip() {
        let f: Polynomial = core::array::from_fn(|at| (at as u16 * 13 + 7) % Q);
        assert_eq!(inverse_ntt(ntt(f)), f);
    }

    #[test]
    fn ntt_multiplication() {
        let f: Polynomial = core::array::from_fn(|at| (at as u16 * 31 + 1) % Q);
        let g: Polynomial = core::array::from_fn(|at| (at as u16 * at as u16 + 5) % Q);
        let mut expected = [0i64; 256];
        for i in 0..256 {
            for j in 0..256 {
                let term = i64::from(f[i]) * i64::from(g[j]);
                if i + j < 256 { expected[i + j] += term } else { expected[i + j - 256] -= term }
            }
        }
        let expected: Polynomial = expected.map(|coefficient| coefficient.rem_euclid(i64::from(Q)) as u16);
        assert_eq!(inverse_ntt(multiply(&ntt(f), &ntt(g))), expected);
    }

    #[test]
    fn compression_rounding() {
        for bits in [1, 4, 10, 11] {
            for x in 0..Q {
                let expected = ((u32::from(x) << (bits + 1)) + u32::from(Q)) / (2 * u32::from(Q)) % (1 << bits);
                assert_eq!(u32::from(compress(x, bits)), expected, "{x} to {bits} bits");
            }
        }
        assert_eq!(decompress(1, 1), 1665);
        assert_eq!(decompress(0, 4), 0);
    }

    #[test]
    fn twelve_bit_coefficients() {
        let f: Polynomial = core::array::from_fn(|at| (at as u16 * 97) % Q);
        let mut bytes = Vec::new();
        encode(&f, 12, &mut bytes);
        assert_eq!(bytes.len(), 384);
        assert_eq!(decode_twelve_bits(&bytes), Some(f));
        let mut invalid = f;
        invalid[17] = Q;
        let mut bytes = Vec::new();
        encode(&invalid, 12, &mut bytes);
        assert_eq!(decode_twelve_bits(&bytes), None);
    }

    #[test]
    fn second_half_key_check() {
        let t = [0u8; T_BYTES];
        let mut public = t.to_vec();
        public.extend_from_slice(&[9u8; SEED_BYTES]);
        let header = header_of(&public);
        let (first, _, half) = encapsulate_first(&header, &[3; 32]);
        assert_eq!(first.len(), FIRST_BYTES);
        assert_eq!(encapsulate_second(&half, &t).map(|second| second.len()), Some(SECOND_BYTES));
        let mut other = t;
        other[0] = 1;
        assert_eq!(encapsulate_second(&half, &other), None);
    }
}

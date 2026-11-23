//! WireGuard (Donenfeld, "WireGuard: Next Generation Kernel Network Tunnel") as the side that begins: a tunnel's config, the handshake, the data messages and one peer's sessions and timers.

pub mod config;
mod noise;
mod peer;

pub(crate) use peer::REKEY_TIMEOUT;
pub use peer::{Peer, Step};

use crate::crypto::{KEY_BYTES, Primitives, X25519};

/// The bytes ChaCha20-Poly1305 signs what it seals with.
pub const TAG_BYTES: usize = 16;
/// The bytes of XChaCha20-Poly1305's nonce.
pub const WIDE_NONCE_BYTES: usize = 24;
// What a tunnel's packet costs on the outer network at the most: IPv6's head, UDP's, a data message's head and its tag.
const OVERHEAD_BYTES: usize = 40 + 8 + noise::DATA_HEAD + TAG_BYTES;

/// The ciphers of the tunnel beside what every protocol takes, done by a library.
pub trait Crypto: Primitives + X25519 {
    // ChaCha20-Poly1305 (RFC 8439) in place, the nonce four zero bytes and the counter, lowest byte first: the tag, or none where the library fails.
    fn seal(&self, key: &[u8; KEY_BYTES], counter: u64, extra: &[u8], data: &mut [u8]) -> Option<[u8; TAG_BYTES]>;
    // In place over the data and the tag behind it: false for what was not sealed so, and the data is then no plaintext.
    fn open(&self, key: &[u8; KEY_BYTES], counter: u64, extra: &[u8], sealed: &mut [u8]) -> bool;
    // XChaCha20-Poly1305, as `open`.
    fn open_wide(&self, key: &[u8; KEY_BYTES], nonce: &[u8; WIDE_NONCE_BYTES], extra: &[u8], sealed: &mut [u8])
    -> bool;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::crypto::blake2s;
    use core::cell::Cell;

    // A group of numbers under a prime in place of the curve, and a stream of hashes in place of the cipher.
    #[derive(Default)]
    pub(crate) struct Toy(Cell<u64>);

    const PRIME: u64 = 0xFFFF_FFFF_FFFF_FFC5;
    const BASE: u64 = 5;

    fn power(base: u64, mut exponent: u64) -> u64 {
        let (mut result, mut square) = (1u128, u128::from(base));
        while exponent > 0 {
            if exponent & 1 == 1 {
                result = result * square % u128::from(PRIME);
            }
            square = square * square % u128::from(PRIME);
            exponent >>= 1;
        }
        result as u64
    }

    fn number(key: &[u8; KEY_BYTES]) -> u64 {
        u64::from_le_bytes(*key.first_chunk().unwrap()) % (PRIME - 2) + 2
    }

    fn to_key(number: u64) -> [u8; KEY_BYTES] {
        let mut key = [0u8; KEY_BYTES];
        key[..8].copy_from_slice(&number.to_le_bytes());
        key
    }

    pub(crate) fn mask(key: &[u8; KEY_BYTES], nonce: &[u8], data: &mut [u8]) {
        for (block, piece) in data.chunks_mut(blake2s::HASH_BYTES).enumerate() {
            let stream = blake2s::hash(&[key, nonce, &block.to_le_bytes()]);
            piece.iter_mut().zip(stream).for_each(|(byte, mask)| *byte ^= mask);
        }
    }

    pub(crate) fn tag(key: &[u8; KEY_BYTES], nonce: &[u8], extra: &[u8], plain: &[u8]) -> [u8; TAG_BYTES] {
        blake2s::keyed(key, &[nonce, &extra.len().to_le_bytes(), extra, plain])
    }

    pub(crate) fn open_with(key: &[u8; KEY_BYTES], nonce: &[u8], extra: &[u8], sealed: &mut [u8]) -> bool {
        let Some(length) = sealed.len().checked_sub(TAG_BYTES) else {
            return false;
        };
        let (data, signed) = sealed.split_at_mut(length);
        mask(key, nonce, data);
        tag(key, nonce, extra, data) == *signed
    }

    impl Toy {
        pub(super) fn seal_wide(
            key: &[u8; KEY_BYTES],
            nonce: &[u8; WIDE_NONCE_BYTES],
            extra: &[u8],
            data: &mut [u8],
        ) -> [u8; TAG_BYTES] {
            let signed = tag(key, nonce, extra, data);
            mask(key, nonce, data);
            signed
        }
    }

    impl Primitives for Toy {
        fn hmac_sha256(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
            blake2s::hmac(key, parts)
        }

        fn hkdf_sha256(&self, salt: &[u8], secret: &[u8], _: &[&[u8]], out: &mut [u8]) -> bool {
            mask(&blake2s::hmac(salt, &[secret]), &[], out);
            true
        }

        fn random(&self, out: &mut [u8]) -> bool {
            self.0.set(self.0.get() + 1);
            out.fill(0);
            mask(&to_key(self.0.get()), &[], out);
            true
        }
    }

    impl X25519 for Toy {
        fn agree(&self, private: &[u8; KEY_BYTES], public: &[u8; KEY_BYTES]) -> Option<[u8; KEY_BYTES]> {
            Some(to_key(power(u64::from_le_bytes(*public.first_chunk()?), number(private))))
        }

        fn public_of(&self, private: &[u8; KEY_BYTES]) -> Option<[u8; KEY_BYTES]> {
            Some(to_key(power(BASE, number(private))))
        }
    }

    impl Crypto for Toy {
        fn seal(&self, key: &[u8; KEY_BYTES], counter: u64, extra: &[u8], data: &mut [u8]) -> Option<[u8; TAG_BYTES]> {
            let signed = tag(key, &counter.to_le_bytes(), extra, data);
            mask(key, &counter.to_le_bytes(), data);
            Some(signed)
        }

        fn open(&self, key: &[u8; KEY_BYTES], counter: u64, extra: &[u8], sealed: &mut [u8]) -> bool {
            open_with(key, &counter.to_le_bytes(), extra, sealed)
        }

        fn open_wide(
            &self,
            key: &[u8; KEY_BYTES],
            nonce: &[u8; WIDE_NONCE_BYTES],
            extra: &[u8],
            sealed: &mut [u8],
        ) -> bool {
            open_with(key, nonce, extra, sealed)
        }
    }

    #[test]
    fn toy_agrees_and_opens() {
        let toy = Toy::default();
        let (a, b) = ([7u8; KEY_BYTES], [9u8; KEY_BYTES]);
        assert_eq!(
            toy.agree(&a, &toy.public_of(&b).unwrap()),
            toy.agree(&b, &toy.public_of(&a).unwrap()),
            "both sides come to one secret"
        );
        let mut sealed = b"a packet of the tunnel".to_vec();
        let signed = toy.seal(&a, 3, b"head", &mut sealed).unwrap();
        assert_ne!(sealed, b"a packet of the tunnel");
        sealed.extend(signed);
        let mut other = sealed.clone();
        assert!(toy.open(&a, 3, b"head", &mut sealed) && sealed.starts_with(b"a packet of the tunnel"));
        assert!(!toy.open(&a, 4, b"head", &mut other.clone()) && !toy.open(&b, 3, b"head", &mut other));
    }
}

//! Keyed primitives: HMAC-SHA256, HKDF-SHA256, random bytes, X25519, Keccak and BLAKE2s.

pub(crate) mod blake2s;
#[cfg(test)]
pub(crate) mod exact;
pub(crate) mod keccak;

pub(crate) const SHA256_BYTES: usize = 32;
/// The bytes of an X25519 key, private, public or agreed.
pub const KEY_BYTES: usize = 32;

/// X25519 (RFC 7748) a library provides.
pub trait X25519 {
    // None if the result is all nought, as RFC 7748 lets a party refuse.
    fn agree(&self, private: &[u8; KEY_BYTES], public: &[u8; KEY_BYTES]) -> Option<[u8; KEY_BYTES]>;
    fn public_of(&self, private: &[u8; KEY_BYTES]) -> Option<[u8; KEY_BYTES]>;
}

/// Key arithmetic a library provides.
pub trait Primitives {
    fn hmac_sha256(&self, key: &[u8], parts: &[&[u8]]) -> [u8; SHA256_BYTES];
    // False where the library fails.
    fn hkdf_sha256(&self, salt: &[u8], secret: &[u8], info: &[&[u8]], out: &mut [u8]) -> bool;
    // False where no random bytes can be had.
    fn random(&self, out: &mut [u8]) -> bool;
}

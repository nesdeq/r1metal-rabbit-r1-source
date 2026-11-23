use crate::base::bytes::same;
use crate::signal::Crypto;
use alloc::vec::Vec;

// Signal Desktop's KEY_SET_LENGTH: the AES key, then the HMAC key.
pub(in crate::signal) const KEYS: usize = 64;
const AES_KEY: usize = 32;
pub(super) const IV: usize = 16;
const MAC: usize = 32;
pub(super) const DIGEST: usize = 32;
const BLOCK: usize = 16;
// Signal Desktop's logPadSize: the least a plaintext is padded to, and each size's growth over the one before.
const LEAST_PADDED: u64 = 541;
const PADDED_STEP: f64 = 1.05;

// The first power of PADDED_STEP no smaller than the size, whole; multiplied up where Desktop takes a logarithm, the two differing only within rounding of a whole number.
fn padded(size: u64) -> u64 {
    let mut bucket = 1.0f64;
    while bucket < size as f64 {
        bucket *= PADDED_STEP;
    }
    (bucket as u64).max(LEAST_PADDED)
}

// Signal Desktop's getAttachmentCiphertextSize.
pub(super) fn sealed_size(size: u64) -> u64 {
    IV as u64 + (padded(size) / BLOCK as u64 + 1) * BLOCK as u64 + MAC as u64
}

// An attachment as the CDN holds it: the IV, the ciphertext and the MAC one after the other, and the digest of all.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Sealed {
    pub(super) blob: Vec<u8>,
    pub(super) digest: [u8; DIGEST],
}

// Signal Desktop's encryptAttachmentV2: padded with noughts, AES-256-CBC under PKCS #7, the IV before and the HMAC-SHA256 of both behind.
pub(super) fn encrypt(crypto: &impl Crypto, plain: &[u8], keys: &[u8; KEYS], iv: &[u8; IV]) -> Option<Sealed> {
    let (aes, mac) = keys.split_at(AES_KEY);
    let length = usize::try_from(padded(plain.len() as u64)).ok()?;
    let mut padding = Vec::with_capacity(length);
    padding.extend_from_slice(plain);
    padding.resize(length, 0);
    let ciphertext = crypto.cbc_encrypt(aes.try_into().ok()?, iv, &padding)?;
    let mut blob = Vec::with_capacity(IV + ciphertext.len() + MAC);
    blob.extend_from_slice(iv);
    blob.extend_from_slice(&ciphertext);
    let tag = crypto.hmac_sha256(mac, &[&blob]);
    blob.extend_from_slice(&tag);
    let digest = crypto.sha256(&[&blob]);
    Some(Sealed { blob, digest })
}

// Signal Desktop's decryptAttachmentV2ToSink: whole blocks, then the MAC and the digest, both compared without stopping at the first difference, then the plaintext cut to its size.
pub(super) fn decrypt(
    crypto: &impl Crypto,
    blob: &[u8],
    keys: &[u8; KEYS],
    (digest, size): (&[u8; DIGEST], u32),
) -> Option<Vec<u8>> {
    let (aes, mac) = keys.split_at(AES_KEY);
    let ciphertext_bytes = blob.len().checked_sub(IV + MAC).filter(|bytes| *bytes > 0 && bytes % BLOCK == 0)?;
    let (signed, tag) = blob.split_at(IV + ciphertext_bytes);
    let (is_digest_right, is_mac_right) =
        (same(&crypto.sha256(&[blob]), digest), same(&crypto.hmac_sha256(mac, &[signed]), tag));
    if !(is_mac_right && is_digest_right) {
        return None;
    }
    let (iv, ciphertext) = signed.split_at(IV);
    let mut plain = crypto.cbc_decrypt(aes.try_into().ok()?, iv.try_into().ok()?, ciphertext)?;
    let size = usize::try_from(size).ok().filter(|size| *size <= plain.len())?;
    plain.truncate(size);
    Some(plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Primitives;
    use crate::signal::tests::{Exact, Standin};

    #[test]
    fn padding_values() {
        // Desktop's logPadSize of each, worked out with its own expression in double precision.
        assert_eq!(
            [0, 1, 541, 542, 1_000, 100_000, 1_048_576, 2_000_000, 100_000_000].map(padded),
            [541, 541, 541, 568, 1_020, 100_155, 1_093_831, 2_062_581, 102_224_512]
        );
        assert_eq!(sealed_size(1), 16 + 544 + 32, "541 padded bytes take 34 blocks, the IV before and the MAC behind");
        assert_eq!(sealed_size(1_000), 16 + 1_024 + 32, "1020 bytes take 63 whole blocks and PKCS #7's sixty-fourth");
    }

    #[test]
    fn round_trip_and_checks() {
        let crypto = Standin::default();
        let keys: [u8; KEYS] = core::array::from_fn(|at| at as u8);
        let plain = b"a photo's bytes";
        let sealed = encrypt(&crypto, plain, &keys, &[7; IV]).unwrap();
        assert_eq!(sealed.blob.len() as u64, sealed_size(plain.len() as u64), "padded, the IV and the MAC about it");
        assert_eq!(&sealed.blob[..IV], &[7; IV], "the IV first");
        assert_eq!(sealed.digest, crypto.sha256(&[&sealed.blob]), "the digest of all of it");
        let size = plain.len() as u32;
        let open = |blob: &[u8], digest, size| decrypt(&crypto, blob, &keys, (digest, size));
        assert_eq!(open(&sealed.blob, &sealed.digest, size).as_deref(), Some(&plain[..]));
        let mut other = sealed.blob.clone();
        *other.last_mut().unwrap() ^= 1;
        assert_eq!(open(&other, &sealed.digest, size), None, "a MAC that is not its own");
        assert_eq!(open(&sealed.blob, &[0; DIGEST], size), None, "a digest that is not its own");
        assert_eq!(open(&sealed.blob, &sealed.digest, 600), None, "a size past what it holds");
        assert_eq!(open(&sealed.blob[..40], &sealed.digest, size), None, "no whole block");
        let exact = Exact::default();
        let sealed = encrypt(&exact, plain, &keys, &[7; IV]).unwrap();
        let (signed, tag) = sealed.blob.split_at(sealed.blob.len() - MAC);
        assert_eq!(
            tag,
            exact.hmac_sha256(&keys[AES_KEY..], &[signed]),
            "HMAC-SHA256 under the second half of the keys"
        );
    }
}

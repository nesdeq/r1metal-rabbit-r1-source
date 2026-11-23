//! STUN (RFC 8489): messages with the attributes ICE and TURN use, the checks of a received one, the framing of messages on a stream, and the URIs of servers (RFC 7064, 7065).

mod attribute;
mod frame;
mod message;
mod uri;

pub(crate) use attribute::{
    ALLOCATION_MISMATCH, Attribute, BAD_REQUEST, DONT_FRAGMENT, FAMILY_UNSUPPORTED, INSUFFICIENT_CAPACITY, IPV4, IPV6,
    ROLE_CONFLICT, SERVER_ERROR, STALE_NONCE, TRY_ALTERNATE, UDP, UNAUTHENTICATED, UNKNOWN_ATTRIBUTE, WORD, family_of,
};
#[cfg(test)]
pub(crate) use attribute::{
    ALTERNATE_DOMAIN, FINGERPRINT, FORBIDDEN, MESSAGE_INTEGRITY, PASSWORD_ALGORITHM, PEER_FAMILY_MISMATCH,
    QUOTA_REACHED, REQUESTED_ADDRESS_FAMILY, REQUESTED_TRANSPORT, TRANSPORT_UNSUPPORTED, USERHASH, WRONG_CREDENTIALS,
};
pub(crate) use frame::{CHANNEL_HEAD, Frames, Framing, prefixed};
pub(crate) use message::{
    ALLOCATE, BINDING, CHANNEL_BIND, CREATE_PERMISSION, Class, DATA_METHOD, Integrity, Message, REFRESH, RTO, SEND,
    STREAM_WAIT, TRANSMISSIONS, Transaction, Writer, is_message, new_transaction, resend_at,
};
pub(crate) use uri::Uri;

use crate::crypto::{Primitives, SHA256_BYTES};

/// The bytes of an HMAC-SHA1, MESSAGE-INTEGRITY's value.
pub const SHA1_BYTES: usize = 20;
/// The bytes of MD5, the long-term credential's key.
pub const MD5_BYTES: usize = 16;

/// The hashes of STUN and TURN beside what every protocol takes, done by a library.
pub trait Crypto: Primitives {
    // RFC 2104 over SHA-1, MESSAGE-INTEGRITY's (RFC 8489, 14.5).
    fn hmac_sha1(&self, key: &[u8], parts: &[&[u8]]) -> [u8; SHA1_BYTES];
    // RFC 1321, the long-term key of the password algorithm MD5 (RFC 8489, 9.2.2).
    fn md5(&self, parts: &[&[u8]]) -> [u8; MD5_BYTES];
    // FIPS 180-4, the long-term key of the password algorithm SHA-256 and USERHASH (RFC 8489, 18.5.1.2, 14.4).
    fn sha256(&self, parts: &[&[u8]]) -> [u8; SHA256_BYTES];
}

// What carries messages between two transport addresses.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Transport {
    Udp,
    Tcp,
    // TCP under TLS.
    Tls,
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::base::math::xorshift64;
    use crate::crypto::exact::{hkdf, hmac, md5, sha1, sha256};
    use core::cell::Cell;

    // The hashes as the standards define them, and random bytes of a seed that is not nought.
    pub(crate) struct Exact(pub(crate) Cell<u64>);

    impl Primitives for Exact {
        fn hmac_sha256(&self, key: &[u8], parts: &[&[u8]]) -> [u8; SHA256_BYTES] {
            hmac(sha256, key, parts)
        }

        fn hkdf_sha256(&self, salt: &[u8], secret: &[u8], info: &[&[u8]], out: &mut [u8]) -> bool {
            hkdf(salt, secret, info, out)
        }

        fn random(&self, out: &mut [u8]) -> bool {
            for chunk in out.chunks_mut(8) {
                self.0.set(xorshift64(self.0.get()));
                chunk.copy_from_slice(&self.0.get().to_le_bytes()[..chunk.len()]);
            }
            true
        }
    }

    impl Crypto for Exact {
        fn hmac_sha1(&self, key: &[u8], parts: &[&[u8]]) -> [u8; SHA1_BYTES] {
            hmac(sha1, key, parts)
        }

        fn md5(&self, parts: &[&[u8]]) -> [u8; MD5_BYTES] {
            md5(parts)
        }

        fn sha256(&self, parts: &[&[u8]]) -> [u8; SHA256_BYTES] {
            sha256(parts)
        }
    }

    #[test]
    fn exact_draws() {
        let (one, other) = (Exact(Cell::new(1)), Exact(Cell::new(2)));
        let (mut first, mut second, mut third) = ([0u8; 12], [0u8; 12], [0u8; 12]);
        assert!(one.random(&mut first) && one.random(&mut second) && other.random(&mut third));
        assert!(first != second && first != third, "each draw and each seed its own bytes");
        assert_eq!(Crypto::md5(&one, &[b"abc"]), md5(&[b"abc"]), "the standard's MD5");
    }
}

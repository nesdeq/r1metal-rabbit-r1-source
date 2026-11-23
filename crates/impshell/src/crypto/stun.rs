use super::{Library, hash_of, hmac_of};
use crate::log;
use aws_lc_rs::{digest, hmac};
use aws_lc_sys as lc;
use r1_funcore::net::stun::{Crypto, MD5_BYTES, SHA1_BYTES};

impl Crypto for Library {
    fn hmac_sha1(&self, key: &[u8], parts: &[&[u8]]) -> [u8; SHA1_BYTES] {
        hmac_of(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key, parts).as_ref().try_into().expect("SHA-1 gives twenty bytes")
    }

    fn md5(&self, parts: &[&[u8]]) -> [u8; MD5_BYTES] {
        let (message, mut hash, mut size) = (parts.concat(), [0u8; MD5_BYTES], 0);
        let made = unsafe {
            let md5 = lc::EVP_md5();
            lc::EVP_Digest(
                message.as_ptr().cast(),
                message.len(),
                hash.as_mut_ptr(),
                &mut size,
                md5,
                std::ptr::null_mut(),
            )
        };
        if made != 1 {
            log!("crypto: the library made no MD5");
        }
        hash
    }

    fn sha256(&self, parts: &[&[u8]]) -> [u8; 32] {
        hash_of(&digest::SHA256, parts).as_ref().try_into().expect("SHA-256 gives thirty-two bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::base::code::hex;

    #[test]
    fn standard_vectors() {
        assert_eq!(
            [hex(&Library.md5(&[])), hex(&Library.md5(&[b"message ", b"digest"]))],
            ["d41d8cd98f00b204e9800998ecf8427e", "f96b697d7cb7938d525a2f31aaf161d0"],
            "RFC 1321, A.5, in parts too"
        );
        assert_eq!(
            hex(&Library.md5(&[b"user:realm:pass"])),
            "8493fbc53ba582fb4c044c456bdc40eb",
            "RFC 8489, 9.2.2: the key of a long-term credential"
        );
        assert_eq!(
            hex(&Crypto::hmac_sha1(&Library, &[0x0B; 20], &[b"Hi ", b"There"])),
            "b617318655057264e28bc0b6fb378c8ef146be00",
            "RFC 2202"
        );
        assert_eq!(
            hex(&Crypto::sha256(&Library, &[b"a", b"bc"])),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "FIPS 180"
        );
    }
}

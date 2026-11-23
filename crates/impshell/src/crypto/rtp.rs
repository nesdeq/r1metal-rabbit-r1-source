use super::Library;
use aws_lc_rs::aead::{AES_128_GCM, AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::cipher::{AES_128, AES_256, EncryptingKey, EncryptionContext, UnboundCipherKey};
use r1_funcore::net::rtp::{BLOCK_BYTES, Crypto, Key, NONCE_BYTES, TAG_BYTES};

fn cipher(key: &Key) -> Option<LessSafeKey> {
    let unbound = match key {
        Key::Aes128(key) => UnboundKey::new(&AES_128_GCM, key),
        Key::Aes256(key) => UnboundKey::new(&AES_256_GCM, key),
    };
    unbound.ok().map(LessSafeKey::new)
}

impl Crypto for Library {
    fn seal(&self, key: &Key, nonce: &[u8; NONCE_BYTES], extra: &[u8], data: &mut [u8]) -> Option<[u8; TAG_BYTES]> {
        let nonce = Nonce::assume_unique_for_key(*nonce);
        let tag = cipher(key)?.seal_in_place_separate_tag(nonce, Aad::from(extra), data).ok()?;
        tag.as_ref().try_into().ok()
    }

    fn open(&self, key: &Key, nonce: &[u8; NONCE_BYTES], extra: &[u8], sealed: &mut [u8]) -> bool {
        let nonce = Nonce::assume_unique_for_key(*nonce);
        cipher(key).is_some_and(|cipher| cipher.open_in_place(nonce, Aad::from(extra), sealed).is_ok())
    }

    fn encrypt(&self, key: &Key, block: &[u8; BLOCK_BYTES]) -> Option<[u8; BLOCK_BYTES]> {
        let unbound = match key {
            Key::Aes128(key) => UnboundCipherKey::new(&AES_128, key),
            Key::Aes256(key) => UnboundCipherKey::new(&AES_256, key),
        };
        // Counter mode over noughts, the block its first counter, gives the block under the cipher.
        let mut out = [0u8; BLOCK_BYTES];
        let counter = EncryptionContext::Iv128((*block).into());
        EncryptingKey::ctr(unbound.ok()?).ok()?.less_safe_encrypt(&mut out, counter).ok()?;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::base::code::{hex, unhex};

    fn bytes(digits: &str) -> Vec<u8> {
        unhex(digits).expect("hex digits")
    }

    #[test]
    fn gcm_submission_cases() {
        let feffe = "feffe9928665731c6d6a8f9467308308";
        let plain = "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39";
        let extra = bytes("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let nonce: [u8; NONCE_BYTES] = bytes("cafebabefacedbaddecaf888").try_into().unwrap();
        let short = Key::Aes128(bytes(feffe).try_into().unwrap());
        let long = Key::Aes256(bytes(&[feffe, feffe].concat()).try_into().unwrap());
        for (key, cipher, tag, case) in [
            (
                short,
                "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091",
                "5bc94fbc3221a5db94fae95ae7121a47",
                4,
            ),
            (
                long,
                "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662",
                "76fc6ece0f4e1768cddf8853bb2d551b",
                16,
            ),
        ] {
            let mut data = bytes(plain);
            let sealed_tag = Library.seal(&key, &nonce, &extra, &mut data).expect("a tag");
            assert_eq!(
                (hex(&data), hex(&sealed_tag)),
                (cipher.into(), tag.into()),
                "the GCM submission's test case {case}"
            );
            data.extend(sealed_tag);
            let mut forged = data.clone();
            assert!(Library.open(&key, &nonce, &extra, &mut data), "test case {case} opens");
            assert_eq!(hex(&data[..data.len() - TAG_BYTES]), plain, "back to its plaintext");
            *forged.last_mut().unwrap() ^= 1;
            assert!(
                !Library.open(&key, &nonce, &extra, &mut forged)
                    && !Library.open(&key, &nonce, &extra, &mut [0; TAG_BYTES - 1]),
                "a tag changed, or less than a tag, opens nothing"
            );
        }
    }

    #[test]
    fn blocks_fips_197() {
        let key: Vec<u8> = (0..32).collect();
        let block: [u8; BLOCK_BYTES] = bytes("00112233445566778899aabbccddeeff").try_into().unwrap();
        let short = Key::Aes128(key[..16].try_into().unwrap());
        let long = Key::Aes256(key.try_into().unwrap());
        assert_eq!(
            (Library.encrypt(&short, &block).map(|out| hex(&out)), Library.encrypt(&long, &block).map(|out| hex(&out))),
            (Some("69c4e0d86a7b0430d8cdb78070b4c55a".into()), Some("8ea2b7ca516745bfeafc49904b496089".into())),
            "FIPS 197, C.1 and C.3"
        );
    }
}

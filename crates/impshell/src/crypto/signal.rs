use super::{Library, hash_of};
use aws_lc_rs::aead::{AES_256_GCM, AES_256_GCM_SIV, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::cipher::{
    AES_256, DecryptionContext, EncryptingKey, EncryptionContext, PaddedBlockDecryptingKey, PaddedBlockEncryptingKey,
    UnboundCipherKey,
};
use aws_lc_rs::digest;
use aws_lc_rs::kem::{Ciphertext, DecapsulationKey, EncapsulationKey, ML_KEM_768, ML_KEM_1024};
use r1_funcore::signal::{Crypto, Lattice};

fn kem_algorithm(lattice: Lattice) -> &'static aws_lc_rs::kem::Algorithm {
    match lattice {
        Lattice::Mlkem768 => &ML_KEM_768,
        Lattice::Mlkem1024 => &ML_KEM_1024,
    }
}

fn aead_key(algorithm: &'static aws_lc_rs::aead::Algorithm, key: &[u8; 32]) -> Option<LessSafeKey> {
    Some(LessSafeKey::new(UnboundKey::new(algorithm, key).ok()?))
}

fn aead_open(
    algorithm: &'static aws_lc_rs::aead::Algorithm,
    key: &[u8; 32],
    nonce: &[u8; 12],
    extra: &[u8],
    sealed: &[u8],
) -> Option<Vec<u8>> {
    let mut data = sealed.to_vec();
    let plain = aead_key(algorithm, key)?
        .open_in_place(Nonce::assume_unique_for_key(*nonce), Aad::from(extra), &mut data)
        .ok()?
        .len();
    data.truncate(plain);
    Some(data)
}

impl Crypto for Library {
    fn sha512(&self, parts: &[&[u8]]) -> [u8; 64] {
        hash_of(&digest::SHA512, parts).as_ref().try_into().expect("SHA-512 gives sixty-four bytes")
    }

    fn sha256(&self, parts: &[&[u8]]) -> [u8; 32] {
        hash_of(&digest::SHA256, parts).as_ref().try_into().expect("SHA-256 gives thirty-two bytes")
    }

    fn sha1(&self, parts: &[&[u8]]) -> [u8; 20] {
        hash_of(&digest::SHA1_FOR_LEGACY_USE_ONLY, parts).as_ref().try_into().expect("SHA-1 gives twenty bytes")
    }

    fn cbc_encrypt(&self, key: &[u8; 32], iv: &[u8; 16], plain: &[u8]) -> Option<Vec<u8>> {
        let key = PaddedBlockEncryptingKey::cbc_pkcs7(UnboundCipherKey::new(&AES_256, key).ok()?).ok()?;
        let mut data = plain.to_vec();
        key.less_safe_encrypt(&mut data, EncryptionContext::Iv128((*iv).into())).ok()?;
        Some(data)
    }

    fn cbc_decrypt(&self, key: &[u8; 32], iv: &[u8; 16], sealed: &[u8]) -> Option<Vec<u8>> {
        let key = PaddedBlockDecryptingKey::cbc_pkcs7(UnboundCipherKey::new(&AES_256, key).ok()?).ok()?;
        let mut data = sealed.to_vec();
        let plain = key.decrypt(&mut data, DecryptionContext::Iv128((*iv).into())).ok()?.len();
        data.truncate(plain);
        Some(data)
    }

    fn ctr(&self, key: &[u8; 32], counter: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
        let key = EncryptingKey::ctr(UnboundCipherKey::new(&AES_256, key).ok()?).ok()?;
        let mut data = data.to_vec();
        key.less_safe_encrypt(&mut data, EncryptionContext::Iv128((*counter).into())).ok()?;
        Some(data)
    }

    fn gcm_decrypt(&self, key: &[u8; 32], nonce: &[u8; 12], extra: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
        aead_open(&AES_256_GCM, key, nonce, extra, sealed)
    }

    fn siv_encrypt(&self, key: &[u8; 32], nonce: &[u8; 12], plain: &[u8]) -> Option<Vec<u8>> {
        let mut data = plain.to_vec();
        aead_key(&AES_256_GCM_SIV, key)?
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(*nonce), Aad::empty(), &mut data)
            .ok()?;
        Some(data)
    }

    fn siv_decrypt(&self, key: &[u8; 32], nonce: &[u8; 12], sealed: &[u8]) -> Option<Vec<u8>> {
        aead_open(&AES_256_GCM_SIV, key, nonce, &[], sealed)
    }

    fn lattice_pair(&self) -> Option<(Vec<u8>, Vec<u8>)> {
        let private = DecapsulationKey::generate(&ML_KEM_1024).ok()?;
        let public = private.encapsulation_key().ok()?.key_bytes().ok()?.as_ref().to_vec();
        Some((private.key_bytes().ok()?.as_ref().to_vec(), public))
    }

    fn encapsulate(&self, public: &[u8]) -> Option<(Vec<u8>, [u8; 32])> {
        let (ciphertext, secret) = EncapsulationKey::new(&ML_KEM_1024, public).ok()?.encapsulate().ok()?;
        Some((ciphertext.as_ref().to_vec(), secret.as_ref().try_into().ok()?))
    }

    fn decapsulate(&self, lattice: Lattice, private: &[u8], ciphertext: &[u8]) -> Option<[u8; 32]> {
        let secret = DecapsulationKey::new(kem_algorithm(lattice), private)
            .ok()?
            .decapsulate(Ciphertext::from(ciphertext))
            .ok()?;
        secret.as_ref().try_into().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};
    use r1_funcore::base::code::hex;
    use r1_funcore::crypto::{Primitives, X25519};
    use r1_funcore::signal::{curve, kem};

    #[test]
    fn incremental_kem_matches_library() {
        for seed in [0u8, 0x5A, 0xFF] {
            let library = DecapsulationKey::generate(&ML_KEM_768).unwrap();
            let library_keys = (
                library.key_bytes().unwrap().as_ref().to_vec(),
                library.encapsulation_key().unwrap().key_bytes().unwrap().as_ref().to_vec(),
            );
            for (private, public) in [library_keys, kem::generate(&[seed; 32], &[seed ^ 0xFF; 32])] {
                assert_eq!((private.len(), public.len()), (kem::PRIVATE_BYTES, kem::T_BYTES + kem::SEED_BYTES));
                assert_eq!(kem::t_of(&private), &public[..kem::T_BYTES], "t̂ where FIPS 203 keeps it");
                let (first, secret, state) = kem::encapsulate_first(&kem::header_of(&public), &[seed; 32]);
                let second = kem::encapsulate_second(&state, kem::t_of(&private)).unwrap();
                assert_eq!(Library.decapsulate(Lattice::Mlkem768, &private, &[first, second].concat()), Some(secret));
                let (ciphertext, library_secret) =
                    EncapsulationKey::new(&ML_KEM_768, &public).unwrap().encapsulate().unwrap();
                let library_secret = <[u8; 32]>::try_from(library_secret.as_ref()).unwrap();
                assert_eq!(Library.decapsulate(Lattice::Mlkem768, &private, ciphertext.as_ref()), Some(library_secret));
            }
        }
    }

    #[test]
    fn kyber_on_ml_kem_1024() {
        let (private, public) = Library.lattice_pair().unwrap();
        assert_eq!(public.len(), 1568, "Kyber1024's public key");
        let (ciphertext, secret) = kem::kyber_encapsulate(&Library, &public).unwrap();
        assert_eq!(ciphertext.len(), 1568, "and its ciphertext");
        assert_eq!(kem::kyber_decapsulate(&Library, &private, &ciphertext), Some(secret));
        assert_ne!(Library.decapsulate(Lattice::Mlkem1024, &private, &ciphertext), Some(secret), "the extra hash");
    }

    #[test]
    fn xeddsa_is_ed25519() {
        let mut private = [0u8; 32];
        assert!(Library.random(&mut private));
        private[0] &= 248;
        private[31] &= 127;
        private[31] |= 64;
        let public = Library.public_of(&private).unwrap();
        let random = [7u8; 64];
        let message: [&[u8]; 2] = [b"a signed ", b"pre-key"];
        let signature = curve::sign(&Library, &private, &message, &random);
        assert!(curve::verify(&Library, &public, &message, &signature));
        assert!(!curve::verify(&Library, &public, &[b"another pre-key"], &signature));
        let mut forged = signature;
        forged[5] ^= 1;
        assert!(!curve::verify(&Library, &public, &message, &forged));

        let edwards = curve::edwards_key(&public, signature[63] >> 7).unwrap();
        let mut plain = signature;
        plain[63] &= 0x7F;
        let joined = [message[0], message[1]].concat();
        assert!(UnparsedPublicKey::new(&ED25519, edwards).verify(&joined, &plain).is_ok(), "AWS-LC's Ed25519 agrees");
    }

    fn bytes32(digits: &str) -> [u8; 32] {
        r1_funcore::base::code::unhex(digits).unwrap().try_into().unwrap()
    }

    #[test]
    fn libsignal_vectors() {
        // libsignal v0.105.1, rust/core/src/curve/curve25519.rs, `test_agreement` and `test_signature`.
        let (alice, alice_public) = (
            bytes32("c806439dc9d2c476ffed8f2580c0888d58ab406bf7ae3698879021b96bb4bf59"),
            bytes32("1bb75966f2e93a3691dfff942bb2a466a1c08b8d78ca3f4d6df8b8bfa2e4ee28"),
        );
        let (bob, bob_public) = (
            bytes32("b03b34c33a1c44f225b662d2bf4859b8135411fa7b0386d45fb75dc5b91b4466"),
            bytes32("653614993d2b15ee9e5fd3d86ce719ef4ec1daae1886a87b3f5fa9565a27a22f"),
        );
        let shared = bytes32("325f239328941ced6e673b86ba41017448e99b649a9c3806c1dd7ca4c477e629");
        assert_eq!((Library.public_of(&alice), Library.public_of(&bob)), (Some(alice_public), Some(bob_public)));
        assert_eq!(
            (Library.agree(&alice, &bob_public), Library.agree(&bob, &alice_public)),
            (Some(shared), Some(shared))
        );
        let identity = bytes32("c097248412e58bf05df487968205132794178e367637f5818f81e0e6ce73e865");
        let identity_public = bytes32("ab7e717d4a163b7d9a1d8071dfe9dcf8cdcd1cea3339b6356be84d887e322c64");
        let ephemeral =
            r1_funcore::base::code::unhex("05edce9d9c415ca78cb7252e72c2c4a554d3eb29485a0e1d503118d1a82d99fb4a")
                .unwrap();
        let signature: [u8; 64] = r1_funcore::base::code::unhex(
            "5de88ca9a89b4a115da79109c67c9c7464a3e4180274f1cb8c63c2984e286dfbede82deb9dcd9fae0bfbb821569b3d9001bd8130cd11d486cef047bd60b86e88",
        )
        .unwrap()
        .try_into()
        .unwrap();
        assert_eq!(Library.public_of(&identity), Some(identity_public));
        assert!(curve::verify(&Library, &identity_public, &[&ephemeral], &signature), "libsignal's signature verifies");
        for at in 0..signature.len() * 8 {
            let mut bad = signature;
            bad[at / 8] ^= 1 << (at % 8);
            assert!(!curve::verify(&Library, &identity_public, &[&ephemeral], &bad), "bit {at} changed");
        }
    }

    #[test]
    fn aes_gcm_siv_rfc_8452() {
        let mut key = [0u8; 32];
        key[0] = 1;
        let mut nonce = [0u8; 12];
        nonce[0] = 3;
        let plain = [1, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(hex(&Library.siv_encrypt(&key, &nonce, &[]).unwrap()), "07f5f4169bbf55a8400cd47ea6fd400f");
        let sealed = Library.siv_encrypt(&key, &nonce, &plain).unwrap();
        assert_eq!(hex(&sealed), "c2ef328e5c71c83b843122130f7364b761e0b97427e3df28", "RFC 8452, C.2");
        assert_eq!(Library.siv_decrypt(&key, &nonce, &sealed).as_deref(), Some(&plain[..]));
    }

    #[test]
    fn hashes_fips_180() {
        assert_eq!(
            hex(&Library.sha256(&[b"a", b"", b"bc"])),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(hex(&Library.sha1(&[b"ab", b"c"])), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(&Library.sha512(&[b"abc"])),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }
}

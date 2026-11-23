use super::Library;
use aws_lc_rs::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use aws_lc_sys as lc;
use r1_funcore::crypto::KEY_BYTES;
use r1_funcore::net::wireguard::{Crypto, TAG_BYTES, WIDE_NONCE_BYTES};

// The nonce of a counter: four bytes of nought, then the counter, lowest byte first.
fn nonce(counter: u64) -> Nonce {
    let mut bytes = [0u8; NONCE_LEN];
    bytes[NONCE_LEN - size_of::<u64>()..].copy_from_slice(&counter.to_le_bytes());
    Nonce::assume_unique_for_key(bytes)
}

fn cipher(key: &[u8; KEY_BYTES]) -> Option<LessSafeKey> {
    UnboundKey::new(&CHACHA20_POLY1305, key).ok().map(LessSafeKey::new)
}

impl Crypto for Library {
    fn seal(&self, key: &[u8; KEY_BYTES], counter: u64, extra: &[u8], data: &mut [u8]) -> Option<[u8; TAG_BYTES]> {
        let tag = cipher(key)?.seal_in_place_separate_tag(nonce(counter), Aad::from(extra), data).ok()?;
        tag.as_ref().try_into().ok()
    }

    fn open(&self, key: &[u8; KEY_BYTES], counter: u64, extra: &[u8], sealed: &mut [u8]) -> bool {
        cipher(key).is_some_and(|cipher| cipher.open_in_place(nonce(counter), Aad::from(extra), sealed).is_ok())
    }

    fn open_wide(
        &self,
        key: &[u8; KEY_BYTES],
        nonce: &[u8; WIDE_NONCE_BYTES],
        extra: &[u8],
        sealed: &mut [u8],
    ) -> bool {
        let (mut plain, mut count) = (vec![0u8; sealed.len()], 0usize);
        let is_open = unsafe {
            let context = lc::EVP_AEAD_CTX_new(lc::EVP_aead_xchacha20_poly1305(), key.as_ptr(), key.len(), TAG_BYTES);
            let opened = !context.is_null()
                && lc::EVP_AEAD_CTX_open(
                    context,
                    plain.as_mut_ptr(),
                    &mut count,
                    plain.len(),
                    nonce.as_ptr(),
                    nonce.len(),
                    sealed.as_ptr(),
                    sealed.len(),
                    extra.as_ptr(),
                    extra.len(),
                ) == 1;
            lc::EVP_AEAD_CTX_free(context);
            opened
        };
        sealed[..count].copy_from_slice(&plain[..count]);
        is_open
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::Fixed;
    use super::*;
    use r1_funcore::base::code::{hex, unhex};
    use r1_funcore::crypto::X25519;
    use r1_funcore::net::wireguard::config::Config;
    use r1_funcore::net::wireguard::{Peer, Step};
    use std::time::Duration;

    impl X25519 for Fixed {
        fn agree(&self, private: &[u8; KEY_BYTES], public: &[u8; KEY_BYTES]) -> Option<[u8; KEY_BYTES]> {
            Library.agree(private, public)
        }

        fn public_of(&self, private: &[u8; KEY_BYTES]) -> Option<[u8; KEY_BYTES]> {
            Library.public_of(private)
        }
    }

    impl Crypto for Fixed {
        fn seal(&self, key: &[u8; KEY_BYTES], counter: u64, extra: &[u8], data: &mut [u8]) -> Option<[u8; TAG_BYTES]> {
            Library.seal(key, counter, extra, data)
        }

        fn open(&self, key: &[u8; KEY_BYTES], counter: u64, extra: &[u8], sealed: &mut [u8]) -> bool {
            Library.open(key, counter, extra, sealed)
        }

        fn open_wide(
            &self,
            key: &[u8; KEY_BYTES],
            nonce: &[u8; WIDE_NONCE_BYTES],
            extra: &[u8],
            sealed: &mut [u8],
        ) -> bool {
            Library.open_wide(key, nonce, extra, sealed)
        }
    }

    fn bytes(digits: &str) -> Vec<u8> {
        unhex(digits).expect("hex digits")
    }

    #[test]
    fn sealed_and_opened() {
        let key: [u8; KEY_BYTES] = std::array::from_fn(|at| 0x80 + at as u8);
        let mut data = b"a packet of the tunnel".to_vec();
        let tag = Library.seal(&key, 0x0102_0304_0506_0708, b"head", &mut data).expect("a tag");
        data.extend(tag);
        assert_eq!(
            hex(&data),
            "95919eda33b13ea903de17ab7a1f00ff87063a8557460776da72fba0ec9d1bb18738eaf45773",
            "ChaCha20-Poly1305 of RFC 8439 under a nonce of four noughts and the counter, as Python's cryptography seals it"
        );
        let mut changed = data.clone();
        assert!(Library.open(&key, 0x0102_0304_0506_0708, b"head", &mut data));
        assert_eq!(&data[..22], b"a packet of the tunnel");
        assert!(
            !Library.open(&key, 0x0102_0304_0506_0709, b"head", &mut changed.clone())
                && !Library.open(&key, 0x0102_0304_0506_0708, b"tail", &mut changed.clone())
                && !Library.open(&key, 0, b"head", &mut changed[..15]),
            "another counter, other bytes signed with it, or less than a tag opens nothing"
        );
        let nonce: [u8; WIDE_NONCE_BYTES] = std::array::from_fn(|at| 0x40 + at as u8);
        let extra = bytes("50515253c0c1c2c3c4c5c6c7");
        let mut sealed = bytes(
            "bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb731c7f1b0b4aa6440bf3a82f4eda7e39ae64c6708c54c216cb96b72e1213b4522f8c9ba40db5d945b11b69b982c1bb9e3f3fac2bc369488f76b2383565d3fff921f9664c97637da9768812f615c68b13b52ec0875924c1c7987947deafd8780acf49",
        );
        let mut tampered = sealed.clone();
        tampered[3] ^= 1;
        assert!(
            Library.open_wide(&key, &nonce, &extra, &mut sealed),
            "XChaCha20-Poly1305, draft-irtf-cfrg-xchacha-03, A.3.1"
        );
        assert!(sealed.starts_with(b"Ladies and Gentlemen of the class of '99"));
        assert!(
            !Library.open_wide(&key, &nonce, &extra, &mut tampered)
                && !Library.open_wide(&key, &nonce, &extra, &mut [])
        );
    }

    // The peer's messages as a second implementation made them, in Python over cryptography and hashlib, from this end's key of 0x11s, the peer's of 0x22s, a preshared key of 0x33s, the peer's handshake key of 0x44s and the random bytes `Fixed` gives.
    const CONFIG: &str = "[Interface]\nPrivateKey = ERERERERERERERERERERERERERERERERERERERERERE=\nAddress = 10.2.0.2/32\nDNS = 10.2.0.1\n[Peer]\nPublicKey = D6poTtKIZ7l/Smot7l34zpdOdrcBjj8iocTPJnhXDyA=\nPresharedKey = MzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM=\nAllowedIPs = 0.0.0.0/0\nEndpoint = 203.0.113.5:51820\n";
    const INITIATION: &str = "01000000a0a1a2a3605a725d2a4adfeeb1a29e17edd621c1b7593ee8cdbc44ac6c4ab6e2f805d23cf3152d8364295db2d19a9284d0ae604e25ab682a556a4cf3f9c3e627d6e23a8b299db7eb1261ea9ad544a7ceb53daee03c6939eeffa3e9443f13d61ea262c086161f38a96c51460a5a02adfbdc709ecb20f1bbee0bd8187303ef3af900000000000000000000000000000000";
    const RESPONSE: &str = "020000000c0b0b0ba0a1a2a3ff2ee45601ec1b67310c7790404585ae697331eee1c1f8cf2419731c1fff3e6b3fc37204558d651c1225d0559414e80093b0962fa2cedf0c348844f9bd231c3400000000000000000000000000000000";
    const KEEPALIVE: &str = "040000000c0b0b0b0000000000000000e15127452c9cbaf59b5559e7d198d93c";
    const DATA_IN: &str = "04000000a0a1a2a30000000000000000f9476c85c93f56515dd5c9822d84793e5ce67561c49f9f349dd175b9cad049450b61e884634dc838508941d2eacb8cb1";
    const DATA_OUT: &str = "040000000c0b0b0b01000000000000008d68b64c5d1e082e6895b58c8d349e06d7ac0739c0524335487b52b693fd173f0de9e178a11daeb2182294fb16dcb63e";
    const COOKIE_REPLY: &str = "03000000a0a1a2a3505152535455565758595a5b5c5d5e5f606162636465666743b9d3b615933d82ae255ae69d8e369ac7383c73dd0fa9a683953a6a276ec92f";
    const INITIATION_WITH_COOKIE: &str = "01000000a0a1a2a3605a725d2a4adfeeb1a29e17edd621c1b7593ee8cdbc44ac6c4ab6e2f805d23cf3152d8364295db2d19a9284d0ae604e25ab682a556a4cf3f9c3e627d6e23a8b299db7eb1261ea9ad544a7ceb53daee03c6939eeffa3e9443f13d61d493d6a5f0fdfe99dd4669e8cf1fee6602cafe39ea3231b03bc0cec31f646561ee34da14499e9e08d9eba70667bfa65b4";
    const UTC: Duration = Duration::new(1_800_000_000, 5);

    const PEER_PUBLIC: &str = "0faa684ed28867b97f4a6a2dee5df8ce974e76b7018e3f22a1c4cf2678570f20";

    fn peer() -> Peer {
        assert_eq!(
            Library.public_of(&[0x22; KEY_BYTES]).map(|public| hex(&public)).as_deref(),
            Some(PEER_PUBLIC),
            "the peer the config names is the one whose key the second implementation answered under"
        );
        let config = Config::parse(CONFIG).expect("a config");
        Peer::new(config.to_keys(&Library).expect("this end's public key"), config.keepalive(), 1384)
    }

    fn sent(steps: &[Step]) -> String {
        match steps {
            [Step::Send(datagram)] => hex(datagram),
            other => panic!("not one datagram: {other:?}"),
        }
    }

    #[test]
    fn handshake_with_a_second_implementation() {
        let (mut peer, second) = (peer(), Duration::from_secs(1));
        assert_eq!(sent(&peer.begin(&Fixed, second, UTC)), INITIATION, "the message that begins the handshake");
        assert_eq!(
            sent(&peer.take(&Fixed, &bytes(RESPONSE), second, UTC)),
            KEEPALIVE,
            "the peer's answer is taken, and the session's first word goes under its keys"
        );
        assert!(peer.is_up(second));
        let packet = [&[0x45, 0, 0, 28][..], &[0; 24]].concat();
        assert_eq!(
            peer.take(&Fixed, &bytes(DATA_IN), second, UTC),
            [Step::Deliver(packet.clone())],
            "a packet the peer sealed comes out whole"
        );
        assert_eq!(
            sent(&peer.send(&Fixed, &packet, second, UTC)),
            DATA_OUT,
            "and one sealed here is as the peer's own"
        );
    }

    #[test]
    fn cookie_of_a_second_implementation() {
        let mut peer = peer();
        assert_eq!(sent(&peer.begin(&Fixed, Duration::ZERO, UTC)), INITIATION);
        assert_eq!(peer.take(&Fixed, &bytes(COOKIE_REPLY), Duration::from_secs(1), UTC), []);
        assert_eq!(
            sent(&peer.tick(&Fixed, Duration::from_secs(5), UTC)),
            INITIATION_WITH_COOKIE,
            "the next try, a nanosecond on by its stamp, is signed with the cookie the peer sealed"
        );
    }
}

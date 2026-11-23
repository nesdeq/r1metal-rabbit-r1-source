pub mod curve;
mod rtp;
mod signal;
mod stun;
mod wireguard;

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::key_wrap::{AES_128, AesKek, KeyWrap};
use aws_lc_rs::{agreement, cmac, digest, hkdf, hmac, pbkdf2, rand};
use aws_lc_sys as lc;
use r1_funcore::crypto::{KEY_BYTES, Primitives, X25519};
use r1_funcore::fs::fscrypt::{KEY_SIZE, Key};
use r1_funcore::storage::safe::{SEAL_KEY_SIZE, SEALED_SIZE, Slot, WORK, Work};
use r1_funcore::wifi::wpa::{self, Crypto, PMK};
use std::num::NonZeroU32;

const ROUNDS: NonZeroU32 = NonZeroU32::new(wpa::ROUNDS).expect("not zero");

/// AWS-LC as the protocols' primitives.
pub struct Library;

fn hmac_of(algorithm: hmac::Algorithm, key: &[u8], parts: &[&[u8]]) -> hmac::Tag {
    let mut context = hmac::Context::with_key(&hmac::Key::new(algorithm, key));
    parts.iter().for_each(|part| context.update(part));
    context.sign()
}

fn hash_of(algorithm: &'static digest::Algorithm, parts: &[&[u8]]) -> digest::Digest {
    let mut context = digest::Context::new(algorithm);
    parts.iter().for_each(|part| context.update(part));
    context.finish()
}

struct KeyLength(usize);

impl hkdf::KeyType for KeyLength {
    fn len(&self) -> usize {
        self.0
    }
}

impl Primitives for Library {
    fn hmac_sha256(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
        hmac_of(hmac::HMAC_SHA256, key, parts).as_ref().try_into().expect("SHA-256 gives thirty-two bytes")
    }

    fn hkdf_sha256(&self, salt: &[u8], secret: &[u8], info: &[&[u8]], out: &mut [u8]) -> bool {
        let seed = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(secret);
        seed.expand(info, KeyLength(out.len())).and_then(|expanded| expanded.fill(out)).is_ok()
    }

    fn random(&self, out: &mut [u8]) -> bool {
        rand::fill(out).is_ok()
    }
}

impl X25519 for Library {
    fn agree(&self, private: &[u8; KEY_BYTES], public: &[u8; KEY_BYTES]) -> Option<[u8; KEY_BYTES]> {
        let private = agreement::PrivateKey::from_private_key(&agreement::X25519, private).ok()?;
        let public = agreement::UnparsedPublicKey::new(&agreement::X25519, public);
        agreement::agree(&private, public, (), |secret| secret.try_into().map_err(|_| ())).ok()
    }

    fn public_of(&self, private: &[u8; KEY_BYTES]) -> Option<[u8; KEY_BYTES]> {
        let private = agreement::PrivateKey::from_private_key(&agreement::X25519, private).ok()?;
        private.compute_public_key().ok()?.as_ref().try_into().ok()
    }
}

impl Crypto for Library {
    fn hmac_sha1(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
        hmac_of(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key, parts).as_ref().try_into().expect("SHA-1 gives twenty bytes")
    }

    fn cmac(&self, key: &[u8; 16], parts: &[&[u8]]) -> Option<[u8; 16]> {
        let mut context = cmac::Context::with_key(&cmac::Key::new(cmac::AES_128, key).ok()?);
        parts.iter().try_for_each(|part| context.update(part)).ok()?;
        context.sign().ok()?.as_ref().try_into().ok()
    }

    fn unwrap_key(&self, key: &[u8; 16], wrapped: &[u8]) -> Option<Vec<u8>> {
        let mut out = vec![0u8; wrapped.len()];
        let plain = AesKek::new(&AES_128, key).ok()?.unwrap(wrapped, &mut out).ok()?.len();
        out.truncate(plain);
        Some(out)
    }

    fn derive_pmk(&self, password: &[u8], name: &[u8]) -> [u8; PMK] {
        let mut key = [0u8; PMK];
        pbkdf2::derive(pbkdf2::PBKDF2_HMAC_SHA1, ROUNDS, name, password, &mut key);
        key
    }
}

/// Seeds the library's random generator on its own thread, about a third of a second once per process.
pub fn warm_up() {
    let thread = std::thread::Builder::new().name("entropy".into()).spawn(|| {
        let _ = rand::fill(&mut [0u8; 1]);
    });
    if let Err(e) = thread {
        crate::log!("crypto: no thread for the random generator's start: {e}");
    }
}

/// The key an earlier firmware's safe is under, by scrypt straight from its passphrase and salt; none if the library fails.
pub fn safe_key(passphrase: &str, salt: &[u8]) -> Option<Key> {
    scrypt(passphrase.as_bytes(), salt, WORK)
}

fn scrypt(password: &[u8], salt: &[u8], work: Work) -> Option<Key> {
    let mut key = Key::zeroed();
    scrypt_into(password, salt, work, key.bytes_mut()).then_some(key)
}

fn scrypt_into(password: &[u8], salt: &[u8], work: Work, output: &mut [u8]) -> bool {
    let result = unsafe {
        lc::EVP_PBE_scrypt(
            password.as_ptr().cast(),
            password.len(),
            salt.as_ptr(),
            salt.len(),
            work.blocks(),
            work.block,
            work.lanes,
            work.limit_bytes(),
            output.as_mut_ptr(),
            output.len(),
        )
    };
    result == 1
}

// The key that seals the safe's key in a key file: what scrypt stretches a passphrase to with the file's salt, which is zeroed once the library has it.
fn seal_key(passphrase: &str, salt: &[u8]) -> Option<LessSafeKey> {
    let mut stretched = [0u8; SEAL_KEY_SIZE];
    let is_drawn = scrypt_into(passphrase.as_bytes(), salt, WORK, &mut stretched);
    let key = UnboundKey::new(&AES_256_GCM, &stretched).ok().filter(|_| is_drawn).map(LessSafeKey::new);
    stretched.fill(0);
    std::hint::black_box(&stretched);
    key
}

/// A new key for the safe, of random bytes; none if the library has none to give.
pub fn new_safe_key() -> Option<Key> {
    let mut key = Key::zeroed();
    Library.random(key.bytes_mut()).then_some(key)
}

/// The safe's key sealed under a passphrase for a key file, with a salt and a nonce of its own; none if the library fails.
pub fn seal_safe_key(passphrase: &str, key: &Key) -> Option<Slot> {
    let mut slot = Slot { salt: random()?, nonce: random()?, sealed: [0; SEALED_SIZE] };
    let (sealer, bound) = (seal_key(passphrase, &slot.salt)?, slot.to_bound());
    let (body, tag) = slot.sealed.split_at_mut(KEY_SIZE);
    body.copy_from_slice(key.bytes());
    let sealed = sealer.seal_in_place_separate_tag(Nonce::assume_unique_for_key(slot.nonce), Aad::from(bound), body);
    match sealed {
        Ok(own_tag) => {
            tag.copy_from_slice(own_tag.as_ref());
            Some(slot)
        }
        Err(_) => {
            slot.sealed.fill(0);
            std::hint::black_box(&slot.sealed);
            None
        }
    }
}

/// The safe's key a key file holds, opened with a passphrase; none for another passphrase than it was sealed under, for a file changed since, and if the library fails.
pub fn open_safe_key(passphrase: &str, slot: &Slot) -> Option<Key> {
    let (opener, mut sealed, mut key) = (seal_key(passphrase, &slot.salt)?, slot.sealed, Key::zeroed());
    let nonce = Nonce::assume_unique_for_key(slot.nonce);
    let opened = opener.open_in_place(nonce, Aad::from(slot.to_bound()), &mut sealed).ok().map(|plain| {
        key.bytes_mut().copy_from_slice(plain);
    });
    sealed.fill(0);
    std::hint::black_box(&sealed);
    opened.map(|()| key)
}

/// SHA-1, for the WebSocket accept value of RFC 6455; it keeps nothing secret.
pub fn sha1(bytes: &[u8]) -> [u8; 20] {
    digest::digest(&digest::SHA1_FOR_LEGACY_USE_ONLY, bytes).as_ref().try_into().expect("SHA-1 gives twenty bytes")
}

/// An incremental SHA-256 hash, to compare two copies.
pub struct Sum(digest::Context);

impl Sum {
    pub fn new() -> Sum {
        Sum(digest::Context::new(&digest::SHA256))
    }

    pub fn add(&mut self, piece: &[u8]) {
        self.0.update(piece);
    }

    /// The hash in hexadecimal, as `sha256sum` prints it.
    pub fn hex(self) -> String {
        r1_funcore::base::code::hex(self.0.finish().as_ref())
    }
}

/// Random bytes; none if the library has none to give.
pub fn random<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0u8; N];
    Library.random(&mut bytes).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::base::code::{hex, unhex};
    use r1_funcore::wifi::rsn;
    use r1_funcore::wifi::wpa::{Error, GroupKey, Step, Supplicant};

    #[test]
    fn scrypt_rfc_7914() {
        let work = Work { log_blocks: 10, block: 8, lanes: 16 };
        assert_eq!(
            scrypt(b"password", b"NaCl", work).map(|key| hex(key.bytes())).as_deref(),
            Some(
                "fdbabe1c9d3472007856e7190d01e9fe7c6ad7cbc8237830e77376634b3731622eaf30d92e22a3886ff109279d9830dac727afb94a83ee6d8360cbdfa2cc0640"
            )
        );
    }

    #[test]
    fn safe_key_sealed_and_opened() {
        let key = new_safe_key().expect("random bytes");
        assert_ne!(key.bytes(), new_safe_key().unwrap().bytes(), "each key of its own random bytes");
        let slot = seal_safe_key("opening", &key).expect("a sealed key");
        assert_ne!(&slot.sealed[..KEY_SIZE], key.bytes(), "what the file holds is not the key");
        assert_eq!(open_safe_key("opening", &slot).map(|opened| *opened.bytes()), Some(*key.bytes()));
        assert!(open_safe_key("Opening", &slot).is_none(), "another passphrase opens nothing");
        let again = seal_safe_key("opening", &key).unwrap();
        assert!(again.salt != slot.salt && again.nonce != slot.nonce, "each seal under a salt and a nonce of its own");
        let reread = Slot::parse(&slot.to_text()).expect("the file's text reads back");
        assert_eq!(open_safe_key("opening", &reread).map(|opened| *opened.bytes()), Some(*key.bytes()));
        for changed in [
            Slot { salt: again.salt, ..slot.clone() },
            Slot { nonce: again.nonce, ..slot.clone() },
            Slot { sealed: again.sealed, ..slot.clone() },
        ] {
            assert!(open_safe_key("opening", &changed).is_none(), "a file changed since it was sealed opens nothing");
        }
    }

    pub(super) struct Fixed;

    impl Primitives for Fixed {
        fn hmac_sha256(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
            Library.hmac_sha256(key, parts)
        }

        fn hkdf_sha256(&self, salt: &[u8], secret: &[u8], info: &[&[u8]], out: &mut [u8]) -> bool {
            Library.hkdf_sha256(salt, secret, info, out)
        }

        fn random(&self, out: &mut [u8]) -> bool {
            out.iter_mut().enumerate().for_each(|(at, byte)| *byte = 0xA0 ^ at as u8);
            true
        }
    }

    impl Crypto for Fixed {
        fn hmac_sha1(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
            Library.hmac_sha1(key, parts)
        }

        fn cmac(&self, key: &[u8; 16], parts: &[&[u8]]) -> Option<[u8; 16]> {
            Library.cmac(key, parts)
        }

        fn unwrap_key(&self, key: &[u8; 16], wrapped: &[u8]) -> Option<Vec<u8>> {
            Library.unwrap_key(key, wrapped)
        }

        fn derive_pmk(&self, password: &[u8], name: &[u8]) -> [u8; PMK] {
            Library.derive_pmk(password, name)
        }
    }

    fn bytes(digits: &str) -> Vec<u8> {
        unhex(digits).expect("hex digits")
    }

    #[test]
    fn standard_vectors() {
        assert_eq!(
            hex(&Library.derive_pmk(b"password", b"IEEE")),
            "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e",
            "IEEE 802.11, the key of a password"
        );
        assert_eq!(
            hex(&Library.hmac_sha1(&[0x0B; 20], &[b"Hi ", b"There"])),
            "b617318655057264e28bc0b6fb378c8ef146be00",
            "RFC 2202, given in two parts"
        );
        assert_eq!(
            hex(&Library.hmac_sha256(&[0x0B; 20], &[b"Hi There"])),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            "RFC 4231"
        );
        let key: [u8; 16] = bytes("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        assert_eq!(
            Library.cmac(&key, &[]).map(|tag| hex(&tag)).as_deref(),
            Some("bb1d6929e95937287fa37d129b756746"),
            "RFC 4493"
        );
        let message = bytes("6bc1bee22e409f96e93d7e117393172a");
        assert_eq!(
            Library.cmac(&key, &[&message[..5], &message[5..]]).map(|tag| hex(&tag)).as_deref(),
            Some("070a16b46b4d4144f79bdd9dd04a287c"),
            "RFC 4493, given in two parts"
        );
        let key: [u8; 16] = bytes("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let wrapped = bytes("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
        assert_eq!(
            Library.unwrap_key(&key, &wrapped).map(|plain| hex(&plain)).as_deref(),
            Some("00112233445566778899aabbccddeeff"),
            "RFC 3394"
        );
        assert_eq!(
            (Library.unwrap_key(&[0; 16], &wrapped), Library.unwrap_key(&key, &wrapped[..23])),
            (None, None),
            "what was wrapped under another key, or is cut short, does not unwrap"
        );
        let (mut first, mut second) = ([0u8; 32], [0u8; 32]);
        assert!(Library.random(&mut first) && Library.random(&mut second) && first != second);
        assert!(random::<16>().is_some_and(|first| random::<16>() != Some(first)));
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d", "FIPS 180");
        let mut sum = Sum::new();
        [&b"a"[..], b"", b"bc"].iter().for_each(|piece| sum.add(piece));
        assert_eq!(
            sum.hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "FIPS 180, in pieces"
        );
        assert_eq!(
            r1_funcore::net::websocket::accept_value("dGhlIHNhbXBsZSBub25jZQ==", sha1),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=",
            "RFC 6455, 1.3"
        );
    }

    const PLAIN_MASTER: &str = "bc52264ae127fbc6189a2100040ae48ca87a011f0074108685371cf4b63ca48a";
    const PLAIN_TRAFFIC: &str = "6abbc0aa9bc9373c751efaa4e1d256ff";
    const PLAIN_FRAMES: [&str; 6] = [
        "0203005f02008a00100000000000000001404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0103007502010a00000000000000000001a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf000000000000000000000000000000000000000000000000000000000000000087a40437f7d6859d4454fbc2b578188b001630140100000fac040100000fac040100000fac020000",
        "020300970213ca00100000000000000002404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0000000000000000000000000000000021222324252600000000000000000000dacb6c4dce516c0e27861082df77cd9e0038ca08ef4ca701478d85276605e2796fcf8eeb5cae1689bec0437abbf1593a013b98560a1ebaddcab8e3d195941fa692344941b6a57c38d174",
        "0103005f02030a0000000000000000000200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000b1750b2d39fc20135d17cc1e295e45870000",
        "0203007f0213820010000000000000000300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000031323334353600000000000000000000d1d8737865e1cc62ed37ac57650f80310020f1fbe6149603fbe7f7fd59a1561772f400a2d26e23e4aa5d076f4c6f744726d5",
        "0103005f0203020000000000000000000300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000a9d35534d0b20ca6cc0aa835a5a840df0000",
    ];
    const PROTECTED_TRAFFIC: &str = "f975d65dad2c8db08992fcc9bdccfe2d";
    const PROTECTED_FRAMES: [&str; 6] = [
        "0203005f02008b00100000000000000001404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0103007b02010b00000000000000000001a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf000000000000000000000000000000000000000000000000000000000000000024e3cb54b2a248ad8343508e79a00b36001c301a0100000fac040100000fac040100000fac06c0000000000fac06",
        "020300b70213cb00100000000000000002404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0000000000000000000000000000000021222324252600000000000000000000c56d95b782f1b5c9d013d94d3071d9f40058b705d89d0d06c799c30576c5ff3b81f04912d160be3525e7c39139e7f84818708c03373c27f55642f2e61ee2e8e818044cab2cd29dd475820c936f3feda34c4b72c7c539fd27cdd2d27b3b5606d7fb465bc10bb5699d990d",
        "0103005f02030b0000000000000000000200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000ad50efd086224ec67e85d5805c4a10120000",
        "0203007f02138300100000000000000003000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000313233343536000000000000000000004c0257e480891f6725f603013d6bcba70020482e7ded2f16d9d68a1f9fa07b7ae2f808d521e744f4b57e0afe3c4fe7df9208",
        "0103005f0203030000000000000000000300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000aaf508800228e4af5be8a1de946700e50000",
    ];
    const WPA3_MASTER: &str = "4e4dfab1a2dd8ac1a91790f953faaa452ae5c6873ab75b63605ba663f8a7fe59";
    const WPA3_TRAFFIC: &str = "9b2f001e456bf057febd38283820387c";
    const WPA3_FRAMES: [&str; 6] = [
        "0203005f02008800100000000000000001404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        "0103007e02010800000000000000000001a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf000000000000000000000000000000000000000000000000000000000000000068bf0b21f6302145c2a3e5fa8b4d59d3001f301a0100000fac040100000fac040100000fac08c0000000000fac06f40120",
        "020300b70213c800100000000000000002404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f0000000000000000000000000000000021222324252600000000000000000000eb12e4b18b53a5f104942e20ecfec2730058d7f6e7e76d4b2706eb44685e9292aa366cb88842d8c42e711e53f2f89a4ba9a6c0ee4a852d3d5ab6bc7bd8c3805748c44210fb18b8d8f4469a25a2604e4e265f847123e653bbc0c1fd3f033a43159843ff0dd6d78e19d4e2",
        "0103005f0203080000000000000000000200000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000f8e7c12e0e1555e281fbd6b5a0bdc4560000",
        "0203007f0213800010000000000000000300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000031323334353600000000000000000000131f7465b7e151974fb9df0025a6073000205d15be346fe345a2061dcfc7c58d8d61b16b3f5ce0f8dfc5edb72fb4ba5b28e5",
        "0103005f02030000000000000000000003000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001af1aea79938e9bc7c4c3d6e5edd71fd0000",
    ];
    const PLAIN: [u8; 22] =
        [48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 2, 0, 0];
    const PROTECTED: [u8; 22] =
        [48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 6, 0xC0, 0];
    const WPA3: [u8; 25] = [
        48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 8, 0xC0, 0, 244, 1, 0x20,
    ];

    #[test]
    fn handshake_frames() {
        let password = Library.derive_pmk(b"correct horse battery", b"r1metal test");
        assert_eq!(hex(&password), PLAIN_MASTER);
        let group: Vec<u8> = (0x10..0x20).collect();
        let cases = [
            (password, &PLAIN[..], PLAIN_FRAMES, PLAIN_TRAFFIC),
            (password, &PROTECTED[..], PROTECTED_FRAMES, PROTECTED_TRAFFIC),
            (bytes(WPA3_MASTER).try_into().unwrap(), &WPA3[..], WPA3_FRAMES, WPA3_TRAFFIC),
        ];
        for (key, elements, [first, second, third, fourth, rekey, rekey_answer], traffic) in cases {
            let choice = rsn::choose(elements).unwrap();
            let mut station = Supplicant::new(key, choice, [2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 9], elements);
            let answer = station.take(&Fixed, &bytes(first)).unwrap();
            assert_eq!(answer.len(), 1);
            let Step::Send(sent) = &answer[0] else { panic!("no frame in answer to the first") };
            assert_eq!(hex(sent), second, "the second frame, signed with the keys the two nonces give");
            let steps = station.take(&Fixed, &bytes(third)).unwrap();
            let Step::Send(sent) = &steps[0] else { panic!("no frame in answer to the third") };
            assert_eq!(hex(sent), fourth, "the fourth frame");
            assert_eq!(steps[1], Step::Pairwise(bytes(traffic).try_into().unwrap()), "the key between the two");
            assert_eq!(
                steps[2],
                Step::Group(GroupKey {
                    index: 2,
                    cipher: rsn::Group::Ccmp.suite(),
                    key: group.clone(),
                    sequence: [0x21, 0x22, 0x23, 0x24, 0x25, 0x26]
                })
            );
            if choice.is_protected {
                let management: Vec<u8> = (0x30..0x40).collect();
                assert_eq!(
                    steps[3..],
                    [Step::Management(GroupKey {
                        index: 4,
                        cipher: rsn::MANAGEMENT,
                        key: management,
                        sequence: [6, 5, 4, 3, 2, 1]
                    })]
                );
            } else {
                assert_eq!(steps.len(), 3);
            }
            let steps = station.take(&Fixed, &bytes(rekey)).unwrap();
            let new_group: Vec<u8> = group.iter().rev().copied().collect();
            assert_eq!(
                steps[1..],
                [Step::Group(GroupKey {
                    index: 1,
                    cipher: rsn::Group::Ccmp.suite(),
                    key: new_group,
                    sequence: [0x31, 0x32, 0x33, 0x34, 0x35, 0x36]
                })]
            );
            let Step::Send(sent) = &steps[0] else { panic!("no frame in answer to the new key") };
            assert_eq!(hex(sent), rekey_answer, "and the confirmation of a new key for what goes to all");
            let mut other = key;
            other[0] ^= 1;
            let mut wrong = Supplicant::new(other, choice, [2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 9], elements);
            wrong.take(&Fixed, &bytes(first)).unwrap();
            assert_eq!(
                wrong.take(&Fixed, &bytes(third)),
                Err(Error::WrongKey),
                "with another key, which is what another password gives, the network's third frame is not signed with the station's keys"
            );
        }
    }
}

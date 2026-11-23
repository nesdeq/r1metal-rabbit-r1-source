//! Signal: the R1 as a linked device of an existing account, its keys, sessions, messages and groups, and the service's requests.

mod account;
pub mod address;
pub mod attachment;
pub mod call;
pub mod command;
pub mod connection;
pub(crate) mod content;
pub mod conversation;
pub mod curve;
pub mod device;
mod errand;
pub mod group;
pub mod kem;
mod keys;
pub mod link;
mod message;
mod sealed;
mod sender_keys;
pub mod service;
mod session;
mod spqr;
pub mod storage;
mod zk;

use crate::crypto::Primitives;
use crate::crypto::X25519;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The name the kernel tags the records of the thread that keeps the R1 linked with.
pub const THREAD: &str = "signal";
pub const AUTO_THREAD: &str = "auto";
pub const MEDIA_THREAD: &str = "attachments";
pub const DRAFT_THREADS: [&str; 2] = ["drafts", "drafter"];
pub const PICTURES_THREAD: &str = "pictures";
pub const VOICES_THREAD: &str = "voices";
pub const THREADS: [&str; 7] =
    [THREAD, AUTO_THREAD, MEDIA_THREAD, DRAFT_THREADS[0], DRAFT_THREADS[1], PICTURES_THREAD, VOICES_THREAD];

/// The log's line on what Signal's work tells.
pub fn to_tagged_line(line: &str) -> String {
    format!("signal: {line}")
}

/// The two lattice key encapsulations of the sessions, as FIPS 203 defines them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lattice {
    // The post-quantum ratchet's.
    Mlkem768,
    // Under the pre-keys' Kyber1024 (`kem`).
    Mlkem1024,
}

/// The library arithmetic of the sessions and groups; each call it can refuse says so with `None` or `false`.
pub trait Crypto: Primitives + X25519 {
    fn sha512(&self, parts: &[&[u8]]) -> [u8; 64];
    fn sha256(&self, parts: &[&[u8]]) -> [u8; 32];
    fn sha1(&self, parts: &[&[u8]]) -> [u8; 20];
    // AES-256 with PKCS #7 padding.
    fn cbc_encrypt(&self, key: &[u8; 32], iv: &[u8; 16], plain: &[u8]) -> Option<Vec<u8>>;
    fn cbc_decrypt(&self, key: &[u8; 32], iv: &[u8; 16], ciphertext: &[u8]) -> Option<Vec<u8>>;
    // AES-256 from a counter block, the same both ways.
    fn ctr(&self, key: &[u8; 32], counter: &[u8; 16], data: &[u8]) -> Option<Vec<u8>>;
    // AES-256, the tag behind the ciphertext.
    fn gcm_decrypt(&self, key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>>;
    // AES-256, the tag behind the ciphertext.
    fn siv_encrypt(&self, key: &[u8; 32], nonce: &[u8; 12], plain: &[u8]) -> Option<Vec<u8>>;
    fn siv_decrypt(&self, key: &[u8; 32], nonce: &[u8; 12], ciphertext: &[u8]) -> Option<Vec<u8>>;
    // ML-KEM-1024: the decapsulation key and the encapsulation key, as FIPS 203 lays them out.
    fn lattice_pair(&self) -> Option<(Vec<u8>, Vec<u8>)>;
    // ML-KEM-1024: a ciphertext and the secret it carries.
    fn encapsulate(&self, public: &[u8]) -> Option<(Vec<u8>, [u8; 32])>;
    // A ciphertext the key cannot open gives FIPS 203's implicit rejection, not none.
    fn decapsulate(&self, lattice: Lattice, private: &[u8], ciphertext: &[u8]) -> Option<[u8; 32]>;
}

fn hkdf<const N: usize>(crypto: &impl Crypto, salt: Option<&[u8]>, secret: &[u8], info: &[&[u8]]) -> Option<[u8; N]> {
    let mut out = [0u8; N];
    crypto.hkdf_sha256(salt.unwrap_or(&[0; crate::crypto::SHA256_BYTES]), secret, info, &mut out).then_some(out)
}

fn write_skipped(skipped: &[(u32, [u8; 32])]) -> Vec<u8> {
    skipped.iter().flat_map(|(index, key)| index.to_be_bytes().into_iter().chain(*key)).collect()
}

fn read_skipped(bytes: &[u8]) -> Option<Vec<(u32, [u8; 32])>> {
    let (entries, rest) = bytes.as_chunks::<36>();
    rest.is_empty().then(|| {
        entries
            .iter()
            .map(|entry| {
                let (index, key) = entry.split_at(4);
                (u32::from_be_bytes(index.try_into().expect("four")), key.try_into().expect("thirty-two"))
            })
            .collect()
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::crypto::Primitives;
    use crate::crypto::exact::{self, hmac, sha1, sha256, sha512};
    use crate::crypto::keccak;
    use alloc::vec;
    use core::cell::RefCell;

    #[derive(Default)]
    pub(crate) struct Standin(RefCell<Vec<Vec<u8>>>);

    fn xor_stream(key: &[&[u8]], data: &[u8]) -> Vec<u8> {
        let mut mask = vec![0u8; data.len()];
        keccak::shake256(key, &mut mask);
        data.iter().zip(mask).map(|(byte, mask)| byte ^ mask).collect()
    }

    impl Standin {
        pub(super) fn seal(key: &[u8; 32], plain: &[u8]) -> Vec<u8> {
            [&key[..16], plain].concat()
        }
    }

    impl Primitives for Standin {
        fn hmac_sha256(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
            keccak::sha3_256(&[&[key], parts].concat())
        }
        fn hkdf_sha256(&self, salt: &[u8], secret: &[u8], info: &[&[u8]], out: &mut [u8]) -> bool {
            keccak::shake256(&[&[salt, secret], info].concat(), out);
            true
        }
        fn random(&self, out: &mut [u8]) -> bool {
            let mut draws = self.0.borrow_mut();
            keccak::shake256(&[&draws.len().to_le_bytes()], out);
            draws.push(out.to_vec());
            true
        }
    }

    impl X25519 for Standin {
        fn agree(&self, private: &[u8; 32], public: &[u8; 32]) -> Option<[u8; 32]> {
            curve::tests::x25519(private, Some(public))
        }
        fn public_of(&self, private: &[u8; 32]) -> Option<[u8; 32]> {
            curve::tests::x25519(private, None)
        }
    }

    impl Crypto for Standin {
        fn sha512(&self, _: &[&[u8]]) -> [u8; 64] {
            [0; 64]
        }
        fn sha256(&self, parts: &[&[u8]]) -> [u8; 32] {
            keccak::sha3_256(parts)
        }
        fn sha1(&self, parts: &[&[u8]]) -> [u8; 20] {
            keccak::sha3_256(parts)[..20].try_into().expect("twenty of thirty-two")
        }
        fn cbc_encrypt(&self, key: &[u8; 32], iv: &[u8; 16], plain: &[u8]) -> Option<Vec<u8>> {
            let padding = 16 - plain.len() % 16;
            Some(xor_stream(&[key, iv], &[plain, &vec![padding as u8; padding]].concat()))
        }
        fn cbc_decrypt(&self, key: &[u8; 32], iv: &[u8; 16], ciphertext: &[u8]) -> Option<Vec<u8>> {
            let mut plain = xor_stream(&[key, iv], ciphertext);
            plain.truncate(plain.len().checked_sub(usize::from(*plain.last()?))?);
            Some(plain)
        }
        fn ctr(&self, key: &[u8; 32], counter: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
            Some(xor_stream(&[key, counter], data))
        }
        fn gcm_decrypt(&self, key: &[u8; 32], _: &[u8; 12], _: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
            let (tag, plain) = ciphertext.split_at_checked(16)?;
            (tag == &key[..16]).then(|| plain.to_vec())
        }
        fn siv_encrypt(&self, key: &[u8; 32], _: &[u8; 12], plain: &[u8]) -> Option<Vec<u8>> {
            Some(Standin::seal(key, plain))
        }
        fn siv_decrypt(&self, key: &[u8; 32], nonce: &[u8; 12], ciphertext: &[u8]) -> Option<Vec<u8>> {
            self.gcm_decrypt(key, nonce, &[], ciphertext)
        }
        fn lattice_pair(&self) -> Option<(Vec<u8>, Vec<u8>)> {
            Some((vec![3; 32], keccak::sha3_256(&[&[3; 32]]).to_vec()))
        }
        fn encapsulate(&self, public: &[u8]) -> Option<(Vec<u8>, [u8; 32])> {
            Some((public.to_vec(), keccak::sha3_256(&[public])))
        }
        fn decapsulate(&self, lattice: Lattice, private: &[u8], ciphertext: &[u8]) -> Option<[u8; 32]> {
            if lattice == Lattice::Mlkem1024 {
                return Some(keccak::sha3_256(&[&keccak::sha3_256(&[private])]));
            }
            let header = kem::header_of(&private[kem::T_BYTES..2 * kem::T_BYTES + kem::SEED_BYTES]);
            self.0.borrow().iter().rev().find_map(|message| {
                let (first, secret, half) = kem::encapsulate_first(&header, message.as_slice().try_into().ok()?);
                let second = kem::encapsulate_second(&half, kem::t_of(private))?;
                ([first, second].concat() == ciphertext).then_some(secret)
            })
        }
    }

    #[derive(Default)]
    pub(super) struct Exact(Standin);

    impl Primitives for Exact {
        fn hmac_sha256(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
            hmac(sha256, key, parts)
        }
        fn hkdf_sha256(&self, salt: &[u8], secret: &[u8], info: &[&[u8]], out: &mut [u8]) -> bool {
            exact::hkdf(salt, secret, info, out)
        }
        fn random(&self, out: &mut [u8]) -> bool {
            self.0.random(out)
        }
    }

    impl X25519 for Exact {
        fn agree(&self, private: &[u8; 32], public: &[u8; 32]) -> Option<[u8; 32]> {
            self.0.agree(private, public)
        }
        fn public_of(&self, private: &[u8; 32]) -> Option<[u8; 32]> {
            self.0.public_of(private)
        }
    }

    impl Crypto for Exact {
        fn sha512(&self, parts: &[&[u8]]) -> [u8; 64] {
            sha512(parts)
        }
        fn sha256(&self, parts: &[&[u8]]) -> [u8; 32] {
            sha256(parts)
        }
        fn sha1(&self, parts: &[&[u8]]) -> [u8; 20] {
            sha1(parts)
        }
        fn cbc_encrypt(&self, key: &[u8; 32], iv: &[u8; 16], plain: &[u8]) -> Option<Vec<u8>> {
            self.0.cbc_encrypt(key, iv, plain)
        }
        fn cbc_decrypt(&self, key: &[u8; 32], iv: &[u8; 16], ciphertext: &[u8]) -> Option<Vec<u8>> {
            self.0.cbc_decrypt(key, iv, ciphertext)
        }
        fn ctr(&self, key: &[u8; 32], counter: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
            let mut out = data.to_vec();
            exact::ctr(key, counter, &mut out);
            Some(out)
        }
        fn gcm_decrypt(&self, key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
            self.0.gcm_decrypt(key, nonce, aad, ciphertext)
        }
        fn siv_encrypt(&self, key: &[u8; 32], nonce: &[u8; 12], plain: &[u8]) -> Option<Vec<u8>> {
            self.0.siv_encrypt(key, nonce, plain)
        }
        fn siv_decrypt(&self, key: &[u8; 32], nonce: &[u8; 12], ciphertext: &[u8]) -> Option<Vec<u8>> {
            self.0.siv_decrypt(key, nonce, ciphertext)
        }
        fn lattice_pair(&self) -> Option<(Vec<u8>, Vec<u8>)> {
            self.0.lattice_pair()
        }
        fn encapsulate(&self, public: &[u8]) -> Option<(Vec<u8>, [u8; 32])> {
            self.0.encapsulate(public)
        }
        fn decapsulate(&self, lattice: Lattice, private: &[u8], ciphertext: &[u8]) -> Option<[u8; 32]> {
            self.0.decapsulate(lattice, private, ciphertext)
        }
    }

    #[test]
    fn exact_vectors() {
        use crate::base::code::hex;
        let exact = Exact::default();
        assert_eq!(
            (exact.sha256(&[b"abc"]), exact.sha1(&[b"abc"]), exact.sha512(&[b"abc"])),
            (sha256(&[b"abc"]), sha1(&[b"abc"]), sha512(&[b"abc"])),
            "the hashes are the standards' own"
        );
        let salt: Vec<u8> = (0..=0x0c).collect();
        let info: Vec<u8> = (0xf0..=0xf9).collect();
        let mut okm = [0u8; 42];
        assert!(exact.hkdf_sha256(&salt, &[0x0b; 22], &[&info[..5], &info[5..]], &mut okm));
        assert_eq!(
            hex(&okm),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
            "RFC 5869, A.1, its info in parts"
        );
        let counter = [0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff];
        let key = crate::base::code::unhex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4").unwrap();
        let plain =
            crate::base::code::unhex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51").unwrap();
        assert_eq!(
            hex(&exact.ctr(&key.try_into().unwrap(), &counter, &plain).unwrap()),
            "601ec313775789a5b7a7f504bbf3d228f443e3ca4d62b59aca84e990cacaf5c5",
            "NIST SP 800-38A, F.5.5, its first two blocks"
        );
    }

    #[test]
    fn debug_without_secrets() {
        use super::address::ServiceId;
        use super::connection::Action;
        use super::content::Text;
        use super::curve::Pair;
        use super::device::{Device, Order};
        use super::errand::Errand;
        use super::group::{Context, Group, GroupFetch, GroupUpdate};
        use super::keys::{KyberKey, Ring};
        use super::link::Provision;
        use super::message::{encode_key, encode_kyber};
        use super::sender_keys::{self, Distribution};
        use super::service::{self, Frame, Request};
        use super::session::{Bundle, ChainKey, Record, State};
        use super::storage::Manifest;
        use alloc::borrow::Cow;
        use alloc::format;
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        device.add_named_contact(ServiceId::Aci([4; 16]), "Ann", false);
        let climbing = device.add_group(&crypto, &[8; 32], &[ServiceId::Aci([4; 16])]);
        let account = device.account.clone();
        let (identity, signed) = (Pair::new(&crypto).unwrap(), Pair::new(&crypto).unwrap());
        let (_, kyber) = crypto.lattice_pair().unwrap();
        let signature = |key: &[u8]| curve::sign(&crypto, &identity.private, &[key], &[0; 64]);
        let bundle = Bundle {
            registration: 7,
            identity: identity.public,
            pre_key: None,
            signed_pre_key: (2, signed.public, signature(&encode_key(&signed.public))),
            kyber_pre_key: (3, kyber.clone(), signature(&encode_kyber(&kyber))),
        };
        let mut record = Record::default();
        record.promote(State::begin(&crypto, &identity, 11, &bundle, false, 0).unwrap());
        let contact = &device.contacts()[&ServiceId::Aci([4; 16])];
        let headers = vec![("Authorization".into(), "Basic YTpi".into())];
        let request = Request::new("GET", "https://storage.signal.org".into(), headers, Cow::Borrowed(&[]));
        let distribution =
            sender_keys::tests::distribution([6; 16], 1, &ChainKey { key: [5; 32], index: 0 }, &identity);
        let mut group = Group::new([8; 32]);
        group.apply(GroupUpdate::State(super::group::State {
            revision: 3,
            title: Some("Climbing".into()),
            members: vec![ServiceId::Aci([4; 16])],
            ..super::group::State::default()
        }));
        let context =
            Context::read(&crate::base::proto::Message::parse(&Context::write(&[8; 32], 2).finish()).unwrap());
        let credential = super::group::tests::credential(&crypto);
        let fetch = || GroupFetch::new(credential.clone(), vec![(address::Peer::Group([8; 32]), [8; 32])]);
        let actual = [
            format!("{device:?}"),
            format!("{account:?}"),
            format!("{:?}", Pair { private: [1; 32], public: [2; 32] }),
            format!(
                "{:?}",
                KyberKey {
                    id: 3,
                    private: vec![9; 4],
                    public: vec![8; 4],
                    signature: [7; 64],
                    created: 5,
                    confirmed: None
                }
            ),
            format!("{:?}", Ring::default()),
            format!("{:?}", Provision { account, code: "code".into() }),
            format!("{contact:?}"),
            format!("{:?}", Manifest { version: 9, records: Vec::new(), record_key: vec![5; 4] }),
            format!("{request:?}"),
            format!(
                "{:?}",
                Text { timestamp: 7, body: "Lunch?".into(), profile_key: Some([6; 32]), ..Text::default() }
            ),
            format!("{:?}", Errand::begin(Order::new(ServiceId::Aci([4; 16]), vec![1], 5, true))),
            format!("{:?}", Action::Names("Basic YTpi".into())),
            format!("{record:?}"),
            format!(
                "{:?}",
                Frame::Request {
                    id: 9,
                    verb: "PUT".into(),
                    path: service::MESSAGE.into(),
                    body: b"envelope".to_vec(),
                    delivered: Some(1_700)
                }
            ),
            format!(
                "{:?}",
                Frame::Response { id: 4, status: 200, body: br#"{"username":"u","password":"p"}"#.to_vec() }
            ),
            format!("{:?}", Distribution::read(&distribution).unwrap()),
            format!("{:?}", sender_keys::Record::default()),
            format!("{group:?}"),
            format!("{:?}", context.unwrap()),
            format!("{credential:?}"),
            format!("{:?}", fetch()),
            format!("{:?}", Action::Groups(alloc::boxed::Box::new(fetch()))),
            format!(
                "{:?}",
                super::device::KnownIdentity {
                    key: [5; 32],
                    verified: 1,
                    first_use: false,
                    timestamp: 9,
                    is_approved: true
                }
            ),
            format!(
                "{:?}",
                super::device::Event::Relays(vec![service::Relay {
                    username: "u".into(),
                    password: "p".into(),
                    urls: vec!["turn:a".into()],
                    urls_with_ips: Vec::new(),
                    hostname: None,
                    ttl: Some(60)
                }])
            ),
            format!(
                "{:?}",
                super::device::Event::Token(
                    climbing,
                    device.membership(&crypto, &climbing, "token".into()).map(alloc::boxed::Box::new)
                )
            ),
            format!("{:?}", device.membership(&crypto, &climbing, "token".into()).unwrap()),
            format!(
                "{:?}",
                super::device::Event::Called(alloc::boxed::Box::new(super::device::Incoming {
                    sender: address::tests::address(ServiceId::Aci([4; 16]), 3),
                    name: Some("Ann".into()),
                    group_name: Some("Climbing".into()),
                    signals: vec![super::call::Signal::Busy { call: 1 }],
                    age: 9,
                    is_accepted: true,
                    identity: [5; 32],
                    own_identity: [6; 32],
                    own_device: 2
                }))
            ),
        ];
        assert_eq!(
            actual,
            [
                "Device",
                "Account { device: 2, .. }",
                "Pair",
                "KyberKey { id: 3, made: 5, .. }",
                "Ring { next_signed: 0, next_once: 0, next_kyber: 0, checked: 0, .. }",
                "Provisioned",
                "Contact",
                "Manifest { version: 9, .. }",
                "Request { method: \"GET\", body: 0, .. }",
                "Text { timestamp: 7, placeholders: [], expire: 0, expire_version: 0, .. }",
                "Errand { timestamp: 5, urgent: true, message: false, outcome: None, .. }",
                "Names",
                "Record { current: Some(State { local_registration: 11, remote_registration: 7, own_account: false, .. }), earlier: [] }",
                "Request { id: 9, verb: \"PUT\", path: \"/api/v1/message\", .. }",
                "Response { id: 4, status: 200, .. }",
                "Distribution",
                "Record { chains: 0, .. }",
                "Group { held: Some(3), named: 0, members: 1, .. }",
                "Context { revision: 2, .. }",
                "AuthCredential { redemption: 1728000000, .. }",
                "Fetching { groups: 1, tokens: 0, asked: 0, .. }",
                "Groups",
                "Known { verified: 1, first_use: false, timestamp: 9, approved: true, .. }",
                "Relays(1)",
                "Token { given: true, .. }",
                "Membership { members: 1, .. }",
                "Called(Incoming { signals: 1, age: 9, accepted: true, .. })",
            ]
        );
    }

    #[test]
    fn debug_without_words() {
        use super::address::{Address, Peer, ServiceId};
        use super::call::{Kind, Signal};
        use super::command::{Command, Outbound, Outgoing, Target};
        use super::content::{Content, Text};
        use super::conversation::tests::entry;
        use super::device::{Access, Course, DropReason, Event, File, Note};
        use super::errand::{self, Outcome};
        use super::service;
        use super::session::Error;
        use alloc::format;
        const ANN: ServiceId = ServiceId::Aci([4; 16]);
        let ann = Peer::Contact(ANN);
        let busy = |target| Outbound { target, device: None, signal: Signal::Busy { call: 1 } };
        let note = Note { peer: ann, call: 9, kind: Kind::Audio, is_outgoing: false, ringer: Some(ANN), at: 5 };
        let lunch = || Outgoing { peer: ann, body: "Lunch?".into(), is_robot: true, enclosures: Vec::new() };
        let sender_entry = super::conversation::Entry { sender: Some(ANN), ..entry(7, false, "Lunch?") };
        let quoting = super::conversation::Entry {
            quote: Some(super::content::Quote { timestamp: 6, author: ANN, text: "Lunch?".into() }),
            ..entry(7, false, "Yes")
        };
        let actual = [
            format!("{:?}", entry(7, false, "Lunch?")),
            format!("{:?}", Event::Arrived { peer: ann, timestamp: 7, text: "Lunch?".into(), is_noticed: true }),
            format!("{:?}", Event::Wrote(ann)),
            format!("{:?}", lunch()),
            format!("{:?}", Command::Send(lunch())),
            format!("{quoting:?}"),
            format!("{:?}", super::content::Identifier::Number("+15550000000".into())),
            format!("{:?}", Command::Read(ann)),
            format!("{:?}", Command::Auto(Some(ANN))),
            format!("{:?}", File::Conversation(ann)),
            format!("{:?}", Address { service: ServiceId::Pni([4; 16]), device: 3 }),
            format!(
                "{:?}",
                Content::Sent {
                    to: Some(ANN),
                    text: Text { timestamp: 7, body: "Lunch?".into(), ..Text::default() },
                    started: Some(8)
                }
            ),
            format!("{:?}", errand::Step::Keys { path: service::bundles_path(&ANN), access: None }),
            format!(
                "{:?}",
                errand::Step::Submit {
                    path: service::messages_path(&ANN),
                    body: "{}".into(),
                    access: Some(Access::Key([6; 16]))
                }
            ),
            format!("{:?}", errand::Step::Done(Outcome::Sent)),
            format!("{sender_entry:?}"),
            format!(
                "{:?}",
                DropReason::Unopened {
                    sender: Address { service: ANN, device: 2 },
                    timestamp: 7,
                    ratchet: Some([5; 32]),
                    group: Some([8; 32]),
                    reason: Error::NoSession
                }
            ),
            format!("{:?}", Command::Signal(busy(Target::Members(Peer::Group([8; 32]), vec![ANN])))),
            format!("{:?}", Command::Course(note, Course::Missed)),
            format!("{:?}", Command::Token(Peer::Group([8; 32]))),
            format!("{:?}", Event::Unsent(alloc::boxed::Box::new(busy(Target::Contact(ANN))))),
            format!("{:?}", Event::Logged),
        ];
        assert_eq!(
            actual,
            [
                "Entry { timestamp: 7, at: 7, owner: false, placeholders: [], deleted: None, status: Received, expire: 0, started: None, read: false, edited: false, robot: false, .. }",
                "Arrived { timestamp: 7, noticed: true, .. }",
                "Wrote",
                "Outgoing { robot: true, .. }",
                "Send(Outgoing { robot: true, .. })",
                "Entry { timestamp: 7, at: 7, owner: false, placeholders: [], deleted: None, status: Received, expire: 0, started: None, read: false, edited: false, robot: false, .. }",
                "Number",
                "Read(Aci)",
                "Auto(Some(Aci))",
                "Conversation(Aci)",
                "Address { service: Pni, device: 3 }",
                "Sent { to: Some(Aci), text: Text { timestamp: 7, placeholders: [], expire: 0, expire_version: 0, .. }, started: Some(8) }",
                "Keys { sealed: false, .. }",
                "Submit { sealed: true, .. }",
                "Done(Sent)",
                "Entry { timestamp: 7, at: 7, owner: false, placeholders: [], deleted: None, status: Received, expire: 0, started: None, read: false, edited: false, robot: false, .. }",
                "Unopened { from: Address { service: Aci, device: 2 }, timestamp: 7, why: NoSession, .. }",
                "Signal(Outbound { target: Members(Group, [Aci]), device: None, signal: Busy { call: 1 } })",
                "Course(Note { peer: Aci, call: 9, kind: Audio, is_outgoing: false, ringer: Some(Aci), at: 5 }, Missed)",
                "Token(Group)",
                "Unsent(Contact(Aci), ..)",
                "Logged",
            ]
        );
    }

    #[test]
    fn skipped_round_trip() {
        let skipped = [(7, [1; 32]), (0x0102_0304, [2; 32])];
        let bytes = write_skipped(&skipped);
        assert_eq!(bytes.len(), 72);
        assert_eq!(bytes[36..40], [1, 2, 3, 4], "the index highest byte first");
        assert_eq!(read_skipped(&bytes).as_deref(), Some(&skipped[..]));
        assert_eq!(read_skipped(&[]), Some(Vec::new()));
        assert_eq!(read_skipped(&bytes[..71]), None);
    }
}

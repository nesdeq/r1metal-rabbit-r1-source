// The ids zkgroup encrypts under a group's keys, as zkcredential's attributes.

use super::super::Crypto;
use super::super::address::ServiceId;
use super::ristretto::Element;
use super::{Scalar, Sho};
use crate::base::code::{UUID_NAMED, to_uuid, unhex};
use alloc::vec::Vec;

pub(in crate::signal) const UID_GENERATORS: &str = "a6324c368df734691147981348b6e7eb42c3307e711b6c7eccd3032d45693f5a048013525b76124bf2640c5e9369c76efbe80aba2a24aa5d8e18a98eba14f837";
pub(in crate::signal) const PROFILE_KEY_GENERATORS: &str = "f6baa317ce1839c93d617e0cd837d19da9c8a4c520bf7c51b1e6c2cb2a049c612e0175894c8730b203ab3bd98ecb2d81abacb65f8a6124f49771d14a9852120c";
const CALC_M1: &[u8] = b"Signal_ZKGroup_20200424_UID_CalcM1";
const FALSE_PNI_NAMESPACE: [u8; 16] =
    [0x01, 0x9f, 0xaa, 0x2b, 0x7b, 0xcb, 0x70, 0xc2, 0xac, 0x7f, 0xcd, 0x77, 0x04, 0x14, 0x68, 0x9a];
const RESERVED: u8 = 0;
const ELEMENTS_BYTES: usize = 64;

pub(in crate::signal) fn generators(text: &str) -> [Element; 2] {
    let bytes = unhex(text).expect("hex of two elements");
    [0, 1].map(|at| Element::decode(&bytes[32 * at..32 * (at + 1)]).expect("the writing of an element"))
}

#[derive(Clone, Copy)]
pub(super) struct Uid {
    pub(super) m1: Element,
    pub(super) m2: Element,
}

impl Uid {
    pub(super) fn new(crypto: &impl Crypto, id: &ServiceId) -> Uid {
        let (ServiceId::Aci(uuid) | ServiceId::Pni(uuid)) = id;
        Uid { m1: m1(crypto, id), m2: Element::lizard(crypto, uuid) }
    }
}

pub(super) fn m1(crypto: &impl Crypto, id: &ServiceId) -> Element {
    let mut sho = Sho::new(crypto, CALC_M1);
    sho.absorb(crypto, &id.to_binary());
    sho.element(crypto)
}

fn uuid_v5(crypto: &impl Crypto, namespace: &[u8; 16], name: &[&[u8]]) -> [u8; 16] {
    let mut parts: Vec<&[u8]> = alloc::vec![namespace];
    parts.extend_from_slice(name);
    to_uuid(crypto.sha1(&parts)[..16].try_into().expect("sixteen bytes of twenty"), UUID_NAMED)
}

pub(super) fn false_pni(crypto: &impl Crypto, aci: &[u8; 16], salt: &[u8]) -> [u8; 16] {
    uuid_v5(crypto, &FALSE_PNI_NAMESPACE, &[&crypto.sha256(&[salt]), aci])
}

#[derive(Clone, Copy)]
pub(in crate::signal) struct KeyPair {
    pub(super) a1: Scalar,
    pub(super) a2: Scalar,
    pub(in crate::signal) public: Element,
}

impl KeyPair {
    pub(in crate::signal) fn derive(crypto: &impl Crypto, sho: &mut Sho, [first, second]: [Element; 2]) -> KeyPair {
        let (a1, a2) = (sho.scalar(crypto), sho.scalar(crypto));
        KeyPair { a1, a2, public: first.times(&a1).add(&second.times(&a2)) }
    }

    pub(super) fn encrypt(&self, uid: &Uid) -> [Element; 2] {
        let first = uid.m1.times(&self.a1);
        [first, first.times(&self.a2).add(&uid.m2)]
    }

    // An id's ciphertext as zkgroup writes it: the reserved byte, then the two elements.
    pub(in crate::signal) fn encrypt_id(&self, crypto: &impl Crypto, id: &ServiceId) -> Vec<u8> {
        let [first, second] = self.encrypt(&Uid::new(crypto, id));
        [&[RESERVED][..], &first.encode(), &second.encode()].concat()
    }

    pub(in crate::signal) fn decrypt_id(&self, crypto: &impl Crypto, ciphertext: &[u8]) -> Option<ServiceId> {
        let (&reserved, elements) = ciphertext.split_first()?;
        if reserved != RESERVED || elements.len() != ELEMENTS_BYTES {
            return None;
        }
        let (first, second) = elements.split_at(ELEMENTS_BYTES / 2);
        let (first, second) = (Element::decode(first)?, Element::decode(second)?);
        if first.same(&Element::BASE) {
            return None;
        }
        let uuid = second.sub(&first.times(&self.a2)).unlizard(crypto)?;
        [ServiceId::Aci(uuid), ServiceId::Pni(uuid)].into_iter().find(|id| m1(crypto, id).times(&self.a1).same(&first))
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::Exact;
    use super::*;
    use crate::base::code::hex;

    #[test]
    fn generator_points() {
        let crypto = Exact::default();
        for (label, text) in [
            (&b"Signal_ZKGroup_20200424_Constant_UidEncryption_SystemParams_Generate"[..], UID_GENERATORS),
            (b"Signal_ZKGroup_20200424_Constant_ProfileKeyEncryption_SystemParams_Generate", PROFILE_KEY_GENERATORS),
        ] {
            let mut sho = Sho::new(&crypto, label);
            sho.absorb(&crypto, b"");
            let expected = [sho.element(&crypto), sho.element(&crypto)];
            assert!(generators(text).iter().zip(&expected).all(|(actual, element)| actual.same(element)), "{text}");
        }
    }

    #[test]
    fn id_encryption() {
        let crypto = Exact::default();
        let input: [u8; 32] = core::array::from_fn(|at| at as u8);
        let uuid: [u8; 16] = core::array::from_fn(|at| at as u8);
        let mut sho = Sho::new(&crypto, b"Test_Uid_Encryption");
        sho.absorb(&crypto, &input);
        let key = KeyPair::derive(&crypto, &mut sho, generators(UID_GENERATORS));
        let aci = ServiceId::Aci(uuid);
        let bytes = key.encrypt_id(&crypto, &aci);
        assert_eq!(
            hex(&bytes[1..]),
            "f89ee7705a66036b908db884211b773ac543ee35c4a3086220fc3e1e35b4234cfa1d2eea2cc2f4b4c42cff39a9dceb57293b5f8770ca60f9e9b74447bfd3bd3d",
            "zkgroup's own vector"
        );
        assert_eq!(key.decrypt_id(&crypto, &bytes), Some(aci));
        let mut sho = Sho::new(&crypto, b"Test_Pni_Encryption");
        sho.absorb(&crypto, &[]);
        let other = KeyPair::derive(&crypto, &mut sho, generators(UID_GENERATORS));
        let pni = ServiceId::Pni(uuid);
        assert_eq!(other.decrypt_id(&crypto, &other.encrypt_id(&crypto, &pni)), Some(pni), "a PNI reads back as one");
        assert_eq!(other.decrypt_id(&crypto, &bytes), None, "under another key");
        let mut reserved = bytes.clone();
        reserved[0] = 1;
        let mut corrupt = bytes.clone();
        corrupt[40] ^= 1;
        assert_eq!(
            [
                key.decrypt_id(&crypto, &reserved),
                key.decrypt_id(&crypto, &corrupt),
                key.decrypt_id(&crypto, &bytes[..64])
            ],
            [None, None, None],
            "the reserved byte set, an element changed, a byte short"
        );
    }

    #[test]
    fn uuid_v5_and_false_pni() {
        let crypto = Exact::default();
        let dns = [0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30, 0xc8];
        assert_eq!(hex(&uuid_v5(&crypto, &dns, &[b"www.example.com"])), "2ed6657de927568b95e12665a8aea6a2");
        assert_eq!(
            uuid_v5(&crypto, &dns, &[b"www.", b"example.com"]),
            uuid_v5(&crypto, &dns, &[b"www.example.com"]),
            "a name in parts"
        );
        let pni = false_pni(&crypto, &[0xA1; 16], b"salt");
        assert_eq!((pni[6] >> 4, pni[8] >> 6), (5, 2), "version 5, RFC 9562's variant");
        assert_ne!(pni, false_pni(&crypto, &[0xA1; 16], b"pepper"), "another salt, another PNI");
    }
}

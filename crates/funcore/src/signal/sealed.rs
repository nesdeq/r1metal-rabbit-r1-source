// Sealed sender, versions 1 and 2.

use super::address::ServiceId;
use super::content;
use super::curve::{self, Pair};
use super::message::{decode_key, encode_key};
use super::{Crypto, hkdf};
use crate::base::bytes::same;
use crate::base::code::unhex;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::base::proto::write_varint;
use alloc::vec::Vec;

const TRUST_ROOTS: [[u8; 32]; 2] = [
    *b"\x7b\xba\x40\x82\x95\xcf\x93\x00\xf2\x0b\x2d\xcd\xf3\xa0\x45\x01\xaa\xc8\xba\x8e\xc0\xd2\x87\x2f\xa2\x0d\x92\xfd\xc8\x1d\x63\x05",
    *b"\x49\x18\xd0\x8f\xbd\xfa\x83\xe0\x0c\x29\xf8\xf8\x07\x3a\x22\xef\x35\xdf\x2b\xea\x90\x3a\xff\x81\xaf\x03\xcc\xbc\x45\xc6\xe9\x3a",
];
const PRODUCTION_CERTIFICATE: &str = "0a250803122105bc9d1d290be964810dfa7e94856480a3f7060d004c9762c24c575a1522353a5a1240c11ec3c401eb0107ab38f8600e8720a63169e0e2eb8a3fae24f63099f85ea319c3c1c46d3454706ae2a679d1fee690a488adda98a2290b66c906bb60295ed781";
const PRODUCTION_ID: u32 = 3;
const REVOKED: [u32; 1] = [0xDEAD_C357];
const LABEL_R: &[u8] = b"Sealed Sender v2: r (2023-08)";
const LABEL_K: &[u8] = b"Sealed Sender v2: K";
const LABEL_DH: &[u8] = b"Sealed Sender v2: DH";
const LABEL_DH_SENDER: &[u8] = b"Sealed Sender v2: DH-sender";
const DELIVERY: &[u8] = b"UnidentifiedDelivery";
const MAC_BYTES: usize = 10;
const V1_VERSION: u8 = 0x11;
const PREKEY: u32 = 1;
const WHISPER: u32 = 2;
pub(super) const SENDER_KEY: u32 = 7;
const PLAINTEXT: u32 = 8;
// libsignal's `ContentHint`s: the default, one a recipient that cannot open it asks again, and one it lets go.
pub(super) const DEFAULT: u32 = 0;
pub(super) const RESENDABLE: u32 = 1;
pub(super) const IMPLICIT: u32 = 2;
const CERTIFICATE_NUMBER: u32 = 1;
const CERTIFICATE_EXPIRES: u32 = 3;
const V2_SEND_VERSION: u8 = 0x23;
const MORE_DEVICES: u16 = 0x8000;
const REGISTRATION_BITS: u32 = 0x3FFF;
const NO_NONCE: [u8; 12] = [0; 12];

// The kind a sealed content names a ciphertext of an envelope's kind by; one of no session's is plaintext.
pub(super) const fn kind_of(envelope_kind: u32) -> u32 {
    match envelope_kind {
        content::PREKEY_MESSAGE => PREKEY,
        content::DOUBLE_RATCHET => WHISPER,
        content::SENDER_KEY => SENDER_KEY,
        _ => PLAINTEXT,
    }
}

// The envelope's kind of the ciphertext a sealed content names by `kind`; none for a kind the R1 does not take.
pub(super) const fn envelope_kind(kind: u32) -> Option<u32> {
    match kind {
        PREKEY => Some(content::PREKEY_MESSAGE),
        WHISPER => Some(content::DOUBLE_RATCHET),
        SENDER_KEY => Some(content::SENDER_KEY),
        PLAINTEXT => Some(content::PLAINTEXT),
        _ => None,
    }
}

/// Why a sealed message was not opened.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    Malformed,
    // It does not open with the R1's key, or its parts disagree.
    Unopened,
    // Not signed by a trust root, or expired.
    Certificate,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Contents {
    pub(super) sender: ServiceId,
    pub(super) device: u32,
    pub(super) identity: [u8; 32],
    pub(super) kind: u8,
    pub(super) content: Vec<u8>,
    pub(super) group: Option<[u8; 32]>,
}

impl core::fmt::Debug for Contents {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Contents").field("device", &self.device).field("kind", &self.kind).finish_non_exhaustive()
    }
}

enum SenderBinding {
    Identity(Vec<u8>),
    Tag { tag: [u8; 16], key: [u8; 32], ephemeral: [u8; 32] },
}

fn open_outer(crypto: &impl Crypto, identity: &Pair, bytes: &[u8]) -> Result<(Vec<u8>, SenderBinding), Error> {
    let (&version, rest) = bytes.split_first().ok_or(Error::Malformed)?;
    match version >> 4 {
        0 | 1 => open_v1(crypto, identity, rest),
        2 => open_v2(crypto, identity, rest),
        _ => Err(Error::Malformed),
    }
}

fn open_v1(crypto: &impl Crypto, identity: &Pair, rest: &[u8]) -> Result<(Vec<u8>, SenderBinding), Error> {
    let message = Message::parse(rest).ok_or(Error::Malformed)?;
    let part = |number| message.bytes(number).ok_or(Error::Malformed);
    let ephemeral = decode_key(part(1)?).ok_or(Error::Malformed)?;
    let (sealed_sender, sealed_content) = (part(2)?, part(3)?);
    let salt = [DELIVERY, &encode_key(&identity.public), &encode_key(&ephemeral)].concat();
    let keys = v1_keys(crypto, &identity.private, &ephemeral, &salt).ok_or(Error::Unopened)?;
    let sender = open_part(crypto, &keys, sealed_sender)?;
    let key = decode_key(&sender).ok_or(Error::Malformed)?;
    let salt = [&keys[..32], sealed_sender].concat();
    let keys = v1_keys(crypto, &identity.private, &key, &salt).ok_or(Error::Unopened)?;
    let plain = open_part(crypto, &keys, sealed_content)?;
    Ok((plain, SenderBinding::Identity(sender)))
}

// Version 1's chain, cipher and MAC keys of what a private and a public key agree on, under a salt.
fn v1_keys(crypto: &impl Crypto, private: &[u8; 32], public: &[u8; 32], salt: &[u8]) -> Option<[u8; 96]> {
    hkdf(crypto, Some(salt), &crypto.agree(private, public)?, &[])
}

// Version 2's mask of a recipient's share of the message key, of what the ephemeral key and the recipient's agree on.
fn v2_mask(crypto: &impl Crypto, shared: &[u8; 32], ephemeral: &[u8; 32], recipient: &[u8; 32]) -> Option<[u8; 32]> {
    hkdf(crypto, None, &[&shared[..], &encode_key(ephemeral), &encode_key(recipient)].concat(), &[LABEL_DH])
}

// Version 2's tag that binds a recipient's share to the sender, of what the sender's key and the recipient's agree on.
fn v2_tag(
    crypto: &impl Crypto,
    shared: &[u8; 32],
    ephemeral: &[u8; 32],
    share: &[u8; 32],
    sender: &[u8; 32],
    recipient: &[u8; 32],
) -> Option<[u8; 16]> {
    let input = [&shared[..], &encode_key(ephemeral), share, &encode_key(sender), &encode_key(recipient)].concat();
    hkdf(crypto, None, &input, &[LABEL_DH_SENDER])
}

fn open_part(crypto: &impl Crypto, keys: &[u8; 96], part: &[u8]) -> Result<Vec<u8>, Error> {
    let at = part.len().checked_sub(MAC_BYTES).ok_or(Error::Malformed)?;
    let (ciphertext, mac) = part.split_at(at);
    if !same(&crypto.hmac_sha256(&keys[64..], &[ciphertext])[..MAC_BYTES], mac) {
        return Err(Error::Unopened);
    }
    let key: &[u8; 32] = keys[32..64].try_into().expect("thirty-two");
    crypto.ctr(key, &[0; 16], ciphertext).ok_or(Error::Unopened)
}

fn open_v2(crypto: &impl Crypto, identity: &Pair, rest: &[u8]) -> Result<(Vec<u8>, SenderBinding), Error> {
    if rest.len() < 80 {
        return Err(Error::Malformed);
    }
    let key: [u8; 32] = rest[..32].try_into().expect("thirty-two");
    let tag: [u8; 16] = rest[32..48].try_into().expect("sixteen");
    let ephemeral: [u8; 32] = rest[48..80].try_into().expect("thirty-two");
    let shared = crypto.agree(&identity.private, &ephemeral).ok_or(Error::Unopened)?;
    let mask = v2_mask(crypto, &shared, &ephemeral, &identity.public).ok_or(Error::Unopened)?;
    let m: [u8; 32] = core::array::from_fn(|at| mask[at] ^ key[at]);
    let r: [u8; 32] = hkdf(crypto, None, &m, &[LABEL_R]).ok_or(Error::Unopened)?;
    if !Pair::from_private(crypto, r).is_some_and(|pair| same(&pair.public, &ephemeral)) {
        return Err(Error::Unopened);
    }
    let k: [u8; 32] = hkdf(crypto, None, &m, &[LABEL_K]).ok_or(Error::Unopened)?;
    let plain = crypto.siv_decrypt(&k, &[0; 12], &rest[80..]).ok_or(Error::Unopened)?;
    Ok((plain, SenderBinding::Tag { tag, key, ephemeral }))
}

fn seal_part(crypto: &impl Crypto, keys: &[u8; 96], plain: &[u8]) -> Option<Vec<u8>> {
    let key: &[u8; 32] = keys[32..64].try_into().expect("thirty-two");
    let ciphertext = crypto.ctr(key, &[0; 16], plain)?;
    let mac = crypto.hmac_sha256(&keys[64..], &[&ciphertext]);
    Some([&ciphertext[..], &mac[..MAC_BYTES]].concat())
}

pub(super) fn write_content(
    kind: u32,
    certificate: &[u8],
    ciphertext: &[u8],
    hint: u32,
    group: Option<&[u8; 32]>,
) -> Vec<u8> {
    Writer::new()
        .varint(1, u64::from(kind))
        .bytes(2, certificate)
        .bytes(3, ciphertext)
        .maybe((hint != 0).then_some(move |w: Writer| w.varint(4, u64::from(hint))))
        .maybe(group.map(|id| move |w: Writer| w.bytes(5, id)))
        .finish()
}

pub(super) fn seal(crypto: &impl Crypto, identity: &Pair, recipient: &[u8; 32], content: &[u8]) -> Option<Vec<u8>> {
    let ephemeral = Pair::new(crypto)?;
    let salt = [DELIVERY, &encode_key(recipient), &encode_key(&ephemeral.public)].concat();
    let first = v1_keys(crypto, &ephemeral.private, recipient, &salt)?;
    let sender = seal_part(crypto, &first, &encode_key(&identity.public))?;
    let second = v1_keys(crypto, &identity.private, recipient, &[&first[..32], &sender].concat())?;
    let message = Writer::new()
        .bytes(1, &encode_key(&ephemeral.public))
        .bytes(2, &sender)
        .bytes(3, &seal_part(crypto, &second, content)?)
        .finish();
    Some([&[V1_VERSION][..], &message].concat())
}

pub(super) struct Recipient {
    pub(super) id: ServiceId,
    pub(super) identity: [u8; 32],
    pub(super) devices: Vec<(u32, u32)>,
}

pub(super) fn seal_many(
    crypto: &impl Crypto,
    identity: &Pair,
    recipients: &[Recipient],
    content: &[u8],
) -> Result<Vec<u8>, Option<ServiceId>> {
    let mut m = [0u8; 32];
    crypto.random(&mut m).then_some(()).ok_or(None)?;
    let r: [u8; 32] = hkdf(crypto, None, &m, &[LABEL_R]).ok_or(None)?;
    let ephemeral = Pair::from_private(crypto, r).ok_or(None)?;
    let k: [u8; 32] = hkdf(crypto, None, &m, &[LABEL_K]).ok_or(None)?;
    let ciphertext = crypto.siv_encrypt(&k, &NO_NONCE, content).ok_or(None)?;
    let mut out = alloc::vec![V2_SEND_VERSION];
    write_varint(&mut out, recipients.len() as u64);
    for recipient in recipients {
        let error = Some(recipient.id);
        out.extend(recipient.id.to_fixed());
        let last = recipient.devices.len().checked_sub(1).ok_or(error)?;
        for (at, (device, registration)) in recipient.devices.iter().enumerate() {
            let registration =
                u16::try_from(*registration).ok().filter(|_| *registration <= REGISTRATION_BITS).ok_or(error)?;
            let flag = if at < last { MORE_DEVICES } else { 0 };
            out.push(u8::try_from(*device).map_err(|_| error)?);
            out.extend((registration | flag).to_be_bytes());
        }
        let shared = crypto.agree(&ephemeral.private, &recipient.identity).ok_or(None)?;
        let mask = v2_mask(crypto, &shared, &ephemeral.public, &recipient.identity).ok_or(None)?;
        let c: [u8; 32] = core::array::from_fn(|at| mask[at] ^ m[at]);
        let shared = crypto.agree(&identity.private, &recipient.identity).ok_or(None)?;
        let tag = v2_tag(crypto, &shared, &ephemeral.public, &c, &identity.public, &recipient.identity).ok_or(None)?;
        out.extend(c);
        out.extend(tag);
    }
    out.extend(ephemeral.public);
    out.extend(ciphertext);
    Ok(out)
}

pub(super) fn parse_expiry(certificate: &[u8]) -> Option<(u64, bool)> {
    let (_, inner, _) = parse_certificate(certificate).ok()?;
    Some((inner.fixed64(CERTIFICATE_EXPIRES)?, inner.has(CERTIFICATE_NUMBER)))
}

fn parse_certificate(bytes: &[u8]) -> Result<(&[u8], Message<'_>, [u8; 64]), Error> {
    let certificate = Message::parse(bytes).ok_or(Error::Malformed)?;
    let signed_bytes = certificate.bytes(1).ok_or(Error::Malformed)?;
    let signature = certificate.bytes(2).and_then(|bytes| bytes.try_into().ok()).ok_or(Error::Malformed)?;
    Ok((signed_bytes, Message::parse(signed_bytes).ok_or(Error::Malformed)?, signature))
}

fn server_key(crypto: &impl Crypto, bytes: &[u8]) -> Result<[u8; 32], Error> {
    let (signed_bytes, inner, signature) = parse_certificate(bytes)?;
    let id = inner.u32(1).ok_or(Error::Malformed)?;
    let key = decode_key(inner.bytes(2).ok_or(Error::Malformed)?).ok_or(Error::Malformed)?;
    let mut is_trusted = false;
    for root in &TRUST_ROOTS {
        is_trusted |= curve::verify(crypto, root, &[signed_bytes], &signature);
    }
    (is_trusted && !REVOKED.contains(&id)).then_some(key).ok_or(Error::Certificate)
}

pub(super) fn open(crypto: &impl Crypto, identity: &Pair, bytes: &[u8], at: u64) -> Result<Contents, Error> {
    let (plain, binding) = open_outer(crypto, identity, bytes)?;
    let content = Message::parse(&plain).ok_or(Error::Malformed)?;
    let kind = content.u32(1).and_then(|kind| u8::try_from(kind).ok()).ok_or(Error::Malformed)?;
    let (signed_bytes, certificate, signature) = parse_certificate(content.bytes(2).ok_or(Error::Malformed)?)?;
    let sender = match certificate.bytes(7) {
        Some(bytes) => ServiceId::Aci(bytes.try_into().map_err(|_| Error::Malformed)?),
        None => ServiceId::from_text(certificate.text(6).ok_or(Error::Malformed)?).ok_or(Error::Malformed)?,
    };
    let sender_identity = decode_key(certificate.bytes(4).ok_or(Error::Malformed)?).ok_or(Error::Malformed)?;
    let server = match (certificate.bytes(5), certificate.u32(8)) {
        (Some(embedded), _) => server_key(crypto, embedded)?,
        (None, Some(PRODUCTION_ID)) => server_key(crypto, &unhex(PRODUCTION_CERTIFICATE).ok_or(Error::Malformed)?)?,
        _ => return Err(Error::Certificate),
    };
    if !curve::verify(crypto, &server, &[signed_bytes], &signature)
        || at > certificate.fixed64(3).ok_or(Error::Malformed)?
    {
        return Err(Error::Certificate);
    }
    let is_bound = match binding {
        SenderBinding::Identity(key) => same(&key, &encode_key(&sender_identity)),
        SenderBinding::Tag { tag, key, ephemeral } => {
            let shared = crypto.agree(&identity.private, &sender_identity).ok_or(Error::Unopened)?;
            v2_tag(crypto, &shared, &ephemeral, &key, &sender_identity, &identity.public)
                .is_some_and(|expected| same(&expected, &tag))
        }
    };
    if !is_bound {
        return Err(Error::Unopened);
    }
    Ok(Contents {
        sender,
        device: certificate.u32(2).ok_or(Error::Malformed)?,
        identity: sender_identity,
        kind,
        content: content.bytes(3).ok_or(Error::Malformed)?.to_vec(),
        group: content.bytes(5).and_then(|id| id.try_into().ok()),
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::content::{PREKEY_MESSAGE, pad, unpad};
    use super::super::message::{PreKeyMessage, SignalMessage, encode_kyber};
    use super::super::session::{self, Bundle, Ciphertext, PreKeys, Record, State};
    use super::super::tests::Standin;
    use super::*;
    use crate::signal::address::Address;
    use alloc::format;
    use alloc::string::String;

    pub(in crate::signal) fn certificate_of(crypto: &impl Crypto, sender: &Pair, address: &Address) -> Vec<u8> {
        let identity = encode_key(&sender.public);
        let certificate = |fields: &[u8]| {
            let signature = curve::sign(crypto, &sender.private, &[fields], &[0; 64]);
            Writer::new().bytes(1, fields).bytes(2, &signature)
        };
        let server = certificate(&Writer::new().varint(1, 7).bytes(2, &identity).finish());
        let fields = Writer::new()
            .bytes(7, &address.service.to_binary())
            .varint(2, u64::from(address.device))
            .bytes(4, &identity)
            .message(5, server)
            .finish();
        // The time it runs out at, a fixed64, which the writer has no call for.
        certificate(&[&fields[..], &[0x19], &1_000u64.to_le_bytes()].concat()).finish()
    }

    fn content_of(
        crypto: &Standin,
        sender: &Pair,
        address: &Address,
        (kind, group): (u8, Option<&[u8; 32]>),
        ciphertext: &[u8],
    ) -> Vec<u8> {
        write_content(u32::from(kind), &certificate_of(crypto, sender, address), ciphertext, 0, group)
    }

    fn v1_of(crypto: &Standin, sender: &Pair, recipient: &[u8; 32], content: &[u8]) -> Vec<u8> {
        seal(crypto, sender, recipient, content).expect("sealed")
    }

    fn v2_of(crypto: &Standin, sender: &Pair, recipient: &[u8; 32], content: &[u8]) -> Vec<u8> {
        let recipient = Recipient { id: ServiceId::Aci([9; 16]), identity: *recipient, devices: alloc::vec![(1, 1)] };
        let sealed = seal_many(crypto, sender, &[recipient], content).expect("sealed");
        to_shares(&sealed).remove(0).2
    }

    #[test]
    fn version_1_sender_check() {
        let crypto = Standin::default();
        let pair = || Pair::new(&crypto).unwrap();
        let (sender, recipient, impostor) = (pair(), pair(), pair());
        let address = Address { service: ServiceId::Aci([1; 16]), device: 3 };
        let content = content_of(&crypto, &sender, &address, (7, Some(&[8; 32])), b"a sender key message");
        let sealed = v1_of(&crypto, &sender, &recipient.public, &content);
        let expected = Contents {
            sender: address.service,
            device: 3,
            identity: sender.public,
            kind: 7,
            content: b"a sender key message".to_vec(),
            group: Some([8; 32]),
        };
        assert_eq!(
            format!("{expected:?}"),
            "Contents { device: 3, kind: 7, .. }",
            "told without key, group or content"
        );
        assert_eq!(
            open(&crypto, &recipient, &sealed, 1_000),
            Ok(expected),
            "who sent it, the ciphertext inside, and the group it is to"
        );
        let mut corrupt = sealed.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(open(&crypto, &recipient, &corrupt, 1_000), Err(Error::Unopened), "changed on its way");
        assert_eq!(open(&crypto, &impostor, &sealed, 1_000), Err(Error::Unopened), "sealed to another key");
        let forgery = v1_of(&crypto, &impostor, &recipient.public, &content);
        assert_eq!(
            open(&crypto, &recipient, &forgery, 1_000),
            Err(Error::Unopened),
            "sealed under a key its certificate does not name"
        );
        let future = [&[0x33][..], &sealed[1..]].concat();
        assert_eq!(open(&crypto, &recipient, &future, 1_000), Err(Error::Malformed), "of a version not known");
    }

    pub(in crate::signal) fn content_for(crypto: &Standin, identity: &Pair, part: &[u8]) -> Vec<u8> {
        open_outer(crypto, identity, part).expect("opened").0
    }

    pub(in crate::signal) type Share = (ServiceId, Vec<(u8, u16)>, Vec<u8>);

    pub(in crate::signal) fn to_shares(sealed: &[u8]) -> Vec<Share> {
        let (&version, rest) = sealed.split_first().unwrap();
        assert_eq!(version, V2_SEND_VERSION);
        let (count, length) = crate::base::proto::read_varint(rest).unwrap();
        let mut rest = &rest[length..];
        let mut recipients = Vec::new();
        for _ in 0..count {
            let id = ServiceId::from_binary(&rest[..17]).unwrap();
            rest = &rest[17..];
            let mut devices = Vec::new();
            loop {
                let registration = u16::from_be_bytes([rest[1], rest[2]]);
                devices.push((rest[0], registration & !MORE_DEVICES));
                rest = &rest[3..];
                if registration & MORE_DEVICES == 0 {
                    break;
                }
            }
            recipients.push((id, devices, rest[..48].to_vec()));
            rest = &rest[48..];
        }
        let shared = rest;
        recipients.into_iter().map(|(id, devices, own)| (id, devices, [&[0x22][..], &own, shared].concat())).collect()
    }

    #[test]
    fn sealed_to_many() {
        let crypto = Standin::default();
        let pair = || Pair::new(&crypto).unwrap();
        let (sender, anna, boris) = (pair(), pair(), pair());
        let address = Address { service: ServiceId::Aci([1; 16]), device: 3 };
        let content = content_of(&crypto, &sender, &address, (7, Some(&[8; 32])), b"a sender key message");
        let (anna_id, boris_id) = (ServiceId::Aci([2; 16]), ServiceId::Pni([3; 16]));
        let recipients = [
            Recipient { id: anna_id, identity: anna.public, devices: alloc::vec![(1, 7), (4, 0x3FFF)] },
            Recipient { id: boris_id, identity: boris.public, devices: alloc::vec![(2, 9)] },
        ];
        let sealed = seal_many(&crypto, &sender, &recipients, &content).expect("sealed");
        let parts = to_shares(&sealed);
        assert_eq!(
            parts.iter().map(|(id, devices, _)| (*id, devices.clone())).collect::<Vec<_>>(),
            [(anna_id, alloc::vec![(1, 7), (4, 0x3FFF)]), (boris_id, alloc::vec![(2, 9)])],
            "each recipient by its id in 17 bytes, each device by its id and registration"
        );
        for ((_, _, part), key) in parts.iter().zip([&anna, &boris]) {
            let opened = open(&crypto, key, part, 1_000).expect("opened");
            assert_eq!(
                (opened.sender, opened.kind, &opened.content[..], opened.group),
                (address.service, 7, &b"a sender key message"[..], Some([8; 32])),
                "as the R1 opens version 2"
            );
        }
        assert_eq!(
            open(&crypto, &boris, &parts[0].2, 1_000).err(),
            Some(Error::Unopened),
            "a part opens for its recipient alone"
        );
        let too_high = Recipient { id: anna_id, identity: anna.public, devices: alloc::vec![(1, 0x4000)] };
        let nothing = Recipient { id: boris_id, identity: boris.public, devices: Vec::new() };
        assert_eq!(
            (seal_many(&crypto, &sender, &[too_high], &content), seal_many(&crypto, &sender, &[nothing], &content)),
            (Err(Some(anna_id)), Err(Some(boris_id))),
            "a registration of more than 14 bits, and a recipient with no device, refuse it"
        );
    }

    #[test]
    fn content_and_expiry() {
        let crypto = Standin::default();
        let (sender, recipient) = (Pair::new(&crypto).unwrap(), Pair::new(&crypto).unwrap());
        let address = Address { service: ServiceId::Aci([1; 16]), device: 3 };
        let certificate = certificate_of(&crypto, &sender, &address);
        assert_eq!(parse_expiry(&certificate), Some((1_000, false)), "it runs out at 1000 ms and names no number");
        let bytes = write_content(WHISPER, &certificate, b"ciphertext", RESENDABLE, Some(&[8; 32]));
        let no_hint = write_content(PLAINTEXT, &certificate, b"plain", 0, None);
        let no_hint = Message::parse(&no_hint).unwrap();
        assert_eq!((no_hint.varint(4), no_hint.bytes(5)), (None, None), "no hint, no group: neither written");
        let sealed = seal(&crypto, &sender, &recipient.public, &bytes).unwrap();
        let (plain, SenderBinding::Identity(identity)) = open_outer(&crypto, &recipient, &sealed).unwrap() else {
            panic!("version 1")
        };
        assert_eq!((plain, identity), (bytes.clone(), encode_key(&sender.public).to_vec()), "opened as sealed");
        let content = Message::parse(&bytes).unwrap();
        assert_eq!(
            (content.varint(1), content.bytes(3), content.varint(4), content.bytes(5)),
            (Some(2), Some(&b"ciphertext"[..]), Some(1), Some(&[8u8; 32][..])),
            "a SignalMessage, resendable, to the group"
        );
        let contents = open(&crypto, &recipient, &seal(&crypto, &sender, &recipient.public, &bytes).unwrap(), 1_000);
        assert_eq!(contents.map(|contents| (contents.kind, contents.group)), Ok((2, Some([8; 32]))));
    }

    #[test]
    fn kinds_both_ways() {
        use super::super::content::{DOUBLE_RATCHET, UNIDENTIFIED_SENDER};
        let kinds = [PREKEY_MESSAGE, DOUBLE_RATCHET, content::SENDER_KEY, content::PLAINTEXT];
        assert_eq!(kinds.map(kind_of), [1, 2, 7, 8], "as UnidentifiedSenderMessage numbers them");
        assert_eq!(kinds.map(|kind| envelope_kind(kind_of(kind))), kinds.map(Some), "and back");
        assert_eq!(kind_of(UNIDENTIFIED_SENDER), 8, "a ciphertext of no session goes as plaintext");
        assert_eq!([0, 3, 6, 9].map(envelope_kind), [None; 4], "a kind not known is none");
    }

    #[test]
    fn certificate_signatures() {
        use super::super::tests::Exact;
        let crypto = Exact::default();
        let (sender, recipient) = (Pair::new(&crypto).unwrap(), Pair::new(&crypto).unwrap());
        let address = Address { service: ServiceId::Aci([1; 16]), device: 3 };
        let open_under = |certificate: &[u8]| {
            let content = write_content(WHISPER, certificate, b"ciphertext", 0, None);
            open(&crypto, &recipient, &seal(&crypto, &sender, &recipient.public, &content).expect("sealed"), 1_000)
        };
        assert_eq!(
            open_under(&certificate_of(&crypto, &sender, &address)),
            Err(Error::Certificate),
            "a server certificate no trust root signed"
        );
        let production = unhex(PRODUCTION_CERTIFICATE).unwrap();
        assert_eq!(
            server_key(&crypto, &production).map(|key| crate::base::code::hex(&key)),
            Ok("bc9d1d290be964810dfa7e94856480a3f7060d004c9762c24c575a1522353a5a".into()),
            "Signal's own server certificate, which a trust root signed"
        );
        let fields = Writer::new()
            .bytes(7, &address.service.to_binary())
            .varint(2, u64::from(address.device))
            .bytes(4, &encode_key(&sender.public))
            .bytes(5, &production)
            .finish();
        let fields = [&fields[..], &[0x19], &1_000u64.to_le_bytes()].concat();
        let signature = curve::sign(&crypto, &sender.private, &[&fields], &[0; 64]);
        assert_eq!(
            open_under(&Writer::new().bytes(1, &fields).bytes(2, &signature).finish()),
            Err(Error::Certificate),
            "a sender certificate the server's key did not sign"
        );
    }

    #[test]
    fn session_under_sealed_sender() {
        let crypto = Standin::default();
        let pair = || Pair::new(&crypto).unwrap();
        // Bob's keys as the service hands them out, with which Alice begins.
        let (identities, signed, once) = ([pair(), pair()], pair(), pair());
        let (kyber_private, kyber) = crypto.lattice_pair().unwrap();
        let by_bob = |key: &[u8]| curve::sign(&crypto, &identities[1].private, &[key], &[0; 64]);
        let bundle = Bundle {
            registration: 7,
            identity: identities[1].public,
            pre_key: Some((5, once.public)),
            signed_pre_key: (2, signed.public, by_bob(&encode_key(&signed.public))),
            kyber_pre_key: (3, kyber.clone(), by_bob(&encode_kyber(&kyber))),
        };
        let keys = PreKeys { signed_pre_key: &signed, kyber_pre_key: &kyber_private, pre_key: Some(&once) };
        let addresses = [1, 2].map(|id| Address { service: ServiceId::Aci([id; 16]), device: u32::from(id) });
        let mut records = [Record::default(), Record::default()];
        records[0].promote(State::begin(&crypto, &identities[0], 11, &bundle, false, 0).unwrap());
        // What one sends the other sealed by the R1's own sealing: a ciphertext of `kind` (1 a PreKey message, 2 a SignalMessage) in version 1, as a message to one device goes, or in version 2, as one to many.
        let seal = |source: usize, kind: u8, ciphertext: &[u8], version: u8| {
            let (sender, recipient) = (&identities[source], &identities[1 - source].public);
            let content = content_of(&crypto, sender, &addresses[source], (kind, None), ciphertext);
            match version {
                1 => v1_of(&crypto, sender, recipient, &content),
                _ => v2_of(&crypto, sender, recipient, &content),
            }
        };
        let send = |records: &mut [Record; 2], source: usize, text: &str| {
            let (local, remote) = (&addresses[source], &addresses[1 - source]);
            records[source].seal(&crypto, &pad(text.as_bytes()), local, remote, 0).unwrap()
        };
        // What the other opens of a message, sealed on its way in a version, and the epoch of the ratchet it went in.
        let receive = |records: &mut [Record; 2], source: usize, message: &Ciphertext, version: u8| {
            let (target, kind) = (1 - source, if message.kind == PREKEY_MESSAGE { 1 } else { 2 });
            let contents =
                open(&crypto, &identities[target], &seal(source, kind, &message.bytes, version), 1_000).unwrap();
            assert_eq!((contents.sender, contents.identity), (addresses[source].service, identities[source].public));
            let (local, remote) = (&addresses[target], &addresses[source]);
            let (plain, inner) = if contents.kind == 1 {
                let prekey = PreKeyMessage::read(&contents.content).unwrap();
                if !records[target].promote_prekey_state(&prekey).unwrap() {
                    records[target]
                        .promote(State::accept(&crypto, &identities[target], 7, &keys, &prekey, false).unwrap());
                }
                (records[target].open_prekey(&crypto, &prekey, remote, local), prekey.message)
            } else {
                (records[target].open(&crypto, &contents.content, remote, local), contents.content)
            };
            (plain.map(|plain| unpad(&plain).unwrap().to_vec()), SignalMessage::read(&inner).unwrap().ratchet[1])
        };
        let mut epoch = 0;
        for round in 0..100 {
            let source = round % 2;
            let texts = (0..1 + (round + 1) % 3).map(|at| format!("{round}.{at}"));
            let mut messages: Vec<(String, Ciphertext)> =
                texts.map(|text| (text.clone(), send(&mut records, source, &text))).collect();
            // Some come last first, and some never.
            if round % 5 == 3 {
                messages.reverse();
            }
            if round % 11 == 6 {
                messages.remove(0);
            }
            // Two rounds in version 1, two in version 2, so that each side seals in both.
            let version = if round / 2 % 2 == 0 { 1 } else { 2 };
            for (text, message) in messages {
                let (plain, at) = receive(&mut records, source, &message, version);
                assert_eq!(plain, Ok(text.into_bytes()));
                epoch = epoch.max(at);
            }
            for record in &records {
                assert_eq!(Record::read(&record.write()).as_ref(), Some(record), "a record reads back as written");
            }
        }
        assert_eq!(epoch, 3, "the ratchet has agreed on two epochs' keys, one sent by each side");
        // A message changed on its way, or opened before, is refused, and changes nothing.
        let message = send(&mut records, 0, "again");
        let mut corrupt = message.clone();
        *corrupt.bytes.last_mut().unwrap() ^= 1;
        assert_eq!(receive(&mut records, 0, &corrupt, 1).0, Err(session::Error::Unopened));
        assert_eq!(receive(&mut records, 0, &message, 1).0, Ok(b"again".to_vec()));
        assert_eq!(receive(&mut records, 0, &message, 2).0, Err(session::Error::Duplicate));
    }
}

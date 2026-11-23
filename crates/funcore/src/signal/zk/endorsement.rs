// Group send endorsements, as zkcredential and zkgroup make them.

use super::super::Crypto;
use super::super::address::ServiceId;
use super::attributes::{KeyPair, m1};
use super::proof::{Points, Statement};
use super::ristretto::Element;
use super::{Reader, Scalar, Sho};
use crate::base::time::SECONDS_PER_DAY;
use alloc::string::String;
use alloc::vec::Vec;

const TAG: &[u8] = b"20240215_Signal_GroupSendEndorsement";
const WEIGHTS: &[u8] = b"Signal_ZKCredential_Endorsements_EndorsementResponse_ProofWeights_20240207";
const WEIGHT_BYTES: usize = 16;
const WEIGHT_MASK: u8 = 0x7F;
const RESERVED: u8 = 0;
const TOKEN_BYTES: usize = 16;
const MIN_LIFETIME: u64 = 2 * 60 * 60;
const MAX_LIFETIME: u64 = 7 * SECONDS_PER_DAY;
const MIN_REMAINING: u64 = 2 * 60 * 60;
const MAX_REMAINING: u64 = 2 * SECONDS_PER_DAY;

#[derive(Clone, PartialEq, Eq)]
pub(in crate::signal) struct Endorsements {
    pub(in crate::signal) expiration: u64,
    pub(in crate::signal) members: Vec<(ServiceId, [u8; 32])>,
}

impl core::fmt::Debug for Endorsements {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Endorsements")
            .field("expiration", &self.expiration)
            .field("members", &self.members.len())
            .finish_non_exhaustive()
    }
}

fn derive_key(crypto: &impl Crypto, root: &Element, expiration: u64) -> Element {
    let mut sho = Sho::new(crypto, TAG);
    sho.absorb(crypto, &expiration.to_be_bytes());
    root.add(&Element::BASE.times(&sho.scalar(crypto)))
}

fn weights(crypto: &impl Crypto, key: &Element, doubles: &[[u8; 32]], endorsements: &[[u8; 32]]) -> Vec<Scalar> {
    let key = key.encode();
    let mut parts: Vec<&[u8]> = alloc::vec![&key];
    parts.extend(doubles.iter().map(|point| &point[..]));
    parts.extend(endorsements.iter().map(|point| &point[..]));
    let mut sho = Sho::new(crypto, WEIGHTS);
    sho.absorb_all(crypto, &parts);
    let mut bytes = alloc::vec![0u8; doubles.len().saturating_sub(1) * WEIGHT_BYTES];
    sho.squeeze(crypto, &mut bytes);
    let weight = |chunk: &[u8; WEIGHT_BYTES]| {
        let mut bytes = [0u8; 32];
        bytes[..WEIGHT_BYTES].copy_from_slice(chunk);
        bytes[WEIGHT_BYTES - 1] &= WEIGHT_MASK;
        Scalar::read(&bytes).expect("below 2^127")
    };
    bytes.as_chunks::<WEIGHT_BYTES>().0.iter().map(weight).collect()
}

fn weighted_sum(points: &[Element], weights: &[Scalar]) -> Element {
    let (first, rest) = points.split_first().expect("a point");
    rest.iter().zip(weights).fold(*first, |sum, (point, weight)| sum.add(&point.times(weight)))
}

fn issuance() -> Statement {
    let mut statement = Statement::new();
    statement.add("weighted_sum(R)", &[("sk_prime", "weighted_sum(E)")]);
    statement.add("G", &[("sk_prime", "PK_prime")]);
    statement
}

fn member_points(crypto: &impl Crypto, members: &[ServiceId], key: &KeyPair) -> Vec<(ServiceId, Element, [u8; 32])> {
    let mut points: Vec<(ServiceId, Element, [u8; 32])> = members
        .iter()
        .map(|member| {
            let point = m1(crypto, member).times(&key.a1);
            (*member, point, point.add(&point).encode())
        })
        .collect();
    points.sort_by_key(|(.., double)| *double);
    points
}

pub(in crate::signal) fn parse_endorsements(
    crypto: &impl Crypto,
    response: &[u8],
    members: &[ServiceId],
    (key, root): (&KeyPair, &Element),
    now: u64,
) -> Option<Endorsements> {
    let mut reader = Reader(response);
    if reader.take(1)? != [RESERVED] {
        return None;
    }
    let count = usize::try_from(reader.number()?).ok()?;
    let endorsements: Vec<[u8; 32]> = (0..count).map(|_| reader.take(32)?.try_into().ok()).collect::<Option<_>>()?;
    let proof = reader.bytes()?;
    let expiration = reader.number()?;
    let remaining = expiration.saturating_sub(now);
    if !reader.0.is_empty()
        || !expiration.is_multiple_of(SECONDS_PER_DAY)
        || !(MIN_LIFETIME..=MAX_LIFETIME).contains(&remaining)
        || endorsements.len() != members.len()
        || members.is_empty()
    {
        return None;
    }
    let entries = member_points(crypto, members, key);
    let doubles: Vec<[u8; 32]> = entries.iter().map(|(_, _, double)| *double).collect();
    let pk_prime = derive_key(crypto, root, expiration);
    let weights = weights(crypto, &pk_prime, &doubles, &endorsements);
    let r_points: Vec<Element> = endorsements.iter().map(|point| Element::decode(point)).collect::<Option<_>>()?;
    let e_points: Vec<Element> = entries.iter().map(|(_, point, _)| *point).collect();
    let points: Points = [
        ("weighted_sum(E)", weighted_sum(&e_points, &weights)),
        ("weighted_sum(R)", weighted_sum(&r_points, &weights)),
        ("PK_prime", pk_prime),
    ]
    .map(|(name, point)| (String::from(name), point))
    .into();
    if !issuance().verify(crypto, proof, &points, &[]) {
        return None;
    }
    let members = entries.iter().map(|(member, ..)| *member).zip(endorsements).collect();
    Some(Endorsements { expiration, members })
}

impl Endorsements {
    pub(in crate::signal) fn is_usable(&self, now: u64) -> bool {
        let remaining = self.expiration.saturating_sub(now);
        remaining > MIN_REMAINING && remaining < MAX_REMAINING
    }

    pub(in crate::signal) fn has(&self, member: &ServiceId) -> bool {
        self.members.iter().any(|(id, _)| id == member)
    }

    pub(in crate::signal) fn token(
        &self,
        crypto: &impl Crypto,
        recipients: &[ServiceId],
        key: &KeyPair,
    ) -> Option<Vec<u8>> {
        let mut sum: Option<Element> = None;
        for member in recipients {
            let (_, point) = self.members.iter().find(|(id, _)| id == member)?;
            let point = Element::decode(point)?;
            sum = Some(sum.map_or(point, |sum| sum.add(&point)));
        }
        let point = sum?.times(&key.a1.invert());
        let hash = crypto.sha256(&[&point.encode()]);
        let mut bytes = alloc::vec![RESERVED];
        bytes.extend((TOKEN_BYTES as u64).to_le_bytes());
        bytes.extend(&hash[..TOKEN_BYTES]);
        bytes.extend(self.expiration.to_le_bytes());
        Some(bytes)
    }
}

#[cfg(test)]
pub(in crate::signal) mod tests {
    use super::super::super::group::Keys;
    use super::super::super::tests::Exact;
    use super::super::credential::tests::Server;
    use super::super::proof::Scalars;
    use super::*;

    pub(in crate::signal) fn issue(
        crypto: &impl Crypto,
        server: &Server,
        (members, key): (&[ServiceId], &KeyPair),
        expiration: u64,
        randomness: &[u8; 32],
    ) -> Vec<u8> {
        let mut sho = Sho::new(crypto, TAG);
        sho.absorb(crypto, &expiration.to_be_bytes());
        let t = sho.scalar(crypto);
        let unit = Scalar::read(&{
            let mut unit = [0u8; 32];
            unit[0] = 1;
            unit
        })
        .unwrap();
        let secret = server.endorsement_root.mul_add(unit, t).invert();
        let pk_prime = Element::BASE.times(&server.endorsement_root.mul_add(unit, t));
        let entries = member_points(crypto, members, key);
        let e_points: Vec<Element> = entries.iter().map(|(_, point, _)| *point).collect();
        let endorsements: Vec<[u8; 32]> = e_points.iter().map(|point| point.times(&secret).encode()).collect();
        let doubles: Vec<[u8; 32]> = entries.iter().map(|(_, _, double)| *double).collect();
        let weights = weights(crypto, &pk_prime, &doubles, &endorsements);
        let sum = weighted_sum(&e_points, &weights);
        let points: Points =
            [("weighted_sum(E)", sum), ("weighted_sum(R)", sum.times(&secret)), ("PK_prime", pk_prime)]
                .map(|(name, point)| (String::from(name), point))
                .into();
        let scalars: Scalars = [(String::from("sk_prime"), secret)].into();
        let proof = issuance().prove(crypto, &scalars, &points, &[], randomness).expect("a proof");
        let mut bytes = alloc::vec![RESERVED];
        bytes.extend((endorsements.len() as u64).to_le_bytes());
        endorsements.iter().for_each(|point| bytes.extend(point));
        bytes.extend((proof.len() as u64).to_le_bytes());
        bytes.extend(proof);
        bytes.extend(expiration.to_le_bytes());
        bytes
    }

    fn expected(crypto: &impl Crypto, server: &Server, recipients: &[ServiceId], expiration: u64) -> Vec<u8> {
        let mut sho = Sho::new(crypto, TAG);
        sho.absorb(crypto, &expiration.to_be_bytes());
        let unit = Scalar::read(&{
            let mut unit = [0u8; 32];
            unit[0] = 1;
            unit
        })
        .unwrap();
        let secret = server.endorsement_root.mul_add(unit, sho.scalar(crypto)).invert();
        let sum = recipients.iter().fold(Element::IDENTITY, |sum, member| sum.add(&m1(crypto, member)));
        crypto.sha256(&[&sum.times(&secret).encode()])[..TOKEN_BYTES].to_vec()
    }

    #[test]
    fn endorsements_and_tokens() {
        let crypto = Exact::default();
        let server = Server::new(&crypto, &[0x44; 32]);
        let key = Keys::new(&crypto, &[4; 32]).uid;
        let (own, moxie, brian) = (ServiceId::Aci([4; 16]), ServiceId::Aci([0xE3; 16]), ServiceId::Aci([0x8C; 16]));
        let members = [own, moxie, brian];
        let now = 1_681_344_000;
        let expiration = now + SECONDS_PER_DAY;
        let response = issue(&crypto, &server, (&members, &key), expiration, &[0x45; 32]);
        let keys = (&key, &server.issuer.endorsement_root);
        let endorsements =
            parse_endorsements(&crypto, &response, &[brian, own, moxie], keys, now).expect("endorsements");
        assert_eq!((endorsements.expiration, endorsements.members.len()), (expiration, 3), "the members in any order");
        assert!(endorsements.has(&moxie) && !endorsements.has(&ServiceId::Aci([1; 16])));
        let token = endorsements.token(&crypto, &[moxie, brian], &key).expect("a token");
        assert_eq!(token.len(), 33, "a reserved byte, the count, 16 bytes of hash and the expiration");
        assert_eq!(
            (token[0], &token[1..9], &token[9..25], &token[25..]),
            (
                0,
                &16u64.to_le_bytes()[..],
                &expected(&crypto, &server, &[moxie, brian], expiration)[..],
                &expiration.to_le_bytes()[..]
            ),
            "the token the chat service makes of the two of them"
        );
        assert_eq!(endorsements.token(&crypto, &[brian, moxie], &key), Some(token), "a set: any order");
        let single = endorsements.token(&crypto, &[moxie], &key).unwrap();
        assert_eq!(&single[9..25], &expected(&crypto, &server, &[moxie], expiration)[..]);
        assert_eq!(
            endorsements.token(&crypto, &[moxie, ServiceId::Aci([1; 16])], &key),
            None,
            "one without an endorsement"
        );
        assert_eq!(endorsements.token(&crypto, &[], &key), None);
        let other = Server::new(&crypto, &[9; 32]);
        let invalid = [
            parse_endorsements(&crypto, &response, &[own, moxie], keys, now),
            parse_endorsements(&crypto, &response, &[own, moxie, ServiceId::Aci([1; 16])], keys, now),
            parse_endorsements(
                &crypto,
                &response,
                &members,
                (&Keys::new(&crypto, &[5; 32]).uid, &server.issuer.endorsement_root),
                now,
            ),
            parse_endorsements(&crypto, &response, &members, (&key, &other.issuer.endorsement_root), now),
            parse_endorsements(&crypto, &[&response[..], &[0]].concat(), &members, keys, now),
            parse_endorsements(&crypto, &response, &members, keys, expiration - MIN_LIFETIME + 1),
            parse_endorsements(&crypto, &response, &members, keys, expiration - MAX_LIFETIME - 1),
        ];
        assert!(
            invalid.iter().all(Option::is_none),
            "a member short, another member, another group, another server, a byte more, too soon, too late"
        );
        let mut corrupt = response.clone();
        corrupt[9] ^= 1;
        assert!(parse_endorsements(&crypto, &corrupt, &members, keys, now).is_none(), "an endorsement changed");
        let unaligned = issue(&crypto, &server, (&members, &key), expiration + 1, &[0x45; 32]);
        assert!(
            parse_endorsements(&crypto, &unaligned, &members, keys, now).is_none(),
            "running out at no day's start"
        );
    }

    #[test]
    fn usable_window() {
        let endorsements = Endorsements { expiration: 10 * SECONDS_PER_DAY, members: Vec::new() };
        let at = |remaining: u64| endorsements.is_usable(endorsements.expiration - remaining);
        assert_eq!(
            [at(MIN_REMAINING), at(MIN_REMAINING + 1), at(MAX_REMAINING - 1), at(MAX_REMAINING)],
            [false, true, true, false],
            "as Signal-Desktop takes them"
        );
        assert!(!endorsements.is_usable(endorsements.expiration + 1), "run out");
    }
}

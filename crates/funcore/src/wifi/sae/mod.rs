//! SAE (IEEE 802.11-2020, 12.4) on P-256 for a station: commit and confirm bodies, their frames and the keys.

pub mod point;

use crate::base::bytes::le16;
use crate::base::bytes::same;
use crate::crypto::Primitives;
use crate::wifi::wpa::PMK;
use alloc::vec::Vec;

/// The bytes of a number of the curve, big end first.
pub const NUMBER: usize = 32;
/// The bytes of a point: its two coordinates.
pub const POINT: usize = 64;
/// The least number that counts as a scalar.
pub const LEAST_SCALAR: u8 = 2;
const HASH: usize = 32;
const PMKID: usize = 16;
pub(super) const ALGORITHM: u16 = 3;
const COMMIT: u16 = 1;
const CONFIRM: u16 = 2;
const GROUP: u16 = 19;
const SUCCESS: u16 = 0;
const HASH_TO_ELEMENT: u16 = 126;
const TOKEN_REQUIRED: u16 = 76;
const HEAD: usize = 6;
const ELEMENT_EXTENSION: u8 = 255;
const TOKEN_CONTAINER: u8 = 93;
const KEYS: &[u8] = b"SAE KCK and PMK";
const KEY_BITS: u16 = 512;
pub(super) const FRAME_KIND: u16 = 0x00B0;
const KIND_BYTE: u8 = FRAME_KIND as u8;
const FRAME_HEAD: usize = 24;
const RECEIVER_AT: usize = 4;
const SENDER_AT: usize = 10;
const BSSID_AT: usize = 16;

pub(super) fn frame(station: &[u8; 6], access: &[u8; 6], body: &[u8]) -> Vec<u8> {
    let mut frame = alloc::vec![0u8; FRAME_HEAD];
    frame[..2].copy_from_slice(&FRAME_KIND.to_le_bytes());
    frame[RECEIVER_AT..RECEIVER_AT + 6].copy_from_slice(access);
    frame[SENDER_AT..SENDER_AT + 6].copy_from_slice(station);
    frame[BSSID_AT..BSSID_AT + 6].copy_from_slice(access);
    frame.extend(body);
    frame
}

/// A point as the standard writes it: its first coordinate, then its second.
pub fn to_point(x: &[u8; NUMBER], y: &[u8; NUMBER]) -> [u8; POINT] {
    let mut point = [0u8; POINT];
    point[..NUMBER].copy_from_slice(x);
    point[NUMBER..].copy_from_slice(y);
    point
}

fn mask(is_set: bool) -> u8 {
    u8::from(is_set).wrapping_neg()
}

fn select(mask: u8, a: u8, b: u8) -> u8 {
    (a & mask) | (b & !mask)
}

fn select_number(mask: u8, a: &[u8; NUMBER], b: &[u8; NUMBER]) -> [u8; NUMBER] {
    core::array::from_fn(|at| select(mask, a[at], b[at]))
}

fn order_addresses(a: &[u8; 6], b: &[u8; 6]) -> [u8; 12] {
    let (greater, smaller) = if a >= b { (a, b) } else { (b, a) };
    core::array::from_fn(|at| if at < 6 { greater[at] } else { smaller[at - 6] })
}

pub(super) fn parse_frame<'a>(frame: &'a [u8], station: &[u8; 6], access: &[u8; 6]) -> Option<&'a [u8]> {
    let has_address = |at: usize, address: &[u8; 6]| frame.get(at..at + 6) == Some(&address[..]);
    (frame.first() == Some(&KIND_BYTE)
        && has_address(RECEIVER_AT, station)
        && has_address(SENDER_AT, access)
        && has_address(BSSID_AT, access))
    .then(|| frame.get(FRAME_HEAD..))
    .flatten()
}

/// The arithmetic of the curve's group SAE needs, done by a library.
pub trait Curve {
    // A number greater than 1 and less than the curve's order; none if there are no random bytes.
    fn random(&self) -> Option<[u8; NUMBER]>;
    // Modulo the order.
    fn add_scalars(&self, a: &[u8; NUMBER], b: &[u8; NUMBER]) -> Option<[u8; NUMBER]>;
    fn order(&self) -> Option<[u8; NUMBER]>;
    // None for what is no point of the curve, and where the result is none.
    fn multiply_point(&self, point: &[u8; POINT], number: &[u8; NUMBER]) -> Option<[u8; POINT]>;
    // None where either is no point of the curve or the sum is none.
    fn add_points(&self, a: &[u8; POINT], b: &[u8; POINT]) -> Option<[u8; POINT]>;
    fn negate(&self, point: &[u8; POINT]) -> Option<[u8; POINT]>;
}

/// True for a number from `LEAST_SCALAR` up to less than the curve's order.
pub fn is_scalar(curve: &impl Curve, number: &[u8; NUMBER]) -> bool {
    let mut least = [0; NUMBER];
    least[NUMBER - 1] = LEAST_SCALAR;
    curve.order().is_some_and(|order| *number >= least && *number < order)
}

/// What a frame that was taken has the station do.
#[derive(Clone, PartialEq, Eq)]
pub enum Step {
    // Send this as the body of an authentication frame.
    Send(Vec<u8>),
    // Both sides have proved the password: the key for the handshake that follows, and its name.
    Agreed { pmk: [u8; PMK], pmkid: [u8; PMKID] },
}

impl core::fmt::Debug for Step {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Step::Send(body) => f.debug_tuple("Send").field(body).finish(),
            Step::Agreed { pmkid, .. } => f.debug_struct("Agreed").field("name", pmkid).finish_non_exhaustive(),
        }
    }
}

/// Why a frame was not taken, or the exchange has failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    // No frame of SAE, or cut short.
    NotSae,
    // The access point refuses, with this status.
    Status(u16),
    // Another curve than the one asked for.
    Group,
    // A number or a point that is none, or the station's own sent back to it.
    Value,
    // A frame for a moment of the exchange that is not now.
    OutOfTurn,
    // The access point's proof is not that of the password: a wrong password shows here.
    Proof,
    // The library failed.
    Arithmetic,
}

impl Error {
    pub(super) const fn is_fatal(self) -> bool {
        !matches!(self, Error::NotSae | Error::OutOfTurn)
    }
}

struct Keys {
    kck: [u8; HASH],
    pmk: [u8; PMK],
    pmkid: [u8; PMKID],
    peer: ([u8; NUMBER], [u8; POINT]),
}

/// The station's side of one exchange.
pub struct Station {
    is_hashed: bool,
    pwe: [u8; POINT],
    private: [u8; NUMBER],
    scalar: [u8; NUMBER],
    element: [u8; POINT],
    keys: Option<Keys>,
    is_agreed: bool,
}

impl Station {
    /// Begins an exchange over the password's point, hashed to or found by looping: the station and its commit body, or none if the library fails.
    pub fn begin(curve: &impl Curve, pwe: [u8; POINT], is_hashed: bool) -> Option<(Station, Vec<u8>)> {
        // The station commits to the sum of two random numbers and to the opposite of the password's point taken the second of them times; the first stays its own.
        let (private, mask) = (curve.random()?, curve.random()?);
        let scalar = curve.add_scalars(&private, &mask).filter(|scalar| is_scalar(curve, scalar))?;
        let element = curve.negate(&curve.multiply_point(&pwe, &mask)?)?;
        let station = Station { is_hashed, pwe, private, scalar, element, keys: None, is_agreed: false };
        let commit = station.commit(&[]);
        Some((station, commit))
    }

    fn status(&self) -> u16 {
        if self.is_hashed { HASH_TO_ELEMENT } else { SUCCESS }
    }

    fn commit(&self, token: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(HEAD + 2 + NUMBER + POINT + token.len() + 3);
        for field in [ALGORITHM, COMMIT, self.status(), GROUP] {
            body.extend(field.to_le_bytes());
        }
        // A token goes before the numbers as it came; where the point was hashed to it goes after them, in an element of its own.
        if !self.is_hashed {
            body.extend(token);
        }
        body.extend(self.scalar);
        body.extend(self.element);
        if self.is_hashed && !token.is_empty() {
            body.extend([ELEMENT_EXTENSION, 1 + token.len() as u8, TOKEN_CONTAINER]);
            body.extend(token);
        }
        body
    }

    fn confirm(
        crypto: &impl Primitives,
        kck: &[u8; HASH],
        count: u16,
        first: (&[u8; NUMBER], &[u8; POINT]),
        second: (&[u8; NUMBER], &[u8; POINT]),
    ) -> [u8; HASH] {
        crypto.hmac_sha256(kck, &[&count.to_le_bytes(), first.0, first.1, second.0, second.1])
    }

    /// Takes the body of an authentication frame from the access point and returns the steps it calls for.
    pub fn take(&mut self, crypto: &impl Primitives, curve: &impl Curve, body: &[u8]) -> Result<Vec<Step>, Error> {
        let (Some(ALGORITHM), Some(turn), Some(status)) = (le16(body, 0), le16(body, 2), le16(body, 4)) else {
            return Err(Error::NotSae);
        };
        let rest = &body[HEAD..];
        match (turn, status) {
            (COMMIT, TOKEN_REQUIRED) if self.keys.is_none() => {
                if le16(rest, 0) != Some(GROUP) {
                    return Err(Error::Group);
                }
                let token = match &rest[2..] {
                    [ELEMENT_EXTENSION, length, TOKEN_CONTAINER, token @ ..]
                        if self.is_hashed && usize::from(*length) == 1 + token.len() =>
                    {
                        token
                    }
                    token if !self.is_hashed => token,
                    _ => return Err(Error::NotSae),
                };
                Ok(alloc::vec![Step::Send(self.commit(token))])
            }
            (COMMIT, status) if status == self.status() => {
                if self.keys.is_some() {
                    return Err(Error::OutOfTurn);
                }
                if le16(rest, 0) != Some(GROUP) {
                    return Err(Error::Group);
                }
                let (Some(scalar), Some(element)) = (rest.get(2..2 + NUMBER), rest.get(2 + NUMBER..2 + NUMBER + POINT))
                else {
                    return Err(Error::NotSae);
                };
                let peer: ([u8; NUMBER], [u8; POINT]) =
                    (scalar.try_into().map_err(|_| Error::NotSae)?, element.try_into().map_err(|_| Error::NotSae)?);
                if !is_scalar(curve, &peer.0) || peer == (self.scalar, self.element) {
                    return Err(Error::Value);
                }
                // The secret both come to: the password's point times the peer's number, plus the peer's point, all times the station's own number; its first coordinate counts.
                let shared = curve
                    .multiply_point(&self.pwe, &peer.0)
                    .and_then(|point| curve.add_points(&point, &peer.1))
                    .and_then(|point| curve.multiply_point(&point, &self.private))
                    .ok_or(Error::Value)?;
                let seed = crypto.hmac_sha256(&[0; HASH], &[&shared[..NUMBER]]);
                let sum = curve.add_scalars(&self.scalar, &peer.0).ok_or(Error::Arithmetic)?;
                let derive = |count: u16| {
                    crypto.hmac_sha256(&seed, &[&count.to_le_bytes(), KEYS, &sum, &KEY_BITS.to_le_bytes()])
                };
                let keys = Keys {
                    kck: derive(1),
                    pmk: derive(2),
                    pmkid: sum[..PMKID].try_into().map_err(|_| Error::Arithmetic)?,
                    peer,
                };
                let confirm =
                    Station::confirm(crypto, &keys.kck, 1, (&self.scalar, &self.element), (&keys.peer.0, &keys.peer.1));
                self.keys = Some(keys);
                let mut body = Vec::with_capacity(HEAD + 2 + confirm.len());
                for field in [ALGORITHM, CONFIRM, SUCCESS, 1] {
                    body.extend(field.to_le_bytes());
                }
                body.extend(confirm);
                Ok(alloc::vec![Step::Send(body)])
            }
            (CONFIRM, SUCCESS) => {
                let Some(keys) = &self.keys else {
                    return Err(Error::OutOfTurn);
                };
                let (Some(count), Some(confirm)) = (le16(rest, 0), rest.get(2..2 + HASH)) else {
                    return Err(Error::NotSae);
                };
                let expected = Station::confirm(
                    crypto,
                    &keys.kck,
                    count,
                    (&keys.peer.0, &keys.peer.1),
                    (&self.scalar, &self.element),
                );
                if !same(&expected, confirm) {
                    return Err(Error::Proof);
                }
                // A proof that comes a second time is as good as the first and changes nothing.
                let is_first = !core::mem::replace(&mut self.is_agreed, true);
                Ok(if is_first { alloc::vec![Step::Agreed { pmk: keys.pmk, pmkid: keys.pmkid }] } else { Vec::new() })
            }
            (COMMIT | CONFIRM, status) => Err(Error::Status(status)),
            _ => Err(Error::NotSae),
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::wifi::wpa::tests::Sums;
    use alloc::vec;
    use core::cell::Cell;

    pub(crate) struct Toy(pub(crate) Cell<u64>);

    const ORDER: u64 = 4_294_967_291;

    fn number(bytes: &[u8]) -> u64 {
        u64::from_be_bytes(bytes[NUMBER - 8..NUMBER].try_into().unwrap()) % ORDER
    }

    pub(crate) fn to_bytes<const N: usize>(value: u64) -> [u8; N] {
        let mut bytes = [0u8; N];
        bytes[NUMBER - 8..NUMBER].copy_from_slice(&value.to_be_bytes());
        bytes
    }

    impl Curve for Toy {
        fn random(&self) -> Option<[u8; NUMBER]> {
            self.0.set(self.0.get().wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407));
            Some(to_bytes(2 + (self.0.get() >> 16) % (ORDER - 2)))
        }

        fn add_scalars(&self, a: &[u8; NUMBER], b: &[u8; NUMBER]) -> Option<[u8; NUMBER]> {
            Some(to_bytes((number(a) + number(b)) % ORDER))
        }

        fn order(&self) -> Option<[u8; NUMBER]> {
            Some(to_bytes(ORDER))
        }

        fn multiply_point(&self, point: &[u8; POINT], factor: &[u8; NUMBER]) -> Option<[u8; POINT]> {
            Some(to_bytes((u128::from(number(point)) * u128::from(number(factor)) % u128::from(ORDER)) as u64))
                .filter(|point| number(point) != 0)
        }

        fn add_points(&self, a: &[u8; POINT], b: &[u8; POINT]) -> Option<[u8; POINT]> {
            Some(to_bytes((number(a) + number(b)) % ORDER)).filter(|point| number(point) != 0)
        }

        fn negate(&self, point: &[u8; POINT]) -> Option<[u8; POINT]> {
            Some(to_bytes((ORDER - number(point)) % ORDER))
        }
    }

    pub(crate) fn send_body(steps: &[Step]) -> Vec<u8> {
        match steps {
            [Step::Send(body)] => body.clone(),
            other => panic!("no frame to send and nothing else: {other:?}"),
        }
    }

    pub(crate) fn frame_from(access: &[u8; 6], station: &[u8; 6], body: &[u8]) -> Vec<u8> {
        let mut bytes = frame(access, station, body);
        bytes[BSSID_AT..BSSID_AT + 6].copy_from_slice(access);
        bytes
    }

    pub(crate) fn body_of(frame: &[u8]) -> &[u8] {
        &frame[FRAME_HEAD..]
    }

    fn exchange(
        pwe_a: [u8; POINT],
        pwe_b: [u8; POINT],
        is_hashed: bool,
    ) -> (Result<Vec<Step>, Error>, Result<Vec<Step>, Error>) {
        let (curve_a, curve_b) = (Toy(Cell::new(1)), Toy(Cell::new(2)));
        let ((mut a, commit_a), (mut b, commit_b)) =
            (Station::begin(&curve_a, pwe_a, is_hashed).unwrap(), Station::begin(&curve_b, pwe_b, is_hashed).unwrap());
        let (confirm_a, confirm_b) = (
            send_body(&a.take(&Sums, &curve_a, &commit_b).unwrap()),
            send_body(&b.take(&Sums, &curve_b, &commit_a).unwrap()),
        );
        (a.take(&Sums, &curve_a, &confirm_b), b.take(&Sums, &curve_b, &confirm_a))
    }

    #[test]
    fn shared_key_and_wrong_password() {
        let (password, another) = (to_bytes::<POINT>(123_456_789), to_bytes::<POINT>(987_654_321));
        for is_hashed in [false, true] {
            let (a, b) = exchange(password, password, is_hashed);
            let (Ok(a), Ok(b)) = (a, b) else { panic!("both know the password") };
            assert!(matches!(a[..], [Step::Agreed { .. }]) && a == b, "the same key and the same name on both sides");
            let Step::Agreed { pmk, .. } = &a[0] else { unreachable!() };
            let text = alloc::format!("{:?}", a[0]);
            assert!(
                text.starts_with("Agreed { name: [")
                    && text.ends_with(", .. }")
                    && !text.contains(&alloc::format!("{pmk:?}")),
                "and it is told by its name alone: {text}"
            );
            assert_eq!(
                exchange(password, another, is_hashed),
                (Err(Error::Proof), Err(Error::Proof)),
                "with another password the proofs are not each other's"
            );
        }
    }

    #[test]
    fn frame_layout() {
        let curve = Toy(Cell::new(7));
        let (mut station, commit) = Station::begin(&curve, to_bytes(55), false).unwrap();
        assert_eq!(
            (&commit[..8], commit.len()),
            (&[3, 0, 1, 0, 0, 0, 19, 0][..], 8 + 32 + 64),
            "SAE, the first turn, accepted, curve 19, then a number and a point"
        );
        let (_, hashed) = Station::begin(&curve, to_bytes(55), true).unwrap();
        assert_eq!(hashed[4..6], [126, 0], "a point that was hashed to says so in the status");
        assert_eq!(
            station
                .take(&Sums, &curve, &[3, 0, 1, 0, 76, 0, 19, 0, 0xAA, 0xBB])
                .map(|steps| send_body(&steps)[..10].to_vec()),
            Ok(vec![3, 0, 1, 0, 0, 0, 19, 0, 0xAA, 0xBB]),
            "a token the access point wants to see again goes before the numbers"
        );
        let (mut other, _) = Station::begin(&curve, to_bytes(55), true).unwrap();
        let repeat =
            send_body(&other.take(&Sums, &curve, &[3, 0, 1, 0, 76, 0, 19, 0, 255, 3, 93, 0xAA, 0xBB]).unwrap());
        assert_eq!(
            (repeat.len(), &repeat[104..]),
            (104 + 5, &[255, 3, 93, 0xAA, 0xBB][..]),
            "and after them in an element of its own where the point was hashed to"
        );
        let (mut peer, peer_commit) = Station::begin(&Toy(Cell::new(9)), to_bytes(55), false).unwrap();
        let confirm = send_body(&station.take(&Sums, &curve, &peer_commit).unwrap());
        assert_eq!(
            (&confirm[..8], confirm.len()),
            (&[3, 0, 2, 0, 0, 0, 1, 0][..], 8 + 32),
            "the second turn: a count and the proof"
        );
        assert_eq!(station.take(&Sums, &curve, &peer_commit), Err(Error::OutOfTurn), "a second commit is not taken");
        let proof = send_body(&peer.take(&Sums, &Toy(Cell::new(9)), &commit).unwrap());
        assert_eq!(station.take(&Sums, &curve, &proof).map(|steps| steps.len()), Ok(1));
        assert_eq!(station.take(&Sums, &curve, &proof), Ok(Vec::new()), "a proof that comes again changes nothing");
    }

    #[test]
    fn frame_body() {
        let (station, access, other) = ([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 9], [2, 0, 0, 0, 0, 7]);
        let outgoing = frame(&station, &access, &[3, 0, 1, 0]);
        assert_eq!(
            outgoing,
            [0xB0, 0, 0, 0, 2, 0, 0, 0, 0, 9, 2, 0, 0, 0, 0, 1, 2, 0, 0, 0, 0, 9, 0, 0, 3, 0, 1, 0],
            "the kind, no time, the access point, the station, the access point's network, no count, the body"
        );
        let mut answer = frame_from(&access, &station, &[3, 0, 1, 0, 0, 0]);
        assert_eq!(parse_frame(&answer, &station, &access), Some(&[3, 0, 1, 0, 0, 0][..]));
        answer[1] = 0x08;
        assert_eq!(
            parse_frame(&answer, &station, &access),
            Some(&[3, 0, 1, 0, 0, 0][..]),
            "a frame that was sent again is as good"
        );
        assert_eq!(
            (
                parse_frame(&answer, &other, &access),
                parse_frame(&answer, &station, &other),
                parse_frame(&outgoing, &station, &access)
            ),
            (None, None, None),
            "not one for another station, from another access point, or the station's own"
        );
        assert_eq!(
            (parse_frame(&answer[..23], &station, &access), parse_frame(&answer[..24], &station, &access)),
            (None, Some(&[][..])),
            "nor one cut short in its head"
        );
        answer[0] = 0xD0;
        assert_eq!(parse_frame(&answer, &station, &access), None, "nor a frame of another kind");
    }

    #[test]
    fn point_and_address_order() {
        let (x, y) = ([1u8; NUMBER], [2u8; NUMBER]);
        let point = to_point(&x, &y);
        assert_eq!((&point[..NUMBER], &point[NUMBER..]), (&x[..], &y[..]));
        let (low, high) = ([2, 0, 0, 0, 0, 1], [2, 0, 0, 0, 0, 9]);
        assert_eq!(order_addresses(&low, &high), order_addresses(&high, &low), "whichever is named first");
        assert_eq!(order_addresses(&low, &high)[..], [high, low].concat()[..], "the greater first");
        assert_eq!(order_addresses(&low, &low)[..], [low, low].concat()[..]);
    }

    #[test]
    fn select_by_mask() {
        assert_eq!((mask(true), mask(false)), (0xFF, 0));
        assert_eq!((select(mask(true), 0x5A, 0xA5), select(mask(false), 0x5A, 0xA5)), (0x5A, 0xA5));
        let (a, b) = ([1u8; NUMBER], [2u8; NUMBER]);
        assert_eq!((select_number(mask(true), &a, &b), select_number(mask(false), &a, &b)), (a, b));
    }

    #[test]
    fn refused_frames() {
        let curve = Toy(Cell::new(7));
        let (mut station, commit) = Station::begin(&curve, to_bytes(55), false).unwrap();
        let (mut other, mut small) = (commit.clone(), commit.clone());
        other[6] = 20;
        small[8..40].copy_from_slice(&to_bytes::<NUMBER>(1));
        for (body, expected, reason) in [
            (&[0, 0, 2, 0, 0, 0][..], Error::NotSae, "another algorithm"),
            (&[3, 0, 1], Error::NotSae, "cut short"),
            (&[3, 0, 1, 0, 77, 0, 19, 0], Error::Status(77), "the access point does not speak the curve"),
            (&[3, 0, 1, 0, 126, 0, 19, 0], Error::Status(126), "nor a point found another way than the station's"),
            (&[3, 0, 2, 0, 0, 0, 1, 0], Error::OutOfTurn, "a proof before any commit"),
            (&commit, Error::Value, "the station's own commit sent back to it"),
            (&other, Error::Group, "another curve"),
            (&small, Error::Value, "a number that is none"),
            (&commit[..60], Error::NotSae, "a commit cut short"),
            (&[3, 0, 3, 0, 0, 0], Error::NotSae, "a turn there is none of"),
        ] {
            assert_eq!(station.take(&Sums, &curve, body), Err(expected), "{reason}");
        }
        assert_eq!(
            [Error::NotSae, Error::OutOfTurn].map(Error::is_fatal),
            [false; 2],
            "what could be from anybody is passed over"
        );
        assert!(
            [Error::Status(77), Error::Group, Error::Value, Error::Proof, Error::Arithmetic]
                .iter()
                .all(|error| error.is_fatal()),
            "and all else ends the exchange"
        );
    }
}

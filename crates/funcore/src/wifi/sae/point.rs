//! The point SAE proves a password over (IEEE 802.11-2020, 12.4.4.2), found by looping or hashed to, every choice between candidates made without a branch.

use super::{Curve, HASH, NUMBER, POINT, mask, order_addresses, select, select_number, to_point};
use crate::crypto::Primitives;
use alloc::vec::Vec;

const ROUNDS: u8 = 40;
const HUNTING: &[u8] = b"SAE Hunting and Pecking";
const CANDIDATE_BITS: u16 = 256;
const FIRST: &[u8] = b"SAE Hash to Element u1 P1";
const SECOND: &[u8] = b"SAE Hash to Element u2 P2";
/// The bytes of the map's input.
pub const MAP_BYTES: usize = 48;
const MAP_NUMBER: u64 = 10;

/// The prime field the curve is over, with the `a` and `b` of its equation, done by a library in a time that tells nothing of the numbers.
pub trait Field: Curve {
    type Number;
    // Big end first, as it is.
    fn read_number(&self, bytes: &[u8]) -> Option<Self::Number>;
    // The same, brought under the prime.
    fn reduce(&self, bytes: &[u8]) -> Option<Self::Number>;
    fn make_number(&self, value: u64) -> Option<Self::Number>;
    // Big end first, as many as a number of the curve has; none if it takes more.
    fn to_bytes(&self, number: &Self::Number) -> Option<[u8; NUMBER]>;
    // Under the prime, as the two that follow.
    fn add(&self, a: &Self::Number, b: &Self::Number) -> Option<Self::Number>;
    fn subtract(&self, a: &Self::Number, b: &Self::Number) -> Option<Self::Number>;
    fn multiply(&self, a: &Self::Number, b: &Self::Number) -> Option<Self::Number>;
    fn is_square(&self, value: &Self::Number) -> Option<bool>;
    // A root of a number that is a square.
    fn sqrt(&self, value: &Self::Number) -> Option<Self::Number>;
    // The inverse of a number, and zero for zero.
    fn invert(&self, value: &Self::Number) -> Option<Self::Number>;
    fn is_zero(&self, value: &Self::Number) -> bool;
    fn is_under_prime(&self, value: &Self::Number) -> bool;
    fn prime(&self) -> Option<[u8; NUMBER]>;
    fn a(&self) -> &Self::Number;
    fn b(&self) -> &Self::Number;
    // A hash brought into the range from 1 to one less than the number of the curve's points.
    fn to_range(&self, hash: &[u8; HASH]) -> Option<[u8; NUMBER]>;
}

fn curve_side<F: Field>(field: &F, x: &F::Number) -> Option<F::Number> {
    let cube = field.multiply(&field.multiply(x, x)?, x)?;
    field.add(&field.add(&cube, &field.multiply(field.a(), x)?)?, field.b())
}

fn point_with_parity<F: Field>(field: &F, x: &[u8; NUMBER], odd: u8) -> Option<[u8; POINT]> {
    let root = field.sqrt(&curve_side(field, &field.read_number(x)?)?)?;
    let (y, other) = (field.to_bytes(&root)?, field.to_bytes(&field.subtract(&field.make_number(0)?, &root)?)?);
    Some(to_point(x, &select_number(mask(y[NUMBER - 1] & 1 == odd), &y, &other)))
}

/// The password's point found by looping (IEEE 802.11-2020, 12.4.4.2.2): the first candidate that is a point's first coordinate, every round doing the same work.
pub fn hunt_and_peck<F: Field>(
    crypto: &impl Primitives,
    field: &F,
    password: &[u8],
    a: &[u8; 6],
    b: &[u8; 6],
) -> Option<[u8; POINT]> {
    let (addresses, prime) = (order_addresses(a, b), field.prime()?);
    // Once the point is found the rounds go on over random bytes in the password's place.
    let mut decoy = alloc::vec![0u8; password.len()];
    if !crypto.random(&mut decoy) {
        return None;
    }
    let (mut found_mask, mut x, mut odd, mut count) = (0u8, [0u8; NUMBER], 0u8, 0u8);
    while count < ROUNDS || found_mask == 0 {
        count = count.checked_add(1)?;
        let base: Vec<u8> =
            password.iter().zip(&decoy).map(|(byte, random)| select(found_mask, *random, *byte)).collect();
        let seed = crypto.hmac_sha256(&addresses, &[&base, &[count]]);
        let candidate =
            crypto.hmac_sha256(&seed, &[&1u16.to_le_bytes(), HUNTING, &prime, &CANDIDATE_BITS.to_le_bytes()]);
        let number = field.read_number(&candidate)?;
        let take_mask = (u8::from(field.is_under_prime(&number))
            & u8::from(field.is_square(&curve_side(field, &number)?)?))
        .wrapping_neg()
            & !found_mask;
        x = select_number(take_mask, &candidate, &x);
        odd = select(take_mask, seed[NUMBER - 1] & 1, odd);
        found_mask |= take_mask;
    }
    // Of the two points with that coordinate, the one whose second is odd where the seed was.
    point_with_parity(field, &x, odd)
}

/// The point the standard's map takes a number to (IEEE 802.11-2020, 12.4.4.2.3, which is RFC 9380, 6.6.2), both candidates worked out whichever is taken.
pub fn map_to_curve<F: Field>(field: &F, input: &[u8]) -> Option<[u8; POINT]> {
    let (zero, one, u) = (field.make_number(0)?, field.make_number(1)?, field.reduce(input)?);
    let z = field.subtract(&zero, &field.make_number(MAP_NUMBER)?)?;
    let zu2 = field.multiply(&z, &field.multiply(&u, &u)?)?;
    let m = field.add(&field.multiply(&zu2, &zu2)?, &zu2)?;
    let b_by_a = field.multiply(field.b(), &field.invert(field.a())?)?;
    // The first candidate is the opposite of b over a, times one more than the inverse of m, and where m is nothing, b over z times a.
    let usual = field.multiply(&field.subtract(&zero, &b_by_a)?, &field.add(&one, &field.invert(&m)?)?)?;
    let rare = field.multiply(&b_by_a, &field.invert(&z)?)?;
    let x1 = select_number(mask(field.is_zero(&m)), &field.to_bytes(&rare)?, &field.to_bytes(&usual)?);
    // The second is the first times z times the square of the number; one of the two is the first coordinate of a point.
    let x2 = field.to_bytes(&field.multiply(&zu2, &field.read_number(&x1)?)?)?;
    let first_mask = mask(field.is_square(&curve_side(field, &field.read_number(&x1)?)?)?);
    // Of the two points with that coordinate, the one whose second is odd where the number is.
    point_with_parity(field, &select_number(first_mask, &x1, &x2), field.to_bytes(&u)?[NUMBER - 1] & 1)
}

/// The password's point hashed to (IEEE 802.11-2020, 12.4.4.2.3 and 12.4.5.2): two numbers derived from it and the network's name mapped to points, their sum taken as often as the addresses say.
pub fn hash_to_element<F: Field>(
    crypto: &impl Primitives,
    field: &F,
    name: &[u8],
    password: &[u8],
    a: &[u8; 6],
    b: &[u8; 6],
) -> Option<[u8; POINT]> {
    let map_label = |label: &[u8]| {
        let mut input = [0u8; MAP_BYTES];
        crypto.hkdf_sha256(name, password, &[label], &mut input).then(|| map_to_curve(field, &input)).flatten()
    };
    let element = field.add_points(&map_label(FIRST)?, &map_label(SECOND)?)?;
    // The number of times: what the addresses hash to, brought into the range from 1 to one less than the number of points.
    let times = field.to_range(&crypto.hmac_sha256(&[0; HASH], &[&order_addresses(a, b)]))?;
    field.multiply_point(&element, &times)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::base::checksum::fnv;
    use crate::base::checksum::fnv_from;
    use crate::wifi::wpa::Crypto;
    use crate::wifi::wpa::PMK;
    use crate::wifi::wpa::tests::Sums;
    use core::cell::Cell;

    pub(crate) struct Toy(pub(crate) Cell<u32>);

    const PRIME: u64 = 4_294_967_279;
    const A: u64 = PRIME - 3;
    const B: u64 = 4;
    const STATION: [u8; 6] = [2, 0, 0, 0, 0, 1];
    const ACCESS: [u8; 6] = [2, 0, 0, 0, 0, 9];

    fn to_value(bytes: &[u8]) -> u64 {
        let (high, low) = bytes.split_at(bytes.len().saturating_sub(8));
        if high.iter().any(|byte| *byte != 0) {
            u64::MAX
        } else {
            low.iter().fold(0, |value, byte| value << 8 | u64::from(*byte))
        }
    }

    fn encode(value: u64) -> [u8; NUMBER] {
        let mut bytes = [0u8; NUMBER];
        bytes[NUMBER - 8..].copy_from_slice(&value.to_be_bytes());
        bytes
    }

    fn plus(a: u64, b: u64) -> u64 {
        ((u128::from(a) + u128::from(b)) % u128::from(PRIME)) as u64
    }

    fn minus(a: u64, b: u64) -> u64 {
        ((u128::from(a % PRIME) + u128::from(PRIME) - u128::from(b % PRIME)) % u128::from(PRIME)) as u64
    }

    fn times(a: u64, b: u64) -> u64 {
        (u128::from(a) * u128::from(b) % u128::from(PRIME)) as u64
    }

    fn power(base: u64, mut exponent: u64) -> u64 {
        let (mut base, mut result) = (base % PRIME, 1);
        while exponent > 0 {
            if exponent & 1 == 1 {
                result = times(result, base);
            }
            (base, exponent) = (times(base, base), exponent >> 1);
        }
        result
    }

    fn curve_value(x: u64) -> u64 {
        plus(plus(times(times(x, x), x), times(A, x)), B)
    }

    fn coordinates(point: &[u8; POINT]) -> Option<(u64, u64)> {
        let (x, y) = (to_value(&point[..NUMBER]), to_value(&point[NUMBER..]));
        (x < PRIME && y < PRIME && times(y, y) == curve_value(x)).then_some((x, y))
    }

    fn point((x, y): (u64, u64)) -> [u8; POINT] {
        to_point(&encode(x), &encode(y))
    }

    fn sum(a: Option<(u64, u64)>, b: Option<(u64, u64)>) -> Option<(u64, u64)> {
        let (Some((x1, y1)), Some((x2, y2))) = (a, b) else {
            return a.or(b);
        };
        if x1 == x2 && plus(y1, y2) == 0 {
            return None;
        }
        let slope = if x1 == x2 {
            times(plus(times(3, times(x1, x1)), A), power(times(2, y1), PRIME - 2))
        } else {
            times(minus(y2, y1), power(minus(x2, x1), PRIME - 2))
        };
        let x3 = minus(minus(times(slope, slope), x1), x2);
        Some((x3, minus(times(slope, minus(x1, x3)), y1)))
    }

    impl Primitives for Toy {
        fn hmac_sha256(&self, key: &[u8], parts: &[&[u8]]) -> [u8; HASH] {
            self.0.set(self.0.get() + 1);
            let sum = fnv(key.iter().chain(parts.iter().flat_map(|part| part.iter())));
            let mut hash = [0u8; HASH];
            hash[HASH - 4..].copy_from_slice(&((sum >> 32) as u32).to_be_bytes());
            hash
        }

        fn hkdf_sha256(&self, salt: &[u8], secret: &[u8], info: &[&[u8]], out: &mut [u8]) -> bool {
            let mut sum = fnv(salt.iter().chain(secret).chain(info.iter().flat_map(|part| part.iter())));
            for byte in out.iter_mut() {
                sum = fnv_from(sum, &[0xFF]);
                *byte = (sum >> 32) as u8;
            }
            true
        }

        fn random(&self, out: &mut [u8]) -> bool {
            out.iter_mut().enumerate().for_each(|(at, byte)| *byte = 0xA0 ^ at as u8);
            true
        }
    }

    impl Crypto for Toy {
        fn hmac_sha1(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
            Sums.hmac_sha1(key, parts)
        }

        fn cmac(&self, _key: &[u8; 16], _parts: &[&[u8]]) -> Option<[u8; 16]> {
            None
        }

        fn unwrap_key(&self, key: &[u8; 16], wrapped: &[u8]) -> Option<Vec<u8>> {
            Sums.unwrap_key(key, wrapped)
        }

        fn derive_pmk(&self, password: &[u8], name: &[u8]) -> [u8; PMK] {
            Sums.derive_pmk(password, name)
        }
    }

    impl Curve for Toy {
        fn random(&self) -> Option<[u8; NUMBER]> {
            Some(encode(2))
        }

        fn add_scalars(&self, a: &[u8; NUMBER], b: &[u8; NUMBER]) -> Option<[u8; NUMBER]> {
            Some(encode(plus(to_value(a), to_value(b))))
        }

        fn order(&self) -> Option<[u8; NUMBER]> {
            Some(encode(PRIME))
        }

        fn multiply_point(&self, at: &[u8; POINT], number: &[u8; NUMBER]) -> Option<[u8; POINT]> {
            let (mut base, mut remaining, mut product) = (Some(coordinates(at)?), to_value(number), None);
            while remaining > 0 {
                if remaining & 1 == 1 {
                    product = sum(product, base);
                }
                (base, remaining) = (sum(base, base), remaining >> 1);
            }
            product.map(point)
        }

        fn add_points(&self, a: &[u8; POINT], b: &[u8; POINT]) -> Option<[u8; POINT]> {
            sum(Some(coordinates(a)?), Some(coordinates(b)?)).map(point)
        }

        fn negate(&self, at: &[u8; POINT]) -> Option<[u8; POINT]> {
            let (x, y) = coordinates(at)?;
            Some(point((x, minus(0, y))))
        }
    }

    impl Field for Toy {
        type Number = u64;

        fn read_number(&self, bytes: &[u8]) -> Option<u64> {
            Some(to_value(bytes))
        }

        fn reduce(&self, bytes: &[u8]) -> Option<u64> {
            Some(bytes.iter().fold(0, |value, byte| plus(times(value, 256), u64::from(*byte))))
        }

        fn make_number(&self, value: u64) -> Option<u64> {
            Some(value % PRIME)
        }

        fn to_bytes(&self, number: &u64) -> Option<[u8; NUMBER]> {
            Some(encode(*number))
        }

        fn add(&self, a: &u64, b: &u64) -> Option<u64> {
            Some(plus(*a, *b))
        }

        fn subtract(&self, a: &u64, b: &u64) -> Option<u64> {
            Some(minus(*a, *b))
        }

        fn multiply(&self, a: &u64, b: &u64) -> Option<u64> {
            Some(times(*a, *b))
        }

        fn is_square(&self, value: &u64) -> Option<bool> {
            Some(power(*value, (PRIME - 1) / 2) == 1)
        }

        fn sqrt(&self, value: &u64) -> Option<u64> {
            Some(power(*value, (PRIME + 1) / 4))
        }

        fn invert(&self, value: &u64) -> Option<u64> {
            Some(power(*value, PRIME - 2))
        }

        fn is_zero(&self, value: &u64) -> bool {
            value.is_multiple_of(PRIME)
        }

        fn is_under_prime(&self, value: &u64) -> bool {
            *value < PRIME
        }

        fn prime(&self) -> Option<[u8; NUMBER]> {
            Some(encode(PRIME))
        }

        fn a(&self) -> &u64 {
            &A
        }

        fn b(&self) -> &u64 {
            &B
        }

        fn to_range(&self, hash: &[u8; HASH]) -> Option<[u8; NUMBER]> {
            Some(encode(to_value(hash) % (PRIME - 1) + 1))
        }
    }

    #[test]
    fn hunt_and_peck_point() {
        let toy = Toy(Cell::new(0));
        let pwe = hunt_and_peck(&toy, &toy, b"a password", &STATION, &ACCESS).unwrap();
        assert!(coordinates(&pwe).is_some(), "a point of the curve");
        assert!(toy.0.get() >= 2 * u32::from(ROUNDS), "after every round, however soon it was found");
        assert_eq!(
            hunt_and_peck(&toy, &toy, b"a password", &ACCESS, &STATION),
            Some(pwe),
            "the same whoever is named first"
        );
        assert_ne!(
            hunt_and_peck(&toy, &toy, b"a Password", &STATION, &ACCESS),
            Some(pwe),
            "another for another password"
        );
        assert_ne!(
            hunt_and_peck(&toy, &toy, b"a password", &STATION, &[2, 0, 0, 0, 0, 7]),
            Some(pwe),
            "and between others"
        );
    }

    #[test]
    fn map_to_curve_parity() {
        let toy = Toy(Cell::new(0));
        for first in [0u8, 1, 2, 3, 0x5A, 0xFF] {
            let input: [u8; MAP_BYTES] = core::array::from_fn(|at| first.wrapping_mul(at as u8 + 1));
            let at = map_to_curve(&toy, &input).unwrap();
            let (_, y) = coordinates(&at).unwrap_or_else(|| panic!("no point of the curve for {first}"));
            assert_eq!(y & 1, toy.reduce(&input).unwrap() & 1, "odd where the number is, for {first}");
        }
        assert!(
            map_to_curve(&toy, &[0; MAP_BYTES]).is_some_and(|at| coordinates(&at).is_some()),
            "nothing is mapped to a point too"
        );
    }

    #[test]
    fn hash_to_element_point() {
        let toy = Toy(Cell::new(0));
        let pwe = hash_to_element(&toy, &toy, b"home", b"a password", &STATION, &ACCESS).unwrap();
        assert!(coordinates(&pwe).is_some(), "a point of the curve");
        assert_eq!(
            hash_to_element(&toy, &toy, b"home", b"a password", &ACCESS, &STATION),
            Some(pwe),
            "whoever is named first"
        );
        assert_ne!(
            hash_to_element(&toy, &toy, b"hoMe", b"a password", &STATION, &ACCESS),
            Some(pwe),
            "another for another network"
        );
        assert_ne!(
            hash_to_element(&toy, &toy, b"home", b"a password", &STATION, &[2, 0, 0, 0, 0, 7]),
            Some(pwe),
            "and between others"
        );
    }
}

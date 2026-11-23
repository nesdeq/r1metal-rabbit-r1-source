use aws_lc_sys as lc;
use r1_funcore::wifi::sae::point::Field;
use r1_funcore::wifi::sae::{Curve, LEAST_SCALAR, NUMBER, POINT, to_point};
use std::ptr;

// A piece of a longer number, a byte shorter than the prime and so under it.
const PIECE: usize = NUMBER - 1;

fn bytes_of(number: *const lc::BIGNUM) -> Option<[u8; NUMBER]> {
    let mut out = [0u8; NUMBER];
    (unsafe { lc::BN_bn2bin_padded(out.as_mut_ptr(), NUMBER, number) } == 1).then_some(out)
}

/// A library number, wiped and freed on drop.
pub struct Number(*mut lc::BIGNUM);

impl Drop for Number {
    fn drop(&mut self) {
        unsafe { lc::BN_clear_free(self.0) }
    }
}

impl Number {
    fn new() -> Option<Number> {
        let number = unsafe { lc::BN_new() };
        (!number.is_null()).then(|| Number(number))
    }

    fn from_bytes(bytes: &[u8]) -> Option<Number> {
        let number = unsafe { lc::BN_bin2bn(bytes.as_ptr(), bytes.len(), ptr::null_mut()) };
        (!number.is_null()).then(|| Number(number))
    }

    fn to_bytes(&self) -> Option<[u8; NUMBER]> {
        bytes_of(self.0)
    }
}

struct Point(*mut lc::EC_POINT);

impl Drop for Point {
    fn drop(&mut self) {
        unsafe { lc::EC_POINT_clear_free(self.0) }
    }
}

struct Scratch(*mut lc::BN_CTX);

impl Drop for Scratch {
    fn drop(&mut self) {
        unsafe { lc::BN_CTX_free(self.0) }
    }
}

struct Montgomery(*mut lc::BN_MONT_CTX);

impl Drop for Montgomery {
    fn drop(&mut self) {
        unsafe { lc::BN_MONT_CTX_free(self.0) }
    }
}

/// P-256 with its order, prime, coefficients, and the exponents of the square test, the root and the inverse.
pub struct P256 {
    group: *const lc::EC_GROUP,
    order: *const lc::BIGNUM,
    scratch: Scratch,
    prime: Number,
    // What the library multiplies under the prime by, without a division.
    montgomery: Montgomery,
    // What a number is multiplied by to stand one piece higher.
    shift: Number,
    a: Number,
    b: Number,
    square_exponent: Number,
    root_exponent: Number,
    inverse_exponent: Number,
}

impl P256 {
    /// None if the library has no memory.
    pub fn new() -> Option<P256> {
        let group = unsafe { lc::EC_group_p256() };
        let order = unsafe { lc::EC_GROUP_get0_order(group) };
        let scratch = unsafe { lc::BN_CTX_new() };
        let scratch = (!scratch.is_null()).then(|| Scratch(scratch))?;
        let (prime, a, b) = (Number::new()?, Number::new()?, Number::new()?);
        if unsafe { lc::EC_GROUP_get_curve_GFp(group, prime.0, a.0, b.0, scratch.0) } != 1 {
            return None;
        }
        // The prime is 3 more than a multiple of 4, so the powers are half of it to tell a square, a quarter of the number after it to take a root, and 2 less than it to invert.
        let (square_exponent, root_exponent, inverse_exponent) = (Number::new()?, Number::new()?, Number::new()?);
        let has_square = unsafe { lc::BN_rshift1(square_exponent.0, prime.0) } == 1;
        let has_root = unsafe {
            !lc::BN_copy(root_exponent.0, prime.0).is_null()
                && lc::BN_add_word(root_exponent.0, 1) == 1
                && lc::BN_rshift(root_exponent.0, root_exponent.0, 2) == 1
        };
        let has_inverse = unsafe {
            !lc::BN_copy(inverse_exponent.0, prime.0).is_null() && lc::BN_sub_word(inverse_exponent.0, 2) == 1
        };
        if !(has_square && has_root && has_inverse) {
            return None;
        }
        let montgomery = unsafe { lc::BN_MONT_CTX_new_for_modulus(prime.0, scratch.0) };
        let montgomery = (!montgomery.is_null()).then(|| Montgomery(montgomery))?;
        let mut shift = [0u8; NUMBER];
        shift[NUMBER - 1 - PIECE] = 1;
        let shift = Number::from_bytes(&shift)?;
        Some(P256 {
            group,
            order,
            scratch,
            prime,
            montgomery,
            shift,
            a,
            b,
            square_exponent,
            root_exponent,
            inverse_exponent,
        })
    }

    fn power(&self, base: &Number, exponent: &Number) -> Option<Number> {
        let result = Number::new()?;
        (unsafe {
            lc::BN_mod_exp_mont_consttime(result.0, base.0, exponent.0, self.prime.0, self.scratch.0, ptr::null())
        } == 1)
            .then_some(result)
    }

    fn new_point(&self) -> Option<Point> {
        let point = unsafe { lc::EC_POINT_new(self.group) };
        (!point.is_null()).then(|| Point(point))
    }

    fn parse_point(&self, coordinates: &[u8; POINT]) -> Option<Point> {
        let (x, y) = (Number::from_bytes(&coordinates[..NUMBER])?, Number::from_bytes(&coordinates[NUMBER..])?);
        let point = self.new_point()?;
        (unsafe { lc::EC_POINT_set_affine_coordinates_GFp(self.group, point.0, x.0, y.0, self.scratch.0) } == 1)
            .then_some(point)
    }

    fn to_coordinates(&self, point: &Point) -> Option<[u8; POINT]> {
        let (x, y) = (Number::new()?, Number::new()?);
        if unsafe { lc::EC_POINT_get_affine_coordinates_GFp(self.group, point.0, x.0, y.0, self.scratch.0) } != 1 {
            return None;
        }
        Some(to_point(&x.to_bytes()?, &y.to_bytes()?))
    }
}

impl Curve for P256 {
    fn random(&self) -> Option<[u8; NUMBER]> {
        let number = Number::new()?;
        (unsafe { lc::BN_rand_range_ex(number.0, LEAST_SCALAR.into(), self.order) } == 1)
            .then(|| number.to_bytes())
            .flatten()
    }

    fn add_scalars(&self, left: &[u8; NUMBER], right: &[u8; NUMBER]) -> Option<[u8; NUMBER]> {
        let (left, right, sum) = (Number::from_bytes(left)?, Number::from_bytes(right)?, Number::new()?);
        (unsafe { lc::BN_mod_add_quick(sum.0, left.0, right.0, self.order) } == 1).then(|| sum.to_bytes()).flatten()
    }

    fn order(&self) -> Option<[u8; NUMBER]> {
        bytes_of(self.order)
    }

    fn multiply_point(&self, point: &[u8; POINT], number: &[u8; NUMBER]) -> Option<[u8; POINT]> {
        let (point, number, product) = (self.parse_point(point)?, Number::from_bytes(number)?, self.new_point()?);
        (unsafe { lc::EC_POINT_mul(self.group, product.0, ptr::null(), point.0, number.0, self.scratch.0) } == 1)
            .then(|| self.to_coordinates(&product))
            .flatten()
    }

    fn add_points(&self, left: &[u8; POINT], right: &[u8; POINT]) -> Option<[u8; POINT]> {
        let (left, right, sum) = (self.parse_point(left)?, self.parse_point(right)?, self.new_point()?);
        (unsafe { lc::EC_POINT_add(self.group, sum.0, left.0, right.0, self.scratch.0) } == 1)
            .then(|| self.to_coordinates(&sum))
            .flatten()
    }

    fn negate(&self, point: &[u8; POINT]) -> Option<[u8; POINT]> {
        let point = self.parse_point(point)?;
        (unsafe { lc::EC_POINT_invert(self.group, point.0, self.scratch.0) } == 1)
            .then(|| self.to_coordinates(&point))
            .flatten()
    }
}

impl Field for P256 {
    type Number = Number;

    fn read_number(&self, bytes: &[u8]) -> Option<Number> {
        Number::from_bytes(bytes)
    }

    fn reduce(&self, bytes: &[u8]) -> Option<Number> {
        // The library's own reduction divides, in a time that follows the number: the pieces, each under the prime, are summed by the steps that do not.
        bytes.rchunks(PIECE).rev().try_fold(self.make_number(0)?, |sum, piece| {
            self.add(&self.multiply(&sum, &self.shift)?, &Number::from_bytes(piece)?)
        })
    }

    fn make_number(&self, value: u64) -> Option<Number> {
        let number = Number::new()?;
        (unsafe { lc::BN_set_word(number.0, value) } == 1).then_some(number)
    }

    fn to_bytes(&self, number: &Number) -> Option<[u8; NUMBER]> {
        number.to_bytes()
    }

    fn add(&self, left: &Number, right: &Number) -> Option<Number> {
        let sum = Number::new()?;
        (unsafe { lc::BN_mod_add_quick(sum.0, left.0, right.0, self.prime.0) } == 1).then_some(sum)
    }

    fn subtract(&self, left: &Number, right: &Number) -> Option<Number> {
        let difference = Number::new()?;
        (unsafe { lc::BN_mod_sub_quick(difference.0, left.0, right.0, self.prime.0) } == 1).then_some(difference)
    }

    fn multiply(&self, left: &Number, right: &Number) -> Option<Number> {
        // Raised into the library's Montgomery form, the left number takes the form's own factor out of the product again.
        let (raised, product) = (Number::new()?, Number::new()?);
        (unsafe {
            lc::BN_to_montgomery(raised.0, left.0, self.montgomery.0, self.scratch.0) == 1
                && lc::BN_mod_mul_montgomery(product.0, raised.0, right.0, self.montgomery.0, self.scratch.0) == 1
        })
        .then_some(product)
    }

    fn is_square(&self, value: &Number) -> Option<bool> {
        Some(unsafe { lc::BN_is_one(self.power(value, &self.square_exponent)?.0) } == 1)
    }

    fn sqrt(&self, value: &Number) -> Option<Number> {
        self.power(value, &self.root_exponent)
    }

    fn invert(&self, value: &Number) -> Option<Number> {
        self.power(value, &self.inverse_exponent)
    }

    fn is_zero(&self, value: &Number) -> bool {
        unsafe { lc::BN_is_zero(value.0) == 1 }
    }

    fn is_under_prime(&self, value: &Number) -> bool {
        unsafe { lc::BN_cmp(value.0, self.prime.0) < 0 }
    }

    fn prime(&self) -> Option<[u8; NUMBER]> {
        self.prime.to_bytes()
    }

    fn a(&self) -> &Number {
        &self.a
    }

    fn b(&self) -> &Number {
        &self.b
    }

    fn to_range(&self, hash: &[u8; 32]) -> Option<[u8; NUMBER]> {
        let (hash, modulus, scalar) = (Number::from_bytes(hash)?, Number::new()?, Number::new()?);
        let is_done = unsafe {
            lc::BN_sub(modulus.0, self.order, lc::BN_value_one()) == 1
                && lc::BN_nnmod(scalar.0, hash.0, modulus.0, self.scratch.0) == 1
                && lc::BN_add_word(scalar.0, 1) == 1
        };
        is_done.then(|| scalar.to_bytes()).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::Library;
    use aws_lc_rs::digest;
    use r1_funcore::base::code::{hex, unhex};
    use r1_funcore::crypto::Primitives;
    use r1_funcore::wifi::sae::point::{self, MAP_BYTES};
    use r1_funcore::wifi::sae::{Station, Step, is_scalar};
    use std::cell::Cell;

    fn bytes<const N: usize>(digits: &str) -> [u8; N] {
        unhex(digits).and_then(|bytes| bytes.try_into().ok()).expect("hex digits of as many bytes")
    }

    struct FixedRandom<'a>(&'a P256, Cell<usize>, [[u8; NUMBER]; 2]);

    impl Curve for FixedRandom<'_> {
        fn random(&self) -> Option<[u8; NUMBER]> {
            self.2.get(self.1.replace(self.1.get() + 1)).copied()
        }

        fn add_scalars(&self, left: &[u8; NUMBER], right: &[u8; NUMBER]) -> Option<[u8; NUMBER]> {
            self.0.add_scalars(left, right)
        }

        fn order(&self) -> Option<[u8; NUMBER]> {
            self.0.order()
        }

        fn multiply_point(&self, point: &[u8; POINT], number: &[u8; NUMBER]) -> Option<[u8; POINT]> {
            self.0.multiply_point(point, number)
        }

        fn add_points(&self, left: &[u8; POINT], right: &[u8; POINT]) -> Option<[u8; POINT]> {
            self.0.add_points(left, right)
        }

        fn negate(&self, point: &[u8; POINT]) -> Option<[u8; POINT]> {
            self.0.negate(point)
        }
    }

    const ORDER: &str = "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551";
    const BASE: &str = "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c2964fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5";
    const TWICE: &str = "7cf27b188d034f7e8a52380304b51ac3c08969e277f21b35a60b48fc4766997807775510db8ed040293d9ac69f7430dbba7dade63ce982299e04b79d227873d1";

    #[test]
    fn curve_arithmetic() {
        let curve = P256::new().unwrap();
        let (base, twice, order) = (bytes::<POINT>(BASE), bytes::<POINT>(TWICE), bytes::<NUMBER>(ORDER));
        let number = |value: u8| std::array::from_fn::<u8, NUMBER, _>(|at| if at == NUMBER - 1 { value } else { 0 });
        assert_eq!(
            (curve.multiply_point(&base, &number(2)), curve.add_points(&base, &base)),
            (Some(twice), Some(twice)),
            "a point taken twice is the point added to itself"
        );
        let opposite = curve.negate(&base).unwrap();
        assert_eq!(
            (opposite[..NUMBER].to_vec(), curve.negate(&opposite), curve.add_points(&base, &opposite)),
            (base[..NUMBER].to_vec(), Some(base), None),
            "the opposite has the same first coordinate, and the two add up to no point"
        );
        assert_eq!(
            (curve.multiply_point(&base, &order), curve.multiply_point(&base, &number(0))),
            (None, None),
            "as many times as there are points, or no times, gives none"
        );
        let mut bad = base;
        bad[POINT - 1] ^= 1;
        assert_eq!(
            (
                curve.multiply_point(&bad, &number(2)),
                curve.add_points(&bad, &base),
                curve.add_points(&base, &bad),
                curve.negate(&bad),
                curve.multiply_point(&[0; POINT], &number(2))
            ),
            (None, None, None, None, None),
            "what is no point of the curve is taken by nothing"
        );
        let mut last = order;
        last[NUMBER - 1] -= 1;
        assert_eq!(
            (curve.add_scalars(&last, &number(2)), curve.add_scalars(&number(3), &number(4))),
            (Some(number(1)), Some(number(7))),
            "numbers add up modulo the number of points"
        );
        assert_eq!(
            [number(0), number(1), number(2), last, order, [0xFF; NUMBER]].map(|value| is_scalar(&curve, &value)),
            [false, false, true, true, false, false],
            "a number fits that is more than 1 and less than the number of points"
        );
        let (first, second) = (curve.random().unwrap(), curve.random().unwrap());
        assert!(
            first != second && is_scalar(&curve, &first) && is_scalar(&curve, &second),
            "random numbers differ and fit"
        );
    }

    type Division = unsafe extern "C" fn(
        *mut lc::BIGNUM,
        *const lc::BIGNUM,
        *const lc::BIGNUM,
        *const lc::BIGNUM,
        *mut lc::BN_CTX,
    ) -> std::os::raw::c_int;

    // What the library's step that divides gives of two numbers under the prime.
    fn divide(curve: &P256, step: Division, left: &Number, right: &Number) -> Option<[u8; NUMBER]> {
        let result = Number::new()?;
        (unsafe { step(result.0, left.0, right.0, curve.prime.0, curve.scratch.0) } == 1)
            .then(|| result.to_bytes())
            .flatten()
    }

    #[test]
    fn field_steps_match_division() {
        let curve = P256::new().unwrap();
        let bytes_of = |number: Option<Number>| number.and_then(|number| number.to_bytes());
        // A number under the prime: its first byte nought.
        let under = |seed: u8| {
            let bytes: [u8; NUMBER] =
                std::array::from_fn(|at| if at == 0 { 0 } else { seed.wrapping_mul(at as u8).wrapping_add(seed) });
            Number::from_bytes(&bytes).unwrap()
        };
        for (left, right) in [(under(1), under(7)), (under(0xFF), under(0x80)), (under(0), under(3))] {
            assert_eq!(bytes_of(curve.add(&left, &right)), divide(&curve, lc::BN_mod_add, &left, &right));
            assert_eq!(bytes_of(curve.subtract(&left, &right)), divide(&curve, lc::BN_mod_sub, &left, &right));
            assert_eq!(bytes_of(curve.multiply(&left, &right)), divide(&curve, lc::BN_mod_mul, &left, &right));
        }
        let over = Number::from_bytes(&[0xFF; NUMBER]).unwrap();
        assert!(!curve.is_under_prime(&over));
        assert_eq!(
            [bytes_of(curve.multiply(&over, &over)), bytes_of(curve.multiply(&over, &under(9)))],
            [divide(&curve, lc::BN_mod_mul, &over, &over), divide(&curve, lc::BN_mod_mul, &over, &under(9))],
            "a candidate past the prime is multiplied as the number it is"
        );
        let pattern: [u8; MAP_BYTES] = std::array::from_fn(|at| 0x5A_u8.wrapping_mul(at as u8 + 1));
        for input in [[0xFF; MAP_BYTES], [0; MAP_BYTES], pattern] {
            let (whole, divided) = (Number::from_bytes(&input).unwrap(), Number::new().unwrap());
            assert_eq!(unsafe { lc::BN_nnmod(divided.0, whole.0, curve.prime.0, curve.scratch.0) }, 1);
            assert_eq!(bytes_of(curve.reduce(&input)), divided.to_bytes(), "the map's bytes under the prime");
        }
    }

    const ADDRESS_A: [u8; 6] = [0x4d, 0x3f, 0x2f, 0xff, 0xe3, 0x87];
    const ADDRESS_B: [u8; 6] = [0xa5, 0xd8, 0xaa, 0x95, 0x8e, 0x3c];
    const PASSWORD: &[u8] = b"mekmitasdigoat";
    const PRIVATE: &str = "992465fd3daa3c60aa6565b7f62a2a7f2e12dd12f198faf4fbed89d7ff1ace94";
    const MASK: &str = "9507a90f777a044d6a0830b91ea3d5dd70bece44e1acffb86983b5e1bf9fb322";
    const OWN_COMMIT: &str = "2e2c0f0db52440ad146d967114ce005ce1eab0aa2c2e5c2871b774f6c2575c65d5ad9e00829707aa36ba8b859738fc961d08243505f47c035376d7ac4bc8d7b95083bf43827d0fc31ed778dd3671fd21a46d1091d64b6f9a1e1272621325dbe1";
    const PEER_COMMIT: &str = "591b96f3397fb945100848e7b550543b6720d88337ee93fc49fd6df7e08b5223e71b9bb048d3873f20556953a96c91536fd8ee6ca9b4a68a148b056a909be03e83ae208f60f8ef5537858074db06687032399862999b511e0a1552a5fea317c2";
    const CONFIRM_KEY: &str = "1e733f6d9bd53256287304338831b09a39406d121017073a5c30db36f36cb81a";
    const MASTER: &str = "4e4dfab1a2dd8ac1a91790f953faaa452ae5c6873ab75b63605ba663f8a7fe59";
    const NAME: &str = "8747a600eea3f9f22475df58ca1e5498";

    #[test]
    fn sae_standard_exchange() {
        let curve = P256::new().unwrap();
        let point = point::hunt_and_peck(&Library, &curve, PASSWORD, &ADDRESS_A, &ADDRESS_B).unwrap();
        assert_eq!(
            point::hunt_and_peck(&Library, &curve, PASSWORD, &ADDRESS_B, &ADDRESS_A),
            Some(point),
            "the password's point is the same whoever's address is named first"
        );
        assert_ne!(point::hunt_and_peck(&Library, &curve, b"mekmitasdigoaT", &ADDRESS_A, &ADDRESS_B), Some(point));
        let (own, peer) = (bytes::<96>(OWN_COMMIT), bytes::<96>(PEER_COMMIT));
        let fixed = FixedRandom(&curve, Cell::new(0), [bytes(PRIVATE), bytes(MASK)]);
        let (mut station, commit) = Station::begin(&fixed, point, false).unwrap();
        assert_eq!(
            hex(&commit),
            format!("0300010000001300{}", hex(&own)),
            "the station commits to what the standard has it commit to"
        );
        let confirm = station.take(&Library, &fixed, &[&[3, 0, 1, 0, 0, 0, 19, 0][..], &peer].concat()).unwrap();
        let key = bytes::<32>(CONFIRM_KEY);
        let proof =
            |count: u16, first: &[u8], second: &[u8]| Library.hmac_sha256(&key, &[&count.to_le_bytes(), first, second]);
        assert_eq!(
            confirm,
            [Step::Send([&[3, 0, 2, 0, 0, 0, 1, 0][..], &proof(1, &own, &peer)].concat())],
            "and proves the password with the standard's key"
        );
        let steps =
            station.take(&Library, &fixed, &[&[3, 0, 2, 0, 0, 0, 7, 0][..], &proof(7, &peer, &own)].concat()).unwrap();
        assert_eq!(
            steps,
            [Step::Agreed { pmk: bytes(MASTER), pmkid: bytes(NAME) }],
            "to the access point's proof it answers with the standard's key and that key's name"
        );
    }

    const HASHED: &str = "c93049b9e64000f848201649e999f2b5c22dea69b5632c9df4d633b8aa1f6c1e73634e94b53d82e7383a8d258199d9dc1a5ee8269d060382ccbf33e614ff59a0";

    #[test]
    fn hash_to_element_vector() {
        let curve = P256::new().unwrap();
        let (first, second) = ([0x00, 0x09, 0x5b, 0x66, 0xec, 0x1e], [0x00, 0x0b, 0x6b, 0xd9, 0x02, 0x46]);
        let hash = |name: &[u8], first: &[u8; 6], second: &[u8; 6]| {
            point::hash_to_element(&Library, &curve, name, b"mekmitasdigoatpsk4internet", first, second)
        };
        let point = hash(b"byteme", &first, &second).unwrap();
        assert_eq!(hex(&point), HASHED);
        assert_eq!(hash(b"byteme", &second, &first), Some(point), "the same whoever's address is named first");
        assert_ne!(hash(b"bytemE", &first, &second), Some(point), "another for a network of another name");
        assert_ne!(
            hash(b"byteme", &first, &[0x00, 0x0b, 0x6b, 0xd9, 0x02, 0x47]),
            Some(point),
            "and between other stations"
        );
    }

    fn sha256(message: &[u8]) -> Vec<u8> {
        digest::digest(&digest::SHA256, message).as_ref().to_vec()
    }

    #[test]
    fn map_to_curve_matches_library() {
        let curve = P256::new().unwrap();
        let tag = b"QUUX-V01-CS02-with-P256_XMD:SHA-256_SSWU_RO_";
        let tagged = [&tag[..], &[tag.len() as u8]].concat();
        for message in [&b""[..], b"abc", b"abcdef0123456789", &[0x71; 300]] {
            // The 96 bytes the library's hash makes two numbers of (RFC 9380, 5.3.1), worked out here so that the map alone is compared.
            let start = sha256(&[&[0u8; 64][..], message, &[0, 96, 0], &tagged].concat());
            let mut blocks = vec![sha256(&[&start[..], &[1], &tagged].concat())];
            for count in 2..=3u8 {
                let mix: Vec<u8> =
                    start.iter().zip(&blocks[blocks.len() - 1]).map(|(left, right)| left ^ right).collect();
                blocks.push(sha256(&[&mix[..], &[count], &tagged].concat()));
            }
            let stream = blocks.concat();
            let own = curve.add_points(
                &point::map_to_curve(&curve, &stream[..MAP_BYTES]).unwrap(),
                &point::map_to_curve(&curve, &stream[MAP_BYTES..]).unwrap(),
            );
            let library = curve.new_point().unwrap();
            assert_eq!(
                unsafe {
                    lc::EC_hash_to_curve_p256_xmd_sha256_sswu(
                        curve.group,
                        library.0,
                        tag.as_ptr(),
                        tag.len(),
                        message.as_ptr(),
                        message.len(),
                    )
                },
                1
            );
            assert_eq!(own, curve.to_coordinates(&library), "a message of {} bytes", message.len());
        }
        assert!(
            point::map_to_curve(&curve, &[0; MAP_BYTES]).is_some_and(|point| curve.parse_point(&point).is_some()),
            "nothing is mapped to a point too"
        );
    }
}

//! Curve25519: an X25519 key pair, and the curve's arithmetic for XEdDSA as libsignal makes it and zkgroup's Ristretto255.

use super::Crypto;

const MASK: u64 = (1 << 51) - 1;

/// An X25519 key pair, its private half clamped as libsignal keeps it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    pub private: [u8; 32],
    pub public: [u8; 32],
}

impl core::fmt::Debug for Pair {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Pair")
    }
}

fn clamp(mut private: [u8; 32]) -> [u8; 32] {
    private[0] &= 248;
    private[31] &= 127;
    private[31] |= 64;
    private
}

impl Pair {
    pub(super) fn from_private(crypto: &impl Crypto, private: [u8; 32]) -> Option<Pair> {
        let private = clamp(private);
        Some(Pair { private, public: crypto.public_of(&private)? })
    }

    pub fn new(crypto: &impl Crypto) -> Option<Pair> {
        let mut private = [0u8; 32];
        crypto.random(&mut private).then_some(())?;
        Pair::from_private(crypto, private)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Field(pub(super) [u64; 5]);

pub(super) const ZERO: Field = Field([0; 5]);
pub(super) const ONE: Field = Field([1, 0, 0, 0, 0]);
pub(super) const D: Field =
    Field([929955233495203, 466365720129213, 1662059464998953, 2033849074728123, 1442794654840575]);
const D2: Field = Field([1859910466990425, 932731440258426, 1072319116312658, 1815898335770999, 633789495995903]);
pub(super) const ROOT_OF_MINUS_ONE: Field =
    Field([1718705420411056, 234908883556509, 2233514472574048, 2117202627021982, 765476049583133]);
const INVERSE_POWER: [u8; 32] = {
    let mut power = [0xFF; 32];
    power[0] = 0xEB;
    power[31] = 0x7F;
    power
};
pub(super) const ROOT_POWER: [u8; 32] = {
    let mut power = [0xFF; 32];
    power[0] = 0xFD;
    power[31] = 0x0F;
    power
};
const ORDER: [u64; 4] = [0x5812_631A_5CF5_D3ED, 0x14DE_F9DE_A2F7_9CD6, 0, 0x1000_0000_0000_0000];
pub(super) const ORDER_BYTES: [u8; 32] = {
    let mut bytes = [0u8; 32];
    let mut at = 0;
    while at < 32 {
        bytes[at] = (ORDER[at / 8] >> (8 * (at % 8))) as u8;
        at += 1;
    }
    bytes
};

fn mul(a: u64, b: u64) -> u128 {
    u128::from(a) * u128::from(b)
}

impl Field {
    pub(super) fn from_bytes(bytes: &[u8; 32]) -> Field {
        let load = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"));
        Field([
            load(0) & MASK,
            (load(6) >> 3) & MASK,
            (load(12) >> 6) & MASK,
            (load(19) >> 1) & MASK,
            (load(24) >> 12) & MASK,
        ])
    }

    fn carry(self) -> Field {
        let mut limbs = self.0;
        for at in 0..4 {
            limbs[at + 1] += limbs[at] >> 51;
            limbs[at] &= MASK;
        }
        limbs[0] += 19 * (limbs[4] >> 51);
        limbs[4] &= MASK;
        limbs[1] += limbs[0] >> 51;
        limbs[0] &= MASK;
        Field(limbs)
    }

    pub(super) fn to_bytes(self) -> [u8; 32] {
        let mut limbs = self.carry().0;
        let mut quotient = (limbs[0] + 19) >> 51;
        for limb in &limbs[1..] {
            quotient = (limb + quotient) >> 51;
        }
        limbs[0] += 19 * quotient;
        for at in 0..4 {
            limbs[at + 1] += limbs[at] >> 51;
            limbs[at] &= MASK;
        }
        limbs[4] &= MASK;
        let mut bytes = [0u8; 32];
        let (mut buffer, mut bits, mut at) = (0u128, 0, 0);
        for limb in limbs {
            buffer |= u128::from(limb) << bits;
            bits += 51;
            while bits >= 8 {
                bytes[at] = buffer as u8;
                buffer >>= 8;
                bits -= 8;
                at += 1;
            }
        }
        bytes[at] = buffer as u8;
        bytes
    }

    pub(super) fn add(self, other: Field) -> Field {
        let mut limbs = self.0;
        limbs.iter_mut().zip(other.0).for_each(|(limb, addend)| *limb += addend);
        Field(limbs).carry()
    }

    pub(super) fn sub(self, other: Field) -> Field {
        let sixteen_p = [36028797018963664, 36028797018963952, 36028797018963952, 36028797018963952, 36028797018963952];
        let mut limbs = self.0;
        for at in 0..5 {
            limbs[at] = limbs[at] + sixteen_p[at] - other.0[at];
        }
        Field(limbs).carry()
    }

    pub(super) fn neg(self) -> Field {
        ZERO.sub(self)
    }

    pub(super) fn mul(self, other: Field) -> Field {
        let (a, b) = (self.0, other.0);
        let b19 = [b[0], b[1] * 19, b[2] * 19, b[3] * 19, b[4] * 19];
        let mut sums = [
            mul(a[0], b[0]) + mul(a[4], b19[1]) + mul(a[3], b19[2]) + mul(a[2], b19[3]) + mul(a[1], b19[4]),
            mul(a[1], b[0]) + mul(a[0], b[1]) + mul(a[4], b19[2]) + mul(a[3], b19[3]) + mul(a[2], b19[4]),
            mul(a[2], b[0]) + mul(a[1], b[1]) + mul(a[0], b[2]) + mul(a[4], b19[3]) + mul(a[3], b19[4]),
            mul(a[3], b[0]) + mul(a[2], b[1]) + mul(a[1], b[2]) + mul(a[0], b[3]) + mul(a[4], b19[4]),
            mul(a[4], b[0]) + mul(a[3], b[1]) + mul(a[2], b[2]) + mul(a[1], b[3]) + mul(a[0], b[4]),
        ];
        for at in 0..4 {
            sums[at + 1] += sums[at] >> 51;
            sums[at] &= u128::from(MASK);
        }
        let mut limbs = sums.map(|sum| sum as u64 & MASK);
        limbs[0] += 19 * (sums[4] >> 51) as u64;
        Field(limbs).carry()
    }

    pub(super) fn square(self) -> Field {
        self.mul(self)
    }

    pub(super) fn power(self, exponent: &[u8; 32]) -> Field {
        let mut result = ONE;
        for bit in (0..256).rev() {
            result = result.square();
            if exponent[bit / 8] >> (bit % 8) & 1 == 1 {
                result = result.mul(self);
            }
        }
        result
    }

    fn invert(self) -> Field {
        self.power(&INVERSE_POWER)
    }

    pub(super) fn same(self, other: Field) -> bool {
        self.to_bytes() == other.to_bytes()
    }

    pub(super) fn is_negative(self) -> bool {
        self.to_bytes()[0] & 1 == 1
    }

    pub(super) fn select(self, other: Field, is_other: bool) -> Field {
        let mask = 0u64.wrapping_sub(u64::from(is_other));
        let mut limbs = self.0;
        limbs.iter_mut().zip(other.0).for_each(|(limb, value)| *limb ^= mask & (*limb ^ value));
        Field(limbs)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Point {
    pub(super) x: Field,
    pub(super) y: Field,
    pub(super) z: Field,
    pub(super) t: Field,
}

pub(super) const IDENTITY: Point = Point { x: ZERO, y: ONE, z: ONE, t: ZERO };
pub(super) const BASE: Point = Point {
    x: Field([1738742601995546, 1146398526822698, 2070867633025821, 562264141797630, 587772402128613]),
    y: Field([1801439850948184, 1351079888211148, 450359962737049, 900719925474099, 1801439850948198]),
    z: ONE,
    t: Field([1841354044333475, 16398895984059, 755974180946558, 900171276175154, 1821297809914039]),
};

impl Point {
    pub(super) fn add(&self, other: &Point) -> Point {
        let a = self.y.sub(self.x).mul(other.y.sub(other.x));
        let b = self.y.add(self.x).mul(other.y.add(other.x));
        let c = self.t.mul(D2).mul(other.t);
        let d = self.z.add(self.z).mul(other.z);
        let (e, f, g, h) = (b.sub(a), d.sub(c), d.add(c), b.add(a));
        Point { x: e.mul(f), y: g.mul(h), z: f.mul(g), t: e.mul(h) }
    }

    fn double(&self) -> Point {
        let a = self.x.square();
        let b = self.y.square();
        let c = self.z.square().add(self.z.square());
        let e = self.x.add(self.y).square().sub(a).sub(b);
        let g = b.sub(a);
        let f = g.sub(c);
        let h = a.add(b).neg();
        Point { x: e.mul(f), y: g.mul(h), z: f.mul(g), t: e.mul(h) }
    }

    pub(super) fn neg(&self) -> Point {
        Point { x: self.x.neg(), y: self.y, z: self.z, t: self.t.neg() }
    }

    fn select(&self, other: &Point, is_other: bool) -> Point {
        Point {
            x: self.x.select(other.x, is_other),
            y: self.y.select(other.y, is_other),
            z: self.z.select(other.z, is_other),
            t: self.t.select(other.t, is_other),
        }
    }

    pub(super) fn times(&self, scalar: &[u8; 32]) -> Point {
        let mut result = IDENTITY;
        for bit in (0..256).rev() {
            result = result.double();
            let sum = result.add(self);
            result = result.select(&sum, scalar[bit / 8] >> (bit % 8) & 1 == 1);
        }
        result
    }

    fn compress(&self) -> [u8; 32] {
        let inverse = self.z.invert();
        let mut bytes = self.y.mul(inverse).to_bytes();
        bytes[31] |= u8::from(self.x.mul(inverse).is_negative()) << 7;
        bytes
    }

    fn decompress(bytes: &[u8; 32]) -> Option<Point> {
        let y = Field::from_bytes(bytes);
        let y2 = y.square();
        let (u, v) = (y2.sub(ONE), D.mul(y2).add(ONE));
        let v3 = v.square().mul(v);
        let mut x = u.mul(v3).mul(u.mul(v3.square().mul(v)).power(&ROOT_POWER));
        let check = v.mul(x.square());
        if check.same(u.neg()) {
            x = x.mul(ROOT_OF_MINUS_ONE);
        } else if !check.same(u) {
            return None;
        }
        if x.is_negative() {
            x = x.neg();
        }
        if bytes[31] >> 7 == 1 {
            x = x.neg();
        }
        Some(Point { x, y, z: ONE, t: x.mul(y) })
    }

    fn is_identity(&self) -> bool {
        self.compress() == IDENTITY.compress()
    }
}

fn to_edwards(public: &[u8; 32], sign: u8) -> Option<Point> {
    let u = Field::from_bytes(public);
    if u.same(ONE.neg()) {
        return None;
    }
    let mut bytes = u.sub(ONE).mul(u.add(ONE).invert()).to_bytes();
    bytes[31] ^= sign << 7;
    Point::decompress(&bytes)
}

/// The Ed25519 public key of an X25519 public key and a sign, under which XEdDSA signatures verify.
pub fn edwards_key(public: &[u8; 32], sign: u8) -> Option<[u8; 32]> {
    Some(to_edwards(public, sign)?.compress())
}

fn sub_with_borrow(a: &[u64; 4], b: &[u64; 4]) -> ([u64; 4], bool) {
    let mut difference = [0u64; 4];
    let mut borrow = false;
    for at in 0..4 {
        let (step, first) = a[at].overflowing_sub(b[at]);
        let (step, second) = step.overflowing_sub(u64::from(borrow));
        difference[at] = step;
        borrow = first || second;
    }
    (difference, borrow)
}

pub(super) fn reduce(number: &[u8]) -> [u8; 32] {
    let mut remainder = [0u64; 4];
    for bit in (0..8 * number.len()).rev() {
        let mut carry = u64::from(number[bit / 8] >> (bit % 8) & 1);
        for limb in remainder.iter_mut() {
            let top = *limb >> 63;
            *limb = (*limb << 1) | carry;
            carry = top;
        }
        let (difference, is_below) = sub_with_borrow(&remainder, &ORDER);
        let mask = 0u64.wrapping_sub(u64::from(!is_below));
        remainder.iter_mut().zip(difference).for_each(|(limb, value)| *limb ^= mask & (*limb ^ value));
    }
    let mut bytes = [0u8; 32];
    bytes.iter_mut().enumerate().for_each(|(at, byte)| *byte = (remainder[at / 8] >> (8 * (at % 8))) as u8);
    bytes
}

fn limbs(scalar: &[u8; 32]) -> [u64; 4] {
    core::array::from_fn(|at| u64::from_le_bytes(scalar[8 * at..8 * at + 8].try_into().expect("eight bytes")))
}

pub(super) fn multiply_add(h: &[u8; 32], a: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
    let (h, a, r) = (limbs(h), limbs(a), limbs(r));
    let mut product = [0u64; 9];
    for (at, &x) in h.iter().enumerate() {
        let mut carry = 0u128;
        for (offset, &y) in a.iter().enumerate() {
            let sum = mul(x, y) + u128::from(product[at + offset]) + carry;
            product[at + offset] = sum as u64;
            carry = sum >> 64;
        }
        product[at + 4] = carry as u64;
    }
    let mut carry = 0u128;
    for (at, limb) in product.iter_mut().enumerate() {
        let sum = u128::from(*limb) + u128::from(r.get(at).copied().unwrap_or(0)) + carry;
        *limb = sum as u64;
        carry = sum >> 64;
    }
    let mut bytes = [0u8; 72];
    bytes.iter_mut().enumerate().for_each(|(at, byte)| *byte = (product[at / 8] >> (8 * (at % 8))) as u8);
    reduce(&bytes)
}

const NONCE_PREFIX: [u8; 32] = {
    let mut prefix = [0xFF; 32];
    prefix[0] = 0xFE;
    prefix
};

/// An XEdDSA signature of a message in parts by a clamped X25519 private key, with 64 random bytes, as libsignal makes it: the Edwards key's sign in the top bit of the signature, the private key never negated.
pub fn sign(crypto: &impl Crypto, private: &[u8; 32], message: &[&[u8]], random: &[u8; 64]) -> [u8; 64] {
    let a = reduce(private);
    let public = BASE.times(&a).compress();
    let mut parts: alloc::vec::Vec<&[u8]> = alloc::vec![&NONCE_PREFIX, private];
    parts.extend_from_slice(message);
    parts.push(random);
    let r = reduce(&crypto.sha512(&parts));
    let big_r = BASE.times(&r).compress();
    let mut parts: alloc::vec::Vec<&[u8]> = alloc::vec![&big_r, &public];
    parts.extend_from_slice(message);
    let h = reduce(&crypto.sha512(&parts));
    let s = multiply_add(&h, &a, &r);
    let mut signature = [0u8; 64];
    signature[..32].copy_from_slice(&big_r);
    signature[32..].copy_from_slice(&s);
    signature[63] = (signature[63] & 0x7F) | (public[31] & 0x80);
    signature
}

pub(super) fn sign_fresh(crypto: &impl Crypto, private: &[u8; 32], message: &[&[u8]]) -> Option<[u8; 64]> {
    let mut random = [0u8; 64];
    crypto.random(&mut random).then_some(())?;
    Some(sign(crypto, private, message, &random))
}

/// True if an XEdDSA signature of a message in parts is the X25519 public key's.
pub fn verify(crypto: &impl Crypto, public: &[u8; 32], message: &[&[u8]], signature: &[u8; 64]) -> bool {
    let Some(point) = to_edwards(public, signature[63] >> 7) else { return false };
    let mut s: [u8; 32] = signature[32..].try_into().expect("thirty-two bytes");
    s[31] &= 0x7F;
    if s[31] & 0xE0 != 0 {
        return false;
    }
    let encoded = point.compress();
    let mut parts: alloc::vec::Vec<&[u8]> = alloc::vec![&signature[..32], &encoded];
    parts.extend_from_slice(message);
    let h = reduce(&crypto.sha512(&parts));
    let check = BASE.times(&reduce(&s)).add(&point.neg().times(&h)).compress();
    check[..] == signature[..32]
}

pub(super) fn is_canonical(public: &[u8; 32]) -> bool {
    let is_unreduced = public[31] & 0x80 != 0
        || (public[0] >= 0u8.wrapping_sub(19) && public[1..31] == [0xFF; 30] && public[31] == 0x7F);
    !is_unreduced && to_edwards(public, 0).is_some_and(|point| point.times(&ORDER_BYTES).is_identity())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::base::code::hex;
    use crate::base::code::unhex;

    pub(in crate::signal) fn x25519(scalar: &[u8; 32], u: Option<&[u8; 32]>) -> Option<[u8; 32]> {
        let point = u.map_or(Some(BASE), |u| to_edwards(u, 0))?.times(scalar);
        let u = point.z.add(point.y).mul(point.z.sub(point.y).invert()).to_bytes();
        (u != [0; 32]).then_some(u)
    }

    fn bytes(text: &str) -> [u8; 32] {
        unhex(text).unwrap().try_into().unwrap()
    }

    #[test]
    fn base_point_encoding() {
        assert_eq!(hex(&BASE.compress()), "5866666666666666666666666666666666666666666666666666666666666666");
        let point = Point::decompress(&BASE.compress()).unwrap();
        assert!(point.add(&BASE.neg()).is_identity());
        assert!(BASE.times(&ORDER_BYTES).is_identity(), "ℓ B is the identity");
        assert!(!BASE.times(&reduce(&[2])).is_identity());
        assert_eq!(BASE.double().compress(), BASE.add(&BASE).compress());
    }

    #[test]
    fn rfc_8032_public_key() {
        let a = bytes("307c83864f2833cb427a2ef1c00a013cfdff2768d980c0a3a520f006904de94f");
        assert_eq!(
            hex(&BASE.times(&reduce(&a)).compress()),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
        assert_eq!(BASE.times(&a).compress(), BASE.times(&reduce(&a)).compress(), "ℓ more is the same point");
    }

    #[test]
    fn x25519_to_edwards() {
        let u = bytes("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        let clamped = bytes("70076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c6a");
        let own = BASE.times(&reduce(&clamped)).compress();
        assert_eq!(hex(&own), "8120f299c37ae1ca64a179f638a6c6fafde968f1c33705e28c413c7579d988cf");
        assert_eq!(to_edwards(&u, 1).unwrap().compress(), own);
        assert_eq!(
            hex(&to_edwards(&u, 0).unwrap().compress()),
            "8120f299c37ae1ca64a179f638a6c6fafde968f1c33705e28c413c7579d9884f"
        );
        let mut minus_one = (ONE.neg()).to_bytes();
        assert!(to_edwards(&minus_one, 0).is_none(), "u = −1 has no Edwards point");
        minus_one[0] ^= 1;
        assert!(to_edwards(&minus_one, 0).is_some());
    }

    #[test]
    fn scalar_reduction() {
        let input: [u8; 64] = core::array::from_fn(|at| at as u8);
        assert_eq!(hex(&reduce(&input)), "7a3c6282f02d37a05023b60d5428e6cc5961d4c31221937adae0b574e4d07205");
        assert_eq!(reduce(&ORDER_BYTES), [0; 32]);
        let h = bytes("fcc5b77f9550d7aab986b1577ba27acc7375767778797a7b7c7d7e7f80818203");
        let r = bytes("d23259b65b62cdfe173e48eaa72ba5b3d7d9dadbdcdddedfe0e1e2e3e4e5e607");
        let a = bytes("70076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c6a");
        assert_eq!(hex(&multiply_add(&h, &a, &r)), "d8b392ab6f825674cc6a845ae627d1ea67767ff23055a66e599bc4a8152a5c0a");
    }

    #[test]
    fn field_encoding() {
        let p_minus_one = ONE.neg().to_bytes();
        assert_eq!(hex(&p_minus_one), "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f");
        assert_eq!(Field::from_bytes(&p_minus_one).add(ONE).to_bytes(), [0; 32], "p is nought");
        let x = Field::from_bytes(&bytes("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"));
        assert!(x.mul(x.invert()).same(ONE));
        assert!(ROOT_OF_MINUS_ONE.square().same(ONE.neg()));
    }

    #[test]
    fn canonical_keys() {
        let u = bytes("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        assert!(is_canonical(&u));
        assert!(!is_canonical(&[0; 32]), "u = 0 is the point of order two");
        let mut top_bit = u;
        top_bit[31] |= 0x80;
        assert!(!is_canonical(&top_bit), "the top bit set");
        let p = ONE.neg().add(ONE).add(ONE.neg()).to_bytes();
        let mut at_p = p;
        at_p[0] = 0xED;
        assert!(!is_canonical(&at_p), "p itself, written as such");
    }
}

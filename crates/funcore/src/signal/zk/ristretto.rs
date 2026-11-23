// Ristretto255 (RFC 9496) over Curve25519's Edwards points, and Lizard as curve25519-dalek has it.

use super::super::Crypto;
use super::super::curve::{BASE, D, Field, IDENTITY, ONE, Point, ROOT_OF_MINUS_ONE, ROOT_POWER, ZERO};
use super::Scalar;
use core::ops::Range;

const INVSQRT_A_MINUS_D: Field =
    Field([278908739862762, 821645201101625, 8113234426968, 1777959178193151, 2118520810568447]);
const SQRT_AD_MINUS_ONE: Field =
    Field([2241493124984347, 425987919032274, 2207028919301688, 1220490630685848, 974799131293748]);
const ONE_MINUS_D_SQUARED: Field =
    Field([1136626929484150, 1998550399581263, 496427632559748, 118527312129759, 45110755273534]);
const D_MINUS_ONE_SQUARED: Field =
    Field([1507062230895904, 1572317787530805, 683053064812840, 317374165784489, 1572899562415810]);
const SQRT_ID: Field =
    Field([2298852427963285, 3837146560810661, 4413131899466403, 3883177008057528, 2352084440532925]);
const DP1_OVER_DM1: Field =
    Field([2159851467815724, 1752228607624431, 1825604053920671, 1212587319275468, 253422448836237]);
const MDOUBLE_INVSQRT_A_MINUS_D: Field =
    Field([1693982333959686, 608509411481997, 2235573344831311, 947681270984193, 266558006233600]);
const MIDOUBLE_INVSQRT_A_MINUS_D: Field =
    Field([1608655899704280, 1999971613377227, 49908634785720, 1873700692181652, 353702208628067]);
const MINVSQRT_ONE_PLUS_D: Field =
    Field([321571956990465, 1251814006996634, 2226845496292387, 189049560751797, 2074948709371214]);
const LIZARD_DATA: Range<usize> = 8..24;
const LOW_BYTE_MASK: u8 = 0xFE;
const HIGH_BYTE_MASK: u8 = 0x3F;

fn sqrt_ratio(u: Field, v: Field) -> (bool, Field) {
    let v3 = v.square().mul(v);
    let v7 = v3.square().mul(v);
    let root = u.mul(v3).mul(u.mul(v7).power(&ROOT_POWER));
    let check = v.mul(root.square());
    let correct = check.same(u);
    let flipped = check.same(u.neg());
    let flipped_i = check.same(u.neg().mul(ROOT_OF_MINUS_ONE));
    let root = root.select(root.mul(ROOT_OF_MINUS_ONE), flipped || flipped_i);
    (correct || flipped, absolute(root))
}

fn invsqrt(v: Field) -> (bool, Field) {
    sqrt_ratio(ONE, v)
}

fn absolute(x: Field) -> Field {
    x.select(x.neg(), x.is_negative())
}

fn preimage(s: Field, t: Field) -> Option<Field> {
    let s_is_zero = s.same(ZERO);
    let mut out = ZERO.select(SQRT_ID, t.same(ONE));
    let a = t.add(ONE).mul(DP1_OVER_DM1);
    let s2 = s.square();
    let (square, y) = invsqrt(s2.square().sub(a.square()).mul(ROOT_OF_MINUS_ONE));
    let signed_s2 = s2.select(s2.neg(), s.is_negative());
    out = out.select(absolute(a.add(signed_s2).mul(y)), !s_is_zero && square);
    (s_is_zero || square).then_some(out)
}

#[derive(Clone, Copy)]
pub(in crate::signal) struct Element(Point);

impl Element {
    pub(super) const BASE: Element = Element(BASE);
    pub(super) const IDENTITY: Element = Element(IDENTITY);

    pub(super) fn decode(bytes: &[u8]) -> Option<Element> {
        let bytes: &[u8; 32] = bytes.try_into().ok()?;
        let s = Field::from_bytes(bytes);
        if s.to_bytes() != *bytes || s.is_negative() {
            return None;
        }
        let ss = s.square();
        let (u1, u2) = (ONE.sub(ss), ONE.add(ss));
        let u2_squared = u2.square();
        let v = D.neg().mul(u1.square()).sub(u2_squared);
        let (square, root) = invsqrt(v.mul(u2_squared));
        let dx = root.mul(u2);
        let dy = root.mul(dx.mul(v));
        let x = absolute(s.add(s).mul(dx));
        let y = u1.mul(dy);
        let t = x.mul(y);
        (square && !t.is_negative() && !y.same(ZERO)).then_some(Element(Point { x, y, z: ONE, t }))
    }

    pub(in crate::signal) fn encode(&self) -> [u8; 32] {
        let Point { x, y, z, t } = self.0;
        let u1 = z.add(y).mul(z.sub(y));
        let u2 = x.mul(y);
        // u1 u2² is always a square.
        let (_, root) = invsqrt(u1.mul(u2.square()));
        let (i1, i2) = (root.mul(u1), root.mul(u2));
        let z_inverse = i1.mul(i2.mul(t));
        let rotate = t.mul(z_inverse).is_negative();
        let rotated_x = x.select(y.mul(ROOT_OF_MINUS_ONE), rotate);
        let rotated_y = y.select(x.mul(ROOT_OF_MINUS_ONE), rotate);
        let denominator = i2.select(i1.mul(INVSQRT_A_MINUS_D), rotate);
        let y = rotated_y.select(rotated_y.neg(), rotated_x.mul(z_inverse).is_negative());
        absolute(denominator.mul(z.sub(y))).to_bytes()
    }

    pub(super) fn same(&self, other: &Element) -> bool {
        let (first, second) = (self.0, other.0);
        first.x.mul(second.y).same(first.y.mul(second.x)) || first.x.mul(second.x).same(first.y.mul(second.y))
    }

    pub(super) fn add(&self, other: &Element) -> Element {
        Element(self.0.add(&other.0))
    }

    pub(super) fn sub(&self, other: &Element) -> Element {
        Element(self.0.add(&other.0.neg()))
    }

    pub(super) fn neg(&self) -> Element {
        Element(self.0.neg())
    }

    pub(super) fn times(&self, scalar: &Scalar) -> Element {
        Element(self.0.times(scalar.bytes()))
    }

    pub(super) fn from_uniform(bytes: &[u8; 64]) -> Element {
        let (first, second) = bytes.split_at(32);
        let map = |half: &[u8]| Element::elligator(Field::from_bytes(half.try_into().expect("thirty-two bytes")));
        map(first).add(&map(second))
    }

    fn elligator(r0: Field) -> Element {
        let r = ROOT_OF_MINUS_ONE.mul(r0.square());
        let n_s = r.add(ONE).mul(ONE_MINUS_D_SQUARED);
        let minus_one = ONE.neg();
        let d = minus_one.sub(D.mul(r)).mul(r.add(D));
        let (square, s) = sqrt_ratio(n_s, d);
        let s_prime = s.mul(r0);
        let s_prime = s_prime.select(s_prime.neg(), !s_prime.is_negative());
        let s = s.select(s_prime, !square);
        let c = minus_one.select(r, !square);
        let n_t = c.mul(r.sub(ONE)).mul(D_MINUS_ONE_SQUARED).sub(d);
        let s_squared = s.square();
        // The point in completed coordinates, x = X/Z and y = Y/T, then in extended ones.
        let (big_x, big_z) = (s.add(s).mul(d), n_t.mul(SQRT_AD_MINUS_ONE));
        let (big_y, big_t) = (ONE.sub(s_squared), ONE.add(s_squared));
        Element(Point { x: big_x.mul(big_t), y: big_y.mul(big_z), z: big_z.mul(big_t), t: big_x.mul(big_y) })
    }

    // The field element Lizard writes 16 bytes of data as: their SHA-256 with the data set into its middle.
    fn lizard_bytes(crypto: &impl Crypto, data: &[u8]) -> [u8; 32] {
        let mut bytes = crypto.sha256(&[data]);
        bytes[LIZARD_DATA].copy_from_slice(data);
        bytes[0] &= LOW_BYTE_MASK;
        bytes[31] &= HIGH_BYTE_MASK;
        bytes
    }

    pub(super) fn lizard(crypto: &impl Crypto, data: &[u8; 16]) -> Element {
        Element::elligator(Field::from_bytes(&Element::lizard_bytes(crypto, data)))
    }

    pub(super) fn unlizard(&self, crypto: &impl Crypto) -> Option<[u8; 16]> {
        let (mut data, mut count) = (None, 0);
        for (s, t) in self.jacobi() {
            for candidate in [preimage(s, t), preimage(s.neg(), t.neg())].into_iter().flatten() {
                let bytes = candidate.to_bytes();
                if Element::lizard_bytes(crypto, &bytes[LIZARD_DATA]) == bytes {
                    count += 1;
                    data = bytes[LIZARD_DATA].try_into().ok();
                }
            }
        }
        if count == 1 { data } else { None }
    }

    fn jacobi(&self) -> [(Field, Field); 4] {
        let Point { x, y, z, .. } = self.0;
        let (x2, y2, z2) = (x.square(), y.square(), z.square());
        let z2_min_y2 = z2.sub(y2);
        let (_, gamma) = invsqrt(y2.square().mul(x2).mul(z2_min_y2));
        let den = gamma.mul(y2);
        let (s_over_x, sp_over_xp) = (den.mul(z.sub(y)), den.mul(z.add(y)));
        let tmp = MDOUBLE_INVSQRT_A_MINUS_D.mul(z);
        let (s0, s1) = (s_over_x.mul(x), sp_over_xp.neg().mul(x));
        let (t0, t1) = (tmp.mul(s_over_x), tmp.mul(sp_over_xp));
        // The same with (X, Y, Z) taken as (Y, X, iZ).
        let den = z2_min_y2.neg().mul(MINVSQRT_ONE_PLUS_D).mul(gamma);
        let iz = ROOT_OF_MINUS_ONE.mul(z);
        let (s_over_y, sp_over_yp) = (den.mul(iz.sub(x)), den.mul(iz.add(x)));
        let tmp = MDOUBLE_INVSQRT_A_MINUS_D.mul(iz);
        let (s2, s3) = (s_over_y.mul(y), sp_over_yp.neg().mul(y));
        let (t2, t3) = (tmp.mul(s_over_y), tmp.mul(sp_over_yp));
        // Where X or Y is nought, every s and t above is: the quartic's points are then (0, 1), (1, −2i/sqrt(−d − 1)) and (−1, −2i/sqrt(−d − 1)).
        let edge = x.same(ZERO) || y.same(ZERO);
        [
            (s0, t0.select(ONE, edge)),
            (s1, t1.select(ONE, edge)),
            (s2.select(ONE, edge), t2.select(MIDOUBLE_INVSQRT_A_MINUS_D, edge)),
            (s3.select(ONE.neg(), edge), t3.select(MIDOUBLE_INVSQRT_A_MINUS_D, edge)),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::Exact;
    use super::*;
    use crate::base::code::hex;
    use crate::base::code::unhex;
    use alloc::string::String;
    use alloc::vec::Vec;

    fn bytes(text: &str) -> Vec<u8> {
        unhex(text).unwrap()
    }

    #[test]
    fn field_constants() {
        let i = ROOT_OF_MINUS_ONE;
        let (_, invsqrt_a_minus_d) = invsqrt(ONE.neg().sub(D));
        let (_, sqrt_id) = sqrt_ratio(i.mul(D), ONE);
        let (_, invsqrt_one_plus_d) = invsqrt(D.add(ONE));
        let pairs = [
            (INVSQRT_A_MINUS_D, invsqrt_a_minus_d),
            (SQRT_AD_MINUS_ONE.square(), D.neg().sub(ONE)),
            (ONE_MINUS_D_SQUARED, ONE.sub(D.square())),
            (D_MINUS_ONE_SQUARED, D.sub(ONE).square()),
            (SQRT_ID, sqrt_id),
            (
                DP1_OVER_DM1,
                D.add(ONE).mul(D.sub(ONE).power(&{
                    let mut exponent = [0xFF; 32];
                    (exponent[0], exponent[31]) = (0xEB, 0x7F);
                    exponent
                })),
            ),
            (MDOUBLE_INVSQRT_A_MINUS_D, INVSQRT_A_MINUS_D.add(INVSQRT_A_MINUS_D).neg()),
            (MIDOUBLE_INVSQRT_A_MINUS_D, MDOUBLE_INVSQRT_A_MINUS_D.mul(i)),
            (MINVSQRT_ONE_PLUS_D, invsqrt_one_plus_d.neg()),
        ];
        for (at, (constant, expected)) in pairs.into_iter().enumerate() {
            assert!(constant.same(expected), "constant {at}");
        }
        assert!(SQRT_AD_MINUS_ONE.is_negative(), "the negative root of a d − 1");
    }

    #[test]
    fn base_multiples() {
        let multiples = [
            "0000000000000000000000000000000000000000000000000000000000000000",
            "e2f2ae0a6abc4e71a884a961c500515f58e30b6aa582dd8db6a65945e08d2d76",
            "6a493210f7499cd17fecb510ae0cea23a110e8d5b901f8acadd3095c73a3b919",
            "94741f5d5d52755ece4f23f044ee27d5d1ea1e2bd196b462166b16152a9d0259",
            "da80862773358b466ffadfe0b3293ab3d9fd53c5ea6c955358f568322daf6a57",
            "e882b131016b52c1d3337080187cf768423efccbb517bb495ab812c4160ff44e",
            "f64746d3c92b13050ed8d80236a7f0007c3b3f962f5ba793d19a601ebb1df403",
            "44f53520926ec81fbd5a387845beb7df85a96a24ece18738bdcfa6a7822a176d",
            "903293d8f2287ebe10e2374dc1a53e0bc887e592699f02d077d5263cdd55601c",
            "02622ace8f7303a31cafc63f8fc48fdc16e1c8c8d234b2f0d6685282a9076031",
            "20706fd788b2720a1ed2a5dad4952b01f413bcf0e7564de8cdc816689e2db95f",
            "bce83f8ba5dd2fa572864c24ba1810f9522bc6004afe95877ac73241cafdab42",
            "e4549ee16b9aa03099ca208c67adafcafa4c3f3e4e5303de6026e3ca8ff84460",
            "aa52e000df2e16f55fb1032fc33bc42742dad6bd5a8fc0be0167436c5948501f",
            "46376b80f409b29dc2b5f6f0c52591990896e5716f41477cd30085ab7f10301e",
            "e0c418f7c8d9c4cdd7395b93ea124f3ad99021bb681dfc3302a9d99a2e53e64e",
        ];
        let mut multiple = Element::IDENTITY;
        for (times, encoding) in multiples.into_iter().enumerate() {
            assert_eq!(hex(&multiple.encode()), encoding, "{times} B");
            let element = Element::decode(&bytes(encoding)).expect("the writing of an element");
            assert!(element.same(&multiple) && element.encode() == multiple.encode(), "{times} B reads back");
            multiple = multiple.add(&Element::BASE);
        }
        let three = Element::BASE.times(&Scalar::from_wide(&{
            let mut three = [0u8; 64];
            three[0] = 3;
            three
        }));
        assert!(three.same(&Element::decode(&bytes(multiples[3])).unwrap()), "B times three");
        assert!(three.sub(&Element::BASE).add(&Element::BASE.neg()).same(&Element::BASE), "3 B − B − B");
    }

    #[test]
    fn invalid_encodings() {
        let invalid = [
            "00ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
            "f3ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
            "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
            "0100000000000000000000000000000000000000000000000000000000000000",
            "01ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
            "ed57ffd8c914fb201471d1c3d245ce3c746fcbe63a3679d51b6a516ebebe0e20",
            "c34c4e1826e5d403b78e246e88aa051c36ccf0aafebffe137d148a2bf9104562",
            "c940e5a4404157cfb1628b108db051a8d439e1a421394ec4ebccb9ec92a8ac78",
            "47cfc5497c53dc8e61c91d17fd626ffb1c49e2bca94eed052281b510b1117a24",
            "f1c6165d33367351b0da8f6e4511010c68174a03b6581212c71c0e1d026c3c72",
            "87260f7a2f12495118360f02c26a470f450dadf34a413d21042b43b9d93e1309",
            "26948d35ca62e643e26a83177332e6b6afeb9d08e4268b650f1f5bbd8d81d371",
            "4eac077a713c57b4f4397629a4145982c661f48044dd3f96427d40b147d9742f",
            "de6a7b00deadc788eb6b6c8d20c0ae96c2f2019078fa604fee5b87d6e989ad7b",
            "bcab477be20861e01e4a0e295284146a510150d9817763caf1a6f4b422d67042",
            "2a292df7e32cababbd9de088d1d1abec9fc0440f637ed2fba145094dc14bea08",
            "f4a9e534fc0d216c44b218fa0c42d99635a0127ee2e53c712f70609649fdff22",
            "8268436f8c4126196cf64b3c7ddbda90746a378625f9813dd9b8457077256731",
            "2810e5cbc2cc4d4eece54f61c6f69758e289aa7ab440b3cbeaa21995c2f4232b",
            "3eb858e78f5a7254d8c9731174a94f76755fd3941c0ac93735c07ba14579630e",
            "a45fdc55c76448c049a1ab33f17023edfb2be3581e9c7aade8a6125215e04220",
            "d483fe813c6ba647ebbfd3ec41adca1c6130c2beeee9d9bf065c8d151c5f396e",
            "8a2e1d30050198c65a54483123960ccc38aef6848e1ec8f5f780e8523769ba32",
            "32888462f8b486c68ad7dd9610be5192bbeaf3b443951ac1a8118419d9fa097b",
            "227142501b9d4355ccba290404bde41575b037693cef1f438c47f8fbf35d1165",
            "5c37cc491da847cfeb9281d407efc41e15144c876e0170b499a96a22ed31e01e",
            "445425117cb8c90edcbc7c1cc0e74f747f2c1efa5630a967c64f287792a48a4b",
            "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
        ];
        for encoding in invalid {
            assert!(Element::decode(&bytes(encoding)).is_none(), "{encoding}");
        }
    }

    #[test]
    fn sqrt_ratio_vectors() {
        let field = |text: &str| Field::from_bytes(&bytes(text).try_into().unwrap());
        let zero = "0000000000000000000000000000000000000000000000000000000000000000";
        let (one, two, four) = (
            "0100000000000000000000000000000000000000000000000000000000000000",
            "0200000000000000000000000000000000000000000000000000000000000000",
            "0400000000000000000000000000000000000000000000000000000000000000",
        );
        let vectors = [
            (zero, zero, true, zero),
            (zero, one, true, zero),
            (one, zero, false, zero),
            (two, one, false, "3c5ff1b5d8e4113b871bd052f9e7bcd0582804c266ffb2d4f4203eb07fdb7c54"),
            (four, one, true, two),
            (one, four, true, "f6ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff3f"),
        ];
        for (u, v, is_square, root) in vectors {
            let (actual, actual_root) = sqrt_ratio(field(u), field(v));
            assert_eq!((actual, hex(&actual_root.to_bytes())), (is_square, String::from(root)), "u {u}, v {v}");
        }
    }

    #[test]
    fn elligator_vectors() {
        let vectors = [
            (
                "b8f98731fd7b597143a006ef0769d329c0f9b939096646c60f7f071aa0668647",
                "b09ded61421d8ca6a85e1a9dd4d8e5a0c3f6e8efa9703fc1402098450bbef656",
            ),
            (
                "e50ef1e34b09763c8099e215b7d95b886200e79c7c4d528b8e86a4a9a93efa34",
                "ea8d4dcbb5e1fa4aab3e0f764ed49613830ebceec2f48d8aa6a2537ae4c9131a",
            ),
            (
                "736d24dcb4df6306cca9131da9445417156dbd957fcd5b66ac2370238645ba22",
                "e8e7335c05a85024adb36844ba9544288caa1b67638c15f22b3efa86d0ff3d59",
            ),
            (
                "1031606babc7a4098110403ef13f84add1a070d769329d51fd69019ae5197853",
                "d0788c81b1b3ed9ffca01c0dce05d3f1c0da016182f114a9772ef61d4f504d54",
            ),
            (
                "9c83a1a2ecfb05bba7ab11b294d25acf56154fa1a7d7ea0188f2b6f826554f56",
                "ca0bec913a0cb59dd106d5584b930b77bf8b2f8e212499c1dfb7b208cd78f86e",
            ),
            (
                "fbb17c3612654bebf5ba132e859de5400a88b5b94e90fea789316b0a3d0a1519",
                "1a42e743cbaf748220883efdd72e05d6a6f86cedd847f4ad488552068ff06829",
            ),
            (
                "e8c11444f04dba4db7282c56961fc6d44c5103d9c5087e807e98a4d0992cbd4d",
                "289d6660c9dfc8c596b56a53677e8f2191e64e06ab92d28f7005f517b78a1278",
            ),
            (
                "ade595b125e61e453d38acbedb73a7c247863b4b1cf4761aa26140100fbd1e40",
                "dc251bcbefc4b0832542bcf3b9fa7117a7d39af3a8d736ab9f24c3510d962b2b",
            ),
            (
                "6a473d6bfa752a975bcad46434bcbe157dda1f12fdf1a08539f203a4bd446f4b",
                "e879b0deb7c49f5aeec1693465a7f4aa7972c406439850b9dd075369b0d0e079",
            ),
            (
                "70ccb65adcc67849ad6bc111e328a224968deb37acb70c27c2882b99f4765b59",
                "e2b5b734f1a33db3ddcfdc49f5f219ec4354b3dea73ea7b620095c1ea57fcc44",
            ),
            (
                "6f18cb7bfebd0ba233c4a388cc8f0ade217051cd222308425a06a43aaab12219",
                "e27710f2c88bf0570bde5c929cf32e77413b01f85cb732af5728ce35d0dc940d",
            ),
            (
                "e1b71e34ec5206b76d19e3b5195229c1504da150f2cb4fcc88f5836eed6a033a",
                "46f04f70369de4924a7ad858e83e9e0d0e927375b0de5ae1f4175ebe96078860",
            ),
            (
                "cff626381e56b05a1bc83d2add1b38d24fb2bd7844c178a74db935c57c80bf7e",
                "1647f1672dc1c390b7659a322744316e332c3e00e5714851a81d496a66288418",
            ),
            (
                "0188d750f02e3f9310f4e6cf52bd4a326aa98a561e83d6caa67dfbe462182415",
                "c4856b0b82694a21ccab85ddaec1f12426b3c46bdbb9b5fde42f9b2ae749294e",
            ),
            (
                "d2cfe4389b74cf3654c3fbd7f9c7744b6defc4fbc2f6fce446929c231927f104",
                "3affe1c573d0a08f27c552458feb5caa4a28390babe31ab9d9cf5ab9c5be233c",
            ),
            (
                "22747b0908285dbd0967396742e303029d6b86dbca4ae69a4e6bdbc3d60e5450",
                "582b5c76df886991eeba7308d67099fd266ccde69d820b426555fd6e6e0e9470",
            ),
        ];
        for (input, output) in vectors {
            let input: [u8; 32] = bytes(input).try_into().unwrap();
            assert_eq!(hex(&Element::elligator(Field::from_bytes(&input)).encode()), output, "Elligator's map");
        }
        let uniform = [
            (
                "5d1be09e3d0c82fc538112490e35701979d99e06ca3e2b5b54bffe8b4dc772c14d98b696a1bbfb5ca32c436cc61c16563790306c79eaca7705668b47dffe5bb6",
                "3066f82a1a747d45120d1740f14358531a8f04bbffe6a819f86dfe50f44a0a46",
            ),
            (
                "f116b34b8f17ceb56e8732a60d913dd10cce47a6d53bee9204be8b44f6678b270102a56902e2488c46120e9276cfe54638286b9e4b3cdb470b542d46c2068d38",
                "f26e5b6f7d362d2d2a94c5d0e7602cb4773c95a2e5c31a64f133189fa76ed61b",
            ),
            (
                "8422e1bbdaab52938b81fd602effb6f89110e1e57208ad12d9ad767e2e25510c27140775f9337088b982d83d7fcf0b2fa1edffe51952cbe7365e95c86eaf325c",
                "006ccd2a9e6867e6a2c5cea83d3302cc9de128dd2a9a57dd8ee7b9d7ffe02826",
            ),
            (
                "ac22415129b61427bf464e17baee8db65940c233b98afce8d17c57beeb7876c2150d15af1cb1fb824bbd14955f2b57d08d388aab431a391cfc33d5bafb5dbbaf",
                "f8f0c87cf237953c5890aec3998169005dae3eca1fbb04548c635953c817f92a",
            ),
            (
                "165d697a1ef3d5cf3c38565beefcf88c0f282b8e7dbd28544c483432f1cec7675debea8ebb4e5fe7d6f6e5db15f15587ac4d4d4a1de7191e0c1ca6664abcc413",
                "ae81e7dedf20a497e10c304a765c1767a42d6e06029758d2d7e8ef7cc4c41179",
            ),
            (
                "a836e6c9a9ca9f1e8d486273ad56a78c70cf18f0ce10abb1c7172ddd605d7fd2979854f47ae1ccf204a33102095b4200e5befc0465accc263175485f0e17ea5c",
                "e2705652ff9f5e44d3e841bf1c251cf7dddb77d140870d1ab2ed64f1a9ce8628",
            ),
            (
                "2cdc11eaeb95daf01189417cdddbf95952993aa9cb9c640eb5058d09702c74622c9965a697a3b345ec24ee56335b556e677b30e6f90ac77d781064f866a3c982",
                "80bd07262511cdde4863f8a7434cef696750681cb9510eea557088f76d9e5065",
            ),
            (
                "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff1200000000000000000000000000000000000000000000000000000000000000",
                "304282791023b73128d277bdcb5c7746ef2eac08dde9f2983379cb8e5ef0517f",
            ),
            (
                "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                "304282791023b73128d277bdcb5c7746ef2eac08dde9f2983379cb8e5ef0517f",
            ),
            (
                "0000000000000000000000000000000000000000000000000000000000000080ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
                "304282791023b73128d277bdcb5c7746ef2eac08dde9f2983379cb8e5ef0517f",
            ),
            (
                "00000000000000000000000000000000000000000000000000000000000000001200000000000000000000000000000000000000000000000000000000000080",
                "304282791023b73128d277bdcb5c7746ef2eac08dde9f2983379cb8e5ef0517f",
            ),
        ];
        for (input, output) in uniform {
            let input: [u8; 64] = bytes(input).try_into().unwrap();
            assert_eq!(hex(&Element::from_uniform(&input).encode()), output, "the one-way map");
        }
    }

    #[test]
    fn lizard_vectors() {
        let crypto = Exact::default();
        let vectors = [
            ("00000000000000000000000000000000", "f0b7e34484f74cf00f15024b738539738646bbbe1e9bc7509a676815227e774f"),
            ("01010101010101010101010101010101", "cc92e81f585afc5caac88660d8d17e9025a44489a363042123f6af0702156e65"),
            ("000102030405060708090a0b0c0d0e0f", "c830573f8a8e7778671f76cdc796dc0a235cf177f197d9fcba06e84e96247444"),
            ("dddddddddddddddddddddddddddddddd", "ccb60554c081841037f821fa827b6a5bc2531f80e2647f1a858611f4ccfe3056"),
        ];
        for (data, encoding) in vectors {
            let data: [u8; 16] = bytes(data).try_into().unwrap();
            let element = Element::lizard(&crypto, &data);
            assert_eq!(hex(&element.encode()), encoding);
            let decoded = Element::decode(&bytes(encoding)).unwrap();
            assert_eq!(decoded.unlizard(&crypto), Some(data), "read back from its writing");
        }
        assert_eq!(Element::BASE.unlizard(&crypto), None, "an element Lizard did not write holds nothing");
        assert_eq!(Element::BASE.times(&Scalar::from_wide(&[7; 64])).unlizard(&crypto), None);
    }
}

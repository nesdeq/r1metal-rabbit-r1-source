// The groups server's auth credential, as zkcredential and zkgroup make it, in libsignal's bincode.

use super::super::Crypto;
use super::super::address::{ServiceId, parse_uuid};
use super::attributes::{KeyPair, UID_GENERATORS, Uid, false_pni, generators};
use super::proof::{Points, RANDOMNESS, Scalars, Statement};
use super::ristretto::Element;
use super::{Reader, Scalar, Sho};
use crate::base::code::unbase64;
use crate::base::code::unhex;
use crate::base::json;
use crate::base::time::SECONDS_PER_DAY;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const SERVER_PUBLIC_PARAMS: &str = "AMhf5ywVwITZMsff/eCyudZx9JDmkkkbV6PInzG4p8x3VqVJSFiMvnvlEKWuRob/1eaIetR31IYeAbm0NdOuHH8Qi+Rexi1wLlpzIo1gstHWBfZzy1+qHRV5A4TqPp15YzBPm0WSggW6PbSn+F4lf57VCnHF7p8SvzAA2ZZJPYJURt8X7bbg+H3i+PEjH9DXItNEqs2sNcug37xZQDLm7X36nOoGPs54XsEGzPdEV+itQNGUFEjY6X9Uv+Acuks7NpyGvCoKxGwgKgE5XyJ+nNKlyHHOLb6N1NuHyBrZrgtY/JYJHRooo5CEqYKBqdFnmbTVGEkCvJKxLnjwKWf+fEPoWeQFj5ObDjcKMZf2Jm2Ae69x+ikU5gBXsRmoF94GXTLfN0/vLt98KDPnxwAQL9j5V1jGOY8jQl6MLxEs56cwXN0dqCnImzVH3TZT1cJ8SW1BRX6qIVxEzjsSGx3yxF3suAilPMqGRp4ffyopjMD1JXiKR2RwLKzizUe5e8XyGOy9fplzhw3jVzTRyUZTRSZKkMLWcQ/gv0E4aONNqs4P+NameAZYOD12qRkxosQQP5uux6B2nRyZ7sAV54DgFyLiRcq1FvwKw2EPQdk4HDoePrO/RNUbyNddnM/mMgj4FW65xCoT1LmjrIjsv/Ggdlx46ueczhMgtBunx1/w8k8V+l8LVZ8gAT6wkU5J+DPQalQguMg12Jzug3q4TbdHiGCmD9EunCwOmsLuLJkz6EcSYXtrlDEnAM+hicw7iergYLLlMXpfTdGxJCWJmP4zqUFeTTmsmhsjGBt7NiEB/9pFFEB3pSbf4iiUukw63Eo8Aqnf4iwob6X1QviCWuc8t0LUlT9vALgh/f2DPVOOmR0RW6bgRvc7DSF20V/omg+YBw==";
const ISSUER_AT: usize = 1 + 6 * 64 + 32;
const SYSTEM: &str = "589c8718e8263a53a78932b6212a46e7fd52de3ad157b5bb277dba494cfd3471d4cc5f90685952917b33366efcce0512a1f8d70f974758266cb04fc424346d37b20f49cb2a081c94b1771fd8c172ae21785c61ea2c7e31947ce351e7b5ff07028c5329beb87b317ffcd981e440819d91136c988d6d9fbea4a87e55ed24a5993aa02f688ab1d3bd19056f94c8a44b8faddfa3c9c79c95ad44311a7bf00e5e862ec2c399f0d689dfb8c2dc0d7caba32afcf58cf0d85f78195a0b5ab732f565595492cfd982321d1f9be4b21fe6a0214306023d6a05d0d23f67ddc1c0400e5e0a5e92d17595131b7a095e740b884b8c9bb0226a39cfd027c769c4f4677c51f21b24da81fb2bd1356a9d0650f6a63fcc90d93bd74a954ba6f75f0e9fca47a6d21734bce7b28f06b76ef2c44d20a07026534e586eb8e1038874a93e44de362ce7bc0844bffc88e390c62519e281aa6fd53ff9ddd1d9ba303cf70004278ea2ae66ce05a2749d29eba56f3efe99e42902825c473dfc3c154c3762d2e76bd103f629d250b2d9d5c243a4cf8f3be21a84f153f44e2733a105cf780a20f03d84fe1ebbeb0e";
const MAX_ATTRIBUTES: usize = 7;
const LABEL: &[u8] = b"20240222_Signal_AuthCredentialZkc";
const ISSUANCE_VERSION: u8 = 3;
const PRESENTATION_VERSION: u8 = 3;
const PRESENTATION: &[u8] = b"Signal_ZKCredential_Presentation_20230410";
const UID_DOMAIN: &str = "Signal_ZKGroup_20230419_UidEncryption";
const ATTRIBUTES: usize = 5;

struct System {
    w: Element,
    w_prime: Element,
    x0: Element,
    x1: Element,
    v: Element,
    y: [Element; MAX_ATTRIBUTES],
}

fn system() -> System {
    let bytes = unhex(SYSTEM).expect("hex of thirteen elements");
    let at = |number: usize| Element::decode(&bytes[32 * number..32 * (number + 1)]).expect("an element");
    System {
        w: at(0),
        w_prime: at(1),
        x0: at(2),
        x1: at(3),
        v: at(4),
        y: core::array::from_fn(|number| at(6 + number)),
    }
}

#[derive(Clone, Copy)]
pub(in crate::signal) struct Issuer {
    c_w: Element,
    i: [Element; MAX_ATTRIBUTES - 1],
    pub(in crate::signal) endorsement_root: Element,
}

impl Issuer {
    pub(in crate::signal) fn production() -> Issuer {
        let params = unbase64(SERVER_PUBLIC_PARAMS).expect("base64");
        let mut reader = Reader(&params[ISSUER_AT..]);
        let c_w = reader.element().expect("an element");
        let i = core::array::from_fn(|_| reader.element().expect("an element"));
        Issuer { c_w, i, endorsement_root: reader.element().expect("an element") }
    }

    fn i(&self, attributes: usize) -> Element {
        self.i[attributes - 2]
    }
}

impl Default for Issuer {
    fn default() -> Issuer {
        Issuer::production()
    }
}

#[derive(Clone, Copy)]
struct Credential {
    t: Scalar,
    u: Element,
    v: Element,
}

#[derive(Clone)]
pub(in crate::signal) struct AuthCredential {
    credential: Credential,
    aci: Uid,
    pni: Uid,
    pub(in crate::signal) issuer: Issuer,
    pub(in crate::signal) redemption: u64,
}

impl core::fmt::Debug for AuthCredential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AuthCredential").field("redemption", &self.redemption).finish_non_exhaustive()
    }
}

fn public_element(crypto: &impl Crypto, redemption: u64) -> Element {
    let mut sho = Sho::new(crypto, LABEL);
    sho.absorb(crypto, &redemption.to_be_bytes());
    sho.element(crypto)
}

fn issuance() -> Statement {
    let names: Vec<(String, String, String)> =
        (0..ATTRIBUTES).map(|at| (format!("y{at}"), format!("G_y{at}"), format!("M{at}"))).collect();
    let mut statement = Statement::new();
    statement.add("C_W", &[("w", "G_w"), ("wprime", "G_wprime")]);
    let mut terms = alloc::vec![("x0", "G_x0"), ("x1", "G_x1")];
    terms.extend(names.iter().map(|(y, g_y, _)| (y.as_str(), g_y.as_str())));
    statement.add("G_V-I", &terms);
    let mut terms = alloc::vec![("w", "G_w"), ("x0", "U"), ("x1", "tU")];
    terms.extend(names.iter().map(|(y, _, m)| (y.as_str(), m.as_str())));
    statement.add("V", &terms);
    statement
}

fn issuance_points(issuer: &Issuer, credential: &Credential, attributes: &[Element; ATTRIBUTES]) -> Points {
    let system = system();
    let mut points: Points = [
        ("C_W", issuer.c_w),
        ("G_w", system.w),
        ("G_wprime", system.w_prime),
        ("G_V-I", system.v.sub(&issuer.i(ATTRIBUTES))),
        ("G_x0", system.x0),
        ("G_x1", system.x1),
        ("V", credential.v),
        ("U", credential.u),
        ("tU", credential.u.times(&credential.t)),
    ]
    .map(|(name, point)| (String::from(name), point))
    .into();
    for (at, element) in attributes.iter().enumerate() {
        points.insert(format!("G_y{at}"), system.y[at]);
        points.insert(format!("M{at}"), *element);
    }
    points
}

fn verify_issuance(
    crypto: &impl Crypto,
    bytes: &[u8],
    uids: (&Uid, &Uid),
    day: u64,
    issuer: &Issuer,
) -> Option<Credential> {
    let mut reader = Reader(bytes);
    if reader.take(1)? != [ISSUANCE_VERSION] {
        return None;
    }
    let credential = Credential { t: reader.scalar()?, u: reader.element()?, v: reader.element()? };
    let proof = reader.bytes()?;
    if !reader.0.is_empty() || !day.is_multiple_of(SECONDS_PER_DAY) {
        return None;
    }
    let (aci, pni) = uids;
    let attributes = [public_element(crypto, day), aci.m1, aci.m2, pni.m1, pni.m2];
    let points = issuance_points(issuer, &credential, &attributes);
    issuance().verify(crypto, proof, &points, &[]).then_some(credential)
}

pub(in crate::signal) fn parse_credential(
    crypto: &impl Crypto,
    document: &str,
    (aci, salt): ([u8; 16], Option<&[u8]>),
    day: u64,
    issuer: &Issuer,
) -> Option<AuthCredential> {
    let item = json::items(document, &["credentials"])
        .find(|item| json::number_at::<u64>(item, &["redemptionTime"]) == Some(day))?;
    let bytes = unbase64(&json::unescape(json::text_at(item, &["credential"])?))?;
    let pni = match json::text_at(document, &["pni"]).and_then(parse_uuid) {
        Some(pni) => pni,
        None => false_pni(crypto, &aci, salt?),
    };
    let (aci, pni) = (Uid::new(crypto, &ServiceId::Aci(aci)), Uid::new(crypto, &ServiceId::Pni(pni)));
    let credential = verify_issuance(crypto, &bytes, (&aci, &pni), day, issuer)?;
    Some(AuthCredential { credential, aci, pni, issuer: *issuer, redemption: day })
}

fn presentation() -> Statement {
    let key = UID_DOMAIN;
    let (a1, a2, z1) = (format!("a1_{key}"), format!("a2_{key}"), format!("z1_{key}"));
    let mut statement = Statement::new();
    statement.add("Z", &[("z", "I")]);
    statement.add("C_x1", &[("t", "C_x0"), ("z0", "G_x0"), ("z", "G_x1")]);
    statement.add("0", &[(&z1, "I"), (&a1, "Z")]);
    statement.add(&format!("A_{key}"), &[(&a1, &format!("G_a1_{key}")), (&a2, &format!("G_a2_{key}"))]);
    for (first, second) in [(1, 2), (3, 4)] {
        statement.add(&format!("E_A{first}"), &[(&a1, &format!("C_y{first}")), (&z1, &format!("G_y{first}"))]);
        statement.add(
            &format!("C_y{second}-E_A{second}"),
            &[("z", &format!("G_y{second}")), (&a2, &format!("-E_A{first}"))],
        );
    }
    statement.add("C_y0", &[("z", "G_y0")]);
    statement
}

pub(in crate::signal) fn present(
    crypto: &impl Crypto,
    credential: &AuthCredential,
    key: &KeyPair,
    randomness: &[u8; RANDOMNESS],
) -> Option<Vec<u8>> {
    let system = system();
    let mut sho = Sho::new(crypto, PRESENTATION);
    sho.absorb(crypto, randomness);
    let z = sho.scalar(crypto);
    let Credential { t, u, v } = credential.credential;
    let attributes = [Element::IDENTITY, credential.aci.m1, credential.aci.m2, credential.pni.m1, credential.pni.m2];
    let c_y: Vec<Element> = system.y.iter().zip(&attributes).map(|(g_y, m)| g_y.times(&z).add(m)).collect();
    let c_x0 = system.x0.times(&z).add(&u);
    let c_x1 = system.x1.times(&z).add(&u.times(&t));
    let c_v = system.v.times(&z).add(&v);
    let i = credential.issuer.i(ATTRIBUTES);
    let domain = UID_DOMAIN;
    let scalars: Scalars = [
        (String::from("z"), z),
        (String::from("t"), t),
        (String::from("z0"), z.neg().mul(t)),
        (format!("a1_{domain}"), key.a1),
        (format!("a2_{domain}"), key.a2),
        (format!("z1_{domain}"), z.neg().mul(key.a1)),
    ]
    .into();
    let [g_a1, g_a2] = generators(UID_GENERATORS);
    let mut points: Points = [
        (String::from("I"), i),
        (String::from("C_x0"), c_x0),
        (String::from("C_x1"), c_x1),
        (String::from("G_x0"), system.x0),
        (String::from("G_x1"), system.x1),
        (String::from("0"), Element::IDENTITY),
        (format!("G_a1_{domain}"), g_a1),
        (format!("G_a2_{domain}"), g_a2),
        (format!("A_{domain}"), key.public),
        (String::from("C_y0"), c_y[0]),
        (String::from("Z"), i.times(&z)),
    ]
    .into();
    for (at, g_y) in system.y.iter().take(ATTRIBUTES).enumerate() {
        points.insert(format!("G_y{at}"), *g_y);
    }
    let ciphertexts = [key.encrypt(&credential.aci), key.encrypt(&credential.pni)];
    for ((first, second), [e_a1, e_a2]) in [(1, 2), (3, 4)].into_iter().zip(ciphertexts) {
        points.insert(format!("C_y{first}"), c_y[first]);
        points.insert(format!("E_A{first}"), e_a1);
        points.insert(format!("-E_A{first}"), e_a1.neg());
        points.insert(format!("C_y{second}-E_A{second}"), c_y[second].sub(&e_a2));
    }
    let mut proof_randomness = [0u8; RANDOMNESS];
    sho.squeeze(crypto, &mut proof_randomness);
    let proof = presentation().prove(crypto, &scalars, &points, &[], &proof_randomness)?;
    let mut bytes = alloc::vec![PRESENTATION_VERSION];
    [c_x0, c_x1, c_v].iter().for_each(|element| bytes.extend(element.encode()));
    bytes.extend((c_y.len() as u64).to_le_bytes());
    c_y.iter().for_each(|element| bytes.extend(element.encode()));
    bytes.extend((proof.len() as u64).to_le_bytes());
    bytes.extend(proof);
    ciphertexts.iter().flatten().for_each(|element| bytes.extend(element.encode()));
    bytes.extend(credential.redemption.to_le_bytes());
    Some(bytes)
}

#[cfg(test)]
pub(in crate::signal) mod tests {
    use super::super::super::address::uuid_to_text;
    use super::super::super::group::Keys;
    use super::super::super::tests::Exact;
    use super::*;
    use crate::base::code::base64;
    use crate::base::code::hex;

    pub(in crate::signal) struct Server {
        w: Scalar,
        w_prime: Scalar,
        x0: Scalar,
        x1: Scalar,
        y: [Scalar; MAX_ATTRIBUTES],
        pub(in crate::signal::zk) endorsement_root: Scalar,
        pub(in crate::signal) issuer: Issuer,
    }

    impl Server {
        pub(in crate::signal) fn new(crypto: &impl Crypto, randomness: &[u8; 32]) -> Server {
            let system = system();
            let mut sho = Sho::new(crypto, b"Signal_ZKCredential_CredentialPrivateKey_generate_20230410");
            sho.absorb(crypto, randomness);
            let w = sho.scalar(crypto);
            let (w_prime, x0, x1) = (sho.scalar(crypto), sho.scalar(crypto), sho.scalar(crypto));
            let y: [Scalar; MAX_ATTRIBUTES] = core::array::from_fn(|_| sho.scalar(crypto));
            let c_w = system.w.times(&w).add(&system.w_prime.times(&w_prime));
            let mut partial = system.v.sub(&system.x0.times(&x0)).sub(&system.x1.times(&x1));
            let i = core::array::from_fn(|at| {
                partial = partial.sub(&system.y[at + 1].times(&y[at + 1]));
                partial.sub(&system.y[0].times(&y[0]))
            });
            let mut sho = Sho::new(crypto, b"Signal_ZKCredential_Endorsements_ServerRootKeyPair_generate_20240207");
            sho.absorb(crypto, randomness);
            let endorsement_root = sho.scalar(crypto);
            let issuer = Issuer { c_w, i, endorsement_root: Element::BASE.times(&endorsement_root) };
            Server { w, w_prime, x0, x1, y, endorsement_root, issuer }
        }

        pub(in crate::signal) fn issue(
            &self,
            crypto: &impl Crypto,
            (aci, pni): (&ServiceId, &ServiceId),
            day: u64,
            randomness: &[u8; 32],
        ) -> Vec<u8> {
            let system = system();
            let (aci, pni) = (Uid::new(crypto, aci), Uid::new(crypto, pni));
            let attributes = [public_element(crypto, day), aci.m1, aci.m2, pni.m1, pni.m2];
            let mut sho = Sho::new(crypto, b"Signal_ZKCredential_Issuance_20230410");
            sho.absorb(crypto, randomness);
            let (t, u) = (sho.scalar(crypto), sho.element(crypto));
            let w = system.w.times(&self.w);
            let v = attributes
                .iter()
                .zip(&self.y)
                .fold(w.add(&u.times(&self.x1.mul_add(t, self.x0))), |v, (m, y)| v.add(&m.times(y)));
            let credential = Credential { t, u, v };
            let mut scalars: Scalars = [("w", self.w), ("wprime", self.w_prime), ("x0", self.x0), ("x1", self.x1)]
                .map(|(name, scalar)| (String::from(name), scalar))
                .into();
            for (at, y) in self.y.iter().take(ATTRIBUTES).enumerate() {
                scalars.insert(format!("y{at}"), *y);
            }
            let points = issuance_points(&self.issuer, &credential, &attributes);
            let mut proof_randomness = [0u8; RANDOMNESS];
            sho.squeeze(crypto, &mut proof_randomness);
            let proof = issuance().prove(crypto, &scalars, &points, &[], &proof_randomness).expect("a proof");
            let mut bytes = alloc::vec![ISSUANCE_VERSION];
            bytes.extend(t.bytes());
            bytes.extend(u.encode());
            bytes.extend(v.encode());
            bytes.extend((proof.len() as u64).to_le_bytes());
            bytes.extend(proof);
            bytes
        }
    }

    pub(in crate::signal) fn answer(credential: &[u8], day: u64, pni: Option<[u8; 16]>) -> String {
        let pni = pni.map_or_else(|| String::from("null"), |pni| format!("\"{}\"", uuid_to_text(&pni)));
        format!(
            r#"{{"credentials":[{{"credential":"{}","redemptionTime":{day}}}],"callLinkAuthCredentials":[],"pni":{pni}}}"#,
            base64(credential)
        )
    }

    fn sequence<const N: usize>(first: u8) -> [u8; N] {
        core::array::from_fn(|at| first.wrapping_add(at as u8))
    }

    #[test]
    fn production_params() {
        let params = unbase64(SERVER_PUBLIC_PARAMS).unwrap();
        assert_eq!(
            params.len(),
            ISSUER_AT + 7 * 32 + 32,
            "zkgroup's 673 bytes: the issuer's key, then the endorsements' root key"
        );
        let issuer = Issuer::production();
        assert_eq!(hex(&issuer.c_w.encode()), hex(&params[ISSUER_AT..ISSUER_AT + 32]));
        assert_eq!(hex(&issuer.endorsement_root.encode()), hex(&params[params.len() - 32..]), "the last element");
    }

    #[test]
    fn presentation_vector() {
        let crypto = Exact::default();
        let server = Server::new(&crypto, &sequence(0));
        let (aci, pni) = (sequence::<16>(0), sequence::<16>(100));
        let day = 123_456 * SECONDS_PER_DAY;
        let bytes = server.issue(&crypto, (&ServiceId::Aci(aci), &ServiceId::Pni(pni)), day, &sequence(200));
        let document = answer(&bytes, day, Some(pni));
        let credential =
            parse_credential(&crypto, &document, (aci, None), day, &server.issuer).expect("the day's credential");
        let key = Keys::new(&crypto, &sequence(100)).uid;
        let presentation = present(&crypto, &credential, &key, &sequence(3)).expect("a presentation");
        assert_eq!(
            hex(&presentation),
            "035e3e79afda8dc0d489fcf7c78f71e1502f2e06e8aeb20149046f85b3004d3f7f982d57dfad49cd1e6c335755cef4cc5e8d3de1eb4f5e8f24d71cf9f2220ae750f47181d71aaabbd48a1916813ec08eea935eb013395bf72f9139da8ef4f9530d05000000000000007a080544e6ee8ee2ff0dc298f18841103a9f9ec38631df8682e241755f86f74e26301872f4f32a9bb80f5b17651c0c83253a8013532384061a1febf79e58e60fa215b31da678305fa2a271655e35824630d0804680ec0bf29b1c775652683c3a5cec537c3514df730f267371d909f29cc6252af30afe3ea846c0cf56478bdc5b7a7f983ea7c24ecef4b371286a6414b2c38a57a7f59a9df33e430736c1a2ca14e00000000000000015416082ff7e3a741a4c3c31be3c95d4a31f2cf742685e0b17cd7f7205230e0e4e67b4b6ed45e705de13a1cb7170897bb32c9db6f9a1108fddfc7fae9eb2ca0c5fc3d8ccbd79d992eeed333626a1f0c37f0b25625955611e5ba33c782c50550045923582280cd93c3e9555b4e36eec20993f60b6aeb9ddb7f2856c4659546f037b33534a0292c77a501a70796f24ff37c8311bdfea8bb6c78f909563fe6e3b0386f36adc92090694ebb106a837bac046ad26e2472ee16408e9fd84269fd78c00c5dde91fcf202a6afad3441b9e2a34f4831d5bf560c81b38d951cb7c88e4d701765de9df4cfa5487f360e29e99343e91811baec331c4680985e608ca5d408e21725c6aa1b61d5a8b48d75f4aaa9a3cbe88d3e0f1a54319081f77c72c8f525474de749a0fef17b06bbce74ca5d0f7a0c45f443a1901f1e3e016d3548e50a2fa19ce6b27ac467ed9c9f5018b2a4456b6c2b1a91454422fdd473c9636a8459e1c170060c77b02000000",
            "zkgroup's own AUTH_CREDENTIAL_PRESENTATION_V4_RESULT"
        );
    }

    #[test]
    fn credential_checks() {
        let crypto = Exact::default();
        let server = Server::new(&crypto, &[1; 32]);
        let (aci, pni) = ([0xA1; 16], [0xB2; 16]);
        let day = 20_000 * SECONDS_PER_DAY;
        let bytes = server.issue(&crypto, (&ServiceId::Aci(aci), &ServiceId::Pni(pni)), day, &[2; 32]);
        let document = answer(&bytes, day, Some(pni));
        assert!(parse_credential(&crypto, &document, (aci, None), day, &server.issuer).is_some());
        assert!(parse_credential(&crypto, &document, ([0xA2; 16], None), day, &server.issuer).is_none(), "another ACI");
        assert!(
            parse_credential(&crypto, &answer(&bytes, day, Some([0xB3; 16])), (aci, None), day, &server.issuer)
                .is_none()
        );
        assert!(
            parse_credential(&crypto, &document, (aci, None), day + SECONDS_PER_DAY, &server.issuer).is_none(),
            "no credential for the day asked"
        );
        let other = Server::new(&crypto, &[9; 32]);
        assert!(
            parse_credential(&crypto, &document, (aci, None), day, &other.issuer).is_none(),
            "another issuer's key"
        );
        let no_pni = server.issue(
            &crypto,
            (&ServiceId::Aci(aci), &ServiceId::Pni(false_pni(&crypto, &aci, b"salt"))),
            day,
            &[3; 32],
        );
        let document = answer(&no_pni, day, None);
        assert!(
            parse_credential(&crypto, &document, (aci, Some(b"salt")), day, &server.issuer).is_some(),
            "an account without a PNI, by its salt"
        );
        assert!(
            parse_credential(&crypto, &document, (aci, Some(b"pepper")), day, &server.issuer).is_none(),
            "another salt"
        );
        assert!(parse_credential(&crypto, &document, (aci, None), day, &server.issuer).is_none(), "no salt");
    }
}

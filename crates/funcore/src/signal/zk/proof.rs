// poksho's proofs of knowledge, as libsignal makes them.

use super::super::Crypto;
use super::ristretto::Element;
use super::{Scalar, Sho, WIDE_BYTES};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const LABEL: &[u8] = b"POKSHO_Ristretto_SHOHMACSHA256";
const BASE: &str = "G";
pub(super) const RANDOMNESS: usize = 32;
const MAX_SCALARS: usize = 256;

pub(super) type Scalars = BTreeMap<String, Scalar>;
pub(super) type Points = BTreeMap<String, Element>;

pub(super) struct Statement {
    equations: Vec<(u8, Vec<(u8, u8)>)>,
    scalars: Vec<String>,
    points: Vec<String>,
}

fn index_of(names: &mut Vec<String>, name: &str) -> u8 {
    let at = names.iter().position(|other| other == name).unwrap_or_else(|| {
        names.push(name.to_string());
        names.len() - 1
    });
    u8::try_from(at).expect("a statement names at most 256 scalars and 256 points")
}

impl Statement {
    pub(super) fn new() -> Statement {
        Statement { equations: Vec::new(), scalars: Vec::new(), points: alloc::vec![String::from(BASE)] }
    }

    pub(super) fn add(&mut self, point: &str, terms: &[(&str, &str)]) {
        let lhs = index_of(&mut self.points, point);
        let terms = terms
            .iter()
            .map(|(scalar, point)| (index_of(&mut self.scalars, scalar), index_of(&mut self.points, point)))
            .collect();
        self.equations.push((lhs, terms));
    }

    fn description(&self) -> Vec<u8> {
        let mut bytes = alloc::vec![u8::try_from(self.equations.len()).expect("at most 255 equations")];
        for (point, terms) in &self.equations {
            bytes.extend([*point, u8::try_from(terms.len()).expect("at most 255 terms")]);
            bytes.extend(terms.iter().flat_map(|(scalar, point)| [*scalar, *point]));
        }
        bytes
    }

    fn sort_points(&self, points: &Points) -> Option<Vec<Element>> {
        if points.len() + 1 != self.points.len() {
            return None;
        }
        let rest = self.points[1..].iter().map(|name| points.get(name).copied());
        core::iter::once(Some(Element::BASE)).chain(rest).collect()
    }

    fn image(&self, scalars: &[Scalar], points: &[Element], challenge: Option<Scalar>) -> Vec<Element> {
        self.equations
            .iter()
            .map(|(lhs, terms)| {
                let sum = terms.iter().fold(Element::IDENTITY, |sum, (scalar, point)| {
                    sum.add(&points[usize::from(*point)].times(&scalars[usize::from(*scalar)]))
                });
                challenge.map_or(sum, |challenge| sum.sub(&points[usize::from(*lhs)].times(&challenge)))
            })
            .collect()
    }

    fn hash(&self, crypto: &impl Crypto, points: &[Element]) -> Sho {
        let encoded: Vec<[u8; 32]> = points.iter().map(Element::encode).collect();
        let description = self.description();
        let mut parts: Vec<&[u8]> = alloc::vec![&description];
        parts.extend(encoded.iter().map(|point| &point[..]));
        let mut sho = Sho::new(crypto, LABEL);
        sho.absorb_all(crypto, &parts);
        sho
    }

    fn challenge(crypto: &impl Crypto, mut sho: Sho, commitment: &[Element], message: &[u8]) -> Scalar {
        let encoded: Vec<[u8; 32]> = commitment.iter().map(Element::encode).collect();
        let mut parts: Vec<&[u8]> = encoded.iter().map(|point| &point[..]).collect();
        parts.push(message);
        sho.absorb_all(crypto, &parts);
        sho.scalar(crypto)
    }

    pub(super) fn prove(
        &self,
        crypto: &impl Crypto,
        scalars: &Scalars,
        points: &Points,
        message: &[u8],
        randomness: &[u8; RANDOMNESS],
    ) -> Option<Vec<u8>> {
        if scalars.len() != self.scalars.len() {
            return None;
        }
        let witness: Vec<Scalar> = self.scalars.iter().map(|name| scalars.get(name).copied()).collect::<Option<_>>()?;
        let elements = self.sort_points(points)?;
        let sho = self.hash(crypto, &elements);
        let mut nonce_sho = sho;
        let mut parts: Vec<&[u8]> = alloc::vec![randomness];
        parts.extend(witness.iter().map(|scalar| &scalar.bytes()[..]));
        nonce_sho.absorb_all(crypto, &parts);
        nonce_sho.absorb(crypto, message);
        let mut bytes = alloc::vec![0u8; WIDE_BYTES * witness.len()];
        nonce_sho.squeeze(crypto, &mut bytes);
        let nonce: Vec<Scalar> = bytes.as_chunks::<WIDE_BYTES>().0.iter().map(Scalar::from_wide).collect();
        let commitment = self.image(&nonce, &elements, None);
        let challenge = Statement::challenge(crypto, sho, &commitment, message);
        let mut proof = challenge.bytes().to_vec();
        for (secret, nonce) in witness.iter().zip(nonce) {
            proof.extend(secret.mul_add(challenge, nonce).bytes());
        }
        self.verify(crypto, &proof, points, message).then_some(proof)
    }

    pub(super) fn verify(&self, crypto: &impl Crypto, proof: &[u8], points: &Points, message: &[u8]) -> bool {
        let (chunks, rest) = proof.as_chunks::<32>();
        let scalars: Option<Vec<Scalar>> = chunks.iter().map(|chunk| Scalar::read(chunk)).collect();
        let (Some(scalars), Some(elements)) = (scalars, self.sort_points(points)) else { return false };
        let Some((challenge, responses)) = scalars.split_first() else { return false };
        if !rest.is_empty() || responses.len() != self.scalars.len() || responses.len() > MAX_SCALARS {
            return false;
        }
        let sho = self.hash(crypto, &elements);
        let commitment = self.image(responses, &elements, Some(*challenge));
        Statement::challenge(crypto, sho, &commitment, message) == *challenge
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::Exact;
    use super::*;
    use crate::base::code::hex;

    fn scalar_from(first: u8) -> Scalar {
        Scalar::from_wide(&core::array::from_fn(|at| first + at as u8))
    }

    #[test]
    fn statement_description() {
        let mut statement = Statement::new();
        statement.add("A", &[("a", "G")]);
        statement.add("B", &[("a", "H")]);
        assert_eq!(statement.description(), [2, 1, 1, 0, 0, 2, 1, 0, 3]);
    }

    #[test]
    fn proof_vector() {
        let crypto = Exact::default();
        let (a, b, c, d) = (scalar_from(10), scalar_from(20), scalar_from(30), scalar_from(40));
        let (h, i) = (Element::BASE.times(&scalar_from(50)), Element::BASE.times(&scalar_from(60)));
        let big_a = Element::BASE.times(&a).add(&h.times(&b)).add(&i.times(&c));
        let big_b = h.times(&c).add(&i.times(&d));
        let mut statement = Statement::new();
        statement.add("A", &[("a", "G"), ("b", "H"), ("c", "I")]);
        statement.add("B", &[("c", "H"), ("d", "I")]);
        assert_eq!(statement.description(), [2, 1, 3, 0, 0, 1, 2, 2, 3, 4, 2, 2, 2, 3, 3]);
        let scalars: Scalars =
            [("a", a), ("b", b), ("c", c), ("d", d)].map(|(name, scalar)| (name.into(), scalar)).into();
        let points: Points =
            [("A", big_a), ("B", big_b), ("H", h), ("I", i)].map(|(name, point)| (name.into(), point)).into();
        let randomness: [u8; 32] = core::array::from_fn(|at| at as u8);
        let mut extra = scalars.clone();
        extra.insert("abc".into(), a);
        assert_eq!(statement.prove(&crypto, &extra, &points, &[], &randomness), None, "a scalar more");
        let proof = statement.prove(&crypto, &scalars, &points, &[], &randomness).expect("a proof");
        assert_eq!(
            hex(&proof),
            "8efc676c33e6b2d0670ed5461a507f6a4bc9153e261db80fa438f3cd80a5c909b113cc0d7990ad616d0a2fc4b831d06357a5ee5d36d44b3427c7901061180c0fb1798c51680fe21b9f98e97955b1597c49314725c1546a369328cf54daae710bfc4a9911422aa77ed6d7231de3003ba5ae9d9fd0c53ced7ad782e29b04684a07221a6ef47ce61d817f01117cf59df69ac35b5bb590f1f7b6d029717bc1a62501",
            "poksho's own vector"
        );
        assert!(statement.verify(&crypto, &proof, &points, &[]));
        let mut extra_points = points.clone();
        extra_points.insert("xyz".into(), big_a);
        assert!(!statement.verify(&crypto, &proof, &extra_points, &[]), "a point more");
        assert!(!statement.verify(&crypto, &proof, &points, &randomness), "another message");
        assert!(!statement.verify(&crypto, &[&proof[..], &[0]].concat(), &points, &[]), "a byte more");
        let mut corrupt = proof.clone();
        *corrupt.last_mut().unwrap() += 1;
        assert!(!statement.verify(&crypto, &corrupt, &points, &[]), "a byte changed");
        assert!(!statement.verify(&crypto, &proof[..proof.len() - 32], &points, &[]), "a response short");
    }
}

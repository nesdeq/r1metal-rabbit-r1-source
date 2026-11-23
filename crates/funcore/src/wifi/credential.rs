//! The credential a network's password derives for a join, and its cache.

use crate::wifi::known::Network;
use crate::wifi::rsn;
use crate::wifi::rsn::Akm;
use crate::wifi::sae::POINT;
use crate::wifi::sae::point;
use crate::wifi::sae::point::Field;
use crate::wifi::wpa::Crypto;
use crate::wifi::wpa::PMK;

/// Why SAE has no curve to prove a password over.
pub const NO_CURVE: &str = "the library has no memory for the curve";
const NO_POINT: &str = "the password gives no point of the curve";

/// The secret a password derives: the four way handshake's PMK, or SAE's PWE between a station and one access point.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    Pmk([u8; PMK]),
    Pwe([u8; POINT]),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SaePeers {
    access: [u8; 6],
    station: [u8; 6],
    is_hashed: bool,
}

/// The last derived credential; a join of the same network between the same peers is spared the work.
pub struct Cache {
    network: Network,
    peers: Option<SaePeers>,
    credential: Credential,
}

impl Cache {
    fn find(&self, network: &Network, peers: Option<SaePeers>) -> Option<&Credential> {
        (self.network == *network && self.peers == peers).then_some(&self.credential)
    }
}

/// The credential of a join of `network` at `access`, which offers `rsn`: none for an open network, the PWE over `field` where SAE proves the password, else the PMK; `cache` serves the same join and is refilled. The error is why SAE has no point to prove the password over.
pub fn derive<F: Field>(
    crypto: &impl Crypto,
    field: impl FnOnce() -> Option<F>,
    network: &Network,
    rsn: &[u8],
    access: [u8; 6],
    station: [u8; 6],
    cache: &mut Option<Cache>,
) -> Result<Option<Credential>, &'static str> {
    if !network.is_locked() {
        return Ok(None);
    }
    let peers = rsn::choose(rsn).and_then(|choice| match choice.akm {
        Akm::Sae { is_hashed } => Some(SaePeers { access, station, is_hashed }),
        Akm::Psk | Akm::PskSha256 => None,
    });
    if let Some(credential) = cache.as_ref().and_then(|cache| cache.find(network, peers)) {
        return Ok(Some(credential.clone()));
    }
    let (password, name) = (network.password.as_bytes(), network.name.as_bytes());
    let credential = match peers {
        None => Credential::Pmk(crypto.derive_pmk(password, name)),
        Some(SaePeers { access, station, is_hashed }) => {
            let field = field().ok_or(NO_CURVE)?;
            let pwe = if is_hashed {
                point::hash_to_element(crypto, &field, name, password, &access, &station)
            } else {
                point::hunt_and_peck(crypto, &field, password, &access, &station)
            };
            Credential::Pwe(pwe.ok_or(NO_POINT)?)
        }
    };
    *cache = Some(Cache { network: network.clone(), peers, credential: credential.clone() });
    Ok(Some(credential))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::sae::point::tests::Toy as ToyField;
    use crate::wifi::wpa::tests::NETWORK;
    use crate::wifi::wpa::tests::PLAIN;
    use crate::wifi::wpa::tests::STATION;
    use crate::wifi::wpa::tests::Sums;
    use crate::wifi::wpa::tests::WPA3;
    use alloc::string::String;
    use core::cell::Cell;

    #[test]
    fn credential_by_offer() {
        let (field, crypto) = (|| Some(ToyField(Cell::new(0))), ToyField(Cell::new(0)));
        let no_field = || -> Option<ToyField> { panic!("no field is made where SAE does not prove the password") };
        let home = Network { name: "home".into(), password: "secret12".into() };
        let mut cache = None;
        let open = Network { password: String::new(), ..home.clone() };
        assert_eq!(
            derive(&crypto, no_field, &open, &PLAIN, NETWORK, STATION, &mut cache).map(|proof| proof.is_none()),
            Ok(true),
            "a network that asks for no password is joined with no proof"
        );
        assert!(cache.is_none(), "and nothing is worked");
        let key = Credential::Pmk(Sums.derive_pmk(b"secret12", b"home"));
        assert!(
            derive(&crypto, no_field, &home, &PLAIN, NETWORK, STATION, &mut cache) == Ok(Some(key.clone())),
            "the handshake starts from the password's key"
        );
        assert!(cache.as_ref().and_then(|cache| cache.find(&home, None)) == Some(&key), "which is kept");
        let mut filled = Some(Cache { network: home.clone(), peers: None, credential: Credential::Pmk([1; PMK]) });
        assert!(
            derive(&crypto, no_field, &home, &PLAIN, NETWORK, STATION, &mut filled)
                == Ok(Some(Credential::Pmk([1; PMK]))),
            "a key kept for the same join is taken as it is"
        );
        let other = Network { password: "secret13".into(), ..home.clone() };
        assert!(
            derive(&crypto, no_field, &other, &PLAIN, NETWORK, STATION, &mut cache)
                == Ok(Some(Credential::Pmk(Sums.derive_pmk(b"secret13", b"home")))),
            "and worked anew for another password"
        );
        let toy = ToyField(Cell::new(0));
        let hash_point = point::hash_to_element(&toy, &toy, b"home", b"secret12", &NETWORK, &STATION).unwrap();
        assert!(
            derive(&crypto, field, &home, &WPA3, NETWORK, STATION, &mut cache) == Ok(Some(Credential::Pwe(hash_point))),
            "SAE proves it over the point hashed to, where the offer says so"
        );
        assert!(
            derive(&crypto, no_field, &home, &WPA3, NETWORK, STATION, &mut cache)
                == Ok(Some(Credential::Pwe(hash_point))),
            "and a join between the same is spared the work"
        );
        let loop_point = point::hunt_and_peck(&toy, &toy, b"secret12", &NETWORK, &STATION).unwrap();
        assert!(
            derive(&crypto, field, &home, &WPA3[..22], NETWORK, STATION, &mut cache)
                == Ok(Some(Credential::Pwe(loop_point))),
            "or over the point found by looping where it does not"
        );
        let other_station = derive(&crypto, field, &home, &WPA3[..22], NETWORK, [2, 0, 0, 0, 0, 7], &mut cache);
        assert!(
            matches!(other_station, Ok(Some(Credential::Pwe(point))) if point != loop_point),
            "another station has another point"
        );
        assert_eq!(
            derive(&crypto, || -> Option<ToyField> { None }, &home, &WPA3, [2, 0, 0, 0, 0, 8], STATION, &mut cache)
                .err(),
            Some("the library has no memory for the curve"),
            "without the field there is no point"
        );
    }
}

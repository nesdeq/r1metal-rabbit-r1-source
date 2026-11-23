use super::Server;
use crate::base::code::unbase64;
use crate::net::stun::{Attribute, Crypto, Integrity, Message, Writer};
use alloc::vec::Vec;

// What a server prepends to its nonce to tell that it follows RFC 8489, and the four characters of its features after it (9.2).
const COOKIE: &[u8] = b"obMatJos2";
const FEATURES_BYTES: usize = 4;
// The security features, bit 0 the highest of the first byte (RFC 8489, 18.1).
const PASSWORD_ALGORITHMS: u32 = 1 << 23;
const USERNAME_ANONYMITY: u32 = 1 << 22;
// The password algorithms (RFC 8489, 18.5).
const MD5: u16 = 0x0001;
const SHA256: u16 = 0x0002;
const ALGORITHM_HEAD: usize = 4;
// The most a realm or a nonce holds when read (RFC 8489, 14.9, 14.10).
const MOST_BYTES: usize = 763;

// The long-term credential as a server challenged for it (RFC 8489, 9.2): its realm and nonce, the key, and what the requests carry of them. It has no `Debug`, as it holds the key.
pub(super) struct Auth {
    realm: Vec<u8>,
    nonce: Vec<u8>,
    key: Vec<u8>,
    algorithms: Option<Vec<u8>>,
    algorithm: Option<u16>,
    userhash: Option<[u8; 32]>,
}

// The security features a nonce's cookie tells; none where it has no cookie.
fn features(nonce: &[u8]) -> u32 {
    let Some(rest) = nonce.strip_prefix(COOKIE) else { return 0 };
    let text = rest.get(..FEATURES_BYTES).and_then(|text| core::str::from_utf8(text).ok());
    match text.and_then(unbase64).as_deref() {
        Some([first, second, third]) => u32::from_be_bytes([0, *first, *second, *third]),
        _ => 0,
    }
}

// True for an answer whose nonce asks for password algorithms and that lists none: it is not believed (RFC 8489, 9.2.5).
pub(super) fn is_downgraded(message: &Message<'_>) -> bool {
    message.nonce.is_some_and(|nonce| features(nonce) & PASSWORD_ALGORITHMS != 0) && message.algorithms.is_none()
}

// The algorithms a PASSWORD-ALGORITHMS lists, in its order; none where it is malformed.
fn listed(value: &[u8]) -> Option<Vec<u16>> {
    let mut numbers = Vec::new();
    let mut at = 0;
    while at < value.len() {
        let number = u16::from_be_bytes([*value.get(at)?, *value.get(at + 1)?]);
        let length = usize::from(u16::from_be_bytes([*value.get(at + 2)?, *value.get(at + 3)?]));
        at += ALGORITHM_HEAD + length.next_multiple_of(ALGORITHM_HEAD);
        numbers.push(number);
    }
    (at == value.len()).then_some(numbers)
}

impl Auth {
    // The credential a challenge (401) asks for; none where it must not be answered: no realm or nonce, a cookie that asks for algorithms the challenge lists none of, or none this side knows (RFC 8489, 9.2.5).
    pub(super) fn new(crypto: &impl Crypto, server: &Server, message: &Message<'_>) -> Option<Auth> {
        let (realm, nonce) = (message.realm?, message.nonce?);
        if realm.len() > MOST_BYTES || nonce.len() > MOST_BYTES || is_downgraded(message) {
            return None;
        }
        let features = features(nonce);
        let algorithm = match message.algorithms {
            Some(value) => {
                let numbers = listed(value)?;
                // SHA-256 where both are listed (RFC 8656, 6).
                Some(if numbers.contains(&SHA256) { SHA256 } else { numbers.contains(&MD5).then_some(MD5)? })
            }
            None => None,
        };
        let user = server.username.as_bytes();
        let parts: [&[u8]; 5] = [user, b":", realm, b":", server.password.as_bytes()];
        let key = if algorithm == Some(SHA256) { crypto.sha256(&parts).to_vec() } else { crypto.md5(&parts).to_vec() };
        let userhash = (features & USERNAME_ANONYMITY != 0).then(|| crypto.sha256(&[user, b":", realm]));
        Some(Auth {
            realm: realm.to_vec(),
            nonce: nonce.to_vec(),
            key,
            algorithms: message.algorithms.map(<[u8]>::to_vec),
            algorithm,
            userhash,
        })
    }

    // True where the challenge is for the realm this credential is already of: answering it again changes nothing (RFC 8489, 9.2.5).
    pub(super) fn is_of(&self, message: &Message<'_>) -> bool {
        message.realm == Some(&self.realm[..])
    }

    // Takes the new nonce of a 438; false where it carries none.
    pub(super) fn renew(&mut self, message: &Message<'_>) -> bool {
        match message.nonce.filter(|nonce| nonce.len() <= MOST_BYTES) {
            Some(nonce) => {
                self.nonce = nonce.to_vec();
                true
            }
            None => false,
        }
    }

    // Adds the credential's attributes to a request; its integrity comes with `integrity`.
    pub(super) fn sign(&self, server: &Server, writer: &mut Writer) {
        match &self.userhash {
            Some(hash) => writer.push(Attribute::Userhash(hash)),
            None => writer.push(Attribute::Username(server.username.as_bytes())),
        };
        writer.push(Attribute::Realm(&self.realm)).push(Attribute::Nonce(&self.nonce));
        if let (Some(list), Some(algorithm)) = (&self.algorithms, self.algorithm) {
            writer.push(Attribute::Algorithms(list)).push(Attribute::Algorithm(algorithm));
        }
    }

    // A server that listed its algorithms takes MESSAGE-INTEGRITY-SHA256 alone from then on (RFC 8489, 9.2.5).
    pub(super) fn integrity(&self) -> Integrity<'_> {
        if self.algorithms.is_some() { Integrity::Sha256(&self.key) } else { Integrity::Sha1(&self.key) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::stun::tests::Exact;
    use crate::net::stun::{ALLOCATE, Class, MD5_BYTES, Transport};
    use alloc::string::ToString;
    use core::cell::Cell;
    use core::time::Duration;

    fn server() -> Server {
        Server {
            address: "192.0.2.15:3478".parse().unwrap(),
            transport: Transport::Udp,
            name: None,
            username: "user".to_string(),
            password: "pass".to_string(),
            expires: Duration::from_secs(3600),
        }
    }

    fn challenge(nonce: &[u8], algorithms: Option<&[u8]>) -> Vec<u8> {
        let mut writer = Writer::new(ALLOCATE, Class::Error, &[1; 12]);
        writer.push(Attribute::Error(401)).push(Attribute::Realm(b"realm")).push(Attribute::Nonce(nonce));
        if let Some(list) = algorithms {
            writer.push(Attribute::Algorithms(list));
        }
        writer.finish(true)
    }

    #[test]
    fn long_term_key() {
        let crypto = Exact(Cell::new(1));
        let bytes = challenge(b"plain", None);
        let auth = Auth::new(&crypto, &server(), &Message::parse(&bytes).unwrap()).unwrap();
        let key: [u8; MD5_BYTES] = auth.key.clone().try_into().unwrap();
        assert_eq!(crate::base::code::hex(&key), "8493fbc53ba582fb4c044c456bdc40eb", "RFC 8489, 9.2.2's example");
        assert!(matches!(auth.integrity(), Integrity::Sha1(_)) && auth.userhash.is_none());
    }

    #[test]
    fn cookie_features() {
        let crypto = Exact(Cell::new(1));
        assert_eq!(features(b"obMatJos2gAAAnonce"), PASSWORD_ALGORITHMS, "the first bit");
        assert_eq!(features(b"obMatJos2wAAA"), PASSWORD_ALGORITHMS | USERNAME_ANONYMITY);
        assert_eq!([features(b"obMatJos2AAAA"), features(b"obMatJos2!!"), features(b"nonce")], [0, 0, 0]);
        let asked = challenge(b"obMatJos2gAAAnonce", None);
        assert!(
            Auth::new(&crypto, &server(), &Message::parse(&asked).unwrap()).is_none(),
            "algorithms asked for, none listed"
        );
        let both = [0, 1, 0, 0, 0, 2, 0, 0];
        let listed = challenge(b"obMatJos2wAAAnonce", Some(&both));
        let auth = Auth::new(&crypto, &server(), &Message::parse(&listed).unwrap()).unwrap();
        assert_eq!(auth.algorithm, Some(SHA256), "SHA-256 where both are listed");
        assert_eq!(auth.key, crypto.sha256(&[b"user:realm:pass"]).to_vec());
        assert_eq!(auth.userhash, Some(crypto.sha256(&[b"user:realm"])), "the user's name hashed for anonymity");
        assert!(matches!(auth.integrity(), Integrity::Sha256(_)));
        let other = challenge(b"obMatJos2gAAAnonce", Some(&[0, 9, 0, 0]));
        assert!(Auth::new(&crypto, &server(), &Message::parse(&other).unwrap()).is_none(), "no algorithm known");
        assert_eq!(
            super::listed(&[0, 1, 0, 3, 7, 7, 7, 0, 0, 2, 0, 0]),
            Some(alloc::vec![MD5, SHA256]),
            "parameters padded"
        );
        assert_eq!(super::listed(&[0, 1, 0, 9]), None);
    }
}

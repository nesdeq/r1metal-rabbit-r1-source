use super::SECRET_BYTES;
use crate::base::bytes::{be32, same};
use crate::signal::{Crypto, hkdf};
use alloc::vec::Vec;

// What RingRTC draws from a secret: the two keys of a frame and the next secret (`core/crypto.rs`).
const AES_INFO: &[u8] = b"RingRTC AES Key";
const HMAC_INFO: &[u8] = b"RingRTC HMAC Key";
const RATCHET_INFO: &[u8] = b"RingRTC Ratchet";
const KEY_BYTES: usize = 32;
const IV_BYTES: usize = 16;
const MAC_BYTES: usize = 16;
const COUNTER_BYTES: usize = 4;
// Behind a frame's ciphertext: the ratchet counter, the frame counter and the MAC (`group_call.rs`, `encrypt_data`).
pub(crate) const TAIL_BYTES: usize = 1 + COUNTER_BYTES + MAC_BYTES;
// `MAX_RECEIVER_STATES_TO_RETAIN`.
pub(crate) const MOST_SECRETS: usize = 5;
// `MAX_OOO_FRAMES`: how far a frame may lie behind the one a secret was last advanced at and still open.
const LATE_FRAMES: u64 = 300;
// `MAX_OOO_RATCHETS`.
const LATE_RATCHETS: u8 = 5;

type Secret = [u8; SECRET_BYTES];

fn advance(crypto: &impl Crypto, secret: &Secret) -> Option<Secret> {
    hkdf(crypto, None, secret, &[RATCHET_INFO])
}

// The counter block of a frame: its counter, highest byte first, then noughts that count the blocks.
fn iv(counter: u64) -> [u8; IV_BYTES] {
    let mut iv = [0u8; IV_BYTES];
    iv[..size_of::<u64>()].copy_from_slice(&counter.to_be_bytes());
    iv
}

// A secret at a step of its ratchet with the keys drawn from it; it has no `Debug`.
#[derive(Clone)]
struct Keys {
    secret: Secret,
    aes: [u8; KEY_BYTES],
    hmac: [u8; KEY_BYTES],
    ratchet: u8,
}

impl Keys {
    fn new(crypto: &impl Crypto, ratchet: u8, secret: Secret) -> Option<Keys> {
        Some(Keys {
            aes: hkdf(crypto, None, &secret, &[AES_INFO])?,
            hmac: hkdf(crypto, None, &secret, &[HMAC_INFO])?,
            secret,
            ratchet,
        })
    }

    fn mac(&self, crypto: &impl Crypto, counter: u64, cipher: &[u8]) -> [u8; KEY_BYTES] {
        let length = (cipher.len() as u32).to_be_bytes();
        crypto.hmac_sha256(&self.hmac, &[&iv(counter), &length, cipher, &[0; 4]])
    }

    fn crypt(&self, crypto: &impl Crypto, counter: u64, data: &[u8]) -> Option<Vec<u8>> {
        crypto.ctr(&self.aes, &iv(counter), data)
    }
}

// What seals the frames this side sends in a group call: audio, video and data under one counter; it has no `Debug`.
pub(crate) struct Outbound {
    keys: Keys,
    next: u64,
}

impl Outbound {
    // None where the library fails.
    pub(crate) fn new(crypto: &impl Crypto, secret: Secret) -> Option<Outbound> {
        Some(Outbound { keys: Keys::new(crypto, 0, secret)?, next: 1 })
    }

    // The frame encrypted, with its tail; none once the counter no longer fits its four bytes, or where the library fails.
    pub(crate) fn seal(&mut self, crypto: &impl Crypto, frame: &[u8]) -> Option<Vec<u8>> {
        let counter = u32::try_from(self.next).ok()?;
        let mut sealed = self.keys.crypt(crypto, self.next, frame)?;
        let mac = self.keys.mac(crypto, self.next, &sealed);
        sealed.push(self.keys.ratchet);
        sealed.extend(counter.to_be_bytes());
        sealed.extend(&mac[..MAC_BYTES]);
        self.next += 1;
        Some(sealed)
    }

    // The ratchet's step and the secret a member opens this side's frames with.
    pub(crate) const fn state(&self) -> (u8, Secret) {
        (self.keys.ratchet, self.keys.secret)
    }

    // Steps the ratchet on, for a device that joined: it opens nothing sent before; false where the library fails.
    pub(crate) fn advance(&mut self, crypto: &impl Crypto) -> bool {
        let next = advance(crypto, &self.keys.secret)
            .and_then(|secret| Keys::new(crypto, self.keys.ratchet.wrapping_add(1), secret));
        next.map(|keys| self.keys = keys).is_some()
    }

    // Takes a new secret at step nought, for a member that left; false where the library fails.
    pub(crate) fn reset(&mut self, crypto: &impl Crypto, secret: Secret) -> bool {
        Keys::new(crypto, 0, secret).map(|keys| self.keys = keys).is_some()
    }
}

// A secret a far device sent, as far as its ratchet was followed, and where it stood before the last frame that moved it.
#[derive(Clone)]
struct Kept {
    keys: Keys,
    moved_at: u64,
    old: Secret,
    old_ratchet: u8,
}

impl Kept {
    // The secret stepped to a frame's ratchet counter: from where it stands for a later frame, from where it stood for one that lies behind.
    fn stepped(&self, crypto: &impl Crypto, ratchet: u8, counter: u64) -> Option<Kept> {
        let is_later = counter > self.moved_at;
        let (mut at, mut secret) =
            if is_later { (self.keys.ratchet, self.keys.secret) } else { (self.old_ratchet, self.old) };
        while at != ratchet {
            (at, secret) = (at.wrapping_add(1), advance(crypto, &secret)?);
        }
        let (old, old_ratchet) = if counter.wrapping_sub(self.moved_at) > LATE_FRAMES {
            (self.keys.secret, self.keys.ratchet)
        } else {
            (self.old, self.old_ratchet)
        };
        Some(Kept { keys: Keys::new(crypto, ratchet, secret)?, moved_at: counter, old, old_ratchet })
    }

    // Brings what is kept for late frames within its reach of the ratchet.
    fn trim(&mut self, crypto: &impl Crypto) -> Option<()> {
        while self.keys.ratchet.wrapping_sub(self.old_ratchet) > LATE_RATCHETS {
            (self.old, self.old_ratchet) = (advance(crypto, &self.old)?, self.old_ratchet.wrapping_add(1));
        }
        Some(())
    }
}

// What opens the frames of one far device: the secrets it sent, the newest first; it has no `Debug`.
#[derive(Default)]
pub(crate) struct Inbound {
    secrets: Vec<Kept>,
}

impl Inbound {
    // Takes a secret the device's owner sent, at its ratchet's step; false where the library fails.
    pub(crate) fn add(&mut self, crypto: &impl Crypto, ratchet: u8, secret: Secret) -> bool {
        let Some(keys) = Keys::new(crypto, ratchet, secret) else { return false };
        self.secrets.truncate(MOST_SECRETS - 1);
        self.secrets.insert(0, Kept { keys, moved_at: 0, old: secret, old_ratchet: ratchet });
        true
    }

    // The frame opened; none for one no secret opens. A secret at the frame's step is tried first, then each stepped to it (`Context::decrypt`).
    pub(crate) fn open(&mut self, crypto: &impl Crypto, sealed: &[u8]) -> Option<Vec<u8>> {
        let (cipher, tail) = sealed.split_at_checked(sealed.len().checked_sub(TAIL_BYTES)?)?;
        let (ratchet, counter, mac) = (tail[0], u64::from(be32(tail, 1)?), &tail[1 + COUNTER_BYTES..]);
        let holds = |keys: &Keys| same(&keys.mac(crypto, counter, cipher)[..MAC_BYTES], mac);
        if let Some(kept) = self.secrets.iter().find(|kept| kept.keys.ratchet == ratchet && holds(&kept.keys)) {
            return kept.keys.crypt(crypto, counter, cipher);
        }
        for kept in &mut self.secrets {
            let Some(mut stepped) = kept.stepped(crypto, ratchet, counter) else { continue };
            if holds(&stepped.keys) {
                stepped.trim(crypto)?;
                *kept = stepped;
                return kept.keys.crypt(crypto, counter, cipher);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::code::{hex, unhex};
    use crate::signal::tests::Exact;

    const PLAIN: &[u8] = b"Whan that Aprille with his shoures soote";

    fn secret() -> Secret {
        core::array::from_fn(|at| at as u8 + 1)
    }

    #[test]
    fn frames_as_ringrtc_seals() {
        let crypto = Exact::default();
        let keys = Keys::new(&crypto, 0, secret()).unwrap();
        assert_eq!(
            (hex(&keys.aes), hex(&keys.hmac), hex(&advance(&crypto, &secret()).unwrap())),
            (
                "7d75533e8ac3d21e0634edeae573faecf2bd83e9c00f920f92707bed23ea142a".into(),
                "69068d6d55f2981dc9208fd76f8f6035b469bf79f1bef772f5ec95c4d883954f".into(),
                "0774ab09e054a097ce8650332d457c126ff8ec3c319a3c6646cd00b26e8c9183".into()
            ),
            "HKDF-SHA256 without salt under the three labels"
        );
        let mut outbound = Outbound::new(&crypto, secret()).unwrap();
        let first = "3be8f59de0c88929b81391f78c0a32d679a25653f21b722a775f73744bca2f3dd9e4340b7610ff1700000000015fdb7f3531d1c55378577ae1ff08e3e8";
        assert_eq!(
            hex(&outbound.seal(&crypto, PLAIN).unwrap()),
            first,
            "AES-256-CTR from the counter's block, then ratchet nought, frame 1 and 16 bytes of HMAC-SHA256 over block, length, ciphertext and four noughts"
        );
        assert!(outbound.advance(&crypto) && outbound.advance(&crypto) && outbound.state().0 == 2, "two steps on");
        outbound.next = 0x0102_0304;
        let later = "402a4457531ef4dfedf917c0d34792d96816f769a5537986aa73bd0d3604987d31c5fddee063620c0201020304f0cd8055e1fe311c3a954242418e9105";
        assert_eq!(
            hex(&outbound.seal(&crypto, PLAIN).unwrap()),
            later,
            "at step 2, the frame counter highest byte first"
        );
        let mut inbound = Inbound::default();
        assert!(inbound.add(&crypto, 0, secret()));
        for sealed in [later, first, later] {
            assert_eq!(
                inbound.open(&crypto, &unhex(sealed).unwrap()).as_deref(),
                Some(PLAIN),
                "opened from the first secret: the later frame, the frame before the steps after it, and the later again"
            );
        }
    }

    #[test]
    fn seals_and_opens() {
        let crypto = Exact::default();
        let mut outbound = Outbound::new(&crypto, [7; 32]).unwrap();
        let mut inbound = Inbound::default();
        assert_eq!(inbound.open(&crypto, &outbound.seal(&crypto, PLAIN).unwrap()), None, "no secret yet");
        let (ratchet, secret) = outbound.state();
        assert!(inbound.add(&crypto, ratchet, secret));
        let sealed = outbound.seal(&crypto, PLAIN).unwrap();
        assert_eq!(sealed.len(), PLAIN.len() + TAIL_BYTES);
        assert_eq!(&sealed[PLAIN.len()..PLAIN.len() + 5], [0, 0, 0, 0, 2], "ratchet nought, the second frame");
        assert_eq!(inbound.open(&crypto, &sealed).as_deref(), Some(PLAIN));
        for at in [0, PLAIN.len() - 1, sealed.len() - 1] {
            let mut changed = sealed.clone();
            changed[at] ^= 1;
            assert_eq!(inbound.open(&crypto, &changed), None, "byte {at} changed: ciphertext or MAC");
        }
        assert_eq!(inbound.open(&crypto, &sealed[..TAIL_BYTES - 1]), None, "shorter than a tail");
        assert_eq!(
            inbound.open(&crypto, &outbound.seal(&crypto, &[]).unwrap()).as_deref(),
            Some(&[][..]),
            "a frame of nothing"
        );
        outbound.next = u64::from(u32::MAX);
        assert!(outbound.seal(&crypto, PLAIN).is_some(), "the last counter");
        assert!(outbound.seal(&crypto, PLAIN).is_none(), "then no more under this side's counter");
    }

    #[test]
    fn ratchet_and_new_secrets() {
        let crypto = Exact::default();
        let mut outbound = Outbound::new(&crypto, [7; 32]).unwrap();
        let mut early = Inbound::default();
        early.add(&crypto, 0, [7; 32]);
        let before = outbound.seal(&crypto, PLAIN).unwrap();
        assert!(outbound.advance(&crypto));
        let (ratchet, secret) = outbound.state();
        let mut late = Inbound::default();
        late.add(&crypto, ratchet, secret);
        let after = outbound.seal(&crypto, PLAIN).unwrap();
        assert_eq!(late.open(&crypto, &before), None, "a device that joined opens nothing sent before");
        assert_eq!(late.open(&crypto, &after).as_deref(), Some(PLAIN));
        assert_eq!(early.open(&crypto, &after).as_deref(), Some(PLAIN), "who was there follows the ratchet");
        assert_eq!(early.open(&crypto, &before).as_deref(), Some(PLAIN), "and still opens a frame that comes late");
        for _ in 0..=LATE_RATCHETS {
            assert!(outbound.advance(&crypto));
        }
        outbound.next += LATE_FRAMES + 1;
        let far = outbound.seal(&crypto, PLAIN).unwrap();
        assert_eq!(early.open(&crypto, &far).as_deref(), Some(PLAIN), "seven steps on");
        assert_eq!(early.open(&crypto, &before), None, "what lies that far behind opens no more");
        assert!(outbound.reset(&crypto, [9; 32]) && outbound.state() == (0, [9; 32]));
        let renewed = outbound.seal(&crypto, PLAIN).unwrap();
        assert_eq!(early.open(&crypto, &renewed), None, "a new secret the member was not sent");
        early.add(&crypto, 0, [9; 32]);
        assert_eq!(early.open(&crypto, &renewed).as_deref(), Some(PLAIN));
        assert_eq!(early.open(&crypto, &far).as_deref(), Some(PLAIN), "the secret before stays for what is on its way");
        for secret in 10..16 {
            early.add(&crypto, 0, [secret; 32]);
        }
        assert_eq!(early.secrets.len(), MOST_SECRETS, "the newest five");
        assert_eq!(early.open(&crypto, &renewed), None, "the oldest let go");
    }

    #[test]
    fn strangers_frames() {
        use crate::base::math::xorshift64;
        let crypto = Exact::default();
        let mut inbound = Inbound::default();
        inbound.add(&crypto, 250, [3; 32]);
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..300 {
            state = xorshift64(state);
            let frame: Vec<u8> = (0..state % 64).map(|at| (state >> (at % 57)) as u8).collect();
            assert_eq!(
                inbound.open(&crypto, &frame),
                None,
                "random bytes open under no secret, whatever step they name"
            );
        }
    }
}

//! A station's WPA2 and WPA3 handshakes: EAPOL-Key frames, key derivation, the four frames that prove the password and the two that renew the group key.

use crate::base::bytes::be16;
use crate::base::bytes::same;
use crate::crypto::Primitives;
use crate::crypto::SHA256_BYTES;
use crate::wifi::VENDOR;
use crate::wifi::elements;
use crate::wifi::rsn;
use crate::wifi::rsn::Akm;
use crate::wifi::rsn::Choice;
use crate::wifi::rsn::Group;
use alloc::vec::Vec;

pub const PMK: usize = 32;
const NONCE: usize = 32;
const MIC: usize = 16;
const KEY: usize = 16;
const SHA1_BYTES: usize = 20;
const EAPOL_VERSION: u8 = 1;
const EAPOL_KEY: u8 = 3;
const EAPOL_HEAD: usize = 4;
const DESCRIPTOR: u8 = 2;
const DESCRIPTOR_AT: usize = 4;
const INFORMATION_AT: usize = 5;
const REPLAY_AT: usize = 9;
const NONCE_AT: usize = 17;
const SEQUENCE_AT: usize = 65;
const MIC_AT: usize = 81;
const DATA_LENGTH_AT: usize = 97;
const DATA_AT: usize = 99;
const VERSION_BITS: u16 = 0x0007;
const PAIRWISE: u16 = 1 << 3;
const INSTALL: u16 = 1 << 6;
const KEY_ACK: u16 = 1 << 7;
const KEY_MIC: u16 = 1 << 8;
const SECURE: u16 = 1 << 9;
const ENCRYPTED: u16 = 1 << 12;
const VERSION_SHA1: u16 = 2;
const VERSION_CMAC: u16 = 3;
const VERSION_AKM: u16 = 0;
const EXPANSION: &[u8] = b"Pairwise key expansion";
const PTK_BITS: u16 = 8 * 3 * KEY as u16;
const GROUP_KEY: u8 = 1;
const MANAGEMENT_KEY: u8 = 9;
const GROUP_KEY_HEAD: usize = 2;
const MANAGEMENT_KEY_HEAD: usize = 8;
const KEY_ID: u8 = 3;
const TKIP_MIC_AT: usize = 16;
const TKIP_MIC_KEY: usize = 8;
const PACKET_NUMBER: usize = 6;
/// PBKDF2's iterations of a password into a PMK.
pub const ROUNDS: u32 = 4096;
pub(crate) const PASSWORD_LENGTHS: (usize, usize) = (8, 63);

/// The arithmetic of the handshakes beside what every protocol takes, done by a library.
pub trait Crypto: Primitives {
    fn hmac_sha1(&self, key: &[u8], parts: &[&[u8]]) -> [u8; SHA1_BYTES];
    // AES-CMAC of a message given in parts.
    fn cmac(&self, key: &[u8; KEY], parts: &[&[u8]]) -> Option<[u8; MIC]>;
    // None if `wrapped` was not wrapped under that key by AES key wrap.
    fn unwrap_key(&self, key: &[u8; KEY], wrapped: &[u8]) -> Option<Vec<u8>>;
    // PBKDF2 over HMAC-SHA1 with the name as its salt, `ROUNDS` times.
    fn derive_pmk(&self, password: &[u8], name: &[u8]) -> [u8; PMK];
}

#[derive(Clone, PartialEq, Eq)]
struct Ptk {
    kck: [u8; KEY],
    kek: [u8; KEY],
    tk: [u8; KEY],
}

fn derive_ptk(
    crypto: &impl Crypto,
    akm: Akm,
    pmk: &[u8; PMK],
    addresses: (&[u8; 6], &[u8; 6]),
    nonces: (&[u8; NONCE], &[u8; NONCE]),
) -> Ptk {
    let (low, high) = if addresses.0 <= addresses.1 { addresses } else { (addresses.1, addresses.0) };
    let (low_nonce, high_nonce) = if nonces.0 <= nonces.1 { nonces } else { (nonces.1, nonces.0) };
    let mut output = [0u8; 2 * SHA256_BYTES];
    match akm {
        // The standard's PRF: HMAC-SHA1 over the label, a zero, the data and a count from 0, as many times as it takes.
        Akm::Psk => {
            for (count, part) in output[..3 * SHA1_BYTES].chunks_mut(SHA1_BYTES).enumerate() {
                let hash = crypto.hmac_sha1(pmk, &[EXPANSION, &[0], low, high, low_nonce, high_nonce, &[count as u8]]);
                part.copy_from_slice(&hash[..part.len()]);
            }
        }
        // The standard's KDF: HMAC-SHA256 over a count from 1, the label, the data and the bits wanted.
        Akm::PskSha256 | Akm::Sae { .. } => {
            for (count, part) in output.chunks_mut(SHA256_BYTES).enumerate() {
                let hash = crypto.hmac_sha256(
                    pmk,
                    &[
                        &(count as u16 + 1).to_le_bytes(),
                        EXPANSION,
                        low,
                        high,
                        low_nonce,
                        high_nonce,
                        &PTK_BITS.to_le_bytes(),
                    ],
                );
                part.copy_from_slice(&hash[..part.len()]);
            }
        }
    }
    let part = |at: usize| output[at * KEY..(at + 1) * KEY].try_into().expect("a part of the derived bytes");
    Ptk { kck: part(0), kek: part(1), tk: part(2) }
}

/// A group key: its number, its nl80211 cipher suite, the key, and the packet number the network has got to.
#[derive(Clone, PartialEq, Eq)]
pub struct GroupKey {
    pub index: u8,
    pub cipher: u32,
    pub key: Vec<u8>,
    pub sequence: [u8; PACKET_NUMBER],
}

impl core::fmt::Debug for GroupKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GroupKey").field("index", &self.index).field("sequence", &self.sequence).finish_non_exhaustive()
    }
}

/// What a frame that was taken has the station do, in this order.
#[derive(Clone, PartialEq, Eq)]
pub enum Step {
    Send(Vec<u8>),
    // Put the TK to use: the handshake has proved the password to both sides.
    Pairwise([u8; KEY]),
    Group(GroupKey),
    Management(GroupKey),
}

impl core::fmt::Debug for Step {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Step::Send(frame) => f.debug_tuple("Send").field(frame).finish(),
            Step::Pairwise(_) => f.write_str("Pairwise(..)"),
            Step::Group(key) => f.debug_tuple("Group").field(key).finish(),
            Step::Management(key) => f.debug_tuple("Management").field(key).finish(),
        }
    }
}

/// Why a frame was not taken; none of these ends the handshake, as a frame from anybody could be refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    // No key frame of RSN, or cut short.
    NotAKeyFrame,
    // Signed in a way this firmware does not speak or the AKM does not call for, or the library gave no MIC.
    Signature,
    // A frame the network sent before one that was taken already.
    Replayed,
    // A frame for a moment of the handshake that is not now.
    OutOfTurn,
    // Not signed with the key the password gives: a wrong password shows here, on the station's side.
    WrongKey,
    // Its data did not unwrap, or lacks the key it is to bring.
    NoKey,
    // The network's element in the handshake is not the one its beacon had: somebody stands in between.
    ElementDiffers,
    NoRandom,
}

struct Frame<'a> {
    information: u16,
    replay: u64,
    nonce: &'a [u8; NONCE],
    sequence: [u8; PACKET_NUMBER],
    data: &'a [u8],
}

fn parse_frame(frame: &[u8]) -> Option<Frame<'_>> {
    if *frame.get(1)? != EAPOL_KEY || *frame.get(DESCRIPTOR_AT)? != DESCRIPTOR {
        return None;
    }
    let data = frame.get(DATA_AT..DATA_AT + usize::from(be16(frame, DATA_LENGTH_AT)?))?;
    // The length the head gives is the frame's, however much came after it.
    if usize::from(be16(frame, 2)?) < DATA_AT - EAPOL_HEAD + data.len() {
        return None;
    }
    Some(Frame {
        information: be16(frame, INFORMATION_AT)?,
        replay: u64::from_be_bytes(frame.get(REPLAY_AT..REPLAY_AT + 8)?.try_into().ok()?),
        nonce: frame.get(NONCE_AT..NONCE_AT + NONCE)?.try_into().ok()?,
        sequence: frame.get(SEQUENCE_AT..SEQUENCE_AT + PACKET_NUMBER)?.try_into().ok()?,
        data,
    })
}

fn build_frame(information: u16, replay: u64, nonce: &[u8; NONCE], data: &[u8]) -> Vec<u8> {
    let mut frame = alloc::vec![0u8; DATA_AT];
    (frame[0], frame[1], frame[DESCRIPTOR_AT]) = (EAPOL_VERSION, EAPOL_KEY, DESCRIPTOR);
    frame[2..4].copy_from_slice(&((DATA_AT - EAPOL_HEAD + data.len()) as u16).to_be_bytes());
    frame[INFORMATION_AT..INFORMATION_AT + 2].copy_from_slice(&information.to_be_bytes());
    frame[REPLAY_AT..REPLAY_AT + 8].copy_from_slice(&replay.to_be_bytes());
    frame[NONCE_AT..NONCE_AT + NONCE].copy_from_slice(nonce);
    frame[DATA_LENGTH_AT..DATA_AT].copy_from_slice(&(data.len() as u16).to_be_bytes());
    frame.extend(data);
    frame
}

fn compute_mic(crypto: &impl Crypto, version: u16, key: &[u8; KEY], frame: &[u8]) -> Option<[u8; MIC]> {
    let end = EAPOL_HEAD + usize::from(be16(frame, 2)?);
    let (head, tail) = (frame.get(..MIC_AT)?, frame.get(MIC_AT + MIC..end)?);
    match version {
        VERSION_SHA1 => crypto.hmac_sha1(key, &[head, &[0; MIC], tail])[..MIC].try_into().ok(),
        VERSION_CMAC | VERSION_AKM => crypto.cmac(key, &[head, &[0; MIC], tail]),
        _ => None,
    }
}

fn sign(crypto: &impl Crypto, version: u16, key: &[u8; KEY], mut frame: Vec<u8>) -> Result<Vec<u8>, Error> {
    let mic = compute_mic(crypto, version, key, &frame).ok_or(Error::Signature)?;
    frame[MIC_AT..MIC_AT + MIC].copy_from_slice(&mic);
    Ok(frame)
}

fn descriptor_version(akm: Akm) -> u16 {
    match akm {
        Akm::Psk => VERSION_SHA1,
        Akm::PskSha256 => VERSION_CMAC,
        Akm::Sae { .. } => VERSION_AKM,
    }
}

fn find_kde(data: &[u8], kind: u8) -> Option<&[u8]> {
    let padding = |element: &&[u8]| element[0] == VENDOR && element.len() == 2;
    elements(data).take_while(|element| !padding(element)).find_map(|element| {
        let body = &element[2..];
        (element[0] == VENDOR && body.len() > 4 && rsn::is_suite(body, kind)).then(|| &body[4..])
    })
}

/// The station's side of the handshakes with one network, from the first frame to the keys and on through every new group key.
pub struct Supplicant {
    pmk: [u8; PMK],
    choice: Choice,
    station: [u8; 6],
    network: [u8; 6],
    rsn: Vec<u8>,
    elements: Vec<u8>,
    handshake: Option<([u8; NONCE], [u8; NONCE], Ptk)>,
    ptk: Option<Ptk>,
    replay: Option<u64>,
    // The number and the key of the group key and of the management group key in use.
    group: Option<(u8, Vec<u8>)>,
    management: Option<(u8, Vec<u8>)>,
}

// Whether a key is not the one in use, which it then becomes: a key put to use again would have its packet number set back (CVE-2017-13080).
fn is_new(in_use: &mut Option<(u8, Vec<u8>)>, key: &GroupKey) -> bool {
    if in_use.as_ref().is_some_and(|(index, held)| *index == key.index && *held == key.key) {
        return false;
    }
    *in_use = Some((key.index, key.key.clone()));
    true
}

// Keeps of a frame's key steps those whose key is not the one in use, which each then becomes.
fn keep_new(steps: &mut Vec<Step>, group: &mut Option<(u8, Vec<u8>)>, management: &mut Option<(u8, Vec<u8>)>) {
    steps.retain(|step| match step {
        Step::Group(key) => is_new(group, key),
        Step::Management(key) => is_new(management, key),
        Step::Send(_) | Step::Pairwise(_) => true,
    });
}

impl Supplicant {
    /// A station proving `pmk` to the network at `network`, having taken `choice` of the RSN element and extension in `rsn`.
    pub fn new(pmk: [u8; PMK], choice: Choice, station: [u8; 6], network: [u8; 6], rsn: &[u8]) -> Supplicant {
        Supplicant {
            pmk,
            choice,
            station,
            network,
            rsn: rsn.to_vec(),
            elements: choice.elements(),
            handshake: None,
            ptk: None,
            replay: None,
            group: None,
            management: None,
        }
    }

    pub(super) fn set_elements(&mut self, elements: &[u8]) {
        self.elements = elements.to_vec();
    }

    pub(super) fn is_agreed(&self) -> bool {
        self.ptk.is_some()
    }

    // True for a frame the network counted no higher than one that was taken already.
    fn is_replayed(&self, frame: &Frame<'_>) -> bool {
        self.replay.is_some_and(|last| frame.replay <= last)
    }

    fn open(&self, crypto: &impl Crypto, raw: &[u8], frame: &Frame<'_>, ptk: &Ptk) -> Result<Vec<u8>, Error> {
        if self.is_replayed(frame) {
            return Err(Error::Replayed);
        }
        let frame_mic = raw.get(MIC_AT..MIC_AT + MIC).ok_or(Error::NotAKeyFrame)?;
        let mic = compute_mic(crypto, frame.information & VERSION_BITS, &ptk.kck, raw).ok_or(Error::Signature)?;
        if !same(&mic, frame_mic) {
            return Err(Error::WrongKey);
        }
        if frame.information & ENCRYPTED == 0 {
            return Err(Error::NoKey);
        }
        crypto.unwrap_key(&ptk.kek, frame.data).ok_or(Error::NoKey)
    }

    fn take_group_keys(
        &self,
        data: &[u8],
        sequence: [u8; PACKET_NUMBER],
        needs_management: bool,
    ) -> Result<Vec<Step>, Error> {
        let group = find_kde(data, GROUP_KEY)
            .filter(|body| body.len() == GROUP_KEY_HEAD + self.choice.group.key_length())
            .ok_or(Error::NoKey)?;
        let mut key = group[GROUP_KEY_HEAD..].to_vec();
        if self.choice.group == Group::Tkip {
            let (sending, receiving) = key[TKIP_MIC_AT..].split_at_mut(TKIP_MIC_KEY);
            sending.swap_with_slice(receiving);
        }
        let mut steps = alloc::vec![Step::Group(GroupKey {
            index: group[0] & KEY_ID,
            cipher: self.choice.group.suite(),
            key,
            sequence
        })];
        let management = find_kde(data, MANAGEMENT_KEY).filter(|body| body.len() == MANAGEMENT_KEY_HEAD + KEY);
        match (self.choice.is_protected, management) {
            (true, Some(body)) => {
                let (index, sequence, key) = (
                    body[0],
                    body[2..MANAGEMENT_KEY_HEAD].try_into().expect("six bytes"),
                    body[MANAGEMENT_KEY_HEAD..].to_vec(),
                );
                steps.push(Step::Management(GroupKey { index, cipher: rsn::MANAGEMENT, key, sequence }));
            }
            (true, None) if needs_management => return Err(Error::NoKey),
            _ => {}
        }
        Ok(steps)
    }

    /// Takes a frame from the network and returns the steps it calls for; a frame that is refused changes nothing.
    pub fn take(&mut self, crypto: &impl Crypto, raw: &[u8]) -> Result<Vec<Step>, Error> {
        let frame = parse_frame(raw).ok_or(Error::NotAKeyFrame)?;
        let version = frame.information & VERSION_BITS;
        if version != descriptor_version(self.choice.akm) {
            return Err(Error::Signature);
        }
        let flags = frame.information & (PAIRWISE | KEY_ACK | KEY_MIC | INSTALL);
        if flags == PAIRWISE | KEY_ACK {
            // First of four: the network's nonce, unsigned; the station answers with its own, signed with the keys both nonces give.
            if self.is_replayed(&frame) {
                return Err(Error::Replayed);
            }
            let snonce = match &self.handshake {
                Some((anonce, snonce, _)) if anonce == frame.nonce => *snonce,
                _ => {
                    let mut nonce = [0u8; NONCE];
                    if !crypto.random(&mut nonce) {
                        return Err(Error::NoRandom);
                    }
                    nonce
                }
            };
            let ptk =
                derive_ptk(crypto, self.choice.akm, &self.pmk, (&self.network, &self.station), (frame.nonce, &snonce));
            let answer = build_frame(version | PAIRWISE | KEY_MIC, frame.replay, &snonce, &self.elements);
            let answer = sign(crypto, version, &ptk.kck, answer)?;
            self.handshake = Some((*frame.nonce, snonce, ptk));
            Ok(alloc::vec![Step::Send(answer)])
        } else if flags == PAIRWISE | KEY_ACK | KEY_MIC | INSTALL {
            // Third of four: signed by the network, which so proves the password, with the first group keys; the station confirms, and only then are keys put to use.
            let Some((anonce, _, ptk)) = &self.handshake else {
                return Err(Error::OutOfTurn);
            };
            if anonce != frame.nonce {
                return Err(Error::OutOfTurn);
            }
            let data = self.open(crypto, raw, &frame, ptk)?;
            if [rsn::ELEMENT, rsn::EXTENSION]
                .iter()
                .any(|number| rsn::find_element(&data, *number) != rsn::find_element(&self.rsn, *number))
            {
                return Err(Error::ElementDiffers);
            }
            let mut keys = self.take_group_keys(&data, frame.sequence, true)?;
            let fourth = build_frame(version | PAIRWISE | KEY_MIC | SECURE, frame.replay, &[0; NONCE], &[]);
            let mut steps = alloc::vec![Step::Send(sign(crypto, version, &ptk.kck, fourth)?)];
            // A third frame that comes again, the confirmation having been lost, is confirmed again; its keys are in use and stay as they are.
            if self.ptk.as_ref() != Some(ptk) {
                steps.push(Step::Pairwise(ptk.tk));
                keep_new(&mut keys, &mut self.group, &mut self.management);
                steps.extend(keys);
                self.ptk = Some(ptk.clone());
            }
            self.replay = Some(frame.replay);
            Ok(steps)
        } else if flags == KEY_ACK | KEY_MIC {
            // The first of two: a new group key, which the station confirms; one sent again, its confirmation having been lost, is confirmed again and stays as it is.
            let Some(ptk) = &self.ptk else {
                return Err(Error::OutOfTurn);
            };
            let data = self.open(crypto, raw, &frame, ptk)?;
            let mut keys = self.take_group_keys(&data, frame.sequence, false)?;
            let second = build_frame(version | KEY_MIC | SECURE, frame.replay, &[0; NONCE], &[]);
            let mut steps = alloc::vec![Step::Send(sign(crypto, version, &ptk.kck, second)?)];
            keep_new(&mut keys, &mut self.group, &mut self.management);
            steps.extend(keys);
            self.replay = Some(frame.replay);
            Ok(steps)
        } else {
            Err(Error::OutOfTurn)
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::base::checksum::fnv;
    use crate::base::checksum::fnv_from;
    use crate::wifi::sae::point::tests::Toy;
    use alloc::vec;
    use core::cell::Cell;

    pub(crate) struct Sums;

    fn fold(key: &[u8], parts: &[&[u8]], out: &mut [u8]) {
        let mut sum = fnv(key.iter().chain(parts.iter().flat_map(|part| part.iter())));
        for byte in out.iter_mut() {
            sum = fnv_from(sum, &[0xFF]);
            *byte ^= (sum >> 32) as u8;
        }
    }

    impl Primitives for Sums {
        fn hmac_sha256(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
            let mut out = [0x5C; 32];
            fold(key, parts, &mut out);
            out
        }

        fn hkdf_sha256(&self, salt: &[u8], secret: &[u8], info: &[&[u8]], out: &mut [u8]) -> bool {
            out.fill(0x1D);
            fold(salt, &[&[secret], info].concat(), out);
            true
        }

        fn random(&self, out: &mut [u8]) -> bool {
            out.iter_mut().enumerate().for_each(|(at, byte)| *byte = 0xA0 ^ at as u8);
            true
        }
    }

    impl Crypto for Sums {
        fn hmac_sha1(&self, key: &[u8], parts: &[&[u8]]) -> [u8; 20] {
            let mut out = [0x36; 20];
            fold(key, parts, &mut out);
            out
        }

        fn cmac(&self, key: &[u8; KEY], parts: &[&[u8]]) -> Option<[u8; MIC]> {
            let mut out = [0x87; MIC];
            fold(key, parts, &mut out);
            Some(out)
        }

        fn unwrap_key(&self, key: &[u8; KEY], wrapped: &[u8]) -> Option<Vec<u8>> {
            let (check, data) = wrapped.split_at_checked(8)?;
            (check == &key[..8]).then(|| data.iter().zip(key.iter().cycle()).map(|(byte, mask)| byte ^ mask).collect())
        }

        fn derive_pmk(&self, password: &[u8], name: &[u8]) -> [u8; PMK] {
            let mut out = [0x4B; PMK];
            fold(password, &[name], &mut out);
            out
        }
    }

    fn wrap(key: &[u8; KEY], data: &[u8]) -> Vec<u8> {
        key[..8].iter().copied().chain(data.iter().zip(key.iter().cycle()).map(|(byte, mask)| byte ^ mask)).collect()
    }

    pub(crate) const STATION: [u8; 6] = [2, 0, 0, 0, 0, 1];
    pub(crate) const NETWORK: [u8; 6] = [2, 0, 0, 0, 0, 9];
    pub(crate) const MASTER_KEY: [u8; PMK] = [7; PMK];
    const ANONCE: [u8; NONCE] = [0x11; NONCE];

    pub(crate) struct Authenticator {
        pmk: [u8; PMK],
        choice: Choice,
        rsn: Vec<u8>,
        nonce: [u8; NONCE],
        replay: u64,
    }

    impl Authenticator {
        pub(crate) fn new(pmk: [u8; PMK], rsn: &[u8]) -> Authenticator {
            let choice = rsn::choose(rsn).expect("an element this firmware takes");
            Authenticator { pmk, choice, rsn: rsn.to_vec(), nonce: ANONCE, replay: 0 }
        }

        fn version(&self) -> u16 {
            descriptor_version(self.choice.akm)
        }

        pub(crate) fn first(&mut self) -> Vec<u8> {
            self.replay += 1;
            build_frame(self.version() | PAIRWISE | KEY_ACK, self.replay, &self.nonce, &[])
        }

        fn keys(&self, answer: &[u8]) -> Option<Ptk> {
            let frame = parse_frame(answer)?;
            let ptk = derive_ptk(&Sums, self.choice.akm, &self.pmk, (&NETWORK, &STATION), (&self.nonce, frame.nonce));
            let mic = compute_mic(&Sums, self.version(), &ptk.kck, answer)?;
            (mic[..] == answer[MIC_AT..MIC_AT + MIC]).then_some(ptk)
        }

        pub(crate) fn data(&self, group: &[u8], management: Option<&[u8; KEY]>) -> Vec<u8> {
            let mut data = self.rsn.clone();
            data.extend([VENDOR, 6 + group.len() as u8, 0x00, 0x0F, 0xAC, GROUP_KEY, 1, 0]);
            data.extend(group);
            if let Some(key) = management {
                data.extend([VENDOR, 4 + MANAGEMENT_KEY_HEAD as u8 + KEY as u8, 0x00, 0x0F, 0xAC, MANAGEMENT_KEY]);
                // The key's number and its packet number.
                data.extend([4, 0, 1, 2, 3, 4, 5, 6]);
                data.extend(key);
            }
            data.extend([VENDOR, 0, 0, 0]);
            data
        }

        pub(crate) fn third(&mut self, answer: &[u8], data: &[u8]) -> Option<Vec<u8>> {
            let ptk = self.keys(answer)?;
            self.replay += 1;
            let mut frame = build_frame(
                self.version() | PAIRWISE | KEY_ACK | KEY_MIC | INSTALL | SECURE | ENCRYPTED,
                self.replay,
                &self.nonce,
                &wrap(&ptk.kek, data),
            );
            frame[SEQUENCE_AT..SEQUENCE_AT + PACKET_NUMBER].copy_from_slice(&[9, 8, 7, 6, 5, 4]);
            sign(&Sums, self.version(), &ptk.kck, frame).ok()
        }

        fn is_signed(&self, answer: &[u8], frame: &[u8]) -> bool {
            self.keys(answer)
                .and_then(|ptk| compute_mic(&Sums, self.version(), &ptk.kck, frame))
                .is_some_and(|mic| mic[..] == frame[MIC_AT..MIC_AT + MIC])
        }
    }

    pub(crate) const PLAIN: [u8; 22] =
        [48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 2, 0, 0];
    const PROTECTED: [u8; 22] =
        [48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 6, 0xC0, 0];
    pub(crate) const WPA3: [u8; 25] = [
        48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 8, 0xC0, 0, 244, 1, 0x20,
    ];

    fn station(rsn: &[u8]) -> Supplicant {
        Supplicant::new(MASTER_KEY, rsn::choose(rsn).unwrap(), STATION, NETWORK, rsn)
    }

    fn first_frame(steps: &[Step]) -> Vec<u8> {
        match steps.first() {
            Some(Step::Send(frame)) => frame.clone(),
            other => panic!("no frame to send first: {other:?}"),
        }
    }

    fn ptk_of(akm: Akm, second: &[u8]) -> Ptk {
        derive_ptk(&Sums, akm, &MASTER_KEY, (&STATION, &NETWORK), (&ANONCE, second[17..49].try_into().unwrap()))
    }

    fn sign_sha1(ptk: &Ptk, information: u16, replay: u64, nonce: &[u8; NONCE], data: &[u8]) -> Vec<u8> {
        sign(&Sums, VERSION_SHA1, &ptk.kck, build_frame(VERSION_SHA1 | information, replay, nonce, data)).unwrap()
    }

    fn group_key(key: &[u8]) -> Step {
        Step::Group(GroupKey { index: 1, cipher: Group::Ccmp.suite(), key: key.to_vec(), sequence: [9, 8, 7, 6, 5, 4] })
    }

    fn management_step() -> Step {
        Step::Management(GroupKey {
            index: 4,
            cipher: rsn::MANAGEMENT,
            key: vec![0x44; 16],
            sequence: [1, 2, 3, 4, 5, 6],
        })
    }

    #[test]
    fn four_way_handshake() {
        let (mut network, mut station) = (Authenticator::new(MASTER_KEY, &PLAIN), station(&PLAIN));
        let second = station.take(&Sums, &network.first()).unwrap();
        assert_eq!(second.len(), 1, "the first frame is answered and nothing else");
        let second = first_frame(&second);
        assert_eq!(
            (second[0], second[1], u16::from_be_bytes([second[2], second[3]]) as usize, second.len()),
            (1, 3, 95 + 22, 99 + 22),
            "an EAPOL frame that carries keys, as long as its head says"
        );
        assert_eq!(
            (u16::from_be_bytes([second[5], second[6]]), &second[9..17], &second[99..]),
            (0x010A, &1u64.to_be_bytes()[..], &PLAIN[..]),
            "about the keys between the two, signed by SHA-1, with the network's count and the station's element"
        );
        assert!(second[17..49] != [0; 32] && second[81..97] != [0; 16] && !station.is_agreed());
        let group = [0x33; 16];
        let third = network
            .third(&second, &network.data(&group, None))
            .expect("the network takes the answer: it is signed with the keys the password gives");
        let steps = station.take(&Sums, &third).unwrap();
        let fourth = first_frame(&steps);
        assert_eq!(
            (u16::from_be_bytes([fourth[5], fourth[6]]), &fourth[9..17], fourth.len()),
            (0x030A, &2u64.to_be_bytes()[..], 99),
            "the confirmation: signed, both sides have their keys, no data"
        );
        assert!(network.is_signed(&second, &fourth));
        assert_eq!(
            steps[1..],
            [Step::Pairwise(ptk_of(Akm::Psk, &second).tk), group_key(&group)],
            "then the key between the two, then the key for what goes to all, with the packet number the network has got to"
        );
        assert!(station.is_agreed());
    }

    #[test]
    fn wrong_password() {
        let (mut network, mut station) = (Authenticator::new([8; PMK], &PLAIN), station(&PLAIN));
        let second = first_frame(&station.take(&Sums, &network.first()).unwrap());
        assert!(
            network.third(&second, &[]).is_none(),
            "the network sees it first: the answer is not signed with its keys"
        );
        let mut honest = Authenticator::new(MASTER_KEY, &PLAIN);
        honest.replay = 1;
        let mut third = honest.third(&second, &honest.data(&[0x33; 16], None)).unwrap();
        third[100] ^= 1;
        assert_eq!(
            station.take(&Sums, &third),
            Err(Error::WrongKey),
            "and a frame that was changed on its way is not taken"
        );
        assert!(!station.is_agreed());
    }

    #[test]
    fn repeated_third_frame() {
        let (mut network, mut station) = (Authenticator::new(MASTER_KEY, &PLAIN), station(&PLAIN));
        let second = first_frame(&station.take(&Sums, &network.first()).unwrap());
        let data = network.data(&[0x33; 16], None);
        let third = network.third(&second, &data).unwrap();
        assert_eq!(station.take(&Sums, &third).unwrap().len(), 3);
        assert_eq!(station.take(&Sums, &third), Err(Error::Replayed), "the very same frame is not taken twice");
        let repeat = network.third(&second, &data).unwrap();
        let steps = station.take(&Sums, &repeat).unwrap();
        assert!(
            steps.len() == 1 && network.is_signed(&second, &first_frame(&steps)),
            "sent anew with a higher count, it is confirmed, and that is all"
        );
        let restart = network.first();
        let answer = first_frame(&station.take(&Sums, &restart).unwrap());
        assert_eq!(answer[17..49], second[17..49], "a first frame with the same nonce gets the same answer");
        network.nonce = [0x22; NONCE];
        let new_answer = first_frame(&station.take(&Sums, &network.first()).unwrap());
        assert_eq!(
            new_answer[17..49],
            second[17..49],
            "the test's random bytes are always the same, but the keys follow the network's new nonce"
        );
        assert_ne!(new_answer[81..97], answer[81..97]);
    }

    #[test]
    fn refused_frames() {
        let (mut network, mut station) = (Authenticator::new(MASTER_KEY, &PLAIN), station(&PLAIN));
        let first = network.first();
        assert_eq!(station.take(&Sums, &first[..98]), Err(Error::NotAKeyFrame), "cut short");
        assert_eq!(
            station.take(&Sums, &[2, 0, 0, 4, 1, 2, 3, 4]),
            Err(Error::NotAKeyFrame),
            "an EAPOL frame of another kind"
        );
        let mut other = first.clone();
        other[6] = other[6] & !7 | 1;
        assert_eq!(station.take(&Sums, &other), Err(Error::Signature), "the signature of the cipher before CCMP");
        let mut cmac = first.clone();
        cmac[6] = cmac[6] & !7 | 3;
        assert_eq!(station.take(&Sums, &cmac), Err(Error::Signature), "nor the signature another proof calls for");
        let stranger = build_frame(VERSION_SHA1 | KEY_ACK | KEY_MIC | SECURE | ENCRYPTED, 5, &[0; NONCE], &[0; 24]);
        assert_eq!(
            station.take(&Sums, &stranger),
            Err(Error::OutOfTurn),
            "a new key for all before any keys are agreed"
        );
        let second = first_frame(&station.take(&Sums, &first).unwrap());
        let ptk = ptk_of(Akm::Psk, &second);
        let data = wrap(&ptk.kek, &network.data(&[1; 16], None));
        let forgery =
            sign_sha1(&ptk, PAIRWISE | KEY_ACK | KEY_MIC | INSTALL | SECURE | ENCRYPTED, 2, &[0x22; NONCE], &data);
        assert_eq!(
            station.take(&Sums, &forgery),
            Err(Error::OutOfTurn),
            "a third frame with another nonce than the first had"
        );
        let plain =
            sign_sha1(&ptk, PAIRWISE | KEY_ACK | KEY_MIC | INSTALL | SECURE, 2, &ANONCE, &network.data(&[1; 16], None));
        assert_eq!(station.take(&Sums, &plain), Err(Error::NoKey), "keys that are not wrapped are not taken");
        let no_group = network.third(&second, &PLAIN).unwrap();
        assert_eq!(
            station.take(&Sums, &no_group),
            Err(Error::NoKey),
            "nor a third frame without the key for what goes to all"
        );
        let mut element = PLAIN.to_vec();
        element[20] = 0x80;
        let downgrade = network.third(&second, &[&element[..], &network.data(&[1; 16], None)[22..]].concat()).unwrap();
        assert_eq!(
            station.take(&Sums, &downgrade),
            Err(Error::ElementDiffers),
            "nor one whose element is not the beacon's"
        );
        let short = network.third(&second, &network.data(&[1; 5], None)).unwrap();
        assert_eq!(station.take(&Sums, &short), Err(Error::NoKey), "nor a key of the wrong length for its cipher");
        assert!(!station.is_agreed());
    }

    #[test]
    fn protected_network() {
        let (mut network, mut station) = (Authenticator::new(MASTER_KEY, &PROTECTED), station(&PROTECTED));
        let second = first_frame(&station.take(&Sums, &network.first()).unwrap());
        assert_eq!(
            (u16::from_be_bytes([second[5], second[6]]), second[99], second[100] as usize, &second[119..127]),
            (0x010B, 48, 26, &[0xC0, 0, 0, 0, 0x00, 0x0F, 0xAC, 6][..]),
            "signed by CMAC, with an element that insists on the protection too and names BIP"
        );
        let ptk = ptk_of(Akm::PskSha256, &second);
        assert_ne!(ptk.tk, ptk_of(Akm::Psk, &second).tk);
        let no_management = network.third(&second, &network.data(&[0x33; 16], None)).unwrap();
        assert_eq!(
            station.take(&Sums, &no_management),
            Err(Error::NoKey),
            "a third frame without the key for management frames is not taken"
        );
        let third = network.third(&second, &network.data(&[0x33; 16], Some(&[0x44; 16]))).unwrap();
        let steps = station.take(&Sums, &third).unwrap();
        assert_eq!(steps[1..], [Step::Pairwise(ptk.tk), group_key(&[0x33; 16]), management_step()]);
    }

    #[test]
    fn sha256_without_protection() {
        const BARE: [u8; 22] =
            [48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 6, 0, 0];
        let (mut network, mut station) = (Authenticator::new(MASTER_KEY, &BARE), station(&BARE));
        let second = first_frame(&station.take(&Sums, &network.first()).unwrap());
        assert_eq!(
            (u16::from_be_bytes([second[5], second[6]]), &second[99..]),
            (0x010B, &BARE[..]),
            "signed by CMAC, with an element that names the proof and no protection"
        );
        let third = network.third(&second, &network.data(&[0x33; 16], None)).unwrap();
        let steps = station.take(&Sums, &third).unwrap();
        assert!(network.is_signed(&second, &first_frame(&steps)) && station.is_agreed());
        assert_eq!(
            steps[1..],
            [Step::Pairwise(ptk_of(Akm::PskSha256, &second).tk), group_key(&[0x33; 16])],
            "the keys are derived over SHA-256, and a third frame without a management key is taken"
        );
    }

    #[test]
    fn handshake_after_sae() {
        let (mut network, mut station) = (Authenticator::new(MASTER_KEY, &WPA3), station(&WPA3));
        let first = network.first();
        let mut older = first.clone();
        older[6] |= 3;
        assert_eq!(
            station.take(&Sums, &older),
            Err(Error::Signature),
            "a frame that names the signature of the older proof is not of this handshake"
        );
        let second = first_frame(&station.take(&Sums, &first).unwrap());
        assert_eq!(
            (
                u16::from_be_bytes([second[5], second[6]]),
                second[99],
                second[100] as usize,
                &second[115..119],
                &second[127..]
            ),
            (0x0108, 48, 26, &[0x00, 0x0F, 0xAC, 8][..], &[244, 1, 0x20][..]),
            "signed the proof's own way, with an element that names SAE and the extension that says the point was hashed to"
        );
        let ptk = ptk_of(Akm::Sae { is_hashed: true }, &second);
        assert!(ptk == ptk_of(Akm::PskSha256, &second), "the keys are derived over SHA-256");
        let data = network.data(&[0x33; 16], Some(&[0x44; 16]));
        let no_extension = network.third(&second, &[&data[..22], &data[25..]].concat()).unwrap();
        assert_eq!(
            station.take(&Sums, &no_extension),
            Err(Error::ElementDiffers),
            "a third frame without the extension the beacon had is not taken"
        );
        let other_extension = network.third(&second, &[&data[..24], &[0x00][..], &data[25..]].concat()).unwrap();
        assert_eq!(
            station.take(&Sums, &other_extension),
            Err(Error::ElementDiffers),
            "nor one whose extension says otherwise"
        );
        let third = network.third(&second, &data).unwrap();
        let steps = station.take(&Sums, &third).unwrap();
        assert!(network.is_signed(&second, &first_frame(&steps)) && station.is_agreed());
        assert_eq!(steps[1..], [Step::Pairwise(ptk.tk), group_key(&[0x33; 16]), management_step()]);
        let mut driver_rsn = self::station(&WPA3);
        driver_rsn.set_elements(&[48, 2, 1, 0, 244, 1, 0x20]);
        let frame = first_frame(&driver_rsn.take(&Sums, &Authenticator::new(MASTER_KEY, &WPA3).first()).unwrap());
        assert_eq!(
            (frame[99..].to_vec(), frame.len()),
            (vec![48, 2, 1, 0, 244, 1, 0x20], 99 + 7),
            "a station whose driver wrote the association request names what that had"
        );
    }

    #[test]
    fn group_rekey() {
        let (mut network, mut station) = (Authenticator::new(MASTER_KEY, &PLAIN), station(&PLAIN));
        let second = first_frame(&station.take(&Sums, &network.first()).unwrap());
        let third = network.third(&second, &network.data(&[0x33; 16], None)).unwrap();
        station.take(&Sums, &third).unwrap();
        let mut data = vec![VENDOR, 22, 0x00, 0x0F, 0xAC, GROUP_KEY, 2, 0];
        data.extend([0x55; 16]);
        let ptk = ptk_of(Akm::Psk, &second);
        let rekey = sign_sha1(&ptk, KEY_ACK | KEY_MIC | SECURE | ENCRYPTED, 3, &[0; NONCE], &wrap(&ptk.kek, &data));
        let steps = station.take(&Sums, &rekey).unwrap();
        let confirmation = first_frame(&steps);
        assert_eq!(
            (u16::from_be_bytes([confirmation[5], confirmation[6]]), &confirmation[9..17]),
            (0x0302, &3u64.to_be_bytes()[..]),
            "the confirmation: signed, not about the keys between the two"
        );
        assert!(network.is_signed(&second, &confirmation));
        assert_eq!(
            steps[1..],
            [Step::Group(GroupKey { index: 2, cipher: Group::Ccmp.suite(), key: vec![0x55; 16], sequence: [0; 6] })]
        );
        assert_eq!(station.take(&Sums, &rekey), Err(Error::Replayed));
        let again = sign_sha1(&ptk, KEY_ACK | KEY_MIC | SECURE | ENCRYPTED, 4, &[0; NONCE], &wrap(&ptk.kek, &data));
        let steps = station.take(&Sums, &again).unwrap();
        assert!(
            steps.len() == 1 && network.is_signed(&second, &first_frame(&steps)),
            "the same key sent anew with a higher count is confirmed and not put to use again"
        );
        let mut first_key = vec![VENDOR, 22, 0x00, 0x0F, 0xAC, GROUP_KEY, 1, 0];
        first_key.extend([0x33; 16]);
        let back = sign_sha1(&ptk, KEY_ACK | KEY_MIC | SECURE | ENCRYPTED, 5, &[0; NONCE], &wrap(&ptk.kek, &first_key));
        assert_eq!(station.take(&Sums, &back).unwrap().len(), 2, "another key than the one in use is put to use");
    }

    #[test]
    fn tkip_group_key_swap() {
        const MIXED: [u8; 26] = [
            48, 24, 1, 0, 0x00, 0x0F, 0xAC, 2, 2, 0, 0x00, 0x0F, 0xAC, 2, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC,
            2, 0, 0,
        ];
        let (mut network, mut station) = (Authenticator::new(MASTER_KEY, &MIXED), station(&MIXED));
        let second = first_frame(&station.take(&Sums, &network.first()).unwrap());
        let group: Vec<u8> = (0..32).collect();
        let third = network.third(&second, &network.data(&group, None)).unwrap();
        let steps = station.take(&Sums, &third).unwrap();
        assert_eq!(
            steps[2],
            Step::Group(GroupKey {
                index: 1,
                cipher: Group::Tkip.suite(),
                key: [&group[..16], &group[24..], &group[16..24]].concat(),
                sequence: [9, 8, 7, 6, 5, 4]
            }),
            "the key that encrypts as it came, then the network's receiving key and its sending key"
        );
    }

    #[test]
    fn no_signature_from_library() {
        let (mut network, mut station) = (Authenticator::new(MASTER_KEY, &PROTECTED), station(&PROTECTED));
        let no_cmac = Toy(Cell::new(0));
        assert_eq!(
            station.take(&no_cmac, &network.first()),
            Err(Error::Signature),
            "an answer without its signature proves nothing and tells the network's nonce was taken"
        );
        assert!(station.handshake.is_none() && !station.is_agreed(), "the refusal changes nothing");
        let second = first_frame(&station.take(&Sums, &network.first()).unwrap());
        let third = network.third(&second, &network.data(&[0x33; 16], Some(&[0x44; 16]))).unwrap();
        assert_eq!(station.take(&no_cmac, &third), Err(Error::Signature), "the network's frame cannot be checked");
        assert!(!station.is_agreed(), "and no key is put to use");
    }

    #[test]
    fn ptk_argument_order() {
        let (low, high) = ([1u8; NONCE], [2u8; NONCE]);
        let first = derive_ptk(&Sums, Akm::Psk, &MASTER_KEY, (&STATION, &NETWORK), (&low, &high));
        let second = derive_ptk(&Sums, Akm::Psk, &MASTER_KEY, (&NETWORK, &STATION), (&high, &low));
        assert!(first == second && first.kck != first.kek && first.kek != first.tk);
    }

    #[test]
    fn step_debug_redaction() {
        let text = alloc::format!(
            "{:?} {:?} {:?}",
            Step::Pairwise([0xAB; KEY]),
            Step::Group(GroupKey {
                index: 1,
                cipher: Group::Ccmp.suite(),
                key: vec![0xAB; KEY],
                sequence: [9; PACKET_NUMBER]
            }),
            Step::Management(GroupKey {
                index: 4,
                cipher: rsn::MANAGEMENT,
                key: vec![0xAB; KEY],
                sequence: [9; PACKET_NUMBER]
            })
        );
        assert_eq!(
            text,
            "Pairwise(..) Group(GroupKey { index: 1, sequence: [9, 9, 9, 9, 9, 9], .. }) Management(GroupKey { index: 4, sequence: [9, 9, 9, 9, 9, 9], .. })"
        );
    }
}

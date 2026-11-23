// The ML-KEM braid of the Sparse Post-Quantum Ratchet: its stages are the spec's.

use super::code::{Chunk, Decoder, Encoder};
use crate::base::bytes::same;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::Crypto;
use crate::signal::Lattice;
use crate::signal::hkdf;
use crate::signal::kem;
use crate::signal::kem::FirstHalf;
use crate::signal::kem::HEADER_BYTES;
use alloc::vec::Vec;

const UPDATE: &[u8] = b"Signal_PQCKA_V1_MLKEM768:Authenticator Update";
const SECRET: &[u8] = b"Signal_PQCKA_V1_MLKEM768:SCKA Key";
const HEADER_MAC: &[u8] = b"Signal_PQCKA_V1_MLKEM768:ekheader";
const CIPHERTEXT_MAC: &[u8] = b"Signal_PQCKA_V1_MLKEM768:ciphertext";
const MAC_BYTES: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Payload {
    Nothing,
    Header(Chunk),
    Key(Chunk),
    KeyAck(Chunk),
    Ack,
    First(Chunk),
    Second(Chunk),
}

impl Payload {
    pub(super) fn kind(&self) -> u8 {
        match self {
            Payload::Nothing => 0,
            Payload::Header(_) => 1,
            Payload::Key(_) => 2,
            Payload::KeyAck(_) => 3,
            Payload::Ack => 4,
            Payload::First(_) => 5,
            Payload::Second(_) => 6,
        }
    }

    pub(super) fn chunk(&self) -> Option<&Chunk> {
        match self {
            Payload::Header(chunk)
            | Payload::Key(chunk)
            | Payload::KeyAck(chunk)
            | Payload::First(chunk)
            | Payload::Second(chunk) => Some(chunk),
            Payload::Nothing | Payload::Ack => None,
        }
    }

    pub(super) fn from_parts(kind: u8, chunk: Option<Chunk>) -> Option<Payload> {
        Some(match kind {
            0 => Payload::Nothing,
            1 => Payload::Header(chunk?),
            2 => Payload::Key(chunk?),
            3 => Payload::KeyAck(chunk?),
            4 => Payload::Ack,
            5 => Payload::First(chunk?),
            6 => Payload::Second(chunk?),
            _ => return None,
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Authenticator {
    root: [u8; 32],
    mac: [u8; 32],
}

impl Authenticator {
    fn new(crypto: &impl Crypto, key: &[u8]) -> Option<Authenticator> {
        let mut authenticator = Authenticator { root: [0; 32], mac: [0; 32] };
        authenticator.update(crypto, 1, key)?;
        Some(authenticator)
    }

    fn update(&mut self, crypto: &impl Crypto, epoch: u64, key: &[u8]) -> Option<()> {
        let out: [u8; 64] =
            hkdf(crypto, Some(&[0; 32]), &[&self.root[..], key].concat(), &[UPDATE, &epoch.to_be_bytes()])?;
        self.root.copy_from_slice(&out[..32]);
        self.mac.copy_from_slice(&out[32..]);
        Some(())
    }

    fn sign(&self, crypto: &impl Crypto, label: &[u8], epoch: u64, data: &[u8]) -> [u8; 32] {
        crypto.hmac_sha256(&self.mac, &[label, &epoch.to_be_bytes(), data])
    }

    fn verify(&self, crypto: &impl Crypto, label: &[u8], epoch: u64, data: &[u8], mac: &[u8]) -> bool {
        same(&self.sign(crypto, label, epoch, data), mac)
    }

    fn write(&self) -> Vec<u8> {
        [self.root, self.mac].concat()
    }

    fn read(bytes: &[u8]) -> Option<Authenticator> {
        (bytes.len() == 64).then(|| Authenticator {
            root: bytes[..32].try_into().expect("half"),
            mac: bytes[32..].try_into().expect("half"),
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct EpochSecret {
    pub(super) epoch: u64,
    pub(super) secret: [u8; 32],
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Braid {
    pub(super) epoch: u64,
    authenticator: Authenticator,
    stage: Stage,
}

impl core::fmt::Debug for Braid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Braid").field("epoch", &self.epoch).finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Stage {
    KeysUnsampled,
    KeysSampled { private: Vec<u8>, encoder: Encoder },
    HeaderSent { private: Vec<u8>, encoder: Encoder, decoder: Decoder },
    FirstReceived { private: Vec<u8>, first: Vec<u8>, encoder: Encoder },
    KeySentFirstReceived { private: Vec<u8>, first: Vec<u8>, decoder: Decoder },
    NoHeaderReceived { decoder: Decoder },
    HeaderReceived { header: [u8; HEADER_BYTES], decoder: Decoder },
    FirstSent { half: FirstHalf, first: Vec<u8>, encoder: Encoder, decoder: Decoder },
    KeyReceivedFirstSent { half: FirstHalf, first: Vec<u8>, key: Vec<u8>, encoder: Encoder },
    FirstAcknowledged { half: FirstHalf, first: Vec<u8>, decoder: Decoder },
    SecondSent { encoder: Encoder },
}

fn new_decoder(length: usize) -> Decoder {
    Decoder::new(length).expect("the braid's objects are of even length")
}

fn new_encoder(object: &[u8]) -> Encoder {
    Encoder::new(object).expect("the braid's objects are of even length")
}

fn derive_epoch_secret(crypto: &impl Crypto, epoch: u64, secret: &[u8; 32]) -> Option<[u8; 32]> {
    hkdf(crypto, Some(&[0; 32]), secret, &[SECRET, &epoch.to_be_bytes()])
}

fn send_second(
    crypto: &impl Crypto,
    epoch: u64,
    authenticator: &Authenticator,
    half: &FirstHalf,
    first: &[u8],
    key: &[u8],
) -> Option<Stage> {
    let second = kem::encapsulate_second(half, key)?;
    let mac = authenticator.sign(crypto, CIPHERTEXT_MAC, epoch, &[first, &second].concat());
    Some(Stage::SecondSent { encoder: new_encoder(&[&second[..], &mac].concat()) })
}

impl Braid {
    pub(super) fn alice(crypto: &impl Crypto, key: &[u8]) -> Option<Braid> {
        Some(Braid { epoch: 1, authenticator: Authenticator::new(crypto, key)?, stage: Stage::KeysUnsampled })
    }

    pub(super) fn bob(crypto: &impl Crypto, key: &[u8]) -> Option<Braid> {
        let stage = Stage::NoHeaderReceived { decoder: new_decoder(HEADER_BYTES + MAC_BYTES) };
        Some(Braid { epoch: 1, authenticator: Authenticator::new(crypto, key)?, stage })
    }

    pub(crate) fn send(mut self, crypto: &impl Crypto) -> Option<(Braid, Payload, Option<EpochSecret>)> {
        let (epoch, mut secret) = (self.epoch, None);
        self.stage = match core::mem::replace(&mut self.stage, Stage::KeysUnsampled) {
            Stage::KeysUnsampled => {
                let mut seeds = [0u8; 64];
                crypto.random(&mut seeds).then_some(())?;
                let (private, public) =
                    kem::generate(seeds[..32].try_into().expect("half"), seeds[32..].try_into().expect("half"));
                let header = kem::header_of(&public);
                let mac = self.authenticator.sign(crypto, HEADER_MAC, epoch, &header);
                Stage::KeysSampled { private, encoder: new_encoder(&[&header[..], &mac].concat()) }
            }
            Stage::HeaderReceived { header, decoder } => {
                let mut message = [0u8; 32];
                crypto.random(&mut message).then_some(())?;
                let (first, shared, half) = kem::encapsulate_first(&header, &message);
                let shared = derive_epoch_secret(crypto, epoch, &shared)?;
                self.authenticator.update(crypto, epoch, &shared)?;
                secret = Some(EpochSecret { epoch, secret: shared });
                Stage::FirstSent { half, encoder: new_encoder(&first), first, decoder }
            }
            stage => stage,
        };
        let payload = match &mut self.stage {
            Stage::KeysSampled { encoder, .. } => Payload::Header(encoder.next_chunk()),
            Stage::HeaderSent { encoder, .. } => Payload::Key(encoder.next_chunk()),
            Stage::FirstReceived { encoder, .. } => Payload::KeyAck(encoder.next_chunk()),
            Stage::FirstSent { encoder, .. } | Stage::KeyReceivedFirstSent { encoder, .. } => {
                Payload::First(encoder.next_chunk())
            }
            Stage::SecondSent { encoder } => Payload::Second(encoder.next_chunk()),
            Stage::KeySentFirstReceived { .. } => Payload::Ack,
            Stage::NoHeaderReceived { .. } | Stage::FirstAcknowledged { .. } => Payload::Nothing,
            Stage::KeysUnsampled | Stage::HeaderReceived { .. } => unreachable!("made into the stage that sends"),
        };
        Some((self, payload, secret))
    }

    pub(super) fn receive(
        mut self,
        crypto: &impl Crypto,
        epoch: u64,
        payload: &Payload,
    ) -> Option<(Braid, Option<EpochSecret>)> {
        if epoch > self.epoch {
            return match self.stage {
                Stage::SecondSent { .. } if epoch == self.epoch + 1 => {
                    Some((Braid { epoch, authenticator: self.authenticator, stage: Stage::KeysUnsampled }, None))
                }
                _ => None,
            };
        }
        if epoch < self.epoch {
            return Some((self, None));
        }
        let mut secret = None;
        let authenticator = &mut self.authenticator;
        self.stage = match (core::mem::replace(&mut self.stage, Stage::KeysUnsampled), payload) {
            (Stage::KeysSampled { private, .. }, Payload::First(chunk)) => {
                let mut decoder = new_decoder(kem::FIRST_BYTES);
                decoder.add(chunk);
                Stage::HeaderSent { encoder: new_encoder(kem::t_of(&private)), private, decoder }
            }
            (Stage::HeaderSent { private, encoder, mut decoder }, Payload::First(chunk)) => {
                decoder.add(chunk);
                match decoder.decode() {
                    Some(first) => Stage::FirstReceived { private, first, encoder },
                    None => Stage::HeaderSent { private, encoder, decoder },
                }
            }
            (Stage::FirstReceived { private, first, .. }, Payload::Second(chunk)) => {
                let mut decoder = new_decoder(kem::SECOND_BYTES + MAC_BYTES);
                decoder.add(chunk);
                Stage::KeySentFirstReceived { private, first, decoder }
            }
            (Stage::KeySentFirstReceived { private, first, mut decoder }, Payload::Second(chunk)) => {
                decoder.add(chunk);
                match decoder.decode() {
                    Some(second) => {
                        let (second, mac) = second.split_at(kem::SECOND_BYTES);
                        let ciphertext = [&first[..], second].concat();
                        let shared = crypto.decapsulate(Lattice::Mlkem768, &private, &ciphertext)?;
                        let shared = derive_epoch_secret(crypto, epoch, &shared)?;
                        authenticator.update(crypto, epoch, &shared)?;
                        if !authenticator.verify(crypto, CIPHERTEXT_MAC, epoch, &ciphertext, mac) {
                            return None;
                        }
                        (self.epoch, secret) = (epoch + 1, Some(EpochSecret { epoch, secret: shared }));
                        Stage::NoHeaderReceived { decoder: new_decoder(HEADER_BYTES + MAC_BYTES) }
                    }
                    None => Stage::KeySentFirstReceived { private, first, decoder },
                }
            }
            (Stage::NoHeaderReceived { mut decoder }, Payload::Header(chunk)) => {
                decoder.add(chunk);
                match decoder.decode() {
                    Some(bytes) => {
                        let (header, mac) = bytes.split_at(HEADER_BYTES);
                        if !authenticator.verify(crypto, HEADER_MAC, epoch, header, mac) {
                            return None;
                        }
                        let header = header.try_into().expect("a header's bytes");
                        Stage::HeaderReceived { header, decoder: new_decoder(kem::T_BYTES) }
                    }
                    None => Stage::NoHeaderReceived { decoder },
                }
            }
            (Stage::FirstSent { half, first, encoder, mut decoder }, Payload::Key(chunk) | Payload::KeyAck(chunk)) => {
                let is_ack = matches!(payload, Payload::KeyAck(_));
                decoder.add(chunk);
                match decoder.decode() {
                    Some(key) if !kem::t_matches(&half.header, &key) => return None,
                    Some(key) if is_ack => send_second(crypto, epoch, authenticator, &half, &first, &key)?,
                    Some(key) => Stage::KeyReceivedFirstSent { half, first, key, encoder },
                    None if is_ack => Stage::FirstAcknowledged { half, first, decoder },
                    None => Stage::FirstSent { half, first, encoder, decoder },
                }
            }
            (Stage::KeyReceivedFirstSent { half, first, key, .. }, Payload::Ack | Payload::KeyAck(_)) => {
                send_second(crypto, epoch, authenticator, &half, &first, &key)?
            }
            (Stage::FirstAcknowledged { half, first, mut decoder }, Payload::Key(chunk) | Payload::KeyAck(chunk)) => {
                decoder.add(chunk);
                match decoder.decode() {
                    Some(key) if !kem::t_matches(&half.header, &key) => return None,
                    Some(key) => send_second(crypto, epoch, authenticator, &half, &first, &key)?,
                    None => Stage::FirstAcknowledged { half, first, decoder },
                }
            }
            (stage, _) => stage,
        };
        Some((self, secret))
    }

    pub(crate) fn write(&self) -> Writer {
        let (stage, writer) = match &self.stage {
            Stage::KeysUnsampled => (1, Writer::new()),
            Stage::KeysSampled { private, encoder } => (2, Writer::new().bytes(3, private).bytes(4, &encoder.write())),
            Stage::HeaderSent { private, encoder, decoder } => {
                (3, Writer::new().bytes(3, private).bytes(4, &encoder.write()).bytes(5, &decoder.write()))
            }
            Stage::FirstReceived { private, first, encoder } => {
                (4, Writer::new().bytes(3, private).bytes(6, first).bytes(4, &encoder.write()))
            }
            Stage::KeySentFirstReceived { private, first, decoder } => {
                (5, Writer::new().bytes(3, private).bytes(6, first).bytes(5, &decoder.write()))
            }
            Stage::NoHeaderReceived { decoder } => (6, Writer::new().bytes(5, &decoder.write())),
            Stage::HeaderReceived { header, decoder } => (7, Writer::new().bytes(7, header).bytes(5, &decoder.write())),
            Stage::FirstSent { half, first, encoder, decoder } => (
                8,
                Writer::new()
                    .bytes(8, &half.message)
                    .bytes(7, &half.header)
                    .bytes(6, first)
                    .bytes(4, &encoder.write())
                    .bytes(5, &decoder.write()),
            ),
            Stage::KeyReceivedFirstSent { half, first, key, encoder } => (
                9,
                Writer::new()
                    .bytes(8, &half.message)
                    .bytes(7, &half.header)
                    .bytes(6, first)
                    .bytes(9, key)
                    .bytes(4, &encoder.write()),
            ),
            Stage::FirstAcknowledged { half, first, decoder } => (
                10,
                Writer::new().bytes(8, &half.message).bytes(7, &half.header).bytes(6, first).bytes(5, &decoder.write()),
            ),
            Stage::SecondSent { encoder } => (11, Writer::new().bytes(4, &encoder.write())),
        };
        writer.varint(1, stage).varint(2, self.epoch).bytes(10, &self.authenticator.write())
    }

    pub(crate) fn read(message: &Message) -> Option<Braid> {
        let epoch = message.varint(2)?;
        let authenticator = Authenticator::read(message.bytes(10)?)?;
        let private = || message.bytes(3).filter(|private| private.len() == kem::PRIVATE_BYTES).map(<[u8]>::to_vec);
        let encoder = || Encoder::read(message.bytes(4)?);
        let decoder = |length| Decoder::read(message.bytes(5)?).filter(|decoder| decoder.length() == length);
        let first = || message.bytes(6).map(<[u8]>::to_vec);
        let header = || message.bytes(7)?.try_into().ok();
        let half = || Some(FirstHalf { message: message.bytes(8)?.try_into().ok()?, header: header()? });
        let stage = match message.varint(1)? {
            1 => Stage::KeysUnsampled,
            2 => Stage::KeysSampled { private: private()?, encoder: encoder()? },
            3 => Stage::HeaderSent { private: private()?, encoder: encoder()?, decoder: decoder(kem::FIRST_BYTES)? },
            4 => Stage::FirstReceived { private: private()?, first: first()?, encoder: encoder()? },
            5 => Stage::KeySentFirstReceived {
                private: private()?,
                first: first()?,
                decoder: decoder(kem::SECOND_BYTES + MAC_BYTES)?,
            },
            6 => Stage::NoHeaderReceived { decoder: decoder(HEADER_BYTES + MAC_BYTES)? },
            7 => Stage::HeaderReceived { header: header()?, decoder: decoder(kem::T_BYTES)? },
            8 => Stage::FirstSent {
                half: half()?,
                first: first()?,
                encoder: encoder()?,
                decoder: decoder(kem::T_BYTES)?,
            },
            9 => Stage::KeyReceivedFirstSent {
                half: half()?,
                first: first()?,
                key: message.bytes(9)?.to_vec(),
                encoder: encoder()?,
            },
            10 => Stage::FirstAcknowledged { half: half()?, first: first()?, decoder: decoder(kem::T_BYTES)? },
            11 => Stage::SecondSent { encoder: encoder()? },
            _ => return None,
        };
        Some(Braid { epoch, authenticator, stage })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn read_back(braid: &Braid) -> Option<Braid> {
        Braid::read(&Message::parse(&braid.write().finish())?)
    }

    #[test]
    fn invalid_stored_braid() {
        let braid = |stage| Braid { epoch: 1, authenticator: Authenticator { root: [1; 32], mac: [2; 32] }, stage };
        let stage = Stage::KeysSampled { private: vec![4; 10], encoder: new_encoder(&[3; HEADER_BYTES + MAC_BYTES]) };
        assert_eq!(read_back(&braid(stage)), None, "a key cut short");
        assert_eq!(read_back(&braid(Stage::NoHeaderReceived { decoder: new_decoder(2) })), None, "a header too short");
    }

    #[test]
    fn debug_without_keys() {
        let authenticator = Authenticator { root: [1; 32], mac: [2; 32] };
        let braid = Braid { epoch: 1, authenticator, stage: Stage::KeysUnsampled };
        assert_eq!(alloc::format!("{braid:?}"), "Braid { epoch: 1, .. }");
    }
}

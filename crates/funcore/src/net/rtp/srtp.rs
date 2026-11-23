use super::packet::{SEQUENCE_AT, SSRC_AT, head_length};
use super::rtcp;
use super::{AES128_BYTES, AES256_BYTES, BLOCK_BYTES, Crypto, Key, NONCE_BYTES, SSRC_BYTES, TAG_BYTES, extend};
use crate::base::bytes::{be16, be32};
use crate::net::window::Window;
use alloc::vec::Vec;

// RFC 7714, 12: the master salt and the session salts.
pub(crate) const SALT_BYTES: usize = 12;
// RFC 3711, 4.3.1 and 4.3.2: the labels of the session keys and salts; AEAD takes no authentication key.
const RTP_KEY: u8 = 0x00;
const RTP_SALT: u8 = 0x02;
const RTCP_KEY: u8 = 0x03;
const RTCP_SALT: u8 = 0x05;
// RFC 3711, 4.3.3 and RFC 6188, 3: the PRF's block is the 112-bit salt field, the label at its eighth byte, and a 16-bit block counter.
const PRF_INPUT: usize = 14;
const LABEL_AT: usize = 7;
// RFC 7714, 8.1 and 9.1: the SSRC after two noughts, then the index.
const NONCE_SSRC_AT: usize = 2;
const NONCE_INDEX_AT: usize = 6;
const NONCE_RTCP_INDEX_AT: usize = 8;
const INDEX_BYTES: usize = 6;
// RFC 7714, 9.2: the first eight bytes of the compound, through its sender's SSRC, and the E flag with the SRTCP index are its associated data.
const RTCP_HEAD: usize = rtcp::SSRC_AT + SSRC_BYTES;
const INDEX_WORD: usize = 4;
const ENCRYPTED: u32 = 1 << 31;
// RFC 7714, 13.1: processing ends before the 48-bit index or the 31-bit SRTCP index cycles back.
const INDICES: u64 = 1 << 48;
const RTCP_INDICES: u32 = 1 << 31;
// This side's SSRCs: a demux id's sixteen, the id with a layer in its low four bits, and the data SSRC to the calling server (call.rs, `LayerId` and `CLIENT_SERVER_DATA_SSRC`); RingRTC's one-to-one SSRCs lie within sixteen of a base (peer_connection.cc).
const OWN_STREAMS: usize = 16 + 1;

// A master key, whose length picks the suite, and a master salt (RFC 7714, 12); it has no `Debug`.
#[derive(PartialEq, Eq)]
pub(crate) struct Master {
    key: Key,
    salt: [u8; SALT_BYTES],
}

// A session key and salt.
struct Session {
    key: Key,
    salt: [u8; SALT_BYTES],
}

// What protects the packets this side sends: its session keys, each SSRC's highest index sent and the SRTCP index; it has no `Debug`.
pub(super) struct Outbound {
    rtp: Session,
    rtcp: Session,
    highest: Vec<(u32, u64)>,
    index: u32,
}

// What opens the packets the far side sends: its session keys and each SSRC's highest index and replay windows; it has no `Debug`.
pub(super) struct Inbound {
    rtp: Session,
    rtcp: Session,
    streams: Vec<Stream>,
    most: usize,
}

struct Stream {
    ssrc: u32,
    highest: Option<u64>,
    rtp: Window,
    rtcp: Window,
}

impl Master {
    // A key of 16 or 32 bytes and a salt of 12; none for other lengths.
    pub(crate) fn new(key: &[u8], salt: &[u8]) -> Option<Master> {
        let key = match key.len() {
            AES128_BYTES => Key::Aes128(key.try_into().ok()?),
            AES256_BYTES => Key::Aes256(key.try_into().ok()?),
            _ => return None,
        };
        Some(Master { key, salt: salt.try_into().ok()? })
    }

    // The session key and salt of two labels: the PRF over the salt filled to its 14 bytes with noughts, as the calling server and libsrtp fill RFC 7714's 12 (srtp.rs, `derive_key_from_master`).
    fn session(&self, crypto: &impl Crypto, key_label: u8, salt_label: u8) -> Option<Session> {
        let mut input = [0u8; PRF_INPUT];
        input[..SALT_BYTES].copy_from_slice(&self.salt);
        let key = match self.key {
            Key::Aes128(_) => Key::Aes128(prf(crypto, &self.key, &input, key_label)?),
            Key::Aes256(_) => Key::Aes256(prf(crypto, &self.key, &input, key_label)?),
        };
        Some(Session { key, salt: prf(crypto, &self.key, &input, salt_label)? })
    }
}

// RFC 3711, 4.3.1 and 4.3.3: AES in counter mode under the master key from the block of the salt field with the label, the index's bytes nought at a key derivation rate of nought, the output cut to its length.
fn prf<const N: usize>(crypto: &impl Crypto, key: &Key, input: &[u8; PRF_INPUT], label: u8) -> Option<[u8; N]> {
    let mut block = [0u8; BLOCK_BYTES];
    block[..PRF_INPUT].copy_from_slice(input);
    block[LABEL_AT] ^= label;
    let mut out = [0u8; N];
    for (counter, piece) in (0u16..).zip(out.chunks_mut(BLOCK_BYTES)) {
        block[PRF_INPUT..].copy_from_slice(&counter.to_be_bytes());
        piece.copy_from_slice(&crypto.encrypt(key, &block)?[..piece.len()]);
    }
    Some(out)
}

fn xor(into: &mut [u8], from: &[u8]) {
    into.iter_mut().zip(from).for_each(|(byte, other)| *byte ^= other);
}

// RFC 7714, 8.1: the salt, the SSRC over its bytes 2 to 5 and the 48-bit index over 6 to 11.
fn rtp_nonce(salt: &[u8; SALT_BYTES], ssrc: u32, index: u64) -> [u8; NONCE_BYTES] {
    let mut nonce = *salt;
    xor(&mut nonce[NONCE_SSRC_AT..NONCE_SSRC_AT + SSRC_BYTES], &ssrc.to_be_bytes());
    let index = index.to_be_bytes();
    xor(&mut nonce[NONCE_INDEX_AT..], &index[index.len() - INDEX_BYTES..]);
    nonce
}

// RFC 7714, 9.1: the salt, the SSRC over its bytes 2 to 5 and the 31-bit index over 8 to 11.
fn rtcp_nonce(salt: &[u8; SALT_BYTES], ssrc: u32, index: u32) -> [u8; NONCE_BYTES] {
    let mut nonce = *salt;
    xor(&mut nonce[NONCE_SSRC_AT..NONCE_SSRC_AT + SSRC_BYTES], &ssrc.to_be_bytes());
    xor(&mut nonce[NONCE_RTCP_INDEX_AT..], &index.to_be_bytes());
    nonce
}

// RFC 7714, 9.2: the compound's first eight bytes and the word of the E flag and the index.
fn rtcp_extra(compound: &[u8], word: u32) -> Option<[u8; RTCP_HEAD + INDEX_WORD]> {
    let mut extra = [0u8; RTCP_HEAD + INDEX_WORD];
    extra[..RTCP_HEAD].copy_from_slice(compound.get(..RTCP_HEAD)?);
    extra[RTCP_HEAD..].copy_from_slice(&word.to_be_bytes());
    Some(extra)
}

impl Outbound {
    // None where the library fails.
    pub(super) fn new(crypto: &impl Crypto, master: &Master) -> Option<Outbound> {
        Some(Outbound {
            rtp: master.session(crypto, RTP_KEY, RTP_SALT)?,
            rtcp: master.session(crypto, RTCP_KEY, RTCP_SALT)?,
            highest: Vec::new(),
            index: 0,
        })
    }

    // Encrypts an RTP packet's payload and padding in place and appends the tag (RFC 7714, 8.2), its index the extension of its sequence number nearest its stream's highest (RFC 3711, 3.3.1); false where it is no packet, its stream is one too many, its index is spent or the library fails, and it is then not to be sent.
    pub(super) fn protect(&mut self, crypto: &impl Crypto, packet: &mut Vec<u8>) -> bool {
        let (Some(head), Some(sequence), Some(ssrc)) =
            (head_length(packet), be16(packet, SEQUENCE_AT), be32(packet, SSRC_AT))
        else {
            return false;
        };
        let place = self.highest.iter().position(|(stream, _)| *stream == ssrc);
        let index = match place {
            Some(at) => extend(sequence, self.highest[at].1),
            None if self.highest.len() < OWN_STREAMS => u64::from(sequence),
            None => return false,
        };
        if index >= INDICES {
            return false;
        }
        let nonce = rtp_nonce(&self.rtp.salt, ssrc, index);
        let (header, payload) = packet.split_at_mut(head);
        let Some(tag) = crypto.seal(&self.rtp.key, &nonce, header, payload) else {
            return false;
        };
        packet.extend(tag);
        match place {
            Some(at) => self.highest[at].1 = self.highest[at].1.max(index),
            None => self.highest.push((ssrc, index)),
        }
        true
    }

    // Encrypts an RTCP compound after its first eight bytes in place and appends the tag, then the E flag with this side's next SRTCP index (RFC 7714, 9.2); false where it is shorter than a packet's head, the indices are spent or the library fails.
    pub(super) fn protect_rtcp(&mut self, crypto: &impl Crypto, compound: &mut Vec<u8>) -> bool {
        let (Some(ssrc), true) = (be32(compound, rtcp::SSRC_AT), self.index < RTCP_INDICES) else {
            return false;
        };
        let word = ENCRYPTED | self.index;
        let (Some(extra), nonce) = (rtcp_extra(compound, word), rtcp_nonce(&self.rtcp.salt, ssrc, self.index)) else {
            return false;
        };
        let Some(tag) = crypto.seal(&self.rtcp.key, &nonce, &extra, &mut compound[RTCP_HEAD..]) else {
            return false;
        };
        compound.extend(tag);
        compound.extend(word.to_be_bytes());
        self.index += 1;
        true
    }
}

impl Inbound {
    // Opening the streams of at most `most` SSRCs, which the call's size bounds; none where the library fails.
    pub(super) fn new(crypto: &impl Crypto, master: &Master, most: usize) -> Option<Inbound> {
        Some(Inbound {
            rtp: master.session(crypto, RTP_KEY, RTP_SALT)?,
            rtcp: master.session(crypto, RTCP_KEY, RTCP_SALT)?,
            streams: Vec::new(),
            most,
        })
    }

    // Where a stream's state lies; none for a stream past the most, which is refused.
    fn find(&self, ssrc: u32) -> Option<Option<usize>> {
        let place = self.streams.iter().position(|stream| stream.ssrc == ssrc);
        (place.is_some() || self.streams.len() < self.most).then_some(place)
    }

    // The state of a stream, made for a packet that opened.
    fn stream(&mut self, place: Option<usize>, ssrc: u32) -> &mut Stream {
        let at = place.unwrap_or_else(|| {
            self.streams.push(Stream { ssrc, highest: None, rtp: Window::default(), rtcp: Window::default() });
            self.streams.len() - 1
        });
        &mut self.streams[at]
    }

    // Opens an SRTP packet in place: the packet without its tag; none for a packet that does not open, comes again, lies too far behind, or of a stream past the most. Its index is estimated (RFC 3711, 3.3.1), checked against replays before it opens, and taken after (3.3, steps 5 and 7).
    pub(super) fn unprotect<'p>(&mut self, crypto: &impl Crypto, datagram: &'p mut [u8]) -> Option<&'p mut [u8]> {
        let head = head_length(datagram)?;
        let length = datagram.len().checked_sub(TAG_BYTES).filter(|length| *length >= head)?;
        let (sequence, ssrc) = (be16(datagram, SEQUENCE_AT)?, be32(datagram, SSRC_AT)?);
        let place = self.find(ssrc)?;
        let known = place.map(|at| &self.streams[at]);
        let index =
            known.and_then(|stream| stream.highest).map_or(u64::from(sequence), |highest| extend(sequence, highest));
        if index >= INDICES || known.is_some_and(|stream| !stream.rtp.is_new(index)) {
            return None;
        }
        let nonce = rtp_nonce(&self.rtp.salt, ssrc, index);
        let (header, sealed) = datagram.split_at_mut(head);
        if !crypto.open(&self.rtp.key, &nonce, header, sealed) {
            return None;
        }
        let stream = self.stream(place, ssrc);
        stream.rtp.take(index);
        stream.highest = Some(stream.highest.map_or(index, |highest| highest.max(index)));
        Some(&mut datagram[..length])
    }

    // Opens an SRTCP compound in place, encrypted or only signed (RFC 7714, 9.2 and 9.3): the compound without its tag and index; none for one that does not open, an index taken before or too far behind, or a stream past the most.
    pub(super) fn unprotect_rtcp<'p>(&mut self, crypto: &impl Crypto, datagram: &'p mut [u8]) -> Option<&'p mut [u8]> {
        let length = datagram.len().checked_sub(TAG_BYTES + INDEX_WORD).filter(|length| *length >= RTCP_HEAD)?;
        let (ssrc, word) = (be32(datagram, rtcp::SSRC_AT)?, be32(datagram, length + TAG_BYTES)?);
        let index = word & !ENCRYPTED;
        let place = self.find(ssrc)?;
        if place.is_some_and(|at| !self.streams[at].rtcp.is_new(u64::from(index))) {
            return None;
        }
        let nonce = rtcp_nonce(&self.rtcp.salt, ssrc, index);
        let is_open = if word & ENCRYPTED == 0 {
            // The index's word goes ahead of the tag so that all the associated data lies in one piece.
            datagram[length..].rotate_right(INDEX_WORD);
            let (extra, tag) = datagram.split_at_mut(length + INDEX_WORD);
            crypto.open(&self.rtcp.key, &nonce, extra, tag)
        } else {
            let extra = rtcp_extra(datagram, word)?;
            crypto.open(&self.rtcp.key, &nonce, &extra, &mut datagram[RTCP_HEAD..length + TAG_BYTES])
        };
        if !is_open {
            return None;
        }
        self.stream(place, ssrc).rtcp.take(u64::from(index));
        Some(&mut datagram[..length])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::code::hex;
    use crate::crypto::exact;
    use crate::net::rtp::tests::{Exact, bytes, key_bytes, rtp};
    use crate::net::wireguard::tests::Toy;
    use alloc::vec;

    // RFC 7714, 16 and 17: the keys 00 01 02 and on, the salt "Quid pro quo".
    fn session(length: usize) -> Session {
        let key: Vec<u8> = (0..length as u8).collect();
        let key = if length == AES128_BYTES {
            Key::Aes128(key.try_into().unwrap())
        } else {
            Key::Aes256(key.try_into().unwrap())
        };
        Session { key, salt: *b"Quid pro quo" }
    }

    fn outbound(length: usize, index: u32) -> Outbound {
        Outbound { rtp: session(length), rtcp: session(length), highest: Vec::new(), index }
    }

    fn inbound(length: usize) -> Inbound {
        Inbound { rtp: session(length), rtcp: session(length), streams: Vec::new(), most: 4 }
    }

    const RTP: &str = "8040f17b 8041f8d3 5501a0b2 47616c6c 69612065 7374206f 6d6e6973 20646976 69736120 696e2070 61727465 73207472 6573";
    const RTCP: &str = "81c8000d 4d617273 4e545031 4e545032 52545020 0000042a 0000e930 4c756e61 deadbeef deadbeef deadbeef deadbeef deadbeef";

    #[test]
    fn rfc_3711_and_6188_key_derivation() {
        let key = Key::Aes128(bytes("E1F97A0D3E018BE0D64FA32C06DE4139").try_into().unwrap());
        let salt: [u8; PRF_INPUT] = bytes("0EC675AD498AFEEBB6960B3AABE6").try_into().unwrap();
        assert_eq!(
            hex(&prf::<16>(&Exact, &key, &salt, 0).unwrap()),
            "c61e7a93744f39ee10734afe3ff7a087",
            "B.3's cipher key"
        );
        assert_eq!(
            hex(&prf::<14>(&Exact, &key, &salt, 2).unwrap()),
            "30cbbc08863d8c85d49db34a9ae1",
            "B.3's cipher salt"
        );
        assert_eq!(
            hex(&prf::<94>(&Exact, &key, &salt, 1).unwrap()),
            [
                "cebe321f6ff7716b6fd4ab49af256a15",
                "6d38baa48f0a0acf3c34e2359e6cdbce",
                "e049646c43d9327ad175578ef7227098",
                "6371c10c9a369ac2f94a8c5fbcdddc25",
                "6d6e919a48b610ef17c2041e47403576",
                "6b68642c59bbfc2f34db60dbdfb2"
            ]
            .concat(),
            "B.3's auth key: six blocks of the counter"
        );
        let key =
            Key::Aes256(bytes("f0f04914b513f2763a1b1fa130f10e29 98f6f6e43e4309d1e622a0e332b9f1b6").try_into().unwrap());
        let salt: [u8; PRF_INPUT] = bytes("3b04803de51ee7c96423ab5b78d2").try_into().unwrap();
        assert_eq!(
            hex(&prf::<32>(&Exact, &key, &salt, 0).unwrap()),
            "5ba1064e30ec51613cad926c5a28ef731ec7fb397f70a960653caf06554cd8c4",
            "RFC 6188, 7.2: AES_256_CM_PRF's cipher key of two blocks"
        );
        assert_eq!(hex(&prf::<14>(&Exact, &key, &salt, 2).unwrap()), "fa31791685ca444a9e07c6c64e93", "its cipher salt");
        assert_eq!(
            hex(&prf::<20>(&Exact, &key, &salt, 1).unwrap()),
            "fd9c32d39ed5fbb5a9dc96b30818454d1313dc05",
            "its auth key"
        );
    }

    #[test]
    fn session_keys_as_the_server_derives() {
        let (master_key, master_salt): (Vec<u8>, Vec<u8>) = ((1..=16).collect(), (101..=112).collect());
        let master = Master::new(&master_key, &master_salt).unwrap();
        // The calling server's `derive_key_from_master`: the salt at the block's start, the label at its byte 7.
        let server = |label: u8| {
            let mut block = [0u8; 16];
            block[..SALT_BYTES].copy_from_slice(&master_salt);
            block[7] ^= label;
            exact::aes(&master_key, &block)
        };
        for (key_label, salt_label) in [(RTP_KEY, RTP_SALT), (RTCP_KEY, RTCP_SALT)] {
            let derived = master.session(&Exact, key_label, salt_label).unwrap();
            assert_eq!(key_bytes(&derived.key), server(key_label), "the key of label {key_label}");
            assert_eq!(derived.salt, server(salt_label)[..SALT_BYTES], "the salt of label {salt_label}");
        }
        let long: Vec<u8> = (1..=32).collect();
        let derived = Master::new(&long, &master_salt).unwrap().session(&Exact, RTP_KEY, RTP_SALT).unwrap();
        assert!(matches!(derived.key, Key::Aes256(_)), "a master key of 32 bytes gives a session key of 32");
        assert!(
            Master::new(&long[..24], &master_salt).is_none() && Master::new(&long[..16], &master_salt[..11]).is_none()
        );
    }

    #[test]
    fn rfc_7714_srtp_vectors() {
        assert_eq!(hex(&rtp_nonce(b"Quid pro quo", 0x5501_a0b2, 0xf17b)), "51753c6580c2726f20718414", "16's IV");
        for (length, sealed, tag) in [
            (
                16,
                "8040f17b 8041f8d3 5501a0b2 f24de3a3 fb34de6c acba861c 9d7e4bca be633bd5 0d294e6f 42a5f47a 51c7d19b 36de3adf 8833899d 7f27beb1 6a9152cf 765ee439 0cce",
                "22493f82d2bce397e9d79e3b19aa4216",
            ),
            (
                32,
                "8040f17b 8041f8d3 5501a0b2 32b1de78 a822fe12 ef9f78fa 332e33aa b1801238 9a58e2f3 b50b2a02 76ffae0f 1ba63799 b87b7aa3 db36dfff d6b0f9bb 7878d7a7 6c13",
                "a866d5910f887463067ceefec45215d4",
            ),
        ] {
            let mut packet = bytes(RTP);
            assert!(outbound(length, 0).protect(&Exact, &mut packet), "{length}-byte key");
            assert_eq!(hex(&packet), hex(&bytes(sealed)), "16.{}.1: the encrypted and tagged packet", length / 16);
            let mut receiver = inbound(length);
            let opened = receiver.unprotect(&Exact, &mut packet).map(|opened| opened.to_vec());
            assert_eq!(opened, Some(bytes(RTP)), "16.{}.2: verified and decrypted", length / 16);
            let session = session(length);
            let nonce = rtp_nonce(&session.salt, 0x5501_a0b2, 0xf17b);
            let only_signed = Exact.seal(&session.key, &nonce, &bytes(RTP), &mut []).unwrap();
            assert_eq!(
                hex(&only_signed),
                tag,
                "16.{}.3: the tag over the whole packet as associated data",
                length / 16
            );
            let mut tagged = only_signed.to_vec();
            assert!(Exact.open(&session.key, &nonce, &bytes(RTP), &mut tagged), "16.{}.4: verified", length / 16);
        }
    }

    #[test]
    fn rfc_7714_srtcp_vectors() {
        let encrypted_128 = "81c8000d 4d617273 63e94885 dcdab67c a727d766 2f6b7e99 7ff5c0f7 6c06f32d c676a5f1 730d6fda 4ce09b46 86303ded 0bb9275b c84aa458 96cf4d2f c5abf872 45d9eade 800005d4";
        let mut compound = bytes(RTCP);
        let mut sender = outbound(16, 0x05d4);
        assert!(sender.protect_rtcp(&Exact, &mut compound));
        assert_eq!(hex(&compound), hex(&bytes(encrypted_128)), "17.1: encrypted and tagged, the E flag set");
        assert_eq!(sender.index, 0x05d5, "the index counts on");
        let opened = inbound(16).unprotect_rtcp(&Exact, &mut compound).map(|opened| opened.to_vec());
        assert_eq!(opened, Some(bytes(RTCP)), "and opened");
        let mut encrypted_256 = bytes(
            "81c8000d 4d617273 d50ae4d1 f5ce5d30 4ba297e4 7d470c28 2c3ece5d bffe0a50 a2eaa5c1 110555be 8415f658 c61de047 6f1b6fad 1d1eb30c 4446839f 57ff6f6c b26ac3be 800005d4",
        );
        let opened = inbound(32).unprotect_rtcp(&Exact, &mut encrypted_256).map(|opened| opened.to_vec());
        assert_eq!(opened, Some(bytes(RTCP)), "17.2: verified and decrypted");
        for (length, tag) in [(16, "841dd9683dd78ec92ae58790125f62b3"), (32, "91db4afbfeee5a978fab4393ed2615fe")] {
            let word = 0x05d4u32.to_be_bytes();
            let session = session(length);
            let nonce = rtcp_nonce(&session.salt, 0x4d61_7273, 0x05d4);
            assert_eq!(hex(&nonce), "517524055203726f207170bb", "17's IV");
            let signed = Exact.seal(&session.key, &nonce, &[bytes(RTCP), word.to_vec()].concat(), &mut []).unwrap();
            assert_eq!(hex(&signed), tag, "17.{}: the tag of the compound only signed", length / 16 + 2);
            let mut tagged = [bytes(RTCP), bytes(tag), word.to_vec()].concat();
            let opened = inbound(length).unprotect_rtcp(&Exact, &mut tagged).map(|opened| opened.to_vec());
            assert_eq!(opened, Some(bytes(RTCP)), "17.{}: verified, the E flag clear", length / 16 + 2);
        }
    }

    fn pair(crypto: &Toy) -> (Outbound, Inbound) {
        let master = Master::new(&[9; 16], &[8; SALT_BYTES]).unwrap();
        (Outbound::new(crypto, &master).unwrap(), Inbound::new(crypto, &master, 3).unwrap())
    }

    #[test]
    fn round_trips_and_replays() {
        let crypto = Toy::default();
        let (mut sender, mut receiver) = pair(&crypto);
        let mut sealed = rtp(1002, 40, b"voice");
        assert!(sender.protect(&crypto, &mut sealed));
        assert_eq!(
            (sealed.len(), &sealed[..12]),
            (12 + 5 + TAG_BYTES, &rtp(1002, 40, b"")[..]),
            "the header in the clear, the tag behind"
        );
        assert_ne!(&sealed[12..17], b"voice", "the payload encrypted");
        let again = sealed.clone();
        assert_eq!(
            receiver.unprotect(&crypto, &mut sealed).map(|opened| opened.to_vec()),
            Some(rtp(1002, 40, b"voice"))
        );
        assert_eq!(receiver.unprotect(&crypto, &mut again.clone()), None, "a replay");
        for at in [0, 1, 3, 7, 11, 14, again.len() - 1] {
            let mut changed = again.clone();
            changed[at] ^= 0x10;
            assert_eq!(receiver.unprotect(&crypto, &mut changed), None, "byte {at} changed: header, payload or tag");
        }
        let mut other = Outbound::new(&crypto, &Master::new(&[7; 16], &[8; SALT_BYTES]).unwrap()).unwrap();
        let mut foreign = rtp(1002, 41, b"voice");
        assert!(other.protect(&crypto, &mut foreign));
        assert_eq!(receiver.unprotect(&crypto, &mut foreign), None, "another key");
        let mut next = rtp(1002, 41, b"next");
        assert!(sender.protect(&crypto, &mut next));
        assert!(receiver.unprotect(&crypto, &mut next).is_some(), "a refusal changes nothing of the stream");
        assert_eq!(receiver.unprotect(&crypto, &mut [0x80; 12 + TAG_BYTES - 1]), None, "no room for the tag");
    }

    #[test]
    fn rollover_on_both_sides() {
        let crypto = Toy::default();
        let (mut sender, mut receiver) = pair(&crypto);
        let mut sent = Vec::new();
        for sequence in (0xFFF0..=0xFFFF).chain(0..0x10) {
            let mut sealed = rtp(1003, sequence, &sequence.to_be_bytes());
            assert!(sender.protect(&crypto, &mut sealed));
            sent.push(sealed);
        }
        assert_eq!(sender.highest, [(1003, 0x1_000F)], "the sender's rollover counter rose at the wrap");
        let order = [0, 1, 2, 17, 15, 16, 14, 3, 20, 18, 31, 19, 4, 30];
        for at in order {
            let opened = receiver.unprotect(&crypto, &mut sent[at].clone()).map(|opened| opened[12..].to_vec());
            let sequence = 0xFFF0u16.wrapping_add(at as u16);
            assert_eq!(
                opened,
                Some(sequence.to_be_bytes().to_vec()),
                "packet {sequence:#x}, out of order across the wrap"
            );
        }
        assert_eq!(receiver.streams[0].highest, Some(0x1_000F), "the receiver's rollover counter and highest");
        assert_eq!(receiver.unprotect(&crypto, &mut sent[17].clone()), None, "a replay across the wrap");
        let mut first_cycle = rtp(1003, 0xFFF5, &0xFFF5u16.to_be_bytes());
        assert!(sender.protect(&crypto, &mut first_cycle), "a retransmission of the cycle before keeps its index");
        assert_eq!(first_cycle, sent[5], "the same index, the same bytes");
        assert!(receiver.unprotect(&crypto, &mut first_cycle).is_some(), "taken: it had not come");
    }

    #[test]
    fn rtcp_round_trips_and_replays() {
        let crypto = Toy::default();
        let (mut sender, mut receiver) = pair(&crypto);
        let report = [0x80, 0xC9, 0, 1, 0, 0, 0x07, 0xD1];
        for index in 0..3u32 {
            let mut compound = report.to_vec();
            assert!(sender.protect_rtcp(&crypto, &mut compound));
            assert_eq!(
                compound[compound.len() - 4..],
                (ENCRYPTED | index).to_be_bytes(),
                "the index counts from nought"
            );
            let again = compound.clone();
            assert_eq!(
                receiver.unprotect_rtcp(&crypto, &mut compound).map(|opened| opened.to_vec()),
                Some(report.to_vec())
            );
            assert_eq!(receiver.unprotect_rtcp(&crypto, &mut again.clone()), None, "a replay of index {index}");
            let mut changed = again.clone();
            changed[2] ^= 1;
            assert_eq!(receiver.unprotect_rtcp(&crypto, &mut changed), None, "the head is signed");
            let mut flag = again;
            let last = flag.len() - 4;
            flag[last] ^= 0x80;
            assert_eq!(receiver.unprotect_rtcp(&crypto, &mut flag), None, "the E flag is signed");
        }
        sender.index = RTCP_INDICES - 1;
        assert!(sender.protect_rtcp(&crypto, &mut report.to_vec()), "the last index");
        assert!(!sender.protect_rtcp(&crypto, &mut report.to_vec()), "then the key is spent");
        assert!(!sender.protect_rtcp(&crypto, &mut report[..7].to_vec()), "no head");
    }

    #[test]
    fn bounds() {
        let crypto = Toy::default();
        let (mut sender, mut receiver) = pair(&crypto);
        for ssrc in 0..OWN_STREAMS as u32 {
            assert!(sender.protect(&crypto, &mut rtp(ssrc, 1, b"")), "stream {ssrc}");
        }
        assert!(!sender.protect(&crypto, &mut rtp(99, 1, b"")), "one stream too many of this side's");
        for ssrc in 0..4 {
            let mut sealed = rtp(ssrc, 1, b"");
            assert!(sender.protect(&crypto, &mut sealed));
            assert_eq!(
                receiver.unprotect(&crypto, &mut sealed).is_some(),
                ssrc < 3,
                "the far side's stream {ssrc} of three"
            );
        }
        sender.highest[0].1 = INDICES - 1;
        assert!(!sender.protect(&crypto, &mut rtp(0, 0, b"")), "the 48-bit index spent");
        assert!(!sender.protect(&crypto, &mut vec![0x80, 0, 0]), "no packet");
    }
}

//! A call's media packets: RTP with its header extensions and its VP8, VP9 and RED payloads, RTCP with its feedback, SRTP and SRTCP over AES-GCM, what a stream sent and a stream received keep, video frames put together, the send rate and its pacing, and the session of a call that ties them.

mod allocation;
mod demux;
mod dependency;
pub(crate) mod estimate;
pub(crate) mod extension;
mod feedback;
pub(crate) mod frames;
mod history;
pub(crate) mod layout;
mod nack;
mod pacer;
mod packet;
mod receive;
mod red;
mod rtcp;
mod rtx;
mod send;
pub(crate) mod session;
pub(crate) mod srtp;
mod transport_cc;
mod vp8;
mod vp9;

use crate::base::time::NANOS_PER_SECOND;
use core::time::Duration;

/// The bytes of AES-GCM's tag, which SRTP and SRTCP carry whole (RFC 7714, 13.2).
pub const TAG_BYTES: usize = 16;
/// The bytes of AES-GCM's nonce (RFC 7714, 8.1).
pub const NONCE_BYTES: usize = 12;
/// The bytes of a block of AES (FIPS 197).
pub const BLOCK_BYTES: usize = 16;
pub(crate) const AES128_BYTES: usize = 16;
pub(crate) const AES256_BYTES: usize = 32;
// RFC 3550, 5.1 and 6.4.1: the version in the first byte's two highest bits, the padding bit after them, lengths in 32-bit words, an SSRC and a sequence number.
const VERSION: u8 = 2;
const VERSION_SHIFT: u8 = 6;
const PADDING_BIT: u8 = 0x20;
const WORD: usize = 4;
const SSRC_BYTES: usize = 4;
const SEQUENCE_BYTES: usize = 2;
// RFC 3711, appendix A: an index estimate looks half the space of sequence numbers either way.
const HALF: u16 = 1 << 15;
// A counter's first value is taken in its second cycle, so that one a little before it is not taken for one a whole cycle ahead.
const FIRST_CYCLE: u64 = 1;
// RFC 3550, 4: the middle 32 bits of NTP's 64 that reports carry.
const NTP_MIDDLE_SHIFT: u32 = 16;

/// An AES key, whose length picks the suite: `AEAD_AES_128_GCM` or `AEAD_AES_256_GCM` (RFC 7714, 12); it has no `Debug`.
#[derive(PartialEq, Eq)]
pub enum Key {
    Aes128([u8; AES128_BYTES]),
    Aes256([u8; AES256_BYTES]),
}

/// AES-GCM and AES a library provides.
pub trait Crypto {
    // AES-GCM (NIST SP 800-38D) in place: the tag, or none where the library fails.
    fn seal(&self, key: &Key, nonce: &[u8; NONCE_BYTES], extra: &[u8], data: &mut [u8]) -> Option<[u8; TAG_BYTES]>;
    // In place over the data and the tag behind it: false for what was not sealed so, and the data is then no plaintext.
    fn open(&self, key: &Key, nonce: &[u8; NONCE_BYTES], extra: &[u8], sealed: &mut [u8]) -> bool;
    // One block under AES (FIPS 197) for the key derivation's counter mode: none where the library fails.
    fn encrypt(&self, key: &Key, block: &[u8; BLOCK_BYTES]) -> Option<[u8; BLOCK_BYTES]>;
}

// True for the first byte of RTP and RTCP, which RFC 7983 (7) tells from STUN's by.
pub(super) fn is_version(first: u8) -> bool {
    first >> VERSION_SHIFT == VERSION
}

// The extended number of a 16-bit sequence number nearest the highest extended number taken (RFC 3711, appendix A); none before the first cycle, which RFC 3711's rollover counter would take as its last.
fn extend(sequence: u16, highest: u64) -> u64 {
    extend_bits(u64::from(sequence), u16::BITS, highest)
}

// The extended number of a counter of `bits` bits nearest the highest extended number taken, as `extend` reckons a sequence number's.
fn extend_bits(value: u64, bits: u32, highest: u64) -> u64 {
    let (span, half) = (1u64 << bits, 1u64 << (bits - 1));
    let (cycle, last) = (highest >> bits, highest & (span - 1));
    let cycle = if last < half {
        if value > last + half { cycle.saturating_sub(1) } else { cycle }
    } else if value < last - half {
        cycle + 1
    } else {
        cycle
    };
    cycle << bits | value & (span - 1)
}

// A time in a clock's RTP units, wrapping as RTP timestamps do.
fn units(time: Duration, rate: u32) -> u32 {
    (time.as_nanos() * u128::from(rate) / u128::from(NANOS_PER_SECOND)) as u32
}

// A time of the caller's clock as an NTP timestamp, seconds in 32.32 fixed point (RFC 3550, 4), its seconds wrapping as NTP's do.
fn ntp(clock: Duration) -> u64 {
    let fraction = (u64::from(clock.subsec_nanos()) << u32::BITS) / NANOS_PER_SECOND;
    clock.as_secs() << u32::BITS | fraction
}

// The middle 32 bits of an NTP timestamp, in units of 1/65536 second (RFC 3550, 4).
fn ntp_middle(ntp: u64) -> u32 {
    (ntp >> NTP_MIDDLE_SHIFT) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KEY_BYTES, blake2s, exact};
    use crate::net::wireguard::tests::{Toy, mask, open_with, tag};
    use alloc::vec;
    use alloc::vec::Vec;

    // Bytes written as hex digits, spaces between them as the RFCs' figures have them.
    pub(super) fn bytes(text: &str) -> Vec<u8> {
        crate::base::code::unhex(&text.replace(' ', "")).unwrap()
    }

    pub(super) fn key_bytes(key: &Key) -> &[u8] {
        match key {
            Key::Aes128(key) => key,
            Key::Aes256(key) => key,
        }
    }

    // An RTP packet of a stream's SSRC and sequence number with a payload, its header as `packet::Header` writes it.
    pub(super) fn rtp(ssrc: u32, sequence: u16, payload: &[u8]) -> Vec<u8> {
        let header = packet::Header {
            marker: false,
            payload_type: 102,
            sequence,
            timestamp: 960,
            ssrc,
            csrcs: &[],
            extensions: &[],
        };
        let mut packet = Vec::new();
        assert!(header.write(&mut packet));
        packet.extend_from_slice(payload);
        packet
    }

    // The tunnel's toy: a stream of hashes in place of AES-GCM, under a key filled up to its 32 bytes.
    fn toy_key(key: &Key) -> [u8; KEY_BYTES] {
        let mut wide = [0u8; KEY_BYTES];
        wide[..key_bytes(key).len()].copy_from_slice(key_bytes(key));
        wide
    }

    impl Crypto for Toy {
        fn seal(&self, key: &Key, nonce: &[u8; NONCE_BYTES], extra: &[u8], data: &mut [u8]) -> Option<[u8; TAG_BYTES]> {
            let key = toy_key(key);
            let signed = tag(&key, nonce, extra, data);
            mask(&key, nonce, data);
            Some(signed)
        }

        fn open(&self, key: &Key, nonce: &[u8; NONCE_BYTES], extra: &[u8], sealed: &mut [u8]) -> bool {
            open_with(&toy_key(key), nonce, extra, sealed)
        }

        fn encrypt(&self, key: &Key, block: &[u8; BLOCK_BYTES]) -> Option<[u8; BLOCK_BYTES]> {
            blake2s::hash(&[&toy_key(key), block]).first_chunk().copied()
        }
    }

    // AES and AES-GCM as the standards define them, for RFC 7714's and RFC 3711's vectors.
    pub(super) struct Exact;

    impl Crypto for Exact {
        fn seal(&self, key: &Key, nonce: &[u8; NONCE_BYTES], extra: &[u8], data: &mut [u8]) -> Option<[u8; TAG_BYTES]> {
            Some(exact::gcm_seal(key_bytes(key), nonce, extra, data))
        }

        fn open(&self, key: &Key, nonce: &[u8; NONCE_BYTES], extra: &[u8], sealed: &mut [u8]) -> bool {
            exact::gcm_open(key_bytes(key), nonce, extra, sealed)
        }

        fn encrypt(&self, key: &Key, block: &[u8; BLOCK_BYTES]) -> Option<[u8; BLOCK_BYTES]> {
            Some(exact::aes(key_bytes(key), block))
        }
    }

    #[test]
    fn index_estimate() {
        assert_eq!(extend(5, 3), 5, "close ahead");
        assert_eq!(extend(2, 3), 2, "close behind");
        assert_eq!(extend(3, 0xFFF0), 0x1_0003, "ahead across the wrap: the next cycle");
        assert_eq!(extend(0xFFF0, 0x1_0003), 0xFFF0, "behind across the wrap: the cycle before");
        assert_eq!(extend(0xFFF0, 3), 0xFFF0, "nothing lies before the first cycle");
        assert_eq!(
            (extend(3 + HALF, 3), extend(4 + HALF, 3)),
            (u64::from(3 + HALF), u64::from(4 + HALF)),
            "appendix A: a half cycle ahead is this cycle, and so is more where none lies before"
        );
        assert_eq!(
            (extend(3 + HALF, 0x5_0003), extend(4 + HALF, 0x5_0003)),
            (0x5_8003, 0x4_8004),
            "appendix A: past half a cycle ahead is the cycle before"
        );
        assert_eq!(
            (extend(0x7FFF, 0x2_FFFF), extend(0x7FFE, 0x2_FFFF)),
            (0x2_7FFF, 0x3_7FFE),
            "appendix A: half a cycle behind is this cycle, past it the next"
        );
        assert_eq!(extend(7, u64::MAX >> u16::BITS << u16::BITS), u64::MAX >> u16::BITS << u16::BITS | 7);
    }

    #[test]
    fn ntp_times() {
        assert_eq!(ntp(Duration::new(3, 500_000_000)), 3 << 32 | 1 << 31, "half a second is the fraction's top bit");
        assert_eq!(ntp(Duration::new(0, 999_999_999)), 0xFFFF_FFFB, "the fraction rounded down");
        assert_eq!(ntp_middle(0xb44d_b705_2000_0000), 0xb705_2000, "RFC 3550's figure 2: SR(n)'s LSR");
        assert_eq!(ntp(Duration::from_secs(1 << 32 | 9)), 9 << 32, "the seconds wrap");
    }

    // What each reader, the openers and the stream states make of a datagram; nothing is asked of it but that every call returns.
    struct Readers {
        inbound: srtp::Inbound,
        receiver: receive::Receiver,
        missing: nack::Missing,
        recorder: transport_cc::Recorder,
        history: history::History,
        rtx_stream: send::Sender,
        departures: transport_cc::Departures,
        structure: dependency::Structure,
        sessions: [session::Session; 2],
    }

    impl Readers {
        fn take(&mut self, crypto: &Toy, datagram: &[u8], now: Duration) {
            demux::kind_of(datagram);
            for at in [0, 1, 12, 16, 24] {
                let bytes = datagram.get(at..).unwrap_or_default();
                let _ = (vp8::Descriptor::read(bytes), vp9::Descriptor::read(bytes), red::primary(bytes));
                let _ = (dependency::Descriptor::mandatory(bytes), dependency::Descriptor::read(bytes, None));
                let _ = dependency::Descriptor::read(bytes, Some(&self.structure));
            }
            for session in &mut self.sessions {
                session.take(crypto, &mut datagram.to_vec(), now);
                while session.poll(crypto, now).is_some() {}
            }
            if let Some(read) = packet::Packet::read(datagram) {
                read.extensions.elements().for_each(|_| ());
                let extensions = read.extensions;
                let _ = (extensions.transport_sequence(), extensions.orientation(), extensions.audio_level());
                self.receiver.receive(read.sequence, read.timestamp, now);
                self.missing.receive(read.sequence);
                self.recorder.note(read.sequence, now);
                self.history.keep(datagram, now);
                self.history.resend(read.sequence.wrapping_sub(3), now, Duration::ZERO);
                self.history.resend_rtx(read.sequence, now, Duration::ZERO, &mut self.rtx_stream, 118, &mut Vec::new());
            }
            for read in rtcp::read(datagram) {
                match read {
                    rtcp::Packet::SenderReport(report) => {
                        report.blocks().for_each(|block| {
                            let _ = block.round_trip(now);
                        });
                        self.receiver.note_report(report.info.ntp, now);
                    }
                    rtcp::Packet::ReceiverReport(report) => report.blocks().for_each(|_| ()),
                    rtcp::Packet::Nack(nack) => nack.lost().for_each(|_| ()),
                    rtcp::Packet::TransportFeedback(feedback) => {
                        feedback.statuses().for_each(|_| ());
                        self.departures.outcomes(&feedback);
                    }
                    rtcp::Packet::Fir(fir) => fir.entries().for_each(|_| ()),
                    rtcp::Packet::Remb(_) | rtcp::Packet::Pli(_) => {}
                }
            }
            rtx::unwrap(&mut datagram.to_vec(), 1, 96);
            packet::set_transport_sequence(&mut datagram.to_vec(), 7);
            self.departures.send(&mut datagram.to_vec(), datagram.len(), now);
            self.inbound.unprotect(crypto, &mut datagram.to_vec());
            self.inbound.unprotect_rtcp(crypto, &mut datagram.to_vec());
            self.receiver.block(now);
            self.missing.due(now, Duration::from_millis(100));
            self.recorder.write(&mut Vec::new(), 1, 2);
        }
    }

    // Random bytes and real packets with bits flipped, from a fixed seed, into every reader and opener, and into a session of each kind of call.
    #[test]
    fn strangers_bytes() {
        use crate::base::math::xorshift64;
        const CASES: u32 = 20_000;
        const LONGEST: u64 = 1500;
        let crypto = Toy::default();
        let master = srtp::Master::new(&[3; AES128_BYTES], &[4; srtp::SALT_BYTES]).unwrap();
        let mut sender = srtp::Outbound::new(&crypto, &master).unwrap();
        let bounds = estimate::Bounds { least: 30_000, start: 300_000, most: 2_000_000 };
        let session = |kind| session::Session::new(kind, &crypto, &master, &master, 16, bounds, 5).unwrap();
        let mut readers = Readers {
            inbound: srtp::Inbound::new(&crypto, &master, 16).unwrap(),
            receiver: receive::Receiver::new(1, 90_000),
            missing: nack::Missing::default(),
            recorder: transport_cc::Recorder::default(),
            history: history::History::default(),
            rtx_stream: send::Sender::new(2, 90_000, 0),
            departures: transport_cc::Departures::default(),
            structure: dependency::Structure::single(0, 640, 480),
            sessions: [session(layout::Kind::Direct { is_caller: true }), session(layout::Kind::Group { demux: 0x10 })],
        };
        let mut samples = vec![
            bytes("b1ec1234 000003e8 00000102 00000007 bede0001 11000500 6672616d 650002"),
            bytes(
                "90010002 00000003 00000004 10000007 06178008 578002044eaaaf2860414d34538a0940409fc077c0 000000 0102",
            ),
            bytes("80760102 03040506 0708090a 0b0c"),
            bytes("8fcd0009 00000001 000003ea 03e8000e 00006400 d2952007 040190ff fc010101 01010101 01010100"),
            bytes("81c80006 000003e9 b44db705 20000000 00001f40 00000064 00003e80"),
        ];
        let mut compound = bytes("81c90007 000003e9 000007d2 1c000001 00010002 00000018 b7052000 00054000");
        compound.extend(bytes(rtcp::tests::SDES));
        compound.extend(bytes(rtcp::tests::BYE));
        assert!(feedback::write_nack(&mut compound, 1, 2, &[5, 6, 9]) && feedback::write_pli(&mut compound, 1, 2));
        assert!(
            feedback::tests::write_fir(&mut compound, 1, &[(2, 3)])
                && feedback::tests::write_remb(&mut compound, 1, 500_000, &[2])
        );
        samples.push(compound.clone());
        let mut sealed = samples[0].clone();
        assert!(sender.protect(&crypto, &mut sealed) && sender.protect_rtcp(&crypto, &mut compound));
        samples.extend([sealed, compound]);
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state = xorshift64(state);
            state
        };
        for case in 0..CASES {
            let now = Duration::from_millis(u64::from(case) * 7);
            let length = (next() % LONGEST) as usize;
            let random: Vec<u8> = (0..length).map(|_| next() as u8).collect();
            readers.take(&crypto, &random, now);
            let mut flipped = samples[case as usize % samples.len()].clone();
            for _ in 0..1 + next() % 4 {
                let bit = next() as usize % (flipped.len() * 8);
                flipped[bit / 8] ^= 1 << (bit % 8);
            }
            readers.take(&crypto, &flipped, now);
        }
    }

    #[test]
    fn toy_and_exact_open_what_they_seal() {
        let nonce = [7u8; NONCE_BYTES];
        for (crypto, name) in [(&Toy::default() as &dyn Crypto, "toy"), (&Exact as &dyn Crypto, "exact")] {
            for key in [Key::Aes128([1; AES128_BYTES]), Key::Aes256([2; AES256_BYTES])] {
                let mut sealed = b"a packet of the call".to_vec();
                let signed = crypto.seal(&key, &nonce, b"head", &mut sealed).unwrap();
                sealed.extend(signed);
                let mut other = sealed.clone();
                assert!(crypto.open(&key, &nonce, b"head", &mut sealed), "{name} opens");
                assert!(sealed.starts_with(b"a packet of the call"), "{name} gives the plaintext back");
                assert!(!crypto.open(&key, &nonce, b"Head", &mut other), "{name}, other associated data");
            }
        }
    }
}

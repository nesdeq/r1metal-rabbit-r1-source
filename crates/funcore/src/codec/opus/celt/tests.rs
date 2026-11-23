extern crate std;

use super::super::packet::{Bandwidth, LARGEST_FRAME, Mode, Packet};
use super::super::support::{read_demo, read_packets, read_samples};
use super::*;
use crate::base::math::{to_sample, xorshift32};
use alloc::collections::BTreeSet;

// Prefixes of the official test vectors of RFC 8251 whose packets are CELT alone, cut after whole packets.
const VECTORS: [(&str, &[u8]); 4] = [
    ("testvector01", include_bytes!("../../../../assets/sound/opus/testvector01_cut.bit")),
    ("testvector07", include_bytes!("../../../../assets/sound/opus/testvector07_cut.bit")),
    ("testvector10", include_bytes!("../../../../assets/sound/opus/testvector10_cut.bit")),
    ("testvector11", include_bytes!("../../../../assets/sound/opus/testvector11_cut.bit")),
];

// A second of a harmonic tone, bursts of noise and hiss, encoded by ffmpeg's libopus in its low-delay application at each frame size, and libopus's decoder of one channel's samples of it.
const FIXTURES: [(&str, &[u8], &[u8]); 5] = [
    (
        "20 ms at 64 kb/s",
        include_bytes!("../../../../assets/sound/opus/celt_20ms_64k_mono.packets"),
        include_bytes!("../../../../assets/sound/opus/celt_20ms_64k_mono.pcm"),
    ),
    (
        "10 ms at 24 kb/s",
        include_bytes!("../../../../assets/sound/opus/celt_10ms_24k_mono.packets"),
        include_bytes!("../../../../assets/sound/opus/celt_10ms_24k_mono.pcm"),
    ),
    (
        "5 ms at 96 kb/s",
        include_bytes!("../../../../assets/sound/opus/celt_5ms_96k_mono.packets"),
        include_bytes!("../../../../assets/sound/opus/celt_5ms_96k_mono.pcm"),
    ),
    (
        "2.5 ms at 48 kb/s",
        include_bytes!("../../../../assets/sound/opus/celt_2_5ms_48k_mono.packets"),
        include_bytes!("../../../../assets/sound/opus/celt_2_5ms_48k_mono.pcm"),
    ),
    (
        "20 ms of two channels at 48 kb/s",
        include_bytes!("../../../../assets/sound/opus/celt_20ms_48k_stereo.packets"),
        include_bytes!("../../../../assets/sound/opus/celt_20ms_48k_stereo.pcm"),
    ),
];

// Decodes a CELT packet's frames onto `out`; the final range of its last frame, nought where that frame is of one byte or none, as Opus reports it.
fn decode_packet(decoder: &mut Decoder, packet: &[u8], out: &mut Vec<f32>) -> u32 {
    let parsed = Packet::read(packet).expect("a packet that parses");
    assert_eq!(parsed.toc.mode, Mode::Celt, "a packet of CELT alone");
    let mut last = 0;
    for frame in parsed.frames() {
        let mut range = RangeDecoder::new(frame);
        let at = out.len();
        out.resize(at + parsed.toc.frame, 0.0);
        let outcome = decoder.decode(&mut range, parsed.toc.bandwidth, false, parsed.toc.is_stereo, &mut out[at..]);
        assert_eq!(outcome, Ok(()), "a frame of a valid stream");
        last = if frame.len() <= 1 { 0 } else { range.range() };
    }
    last
}

#[test]
fn vectors_hold_final_range() {
    let mut seen = BTreeSet::new();
    for (name, file) in VECTORS {
        let mut decoder = Decoder::new();
        let mut out = Vec::new();
        let packets = read_demo(file);
        for (number, &(packet, final_range)) in packets.iter().enumerate() {
            let toc = Packet::read(packet).expect("a packet that parses").toc;
            seen.insert((toc.frame, toc.bandwidth, toc.is_stereo));
            assert_eq!(
                decode_packet(&mut decoder, packet, &mut out),
                final_range,
                "{name} packet {number}: the encoder's final range"
            );
        }
        assert!(packets.len() > 90, "{name}: a prefix of whole packets");
    }
    for frame in [120, 240, 480, 960] {
        for bandwidth in [Bandwidth::Narrow, Bandwidth::Wide, Bandwidth::SuperWide, Bandwidth::Full] {
            assert!(seen.contains(&(frame, bandwidth, false)), "one channel at {frame} samples and {bandwidth:?}");
        }
        assert!(seen.contains(&(frame, Bandwidth::Full, true)), "two channels at {frame} samples");
    }
}

// The most any sample lay from libopus's in any fixture, measured at 1; the bound is twice that.
const NEAREST: i32 = 2;
// The decibels of a fixture's samples above their difference from libopus's, measured at 86.9 at least (2.5 ms frames); the bound is 6 dB under it.
const AGREEMENT: f64 = 80.9;

#[test]
fn fixtures_match_libopus() {
    for (name, packets, reference) in FIXTURES {
        let (mut decoder, mut out) = (Decoder::new(), Vec::new());
        for packet in read_packets(packets) {
            decode_packet(&mut decoder, packet, &mut out);
        }
        let reference = read_samples(reference);
        assert_eq!(out.len(), reference.len(), "{name}: as many samples as libopus gives");
        let ours: Vec<i16> = out.iter().map(|&value| to_sample(value)).collect();
        let worst = ours.iter().zip(&reference).map(|(&one, &other)| (i32::from(one) - i32::from(other)).abs()).max();
        assert!(worst <= Some(NEAREST), "{name}: up to {worst:?} from libopus's samples");
        let signal: f64 = reference.iter().map(|&sample| f64::from(sample).powi(2)).sum();
        let noise: f64 =
            ours.iter().zip(&reference).map(|(&one, &other)| (f64::from(one) - f64::from(other)).powi(2)).sum();
        let ratio = 10.0 * (signal / noise.max(1.0)).log10();
        assert!(ratio > AGREEMENT, "{name}: {ratio} dB");
        assert!(ours.iter().any(|&sample| sample.unsigned_abs() > 5_000), "{name}: and there is sound in them");
    }
}

#[test]
fn frames_allocate_nothing() {
    let streams: Vec<Vec<&[u8]>> = FIXTURES.iter().map(|(_, packets, _)| read_packets(packets)).collect();
    let mut decoder = Decoder::new();
    let mut out = [0.0f32; 960];
    let before = crate::base::alloc_count::allocations();
    for packet in streams.iter().flatten() {
        let parsed = Packet::read(packet).expect("a packet that parses");
        let size = parsed.toc.frame;
        let mut range = RangeDecoder::new(parsed.frames()[0]);
        let outcome = decoder.decode(&mut range, parsed.toc.bandwidth, false, parsed.toc.is_stereo, &mut out[..size]);
        assert_eq!((outcome, decoder.conceal(&mut out[..size])), (Ok(()), Ok(())));
    }
    assert_eq!(crate::base::alloc_count::allocations(), before, "decoding and concealing allocate nothing");
}

#[test]
fn reset_decodes_as_new() {
    let (_, packets, _) = FIXTURES[0];
    let packets = read_packets(packets);
    let (mut used, mut fresh) = (Decoder::new(), Decoder::new());
    let (mut ours, mut theirs) = (Vec::new(), Vec::new());
    for packet in &packets[..12] {
        decode_packet(&mut used, packet, &mut ours);
    }
    used.reset();
    ours.clear();
    for packet in &packets {
        decode_packet(&mut used, packet, &mut ours);
        decode_packet(&mut fresh, packet, &mut theirs);
    }
    assert_eq!(ours, theirs, "nothing is left of the frames before a reset");
}

#[test]
fn silent_frame_and_sizes() {
    let mut decoder = Decoder::new();
    let mut out = [1.0f32; 120];
    let silence = [0xFF, 0xFF];
    let mut range = RangeDecoder::new(&silence);
    assert_eq!(decoder.decode(&mut range, Bandwidth::Full, false, false, &mut out), Ok(()));
    assert_eq!(out, [0.0; 120], "the flag of 1 in 32768 set: silence");
    assert_eq!(range.tell(), 16, "and every bit of the frame counted as read");
    let mut odd = [0.0f32; 100];
    let mut range = RangeDecoder::new(&silence);
    assert_eq!(
        decoder.decode(&mut range, Bandwidth::Full, false, false, &mut odd),
        Err(Flaw::Invalid("a frame size CELT does not have"))
    );
    assert_eq!(decoder.conceal(&mut odd), Err(Flaw::Invalid("a frame size CELT does not have")));
    let mut range = RangeDecoder::new(&silence);
    assert_eq!(
        decoder.decode(&mut range, Bandwidth::Wide, true, false, &mut out),
        Err(Flaw::Invalid("a hybrid frame of no band above SILK's")),
        "hybrid frames are super wideband or fullband"
    );
}

#[test]
fn concealment_fades() {
    let mut fresh = Decoder::new();
    let mut out = [0.0f32; 960];
    assert_eq!(fresh.conceal(&mut out), Ok(()));
    assert!(out.iter().all(|&sample| to_sample(sample) == 0), "nothing decoded, nothing to conceal");
    let (_, packets, _) = FIXTURES[0];
    let packets = read_packets(packets);
    let (mut decoder, mut played) = (Decoder::new(), Vec::new());
    for packet in &packets[..25] {
        decode_packet(&mut decoder, packet, &mut played);
    }
    let energy = |samples: &[f32]| samples.iter().map(|&sample| f64::from(sample).powi(2)).sum::<f64>();
    let heard = energy(&played[played.len() - 960..]);
    let mut lost = Vec::new();
    for _ in 0..8 {
        assert_eq!(decoder.conceal(&mut out), Ok(()));
        assert!(out.iter().all(|sample| sample.is_finite()));
        lost.push(energy(&out[OVERLAP..]));
    }
    assert!(lost[0] > 0.0 && lost[0] < heard, "noise under the last frame: {} for {heard}", lost[0]);
    assert!(lost.windows(2).take(4).all(|pair| pair[1] < pair[0]), "fading with each frame lost: {lost:?}");
    played.clear();
    decode_packet(&mut decoder, packets[25], &mut played);
    assert!(played.iter().any(|&sample| sample.abs() > 1_000.0), "and the next frame plays");
}

// A frame's bytes for the robustness test: a real frame with some bits turned over, or the generator's bytes.
fn bytes_of(state: &mut u32, frames: &[&[u8]], bytes: &mut Vec<u8>) {
    let mut next = || {
        *state = xorshift32(*state);
        *state
    };
    bytes.clear();
    if next() % 2 == 0 {
        bytes.extend_from_slice(frames[next() as usize % frames.len()]);
        for _ in 0..1 + next() % 8 {
            if !bytes.is_empty() {
                let at = next() as usize % bytes.len();
                bytes[at] ^= 1 << (next() % 8);
            }
        }
    } else {
        let len = next() as usize % (LARGEST_FRAME + 1);
        bytes.extend((0..len).map(|_| next() as u8));
    }
}

#[test]
fn hostile_frames_decode() {
    let mut frames: Vec<&[u8]> = Vec::new();
    for (_, file) in VECTORS {
        for &(packet, _) in read_demo(file).iter().step_by(17) {
            frames.extend(Packet::read(packet).expect("a packet that parses").frames());
        }
    }
    for (_, packets, _) in FIXTURES {
        for packet in read_packets(packets).into_iter().step_by(7) {
            frames.extend(Packet::read(packet).expect("a packet that parses").frames());
        }
    }
    let bandwidths = [Bandwidth::Narrow, Bandwidth::Medium, Bandwidth::Wide, Bandwidth::SuperWide, Bandwidth::Full];
    let (mut decoder, mut state, mut bytes) = (Decoder::new(), 0x1234_5678u32, Vec::new());
    let mut out = [0.0f32; 960];
    for case in 0..20_000 {
        bytes_of(&mut state, &frames, &mut bytes);
        state = xorshift32(state);
        let size = SHORT << (state % 4);
        let bandwidth = bandwidths[(state >> 2) as usize % bandwidths.len()];
        let (is_hybrid, is_stereo) = (state >> 5 & 3 == 0, state >> 7 & 1 == 1);
        let mut range = RangeDecoder::new(&bytes);
        let _ = decoder.decode(&mut range, bandwidth, is_hybrid, is_stereo, &mut out[..size]);
        assert!(out[..size].iter().all(|sample| sample.is_finite()), "case {case}: samples, finite");
        match state >> 8 & 31 {
            0 => decoder.reset(),
            1 => assert_eq!(decoder.conceal(&mut out[..size]), Ok(())),
            _ => {}
        }
    }
}

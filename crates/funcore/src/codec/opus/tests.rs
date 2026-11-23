extern crate std;

use super::support::{read_demo, read_packets, read_samples};
use super::*;
use crate::base::alloc_count::allocations;
use crate::base::checksum::crc32;
use crate::base::math::{sine_cosine, xorshift32};

// The official test vectors of RFC 8251, each a run of whole packets with the encoder's final range behind each: prefixes of at most 64 KB, vector 12 whole, and of vector 10 the packets from its byte 208703 to 241879, where CELT frames and hybrid ones take turns.
const VECTORS: [(&str, &[u8]); 11] = [
    ("01, CELT", include_bytes!("../../../assets/sound/opus/testvector01_cut.bit")),
    ("02, SILK at 8 kHz", include_bytes!("../../../assets/sound/opus/testvector02_cut.bit")),
    ("03, SILK at 12 kHz", include_bytes!("../../../assets/sound/opus/testvector03_cut.bit")),
    ("04, SILK at 16 kHz", include_bytes!("../../../assets/sound/opus/testvector04_cut.bit")),
    ("05, hybrid", HYBRID),
    ("07, CELT", include_bytes!("../../../assets/sound/opus/testvector07_cut.bit")),
    ("08, SILK then CELT", SILK_THEN_CELT),
    ("10, CELT", include_bytes!("../../../assets/sound/opus/testvector10_cut.bit")),
    ("10, CELT and hybrid by turns", TURNS),
    ("11, CELT", include_bytes!("../../../assets/sound/opus/testvector11_cut.bit")),
    ("12, SILK and hybrid by turns", include_bytes!("../../../assets/sound/opus/testvector12.bit")),
];
const HYBRID: &[u8] = include_bytes!("../../../assets/sound/opus/testvector05_cut.bit");
const SILK_THEN_CELT: &[u8] = include_bytes!("../../../assets/sound/opus/testvector08_cut.bit");
const TURNS: &[u8] = include_bytes!("../../../assets/sound/opus/testvector10_modes.bit");
// What libopus 1.6.1's decoder of one channel at 48 kHz makes of three of them.
const HYBRID_SOUND: &[u8] = include_bytes!("../../../assets/sound/opus/testvector05_cut.pcm");
const SILK_THEN_CELT_SOUND: &[u8] = include_bytes!("../../../assets/sound/opus/testvector08_cut.pcm");
const TURNS_SOUND: &[u8] = include_bytes!("../../../assets/sound/opus/testvector10_modes.pcm");
// SILK at 16 kHz in frames of 20 ms with a copy of each frame in the packet behind it (4.2.5).
const COPIES: &[u8] = include_bytes!("../../../assets/sound/opus/silk_wb_20ms_24k_fec.packets");

// A vector's packets decoded one after another: the sound, and the packets that did not end in their final range.
fn decode_all(file: &[u8]) -> (Vec<i16>, usize) {
    let (mut decoder, mut sound, mut out) = (Decoder::new(), Vec::new(), [0i16; 5760]);
    let mut strays = 0;
    for (packet, range) in read_demo(file) {
        let count = decoder.decode(packet, &mut out).expect("a packet that parses");
        assert_eq!(Ok(count), Decoder::samples_of(packet));
        sound.extend_from_slice(&out[..count]);
        strays += usize::from(decoder.final_range() != range);
    }
    (sound, strays)
}

#[test]
fn vectors_end_in_final_range() {
    let mut changes = Vec::new();
    for (name, file) in VECTORS {
        assert_eq!(decode_all(file).1, 0, "vector {name}: every packet ends in its encoder's final range");
        let modes: Vec<Mode> = read_demo(file).iter().map(|(packet, _)| Toc::read(packet[0]).mode).collect();
        changes.extend(modes.windows(2).filter(|pair| pair[0] != pair[1]).map(|pair| (pair[0], pair[1])));
    }
    for change in [
        (Mode::Silk, Mode::Celt),
        (Mode::Celt, Mode::Hybrid),
        (Mode::Hybrid, Mode::Celt),
        (Mode::Silk, Mode::Hybrid),
        (Mode::Hybrid, Mode::Silk),
    ] {
        assert!(changes.contains(&change), "the vectors change mode from {:?} to {:?}", change.0, change.1);
    }
}

#[test]
fn reset_decodes_as_new() {
    let (fresh, _) = decode_all(TURNS);
    let mut decoder = Decoder::new();
    let mut out = [0i16; 5760];
    for (packet, _) in read_demo(HYBRID).into_iter().take(40) {
        decoder.decode(packet, &mut out).expect("a packet that parses");
    }
    decoder.conceal(960, &mut out).expect("20 ms");
    decoder.reset();
    let mut again = Vec::new();
    for (packet, range) in read_demo(TURNS) {
        let count = decoder.decode(packet, &mut out).expect("a packet that parses");
        again.extend_from_slice(&out[..count]);
        assert_eq!(decoder.final_range(), range, "every packet in its encoder's final range");
    }
    assert!(again == fresh, "a decoder reset after other sound decodes sample for sample as a new one");
}

// The decibels of libopus's samples above their difference from this decoder's.
fn agreement(sound: &[i16], reference: &[i16]) -> f64 {
    let signal: f64 = reference.iter().map(|&sample| f64::from(sample).powi(2)).sum();
    let noise: f64 =
        sound.iter().zip(reference).map(|(&one, &other)| (f64::from(one) - f64::from(other)).powi(2)).sum();
    10.0 * (signal / noise.max(1.0)).log10()
}

#[test]
fn sound_near_libopus() {
    let late = Decoder::new().late as usize;
    assert_eq!(
        late, 238,
        "5 ms: what the filter from 8 kHz holds back, 264 samples, less the 26 SILK lies behind CELT"
    );
    // The bounds lie 6 dB under what was measured: 40.4, 67.9 and 49.7 dB. The SILK layer comes to 48 kHz by another filter than libopus's, of another phase.
    for (name, file, reference, bound) in [
        ("hybrid", HYBRID, HYBRID_SOUND, 34.4),
        ("SILK then CELT", SILK_THEN_CELT, SILK_THEN_CELT_SOUND, 61.9),
        ("CELT and hybrid by turns", TURNS, TURNS_SOUND, 43.7),
    ] {
        let (sound, reference) = (decode_all(file).0, read_samples(reference));
        assert_eq!(sound.len(), reference.len(), "{name}: as many samples as libopus gives");
        assert!(sound[..late].iter().all(|&sample| sample == 0), "{name}: silence for the time the sound is late by");
        let ratio = agreement(&sound[late..], &reference);
        assert!(ratio > bound, "{name}: {ratio} dB");
    }
}

// How far a sound lies from another, summed over their samples.
fn distance(sound: &[i16], other: &[i16]) -> f64 {
    sound.iter().zip(other).map(|(&one, &other)| (f64::from(one) - f64::from(other)).powi(2)).sum()
}

#[test]
fn lost_packets_made_up() {
    let packets = read_packets(COPIES);
    let is_lost = |number: usize| number % 7 == 5 && number + 1 < packets.len();
    let run = |by_copy: Option<bool>| {
        let (mut decoder, mut sound, mut out) = (Decoder::new(), Vec::new(), [0i16; 960]);
        for (number, packet) in packets.iter().enumerate() {
            let count = match by_copy.filter(|_| is_lost(number)) {
                Some(true) => decoder.recover(packets[number + 1], 960, &mut out),
                Some(false) => decoder.conceal(960, &mut out),
                None => decoder.decode(packet, &mut out),
            };
            assert_eq!(count, Ok(960), "packet {number}: 20 ms");
            sound.extend_from_slice(&out);
        }
        sound
    };
    let (whole, copied, concealed) = (run(None), run(Some(true)), run(Some(false)));
    assert!(whole.iter().any(|sample| sample.unsigned_abs() > 5000), "a voice");
    let (near, far) = (distance(&copied, &whole), distance(&concealed, &whole));
    assert!(near > 0.0 && near < far / 2.0, "a copy lies nearer the sound than a concealment: {near} for {far}");
    let (mut decoder, mut out) = (Decoder::new(), [7i16; 2880]);
    assert_eq!(decoder.conceal(2880, &mut out), Ok(2880));
    assert_eq!(out, [0; 2880], "nothing decoded, nothing to conceal");
    assert_eq!(decoder.recover(packets[1], 480, &mut out), Ok(480), "a copy of more than was lost conceals");
    assert_eq!(decoder.decode(packets[0], &mut out), Ok(960));
    assert_eq!(decoder.conceal(100, &mut out), Err(Flaw::Invalid("a time to conceal that is no count of 2.5 ms")));
    assert_eq!(decoder.decode(packets[1], &mut out[..959]), Err(Flaw::Short), "a packet needs room for its samples");
    assert_eq!(decoder.decode(&[], &mut out), Err(Flaw::Short), "no packet at all is for the caller to conceal");
    assert_eq!((Decoder::samples_of(packets[0]), Decoder::samples_of(&[0xFF, 0x03])), (Ok(960), Ok(2880)));
}

// Fixtures of SILK with copies of 40 ms in two channels, and of 60 ms with none asked for.
const STEREO_COPIES: &[u8] = include_bytes!("../../../assets/sound/opus/silk_wb_40ms_32k_stereo_fec.packets");
const NO_COPIES: &[u8] = include_bytes!("../../../assets/sound/opus/silk_wb_60ms_16k_mono.packets");

// libwebrtc's reading of the same flags (opus_interface.cc, `WebRtcOpus_PacketHasFec`): the first frame's first bits, a channel's VAD flags then its LBRR flag, uniform symbols of the range coder that stand as they are in the first byte.
fn has_copy_by_bits(packet: &[u8]) -> bool {
    const SILK_FRAME: usize = 960;
    const FIRST_BIT: u8 = 0x80;
    let Ok(read) = Packet::read(packet) else {
        return false;
    };
    let first = read.frames()[0];
    let silk_frames = read.toc.frame.div_ceil(SILK_FRAME);
    if read.toc.mode == Mode::Celt || first.is_empty() {
        return false;
    }
    let channels = 1 + usize::from(read.toc.is_stereo);
    (1..=channels).any(|channel| first[0] & (FIRST_BIT >> (channel * (silk_frames + 1) - 1)) != 0)
}

#[test]
fn copy_flags() {
    // Of 76, 38 and 26 packets, those that hold a copy, and the samples a copy covers.
    let files = [
        ("SILK, 20 ms", COPIES, 67, Some(960)),
        ("SILK, 40 ms in two channels", STEREO_COPIES, 34, Some(1920)),
        ("SILK, no copies", NO_COPIES, 0, None),
    ];
    for (name, file, count, span) in files {
        let packets = read_packets(file);
        let copied = packets.iter().filter(|packet| Decoder::copy_of(packet).is_some()).count();
        assert_eq!(copied, count, "{name}: the packets that hold a copy");
        for packet in &packets {
            let copy = Decoder::copy_of(packet);
            assert_eq!(copy.is_some(), has_copy_by_bits(packet), "{name}: the flag as libwebrtc reads it");
            assert!(copy.is_none() || copy == span, "{name}: a copy covers the frame before the packet");
        }
    }
    for (name, file) in [("05, hybrid", HYBRID), ("10, CELT and hybrid by turns", TURNS)] {
        for (packet, _) in read_demo(file) {
            assert_eq!(
                Decoder::copy_of(packet).is_some(),
                has_copy_by_bits(packet),
                "vector {name}: as libwebrtc reads it"
            );
        }
    }
    assert_eq!(Decoder::copy_of(&[read_packets(COPIES)[1][0]]), None, "a packet of its first byte alone holds none");
}

#[test]
fn hostile_packets_decode() {
    let mut real: Vec<&[u8]> = read_packets(COPIES);
    for (_, file) in VECTORS {
        real.extend(read_demo(file).into_iter().step_by(23).map(|(packet, _)| packet));
    }
    let mut state = 0x0BAD_5EED_u32;
    let mut next = move |below: u32| {
        state = xorshift32(state);
        state % below
    };
    let (mut decoder, mut out) = (Decoder::new(), [0i16; 5760]);
    for case in 0..6000 {
        let mut packet: Vec<u8> = match next(3) {
            0 => (0..next(400)).map(|_| next(256) as u8).collect(),
            _ => real[next(real.len() as u32) as usize].to_vec(),
        };
        for _ in 0..next(6) {
            if !packet.is_empty() {
                let at = next(packet.len() as u32) as usize;
                packet[at] ^= 1 << next(8);
            }
        }
        let lost = 120 * (1 + next(48)) as usize;
        let count = match next(8) {
            0 => decoder.conceal(lost, &mut out),
            1 => decoder.recover(&packet, lost, &mut out),
            _ => decoder.decode(&packet, &mut out),
        };
        if let Ok(count) = count {
            assert!(count <= out.len() && count.is_multiple_of(120), "case {case}: {count} samples");
        }
    }
}

// A frame of 20 ms, and the CELT layer's overlap, which its sound lies behind its input by before the decoder's own delay.
const FRAME: usize = 960;
const OVERLAP: usize = 120;

// Two seconds of what stands for a voice, by formula: syllables of harmonics of a pitch that glides, bursts of hiss, a click and pauses.
fn voice() -> Vec<i16> {
    let mut state = 0x7E57_u32;
    (0..2 * RATE as usize)
        .map(|at| {
            let time = at as f64 / f64::from(RATE);
            let syllable = (time * 4.0).fract();
            let pitch = 120.0 + 40.0 * sine_cosine(time * 0.7).0;
            let envelope = if syllable < 0.6 { sine_cosine(syllable / 1.2).0 } else { 0.0 };
            let voiced: f64 = (1..=30).map(|k| sine_cosine(pitch * f64::from(k) * time).0 / f64::from(k)).sum();
            state = xorshift32(state);
            let hiss = f64::from(state as i32) / f64::from(i32::MAX);
            let fricative = if (0.65..0.8).contains(&syllable) { 3000.0 * hiss } else { 0.0 };
            let click = if at % 24_000 == 12_000 { 25_000.0 } else { 0.0 };
            6000.0 * envelope * voiced + fricative + click + 20.0 * hiss
        })
        .map(|value| value.clamp(-32_768.0, 32_767.0) as i16)
        .collect()
}

// Encodes `pcm` in packets of `frames` frames and `len` bytes, each decoded at once: the packets and the sound decoded, every packet decoding to its encoder's final range.
fn round_trip(bandwidth: Bandwidth, pcm: &[i16], frames: usize, len: usize) -> (Vec<Vec<u8>>, Vec<i16>) {
    let (mut encoder, mut decoder) = (Encoder::new(bandwidth), Decoder::new());
    let (mut packets, mut sound, mut out) = (Vec::new(), Vec::new(), [0i16; 6 * FRAME]);
    for (number, samples) in pcm.chunks_exact(frames * FRAME).enumerate() {
        let mut packet = vec![0u8; len];
        assert_eq!(encoder.encode(samples, &mut packet), Ok(len), "packet {number}: exactly the length asked for");
        let count = decoder.decode(&packet, &mut out).expect("a packet that parses");
        assert_eq!(count, samples.len(), "packet {number}: as many samples as went in");
        assert_eq!(
            decoder.final_range(),
            encoder.final_range(),
            "packet {number}: the decoder ends in the encoder's range"
        );
        sound.extend_from_slice(&out[..count]);
        packets.push(packet);
    }
    (packets, sound)
}

// The decibels of a sound above its difference from what was decoded of it, the decoded sound late by the overlap and the decoder's delay.
fn ratio(original: &[i16], decoded: &[i16]) -> f64 {
    let late = OVERLAP + Decoder::new().late as usize;
    let pairs = original.iter().zip(&decoded[late..]);
    let (signal, noise) = pairs.fold((0.0, 0.0), |(signal, noise), (&one, &other)| {
        (signal + f64::from(one).powi(2), noise + (f64::from(one) - f64::from(other)).powi(2))
    });
    10.0 * (signal / noise.max(1.0)).log10()
}

#[test]
fn encoded_voice_decodes() {
    let pcm = voice();
    let (packets, sound) = round_trip(Bandwidth::Wide, &pcm, 3, 240);
    assert_eq!(packets.len(), 33);
    assert!(packets.iter().all(|packet| packet[0] >> 3 == 23 && packet[0] & 3 == 3), "CELT, wideband, 20 ms, code 3");
    let bytes: Vec<u8> = packets.concat();
    assert_eq!(crc32(&bytes), 0x4D67_A1BD, "the same bytes on every machine");
    // Measured at 12.82 dB; the bound lies 1 dB under.
    let measured = ratio(&pcm, &sound);
    assert!(measured > 11.8, "the voice through the encoder and back: {measured} dB");
}

#[test]
fn encoding_takes_no_memory() {
    let (mut encoder, mut packet) = (Encoder::new(Bandwidth::Wide), [0u8; 240]);
    for (number, samples) in voice().as_chunks::<{ 3 * FRAME }>().0.iter().enumerate() {
        let before = allocations();
        encoder.encode(samples, &mut packet).expect("a packet that fits");
        let taken = allocations() - before;
        assert!(
            number == 0 || taken == 0,
            "packet {number}: coded in the room the first one made, {taken} allocations"
        );
    }
}

#[test]
fn every_frame_ends_in_its_range() {
    let pcm = voice();
    let mut ratios = Vec::new();
    for (bandwidth, config) in [
        (Bandwidth::Narrow, 19),
        (Bandwidth::Medium, 23),
        (Bandwidth::Wide, 23),
        (Bandwidth::SuperWide, 27),
        (Bandwidth::Full, 31),
    ] {
        let (packets, sound) = round_trip(bandwidth, &pcm, 1, 80);
        assert!(packets.iter().all(|packet| packet[0] == config << 3), "{bandwidth:?}: CELT, 20 ms, code 0");
        ratios.push(ratio(&pcm, &sound));
    }
    // Measured at 12.51, 12.82, 12.82, 12.95 and 11.63 dB; the bounds lie 1 dB under.
    let bounds = [11.5, 11.8, 11.8, 11.9, 10.6];
    assert!(ratios.iter().zip(bounds).all(|(&ratio, bound)| ratio > bound), "{ratios:?} dB, a band each");
}

#[test]
fn packets_of_each_length() {
    let pcm = &voice()[..6 * FRAME * 2];
    for frames in 1..=6 {
        for len in [2 * frames + 2, 3 * frames + 2, 40, 80, 160, 240, 241, 500, 1276, 1277, 4000] {
            round_trip(Bandwidth::Full, pcm, frames, len);
        }
    }
    let mut encoder = Encoder::new(Bandwidth::Wide);
    let mut packet = [0u8; 240];
    assert_eq!(
        encoder.encode(&pcm[..961], &mut packet),
        Err(Flaw::Invalid("samples that are no count of 20 ms frames"))
    );
    assert_eq!(encoder.encode(&pcm[..0], &mut packet), Err(Flaw::Invalid("a count of frames")));
    assert_eq!(encoder.encode(&[0; 7 * FRAME], &mut packet), Err(Flaw::Invalid("a count of frames")), "140 ms");
    assert_eq!(encoder.encode(&pcm[..3 * FRAME], &mut packet[..7]), Err(Flaw::Short), "frames of less than two bytes");
}

#[test]
fn extremes_encode() {
    let mut state = 0xF00D_u32;
    let noise: Vec<i16> = (0..6 * FRAME)
        .map(|_| {
            state = xorshift32(state);
            state as i16
        })
        .collect();
    let square: Vec<i16> = (0..6 * FRAME).map(|at| if at % 110 < 55 { i16::MAX } else { i16::MIN }).collect();
    let nyquist: Vec<i16> = (0..6 * FRAME).map(|at| if at % 2 == 0 { i16::MAX } else { i16::MIN }).collect();
    let impulses: Vec<i16> = (0..6 * FRAME).map(|at| if at % 997 == 0 { i16::MIN } else { 0 }).collect();
    let signals: [(&str, Vec<i16>); 6] = [
        ("silence", vec![0; 6 * FRAME]),
        ("full-scale noise", noise),
        ("a square wave at the clipping level", square),
        ("a constant level", vec![i16::MAX; 6 * FRAME]),
        ("the highest frequency at full scale", nyquist),
        ("impulses", impulses),
    ];
    for (name, pcm) in &signals {
        for bandwidth in [Bandwidth::Narrow, Bandwidth::Full] {
            for (frames, len) in [(3, 11), (3, 240), (1, 1276), (6, 7000)] {
                let (_, sound) = round_trip(bandwidth, pcm, frames, len);
                assert_eq!(sound.len(), pcm.len(), "{name}: every sample");
            }
        }
    }
    let (_, sound) = round_trip(Bandwidth::Full, &signals[0].1, 3, 240);
    assert!(sound.iter().all(|&sample| sample == 0), "silence stays silent");
}

#[test]
fn random_input_encodes() {
    let mut state = 0x0DDB_A115_u32;
    let mut next = move |below: u32| {
        state = xorshift32(state);
        state % below
    };
    let (mut encoder, mut decoder, mut out) = (Encoder::new(Bandwidth::Full), Decoder::new(), [0i16; 6 * FRAME]);
    for case in 0..300 {
        let ragged = if next(4) == 0 { next(FRAME as u32) as usize } else { 0 };
        let count = next(8) as usize * FRAME + ragged;
        let scale = [0, 1, 300, i32::from(i16::MAX)][next(4) as usize];
        let pcm: Vec<i16> =
            (0..count).map(|_| ((next(1 << 16) as i32 - (1 << 15)) * scale / i32::from(i16::MAX)) as i16).collect();
        let mut packet = vec![0u8; next(1500) as usize];
        if let Ok(len) = encoder.encode(&pcm, &mut packet) {
            assert_eq!(len, packet.len(), "case {case}: the packet filled");
            assert_eq!(decoder.decode(&packet, &mut out), Ok(count), "case {case}: decodes to as many samples");
            assert_eq!(decoder.final_range(), encoder.final_range(), "case {case}: in the encoder's range");
        }
    }
}

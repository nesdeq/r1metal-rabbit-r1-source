use super::super::packet::{Bandwidth, Mode, Packet};
use super::super::range::RangeDecoder;
use super::super::support::{read_demo, read_packets, read_samples};
use super::*;
use crate::base::checksum::crc32;
use crate::base::math::xorshift32;
use alloc::vec;
use alloc::vec::Vec;

// Bits a SILK-only frame has left after its SILK data where it carries a redundant CELT frame (4.5.1.1).
const REDUNDANCY_BITS: u32 = 17;
// libopus decoding at an API rate equal to the internal rate passes SILK's samples through its resampler's copy, 4, 9 or 12 samples late (silk/resampler.c, delay_matrix_dec).
const REFERENCE_DELAY: [usize; 3] = [4, 9, 12];

// Prefixes of the official vectors of RFC 8251 that are SILK alone, each cut after a whole packet before 64 KB, and the whole of vector 12, of SILK at each bandwidth and hybrid frames.
const VECTOR_02: &[u8] = include_bytes!("../../../../assets/sound/opus/testvector02_cut.bit");
const VECTOR_03: &[u8] = include_bytes!("../../../../assets/sound/opus/testvector03_cut.bit");
const VECTOR_04: &[u8] = include_bytes!("../../../../assets/sound/opus/testvector04_cut.bit");
const VECTOR_12: &[u8] = include_bytes!("../../../../assets/sound/opus/testvector12.bit");

// libopus as FFmpeg runs it, of a synthetic voice, mono: packets each behind a 2-byte length, and what libopus decodes of them at the internal rate, 16-bit samples.
const NB_20: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_nb_20ms_8k_mono.packets");
const NB_20_PCM: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_nb_20ms_8k_mono.pcm");
const MB_40: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_mb_40ms_12k_mono.packets");
const MB_40_PCM: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_mb_40ms_12k_mono.pcm");
const WB_60: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_60ms_16k_mono.packets");
const WB_60_PCM: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_60ms_16k_mono.pcm");
const WB_10: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_10ms_20k_mono.packets");
const WB_10_PCM: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_10ms_20k_mono.pcm");
// The same with in-band FEC, and what libopus decodes of it with packets lost: in place of each, the LBRR frames of the next, or a concealment.
const FEC: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_20ms_24k_fec.packets");
const FEC_LBRR_PCM: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_20ms_24k_fec_lbrr.pcm");
const FEC_PLC_PCM: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_20ms_24k_fec_plc.pcm");
const FEC_LBRR_LOST: [usize; 9] = [5, 12, 19, 26, 33, 46, 53, 60, 67];
const FEC_PLC_LOST: [usize; 10] = [5, 12, 19, 26, 33, 40, 47, 54, 61, 68];
// Stereo in 40 ms frames with in-band FEC, decoded for one channel with packets lost to the next one's LBRR frames.
const STEREO: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_40ms_32k_stereo_fec.packets");
const STEREO_LBRR_PCM: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_40ms_32k_stereo_fec_lbrr.pcm");
const STEREO_LOST: [usize; 7] = [4, 9, 14, 19, 24, 29, 34];
// The final range libopus reports after decoding each lost packet from the next one's LBRR frames.
const FEC_LBRR_RANGES: [u32; 9] = [
    0x11bb_b430,
    0x0eda_57ee,
    0x41d8_0050,
    0x2635_ed38,
    0x1613_f437,
    0x06d2_1e5a,
    0x5e80_3000,
    0x00c8_72bb,
    0x30cc_7400,
];
const STEREO_LBRR_RANGES: [u32; 7] =
    [0x16c1_b860, 0x0908_a510, 0x2eea_52b2, 0x0083_afc5, 0x0098_f4e0, 0x4bf5_05e0, 0x12d3_eb14];
// libopus's concealment, which 4.4 leaves open, is not the one of RFC 6716's reference decoder this one follows: up to 82 apart where measured, so a bound of twice that.
const CONCEALMENT_GAP: i32 = 164;

#[derive(Default, Debug, PartialEq, Eq)]
struct Count {
    checked: usize,
    mismatched: usize,
    redundant: usize,
    hybrid: usize,
    celt: usize,
}

// Decodes every SILK-only and hybrid packet of a vector as the Opus layer would, resetting after CELT-only ones (4.5.2), and holds each SILK-only packet without redundancy to its final range.
fn run_vector(bytes: &[u8]) -> (Count, Vec<i16>) {
    let mut decoder = Decoder::new();
    let mut count = Count::default();
    let mut out = vec![0i16; MOST_FRAMES * MOST_FRAME];
    let mut sound = Vec::new();
    let mut last_mode = None;
    for (bytes, range) in read_demo(bytes) {
        let packet = Packet::read(bytes).expect("a packet that reads");
        let toc = packet.toc;
        if toc.mode == Mode::Celt {
            count.celt += 1;
            last_mode = Some(Mode::Celt);
            continue;
        }
        if last_mode == Some(Mode::Celt) {
            decoder.reset();
        }
        last_mode = Some(toc.mode);
        let bandwidth = if toc.mode == Mode::Hybrid { Bandwidth::Wide } else { toc.bandwidth };
        let mut last = (0, false);
        for frame in packet.frames() {
            let mut decoding = RangeDecoder::new(frame);
            let written = decoder.decode(&mut decoding, bandwidth, toc.frame, toc.is_stereo, &mut out).unwrap();
            sound.extend_from_slice(&out[..written]);
            last = (decoding.range(), decoding.tell() + REDUNDANCY_BITS <= u8::BITS * frame.len() as u32);
        }
        match (toc.mode, last) {
            (Mode::Hybrid, _) => count.hybrid += 1,
            (_, (_, true)) => count.redundant += 1,
            (_, (found, false)) => {
                count.checked += 1;
                count.mismatched += usize::from(found != range);
            }
        }
    }
    (count, sound)
}

fn checksum(sound: &[i16]) -> u32 {
    crc32(&sound.iter().flat_map(|sample| sample.to_le_bytes()).collect::<Vec<u8>>())
}

#[test]
fn official_vectors_exact() {
    for (name, bytes, expected, samples, sum) in [
        ("02", VECTOR_02, Count { checked: 793, ..Count::default() }, 161_040, 0x3408_ab7c),
        ("03", VECTOR_03, Count { checked: 730, ..Count::default() }, 218_760, 0x55de_b25c),
        ("04", VECTOR_04, Count { checked: 774, ..Count::default() }, 317_920, 0x4203_7bed),
        (
            "12",
            VECTOR_12,
            Count { checked: 1056, redundant: 12, hybrid: 264, ..Count::default() },
            352_160,
            0xc841_42aa,
        ),
    ] {
        let (count, sound) = run_vector(bytes);
        assert_eq!(count, expected, "vector {name}: every SILK-only packet ends in its encoder's final range");
        assert_eq!((sound.len(), checksum(&sound)), (samples, sum), "vector {name}: the sound as it was decoded");
    }
    let configurations: Vec<(usize, bool)> = [VECTOR_02, VECTOR_03, VECTOR_04]
        .iter()
        .flat_map(|bytes| read_demo(bytes))
        .map(|(bytes, _)| Packet::read(bytes).unwrap().toc)
        .map(|toc| (toc.frame, toc.is_stereo))
        .collect();
    for frame in SILK_FRAMES {
        assert!(configurations.contains(&(frame, false)), "the prefixes hold mono frames of {frame} samples");
    }
    assert!(configurations.contains(&(SILK_FRAMES[1], true)) && configurations.contains(&(SILK_FRAMES[3], true)));
}

// How a lost packet is decoded: concealed, or from the next packet's LBRR frames.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Loss {
    Conceal,
    Lbrr,
}

// Decodes a fixture's packets, those numbered in `lost` as lost, into the sound and the final range of each.
fn run_packets(packets: &[&[u8]], lost: &[usize], loss: Loss) -> (Vec<i16>, Vec<u32>) {
    let mut decoder = Decoder::new();
    let mut out = vec![0i16; MOST_FRAMES * MOST_FRAME];
    let (mut sound, mut ranges) = (Vec::new(), Vec::new());
    for (number, bytes) in packets.iter().enumerate() {
        let packet = Packet::read(bytes).unwrap();
        let next = packets.get(number + 1).map(|bytes| Packet::read(bytes).unwrap());
        let (written, range) = match next {
            _ if !lost.contains(&number) => {
                let mut range = RangeDecoder::new(packet.frames()[0]);
                let toc = packet.toc;
                (decoder.decode(&mut range, toc.bandwidth, toc.frame, toc.is_stereo, &mut out).unwrap(), range.range())
            }
            Some(next) if loss == Loss::Lbrr => {
                let mut range = RangeDecoder::new(next.frames()[0]);
                let toc = next.toc;
                let written = decoder.decode_lbrr(&mut range, toc.bandwidth, toc.frame, toc.is_stereo, &mut out);
                (written.unwrap(), range.range())
            }
            _ => (decoder.conceal(packet.toc.frame, &mut out).unwrap(), 0),
        };
        sound.extend_from_slice(&out[..written]);
        ranges.push(range);
    }
    (sound, ranges)
}

// Holds the sound to libopus's at the internal rate of that bandwidth, which comes the copy's delay later.
fn assert_matches(name: &str, sound: &[i16], reference: &[u8], band: usize) {
    let reference = read_samples(reference);
    let delay = REFERENCE_DELAY[band];
    assert_eq!(sound.len(), reference.len(), "{name}: as many samples as libopus gives");
    assert_eq!(sound[..sound.len() - delay], reference[delay..], "{name}: libopus's samples");
}

#[test]
fn fixtures_match_libopus() {
    for (name, packets, reference, band) in [
        ("NB in 20 ms", NB_20, NB_20_PCM, 0),
        ("MB in 40 ms", MB_40, MB_40_PCM, 1),
        ("WB in 60 ms", WB_60, WB_60_PCM, 2),
        ("WB in 10 ms", WB_10, WB_10_PCM, 2),
    ] {
        let packets = read_packets(packets);
        assert!(packets.iter().all(|packet| Packet::read(packet).unwrap().toc.mode == Mode::Silk));
        assert_matches(name, &run_packets(&packets, &[], Loss::Conceal).0, reference, band);
    }
}

// Whether a packet's mid channel has LBRR frames: its flag follows its VAD flags, the first bits of the frame (4.2.3).
fn has_lbrr(packet: &[u8]) -> bool {
    let toc = Packet::read(packet).unwrap().toc;
    let (frames, _) = LAYOUTS[SILK_FRAMES.iter().position(|&frame| frame == toc.frame).unwrap()];
    packet[1] >> (u8::BITS as usize - 1 - frames) & 1 != 0
}

#[test]
fn lbrr_decoded_for_lost_packets() {
    for (name, packets, reference, lost, ranges) in [
        ("mono", FEC, FEC_LBRR_PCM, &FEC_LBRR_LOST[..], &FEC_LBRR_RANGES[..]),
        ("stereo", STEREO, STEREO_LBRR_PCM, &STEREO_LOST, &STEREO_LBRR_RANGES),
    ] {
        let packets = read_packets(packets);
        for &number in lost {
            assert!(has_lbrr(packets[number + 1]), "{name}: packet {} holds LBRR frames", number + 1);
        }
        let (sound, found) = run_packets(&packets, lost, Loss::Lbrr);
        assert_matches(name, &sound, reference, 2);
        let found: Vec<u32> = lost.iter().map(|&number| found[number]).collect();
        assert_eq!(found, ranges, "{name}: the LBRR frames read where libopus reads them");
    }
}

#[test]
fn lbrr_missing_conceals() {
    // Packet 41 holds no LBRR frame: its LBRR copy of packet 40 is a concealment.
    let packets = read_packets(FEC);
    assert!(!has_lbrr(packets[41]), "packet 41's LBRR flag is clear");
    let (from_lbrr, _) = run_packets(&packets[..43], &[40], Loss::Lbrr);
    let (concealed, _) = run_packets(&packets[..43], &[40], Loss::Conceal);
    assert_eq!(from_lbrr, concealed, "and the frames after it decode the same");
}

#[test]
fn concealment_near_libopus() {
    let packets = read_packets(FEC);
    let (sound, _) = run_packets(&packets, &FEC_PLC_LOST, Loss::Conceal);
    let reference = read_samples(FEC_PLC_PCM);
    let delay = REFERENCE_DELAY[2];
    let first_lost = FEC_PLC_LOST[0] * sound.len() / packets.len();
    assert_eq!(sound[..first_lost - delay], reference[delay..first_lost], "up to the first loss, libopus's samples");
    let gap =
        sound.iter().zip(&reference[delay..]).map(|(&one, &other)| (i32::from(one) - i32::from(other)).abs()).max();
    assert!(gap <= Some(CONCEALMENT_GAP), "and near them after it: {gap:?} apart");
    let loudest = reference.iter().map(|sample| sample.unsigned_abs()).max();
    assert!(loudest > Some(10_000), "of a loud signal: {loudest:?}");
}

#[test]
fn reset_decodes_as_new() {
    let packets = read_packets(WB_60);
    let (mut used, mut fresh) = (Decoder::new(), Decoder::new());
    let (mut one, mut other) = ([0i16; MOST_FRAMES * MOST_FRAME], [0i16; MOST_FRAMES * MOST_FRAME]);
    let decode = |decoder: &mut Decoder, packet: &[u8], out: &mut [i16]| {
        let packet = Packet::read(packet).unwrap();
        let mut range = RangeDecoder::new(packet.frames()[0]);
        decoder.decode(&mut range, packet.toc.bandwidth, packet.toc.frame, packet.toc.is_stereo, out).unwrap()
    };
    for packet in &packets[..10] {
        decode(&mut used, packet, &mut one);
    }
    used.reset();
    assert_eq!(used.bandwidth(), None, "a reset forgets the internal rate");
    for (number, packet) in packets.iter().enumerate() {
        assert_eq!(decode(&mut used, packet, &mut one), decode(&mut fresh, packet, &mut other));
        assert_eq!(one, other, "packet {number}");
    }
    assert_eq!(used.bandwidth(), Some(Bandwidth::Wide));
}

#[test]
fn parameters_refused() {
    let mut decoder = Decoder::new();
    let mut out = [0i16; MOST_FRAMES * MOST_FRAME];
    assert_eq!(decoder.conceal(SILK_FRAMES[1], &mut out), Ok(0), "nothing to conceal from before the first frame");
    let frame = [0u8; 8];
    let mut range = RangeDecoder::new(&frame);
    let wide = Bandwidth::SuperWide;
    assert_eq!(
        decoder.decode(&mut range, wide, SILK_FRAMES[1], false, &mut out),
        Err(Flaw::Invalid("a bandwidth of SILK"))
    );
    assert_eq!(
        decoder.decode(&mut range, Bandwidth::Wide, 120, false, &mut out),
        Err(Flaw::Invalid("a duration of SILK"))
    );
    assert_eq!(decoder.decode(&mut range, Bandwidth::Wide, SILK_FRAMES[1], false, &mut out[..319]), Err(Flaw::Short));
    assert_eq!(decoder.conceal(240, &mut out), Ok(0));
    assert_eq!(decoder.decode(&mut range, Bandwidth::Narrow, SILK_FRAMES[3], true, &mut out), Ok(480));
    assert_eq!(decoder.conceal(SILK_FRAMES[0], &mut out), Ok(80), "10 ms at the last frame's 8 kHz");
    assert_eq!(decoder.conceal(SILK_FRAMES[0], &mut out[..79]), Err(Flaw::Short));
    assert_eq!(decoder.bandwidth(), Some(Bandwidth::Narrow));
}

#[test]
fn samples_one_late() {
    let mut decoder = Decoder::new();
    let (mut first, mut second) = ([0i16; MOST_FRAME], [0i16; MOST_FRAME]);
    let packets = read_packets(NB_20);
    let packet = Packet::read(packets[30]).unwrap();
    let mut range = RangeDecoder::new(packet.frames()[0]);
    let written = decoder.decode(&mut range, Bandwidth::Narrow, SILK_FRAMES[1], false, &mut first).unwrap();
    // A frame of 20 ms fills the history of the LTP filter, which holds the samples as they were made.
    let made = decoder.mid.history;
    assert_eq!(first[1..written], made[..written - 1], "each sample comes one late");
    decoder.conceal(SILK_FRAMES[1], &mut second).unwrap();
    assert_eq!(second[0], made[written - 1], "and the last of a frame comes first in the next");
    assert_ne!(second[0], 0, "of a frame of sound");
}

#[test]
fn broken_frames_decode() {
    let mut state = 0x5EED_u32;
    let mut next = move |below: u32| {
        state = xorshift32(state);
        state % below
    };
    let mut decoder = Decoder::new();
    let mut out = [0i16; MOST_FRAMES * MOST_FRAME];
    let real: Vec<&[u8]> =
        [NB_20, MB_40, WB_60, WB_10, FEC, STEREO].iter().flat_map(|bytes| read_packets(bytes)).collect();
    let bandwidths = [Bandwidth::Narrow, Bandwidth::Medium, Bandwidth::Wide];
    for case in 0..20_000 {
        let mut frame: Vec<u8> = if case % 2 == 0 {
            (0..next(80)).map(|_| next(256) as u8).collect()
        } else {
            real[next(real.len() as u32) as usize][1..].to_vec()
        };
        for _ in 0..1 + next(8) {
            if !frame.is_empty() {
                let at = next(frame.len() as u32) as usize;
                frame[at] ^= 1 << next(8);
            }
        }
        let bandwidth = bandwidths[next(3) as usize];
        let duration = SILK_FRAMES[next(4) as usize];
        let is_stereo = next(2) == 1;
        let mut range = RangeDecoder::new(&frame);
        let written = match next(8) {
            0 => decoder.decode_lbrr(&mut range, bandwidth, duration, is_stereo, &mut out),
            1 => decoder.conceal(duration, &mut out),
            2 => {
                decoder.reset();
                Ok(0)
            }
            _ => decoder.decode(&mut range, bandwidth, duration, is_stereo, &mut out),
        };
        assert!(written.is_ok(), "case {case}: a frame of any bytes decodes");
    }
}

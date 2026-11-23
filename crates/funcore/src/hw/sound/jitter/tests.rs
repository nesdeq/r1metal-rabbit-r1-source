extern crate std;

use super::*;
use crate::base::alloc_count::allocations;
use crate::base::math::xorshift64;
use crate::codec::opus::support::read_packets;
use crate::hw::sound::card::{self, Direction};
use crate::hw::sound::{PEAK_FLOOR, peak};
use alloc::vec;

// SILK at 16 kHz in frames of 20 ms, each with a copy of the frame before it (4.2.5): a voice with pauses.
const COPIES: &[u8] = include_bytes!("../../../../assets/sound/opus/silk_wb_20ms_24k_fec.packets");
// CELT at 64 kbit/s in frames of 20 ms: sound with no pause.
const CELT: &[u8] = include_bytes!("../../../../assets/sound/opus/celt_20ms_64k_mono.packets");
const BLOCK: usize = card::PERIOD;
// Signal's packets: 60 ms (peer_connection.cc, `ptime`), three frames of 20 ms.
const PACKET: usize = 2880;
const FRAMES: usize = 3;
// RFC 6716, 3.2.5: a packet of code 3 whose frames have sizes of their own; and 3.2.1, the least size that takes two bytes.
const CODE_THREE: u8 = 3;
const VBR: u8 = 0x80;
const TWO_BYTE_SIZE: usize = 252;
// In silence WebRTC sends a packet every 400 ms (opus_interface.cc, `WebRtcOpus_Encode`; the calling server's `handle_audio_level`): every seventh slot of 60 ms.
const SILENT_SLOTS: u64 = 7;
// The network's own delay in the simulation.
const BASE: f64 = 0.03;
// The least a seek moves the follower's place by: 60 ms (pace.rs, `SEEK_SECONDS`).
const SEEK: i64 = 2880;

// The far side's packets: a fixture's frames of 20 ms three at a time, as one packet of 60 ms.
fn packets(fixture: &[u8]) -> Vec<Vec<u8>> {
    read_packets(fixture)
        .as_chunks::<FRAMES>()
        .0
        .iter()
        .map(|three| {
            let mut packet = vec![three[0][0] | CODE_THREE, VBR | FRAMES as u8];
            for frame in &three[..FRAMES - 1] {
                let size = frame.len() - 1;
                if size < TWO_BYTE_SIZE {
                    packet.push(size as u8);
                } else {
                    let first = TWO_BYTE_SIZE + size % 4;
                    packet.extend([first as u8, ((size - first) / 4) as u8]);
                }
            }
            three.iter().for_each(|frame| packet.extend_from_slice(&frame[1..]));
            packet
        })
        .collect()
}

#[derive(Clone)]
struct Sent {
    sequence: u64,
    timestamp: u32,
    sent: f64,
    payload: Vec<u8>,
}

// What the far side sends for `seconds`: packets of 60 ms round and round a fixture, talking throughout or by turns of talk and silence, its clock `drift` faster than ours; in silence the packet of its first byte alone that WebRTC sends.
fn far_side(fixture: &[u8], seconds: f64, turns: Option<(f64, f64)>, drift: f64) -> Vec<Sent> {
    let sound = packets(fixture);
    let silent = vec![sound[0][0] & !CODE_THREE];
    let (mut sent, mut silent_slots, mut said) = (Vec::new(), 0, 0);
    let first_timestamp = u32::MAX - 10 * PACKET as u32;
    for slot in 0u64.. {
        let at = slot as f64 * PACKET as f64 / f64::from(RATE) / (1.0 + drift);
        if at >= seconds {
            break;
        }
        let is_talking = turns.is_none_or(|(talk, pause)| at % (talk + pause) < talk);
        silent_slots = if is_talking { 0 } else { silent_slots + 1 };
        let payload = if is_talking {
            said += 1;
            sound[(said - 1) % sound.len()].clone()
        } else if (silent_slots - 1) % SILENT_SLOTS <= 1 {
            silent.clone()
        } else {
            continue;
        };
        let timestamp = first_timestamp.wrapping_add((slot * PACKET as u64) as u32);
        sent.push(Sent { sequence: (1 << 20) + sent.len() as u64, timestamp, sent: at, payload });
    }
    sent
}

#[derive(Clone, Copy)]
enum Path {
    Steady,
    // A delay up to that many seconds more, any as likely.
    Uniform(f64),
    // Every `every` s from `from` s on, the network holds what is sent for `hold` s and lets it go at once.
    Bursts { from: f64, every: f64, hold: f64 },
    // `extra` s more from `from` s to `until` s.
    Route { from: f64, until: f64, extra: f64 },
}

#[derive(Clone, Copy)]
enum Loss {
    None,
    // That share of packets lost, in runs of that many.
    Runs(f64, usize),
}

#[derive(Clone, Copy)]
struct Network {
    path: Path,
    loss: Loss,
    copies: f64,
    seed: u64,
}

const CALM: Network = Network { path: Path::Steady, loss: Loss::None, copies: 0.0, seed: 1 };

const fn path(path: Path) -> Network {
    Network { path, ..CALM }
}

struct Random(u64);

impl Random {
    fn share(&mut self) -> f64 {
        self.0 = xorshift64(self.0);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, count: usize) -> usize {
        (self.share() * count as f64) as usize
    }
}

// When each packet comes, as the network delays, loses and copies them, in the order they come; and how many were lost.
fn arrivals(sent: &[Sent], network: Network) -> (Vec<(f64, usize)>, usize) {
    let mut random = Random(network.seed);
    let (mut come, mut lost, mut left) = (Vec::new(), 0, 0);
    for (index, packet) in sent.iter().enumerate() {
        if let Loss::Runs(share, length) = network.loss
            && left == 0
            && random.share() < share / length as f64
        {
            left = length;
        }
        if left > 0 {
            (left, lost) = (left - 1, lost + 1);
            continue;
        }
        let delay = BASE
            + match network.path {
                Path::Steady => 0.0,
                Path::Uniform(most) => most * random.share(),
                Path::Bursts { from, every, hold } => {
                    let into = (packet.sent - from).rem_euclid(every);
                    if packet.sent >= from && into < hold { hold - into } else { 0.0 }
                }
                Path::Route { from, until, extra } => {
                    if (from..until).contains(&packet.sent) {
                        extra
                    } else {
                        0.0
                    }
                }
            };
        come.push((packet.sent + delay, index));
        if random.share() < network.copies {
            come.push((packet.sent + delay + BASE * random.share(), index));
        }
    }
    come.sort_by(|one, other| one.0.total_cmp(&other.0));
    (come, lost)
}

// When the speaker's thread asks for its `pull`-th block, and the card's delay then: the ring of four blocks filled at once, then a block as each one plays, the card's clock `drift` faster than ours.
fn pull_at(pull: usize, drift: f64) -> (f64, usize) {
    let played = ((pull + 1) * BLOCK).saturating_sub(card::buffer_samples(Direction::Out));
    (played as f64 / f64::from(RATE) / (1.0 + drift), pull * BLOCK - played)
}

#[derive(Default)]
struct Outcome {
    stats: Stats,
    lost: usize,
    // The delay at each second, in milliseconds.
    delays: Vec<f64>,
    // The most the reader lay off the plan at a block's end, in samples, and the seeks after the first.
    worst_offset: f64,
    seeks: usize,
    output: Vec<i16>,
}

fn play(sent: &[Sent], come: &[(f64, usize)], seconds: f64, card_drift: f64, is_kept: bool) -> Outcome {
    let (mut stream, mut out, mut next, mut outcome) = (Stream::default(), [0i16; BLOCK], 0, Outcome::default());
    for pull in 0.. {
        let (time, delay) = pull_at(pull, card_drift);
        if time >= seconds {
            break;
        }
        while let Some(&(at, index)) = come.get(next)
            && at <= time
        {
            let packet = &sent[index];
            stream.push(packet.sequence, packet.timestamp, Duration::from_secs_f64(at), &packet.payload);
            next += 1;
        }
        let (is_playing, given) = (stream.plan.is_some(), stream.reader.given());
        stream.pull(Duration::from_secs_f64(time), delay, &mut out);
        if let Some(plan) = stream.plan
            && is_playing
        {
            let read = stream.reader.given() as i64 - given as i64;
            outcome.seeks += usize::from((read - BLOCK as i64).abs() > SEEK);
            let end = place_of(Duration::from_secs_f64(time)) + (delay + BLOCK) as f64;
            outcome.worst_offset = outcome.worst_offset.max((end - plan - stream.reader.place() as f64).abs());
        }
        if time >= outcome.delays.len() as f64 {
            outcome.delays.push(stream.stats().delay.as_secs_f64() * 1000.0);
        }
        if is_kept {
            outcome.output.extend_from_slice(&out);
        }
    }
    outcome.stats = stream.stats();
    outcome
}

fn run(fixture: &[u8], seconds: f64, turns: Option<(f64, f64)>, network: Network, drifts: (f64, f64)) -> Outcome {
    let sent = far_side(fixture, seconds, turns, drifts.0);
    let (come, lost) = arrivals(&sent, network);
    Outcome { lost, ..play(&sent, &come, seconds, drifts.1, false) }
}

// The delay of the calm before `from` s, and the seconds after `until` s until it lies within `slack` ms of that for good.
fn recovery(delays: &[f64], from: usize, until: usize, slack: f64) -> (f64, usize) {
    let floor = delays[from / 2..from].iter().copied().fold(f64::MAX, f64::min);
    let last_off = delays[until..].iter().rposition(|&delay| delay > floor + slack);
    (floor, last_off.map_or(0, |last| last + 1))
}

fn median(delays: &[f64]) -> f64 {
    let mut sorted = delays.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

fn milliseconds(samples: u64) -> f64 {
    samples as f64 * 1000.0 / f64::from(RATE)
}

// No seek and no step off the plan as far as one, nothing played late: what every network here leaves.
fn assert_smooth(name: &str, outcome: &Outcome) {
    assert_eq!(outcome.seeks, 0, "{name}: no seek");
    assert!(outcome.worst_offset < SEEK as f64, "{name}: off the plan by {} samples at the most", outcome.worst_offset);
    assert_eq!(outcome.stats.late, 0, "{name}: every packet that came was played");
}

// The delay with no jitter: one bucket of spread and the lag of the fastest pace, 28.2 ms measured.
const FLOOR: f64 = 28.2;
const SLACK: f64 = 1.0;

#[test]
fn calm_network_plays_decoder_sound() {
    let seconds = 20.0;
    let sent = far_side(COPIES, seconds, None, 0.0);
    let outcome = play(&sent, &arrivals(&sent, CALM).0, seconds, 0.0, true);
    let (mut decoder, mut decoded, mut frame) = (Decoder::new(), Vec::new(), [0i16; PACKET]);
    for packet in &sent {
        assert_eq!(decoder.decode(&packet.payload, &mut frame), Ok(PACKET), "a packet of 60 ms");
        decoded.extend_from_slice(&frame);
    }
    let first = |sound: &[i16]| sound.iter().position(|&sample| sample != 0).unwrap();
    let late = first(&outcome.output) - first(&decoded);
    let heard = &outcome.output[late..];
    assert!(heard == &decoded[..heard.len()], "the decoder's sound sample for sample");
    // 30 ms of the network, the card's lead of 4096, the plan's 1 bucket and lag of 384 with the lead's 0.2 % and 1, and the curve's 2.
    assert_eq!(late, 6891, "late by the network, the card's lead, the plan's delay and the curve's reach");
    assert_eq!(outcome.stats, Stats { delay: outcome.stats.delay, ..Stats::default() }, "nothing made up or late");
    assert!((outcome.stats.delay.as_secs_f64() * 1000.0 - FLOOR).abs() < SLACK, "the delay at its floor");
    assert_smooth("calm", &outcome);
}

#[test]
fn jitter_shapes() {
    for most in [0.04, 0.08, 0.15] {
        let outcome = run(COPIES, 120.0, None, path(Path::Uniform(most)), (0.0, 0.0));
        let name = alloc::format!("uniform up to {most} s");
        assert_smooth(&name, &outcome);
        let (delay, spread) = (median(&outcome.delays), most * 1000.0);
        // Above the spread by the lag of the fastest pace, 8 ms, and up to a bucket of 20 ms.
        assert!(delay > spread && delay < spread + FLOOR, "{name}: a delay of {delay} ms");
        assert!(milliseconds(outcome.stats.concealed) <= spread, "{name}: only the first packets waited for");
    }
    let bursts = run(COPIES, 120.0, None, path(Path::Bursts { from: 7.0, every: 7.0, hold: 0.3 }), (0.0, 0.0));
    assert_smooth("bursts", &bursts);
    assert!(median(&bursts.delays) > 270.0, "bursts in 7 % of spans are waited out: {}", median(&bursts.delays));
    assert!(milliseconds(bursts.stats.waited) <= 300.0, "only the first burst found the delay short");
    // One burst does not stay in the delay: the 2 s it is the latest of, then the next pause cutting the excess; 5 s measured.
    let burst = run(COPIES, 80.0, None, path(Path::Bursts { from: 40.0, every: 100.0, hold: 0.3 }), (0.0, 0.0));
    assert_smooth("one burst", &burst);
    let (floor, back) = recovery(&burst.delays, 40, 41, SLACK);
    assert!((floor - FLOOR).abs() < SLACK && back <= 6, "one burst: back at {floor} ms {back} s after it");
    for (name, turns) in [("route, talk", None), ("route, turns", Some((4.0, 2.0)))] {
        let route = run(COPIES, 120.0, turns, path(Path::Route { from: 40.0, until: 70.0, extra: 0.2 }), (0.0, 0.0));
        assert_smooth(name, &route);
        assert!(milliseconds(route.stats.waited) <= 200.0, "{name}: the 200 ms waited for at once");
        let (floor, back) = recovery(&route.delays, 40, 70, SLACK);
        assert!((floor - FLOOR).abs() < SLACK && back <= 6, "{name}: back at {floor} ms {back} s after the 200 ms go");
    }
}

#[test]
fn continuous_sound_accelerated() {
    // Sound with no pause gives no silence to cut: the excess goes by accelerating it, a pitch period at a time where the periods beside each other are alike, as NetEq does, in place of the plan's own pace of 1 ms a second, which would take 200 s for the 200 ms.
    let seconds = 120.0;
    let outcome = run(CELT, seconds, None, path(Path::Route { from: 40.0, until: 70.0, extra: 0.2 }), (0.0, 0.0));
    assert_smooth("route, no pause", &outcome);
    let (floor, back) = recovery(&outcome.delays, 40, 70, SLACK);
    assert!((floor - FLOOR).abs() < SLACK, "the floor of sound without pauses is that of a voice");
    assert!(back <= 20, "200 ms accelerated away: back {back} s after the 200 ms go");
}

#[test]
fn restart_keeps_decoder() {
    let sent = far_side(COPIES, 2.0, None, 0.0);
    let come = arrivals(&sent, CALM).0;
    let mut stream = Stream::default();
    // The packets played through from `from` s on, at numbers and timestamps moved by `moved`: the loudest sample heard.
    let play_from = |stream: &mut Stream, from: f64, moved: u64| {
        let (mut out, mut next, mut loudest) = ([0i16; BLOCK], 0, 0);
        for pull in 0.. {
            let (time, delay) = pull_at(pull, 0.0);
            if next == come.len() && stream.reader.sizes().0 == 0 {
                break;
            }
            while let Some(&(at, index)) = come.get(next)
                && at <= time
            {
                let packet = &sent[index];
                let (sequence, timestamp) = (packet.sequence + moved, packet.timestamp.wrapping_add(moved as u32));
                stream.push(sequence, timestamp, Duration::from_secs_f64(from + at), &packet.payload);
                next += 1;
            }
            stream.pull(Duration::from_secs_f64(from + time), delay, &mut out);
            loudest = loudest.max(peak(&out));
        }
        loudest
    };
    assert!(u32::from(play_from(&mut stream, 0.0, 0)) > PEAK_FLOOR, "the stream plays");
    let before = allocations();
    stream.restart();
    assert_eq!(allocations(), before, "a restart resets the decoder and makes none anew");
    assert_eq!(stream.stats().restarts, 1, "and is counted");
    let loudest = play_from(&mut stream, 10.0, 0x1234_5678);
    assert!(u32::from(loudest) > PEAK_FLOOR, "the stream begun anew plays: a peak of {loudest}");
}

#[test]
fn losses_made_up() {
    for (share, length) in [(0.01, 1), (0.05, 1), (0.2, 1), (0.01, 3), (0.05, 3), (0.2, 3)] {
        let network = Network { loss: Loss::Runs(share, length), seed: 3, ..CALM };
        let outcome = run(COPIES, 120.0, None, network, (0.0, 0.0));
        let name = alloc::format!("{share} lost in runs of {length}");
        assert_smooth(&name, &outcome);
        assert_eq!(outcome.stats.waited, 0, "{name}: a loss with a packet behind it adds no delay");
        assert!((median(&outcome.delays) - FLOOR).abs() < SLACK, "{name}: the delay stays at its floor");
        let (made, lost) = (outcome.stats.concealed, (outcome.lost * PACKET) as u64);
        assert!(made <= lost, "{name}: no more made up than was lost, {made} of {lost} samples");
        if length == 1 {
            assert!(
                outcome.stats.recovered * 10 >= outcome.lost as u64 * 7,
                "{name}: most single losses taken from the copy behind: {} of {}",
                outcome.stats.recovered,
                outcome.lost
            );
        }
    }
}

#[test]
fn copies_and_reordering() {
    let network = Network { path: Path::Uniform(0.08), copies: 0.05, ..CALM };
    let sent = far_side(COPIES, 120.0, None, 0.0);
    let (come, _) = arrivals(&sent, network);
    let reordered = come.windows(2).filter(|pair| sent[pair[1].1].sequence < sent[pair[0].1].sequence).count();
    assert!(reordered > 50, "the jitter puts packets out of order, 69 measured: {reordered}");
    let outcome = play(&sent, &come, 120.0, 0.0, false);
    assert_smooth("copies and reordering", &outcome);
    assert!(milliseconds(outcome.stats.concealed) <= 80.0, "only the first packets waited for");
}

#[test]
fn clocks_drift() {
    // 0.05 % fast and slow for ten minutes, the far side's clock and the card's.
    for drifts in [(0.0005, 0.0), (-0.0005, 0.0), (0.0, 0.0005), (0.0, -0.0005)] {
        let outcome = run(COPIES, 600.0, None, CALM, drifts);
        let name = alloc::format!("drifts {drifts:?}");
        assert_smooth(&name, &outcome);
        assert_eq!(outcome.stats.concealed, 0, "{name}: nothing made up");
        assert!(
            outcome.delays[10..].iter().all(|&delay| (delay - FLOOR).abs() < SLACK),
            "{name}: the delay at its floor throughout"
        );
        assert!(outcome.worst_offset < 2.0 * 96.0, "{name}: the lag of 0.05 %, 96 samples, and no more than twice it");
    }
}

#[test]
fn silence_and_return() {
    let turns = run(COPIES, 120.0, Some((4.0, 2.0)), CALM, (0.0, 0.0));
    assert_smooth("turns", &turns);
    assert_eq!(turns.stats.concealed, 0, "the far side's silence is no loss");
    assert!(turns.delays[2..].iter().all(|&delay| (delay - FLOOR).abs() < SLACK), "nor does it move the delay");
    // A server that drops a silent member's packets: 20 s with none, then talk.
    let mut sent = far_side(COPIES, 60.0, Some((20.0, 20.0)), 0.0);
    sent.retain(|packet| packet.payload.len() > 2);
    let outcome = play(&sent, &arrivals(&sent, CALM).0, 60.0, 0.0, true);
    assert_smooth("silence of 20 s", &outcome);
    assert!(outcome.delays[45..].iter().all(|&delay| (delay - FLOOR).abs() < SLACK), "talk again at the floor");
    let loud = |from: f64| outcome.output[(from * f64::from(RATE)) as usize..][..RATE as usize].iter().any(|&s| s != 0);
    assert!(loud(42.0) && !loud(30.0), "sound again after the silence, none in it");
    // The same of a member whose sound has no pause: the CELT layer makes up noise at its background's level for as long as it is asked.
    let mut sent = far_side(CELT, 60.0, Some((20.0, 20.0)), 0.0);
    sent.retain(|packet| packet.payload.len() > 2);
    let outcome = play(&sent, &arrivals(&sent, CALM).0, 60.0, 0.0, true);
    let loud = |from: f64| outcome.output[(from * f64::from(RATE)) as usize..][..RATE as usize].iter().any(|&s| s != 0);
    assert!(loud(21.0), "what was lost at the silence's beginning is made up");
    assert!(!loud(23.0) && !loud(30.0), "a stream nothing came of for 2 s rests: nothing is made up for it");
    assert!(loud(42.0), "and it sounds again as its packets come");
    assert_smooth("silence of 20 s, no pause", &outcome);
}

#[test]
fn restart_plays_on() {
    // A far side that comes back after 10 s at other timestamps and numbers.
    let mut sent = far_side(COPIES, 60.0, None, 0.0);
    sent.retain(|packet| !(25.0..35.0).contains(&packet.sent));
    for packet in sent.iter_mut().filter(|packet| packet.sent >= 35.0) {
        (packet.timestamp, packet.sequence) = (packet.timestamp.wrapping_add(0x6A5D_39A1), packet.sequence - 900);
    }
    let outcome = play(&sent, &arrivals(&sent, CALM).0, 60.0, 0.0, true);
    assert_eq!(outcome.stats.restarts, 1, "the stream begins anew once");
    assert!(outcome.delays[38..].iter().all(|&delay| (delay - FLOOR).abs() < SLACK), "at its floor again");
    let loud = |from: f64| outcome.output[(from * f64::from(RATE)) as usize..][..RATE as usize].iter().any(|&s| s != 0);
    assert!(loud(36.0) && loud(50.0), "and plays on");
}

#[test]
fn strangers_packets_bounded() {
    // Random packets, numbers, timestamps, times and pulls from a fixed seed: every call returns, and what is held stays bounded.
    let real = packets(COPIES);
    let mut random = Random(0x5EED_0F57);
    let (mut stream, mut out, mut now) = (Stream::default(), vec![0i16; 2 * BLOCK], Duration::ZERO);
    let (mut sequence, mut timestamp) = (0u64, 0u32);
    for case in 0..40_000 {
        match random.below(8) {
            0 => sequence = sequence.wrapping_add(random.below(1 << 20) as u64),
            1 => sequence = random.below(usize::MAX) as u64,
            2 => timestamp = timestamp.wrapping_add(random.below(1 << 31) as u32),
            _ => (sequence, timestamp) = (sequence.wrapping_add(1), timestamp.wrapping_add(PACKET as u32)),
        }
        let mut payload: Vec<u8> = match random.below(3) {
            0 => (0..random.below(400)).map(|_| random.below(256) as u8).collect(),
            _ => real[random.below(real.len())].clone(),
        };
        if !payload.is_empty() && random.below(2) == 0 {
            let at = random.below(payload.len());
            payload[at] ^= 1 << random.below(8);
        }
        now += Duration::from_micros(random.below(80_000) as u64);
        stream.push(sequence, timestamp, now, &payload);
        if random.below(3) == 0 {
            let length = random.below(out.len());
            stream.pull(now, random.below(2 * card::buffer_samples(Direction::Out)), &mut out[..length]);
        }
        let (held, line) = stream.reader.sizes();
        assert!(held <= MOST_PACKETS, "case {case}: {held} packets held");
        // A seek back's zeros, at the most the most spread, and a packet of 120 ms with what was made up behind it.
        assert!(line <= MOST_SPREAD as usize + 4 * PACKET, "case {case}: {line} samples on the line");
    }
    let mut streams = [Stream::default(), Stream::default()];
    mix(&mut streams, now, 0, &mut []);
}

#[test]
fn mix_levels_and_saturates() {
    let seconds = 20.0;
    let voice = packets(COPIES);
    let talkers = |count: usize| {
        let mut streams: Vec<Stream> = (0..count).map(|_| Stream::default()).collect();
        let mut senders: Vec<Vec<Sent>> = (0..count).map(|_| far_side(COPIES, seconds, None, 0.0)).collect();
        // Each talker starts at another place in the voice, so that they speak over one another.
        for (talker, sent) in senders.iter_mut().enumerate() {
            for (index, packet) in sent.iter_mut().enumerate() {
                packet.payload = voice[(index + talker * voice.len() / 4) % voice.len()].clone();
            }
        }
        let comes: Vec<Vec<(f64, usize)>> = senders.iter().map(|sent| arrivals(sent, CALM).0).collect();
        let mut nexts = vec![0; count];
        let (mut out, mut clipped, mut total, mut levels) = (vec![0i16; BLOCK], 0, 0, vec![0u32; count]);
        for pull in 0.. {
            let (time, delay) = pull_at(pull, 0.0);
            if time >= seconds {
                break;
            }
            for (((stream, sent), come), next) in streams.iter_mut().zip(&senders).zip(&comes).zip(&mut nexts) {
                while let Some(&(at, index)) = come.get(*next)
                    && at <= time
                {
                    let packet = &sent[index];
                    stream.push(packet.sequence, packet.timestamp, Duration::from_secs_f64(at), &packet.payload);
                    *next += 1;
                }
            }
            mix(&mut streams, Duration::from_secs_f64(time), delay, &mut out);
            let sum = |index: usize| streams.iter().map(|stream| i32::from(stream.block[index])).sum::<i32>();
            assert!(
                (0..BLOCK).all(|index| i32::from(out[index]) == sum(index).clamp(i16::MIN.into(), i16::MAX.into())),
                "the sum of the levelled blocks, held to what a sample can be"
            );
            clipped += (0..BLOCK).filter(|&index| sum(index).abs() > i32::from(i16::MAX)).count();
            total += BLOCK;
            for (level, stream) in levels.iter_mut().zip(&streams) {
                *level = (*level).max(u32::from(peak(&stream.block)));
            }
        }
        (clipped as f64 / total as f64, levels)
    };
    let (one, levels) = talkers(1);
    assert!(one == 0.0 && levels[0] > PEAK_FLOOR, "one voice clips nowhere and is heard");
    let (two, _) = talkers(2);
    let (four, levels) = talkers(4);
    // Measured at 0.46 % and 1.76 % of samples; the bounds are about twice that.
    assert!(two < 0.01 && four < 0.03, "voices brought to one level clip little together: {two}, {four}");
    assert!(levels.iter().all(|&level| level > PEAK_FLOOR), "each is heard in the sum");
    let mut silent = [Stream::default()];
    let mut out = [7i16; BLOCK];
    mix(&mut silent, Duration::ZERO, 0, &mut out);
    assert_eq!(out, [0; BLOCK], "a stream with no packet is silent");
}

#[test]
fn stats_line() {
    let stats =
        Stats { late: 2, concealed: 960, recovered: 1, waited: 480, restarts: 1, delay: Duration::from_millis(28) };
    assert_eq!(
        stats.to_line(7),
        "sound: member 7: 2 packets late, 1 recovered, 960 samples made up, 480 waited for, 1 restarts, delay 28 ms"
    );
}

use super::echo::{self, Canceller, Figures};
use super::jitter::{self, Stats, Stream};
use super::level::Leveller;
use super::{FULL_SCALE, decibels, samples_in, to_span};
use crate::base::math::dot;
use crate::codec::opus::{self, Bandwidth, Encoder};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

/// The rate a call's sound is recorded and played at.
pub const RATE: u32 = opus::RATE;
// Signal's packets of sound: 60 ms each (peer_connection.cc, `ptime`), at a constant 32 kbit/s (`maxaveragebitrate`, `cbr`).
const PACKET_MS: usize = 60;
const BITS_PER_SECOND: usize = 32_000;
const PACKET_SAMPLES: usize = RATE as usize * PACKET_MS / 1000;
const PACKET_BYTES: usize = BITS_PER_SECOND * PACKET_MS / 1000 / u8::BITS as usize;
// RFC 6464, 3: a level in decibels under the overload point, 0 to 127, 127 for silence.
const QUIETEST: f32 = 127.0;

/// A packet of the microphone's sound for the far side: Opus, the time of its first sample counted from the first recorded, its level in decibels under full scale, and whether it holds voice.
#[derive(Clone, PartialEq, Eq)]
pub struct Packet {
    pub(crate) opus: Vec<u8>,
    pub(crate) sampled: Duration,
    pub(crate) level: u8,
    pub(crate) is_voice: bool,
}

// What the card did to a call's sound: the speaker's underruns, the microphone's overruns and the samples they lost, and the samples the speaker's ring held as each block was asked for, the least and their sum over the blocks.
#[derive(Clone, Copy, Default)]
struct Card {
    underruns: u64,
    overruns: u64,
    lost: u64,
    least: Option<usize>,
    held: u64,
    blocks: u64,
}

impl Card {
    // The log line of what the card did.
    fn to_line(self) -> String {
        let ms = |samples: f64| samples * 1000.0 / f64::from(RATE);
        let ring = self.least.map_or(String::from("no block asked for"), |least| {
            let mean = self.held as f64 / self.blocks.max(1) as f64;
            format!("the speaker's ring {:.0} ms at the least and {:.0} ms on average", ms(least as f64), ms(mean))
        });
        format!(
            "sound: card: {} underruns of the speaker, {} overruns of the microphone losing {} samples, {ring}",
            self.underruns, self.overruns, self.lost
        )
    }
}

// A far member's packet on its way from the calls' thread to the speaker's: its member's number, its extended sequence number, its RTP timestamp, when it came and its payload.
struct Arrival {
    member: u32,
    sequence: u64,
    timestamp: u32,
    at: Duration,
    bytes: Vec<u8>,
}

/// What passes between a call's two sound threads and the calls' own under the one lock they share: the blocks the speaker played, for the microphone's canceller; the microphone's packets, for the far side, and their buffers back; the far members' packets, for the speaker; whether the microphone is muted; and what the canceller and each member's sound tell. Each sound thread works on its own side, `Microphone` or `Speaker`, and takes the lock only to move what passes.
#[derive(Default)]
pub struct Sound {
    is_muted: bool,
    // The blocks the card played since the microphone's side took them, each by the card's sample its first is heard at and its length, their samples in a row; and where the block the speaker's side filled last is heard from.
    played: Vec<(u64, usize)>,
    samples: Vec<i16>,
    heard_at: u64,
    // The packets that wait for the calls' thread, and those it sent, whose buffers the microphone's side fills again.
    packets: Vec<Packet>,
    spent: Vec<Packet>,
    arrivals: Vec<Arrival>,
    spare: Vec<Vec<u8>>,
    echo: Figures,
    card: Card,
    members: Vec<(u32, Stats)>,
}

// The decibels a block's mean power lies under full scale (RFC 6464, 3).
fn level_of(samples: &[i16]) -> u8 {
    let share = dot(samples, samples) / samples.len().max(1) as f32 / (FULL_SCALE * FULL_SCALE);
    (0.5 - decibels(share)).clamp(0.0, QUIETEST) as u8
}

impl Sound {
    /// Takes a block as the card plays it, at its volume, the one the speaker's side filled last: what the microphone's sound is cleared of. What lies further behind the newest than the canceller keeps of the far side goes.
    pub fn hear(&mut self, block: &[i16]) {
        self.played.push((self.heard_at, block.len()));
        self.samples.extend_from_slice(block);
        while self.samples.len() > echo::RING && self.played.len() > 1 {
            let (_, length) = self.played.remove(0);
            self.samples.drain(..length);
        }
    }

    /// Counts an underrun of the speaker: the card ran dry before a block.
    pub const fn note_underrun(&mut self) {
        self.card.underruns += 1;
    }

    /// Swaps the packets that wait for the far side into `taken`, whose room is left for the next.
    pub fn take_packets(&mut self, taken: &mut Vec<Packet>) {
        core::mem::swap(&mut self.packets, taken);
    }

    /// Takes back the packets the far side was sent, emptying `sent`, so that the microphone's side codes the next into their buffers and takes no memory.
    pub fn give_back(&mut self, sent: &mut Vec<Packet>) {
        self.spent.append(sent);
    }

    /// Takes a packet of a far member's sound that came at `now`: its member's number, its extended sequence number, its RTP timestamp and its payload. Packets that waited for the speaker's side longer than a plan's delay go.
    pub fn push(&mut self, member: u32, sequence: u64, timestamp: u32, now: Duration, payload: &[u8]) {
        let (arrivals, spare) = (&mut self.arrivals, &mut self.spare);
        spare.extend(arrivals.extract_if(.., |arrival| jitter::is_stale(arrival.at, now)).map(|arrival| arrival.bytes));
        let mut bytes = spare.pop().unwrap_or_default();
        bytes.clear();
        bytes.extend_from_slice(payload);
        arrivals.push(Arrival { member, sequence, timestamp, at: now, bytes });
    }

    /// Has the microphone heard no more, or again.
    pub const fn set_muted(&mut self, is_muted: bool) {
        self.is_muted = is_muted;
    }

    /// The log lines of the call's sound: what the echo canceller did, what the card did, then how each far member's sound came.
    pub fn to_lines(&self) -> Vec<String> {
        let members = self.members.iter().map(|(member, stats)| stats.to_line(*member));
        [self.echo.to_line(), self.card.to_line()].into_iter().chain(members).collect()
    }
}

/// A call's sound on the microphone's thread: each block cleared of what the speaker played, levelled, and encoded a packet at a time.
pub struct Microphone {
    canceller: Canceller,
    leveller: Leveller,
    encoder: Encoder,
    is_muted: bool,
    block: Vec<i16>,
    // The microphone's samples that fill no packet yet, whether any of them held voice, and the count of the samples before them.
    taken: Vec<i16>,
    has_voice: bool,
    count: u64,
    // The packets coded, and the buffers of those sent, for the next.
    packets: Vec<Packet>,
    spare: Vec<Vec<u8>>,
    // The blocks the speaker played, as taken from the call's sound.
    played: Vec<(u64, usize)>,
    samples: Vec<i16>,
    // The card's overruns of the microphone and the samples they lost.
    overruns: u64,
    lost: u64,
}

impl Default for Microphone {
    fn default() -> Microphone {
        Microphone {
            canceller: Canceller::default(),
            leveller: Leveller::default(),
            // The canceller lets nothing above wideband through.
            encoder: Encoder::new(Bandwidth::Wide),
            is_muted: false,
            block: Vec::new(),
            taken: Vec::new(),
            has_voice: false,
            count: 0,
            packets: Vec::new(),
            spare: Vec::new(),
            played: Vec::new(),
            samples: Vec::new(),
            overruns: 0,
            lost: 0,
        }
    }
}

impl Microphone {
    /// Takes from the call's sound the blocks the speaker played since, the buffers of the packets sent and whether the microphone is muted.
    pub fn take(&mut self, sound: &mut Sound) {
        core::mem::swap(&mut self.played, &mut sound.played);
        core::mem::swap(&mut self.samples, &mut sound.samples);
        self.spare.extend(sound.spent.drain(..).map(|packet| packet.opus));
        self.is_muted = sound.is_muted;
    }

    /// Takes a block the microphone recorded, `delay` samples recorded after it waiting to be read at `now`. While the microphone is muted nothing is encoded, and the far side hears a pause.
    pub fn record(&mut self, block: &[i16], now: Duration, delay: usize) {
        let mut from = 0;
        for &(at, length) in &self.played {
            self.canceller.play(&self.samples[from..from + length], at);
            from += length;
        }
        self.played.clear();
        self.samples.clear();
        let at = samples_in(now, RATE).wrapping_sub((delay + block.len()) as u64);
        self.block.clear();
        self.block.extend_from_slice(block);
        self.canceller.record(&mut self.block, at);
        self.has_voice |= self.leveller.level(&mut self.block);
        self.taken.extend_from_slice(&self.block);
        while self.taken.len() >= PACKET_SAMPLES {
            let mut opus = self.spare.pop().unwrap_or_default();
            opus.resize(PACKET_BYTES, 0);
            let samples = &self.taken[..PACKET_SAMPLES];
            if !self.is_muted && self.encoder.encode(samples, &mut opus).is_ok() {
                let (sampled, level) = (to_span(self.count, RATE), level_of(samples));
                self.packets.push(Packet { opus, sampled, level, is_voice: self.has_voice });
            } else {
                self.spare.push(opus);
            }
            self.taken.drain(..PACKET_SAMPLES);
            (self.count, self.has_voice) = (self.count + PACKET_SAMPLES as u64, false);
        }
    }

    /// Counts an overrun of the microphone and the samples it lost before the next block.
    pub fn lose(&mut self, samples: usize) {
        self.canceller.lose(samples as u64);
        self.count += samples as u64;
        (self.overruns, self.lost) = (self.overruns + 1, self.lost + samples as u64);
    }

    /// Leaves the packets that wait and what the canceller tells in the call's sound; true where packets wait for the calls' thread.
    pub fn give(&mut self, sound: &mut Sound) -> bool {
        // Where the calls' thread took the packets that waited, the microphone's are swapped in whole for the room it left, so that nothing grows under the lock; where it coded none, both keep their room.
        if sound.packets.is_empty() && !self.packets.is_empty() {
            core::mem::swap(&mut sound.packets, &mut self.packets);
        } else {
            sound.packets.append(&mut self.packets);
        }
        sound.echo = self.canceller.figures();
        (sound.card.overruns, sound.card.lost) = (self.overruns, self.lost);
        !sound.packets.is_empty()
    }
}

/// A call's sound on the speaker's thread: each far member's packets in a jitter buffer of its own, made at its first packet, and the members' sound mixed into each block.
#[derive(Default)]
pub struct Speaker {
    // The far members that sound, each by the number its packets come under, and the stream of each.
    members: Vec<u32>,
    streams: Vec<Stream>,
    // The packets taken from the call's sound, and the buffers of those played, for it to fill again.
    arrivals: Vec<Arrival>,
    spent: Vec<Vec<u8>>,
}

impl Speaker {
    /// Takes the far members' packets that came, leaves what each member's sound tells in the call's sound, and tells it where the block filled next is heard from: `delay` samples after `now`, what the card's ring holds.
    pub fn exchange(&mut self, sound: &mut Sound, now: Duration, delay: usize) {
        let card = &mut sound.card;
        (card.least, card.held, card.blocks) =
            (Some(card.least.map_or(delay, |least| least.min(delay))), card.held + delay as u64, card.blocks + 1);
        sound.spare.append(&mut self.spent);
        self.arrivals.append(&mut sound.arrivals);
        sound.members.resize(self.members.len(), (0, Stats::default()));
        for (kept, (&member, stream)) in sound.members.iter_mut().zip(self.members.iter().zip(&self.streams)) {
            *kept = (member, stream.stats());
        }
        sound.heard_at = samples_in(now, RATE).wrapping_add(delay as u64);
    }

    /// Fills the block the speaker plays next, its first sample heard `delay` samples after `now`, with the far members' sound.
    pub fn play(&mut self, now: Duration, delay: usize, out: &mut [i16]) {
        for arrival in self.arrivals.drain(..) {
            let at = self.members.iter().position(|&held| held == arrival.member).unwrap_or_else(|| {
                self.members.push(arrival.member);
                self.streams.push(Stream::default());
                self.streams.len() - 1
            });
            self.streams[at].push(arrival.sequence, arrival.timestamp, arrival.at, &arrival.bytes);
            self.spent.push(arrival.bytes);
        }
        jitter::mix(&mut self.streams, now, delay, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::alloc_count::allocations;
    use crate::base::math::{sine_cosine, xorshift64};
    use crate::hw::sound::card;
    use crate::hw::sound::peak;

    const BLOCK: usize = card::PERIOD;

    // A tone of 440 Hz at a quarter of full scale, from its sample `from` on.
    fn tone(from: usize, count: usize) -> Vec<i16> {
        (from..from + count).map(|at| (8192.0 * sine_cosine(at as f64 * 440.0 / f64::from(RATE)).0) as i16).collect()
    }

    fn at(block: usize) -> Duration {
        to_span((block * BLOCK) as u64, RATE)
    }

    // A block through the microphone's side as its thread takes it, read at once at `now`: true where packets wait.
    fn record(microphone: &mut Microphone, sound: &mut Sound, block: &[i16], now: Duration) -> bool {
        microphone.take(sound);
        microphone.record(block, now, 0);
        microphone.give(sound)
    }

    // The packets that wait for the far side as the calls' thread takes them: a copy for the far side, theirs given back.
    fn sent(sound: &mut Sound) -> Vec<Packet> {
        let mut packets = Vec::new();
        sound.take_packets(&mut packets);
        let copies = packets.clone();
        sound.give_back(&mut packets);
        copies
    }

    #[test]
    fn microphone_to_packets() {
        let (mut microphone, mut sound) = (Microphone::default(), Sound::default());
        let mut packets = Vec::new();
        for block in 0..30 {
            if record(&mut microphone, &mut sound, &tone(block * BLOCK, BLOCK), at(block + 1)) {
                packets.extend(sent(&mut sound));
            }
        }
        assert_eq!(packets.len(), 30 * BLOCK / PACKET_SAMPLES, "a packet for each 60 ms recorded");
        assert!(packets.iter().all(|packet| packet.opus.len() == 240), "of 240 bytes: 32 kbit/s");
        assert_eq!(
            packets.iter().map(|packet| packet.sampled).collect::<Vec<_>>(),
            (0..packets.len() as u32).map(|number| Duration::from_millis(60) * number).collect::<Vec<_>>(),
            "each stamped 60 ms after the one before"
        );
        assert!(packets[3..].iter().all(|packet| packet.level < 30), "a loud tone's level: {}", packets[3].level);
        assert_eq!(opus::Decoder::samples_of(&packets[0].opus), Ok(PACKET_SAMPLES), "three frames of 20 ms");
        microphone.lose(PACKET_SAMPLES);
        sound.set_muted(true);
        for block in 30..40 {
            assert!(
                !record(&mut microphone, &mut sound, &tone(block * BLOCK, BLOCK), at(block + 1)),
                "muted: no packet"
            );
        }
        sound.set_muted(false);
        let mut later = Vec::new();
        for block in 40..43 {
            record(&mut microphone, &mut sound, &tone(block * BLOCK, BLOCK), at(block + 1));
            later.extend(sent(&mut sound));
        }
        let expected = (43 * BLOCK / PACKET_SAMPLES) as u32;
        assert_eq!(
            later.last().map(|packet| packet.sampled),
            Some(Duration::from_millis(60) * expected),
            "the time lost and the time muted pass on the far side's clock"
        );
        assert_eq!((level_of(&[0; 480]), level_of(&[i16::MIN; 480]), level_of(&[]),), (127, 0, 127));
    }

    #[test]
    fn far_packets_to_speaker() {
        let (mut far, mut far_sound) = (Microphone::default(), Sound::default());
        let (mut speaker, mut sound) = (Speaker::default(), Sound::default());
        let mut out = [0i16; BLOCK];
        speaker.exchange(&mut sound, Duration::ZERO, 0);
        speaker.play(Duration::ZERO, 0, &mut out);
        assert_eq!(out, [0; BLOCK], "no far member: silence");
        let (mut sequence, mut heard) = (0u64, 0u16);
        for block in 0..120 {
            let now = at(block + 1);
            if record(&mut far, &mut far_sound, &tone(block * BLOCK, BLOCK), now) {
                for packet in sent(&mut far_sound) {
                    let timestamp = (sequence * PACKET_SAMPLES as u64) as u32;
                    sound.push(7, sequence, timestamp, now, &packet.opus);
                    sequence += 1;
                }
            }
            speaker.exchange(&mut sound, now, 4 * BLOCK);
            speaker.play(now, 4 * BLOCK, &mut out);
            sound.hear(&out);
            heard = heard.max(peak(&out));
        }
        assert!(heard > 1000, "the far member's tone comes out of the speaker's blocks: a peak of {heard}");
        assert_eq!(speaker.members, [7], "one stream for the one member");
        let lines = sound.to_lines();
        assert!(
            lines.len() == 3
                && lines[0].starts_with("echo: ")
                && lines[1].starts_with("sound: card: 0 underruns")
                && lines[2].starts_with("sound: member 7: 0 packets late"),
            "the canceller's line, the card's, then the member's: {lines:?}"
        );
    }

    #[test]
    fn card_line() {
        let (mut microphone, mut speaker, mut sound) = (Microphone::default(), Speaker::default(), Sound::default());
        assert_eq!(
            sound.to_lines()[1],
            "sound: card: 0 underruns of the speaker, 0 overruns of the microphone losing 0 samples, no block asked for"
        );
        for delay in [2048, 1024, 1536] {
            speaker.exchange(&mut sound, Duration::ZERO, delay);
        }
        sound.note_underrun();
        microphone.lose(480);
        microphone.lose(960);
        microphone.give(&mut sound);
        assert_eq!(
            sound.to_lines()[1],
            "sound: card: 1 underruns of the speaker, 2 overruns of the microphone losing 1440 samples, the speaker's ring 21 ms at the least and 32 ms on average",
            "the card's underruns, overruns and how full the speaker's ring stood as each block was asked for"
        );
    }

    #[test]
    fn lock_moves_data_alone() {
        let (mut microphone, mut speaker, mut sound) = (Microphone::default(), Speaker::default(), Sound::default());
        let (block, mut out, mut taken) = (tone(0, BLOCK), [0i16; BLOCK], Vec::new());
        let payload = [0xF8, 0xFF, 0xFE];
        for number in 0..50u32 {
            let now = at(number as usize + 1);
            let before = allocations();
            sound.push(7, u64::from(number), number * PACKET_SAMPLES as u32, now, &payload);
            speaker.exchange(&mut sound, now, 4 * BLOCK);
            let exchanged = allocations();
            speaker.play(now, 4 * BLOCK, &mut out);
            let heard = allocations();
            sound.hear(&out);
            microphone.take(&mut sound);
            let taken_under = allocations();
            microphone.record(&block, now, 0);
            let given = allocations();
            microphone.give(&mut sound);
            sound.take_packets(&mut taken);
            sound.give_back(&mut taken);
            let after = allocations();
            if number >= 4 {
                assert_eq!(
                    (exchanged - before) + (taken_under - heard) + (after - given),
                    0,
                    "block {number}: under the lock a packet pushed, the speaker's exchange, a block heard, the microphone's take and give and the packets taken and given back take no memory once their buffers have grown"
                );
            }
        }
        assert_eq!(sound.arrivals.len(), 0, "every packet reached the speaker's side");
        let mut stalled = Sound::default();
        for number in 0..(echo::RING / BLOCK + 8) as u32 {
            stalled.hear(&out);
            stalled.push(9, u64::from(number), 0, at(number as usize), &payload);
        }
        assert!(
            stalled.samples.len() <= echo::RING
                && stalled.played.iter().map(|block| block.1).sum::<usize>() == stalled.samples.len(),
            "blocks played that no microphone takes are held no further back than the canceller reaches: {} samples",
            stalled.samples.len()
        );
        assert!(
            stalled.arrivals.len() as u64 <= jitter_reach(),
            "packets no speaker takes are held no longer than a plan's delay: {}",
            stalled.arrivals.len()
        );
    }

    #[test]
    fn sound_threads_take_no_memory() {
        let (mut far, mut far_sound) = (Microphone::default(), Sound::default());
        let (mut microphone, mut speaker, mut sound) = (Microphone::default(), Speaker::default(), Sound::default());
        let (mut out, mut sequence, mut taken) = ([0i16; BLOCK], 0u64, Vec::new());
        for block in 0..4 * jitter_reach() as usize {
            let now = at(block + 1);
            if record(&mut far, &mut far_sound, &tone(block * BLOCK, BLOCK), now) {
                for packet in sent(&mut far_sound) {
                    sound.push(7, sequence, (sequence * PACKET_SAMPLES as u64) as u32, now, &packet.opus);
                    sequence += 1;
                }
            }
            let recorded = tone(block * BLOCK, BLOCK);
            let before = allocations();
            speaker.exchange(&mut sound, now, card::CALL_BUFFER);
            speaker.play(now, card::CALL_BUFFER, &mut out);
            sound.hear(&out);
            microphone.take(&mut sound);
            microphone.record(&recorded, now, 0);
            microphone.give(&mut sound);
            let memory = allocations() - before;
            sound.take_packets(&mut taken);
            sound.give_back(&mut taken);
            assert!(
                (block as u64) < jitter_reach() || memory == 0,
                "block {block}: past a plan's most delay the speaker's and the microphone's threads play, record and code in the room they have, the packets' buffers given back: {memory} allocations"
            );
        }
    }

    // The blocks within a plan's most delay, 500 ms.
    fn jitter_reach() -> u64 {
        (0..).take_while(|&number| !jitter::is_stale(Duration::ZERO, at(number as usize))).count() as u64 + 1
    }

    // A thread's lateness after its block is due: within 1 ms in seven of ten blocks, and in three from 2 ms to 16 ms, a frame of a drawing thread above it in its way.
    fn lateness(state: &mut u64) -> Duration {
        let mut share = || {
            *state = xorshift64(*state);
            (*state >> 11) as f64 / (1u64 << 53) as f64
        };
        let seconds = if share() < 0.7 { 0.001 * share() } else { 0.002 + 0.014 * share() };
        Duration::from_secs_f64(seconds)
    }

    // The realignments of the canceller over a minute of a call whose threads come late as `lateness` has them: the speaker's stamps from a clock read where its delay is or after its block's work, and the microphone's with the samples that wait behind its block or without them.
    fn realignments(is_read_with_delay: bool) -> u64 {
        let (mut microphone, mut speaker, mut sound) = (Microphone::default(), Speaker::default(), Sound::default());
        let (mut state, mut out, silence) = (0x5EED_u64, [0i16; BLOCK], [0i16; BLOCK]);
        let ring = card::CALL_BUFFER;
        let seconds = |samples: usize| samples as f64 / f64::from(RATE);
        // How late a clock read beside a delay lies behind it: up to 1 ms, for the card's pointer and the read.
        let error = |state: &mut u64| {
            *state = xorshift64(*state);
            Duration::from_secs_f64(0.001 * ((*state >> 11) as f64 / (1u64 << 53) as f64))
        };
        for block in 0..60 * RATE as usize / BLOCK {
            // The speaker's block that many ahead, written once the ring has room for the one before it.
            let ahead = block + ring / BLOCK - 1;
            let woken = Duration::from_secs_f64(seconds((ahead * BLOCK).saturating_sub(ring))) + lateness(&mut state);
            let delay = (ahead * BLOCK).saturating_sub(samples_in(woken, RATE) as usize);
            let read = if is_read_with_delay { woken + error(&mut state) } else { woken + lateness(&mut state) };
            speaker.exchange(&mut sound, read, delay);
            speaker.play(read, delay, &mut out);
            sound.hear(&silence);
            // The microphone's block, read once it is whole and its thread runs.
            let woken = Duration::from_secs_f64(seconds((block + 1) * BLOCK)) + lateness(&mut state);
            let waiting = (samples_in(woken, RATE) as usize).saturating_sub((block + 1) * BLOCK);
            microphone.take(&mut sound);
            if is_read_with_delay {
                microphone.record(&silence, woken + error(&mut state), waiting);
            } else {
                microphone.record(&silence, woken + lateness(&mut state), 0);
            }
            microphone.give(&mut sound);
        }
        sound.echo.realignments
    }

    #[test]
    fn stamps_hold_alignment() {
        // The bound lies far under the realignments a minute of late stamps makes.
        let late = realignments(false);
        assert!(
            late > 300,
            "stamps of a clock read when a late thread gets to it realign the canceller again and again: {late} in a minute"
        );
        assert_eq!(
            realignments(true),
            0,
            "stamps of a clock read beside the card's delay keep it aligned however late the threads come"
        );
    }
}

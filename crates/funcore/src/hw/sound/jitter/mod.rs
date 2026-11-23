// A call's sound from the far side: each member's Opus packets as the network brings them, held, decoded and kept to the speaker's clock, and the members' sounds mixed into the block the speaker plays.

mod accelerate;
mod reader;
#[cfg(test)]
mod tests;
mod timing;

use super::level::Leveller;
use super::pace::{FULL_PACE_LAG, Follower, MAX_PACE_CHANGE};
use super::to_span;
use crate::codec::opus::{self, Decoder};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;
use reader::{Reader, Rooms};
use timing::{MOST_SPREAD, Timing};

const RATE: u32 = opus::RATE;
// RingRTC's most packets held, 50 (peer_connection_factory.rs, `max_packets`).
const MOST_PACKETS: usize = 50;
// The most bytes a packet holds: 120 ms at Opus's highest rate of 510 kbit/s (RFC 6716, 1 and 3.2.5).
const MOST_BYTES: usize = 510_000 / 8 * 120 / 1000;
// The plan's own change, half the pace's range: the other half is left to the two clocks' drift.
const PLAN_RATE: f64 = MAX_PACE_CHANGE as f64 / 2.0;

// What a stream tells of its sound for the call's log; what was lost on the way, and the jitter, are the RTP receiver's.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Stats {
    // Packets that came after their time was played.
    late: u64,
    // Samples made up for sound that was lost or had not come.
    concealed: u64,
    // Packets made up from the copy the packet behind them holds.
    recovered: u64,
    // Samples made up while no sound had come, by which the delay grew.
    waited: u64,
    // Times the far side's packets fit no plan and the stream began anew.
    restarts: u64,
    // How long the soonest sound waits before it is heard, the card's delay and the block it is played in left out.
    delay: Duration,
}

impl Stats {
    // The log line of a far member's sound, by the number its packets come under.
    pub(super) fn to_line(self, member: u32) -> String {
        format!(
            "sound: member {member}: {} packets late, {} recovered, {} samples made up, {} waited for, {} restarts, delay {} ms",
            self.late,
            self.recovered,
            self.concealed,
            self.waited,
            self.restarts,
            self.delay.as_millis()
        )
    }
}

// The sound of one far member: its packets held and decoded, played at a delay that follows the network and kept to the speaker's clock by the follower.
#[derive(Default)]
pub(super) struct Stream {
    reader: Reader,
    follower: Follower,
    timing: Timing,
    // The last timestamp taken and its place on the far side's clock, counted from the first packet's.
    stamp: Option<(u32, i64)>,
    transit: Option<f64>,
    // The planned samples between a sound's place on the far side's clock and the time it is heard; none before the first pull with sound.
    plan: Option<f64>,
    // When packets that fit no plan began to come, none that fit since.
    stray: Option<f64>,
    leveller: Leveller,
    block: Vec<i16>,
}

// A time of the caller's clock in samples.
fn place_of(time: Duration) -> f64 {
    time.as_secs_f64() * f64::from(RATE)
}

// True for a packet that came at `at` and has not reached its stream by `now`: it waited longer than any plan holds, and its sound can be played no more.
pub(super) fn is_stale(at: Duration, now: Duration) -> bool {
    place_of(now.saturating_sub(at)) > MOST_SPREAD
}

impl Stream {
    // Takes a packet of the far side: its extended sequence number, its RTP timestamp, when it came and its payload; one that is no Opus packet is passed over.
    pub(crate) fn push(&mut self, sequence: u64, timestamp: u32, now: Duration, payload: &[u8]) {
        let Ok(samples) = Decoder::samples_of(payload) else {
            return;
        };
        if payload.len() > MOST_BYTES {
            return;
        }
        let at = self.stamp.map_or(0, |(last, place)| place + i64::from(timestamp.wrapping_sub(last) as i32));
        let transit = place_of(now) - at as f64;
        let does_fit =
            !self.reader.is_behind(sequence) && self.transit.is_none_or(|last| (transit - last).abs() <= MOST_SPREAD);
        if !does_fit {
            // Packets that keep coming at timestamps or numbers no plan holds are a stream begun anew.
            let since = *self.stray.get_or_insert(place_of(now));
            if place_of(now) - since < MOST_SPREAD {
                return;
            }
            self.restart();
            return self.push(sequence, timestamp, now, payload);
        }
        self.stray = None;
        (self.stamp, self.transit) = (Some((timestamp, at)), Some(transit));
        self.timing.note_arrival(now, transit);
        self.reader.hold(sequence, at, samples, payload);
    }

    // Begins the stream anew as a new one would begin, but for its reader's buffers, decoder and counts, its leveller and its block.
    fn restart(&mut self) {
        let Stream { reader, follower, timing, stamp, transit, plan, stray, leveller: _, block: _ } = self;
        reader.restart();
        reader.stats.restarts += 1;
        (*follower, *timing) = (Follower::default(), Timing::default());
        (*stamp, *transit, *plan, *stray) = (None, None, None, None);
    }

    // The samples a sound must come before it is heard: a pull's lead at the fastest pace with the one sample more a fraction carried over may take, and the lag of the fastest pace.
    fn margin(&self) -> f64 {
        self.timing.lead() as f64 * (1.0 + f64::from(MAX_PACE_CHANGE)) + 1.0 + f64::from(FULL_PACE_LAG)
    }

    // Fills `out` with the stream's next samples, the first of them heard `delay` samples after `now`; silence before the first packet.
    fn pull(&mut self, now: Duration, delay: usize, out: &mut [i16]) {
        let lead = delay.saturating_add(out.len());
        self.timing.note_lead(now, lead);
        let margin = self.margin();
        let goal = self.timing.goal().map(|transit| transit + margin);
        let step = PLAN_RATE * out.len() as f64;
        let plan = match (self.plan, goal) {
            (Some(plan), Some(goal)) => plan + (goal - plan).clamp(-step, step),
            (Some(plan), None) | (None, Some(plan)) => plan,
            (None, None) => return self.follower.follow(None, &mut self.reader, out),
        };
        let aim = goal.unwrap_or(plan);
        let floor = self.timing.floor();
        self.reader.rooms = Rooms {
            stall: floor.map_or(0, |floor| (floor + MOST_SPREAD + margin - plan).max(0.0) as i64),
            shed: (plan - aim).max(0.0) as i64,
            stretch: (aim - plan).max(0.0) as i64,
            delay: floor.map_or(0, |floor| (plan - margin - floor) as i64),
            limit: floor.zip(self.timing.limit()).map_or(0, |(floor, limit)| (limit - floor) as i64),
            is_resting: floor.is_none(),
        };
        let heard = place_of(now) + delay as f64;
        let target = self.reader.given() as f64 + heard - plan - self.reader.place() as f64;
        self.follower.follow(Some(target), &mut self.reader, out);
        let plan = plan + self.reader.take_moved() as f64;
        self.plan = Some(plan);
        if let Some(floor) = floor {
            let delay = plan - self.reader.pending() as f64 - floor - lead as f64;
            self.reader.stats.delay = to_span(delay.max(0.0) as u64, RATE);
        }
    }

    pub(super) const fn stats(&self) -> Stats {
        self.reader.stats
    }
}

// Mixes the next block of every stream into `out`, its first sample heard `delay` samples after `now`: each brought to one level of voice by its own leveller, the sum held to what a sample can be.
pub(super) fn mix(streams: &mut [Stream], now: Duration, delay: usize, out: &mut [i16]) {
    for stream in streams.iter_mut() {
        let mut block = core::mem::take(&mut stream.block);
        block.resize(out.len(), 0);
        stream.pull(now, delay, &mut block);
        stream.leveller.level(&mut block);
        stream.block = block;
    }
    for (index, sample) in out.iter_mut().enumerate() {
        let sum: i32 = streams.iter().map(|stream| i32::from(stream.block[index])).sum();
        *sample = sum.clamp(i16::MIN.into(), i16::MAX.into()) as i16;
    }
}

use super::super::PEAK_FLOOR;
use super::super::pace::Source;
use super::accelerate::{self, WINDOW};
use super::timing::MOST_SPREAD;
use super::{MOST_PACKETS, RATE, Stats};
use crate::codec::opus::Decoder;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

// 10 ms: what is made up at a time, as libwebrtc conceals Opus (opus_interface.cc, `kWebRtcOpusPlcFrameSizeMs`), and the least the SILK layer conceals.
const UNIT: usize = 480;
// A try at accelerating each 10 ms given, NetEq's block; after a period taken out none for 5 blocks, 50 ms (decision_logic.h, `kMinTimescaleInterval`), but in fast mode, which a delay four times its limit asks for (decision_logic.cc, `ExpectedPacketAvailable`).
const TRY_EVERY: u64 = UNIT as u64;
const PAUSE: u64 = 5 * TRY_EVERY;
const FAST_FACTOR: i64 = 4;
// A packet of two bytes or fewer is the far side's word that it sends nothing while it is silent (opus_interface.cc, `DetermineAudioType`).
const SILENT_BYTES: usize = 2;
// 1 s of comfort noise without a packet, after which the silence is taken for a stall (decision_logic.cc, `kCngTimeoutMs`).
const SILENCE_TIMEOUT: usize = RATE as usize;
// 2.5 ms, the shortest Opus frame (RFC 6716, 2.1.4): the end of a run of quiet samples that is kept to lead into what follows.
const QUIET_RUN: usize = 120;
// The sequence numbers behind the next that are told played or not, a bit each.
const PLAYED_BITS: u64 = u64::BITS as u64;

// A packet held until its sound is due: its first sample's place on the far side's clock and the samples it lasts.
struct Held {
    sequence: u64,
    at: i64,
    samples: usize,
    bytes: Vec<u8>,
}

// What a block may do to the plan: wait for sound that has not come, cut silence, or stretch it, in samples; and the plan's delay beyond the soonest packet's as the pull found it, a wait's samples still pending in it, and the limit over which sound is accelerated, NetEq's playout delay and high limit; and whether nothing came for as long as arrivals are kept, which leaves no sound to make up.
#[derive(Clone, Copy, Default)]
pub(super) struct Rooms {
    pub(super) stall: i64,
    pub(super) shed: i64,
    pub(super) stretch: i64,
    pub(super) delay: i64,
    pub(super) limit: i64,
    pub(super) is_resting: bool,
}

// The far side's sound as a run of samples: packets decoded in order of their sequence numbers, the time between them made up, and the plan's changes told. A place counts samples of the far side's clock from the first packet's timestamp; a sample given counts samples the follower read.
pub(super) struct Reader {
    held: VecDeque<Held>,
    spare: Vec<Vec<u8>>,
    decoder: Decoder,
    line: VecDeque<i16>,
    // Of the line's first samples: those a seek back put before the rest, then those made up while the far side's clock stood; and those found quiet, which the next scan for quiet ones starts behind.
    held_back: usize,
    inserted: usize,
    quiet: usize,
    scratch: Vec<i16>,
    next: Option<u64>,
    played: u64,
    fed: i64,
    given: u64,
    is_silent: bool,
    silence: usize,
    stalled: i64,
    moved: i64,
    // The sample given from which the next try at accelerating may be.
    next_try: u64,
    pub(super) rooms: Rooms,
    pub(super) stats: Stats,
    // The samples the scans for quiet ones looked at.
    #[cfg(test)]
    looked: u64,
}

impl Default for Reader {
    fn default() -> Reader {
        Reader {
            held: VecDeque::new(),
            spare: Vec::new(),
            decoder: Decoder::new(),
            line: VecDeque::new(),
            held_back: 0,
            inserted: 0,
            quiet: 0,
            scratch: Vec::new(),
            next: None,
            played: 0,
            fed: 0,
            given: 0,
            is_silent: false,
            silence: 0,
            stalled: 0,
            moved: 0,
            next_try: 0,
            rooms: Rooms::default(),
            stats: Stats::default(),
            #[cfg(test)]
            looked: 0,
        }
    }
}

impl Reader {
    // Forgets the stream for one begun anew, as a reader made new would be, but for its buffers, its decoder, reset and not made anew, and its counts.
    pub(super) fn restart(&mut self) {
        let Reader {
            held,
            spare,
            decoder,
            line,
            held_back,
            inserted,
            quiet,
            scratch: _,
            next,
            played,
            fed,
            given,
            is_silent,
            silence,
            stalled,
            moved,
            next_try,
            rooms,
            stats: _,
            #[cfg(test)]
                looked: _,
        } = self;
        spare.extend(held.drain(..).map(|packet| packet.bytes));
        decoder.reset();
        line.clear();
        (*held_back, *inserted, *quiet, *next, *played, *fed, *given) = (0, 0, 0, None, 0, 0, 0);
        (*is_silent, *silence, *stalled, *moved, *next_try, *rooms) = (false, 0, 0, 0, 0, Rooms::default());
    }

    // The far side's place of the next sample given that is not made up.
    pub(super) fn place(&self) -> i64 {
        self.fed - (self.line.len() - self.held_back - self.inserted) as i64
    }

    pub(super) const fn given(&self) -> u64 {
        self.given
    }

    // The samples the plan moved by since it was last asked: more for sound made up while the far side's clock stood, less for silence cut.
    pub(super) const fn take_moved(&mut self) -> i64 {
        core::mem::replace(&mut self.moved, 0)
    }

    // The samples of a wait already played that a gap behind it may still take back.
    pub(super) fn pending(&self) -> i64 {
        (self.stalled - self.inserted as i64).max(0)
    }

    // The packets held and the samples waiting on the line.
    #[cfg(test)]
    pub(super) fn sizes(&self) -> (usize, usize) {
        (self.held.len(), self.line.len())
    }

    // True for a sequence number further behind the next than played ones are told.
    pub(super) fn is_behind(&self, sequence: u64) -> bool {
        self.next.is_some_and(|next| sequence.saturating_add(PLAYED_BITS) < next)
    }

    // Keeps a packet until its sound is due; a copy is passed over, and one whose sound is past is late.
    pub(super) fn hold(&mut self, sequence: u64, at: i64, samples: usize, bytes: &[u8]) {
        if let Some(next) = self.next
            && sequence < next
        {
            let behind = next - 1 - sequence;
            if behind < PLAYED_BITS && self.played >> behind & 1 == 1 {
                return;
            }
            if behind < PLAYED_BITS {
                self.played |= 1 << behind;
            }
            self.stats.late += 1;
            return;
        }
        if at.saturating_add(samples as i64) <= self.place() {
            self.stats.late += 1;
            return;
        }
        let Err(index) = self.held.binary_search_by_key(&sequence, |held| held.sequence) else {
            return;
        };
        if self.held.len() == MOST_PACKETS {
            if index == self.held.len() {
                return;
            }
            if let Some(last) = self.held.pop_back() {
                self.spare.push(last.bytes);
            }
        }
        let mut buffer = self.spare.pop().unwrap_or_default();
        buffer.clear();
        buffer.extend_from_slice(bytes);
        self.held.insert(index, Held { sequence, at, samples, bytes: buffer });
    }

    // Counts a packet's sequence number as come: the next one is the one after it where it lies ahead.
    fn mark(&mut self, sequence: u64) {
        match self.next {
            Some(next) if sequence < next => {
                let behind = next - 1 - sequence;
                if behind < PLAYED_BITS {
                    self.played |= 1 << behind;
                }
            }
            next => {
                let after = sequence.saturating_add(1);
                let ahead = next.map_or(PLAYED_BITS, |next| after - next);
                self.played = if ahead >= PLAYED_BITS { 1 } else { self.played << ahead | 1 };
                self.next = Some(after);
            }
        }
    }

    // Lets go of the packets whose sound ends where the far side's clock is not yet past `until`: late ones, or ones a seek passed over.
    fn pass(&mut self, until: i64, is_late: bool) {
        while let Some(first) = self.held.front()
            && first.at.saturating_add(first.samples as i64) <= until
        {
            let Some(held) = self.held.pop_front() else {
                return;
            };
            self.stats.late += u64::from(is_late);
            self.mark(held.sequence);
            self.spare.push(held.bytes);
        }
    }

    // Makes up `count` samples, a unit at the most, onto the line.
    fn conceal(&mut self, count: usize) -> usize {
        self.scratch.resize(UNIT, 0);
        let made = self.decoder.conceal(UNIT, &mut self.scratch).unwrap_or(0).min(count);
        self.line.extend(&self.scratch[..made]);
        made
    }

    // Fills the line, which is empty, with what comes next at the far side's place.
    fn refill(&mut self) {
        let place = self.fed;
        self.pass(place, true);
        let Some((at, sequence)) = self.held.front().map(|first| (first.at, first.sequence)) else {
            return self.wait();
        };
        if at <= place {
            return self.decode(place);
        }
        let gap = at.saturating_sub(place);
        if self.next.is_some_and(|next| sequence > next) {
            // Sound was lost before the packet at hand: made up while the clock stands, so that the lost packet is still played if it comes, and set against the gap once that is made up.
            if self.stalled >= gap {
                return self.credit(gap);
            }
            return self.make_up(gap - self.stalled);
        }
        if self.stalled > 0 {
            return self.credit(self.stalled.min(gap));
        }
        self.fill_silence(gap);
    }

    // Sets sound made up while waiting against a gap the far side left: the far side's clock moves on without a sample, and the plan gives back what the wait took, which no cut takes again.
    fn credit(&mut self, samples: i64) {
        self.fed = self.fed.saturating_add(samples);
        self.stalled -= samples;
        self.moved -= samples;
        self.rooms.shed = (self.rooms.shed - samples).max(0);
        self.rooms.stall = self.rooms.stall.saturating_add(samples);
    }

    fn decode(&mut self, place: i64) {
        let Some(held) = self.held.pop_front() else {
            return;
        };
        self.scratch.resize(held.samples, 0);
        let count = match self.decoder.decode(&held.bytes, &mut self.scratch) {
            Ok(count) => count,
            Err(_) => self.decoder.conceal(held.samples, &mut self.scratch).unwrap_or(0),
        };
        let skip = usize::try_from(place.saturating_sub(held.at)).unwrap_or(0).min(count);
        self.line.extend(&self.scratch[skip..count]);
        self.fed = held.at.saturating_add(count as i64).max(place);
        self.mark(held.sequence);
        // What was made up while waiting for this packet is set against no gap: the delay grew by it.
        self.stats.waited += self.stalled.max(0) as u64;
        (self.is_silent, self.silence, self.stalled) = (held.bytes.len() <= SILENT_BYTES, 0, 0);
        self.spare.push(held.bytes);
    }

    // Makes up lost sound toward the packet at hand, its end from the copy that packet holds of it.
    fn make_up(&mut self, left: i64) {
        let Some(first) = self.held.front() else {
            return;
        };
        let copy = Decoder::copy_of(&first.bytes).filter(|&span| span as i64 == left).and_then(|span| {
            self.scratch.resize(span, 0);
            self.decoder.recover(&first.bytes, span, &mut self.scratch).ok().filter(|&made| made > 0)
        });
        let made = match copy {
            Some(made) => {
                self.line.extend(&self.scratch[..made]);
                self.stats.recovered += 1;
                made
            }
            None => {
                let made = self.conceal(usize::try_from(left).unwrap_or(UNIT));
                self.stats.concealed += made as u64;
                made
            }
        };
        if self.rooms.stall > 0 {
            self.inserted += made;
            self.stalled += made as i64;
            self.rooms.stall -= made as i64;
        } else {
            self.lose(made);
        }
    }

    // Takes sound made up as the far side's time once the plan may wait no longer: what it waited so far is delay.
    fn lose(&mut self, made: usize) {
        self.stats.waited += self.stalled.max(0) as u64;
        self.stalled = 0;
        self.fed = self.fed.saturating_add(made as i64);
    }

    // Fills a gap the far side left in its timestamps while silent: cut where the plan holds too much delay, stretched where too little.
    fn fill_silence(&mut self, gap: i64) {
        if self.rooms.shed > 0 {
            let cut = self.rooms.shed.min(gap);
            (self.fed, self.moved, self.rooms.shed) =
                (self.fed.saturating_add(cut), self.moved - cut, self.rooms.shed - cut);
            return;
        }
        if self.rooms.stretch > 0 {
            let made = self.conceal(UNIT);
            self.inserted += made;
            self.rooms.stretch -= made as i64;
            return;
        }
        let made = self.conceal(usize::try_from(gap).unwrap_or(UNIT));
        self.fed = self.fed.saturating_add(made as i64);
    }

    // Fills the line while no packet is at hand: comfort noise while the far side is silent, else sound made up while the plan waits, as long as it may; noughts, the decoder left alone, once the stream rests.
    fn wait(&mut self) {
        if self.rooms.is_resting {
            self.line.extend([0; UNIT]);
            self.stats.concealed += UNIT as u64;
            return self.lose(UNIT);
        }
        if self.is_silent && self.silence < SILENCE_TIMEOUT {
            if self.rooms.shed > 0 {
                let cut = self.rooms.shed;
                (self.fed, self.moved, self.rooms.shed) = (self.fed.saturating_add(cut), self.moved - cut, 0);
                self.silence = self.silence.saturating_add(cut as usize);
                return;
            }
            let made = self.conceal(UNIT);
            if self.rooms.stretch > 0 {
                self.inserted += made;
                self.rooms.stretch -= made as i64;
            } else {
                self.fed = self.fed.saturating_add(made as i64);
                self.silence += made;
            }
            return;
        }
        let made = self.conceal(UNIT);
        self.stats.concealed += made as u64;
        if self.rooms.stall > 0 {
            self.inserted += made;
            self.stalled += made as i64;
            self.rooms.stall -= made as i64;
        } else {
            self.lose(made);
        }
    }

    // Cuts quiet samples off the front of the line while the plan holds more delay than it needs, but for a run's last stretch; the scan goes on from the quiet samples found before, as the line grows only once it is empty.
    fn cut_quiet(&mut self) {
        if self.held_back > 0 || self.inserted > 0 {
            return;
        }
        let found =
            self.line.range(self.quiet..).take_while(|sample| u32::from(sample.unsigned_abs()) < PEAK_FLOOR).count();
        self.quiet += found;
        #[cfg(test)]
        {
            self.looked += (found + usize::from(self.quiet < self.line.len())) as u64;
        }
        let cut = self.quiet.saturating_sub(QUIET_RUN).min(usize::try_from(self.rooms.shed).unwrap_or(0));
        if cut > 0 {
            self.line.drain(..cut);
            self.quiet -= cut;
            self.rooms.shed -= cut as i64;
            self.moved -= cut as i64;
        }
    }

    // Takes a pitch period out of the sound that plays next while the plan's delay stands over its limit, as NetEq accelerates, never more than the plan holds over its aim; the delay is the pull's as the block has moved it since, by cuts and waits.
    fn accelerate(&mut self) {
        let Rooms { delay, limit, shed, .. } = self.rooms;
        let delay = delay + self.moved - self.pending();
        let is_ready = self.given >= self.next_try && self.held_back == 0 && self.inserted == 0;
        if limit <= 0 || delay < limit || !is_ready || self.line.len() < WINDOW {
            return;
        }
        let is_fast = delay >= FAST_FACTOR * limit;
        self.next_try = self.given + TRY_EVERY;
        let lag = accelerate::period(&self.line.make_contiguous()[..WINDOW], is_fast).filter(|&lag| lag as i64 <= shed);
        let Some(lag) = lag else {
            return;
        };
        accelerate::cut(&mut self.line, lag);
        self.rooms.shed = shed - lag as i64;
        (self.moved, self.quiet) = (self.moved - lag as i64, 0);
        if !is_fast {
            self.next_try = self.given + PAUSE;
        }
    }
}

impl Source for Reader {
    fn seek(&mut self, sample: u64) {
        if sample >= self.given {
            let mut left = sample - self.given;
            while left > 0
                && let Some(_) = self.line.pop_front()
            {
                if self.held_back > 0 {
                    self.held_back -= 1;
                } else {
                    self.inserted = self.inserted.saturating_sub(1);
                }
                left -= 1;
            }
            self.fed = self.fed.saturating_add(i64::try_from(left).unwrap_or(i64::MAX));
            self.stalled = 0;
            self.pass(self.place(), false);
        } else {
            let back = usize::try_from(self.given - sample).unwrap_or(usize::MAX).min(MOST_SPREAD as usize);
            (0..back).for_each(|_| self.line.push_front(0));
            self.held_back += back;
        }
        (self.given, self.quiet) = (sample, 0);
    }

    fn next(&mut self) -> i16 {
        loop {
            if self.rooms.shed > 0 {
                self.cut_quiet();
                self.accelerate();
            }
            if let Some(sample) = self.line.pop_front() {
                self.quiet = self.quiet.saturating_sub(1);
                if self.held_back > 0 {
                    self.held_back -= 1;
                } else if self.inserted > 0 {
                    self.inserted -= 1;
                    self.moved += 1;
                }
                self.given += 1;
                return sample;
            }
            self.refill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_scanned_once() {
        let mut reader = Reader::default();
        // Runs of quiet samples too short to cut, each behind a loud one, while there is delay to shed.
        let run = QUIET_RUN - 20;
        let loud = i16::try_from(PEAK_FLOOR).unwrap_or(i16::MAX);
        reader.line = (0..RATE as usize).map(|at| if at % (run + 1) == run { loud } else { 1 }).collect();
        reader.rooms.shed = i64::from(RATE);
        let given = RATE as usize - QUIET_RUN;
        let first: Vec<i16> = (0..given).map(|_| reader.next()).collect();
        assert!(first.iter().filter(|&&sample| sample == loud).count() == given / (run + 1), "nothing is cut");
        // A scan made anew for every sample looks at half a run, 50 here; one that goes on from the last looks at each sample once, and at each loud one as often as a scan ends there.
        let looked = reader.looked as f64 / given as f64;
        assert!(
            looked <= 2.0,
            "each sample is looked at once, and each loud one as often as a scan ends at it: {looked} a sample"
        );
        (reader.line, reader.quiet) = ((0..RATE as usize).map(|_| 1).collect(), 0);
        reader.next();
        assert_eq!(reader.line.len(), QUIET_RUN - 1, "a quiet run is cut but for its last stretch");
    }

    // A reader whose line holds `quiet` quiet samples and then a voice of a period of 5 ms, with its plan's delay `over` its limit and room to shed: the samples the plan moved by once 60 ms are given.
    fn moved_over(quiet: usize, over: i64) -> i64 {
        let (period, limit) = (240, 960);
        let mut reader = Reader::default();
        let voice = (0..4 * WINDOW).map(|at| ((at % period) as i32 * 64 - 7680) as i16);
        reader.line = (0..quiet).map(|_| 1).chain(voice).collect();
        reader.rooms = Rooms { shed: i64::from(RATE), delay: limit + over, limit, ..Rooms::default() };
        (0..2 * WINDOW).for_each(|_| _ = reader.next());
        reader.take_moved()
    }

    #[test]
    fn accelerate_reads_delay_now() {
        assert!(moved_over(0, 2880) < 0, "a delay over its limit takes a period out of a voice");
        let quiet = 4 * WINDOW;
        assert_eq!(
            moved_over(quiet, (quiet / 2) as i64),
            -((quiet - QUIET_RUN) as i64),
            "once the quiet cut brought the delay under its limit in that block, no period goes"
        );
    }
}

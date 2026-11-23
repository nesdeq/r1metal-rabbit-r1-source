use super::RATE;
use core::time::Duration;

// 500 ms: the span whose latest packet is taken (delay_manager.h, `resample_interval_ms`).
const INTERVAL: Duration = Duration::from_millis(500);
// 2 s of arrivals tell where the soonest lies (decision_logic.cc, `kPacketHistorySizeMs`): four spans before the one under way.
const HISTORY: u64 = 4;
// A span's soonest is that of the 2 s before it or after it, whichever is the later, so that a route that changes is not taken for a delay; it is weighed once the 2 s after it are over.
const SLOTS: usize = 2 * HISTORY as usize + 2;
// Buckets of 20 ms, a hundred of them (underrun_optimizer.cc, `kBucketSizeMs`, `kDelayBuckets`).
const BUCKET: f64 = 0.02 * RATE as f64;
const BUCKETS: usize = 100;
// Each span weighed keeps 0.983 of what was, and less at first by a weight of 2 (delay_manager.h, `forget_factor`, `start_forget_weight`; histogram.cc, `Add`).
const FORGET: f32 = 0.983;
const START_WEIGHT: f32 = 2.0;
// The delay 95 % of spans keep under (delay_manager.h, `quantile`).
const QUANTILE: f32 = 0.95;
// The most delay planned beyond the soonest: RingRTC's 500 ms (peer_connection_factory.rs, `max_target_delay_ms`).
pub(super) const MOST_SPREAD: f64 = 0.5 * RATE as f64;
// What the limit over which sound is accelerated lies above the spread and the latest, 20 ms (decision_logic.cc, `kDelayAdjustmentGranularityMs`).
const GRANULARITY: f64 = 0.02 * RATE as f64;

// The packets that came in one span of the caller's clock: the least and the most transit, and the most lead a pull had.
#[derive(Clone, Copy, Default)]
struct Slot {
    index: u64,
    transits: Option<(f64, f64)>,
    lead: usize,
}

// The delay a stream needs as NetEq's delay manager reckons it in outline (libwebrtc, modules/audio_coding/neteq): how much later than the soonest each packet came, the latest of each span in a histogram that forgets, and the delay 95 % of spans kept under. A transit is the samples between a packet's arrival on the caller's clock and its timestamp; a lead the samples between a pull and the end of its block's sound.
pub(super) struct Timing {
    slots: [Slot; SLOTS],
    at: Option<u64>,
    buckets: [f32; BUCKETS],
    weighed: u32,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing { slots: [Slot::default(); SLOTS], at: None, buckets: [0.0; BUCKETS], weighed: 0 }
    }
}

impl Timing {
    pub(super) fn note_arrival(&mut self, now: Duration, transit: f64) {
        let slot = self.slot_at(now);
        slot.transits =
            Some(slot.transits.map_or((transit, transit), |(low, high)| (low.min(transit), high.max(transit))));
    }

    pub(super) fn note_lead(&mut self, now: Duration, lead: usize) {
        let slot = self.slot_at(now);
        slot.lead = slot.lead.max(lead);
    }

    // The slot of the span under way; spans that end are weighed once the 2 s after them are over, and a clock that goes back stays in the span it was in.
    fn slot_at(&mut self, now: Duration) -> &mut Slot {
        let index = u64::try_from(now.as_nanos() / INTERVAL.as_nanos()).unwrap_or(u64::MAX);
        let at = match self.at {
            Some(at) if index > at => {
                for next in at + 1..=index.min(at + SLOTS as u64) {
                    if let Some(over) = next.checked_sub(HISTORY + 1) {
                        self.weigh(over);
                    }
                    self.slots[next as usize % SLOTS] = Slot { index: next, ..Slot::default() };
                }
                self.slots[index as usize % SLOTS] = Slot { index, ..Slot::default() };
                index
            }
            Some(at) => at,
            None => {
                self.slots[index as usize % SLOTS] = Slot { index, ..Slot::default() };
                index
            }
        };
        self.at = Some(at);
        &mut self.slots[at as usize % SLOTS]
    }

    fn slots_over(&self, first: u64, last: u64) -> impl Iterator<Item = &Slot> {
        self.slots.iter().filter(move |slot| (first..=last).contains(&slot.index))
    }

    fn soonest(&self, first: u64, last: u64) -> Option<f64> {
        self.slots_over(first, last).filter_map(|slot| slot.transits).map(|(low, _)| low).reduce(f64::min)
    }

    // Adds a span's latest packet, relative to the soonest about it, to the histogram.
    fn weigh(&mut self, index: u64) {
        let Some((_, latest)) = self.slots_over(index, index).find_map(|slot| slot.transits) else {
            return;
        };
        let (before, after) =
            (self.soonest(index.saturating_sub(HISTORY), index), self.soonest(index, index + HISTORY));
        let Some(soonest) = before.into_iter().chain(after).reduce(f64::max) else {
            return;
        };
        let bucket = ((latest - soonest).max(0.0) / BUCKET) as usize;
        if bucket >= BUCKETS {
            return;
        }
        self.weighed = self.weighed.saturating_add(1);
        let forget = (1.0 - START_WEIGHT / self.weighed as f32).clamp(0.0, FORGET);
        self.buckets.iter_mut().for_each(|share| *share *= forget);
        self.buckets[bucket] += 1.0 - forget;
    }

    // The spans of the last 2 s and the one under way.
    fn recent(&self) -> impl Iterator<Item = &Slot> {
        let at = self.at.unwrap_or(0);
        self.slots_over(at.saturating_sub(HISTORY), at)
    }

    // The least transit of the last 2 s.
    pub(super) fn floor(&self) -> Option<f64> {
        self.recent().filter_map(|slot| slot.transits).map(|(low, _)| low).reduce(f64::min)
    }

    // The most lead of the last 2 s.
    pub(super) fn lead(&self) -> usize {
        self.recent().map(|slot| slot.lead).max().unwrap_or(0)
    }

    // The samples beyond the soonest that 95 % of spans kept under, in whole buckets; one bucket before any span is weighed.
    fn spread(&self) -> f64 {
        if self.weighed == 0 {
            return BUCKET;
        }
        let mut sum = 0.0;
        let bucket = self.buckets.iter().position(|&share| {
            sum += share;
            sum >= QUANTILE
        });
        (bucket.unwrap_or(BUCKETS - 1) + 1) as f64 * BUCKET
    }

    // The latest transit of the last 2 s, the soonest where none is later.
    fn latest(&self, floor: f64) -> f64 {
        self.recent().filter_map(|slot| slot.transits).map(|(_, high)| high).fold(floor, f64::max)
    }

    // The transit a packet is planned to have at the most: the soonest and the spread, or the latest of the last 2 s where that is later, never more than the most spread beyond the soonest; none while nothing came for 2 s.
    pub(super) fn goal(&self) -> Option<f64> {
        let floor = self.floor()?;
        Some((floor + self.spread()).max(self.latest(floor)).min(floor + MOST_SPREAD))
    }

    // The transit over which a plan's delay is cut by accelerating the sound, NetEq's high limit: the spread over the latest of the last 2 s and the granularity of its steps (decision_logic.cc, `ExpectedPacketAvailable` in its stable delay mode).
    pub(super) fn limit(&self) -> Option<f64> {
        let floor = self.floor()?;
        Some(self.latest(floor) + self.spread() + GRANULARITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A packet every 60 ms; and a transit of a second's worth of samples, under which the floor lies.
    const EVERY: Duration = Duration::from_millis(60);
    const TRANSIT: f64 = RATE as f64;

    // Packets for `spans` spans of 500 ms from the span `from` on, each transit as `transit` gives it from the span and the packet's place in it.
    fn feed(timing: &mut Timing, from: u64, spans: u64, transit: impl Fn(u64, u32) -> f64) {
        for span in from..from + spans {
            let start = INTERVAL * span as u32;
            for (packet, offset) in (0..).zip((0..).map(|step| EVERY * step).take_while(|offset| *offset < INTERVAL)) {
                timing.note_arrival(start + offset, transit(span, packet));
            }
        }
    }

    #[test]
    fn spread_of_late_spans() {
        let mut timing = Timing::default();
        assert_eq!((timing.goal(), timing.spread()), (None, BUCKET), "nothing came: no goal, and a bucket of spread");
        // One span in ten has a packet 100 ms late: more than 5 %, so the spread waits for it.
        let late = 0.1 * RATE as f64;
        feed(&mut timing, 0, 200, |span, packet| TRANSIT + if span % 10 == 5 && packet == 3 { late } else { 0.0 });
        assert_eq!(timing.spread(), 6.0 * BUCKET, "100 ms lies in the sixth bucket");
        // Calm again: each span weighed keeps 0.983 of the late ones' share, under 5 % within 40 spans.
        feed(&mut timing, 200, 60, |_, _| TRANSIT);
        assert_eq!(timing.spread(), BUCKET, "late spans long past are forgotten");
        assert_eq!(timing.goal(), Some(TRANSIT + BUCKET), "the goal is the floor and the spread");
    }

    #[test]
    fn route_step_not_spread() {
        let mut timing = Timing::default();
        let step = 0.2 * RATE as f64;
        feed(&mut timing, 0, 100, |_, _| TRANSIT);
        feed(&mut timing, 100, 2, |_, _| TRANSIT + step);
        assert_eq!(timing.floor(), Some(TRANSIT), "the soonest of the 2 s before is still the old route's");
        assert_eq!(timing.goal(), Some(TRANSIT + step), "but the goal is the latest of them");
        feed(&mut timing, 102, 100, |_, _| TRANSIT + step);
        assert_eq!(timing.floor(), Some(TRANSIT + step), "the floor follows the route");
        assert_eq!(timing.spread(), BUCKET, "and the step is no spread: one span of a hundred holds it");
    }

    #[test]
    fn limit_above_latest() {
        let mut timing = Timing::default();
        assert_eq!(timing.limit(), None, "nothing came: no limit");
        let step = 0.2 * RATE as f64;
        feed(&mut timing, 0, 100, |_, _| TRANSIT);
        assert_eq!(timing.limit(), Some(TRANSIT + BUCKET + GRANULARITY), "calm: the spread and 20 ms over the soonest");
        feed(&mut timing, 100, 2, |_, _| TRANSIT + step);
        assert_eq!(
            timing.limit(),
            Some(TRANSIT + step + BUCKET + GRANULARITY),
            "a later route of the last 2 s lifts it"
        );
    }

    #[test]
    fn goal_bounds_and_lead() {
        let mut timing = Timing::default();
        feed(&mut timing, 0, 10, |span, packet| {
            TRANSIT + if span == 8 && packet == 0 { 2.0 * RATE as f64 } else { 0.0 }
        });
        assert_eq!(timing.goal(), Some(TRANSIT + MOST_SPREAD), "a packet 2 s late plans no more than the most spread");
        let now = INTERVAL * 10;
        timing.note_lead(now, 4096);
        timing.note_lead(now + INTERVAL * 3, 1024);
        assert_eq!(timing.lead(), 4096, "the most lead of 2 s");
        timing.note_lead(now + INTERVAL * 5, 2048);
        assert_eq!(timing.lead(), 2048, "and of no more");
        timing.note_arrival(now, TRANSIT);
        assert_eq!(timing.at, Some(15), "a clock that goes back stays in its span");
        timing.note_arrival(INTERVAL * 1_000, TRANSIT);
        assert_eq!((timing.floor(), timing.lead()), (Some(TRANSIT), 0), "after a long gap only what came since counts");
    }
}

use super::transport_cc::Outcome;
use crate::base::math::{power_f64, sqrt};
use crate::base::time::MICROS_PER_SECOND;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::time::Duration;

// Arrival-time groups (draft-ietf-rmcat-gcc-02, 5.2; delay_based_bwe.cc:42-43, inter_arrival_delta.cc:23-24, inter_arrival_delta.h:28-30): packets sent within 5 ms make a group, as do packets that arrive within 5 ms of the group with a delay variation under nought and within 100 ms of its first; three groups out of order or a clock jump of 3 s begin anew, and so does feedback silent for 2 s.
const GROUP_SPAN: Duration = Duration::from_millis(5);
const BURST_SPAN: Duration = Duration::from_millis(5);
const BURST_LONGEST: Duration = Duration::from_millis(100);
const REORDERED_RESET: u32 = 3;
const CLOCK_JUMP: Duration = Duration::from_secs(3);
const STREAM_TIMEOUT: Duration = Duration::from_secs(2);
// The trendline filter and over-use detector (trendline_estimator.h:29; trendline_estimator.cc:37-38, 112-115, 173-176, 319-322): 20 groups, smoothing 0.9, a gain of 4, the threshold's first 12.5 ms and its range of 6 to 600, its rise 0.0087 and fall 0.039 a millisecond over at most 100 ms, held where the trend lies 15 past it; over-use signalled after 10 ms; the gain grows with the deltas up to 60, counted up to 1000.
const WINDOW: usize = 20;
const SMOOTHING: f64 = 0.9;
const GAIN: f64 = 4.0;
const FIRST_THRESHOLD: f64 = 12.5;
const LEAST_THRESHOLD: f64 = 6.0;
const MOST_THRESHOLD: f64 = 600.0;
const THRESHOLD_UP: f64 = 0.0087;
const THRESHOLD_DOWN: f64 = 0.039;
const MOST_THRESHOLD_STEP_MS: f64 = 100.0;
const MOST_ADAPT_OFFSET: f64 = 15.0;
const OVERUSE_TIME: f64 = 10.0;
const GAIN_DELTAS: u32 = 60;
const MOST_DELTAS: u32 = 1000;
// AIMD rate control (draft 5.5; aimd_rate_control.cc:34-35, 122, 129, 194-205, 239, 278-279, 346-352): back off to 0.85 of the throughput less 5 kbit/s, grow by 8 % a second far from the link's capacity and half a packet a response time near it, never past 1.5 times the throughput and 10 kbit/s more; a response time is the round trip and 100 ms, twice over, at least 4 kbit/s a second, by packets of 1200 bytes at 30 frames a second; a further decrease after a round trip of 10 to 200 ms, or once the throughput falls under half; 200 ms of round trip until one is known.
const BETA: f64 = 0.85;
const DECREASE_LESS: f64 = 5_000.0;
const INCREASE: f64 = 1.08;
const INCREASE_LIMIT: f64 = 1.5;
const INCREASE_LIMIT_MORE: f64 = 10_000.0;
const LEAST_MULTIPLICATIVE: f64 = 1_000.0;
const RESPONSE_MORE: Duration = Duration::from_millis(100);
const LEAST_ADDITIVE: f64 = 4_000.0;
const PACKET_BITS: u64 = 1200 * 8;
const FRAMES_PER_SECOND: u64 = 30;
const LEAST_REDUCE_INTERVAL: Duration = Duration::from_millis(10);
const MOST_REDUCE_INTERVAL: Duration = Duration::from_millis(200);
const REDUCE_FRACTION: f64 = 0.5;
const DEFAULT_ROUND_TRIP: Duration = Duration::from_millis(200);
// Over-use before any throughput is known halves the rate, a further time where the throughput told lies a bit a second under half (delay_based_bwe.cc:225-234, aimd_rate_control.cc:135-139); the over-use timer begins at half the send delta (trendline_estimator.cc:276-280).
const UNDER_HALF: f64 = 1.0;
const FIRST_OVERUSE_SHARE: f64 = 0.5;
// The link capacity's estimate (link_capacity_estimator.cc:23-31, 40, 62; link_capacity_estimator.h:35): updated at over-use with weight 0.05, its normalised deviation from 0.4 within 0.4 to 2.5, its bounds three deviations either side.
const CAPACITY_WEIGHT: f64 = 0.05;
const LEAST_DEVIATION: f64 = 0.4;
const MOST_DEVIATION: f64 = 2.5;
const DEVIATIONS: f64 = 3.0;
// The acknowledged throughput as libwebrtc reckons it by default (acknowledged_bitrate_estimator_interface.h:36-53; robust_throughput_estimator.cc:83, 188): the packets of the last second and at least 20, no more than 500 nor of more than 5 s, an estimate from 10; a packet a second behind the newest begins anew; spans of 1 ms at the least.
const WINDOW_PACKETS: usize = 20;
const MOST_WINDOW_PACKETS: usize = 500;
const LEAST_WINDOW: Duration = Duration::from_secs(1);
const MOST_WINDOW: Duration = Duration::from_secs(5);
const REQUIRED_PACKETS: usize = 10;
const MOST_REORDERING: Duration = Duration::from_secs(1);
const LEAST_SPAN: Duration = Duration::from_millis(1);
// Loss-based control (draft 6; send_side_bandwidth_estimation.cc:39-51, 115-118, 466, 484-489, 506-509, 541-543): a fraction lost in 256ths over at least 20 packets; under 2 % grow 8 % on the least rate of the last second and 1 kbit/s more, over 10 % cut by half the fraction once each 300 ms and a round trip, between hold; a report older than 1.2 times 5 s counts no more; in the first 2 s without loss the delay-based estimate is taken whole; where feedback stops for 3 s while packets go, 0.8 of the rate each second, down to 5 kbit/s (`RttBasedBackoff`).
const LEAST_PACKETS: u64 = 20;
const FRACTION_ONE: u64 = 256;
const LOW_LOSS: f64 = 0.02;
const HIGH_LOSS: f64 = 0.1;
const LOSS_INCREASE: f64 = 1.08;
const LOSS_INCREASE_MORE: f64 = 1_000.0;
const INCREASE_INTERVAL: Duration = Duration::from_secs(1);
const HISTORY_SLACK: Duration = Duration::from_millis(1);
const DECREASE_INTERVAL: Duration = Duration::from_millis(300);
const REPORT_INTERVAL: Duration = Duration::from_secs(5);
const REPORT_AGE: f64 = 1.2;
const START_PHASE: Duration = Duration::from_secs(2);
const BACKOFF_ROUND_TRIP: Duration = Duration::from_secs(3);
const BACKOFF_FRACTION: f64 = 0.8;
const BACKOFF_INTERVAL: Duration = Duration::from_secs(1);
const BACKOFF_FLOOR: f64 = 5_000.0;
// The least rate goog_cc takes from any bounds (bwe_defines.h:24-25, `kCongestionControllerMinBitrate`).
const LEAST_RATE: u64 = 5_000;
// The pacer's rate over the target: 2.5 until transport-wide feedback comes, 1.1 from then on (goog_cc_network_control.cc:55, 58, 661-665).
const PACE_FIRST: f64 = 2.5;
const PACE: f64 = 1.1;
const MICROS_PER_MILLI: f64 = 1_000.0;
const KILO: f64 = 1_000.0;

// The rate a call allows this side, in bits a second: the least, the first and the most.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Bounds {
    pub(crate) least: u64,
    pub(crate) start: u64,
    pub(crate) most: u64,
}

// What the over-use detector holds the link to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Usage {
    Normal,
    Over,
    Under,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Control {
    Hold,
    Increase,
    Decrease,
}

// A group of packets in microseconds: its first and last departure, its first and last arrival, the feedback's time at its last.
#[derive(Clone, Copy)]
struct Group {
    first_sent: i64,
    sent: i64,
    first_arrival: i64,
    complete: i64,
    system: i64,
}

#[derive(Default)]
struct Groups {
    current: Option<Group>,
    previous: Option<Group>,
    reordered: u32,
}

struct Trend {
    deltas: u32,
    first_arrival: Option<f64>,
    accumulated: f64,
    smoothed: f64,
    history: VecDeque<(f64, f64)>,
    previous: f64,
    threshold: f64,
    last_update: Option<f64>,
    over_time: Option<f64>,
    over_count: u32,
    usage: Usage,
}

struct Capacity {
    estimate: Option<f64>,
    deviation: f64,
}

struct Aimd {
    least: f64,
    current: f64,
    throughput: f64,
    capacity: Capacity,
    control: Control,
    last_change: Option<Duration>,
    round_trip: Duration,
}

// A packet acknowledged, in microseconds: its departure and arrival, and its bytes.
#[derive(Clone, Copy)]
struct Acked {
    sent: i64,
    arrival: i64,
    bytes: usize,
}

#[derive(Default)]
struct Throughput {
    window: VecDeque<Acked>,
    discarded: Option<i64>,
}

struct Loss {
    least: f64,
    most: f64,
    current: f64,
    delay_limit: f64,
    receiver_limit: f64,
    lost: u64,
    expected: u64,
    fraction: u64,
    first_report: Option<Duration>,
    last_report: Option<Duration>,
    has_decreased: bool,
    last_decrease: Option<Duration>,
    history: VecDeque<(Duration, f64)>,
    round_trip: Duration,
    propagation: Option<(Duration, Duration)>,
    last_sent: Option<Duration>,
}

// The rate this side may send at, from transport-wide feedback, as goog_cc reckons it in outline (draft-ietf-rmcat-gcc-02; modules/congestion_controller/goog_cc): a delay-based estimate by arrival-time groups, a trendline and an over-use detector driving AIMD rate control on the acknowledged throughput, under a loss-based estimate that takes the lower of both, within the call's bounds.
pub(super) struct Estimator {
    groups: Groups,
    trend: Trend,
    aimd: Aimd,
    throughput: Throughput,
    loss: Loss,
    last_feedback: Option<Duration>,
}

fn micros(time: Duration) -> i64 {
    i64::try_from(time.as_micros()).unwrap_or(i64::MAX)
}

impl Groups {
    // The send and arrival deltas in milliseconds where a packet ends a group and one stood before it (inter_arrival_delta.cc, `ComputeDeltas`).
    fn take(&mut self, sent: i64, arrival: i64, system: i64) -> Option<(f64, f64)> {
        let Some(mut current) = self.current else {
            self.current = Some(Group { first_sent: sent, sent, first_arrival: arrival, complete: arrival, system });
            return None;
        };
        if current.first_sent > sent {
            return None;
        }
        let mut deltas = None;
        if self.is_new_group(&current, sent, arrival) {
            if let Some(previous) = self.previous {
                let send_delta = current.sent - previous.sent;
                let arrival_delta = current.complete - previous.complete;
                if arrival_delta - (current.system - previous.system) >= micros(CLOCK_JUMP) {
                    *self = Groups::default();
                    return None;
                }
                if arrival_delta < 0 {
                    self.reordered += 1;
                    if self.reordered >= REORDERED_RESET {
                        *self = Groups::default();
                    }
                    return None;
                }
                self.reordered = 0;
                deltas = Some((send_delta as f64 / MICROS_PER_MILLI, arrival_delta as f64 / MICROS_PER_MILLI));
            }
            self.previous = Some(current);
            current = Group { first_sent: sent, sent, first_arrival: arrival, complete: arrival, system };
        } else {
            current.sent = current.sent.max(sent);
        }
        (current.complete, current.system) = (arrival, system);
        self.current = Some(current);
        deltas
    }

    fn is_new_group(&self, current: &Group, sent: i64, arrival: i64) -> bool {
        let arrival_delta = arrival - current.complete;
        let send_delta = sent - current.sent;
        let is_burst = send_delta == 0
            || arrival_delta - send_delta < 0
                && arrival_delta <= micros(BURST_SPAN)
                && arrival - current.first_arrival < micros(BURST_LONGEST);
        !is_burst && sent - current.first_sent > micros(GROUP_SPAN)
    }
}

impl Default for Trend {
    fn default() -> Trend {
        Trend {
            deltas: 0,
            first_arrival: None,
            accumulated: 0.0,
            smoothed: 0.0,
            history: VecDeque::new(),
            previous: 0.0,
            threshold: FIRST_THRESHOLD,
            last_update: None,
            over_time: None,
            over_count: 0,
            usage: Usage::Normal,
        }
    }
}

impl Trend {
    // Takes a group's arrival and send deltas and its arrival in milliseconds (trendline_estimator.cc, `UpdateTrendline`).
    fn update(&mut self, arrival_delta: f64, send_delta: f64, arrival: f64) {
        self.deltas = (self.deltas + 1).min(MOST_DELTAS);
        let first = *self.first_arrival.get_or_insert(arrival);
        self.accumulated += arrival_delta - send_delta;
        self.smoothed = SMOOTHING * self.smoothed + (1.0 - SMOOTHING) * self.accumulated;
        self.history.push_back((arrival - first, self.smoothed));
        if self.history.len() > WINDOW {
            self.history.pop_front();
        }
        let mut trend = self.previous;
        if self.history.len() == WINDOW {
            trend = self.slope().unwrap_or(trend);
        }
        self.detect(trend, send_delta, arrival);
    }

    // The least-squares slope of the smoothed delay over the arrivals.
    fn slope(&self) -> Option<f64> {
        let count = self.history.len() as f64;
        let x_mean = self.history.iter().map(|(x, _)| x).sum::<f64>() / count;
        let y_mean = self.history.iter().map(|(_, y)| y).sum::<f64>() / count;
        let numerator: f64 = self.history.iter().map(|(x, y)| (x - x_mean) * (y - y_mean)).sum();
        let denominator: f64 = self.history.iter().map(|(x, _)| (x - x_mean) * (x - x_mean)).sum();
        (denominator != 0.0).then(|| numerator / denominator)
    }

    // trendline_estimator.cc, `Detect` and `UpdateThreshold`.
    fn detect(&mut self, trend: f64, send_delta: f64, now: f64) {
        if self.deltas < 2 {
            self.usage = Usage::Normal;
            return;
        }
        let modified = f64::from(self.deltas.min(GAIN_DELTAS)) * trend * GAIN;
        if modified > self.threshold {
            let over_time = self.over_time.map_or(send_delta * FIRST_OVERUSE_SHARE, |time| time + send_delta);
            self.over_time = Some(over_time);
            self.over_count += 1;
            if over_time > OVERUSE_TIME && self.over_count > 1 && trend >= self.previous {
                (self.over_time, self.over_count, self.usage) = (Some(0.0), 0, Usage::Over);
            }
        } else if modified < -self.threshold {
            (self.over_time, self.over_count, self.usage) = (None, 0, Usage::Under);
        } else {
            (self.over_time, self.over_count, self.usage) = (None, 0, Usage::Normal);
        }
        self.previous = trend;
        let last = *self.last_update.get_or_insert(now);
        if modified.abs() > self.threshold + MOST_ADAPT_OFFSET {
            self.last_update = Some(now);
            return;
        }
        let step = if modified.abs() < self.threshold { THRESHOLD_DOWN } else { THRESHOLD_UP };
        let elapsed = (now - last).min(MOST_THRESHOLD_STEP_MS);
        self.threshold = (self.threshold + step * (modified.abs() - self.threshold) * elapsed)
            .clamp(LEAST_THRESHOLD, MOST_THRESHOLD);
        self.last_update = Some(now);
    }
}

impl Capacity {
    fn upper(&self) -> f64 {
        self.estimate.map_or(f64::INFINITY, |estimate| (estimate + DEVIATIONS * self.deviation_kbps(estimate)) * KILO)
    }

    fn lower(&self) -> f64 {
        self.estimate.map_or(0.0, |estimate| (estimate - DEVIATIONS * self.deviation_kbps(estimate)).max(0.0) * KILO)
    }

    fn deviation_kbps(&self, estimate: f64) -> f64 {
        f64::from(sqrt((self.deviation * estimate) as f32))
    }

    // link_capacity_estimator.cc, `Update`, on a throughput of bits a second.
    fn note_overuse(&mut self, throughput: f64) {
        let sample = throughput / KILO;
        let estimate =
            self.estimate.map_or(sample, |estimate| (1.0 - CAPACITY_WEIGHT) * estimate + CAPACITY_WEIGHT * sample);
        self.estimate = Some(estimate);
        let error = estimate - sample;
        let deviation = (1.0 - CAPACITY_WEIGHT) * self.deviation + CAPACITY_WEIGHT * error * error / estimate.max(1.0);
        self.deviation = deviation.clamp(LEAST_DEVIATION, MOST_DEVIATION);
    }
}

impl Aimd {
    // aimd_rate_control.cc, `TimeToReduceFurther`.
    fn is_time_to_reduce(&self, now: Duration, throughput: f64) -> bool {
        let interval = self.round_trip.clamp(LEAST_REDUCE_INTERVAL, MOST_REDUCE_INTERVAL);
        self.last_change.is_none_or(|last| now.saturating_sub(last) >= interval)
            || throughput < REDUCE_FRACTION * self.current
    }

    // aimd_rate_control.cc, `ChangeBitrate` and `ChangeState`.
    fn update(&mut self, usage: Usage, throughput: Option<f64>, now: Duration) {
        let throughput = throughput.unwrap_or(self.throughput);
        self.throughput = throughput;
        self.control = match (usage, self.control) {
            (Usage::Normal, Control::Hold) => {
                self.last_change = Some(now);
                Control::Increase
            }
            (Usage::Normal, control) => control,
            (Usage::Over, _) => Control::Decrease,
            (Usage::Under, _) => Control::Hold,
        };
        let mut next = None;
        match self.control {
            Control::Hold => {}
            Control::Increase => {
                if throughput > self.capacity.upper() {
                    self.capacity.estimate = None;
                }
                let limit = INCREASE_LIMIT * throughput + INCREASE_LIMIT_MORE;
                if self.current < limit {
                    let elapsed = self.last_change.map_or(0.0, |last| now.saturating_sub(last).as_secs_f64());
                    let increase = if self.capacity.estimate.is_some() {
                        self.near_increase() * elapsed
                    } else {
                        let factor =
                            if self.last_change.is_some() { power_f64(INCREASE, elapsed.min(1.0)) } else { INCREASE };
                        (self.current * (factor - 1.0)).max(LEAST_MULTIPLICATIVE)
                    };
                    next = Some((self.current + increase).min(limit));
                }
                self.last_change = Some(now);
            }
            Control::Decrease => {
                let mut decreased = throughput * BETA;
                if decreased > DECREASE_LESS {
                    decreased -= DECREASE_LESS;
                }
                if let Some(estimate) = self.capacity.estimate.filter(|_| decreased > self.current) {
                    decreased = BETA * estimate * KILO;
                }
                if decreased < self.current {
                    next = Some(decreased);
                }
                if throughput < self.capacity.lower() {
                    self.capacity.estimate = None;
                }
                self.capacity.note_overuse(throughput);
                self.control = Control::Hold;
                self.last_change = Some(now);
            }
        }
        self.current = next.unwrap_or(self.current).max(self.least);
    }

    // aimd_rate_control.cc, `GetNearMaxIncreaseRateBpsPerSecond`: a packet's share of a frame at 30 a second each response time, in bits a second each second.
    fn near_increase(&self) -> f64 {
        let frame = (self.current as u64) / FRAMES_PER_SECOND;
        let packets = frame.div_ceil(PACKET_BITS).max(1);
        let packet = frame as f64 / packets as f64;
        let response = ((self.round_trip + RESPONSE_MORE) * 2).as_secs_f64();
        (packet / response).max(LEAST_ADDITIVE)
    }
}

impl Throughput {
    // Takes packets acknowledged in the order of their arrivals (robust_throughput_estimator.cc, `IncomingPacketFeedbackVector`).
    fn take(&mut self, packets: &[Acked]) {
        for &packet in packets {
            self.window.push_back(packet);
            let mut at = self.window.len() - 1;
            while at > 0 && self.window[at].arrival < self.window[at - 1].arrival {
                self.window.swap(at, at - 1);
                at -= 1;
            }
            if self.window.back().is_some_and(|newest| newest.arrival - packet.arrival > micros(MOST_REORDERING)) {
                (self.window, self.discarded) = (VecDeque::new(), None);
            }
        }
        while self.is_first_outside() {
            if let Some(first) = self.window.pop_front() {
                self.discarded = Some(self.discarded.map_or(first.sent, |discarded| discarded.max(first.sent)));
            }
        }
    }

    fn is_first_outside(&self) -> bool {
        let (Some(first), Some(last)) = (self.window.front(), self.window.back()) else {
            return false;
        };
        let span = last.arrival - first.arrival;
        self.window.len() > MOST_WINDOW_PACKETS
            || span > micros(MOST_WINDOW)
            || self.window.len() > WINDOW_PACKETS && span > micros(LEAST_WINDOW)
    }

    // In bits a second: the lower of the rates the packets were sent and received at, the largest gap between arrivals taken for the second largest (robust_throughput_estimator.cc, `bitrate`).
    fn bitrate(&self) -> Option<f64> {
        if self.window.len() < REQUIRED_PACKETS {
            return None;
        }
        let (mut largest, mut second) = (0, 0);
        for (before, after) in self.window.iter().zip(self.window.iter().skip(1)) {
            let gap = after.arrival - before.arrival;
            if gap > largest {
                (second, largest) = (largest, gap);
            } else if gap > second {
                second = gap;
            }
        }
        let (mut first_arrival, mut last_arrival, mut received, mut first_bytes) = (i64::MAX, i64::MIN, 0, 0);
        let (mut first_sent, mut last_sent, mut sent, mut last_bytes, mut counted) = (i64::MAX, i64::MIN, 0, 0, 0);
        for packet in &self.window {
            if packet.arrival < first_arrival {
                (first_arrival, first_bytes) = (packet.arrival, packet.bytes);
            }
            last_arrival = last_arrival.max(packet.arrival);
            received += packet.bytes;
            if self.discarded.is_some_and(|discarded| packet.sent < discarded) {
                continue;
            }
            if packet.sent > last_sent {
                (last_sent, last_bytes) = (packet.sent, packet.bytes);
            }
            first_sent = first_sent.min(packet.sent);
            sent += packet.bytes;
            counted += 1;
        }
        let rate = |bytes: usize, span: i64| {
            f64::from(u8::BITS) * bytes as f64 * MICROS_PER_SECOND as f64 / span.max(micros(LEAST_SPAN)) as f64
        };
        let receive_rate = rate(received - first_bytes, last_arrival - first_arrival - largest + second);
        if counted < REQUIRED_PACKETS {
            return Some(receive_rate);
        }
        Some(rate(sent - last_bytes, last_sent - first_sent).min(receive_rate))
    }
}

impl Loss {
    fn is_in_start_phase(&self, now: Duration) -> bool {
        self.first_report.is_none_or(|first| now.saturating_sub(first) < START_PHASE)
    }

    // send_side_bandwidth_estimation.cc, `RttBasedBackoff::IsRttAboveLimit`: the round trip of the last feedback and the time packets went since it.
    fn is_round_trip_long(&self) -> bool {
        let Some((at, round_trip)) = self.propagation else {
            return false;
        };
        round_trip + self.last_sent.map_or(Duration::ZERO, |sent| sent.saturating_sub(at)) > BACKOFF_ROUND_TRIP
    }

    // send_side_bandwidth_estimation.cc, `UpdatePacketsLost`.
    fn note_lost(&mut self, lost: u64, total: u64, now: Duration) {
        self.first_report.get_or_insert(now);
        if total == 0 {
            return;
        }
        let expected = self.expected + total;
        if expected < LEAST_PACKETS {
            (self.expected, self.lost) = (expected, self.lost + lost);
            return;
        }
        self.has_decreased = false;
        self.fraction = ((self.lost + lost) * FRACTION_ONE / expected).min(FRACTION_ONE - 1);
        (self.lost, self.expected) = (0, 0);
        self.last_report = Some(now);
        self.update(now);
    }

    // send_side_bandwidth_estimation.cc, `UpdateEstimate`.
    fn update(&mut self, now: Duration) {
        if self.is_round_trip_long() {
            let is_due = self.last_decrease.is_none_or(|last| now.saturating_sub(last) >= BACKOFF_INTERVAL);
            if is_due && self.current > BACKOFF_FLOOR {
                self.last_decrease = Some(now);
                self.set((self.current * BACKOFF_FRACTION).max(BACKOFF_FLOOR));
                return;
            }
            self.set(self.current);
            return;
        }
        if self.fraction == 0 && self.is_in_start_phase(now) {
            let mut next = self.current;
            if self.receiver_limit.is_finite() {
                next = next.max(self.receiver_limit);
            }
            if self.delay_limit.is_finite() {
                next = next.max(self.delay_limit);
            }
            if next != self.current {
                self.history.clear();
                self.history.push_back((now, self.current));
                self.set(next);
                return;
            }
        }
        while self.history.front().is_some_and(|(at, _)| now.saturating_sub(*at) + HISTORY_SLACK > INCREASE_INTERVAL) {
            self.history.pop_front();
        }
        while self.history.back().is_some_and(|(_, rate)| self.current <= *rate) {
            self.history.pop_back();
        }
        self.history.push_back((now, self.current));
        let Some(report) = self.last_report else {
            self.set(self.current);
            return;
        };
        if now.saturating_sub(report).as_secs_f64() < REPORT_AGE * REPORT_INTERVAL.as_secs_f64() {
            let loss = self.fraction as f64 / FRACTION_ONE as f64;
            if loss <= LOW_LOSS {
                let least = self.history.front().map_or(self.current, |(_, rate)| *rate);
                self.set(least * LOSS_INCREASE + LOSS_INCREASE_MORE);
                return;
            }
            let is_due =
                self.last_decrease.is_none_or(|last| now.saturating_sub(last) >= DECREASE_INTERVAL + self.round_trip);
            if loss > HIGH_LOSS && !self.has_decreased && is_due {
                self.last_decrease = Some(now);
                self.has_decreased = true;
                self.set(self.current * (2 * FRACTION_ONE - self.fraction) as f64 / (2 * FRACTION_ONE) as f64);
                return;
            }
        }
        self.set(self.current);
    }

    // send_side_bandwidth_estimation.cc, `UpdateTargetBitrate`: under the delay-based estimate and the most, over the least.
    fn set(&mut self, rate: f64) {
        self.current = rate.min(self.delay_limit.min(self.most)).max(self.least);
    }
}

impl Estimator {
    pub(super) fn new(bounds: Bounds) -> Estimator {
        let least = bounds.least.max(LEAST_RATE) as f64;
        let most = (bounds.most as f64).max(least);
        let start = (bounds.start as f64).clamp(least, most);
        Estimator {
            groups: Groups::default(),
            trend: Trend::default(),
            aimd: Aimd {
                least,
                current: start,
                throughput: start,
                capacity: Capacity { estimate: None, deviation: LEAST_DEVIATION },
                control: Control::Hold,
                last_change: None,
                round_trip: DEFAULT_ROUND_TRIP,
            },
            throughput: Throughput::default(),
            loss: Loss {
                least,
                most,
                current: start,
                delay_limit: f64::INFINITY,
                receiver_limit: f64::INFINITY,
                lost: 0,
                expected: 0,
                fraction: 0,
                first_report: None,
                last_report: None,
                has_decreased: false,
                last_decrease: None,
                history: VecDeque::new(),
                round_trip: Duration::ZERO,
                propagation: None,
                last_sent: None,
            },
            last_feedback: None,
        }
    }

    // Takes the least and the most rate anew, as a call's data mode changes them (send_side_bandwidth_estimation.cc, `SetMinMaxBitrate`).
    pub(super) fn set_bounds(&mut self, least: u64, most: u64) {
        let least = least.max(LEAST_RATE) as f64;
        (self.loss.least, self.loss.most, self.aimd.least) = (least, (most as f64).max(least), least);
        self.aimd.current = self.aimd.current.max(least);
        self.loss.set(self.loss.current);
    }

    // Takes the outcomes of a feedback message received now (goog_cc_network_control.cc, `OnTransportPacketsFeedback`; delay_based_bwe.cc, `IncomingPacketFeedbackVector`).
    pub(crate) fn take(&mut self, outcomes: &[Outcome], now: Duration) {
        if outcomes.is_empty() {
            return;
        }
        let mut received: Vec<Acked> = outcomes
            .iter()
            .filter_map(|outcome| {
                Some(Acked { sent: micros(outcome.sent), arrival: outcome.arrival?, bytes: outcome.bytes })
            })
            .collect();
        received.sort_by_key(|packet| packet.arrival);
        if let Some(latest) = received.last().map(|packet| packet.arrival) {
            let propagation = received.iter().map(|packet| micros(now) - packet.sent - (latest - packet.arrival)).min();
            let propagation = u64::try_from(propagation.unwrap_or(0)).unwrap_or(0);
            self.loss.propagation = Some((now, Duration::from_micros(propagation)));
        }
        self.throughput.take(&received);
        if self.last_feedback.is_none_or(|last| now.saturating_sub(last) > STREAM_TIMEOUT) {
            (self.groups, self.trend) = (Groups::default(), Trend::default());
        }
        self.last_feedback = Some(now);
        for packet in &received {
            if let Some((send_delta, arrival_delta)) = self.groups.take(packet.sent, packet.arrival, micros(now)) {
                self.trend.update(arrival_delta, send_delta, packet.arrival as f64 / MICROS_PER_MILLI);
            }
        }
        let throughput = self.throughput.bitrate();
        if self.trend.usage == Usage::Over {
            match throughput {
                Some(throughput) if self.aimd.is_time_to_reduce(now, throughput) => {
                    self.aimd.update(Usage::Over, Some(throughput), now);
                }
                None if self.aimd.is_time_to_reduce(now, self.aimd.current * REDUCE_FRACTION - UNDER_HALF) => {
                    self.aimd.current = (self.aimd.current * REDUCE_FRACTION).max(self.aimd.least);
                    self.aimd.last_change = Some(now);
                }
                _ => {}
            }
        } else {
            self.aimd.update(self.trend.usage, throughput, now);
        }
        self.loss.delay_limit = self.aimd.current;
        self.loss.set(self.loss.current);
        let lost = outcomes.iter().filter(|outcome| outcome.arrival.is_none()).count() as u64;
        self.loss.note_lost(lost, outcomes.len() as u64, now);
    }

    // Takes a round trip a report block told (goog_cc_network_control.cc, `OnRoundTripTimeUpdate`).
    pub(super) fn note_round_trip(&mut self, round_trip: Duration) {
        if !round_trip.is_zero() {
            (self.aimd.round_trip, self.loss.round_trip) = (round_trip, round_trip);
        }
    }

    // Takes a packet sent with a transport-wide number, which feedback is to come back for (goog_cc_network_control.cc, `OnSentPacket`).
    pub(super) fn note_sent(&mut self, now: Duration) {
        self.loss.last_sent = Some(now);
        self.loss.propagation.get_or_insert((now, Duration::ZERO));
    }

    // Takes the most a receiver's REMB allows, which caps the target (send_side_bandwidth_estimation.cc, `UpdateReceiverEstimate`).
    pub(super) fn limit(&mut self, bitrate: u64) {
        self.loss.receiver_limit = if bitrate == 0 { f64::INFINITY } else { bitrate as f64 };
    }

    // Runs the loss-based control on the clock, as goog_cc does at each process interval (`OnProcessInterval`): it grows the rate toward the delay-based estimate and backs off where feedback stops.
    pub(super) fn tick(&mut self, now: Duration) {
        self.loss.update(now);
    }

    // The bits a second this side may send now.
    pub(super) fn target(&self) -> u64 {
        self.loss.current.min(self.loss.receiver_limit).max(self.loss.least) as u64
    }

    // The bits a second the pacer sends at.
    pub(super) fn pacing_rate(&self) -> u64 {
        let factor = if self.last_feedback.is_some() { PACE } else { PACE_FIRST };
        (self.target() as f64 * factor) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::xorshift64;

    // A link of a bottleneck's capacity, its queue cut at 500 ms, 50 ms each way, and the losses a seed draws; a sender of packets of 1200 bytes at the target, feedback each 100 ms, the round trip each second, the clock each 25 ms.
    const PACKET_BYTES: usize = 1200;
    const DELAY: Duration = Duration::from_millis(50);
    const QUEUE: Duration = Duration::from_millis(500);
    const FEEDBACK: Duration = Duration::from_millis(100);
    const TICK: Duration = Duration::from_millis(25);
    const REPORTS: Duration = Duration::from_secs(1);
    const STEP: Duration = Duration::from_millis(1);
    const SAMPLE: Duration = Duration::from_millis(100);
    const RECEIVER_CLOCK: i64 = 7_000_000_000;
    const PER_MILLE: u64 = 1000;
    const BOUNDS: Bounds = Bounds { least: 30_000, start: 300_000, most: 2_000_000 };

    struct Link<'a> {
        capacity: &'a dyn Fn(Duration) -> u64,
        loss_per_mille: u64,
        is_feedback_lost: &'a dyn Fn(Duration) -> bool,
    }

    // The target each 100 ms and the time each packet waited in the queue.
    struct Run {
        targets: Vec<(Duration, u64)>,
        waits: Vec<(Duration, Duration)>,
    }

    impl Run {
        fn mean(&self, from: u64, to: u64) -> u64 {
            let range = Duration::from_secs(from)..Duration::from_secs(to);
            let taken: Vec<u64> =
                self.targets.iter().filter(|(at, _)| range.contains(at)).map(|(_, target)| *target).collect();
            taken.iter().sum::<u64>() / taken.len() as u64
        }

        fn at(&self, second: f64) -> u64 {
            let time = Duration::from_secs_f64(second);
            self.targets.iter().find(|(at, _)| *at >= time).map_or(0, |(_, target)| *target)
        }

        fn most_wait(&self, from: u64, to: u64) -> Duration {
            let range = Duration::from_secs(from)..Duration::from_secs(to);
            self.waits.iter().filter(|(at, _)| range.contains(at)).map(|(_, wait)| *wait).max().unwrap_or_default()
        }
    }

    fn simulate(link: &Link, start: u64, seconds: u64, seed: u64) -> Run {
        let mut estimator = Estimator::new(Bounds { start, ..BOUNDS });
        let (mut state, mut free_at, mut credit) = (seed, Duration::ZERO, 0.0);
        let mut pending: VecDeque<(Duration, Outcome)> = VecDeque::new();
        let mut run = Run { targets: Vec::new(), waits: Vec::new() };
        let (mut feedback, mut tick, mut report, mut sample) = (FEEDBACK, TICK, REPORTS, Duration::ZERO);
        let mut wait = Duration::ZERO;
        let mut now = Duration::ZERO;
        while now < Duration::from_secs(seconds) {
            credit += estimator.target() as f64 * STEP.as_secs_f64();
            let bits = (PACKET_BYTES * 8) as f64;
            while credit >= bits {
                credit -= bits;
                estimator.note_sent(now);
                let begin = free_at.max(now);
                wait = begin - now;
                state = xorshift64(state);
                let is_lost = wait > QUEUE || state % PER_MILLE < link.loss_per_mille;
                let done = begin + Duration::from_secs_f64(bits / (link.capacity)(now) as f64);
                if wait <= QUEUE {
                    free_at = done;
                }
                run.waits.push((now, wait));
                let arrival = (!is_lost).then(|| micros(done + DELAY) + RECEIVER_CLOCK);
                pending.push_back((done + DELAY, Outcome { sent: now, bytes: PACKET_BYTES, arrival }));
            }
            if now >= feedback {
                feedback += FEEDBACK;
                let mut outcomes = Vec::new();
                while let Some((_, outcome)) = pending.pop_front_if(|(at, _)| *at + DELAY <= now) {
                    outcomes.push(outcome);
                }
                if !(link.is_feedback_lost)(now) {
                    estimator.take(&outcomes, now);
                }
            }
            if now >= tick {
                tick += TICK;
                estimator.tick(now);
            }
            if now >= report {
                report += REPORTS;
                estimator.note_round_trip(DELAY * 2 + wait);
            }
            if now >= sample {
                sample += SAMPLE;
                run.targets.push((now, estimator.target()));
            }
            now += STEP;
        }
        run
    }

    fn steady(_: Duration) -> u64 {
        1_000_000
    }

    fn never(_: Duration) -> bool {
        false
    }

    #[test]
    fn fixed_capacity() {
        let run = simulate(&Link { capacity: &steady, loss_per_mille: 0, is_feedback_lost: &never }, 300_000, 60, 1);
        let (mean, wait) = (run.mean(30, 60), run.most_wait(30, 60));
        assert!((700_000..=1_100_000).contains(&mean), "a link of 1 Mbit/s settles near its capacity: {mean}");
        assert!(wait < Duration::from_millis(250), "the queue stays short once settled: {wait:?}");
        assert!(run.at(1.0) <= 1_100_000, "it starts from 300 kbit/s and does not leap past the link");
    }

    #[test]
    fn drop_and_recovery() {
        let capacity = |at: Duration| if (20..40).contains(&at.as_secs()) { 500_000 } else { 2_000_000 };
        let run = simulate(&Link { capacity: &capacity, loss_per_mille: 0, is_feedback_lost: &never }, 300_000, 90, 2);
        let (before, after_drop, during, recovered) = (run.at(19.9), run.at(22.0), run.mean(25, 40), run.mean(80, 90));
        assert!(before >= 1_200_000, "from 300 kbit/s it climbs 8 % a second toward 2 Mbit/s: {before}");
        assert!(after_drop <= 650_000, "two seconds after the drop it is near the new capacity: {after_drop}");
        assert!((300_000..=600_000).contains(&during), "it holds near 500 kbit/s: {during}");
        assert!(recovered >= 1_200_000, "it climbs again once the capacity is back: {recovered}");
    }

    #[test]
    fn random_loss() {
        let link = |loss_per_mille| Link { capacity: &|_| 1_500_000, loss_per_mille, is_feedback_lost: &never };
        let light = simulate(&link(10), 300_000, 60, 3).mean(30, 60);
        let heavy = simulate(&link(150), 300_000, 60, 4).mean(30, 60);
        assert!(light >= 900_000, "1 % loss is under 2 %, which lets the rate grow: {light}");
        assert!(heavy <= 100_000, "15 % loss, over 10 %, halves its fraction off each time: {heavy}");
    }

    #[test]
    fn building_queue() {
        let run = simulate(&Link { capacity: &steady, loss_per_mille: 0, is_feedback_lost: &never }, 2_000_000, 30, 5);
        let (early, later) = (run.at(3.0), run.most_wait(10, 30));
        assert!(early <= 1_000_000, "a queue that builds is seen and the rate brought under the capacity: {early}");
        assert!(later < Duration::from_millis(250), "the queue drains and stays short: {later:?}");
    }

    #[test]
    fn feedback_lost() {
        let lost = |at: Duration| (20..30).contains(&at.as_secs());
        let run = simulate(&Link { capacity: &steady, loss_per_mille: 0, is_feedback_lost: &lost }, 300_000, 60, 6);
        let (before, last, after) = (run.at(20.0), run.at(29.9), run.mean(50, 60));
        assert!(last * 3 <= before, "no feedback for more than 3 s: 0.8 of the rate each second: {before} to {last}");
        assert!(last >= BOUNDS.least, "never under the least");
        assert!(after >= 600_000, "the rate grows again once feedback comes: {after}");
    }

    #[test]
    fn acknowledged_rate() {
        let mut throughput = Throughput::default();
        let packets: Vec<Acked> =
            (0..20).map(|at| Acked { sent: at * 10_000, arrival: 5_000_000 + at * 10_000, bytes: 1000 }).collect();
        throughput.take(&packets[..9]);
        assert_eq!(throughput.bitrate(), None, "nine packets are too few");
        throughput.take(&packets[9..]);
        assert_eq!(throughput.bitrate(), Some(800_000.0), "1000 bytes each 10 ms, the first's bytes left out");
        let late = Acked { sent: 0, arrival: 5_000_000 - 2_000_000, bytes: 1000 };
        throughput.take(&[late]);
        assert_eq!(throughput.bitrate(), None, "a packet two seconds behind the newest begins anew");
    }

    #[test]
    fn pacing_factor() {
        let mut estimator = Estimator::new(BOUNDS);
        assert_eq!(estimator.pacing_rate(), 750_000, "2.5 times the start until feedback comes");
        let outcome = Outcome { sent: Duration::ZERO, bytes: 1200, arrival: Some(50_000) };
        estimator.take(&[outcome], Duration::from_millis(100));
        assert_eq!(estimator.pacing_rate(), estimator.target() * 11 / 10, "1.1 times the target from then on");
        estimator.set_bounds(400_000, 500_000);
        assert_eq!(estimator.target(), 400_000, "new bounds hold the target");
        estimator.limit(350_000);
        assert_eq!(estimator.target(), 400_000, "a REMB under the least is held to the least");
    }
}

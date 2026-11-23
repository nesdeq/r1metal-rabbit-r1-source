use super::card::{self, Direction};
use super::{FULL_SCALE, decibels};
use crate::base::fft::{Complex, Real, rotate};
use crate::base::math::{dot, sine_cosine, sqrt, to_sample};
use crate::codec::aac::RATE;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

// Samples the filter and the suppressor advance by: a quarter of the card's block, 5.3 ms.
const HOP: usize = card::PERIOD / 4;
// Samples of a transform: a hop and the one before it.
const SIZE: usize = 2 * HOP;
const BINS: usize = HOP + 1;
// The top of wideband speech, all that a call sends.
const WIDEBAND: usize = 8000;
// The bins the suppressor lets through, 94 Hz to 7.97 kHz: above nought and under the top of wideband.
const LOW: usize = 1;
const HIGH: usize = WIDEBAND * SIZE / RATE as usize;
const KEPT: usize = HIGH - LOW + 1;
// The most a stamp lies off the card's clock: a tick of the kernel's scheduler at its 250 Hz, 4 ms.
const STAMP_ERROR: u64 = RATE as u64 / card::KERNEL_HZ as u64;
// How far apart two stamps' errors may lie: of the speaker's against the microphone's, or of a stream's against its own count.
const SKEW: u64 = 2 * STAMP_ERROR;
// The echo of the case and of the hand and face about it, 16 ms, assumed: a measured impulse response settles it.
const CASE: usize = RATE as usize * 16 / 1000;
// The filter's partitions of a hop each (Soo and Pang, 1990: the multidelay block frequency domain filter): the skew either way and the case's echo behind it.
const PARTITIONS: usize = (2 * SKEW as usize + CASE).div_ceil(HOP);
// The far side's samples kept: what a call's speaker holds ahead of what is heard and a block more, what the microphone holds behind it, and the filter's reach moved to the farthest lag the search takes.
pub(super) const RING: usize = (card::CALL_BUFFER
    + card::PERIOD
    + card::buffer_samples(Direction::In)
    + SKEW as usize
    + MOST_LAG
    + (PARTITIONS + 1) * HOP)
    .next_power_of_two();
// The microphone's noise floor at the 24 dB of preamp it records with, -67.4 dBFS as measured on the card, as a power of full scale.
const NOISE: f32 = 1.82e-7;
// The echo's share of the played power: -15 dB measured on the card at 12 dB of preamp, 12 dB more at the 24 dB it records with.
const COUPLING: f32 = 0.501;
// The least share of the echo a linear filter leaves: the speaker's harmonics, 50 dB down as measured on the card.
const MIN_LEAK: f32 = 1.0e-5;
// A bin's step is a quarter of the residual echo's share of its error, the step Valin (2007) takes whole: near sound then moves the filter by a quarter of the echo it leaves, 1 dB, within the 3 dB it may move while both talk.
const STEP_SHARE: f32 = 0.25;
// The step is at most a half, so that an estimate too high cannot step past the filter's aim.
const MAX_STEP: f32 = 0.5;
// The leak's averages follow the far side's speech within 2 s at the most, as Speex's do, so that near speech, which no echo follows, cannot move them while both talk.
const LEAK_RATE: f32 = HOP as f32 / (2.0 * RATE as f32);
// The bins' mean powers, from which the leak's averages take their changes, follow within a tenth of a second, a syllable's part.
const AVERAGE: f32 = HOP as f32 / (0.1 * RATE as f32);
// The suppressor's means of the echo left follow the far side's sound within half a second: tuned by the simulation.
const LEFT_RATE: f32 = HOP as f32 / (0.5 * RATE as f32);
// The step follows the coupling until the estimate stands 10 dB over the error, from where the leak's regression reads the echo left: tuned by the simulation.
const ADAPTED: f32 = 10.0;
// The two filters' errors are compared over about 16 hops, 85 ms, longer than a voiced sound holds still, so that a filter that fits near speech does not show less error by chance.
const COMPARING: f32 = 0.9375;
// An estimate of more than a hop at full scale 16 times over, 12 dB, is no echo a microphone records: the filter went wrong.
const DIVERGED: f32 = 16.0;
// The power a bin's far sound has over the filter's reach whose echo stands at the noise floor: what keeps a step from growing on silence.
const REGULAR: f32 = PARTITIONS as f32 * SIZE as f32 * NOISE / COUPLING;
// The suppressor takes the echo left as eight times, 9 dB over, its estimate: the least that keeps the simulated far side's echo under the noise floor over all its seeds.
const OVER: f32 = 8.0;
// Held powers, the suppressor's echo left and the error the step takes, die away by no more than half a hop, 3 dB in 5.3 ms: what the simulated echo left after a loud hop needs.
const HOLD: f32 = 0.5;
// The suppressor's smoothing of a bin's share of near sound (Ephraim and Malah, 1984): their 0.98, a hop here, which the simulation keeps against faster values that let the far side's echo through.
const SMOOTHING: f32 = 0.98;
// The least gain of the suppressor, 40 dB down, as Speex's echo suppression has it.
const FLOOR: f32 = 0.01;
// The search for the echo's lag behind what the speaker played runs at 6 kHz, an eighth of the card's rate: the largest factor of a hop that keeps it at AEC3's 4 kHz or over (echo_canceller3_config.h, `down_sampling_factor` 4 at 16 kHz).
const DOWN: usize = 8;
// What the search keeps of each side at 6 kHz, 341 ms: the microphone's newer half is correlated with the far side's whole.
const SPAN: usize = 2048;
const NEAR_SPAN: usize = SPAN / 2;
// A search each time half the span has come anew, 171 ms.
const SEARCH_HOPS: usize = NEAR_SPAN * DOWN / HOP;
// Each search's correlation weighs a third in the mean the lag is read from: tuned against `lag_beyond_stamps_found`.
const SEARCH_WEIGHT: f32 = 1.0 / 3.0;
// A lag stands where the mean correlation peaks at four times its mean size and more: tuned against `lag_beyond_stamps_found`.
const PEAK: f32 = 4.0;
// Both frames taper over a hop at each end, and no lag within a hop of either end of the range is taken, where the frames' ends make peaks of their own: tuned against `periodic_far_side_lag`.
const EDGE: usize = HOP / DOWN;
// A search refuses where a second peak, more than a hop from the first, clears `PEAK` and half the first, as a far side whose period is longer than the skew has one: tuned against `periodic_far_side_lag`.
const RIVAL: f32 = 0.5;
// The farthest lag the search takes behind the far side it reads, in samples of the card.
const MOST_LAG: usize = (NEAR_SPAN - EDGE) * DOWN;
// A lag is taken once five searches in a row put it within a hop, as AEC3 takes a delay its histogram holds five times (echo_canceller3_config.h, `delay_selection_thresholds`).
const AGREEING: usize = 5;
// A hop counts as converged where the filter's error stands 3 dB under a microphone that holds 50 of 32768 a sample and more (AEC3, subtractor_output_analyzer.cc, `kConvergenceThreshold`).
const CONVERGED_SHARE: f32 = 0.5;
const CONVERGED_LEVEL: f32 = 50.0 * 50.0 * HOP as f32 / (FULL_SCALE * FULL_SCALE);

// How far `at` lies from `from` on the card's clock, either way.
fn distance(at: u64, from: u64) -> u64 {
    at.wrapping_sub(from).min(from.wrapping_sub(at))
}

// Where a stream's block stands: where its count says while its stamp agrees, else at its stamp; and whether it stood elsewhere.
fn place(next: Option<u64>, at: u64) -> (u64, bool) {
    match next {
        Some(next) if distance(at, next) <= SKEW => (next, false),
        _ => (at, next.is_some()),
    }
}

fn power(value: Complex) -> f32 {
    value[0] * value[0] + value[1] * value[1]
}

// Adds to each sum its point turned by its turn, or by the turn's conjugate where `sign` is -1: a partition's products of spectra, bin by bin.
fn add_products(sums: &mut [Complex], points: &[Complex], turns: &[Complex], sign: f32) {
    for ((sum, &point), &turn) in sums.iter_mut().zip(points).zip(turns) {
        let product = rotate(point, turn, sign);
        *sum = [sum[0] + product[0], sum[1] + product[1]];
    }
}

// Tapers a frame's ends by `ramp`'s rise and its fall, so that what the frame cuts off spreads over no other band.
fn taper(frame: &mut [f32], ramp: &[f32]) {
    let last = frame.len() - 1;
    for (at, &weight) in ramp.iter().enumerate() {
        frame[at] *= weight;
        frame[last - at] *= weight;
    }
}

// The far side by the card's time: what the speaker played from `start` to `end`, silence elsewhere.
struct Far {
    samples: Vec<f32>,
    start: u64,
    end: u64,
    // Where the speaker's next block goes by its own count.
    next: Option<u64>,
}

impl Far {
    // Takes a block played from `at` on; true where the speaker's count started anew.
    fn take(&mut self, block: &[i16], at: u64) -> bool {
        let (start, is_moved) = place(self.next, at);
        self.next = Some(start.wrapping_add(block.len() as u64));
        if start.wrapping_sub(self.start) < self.end.wrapping_sub(self.start) {
            // What was taken past the start of a block that begins before the last ended is played no more.
            self.end = start;
        } else {
            let gap = start.wrapping_sub(self.end);
            if gap >= RING as u64 {
                (self.start, self.end) = (start, start);
            } else {
                (0..gap).for_each(|_| self.push(0.0));
            }
        }
        for &sample in block {
            self.push(f32::from(sample) / FULL_SCALE);
        }
        is_moved
    }

    fn push(&mut self, sample: f32) {
        self.samples[self.end as usize % RING] = sample;
        self.end = self.end.wrapping_add(1);
        if self.end.wrapping_sub(self.start) > RING as u64 {
            self.start = self.end.wrapping_sub(RING as u64);
        }
    }

    fn read(&self, from: u64, out: &mut [f32]) {
        let length = self.end.wrapping_sub(self.start);
        for (offset, sample) in out.iter_mut().enumerate() {
            let at = from.wrapping_add(offset as u64);
            *sample = if at.wrapping_sub(self.start) < length { self.samples[at as usize % RING] } else { 0.0 };
        }
    }
}

// The adaptive filter: what the far side's sound becomes at the microphone, partition by partition, bin by bin; in every bin, so that a partition's taps can stay within its hop.
struct Filter {
    // The far side's spectra over the filter's reach, the newest in slot `newest`; the filter's, partition by partition.
    spectra: Vec<Complex>,
    newest: usize,
    weights: Vec<Complex>,
    // Two echo path models (Ochiai, Araseki and Ogihara, 1977): `weights` learn, the foreground's estimate is taken out, and each takes the other's weights where its error is shown worse.
    foreground: Vec<Complex>,
    // How much less the learning filter's error is than the foreground's over the last hops, and that difference's variance by chance.
    gain: f32,
    spread: f32,
    far_power: Vec<f32>,
    estimate_power: Vec<f32>,
    error_bins: Vec<Complex>,
    error_power: Vec<f32>,
    // Each bin's error power as the step takes it, dying away no faster than `HOLD` a hop, so that near sound's dips between its harmonics make no large step.
    held_error: Vec<f32>,
    steps: Vec<f32>,
    mean_estimate: Vec<f32>,
    mean_error: Vec<f32>,
    // The leak: how much of the estimate's power stays in the error (Valin, 2007), and the averages it is the ratio of.
    leak: f32,
    cross: f32,
    square: f32,
    // The share of this hop's error the leak takes for echo: nought without far sound, near one where nothing else sounds.
    echo_share: f32,
    // The estimate's power and the error's over hops of far sound; until the one is `ADAPTED` times the other, the step follows the coupling.
    heard_estimate: f32,
    heard_error: f32,
    is_adapted: bool,
    constrained: usize,
}

impl Filter {
    fn new() -> Filter {
        Filter {
            spectra: vec![[0.0; 2]; PARTITIONS * BINS],
            newest: 0,
            weights: vec![[0.0; 2]; PARTITIONS * BINS],
            foreground: vec![[0.0; 2]; PARTITIONS * BINS],
            gain: 0.0,
            spread: 0.0,
            far_power: vec![0.0; BINS],
            estimate_power: vec![0.0; BINS],
            error_bins: vec![[0.0; 2]; BINS],
            error_power: vec![0.0; BINS],
            held_error: vec![0.0; BINS],
            steps: vec![0.0; BINS],
            mean_estimate: vec![0.0; BINS],
            mean_error: vec![0.0; BINS],
            leak: 1.0,
            cross: 0.0,
            square: 0.0,
            echo_share: 0.0,
            heard_estimate: 0.0,
            heard_error: 0.0,
            is_adapted: false,
            constrained: 0,
        }
    }

    // Where the far side's spectrum of a partition starts.
    const fn slot(&self, partition: usize) -> usize {
        (self.newest + partition) % PARTITIONS * BINS
    }

    // Starts the step again from the coupling, as the streams' alignment may have changed; the weights stay.
    const fn unlearn(&mut self) {
        (self.leak, self.cross, self.square, self.is_adapted) = (1.0, 0.0, 0.0, false);
        (self.heard_estimate, self.heard_error) = (0.0, 0.0);
    }
}

// The residual echo suppressor: a gain for each bin of the error, in frames of two hops under a root of a Hann window.
struct Suppressor {
    window: Vec<f32>,
    gains: Vec<f32>,
    // A bin's power of near sound after the last frame's gain, for the next frame's share (Ephraim and Malah, 1984).
    clean: Vec<f32>,
    overlap: Vec<f32>,
    error_bins: Vec<Complex>,
    estimate_bins: Vec<Complex>,
    shaped: Vec<f32>,
    // Where the echo the filter leaves falls, the speaker's distortion with it: each bin's mean error power, its estimate's and the whole estimate's, over hops as far as their error is echo.
    mean_error: Vec<f32>,
    mean_estimate: Vec<f32>,
    mean_total: f32,
    // Each bin's echo left as the last frame took it, which dies away no faster than `HOLD` a hop.
    held: Vec<f32>,
}

impl Suppressor {
    fn new() -> Suppressor {
        Suppressor {
            window: (0..SIZE).map(|at| sine_cosine((at as f64 + 0.5) / (2 * SIZE) as f64).0 as f32).collect(),
            gains: vec![1.0; KEPT],
            clean: vec![0.0; KEPT],
            overlap: vec![0.0; HOP],
            error_bins: vec![[0.0; 2]; KEPT],
            estimate_bins: vec![[0.0; 2]; KEPT],
            shaped: vec![0.0; HOP],
            mean_error: vec![0.0; KEPT],
            mean_estimate: vec![0.0; KEPT],
            mean_total: 0.0,
            held: vec![0.0; KEPT],
        }
    }
}

// The search for the echo's lag behind what the speaker played, as AEC3's delay estimator finds it in outline (echo_path_delay_estimator.cc, matched_filter_lag_aggregator.cc), so that the filter's reach stands where the echo is whatever the card's paths add to the stamps: both sides at 6 kHz, their correlation by the phase transform (Knapp and Carter, 1976) over the last 341 ms, its mean over the searches with far sound, and the lag at its peak once searches agree and no rival stands beside it.
struct Lag {
    fft: Real,
    // Each side at 6 kHz, the oldest first; the far side read a skew ahead of the microphone's stamps, so that a lag of nought is an echo a skew early.
    far: Vec<f32>,
    near: Vec<f32>,
    hops: usize,
    frame: Vec<f32>,
    // The rise of a raised cosine over a frame's first samples, and its fall over the last.
    ramp: Vec<f32>,
    far_bins: Vec<Complex>,
    near_bins: Vec<Complex>,
    mean: Vec<Complex>,
    // The lags the last searches found, the newest at `next`, and the one taken.
    found: [Option<usize>; AGREEING],
    next: usize,
    taken: Option<usize>,
}

impl Lag {
    fn new() -> Lag {
        Lag {
            fft: Real::new(SPAN),
            far: vec![0.0; SPAN],
            near: vec![0.0; SPAN],
            hops: 0,
            frame: vec![0.0; SPAN],
            ramp: (0..EDGE)
                .map(|at| {
                    let sine = sine_cosine((at as f64 + 0.5) / (4 * EDGE) as f64).0 as f32;
                    sine * sine
                })
                .collect(),
            far_bins: vec![[0.0; 2]; SPAN / 2 + 1],
            near_bins: vec![[0.0; 2]; SPAN / 2 + 1],
            mean: vec![[0.0; 2]; SPAN / 2 + 1],
            found: [None; AGREEING],
            next: 0,
            taken: None,
        }
    }

    // Takes a hop of each side, and searches each time half the span is new.
    fn push(&mut self, near: &[f32], far: &[f32]) {
        let fresh = HOP / DOWN;
        for (history, hop) in [(&mut self.near, near), (&mut self.far, far)] {
            history.copy_within(fresh.., 0);
            for (to, part) in history[SPAN - fresh..].iter_mut().zip(hop.as_chunks::<DOWN>().0) {
                *to = part.iter().sum::<f32>() / DOWN as f32;
            }
        }
        self.hops += 1;
        if self.hops == SEARCH_HOPS {
            self.hops = 0;
            self.search();
        }
    }

    // Weighs this span's correlation into the mean where the far side sounds loud enough for its echo to stand over the microphone's noise, and reads the lag at the mean's peak where no rival stands beside it.
    fn search(&mut self) {
        let recent = &self.far[NEAR_SPAN..];
        if COUPLING * dot(recent, recent) < NEAR_SPAN as f32 * NOISE {
            return;
        }
        self.frame.copy_from_slice(&self.far);
        taper(&mut self.frame, &self.ramp);
        self.fft.forward(&self.frame, &mut self.far_bins);
        self.frame[..NEAR_SPAN].fill(0.0);
        self.frame[NEAR_SPAN..].copy_from_slice(&self.near[NEAR_SPAN..]);
        taper(&mut self.frame[NEAR_SPAN..], &self.ramp);
        self.fft.forward(&self.frame, &mut self.near_bins);
        for ((mean, &near), &far) in self.mean.iter_mut().zip(&self.near_bins).zip(&self.far_bins) {
            let cross = rotate(near, far, -1.0);
            let size = sqrt(power(cross));
            if size > 0.0 {
                for (part, value) in mean.iter_mut().zip(cross) {
                    *part += SEARCH_WEIGHT * (value / size - *part);
                }
            }
        }
        self.fft.inverse(&self.mean, &mut self.frame);
        let lags = &self.frame[EDGE..=NEAR_SPAN - EDGE];
        let size = lags.iter().map(|value| value.abs()).sum::<f32>() / lags.len() as f32;
        let peak = (0..lags.len()).fold(0, |best, lag| if lags[lag] > lags[best] { lag } else { best });
        let beside = (0..lags.len()).filter(|lag| lag.abs_diff(peak) > HOP / DOWN);
        let rival = beside.map(|lag| lags[lag]).fold(0.0, f32::max);
        let is_rivalled = rival > PEAK * size && rival > RIVAL * lags[peak];
        self.found[self.next] = (lags[peak] > PEAK * size && !is_rivalled).then_some(EDGE + peak);
        self.next = (self.next + 1) % AGREEING;
        let mut range = Some((usize::MAX, 0));
        for lag in self.found {
            range = range.zip(lag).map(|((low, high), lag)| (low.min(lag), high.max(lag)));
        }
        if let Some((low, high)) = range.filter(|(low, high)| high - low <= HOP / DOWN) {
            self.taken = Some((low + high) / 2);
        }
    }

    // The echo's lag behind the stamps in samples of the card, once taken.
    fn taken(&self) -> Option<i64> {
        self.taken.map(|lag| (lag * DOWN) as i64 - SKEW as i64)
    }
}

// What the canceller tells of itself in the call's log: whether it converged, the echo's lag behind the stamps it took, the share of hops of far sound whose error stood 3 dB under the microphone, the echo's level against the microphone's, the decibels the filter and the suppressor take off, the hops of far sound and the realignments.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(super) struct Figures {
    is_converged: bool,
    lag: Option<i64>,
    converged: u64,
    level: Option<f32>,
    filter: f32,
    suppressor: f32,
    far_hops: u64,
    pub(super) realignments: u64,
}

impl Figures {
    // The log line of what the canceller did.
    pub(super) fn to_line(self) -> String {
        let (filter, suppressor) = (self.filter, self.suppressor);
        let state = if self.is_converged { "converged" } else { "learning" };
        let lag = self
            .lag
            .map_or(String::from("no lag found"), |lag| format!("lag {:.1} ms", lag as f32 * 1000.0 / RATE as f32));
        let share = 100 * self.converged / self.far_hops.max(1);
        let level = self
            .level
            .map_or(String::from("no echo measured"), |level| format!("the echo {level:.0} dB against the microphone"));
        format!(
            "echo: {state}, {lag}, 3 dB under the microphone in {share} % of hops of far sound, {level}, {filter:.0} dB off by the filter and {suppressor:.0} dB by the suppressor, {} hops of far sound, {} realignments",
            self.far_hops, self.realignments
        )
    }
}

// An acoustic echo canceller for a call: takes what the speaker plays out of what the microphone records.
pub(super) struct Canceller {
    fft: Real,
    far: Far,
    filter: Filter,
    suppressor: Suppressor,
    near_next: Option<u64>,
    last_hop: Option<u64>,
    near: Vec<f32>,
    filled: usize,
    // The foreground's estimate and error of this hop and the last, and the learning filter's of this hop.
    estimate: Vec<f32>,
    last_estimate: Vec<f32>,
    error: Vec<f32>,
    last_error: Vec<f32>,
    learning_estimate: Vec<f32>,
    learning_error: Vec<f32>,
    frame: Vec<f32>,
    bins: Vec<Complex>,
    // Samples ready to be handed back, the oldest at `head`.
    out: Vec<i16>,
    head: usize,
    ready: usize,
    far_hops: u64,
    realignments: u64,
    // The share of the error's power the suppressor lets through, over hops of far sound.
    passed: f32,
    // The search for the echo's lag, the far side's hop it reads, and the samples the filter's reach stands behind the stamps by.
    lag: Lag,
    ahead: Vec<f32>,
    shift: i64,
    // Of the hops of far sound: those whose error stood 3 dB under the microphone, and the powers of the estimate and of the microphone summed over them.
    converged: u64,
    echo_power: f64,
    near_power: f64,
}

impl Default for Canceller {
    fn default() -> Canceller {
        Canceller {
            lag: Lag::new(),
            ahead: vec![0.0; HOP],
            shift: 0,
            converged: 0,
            echo_power: 0.0,
            near_power: 0.0,
            fft: Real::new(SIZE),
            far: Far { samples: vec![0.0; RING], start: 0, end: 0, next: None },
            filter: Filter::new(),
            suppressor: Suppressor::new(),
            near_next: None,
            last_hop: None,
            near: vec![0.0; HOP],
            filled: 0,
            estimate: vec![0.0; HOP],
            last_estimate: vec![0.0; HOP],
            error: vec![0.0; HOP],
            last_error: vec![0.0; HOP],
            learning_estimate: vec![0.0; HOP],
            learning_error: vec![0.0; HOP],
            frame: vec![0.0; SIZE],
            bins: vec![[0.0; 2]; BINS],
            out: vec![0; 2 * HOP],
            head: 0,
            ready: 0,
            far_hops: 0,
            realignments: 0,
            passed: 1.0,
        }
    }
}

impl Canceller {
    // Takes a block as the speaker plays it, after its volume, `at` the card's sample its first is heard at.
    pub(super) fn play(&mut self, block: &[i16], at: u64) {
        if self.far.take(block, at) {
            self.realign();
        }
    }

    // Takes a block the microphone recorded, `at` the card's sample its first was taken at, and gives it back a hop late with the speaker's sound taken out.
    pub(super) fn record(&mut self, block: &mut [i16], at: u64) {
        let (start, is_moved) = place(self.near_next, at);
        if is_moved {
            self.realign();
        }
        self.near_next = Some(start.wrapping_add(block.len() as u64));
        let mut done = 0;
        while done < block.len() {
            let take = (HOP - self.filled).min(block.len() - done);
            for (to, &sample) in self.near[self.filled..].iter_mut().zip(&block[done..done + take]) {
                *to = f32::from(sample) / FULL_SCALE;
            }
            (self.filled, done) = (self.filled + take, done + take);
            if self.filled == HOP {
                self.filled = 0;
                self.hop(start.wrapping_add(done as u64).wrapping_sub(HOP as u64));
            }
            for sample in &mut block[done - take..done] {
                *sample = self.pop();
            }
        }
    }

    // Counts samples the microphone lost before its next block; a block the speaker lost is told by no count, and the next is placed by its stamp.
    pub(super) fn lose(&mut self, samples: u64) {
        self.near_next = self.near_next.map(|next| next.wrapping_add(samples));
    }

    // True once the filter's estimate has stood 10 dB over what it leaves, until the streams or the path move.
    const fn is_converged(&self) -> bool {
        self.filter.is_adapted
    }

    // Over the last 2 s of far sound: the decibels of what the microphone heard over what the filter left, and those the suppressor takes off that.
    fn removed(&self) -> (f32, f32) {
        let filter = &self.filter;
        let heard = (filter.heard_estimate + filter.heard_error) / filter.heard_error.max(f32::MIN_POSITIVE);
        (decibels(heard).max(0.0), (-decibels(self.passed)).max(0.0))
    }

    pub(super) fn figures(&self) -> Figures {
        let (filter, suppressor) = self.removed();
        let (far_hops, realignments, converged) = (self.far_hops, self.realignments, self.converged);
        let level = (self.near_power > 0.0).then(|| decibels((self.echo_power / self.near_power) as f32));
        let is_converged = self.is_converged();
        Figures { is_converged, lag: self.lag.taken(), converged, level, filter, suppressor, far_hops, realignments }
    }

    // Moves the filter's reach to the echo's lag where the search took one more than a skew away from it, the filter started anew there; true where it moved.
    fn follow_lag(&mut self) -> bool {
        let Some(lag) = self.lag.taken().filter(|lag| lag.abs_diff(self.shift) > SKEW) else {
            return false;
        };
        self.shift = lag;
        self.filter.weights.fill([0.0; 2]);
        self.filter.foreground.fill([0.0; 2]);
        self.realign();
        true
    }

    fn realign(&mut self) {
        self.realignments += 1;
        self.filter.unlearn();
    }

    fn pop(&mut self) -> i16 {
        if self.ready == 0 {
            return 0;
        }
        let sample = self.out[self.head];
        (self.head, self.ready) = ((self.head + 1) % self.out.len(), self.ready - 1);
        sample
    }

    fn push(&mut self, sample: i16) {
        let capacity = self.out.len();
        self.out[(self.head + self.ready) % capacity] = sample;
        if self.ready == capacity {
            self.head = (self.head + 1) % capacity;
        } else {
            self.ready += 1;
        }
    }

    fn hop(&mut self, at: u64) {
        self.far.read(at.wrapping_add(SKEW), &mut self.ahead);
        self.lag.push(&self.near, &self.ahead);
        let is_moved = self.follow_lag();
        let is_next = !is_moved && self.last_hop == Some(at.wrapping_sub(HOP as u64));
        self.last_hop = Some(at);
        if is_next {
            self.filter.newest = (self.filter.newest + PARTITIONS - 1) % PARTITIONS;
            self.far_spectrum(at, 0);
        } else {
            (0..PARTITIONS).for_each(|partition| self.far_spectrum(at, partition));
        }
        self.estimate_echo();
        for at in 0..HOP {
            self.error[at] = self.near[at] - self.estimate[at];
            self.learning_error[at] = self.near[at] - self.learning_estimate[at];
        }
        let is_far = self.adapt(is_next);
        if is_far {
            let (near, error) = (dot(&self.near, &self.near), dot(&self.error, &self.error));
            self.converged += u64::from(near > CONVERGED_LEVEL && error < CONVERGED_SHARE * near);
            self.echo_power += f64::from(dot(&self.estimate, &self.estimate));
            self.near_power += f64::from(near);
        }
        self.suppress(is_far);
        self.synthesize();
        for at in 0..HOP {
            self.push(to_sample(self.suppressor.shaped[at] * FULL_SCALE));
        }
        core::mem::swap(&mut self.last_estimate, &mut self.estimate);
        core::mem::swap(&mut self.last_error, &mut self.error);
    }

    // The far side's frame of a partition of the hop at `at`, its spectrum kept in that partition's slot.
    fn far_spectrum(&mut self, at: u64, partition: usize) {
        let from = at.wrapping_add(SKEW).wrapping_add_signed(-self.shift).wrapping_sub(((partition + 1) * HOP) as u64);
        self.far.read(from, &mut self.frame);
        let slot = self.filter.slot(partition);
        self.fft.forward(&self.frame, &mut self.filter.spectra[slot..slot + BINS]);
    }

    // The bins of a hop behind `HOP` noughts.
    fn padded(&mut self, samples: &[f32]) {
        self.frame[..HOP].fill(0.0);
        self.frame[HOP..].copy_from_slice(samples);
        self.fft.forward(&self.frame, &mut self.bins);
    }

    // The kept bins of the last hop and this one under the window.
    fn windowed(&mut self, last: &[f32], now: &[f32], out: &mut [Complex]) {
        for (at, sample) in self.frame.iter_mut().enumerate() {
            let value = if at < HOP { last[at] } else { now[at - HOP] };
            *sample = value * self.suppressor.window[at];
        }
        self.fft.forward(&self.frame, &mut self.bins);
        out.copy_from_slice(&self.bins[LOW..=HIGH]);
    }

    fn estimate_echo(&mut self) {
        let filter = &mut self.filter;
        self.bins.fill([0.0; 2]);
        filter.far_power.fill(0.0);
        for partition in 0..PARTITIONS {
            let spectrum = &filter.spectra[filter.slot(partition)..][..BINS];
            add_products(&mut self.bins, &filter.weights[partition * BINS..][..BINS], spectrum, 1.0);
            for (far, &value) in filter.far_power.iter_mut().zip(spectrum) {
                *far += power(value);
            }
        }
        self.fft.inverse(&self.bins, &mut self.frame);
        self.learning_estimate.copy_from_slice(&self.frame[HOP..]);
        let filter = &self.filter;
        self.bins.fill([0.0; 2]);
        for partition in 0..PARTITIONS {
            let spectrum = &filter.spectra[filter.slot(partition)..][..BINS];
            add_products(&mut self.bins, &filter.foreground[partition * BINS..][..BINS], spectrum, 1.0);
        }
        self.fft.inverse(&self.bins, &mut self.frame);
        self.estimate.copy_from_slice(&self.frame[HOP..]);
    }

    // Gives the foreground the learning filter's weights where its error is less beyond chance, and the learning filter the foreground's where its error is more by twice that: the test is Speex's.
    fn compare(&mut self) {
        let (foreground, learning) = (dot(&self.error, &self.error), dot(&self.learning_error, &self.learning_error));
        let differences = self.estimate.iter().zip(&self.learning_estimate).map(|(one, other)| one - other);
        let apart: f32 = differences.map(|difference| difference * difference).sum();
        // Less error by chance comes of the foreground's error along the estimates' difference, whose variance their powers' product bounds.
        let (gain, spread) = (foreground - learning, foreground * apart);
        let filter = &mut self.filter;
        filter.gain = COMPARING * filter.gain + (1.0 - COMPARING) * gain;
        filter.spread = COMPARING * COMPARING * filter.spread + (1.0 - COMPARING) * (1.0 - COMPARING) * spread;
        let better = |gain: f32, spread: f32| gain * gain.abs() > spread;
        if better(filter.gain, filter.spread) {
            filter.foreground.copy_from_slice(&filter.weights);
        } else if better(-gain, 4.0 * spread) || better(-filter.gain, 4.0 * filter.spread) {
            filter.weights.copy_from_slice(&filter.foreground);
        } else {
            return;
        }
        (filter.gain, filter.spread) = (0.0, 0.0);
    }

    // Takes a step of the learning filter where the hop follows on and the far side sounds; true where it sounds.
    fn adapt(&mut self, is_next: bool) -> bool {
        let is_wild = |sound: &[f32]| {
            let power = dot(sound, sound);
            !power.is_finite() || power > DIVERGED * HOP as f32
        };
        if is_wild(&self.estimate) {
            self.filter.foreground.fill([0.0; 2]);
            self.filter.weights.fill([0.0; 2]);
            self.filter.unlearn();
            return false;
        }
        if is_wild(&self.learning_estimate) {
            self.filter.weights.copy_from_slice(&self.filter.foreground);
            self.filter.unlearn();
            return false;
        }
        let estimate = core::mem::take(&mut self.learning_estimate);
        self.padded(&estimate);
        self.learning_estimate = estimate;
        for (power_of, value) in self.filter.estimate_power.iter_mut().zip(&self.bins) {
            *power_of = power(*value);
        }
        let estimate_sum = dot(self.bins.as_flattened(), self.bins.as_flattened());
        let error = core::mem::take(&mut self.learning_error);
        self.padded(&error);
        self.learning_error = error;
        self.filter.error_bins.copy_from_slice(&self.bins);
        let filter = &mut self.filter;
        for (power_of, &value) in filter.error_power.iter_mut().zip(&filter.error_bins) {
            *power_of = power(value);
        }
        let error_sum = dot(filter.error_bins.as_flattened(), filter.error_bins.as_flattened());
        let far_sum: f32 = filter.far_power.iter().sum();
        // A hop's far sound over the filter's reach, as the power of one hop, whose echo reaches the noise floor.
        let far_hop = far_sum / (2 * PARTITIONS) as f32;
        filter.echo_share = 0.0;
        if COUPLING * far_hop < (BINS * HOP) as f32 * NOISE {
            return false;
        }
        self.far_hops += 1;
        let (mut cross, mut square) = (0.0f32, 0.0f32);
        for bin in 0..BINS {
            let from_estimate = filter.estimate_power[bin] - filter.mean_estimate[bin];
            let from_error = filter.error_power[bin] - filter.mean_error[bin];
            (cross, square) = (cross + from_estimate * from_error, square + from_estimate * from_estimate);
            filter.mean_estimate[bin] += AVERAGE * from_estimate;
            filter.mean_error[bin] += AVERAGE * from_error;
        }
        // The averages move at full rate while the estimate holds a quarter of the error's power, slower as near sound outweighs it.
        let rate = (4.0 * LEAK_RATE * estimate_sum / error_sum.max(f32::MIN_POSITIVE)).min(LEAK_RATE);
        filter.cross += rate * (cross - filter.cross);
        filter.square += rate * (square - filter.square);
        if filter.square > 0.0 {
            filter.leak = (filter.cross / filter.square).clamp(MIN_LEAK, 1.0);
        }
        filter.echo_share = (filter.leak * estimate_sum / error_sum.max(f32::MIN_POSITIVE)).min(1.0);
        // A leak past the share at which the filter counts as having learned the path is a path lost: the step follows the coupling again.
        if filter.is_adapted && filter.leak * ADAPTED > 1.0 {
            filter.unlearn();
        }
        filter.heard_estimate += LEAK_RATE * (estimate_sum - filter.heard_estimate);
        filter.heard_error += LEAK_RATE * (error_sum - filter.heard_error);
        if !filter.is_adapted && filter.heard_estimate >= ADAPTED * filter.heard_error {
            // The leak starts from the share of the estimate the error held as the filter learned, which its averages then follow.
            filter.leak = (filter.heard_error / filter.heard_estimate).clamp(MIN_LEAK, 1.0);
            (filter.square, filter.is_adapted) = (square, true);
            filter.cross = filter.leak * square;
        }
        for (held, &error) in filter.held_error.iter_mut().zip(&filter.error_power) {
            *held = error.max(HOLD * *held);
        }
        if filter.is_adapted {
            for bin in 0..BINS {
                let share = filter.leak * filter.estimate_power[bin] / filter.held_error[bin].max(f32::MIN_POSITIVE);
                filter.steps[bin] = (STEP_SHARE * share).min(MAX_STEP);
            }
        } else {
            let share = COUPLING * far_hop / error_sum.max(f32::MIN_POSITIVE);
            filter.steps.fill(share.min(MAX_STEP));
        }
        self.compare();
        if !is_next {
            return true;
        }
        let filter = &mut self.filter;
        let steps = filter.error_bins.iter().zip(&filter.steps).zip(&filter.far_power);
        for (scaled, ((&error, &step), &far)) in self.bins.iter_mut().zip(steps) {
            let step = step / (far + REGULAR);
            *scaled = [error[0] * step, error[1] * step];
        }
        for partition in 0..PARTITIONS {
            let spectrum = &filter.spectra[filter.slot(partition)..][..BINS];
            add_products(&mut filter.weights[partition * BINS..][..BINS], &self.bins, spectrum, -1.0);
        }
        // Partitions are held to their first half in time, where a product of spectra is a convolution and not a circle: the one holding the most of the path every hop, as Speex holds its first, and the others in turn.
        let strongest = (0..PARTITIONS)
            .map(|partition| {
                let weights = filter.weights[partition * BINS..(partition + 1) * BINS].as_flattened();
                dot(weights, weights)
            })
            .enumerate()
            .fold((0, f32::MIN), |best, (partition, sum)| if sum > best.1 { (partition, sum) } else { best })
            .0;
        let turn = filter.constrained;
        filter.constrained = (turn + 1) % PARTITIONS;
        for partition in [Some(strongest), Some(turn).filter(|turn| *turn != strongest)].into_iter().flatten() {
            let weights = &mut self.filter.weights[partition * BINS..(partition + 1) * BINS];
            self.fft.inverse(weights, &mut self.frame);
            self.frame[HOP..].fill(0.0);
            self.fft.forward(&self.frame, weights);
        }
        true
    }

    // Sets each bin's gain from its error and the echo left in it.
    fn suppress(&mut self, is_far: bool) {
        // The frame's error in step with the estimate, the echo a loud peak's clipping leaves, which near sound does not follow: its power, at most the error's, and the estimate's.
        let (mut along, mut square) = (0.0f32, 0.0f32);
        for (at, &weight) in self.suppressor.window.iter().enumerate() {
            let (error, estimate) = if at < HOP {
                (self.last_error[at], self.last_estimate[at])
            } else {
                (self.error[at - HOP], self.estimate[at - HOP])
            };
            let weighted = weight * weight * estimate;
            (along, square) = (along + weighted * error, square + weighted * estimate);
        }
        let in_step = if square > 0.0 { along * along / square } else { 0.0 };
        let (last, now) = (core::mem::take(&mut self.last_error), core::mem::take(&mut self.error));
        let mut error_bins = core::mem::take(&mut self.suppressor.error_bins);
        self.windowed(&last, &now, &mut error_bins);
        (self.last_error, self.error, self.suppressor.error_bins) = (last, now, error_bins);
        let (last, now) = (core::mem::take(&mut self.last_estimate), core::mem::take(&mut self.estimate));
        let mut estimate_bins = core::mem::take(&mut self.suppressor.estimate_bins);
        self.windowed(&last, &now, &mut estimate_bins);
        (self.last_estimate, self.estimate, self.suppressor.estimate_bins) = (last, now, estimate_bins);
        let (filter, suppressor) = (&self.filter, &mut self.suppressor);
        let total = dot(suppressor.estimate_bins.as_flattened(), suppressor.estimate_bins.as_flattened());
        if filter.is_adapted {
            let rate = LEFT_RATE * filter.echo_share;
            suppressor.mean_total += rate * (total - suppressor.mean_total);
            for bin in 0..KEPT {
                suppressor.mean_error[bin] += rate * (power(suppressor.error_bins[bin]) - suppressor.mean_error[bin]);
                suppressor.mean_estimate[bin] +=
                    rate * (power(suppressor.estimate_bins[bin]) - suppressor.mean_estimate[bin]);
            }
        }
        let louder = if suppressor.mean_total > 0.0 { total / suppressor.mean_total } else { 0.0 };
        // A bin of a frame at full scale under the window holds at most the frame's samples squared.
        let bound = DIVERGED * (SIZE * SIZE) as f32;
        let (mut before, mut after) = (0.0f32, 0.0f32);
        for bin in 0..KEPT {
            let error = power(suppressor.error_bins[bin]);
            let estimate = power(suppressor.estimate_bins[bin]);
            // The bin's mean echo left, as loud as the echo is now against its mean, in the bin or over all.
            let mean = suppressor.mean_estimate[bin];
            let loud = if mean > 0.0 { louder.max(estimate / mean) } else { louder };
            // The error in step with the estimate is the estimate scaled, so each bin holds the same share of it as of the estimate.
            let coherent = if square > 0.0 { in_step * estimate / square } else { 0.0 };
            let mut echo = OVER * (suppressor.mean_error[bin] * loud).max(coherent);
            if !filter.is_adapted {
                let far = (0..PARTITIONS).map(|partition| power(filter.spectra[filter.slot(partition) + LOW + bin]));
                // A frame of two hops under the window holds half the power of two hops without it.
                echo = echo.max(COUPLING * far.fold(0.0f32, f32::max) / 2.0);
            }
            echo = echo.max(HOLD * suppressor.held[bin]);
            // An echo left past what a bin can hold, or none to be told, is a filter gone wrong: the bin counts as echo alone.
            if !echo.is_finite() || echo > bound {
                echo = bound;
            }
            suppressor.held[bin] = echo;
            // The decision-directed estimate of the near sound's power (Ephraim and Malah, 1984), and its share of the bin as the gain.
            let near = SMOOTHING * suppressor.clean[bin] + (1.0 - SMOOTHING) * (error - echo).max(0.0);
            let gain = if near + echo > 0.0 { (near / (near + echo)).max(FLOOR) } else { 1.0 };
            suppressor.gains[bin] = gain;
            suppressor.clean[bin] = gain * gain * error;
            (before, after) = (before + error, after + gain * gain * error);
        }
        if is_far && before > 0.0 {
            self.passed += LEAK_RATE * (after / before - self.passed);
        }
    }

    // The error under the suppressor's gains, back in time and laid over the last frame's second half.
    fn synthesize(&mut self) {
        self.bins.fill([0.0; 2]);
        let suppressor = &mut self.suppressor;
        for ((bin, &value), &gain) in
            self.bins[LOW..=HIGH].iter_mut().zip(&suppressor.error_bins).zip(&suppressor.gains)
        {
            *bin = [value[0] * gain, value[1] * gain];
        }
        self.fft.inverse(&self.bins, &mut self.frame);
        for at in 0..HOP {
            suppressor.shaped[at] = suppressor.overlap[at] + self.frame[at] * suppressor.window[at];
            suppressor.overlap[at] = self.frame[HOP + at] * suppressor.window[HOP + at];
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use crate::base::alloc_count::allocations;

    // Samples a recorded block comes back late by, where every block's length is a whole number of them; a block of another length adds up to one less than as many again.
    const DELAY: usize = HOP;
    use crate::base::math::xorshift64;
    use core::f64::consts::{PI, TAU};
    use core::ops::Range;

    const SECOND: usize = RATE as usize;
    const FULL: f64 = FULL_SCALE as f64;
    // Where the card's clock stands when a call begins: far from nought, so that no stamp an error moves wraps.
    const BASE: u64 = 1 << 40;
    // The far side's level over its speech: what the leveller brings a voice to, 20 dB below full scale.
    const FAR_LEVEL: f64 = -20.0;
    // The near side's: speech next to the device peaks under -30 dBFS at 12 dB of preamp, under -18 dBFS at the 24 dB the microphone records with, and speech stands about 12 dB under its peaks.
    const NEAR_LEVEL: f64 = -30.0;
    // A woman on the far side, a man on the near side.
    const FAR_PITCH: f64 = 210.0;
    const NEAR_PITCH: f64 = 110.0;
    // The noise floor at the microphone's 24 dB, as measured on the card.
    const NOISE_LEVEL: f64 = -67.4;
    // The echo at 1 kHz: -15 dB measured at 12 dB of preamp, 12 dB more at 24 dB.
    const ECHO_GAIN: f64 = -3.0;
    // An echo return loss enhancement the filter is to reach, and the least it is to settle at.
    const REACHED: f64 = 20.0;
    const SETTLED: f64 = 25.0;

    struct Random(u64);

    impl Random {
        fn bits(&mut self) -> u64 {
            self.0 = xorshift64(self.0);
            self.0
        }

        fn unit(&mut self) -> f64 {
            (self.bits() >> 11) as f64 / (1u64 << 53) as f64
        }

        fn between(&mut self, low: f64, high: f64) -> f64 {
            low + (high - low) * self.unit()
        }

        // Box and Muller's normal deviate.
        fn normal(&mut self) -> f64 {
            let (first, second) = (self.unit().max(f64::MIN_POSITIVE), self.unit());
            (-2.0 * first.ln()).sqrt() * (TAU * second).cos()
        }
    }

    // A pair of poles at `hertz`, `width` Hz wide, with a gain of one at their peak.
    struct Resonance {
        feedback: [f64; 2],
        gain: f64,
        last: [f64; 2],
    }

    impl Resonance {
        fn new(hertz: f64, width: f64) -> Resonance {
            let radius = (-PI * width / f64::from(RATE)).exp();
            let angle = TAU * hertz / f64::from(RATE);
            let gain = (1.0 - radius) * (1.0 - 2.0 * radius * (2.0 * angle).cos() + radius * radius).sqrt();
            Resonance { feedback: [2.0 * radius * angle.cos(), -radius * radius], gain, last: [0.0; 2] }
        }

        fn next(&mut self, input: f64) -> f64 {
            let out = self.gain * input + self.feedback[0] * self.last[0] + self.feedback[1] * self.last[1];
            self.last = [out, self.last[0]];
            out
        }
    }

    // The first three formants of i, ɛ, a, ɔ and u (Peterson and Barney, 1952, men's means) and their widths.
    const VOWELS: [[f64; 3]; 5] = [
        [270.0, 2290.0, 3010.0],
        [530.0, 1840.0, 2480.0],
        [730.0, 1090.0, 2440.0],
        [570.0, 840.0, 2410.0],
        [300.0, 870.0, 2240.0],
    ];
    const WIDTHS: [f64; 3] = [80.0, 100.0, 140.0];

    fn samples(seconds: f64) -> usize {
        (seconds * f64::from(RATE)) as usize
    }

    // The first sample of the card's block nearest a time.
    fn block_at(seconds: f64) -> usize {
        samples(seconds) / card::PERIOD * card::PERIOD
    }

    // Speech-like sound at `level` dBFS over its syllables: talk spurts of 1.2 to 2.8 s between pauses of 0.25 to 0.7 s, of syllables each a consonant of hiss through a resonance between 2.5 and 5.5 kHz, 12 dB under its vowel, and a vowel of pitch pulses through a glottis's two poles and the vowel's three formants, under rises of 10 ms and falls of 25 ms; the pitch falls by a fifth of itself across a spurt, and the lips radiate.
    fn speech(seconds: f64, pitch: f64, level: f64, seed: u64) -> Vec<f64> {
        let mut random = Random(seed);
        let length = samples(seconds);
        let (mut out, mut is_spoken) = (vec![0.0; length], vec![false; length]);
        // A higher voice's shorter tract raises its formants, by a sixth for a woman's pitch.
        let tract = (pitch / 120.0).powf(0.25);
        let mut at = 0;
        while at < length {
            let (spurt_start, spurt_end) = (at, (at + samples(random.between(1.2, 2.8))).min(length));
            while at < spurt_end {
                let consonant = samples(random.between(0.03, 0.08));
                let total = consonant + samples(random.between(0.10, 0.22));
                let mut hiss = Resonance::new(random.between(2500.0, 5500.0), 1500.0);
                let formants = VOWELS[(random.unit() * VOWELS.len() as f64) as usize % VOWELS.len()];
                let mut resonances: Vec<Resonance> =
                    formants.iter().zip(WIDTHS).map(|(&hertz, width)| Resonance::new(hertz * tract, width)).collect();
                let syllable_pitch = pitch * random.between(0.9, 1.1);
                let (mut phase, mut glottis) = (0.0, [0.0f64; 2]);
                let mut syllable = vec![0.0; total];
                for (offset, sample) in syllable.iter_mut().enumerate() {
                    *sample = if offset < consonant {
                        hiss.next(random.normal())
                    } else {
                        let fall = 1.1 - 0.2 * (at + offset - spurt_start) as f64 / (spurt_end - spurt_start) as f64;
                        phase += syllable_pitch * fall / f64::from(RATE);
                        let pulse = if phase >= 1.0 { 1.0 } else { 0.0 };
                        phase -= pulse;
                        glottis[0] = 0.95 * glottis[0] + pulse + 0.01 * random.normal();
                        glottis[1] = 0.95 * glottis[1] + 0.05 * glottis[0];
                        resonances.iter_mut().fold(glottis[1], |sound, resonance| resonance.next(sound))
                    };
                }
                let rms =
                    |part: &[f64]| (part.iter().map(|value| value * value).sum::<f64>() / part.len() as f64).sqrt();
                let (consonant_rms, vowel_rms) = (rms(&syllable[..consonant]), rms(&syllable[consonant..]));
                for (offset, &value) in syllable.iter().enumerate() {
                    if at + offset >= length {
                        break;
                    }
                    let (rise, fall) = (samples(0.010) as f64, samples(0.025) as f64);
                    let envelope = (offset as f64 / rise).min((total - offset) as f64 / fall).min(1.0);
                    let shaped = (0.5 - 0.5 * (PI * envelope).cos()) * value;
                    out[at + offset] =
                        if offset < consonant { 0.25 * shaped / consonant_rms } else { shaped / vowel_rms };
                    is_spoken[at + offset] = true;
                }
                at += total + samples(random.between(0.02, 0.07));
            }
            at += samples(random.between(0.25, 0.7));
        }
        let mut last = 0.0;
        for sample in &mut out {
            (*sample, last) = (*sample - 0.95 * last, *sample);
        }
        let spoken: f64 = out.iter().zip(&is_spoken).filter(|&(_, &is)| is).map(|(value, _)| value * value).sum();
        let count = is_spoken.iter().filter(|&&is| is).count().max(1);
        let scale = 10f64.powf(level / 20.0) / (spoken / count as f64).sqrt();
        out.iter_mut().for_each(|sample| *sample *= scale);
        out
    }

    // The loudspeaker's distortion: a soft clip, and a square term for the even harmonics.
    fn speaker(sample: f64) -> f64 {
        (1.2 * sample).tanh() / 1.2 + 0.08 * sample * sample
    }

    // An echo path of 24 ms at the card's rate, scaled to `gain` dB at 1 kHz: the direct path 5 cm away, reflections at their delays and levels, and the case's ringing from 1 ms on, `ringing` dB of the direct path's power over all, dying 60 dB in 20 ms; all through the speaker's rise to its resonance at 700 Hz.
    fn path(gain: f64, reflections: &[(f64, f64)], ringing: f64, seed: u64) -> Vec<f64> {
        let mut random = Random(seed);
        let (length, direct, decay) = (samples(0.024), samples(0.05 / 343.0), samples(0.020) as f64);
        let mut acoustic = vec![0.0; length];
        acoustic[direct] = 1.0;
        for &(seconds, level) in reflections {
            acoustic[direct + samples(seconds)] += 10f64.powf(level / 20.0);
        }
        // A tail whose amplitude dies by 6.9 nepers, 60 dB, in its decay holds a decay over 13.8 of its first power.
        let tail = (10f64.powf(ringing / 10.0) * 13.8 / decay).sqrt();
        for (at, sample) in acoustic.iter_mut().enumerate().skip(samples(0.001)) {
            *sample += tail * random.normal() * (-6.9 * at as f64 / decay).exp();
        }
        // The speaker as a high pass of two poles at its resonance, 700 Hz, Q 1.2 (the cookbook of Bristow-Johnson).
        let (omega, quality) = (TAU * 700.0 / f64::from(RATE), 1.2);
        let alpha = omega.sin() / (2.0 * quality);
        let (cosine, norm) = (omega.cos(), 1.0 + alpha);
        let forward = [(1.0 + cosine) / 2.0 / norm, -(1.0 + cosine) / norm, (1.0 + cosine) / 2.0 / norm];
        let back = [-2.0 * cosine / norm, (1.0 - alpha) / norm];
        let (mut ins, mut outs) = ([0.0f64; 2], [0.0f64; 2]);
        let mut path: Vec<f64> = acoustic
            .iter()
            .map(|&input| {
                let out = forward[0] * input + forward[1] * ins[0] + forward[2] * ins[1]
                    - back[0] * outs[0]
                    - back[1] * outs[1];
                (ins, outs) = ([input, ins[0]], [out, outs[0]]);
                out
            })
            .collect();
        let (mut real, mut imaginary) = (0.0, 0.0);
        for (at, &value) in path.iter().enumerate() {
            let angle = TAU * 1000.0 * at as f64 / f64::from(RATE);
            (real, imaginary) = (real + value * angle.cos(), imaginary - value * angle.sin());
        }
        let scale = 10f64.powf(gain / 20.0) / (real * real + imaginary * imaginary).sqrt();
        path.iter_mut().for_each(|value| *value *= scale);
        path
    }

    // The device in a hand: the hand at 0.5 ms 10 dB down, the face at 2.5 ms 18 dB down, the case's ringing 10 dB under the direct path.
    fn hand(gain: f64) -> Vec<f64> {
        path(gain, &[(0.0005, -10.0), (0.0025, -18.0)], -10.0, 11)
    }

    // The device on a table: the table's reflection at 0.15 ms 3 dB down, the room's nearest wall at 6 ms 20 dB down, the case's ringing 6 dB under the direct path through the table, and 2 dB more echo at 1 kHz.
    fn table(gain: f64) -> Vec<f64> {
        path(gain + 2.0, &[(0.000_15, -3.0), (0.006, -20.0)], -6.0, 23)
    }

    // A convolution by blocks through transforms of 4096 samples.
    fn convolve(signal: &[f64], path: &[f64]) -> Vec<f64> {
        let size = 4096;
        let block = size - path.len();
        let mut real = Real::new(size);
        let mut frame = vec![0.0f32; size];
        let (mut bins, mut taps) = (vec![[0.0f32; 2]; size / 2 + 1], vec![[0.0f32; 2]; size / 2 + 1]);
        frame[..path.len()].iter_mut().zip(path).for_each(|(to, &value)| *to = value as f32);
        real.forward(&frame, &mut taps);
        let mut out = vec![0.0; signal.len() + size];
        for start in (0..signal.len()).step_by(block) {
            frame.fill(0.0);
            let end = (start + block).min(signal.len());
            frame[..end - start].iter_mut().zip(&signal[start..end]).for_each(|(to, &value)| *to = value as f32);
            real.forward(&frame, &mut bins);
            bins.iter_mut().zip(&taps).for_each(|(bin, &tap)| *bin = rotate(*bin, tap, 1.0));
            real.inverse(&bins, &mut frame);
            out[start..start + size].iter_mut().zip(&frame).for_each(|(to, &value)| *to += f64::from(value));
        }
        out.truncate(signal.len());
        out
    }

    fn quantize(sound: &[f64]) -> Vec<i16> {
        sound.iter().map(|&value| (value * FULL).round().clamp(-FULL, FULL - 1.0) as i16).collect()
    }

    fn noise(length: usize, seed: u64) -> Vec<f64> {
        let (mut random, size) = (Random(seed), 10f64.powf(NOISE_LEVEL / 20.0));
        (0..length).map(|_| size * random.normal()).collect()
    }

    // What the microphone hears of each part, and what it records of them, clipped at full scale: the echo of what is played through the speaker and the paths, each from its sample on; near speech; and its noise.
    struct Scene {
        played: Vec<i16>,
        echo: Vec<f64>,
        near: Vec<f64>,
        noise: Vec<f64>,
        recorded: Vec<i16>,
    }

    impl Scene {
        fn new(far: &[f64], near: &[f64], paths: &[(usize, Vec<f64>)]) -> Scene {
            let played = quantize(far);
            let driven: Vec<f64> = played.iter().map(|&sample| speaker(f64::from(sample) / FULL)).collect();
            let mut echo = vec![0.0; far.len()];
            for (number, (from, path)) in paths.iter().enumerate() {
                let until = paths.get(number + 1).map_or(far.len(), |next| next.0);
                echo[*from..until].copy_from_slice(&convolve(&driven, path)[*from..until]);
            }
            let noise = noise(far.len(), 7);
            let sum: Vec<f64> = (0..far.len()).map(|at| echo[at] + near[at] + noise[at]).collect();
            Scene { played, echo, near: near.to_vec(), noise, recorded: quantize(&sum) }
        }
    }

    // A stamp's error: from where it starts, a random walk of `step` samples a block, turned back at the bound.
    struct Wander {
        error: f64,
        step: f64,
        random: Random,
    }

    impl Wander {
        // An error anywhere within the bound that wanders 2 samples a block.
        fn new(seed: u64) -> Wander {
            let mut random = Random(seed);
            let bound = STAMP_ERROR as f64;
            Wander { error: random.between(-bound, bound), step: 2.0, random }
        }

        fn fixed(error: i64) -> Wander {
            Wander { error: error as f64, step: 0.0, random: Random(1) }
        }

        fn next(&mut self) -> i64 {
            let bound = STAMP_ERROR as f64;
            self.error += self.step * self.random.normal();
            if self.error.abs() > bound {
                self.error = (2.0 * bound * self.error.signum() - self.error).clamp(-bound, bound);
            }
            self.error.round() as i64
        }
    }

    fn stamp(at: usize, error: i64) -> u64 {
        BASE.wrapping_add_signed(at as i64 + error)
    }

    // A call: the speaker's blocks handed over three blocks before they are heard, the microphone's a hop at a time as recorded, each stamped off by its error; and what came of each hop.
    struct Session {
        canceller: Canceller,
        speaker: Wander,
        microphone: Wander,
        played: usize,
        recorded: usize,
        first: [Option<i64>; 2],
        estimates: Vec<f32>,
        gains: Vec<f32>,
    }

    impl Session {
        fn with(speaker: Wander, microphone: Wander) -> Session {
            Session {
                canceller: Canceller::default(),
                speaker,
                microphone,
                played: 0,
                recorded: 0,
                first: [None; 2],
                estimates: Vec::new(),
                gains: Vec::new(),
            }
        }

        fn new(seed: u64) -> Session {
            Session::with(Wander::new(seed), Wander::new(seed + 1))
        }

        // Runs the call on to sample `until`.
        fn run(&mut self, scene: &Scene, until: usize) {
            while self.recorded + HOP <= until.min(scene.recorded.len()) {
                while self.played < (self.recorded + HOP + 3 * card::PERIOD).min(scene.played.len()) {
                    let end = (self.played + card::PERIOD).min(scene.played.len());
                    let error = self.speaker.next();
                    self.first[0].get_or_insert(error);
                    self.canceller.play(&scene.played[self.played..end], stamp(self.played, error));
                    self.played = end;
                }
                let error = self.microphone.next();
                self.first[1].get_or_insert(error);
                let mut block = scene.recorded[self.recorded..self.recorded + HOP].to_vec();
                self.canceller.record(&mut block, stamp(self.recorded, error));
                self.estimates.extend_from_slice(&self.canceller.last_estimate);
                self.gains.extend_from_slice(&self.canceller.suppressor.gains);
                self.recorded += HOP;
            }
        }

        // How much later than the path the filter reads the far side: the microphone's first error less the speaker's.
        fn skew(&self) -> i64 {
            self.first[1].unwrap_or(0) - self.first[0].unwrap_or(0)
        }
    }

    fn db(ratio: f64) -> f64 {
        10.0 * ratio.max(1.0e-30).log10()
    }

    fn squares(sound: impl Iterator<Item = f64>) -> f64 {
        sound.map(|value| value * value).sum()
    }

    // The filter's echo return loss enhancement over each half second whose echo stands 20 dB over the noise floor: the echo's power over what the estimate leaves of it.
    fn erle(echo: &[f64], estimates: &[f32]) -> Vec<Option<f64>> {
        let window = SECOND / 2;
        let floor = 100.0 * 10f64.powf(NOISE_LEVEL / 10.0) * window as f64;
        (0..estimates.len() / window)
            .map(|number| {
                let range = number * window..(number + 1) * window;
                let heard = squares(echo[range.clone()].iter().copied());
                let left = squares(range.map(|at| echo[at] - f64::from(estimates[at])));
                (heard > floor).then(|| db(heard / left))
            })
            .collect()
    }

    // Seconds from sample `from` to the end of the first half second after it in which the filter reaches `aim`.
    fn reached(windows: &[Option<f64>], from: usize, aim: f64) -> Option<f64> {
        let window = SECOND / 2;
        let first = windows
            .iter()
            .enumerate()
            .skip(from.div_ceil(window))
            .find(|(_, value)| value.is_some_and(|value| value >= aim));
        first.map(|(number, _)| ((number + 1) * window - from) as f64 / SECOND as f64)
    }

    // The filter's echo return loss enhancement over a span: the echo's power over what the estimate leaves of it.
    fn settled(echo: &[f64], estimates: &[f32], span: Range<usize>) -> f64 {
        let left = squares(span.clone().map(|at| echo[at] - f64::from(estimates[at])));
        db(squares(echo[span].iter().copied()) / left)
    }

    // What the canceller's suppressor makes of one part of what the microphone hears, less the filter's estimates where given: each hop's gains on that part alone, a hop late.
    fn through(part: &[f64], estimates: Option<&[f32]>, gains: &[f32]) -> Vec<f64> {
        let mut shadow = Canceller::default();
        let (mut last, mut now, mut bins) = (vec![0.0f32; HOP], vec![0.0f32; HOP], vec![[0.0f32; 2]; KEPT]);
        let mut out = Vec::new();
        for hop in 0..gains.len() / KEPT {
            for (at, sample) in now.iter_mut().enumerate() {
                let index = hop * HOP + at;
                *sample = part[index] as f32 - estimates.map_or(0.0, |estimates| estimates[index]);
            }
            shadow.windowed(&last, &now, &mut bins);
            shadow.suppressor.error_bins.copy_from_slice(&bins);
            shadow.suppressor.gains.copy_from_slice(&gains[hop * KEPT..(hop + 1) * KEPT]);
            shadow.synthesize();
            out.extend(shadow.suppressor.shaped.iter().map(|&sample| f64::from(sample)));
            core::mem::swap(&mut last, &mut now);
        }
        out
    }

    // Samples of a window of the ear's measure, 85 ms: about the 100 ms over which the ear sums a sound's loudness.
    const WINDOW: usize = 4096;
    // The band a call carries, from the lowest of Zwicker's critical bands to the top of wideband.
    const CARRIED: Range<f64> = 100.0..8000.0;

    // The power of each window of a sound under a Hann window, each bin of the band a call carries weighted by `weight` at its frequency.
    fn windows(sound: &[f64], weight: impl Fn(f64) -> f64) -> Vec<f64> {
        let mut real = Real::new(WINDOW);
        let (mut frame, mut bins) = (vec![0.0f32; WINDOW], vec![[0.0f32; 2]; WINDOW / 2 + 1]);
        let bin_of = |hertz: f64| (hertz * WINDOW as f64 / f64::from(RATE)).round() as usize;
        (0..sound.len() / WINDOW)
            .map(|number| {
                for (at, sample) in frame.iter_mut().enumerate() {
                    let hann = 0.5 - 0.5 * (TAU * (at as f64 + 0.5) / WINDOW as f64).cos();
                    *sample = (sound[number * WINDOW + at] * hann) as f32;
                }
                real.forward(&frame, &mut bins);
                (bin_of(CARRIED.start)..bin_of(CARRIED.end))
                    .map(|bin| f64::from(power(bins[bin])) * weight(bin as f64 * f64::from(RATE) / WINDOW as f64))
                    .sum()
            })
            .collect()
    }

    // The level of each window in the band a call carries.
    fn levels(sound: &[f64]) -> Vec<f64> {
        windows(sound, |_| 1.0)
    }

    // The level of each window in the band a call carries as the ear weighs it: IEC 61672-1's A-weighting.
    fn weighted(sound: &[f64]) -> Vec<f64> {
        windows(sound, |hertz| {
            let square = hertz * hertz;
            let response = 12_194.0f64.powi(2) * square * square
                / ((square + 20.6f64.powi(2))
                    * ((square + 107.7f64.powi(2)) * (square + 737.9f64.powi(2))).sqrt()
                    * (square + 12_194.0f64.powi(2)));
            response * response * 10f64.powf(0.2)
        })
    }

    // The foreground filter's distance from the path in the bins the suppressor lets through, as a power over the path's: the path as its taps, `skew` samples later than the filter's lead.
    fn misalignment(canceller: &Canceller, path: &[f64], skew: i64) -> f64 {
        let mut real = Real::new(SIZE);
        let (mut frame, mut bins) = (vec![0.0f32; SIZE], vec![[0.0f32; 2]; BINS]);
        let (mut apart, mut whole) = (0.0, 0.0);
        for partition in 0..PARTITIONS {
            frame.fill(0.0);
            for (offset, tap) in frame[..HOP].iter_mut().enumerate() {
                let at = (partition * HOP + offset) as i64 - SKEW as i64 - skew;
                *tap = usize::try_from(at).ok().and_then(|at| path.get(at)).map_or(0.0, |&value| value as f32);
            }
            real.forward(&frame, &mut bins);
            for (bin, &value) in bins.iter().enumerate().take(HIGH + 1).skip(LOW) {
                let weight = canceller.filter.foreground[partition * BINS + bin];
                apart += f64::from(power([weight[0] - value[0], weight[1] - value[1]]));
                whole += f64::from(power(value));
            }
        }
        apart / whole
    }

    // True while every number the canceller keeps is finite.
    fn is_finite(canceller: &Canceller) -> bool {
        let (filter, suppressor) = (&canceller.filter, &canceller.suppressor);
        let complex = [&filter.spectra, &filter.weights, &filter.foreground, &filter.error_bins];
        let more = [&suppressor.error_bins, &suppressor.estimate_bins, &canceller.bins];
        let real = [
            &canceller.far.samples,
            &filter.far_power,
            &filter.estimate_power,
            &filter.error_power,
            &filter.steps,
            &filter.mean_estimate,
            &filter.mean_error,
            &suppressor.gains,
            &suppressor.clean,
            &suppressor.overlap,
            &suppressor.shaped,
            &suppressor.mean_error,
            &suppressor.mean_estimate,
            &suppressor.held,
            &canceller.estimate,
            &canceller.last_estimate,
            &canceller.error,
            &canceller.last_error,
            &canceller.learning_estimate,
            &canceller.learning_error,
            &canceller.near,
            &canceller.frame,
        ];
        let single = [
            filter.gain,
            filter.spread,
            filter.leak,
            filter.cross,
            filter.square,
            filter.echo_share,
            filter.heard_estimate,
            filter.heard_error,
            suppressor.mean_total,
            canceller.passed,
        ];
        complex.iter().chain(&more).all(|values| values.iter().flatten().all(|value| value.is_finite()))
            && real.iter().all(|values| values.iter().all(|value| value.is_finite()))
            && single.iter().all(|value| value.is_finite())
    }

    #[test]
    fn speaker_distortion() {
        let tone: Vec<f64> = (0..SECOND).map(|at| 0.5 * (TAU * 1000.0 * at as f64 / f64::from(RATE)).sin()).collect();
        let driven: Vec<f64> = tone.iter().map(|&sample| speaker(sample)).collect();
        let size_at = |hertz: f64| {
            let (mut real, mut imaginary) = (0.0, 0.0);
            for (at, &value) in driven.iter().enumerate() {
                let angle = TAU * hertz * at as f64 / f64::from(RATE);
                (real, imaginary) = (real + value * angle.cos(), imaginary + value * angle.sin());
            }
            (real * real + imaginary * imaginary).sqrt()
        };
        let harmonics = (2..=5).map(|order| size_at(1000.0 * f64::from(order)).powi(2)).sum::<f64>().sqrt();
        let distortion = harmonics / size_at(1000.0);
        assert!(
            (0.02..0.05).contains(&distortion),
            "the loudspeaker's model distorts a tone at -6 dBFS by a few percent: {:.2} %",
            100.0 * distortion
        );
    }

    #[test]
    fn placing_by_count() {
        assert_eq!(place(None, 500), (500, false), "a stream's first block stands at its stamp");
        assert_eq!(place(Some(1000), 1000 + SKEW), (1000, false), "one within the skew follows its count");
        assert_eq!(place(Some(1000), 1000 - SKEW), (1000, false), "either way");
        assert_eq!(place(Some(1000), 1001 + SKEW), (1001 + SKEW, true), "one beyond starts anew at its stamp");
        assert_eq!(place(Some(5), u64::MAX - 3), (5, false), "and the clock's wrap is no distance");
        assert_eq!(place(Some(1000), 3), (3, true), "nor is a stamp gone back");
    }

    #[test]
    fn far_ring() {
        let mut far = Far { samples: vec![0.0; RING], start: 0, end: 0, next: None };
        assert!(!far.take(&[16_384; 4], 100), "the speaker's first block");
        let mut read = [9.0f32; 8];
        far.read(98, &mut read);
        assert_eq!(read, [0.0, 0.0, 0.5, 0.5, 0.5, 0.5, 0.0, 0.0], "silence before and after what was played");
        assert!(!far.take(&[-16_384; 2], 104 + SKEW));
        far.read(103, &mut read[..4]);
        assert_eq!(read[..4], [0.5, -0.5, -0.5, 0.0], "a block within the skew follows on its count");
        assert!(far.take(&[8192; 2], 2000), "a block beyond it starts anew at its stamp");
        far.read(104, &mut read);
        assert_eq!(read, [-0.5, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], "with silence up to it");
        assert!(far.take(&[4096], 105), "a block gone back takes its place");
        far.read(104, &mut read[..3]);
        assert_eq!(read[..3], [-0.5, 0.125, 0.0], "and what followed is played no more");
        far.take(&[1; 4], 105 + 10 * RING as u64);
        far.read(105, &mut read[..2]);
        assert_eq!(read[..2], [0.0, 0.0], "what lies a ring behind the newest is no longer held");
    }

    #[test]
    fn delay_and_lengths() {
        let mut canceller = Canceller::default();
        let (mut block, mut out) = ([0i16; card::PERIOD], Vec::new());
        block[100] = 10_000;
        for number in 0..3 {
            let mut recorded = if number == 0 { block } else { [0; card::PERIOD] };
            canceller.record(&mut recorded, (number * card::PERIOD) as u64);
            out.extend_from_slice(&recorded);
        }
        let loudest = |sound: &[i16]| (0..sound.len()).max_by_key(|&at| sound[at].unsigned_abs());
        assert_eq!(loudest(&out), Some(100 + DELAY), "a click comes out a hop late");
        let mut odd = Canceller::default();
        let (mut sent, mut back) = (Vec::new(), Vec::new());
        let mut at = 0u64;
        for length in [100usize, 1, 700, 255, 256, 1024, 33, 2000] {
            let mut part: Vec<i16> =
                (0..length).map(|offset| if at + offset as u64 == 1500 { 10_000 } else { 0 }).collect();
            sent.extend_from_slice(&part);
            odd.record(&mut part, at);
            back.extend_from_slice(&part);
            at += length as u64;
        }
        assert_eq!(back.len(), sent.len(), "every block comes back whole");
        let late = loudest(&back).map(|at| at - 1500);
        assert!(late.is_some_and(|late| (DELAY..2 * DELAY).contains(&late)), "blocks of other lengths: {late:?} late");
    }

    #[test]
    fn no_allocation() {
        let mut canceller = Canceller::default();
        let far = quantize(&speech(1.0, FAR_PITCH, FAR_LEVEL, 3));
        let mut block = [0i16; card::PERIOD];
        for (number, chunk) in far.as_chunks::<{ card::PERIOD }>().0.iter().enumerate() {
            let at = (number * card::PERIOD) as u64;
            block.copy_from_slice(chunk);
            let before = allocations();
            canceller.play(chunk, at);
            canceller.record(&mut block, at.wrapping_sub(500));
            canceller.lose(7);
            assert_eq!(allocations(), before, "a block played, a block recorded and samples lost take no memory");
        }
    }

    // The seeds of the far side's speech the calls are made of, a session's stamps and noise following from each.
    const SEEDS: [u64; 3] = [5, 33, 57];

    #[test]
    fn far_alone_converges() {
        for seed in SEEDS {
            let far = speech(40.0, FAR_PITCH, FAR_LEVEL, seed);
            let silence = vec![0.0; far.len()];
            for (gain, coupling) in
                [(ECHO_GAIN, "-3 dB, the card's at 24 dB of preamp"), (ECHO_GAIN - 12.0, "-15 dB, at 12 dB")]
            {
                let case = format!("seed {seed}, coupling {coupling}");
                let scene = Scene::new(&far, &silence, &[(0, hand(gain))]);
                let mut session = Session::new(seed + 100);
                session.run(&scene, far.len());
                let first = reached(&erle(&scene.echo, &session.estimates), 0, REACHED);
                let enhancement = settled(&scene.echo, &session.estimates, 20 * SECOND..session.estimates.len());
                assert!(
                    first.is_some_and(|seconds| seconds <= 4.0),
                    "{case}: 20 dB within 4 s of far speech: {first:?} s"
                );
                assert!(enhancement >= SETTLED, "{case}: the filter settles at {enhancement:.1} dB over the last 20 s");
                let residual = through(&scene.echo, Some(&session.estimates), &session.gains);
                let (heard, floor) = (weighted(&residual[HOP..]), weighted(&scene.noise));
                let mut over: Vec<f64> = heard
                    .iter()
                    .zip(&floor)
                    .skip(20 * SECOND / WINDOW)
                    .map(|(heard, floor)| db(heard / floor))
                    .collect();
                over.sort_by(|one, other| other.total_cmp(one));
                // The level a fluctuating sound stands over one hundredth of the time, as ISO 1996-1 takes its percentiles.
                let exceeded = over[over.len() / 100];
                assert!(
                    exceeded < 0.0,
                    "{case}: what the far side hears of itself stands under the microphone's noise floor, A-weighted, in 99 of every 100 windows of 85 ms over the last 20 s: {exceeded:.1} dB from it at the hundredth, {:.1} dB at most",
                    over[0]
                );
                let (filter, _) = session.canceller.removed();
                assert!(
                    session.canceller.figures().to_line().starts_with("echo: converged") && filter >= REACHED as f32,
                    "{case}: the canceller tells it has converged and that the filter takes {filter:.1} dB off"
                );
            }
        }
    }

    // The most the near side's level changes through the suppressor, in decibels, over the 85 ms from sample `from` on in which it speaks: its gains on the near side's sound alone.
    fn change(near: &[f64], gains: &[f32], from: usize) -> f64 {
        let out = through(near, None, gains);
        let (heard, kept) = (levels(&near[from..out.len() - HOP]), levels(&out[from + HOP..]));
        let loudest = heard.iter().copied().fold(0.0, f64::max);
        heard
            .iter()
            .zip(&kept)
            .filter(|&(&heard, _)| heard > loudest / 100.0)
            .map(|(heard, kept)| db(heard / kept).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn near_alone_untouched() {
        let near = speech(20.0, NEAR_PITCH, NEAR_LEVEL, 17);
        let scene = Scene::new(&vec![0.0; near.len()], &near, &[(0, hand(ECHO_GAIN))]);
        let mut fresh = Session::new(13);
        fresh.run(&scene, near.len());
        let before = change(&scene.near, &fresh.gains, 0);
        assert!(
            before < 1.0,
            "before the filter learns, the near side comes through within 1 dB in every 85 ms it speaks: {before:.2} dB at most"
        );
        let gap = 20 * SECOND + SECOND / 4;
        let far: Vec<f64> = speech(20.0, FAR_PITCH, FAR_LEVEL, 5)
            .into_iter()
            .chain(vec![0.0; gap - 20 * SECOND + near.len()])
            .collect();
        let later: Vec<f64> = vec![0.0; gap].into_iter().chain(near.iter().copied()).collect();
        let scene = Scene::new(&far, &later, &[(0, hand(ECHO_GAIN))]);
        let mut session = Session::new(13);
        session.run(&scene, gap);
        assert!(session.canceller.is_converged(), "20 s of far speech teach the filter the path");
        session.run(&scene, far.len());
        let after = change(&scene.near, &session.gains, gap);
        assert!(
            after < 1.0,
            "after it learned, the near side comes through within 1 dB in every 85 ms it speaks: {after:.2} dB at most"
        );
    }

    #[test]
    fn double_talk_holds() {
        let start = 20 * SECOND;
        for seed in SEEDS {
            let far = speech(40.0, FAR_PITCH, FAR_LEVEL, seed);
            let near: Vec<f64> = speech(40.0, NEAR_PITCH, NEAR_LEVEL, seed + 7)
                .iter()
                .enumerate()
                .map(|(at, &value)| if at < start { 0.0 } else { value })
                .collect();
            let scene = Scene::new(&far, &near, &[(0, hand(ECHO_GAIN))]);
            let mut session = Session::new(seed + 200);
            session.run(&scene, start);
            let single = misalignment(&session.canceller, &hand(ECHO_GAIN), session.skew());
            let mut worst = f64::MIN;
            for step in 1..=40 {
                session.run(&scene, start + step * SECOND / 2);
                worst = worst.max(db(misalignment(&session.canceller, &hand(ECHO_GAIN), session.skew()) / single));
            }
            assert!(
                worst <= 3.0,
                "seed {seed}: the filter holds while both talk: {:.1} dB from the path alone, at most {worst:.1} dB more over 20 s of both",
                db(single)
            );
            let end = session.estimates.len();
            let near_out = through(&scene.near, None, &session.gains);
            let (heard, kept) = (levels(&scene.near[start..end - HOP]), levels(&near_out[start + HOP..]));
            let loss = db(heard.iter().sum::<f64>() / kept.iter().sum::<f64>());
            let loudest = heard.iter().copied().fold(0.0, f64::max);
            let mut losses: Vec<f64> = heard
                .iter()
                .zip(&kept)
                .filter(|&(&heard, _)| heard > loudest / 100.0)
                .map(|(heard, kept)| db(heard / kept))
                .collect();
            losses.sort_by(f64::total_cmp);
            assert!(
                loss <= 4.0,
                "seed {seed}: the near side loses {loss:.1} dB of its level while both talk, at most 4; over its 85 ms the median {:.1} dB, nine in ten under {:.1} dB, the most {:.1} dB, where the echo stands up to 20 dB over it",
                losses[losses.len() / 2],
                losses[losses.len() * 9 / 10],
                losses[losses.len() - 1]
            );
            let echo_out = through(&scene.echo, Some(&session.estimates), &session.gains);
            let down = db(squares(scene.echo[start..end - HOP].iter().copied())
                / squares(echo_out[start + HOP..].iter().copied()));
            assert!(
                down >= 40.0,
                "seed {seed}: the far side's echo stays {down:.1} dB down while both talk, at least 40"
            );
        }
    }

    #[test]
    fn path_change_reconverges() {
        let change = 20 * SECOND;
        for seed in SEEDS {
            let far = speech(40.0, FAR_PITCH, FAR_LEVEL, seed);
            let scene = Scene::new(&far, &vec![0.0; far.len()], &[(0, hand(ECHO_GAIN)), (change, table(ECHO_GAIN))]);
            let mut session = Session::new(seed + 300);
            session.run(&scene, far.len());
            let again = reached(&erle(&scene.echo, &session.estimates), change, REACHED);
            let enhancement = settled(&scene.echo, &session.estimates, change + 10 * SECOND..session.estimates.len());
            assert!(
                again.is_some_and(|seconds| seconds <= 5.0),
                "seed {seed}: put on a table, the filter is back at 20 dB within 5 s: {again:?} s"
            );
            assert!(enhancement >= SETTLED, "seed {seed}: and settles at {enhancement:.1} dB on the table's path");
        }
    }

    #[test]
    fn skew_both_ways() {
        let far = speech(40.0, FAR_PITCH, FAR_LEVEL, 5);
        let scene = Scene::new(&far, &vec![0.0; far.len()], &[(0, hand(ECHO_GAIN))]);
        let bound = STAMP_ERROR as i64;
        for (speaker, microphone) in [(-bound, bound), (bound, -bound)] {
            let mut session = Session::with(Wander::fixed(speaker), Wander::fixed(microphone));
            session.run(&scene, far.len());
            let enhancement = settled(&scene.echo, &session.estimates, 20 * SECOND..session.estimates.len());
            assert!(
                enhancement >= SETTLED,
                "stamps {} samples apart, the most two errors make, are within the filter's reach: {enhancement:.1} dB",
                microphone - speaker
            );
        }
    }

    // A call whose streams start apart, each losing samples once, the microphone's counted and the speaker's told by its next block's stamp alone, and whose speaker stops and starts again with its stamps moved.
    struct Course {
        speaker_from: usize,
        microphone_from: usize,
        speaker_lost: Range<usize>,
        microphone_lost: Range<usize>,
        pause: Range<usize>,
        moved: i64,
    }

    // What came of a course: the scene, the filter's estimates by the card's time, and what the microphone recorded and what came back, in the order recorded.
    struct Staged {
        scene: Scene,
        estimates: Vec<f32>,
        recorded: Vec<i16>,
        back: Vec<i16>,
    }

    fn stage(course: &Course) -> Staged {
        let mut far = speech(30.0, FAR_PITCH, FAR_LEVEL, 5);
        let length = far.len() / HOP * HOP;
        for (at, sample) in far.iter_mut().enumerate() {
            let is_silent = at < course.speaker_from || course.speaker_lost.contains(&at) || course.pause.contains(&at);
            *sample = if is_silent { 0.0 } else { *sample };
        }
        let scene = Scene::new(&far, &vec![0.0; far.len()], &[(0, hand(ECHO_GAIN))]);
        let (mut canceller, mut speaker, mut microphone) = (Canceller::default(), Wander::new(13), Wander::new(14));
        let (mut estimates, mut recorded, mut back) = (vec![0.0f32; length], Vec::new(), Vec::new());
        let (mut played, mut is_lost) = (course.speaker_from, false);
        for at in (0..length).step_by(HOP) {
            while played < (at + HOP + 3 * card::PERIOD).min(length) {
                if course.pause.contains(&played) {
                    played = course.pause.end;
                } else if played == course.speaker_lost.start {
                    played = course.speaker_lost.end;
                } else {
                    let error = speaker.next() + if played >= course.pause.end { course.moved } else { 0 };
                    let end = (played + card::PERIOD).min(length);
                    canceller.play(&scene.played[played..end], stamp(played, error));
                    played = end;
                }
            }
            if at < course.microphone_from || course.microphone_lost.contains(&at) {
                is_lost |= course.microphone_lost.contains(&at);
                continue;
            }
            if is_lost {
                canceller.lose(course.microphone_lost.len() as u64);
                is_lost = false;
            }
            let mut block = scene.recorded[at..at + HOP].to_vec();
            recorded.extend_from_slice(&block);
            canceller.record(&mut block, stamp(at, microphone.next()));
            back.extend_from_slice(&block);
            estimates[at..at + HOP].copy_from_slice(&canceller.last_estimate);
            assert!(is_finite(&canceller), "every number the canceller keeps stays finite");
        }
        Staged { scene, estimates, recorded, back }
    }

    #[test]
    fn streams_start_stop() {
        let courses = [
            ("the speaker first", block_at(0.0), block_at(2.0)),
            ("the microphone first", block_at(3.0), block_at(0.0)),
        ];
        for (name, speaker_from, microphone_from) in courses {
            let course = Course {
                speaker_from,
                microphone_from,
                speaker_lost: block_at(12.0)..block_at(12.0) + 2 * card::PERIOD,
                microphone_lost: block_at(8.0)..block_at(8.0) + 3 * card::PERIOD,
                pause: block_at(16.0)..block_at(19.0),
                moved: STAMP_ERROR as i64 / 2,
            };
            let staged = stage(&course);
            let to_sound = |sound: &[i16]| sound.iter().map(|&sample| f64::from(sample) / FULL).collect::<Vec<f64>>();
            let (heard, back) = (to_sound(&staged.recorded), to_sound(&staged.back));
            let (heard, back) = (windows(&heard[..heard.len() - DELAY], |_| 1.0), windows(&back[DELAY..], |_| 1.0));
            let burst = heard.iter().zip(&back).map(|(heard, back)| db(back / heard)).fold(f64::MIN, f64::max);
            assert!(
                burst <= 1.0,
                "{name}: nothing comes back louder than it was recorded, at most {burst:.1} dB over it in any 85 ms"
            );
            let windows = erle(&staged.scene.echo, &staged.estimates);
            let events = [
                ("both run", speaker_from.max(microphone_from)),
                ("the microphone's loss", course.microphone_lost.end),
                ("the speaker's loss", course.speaker_lost.end),
                ("the speaker's return, its stamps moved by 2 ms", course.pause.end),
            ];
            for (event, from) in events {
                let back = reached(&windows, from, REACHED);
                assert!(
                    back.is_some_and(|seconds| seconds <= 5.0),
                    "{name}: after {event} the filter is at 20 dB within 5 s: {back:?} s"
                );
            }
        }
    }

    #[test]
    fn hostile_input_bounded() {
        let mut canceller = Canceller::default();
        let mut at = 0u64;
        let silence = [0i16; card::PERIOD];
        for _ in 0..100 {
            let mut block = silence;
            canceller.play(&silence, at);
            canceller.record(&mut block, at);
            assert_eq!(block, silence, "silence comes back silence");
            at += card::PERIOD as u64;
        }
        let square: Vec<i16> =
            (0..card::PERIOD).map(|offset| if offset < card::PERIOD / 2 { i16::MAX } else { i16::MIN }).collect();
        let level: Vec<i16> = vec![20_000; card::PERIOD];
        for (played, recorded) in [(&square, &square), (&level, &vec![-15_000; card::PERIOD])] {
            for _ in 0..200 {
                let mut block = recorded.clone();
                canceller.play(played, at);
                canceller.record(&mut block, at);
                assert!(
                    is_finite(&canceller),
                    "full scale square waves and a level played and recorded leave every number finite"
                );
                at += card::PERIOD as u64;
            }
        }
        let mut random = Random(77);
        for _ in 0..3000 {
            let length = (random.unit() * 3000.0) as usize;
            let mut block: Vec<i16> = (0..length).map(|_| random.bits() as i16).collect();
            let stamp = match random.bits() % 4 {
                0 => random.bits(),
                1 => at.wrapping_sub(random.bits() % 1_000_000),
                2 => at.wrapping_add(random.bits() % 1000),
                _ => at,
            };
            match random.bits() % 3 {
                0 => canceller.play(&block, stamp),
                1 => canceller.record(&mut block, stamp),
                _ => canceller.lose(random.bits()),
            }
            at = at.wrapping_add(length as u64);
            assert!(
                is_finite(&canceller),
                "blocks of any length at stamps that jump anywhere, back too, leave every number finite"
            );
        }
        let far = speech(20.0, FAR_PITCH, FAR_LEVEL, 5);
        let scene = Scene::new(&far, &vec![0.0; far.len()], &[(0, hand(ECHO_GAIN))]);
        // The hop the last block left part filled is filled up, so that the call's blocks of a hop each are the canceller's hops and its estimates stand by them.
        canceller.record(&mut vec![0; (HOP - canceller.filled) % HOP], at);
        let mut session = Session::new(13);
        session.canceller = canceller;
        session.run(&scene, far.len());
        let enhancement = settled(&scene.echo, &session.estimates, 15 * SECOND..session.estimates.len());
        assert!(
            enhancement >= REACHED,
            "after all that, a call teaches the filter the path again: {enhancement:.1} dB over its last 5 s"
        );
    }

    #[test]
    fn log_line() {
        assert_eq!(
            Canceller::default().figures().to_line(),
            "echo: learning, no lag found, 3 dB under the microphone in 0 % of hops of far sound, no echo measured, 0 dB off by the filter and 0 dB by the suppressor, 0 hops of far sound, 0 realignments"
        );
        let figures = Figures {
            is_converged: true,
            lag: Some(1920),
            converged: 300,
            level: Some(-2.4),
            filter: 21.2,
            suppressor: 9.7,
            far_hops: 1000,
            realignments: 1,
        };
        assert_eq!(
            figures.to_line(),
            "echo: converged, lag 40.0 ms, 3 dB under the microphone in 30 % of hops of far sound, the echo -2 dB against the microphone, 21 dB off by the filter and 10 dB by the suppressor, 1000 hops of far sound, 1 realignments"
        );
    }

    // The card's paths add this much to what the stamps place, beyond the filter's reach about them: an assumption, as no impulse response of the device is measured.
    const PATHS: f64 = 0.040;

    #[test]
    fn lag_beyond_stamps_found() {
        for seed in SEEDS {
            let far = speech(40.0, FAR_PITCH, FAR_LEVEL, seed);
            let delayed: Vec<f64> = vec![0.0; samples(PATHS)].into_iter().chain(hand(ECHO_GAIN)).collect();
            let scene = Scene::new(&far, &vec![0.0; far.len()], &[(0, delayed)]);
            let mut session = Session::new(seed + 400);
            session.run(&scene, far.len());
            let figures = session.canceller.figures();
            let lag = figures.lag.map(|lag| lag as f64 / f64::from(RATE));
            assert!(
                lag.is_some_and(|lag| (lag - PATHS).abs() < 0.005),
                "seed {seed}: the echo's lag behind the stamps is found, {PATHS} s: {lag:?}"
            );
            let enhancement = settled(&scene.echo, &session.estimates, 20 * SECOND..session.estimates.len());
            assert!(enhancement >= SETTLED, "seed {seed}: and the filter settles there at {enhancement:.1} dB");
            assert!(
                figures.converged * 2 > figures.far_hops && figures.level.is_some_and(|level| level > -3.0),
                "seed {seed}: most hops of far sound stand 3 dB under the microphone, and the echo's estimate near its level: {figures:?}"
            );
        }
    }

    // A sound brought to `level` dBFS over all of it.
    fn at_level(sound: Vec<f64>, level: f64) -> Vec<f64> {
        let scale = 10f64.powf(level / 20.0) / (squares(sound.iter().copied()) / sound.len() as f64).sqrt();
        sound.into_iter().map(|sample| sample * scale).collect()
    }

    // A pitch whose period, 10 ms, lies over the skew.
    const LOW_PITCH: f64 = 100.0;

    #[test]
    fn periodic_far_side_lag() {
        let length = samples(40.0);
        let wave =
            |at: usize, harmonic: u32| (TAU * LOW_PITCH * f64::from(harmonic) * at as f64 / f64::from(RATE)).sin();
        let tone = at_level((0..length).map(|at| wave(at, 1)).collect(), FAR_LEVEL);
        // A buzz of the pitch: its harmonics up to 3 kHz, each as loud as its number is small.
        let harmonics = |at: usize| (1..=30).map(|harmonic| wave(at, harmonic) / f64::from(harmonic)).sum();
        let buzz = at_level((0..length).map(harmonics).collect(), FAR_LEVEL);
        let turn = samples(10.0);
        for seed in SEEDS {
            let voice = speech(40.0, LOW_PITCH, FAR_LEVEL, seed);
            let by_turns: Vec<f64> =
                (0..length).map(|at| if (at / turn).is_multiple_of(2) { buzz[at] } else { voice[at] }).collect();
            let cases = [
                ("a voice of 90 Hz", speech(40.0, 90.0, FAR_LEVEL, seed), true),
                ("a voice of 110 Hz", speech(40.0, 110.0, FAR_LEVEL, seed), true),
                ("a tone", tone.clone(), false),
                ("a buzz", buzz.clone(), false),
                ("a buzz and a voice by turns", by_turns, false),
            ];
            for (name, far, is_voice) in cases {
                let delayed: Vec<f64> = vec![0.0; samples(PATHS)].into_iter().chain(hand(ECHO_GAIN)).collect();
                let scene = Scene::new(&far, &vec![0.0; far.len()], &[(0, delayed)]);
                let mut session = Session::new(seed + 400);
                session.run(&scene, far.len());
                let figures = session.canceller.figures();
                let lag = figures.lag.map(|lag| lag as f64 / f64::from(RATE));
                let is_echo = |lag: f64| (lag - PATHS).abs() < 0.005;
                assert!(
                    figures.realignments <= 1,
                    "seed {seed}, {name}: the reach moves once at the most: {} moves",
                    figures.realignments
                );
                if is_voice {
                    let enhancement = settled(&scene.echo, &session.estimates, 20 * SECOND..session.estimates.len());
                    assert!(
                        lag.is_some_and(is_echo) && enhancement >= SETTLED,
                        "seed {seed}, {name}: the echo's lag, {PATHS} s, is found and the filter settles there: {lag:?}, {enhancement:.1} dB"
                    );
                } else {
                    assert!(
                        lag.is_none_or(is_echo),
                        "seed {seed}, {name}: no lag a period off the echo's is taken: {lag:?}"
                    );
                }
            }
        }
    }
}

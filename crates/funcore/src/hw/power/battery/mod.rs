//! The fuel gauge and low voltage cutoff: the coulomb counter's charge out since full, against the charge from full to cutoff.

use crate::base::lines::numbers;
use crate::base::lines::value;
use crate::base::text::to_tenths;
use crate::base::time::MILLIS_PER_SECOND;
use crate::base::time::NANOS_PER_MS;
use crate::base::time::SECONDS_PER_MINUTE;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::ops::RangeInclusive;
use core::time::Duration;

mod meter;
#[cfg(test)]
pub(super) mod tests;

pub use meter::{Actions, Cell, Meter, Reading, Recall, Record, Report, Shutdown, State, Step, Supply, Switch};

/// The gauge's directory in the data partition's private directory, beside the device's use.
pub const DIRECTORY: &str = ".r1metal/battery";
/// The gauge's file in that directory.
pub const FILE: &str = "gauge";
const LEVEL_SCALE: i64 = 1_000_000;
pub(super) const PERCENT: i64 = 100;
const CUTOFF_MV: i32 = 3400;
const CUTOFF_WINDOW: usize = 10;
const SAMPLE_PERIOD: Duration = Duration::from_secs(1);
const KERNEL_LEVEL_EMPTY: u32 = 2;
const NEAR_END_MV: i32 = 3500;
const NEAR_END_LEVEL: f64 = 0.05;
const LIGHT_CURRENT: f64 = 1500.0;
const EMPTY_LEVEL: f64 = 0.5 / PERCENT as f64;
const ASSUMED_CAPACITY: f64 = 5_000.0;
const CAPACITIES: RangeInclusive<f64> = 1000.0..=30_000.0;
const MAX_RUNS: f64 = 4.0;
const MINUTE_MS: u64 = SECONDS_PER_MINUTE * MILLIS_PER_SECOND;
const TAPER_MINUTES: usize = 10;
const CHARGER_MV: f64 = 4350.0;
const RISE_MV: f64 = 2.0;
const WARM_C: i32 = 45;
const FLAT_PART: f64 = 0.05;
const FLAT_RUN: u32 = 2;
const LOW_CURRENT: f64 = 150.0;
const MAX_HOLD_MINUTES: u32 = 180;
const SETTLE_MS: u64 = 30_000;
const SLEW_MS: u64 = 10_000;
// A photo took the cell to its cutoff at 8 % shown and the viewfinder's start browned it out at 6 %; 10 clears both by more than the shown level's 1.5 points.
const CAMERA_RESERVE: u8 = 10;
// Low under a seventh of a full battery: one bar of the seven its level is drawn in.
const LOW_PARTS: i64 = 7;
const TENTHS: i32 = 10;
const HOUR_MS: f64 = 3_600_000.0;
const FULL_STATUS: &str = "Full";
const CHARGING_STATUS: &str = "Charging";
const HOLD_STATUS: &str = "Not charging";
// The status of a kernel file that could not be read.
const UNKNOWN_STATUS: &str = "?";
/// The log line of a way down that went on before the battery's thread kept the gauge's record.
pub const UNSAVED_LINE: &str = "battery: what the gauge has measured is not kept in the time a way down waits";
const SUSPEND_FLOOR_MS: u64 = 100;
const OUT_KEY: &str = "out";
const CAPACITY_KEY: &str = "capacity";
const RUNS_KEY: &str = "runs";
const COUNTER_KEY: &str = "counter";
const BOOT_KEY: &str = "boot";
const CUT_KEY: &str = "cut";
// The boot a gauge keeps where the kernel's could not be read: no boot is the same as it.
const UNKNOWN_BOOT: &str = "?";

/// What the charger does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Charger {
    // No cable: the battery carries the device.
    Idle,
    Charging,
    // It has ended a charge.
    Done,
    // A cable is in and the charger holds off.
    Held,
}

impl Charger {
    const fn is_plugged(self) -> bool {
        !matches!(self, Charger::Idle)
    }
}

/// The charger's status from the kernel's file, `text` its text where it could be read.
pub fn to_status(text: Option<&str>) -> String {
    String::from(text.map_or(UNKNOWN_STATUS, str::trim))
}

/// The log line of where the cutoff lies.
pub fn to_cutoff_line() -> String {
    format!(
        "battery cutoff: while discharging, a mean of {CUTOFF_WINDOW} samples under {CUTOFF_MV} mV or the kernel's low battery level at {KERNEL_LEVEL_EMPTY}"
    )
}

// The charger's state from the kernel's word; none for a status that could not be read.
fn parse_charger(status: &str) -> Option<Charger> {
    match status {
        UNKNOWN_STATUS => None,
        FULL_STATUS => Some(Charger::Done),
        CHARGING_STATUS => Some(Charger::Charging),
        HOLD_STATUS => Some(Charger::Held),
        _ => Some(Charger::Idle),
    }
}

pub(crate) fn is_camera_reserve(level: u8, supply: Supply) -> bool {
    !supply.is_plugged() && level < CAMERA_RESERVE
}

pub(crate) fn is_low(level: u8, supply: Supply) -> bool {
    !supply.is_plugged() && i64::from(level) * LOW_PARTS < PERCENT
}

const fn to_suspend_ms(suspended_ns: u64) -> u64 {
    let ms = suspended_ns / NANOS_PER_MS;
    if ms < SUSPEND_FLOOR_MS { 0 } else { ms }
}

/// One reading of the hardware.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sample {
    /// Monotonic time in nanoseconds.
    pub at_ns: u64,
    /// Milliseconds the system was suspended since the sample before.
    pub suspend_ms: u64,
    /// Battery voltage in mV.
    pub voltage: i32,
    /// Battery current in 0.1 mA, positive while charging.
    pub current: i32,
    /// Coulomb counter in 0.1 mAh, rising while charging; the kernel starts it anew at every boot.
    pub charge: i32,
    /// Battery temperature in °C.
    pub temperature: i32,
    pub charger: Charger,
}

impl Sample {
    pub(super) const fn at_ms(&self) -> u64 {
        self.at_ns / NANOS_PER_MS
    }

    pub(crate) const fn current_ma(&self) -> i32 {
        self.current / TENTHS
    }

    // The sample's measures as the logs write them.
    pub(super) fn to_text(self) -> String {
        format!(
            "{} mV {} mA {} mAh {} C",
            self.voltage,
            to_tenths(self.current.into()),
            to_tenths(self.charge.into()),
            self.temperature
        )
    }
}

fn mean<'a>(samples: impl Iterator<Item = &'a (i32, i32)>) -> (f64, f64) {
    let (mut voltages, mut currents, mut count) = (0.0, 0.0, 0.0f64);
    for &(voltage, current) in samples {
        (voltages, currents, count) = (voltages + f64::from(voltage), currents + f64::from(current), count + 1.0);
    }
    (voltages / count.max(1.0), currents / count.max(1.0))
}

#[derive(Clone, Copy)]
struct Debounce {
    stable: Charger,
    latest: Charger,
    since_ms: u64,
}

impl Debounce {
    fn take(&mut self, charger: Charger, at_ms: u64) -> Charger {
        if charger != self.latest {
            (self.latest, self.since_ms) = (charger, at_ms);
        }
        if charger == Charger::Idle || at_ms.saturating_sub(self.since_ms) >= SETTLE_MS {
            self.stable = charger;
        }
        self.stable
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum End {
    Flat,
    Low,
    Long,
}

struct Taper {
    start_ms: u64,
    minutes: VecDeque<(f64, f64)>,
    minute: (u64, f64, f64, f64),
    hold_minutes: u32,
    flat_minutes: u32,
}

impl Taper {
    fn new(start_ms: u64) -> Taper {
        Taper {
            start_ms,
            minutes: VecDeque::with_capacity(2 * TAPER_MINUTES),
            minute: (0, 0.0, 0.0, 0.0),
            hold_minutes: 0,
            flat_minutes: 0,
        }
    }

    fn add_sample(&mut self, sample: &Sample) -> Option<End> {
        let index = sample.at_ms().saturating_sub(self.start_ms) / MINUTE_MS;
        let (at, current, voltage, count) = self.minute;
        let end = if index != at && count > 0.0 {
            self.minute = (index, 0.0, 0.0, 0.0);
            self.add_minute((current / count, voltage / count), sample.temperature)
        } else {
            None
        };
        self.minute.1 += f64::from(sample.current);
        self.minute.2 += f64::from(sample.voltage);
        self.minute.3 += 1.0;
        end
    }

    fn add_minute(&mut self, minute: (f64, f64), temperature: i32) -> Option<End> {
        if self.minutes.len() == 2 * TAPER_MINUTES {
            self.minutes.pop_front();
        }
        self.minutes.push_back(minute);
        let window = |skip: usize| {
            let (current, voltage) = self.minutes.iter().skip(skip).take(TAPER_MINUTES).fold(
                (0.0, 0.0),
                |(current, voltage), (minute_current, minute_voltage)| {
                    (current + minute_current, voltage + minute_voltage)
                },
            );
            (current / TAPER_MINUTES as f64, voltage / TAPER_MINUTES as f64)
        };
        if self.minutes.len() < 2 * TAPER_MINUTES {
            return None;
        }
        let (older, newer) = (window(0), window(TAPER_MINUTES));
        let is_held = newer.0 > 0.0 && newer.1 >= CHARGER_MV && newer.1 <= older.1 + RISE_MV && temperature < WARM_C;
        self.hold_minutes += u32::from(is_held);
        self.flat_minutes = if is_held && older.0 - newer.0 < FLAT_PART * newer.0 { self.flat_minutes + 1 } else { 0 };
        if self.flat_minutes >= FLAT_RUN {
            Some(End::Flat)
        } else if is_held && newer.0 < LOW_CURRENT {
            Some(End::Low)
        } else if self.hold_minutes >= MAX_HOLD_MINUTES {
            Some(End::Long)
        } else {
            None
        }
    }
}

/// What a gauge keeps across restarts.
#[derive(Clone, PartialEq, Debug)]
pub struct Memory {
    out: Option<f64>,
    capacity: f64,
    runs: f64,
    is_cut_off: bool,
    counter: i32,
    boot: String,
}

fn parse_number(text: &str, key: &str) -> Option<f64> {
    let [value] = numbers::<f64, 1>(text, key).ok()??;
    value.is_finite().then_some(value)
}

impl Memory {
    // None unless every value is there and possible, so the cell is counted anew.
    pub(crate) fn parse(text: &str) -> Option<Memory> {
        let out = match value(text, OUT_KEY) {
            Some(_) => Some(parse_number(text, OUT_KEY)?),
            None => None,
        };
        Some(Memory {
            out,
            capacity: parse_number(text, CAPACITY_KEY).filter(|capacity| CAPACITIES.contains(capacity))?,
            runs: parse_number(text, RUNS_KEY).filter(|runs| (0.0..=MAX_RUNS).contains(runs))?,
            is_cut_off: match value(text, CUT_KEY) {
                Some("1") => true,
                Some("0") | None => false,
                Some(_) => return None,
            },
            counter: value(text, COUNTER_KEY)?.parse().ok()?,
            boot: String::from(value(text, BOOT_KEY).filter(|boot| !boot.is_empty())?),
        })
    }

    /// One line for each value, its name first; the charge out only where it is known.
    pub fn text(&self) -> String {
        let mut text = String::new();
        if let Some(out) = self.out {
            let _ = writeln!(text, "{OUT_KEY} {out}");
        }
        let _ = writeln!(text, "{CAPACITY_KEY} {}\n{RUNS_KEY} {}", self.capacity, self.runs);
        let _ = writeln!(text, "{CUT_KEY} {}", u8::from(self.is_cut_off));
        let _ = writeln!(text, "{COUNTER_KEY} {}\n{BOOT_KEY} {}", self.counter, self.boot);
        text
    }
}

/// A gauge's figures: charges in 0.1 mAh, the mean of the newest currents in 0.1 mA, positive while charging.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Figures {
    pub out: Option<i64>,
    pub capacity: i64,
    pub is_measured: bool,
    pub current: i64,
}

impl Figures {
    pub(crate) fn time_to_goal(&self, goal: u8) -> Option<u64> {
        let to_go = self.out? - self.capacity * (PERCENT - i64::from(goal)) / PERCENT;
        if to_go <= 0 {
            return Some(0);
        }
        (self.current > 0).then(|| (to_go as f64 / self.current as f64 * HOUR_MS) as u64)
    }
}

struct Gauge {
    out: Option<f64>,
    capacity: f64,
    runs: f64,
    is_cut_off: bool,
    counter: Option<i32>,
    last_ms: Option<u64>,
    debounce: Debounce,
    taper: Option<Taper>,
    is_topped: bool,
    recent: VecDeque<(i32, i32)>,
    end_level: f64,
    clock_ms: u64,
    display: Option<(i64, u64)>,
    lines: Vec<String>,
}

impl Default for Gauge {
    fn default() -> Self {
        Gauge {
            out: None,
            capacity: ASSUMED_CAPACITY,
            runs: 0.0,
            is_cut_off: false,
            counter: None,
            last_ms: None,
            debounce: Debounce { stable: Charger::Idle, latest: Charger::Idle, since_ms: 0 },
            taper: None,
            is_topped: false,
            recent: VecDeque::with_capacity(CUTOFF_WINDOW),
            end_level: 1.0,
            clock_ms: 0,
            display: None,
            lines: Vec::new(),
        }
    }
}

impl Gauge {
    fn memory(&self, boot: &str) -> Memory {
        Memory {
            out: self.out,
            capacity: self.capacity,
            runs: self.runs,
            is_cut_off: self.is_cut_off,
            counter: self.counter.unwrap_or(0),
            boot: String::from(boot),
        }
    }

    fn recall(&mut self, memory: Memory, boot: &str, counter: i32) {
        let start = if boot != UNKNOWN_BOOT && memory.boot == boot { memory.counter } else { 0 };
        self.out = memory.out.map(|out| (out - f64::from(counter - start)).max(0.0));
        let capacity = if memory.runs > 0.0 { memory.capacity } else { ASSUMED_CAPACITY };
        (self.capacity, self.runs, self.is_cut_off) = (capacity, memory.runs, memory.is_cut_off);
        self.counter = Some(counter);
    }

    fn take_lines(&mut self) -> Vec<String> {
        core::mem::take(&mut self.lines)
    }

    fn figures(&self) -> Figures {
        Figures {
            out: self.out.map(|out| out as i64),
            capacity: self.capacity as i64,
            is_measured: self.runs > 0.0,
            current: mean(self.recent.iter()).1 as i64,
        }
    }

    fn update(&mut self, sample: &Sample) -> Option<u8> {
        // The counter counts through every suspend; a gauge begins with the boot, whose counter the kernel starts anew.
        let change = self.counter.map_or(0, |last| sample.charge - last);
        self.counter = Some(sample.charge);
        self.out = self.out.map(|out| (out - f64::from(change)).max(0.0));
        let awake_ms = self.last_ms.map_or(0, |last| sample.at_ms().saturating_sub(last));
        (self.last_ms, self.clock_ms) = (Some(sample.at_ms()), self.clock_ms + awake_ms + sample.suspend_ms);
        if self.recent.len() == CUTOFF_WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back((sample.voltage, sample.current));
        let charger = self.debounce.take(sample.charger, sample.at_ms());
        self.watch_for_full(sample, charger);
        self.track_near_end(charger);
        let Some(level) = self.level(charger) else {
            self.display = None;
            return None;
        };
        Some(self.slew(level))
    }

    fn watch_for_full(&mut self, sample: &Sample, charger: Charger) {
        if charger != Charger::Charging {
            (self.taper, self.is_topped) = (None, false);
            return;
        }
        if sample.suspend_ms > 0 {
            self.taper = None;
        }
        let end = self.taper.get_or_insert_with(|| Taper::new(sample.at_ms())).add_sample(sample);
        if let Some(end) = end.filter(|_| !self.is_topped) {
            (self.out, self.is_topped, self.is_cut_off) = (Some(0.0), true, false);
            let how = match end {
                End::Flat => "its current stopped falling".into(),
                End::Low => format!("its current fell under {} mA", LOW_CURRENT / f64::from(TENTHS)),
                End::Long => "the charger's voltage held the longest a charge is let to".into(),
            };
            self.lines.push(format!("full: {how}, at {:.1} mA", f64::from(sample.current) / TENTHS as f64));
        }
    }

    fn track_near_end(&mut self, charger: Charger) {
        let (voltage, current) = mean(self.recent.iter());
        if charger == Charger::Charging {
            self.end_level = 1.0;
        } else if self.recent.len() == CUTOFF_WINDOW
            && current < 0.0
            && current > -LIGHT_CURRENT
            && voltage < f64::from(NEAR_END_MV)
        {
            let share = (voltage - f64::from(CUTOFF_MV)) / f64::from(NEAR_END_MV - CUTOFF_MV);
            self.end_level = self.end_level.min(NEAR_END_LEVEL * share.max(0.0));
            if self.end_level < EMPTY_LEVEL {
                self.measure("empty by its voltage");
            }
        }
    }

    fn level(&self, charger: Charger) -> Option<i64> {
        let out = self.out?;
        let mut level = (1.0 - out / self.capacity).clamp(0.0, 1.0).min(self.end_level);
        if charger == Charger::Charging && !self.is_topped {
            level = level.min(1.0 - 1.0 / PERCENT as f64);
        }
        if self.end_level >= 1.0 && mean(self.recent.iter()).1 < 0.0 {
            level = level.max(1.0 / PERCENT as f64);
        }
        Some((level * LEVEL_SCALE as f64) as i64)
    }

    fn slew(&mut self, level: i64) -> u8 {
        let near_end = (NEAR_END_LEVEL * LEVEL_SCALE as f64) as i64;
        let display = match self.display {
            Some((last, _)) if level <= near_end && level < last => (level, self.clock_ms),
            Some((last, since)) => {
                let reach = (self.clock_ms.saturating_sub(since) / SLEW_MS) as i64 * (LEVEL_SCALE / PERCENT);
                (level.clamp(last - reach, last + reach), if reach > 0 { self.clock_ms } else { since })
            }
            None => (level, self.clock_ms),
        };
        self.display = Some(display);
        ((display.0 * PERCENT + LEVEL_SCALE / 2) / LEVEL_SCALE) as u8
    }

    fn verdict(&self, kernel_level: Option<u32>) -> Verdict {
        let (voltage, current) = mean(self.recent.iter());
        if current >= 0.0 {
            Verdict::Run
        } else if self.recent.len() == CUTOFF_WINDOW && voltage < f64::from(CUTOFF_MV) {
            Verdict::LowVoltage(voltage as i32)
        } else if kernel_level == Some(KERNEL_LEVEL_EMPTY) {
            Verdict::KernelLatch
        } else {
            Verdict::Run
        }
    }

    fn cut(&mut self, verdict: Verdict) {
        if verdict.reason().is_some() {
            self.measure("cut off");
        }
    }

    fn measure(&mut self, how: &str) {
        let Some(out) = self.out.filter(|_| !self.is_cut_off) else {
            return;
        };
        self.is_cut_off = true;
        if !CAPACITIES.contains(&out) {
            self.lines.push(format!("{how} {} mAh after full, which no cell gives: not taken", to_tenths(out as i64)));
            return;
        }
        self.runs = (self.runs + 1.0).min(MAX_RUNS);
        self.capacity += (out - self.capacity) / self.runs;
        self.lines.push(format!(
            "{how} {} mAh after full: {} mAh from full to cutoff",
            to_tenths(out as i64),
            to_tenths(self.capacity as i64)
        ));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    Run,
    LowVoltage(i32),
    KernelLatch,
}

impl Verdict {
    fn reason(self) -> Option<String> {
        match self {
            Verdict::Run => None,
            Verdict::LowVoltage(mean) => Some(format!("mean of {CUTOFF_WINDOW} samples {mean} mV")),
            Verdict::KernelLatch => Some("the kernel's low voltage latch is set".into()),
        }
    }
}

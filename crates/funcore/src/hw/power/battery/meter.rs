use super::{
    CUTOFF_MV, Charger, Figures, Gauge, Memory, SAMPLE_PERIOD, Sample, UNKNOWN_BOOT, parse_charger, to_suspend_ms,
};
use crate::base::handoff::Latch;
use crate::base::time::suspended_ns;
use crate::hw::power::charge::Charge;
use crate::hw::power::usage::Usage;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

const CHARGE_ON: &str = "0 0";
const CHARGE_OFF: &str = "0 1";
const CARRYING_ON_LINE: &str = "battery: carrying on from what the gauge kept on the data partition";
const MEMORY_UNREADABLE_LINE: &str =
    "battery: what the gauge kept is unreadable, measuring the cell and counting its use anew";
const USAGE_UNREADABLE_LINE: &str = "battery: what was kept of the device's use is unreadable, counting anew";

/// What feeds the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Supply {
    Battery,
    // A USB cable the device runs from while the battery is kept as it is.
    Cable,
    Charging,
    // On a charger that has ended its charge and tops the battery up.
    Charged,
}

impl Supply {
    pub(crate) const fn is_plugged(self) -> bool {
        !matches!(self, Supply::Battery)
    }
}

/// What the power screen tells of the cell; `goal` is the percent the cable charges to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub sample: Sample,
    pub figures: Figures,
    pub usage: Usage,
    pub goal: u8,
}

/// The battery thread's reading: the level, none before the first full charge, the kernel's status and the cell.
#[derive(Clone, PartialEq, Debug)]
pub struct Reading {
    pub(crate) level: Option<u8>,
    pub(crate) status: String,
    is_held: bool,
    pub(crate) cell: Cell,
    // The screen shows it: the partition that holds what the gauge counted has settled. A sleep and a cable take every reading.
    pub(crate) is_shown: bool,
}

impl Reading {
    fn summary(&self) -> String {
        format!("{} {}", self.percent(), self.status)
    }

    fn percent(&self) -> String {
        self.level.map_or_else(|| String::from("-%"), |level| format!("{level}%"))
    }

    pub(crate) const fn is_under_cutoff(&self) -> bool {
        self.cell.sample.voltage < CUTOFF_MV
    }

    pub(crate) const fn supply(&self) -> Supply {
        match (self.cell.sample.charger, self.is_held) {
            (Charger::Idle, _) => Supply::Battery,
            // The charger takes its time to say that it has stopped; the picture does not wait for it.
            (Charger::Held, _) | (Charger::Charging | Charger::Done, true) => Supply::Cable,
            (Charger::Charging, false) => Supply::Charging,
            (Charger::Done, false) => Supply::Charged,
        }
    }

    fn to_log_line(&self) -> String {
        format!("{} {} {}", self.cell.sample.to_text(), self.percent(), self.status)
    }

    fn measures(&self) -> String {
        let sample = self.cell.sample;
        format!("{} mV {:+} mA {} C", sample.voltage, sample.current_ma(), sample.temperature)
    }
}

/// What the meter saves on the data partition.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Record {
    pub memory: Option<Memory>,
    pub usage: Option<Usage>,
}

/// The newest reading since it was last taken.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Report {
    pub reading: Option<Reading>,
}

/// What the battery's thread and the main loop share.
#[derive(Default)]
pub struct State {
    pub is_hosted: bool,
    is_touched: bool,
    // The saves the main loop asked for before a way down, and the last the thread made.
    asked_saves: u64,
    saved: u64,
    pub report: Report,
}

impl State {
    /// Takes a finger on the battery.
    pub const fn touch(&mut self) {
        self.is_touched = true;
    }

    const fn take_touch(&mut self) -> Option<bool> {
        if core::mem::replace(&mut self.is_touched, false) { Some(self.is_hosted) } else { None }
    }

    /// Asks the thread to save its record for a coming way down; the number its save is told by.
    pub const fn ask_save(&mut self) -> u64 {
        self.asked_saves += 1;
        self.asked_saves
    }

    const fn save_due(&self) -> Option<u64> {
        if self.saved < self.asked_saves { Some(self.asked_saves) } else { None }
    }

    /// Notes the save of that number made.
    pub const fn note_saved(&mut self, number: u64) {
        self.saved = number;
    }

    /// True once the save of that number is made.
    pub const fn is_saved(&self, number: u64) -> bool {
        self.saved >= number
    }

    /// Takes the report whole.
    pub fn take_report(&mut self) -> Report {
        core::mem::take(&mut self.report)
    }
}

/// The way down an empty battery takes: the log line and what is saved first.
#[derive(Clone, PartialEq, Debug)]
pub struct Shutdown {
    pub line: String,
    pub record: Record,
}

/// The kernel's charge switch where a cable's decision changed, with its log line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Switch {
    is_on: bool,
    pub line: String,
}

impl Switch {
    /// The text of the kernel's file for the charge.
    pub const fn to_text(&self) -> &'static str {
        if self.is_on { CHARGE_ON } else { CHARGE_OFF }
    }
}

/// What the battery's thread does until the next sample.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    // Saves the record for a way down, the save of that number (`State::note_saved`).
    Save(u64),
    // Answers a finger on the battery, with whether a host has the device on its port.
    Touch(bool),
    Wait(Duration),
    Sample,
}

/// What follows a look at the gauge's file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recall {
    // Nothing follows; the log line where there is one.
    Done(Option<&'static str>),
    // The usage file is read next; the log line comes first.
    Usage(&'static str),
}

/// What the meter made of a sample, in the order the battery thread carries it out.
#[derive(Clone, PartialEq, Debug)]
pub struct Actions {
    /// A log line for each thing the gauge learned.
    pub lines: Vec<String>,
    /// The way down where the cutoff finds the battery empty; it comes first.
    pub shutdown: Option<Shutdown>,
    /// The kernel's switch where a cable's decision changed.
    pub switch: Option<Switch>,
    /// The reading's line of the power log.
    pub log_line: String,
    /// The log line and the snapshot's name where the level changed.
    pub level: Option<(String, String)>,
    /// True where the meter's record is to be saved.
    pub should_save: bool,
    /// The reading for the sleep and the screen, which shows it once the data partition has settled.
    pub reading: Reading,
}

/// The battery thread's state from one sample to the next.
pub struct Meter {
    boot: String,
    gauge: Gauge,
    charge: Charge,
    switch: Latch<bool>,
    usage: Usage,
    last_level: Option<u8>,
    is_recalled: bool,
    saved_usage: Option<Usage>,
    reading: Option<Reading>,
    due_ns: u64,
    clocks: Option<(u64, u64)>,
    trouble: Latch<String>,
    charger: Charger,
}

impl Meter {
    /// A meter yet to take its first sample in the kernel's boot `boot`, none where it could not be read.
    pub fn new(boot: Option<String>) -> Meter {
        Meter {
            boot: boot.unwrap_or_else(|| UNKNOWN_BOOT.into()),
            gauge: Gauge::default(),
            charge: Charge::default(),
            switch: Latch::default(),
            usage: Usage::default(),
            charger: Charger::Idle,
            last_level: None,
            is_recalled: false,
            saved_usage: None,
            reading: None,
            due_ns: 0,
            clocks: None,
            trouble: Latch::default(),
        }
    }

    /// Sets the monotonic time the first sample is due at.
    pub const fn set_due(&mut self, now_ns: u64) {
        self.due_ns = now_ns;
    }

    /// Notes a pass that ended at `now_ns`: the next sample is a period after this one was due, never in the past.
    pub const fn note_sampled(&mut self, now_ns: u64) {
        let due_ns = self.due_ns + SAMPLE_PERIOD.as_nanos() as u64;
        self.due_ns = if due_ns > now_ns { due_ns } else { now_ns };
    }

    /// Takes the clock that stops in a suspend and the one that runs through it; returns the suspended milliseconds.
    pub fn note_clocks(&mut self, running_ns: u64, whole_ns: u64) -> u64 {
        let suspend_ms = self.clocks.map_or(0, |last| to_suspend_ms(suspended_ns(last, (running_ns, whole_ns))));
        self.clocks = Some((running_ns, whole_ns));
        suspend_ms
    }

    /// What the charger does by the kernel's status: one that could not be read leaves it as it last stood.
    pub fn charger_of(&mut self, status: &str) -> Charger {
        self.charger = parse_charger(status).unwrap_or(self.charger);
        self.charger
    }

    /// Takes why the battery could not be read; the same where it is to be logged, once until a reading comes.
    pub fn note_unread(&mut self, reason: String) -> Option<String> {
        self.trouble.change(reason)
    }

    /// The thread's next step before the sample due.
    pub fn next_step(&self, state: &mut State, now_ns: u64) -> Step {
        if let Some(number) = state.save_due() {
            return Step::Save(number);
        }
        if let Some(is_hosted) = state.take_touch() {
            return Step::Touch(is_hosted);
        }
        match self.due_ns.saturating_sub(now_ns) {
            0 => Step::Sample,
            wait => Step::Wait(Duration::from_nanos(wait)),
        }
    }

    /// True once, with the partition ready, for reading what an earlier run saved.
    pub const fn is_recall_due(&self, is_ready: bool) -> bool {
        !self.is_recalled && is_ready
    }

    /// Carries on from the gauge's file, the coulomb counter reading `counter`; a gauge counted anew counts the use anew.
    pub fn recall_memory(&mut self, file: Option<&[u8]>, counter: i32) -> Recall {
        let memory = file.and_then(|bytes| core::str::from_utf8(bytes).ok()).and_then(Memory::parse);
        let is_found = memory.is_some();
        self.recall(memory, counter);
        if is_found { Recall::Usage(CARRYING_ON_LINE) } else { Recall::Done(file.map(|_| MEMORY_UNREADABLE_LINE)) }
    }

    fn recall(&mut self, memory: Option<Memory>, counter: i32) {
        self.is_recalled = true;
        if let Some(memory) = memory {
            self.gauge.recall(memory, &self.boot, counter);
        }
    }

    /// Carries on from the usage file at the UTC second `utc`; the log line where it is unreadable.
    pub fn recall_usage(&mut self, file: Option<&[u8]>, utc: u64) -> Option<&'static str> {
        let bytes = file?;
        let Some((usage, saved_utc)) = core::str::from_utf8(bytes).ok().and_then(Usage::parse) else {
            return Some(USAGE_UNREADABLE_LINE);
        };
        self.usage.recall(usage, utc.saturating_sub(saved_utc));
        None
    }

    /// What the meter would save now.
    pub fn record(&self) -> Record {
        Record { memory: self.is_recalled.then(|| self.gauge.memory(&self.boot)), usage: self.saved_usage }
    }

    /// Takes a sample with the kernel's status as the logs tell it and its low battery level, whether a host has the device and whether the partition has settled.
    pub fn take(
        &mut self,
        sample: Sample,
        status: String,
        kernel_level: Option<u32>,
        is_hosted: bool,
        is_settled: bool,
    ) -> Actions {
        self.trouble.forget();
        // The gauge takes the sample and the cutoff's verdict on it before what it keeps is kept: a power off keeps what the very last one taught.
        let level = self.gauge.update(&sample);
        let verdict = self.gauge.verdict(kernel_level);
        self.gauge.cut(verdict);
        let lines = self.gauge.take_lines();
        let shutdown = verdict.reason().map(|reason| Shutdown {
            line: format!(
                "battery empty at {} mV, {:+} mA: {reason}; powering off",
                sample.voltage,
                sample.current_ma()
            ),
            record: self.record(),
        });
        let (is_plugged, figures) = (sample.charger.is_plugged(), self.gauge.figures());
        let switch = self.steer(is_plugged, is_hosted, level);
        // The device's use goes by stretches between a cable put in and pulled, counted where the level is.
        let has_new_stretch = self.usage.update(&sample, is_plugged, level, figures.out);
        if self.is_recalled {
            self.saved_usage = Some(self.usage);
        }
        let is_held = self.charge.is_held(is_plugged);
        let cell = Cell { sample, figures, usage: self.usage, goal: self.charge.goal() };
        let reading = Reading { level, status, is_held, cell, is_shown: is_settled };
        let has_lines = !lines.is_empty();
        let mut actions = Actions {
            lines: lines.into_iter().map(|line| format!("battery: {line}")).collect(),
            shutdown,
            switch,
            log_line: reading.to_log_line(),
            level: None,
            should_save: false,
            reading: reading.clone(),
        };
        // Nothing is told or shown before the gauge has recalled what it counted, while the partition that holds it is being brought up.
        if is_settled {
            // A level is told once for every change; what is kept is kept for that, for what the gauge learned, and where a stretch of use began.
            let is_new_level = level.is_some_and(|level| self.last_level.replace(level) != Some(level));
            let summary = reading.summary();
            actions.level = is_new_level
                .then(|| (format!("battery: {summary}, {}", reading.measures()), format!("battery {summary}")));
            actions.should_save = self.is_recalled && (is_new_level || has_lines || has_new_stretch);
            self.reading = Some(reading);
        }
        actions
    }

    /// Takes a finger on the battery, answered at once by the reading that shows; none where nothing shows or the touch changes nothing.
    pub fn touch(&mut self, is_hosted: bool) -> Option<(Option<Switch>, Reading)> {
        let reading = self.reading.as_ref()?;
        let (is_plugged, level) = (reading.cell.sample.charger.is_plugged(), reading.level);
        if !self.charge.touch(level) {
            return None;
        }
        let switch = self.steer(is_plugged, is_hosted, level);
        let (is_held, goal) = (self.charge.is_held(is_plugged), self.charge.goal());
        let reading = self.reading.as_mut()?;
        (reading.is_held, reading.cell.goal) = (is_held, goal);
        Some((switch, reading.clone()))
    }

    fn steer(&mut self, is_plugged: bool, is_hosted: bool, level: Option<u8>) -> Option<Switch> {
        let is_fed = self.charge.decide(is_plugged, is_hosted, level);
        let line = self.charge.to_log_line(is_plugged, is_fed, level)?;
        self.switch.change(is_fed)?;
        Some(Switch { is_on: is_fed, line: format!("battery: {line}") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::time::MILLIS_PER_SECOND;
    use crate::hw::power::battery::CUTOFF_WINDOW;
    use crate::hw::power::battery::KERNEL_LEVEL_EMPTY;
    use crate::hw::power::battery::UNKNOWN_STATUS;
    use crate::hw::power::battery::to_status;
    use crate::hw::power::battery::{CHARGING_STATUS, FULL_STATUS, HOLD_STATUS};
    use crate::hw::power::charge::CEILING;

    const BOOT: &str = "4d3c2b1a";
    const DISCHARGING: &str = "Discharging";

    // A sample and the status it was read with, as the battery's thread hands the meter the two.
    fn sample(meter: &mut Meter, second: u64, (voltage, current, charge): (i32, i32, i32), status: &str) -> Sample {
        let charger = meter.charger_of(status);
        super::super::tests::sample(second * MILLIS_PER_SECOND, voltage, current, charge, charger)
    }

    fn memory() -> Option<Memory> {
        Memory::parse("out 0\ncapacity 4000\nruns 1\ncounter 0\nboot 4d3c2b1a\n")
    }

    fn full() -> Meter {
        let mut meter = Meter::new(Some(BOOT.into()));
        meter.recall(memory(), 0);
        meter
    }

    fn draw(meter: &mut Meter, second: u64) -> Actions {
        let sample = sample(meter, second, (3390, -1000, -3600), DISCHARGING);
        meter.take(sample, DISCHARGING.into(), None, false, true)
    }

    fn on_cable(meter: &mut Meter, second: u64, status: &str, is_hosted: bool) -> Actions {
        let sample = sample(meter, second, (4300, 0, -100), status);
        meter.take(sample, status.into(), None, is_hosted, true)
    }

    #[test]
    fn reading_lines() {
        let sample = super::super::tests::sample(0, 4325, -1234, 0, Charger::Done);
        let cell = Cell {
            sample,
            figures: Figures { out: Some(0), capacity: 3600, is_measured: true, current: -1234 },
            usage: Usage::default(),
            goal: 100,
        };
        let reading = Reading { level: Some(100), status: FULL_STATUS.into(), is_held: false, cell, is_shown: true };
        assert_eq!(reading.summary(), "100% Full");
        assert_eq!(reading.measures(), "4325 mV -123 mA 25 C");
        assert_eq!(reading.to_log_line(), "4325 mV -123.4 mA 0.0 mAh 25 C 100% Full", "the power log's line");
        assert!(!reading.is_under_cutoff());
        let with = |charger: Charger, is_held: bool| Reading {
            cell: Cell { sample: Sample { charger, ..sample }, ..cell },
            is_held,
            ..reading.clone()
        };
        assert_eq!(
            [Charger::Done, Charger::Charging, Charger::Idle, Charger::Held]
                .map(|charger| with(charger, false).supply()),
            [Supply::Charged, Supply::Charging, Supply::Battery, Supply::Cable]
        );
        assert_eq!(
            [Charger::Charging, Charger::Done, Charger::Held].map(|charger| with(charger, true).supply()),
            [Supply::Cable; 3],
            "a battery the firmware keeps as it is shows so before the charger says that it has stopped"
        );
        assert_eq!(with(Charger::Idle, true).supply(), Supply::Battery, "no cable, whatever was held");
        let under = Reading { cell: Cell { sample: Sample { voltage: 3399, ..sample }, ..cell }, ..reading.clone() };
        assert!(under.is_under_cutoff(), "a voltage under the cutoff");
        let unknown = Reading { level: None, status: CHARGING_STATUS.into(), ..reading };
        assert_eq!(
            (unknown.summary(), unknown.to_log_line()),
            ("-% Charging".into(), "4325 mV -123.4 mA 0.0 mAh 25 C -% Charging".into()),
            "before the first full charge the level is not told"
        );
    }

    #[test]
    fn cutoff_keeps_verdict() {
        let mut meter = full();
        for second in 1..CUTOFF_WINDOW as u64 {
            assert_eq!(draw(&mut meter, second).shutdown, None, "{second} s: a window not yet full");
        }
        let before = meter.record();
        let actions = draw(&mut meter, CUTOFF_WINDOW as u64);
        let shutdown = actions.shutdown.expect("the mean of a whole window under the cutoff");
        assert_eq!(shutdown.line, "battery empty at 3390 mV, -100 mA: mean of 10 samples 3390 mV; powering off");
        assert!(
            actions.lines.iter().any(|line| line.starts_with("battery: empty by its voltage 360.0 mAh after full")),
            "{:?}",
            actions.lines
        );
        let cut = shutdown.record.memory.clone().expect("a meter that recalled keeps its gauge");
        assert!(
            cut.is_cut_off && cut.runs == 2.0 && cut.capacity == 3800.0,
            "kept as the verdict on this very sample left the gauge: {cut:?}"
        );
        assert_eq!(
            shutdown.record.usage, before.usage,
            "the use is counted after the way down, which keeps it as it stood"
        );
        assert_eq!(meter.record().memory, Some(cut), "what is kept from then on has the cut too");
        assert_ne!(meter.record().usage, before.usage, "and the use counted with the sample");

        let mut unrecalled = Meter::new(Some(BOOT.into()));
        for second in 1..CUTOFF_WINDOW as u64 {
            draw(&mut unrecalled, second);
        }
        let mut late = Meter::new(Some(BOOT.into()));
        for second in 1..CUTOFF_WINDOW as u64 {
            draw(&mut late, second);
        }
        late.recall(memory(), -3600);
        let (none, first) = (
            draw(&mut unrecalled, CUTOFF_WINDOW as u64).shutdown.map(|shutdown| shutdown.record),
            draw(&mut late, CUTOFF_WINDOW as u64).shutdown.map(|shutdown| shutdown.record),
        );
        assert_eq!(none, Some(Record::default()), "nothing is kept before the partition is there to keep it on");
        assert!(
            first.is_some_and(|record| record.memory.is_some() && record.usage.is_none()),
            "the use is kept from its first count after the recall on"
        );
    }

    #[test]
    fn unread_status_keeps_charger() {
        let mut meter = Meter::new(Some(BOOT.into()));
        let statuses =
            [UNKNOWN_STATUS, DISCHARGING, CHARGING_STATUS, UNKNOWN_STATUS, FULL_STATUS, HOLD_STATUS, UNKNOWN_STATUS];
        assert_eq!(
            statuses.map(|status| meter.charger_of(status)),
            [
                Charger::Idle,
                Charger::Idle,
                Charger::Charging,
                Charger::Charging,
                Charger::Done,
                Charger::Held,
                Charger::Held
            ],
            "a status that could not be read leaves the charger as it last stood, on no cable before the first"
        );
        assert_eq!(
            (to_status(None), to_status(Some("Charging\n"))),
            (UNKNOWN_STATUS.into(), CHARGING_STATUS.into()),
            "a file that could not be read is the status that leaves it so, one read is its word"
        );
        let mut lifted = full();
        on_cable(&mut lifted, 1, CHARGING_STATUS, true);
        lifted.touch(true).expect("a touch at the ceiling charges to full");
        let unread = on_cable(&mut lifted, 2, UNKNOWN_STATUS, true);
        assert_eq!(
            (unread.switch, unread.reading.supply(), unread.reading.cell.goal, unread.reading.status.as_str()),
            (None, Supply::Charging, 100, UNKNOWN_STATUS),
            "a sample whose status could not be read pulls no cable: the touch's goal stands, and the log tells the word"
        );
    }

    #[test]
    fn latch_cuts_off() {
        let mut meter = full();
        let sample = sample(&mut meter, 1, (3700, -1000, 0), DISCHARGING);
        let actions = meter.take(sample, DISCHARGING.into(), Some(KERNEL_LEVEL_EMPTY), false, true);
        assert_eq!(
            actions.shutdown.map(|shutdown| shutdown.line),
            Some("battery empty at 3700 mV, -100 mA: the kernel's low voltage latch is set; powering off".into()),
            "the latch while discharging is the cutoff, wherever the voltage stands"
        );
    }

    #[test]
    fn switch_changes() {
        let switch = |meter: &mut Meter, second: u64, status: &str, is_hosted: bool| {
            on_cable(meter, second, status, is_hosted).switch
        };
        let charging = |goal: u8| Some(Switch { is_on: true, line: format!("battery: charging to {goal}%") });
        let mut meter = Meter::new(Some(BOOT.into()));
        assert_eq!(switch(&mut meter, 1, DISCHARGING, false), None, "no cable, nothing to switch");
        assert_eq!(switch(&mut meter, 2, CHARGING_STATUS, false), charging(100), "a cable put in charges");
        assert_eq!(switch(&mut meter, 3, CHARGING_STATUS, false), None, "and the same decision is not told again");
        assert_eq!(switch(&mut meter, 4, DISCHARGING, false), None, "a cable pulled leaves the switch as it is");
        assert_eq!(switch(&mut meter, 5, CHARGING_STATUS, true), None, "which the next cable that charges finds so");
        let mut held = full();
        assert_eq!(
            switch(&mut held, 1, CHARGING_STATUS, true),
            Some(Switch { is_on: false, line: "battery: kept as it is at 98%, the device runs on the cable".into() }),
            "a battery over the ceiling on a computer's port is kept as it is"
        );
        assert_eq!(switch(&mut held, 2, CHARGING_STATUS, true), None);
        assert_eq!(switch(&mut held, 3, DISCHARGING, true), None, "pulled, the switch stays off");
        assert_eq!(switch(&mut held, 4, CHARGING_STATUS, false), charging(100), "and the next cable has it charge");
    }

    #[test]
    fn unsettled_partition() {
        let mut meter = full();
        let take = |meter: &mut Meter, second: u64, is_settled: bool| {
            let sample = sample(meter, second, (4000, -100, 0), DISCHARGING);
            meter.take(sample, DISCHARGING.into(), None, false, is_settled)
        };
        let unsettled = take(&mut meter, 1, false);
        assert_eq!((&unsettled.level, unsettled.should_save, unsettled.reading.is_shown), (&None, false, false));
        assert_eq!(
            (unsettled.reading.cell.sample.voltage, unsettled.reading.supply()),
            (4000, Supply::Battery),
            "the sleep and the cable take the reading the screen does not show"
        );
        assert_eq!(
            unsettled.log_line, "4000 mV -10.0 mA 0.0 mAh 25 C 100% Discharging",
            "the power log has every reading"
        );
        assert_eq!(meter.touch(false), None, "and a touch finds nothing shown to answer with");
        let settled = take(&mut meter, 2, true);
        assert_eq!(
            settled.level,
            Some(("battery: 100% Discharging, 4000 mV -10 mA 25 C".into(), "battery 100% Discharging".into())),
            "the level is told once the partition has settled"
        );
        assert!(settled.should_save && settled.reading.is_shown && settled.reading.level == Some(100));
        let again = take(&mut meter, 3, true);
        assert_eq!((again.level, again.should_save), (None, false), "and once for every change");
        assert!(again.reading.is_shown, "while every reading is shown");
    }

    #[test]
    fn touch_at_ceiling() {
        let mut meter = full();
        let shown = on_cable(&mut meter, 1, CHARGING_STATUS, true).reading;
        assert_eq!((shown.supply(), shown.cell.goal), (Supply::Cable, CEILING), "kept at the ceiling");
        let (switch, reading) = meter.touch(true).expect("a touch asks for a full battery");
        assert_eq!(switch, Some(Switch { is_on: true, line: "battery: charging to 100%".into() }));
        let charging = Reading { is_held: false, cell: Cell { goal: 100, ..shown.cell }, ..shown.clone() };
        assert_eq!(reading, charging, "the reading that showed, charging");
        assert_eq!(reading.supply(), Supply::Charging);
        let mut charger = full();
        on_cable(&mut charger, 1, CHARGING_STATUS, false);
        assert_eq!(charger.touch(false), None, "a touch on a charger's battery changes nothing");
    }

    #[test]
    fn next_steps() {
        const SECOND_NS: u64 = 1_000_000_000;
        let (mut meter, mut state) = (Meter::new(Some(BOOT.into())), State::default());
        meter.set_due(5 * SECOND_NS);
        state.is_hosted = true;
        state.touch();
        assert_eq!(meter.next_step(&mut state, 4 * SECOND_NS), Step::Touch(true));
        assert_eq!(meter.next_step(&mut state, 4 * SECOND_NS), Step::Wait(SAMPLE_PERIOD));
        assert_eq!(meter.next_step(&mut state, 5 * SECOND_NS), Step::Sample);
        meter.note_sampled(5 * SECOND_NS + 1);
        assert_eq!(meter.next_step(&mut state, 5 * SECOND_NS + 1), Step::Wait(SAMPLE_PERIOD - Duration::from_nanos(1)));
        meter.note_sampled(9 * SECOND_NS);
        assert_eq!(meter.next_step(&mut state, 9 * SECOND_NS), Step::Sample, "a late pass is not made up for");
        let number = state.ask_save();
        state.touch();
        assert_eq!(meter.next_step(&mut state, 9 * SECOND_NS), Step::Save(number), "a way down's save goes first");
        assert!(!state.is_saved(number));
        state.note_saved(number);
        assert!(state.is_saved(number), "the way down is told");
        assert_eq!(meter.next_step(&mut state, 9 * SECOND_NS), Step::Touch(true), "and once");
    }

    #[test]
    fn recall_lines() {
        let file: &[u8] = b"out 0\ncapacity 4000\nruns 1\ncounter 0\nboot 4d3c2b1a\n";
        let mut meter = Meter::new(Some(BOOT.into()));
        assert!(meter.is_recall_due(true) && !meter.is_recall_due(false));
        assert_eq!(meter.recall_memory(Some(file), 0), Recall::Usage(CARRYING_ON_LINE));
        assert!(!meter.is_recall_due(true));
        assert_eq!(meter.recall_usage(None, 0), None);
        assert_eq!(meter.recall_usage(Some(b"saved"), 0), Some(USAGE_UNREADABLE_LINE));
        assert_eq!(meter.recall_usage(Some(b"saved 5\n"), 7), None);
        let mut fresh = Meter::new(Some(BOOT.into()));
        assert_eq!(fresh.recall_memory(None, 0), Recall::Done(None));
        assert_eq!(fresh.recall_memory(Some(&[0xff]), 0), Recall::Done(Some(MEMORY_UNREADABLE_LINE)));
    }

    #[test]
    fn unread_boot_is_another() {
        let kept = |boot: Option<&str>, kept_boot: &str| {
            let mut meter = Meter::new(boot.map(String::from));
            let file = format!("out 1000\ncapacity 4000\nruns 1\ncounter -500\nboot {kept_boot}\n");
            meter.recall(Memory::parse(&file), -600);
            meter.record().memory.expect("a meter that recalled keeps its gauge")
        };
        assert_eq!(
            kept(Some(BOOT), BOOT).out,
            Some(1100.0),
            "in the boot its file was kept in, the counter goes on from the one kept"
        );
        assert_eq!(kept(Some(BOOT), "0a1b2c3d").out, Some(1600.0), "in another boot the counter began anew");
        assert_eq!(kept(None, BOOT).out, Some(1600.0), "a boot that could not be read is another boot");
        let unread = kept(None, UNKNOWN_BOOT);
        assert_eq!(unread.out, Some(1600.0), "and not the one a file was kept in whose boot could not be read either");
        assert!(unread.text().ends_with("boot ?\n"), "its file tells the boot as unread: {}", unread.text());
    }

    #[test]
    fn trouble_once() {
        let mut meter = Meter::new(Some(BOOT.into()));
        assert_eq!(meter.note_unread("gone".into()), Some("gone".into()));
        assert_eq!(meter.note_unread("gone".into()), None);
        draw(&mut meter, 1);
        assert_eq!(meter.note_unread("gone".into()), Some("gone".into()));
    }

    #[test]
    fn clock_gap() {
        let mut meter = Meter::new(Some(BOOT.into()));
        assert_eq!(meter.note_clocks(0, 0), 0);
        assert_eq!(meter.note_clocks(1_000, 1_000), 0);
        assert_eq!(meter.note_clocks(2_000, 2_000_002_000), 2_000);
    }

    #[test]
    fn switch_text() {
        let switch = |is_on: bool| Switch { is_on, line: String::new() }.to_text();
        assert_eq!((switch(true), switch(false)), ("0 0", "0 1"));
    }
}

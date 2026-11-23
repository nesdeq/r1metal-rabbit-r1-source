use crate::base::text::to_tenths;
use crate::base::time::{MILLIS_PER_SECOND, MINUTES_PER_HOUR, SECONDS_PER_MINUTE};
use crate::hw::power::battery::{Cell, Supply, is_low};
use alloc::format;
use alloc::string::String;
use core::fmt::Write;

pub(crate) const MEASURING: &str = "MEASURING";
const NO_COUNT: &str = "NOT COUNTED YET";
const AFTER_FULL_CHARGE: &str = "KNOWN AFTER A FULL CHARGE";
const ON_THE_CABLE: &str = "RUNS ON THE CABLE";
const MS_PER_MINUTE: u64 = MILLIS_PER_SECOND * SECONDS_PER_MINUTE;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Line {
    Battery,
    TimeLeft,
    Cable,
    Awake,
    Charge,
    Cell,
    Capacity,
}

pub(crate) const LINES: [Line; 7] =
    [Line::Battery, Line::TimeLeft, Line::Cable, Line::Awake, Line::Charge, Line::Cell, Line::Capacity];

fn to_span(ms: u64) -> String {
    let minutes = ms / MS_PER_MINUTE;
    match minutes / MINUTES_PER_HOUR {
        0 => format!("{minutes} MIN"),
        hours => format!("{hours} H {} MIN", minutes % MINUTES_PER_HOUR),
    }
}

fn to_cable_word(cell: &Cell) -> String {
    let Some(stretch) = cell.usage.stretch else {
        return MEASURING.into();
    };
    let state = if stretch.is_plugged { "IN" } else { "OFF" };
    let level = stretch.level.map_or_else(String::new, |level| format!(", FROM {level}%"));
    format!("{state} FOR {}{level}", to_span(stretch.span_ms))
}

fn to_awake_word(cell: &Cell) -> String {
    cell.usage.stretch.map_or_else(
        || MEASURING.into(),
        |stretch| format!("{}, OF {}", to_span(stretch.awake_ms), to_span(stretch.span_ms)),
    )
}

fn to_charge_word(cell: &Cell) -> String {
    let Some(out) = cell.figures.out else {
        return NO_COUNT.into();
    };
    let is_plugged = cell.usage.stretch.is_some_and(|stretch| stretch.is_plugged);
    match cell.usage.charge_flow(Some(out)) {
        Some((charge, ma)) => {
            let is_drawn = charge < 0 || charge == 0 && !is_plugged;
            let direction = if is_drawn { "DRAWN" } else { "PUT IN" };
            format!("{} MAH {direction}, AT {} MA MEAN", to_tenths(charge.abs()), ma.abs())
        }
        None => MEASURING.into(),
    }
}

fn to_cell_word(cell: &Cell) -> String {
    let sample = cell.sample;
    format!("{} MV, {:+} MA, {} C", sample.voltage, sample.current_ma(), sample.temperature)
}

fn to_capacity_word(cell: &Cell) -> String {
    let capacity = to_tenths(cell.figures.capacity);
    if cell.figures.is_measured {
        format!("{capacity} MAH, FROM FULL TO CUTOFF")
    } else {
        format!("{capacity} MAH ASSUMED, NOT MEASURED YET")
    }
}

// The cell's charge as its readings last told it.
#[derive(Default)]
pub(crate) struct State {
    pub(crate) reading: Option<(u8, Supply)>,
    pub(crate) is_plugged: bool,
    pub(crate) is_low: bool,
    pub(crate) is_on_cable: bool,
    is_known: bool,
    is_charging: bool,
    is_full: bool,
}

// What a reading changed.
pub(crate) struct Change {
    pub(crate) was_known: bool,
    pub(crate) is_replugged: bool,
    pub(crate) has_filled: bool,
    pub(crate) is_level_new: bool,
    // The cell came to be low or no more, or the cable to hold it or no more.
    pub(crate) is_tint_new: bool,
}

impl State {
    // Takes whether a cable is in; true if that is new.
    pub(crate) fn plug(&mut self, is_plugged: bool) -> bool {
        core::mem::replace(&mut self.is_plugged, is_plugged) != is_plugged
    }

    pub(crate) fn take(&mut self, level: Option<u8>, supply: Supply) -> Change {
        let is_plugged = supply.is_plugged();
        let is_low = level.is_some_and(|level| is_low(level, supply));
        let (is_charging, is_on_cable) = (supply == Supply::Charging, supply == Supply::Cable);
        let was_known = core::mem::replace(&mut self.is_known, true);
        let has_filled = was_known && is_plugged && self.is_charging && !is_charging && !self.is_full;
        let is_replugged = self.plug(is_plugged);
        (self.is_charging, self.is_full) = (is_charging, (self.is_full || has_filled) && is_plugged);
        let is_low_new = core::mem::replace(&mut self.is_low, is_low) != is_low;
        let is_cable_new = core::mem::replace(&mut self.is_on_cable, is_on_cable) != is_on_cable;
        let is_level_new = self.reading.map(|reading| reading.0) != level;
        self.reading = level.map(|level| (level, supply));
        Change { was_known, is_replugged, has_filled, is_level_new, is_tint_new: is_low_new || is_cable_new }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Facts {
    pub(crate) reading: Option<(u8, Supply)>,
    pub(crate) is_low: bool,
    pub(crate) cell: Option<Cell>,
}

impl Facts {
    pub(crate) fn is_uncounted(&self) -> bool {
        self.cell.is_some_and(|cell| cell.figures.out.is_none())
    }

    pub(crate) fn write_charge(&self, text: &mut String) {
        let Some((level, supply)) = self.reading else {
            return text.push_str(if self.is_uncounted() { AFTER_FULL_CHARGE } else { MEASURING });
        };
        let goal = self.cell.map(|cell| cell.goal);
        let _ = match (supply, goal) {
            (Supply::Battery, _) if self.is_low => write!(text, "{level}%, ON THE BATTERY, LOW"),
            (Supply::Battery, _) => write!(text, "{level}%, ON THE BATTERY"),
            (Supply::Charging, Some(goal)) => write!(text, "{level}%, CHARGING TO {goal}%"),
            (Supply::Charging, None) => write!(text, "{level}%, CHARGING"),
            (Supply::Cable, _) => write!(text, "{level}%, HELD"),
            (Supply::Charged, _) => write!(text, "{level}%, CHARGED"),
        };
    }

    pub(crate) fn to_time_left_word(self, cell: &Cell, is_folded: bool) -> String {
        let (out, usable) = (cell.figures.out, Some(cell.figures.capacity));
        let or_measuring = |ms: Option<u64>, to_word: &dyn Fn(String) -> String| {
            ms.map_or_else(|| MEASURING.into(), |ms| to_word(to_span(ms)))
        };
        match self.reading.map(|reading| reading.1) {
            Some(Supply::Cable | Supply::Charged) => ON_THE_CABLE.into(),
            _ if out.is_none() => NO_COUNT.into(),
            Some(Supply::Charging) => match cell.figures.time_to_goal(cell.goal) {
                Some(0) => format!("NEARLY AT {}%", cell.goal),
                ms => or_measuring(ms, &|time| format!("ABOUT {time} TO {}%", cell.goal)),
            },
            _ if is_folded => {
                or_measuring(cell.usage.time_to_empty(out, usable), &|time| format!("ABOUT {time} LEFT, AS USED"))
            }
            _ => or_measuring(cell.usage.time_to_empty(out, usable), &|time| format!("ABOUT {time}, AS USED")),
        }
    }

    pub(crate) fn to_words(self, line: Line) -> (&'static str, String) {
        let from_cell = |word: &dyn Fn(&Cell) -> String| self.cell.map_or_else(|| MEASURING.into(), |cell| word(&cell));
        match line {
            Line::Battery => {
                let mut word = String::new();
                self.write_charge(&mut word);
                ("BATTERY", word)
            }
            Line::TimeLeft => ("TIME LEFT", from_cell(&|cell| self.to_time_left_word(cell, false))),
            Line::Cable => ("CABLE", from_cell(&to_cable_word)),
            Line::Awake => ("AWAKE", from_cell(&to_awake_word)),
            Line::Charge => ("CHARGE", from_cell(&to_charge_word)),
            Line::Cell => ("CELL", from_cell(&to_cell_word)),
            Line::Capacity => ("CAPACITY", from_cell(&to_capacity_word)),
        }
    }

    pub(crate) fn to_text(self) -> String {
        let to_line = |line: Line| {
            let (name, word) = self.to_words(line);
            format!("{name}: {word}")
        };
        LINES.map(to_line).join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::power::battery::{Charger, Figures, Sample};
    use crate::hw::power::usage::Usage;

    // Off the cable for 5 h 12 min from 80 %, 3 h 20 min of it awake, the charge out counted from the stretch's start, 360 mAh drawn in the last hour.
    const OFF: &str =
        "plugged 0\nlevel 80\nlasted 18720000\nawake 12000000\nfed 0\nfrom 0 0 0\ndraw 3600 3600000\nsaved 7\n";
    // On the cable for a second from 50 %, the charge out 60 mAh at its start.
    const ON: &str = "plugged 1\nlevel 50\nlasted 1000\nawake 1000\nfed 1000\nfrom 600 0 0\nsaved 7\n";

    fn cell(usage: &str, out: Option<i64>, is_measured: bool) -> Cell {
        let sample = Sample {
            at_ns: 0,
            suspend_ms: 0,
            voltage: 3982,
            current: -1234,
            charge: 0,
            temperature: 29,
            charger: Charger::Idle,
        };
        let figures = Figures { out, capacity: 4000, is_measured, current: 3600 };
        Cell { sample, figures, usage: Usage::parse(usage).expect("a usage").0, goal: 80 }
    }

    fn facts(reading: Option<(u8, Supply)>, cell: Option<Cell>) -> Facts {
        Facts { reading, is_low: false, cell }
    }

    #[test]
    fn lines_for_the_model() {
        let off = facts(Some((64, Supply::Battery)), Some(cell(OFF, Some(600), true)));
        assert_eq!(
            off.to_text(),
            "BATTERY: 64%, ON THE BATTERY\nTIME LEFT: ABOUT 56 MIN, AS USED\nCABLE: OFF FOR 5 H 12 MIN, FROM 80%\nAWAKE: 3 H 20 MIN, OF 5 H 12 MIN\nCHARGE: 60.0 MAH DRAWN, AT 11 MA MEAN\nCELL: 3982 MV, -123 MA, 29 C\nCAPACITY: 400.0 MAH, FROM FULL TO CUTOFF",
            "a line each in the screen's order: 340 mAh left at 360 mAh an hour"
        );
        assert_eq!(
            facts(None, None).to_text(),
            "BATTERY: MEASURING\nTIME LEFT: MEASURING\nCABLE: MEASURING\nAWAKE: MEASURING\nCHARGE: MEASURING\nCELL: MEASURING\nCAPACITY: MEASURING",
            "before the first reading"
        );
        assert_eq!([59_999, 60_000, 3_600_000].map(to_span), ["0 MIN", "1 MIN", "1 H 0 MIN"]);
    }

    #[test]
    fn charge_words() {
        let charge = |reading, cell, is_low| {
            let mut text = String::new();
            Facts { reading, is_low, cell }.write_charge(&mut text);
            text
        };
        let counted = Some(cell(OFF, Some(600), true));
        assert_eq!(
            [
                charge(Some((12, Supply::Battery)), counted, true),
                charge(Some((52, Supply::Charging)), counted, false),
                charge(Some((52, Supply::Charging)), None, false),
                charge(Some((80, Supply::Cable)), counted, false),
                charge(Some((100, Supply::Charged)), counted, false),
                charge(None, counted, false),
                charge(None, Some(cell(OFF, None, false)), false)
            ],
            [
                "12%, ON THE BATTERY, LOW",
                "52%, CHARGING TO 80%",
                "52%, CHARGING",
                "80%, HELD",
                "100%, CHARGED",
                "MEASURING",
                "KNOWN AFTER A FULL CHARGE"
            ],
            "the level with what is done with the cell, and the goal a cable charges to"
        );
        assert!(facts(None, Some(cell(OFF, None, false))).is_uncounted() && !facts(None, counted).is_uncounted());
        assert_eq!(
            [(ON, Some(600)), (OFF, Some(0)), (OFF, None), ("saved 7\n", Some(600))]
                .map(|(usage, out)| to_charge_word(&cell(usage, out, true))),
            ["0.0 MAH PUT IN, AT 0 MA MEAN", "0.0 MAH DRAWN, AT 0 MA MEAN", "NOT COUNTED YET", "MEASURING"],
            "nothing moved yet is put in on the cable and drawn off it"
        );
        assert_eq!(
            [to_cable_word(&cell(ON, Some(600), true)), to_capacity_word(&cell(ON, Some(600), false))],
            ["IN FOR 0 MIN, FROM 50%", "400.0 MAH ASSUMED, NOT MEASURED YET"]
        );
    }

    #[test]
    fn reading_changes() {
        let mut state = State::default();
        let mut take = |level, supply| {
            let change = state.take(level, supply);
            let flags =
                [change.was_known, change.is_replugged, change.has_filled, change.is_level_new, change.is_tint_new];
            (flags, state.reading, state.is_plugged, state.is_low)
        };
        assert_eq!(
            take(None, Supply::Battery),
            ([false; 5], None, false, false),
            "the first reading, with no level yet, changes nothing that shows"
        );
        assert_eq!(
            take(Some(14), Supply::Battery),
            ([true, false, false, true, true], Some((14, Supply::Battery)), false, true),
            "a level under a seventh off the cable is low"
        );
        assert_eq!(
            take(Some(14), Supply::Charging),
            ([true, true, false, false, true], Some((14, Supply::Charging)), true, false),
            "a cable put in is told, and on it nothing is low"
        );
        assert_eq!(
            take(Some(80), Supply::Cable).0,
            [true, false, true, true, true],
            "a charge that ends on the cable has filled the cell, which the cable holds from then on"
        );
        assert_eq!(take(Some(80), Supply::Charging).0, [true, false, false, false, true]);
        assert_eq!(
            take(Some(81), Supply::Charged).0,
            [true, false, false, true, false],
            "filled once, it fills no more while the cable stays in"
        );
        assert_eq!(take(Some(81), Supply::Battery).0, [true, true, false, false, false], "a cable pulled is told");
        assert_eq!(take(Some(81), Supply::Charging).0, [true, true, false, false, false]);
        assert_eq!(
            take(Some(82), Supply::Charged).0,
            [true, false, true, true, false],
            "and after it the cell fills anew"
        );
        assert!(state.plug(false) && !state.plug(false) && !state.is_plugged, "a cable alone is taken too");
    }

    #[test]
    fn time_left_by_supply() {
        let word = |supply, cell: Cell, is_folded| facts(Some((50, supply)), None).to_time_left_word(&cell, is_folded);
        let off = cell(OFF, Some(600), true);
        assert_eq!(
            [
                word(Supply::Cable, off, false),
                word(Supply::Charged, off, false),
                word(Supply::Battery, cell(OFF, None, false), false),
                word(Supply::Charging, off, false),
                word(Supply::Charging, cell(ON, Some(2600), true), false),
                word(Supply::Battery, off, true),
                word(Supply::Battery, cell(ON, Some(600), true), false)
            ],
            [
                "RUNS ON THE CABLE",
                "RUNS ON THE CABLE",
                "NOT COUNTED YET",
                "NEARLY AT 80%",
                "ABOUT 30 MIN TO 80%",
                "ABOUT 56 MIN LEFT, AS USED",
                "MEASURING"
            ],
            "held or charged nothing runs out; charging, the time to the goal: 180 mAh at 360 mA; off the cable the time as used, and none to tell by on it"
        );
    }
}

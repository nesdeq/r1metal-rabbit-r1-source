//! What the device has used since a cable last went in or came out, its draw off the cable, and the time to empty.

use super::battery::{Charger, PERCENT, Sample};
use crate::base::lines::numbers;
use crate::base::math::pow2_f64;
use crate::base::time::MILLIS_PER_SECOND;
use alloc::string::String;
use core::f64::consts::LOG2_E;
use core::fmt::Write;

/// The usage file in the battery's directory.
pub const FILE: &str = "usage";
const TENTH_MAH_IN_MA_MS: i64 = 360_000;
const DRAW_MS: f64 = 21_600_000.0;
const PLUGGED_KEY: &str = "plugged";
const LEVEL_KEY: &str = "level";
const LASTED_KEY: &str = "lasted";
const AWAKE_KEY: &str = "awake";
const FED_KEY: &str = "fed";
const FROM_KEY: &str = "from";
const DRAW_KEY: &str = "draw";
const SAVED_KEY: &str = "saved";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Baseline {
    out: i64,
    span_ms: u64,
    charge_ms: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Stretch {
    pub(crate) is_plugged: bool,
    pub(crate) level: Option<u8>,
    pub(crate) span_ms: u64,
    pub(crate) awake_ms: u64,
    charge_ms: u64,
    baseline: Option<Baseline>,
}

impl Stretch {
    fn charge_flow(&self, out: Option<i64>) -> Option<(i64, u64, u64)> {
        let (baseline, out) = (self.baseline?, out?);
        let flow = if self.is_plugged { baseline.out - out } else { out - baseline.out };
        Some((flow, self.span_ms.saturating_sub(baseline.span_ms), self.charge_ms.saturating_sub(baseline.charge_ms)))
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct Draw {
    charge: f64,
    time: f64,
}

impl Draw {
    fn add(&mut self, charge: f64, ms: f64) {
        let decay = pow2_f64(-ms / DRAW_MS * LOG2_E);
        (self.charge, self.time) = (self.charge * decay + charge, self.time * decay + ms);
    }

    fn rate(&self) -> Option<f64> {
        (self.charge > 0.0 && self.time > 0.0).then(|| self.charge / self.time)
    }
}

/// What a device has used: its stretch, its draw off the cable, and the last sample's counter and time.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Usage {
    pub(crate) stretch: Option<Stretch>,
    draw: Draw,
    counter: Option<i32>,
    last_ms: Option<u64>,
}

impl Usage {
    /// Takes a sample with whether a cable is attached, the level and the charge out since full; true if a stretch began.
    pub fn update(&mut self, sample: &Sample, is_plugged: bool, level: Option<u8>, out: Option<i64>) -> bool {
        let elapsed = self.last_ms.map_or(0, |last| sample.at_ms().saturating_sub(last));
        self.last_ms = Some(sample.at_ms());
        // Off the cable the device draws what the counter counts, awake and asleep.
        let drawn = self.counter.map_or(0.0, |last| f64::from(last - sample.charge));
        self.counter = Some(sample.charge);
        if !is_plugged {
            self.draw.add(drawn, (elapsed + sample.suspend_ms) as f64);
        }
        match &mut self.stretch {
            Some(stretch) if stretch.is_plugged == is_plugged => {
                let charge_ms = if sample.charger == Charger::Charging { elapsed } else { 0 };
                stretch.span_ms += elapsed + sample.suspend_ms;
                (stretch.awake_ms, stretch.charge_ms) = (stretch.awake_ms + elapsed, stretch.charge_ms + charge_ms);
                if stretch.baseline.is_none() {
                    stretch.baseline =
                        out.map(|out| Baseline { out, span_ms: stretch.span_ms, charge_ms: stretch.charge_ms });
                }
                false
            }
            slot => {
                let baseline = out.map(|out| Baseline { out, span_ms: 0, charge_ms: 0 });
                *slot = Some(Stretch { is_plugged, level, span_ms: 0, awake_ms: 0, charge_ms: 0, baseline });
                true
            }
        }
    }

    pub(crate) fn charge_flow(&self, out: Option<i64>) -> Option<(i64, i64)> {
        let stretch = self.stretch?;
        let (flow, span, charge_ms) = stretch.charge_flow(out)?;
        let added = if stretch.is_plugged { flow } else { -flow };
        let time = if added > 0 && charge_ms > 0 { charge_ms } else { span };
        (time > 0).then(|| (added, added * TENTH_MAH_IN_MA_MS / time as i64))
    }

    pub(crate) fn time_to_empty(&self, out: Option<i64>, usable: Option<i64>) -> Option<u64> {
        self.stretch.filter(|stretch| !stretch.is_plugged)?;
        let remaining = (usable? - out?).max(0) as f64;
        Some((remaining / self.draw.rate()? + 0.5) as u64)
    }

    pub(super) fn recall(&mut self, saved: Usage, gap_s: u64) {
        let gap = gap_s.saturating_mul(MILLIS_PER_SECOND);
        let stretch = saved.stretch.map(|stretch| Stretch { span_ms: stretch.span_ms.saturating_add(gap), ..stretch });
        (self.stretch, self.draw) = (stretch, saved.draw);
    }

    /// One line for each known value, its name first, and last the UTC second it is written at.
    pub fn text(&self, utc: u64) -> String {
        let mut text = String::new();
        if let Some(stretch) = self.stretch {
            let _ = writeln!(text, "{PLUGGED_KEY} {}", u8::from(stretch.is_plugged));
            if let Some(level) = stretch.level {
                let _ = writeln!(text, "{LEVEL_KEY} {level}");
            }
            let _ = writeln!(text, "{LASTED_KEY} {}", stretch.span_ms);
            let _ = writeln!(text, "{AWAKE_KEY} {}", stretch.awake_ms);
            let _ = writeln!(text, "{FED_KEY} {}", stretch.charge_ms);
            if let Some(baseline) = stretch.baseline {
                let _ = writeln!(text, "{FROM_KEY} {} {} {}", baseline.out, baseline.span_ms, baseline.charge_ms);
            }
        }
        let _ = writeln!(text, "{DRAW_KEY} {} {}", (self.draw.charge + 0.5) as i64, (self.draw.time + 0.5) as i64);
        let _ = writeln!(text, "{SAVED_KEY} {utc}");
        text
    }

    pub(crate) fn parse(text: &str) -> Option<(Usage, u64)> {
        let field = |key: &str| numbers::<i64, 1>(text, key).ok().map(|values| values.map(|[value]| value));
        let to_unsigned = |value: i64| u64::try_from(value).ok();
        let utc = to_unsigned(field(SAVED_KEY)??)?;
        let stretch =
            match (field(PLUGGED_KEY)?, field(LEVEL_KEY)?, field(LASTED_KEY)?, field(AWAKE_KEY)?, field(FED_KEY)?) {
                (None, None, None, None, None) => None,
                (Some(plugged @ 0..=1), level, Some(span), Some(awake), Some(charge))
                    if level.is_none_or(|level| (0..=PERCENT).contains(&level)) =>
                {
                    let (span_ms, awake_ms, charge_ms) =
                        (to_unsigned(span)?, to_unsigned(awake)?, to_unsigned(charge)?);
                    let baseline = match numbers::<i64, 3>(text, FROM_KEY).ok()? {
                        Some([out, span_at, charge_at]) => {
                            let (span_at, charge_at) = (to_unsigned(span_at)?, to_unsigned(charge_at)?);
                            (out >= 0 && span_at <= span_ms && charge_at <= charge_ms).then_some(())?;
                            Some(Baseline { out, span_ms: span_at, charge_ms: charge_at })
                        }
                        None => None,
                    };
                    (awake_ms <= span_ms && charge_ms <= span_ms).then_some(())?;
                    Some(Stretch {
                        is_plugged: plugged == 1,
                        level: level.map(|level| level as u8),
                        span_ms,
                        awake_ms,
                        charge_ms,
                        baseline,
                    })
                }
                _ => return None,
            };
        let draw = match numbers::<i64, 2>(text, DRAW_KEY).ok()? {
            Some([charge, time]) if time >= 0 => Draw { charge: charge as f64, time: time as f64 },
            Some(_) => return None,
            None => Draw::default(),
        };
        Some((Usage { stretch, draw, counter: None, last_ms: None }, utc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND_MS: u64 = 1000;
    const HOUR_MS: u64 = 3_600_000;

    fn is_near(actual: Option<u64>, ms: f64) -> bool {
        actual.is_some_and(|actual| (actual as f64 / ms - 1.0).abs() < 0.001)
    }

    fn sample(at_ms: u64, suspend_ms: u64, charger: Charger) -> Sample {
        Sample { suspend_ms, ..crate::hw::power::battery::tests::sample(at_ms, 3900, -1000, 0, charger) }
    }

    fn hour_off(usage: &mut Usage, at_ms: u64, charge: i32, tenths: i32, asleep_ms: u64) -> (u64, i32) {
        let (mut at, mut charge) = (at_ms, charge);
        for _ in 0..3600 {
            (at, charge) = (at + SECOND_MS - asleep_ms, charge - tenths);
            usage.update(&Sample { charge, ..sample(at, asleep_ms, Charger::Idle) }, false, Some(50), Some(0));
        }
        (at, charge)
    }

    #[test]
    fn stretches() {
        let mut usage = Usage::default();
        assert!(
            usage.update(&sample(0, 0, Charger::Charging), true, Some(70), Some(1200)),
            "the first sample begins one"
        );
        assert!(!usage.update(&sample(SECOND_MS, 0, Charger::Charging), true, Some(70), Some(1190)));
        assert!(
            !usage.update(&sample(2 * SECOND_MS, 0, Charger::Idle), true, Some(70), Some(1190)),
            "held on the cable"
        );
        let on = usage.stretch.unwrap();
        assert_eq!(
            (on.is_plugged, on.level, on.span_ms, on.awake_ms, on.charge_ms),
            (true, Some(70), 2 * SECOND_MS, 2 * SECOND_MS, SECOND_MS),
            "a charger fed the cell in one second of the two"
        );
        assert!(
            usage.update(&sample(3 * SECOND_MS, 0, Charger::Idle), false, Some(71), Some(1190)),
            "the cable comes out"
        );
        usage.update(&sample(4 * SECOND_MS, 60 * SECOND_MS, Charger::Idle), false, Some(71), Some(1192));
        let off = usage.stretch.unwrap();
        assert_eq!(
            (off.is_plugged, off.level, off.span_ms, off.awake_ms),
            (false, Some(71), 61 * SECOND_MS, SECOND_MS),
            "a second awake and a minute suspended"
        );
        assert_eq!(usage.charge_flow(Some(1192)), Some((-2, -2 * TENTH_MAH_IN_MA_MS / (61 * SECOND_MS) as i64)));
    }

    #[test]
    fn held_cell_draw() {
        let mut usage = Usage::default();
        usage.update(&sample(0, 0, Charger::Idle), true, Some(80), Some(1200));
        usage.update(&sample(SECOND_MS, 0, Charger::Charging), true, Some(80), Some(1200));
        for second in 2..=100 {
            usage.update(
                &sample(second * SECOND_MS, 0, Charger::Idle),
                true,
                Some(80),
                Some(1200 + second as i64 / 10),
            );
        }
        assert_eq!(
            usage.charge_flow(Some(1210)),
            Some((-10, -10 * TENTH_MAH_IN_MA_MS / (100 * SECOND_MS) as i64)),
            "1 mAh drawn in the 100 s held, at 36 mA, not -3600 mA put in over the one second the charger fed"
        );
        assert_eq!(
            usage.charge_flow(Some(1195)),
            Some((5, 5 * TENTH_MAH_IN_MA_MS / SECOND_MS as i64)),
            "what was put in is told over the time the charger fed"
        );
        let mut held = Usage::default();
        held.update(&sample(0, 0, Charger::Idle), true, Some(80), Some(1200));
        held.update(&sample(10 * SECOND_MS, 0, Charger::Idle), true, Some(80), Some(1199));
        assert_eq!(
            held.charge_flow(Some(1199)),
            Some((1, TENTH_MAH_IN_MA_MS / (10 * SECOND_MS) as i64)),
            "and over all the time where no charger fed"
        );
    }

    #[test]
    fn late_baseline() {
        let mut usage = Usage::default();
        usage.update(&sample(0, 0, Charger::Idle), false, Some(60), None);
        usage.update(&sample(HOUR_MS, 0, Charger::Idle), false, Some(60), None);
        assert_eq!(usage.charge_flow(None), None, "before the first full charge nothing is counted");
        usage.update(&sample(HOUR_MS + SECOND_MS, 0, Charger::Idle), false, Some(60), Some(500));
        usage.update(&sample(HOUR_MS + 11 * SECOND_MS, 0, Charger::Idle), false, Some(60), Some(510));
        assert_eq!(
            (usage.stretch.unwrap().baseline.unwrap().span_ms, usage.charge_flow(Some(510))),
            (HOUR_MS + SECOND_MS, Some((-10, -360))),
            "from the first counted sample: 1 mAh in ten seconds"
        );
    }

    #[test]
    fn time_left() {
        let mut usage = Usage::default();
        usage.update(&sample(0, 0, Charger::Idle), false, Some(100), Some(0));
        assert_eq!(usage.time_to_empty(Some(0), Some(4000)), None, "nothing drawn, nothing to tell by");
        let (at, charge) = hour_off(&mut usage, 0, 0, 1, 0);
        assert!(
            is_near(usage.time_to_empty(Some(3600), Some(4000)), HOUR_MS as f64 / 9.0),
            "360 mAh an hour awake: the 40 left in a ninth of an hour"
        );
        assert_eq!(usage.time_to_empty(Some(4100), Some(4000)), Some(0), "a cell drawn past empty has nothing left");
        assert_eq!(
            (usage.time_to_empty(None, Some(4000)), usage.time_to_empty(Some(0), None)),
            (None, None),
            "no count or no measured cell, no time"
        );
        let mut sleepy = Usage::default();
        sleepy.update(&sample(0, 0, Charger::Idle), false, Some(100), Some(0));
        hour_off(&mut sleepy, 0, 0, 1, 999);
        assert!(
            is_near(sleepy.time_to_empty(Some(0), Some(3600)), HOUR_MS as f64),
            "asleep all but a millisecond a second, the counter counting through every suspend: 360 mAh drawn in the hour, and 360 mAh more last another"
        );
        usage.update(&Sample { charge, ..sample(at + SECOND_MS, 0, Charger::Charging) }, true, Some(10), Some(3600));
        assert_eq!(usage.time_to_empty(Some(3600), Some(4000)), None, "on a cable nothing runs out");
    }

    #[test]
    fn draw_decay() {
        let mut usage = Usage::default();
        usage.update(&sample(0, 0, Charger::Idle), false, Some(100), Some(0));
        let (at, charge) = hour_off(&mut usage, 0, 0, 1, 0);
        // On the cable for a day nothing is drawn and nothing grows old.
        usage.update(&Sample { charge, ..sample(at + SECOND_MS, 0, Charger::Charging) }, true, Some(10), Some(0));
        usage.update(&Sample { charge, ..sample(at + 86_400_000, 0, Charger::Idle) }, true, Some(100), Some(0));
        usage.update(&Sample { charge, ..sample(at + 86_401_000, 0, Charger::Idle) }, false, Some(100), Some(0));
        assert!(
            is_near(usage.time_to_empty(Some(0), Some(3600)), HOUR_MS as f64),
            "the hour at 360 mA before the cable tells the rate"
        );
        let (_, _) = hour_off(&mut usage, at + 86_401_000, charge, 3, 0);
        let old = pow2_f64(-(HOUR_MS as f64) / DRAW_MS * LOG2_E);
        let rate = (3600.0 * old + 10_800.0) / (HOUR_MS as f64 * old + HOUR_MS as f64);
        assert!(
            is_near(usage.time_to_empty(Some(0), Some(14_400)), 14_400.0 / rate),
            "an hour at three times the draw weighs more than the hour before it, grown an hour old"
        );
    }

    #[test]
    fn text_round_trip() {
        let mut usage = Usage::default();
        usage.update(&sample(0, 0, Charger::Idle), false, Some(100), Some(0));
        let (at, charge) = hour_off(&mut usage, 0, 0, 1, 0);
        usage.update(&Sample { charge, ..sample(at + SECOND_MS, 0, Charger::Charging) }, true, Some(10), Some(3600));
        usage.update(
            &Sample { charge: charge + 3, ..sample(at + 2 * SECOND_MS, 0, Charger::Charging) },
            true,
            Some(10),
            Some(3597),
        );
        let text = usage.text(1_791_499_260);
        assert!(
            text.starts_with("plugged 1\nlevel 10\nlasted 1000\nawake 1000\nfed 1000\nfrom 3600 0 0\ndraw "),
            "{text}"
        );
        let (saved, second) = Usage::parse(&text).unwrap();
        assert_eq!((saved.stretch, second), (usage.stretch, 1_791_499_260));
        assert!(
            (saved.draw.rate().unwrap() / usage.draw.rate().unwrap() - 1.0).abs() < 0.001,
            "the draw as written, to the 0.1 mAh and ms"
        );
        let mut fresh = Usage::default();
        fresh.update(&sample(5 * SECOND_MS, 0, Charger::Charging), true, Some(11), Some(3590));
        fresh.recall(saved, 90);
        assert_eq!(
            (fresh.stretch.unwrap().span_ms, fresh.stretch.unwrap().awake_ms),
            (91 * SECOND_MS, SECOND_MS),
            "the minute and a half between the runs is in the stretch"
        );
        fresh.update(&sample(6 * SECOND_MS, 0, Charger::Charging), true, Some(11), Some(3589));
        assert_eq!(fresh.stretch.unwrap().span_ms, 92 * SECOND_MS, "and it is counted on from this run's samples");
        let mut off = Usage::default();
        off.recall(
            Usage::parse("plugged 0\nlevel 80\nlasted 0\nawake 0\nfed 0\ndraw 3600 3600000\nsaved 7\n").unwrap().0,
            3600,
        );
        off.update(&sample(0, 0, Charger::Idle), false, Some(80), Some(0));
        assert_eq!(
            (off.draw.charge, off.draw.time),
            (3600.0, HOUR_MS as f64),
            "the hour between the runs draws nothing: a restart's few seconds, or a device switched off"
        );
        assert_eq!(Usage::parse("saved 5\n"), Some((Usage::default(), 5)), "a usage of nothing yet");
        let unknown = Usage::parse("plugged 0\nlasted 0\nawake 0\nfed 0\nsaved 7\n").unwrap().0;
        assert_eq!(
            unknown.stretch.map(|stretch| stretch.level),
            Some(None),
            "a stretch begun before the level was known"
        );
    }

    #[test]
    fn refused_text() {
        let text = "plugged 0\nlevel 80\nlasted 5000\nawake 4000\nfed 0\nfrom 10 1000 0\ndraw 30 9000\nsaved 7\n";
        assert!(Usage::parse(text).is_some());
        for broken in [
            "",
            "plugged 0\nlevel 80\nlasted 5000\nawake 4000\nfed 0\n",
            "plugged 2\nlevel 80\nlasted 5000\nawake 4000\nfed 0\nsaved 7\n",
            "plugged 0\nlevel 101\nlasted 5000\nawake 4000\nfed 0\nsaved 7\n",
            "plugged 0\nlevel 80\nlasted 5000\nawake 6000\nfed 0\nsaved 7\n",
            "plugged 0\nlevel 80\nlasted 5000\nawake 4000\nsaved 7\n",
            "plugged 0\nlevel 80\nlasted 5000\nawake 4000\nfed 0\nfrom 10 6000 0\nsaved 7\n",
            "plugged 0\nlevel 80\nlasted 5000\nawake 4000\nfed 0\nfrom -1 1000 0\nsaved 7\n",
            "plugged 0\nlevel 80\nlasted 5000\nawake 4000\nfed 0\nfrom 10 1000\nsaved 7\n",
            "draw 30 -9000\nsaved 7\n",
            "draw 30 9000 1\nsaved 7\n",
            "saved -7\n",
            "saved many\n",
        ] {
            assert_eq!(Usage::parse(broken), None, "{broken:?}");
        }
    }
}

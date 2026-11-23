//! One sleep: when to suspend, when to stay up in the dark, what the suspends came to, and its log lines.

use crate::base::text::to_tenths;
use crate::base::time::NANOS_PER_MS;
use crate::base::time::NANOS_PER_SECOND;
use crate::hw::power::battery::Reading;
use alloc::format;
use alloc::string::String;

const LOOK_AFTER_NS: u64 = 2000 * NANOS_PER_MS;
const QUICK_RETRIES: u32 = 3;
const TENTH_NS: u64 = 100 * NANOS_PER_MS;
/// The log line of a sleep that begins.
pub const SCREEN_OFF_LINE: &str = "sleep: screen off";
/// The log line of a wake a cable brought.
pub const CABLE_LINE: &str = "wake: a cable is put in";

/// What the main loop does next for a sleep.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    // Flushes what waits to be written, then suspends where the parts are quiet still (`hand_back`).
    Suspend,
    // Asks the kernel whether anything keeps it awake.
    Ask,
}

/// The state of one sleep, from the screen going dark to the button waking the device.
pub struct Doze {
    is_held: bool,
    is_clear: bool,
    is_asked: bool,
    wake_count: Option<String>,
    refusals: u32,
    resume_ns: Option<u64>,
    start_ns: u64,
    suspends: u32,
    last_suspend_ns: u64,
    suspend_ns: u64,
}

impl Doze {
    pub(crate) const fn new(is_held: bool, is_plugged: bool, start_ns: u64) -> Doze {
        Doze {
            is_held,
            is_clear: !is_plugged,
            is_asked: false,
            wake_count: None,
            refusals: 0,
            resume_ns: None,
            start_ns,
            suspends: 0,
            last_suspend_ns: 0,
            suspend_ns: 0,
        }
    }

    const fn is_ready(&self) -> bool {
        !self.is_held && self.is_clear
    }

    // True, once, when the kernel is to be asked whether anything keeps it awake: its drivers know of a cable before any reading of the battery.
    const fn should_ask(&mut self, is_quiet: bool) -> bool {
        let should_ask = is_quiet && self.is_ready() && !self.is_asked && self.wake_count.is_none();
        self.is_asked |= should_ask;
        should_ask
    }

    /// Takes the kernel's count of wake events, none where it could not be read, which waits for the next look at the battery; one that comes while nothing is asked counts for nothing.
    pub fn take_answer(&mut self, count: Option<String>) {
        match count {
            _ if !self.is_asked => {}
            Some(count) => (self.is_asked, self.wake_count) = (false, Some(count)),
            None => (self.is_asked, self.is_clear) = (false, false),
        }
    }

    /// The kernel's count as a suspend begins, where the parts are quiet still after the flush; taken either way, it counts for that one suspend, and a suspend given up asks the kernel anew.
    pub fn hand_back(&mut self, is_quiet: bool) -> Option<String> {
        let should_suspend = self.should_suspend(is_quiet);
        self.wake_count.take().filter(|_| should_suspend)
    }

    /// True when the device should suspend now: quiet, nothing against it, and the kernel's count in hand.
    pub const fn should_suspend(&self, is_quiet: bool) -> bool {
        is_quiet && self.is_ready() && self.wake_count.is_some()
    }

    /// The main loop's next step for the sleep, by whether the parts are quiet; none while it waits.
    pub fn next_step(&mut self, is_quiet: bool) -> Option<Step> {
        if self.should_suspend(is_quiet) {
            return Some(Step::Suspend);
        }
        self.should_ask(is_quiet).then_some(Step::Ask)
    }

    pub(crate) const fn take_button(&mut self, is_down: bool) -> bool {
        self.is_held &= is_down;
        is_down
    }

    /// Notes a suspend that ended at `now_ns`; one that ran owes a look at the battery, one refused is retried at once `QUICK_RETRIES` times in a row.
    pub fn take_suspend(&mut self, is_refused: bool, now_ns: u64, suspend_ns: u64) {
        self.wake_count = None;
        if is_refused {
            self.refusals += 1;
            self.is_clear &= self.refusals <= QUICK_RETRIES;
        } else {
            self.refusals = 0;
            (self.is_clear, self.resume_ns) = (false, Some(now_ns));
            (self.suspends, self.last_suspend_ns, self.suspend_ns) =
                (self.suspends + 1, suspend_ns, self.suspend_ns + suspend_ns);
        }
    }

    pub(crate) fn take_reading(&mut self, reading: &Reading) -> Option<String> {
        let look = self.look(reading.is_under_cutoff(), reading.supply().is_plugged(), reading.cell.sample.at_ns);
        look.map(|after_ns| self.dark_look(reading, after_ns))
    }

    const fn look(&mut self, is_under_cutoff: bool, is_plugged: bool, at_ns: u64) -> Option<u64> {
        let keeps_up = is_under_cutoff || is_plugged;
        let Some(resume_ns) = self.resume_ns else {
            self.is_clear = !keeps_up;
            return None;
        };
        if at_ns < resume_ns + LOOK_AFTER_NS {
            self.is_clear &= !keeps_up;
            return None;
        }
        (self.is_clear, self.resume_ns) = (!keeps_up, None);
        Some(at_ns - resume_ns)
    }

    fn dark_look(&self, reading: &Reading, after_ns: u64) -> String {
        let seconds = |ns: u64| ns / NANOS_PER_SECOND;
        format!(
            "sleep: dark look {} s after the resume, {} s suspended before it and {} s in {} suspends since the screen went dark: {} {}",
            to_tenths((after_ns / TENTH_NS) as i64),
            seconds(self.last_suspend_ns),
            seconds(self.suspend_ns),
            self.suspends,
            reading.cell.sample.to_text(),
            reading.status
        )
    }

    pub(crate) fn to_wake_line(&self, now_ns: u64) -> String {
        let seconds = (now_ns.saturating_sub(self.start_ns) + self.suspend_ns) / NANOS_PER_SECOND;
        format!("wake: slept {seconds} s in {} suspends", self.suspends)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::power::battery::Meter;
    use crate::hw::power::battery::Sample;

    const MS: u64 = NANOS_PER_MS;
    const RESUME: u64 = 10_000 * MS;
    const LOOK: u64 = RESUME + LOOK_AFTER_NS;
    const SUSPEND: u64 = 5_401 * NANOS_PER_SECOND;
    const COUNT: &str = "4711\n";
    const DISCHARGING: &str = "Discharging";

    fn reading(at_ns: u64, voltage: i32, status: &str) -> Reading {
        let mut meter = Meter::new(Some("4d3c2b1a".into()));
        let charger = meter.charger_of(status);
        let sample = Sample { at_ns, ..crate::hw::power::battery::tests::sample(0, voltage, -352, 1234, charger) };
        meter.take(sample, status.into(), None, false, true).reading
    }

    #[test]
    fn dark_look_line() {
        let (last_suspend_ns, suspend_ns) = (5_401 * NANOS_PER_SECOND, 10_802 * NANOS_PER_SECOND + 999_999_999);
        let doze = Doze { suspends: 2, last_suspend_ns, suspend_ns, ..Doze::new(false, false, 0) };
        let line = doze.dark_look(&reading(7_302 * MS, 4127, DISCHARGING), 2_340 * MS);
        assert_eq!(
            line,
            "sleep: dark look 2.3 s after the resume, 5401 s suspended before it and 10802 s in 2 suspends since the screen went dark: 4127 mV -35.2 mA 123.4 mAh 25 C Discharging"
        );
        assert!(!line.contains('\n'), "one line");
    }

    #[test]
    fn suspend_counts() {
        let mut doze = Doze::new(false, false, 100 * NANOS_PER_SECOND);
        doze.take_suspend(false, RESUME, SUSPEND);
        let early = doze.take_reading(&reading(RESUME + MS, 4127, DISCHARGING));
        assert_eq!(early, None, "a reading of the wake's first moments is no look");
        let first = doze.take_reading(&reading(LOOK, 4127, DISCHARGING)).expect("the look the suspend owes");
        assert!(
            first.contains("2.0 s after the resume, 5401 s suspended before it and 5401 s in 1 suspends"),
            "{first}"
        );
        doze.take_suspend(true, LOOK, SUSPEND);
        doze.take_suspend(false, 2 * LOOK, 99 * NANOS_PER_SECOND);
        let second = doze.take_reading(&reading(2 * LOOK + LOOK_AFTER_NS, 4127, DISCHARGING)).expect("the next look");
        assert!(
            second.contains("99 s suspended before it and 5500 s in 2 suspends"),
            "a suspend the kernel refused counts for nothing: {second}"
        );
        assert_eq!(
            doze.to_wake_line(600 * NANOS_PER_SECOND),
            "wake: slept 6000 s in 2 suspends",
            "awake and suspended"
        );
    }

    #[test]
    fn button_hold() {
        assert!(Doze::new(false, false, 0).is_ready(), "sleep asked for without the button suspends at once");
        let mut doze = Doze::new(true, false, 0);
        doze.look(false, false, 0);
        assert!(!doze.is_ready(), "a look at the battery does not suspend under a held button");
        assert!(!doze.take_button(false) && doze.is_ready(), "it suspends once the button is let go");
        doze.take_suspend(false, RESUME, SUSPEND);
        assert!(doze.take_button(true));
    }

    #[test]
    fn look_after_resume() {
        let mut doze = Doze::new(false, false, 0);
        doze.take_suspend(false, RESUME, SUSPEND);
        assert!(!doze.is_ready());
        for (at, what) in [
            (RESUME - 5 * MS, "a reading that was taken before the suspend and read after it"),
            (RESUME + MS, "a reading from the first moments of the wake"),
            (LOOK - MS, "one a millisecond short of the time"),
        ] {
            assert_eq!(doze.look(false, false, at), None, "{what}");
            assert!(!doze.is_ready(), "{what} is no look");
        }
        assert_eq!(doze.look(false, false, LOOK + 700 * MS), Some(LOOK_AFTER_NS + 700 * MS), "after the resume");
        assert!(doze.is_ready());
        assert_eq!(doze.look(false, false, LOOK + 1_700 * MS), None, "the next reading is no second look");
        doze.take_suspend(false, 2 * LOOK, SUSPEND);
        assert!(!doze.is_ready());
        assert_eq!(doze.look(false, false, 2 * LOOK + LOOK_AFTER_NS), Some(LOOK_AFTER_NS), "each suspend owes one");
    }

    #[test]
    fn cable_or_low_cell() {
        let mut doze = Doze::new(false, false, 0);
        doze.take_suspend(false, RESUME, SUSPEND);
        doze.look(false, true, RESUME + 500 * MS);
        assert!(!doze.is_ready());
        assert_eq!(doze.look(false, false, LOOK), Some(LOOK_AFTER_NS));
        assert!(doze.is_ready(), "the look decides: the cable it saw at the first moment is gone");
        doze.take_suspend(false, 2 * LOOK, SUSPEND);
        doze.look(true, false, 2 * LOOK + LOOK_AFTER_NS);
        assert!(!doze.is_ready(), "a battery under the cutoff keeps it up");
        doze.look(true, false, 2 * LOOK + LOOK_AFTER_NS + 1_000 * MS);
        assert!(!doze.is_ready());
        doze.look(false, false, 2 * LOOK + LOOK_AFTER_NS + 2_000 * MS);
        assert!(doze.is_ready(), "until it recovers");
    }

    #[test]
    fn stays_up_on_cable() {
        let mut doze = Doze::new(true, true, 0);
        doze.take_button(false);
        assert!(!doze.is_ready(), "the button is let go, but the cable is in");
        doze.look(false, true, 0);
        assert!(!doze.is_ready());
        doze.look(false, false, 1_000 * MS);
        assert!(doze.is_ready());
        doze.take_suspend(false, RESUME, SUSPEND);
        doze.look(false, true, LOOK);
        assert!(!doze.is_ready(), "and so does a cable put in while it sleeps");
        assert!(doze.take_button(true), "the button still wakes it");
    }

    #[test]
    fn refused_suspends() {
        let mut doze = Doze::new(false, false, 0);
        for refusals in 1..=QUICK_RETRIES {
            doze.take_suspend(true, RESUME, 0);
            assert!(doze.is_ready(), "refused {refusals} times in a row: nothing speaks against another try");
        }
        doze.take_suspend(true, RESUME, 0);
        assert!(!doze.is_ready(), "a kernel that refuses once more is left alone until the next look");
        assert_eq!(doze.look(false, false, RESUME + 1_000 * MS), None, "nothing ran, so no look at the cell is owed");
        assert!(doze.is_ready());
        doze.take_suspend(true, RESUME + 1_000 * MS, 0);
        assert!(!doze.is_ready(), "and every refusal after that waits for a look, as often as one comes");
        doze.look(false, false, RESUME + 2_000 * MS);
        doze.take_suspend(false, RESUME + 2_000 * MS, SUSPEND);
        doze.look(false, false, RESUME + 2_000 * MS + LOOK_AFTER_NS);
        doze.take_suspend(true, RESUME + 2_000 * MS + LOOK_AFTER_NS, 0);
        assert!(doze.is_ready(), "the count of refusals in a row begins again once a suspend has run");
    }

    #[test]
    fn kernel_count() {
        let mut doze = Doze::new(true, false, 0);
        assert!(!doze.should_ask(true) && !doze.should_suspend(true), "nothing is asked under a held button");
        doze.take_button(false);
        assert!(
            doze.is_ready() && !doze.should_suspend(true),
            "the firmware has no objection, the kernel has not spoken"
        );
        assert!(!doze.should_ask(false), "while something runs the kernel is not asked");
        assert!(doze.should_ask(true) && !doze.should_ask(true), "asked once nothing does");
        assert!(!doze.should_suspend(true));
        doze.take_answer(Some(COUNT.into()));
        assert!(
            !doze.should_suspend(false) && doze.should_suspend(true),
            "its word in hand, the suspend waits until nothing runs"
        );
        assert!(!doze.should_ask(true), "and nothing more is asked");
        assert_eq!(doze.hand_back(true).as_deref(), Some(COUNT), "the count as the kernel gave it, handed back");
        doze.take_suspend(false, RESUME, SUSPEND);
        assert!(
            !doze.should_suspend(true) && !doze.should_ask(true),
            "after running again a look at the battery comes first"
        );
        doze.look(false, false, LOOK);
        assert!(!doze.should_suspend(true) && doze.should_ask(true), "and then the kernel is asked anew");
        doze.take_answer(Some(COUNT.into()));
        assert!(doze.should_suspend(true));
    }

    #[test]
    fn cable_during_sleep() {
        let mut doze = Doze::new(false, false, 0);
        assert!(doze.should_ask(true));
        doze.take_answer(Some(COUNT.into()));
        doze.hand_back(true);
        doze.take_suspend(false, RESUME, SUSPEND);
        doze.look(false, false, LOOK);
        assert!(
            doze.is_ready() && doze.should_ask(true) && !doze.should_suspend(true),
            "the look says no cable, and the kernel, whose port knows better, keeps its answer"
        );
        doze.look(false, false, LOOK + 1_000 * MS);
        assert!(!doze.should_suspend(true) && !doze.should_ask(true), "however many looks say so");
        doze.look(false, true, LOOK + 2_000 * MS);
        assert!(!doze.is_ready() && !doze.should_suspend(true), "until the battery tells of the cable too");
        doze.look(false, false, LOOK + 3_000 * MS);
        doze.take_answer(Some(COUNT.into()));
        assert!(doze.should_suspend(true), "the cable pulled, the kernel answers and it suspends again");
    }

    #[test]
    fn count_taken_back() {
        let mut doze = Doze::new(false, false, 0);
        assert!(doze.should_ask(true));
        doze.take_answer(Some(COUNT.into()));
        doze.hand_back(true);
        assert!(
            !doze.should_suspend(true) && doze.is_ready() && doze.should_ask(true),
            "a word handed back that the kernel did not take: nothing was suspended, and no look at the battery is waited for"
        );
        assert_eq!(doze.hand_back(true), None, "and none is left to hand back");
        doze.take_answer(Some(COUNT.into()));
        doze.hand_back(true);
        doze.take_suspend(true, RESUME, 0);
        assert!(
            !doze.should_suspend(true) && doze.is_ready() && doze.should_ask(true),
            "a suspend the kernel broke off is asked about anew at once"
        );
        assert!(doze.take_button(true), "the button wakes it whatever the kernel says");
    }

    #[test]
    fn quiet_after_flush() {
        let mut doze = Doze::new(false, false, 0);
        assert_eq!(doze.next_step(false), None, "while something runs nothing is asked");
        assert_eq!((doze.next_step(true), doze.next_step(true)), (Some(Step::Ask), None), "asked once");
        doze.take_answer(Some(COUNT.into()));
        assert_eq!((doze.next_step(false), doze.next_step(true)), (None, Some(Step::Suspend)));
        assert_eq!(doze.hand_back(false), None, "a part that became busy during the flush: no suspend");
        assert_eq!(doze.next_step(false), None, "nor a question while it runs");
        assert_eq!(doze.next_step(true), Some(Step::Ask), "then the kernel is asked anew, its count spent");
        doze.take_answer(Some(COUNT.into()));
        assert_eq!(doze.next_step(true), Some(Step::Suspend));
        assert_eq!(doze.hand_back(true).as_deref(), Some(COUNT), "quiet still: the count goes back to the kernel");
        doze.take_suspend(true, RESUME, 0);
        assert_eq!(doze.next_step(true), Some(Step::Ask), "and a refused suspend is asked about anew at once");
        doze.take_answer(Some(COUNT.into()));
        doze.hand_back(true);
        doze.take_suspend(false, RESUME, SUSPEND);
        assert_eq!(doze.next_step(true), None, "one that ran waits for its look at the battery");
    }

    #[test]
    fn unanswered_kernel() {
        let mut doze = Doze::new(false, false, 0);
        assert!(doze.should_ask(true));
        doze.take_answer(None);
        assert!(
            !doze.should_ask(true) && !doze.should_suspend(true),
            "not at once, or a kernel that cannot be asked would be asked without end"
        );
        assert_eq!(doze.look(false, false, RESUME), None);
        assert!(doze.should_ask(true), "the next look sets it going again");
        doze.take_answer(Some(COUNT.into()));
        doze.take_answer(None);
        assert!(doze.should_suspend(true), "and a second answer to the one asking is for nobody");
    }
}

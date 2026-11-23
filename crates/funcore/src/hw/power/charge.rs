use alloc::format;
use alloc::string::String;

pub(super) const CEILING: u8 = 80;
const FULL: u8 = 100;
const RESUME: u8 = 5;

#[derive(Debug)]
pub(super) struct Charge {
    is_hosted: bool,
    is_lifted: bool,
    is_feeding: bool,
}

impl Default for Charge {
    fn default() -> Self {
        Charge { is_hosted: false, is_lifted: false, is_feeding: true }
    }
}

impl Charge {
    pub(super) const fn goal(&self) -> u8 {
        if self.is_hosted && !self.is_lifted { CEILING } else { FULL }
    }

    pub(super) fn decide(&mut self, is_plugged: bool, is_hosted: bool, level: Option<u8>) -> bool {
        if !is_plugged {
            // What was learned of a cable goes with it, and the next one finds a battery that takes a charge.
            *self = Charge::default();
            return true;
        }
        self.is_hosted |= is_hosted;
        let goal = self.goal();
        // A battery that has reached its goal is charged again only well under it.
        self.is_feeding = level.is_none_or(|level| if self.is_feeding { level < goal } else { level + RESUME <= goal });
        self.is_feeding
    }

    pub(super) const fn is_held(&self, is_plugged: bool) -> bool {
        is_plugged && !self.is_feeding
    }

    pub(super) fn touch(&mut self, level: Option<u8>) -> bool {
        let (is_kept, is_lifting) = (!self.is_lifted && !self.is_feeding, self.is_lifted && self.is_feeding);
        let is_touched = self.is_hosted && (is_kept || is_lifting) && level.is_some_and(|level| level < FULL);
        if is_touched {
            (self.is_lifted, self.is_feeding) = (is_kept, is_kept);
        }
        is_touched
    }

    pub(super) fn to_log_line(&self, is_plugged: bool, is_fed: bool, level: Option<u8>) -> Option<String> {
        is_plugged.then(|| match level.filter(|_| !is_fed) {
            Some(level) => format!("kept as it is at {level}%, the device runs on the cable"),
            None => format!("charging to {}%", self.goal()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charger_fills() {
        let mut charge = Charge::default();
        assert!(charge.decide(true, false, Some(79)) && charge.decide(true, false, Some(99)));
        assert!(!charge.is_held(true), "on its way up it is not held");
        assert!(!charge.decide(true, false, Some(100)), "full: it is kept as it is");
        assert!(charge.is_held(true) && !charge.is_held(false), "held, with a cable to run on");
        assert!(!charge.decide(true, false, Some(96)), "a few steps down are no reason to top it up");
        assert!(charge.decide(true, false, Some(95)), "five percent under full it is charged again");
        assert!(charge.decide(true, false, Some(99)) && !charge.decide(true, false, Some(100)), "up to full");
        assert_eq!(charge.goal(), 100);
        assert!(!charge.touch(Some(99)), "a full battery on a charger has nothing to ask for");
    }

    #[test]
    fn computer_port() {
        let mut charge = Charge::default();
        assert!(charge.decide(true, false, Some(85)), "nobody has spoken on the port yet: a charger");
        assert!(!charge.decide(true, true, Some(85)), "a host has: over the ceiling it is kept as it is");
        assert_eq!(charge.goal(), CEILING);
        assert!(!charge.decide(true, true, Some(76)) && charge.decide(true, true, Some(75)));
        assert!(charge.decide(true, true, Some(79)) && !charge.decide(true, true, Some(80)));
        assert!(!charge.decide(true, false, Some(90)), "a host that sleeps or restarts leaves a computer's port");
        assert!(charge.decide(false, false, Some(90)), "no cable: nothing holds the next charge back");
        assert!(!charge.is_held(true), "and what held the battery went with the cable");
        assert!(charge.decide(true, false, Some(90)), "and the next cable is a charger's until a host speaks");
        let mut fresh = Charge::default();
        assert!(
            fresh.decide(true, true, Some(79)) && !fresh.decide(true, true, Some(80)),
            "a cable just put in charges up to its goal from the percent under it too"
        );
    }

    #[test]
    fn touches() {
        let mut charge = Charge::default();
        assert!(charge.decide(true, true, Some(60)));
        assert!(!charge.touch(Some(60)), "a battery charged up to the ceiling has nothing to ask for");
        assert!(!charge.decide(true, true, Some(80)));
        assert!(charge.touch(Some(80)), "kept at the ceiling: a touch asks for a full one");
        assert_eq!(charge.goal(), 100);
        assert!(charge.decide(true, true, Some(80)) && charge.decide(true, true, Some(90)));
        assert!(charge.touch(Some(90)), "on its way to full a touch takes that back");
        assert_eq!(charge.goal(), CEILING);
        assert!(
            !charge.decide(true, true, Some(90)) && !charge.decide(true, true, Some(90)),
            "and it is kept as it stands, over the ceiling"
        );
        assert!(charge.touch(Some(90)), "kept there, a touch asks for a full one again");
        assert!(charge.decide(true, true, Some(90)) && charge.decide(true, true, Some(99)));
        assert!(charge.decide(true, true, Some(99)) && !charge.decide(true, true, Some(100)), "full: kept as it is");
        assert!(!charge.touch(Some(100)) && !charge.touch(Some(99)), "and a full one has nothing to ask for");
        assert!(!charge.decide(true, true, Some(99)));
        charge.decide(false, false, Some(99));
        assert!(!charge.decide(true, true, Some(93)), "a touch counts for one cable");
        assert!(
            charge.touch(Some(93)) && charge.decide(true, true, Some(93)),
            "kept over the ceiling is kept short too"
        );
    }

    #[test]
    fn cable_lines() {
        let mut charge = Charge::default();
        charge.decide(true, true, Some(60));
        assert_eq!(charge.to_log_line(true, true, Some(60)).as_deref(), Some("charging to 80%"));
        assert_eq!(
            charge.to_log_line(true, false, Some(80)).as_deref(),
            Some("kept as it is at 80%, the device runs on the cable")
        );
        assert_eq!(charge.to_log_line(false, true, Some(60)), None, "nothing of a cable where there is none");
    }

    #[test]
    fn uncounted_level() {
        let mut charge = Charge::default();
        assert!(charge.decide(true, true, None), "the gauge has to see the cell full once");
        assert!(charge.decide(true, false, None) && !charge.touch(None));
    }
}

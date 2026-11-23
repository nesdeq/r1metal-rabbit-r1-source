//! The radio's scans: a survey of the air, and a join's looks from where it joined before to every channel.

use crate::wifi::known::Known;
use crate::wifi::netlink::Channel;
use crate::wifi::places::Place;
use crate::wifi::places::Places;
use crate::wifi::places::scan_places;
use crate::wifi::scan::Network;
use crate::wifi::scan::Scan;
use alloc::vec;
use alloc::vec::Vec;

/// The outcome of a scan.
pub enum ScanOutcome {
    Found(Vec<Network>),
    GaveWay,
    Failed,
}

/// The radio as a search reaches it, each method one effect.
pub trait Scanner {
    // None where the kernel names no channels.
    fn channels(&mut self) -> Option<Vec<Channel>>;
    fn scan(&mut self, how: &Scan) -> ScanOutcome;
}

fn warm_up(scanner: &mut impl Scanner) -> bool {
    let first = scanner.channels().and_then(|channels| Scan::warmup_frequency(&channels));
    first.is_some_and(|frequency| matches!(scanner.scan(&Scan::Passive(vec![frequency])), ScanOutcome::GaveWay))
}

/// Listens on every channel, the chip warmed up first.
pub fn survey(scanner: &mut impl Scanner) -> ScanOutcome {
    if warm_up(scanner) {
        return ScanOutcome::GaveWay;
    }
    scanner.scan(&Scan::Passive(Vec::new()))
}

fn scan_active(scanner: &mut impl Scanner, known: &Known) -> Result<Option<Vec<Network>>, ScanOutcome> {
    let Some(how) = Scan::active(&scanner.channels().unwrap_or_default()) else {
        return Ok(None);
    };
    match scanner.scan(&how) {
        ScanOutcome::Found(networks) => Ok(known.is_settled_by(&networks).then_some(networks)),
        ScanOutcome::Failed => Ok(None),
        ScanOutcome::GaveWay => Err(ScanOutcome::GaveWay),
    }
}

fn look_on(scanner: &mut impl Scanner, frequencies: &[u32]) -> Result<Vec<Network>, ScanOutcome> {
    let channels = scanner.channels();
    match scanner.scan(&Scan::from_frequencies(frequencies, channels.as_deref())) {
        ScanOutcome::Found(networks) => Ok(networks),
        outcome => Err(outcome),
    }
}

/// Looks for `known` where it was joined before, then, unless the look is narrow, on every channel: the outcome, and whether the look was everywhere; with none known, nowhere.
pub fn find_known(scanner: &mut impl Scanner, known: &Known, places: &Places, is_narrow: bool) -> (ScanOutcome, bool) {
    if known.is_empty() {
        return (ScanOutcome::Found(Vec::new()), false);
    }
    let mut look_count = 0;
    let result = scan_places(known, places, |channels| {
        look_count += 1;
        look_on(scanner, channels)
    });
    match result {
        Ok(Some(networks)) => return (ScanOutcome::Found(networks), false),
        Err(outcome) => return (outcome, false),
        Ok(None) if is_narrow => return (ScanOutcome::Found(Vec::new()), false),
        // With no look made, the scan that follows would be the chip's first since it has power.
        Ok(None) if look_count == 0 && warm_up(scanner) => return (ScanOutcome::GaveWay, false),
        Ok(None) => {}
    }
    match scan_active(scanner, known) {
        Ok(Some(networks)) => (ScanOutcome::Found(networks), false),
        Ok(None) => (scanner.scan(&Scan::Passive(Vec::new())), true),
        Err(outcome) => (outcome, false),
    }
}

/// Looks for `known` where it was joined before alone: whether one of them is heard; none where the look gave way or could not be made.
pub fn hear_known(scanner: &mut impl Scanner, known: &Known, places: &Places) -> Option<bool> {
    match find_known(scanner, known, places, true).0 {
        ScanOutcome::Found(networks) => Some(known.find_default(&networks).is_some()),
        ScanOutcome::GaveWay | ScanOutcome::Failed => None,
    }
}

/// The place to keep for the last join's network, one of `known`, on its low band channel, where a look there hears it well.
pub fn to_low_band(scanner: &mut impl Scanner, known: &Known, places: &Places) -> Option<Place> {
    let place = places.last()?;
    let low_band = place.to_low_band()?;
    let networks = look_on(scanner, &[low_band.frequency]).ok()?;
    known.is_heard_well(&networks, &place.network).then_some(low_band)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::known::Entry;
    use crate::wifi::scan::tests::beacon;

    struct Air {
        channels: Option<Vec<Channel>>,
        answers: Vec<ScanOutcome>,
        scans: Vec<Scan>,
    }

    impl Air {
        fn new(answers: Vec<ScanOutcome>) -> Air {
            let channels = Some(vec![Channel { frequency: 2412, disabled: false, no_ir: false }]);
            Air { channels, answers, scans: Vec::new() }
        }
    }

    impl Scanner for Air {
        fn channels(&mut self) -> Option<Vec<Channel>> {
            self.channels.clone()
        }

        fn scan(&mut self, how: &Scan) -> ScanOutcome {
            self.scans.push(how.clone());
            self.answers.remove(0)
        }
    }

    fn found(networks: &[Network]) -> ScanOutcome {
        ScanOutcome::Found(networks.to_vec())
    }

    #[test]
    fn survey_warm_up() {
        let mut air = Air::new(vec![found(&[]), found(&[])]);
        assert!(matches!(survey(&mut air), ScanOutcome::Found(_)));
        assert_eq!(air.scans, [Scan::Passive(vec![2412]), Scan::Passive(Vec::new())]);
        let mut air = Air::new(vec![ScanOutcome::GaveWay]);
        assert!(matches!(survey(&mut air), ScanOutcome::GaveWay));
        assert_eq!(air.scans.len(), 1);
    }

    #[test]
    fn nothing_known() {
        let mut air = Air::new(Vec::new());
        let (outcome, is_everywhere) = find_known(&mut air, &Known::default(), &Places::default(), false);
        assert!(!is_everywhere && matches!(outcome, ScanOutcome::Found(networks) if networks.is_empty()));
        assert!(air.scans.is_empty(), "no network to look for, no look");
    }

    #[test]
    fn narrow_look_places() {
        let known = Known::parse("name home\npassword secret12\n");
        let mut air = Air::new(Vec::new());
        let (outcome, is_everywhere) = find_known(&mut air, &known, &Places::default(), true);
        assert!(!is_everywhere && matches!(outcome, ScanOutcome::Found(networks) if networks.is_empty()));
        assert!(air.scans.is_empty(), "no place, no look");
        let mut places = Places::default();
        let place =
            Place { network: Entry { name: "home".into(), is_locked: true }, frequency: 2412, other_band: None };
        places.add_if_known(place, &known);
        let mut air = Air::new(vec![found(&[]), found(&[])]);
        let (outcome, _) = find_known(&mut air, &known, &places, true);
        assert!(matches!(outcome, ScanOutcome::Found(networks) if networks.is_empty()));
        assert_eq!(air.scans.len(), 2, "where it joined before alone, the silent first look made again");
    }

    #[test]
    fn known_heard_at_places() {
        let known = Known::parse("name home\npassword secret12\n");
        let mut places = Places::default();
        let place =
            Place { network: Entry { name: "home".into(), is_locked: true }, frequency: 2412, other_band: None };
        places.add_if_known(place, &known);
        let mut air = Air::new(vec![found(&[beacon("home", 1, 2412, -50)])]);
        assert_eq!(hear_known(&mut air, &known, &places), Some(true));
        let mut air = Air::new(vec![found(&[]), found(&[beacon("cafe", 2, 2412, -40)])]);
        assert_eq!(hear_known(&mut air, &known, &places), Some(false), "another network there is none of them");
        assert_eq!(air.scans.len(), 2, "and no look goes past the places");
        let mut air = Air::new(vec![ScanOutcome::GaveWay]);
        assert_eq!(hear_known(&mut air, &known, &places), None, "a look that gave way says nothing");
        let mut air = Air::new(vec![ScanOutcome::Failed]);
        assert_eq!(hear_known(&mut air, &known, &places), None, "nor one that could not be made");
    }

    #[test]
    fn known_every_channel_fallback() {
        let known = Known::parse("name home\npassword secret12\n");
        let mut air = Air::new(vec![found(&[]), ScanOutcome::Failed, found(&[])]);
        let (outcome, is_everywhere) = find_known(&mut air, &known, &Places::default(), false);
        assert!(is_everywhere && matches!(outcome, ScanOutcome::Found(_)));
        assert_eq!(
            air.scans,
            [Scan::Passive(vec![2412]), Scan::Active(vec![2412]), Scan::Passive(Vec::new())],
            "no place to look at warms the chip up, and an active scan that fails leaves the passive one"
        );
    }

    #[test]
    fn known_settled_active_scan() {
        let known = Known::parse("name home\npassword secret12\n");
        let mut air = Air::new(vec![found(&[]), found(&[beacon("home", 1, 2412, -50)])]);
        let (outcome, is_everywhere) = find_known(&mut air, &known, &Places::default(), false);
        assert!(!is_everywhere && matches!(outcome, ScanOutcome::Found(_)));
        assert_eq!(air.scans.len(), 2);
    }

    #[test]
    fn low_band_signal() {
        let (known, mut places) = (Known::parse("name home\npassword secret12\n"), Places::default());
        let place =
            Place { network: Entry { name: "home".into(), is_locked: true }, frequency: 5180, other_band: Some(2412) };
        places.add_if_known(place, &known);
        let mut air = Air::new(vec![found(&[beacon("home", 1, 2412, -50)])]);
        assert_eq!(to_low_band(&mut air, &known, &places).map(|place| place.frequency), Some(2412));
        let mut air = Air::new(vec![found(&[beacon("home", 1, 2412, -80)])]);
        assert_eq!(to_low_band(&mut air, &known, &places), None);
        let mut air = Air::new(Vec::new());
        assert_eq!(to_low_band(&mut air, &known, &Places::default()), None);
        assert!(air.scans.is_empty(), "no place to turn, no look");
    }
}

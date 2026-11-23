//! The places known networks were joined at, their file text, and where a scan looks first.

use crate::wifi::known::Entry;
use crate::wifi::known::Known;
use crate::wifi::known::NAME;
use crate::wifi::known::Network;
use crate::wifi::known::is_plain;
use crate::wifi::scan;
use crate::wifi::scan::is_low_band;
use alloc::string::String;
use alloc::vec::Vec;

/// The file of the WiFi directory that holds the join places, one for each known network, the last join's first.
pub const PLACE_FILE: &str = "place";
const LOCKED: &str = "locked ";
const FREQUENCY: &str = "frequency ";
const OTHER: &str = "other ";
const YES: &str = "yes";
const NO: &str = "no";

fn find_on_other_band<'a>(
    known: &Network,
    access: &scan::Network,
    networks: &'a [scan::Network],
) -> Option<&'a scan::Network> {
    networks
        .iter()
        .filter(|network| known.is_heard_as(network) && is_low_band(network.frequency) != is_low_band(access.frequency))
        .max_by_key(|network| network.signal)
}

/// Where the device joined a known network: its access point's channel in MHz, and its channel on the other band if it has one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Place {
    pub network: Entry,
    pub frequency: u32,
    pub other_band: Option<u32>,
}

impl Place {
    fn parse(text: &str) -> Option<Place> {
        let mut lines = text.split('\n');
        let name = lines.next()?.strip_prefix(NAME).filter(|name| !name.is_empty() && is_plain(name))?;
        let is_locked = match lines.next()?.strip_prefix(LOCKED)? {
            YES => true,
            NO => false,
            _ => return None,
        };
        let frequency = lines.next()?.strip_prefix(FREQUENCY)?.parse().ok()?;
        let other_band = match lines.next() {
            None | Some("") => None,
            Some(line) => Some(line.strip_prefix(OTHER)?.parse().ok()?),
        };
        Some(Place { network: Entry { name: name.into(), is_locked }, frequency, other_band })
    }

    pub(super) fn to_low_band(&self) -> Option<Place> {
        let low_band = self.other_band.filter(|other| is_low_band(*other) && !is_low_band(self.frequency))?;
        Some(Place { network: self.network.clone(), frequency: low_band, other_band: Some(self.frequency) })
    }

    /// The log line of a place on the low band that was heard well.
    pub fn to_low_band_line(&self) -> String {
        alloc::format!("wifi: the network is heard well on {} MHz, the next join looks there first", self.frequency)
    }

    fn to_text(&self) -> String {
        let word = if self.network.is_locked { YES } else { NO };
        let other = self.other_band.map_or_else(String::new, |other| alloc::format!("{OTHER}{other}\n"));
        alloc::format!("{NAME}{}\n{LOCKED}{word}\n{FREQUENCY}{}\n{other}", self.network.name, self.frequency)
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct ScanPlan {
    last: Option<u32>,
    newer: Vec<u32>,
    other_bands: Vec<u32>,
    older: Vec<u32>,
}

/// The places the device joined its known networks at, one for each, the last join's first.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Places {
    places: Vec<Place>,
}

impl Places {
    /// The places of a file's text, each from its name line on; what is no place is skipped, and of two places of one network the first counts.
    pub fn parse(text: &str) -> Places {
        let mut records: Vec<String> = Vec::new();
        for line in text.split('\n') {
            match records.last_mut() {
                _ if line.starts_with(NAME) => records.push(line.into()),
                Some(record) => {
                    record.push('\n');
                    record.push_str(line);
                }
                None => {}
            }
        }
        let mut places = Places::default();
        for place in records.iter().filter_map(|record| Place::parse(record)) {
            if places.find(&place.network).is_none() {
                places.places.push(place);
            }
        }
        places
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        self.places.iter().map(Place::to_text).collect()
    }

    fn add(&mut self, place: Place) -> bool {
        if self.places.first() == Some(&place) {
            return false;
        }
        self.places.retain(|other| other.network != place.network);
        self.places.insert(0, place);
        true
    }

    pub(crate) fn add_if_known(&mut self, place: Place, known: &Known) -> bool {
        known.contains(&place.network) && self.add(place)
    }

    /// The place of a join of `network` that took `access` among `networks`; the other band's channel comes from the earlier place where the look was not on every channel.
    pub fn locate(
        &self,
        is_everywhere: bool,
        network: &Network,
        access: &scan::Network,
        networks: &[scan::Network],
    ) -> Place {
        let other_band = match self.find(&network.to_entry()).filter(|_| !is_everywhere) {
            None => find_on_other_band(network, access, networks).map(|access| access.frequency),
            Some(place) if place.frequency == access.frequency => place.other_band,
            Some(place) => Some(place.frequency),
        };
        Place { network: network.to_entry(), frequency: access.frequency, other_band }
    }

    /// Forgets the places of networks that are known no more; true if there was one.
    pub fn retain_known(&mut self, known: &Known) -> bool {
        let count = self.places.len();
        self.places.retain(|place| known.contains(&place.network));
        self.places.len() != count
    }

    fn find(&self, network: &Entry) -> Option<&Place> {
        self.places.iter().find(|place| place.network == *network)
    }

    /// The place of the last join.
    pub fn last(&self) -> Option<&Place> {
        self.places.first()
    }

    // True where a network of `known` has a place, which a join looks at first.
    pub(super) fn holds_known(&self, known: &Known) -> bool {
        self.last_known(known).is_some()
    }

    // The place of the last join of a network of `known`.
    fn last_known(&self, known: &Known) -> Option<&Place> {
        self.places.iter().find(|place| known.contains(&place.network))
    }

    /// The log text of the places: how many, and the channels of the last join's.
    pub fn to_log_line(&self) -> String {
        let last = self.last().map_or_else(String::new, |place| {
            let other =
                place.other_band.map_or_else(String::new, |other| alloc::format!(" and {other} MHz on the other band"));
            alloc::format!(", the last on {} MHz{other}", place.frequency)
        });
        alloc::format!("places of joins kept: {}{last}", self.places.len())
    }

    fn plan(&self, known: &Known) -> ScanPlan {
        let Some(last) = self.last_known(known) else {
            return ScanPlan::default();
        };
        let entries = known.entries();
        let at = entries.iter().position(|entry| *entry == last.network).unwrap_or(0);
        let places_of = |entries: &[Entry]| entries.iter().filter_map(|entry| self.find(entry)).collect::<Vec<_>>();
        let (newer, older) = (places_of(&entries[..at]), places_of(&entries[at + 1..]));
        let mut channels = alloc::vec![last.frequency];
        let mut take_new = |candidates: Vec<u32>| -> Vec<u32> {
            let mut unique = Vec::new();
            for channel in candidates {
                if !channels.contains(&channel) {
                    channels.push(channel);
                    unique.push(channel);
                }
            }
            unique
        };
        ScanPlan {
            last: Some(last.frequency),
            newer: take_new(newer.iter().map(|place| place.frequency).collect()),
            other_bands: take_new(
                [last].into_iter().chain(newer.iter().copied()).filter_map(|place| place.other_band).collect(),
            ),
            older: take_new(
                older.iter().flat_map(|place| [Some(place.frequency), place.other_band]).flatten().collect(),
            ),
        }
    }
}

pub(super) fn scan_places<E>(
    known: &Known,
    places: &Places,
    mut look: impl FnMut(&[u32]) -> Result<Vec<scan::Network>, E>,
) -> Result<Option<Vec<scan::Network>>, E> {
    let plan = places.plan(known);
    let Some(last) = plan.last else {
        return Ok(None);
    };
    let find_default = |networks: &[scan::Network]| known.find_default(networks).map(|(known, _)| known.to_entry());
    let mut visited = alloc::vec![last];
    let mut networks = look(&visited)?;
    if find_default(&networks) != known.first().map(Network::to_entry) {
        // Where networks chosen since the last join were joined, and the first channel once more if it was silent: the chip's first scan since it has power is short or deaf.
        let retry = find_default(&networks).is_none().then_some(last);
        let nearby: Vec<u32> = retry.into_iter().chain(plan.newer.iter().copied()).collect();
        if !nearby.is_empty() {
            networks = look(&nearby)?;
            visited.extend(nearby);
        }
    }
    for channels in [&plan.other_bands, &plan.older] {
        if find_default(&networks).is_none() && !channels.is_empty() {
            networks = look(channels)?;
            visited.extend(channels);
        }
    }
    // A network heard on 2.4 GHz below the floor is looked for on its channel of the other band too, and the better access point is the one to join.
    let other_band = known.find_default(&networks).and_then(|(known, access)| {
        let is_weak = is_low_band(access.frequency) && !access.is_low_power();
        let place = places.find(&known.to_entry())?;
        let channels = [Some(place.frequency), place.other_band].into_iter().flatten();
        channels.filter(|channel| is_weak && !is_low_band(*channel)).find(|channel| !visited.contains(channel))
    });
    if let Some(channel) = other_band {
        networks = look(&[channel])?;
    }
    Ok(find_default(&networks).is_some().then_some(networks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::scan::Security;
    use crate::wifi::scan::tests::beacon;
    use alloc::vec;

    fn network(name: &str, password: &str) -> Network {
        Network { name: name.into(), password: password.into() }
    }

    fn entry(name: &str, is_locked: bool) -> Entry {
        Entry { name: name.into(), is_locked }
    }

    fn joined_at(name: &str, frequency: u32, other_band: Option<u32>) -> Place {
        Place { network: entry(name, true), frequency, other_band }
    }

    #[test]
    fn other_band_access_point() {
        let known = Known::parse("name home\npassword secret12\nname cafe\npassword \n");
        let at = |last: u8, frequency: u32, signal: i32| beacon("home", last, frequency, signal);
        let home = known.first().unwrap();
        let around = [
            at(1, 2417, -60),
            at(2, 5240, -50),
            at(3, 5500, -45),
            at(4, 2462, -80),
            beacon("cafe", 5, 5745, -30),
            scan::Network { security: Security::Open, ..at(6, 5765, -20) },
        ];
        assert_eq!(
            find_on_other_band(home, &around[0], &around).map(|access| access.address[5]),
            Some(3),
            "the network's strongest access point on the other band, not another network's, nor one of that name that asks for nothing"
        );
        assert_eq!(find_on_other_band(home, &around[1], &around).map(|access| access.address[5]), Some(1));
        assert_eq!(
            find_on_other_band(home, &around[0], &around[..1]),
            None,
            "none where the network is heard on one band alone"
        );
    }

    #[test]
    fn place_text() {
        let place = Place { network: entry("home net", true), frequency: 5180, other_band: None };
        assert_eq!(place.to_text(), "name home net\nlocked yes\nfrequency 5180\n");
        assert_eq!(Place::parse(&place.to_text()), Some(place.clone()));
        let both = Place { other_band: Some(2417), ..place };
        assert_eq!(
            both.to_text(),
            "name home net\nlocked yes\nfrequency 5180\nother 2417\n",
            "with the network's channel on the other band, where it has one"
        );
        assert_eq!(Place::parse(&both.to_text()), Some(both));
        assert_eq!(
            Place::parse("name cafe\nlocked no\nfrequency 2412"),
            Some(Place { network: entry("cafe", false), frequency: 2412, other_band: None }),
            "a last line that was cut short counts"
        );
        for invalid in [
            "",
            "name \nlocked no\nfrequency 2412\n",
            "name x\nlocked maybe\nfrequency 2412\n",
            "name x\nlocked yes\nfrequency there\n",
            "name x\nfrequency 2412\nlocked yes\n",
            "locked yes\nfrequency 2412\n",
            "name a\tb\nlocked no\nfrequency 2412\n",
            "name x\nlocked yes\nfrequency 2412\nother there\n",
            "name x\nlocked yes\nfrequency 2412\nnoise\n",
        ] {
            assert_eq!(Place::parse(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn places_by_network() {
        let mut places = Places::default();
        assert!(places.to_text().is_empty() && places.last().is_none());
        assert!(places.add(joined_at("home", 2417, Some(5240))) && places.add(joined_at("road", 2437, None)));
        assert!(!places.add(joined_at("road", 2437, None)), "the last join's place as it stands changes nothing");
        assert!(places.add(joined_at("home", 5240, Some(2417))), "joined again, a network has one place, the new one");
        assert_eq!(
            (places.places.len(), places.last(), places.find(&entry("road", true)), places.find(&entry("road", false))),
            (2, Some(&joined_at("home", 5240, Some(2417))), Some(&joined_at("road", 2437, None)), None)
        );
        assert_eq!(
            places.to_text(),
            "name home\nlocked yes\nfrequency 5240\nother 2417\nname road\nlocked yes\nfrequency 2437\n"
        );
        assert_eq!(Places::parse(&places.to_text()), places);
        assert_eq!(
            Places::parse(
                "noise\nname home\nlocked yes\nfrequency 5240\nname x\nlocked perhaps\nname home\nlocked yes\nfrequency 1\n"
            ),
            Places { places: vec![joined_at("home", 5240, None)] },
            "what is no place is passed over, and of two places of one network the first counts"
        );
        assert_eq!(
            Places::parse("name cafe\nlocked no\nfrequency 2412"),
            Places { places: vec![Place { network: entry("cafe", false), frequency: 2412, other_band: None }] },
            "one place alone, its last line cut short, reads as it always did"
        );
        let known = Known::parse("name road\npassword 12345678\n");
        assert!(places.retain_known(&known) && !places.retain_known(&known));
        assert_eq!(
            places,
            Places { places: vec![joined_at("road", 2437, None)] },
            "of a network that is forgotten nothing stays"
        );
        assert!(!places.add_if_known(joined_at("home", 2417, None), &known), "nor is its place kept anew");
        assert!(
            places.add_if_known(joined_at("road", 5180, None), &known)
                && !places.add_if_known(joined_at("road", 5180, None), &known),
            "a known network's is, once"
        );
    }

    #[test]
    fn scan_plan() {
        let known = Known::parse(
            "name road\npassword 12345678\nname home\npassword secret12\nname work\npassword 12345678\nname new\npassword 12345678\n",
        );
        let mut places = Places::default();
        assert_eq!(places.plan(&known), ScanPlan::default(), "no place, nowhere to look first");
        assert!(!places.holds_known(&known));
        places.add(joined_at("work", 5180, Some(2412)));
        assert!(places.holds_known(&known) && !places.holds_known(&Known::default()), "a place of a known network");
        places.add(joined_at("road", 2437, None));
        places.add(joined_at("home", 2417, Some(5240)));
        assert_eq!(
            places.plan(&known),
            ScanPlan { last: Some(2417), newer: vec![2437], other_bands: vec![5240], older: vec![5180, 2412] },
            "home was joined last, road was chosen after it, work before; a network never joined has no place"
        );
        places.add(joined_at("road", 2437, None));
        assert_eq!(
            places.plan(&known),
            ScanPlan { last: Some(2437), newer: vec![], other_bands: vec![], older: vec![2417, 5240, 5180, 2412] },
            "joined last and chosen last: one channel first"
        );
        places.add(joined_at("work", 2437, Some(5240)));
        assert_eq!(
            places.plan(&known),
            ScanPlan { last: Some(2437), newer: vec![2417], other_bands: vec![5240], older: vec![] },
            "no channel is named twice"
        );
        let single = Known::parse("name home\npassword secret12\n");
        assert_eq!(
            places.plan(&single),
            ScanPlan { last: Some(2417), newer: vec![], other_bands: vec![5240], older: vec![] },
            "of the places only those of the networks asked about count"
        );
        assert_eq!(places.plan(&Known::default()), ScanPlan::default());
    }

    #[test]
    fn places_log_text() {
        let mut places = Places::default();
        assert_eq!(places.to_log_line(), "places of joins kept: 0");
        places.add(Place { network: entry("home", true), frequency: 2437, other_band: None });
        assert_eq!(places.to_log_line(), "places of joins kept: 1, the last on 2437 MHz");
        places.add(Place { network: entry("road", true), frequency: 5180, other_band: Some(2412) });
        assert_eq!(
            places.to_log_line(),
            "places of joins kept: 2, the last on 5180 MHz and 2412 MHz on the other band",
            "never a network's name"
        );
    }

    #[test]
    fn locate_other_band() {
        let network = network("home", "secret12");
        let around = [beacon("home", 1, 2437, -50), beacon("home", 2, 5180, -50)];
        let place = Places::default().locate(false, &network, &around[0], &around);
        assert_eq!(
            place,
            Place { network: network.to_entry(), frequency: 2437, other_band: Some(5180) },
            "from what was heard, where no place is kept"
        );
        let mut places = Places::default();
        places.add(place.clone());
        assert_eq!(
            places.locate(false, &network, &around[0], &around[..1]),
            place,
            "back at the kept place after a look on its channel alone, the other channel stays"
        );
        assert_eq!(
            places.locate(false, &network, &around[1], &around[1..]),
            Place { network: network.to_entry(), frequency: 5180, other_band: Some(2437) },
            "joined on the other channel, the two change places"
        );
        assert_eq!(
            places.locate(true, &network, &around[0], &around[..1]),
            Place { other_band: None, ..place.clone() },
            "where the look was on every channel, what it heard is all there is"
        );
        assert_eq!(
            Place { frequency: 5180, other_band: Some(2437), ..place.clone() }.to_low_band(),
            Some(place.clone()),
            "on the other band, the place turns to its cheap band's channel"
        );
        assert_eq!(
            (place.to_low_band(), Place { other_band: None, frequency: 5180, ..place }.to_low_band()),
            (None, None),
            "not on the cheap band already, nor without a channel there"
        );
        assert!(network.is_locked() && !self::network("cafe", "").is_locked());
    }

    fn search(
        known: &Known,
        places: &Places,
        air: &[(&str, u32, i32)],
        deaf: usize,
    ) -> (Option<(String, u32)>, Vec<Vec<u32>>) {
        let air: Vec<scan::Network> =
            air.iter().map(|(name, frequency, signal)| beacon(name, *frequency as u8, *frequency, *signal)).collect();
        let (mut looks, mut channels) = (Vec::new(), Vec::new());
        let result = scan_places(known, places, |frequencies| {
            looks.push(frequencies.to_vec());
            if looks.len() > deaf {
                channels.extend(frequencies);
            }
            Ok::<_, ()>(air.iter().filter(|network| channels.contains(&network.frequency)).cloned().collect())
        });
        let default = result.unwrap().and_then(|networks| {
            known.find_default(&networks).map(|(network, access)| (network.name.clone(), access.frequency))
        });
        (default, looks)
    }

    fn places_of(joins: &[Place]) -> Places {
        let mut places = Places::default();
        for place in joins.iter().rev() {
            places.add(place.clone());
        }
        places
    }

    #[test]
    fn first_look_last_chosen() {
        let known = Known::parse("name home\npassword secret12\nname road\npassword 12345678\n");
        let places = places_of(&[joined_at("home", 2417, Some(5240)), joined_at("road", 2437, None)]);
        let (default, looks) =
            search(&known, &places, &[("home", 2417, -50), ("home", 5240, -45), ("road", 2437, -40)], 0);
        assert_eq!(default, Some(("home".into(), 2417)));
        assert_eq!(looks, [[2417]], "one look, though another known network is around");
        let (default, looks) = search(&known, &places, &[("home", 2417, -50)], 1);
        assert_eq!(default, Some(("home".into(), 2417)));
        assert_eq!(looks, [[2417], [2417]], "a first look that hears nothing is made again");
        assert_eq!(
            search(&known, &Places::default(), &[("home", 2417, -50)], 0),
            (None, Vec::new()),
            "no place, no look"
        );
    }

    #[test]
    fn second_look_last_joined() {
        let known = Known::parse("name road\npassword 12345678\nname home\npassword secret12\n");
        let places = places_of(&[joined_at("home", 2417, Some(5240)), joined_at("road", 2437, None)]);
        let (default, looks) = search(&known, &places, &[("home", 2417, -50), ("home", 5240, -45)], 0);
        assert_eq!(default, Some(("home".into(), 2417)));
        assert_eq!(looks, [vec![2417], vec![2437]], "the second look is for the network chosen since");
        assert_eq!(
            search(&known, &places, &[("home", 2417, -50), ("road", 2437, -60)], 0).0,
            Some(("road".into(), 2437)),
            "which is the one to join where it is heard"
        );
        let (default, looks) = search(&known, &places, &[("home", 2417, -50)], 1);
        assert_eq!(default, Some(("home".into(), 2417)));
        assert_eq!(looks, [vec![2417], vec![2417, 2437]], "a silent first channel is looked at again with it");
    }

    #[test]
    fn moved_device() {
        let known = Known::parse("name home\npassword secret12\nname road\npassword 12345678\n");
        let places = places_of(&[joined_at("home", 2417, Some(5240)), joined_at("road", 2437, Some(5180))]);
        let (default, looks) = search(&known, &places, &[("road", 5180, -55), ("cafe", 2417, -40)], 0);
        assert_eq!(default, Some(("road".into(), 5180)));
        assert_eq!(
            looks,
            [vec![2417], vec![2417], vec![5240], vec![2437, 5180]],
            "the last place twice, its other band, then both channels of the other known network"
        );
        let (default, looks) = search(&known, &places, &[("cafe", 2417, -40)], 0);
        assert_eq!((default, looks.len()), (None, 4), "what is left is to look everywhere");
    }

    #[test]
    fn weak_low_band_other_band() {
        let known = Known::parse("name home\npassword secret12\n");
        let places = places_of(&[joined_at("home", 2417, Some(5240))]);
        let (default, looks) = search(&known, &places, &[("home", 2417, -78), ("home", 5240, -60)], 0);
        assert_eq!(default, Some(("home".into(), 5240)), "the stronger of the two");
        assert_eq!(looks, [[2417], [5240]]);
        let (default, looks) = search(&known, &places, &[("home", 2417, -78)], 0);
        assert_eq!(default, Some(("home".into(), 2417)), "or the weak one, if it is all there is");
        assert_eq!(looks, [[2417], [5240]]);
        let upper = places_of(&[joined_at("home", 5240, Some(2417))]);
        let (default, looks) = search(&known, &upper, &[("home", 2417, -78)], 0);
        assert_eq!(default, Some(("home".into(), 2417)));
        assert_eq!(looks, [[5240], [5240], [2417]], "no channel is turned to that was looked at");
    }
}

//! The networks the device knows, the one chosen last first, their file text, changes to them, and the list's rows of the networks heard, ranked by them.

use crate::wifi::scan;
use crate::wifi::scan::Security;
use alloc::string::String;
use alloc::vec::Vec;

/// The file of the WiFi directory that holds the known networks with their passwords.
pub const NETWORK_FILE: &str = "networks";
pub const UNKNOWN: &str = "no network is known";
pub(super) const NAME: &str = "name ";
const PASSWORD: &str = "password ";

// True for a name or a password a line of the files holds whole: one without a control character.
pub(super) fn is_plain(value: &str) -> bool {
    !value.chars().any(char::is_control)
}

/// A known network with its password.
#[derive(Clone, PartialEq, Eq)]
pub struct Network {
    pub name: String,
    /// Empty for a network that asks for none.
    pub password: String,
}

impl core::fmt::Debug for Network {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Network").field("name", &self.name).field("locked", &!self.password.is_empty()).finish()
    }
}

impl Network {
    /// True for a network that asks for a password.
    pub const fn is_locked(&self) -> bool {
        !self.password.is_empty()
    }

    // The network without its password.
    pub(crate) fn to_entry(&self) -> Entry {
        Entry { name: self.name.clone(), is_locked: self.is_locked() }
    }

    fn is_entry(&self, entry: &Entry) -> bool {
        self.name == entry.name && self.is_locked() == entry.is_locked
    }

    pub(super) fn is_heard_as(&self, network: &scan::Network) -> bool {
        network.name == self.name.as_bytes()
            && matches!(
                (self.is_locked(), network.security),
                (false, Security::Open) | (true, Security::Password { .. })
            )
    }
}

/// A known network without its password: its name, and whether a password is kept with it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub name: String,
    pub is_locked: bool,
}

/// A change to the known networks; the first two are made only once a join has proved the network.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Change {
    // Known from now on, and the one chosen last.
    Remember(Network),
    // A known network chosen anew.
    Prefer(Entry),
    Forget(Entry),
    ForgetAll,
}

/// The outcome of a join that failed to prove a chosen network.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrialError {
    // No access point of the network was heard.
    NotHeard,
    // The network would not take the proof of the password, or ended the handshake.
    WrongPassword,
    // The network refused the station or answered nothing.
    NotLetIn,
    // The network took the station in and lent it no address.
    NoAddress,
    // The radio did not come on, could not look or join, or had no network to join.
    NoRadio,
}

impl Change {
    /// The log text of the change: it names the network and never its password.
    pub fn to_log_line(&self) -> String {
        match self {
            Change::Remember(network) => alloc::format!("{:?} remembered", network.name),
            Change::Prefer(entry) => alloc::format!("{:?} chosen anew", entry.name),
            Change::Forget(entry) => alloc::format!("{:?} forgotten", entry.name),
            Change::ForgetAll => "all forgotten".into(),
        }
    }

    fn edit<T>(&self, list: &mut Vec<T>, is_entry: impl Fn(&T, &Entry) -> bool, make: impl FnOnce(&Network) -> T) {
        match self {
            Change::Remember(network) => {
                let entry = network.to_entry();
                list.retain(|item| !is_entry(item, &entry));
                list.insert(0, make(network));
            }
            Change::Prefer(entry) => {
                if let Some(at) = list.iter().position(|item| is_entry(item, entry)) {
                    list[..=at].rotate_right(1);
                }
            }
            Change::Forget(entry) => list.retain(|item| !is_entry(item, entry)),
            Change::ForgetAll => list.clear(),
        }
    }

    pub(crate) fn apply_to(&self, entries: &mut Vec<Entry>) {
        self.edit(entries, |item, entry| item == entry, Network::to_entry);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Access {
    Open,
    Password,
    Barred,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Row {
    pub(crate) name: String,
    pub(crate) access: Access,
    pub(crate) signal: i32,
}

impl Row {
    pub(crate) fn matches(&self, entry: &Entry) -> bool {
        self.name == entry.name && self.access == if entry.is_locked { Access::Password } else { Access::Open }
    }
}

pub(crate) fn rank(rows: &mut [Row], known: &[Entry]) {
    let place = |row: &Row| known.iter().position(|entry| row.matches(entry)).unwrap_or(usize::MAX);
    rows.sort_by(|a, b| place(a).cmp(&place(b)).then(b.signal.cmp(&a.signal)).then_with(|| a.name.cmp(&b.name)));
}

pub(crate) fn find_missing(known: &[Entry], rows: &[Row]) -> Vec<Entry> {
    known.iter().filter(|entry| !rows.iter().any(|row| row.matches(entry))).cloned().collect()
}

pub(crate) fn to_rows(networks: &[scan::Network], known: &[Entry]) -> Vec<Row> {
    let mut rows: Vec<(&[u8], Row)> = Vec::new();
    for network in networks.iter().filter(|network| network.name.iter().any(|byte| *byte != 0)) {
        let text = core::str::from_utf8(&network.name).ok().filter(|text| is_plain(text));
        let access = match (text, network.security) {
            (Some(_), Security::Open) => Access::Open,
            (Some(_), Security::Password { .. }) => Access::Password,
            _ => Access::Barred,
        };
        let is_same = |row: &&mut (&[u8], Row)| row.0 == &network.name[..] && row.1.access == access;
        if let Some(row) = rows.iter_mut().find(is_same) {
            row.1.signal = row.1.signal.max(network.signal);
            continue;
        }
        let name = String::from_utf8_lossy(&network.name)
            .chars()
            .map(|character| if character.is_control() { '?' } else { character })
            .collect();
        rows.push((&network.name, Row { name, access, signal: network.signal }));
    }
    let mut rows: Vec<Row> = rows.into_iter().map(|row| row.1).collect();
    rank(&mut rows, known);
    rows
}

/// The known networks, the one the user chose last first: where several of them are heard, the first of those is the one to join.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Known {
    networks: Vec<Network>,
}

impl Known {
    /// The networks of a file's text, in its order, each a name line and a password line; anything else is skipped.
    pub fn parse(text: &str) -> Known {
        let (mut networks, mut name) = (Vec::new(), None);
        for line in text.split('\n') {
            let password = line.strip_prefix(PASSWORD).filter(|password| is_plain(password));
            match (name.take(), password) {
                (Some(name), Some(password)) => networks.push(Network { name, password: password.into() }),
                _ => {
                    let named = line.strip_prefix(NAME).filter(|name| !name.is_empty() && is_plain(name));
                    name = named.map(String::from);
                }
            }
        }
        Known { networks }
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        self.networks
            .iter()
            .flat_map(|network| [NAME, network.name.as_str(), "\n", PASSWORD, network.password.as_str(), "\n"])
            .collect()
    }

    /// Takes a network as the one chosen last; a known one of the same name with the same need of a password gives way to it.
    pub fn remember(&mut self, network: Network) {
        self.apply(Change::Remember(network));
    }

    pub(crate) fn apply(&mut self, change: Change) {
        change.edit(&mut self.networks, Network::is_entry, Network::clone);
    }

    // Keeps the networks `keep` takes, in their order.
    pub(super) fn retain(&mut self, keep: impl Fn(&Entry) -> bool) {
        self.networks.retain(|network| keep(&network.to_entry()));
    }

    /// The known network to join among those heard: the one chosen last that is heard, at its strongest low band access point heard well, else its strongest.
    pub fn find_default<'a>(&'a self, networks: &'a [scan::Network]) -> Option<(&'a Network, &'a scan::Network)> {
        self.networks.iter().find_map(|known| {
            networks
                .iter()
                .filter(|network| known.is_heard_as(network))
                .max_by_key(|network| (network.is_low_power(), network.signal))
                .map(|network| (known, network))
        })
    }

    /// The network chosen last: where it is heard it is the one to join, whatever else is heard.
    pub fn first(&self) -> Option<&Network> {
        self.networks.first()
    }

    pub(super) fn is_heard_well(&self, networks: &[scan::Network], entry: &Entry) -> bool {
        self.find_default(networks).is_some_and(|(known, access)| known.to_entry() == *entry && access.is_low_power())
    }

    // A look that heard the network chosen last well needs no other.
    pub(super) fn is_settled_by(&self, networks: &[scan::Network]) -> bool {
        self.first().is_some_and(|first| self.is_heard_well(networks, &first.to_entry()))
    }

    fn find(&self, entry: &Entry) -> Option<&Network> {
        self.networks.iter().find(|network| network.is_entry(entry))
    }

    pub(super) fn contains(&self, entry: &Entry) -> bool {
        self.find(entry).is_some()
    }

    /// The network a join proves before a change is made, with its password; for a change that names none, the trial's error.
    pub fn to_candidate(&self, change: &Change) -> Result<Network, TrialError> {
        match change {
            Change::Remember(network) => Ok(network.clone()),
            Change::Prefer(entry) => self.find(entry).cloned().ok_or(TrialError::NoRadio),
            Change::Forget(_) | Change::ForgetAll => Err(TrialError::NoRadio),
        }
    }

    /// The entries, the one chosen last first.
    pub fn entries(&self) -> Vec<Entry> {
        self.networks.iter().map(Network::to_entry).collect()
    }

    pub fn len(&self) -> usize {
        self.networks.len()
    }

    /// The error text of a join that heard none of them, or had none to look for.
    pub fn to_unheard_text(&self) -> String {
        if self.is_empty() {
            return String::from(UNKNOWN);
        }
        alloc::format!("none of the {} known networks is heard", self.len())
    }

    /// The log line of the file read.
    pub fn to_read_line(&self) -> String {
        alloc::format!("wifi: {} known, as kept", self.len())
    }

    /// The log line of the file written after a change.
    pub fn to_kept_line(&self, change: &str) -> String {
        alloc::format!("wifi: {change}, {} known and kept", self.len())
    }

    /// The log line of the file not written after a change, and why.
    pub fn to_unkept_line(&self, change: &str, error: &dyn core::fmt::Display) -> String {
        alloc::format!("wifi: {change}, keeping the {} known: {error}", self.len())
    }

    pub fn is_empty(&self) -> bool {
        self.networks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wifi::scan::is_low_band;
    use crate::wifi::scan::tests::beacon;
    use alloc::vec;

    fn network(name: &str, password: &str) -> Network {
        Network { name: name.into(), password: password.into() }
    }

    fn entry(name: &str, is_locked: bool) -> Entry {
        Entry { name: name.into(), is_locked }
    }

    #[test]
    fn text_round_trip() {
        let mut known = Known::default();
        assert!(known.is_empty() && known.to_text().is_empty() && Known::parse("") == known);
        known.remember(network("home", "a secret of 8"));
        known.remember(network("cafe", ""));
        known.remember(network("a b", "p w "));
        assert_eq!(
            known.to_text(),
            "name a b\npassword p w \nname cafe\npassword \nname home\npassword a secret of 8\n"
        );
        assert_eq!(Known::parse(&known.to_text()), known);
        assert_eq!(
            known.entries(),
            vec![entry("a b", true), entry("cafe", false), entry("home", true)],
            "the list is told the names and whether a password is kept, never the password"
        );
        known.remember(network("home", "another one"));
        assert_eq!(
            (known.len(), known.entries()[0].clone(), known.to_text().contains("a secret of 8")),
            (3, entry("home", true), false),
            "chosen again, a network is the first, with the password it has now"
        );
        known.remember(network("home", ""));
        assert_eq!(
            known.entries()[..2],
            [entry("home", false), entry("home", true)],
            "a network of the same name that asks for nothing is another network"
        );
    }

    #[test]
    fn forget() {
        let mut known =
            Known::parse("name home\npassword secret12\nname cafe\npassword \nname work\npassword 12345678\n");
        known.apply(Change::Forget(entry("cafe", true)));
        known.apply(Change::Forget(entry("nowhere", false)));
        assert_eq!(known.len(), 3, "what is not known is not forgotten");
        known.apply(Change::Forget(entry("cafe", false)));
        assert_eq!(known.entries(), vec![entry("home", true), entry("work", true)], "the others keep their order");
        known.apply(Change::ForgetAll);
        assert!(known.is_empty() && known.to_text().is_empty(), "and then no password is left anywhere");
    }

    #[test]
    fn retained_in_order() {
        let mut known =
            Known::parse("name home\npassword secret12\nname cafe\npassword \nname work\npassword 12345678\n");
        known.retain(|kept| *kept != entry("cafe", false));
        assert_eq!(known.entries(), vec![entry("home", true), entry("work", true)], "the others in their order");
        known.retain(|kept| *kept != entry("home", false));
        assert_eq!(known.len(), 2, "a network of that name that asks for a password is another");
    }

    #[test]
    fn change_list_parity() {
        let mut known =
            Known::parse("name home\npassword secret12\nname cafe\npassword \nname work\npassword 12345678\n");
        let mut entries = known.entries();
        let changes = [
            Change::Prefer(entry("work", true)),
            Change::Prefer(entry("work", false)),
            Change::Remember(network("cafe", "now it asks")),
            Change::Remember(network("new", "")),
            Change::Forget(entry("home", true)),
            Change::Forget(entry("nowhere", true)),
            Change::Prefer(entry("cafe", false)),
        ];
        for change in changes {
            change.apply_to(&mut entries);
            known.apply(change.clone());
            assert_eq!(known.entries(), entries, "after {change:?}");
        }
        assert_eq!(
            alloc::format!("{:?}", Change::Remember(network("cafe", "now it asks"))),
            "Remember(Network { name: \"cafe\", locked: true })",
            "a change is told without its password"
        );
        assert_eq!(
            entries,
            vec![entry("cafe", false), entry("new", false), entry("cafe", true), entry("work", true)],
            "a network chosen anew stands first and keeps its password, one that is not known is neither chosen nor forgotten"
        );
        assert!(known.to_text().contains("password 12345678\n") && known.to_text().contains("password now it asks\n"));
        assert_eq!(
            [
                Change::Remember(network("guest", "typed in")),
                Change::Prefer(entry("work", true)),
                Change::Prefer(entry("work", false)),
                Change::Forget(entry("work", true)),
                Change::ForgetAll,
            ]
            .map(|change| known.to_candidate(&change)),
            [
                Ok(network("guest", "typed in")),
                Ok(network("work", "12345678")),
                Err(TrialError::NoRadio),
                Err(TrialError::NoRadio),
                Err(TrialError::NoRadio)
            ],
            "a join proves a new network with what was typed and a known one with what is kept; a change that names none to join is a join the radio could not make"
        );
        Change::ForgetAll.apply_to(&mut entries);
        known.apply(Change::ForgetAll);
        assert!(entries.is_empty() && known.is_empty());
    }

    #[test]
    fn default_network() {
        let known = Known::parse("name away\npassword 12345678\nname cafe\npassword \nname home\npassword secret12\n");
        let open = |name: &str, last: u8, signal: i32| scan::Network {
            security: Security::Open,
            ..beacon(name, last, 2412, signal)
        };
        let around = [
            beacon("home", 1, 2412, -70),
            beacon("cafe", 2, 2412, -80),
            beacon("home", 3, 2412, -50),
            open("home", 4, -40),
            open("other", 5, -30),
        ];
        let default = known.find_default(&around).map(|(known, access)| (known.name.as_str(), access.address[5]));
        assert_eq!(
            default,
            Some(("home", 3)),
            "away is not heard, cafe asks for a password here and is another network, home is heard from two access points that ask for one"
        );
        let mut known = known;
        known.remember(network("other", ""));
        assert_eq!(
            known.find_default(&around).map(|(known, access)| (known.name.as_str(), access.address[5])),
            Some(("other", 5)),
            "chosen later, another network is the default where both are heard"
        );
        assert_eq!(
            known.find_default(&around[..3]).map(|(known, _)| known.name.as_str()),
            Some("home"),
            "and where it is not heard, the one before it"
        );
        assert_eq!(
            (known.first().map(|first| first.name.as_str()), Known::default().first()),
            (Some("other"), None),
            "the one chosen last is the first"
        );
        assert_eq!((known.find_default(&[]), Known::default().find_default(&around)), (None, None));
    }

    #[test]
    fn low_band_preference() {
        let known = Known::parse("name home\npassword secret12\nname cafe\npassword \n");
        let at = |last: u8, frequency: u32, signal: i32| beacon("home", last, frequency, signal);
        for (around, expected, reason) in [
            (vec![at(1, 2417, -60), at(2, 5240, -40)], 1, "the band that costs less, though the other is stronger"),
            (vec![at(1, 2417, -70), at(2, 5240, -40), at(3, 2462, -55)], 3, "of two on that band the stronger"),
            (
                vec![at(1, 2417, -71), at(2, 5240, -60)],
                2,
                "below the floor the band counts for nothing, and the stronger access point is taken",
            ),
            (vec![at(1, 2417, -75), at(2, 5240, -80)], 1, "which may be the one on 2.4 GHz"),
            (vec![at(1, 5180, -43), at(2, 5240, -40)], 2, "and on one band the stronger, as ever"),
        ] {
            assert_eq!(known.find_default(&around).map(|(_, access)| access.address[5]), Some(expected), "{reason}");
        }
        assert!(
            is_low_band(2484)
                && !is_low_band(5180)
                && at(1, 2417, -70).is_low_power()
                && !at(1, 2417, -71).is_low_power()
                && !at(1, 5180, -30).is_low_power()
        );
        let home = known.first().unwrap();
        let around = [
            at(1, 2417, -60),
            at(2, 5240, -50),
            at(3, 5500, -45),
            at(4, 2462, -80),
            beacon("cafe", 5, 5745, -30),
            scan::Network { security: Security::Open, ..at(6, 5765, -20) },
        ];
        let entry = home.to_entry();
        assert!(known.is_heard_well(&around, &entry), "the network to join, on the cheap band at -60 dBm");
        assert!(
            !known.is_heard_well(&around[1..], &entry) && !known.is_heard_well(&around[3..4], &entry),
            "not where it is on the other band alone, nor heard below the floor"
        );
        assert!(!known.is_heard_well(&around, &network("cafe", "").to_entry()), "nor where another is the one to join");
        assert!(
            known.is_settled_by(&around)
                && !known.is_settled_by(&around[1..])
                && !Known::default().is_settled_by(&around),
            "the network chosen last heard well settles a look; heard on the other band alone, or with nothing known, it does not"
        );
    }

    #[test]
    fn contains_by_name_and_lock() {
        let known = Known::parse("name home\npassword secret12\nname cafe\npassword \n");
        assert!(
            known.contains(&entry("home", true))
                && known.contains(&entry("cafe", false))
                && !known.contains(&entry("home", false))
                && !known.contains(&entry("work", true))
        );
    }

    fn row(name: &str, access: Access, signal: i32) -> Row {
        Row { name: name.into(), access, signal }
    }

    #[test]
    fn heard_rows() {
        let open = |name: &[u8], signal: i32| scan::Network {
            name: name.to_vec(),
            security: Security::Open,
            ..beacon("", 9, 2412, signal)
        };
        let around = [
            beacon("home", 1, 2412, -70),
            beacon("home", 2, 5180, -50),
            open(b"home", -40),
            open(b"cafe", -60),
            open(&[0, 0, 0], -20),
            open(b"", -20),
            scan::Network { security: Security::Other, ..beacon("work", 3, 2437, -30) },
            open(b"a\tb", -80),
            open(&[0xFF, b'x'], -85),
        ];
        let known = [entry("cafe", false), entry("home", true), entry("work", true), entry("away", true)];
        let rows = to_rows(&around, &known);
        assert_eq!(
            rows[..2],
            [row("cafe", Access::Open, -60), row("home", Access::Password, -50)],
            "the known ones first in their order, a network's access points one row at its strongest signal"
        );
        assert_eq!(
            rows[2..4],
            [row("work", Access::Barred, -30), row("home", Access::Open, -40)],
            "then by signal: an offer no join speaks is barred, and the same name without a password is another row"
        );
        assert_eq!(
            rows[4..],
            [row("a?b", Access::Barred, -80), row("\u{FFFD}x", Access::Barred, -85)],
            "a name no line of the file holds is barred and shown with what cannot be shown replaced; a hidden name is no row"
        );
        assert_eq!(
            find_missing(&known, &rows),
            [entry("work", true), entry("away", true)],
            "a known network is missing where no row lets it in: one heard barred, one not heard"
        );
        let home = &rows[1];
        assert!(
            home.matches(&entry("home", true))
                && !home.matches(&entry("home", false))
                && !home.matches(&entry("cafe", true))
                && !rows[2].matches(&entry("work", false)),
            "a row is a known network's by its name and its need of a password"
        );
    }

    #[test]
    fn rank_order() {
        let mut rows = [
            row("b", Access::Open, -50),
            row("a", Access::Open, -50),
            row("c", Access::Open, -40),
            row("z", Access::Password, -90),
            row("y", Access::Open, -95),
        ];
        rank(&mut rows, &[entry("y", false), entry("z", true), entry("c", true)]);
        assert_eq!(
            rows.map(|row| row.name),
            ["y", "z", "c", "a", "b"],
            "known rows in the order of the known, whatever their signal, then the stronger, then by name"
        );
    }

    #[test]
    fn corrupt_text() {
        let read = |text: &str| Known::parse(text).entries();
        assert_eq!(read("name x\npassword p w"), vec![entry("x", true)], "a last line that was cut short counts");
        assert_eq!(
            read("name \npassword x\nname y\npassword \n"),
            vec![entry("y", false)],
            "a network without a name is none"
        );
        assert_eq!(
            read("password x\nname y\nname z\npassword 12345678\nnoise\nname w\n"),
            vec![entry("z", true)],
            "a name without its password is passed over, and so is a password without a name"
        );
        assert_eq!(
            read("name a\r\npassword b\r\nname c\npassword d\tx\n"),
            vec![],
            "a line with a character no name or password of ours has is no line of ours"
        );
    }

    #[test]
    fn unheard_text() {
        let known = Known::parse("name home\npassword secret12\nname cafe\npassword \n");
        assert_eq!(known.to_unheard_text(), "none of the 2 known networks is heard");
        assert_eq!(Known::default().to_unheard_text(), UNKNOWN, "none known, none to look for");
    }

    #[test]
    fn change_log_text() {
        let changes = [
            Change::Remember(network("home", "secret12")),
            Change::Prefer(entry("home", true)),
            Change::Forget(entry("home", true)),
            Change::ForgetAll,
        ];
        assert_eq!(
            changes.map(|change| change.to_log_line()),
            ["\"home\" remembered", "\"home\" chosen anew", "\"home\" forgotten", "all forgotten"]
        );
        let known = Known::parse("name home\npassword secret12\n");
        assert_eq!(
            [known.to_kept_line("all forgotten"), known.to_unkept_line("all forgotten", &"no room")],
            ["wifi: all forgotten, 1 known and kept", "wifi: all forgotten, keeping the 1 known: no room"]
        );
    }
}

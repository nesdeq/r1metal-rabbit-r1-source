//! Scans: the request, its state by the kernel's messages, and the networks of its results.

use crate::base::bytes::le16;
use crate::base::bytes::le32;
use crate::wifi::VENDOR;
use crate::wifi::elements;
use crate::wifi::netlink::Channel;
use crate::wifi::netlink::Message;
use crate::wifi::netlink::add_attribute;
use crate::wifi::netlink::find_attribute;
use crate::wifi::netlink::interface_attributes;
use crate::wifi::rsn;
use crate::wifi::rsn::Akm;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const GET_SCAN: u8 = 32;
pub const TRIGGER_SCAN: u8 = 33;
pub(super) const NEW_SCAN_RESULTS: u8 = 34;
const SCAN_ABORTED: u8 = 35;
pub const ABORT_SCAN: u8 = 114;
const EBUSY: i32 = 16;
const BSS: u16 = 47;
const BSS_BSSID: u16 = 1;
const BSS_FREQUENCY: u16 = 2;
const BSS_CAPABILITY: u16 = 5;
const BSS_INFORMATION_ELEMENTS: u16 = 6;
const BSS_SIGNAL_MBM: u16 = 7;
const SCAN_FREQUENCIES: u16 = 44;
const SCAN_SSIDS: u16 = 45;

const ELEMENT_SSID: u8 = 0;
const WPA_HEAD: [u8; 4] = [0x00, 0x50, 0xF2, 0x01];
const PRIVACY: u16 = 1 << 4;
const MBM_PER_DBM: i32 = 100;
const LOW_BAND_END: u32 = 5000;
const LOW_POWER_FLOOR: i32 = -70;
/// The error text of a scan that did not end in `TIMEOUT`.
pub const LATE: &str = "the scan did not end in the time a scan is given";
/// The error text of a scan the driver gave up.
pub const ABORTED: &str = "the driver gave the scan up";

// The core draws less on the 2.4 GHz band.
pub(super) const fn is_low_band(frequency: u32) -> bool {
    frequency < LOW_BAND_END
}

/// A scan on the channels of these frequencies in MHz, or on every channel if none is named.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Scan {
    // Listening: the request names no network, and nothing is sent.
    Passive(Vec<u32>),
    // Probing: every network is asked to answer.
    Active(Vec<u32>),
}

impl Scan {
    pub(super) fn from_frequencies(frequencies: &[u32], channels: Option<&[Channel]>) -> Scan {
        let can_probe = channels.is_some_and(|channels| {
            let is_open = |frequency: &u32| {
                channels.iter().any(|channel| channel.frequency == *frequency && channel.allows_probe())
            };
            frequencies.iter().all(is_open)
        });
        if can_probe { Scan::Active(frequencies.to_vec()) } else { Scan::Passive(frequencies.to_vec()) }
    }

    pub(super) fn active(channels: &[Channel]) -> Option<Scan> {
        let frequencies: Vec<u32> =
            channels.iter().filter(|channel| channel.allows_probe()).map(|channel| channel.frequency).collect();
        (!frequencies.is_empty()).then_some(Scan::Active(frequencies))
    }

    // The chip's first scan since it has power stays about half a beacon's time on a channel.
    pub(super) fn warmup_frequency(channels: &[Channel]) -> Option<u32> {
        channels.iter().find(|channel| !channel.disabled).map(|channel| channel.frequency)
    }

    /// The attributes of the request that starts the scan on an interface.
    pub fn to_attributes(&self, interface: u32) -> Vec<u8> {
        let mut attributes = interface_attributes(interface, &[]);
        let frequencies = match self {
            Scan::Passive(frequencies) => frequencies,
            Scan::Active(frequencies) => {
                // A name of no length is the one every network answers to.
                let mut names = Vec::new();
                add_attribute(&mut names, 1, &[]);
                add_attribute(&mut attributes, SCAN_SSIDS, &names);
                frequencies
            }
        };
        if !frequencies.is_empty() {
            let mut list = Vec::new();
            for (index, frequency) in frequencies.iter().enumerate() {
                add_attribute(&mut list, index as u16 + 1, &frequency.to_le_bytes());
            }
            add_attribute(&mut attributes, SCAN_FREQUENCIES, &list);
        }
        attributes
    }

    // Never a network's name.
    fn to_log_line(&self, networks: &[Network], ms: u128) -> String {
        let verb = match self {
            Scan::Passive(_) => "heard",
            Scan::Active(_) => "asked and heard",
        };
        let low = networks.iter().filter(|network| is_low_band(network.frequency)).count();
        let joinable = networks
            .iter()
            .filter(|network| matches!(network.security, Security::Open | Security::Password { .. }))
            .count();
        let sae =
            networks.iter().filter(|network| matches!(network.security, Security::Password { has_sae: true })).count();
        format!(
            "{verb} {} networks in {ms} ms, {low} on 2.4 GHz and {} on 5 GHz, {joinable} of them open or let in by a password, {sae} of those by SAE",
            networks.len(),
            networks.len() - low
        )
    }

    /// The radio's log line of a scan that heard those networks.
    pub fn to_heard_line(&self, networks: &[Network], ms: u128) -> String {
        format!("radio: {}", self.to_log_line(networks, ms))
    }
}

/// The log line of a scan that gave way.
pub fn to_gave_way_line(ms: u128) -> String {
    format!("radio: the scan gave way to a new wish after {ms} ms")
}

/// The state of a scan that was asked for, by the kernel's messages since.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    // Not yet answered: a scan that ends before the answer is an earlier one.
    Asked,
    // Taken by the kernel: the next end of a scan is its own.
    Running,
    // Refused because an earlier scan still runs, whose end is waited for.
    Busy,
}

/// What a kernel message means for a scan that was asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Nothing,
    // The earlier scan has ended: this one is to be asked for anew, and counts as asked.
    Retry,
    // It has ended, and its results can be listed.
    Ended,
    // The driver gave it up.
    Aborted,
    // The kernel refused it, with this error number.
    Refused(i32),
}

impl State {
    /// Takes a message off the socket that asked for the scan and hears of every scan's end.
    pub fn update(&mut self, message: &Message) -> Event {
        let end = message
            .command()
            .map(|(command, _)| command)
            .filter(|command| matches!(*command, NEW_SCAN_RESULTS | SCAN_ABORTED));
        let (next, event) = match (*self, message.ack_status(), end) {
            (State::Asked, Some(0), _) => (State::Running, Event::Nothing),
            (State::Asked, Some(EBUSY), _) => (State::Busy, Event::Nothing),
            (state, Some(code), _) if code != 0 => (state, Event::Refused(code)),
            (State::Busy, _, Some(_)) => (State::Asked, Event::Retry),
            (State::Running, _, Some(NEW_SCAN_RESULTS)) => (State::Running, Event::Ended),
            (State::Running, _, Some(_)) => (State::Running, Event::Aborted),
            (state, _, _) => (state, Event::Nothing),
        };
        *self = next;
        event
    }
}

/// How a network lets a station in, by its beacon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Security {
    Open,
    // By a password a join here proves (`rsn::choose`): by SAE as WPA3 has it, else with the four way handshake of WPA2.
    Password { has_sae: bool },
    // WEP, the WPA of before RSN alone, a login, or an offer no join here speaks.
    Other,
}

/// A network a scan heard.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Network {
    pub address: [u8; 6],
    /// Of its channel, in MHz.
    pub frequency: u32,
    /// In dBm.
    pub signal: i32,
    /// As it is sent, which need not be text.
    pub name: Vec<u8>,
    pub security: Security,
    /// Its RSN element and, after it, that element's extension if it has one, each whole; empty if it has no RSN element.
    pub rsn: Vec<u8>,
}

impl Network {
    pub(super) const fn is_low_power(&self) -> bool {
        is_low_band(self.frequency) && self.signal >= LOW_POWER_FLOOR
    }
}

fn parse_security(ies: &[u8], capability: u16) -> Security {
    if let Some(choice) = rsn::choose(ies) {
        return Security::Password { has_sae: matches!(choice.akm, Akm::Sae { .. }) };
    }
    let asks_proof =
        |element: &[u8]| element[0] == rsn::ELEMENT || (element[0] == VENDOR && element[2..].starts_with(&WPA_HEAD));
    if capability & PRIVACY != 0 || elements(ies).any(asks_proof) { Security::Other } else { Security::Open }
}

/// The network of a `GET_SCAN` dump message, by its command and attributes; none if it leaves out the address, the frequency or the signal.
pub fn to_network(command: u8, result: &[u8]) -> Option<Network> {
    (command == NEW_SCAN_RESULTS).then(|| parse_network(result)).flatten()
}

fn parse_network(result: &[u8]) -> Option<Network> {
    let bss = find_attribute(result, BSS)?;
    let ies = find_attribute(bss, BSS_INFORMATION_ELEMENTS).unwrap_or(&[]);
    let capability = find_attribute(bss, BSS_CAPABILITY).and_then(|field| le16(field, 0)).unwrap_or(0);
    Some(Network {
        address: find_attribute(bss, BSS_BSSID)?.try_into().ok()?,
        frequency: le32(find_attribute(bss, BSS_FREQUENCY)?, 0)?,
        signal: le32(find_attribute(bss, BSS_SIGNAL_MBM)?, 0)? as i32 / MBM_PER_DBM,
        name: rsn::find_element(ies, ELEMENT_SSID).map_or_else(Vec::new, |name| name[2..].to_vec()),
        security: parse_security(ies, capability),
        rsn: rsn::find_rsn(ies).unwrap_or_default(),
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::wifi::netlink::attributes;
    use crate::wifi::netlink::messages;
    use crate::wifi::netlink::nest;
    use crate::wifi::netlink::request;
    use crate::wifi::netlink::tests::answer;
    use alloc::vec;

    pub(crate) fn beacon(name: &str, last: u8, frequency: u32, signal: i32) -> Network {
        Network {
            address: [2, 0, 0, 0, 0, last],
            frequency,
            signal,
            name: name.as_bytes().to_vec(),
            security: Security::Password { has_sae: false },
            rsn: Vec::new(),
        }
    }

    // An RSN element with CCMP for the group and between the two, those proofs, and the capabilities and what follows them in `rest`.
    fn offer(suites: &[[u8; 4]], rest: &[u8]) -> Vec<u8> {
        let mut body = vec![1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, suites.len() as u8, 0];
        suites.iter().for_each(|suite| body.extend(suite));
        body.extend(rest);
        [vec![rsn::ELEMENT, body.len() as u8], body].concat()
    }

    fn rsn(suites: &[[u8; 4]]) -> Vec<u8> {
        offer(suites, &[])
    }

    fn result(name: &[u8], elements: &[u8], capability: u16) -> Vec<u8> {
        let mut attributes = Vec::new();
        add_attribute(&mut attributes, BSS_BSSID, &[2, 0, 0, 0, 0, 7]);
        add_attribute(&mut attributes, BSS_FREQUENCY, &5180u32.to_le_bytes());
        add_attribute(&mut attributes, BSS_CAPABILITY, &capability.to_le_bytes());
        add_attribute(&mut attributes, BSS_SIGNAL_MBM, &(-5250i32).to_le_bytes());
        add_attribute(
            &mut attributes,
            BSS_INFORMATION_ELEMENTS,
            &[&[ELEMENT_SSID, name.len() as u8][..], name, elements].concat(),
        );
        nest(BSS, &attributes)
    }

    #[test]
    fn scan_result() {
        let (handshake, sae, login) = ([0x00, 0x0F, 0xAC, 2], [0x00, 0x0F, 0xAC, 8], [0x00, 0x0F, 0xAC, 1]);
        let network = parse_network(&result(b"home", &rsn(&[handshake]), PRIVACY)).unwrap();
        assert_eq!(
            (network.address, network.frequency, network.signal, &network.name[..]),
            ([2, 0, 0, 0, 0, 7], 5180, -52, &b"home"[..])
        );
        assert_eq!(network.security, Security::Password { has_sae: false });
        assert_eq!(
            (&network.rsn[..], parse_network(&result(b"x", &[], 0)).unwrap().rsn.len()),
            (&rsn(&[handshake])[..], 0),
            "with its RSN element whole, if it has one"
        );
        let extended = [&[rsn::EXTENSION, 1, 0x20][..], &[VENDOR, 1, 9], &rsn(&[sae])].concat();
        assert_eq!(
            parse_network(&result(b"x", &extended, PRIVACY)).unwrap().rsn,
            [&rsn(&[sae])[..], &[rsn::EXTENSION, 1, 0x20]].concat(),
            "and after it that element's extension, wherever the beacon had it"
        );
        assert!(
            parse_network(&result(b"x", &[rsn::EXTENSION, 1, 0x20], 0)).unwrap().rsn.is_empty(),
            "an extension of no element is none"
        );
        let security =
            |elements: &[u8], capability| parse_network(&result(b"x", elements, capability)).unwrap().security;
        let protected = [0x80, 0];
        assert_eq!(
            security(&offer(&[handshake, sae], &protected), PRIVACY),
            Security::Password { has_sae: true },
            "both generations with protected management frames: the join takes SAE"
        );
        assert_eq!(security(&offer(&[sae], &[0xC0, 0]), PRIVACY), Security::Password { has_sae: true });
        assert_eq!(
            security(&rsn(&[handshake, sae]), PRIVACY),
            Security::Password { has_sae: false },
            "SAE without that protection is passed over for the handshake, as the join passes it over"
        );
        assert_eq!(security(&rsn(&[sae]), PRIVACY), Security::Other, "and alone it is an offer no join here speaks");
        let tkip = [48, 18, 1, 0, 0x00, 0x0F, 0xAC, 2, 1, 0, 0x00, 0x0F, 0xAC, 2, 1, 0, 0x00, 0x0F, 0xAC, 2];
        assert_eq!(security(&tkip, PRIVACY), Security::Other, "nor TKIP between the two, whatever the proof");
        assert_eq!(security(&rsn(&[login]), PRIVACY), Security::Other);
        let fast = [[0x00, 0x0F, 0xAC, 9], [0x00, 0x0F, 0xAC, 4]];
        assert_eq!(
            security(&rsn(&fast), PRIVACY),
            Security::Other,
            "fast transition alone, by SAE or by the handshake, which no join here speaks"
        );
        assert_eq!(
            security(&offer(&[fast[0], sae], &protected), PRIVACY),
            Security::Password { has_sae: true },
            "beside SAE it is passed over"
        );
        assert_eq!(security(&[VENDOR, 4, 0x00, 0x50, 0xF2, 0x01], PRIVACY), Security::Other, "the WPA of before RSN");
        assert_eq!(security(&[], PRIVACY), Security::Other, "WEP");
        assert_eq!(security(&[], 0), Security::Open);
        assert_eq!(security(&rsn(&[handshake])[..10], PRIVACY), Security::Other, "an element cut short offers nothing");
        let mut overcounted = rsn(&[handshake]);
        overcounted[14] = 2;
        assert_eq!(
            security(&overcounted, PRIVACY),
            Security::Other,
            "nor one whose list of proofs counts more than it holds, which joining would refuse"
        );
        assert_eq!(security(&overcounted, 0), Security::Other, "whatever its beacon says of privacy");
        assert_eq!(parse_network(&nest(BSS, &[])), None);
        assert_eq!(parse_network(&[]), None);
    }

    #[test]
    fn network_command() {
        let attributes = result(b"home", &[], 0);
        assert_eq!(to_network(NEW_SCAN_RESULTS, &attributes), parse_network(&attributes));
        assert!(to_network(NEW_SCAN_RESULTS, &attributes).is_some());
        assert_eq!(to_network(GET_SCAN, &attributes), None);
    }

    #[test]
    fn result_without_signal() {
        let mut attributes = Vec::new();
        add_attribute(&mut attributes, BSS_BSSID, &[2, 0, 0, 0, 0, 7]);
        add_attribute(&mut attributes, BSS_FREQUENCY, &5180u32.to_le_bytes());
        assert_eq!(parse_network(&nest(BSS, &attributes)), None, "it must not count as the strongest of all");
        add_attribute(&mut attributes, BSS_SIGNAL_MBM, &(-9100i32).to_le_bytes());
        assert_eq!(parse_network(&nest(BSS, &attributes)).map(|network| network.signal), Some(-91));
    }

    #[test]
    fn scan_attributes() {
        assert_eq!(Scan::Passive(Vec::new()).to_attributes(30), [8, 0, 3, 0, 30, 0, 0, 0]);
        let passive = Scan::Passive(vec![2412]).to_attributes(30);
        assert_eq!(
            (passive.len(), find_attribute(&passive, SCAN_SSIDS)),
            (20, None),
            "one that listens on a channel names the channel and still no network"
        );
        assert_eq!(
            find_attribute(&passive, SCAN_FREQUENCIES).map(|list| attributes(list).collect::<Vec<_>>()),
            Some(vec![(1, &2412u32.to_le_bytes()[..])])
        );
        let every_channel = Scan::Active(Vec::new()).to_attributes(30);
        assert_eq!(every_channel, [8, 0, 3, 0, 30, 0, 0, 0, 8, 0, 45, 0, 4, 0, 1, 0]);
        let active = Scan::Active(vec![2412, 5180]).to_attributes(30);
        assert_eq!(active[..16], every_channel[..]);
        let list = find_attribute(&active, SCAN_FREQUENCIES).unwrap();
        assert_eq!(
            attributes(list).collect::<Vec<_>>(),
            [(1, &2412u32.to_le_bytes()[..]), (2, &5180u32.to_le_bytes()[..])]
        );
        assert_eq!(find_attribute(&every_channel, SCAN_FREQUENCIES), None);
    }

    #[test]
    fn active_by_rules() {
        let plain = Channel { frequency: 2412, disabled: false, no_ir: false };
        let channels = [
            Channel { frequency: 2412, ..plain },
            Channel { frequency: 2467, no_ir: true, ..plain },
            Channel { frequency: 5180, ..plain },
            Channel { frequency: 5885, disabled: true, ..plain },
        ];
        assert_eq!(
            Scan::from_frequencies(&[2412, 5180], Some(&channels)),
            Scan::Active(vec![2412, 5180]),
            "open on every one"
        );
        assert_eq!(
            Scan::from_frequencies(&[2412, 2467], Some(&channels)),
            Scan::Passive(vec![2412, 2467]),
            "quiet on one"
        );
        assert_eq!(
            Scan::from_frequencies(&[2437], Some(&channels)),
            Scan::Passive(vec![2437]),
            "a channel the rules do not name"
        );
        assert_eq!(
            Scan::from_frequencies(&[2412], None),
            Scan::Passive(vec![2412]),
            "or where the kernel tells no rules"
        );
        assert_eq!(Scan::active(&channels), Some(Scan::Active(vec![2412, 5180])), "everywhere it may send first");
        assert_eq!(Scan::active(&channels[1..2]), None, "and nowhere where it may on none");
        assert_eq!(
            (
                Scan::warmup_frequency(&channels[3..]),
                Scan::warmup_frequency(&channels[1..]),
                Scan::warmup_frequency(&[])
            ),
            (None, Some(2467), None),
            "a channel to listen on first is one the rules let it use at all"
        );
    }

    #[test]
    fn state_by_messages() {
        let answer = |code: i32| answer(7, -code);
        let event = |command: u8| request(21, 0, 0, command, 1, &interface_attributes(30, &[]));
        let update = |state: &mut State, datagram: &[u8]| state.update(&messages(datagram).next().unwrap());
        let mut state = State::Asked;
        assert_eq!(
            (update(&mut state, &event(SCAN_ABORTED)), update(&mut state, &event(NEW_SCAN_RESULTS)), state),
            (Event::Nothing, Event::Nothing, State::Asked),
            "a scan that ends before the kernel has answered is an earlier one"
        );
        assert_eq!((update(&mut state, &answer(0)), state), (Event::Nothing, State::Running));
        assert_eq!(update(&mut state, &event(TRIGGER_SCAN)), Event::Nothing, "the word that it has begun");
        assert_eq!(update(&mut state, &event(NEW_SCAN_RESULTS)), Event::Ended);
        assert_eq!(update(&mut State::Running, &event(SCAN_ABORTED)), Event::Aborted);
        for end in [NEW_SCAN_RESULTS, SCAN_ABORTED] {
            let mut busy = State::Asked;
            assert_eq!(
                (update(&mut busy, &answer(16)), busy),
                (Event::Nothing, State::Busy),
                "refused while another scan runs"
            );
            assert_eq!(update(&mut busy, &event(TRIGGER_SCAN)), Event::Nothing);
            assert_eq!(
                (update(&mut busy, &event(end)), busy),
                (Event::Retry, State::Asked),
                "however the other ends, the scan is asked for anew"
            );
        }
        assert_eq!(update(&mut State::Asked, &answer(19)), Event::Refused(19));
        assert_eq!(
            (update(&mut State::Running, &answer(16)), update(&mut State::Busy, &answer(0))),
            (Event::Refused(16), Event::Nothing),
            "busy is an answer to the asking alone"
        );
    }

    #[test]
    fn scan_log_text() {
        let network = |frequency: u32, security: Security| Network { security, ..beacon("home", 7, frequency, -50) };
        let around = [
            network(2412, Security::Open),
            network(5180, Security::Password { has_sae: true }),
            network(5240, Security::Password { has_sae: false }),
            network(2437, Security::Other),
        ];
        assert_eq!(
            Scan::Passive(Vec::new()).to_log_line(&around, 812),
            "heard 4 networks in 812 ms, 2 on 2.4 GHz and 2 on 5 GHz, 3 of them open or let in by a password, 1 of those by SAE"
        );
        assert!(Scan::Active(vec![2412]).to_log_line(&[], 5).starts_with("asked and heard 0 networks in 5 ms"));
        assert!(
            Scan::Active(vec![2412]).to_heard_line(&[], 5).starts_with("radio: asked and heard 0 networks in 5 ms"),
            "the radio's line, under its name"
        );
        assert_eq!(to_gave_way_line(40), "radio: the scan gave way to a new wish after 40 ms");
    }
}

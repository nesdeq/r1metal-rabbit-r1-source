//! A join of a network: what it is for, the time its steps get, its steps from SAE to the handshake's keys, and its errors.

use crate::base::code::hex;
use crate::crypto::Primitives;
use crate::net::dhcp::FIRST_WAIT;
use crate::net::ipv6::JOIN_WRITES;
use crate::net::ipv6::OFF_WRITES;
use crate::wifi::TIMEOUT;
use crate::wifi::credential::Credential;
use crate::wifi::known;
use crate::wifi::known::Entry;
use crate::wifi::known::TrialError;
use crate::wifi::netlink;
use crate::wifi::netlink::CONNECT;
use crate::wifi::netlink::ConnectRequest;
use crate::wifi::netlink::DISCONNECT;
use crate::wifi::netlink::EXTERNAL_AUTH;
use crate::wifi::netlink::FRAME;
use crate::wifi::netlink::Message;
use crate::wifi::netlink::SUCCESS;
use crate::wifi::rsn;
use crate::wifi::rsn::Akm;
use crate::wifi::rsn::Choice;
use crate::wifi::sae;
use crate::wifi::sae::Curve;
use crate::wifi::sae::POINT;
use crate::wifi::scan;
use crate::wifi::time_left;
use crate::wifi::wpa;
use crate::wifi::wpa::Crypto;
use crate::wifi::wpa::Supplicant;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

const ACTION: u16 = 0x00D0;
const NO_ACTION: [u8; 4] = [127, 0, 0, 0];

pub const NO_TRIAL_LINE: &str = "wifi: nothing to prove: the change names no network to join";
const LOST: &str = "wifi: out of the network";
// What a network gets for its association and for a resolver's answer where the modem stands in for it: the lower end of the 1 to 2 s the owner gives WiFi to serve, which with the core's power-up and power-down ends WiFi's turn within 2 s of the ask.
const BACKED_LIMIT: Duration = Duration::from_secs(1);

/// The time each step of a join gets, counted from the step's beginning.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// From the join's beginning to keys in use.
    pub link: Duration,
    pub address: Duration,
    /// For a resolver's answer; none where the join proves no reach.
    pub reach: Option<Duration>,
}

impl Limits {
    // Of a join nothing stands in for: `TIMEOUT` for each step, and no proof of reach.
    pub(super) const ALONE: Limits = Limits { link: TIMEOUT, address: TIMEOUT, reach: None };
    // Of a stay's join the modem stands in for: an address gets until the client would ask again, as a server that probes the address first answers after about 3.1 s.
    pub(super) const BACKED: Limits = Limits { link: BACKED_LIMIT, address: FIRST_WAIT, reach: Some(BACKED_LIMIT) };
}

/// The log line of a join beginning; `ms` is the credential's time.
pub fn to_joining_line(
    network: &known::Network,
    access: &scan::Network,
    credential: Option<&Credential>,
    ms: u128,
) -> String {
    let proof = if matches!(credential, Some(Credential::Pwe(_))) { "point for SAE" } else { "key" };
    format!(
        "wifi: joining {:?} on {} MHz, heard at {} dBm; the password's {proof} took {ms} ms",
        network.name, access.frequency, access.signal
    )
}

/// The log line of a link that ended.
pub fn to_lost_line(error: JoinError) -> String {
    format!("{LOST}: {error:?}")
}

/// The log line of a link whose news could not be read, with the read's error.
pub fn to_unread_line(error: &str) -> String {
    format!("{LOST}: {error}")
}

/// The log line of the RSN element and extension the driver wrote into the association request.
pub fn to_rsn_line(rsn: &[u8]) -> String {
    format!("wifi: the driver's association request says of RSN {}", hex(rsn))
}

/// The log line of a frame of SAE that was skipped.
pub fn to_skipped_line(error: sae::Error) -> String {
    format!("wifi: a frame of SAE was not taken: {error:?}")
}

/// The log line of a frame of the handshake that was not taken.
pub fn to_untaken_line(error: wpa::Error) -> String {
    format!("wifi: a frame of the handshake was not taken: {error:?}")
}

/// The log line of a stay that began `ms` after the look for it.
pub fn to_stay_line(ms: u128) -> String {
    format!("wifi: in the network {ms} ms after the look for it began")
}

/// The log line of a network proved `ms` after the look for it.
pub fn to_proved_line(ms: u128) -> String {
    format!("wifi: the network is proved {ms} ms after the look for it began, and left again")
}

/// The log line of a network to prove that is not among `count` access points.
pub fn to_unheard_line(count: usize) -> String {
    format!("wifi: the network to prove is not heard among {count} access points")
}

/// What a join is for: a stay, whose traffic the interface carries, or a trial, which proves the network and carries none.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    Stay,
    Trial,
}

impl Purpose {
    /// The kernel's IPv6 settings of the interface written before the join, in order: a trial's keep IPv6 off, so that no router's announcement routes a stay over the modem through it.
    pub const fn to_ipv6_writes(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Purpose::Stay => &JOIN_WRITES,
            Purpose::Trial => &OFF_WRITES,
        }
    }

    /// True where the join asks the routers for their announcement and puts its lease on the interface, the default route with it.
    pub const fn is_routed(self) -> bool {
        matches!(self, Purpose::Stay)
    }
}

/// Why a station is not in a network, or no longer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoinError {
    // A new request for the radio came first.
    GaveWay,
    // The network or the driver refused the station, with this status.
    Refused(u16),
    Silent,
    // The join was not through in its limit.
    Late,
    // The network sent the station away or went silent, with this reason; during the handshake that is what a wrong password looks like.
    Lost(u16),
    // The network offers nothing this firmware speaks.
    NotSpoken,
    // A wrong password shows as `sae::Error::Proof`.
    Sae(sae::Error),
}

impl JoinError {
    /// True if the network is told that the station leaves; one that sent the station away has nothing to be told.
    pub const fn should_disconnect(self) -> bool {
        matches!(self, JoinError::GaveWay | JoinError::Sae(_) | JoinError::Late)
    }

    const fn to_trial_error(self, is_locked: bool) -> TrialError {
        match self {
            JoinError::Sae(_) | JoinError::Lost(_) | JoinError::Late if is_locked => TrialError::WrongPassword,
            _ => TrialError::NotLetIn,
        }
    }
}

/// How a stay's join that came to no stay ends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StayEnd {
    // A new request for the radio came first; the join is made again once that is seen through.
    GaveWay,
    // Why, in the log's words, and the network to pass over where one was heard and did not serve.
    Failed(String, Option<Entry>),
}

/// Why the station is not in the network it set out for, from the join to the proof of its reach.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LinkError {
    // A new request for the radio came first.
    GaveWay,
    NotJoined(JoinError),
    // The network took the station in and lent it no address.
    NoAddress(JoinError),
    // The network lent an address and no resolver answered through it, or the link was lost meanwhile.
    Unreached(JoinError),
    // The firmware could not do its own part, in its words.
    Failed(String),
}

impl LinkError {
    /// The error of a join.
    pub const fn from_join(error: JoinError) -> LinkError {
        match error {
            JoinError::GaveWay => LinkError::GaveWay,
            error => LinkError::NotJoined(error),
        }
    }

    /// The error of an address request.
    pub const fn from_address(error: JoinError) -> LinkError {
        match error {
            JoinError::GaveWay => LinkError::GaveWay,
            error => LinkError::NoAddress(error),
        }
    }

    /// The error of a proof of reach.
    pub const fn from_reach(error: JoinError) -> LinkError {
        match error {
            JoinError::GaveWay => LinkError::GaveWay,
            error => LinkError::Unreached(error),
        }
    }

    /// The log text of the error.
    pub fn to_log_line(&self) -> String {
        match self {
            LinkError::GaveWay => String::from("gave way to a new wish"),
            LinkError::NotJoined(error) => format!("not joined: {error:?}"),
            LinkError::NoAddress(error) => format!("no address: {error:?}"),
            LinkError::Unreached(error) => format!("no reach: {error:?}"),
            LinkError::Failed(reason) => reason.clone(),
        }
    }

    // The network a stay's join that stopped so passes over: `network`, where it did not take the station in, lent it no address or reached no resolver; none where the join gave way or the firmware could not do its own part.
    fn to_culprit(&self, network: &known::Network) -> Option<Entry> {
        matches!(self, LinkError::NotJoined(_) | LinkError::NoAddress(_) | LinkError::Unreached(_))
            .then(|| network.to_entry())
    }

    /// How a stay's join that stopped so ends, set out for `network` where one was heard: none was where the look for the known networks failed or heard none of them.
    pub fn to_stay_end(&self, network: Option<&known::Network>) -> StayEnd {
        match self {
            LinkError::GaveWay => StayEnd::GaveWay,
            error => StayEnd::Failed(error.to_log_line(), network.and_then(|network| error.to_culprit(network))),
        }
    }

    /// The list's error of a trial that stopped so, by whether the network asks for a password; none where it gave way, as the join is made again.
    pub const fn to_trial_error(&self, is_locked: bool) -> Option<TrialError> {
        match self {
            LinkError::GaveWay => None,
            LinkError::NotJoined(error) => Some(error.to_trial_error(is_locked)),
            // A trial proves no reach: a network that serves no stay counts with the one that lends no address.
            LinkError::NoAddress(_) | LinkError::Unreached(_) => Some(TrialError::NoAddress),
            LinkError::Failed(_) => Some(TrialError::NoRadio),
        }
    }
}

/// What a kernel message or a frame has a station do for its join, in this order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    // Send this management frame to the access point.
    Send(Vec<u8>),
    // Tell the driver whether SAE has proved the password.
    Report(bool),
    // The network has taken the station in; with the RSN element and extension the driver wrote into the association request, for the log, where they are not what the station chose.
    Joined(Option<Vec<u8>>),
    // A frame of SAE was skipped, for this reason.
    Skipped(sae::Error),
    Failed(JoinError),
}

/// What a join does next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Next {
    Wait(Duration),
    // Through, with the log's line.
    Done(String),
    Failed(JoinError),
}

/// A station's join of a network.
pub struct Attempt {
    pub station: [u8; 6],
    pub access_point: [u8; 6],
    choice: Option<Choice>,
    rsn: Vec<u8>,
    elements: Vec<u8>,
    pwe: Option<[u8; POINT]>,
    exchange: Option<sae::Station>,
    supplicant: Option<Supplicant>,
    is_connected: bool,
    proof_ms: Option<u128>,
    join_ms: Option<u128>,
}

impl Attempt {
    /// A station setting out for the access point, which offers `rsn`; the error where the network offers nothing this firmware speaks, or `credential` does not fit.
    pub fn new(
        rsn: &[u8],
        credential: Option<Credential>,
        station: [u8; 6],
        access_point: [u8; 6],
    ) -> Result<Attempt, JoinError> {
        let mut attempt = Attempt {
            station,
            access_point,
            choice: None,
            rsn: rsn.to_vec(),
            elements: Vec::new(),
            pwe: None,
            exchange: None,
            supplicant: None,
            is_connected: false,
            proof_ms: None,
            join_ms: None,
        };
        if rsn.is_empty() {
            return Ok(attempt);
        }
        let choice = rsn::choose(rsn).ok_or(JoinError::NotSpoken)?;
        // The PMK goes with the handshake and the PWE with SAE; a network that asks for a password is not joined with what the other takes, nor with nothing.
        match (choice.akm, credential) {
            (Akm::Sae { .. }, Some(Credential::Pwe(pwe))) => attempt.pwe = Some(pwe),
            (Akm::Psk | Akm::PskSha256, Some(Credential::Pmk(pmk))) => {
                attempt.supplicant = Some(Supplicant::new(pmk, choice, station, access_point, rsn));
            }
            _ => return Err(JoinError::NotSpoken),
        }
        (attempt.choice, attempt.elements) = (Some(choice), choice.elements());
        Ok(attempt)
    }

    /// True while the password is to be proved by SAE, which the station does itself over a curve it is given.
    pub const fn uses_sae(&self) -> bool {
        self.pwe.is_some()
    }

    fn is_hashed(&self) -> bool {
        self.choice.is_some_and(|choice| choice.akm == Akm::Sae { is_hashed: true })
    }

    const fn is_sae_done(&self) -> bool {
        self.pwe.is_some() && self.supplicant.is_some()
    }

    /// What a join that began `elapsed` ago does next, with `limit` to be through in.
    pub fn next(&mut self, elapsed: Duration, limit: Duration) -> Next {
        if let Some(join_ms) = self.join_ms
            && self.try_finish()
        {
            return Next::Done(format!("wifi: {}", self.to_log_line(self.proof_ms, join_ms, elapsed.as_millis())));
        }
        time_left(elapsed, limit).map_or(Next::Failed(JoinError::Late), Next::Wait)
    }

    /// Notes the network taking the station in, `elapsed` after the join began.
    pub const fn note_join(&mut self, elapsed: Duration) {
        self.join_ms = Some(elapsed.as_millis());
    }

    /// Notes when SAE proved the password, if it just has, `elapsed` after the join began.
    pub const fn note_proof(&mut self, elapsed: Duration) {
        if self.proof_ms.is_none() && self.is_sae_done() {
            self.proof_ms = Some(elapsed.as_millis());
        }
    }

    /// True if a handshake frame came from the access point joined, by its source address.
    pub fn is_from_access_point(&self, source: &[u8]) -> bool {
        source.starts_with(&self.access_point)
    }

    fn to_log_line(&self, proof_ms: Option<u128>, join_ms: u128, keys_ms: u128) -> String {
        let point = if self.is_hashed() { "hashed to" } else { "found by looping" };
        let sae_text = proof_ms.map_or_else(String::new, |proof_ms| {
            format!("the password proved by SAE over a point {point} after {proof_ms} ms, ")
        });
        format!("joined, {sae_text}taken in after {join_ms} ms, keys in use after {keys_ms} ms")
    }

    /// The request to join the network of that name on that channel in MHz.
    pub fn request<'a>(&'a self, name: &'a [u8], frequency: u32) -> ConnectRequest<'a> {
        ConnectRequest {
            ssid: name,
            bssid: self.access_point,
            frequency,
            ie: &self.elements,
            group: self.choice.map_or(0, |choice| choice.group.suite()),
            pairwise: rsn::PAIRWISE,
            akm: self.choice.map_or(0, |choice| choice.akm.suite()),
            mfp_required: self.choice.is_some_and(|choice| choice.is_required),
            is_sae: self.pwe.is_some(),
        }
    }

    /// The attributes of the `REGISTER_FRAME` requests to send before the join, for the management frames SAE takes: authentication frames of SAE, and action frames of a kind nobody sends; none without SAE.
    pub fn frame_registrations(&self, interface: u32) -> Vec<Vec<u8>> {
        if self.pwe.is_none() {
            return Vec::new();
        }
        alloc::vec![
            netlink::register_frame_attributes(interface, sae::FRAME_KIND, &sae::ALGORITHM.to_le_bytes()),
            netlink::register_frame_attributes(interface, ACTION, &NO_ACTION),
        ]
    }

    /// The actions of a message of the kernel's wireless stack, with `crypto`'s hashes over `curve` while SAE proves the password.
    pub fn handle<C: Curve>(&mut self, crypto: &impl Primitives, curve: Option<&C>, message: &Message) -> Vec<Action> {
        match message.command() {
            Some((CONNECT, attributes)) => alloc::vec![match netlink::parse_connect_status(attributes) {
                Some(SUCCESS) => Action::Joined(self.note_connected(netlink::parse_req_ie(attributes))),
                Some(status) => Action::Failed(JoinError::Refused(status)),
                None => Action::Failed(JoinError::Silent),
            }],
            Some((DISCONNECT, attributes)) => {
                alloc::vec![Action::Failed(JoinError::Lost(netlink::parse_reason(attributes)))]
            }
            Some((EXTERNAL_AUTH, attributes)) => self.commit(curve, netlink::parse_external_auth(attributes)),
            Some((FRAME, attributes)) => netlink::parse_frame(attributes)
                .map_or_else(Vec::new, |frame| self.take_authentication(crypto, curve, frame)),
            _ => Vec::new(),
        }
    }

    fn note_connected(&mut self, request_ie: &[u8]) -> Option<Vec<u8>> {
        self.is_connected = true;
        let (Some(supplicant), Some(rsn)) = (self.supplicant.as_mut(), rsn::find_rsn(request_ie)) else {
            return None;
        };
        supplicant.set_elements(&rsn);
        (rsn != self.elements).then_some(rsn)
    }

    fn commit<C: Curve>(&mut self, curve: Option<&C>, access: Option<[u8; 6]>) -> Vec<Action> {
        if access != Some(self.access_point) {
            return Vec::new();
        }
        let is_hashed = self.is_hashed();
        match self.pwe.zip(curve).and_then(|(pwe, curve)| sae::Station::begin(curve, pwe, is_hashed)) {
            Some((station, commit)) => {
                self.exchange = Some(station);
                alloc::vec![Action::Send(sae::frame(&self.station, &self.access_point, &commit))]
            }
            None => self.fail_sae(sae::Error::Arithmetic),
        }
    }

    fn take_authentication<C: Curve>(
        &mut self,
        crypto: &impl Primitives,
        curve: Option<&C>,
        frame: &[u8],
    ) -> Vec<Action> {
        let Some(body) = sae::parse_frame(frame, &self.station, &self.access_point) else {
            return Vec::new();
        };
        let (Some(curve), Some(exchange), Some(choice)) = (curve, self.exchange.as_mut(), self.choice) else {
            return Vec::new();
        };
        match exchange.take(crypto, curve, body) {
            Ok(steps) => steps
                .into_iter()
                .map(|step| match step {
                    sae::Step::Send(body) => Action::Send(sae::frame(&self.station, &self.access_point, &body)),
                    sae::Step::Agreed { pmk, .. } => {
                        self.supplicant =
                            Some(Supplicant::new(pmk, choice, self.station, self.access_point, &self.rsn));
                        Action::Report(true)
                    }
                })
                .collect(),
            Err(error) if !error.is_fatal() => alloc::vec![Action::Skipped(error)],
            Err(error) => self.fail_sae(error),
        }
    }

    fn fail_sae(&mut self, error: sae::Error) -> Vec<Action> {
        self.exchange = None;
        alloc::vec![Action::Report(false), Action::Failed(JoinError::Sae(error))]
    }

    /// Takes a frame of the handshake from the access point: its steps, or why it was not taken; nothing before there is a handshake.
    pub fn take_eapol(&mut self, crypto: &impl Crypto, frame: &[u8]) -> Result<Vec<wpa::Step>, wpa::Error> {
        self.supplicant.as_mut().map_or(Ok(Vec::new()), |supplicant| supplicant.take(crypto, frame))
    }

    fn try_finish(&mut self) -> bool {
        let is_done =
            self.is_connected && (self.choice.is_none() || self.supplicant.as_ref().is_some_and(Supplicant::is_agreed));
        if is_done {
            (self.pwe, self.exchange) = (None, None);
        }
        is_done
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::arp;
    use crate::wifi::netlink::BSSID;
    use crate::wifi::netlink::EXTERNAL_AUTH_ACTION;
    use crate::wifi::netlink::REASON_CODE;
    use crate::wifi::netlink::REQ_IE;
    use crate::wifi::netlink::STATUS_CODE;
    use crate::wifi::netlink::TIMED_OUT;
    use crate::wifi::netlink::add_attribute;
    use crate::wifi::netlink::messages;
    use crate::wifi::sae::tests::Toy;
    use crate::wifi::sae::tests::body_of;
    use crate::wifi::sae::tests::frame_from;
    use crate::wifi::sae::tests::send_body;
    use crate::wifi::sae::tests::to_bytes;
    use crate::wifi::wpa::tests::Authenticator;
    use crate::wifi::wpa::tests::MASTER_KEY;
    use crate::wifi::wpa::tests::NETWORK;
    use crate::wifi::wpa::tests::PLAIN;
    use crate::wifi::wpa::tests::STATION;
    use crate::wifi::wpa::tests::Sums;
    use crate::wifi::wpa::tests::WPA3;
    use core::cell::Cell;

    fn event(command: u8, attributes: &[u8]) -> Vec<u8> {
        netlink::request(21, 0, 0, command, 1, attributes)
    }

    fn handle(attempt: &mut Attempt, curve: Option<&Toy>, datagram: &[u8]) -> Vec<Action> {
        attempt.handle(&Sums, curve, &messages(datagram).next().unwrap())
    }

    fn connect_event(status: u16, request_ie: &[u8]) -> Vec<u8> {
        let mut attributes = Vec::new();
        add_attribute(&mut attributes, STATUS_CODE, &status.to_le_bytes());
        add_attribute(&mut attributes, REQ_IE, request_ie);
        event(CONNECT, &attributes)
    }

    fn auth_event(access: [u8; 6]) -> Vec<u8> {
        let mut attributes = Vec::new();
        add_attribute(&mut attributes, EXTERNAL_AUTH_ACTION, &0u32.to_le_bytes());
        add_attribute(&mut attributes, BSSID, &access);
        event(EXTERNAL_AUTH, &attributes)
    }

    fn frame_event(frame: &[u8]) -> Vec<u8> {
        event(FRAME, &netlink::frame_attributes(30, frame))
    }

    fn frame_of(actions: Vec<Action>) -> Vec<u8> {
        match &actions[..] {
            [Action::Send(frame)] => frame.clone(),
            other => panic!("no frame to send and nothing else: {other:?}"),
        }
    }

    #[test]
    fn request_by_offer() {
        let (key, point) = (Credential::Pmk(MASTER_KEY), Credential::Pwe(to_bytes(55)));
        let mut open = Attempt::new(&[], Some(key.clone()), STATION, NETWORK).unwrap();
        let request = open.request(b"cafe", 2412);
        assert_eq!(
            (request.ie, request.group, request.akm, request.is_sae, open.uses_sae()),
            (&[][..], 0, 0, false, false),
            "a network that asks for nothing is joined with no word of keys, whatever is known of a password"
        );
        assert!(open.frame_registrations(30).is_empty(), "nor any frame asked for");
        assert!(!open.try_finish(), "not before the network has taken the station in");
        assert_eq!(handle(&mut open, None, &connect_event(SUCCESS, &[])), [Action::Joined(None)]);
        assert!(open.try_finish(), "and then at once");
        assert_eq!(
            open.to_log_line(None, 90, 90),
            "joined, taken in after 90 ms, keys in use after 90 ms",
            "no proof to tell"
        );
        let plain = Attempt::new(&PLAIN, Some(key.clone()), STATION, NETWORK).unwrap();
        let request = plain.request(b"home", 2437);
        assert_eq!(
            (request.ssid, request.bssid, request.frequency, request.ie, request.is_sae),
            (&b"home"[..], NETWORK, 2437, &PLAIN[..], false),
            "the element says back what the station took"
        );
        assert_eq!(
            (request.group, request.pairwise, request.akm, request.mfp_required),
            (rsn::Group::Ccmp.suite(), rsn::PAIRWISE, Akm::Psk.suite(), false)
        );
        let wpa3 = Attempt::new(&WPA3, Some(point.clone()), STATION, NETWORK).unwrap();
        assert!(wpa3.uses_sae() && wpa3.is_hashed() && !wpa3.is_sae_done() && wpa3.request(b"home", 5180).is_sae);
        assert_eq!(
            wpa3.frame_registrations(30),
            [
                netlink::register_frame_attributes(30, sae::FRAME_KIND, &[3, 0]),
                netlink::register_frame_attributes(30, 0x00D0, &[127, 0, 0, 0])
            ],
            "SAE's frames are asked for, and action frames, without which the driver hands up none"
        );
        let refusal =
            |rsn: &[u8], credential: Option<Credential>| Attempt::new(rsn, credential, STATION, NETWORK).err();
        assert_eq!(
            [
                refusal(&PLAIN, Some(point)),
                refusal(&PLAIN, None),
                refusal(&WPA3, Some(key.clone())),
                refusal(&WPA3, None),
                refusal(&PLAIN[..10], Some(key))
            ],
            [Some(JoinError::NotSpoken); 5],
            "the handshake takes no point, SAE no key, neither nothing, and an offer cut short is none"
        );
    }

    #[test]
    fn kernel_events() {
        let mut plain = Attempt::new(&PLAIN, Some(Credential::Pmk(MASTER_KEY)), STATION, NETWORK).unwrap();
        let other = [48, 2, 1, 0, 244, 1, 0x20];
        assert_eq!(
            handle(&mut plain, None, &connect_event(SUCCESS, &[&[0, 1, b'x'][..], &PLAIN].concat())),
            [Action::Joined(None)],
            "the association request had what the station chose"
        );
        assert_eq!(
            handle(&mut plain, None, &connect_event(SUCCESS, &[&[0, 1, b'x'][..], &other].concat())),
            [Action::Joined(Some(other.to_vec()))],
            "the driver wrote others, which the log is told of"
        );
        assert_eq!(handle(&mut plain, None, &connect_event(17, &[])), [Action::Failed(JoinError::Refused(17))]);
        let mut late = Vec::new();
        add_attribute(&mut late, STATUS_CODE, &SUCCESS.to_le_bytes());
        add_attribute(&mut late, TIMED_OUT, &[]);
        assert_eq!(
            handle(&mut plain, None, &event(CONNECT, &late)),
            [Action::Failed(JoinError::Silent)],
            "a join that timed out"
        );
        let mut lost = Vec::new();
        add_attribute(&mut lost, REASON_CODE, &15u16.to_le_bytes());
        assert_eq!(handle(&mut plain, None, &event(DISCONNECT, &lost)), [Action::Failed(JoinError::Lost(15))]);
        assert!(handle(&mut plain, None, &event(netlink::NEW_KEY, &[])).is_empty(), "anything else asks nothing");
        assert_eq!(
            handle(&mut plain, None, &auth_event(NETWORK)),
            [Action::Report(false), Action::Failed(JoinError::Sae(sae::Error::Arithmetic))],
            "SAE that is due without a point to prove with fails"
        );
    }

    fn begin_access(curve: &Toy, point: [u8; POINT]) -> (sae::Station, Vec<u8>) {
        sae::Station::begin(curve, point, true).unwrap()
    }

    #[test]
    fn sae_then_handshake() {
        let (station_curve, access_curve) = (Toy(Cell::new(1)), Toy(Cell::new(2)));
        let mut attempt = Attempt::new(&WPA3, Some(Credential::Pwe(to_bytes(55))), STATION, NETWORK).unwrap();
        assert!(
            handle(&mut attempt, Some(&station_curve), &auth_event([2, 0, 0, 0, 0, 7])).is_empty(),
            "due at another access point"
        );
        let commit = frame_of(handle(&mut attempt, Some(&station_curve), &auth_event(NETWORK)));
        let (mut ap, ap_commit) = begin_access(&access_curve, to_bytes(55));
        let confirm = frame_of(handle(
            &mut attempt,
            Some(&station_curve),
            &frame_event(&frame_from(&NETWORK, &STATION, &ap_commit)),
        ));
        let ap_confirm = send_body(&ap.take(&Sums, &access_curve, body_of(&commit)).unwrap());
        assert!(!attempt.is_sae_done());
        assert_eq!(
            handle(&mut attempt, Some(&station_curve), &frame_event(&frame_from(&NETWORK, &STATION, &ap_confirm))),
            [Action::Report(true)],
            "the access point's proof is the password's: the driver is told"
        );
        assert!(attempt.is_sae_done() && !attempt.try_finish());
        let agreement = ap.take(&Sums, &access_curve, body_of(&confirm)).unwrap();
        let [sae::Step::Agreed { pmk, .. }] = &agreement[..] else { panic!("the access point does not agree") };
        let elements = attempt.elements.clone();
        assert_eq!(
            handle(&mut attempt, Some(&station_curve), &connect_event(SUCCESS, &elements)),
            [Action::Joined(None)]
        );
        let mut network = Authenticator::new(*pmk, &WPA3);
        let second = attempt.take_eapol(&Sums, &network.first()).unwrap();
        let [wpa::Step::Send(second)] = &second[..] else { panic!("no answer to the first frame") };
        let third = network.third(second, &network.data(&[0x33; 16], Some(&[0x44; 16]))).unwrap();
        let steps = attempt.take_eapol(&Sums, &third).unwrap();
        assert!(
            matches!(
                steps[..],
                [wpa::Step::Send(_), wpa::Step::Pairwise(_), wpa::Step::Group(_), wpa::Step::Management(_)]
            ),
            "the handshake starts from SAE's key, which the network shares"
        );
        assert!(attempt.try_finish() && !attempt.is_sae_done(), "through, the proof is let go of");
        assert_eq!(
            attempt.to_log_line(Some(40), 120, 300),
            "joined, the password proved by SAE over a point hashed to after 40 ms, taken in after 120 ms, keys in use after 300 ms"
        );
        assert!(
            attempt.is_from_access_point(&[NETWORK.as_slice(), &[0, 0]].concat()),
            "the address the frame came from"
        );
        assert!(
            !attempt.is_from_access_point(&[2, 0, 0, 0, 0, 7, 0, 0]) && !attempt.is_from_access_point(&NETWORK[..5])
        );
        assert!(
            handle(&mut attempt, Some(&station_curve), &frame_event(&frame_from(&NETWORK, &STATION, &ap_confirm)))
                .is_empty(),
            "and what comes of SAE after it changes nothing"
        );
    }

    #[test]
    fn failed_sae() {
        let curve = Toy(Cell::new(1));
        let mut attempt = Attempt::new(&WPA3, Some(Credential::Pwe(to_bytes(55))), STATION, NETWORK).unwrap();
        let commit = frame_of(handle(&mut attempt, Some(&curve), &auth_event(NETWORK)));
        let stranger = frame_from(&[2, 0, 0, 0, 0, 7], &STATION, &[3, 0, 1, 0, 0, 0]);
        assert!(
            handle(&mut attempt, Some(&curve), &frame_event(&stranger)).is_empty(),
            "a frame of another access point"
        );
        assert_eq!(
            handle(
                &mut attempt,
                Some(&curve),
                &frame_event(&frame_from(&NETWORK, &STATION, &[3, 0, 2, 0, 0, 0, 1, 0]))
            ),
            [Action::Skipped(sae::Error::OutOfTurn)],
            "a proof before the access point committed could be anybody's"
        );
        let wrong = Toy(Cell::new(2));
        let (mut ap, ap_commit) = begin_access(&wrong, to_bytes(56));
        frame_of(handle(&mut attempt, Some(&curve), &frame_event(&frame_from(&NETWORK, &STATION, &ap_commit))));
        let ap_confirm = send_body(&ap.take(&Sums, &wrong, body_of(&commit)).unwrap());
        assert_eq!(
            handle(&mut attempt, Some(&curve), &frame_event(&frame_from(&NETWORK, &STATION, &ap_confirm))),
            [Action::Report(false), Action::Failed(JoinError::Sae(sae::Error::Proof))],
            "an access point of another password: the driver is told, and the join is over"
        );
        assert!(!attempt.is_sae_done() && !attempt.try_finish());
    }

    #[test]
    fn eapol_before_handshake() {
        let mut wpa3 = Attempt::new(&WPA3, Some(Credential::Pwe(to_bytes(55))), STATION, NETWORK).unwrap();
        let mut network = Authenticator::new(MASTER_KEY, &WPA3);
        assert_eq!(wpa3.take_eapol(&Sums, &network.first()), Ok(Vec::new()), "SAE has not proved the password yet");
        let mut plain = Attempt::new(&PLAIN, Some(Credential::Pmk(MASTER_KEY)), STATION, NETWORK).unwrap();
        let mut network = Authenticator::new(MASTER_KEY, &PLAIN);
        assert_eq!(plain.take_eapol(&Sums, &network.first()).map(|steps| steps.len()), Ok(1));
        assert!(!plain.try_finish(), "neither taken in nor agreed");
    }

    #[test]
    fn trial_error_of_join_error() {
        let ends = [JoinError::Sae(sae::Error::Arithmetic), JoinError::Lost(15), JoinError::Late];
        assert_eq!(ends.map(|error| error.to_trial_error(true)), [TrialError::WrongPassword; 3]);
        assert_eq!(
            ends.map(|error| error.to_trial_error(false)),
            [TrialError::NotLetIn; 3],
            "a network that asks for none has no password to be wrong"
        );
        for error in [JoinError::Refused(17), JoinError::Silent, JoinError::NotSpoken] {
            assert_eq!(
                (error.to_trial_error(true), error.to_trial_error(false)),
                (TrialError::NotLetIn, TrialError::NotLetIn),
                "{error:?}: the network did not take the station in, whatever the password"
            );
        }
    }

    #[test]
    fn next_wait_done_late() {
        let milli = Duration::from_millis(1);
        let mut open = Attempt::new(&[], None, STATION, NETWORK).unwrap();
        assert_eq!(
            [open.next(TIMEOUT - milli, TIMEOUT), open.next(TIMEOUT, TIMEOUT)],
            [Next::Wait(milli), Next::Failed(JoinError::Late)],
            "the network's word is waited for as long as the patience left, and at the limit the join is late"
        );
        assert_eq!(
            open.next(BACKED_LIMIT, Limits::BACKED.link),
            Next::Failed(JoinError::Late),
            "where the modem stands in, a join is late far sooner"
        );
        assert_eq!(handle(&mut open, None, &connect_event(SUCCESS, &[])), [Action::Joined(None)]);
        open.note_proof(30 * milli);
        open.note_join(90 * milli);
        assert_eq!(
            open.next(120 * milli, TIMEOUT),
            Next::Done("wifi: joined, taken in after 90 ms, keys in use after 120 ms".into()),
            "no proof to tell without SAE"
        );
    }

    #[test]
    fn limits_and_culprits() {
        assert_eq!(Limits::ALONE, Limits { link: TIMEOUT, address: TIMEOUT, reach: None }, "alone a join proves none");
        assert!(
            Limits::BACKED.link < TIMEOUT && Limits::BACKED.address < TIMEOUT && Limits::BACKED.reach.is_some(),
            "where the modem stands in, every step is cut short and the network's reach is proved"
        );
        assert!(
            Limits::BACKED.address > Duration::from_millis(3100),
            "an address still comes from a server that probes it first"
        );
        let [(_, retry), (_, asks)] = arp::JOIN_WRITES;
        let (retry, asks) = (Duration::from_millis(retry.parse().unwrap()), asks.parse::<u32>().unwrap());
        assert_eq!(
            Limits::BACKED.reach,
            Some(retry * asks),
            "a router that did not answer the asking for its address is asked again for as long as the proof lasts"
        );
        let home = known::Network { name: "home".into(), password: "secret12".into() };
        let entry = Entry { name: "home".into(), is_locked: true };
        assert_eq!(
            [
                LinkError::NotJoined(JoinError::Late).to_culprit(&home),
                LinkError::NoAddress(JoinError::Silent).to_culprit(&home),
                LinkError::Unreached(JoinError::Silent).to_culprit(&home),
                LinkError::GaveWay.to_culprit(&home),
                LinkError::Failed("joining: no driver".into()).to_culprit(&home)
            ],
            [Some(entry.clone()), Some(entry.clone()), Some(entry.clone()), None, None],
            "a network that did not serve is passed over, not one the firmware never got to"
        );
        let unreached = LinkError::Unreached(JoinError::Silent);
        assert_eq!(
            [
                unreached.to_stay_end(Some(&home)),
                unreached.to_stay_end(None),
                LinkError::GaveWay.to_stay_end(Some(&home))
            ],
            [
                StayEnd::Failed("no reach: Silent".into(), Some(entry)),
                StayEnd::Failed("no reach: Silent".into(), None),
                StayEnd::GaveWay
            ],
            "a stay's join ends with its reason and the network that did not serve, where one was set out for; one that gave way is made again"
        );
    }

    #[test]
    fn link_error_text_and_trial() {
        let gave_way = JoinError::GaveWay;
        assert_eq!(
            [LinkError::from_join(gave_way), LinkError::from_address(gave_way), LinkError::from_reach(gave_way)],
            [LinkError::GaveWay, LinkError::GaveWay, LinkError::GaveWay],
            "a new wish that came first ends a join, an asking for an address and a proof of reach alike"
        );
        let errors = [
            LinkError::GaveWay,
            LinkError::from_join(JoinError::Lost(15)),
            LinkError::from_join(JoinError::Refused(17)),
            LinkError::from_address(JoinError::Silent),
            LinkError::from_reach(JoinError::Silent),
            LinkError::from_reach(JoinError::Lost(3)),
            LinkError::Failed("joining: no driver".into()),
        ];
        assert_eq!(
            errors.clone().map(|error| error.to_log_line()),
            [
                "gave way to a new wish",
                "not joined: Lost(15)",
                "not joined: Refused(17)",
                "no address: Silent",
                "no reach: Silent",
                "no reach: Lost(3)",
                "joining: no driver"
            ],
            "a network no resolver answered through, or whose link went meanwhile, has reached nothing"
        );
        assert_eq!(
            errors.clone().map(|error| error.to_trial_error(true)),
            [
                None,
                Some(TrialError::WrongPassword),
                Some(TrialError::NotLetIn),
                Some(TrialError::NoAddress),
                Some(TrialError::NoAddress),
                Some(TrialError::NoAddress),
                Some(TrialError::NoRadio)
            ],
            "a giving way tells the list nothing, as the join is made again"
        );
        assert_eq!(
            errors[1].to_trial_error(false),
            Some(TrialError::NotLetIn),
            "a network that asks for no password has none to be wrong"
        );
    }

    #[test]
    fn link_lines() {
        assert_eq!(
            [to_lost_line(JoinError::Lost(15)), to_unread_line("Network is down (os error 100)")],
            ["wifi: out of the network: Lost(15)", "wifi: out of the network: Network is down (os error 100)"],
            "a loss the network told and one a read told"
        );
        assert_eq!(to_rsn_line(&[48, 2, 1, 0]), "wifi: the driver's association request says of RSN 30020100");
        assert_eq!(to_skipped_line(sae::Error::OutOfTurn), "wifi: a frame of SAE was not taken: OutOfTurn");
        let mut plain = Attempt::new(&PLAIN, Some(Credential::Pmk(MASTER_KEY)), STATION, NETWORK).unwrap();
        let error = plain.take_eapol(&Sums, &[]).expect_err("no frame of the handshake");
        assert_eq!(to_untaken_line(error), format!("wifi: a frame of the handshake was not taken: {error:?}"));
        let home = known::Network { name: "home".into(), password: "secret12".into() };
        let access = scan::tests::beacon("home", 7, 5180, -52);
        let (point, key) = (Credential::Pwe(to_bytes(55)), Credential::Pmk(MASTER_KEY));
        assert_eq!(
            [Some(&point), Some(&key), None].map(|credential| to_joining_line(&home, &access, credential, 12)),
            [
                "wifi: joining \"home\" on 5180 MHz, heard at -52 dBm; the password's point for SAE took 12 ms",
                "wifi: joining \"home\" on 5180 MHz, heard at -52 dBm; the password's key took 12 ms",
                "wifi: joining \"home\" on 5180 MHz, heard at -52 dBm; the password's key took 12 ms"
            ],
            "what was made of the password is named by what the join takes"
        );
        assert_eq!(to_stay_line(840), "wifi: in the network 840 ms after the look for it began");
        assert_eq!(
            to_proved_line(900),
            "wifi: the network is proved 900 ms after the look for it began, and left again"
        );
        assert_eq!(to_unheard_line(17), "wifi: the network to prove is not heard among 17 access points");
    }

    #[test]
    fn purpose_setup() {
        assert!(
            Purpose::Stay.is_routed() && Purpose::Stay.to_ipv6_writes() == JOIN_WRITES,
            "a stay sets the interface up for its traffic"
        );
        assert!(
            !Purpose::Trial.is_routed() && Purpose::Trial.to_ipv6_writes() == [("disable_ipv6", "1")],
            "a trial sets no route and gives IPv6 up, so that it takes no traffic of a stay over the modem"
        );
    }

    #[test]
    fn disconnect_errors() {
        let disconnects = [JoinError::GaveWay, JoinError::Sae(sae::Error::Proof), JoinError::Late];
        let others = [JoinError::Refused(17), JoinError::Silent, JoinError::Lost(3), JoinError::NotSpoken];
        assert!(
            disconnects.iter().all(|error| error.should_disconnect())
                && !others.iter().any(|error| error.should_disconnect())
        );
    }
}

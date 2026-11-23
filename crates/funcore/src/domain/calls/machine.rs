use super::direct::{Call, Collision, Effect, Way};
use super::group;
use super::picture::{Arrival, Frame};
use super::state::{Answer, State, Wishes};
use super::{Order, View};
use crate::codec::http::{Head, OK};
use crate::domain::cause::Cause;
use crate::hw::sound::call::Packet;
use crate::net::ice::{self, Agent, Role, Socket};
use crate::net::rtp::estimate::Bounds;
use crate::net::rtp::extension::AudioLevel;
use crate::net::rtp::layout::Kind as Streams;
use crate::net::rtp::session::{Encoded, Session, Taken};
use crate::net::stun::{Transport, Uri};
use crate::net::{rtp, stun, turn};
use crate::signal;
use crate::signal::address::{Peer, ServiceId};
use crate::signal::call::data::{Message, Sending};
use crate::signal::call::sfu;
use crate::signal::call::{self, BUSY_RESPONSE, Candidate, Codec, GroupSignal, Kind, Parameters, Signal};
use crate::signal::command::{Command, Outbound, Target};
use crate::signal::curve::Pair;
use crate::signal::device::{Course, Incoming, Note};
use crate::signal::service::{Relay, Request};
use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};
use core::time::Duration;

// The most this side wants sent to it, in bits a second: what RingRTC's low data mode asks (`DataMode::Low`), as the screen shows a far picture small and the modem's connection may carry it.
const MAX_BITRATE: u64 = 300_000;
// What a one-to-one call sends, in bits a second: RingRTC's least and its most over a relay (connection.rs, `MIN_SEND_RATE`, `RELAYED_MAX_SEND_RATE`), and the rate libwebrtc's factory begins at and stops at (peer_connection_factory.cc), which RingRTC leaves as they are.
const LEAST_SEND: u64 = 30_000;
const FIRST_SEND: u64 = 300_000;
const MOST_SEND: u64 = 2_000_000;
const MOST_RELAYED: u64 = 1_000_000;
const BOUNDS: Bounds = Bounds { least: LEAST_SEND, start: FIRST_SEND, most: MOST_SEND };
// The far side's streams a way takes: its sound, its picture, the picture sent again, and its data.
const FAR_STREAMS: usize = 4;
// The calling server, the one far side of a group call's agent.
const SERVER: u32 = 0;
const NO_AGENT_LINE: &str = "calls: the library gave no random bytes for a call";
const NO_KEYS_LINE: &str = "calls: no keys agreed with a device of the far side";
const UNHELD_LINE: &str = "calls: no stay in a network for the call";

/// What a call asks of the library: Signal's primitives, STUN's and SRTP's.
pub trait Library: signal::Crypto + stun::Crypto + rtp::Crypto {}

impl<T: signal::Crypto + stun::Crypto + rtp::Crypto> Library for T {}

/// What the calls' thread does next, one effect each; it has no `Debug`, as it holds the far side's sound.
pub enum Step {
    // Holds the network for the call, and waits for it; `Machine::take_hold` takes what came of it.
    Hold,
    // Lets go of the hold.
    Release,
    // Reads the addresses the device holds in the stay; `Answer::Addresses` brings them.
    Addresses,
    // Opens a UDP socket on the local address, any port; `Machine::take_bound` takes what came of it.
    Bind { socket: Socket, local: IpAddr },
    // Connects a TCP stream, under TLS checked by the name where one is given; `Machine::take_connected` takes what came of it.
    Connect { socket: Socket, local: IpAddr, far: SocketAddr, name: Option<String> },
    // A datagram for the address, from the socket.
    Send { socket: Socket, to: SocketAddr, datagram: Vec<u8> },
    // Bytes for the stream, written as they are.
    Write { socket: Socket, bytes: Vec<u8> },
    Close(Socket),
    // Starts the microphone and the speaker for the call's sound, or stops them.
    Sound(bool),
    // Has the microphone heard no more, or again.
    Mute(bool),
    // A packet of a far member's sound for the speaker: the member's number, the packet's extended sequence number, its RTP timestamp and its Opus.
    Hear { member: u32, sequence: u64, timestamp: u32, payload: Vec<u8> },
    // Asks the calling server a request over HTTPS, under Signal's own roots; `Answer::Fetched` brings what came, by the number.
    Fetch { number: u32, request: Request<'static> },
    Log(String),
    // Waits for news, that long at the most.
    Wait(Option<Duration>),
}

// How the hold of the network stands while a call wants one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Hold {
    Asked,
    Held,
}

// The times of a step: the paced clock's, and the wall clock's in milliseconds for what the log keeps.
#[derive(Clone, Copy)]
struct Clock {
    now: Duration,
    wall: u64,
}

// A device of the far side of a one-to-one call: the session of its way, the most it wants sent to it, and whether its way runs over a relay.
struct Far {
    device: u32,
    session: Session,
    limit: Option<u64>,
    is_relayed: bool,
}

impl Far {
    // RingRTC's cap on what is sent (connection.rs, `set_max_send_bitrate`): the least of what the call may, what the far side wants and what a relay carries, never under the least.
    fn bound(&mut self) {
        let carried = if self.is_relayed { MOST_RELAYED } else { MOST_SEND };
        let most = carried.min(self.limit.unwrap_or(MOST_SEND)).max(LEAST_SEND);
        self.session.set_bounds(LEAST_SEND, most);
    }
}

// A one-to-one call's own: its course, the relays until its agent is made, its key pair, and each device of the far side that answered.
struct Direct {
    call: Call,
    relays: Option<Vec<Relay>>,
    pair: Option<Pair>,
    // The identity keys the call's keys are drawn under, the caller's then the callee's; for a call begun here they come with the first answer.
    identities: Option<([u8; 32], [u8; 32])>,
    fars: Vec<Far>,
    // The device sound and pictures flow with, once the call is talked in.
    media: Option<u32>,
    far: Sending,
}

// A group's call's own: its course, the session of its way to the calling server once joined, the request the server was last asked, by its number and whether it is the join, whether the call was begun with this side's picture, and whether sound and pictures flow.
struct Group {
    call: group::Call,
    session: Option<Session>,
    asked: Option<(u32, bool)>,
    is_video: bool,
    is_flowing: bool,
}

impl Group {
    fn new(call: group::Call, is_video: bool) -> Box<Group> {
        Box::new(Group { call, session: None, asked: None, is_video, is_flowing: false })
    }
}

// A call by its sort, with what that adds: one-to-one, or a group's through the calling server.
enum Sort {
    Direct(Box<Direct>),
    Group(Box<Group>),
}

impl Sort {
    // The session of a side's way: a far device's in a one-to-one call, the calling server's in a group's.
    fn session_mut(&mut self, side: u32) -> Option<&mut Session> {
        match self {
            Sort::Direct(direct) => direct.far_mut(side).map(|far| &mut far.session),
            Sort::Group(group) => group.session.as_mut(),
        }
    }
}

// A call that stands with what carries it: its sort, the agent of its ways, the device's addresses the agent was given or is to be, and what the owner set of this side's sound and picture.
struct Standing {
    sort: Sort,
    name: Option<String>,
    addresses: Option<Vec<IpAddr>>,
    agent: Option<Agent>,
    locals: Vec<IpAddr>,
    is_muted: bool,
    is_video_on: bool,
    since: Option<Duration>,
    // The microphone and the speaker run for the call.
    is_sounding: bool,
    // The next frame of this side's picture is to be a key frame, and none but a key frame goes, as one before was not sent.
    wants_key: bool,
    is_picture_cut: bool,
}

/// The calls' machine: the call that stands, the hold of the network it stands on, and what its thread is to do.
#[derive(Default)]
pub struct Machine {
    standing: Option<Standing>,
    // The call that ended, as its screen shows it until the owner lets it go or another begins.
    ended: Option<View>,
    hold: Option<Hold>,
    steps: VecDeque<Step>,
    commands: Vec<Command>,
    // The requests asked of the calling server so far: the last one's number.
    fetches: u32,
    // The far side's frames to decode, in order, not yet taken.
    arrivals: Vec<Arrival>,
}

// The servers a relay of the service's stands for, by the addresses it names, so that no name is looked up; `had` is when the service handed it out.
fn to_servers(relay: &Relay, had: Duration) -> (Vec<turn::Server>, Vec<SocketAddr>) {
    let expires = relay.ttl.map_or(Duration::MAX, |ttl| had.saturating_add(Duration::from_secs(ttl)));
    let (mut servers, mut stuns) = (Vec::new(), Vec::new());
    for uri in relay.urls_with_ips.iter().filter_map(|text| Uri::parse(text)) {
        let Ok(ip) = uri.host.parse::<IpAddr>() else { continue };
        let address = SocketAddr::new(ip, uri.port);
        if uri.is_relay {
            servers.push(turn::Server {
                address,
                transport: uri.transport,
                name: relay.hostname.clone().filter(|_| uri.transport == Transport::Tls),
                username: relay.username.clone(),
                password: relay.password.clone(),
                expires,
            });
        } else if uri.transport == Transport::Udp {
            stuns.push(address);
        }
    }
    (servers, stuns)
}

impl Direct {
    fn new(call: Call) -> Direct {
        Direct {
            call,
            relays: None,
            pair: None,
            identities: None,
            fars: Vec::new(),
            media: None,
            far: Sending::default(),
        }
    }

    fn peer(&self) -> Peer {
        Peer::Contact(self.call.peer())
    }

    fn far_mut(&mut self, device: u32) -> Option<&mut Far> {
        self.fars.iter_mut().find(|far| far.device == device)
    }

    fn note(&self, wall: u64) -> Note {
        let kind = if self.call.is_video() { Kind::Video } else { Kind::Audio };
        let ringer = (!self.call.is_outgoing()).then_some(self.call.peer());
        Note { peer: self.peer(), call: self.call.id(), kind, is_outgoing: self.call.is_outgoing(), ringer, at: wall }
    }
}

// What this side sends, as a one-to-one call's far side is told it.
const fn to_sending(is_muted: bool, is_video_on: bool) -> Sending {
    Sending { is_video_on: Some(is_video_on), is_sharing_screen: None, is_audio_on: Some(!is_muted) }
}

impl Standing {
    fn new(sort: Sort, name: Option<String>) -> Standing {
        Standing {
            sort,
            name,
            addresses: None,
            agent: None,
            locals: Vec::new(),
            is_muted: false,
            is_video_on: false,
            since: None,
            is_sounding: false,
            wants_key: true,
            is_picture_cut: false,
        }
    }

    // The call as its screen shows it; none for one that came in and does not ring yet.
    fn view(&self) -> Option<View> {
        let (peer, is_outgoing, is_video, phase, has_far_video, members) = match &self.sort {
            Sort::Direct(direct) => {
                let (call, has_far_video) = (&direct.call, direct.far.is_video_on == Some(true));
                (direct.peer(), call.is_outgoing(), call.is_video(), call.phase()?, has_far_video, Vec::new())
            }
            Sort::Group(group) => {
                let call = &group.call;
                (call.group(), call.is_outgoing(), group.is_video, call.phase(), call.has_shown_video(), call.members())
            }
        };
        Some(View {
            peer,
            name: self.name.clone(),
            is_outgoing,
            is_video,
            phase,
            since: self.since,
            is_muted: self.is_muted,
            is_video_on: self.is_video_on,
            has_far_video,
            members,
        })
    }

    // True where the call has ended and is to be let go of.
    fn is_over(&self) -> bool {
        match &self.sort {
            Sort::Direct(direct) => direct.call.ending().is_some(),
            Sort::Group(group) => group.call.ending().is_some(),
        }
    }

    // The way sound and pictures flow by once the call is talked in: its side at the agent, and its session.
    fn media_mut(&mut self) -> Option<(u32, &mut Session)> {
        match &mut self.sort {
            Sort::Direct(direct) => {
                let device = direct.media?;
                direct.far_mut(device).map(|far| (device, &mut far.session))
            }
            Sort::Group(group) => group.session.as_mut().filter(|_| group.is_flowing).map(|session| (SERVER, session)),
        }
    }

    fn media(&self) -> Option<&Session> {
        match &self.sort {
            Sort::Direct(direct) => {
                let device = direct.media?;
                direct.fars.iter().find(|far| far.device == device).map(|far| &far.session)
            }
            Sort::Group(group) => group.session.as_ref().filter(|_| group.is_flowing),
        }
    }

    // What a frame of sound or of a picture goes out as: itself in a one-to-one call, sealed for the members in a group's; none where it cannot be sealed.
    fn seal<'f>(&mut self, crypto: &impl Library, frame: &'f [u8]) -> Option<Cow<'f, [u8]>> {
        match &mut self.sort {
            Sort::Direct(_) => Some(Cow::Borrowed(frame)),
            Sort::Group(group) => group.call.seal(crypto, frame).map(Cow::Owned),
        }
    }

    // Sends over a side's way what its session has to send now.
    fn flush(&mut self, crypto: &impl Library, side: u32, now: Duration) {
        let (Some(agent), Some(session)) = (self.agent.as_mut(), self.sort.session_mut(side)) else { return };
        while let Some(datagram) = session.poll(crypto, now) {
            agent.send(crypto, side, datagram, now);
        }
    }
}

impl Machine {
    /// Leaves on the board what the thread has for the others before it waits: what goes to Signal's thread, the call as its screen shows it, the rate and the key frame's wish of this side's picture, and the far side's frames, a key frame asked of each sender whose frames the board dropped. Whether the pictures' threads have news of it, and whether the main loop has.
    pub fn leave(&mut self, state: &mut State) -> (bool, bool) {
        let is_news = state.note(self.take_commands(), self.view(), self.is_standing());
        let is_coding_news = state.set_video(self.video_rate(), self.take_key_wish());
        let (is_decoding_news, dropped) = state.add_arrivals(self.take_arrivals());
        self.want_keys(&dropped);
        (is_coding_news || is_decoding_news, is_news || is_coding_news)
    }

    // What goes to Signal's thread, taken once.
    fn take_commands(&mut self) -> Vec<Command> {
        core::mem::take(&mut self.commands)
    }

    // The far side's frames to decode, in order, taken once.
    fn take_arrivals(&mut self) -> Vec<Arrival> {
        core::mem::take(&mut self.arrivals)
    }

    // Asks a key frame of each sender a far frame of whose did not decode or was dropped, which leaves the frames after it nothing to stand on: each by its side at the agent and its number on that way.
    fn want_keys(&mut self, senders: &[(u32, u32)]) {
        let Some(standing) = self.standing.as_mut() else { return };
        for &(side, from) in senders {
            if let Some(session) = standing.sort.session_mut(side) {
                session.want_key(from);
            }
        }
    }

    // The call as its screen shows it: the one that stands, else the one that ended.
    fn view(&self) -> Option<View> {
        match &self.standing {
            Some(standing) => standing.view(),
            None => self.ended.clone(),
        }
    }

    // True while a call stands.
    const fn is_standing(&self) -> bool {
        self.standing.is_some()
    }

    // The one-to-one call that stands.
    fn direct_mut(&mut self) -> Option<&mut Direct> {
        match &mut self.standing.as_mut()?.sort {
            Sort::Direct(direct) => Some(direct),
            Sort::Group(_) => None,
        }
    }

    // The group's call that stands.
    fn group_mut(&mut self) -> Option<&mut Group> {
        match &mut self.standing.as_mut()?.sort {
            Sort::Group(group) => Some(group),
            Sort::Direct(_) => None,
        }
    }

    // Fails the call that stands for a cause.
    fn fail(&mut self, crypto: &impl Library, cause: Cause, clock: Clock) {
        match self.standing.as_mut().map(|standing| &mut standing.sort) {
            Some(Sort::Direct(direct)) => {
                let effects = direct.call.fail(cause);
                self.apply(crypto, effects, clock);
            }
            Some(Sort::Group(group)) => {
                let effects = group.call.fail(crypto, cause);
                self.apply_group(crypto, effects, clock);
            }
            None => {}
        }
    }

    fn log(&mut self, line: &str) {
        self.steps.push_back(Step::Log(String::from(line)));
    }

    fn signal(&mut self, to: ServiceId, signal: Signal) {
        self.commands.push(Command::Signal(Outbound { target: Target::Contact(to), device: None, signal }));
    }

    // Eight random bytes as a number, never nought: a call's id, which Signal's apps drop at nought, and what a way's streams begin their counts from.
    fn draw(crypto: &impl Library) -> Option<u64> {
        let mut bytes = [0u8; 8];
        crypto.random(&mut bytes).then(|| u64::from_le_bytes(bytes).max(1))
    }

    // Has the network held for the call that stands, and the device's addresses asked.
    fn hold_network(&mut self) {
        if self.hold.is_none() {
            self.hold = Some(Hold::Asked);
            self.steps.push_back(Step::Hold);
        } else if self.hold == Some(Hold::Held) {
            self.steps.push_back(Step::Addresses);
        }
    }

    // Begins a call's stand on the network.
    fn stand(&mut self, standing: Standing) {
        (self.standing, self.ended) = (Some(standing), None);
        self.hold_network();
    }

    // Begins a one-to-one call's stand, its relays asked of Signal's service.
    fn stand_direct(&mut self, direct: Direct, name: Option<String>, is_video_on: bool) {
        let mut standing = Standing::new(Sort::Direct(Box::new(direct)), name);
        standing.is_video_on = is_video_on;
        self.commands.push(Command::Relays);
        self.stand(standing);
    }

    /// Takes what came of holding the network.
    pub fn take_hold(&mut self, crypto: &impl Library, is_held: bool, now: Duration, wall: u64) {
        if self.hold != Some(Hold::Asked) {
            return;
        }
        if is_held {
            self.hold = Some(Hold::Held);
            return self.steps.push_back(Step::Addresses);
        }
        self.hold = None;
        self.log(UNHELD_LINE);
        self.fail(crypto, Cause::NoNetwork, Clock { now, wall });
    }

    /// Takes the end of the stay the call's hold made: a call that stands asks for a hold anew, whose stay's addresses its ways are then sought from.
    pub fn take_down(&mut self) {
        if self.hold != Some(Hold::Held) {
            return;
        }
        self.steps.push_back(Step::Release);
        self.hold = self.standing.as_ref().map(|_| Hold::Asked);
        if self.hold.is_some() {
            self.steps.push_back(Step::Hold);
        }
    }

    /// Takes what the main loop and the pictures' threads hand over: the owner's orders, Signal's call messages, relays and signals not sent, what was found for the thread, this side's frames and the senders of far frames that did not decode.
    pub fn take_wishes(&mut self, crypto: &impl Library, wishes: Wishes, now: Duration, wall: u64) {
        let clock = Clock { now, wall };
        if let (Some(relays), Some(direct)) = (wishes.relays, self.direct_mut()) {
            direct.relays.get_or_insert(relays);
        }
        for answer in wishes.answers {
            let addresses = match answer {
                Answer::Addresses(addresses) => addresses,
                Answer::Fetched(number, answer) => {
                    self.take_fetched(crypto, number, answer, clock);
                    continue;
                }
            };
            let Some(standing) = self.standing.as_mut() else { continue };
            let Some(agent) = standing.agent.as_mut() else {
                standing.addresses = Some(addresses);
                continue;
            };
            // The addresses of a new stay take the place of the last one's.
            for gone in standing.locals.iter().filter(|ip| !addresses.contains(ip)) {
                agent.drop_local(crypto, *gone, now);
            }
            addresses.iter().for_each(|ip| agent.local(*ip));
            standing.locals = addresses;
        }
        for (peer, membership) in wishes.tokens {
            let Some(group) = self.group_mut().filter(|group| group.call.group() == peer) else { continue };
            let effects = group.call.take_membership(crypto, membership, now);
            self.apply_group(crypto, effects, clock);
        }
        for incoming in wishes.signals {
            self.take_incoming(crypto, incoming, clock);
        }
        for outbound in wishes.unsent {
            let effects = self.direct_mut().map(|direct| direct.call.note_unsent(&outbound.signal)).unwrap_or_default();
            self.apply(crypto, effects, clock);
        }
        for order in wishes.orders {
            self.take_order(crypto, order, clock);
        }
        self.want_keys(&wishes.flawed);
        self.take_frames(crypto, wishes.frames, now);
        self.proceed(crypto, clock);
    }

    fn take_order(&mut self, crypto: &impl Library, order: Order, clock: Clock) {
        let Some(standing) = self.standing.as_mut() else {
            match order {
                Order::Call { peer: Peer::Contact(peer), name, is_video } => {
                    let Some(id) = Self::draw(crypto) else { return self.log(NO_AGENT_LINE) };
                    let direct = Direct::new(Call::outgoing(id, peer, is_video, clock.now));
                    self.stand_direct(direct, name, is_video);
                }
                // A group's call is joined under the agent's credentials, so the agent is made at once.
                Order::Call { peer, name, is_video } => {
                    let Some(agent) = Agent::new(crypto, Role::Controlling, false) else {
                        return self.log(NO_AGENT_LINE);
                    };
                    let (call, effects) =
                        group::Call::outgoing(peer, is_video, (agent.ufrag(), agent.pwd()), clock.now);
                    let mut standing = Standing::new(Sort::Group(Group::new(call, is_video)), name);
                    (standing.agent, standing.is_video_on) = (Some(agent), is_video);
                    self.stand(standing);
                    self.apply_group(crypto, effects, clock);
                }
                Order::Decline | Order::End => self.ended = None,
                _ => {}
            }
            return;
        };
        // What the owner set of this side's sound and picture is the standing call's, whatever its sort.
        match order {
            Order::Mute(is_muted) => {
                standing.is_muted = is_muted;
                if standing.is_sounding {
                    self.steps.push_back(Step::Mute(is_muted));
                }
            }
            Order::Video(is_on) => {
                standing.is_video_on = is_on;
                standing.wants_key |= is_on;
            }
            _ => {}
        }
        let direct = match &mut standing.sort {
            Sort::Direct(direct) => direct,
            Sort::Group(group) => {
                let effects = match order {
                    // A ring taken: the network is held from now on, and the call joined under the agent's credentials.
                    Order::Accept => {
                        standing.agent = standing.agent.take().or_else(|| Agent::new(crypto, Role::Controlling, false));
                        let Some(agent) = standing.agent.as_ref() else {
                            self.log(NO_AGENT_LINE);
                            return self.fail(crypto, Cause::SessionFailed, clock);
                        };
                        let effects = group.call.accept((agent.ufrag(), agent.pwd()), clock.now);
                        self.hold_network();
                        effects
                    }
                    Order::Decline | Order::End => group.call.end(crypto),
                    Order::Mute(is_muted) => group.call.mute(crypto, is_muted),
                    Order::Video(is_on) => group.call.video(crypto, is_on),
                    Order::Call { .. } => return,
                };
                return self.apply_group(crypto, effects, clock);
            }
        };
        let effects = match order {
            Order::Accept => {
                standing.is_video_on = direct.call.is_video();
                direct.call.set_sending(to_sending(standing.is_muted, standing.is_video_on));
                direct.call.accept(clock.now)
            }
            Order::Decline | Order::End => direct.call.end(),
            Order::Mute(_) | Order::Video(_) => {
                direct.call.set_sending(to_sending(standing.is_muted, standing.is_video_on))
            }
            Order::Call { .. } => return,
        };
        self.apply(crypto, effects, clock);
    }

    // A call message that passed Signal's gate: an offer begins a call, collides with the one that stands or is answered busy; any other signal is the standing call's where its sender is whom the call is with.
    fn take_incoming(&mut self, crypto: &impl Library, incoming: Incoming, clock: Clock) {
        let Incoming { sender, name, group_name, signals, identity, own_identity, own_device, .. } = incoming;
        let (from, device) = (sender.service(), sender.device());
        for signal in signals {
            let is_with = self.direct_mut().is_some_and(|direct| direct.call.peer() == from);
            match signal {
                Signal::Offer { call: id, is_video, parameters: Some(parameters) } if id != 0 => {
                    let is_free = self.standing.is_none();
                    let collision = match self.direct_mut() {
                        None if is_free => Collision::Replaced { with_hangup: false },
                        Some(direct) if is_with => direct.call.collide(device, id),
                        _ => Collision::Busy,
                    };
                    let gives_way = match collision {
                        Collision::Replaced { with_hangup } => Some(with_hangup),
                        Collision::Both => Some(true),
                        Collision::Busy | Collision::Kept => None,
                    };
                    if let Some((with_hangup, direct)) = gives_way.zip(self.direct_mut()) {
                        let effects = direct.call.give_way(with_hangup);
                        self.apply(crypto, effects, clock);
                    }
                    match collision {
                        Collision::Kept => {}
                        Collision::Busy | Collision::Both => {
                            self.signal(from, Signal::Busy { call: id });
                            let kind = if is_video { Kind::Video } else { Kind::Audio };
                            let (peer, ringer) = (Peer::Contact(from), Some(from));
                            let note = Note { peer, call: id, kind, is_outgoing: false, ringer, at: clock.wall };
                            self.commands.push(Command::Course(note, Course::Missed));
                        }
                        Collision::Replaced { .. } => {
                            let call =
                                Call::incoming(id, (from, device), own_device, (is_video, parameters), clock.now);
                            let mut direct = Direct::new(call);
                            direct.identities = Some((identity, own_identity));
                            self.stand_direct(direct, name.clone(), false);
                        }
                    }
                }
                Signal::Group { signal, .. } => self.take_group_signal(crypto, from, group_name.clone(), signal, clock),
                Signal::Offer { .. } => {}
                signal if is_with => {
                    let Some(direct) = self.direct_mut() else { continue };
                    if direct.call.is_outgoing() {
                        direct.identities.get_or_insert((own_identity, identity));
                    }
                    let effects = direct.call.take_signal(device, signal);
                    self.apply(crypto, effects, clock);
                }
                _ => {}
            }
        }
    }

    // A group's call message that passed Signal's gate: the standing call's where it is of its group; a ring where no call stands begins one that rings here, and a ring while another call stands is answered busy to the account's own devices.
    fn take_group_signal(
        &mut self,
        crypto: &impl Library,
        from: ServiceId,
        name: Option<String>,
        signal: GroupSignal,
        clock: Clock,
    ) {
        let peer = signal.group();
        if let Some(group) = self.group_mut().filter(|group| group.call.group() == peer) {
            let effects = group.call.take_signal(crypto, from, &signal, clock.now);
            return self.apply_group(crypto, effects, clock);
        }
        let GroupSignal::Ring { ring, is_cancelled: false, .. } = signal else { return };
        if self.standing.is_some() {
            let signal = GroupSignal::Answered { group: peer, ring, kind: BUSY_RESPONSE };
            let signal = Signal::Group { signal, is_urgent: true };
            return self.commands.push(Command::Signal(Outbound { target: Target::Own, device: None, signal }));
        }
        // A ring holds no network: the call is joined once the owner takes it.
        let (call, effects) = group::Call::ringing(peer, (ring, from), clock.now);
        (self.standing, self.ended) = (Some(Standing::new(Sort::Group(Group::new(call, false)), name)), None);
        self.apply_group(crypto, effects, clock);
    }

    // What the calling server answered the request of that number: a redirect followed, the join's or the peek's answer read, or its status passed on.
    fn take_fetched(&mut self, crypto: &impl Library, number: u32, answer: Option<(Head, Vec<u8>)>, clock: Clock) {
        let Some(group) = self.group_mut() else { return };
        let Some((_, is_join)) = group.asked.take_if(|asked| asked.0 == number) else { return };
        let (call, now) = (&mut group.call, clock.now);
        let status = answer.as_ref().map(|(head, _)| head.status);
        let effects = if let Some(url) = answer.as_ref().and_then(|(head, _)| head.redirect_location()) {
            call.follow(crypto, url, now)
        } else {
            let read = answer.as_ref().filter(|_| status == Some(OK));
            let document = read.and_then(|(_, body)| core::str::from_utf8(body).ok());
            if is_join {
                call.take_joined(crypto, document.and_then(sfu::parse_join).ok_or(status), now)
            } else {
                call.take_peek(crypto, document.and_then(sfu::parse_peek).ok_or(status), now)
            }
        };
        self.apply_group(crypto, effects, clock);
    }

    // Has a request asked of the calling server under the next number, which its answer is known by.
    fn ask(fetches: &mut u32, steps: &mut VecDeque<Step>, group: &mut Group, request: Request<'static>, is_join: bool) {
        *fetches = fetches.wrapping_add(1);
        group.asked = Some((*fetches, is_join));
        steps.push_back(Step::Fetch { number: *fetches, request });
    }

    // Does what a step of a group call's course asks; a call that ended is let go of with its way.
    fn apply_group(&mut self, crypto: &impl Library, effects: Vec<group::Effect>, clock: Clock) {
        let mut queue: VecDeque<group::Effect> = effects.into();
        while let Some(effect) = queue.pop_front() {
            let Some(standing) = self.standing.as_mut() else { return };
            let Sort::Group(group) = &mut standing.sort else { return };
            match effect {
                group::Effect::Membership => self.commands.push(Command::Token(group.call.group())),
                group::Effect::Join(request) => Self::ask(&mut self.fetches, &mut self.steps, group, request, true),
                group::Effect::Peek(request) => Self::ask(&mut self.fetches, &mut self.steps, group, request, false),
                group::Effect::Signal(outbound) => self.commands.push(Command::Signal(outbound)),
                group::Effect::Regather => self.steps.push_back(Step::Addresses),
                group::Effect::Open { demux_id, members, ufrag, pwd, keys } => {
                    // The way takes the streams of as many devices as a call takes at the least, however few the group's members.
                    let (streams, most) =
                        (Streams::Group { demux: demux_id }, FAR_STREAMS * members.max(sfu::CALL_DEVICES));
                    let session = Self::draw(crypto)
                        .and_then(|random| Session::new(streams, crypto, &keys.0, &keys.1, most, BOUNDS, random));
                    match (session, standing.agent.as_mut()) {
                        (Some(session), Some(agent)) => {
                            agent.far_credentials(SERVER, &ufrag, &pwd);
                            group.session = Some(session);
                        }
                        _ => {
                            queue.extend(group.call.fail(crypto, Cause::Refused));
                            self.steps.push_back(Step::Log(String::from(NO_KEYS_LINE)));
                        }
                    }
                }
                group::Effect::Server { address, transport, name } => {
                    if let Some(agent) = standing.agent.as_mut() {
                        agent.far_server(SERVER, address, transport, name.as_deref());
                    }
                }
                group::Effect::Data { is_to_server, payload } => {
                    if let Some(session) = group.session.as_mut() {
                        session.send_data(&payload, is_to_server, clock.now);
                    }
                    standing.flush(crypto, SERVER, clock.now);
                }
                // Sound and pictures flow while another device is in the call, sent at the rate the call's size leaves.
                group::Effect::Media(rate) => {
                    if let (Some(rate), Some(session)) = (rate, group.session.as_mut()) {
                        session.set_bounds(LEAST_SEND, rate.max(LEAST_SEND));
                    }
                    group.is_flowing = rate.is_some();
                    if standing.is_sounding != group.is_flowing {
                        standing.is_sounding = group.is_flowing;
                        self.steps.push_back(Step::Sound(group.is_flowing));
                    }
                    if group.is_flowing {
                        standing.since.get_or_insert(clock.now);
                        if standing.is_muted {
                            self.steps.push_back(Step::Mute(true));
                        }
                    }
                }
                group::Effect::Muted => {
                    standing.is_muted = true;
                    if standing.is_sounding {
                        self.steps.push_back(Step::Mute(true));
                    }
                }
                group::Effect::Log { call, is_outgoing, ringer, course } => {
                    let peer = group.call.group();
                    let note = Note { peer, call, kind: Kind::Group, is_outgoing, ringer, at: clock.wall };
                    self.commands.push(Command::Course(note, course));
                }
            }
        }
        if self.standing.as_ref().is_some_and(Standing::is_over) {
            self.finish(crypto, clock);
        }
    }

    // Goes on with a one-to-one call once its relays, the hold and the device's addresses are in: the agent of its ways, its key pair, and its offer or its answer.
    fn proceed(&mut self, crypto: &impl Library, clock: Clock) {
        let is_held = self.hold == Some(Hold::Held);
        let Some(standing) = self.standing.as_mut().filter(|standing| standing.agent.is_none() && is_held) else {
            return;
        };
        let Sort::Direct(direct) = &mut standing.sort else { return };
        if direct.relays.is_none() || standing.addresses.is_none() {
            return;
        }
        let (relays, addresses) =
            (direct.relays.take().unwrap_or_default(), standing.addresses.take().unwrap_or_default());
        let role = if direct.call.is_outgoing() { Role::Controlling } else { Role::Controlled };
        let made = Agent::new(crypto, role, false).zip(Pair::new(crypto));
        let Some((mut agent, pair)) = made else {
            self.log(NO_AGENT_LINE);
            return self.fail(crypto, Cause::SessionFailed, clock);
        };
        addresses.iter().for_each(|ip| agent.local(*ip));
        for relay in &relays {
            let (servers, stuns) = to_servers(relay, clock.now);
            servers.into_iter().for_each(|server| agent.add_relay(server));
            stuns.into_iter().for_each(|server| agent.add_stun(server));
        }
        let parameters = Parameters {
            public_key: pair.public,
            ufrag: String::from(agent.ufrag()),
            pwd: String::from(agent.pwd()),
            max_bitrate: Some(MAX_BITRATE),
            encodes: alloc::vec![Codec::Vp8],
            decodes: alloc::vec![Codec::Vp8],
        };
        (standing.agent, standing.locals, direct.pair) = (Some(agent), addresses, Some(pair));
        let effects = direct.call.proceed(parameters);
        self.apply(crypto, effects, clock);
    }

    // Does what a step of the call's course asks, and what that brings in turn; a call that ended is let go of with its ways.
    fn apply(&mut self, crypto: &impl Library, effects: Vec<Effect>, clock: Clock) {
        let mut queue: VecDeque<Effect> = effects.into();
        while let Some(effect) = queue.pop_front() {
            let Some(standing) = self.standing.as_mut() else { return };
            let Sort::Direct(direct) = &mut standing.sort else { return };
            match effect {
                Effect::Signal(outbound) => self.commands.push(Command::Signal(outbound)),
                Effect::Log(course) => self.commands.push(Command::Course(direct.note(clock.wall), course)),
                Effect::Tell(device, word) => {
                    if let Some(far) = direct.far_mut(device) {
                        far.session.send_data(&word, false, clock.now);
                    }
                    standing.flush(crypto, device, clock.now);
                }
                Effect::Open(device, parameters) => {
                    let keys = direct.pair.as_ref().zip(direct.identities).and_then(|(pair, (caller, callee))| {
                        call::negotiate(crypto, pair, &parameters.public_key, &caller, &callee)
                    });
                    // The offer's key seals what the caller sends, the answer's what the callee does.
                    let is_caller = direct.call.is_outgoing();
                    let session = keys.zip(Self::draw(crypto)).and_then(|((offer, answer), random)| {
                        let (sending, receiving) = if is_caller { (&offer, &answer) } else { (&answer, &offer) };
                        let streams = Streams::Direct { is_caller };
                        Session::new(streams, crypto, sending, receiving, FAR_STREAMS, BOUNDS, random)
                    });
                    match (session, standing.agent.as_mut()) {
                        (Some(session), Some(agent)) => {
                            agent.far_credentials(device, &parameters.ufrag, &parameters.pwd);
                            let mut far = Far { device, session, limit: parameters.max_bitrate, is_relayed: false };
                            far.bound();
                            direct.fars.push(far);
                        }
                        _ => {
                            queue.extend(direct.call.fail(Cause::Refused));
                            self.log(NO_KEYS_LINE);
                        }
                    }
                }
                Effect::Candidates(device, candidates) => {
                    let Some(agent) = standing.agent.as_mut() else { continue };
                    for candidate in candidates {
                        match candidate {
                            Candidate::Added(line) => agent.far_candidate(device, &line),
                            Candidate::Removed(address) => agent.drop_far(device, address),
                        }
                    }
                }
                Effect::Close(device) => {
                    direct.fars.retain(|far| far.device != device);
                    if let Some(agent) = standing.agent.as_mut() {
                        agent.drop_side(device);
                    }
                }
                Effect::Regather => self.steps.push_back(Step::Addresses),
                Effect::Media(device) => {
                    (standing.since, direct.media, standing.is_sounding) = (Some(clock.now), Some(device), true);
                    self.steps.push_back(Step::Sound(true));
                    if standing.is_muted {
                        self.steps.push_back(Step::Mute(true));
                    }
                }
                Effect::Far(sending) => {
                    direct.far = Sending {
                        is_video_on: sending.is_video_on.or(direct.far.is_video_on),
                        is_sharing_screen: sending.is_sharing_screen.or(direct.far.is_sharing_screen),
                        is_audio_on: sending.is_audio_on.or(direct.far.is_audio_on),
                    };
                }
                Effect::Limit(limit) => {
                    if let Some(far) = direct.media.and_then(|device| direct.far_mut(device)) {
                        far.limit = Some(limit);
                        far.bound();
                    }
                }
            }
        }
        if self.standing.as_ref().is_some_and(Standing::is_over) {
            self.finish(crypto, clock);
        }
    }

    // Lets go of a call that ended: its view kept for its screen, its sound stopped, its agent's last packets sent and its sockets closed, the hold let go.
    fn finish(&mut self, crypto: &impl Library, clock: Clock) {
        let Some(mut standing) = self.standing.take() else { return };
        self.ended = standing.view();
        if standing.is_sounding {
            self.steps.push_back(Step::Sound(false));
        }
        if let Some(mut agent) = standing.agent.take() {
            agent.close(crypto, clock.now);
            while let Some(step) = agent.next(crypto, clock.now) {
                self.steps.extend(to_step(step));
            }
        }
        if self.hold.take().is_some() {
            self.steps.push_back(Step::Release);
        }
    }

    // A step of the agent's in a group's call: the way's state and what came over it are the course's, the sound of every device heard and the picture of the one shown opened for its decoder; the candidates found are told nobody, as the server learns this side's addresses from its checks.
    fn take_group_step(&mut self, crypto: &impl Library, step: ice::Step, clock: Clock) {
        let Some(standing) = self.standing.as_mut() else { return };
        let Sort::Group(group) = &mut standing.sort else { return };
        let now = clock.now;
        let effects = match step {
            ice::Step::Candidate(_) | ice::Step::Removed(_) | ice::Step::Selected { .. } => return,
            ice::Step::State { state, .. } => {
                let Some(way) = to_way(state) else { return };
                group.call.note_way(crypto, way, now)
            }
            ice::Step::Deliver { mut packet, .. } => {
                match group.session.as_mut().map(|session| session.take(crypto, &mut packet, now)) {
                    Some(Taken::Server { payload }) => group.call.take_update(crypto, &payload, now),
                    Some(Taken::Data { from, timestamp, payload }) => {
                        group.call.take_relayed(crypto, (from, timestamp), &payload, now)
                    }
                    Some(Taken::Audio { from, sequence, timestamp, payload, .. }) if group.is_flowing => {
                        if let Some(payload) = group.call.open(crypto, from, &payload) {
                            self.steps.push_back(Step::Hear { member: from, sequence, timestamp, payload });
                        }
                        return;
                    }
                    Some(Taken::Video { from, frames }) if group.is_flowing && group.call.shown() == Some(from) => {
                        for frame in frames {
                            match frame.open(|layer| group.call.open(crypto, from, layer)) {
                                Some(opened) => self.arrivals.push(Arrival::new(&opened, (SERVER, from))),
                                // A frame that does not open leaves the pictures after it nothing to stand on.
                                None => group.session.iter_mut().for_each(|session| session.want_key(from)),
                            }
                        }
                        return;
                    }
                    Some(Taken::KeyFrame) => {
                        standing.wants_key = true;
                        return;
                    }
                    _ => return,
                }
            }
            step => {
                self.steps.extend(to_step(step));
                return;
            }
        };
        self.apply_group(crypto, effects, clock);
    }

    // Takes the agent's steps until it has none: its sockets' are the thread's, the rest the call's.
    fn drain(&mut self, crypto: &impl Library, clock: Clock) {
        loop {
            let Some(agent) = self.agent() else { return };
            let mut told = Vec::new();
            while let Some(step) = agent.next(crypto, clock.now) {
                told.push(step);
            }
            if told.is_empty() {
                return;
            }
            let mut found = Vec::new();
            for step in told {
                let Some(standing) = self.standing.as_mut() else { return };
                let direct = match &mut standing.sort {
                    Sort::Direct(direct) => direct,
                    Sort::Group(_) => {
                        self.take_group_step(crypto, step, clock);
                        continue;
                    }
                };
                let effects = match step {
                    ice::Step::Candidate(line) => {
                        found.push(Candidate::Added(line));
                        continue;
                    }
                    ice::Step::Removed(address) => {
                        found.push(Candidate::Removed(address));
                        continue;
                    }
                    ice::Step::State { side, state } => {
                        let Some(way) = to_way(state) else { continue };
                        direct.call.note_way(side, way, clock.now)
                    }
                    ice::Step::Deliver { side, mut packet } => {
                        let is_heard = direct.media == Some(side);
                        let taken = direct.far_mut(side).map(|far| far.session.take(crypto, &mut packet, clock.now));
                        match taken {
                            Some(Taken::Data { payload, .. }) => match Message::read(&payload) {
                                Some(word) => direct.call.take_word(side, &word, clock.now),
                                None => continue,
                            },
                            Some(Taken::Audio { from, sequence, timestamp, payload, .. }) if is_heard => {
                                self.steps.push_back(Step::Hear { member: from, sequence, timestamp, payload });
                                continue;
                            }
                            Some(Taken::Video { from, frames }) if is_heard => {
                                self.arrivals.extend(frames.iter().map(|frame| Arrival::new(frame, (side, from))));
                                continue;
                            }
                            Some(Taken::KeyFrame) if is_heard => {
                                standing.wants_key = true;
                                continue;
                            }
                            _ => continue,
                        }
                    }
                    ice::Step::Selected { side, path } => {
                        if let Some(far) = direct.far_mut(side) {
                            far.is_relayed = path.is_some_and(|path| path.is_relayed());
                            far.bound();
                        }
                        continue;
                    }
                    step => {
                        self.steps.extend(to_step(step));
                        continue;
                    }
                };
                self.apply(crypto, effects, clock);
            }
            if let Some(direct) = self.direct_mut() {
                let effects = direct.call.tell_candidates(found);
                self.apply(crypto, effects, clock);
            }
        }
    }

    fn agent(&mut self) -> Option<&mut Agent> {
        self.standing.as_mut().and_then(|standing| standing.agent.as_mut())
    }

    /// Takes what came of opening a socket: the address it is bound to, or none.
    pub fn take_bound(&mut self, crypto: &impl Library, socket: Socket, bound: Option<SocketAddr>, now: Duration) {
        if let Some(agent) = self.agent() {
            agent.take_bound(crypto, socket, bound, now);
        }
    }

    /// Takes what came of connecting a stream: the address it is connected from, or none.
    pub fn take_connected(
        &mut self,
        crypto: &impl Library,
        socket: Socket,
        connected: Option<SocketAddr>,
        now: Duration,
    ) {
        if let Some(agent) = self.agent() {
            agent.take_connected(crypto, socket, connected, now);
        }
    }

    /// Takes a datagram that came to a socket from an address.
    pub fn take_datagram(
        &mut self,
        crypto: &impl Library,
        socket: Socket,
        from: SocketAddr,
        datagram: &[u8],
        now: Duration,
    ) {
        if let Some(agent) = self.agent() {
            agent.take_datagram(crypto, socket, from, datagram, now);
        }
    }

    /// Takes bytes that came on a stream.
    pub fn take_stream(&mut self, crypto: &impl Library, socket: Socket, bytes: &[u8], now: Duration) {
        if let Some(agent) = self.agent() {
            agent.take_stream(crypto, socket, bytes, now);
        }
    }

    /// Takes the end of a socket.
    pub fn take_closed(&mut self, socket: Socket) {
        if let Some(agent) = self.agent() {
            agent.take_closed(socket);
        }
    }

    /// Takes the packets of the microphone's sound, which go to the device the call is talked in with.
    pub fn take_sound(&mut self, crypto: &impl Library, packets: Vec<Packet>, now: Duration) {
        let Some(standing) = self.standing.as_mut() else { return };
        let Some(side) = standing.media_mut().map(|(side, _)| side) else { return };
        for packet in &packets {
            let level = AudioLevel { level: packet.level, is_voice: packet.is_voice };
            let Some(opus) = standing.seal(crypto, &packet.opus) else { continue };
            if let Some((_, session)) = standing.media_mut() {
                session.send_audio(&opus, packet.sampled, level, now);
            }
        }
        standing.flush(crypto, side, now);
    }

    // The bits a second this side's picture may be coded at, while it goes to the far side; none while none goes.
    fn video_rate(&self) -> Option<u64> {
        let standing = self.standing.as_ref().filter(|standing| standing.is_video_on)?;
        standing.media().map(Session::video_rate)
    }

    // True, once, where the next frame of this side's picture is to be a key frame.
    fn take_key_wish(&mut self) -> bool {
        self.standing.as_mut().is_some_and(|standing| core::mem::take(&mut standing.wants_key))
    }

    // Takes the frames of this side's picture as their encoder wrote them, which go to the device the call is talked in with while the picture is on.
    fn take_frames(&mut self, crypto: &impl Library, frames: Vec<Frame>, now: Duration) {
        let Some(standing) = self.standing.as_mut().filter(|standing| standing.is_video_on) else { return };
        let mut is_cut = standing.is_picture_cut;
        let Some(side) = standing.media_mut().map(|(side, _)| side) else { return };
        for frame in &frames {
            // A frame after one that was not sent has nothing to stand on at the far side: none goes until a key frame does.
            if is_cut && !frame.is_key {
                continue;
            }
            let sealed = standing.seal(crypto, &frame.bytes);
            let sent = sealed.zip(standing.media_mut()).is_some_and(|(bytes, (_, session))| {
                let encoded = Encoded {
                    bytes: &bytes,
                    is_key: frame.is_key,
                    width: frame.size.0,
                    height: frame.size.1,
                    frame_rate: frame.frame_rate,
                    sampled: frame.sampled,
                    orientation: frame.orientation,
                };
                session.send_video(encoded, now)
            });
            is_cut = !sent;
        }
        standing.wants_key |= is_cut && !standing.is_picture_cut;
        standing.is_picture_cut = is_cut;
        standing.flush(crypto, side, now);
    }

    /// The thread's next step at `now`, by the paced clock, with the wall clock's time in milliseconds for the log.
    pub fn next(&mut self, crypto: &impl Library, now: Duration, wall: u64) -> Step {
        if let Some(step) = self.steps.pop_front() {
            return step;
        }
        let clock = Clock { now, wall };
        match self.standing.as_mut().map(|standing| &mut standing.sort) {
            Some(Sort::Direct(direct)) => {
                let effects = direct.call.tick(now);
                self.apply(crypto, effects, clock);
            }
            Some(Sort::Group(group)) => {
                let effects = group.call.tick(crypto, now);
                self.apply_group(crypto, effects, clock);
            }
            None => {}
        }
        self.proceed(crypto, clock);
        self.drain(crypto, clock);
        // What the streams of the call's sound and pictures owe on the clock goes with every look: their reports, their feedback and what the pacer lets go.
        let stream = self.standing.as_mut().and_then(|standing| {
            let side = standing.media_mut()?.0;
            standing.flush(crypto, side, now);
            standing.media().map(|session| session.next_time(now).saturating_sub(now))
        });
        self.drain(crypto, clock);
        if let Some(step) = self.steps.pop_front() {
            return step;
        }
        let call = match self.standing.as_ref().map(|standing| &standing.sort) {
            Some(Sort::Direct(direct)) => direct.call.next_wait(now),
            Some(Sort::Group(group)) => group.call.next_wait(now),
            None => None,
        };
        let agent = self.agent().and_then(|agent| agent.next_wait(now));
        Step::Wait(call.into_iter().chain(agent).chain(stream).min())
    }
}

// What a state of a side's way is to a call's course; none while it is still sought for the first time.
const fn to_way(state: ice::State) -> Option<Way> {
    match state {
        ice::State::Connecting => None,
        ice::State::Connected => Some(Way::Up),
        ice::State::Disconnected => Some(Way::Down),
        ice::State::Failed => Some(Way::Lost),
    }
}

// The thread's step of one of the agent's that is a socket's or a log line; none for what the call takes itself.
fn to_step(step: ice::Step) -> Option<Step> {
    Some(match step {
        ice::Step::Bind { socket, local } => Step::Bind { socket, local },
        ice::Step::Connect { socket, local, far, name } => Step::Connect { socket, local, far, name },
        ice::Step::Send { socket, to, datagram } => Step::Send { socket, to, datagram },
        ice::Step::Write { socket, bytes } => Step::Write { socket, bytes },
        ice::Step::Close(socket) => Step::Close(socket),
        ice::Step::Log(line) => Step::Log(line),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::picture::Pictures;
    use super::super::state::Decoding;
    use super::super::{Ending, Member, Phase};
    use super::*;
    use crate::base::code::{hex, unhex};
    use crate::base::json;
    use crate::base::pixel::PixelFormat;
    use crate::codec::ivf;
    use crate::codec::vp8::tests::CALL;
    use crate::codec::vp8::{Decoder, is_key_frame};
    use crate::crypto::exact;
    use crate::gfx::{Picture, to_fill};
    use crate::net::rtp::extension::{Orientation, Turn};
    use crate::net::wireguard::tests::Toy;
    use crate::signal::address::tests::address;
    use crate::signal::call::frame;
    use crate::signal::call::sfu::tests::{joined_or_left, numbered, speaker};
    use crate::signal::call::sfu::{Heartbeat, Relayed};
    use crate::signal::call::{ACCEPTED, DECLINED};
    use crate::signal::device::{GroupMember, Membership};
    use crate::signal::tests::Standin;
    use alloc::format;

    #[test]
    fn relay_to_servers() {
        let relay = Relay {
            username: "user".into(),
            password: "pass".into(),
            urls: alloc::vec!["turn:turn.example.org".into()],
            urls_with_ips: alloc::vec![
                "turn:192.0.2.9".into(),
                "turn:192.0.2.9:80?transport=tcp".into(),
                "turns:[2001:db8::9]:443?transport=tcp".into(),
                "stun:192.0.2.9".into(),
                "turn:turn.example.org".into(),
                "http://192.0.2.9".into(),
            ],
            hostname: Some("turn.example.org".into()),
            ttl: Some(600),
        };
        let (servers, stuns) = to_servers(&relay, Duration::from_secs(10));
        let told: Vec<(SocketAddr, Transport, Option<&str>)> =
            servers.iter().map(|server| (server.address, server.transport, server.name.as_deref())).collect();
        assert_eq!(
            told,
            [
                ("192.0.2.9:3478".parse().unwrap(), Transport::Udp, None),
                ("192.0.2.9:80".parse().unwrap(), Transport::Tcp, None),
                ("[2001:db8::9]:443".parse().unwrap(), Transport::Tls, Some("turn.example.org")),
            ],
            "each address a server, the name for the certificate under TLS alone; a name and what is no relay passed over"
        );
        assert!(servers.iter().all(|server| {
            (server.username.as_str(), server.password.as_str(), server.expires)
                == ("user", "pass", Duration::from_secs(610))
        }));
        assert_eq!(stuns, ["192.0.2.9:3478".parse::<SocketAddr>().unwrap()], "its STUN server over UDP");
        let lasting = Relay { ttl: None, ..relay };
        assert_eq!(to_servers(&lasting, Duration::ZERO).0[0].expires, Duration::MAX, "no time named: no end");
    }

    // Signal's stand-in with the standards' hashes for STUN and the tunnel's toy for SRTP, so that it is a call's whole library.
    impl stun::Crypto for Standin {
        fn hmac_sha1(&self, key: &[u8], parts: &[&[u8]]) -> [u8; stun::SHA1_BYTES] {
            exact::hmac(exact::sha1, key, parts)
        }

        fn md5(&self, parts: &[&[u8]]) -> [u8; stun::MD5_BYTES] {
            exact::md5(parts)
        }

        fn sha256(&self, parts: &[&[u8]]) -> [u8; 32] {
            exact::sha256(parts)
        }
    }

    impl rtp::Crypto for Standin {
        fn seal(
            &self,
            key: &rtp::Key,
            nonce: &[u8; rtp::NONCE_BYTES],
            extra: &[u8],
            data: &mut [u8],
        ) -> Option<[u8; rtp::TAG_BYTES]> {
            rtp::Crypto::seal(&Toy::default(), key, nonce, extra, data)
        }

        fn open(&self, key: &rtp::Key, nonce: &[u8; rtp::NONCE_BYTES], extra: &[u8], sealed: &mut [u8]) -> bool {
            rtp::Crypto::open(&Toy::default(), key, nonce, extra, sealed)
        }

        fn encrypt(&self, key: &rtp::Key, block: &[u8; rtp::BLOCK_BYTES]) -> Option<[u8; rtp::BLOCK_BYTES]> {
            rtp::Crypto::encrypt(&Toy::default(), key, block)
        }
    }

    const WALL: u64 = 1_800_000_000_000;
    const ANN: ServiceId = ServiceId::Aci([0xA1; 16]);
    const BEN: ServiceId = ServiceId::Aci([0xB2; 16]);
    const CAT: ServiceId = ServiceId::Aci([0xC3; 16]);

    // A device in a call: its machine, the state its threads share, the far pictures' decoders and the picture they make in, the screen's far picture, its account and device number, its address, the sockets it bound, and what it asked of Signal's thread and let go of.
    struct Side {
        machine: Machine,
        state: State,
        far: Pictures,
        made: Picture,
        screen: Picture,
        service: ServiceId,
        device: u32,
        ip: IpAddr,
        bound: Vec<(Socket, SocketAddr)>,
        courses: Vec<Course>,
        asked: Vec<Command>,
        is_held: bool,
        is_released: bool,
        // The requests it has for the calling server, each by its number.
        fetches: Vec<(u32, Request<'static>)>,
        // Whether its microphone and speaker run and whether the microphone is muted, and the far side's sound handed to its speaker.
        is_sounding: bool,
        is_muted: bool,
        heard: Vec<(u32, u64, Vec<u8>)>,
        // The far side's pictures as its screen takes them.
        pictures: Vec<Picture>,
    }

    impl Side {
        fn new(service: ServiceId, device: u32, ip: &str) -> Side {
            Side {
                machine: Machine::default(),
                state: State::new(PixelFormat::XRGB8888),
                far: Pictures::default(),
                made: Picture::default(),
                screen: Picture::default(),
                service,
                device,
                ip: ip.parse().unwrap(),
                bound: Vec::new(),
                courses: Vec::new(),
                asked: Vec::new(),
                is_held: false,
                is_released: false,
                fetches: Vec::new(),
                is_sounding: false,
                is_muted: false,
                heard: Vec::new(),
                pictures: Vec::new(),
            }
        }

        fn identity(&self) -> [u8; 32] {
            [self.device as u8; 32]
        }

        fn phase(&self) -> Option<Phase> {
            self.machine.view().map(|view| view.phase)
        }

        // Hands the far side's frames over as the calls' thread does at its wait, and decodes them as the far pictures' thread does: each picture for the screen, each flaw back to the machine.
        fn decode(&mut self) {
            let (machine, state) = (&mut self.machine, &mut self.state);
            state.note(Vec::new(), machine.view(), machine.is_standing());
            let (_, dropped) = state.add_arrivals(machine.take_arrivals());
            machine.want_keys(&dropped);
            while let Some(step) = state.next_arrival() {
                match step {
                    Decoding::Decode { arrival, room, format } => {
                        let shown = self.far.decode(arrival).show(room, format, &mut self.made);
                        state.add_shown(shown, &mut self.made, Duration::ZERO, Duration::ZERO);
                    }
                    Decoding::Rest => (self.far, self.made) = (Pictures::default(), Picture::default()),
                }
            }
            if state.take_report(&mut self.screen).has_picture {
                self.pictures.push(self.screen.clone());
            }
            machine.want_keys(&state.take_wishes().flawed);
        }

        // Has the device do its steps until it waits, its datagrams put on the wire and its far frames decoded: the time it waits at the most, and what it asked of Signal's thread.
        fn pump(&mut self, crypto: &Standin, now: Duration, wire: &mut Wire) -> (Option<Duration>, Vec<Command>) {
            let wait = loop {
                match self.machine.next(crypto, now, WALL) {
                    Step::Wait(wait) => {
                        self.decode();
                        break wait;
                    }
                    Step::Hold => {
                        self.is_held = true;
                        self.machine.take_hold(crypto, true, now, WALL);
                    }
                    Step::Release => self.is_released = true,
                    Step::Addresses => {
                        let answers = alloc::vec![Answer::Addresses(alloc::vec![self.ip])];
                        let wishes = Wishes { answers, relays: Some(Vec::new()), ..Wishes::default() };
                        self.machine.take_wishes(crypto, wishes, now, WALL);
                    }
                    Step::Bind { socket, local } => {
                        let address = SocketAddr::new(local, 40_000 + self.bound.len() as u16);
                        self.bound.push((socket, address));
                        self.machine.take_bound(crypto, socket, Some(address), now);
                    }
                    Step::Connect { socket, .. } => self.machine.take_connected(crypto, socket, None, now),
                    Step::Send { socket, to, datagram } => {
                        if let Some((_, from)) = self.bound.iter().find(|(bound, _)| *bound == socket) {
                            wire.push((*from, to, datagram));
                        }
                    }
                    Step::Close(socket) => self.bound.retain(|(bound, _)| *bound != socket),
                    Step::Sound(is_on) => self.is_sounding = is_on,
                    Step::Mute(is_muted) => self.is_muted = is_muted,
                    Step::Hear { member, sequence, payload, .. } => self.heard.push((member, sequence, payload)),
                    Step::Fetch { number, request } => self.fetches.push((number, request)),
                    Step::Write { .. } | Step::Log(_) => {}
                }
            };
            (wait, self.machine.take_commands())
        }
    }

    // Datagrams on their way: where each came from, where it goes, and its bytes.
    type Wire = Vec<(SocketAddr, SocketAddr, Vec<u8>)>;

    // Two devices, a network that carries their datagrams between the addresses they bound, and Signal's service, which carries each one's signals to the other as call messages that passed the gate.
    struct World {
        sides: [Side; 2],
        crypto: Standin,
        now: Duration,
        datagrams: Wire,
    }

    impl World {
        fn new() -> World {
            World {
                sides: [Side::new(ANN, 1, "198.51.100.10"), Side::new(BEN, 2, "203.0.113.20")],
                crypto: Standin::default(),
                now: Duration::from_secs(100),
                datagrams: Vec::new(),
            }
        }

        fn order(&mut self, at: usize, order: Order) {
            let wishes = Wishes { orders: alloc::vec![order], ..Wishes::default() };
            self.sides[at].machine.take_wishes(&self.crypto, wishes, self.now, WALL);
        }

        // A call message from `sender` to the side `at`, as Signal's gate hands it on.
        fn deliver(&mut self, at: usize, sender: (ServiceId, u32, [u8; 32]), signal: Signal) {
            let side = &mut self.sides[at];
            let incoming = Incoming {
                sender: address(sender.0, sender.1),
                name: Some("Far".into()),
                group_name: None,
                signals: alloc::vec![signal],
                age: 0,
                is_accepted: true,
                identity: sender.2,
                own_identity: side.identity(),
                own_device: side.device,
            };
            let wishes = Wishes { signals: alloc::vec![incoming], ..Wishes::default() };
            side.machine.take_wishes(&self.crypto, wishes, self.now, WALL);
        }

        // Has a side do its steps until it waits, and carries what it asked of Signal's thread; the time it waits at the most.
        fn pump(&mut self, at: usize) -> Option<Duration> {
            let (wait, commands) = self.sides[at].pump(&self.crypto, self.now, &mut self.datagrams);
            let side = &self.sides[at];
            let sender = (side.service, side.device, side.identity());
            for command in commands {
                match command {
                    Command::Signal(Outbound { target: Target::Contact(to), device, signal }) => {
                        let other = 1 - at;
                        let is_for = to == self.sides[other].service
                            && device.is_none_or(|device| device == self.sides[other].device);
                        if is_for {
                            self.deliver(other, sender, signal);
                        }
                    }
                    Command::Course(_, course) => self.sides[at].courses.push(course),
                    command => self.sides[at].asked.push(command),
                }
            }
            wait
        }

        // Runs both devices and the network on for a span.
        fn run(&mut self, span: Duration) {
            let until = self.now + span;
            while self.now < until {
                let waits = [self.pump(0), self.pump(1)];
                let datagrams = core::mem::take(&mut self.datagrams);
                let is_idle = datagrams.is_empty();
                for (from, to, bytes) in datagrams {
                    for side in &mut self.sides {
                        if let Some((socket, _)) = side.bound.iter().find(|(_, address)| *address == to) {
                            side.machine.take_datagram(&self.crypto, *socket, from, &bytes, self.now);
                        }
                    }
                }
                if is_idle {
                    let wait = waits.into_iter().flatten().min().unwrap_or(span);
                    self.now += wait.clamp(Duration::from_millis(1), Duration::from_millis(20));
                }
            }
        }
    }

    #[test]
    fn two_devices_call() {
        let mut world = World::new();
        world.order(0, Order::Call { peer: Peer::Contact(BEN), name: Some("Ben".into()), is_video: false });
        assert!(world.sides[0].machine.is_standing() && world.sides[0].phase() == Some(Phase::Calling));
        world.run(Duration::from_secs(3));
        assert_eq!(world.sides[0].asked, [Command::Relays], "the relays asked of Signal's service at the click");
        assert_eq!(world.sides[1].asked, [Command::Relays], "and at the offer");
        assert_eq!(
            (world.sides[0].phase(), world.sides[1].phase()),
            (Some(Phase::Ringing), Some(Phase::Incoming)),
            "the way stands: it rings on both sides"
        );
        let callee = world.sides[1].machine.view().unwrap();
        assert_eq!((callee.name.as_deref(), callee.is_outgoing, callee.since), (Some("Far"), false, None));
        world.order(1, Order::Accept);
        world.run(Duration::from_secs(1));
        assert_eq!(
            (world.sides[0].phase(), world.sides[1].phase()),
            (Some(Phase::Talking), Some(Phase::Talking)),
            "taken: the callee's word over the sealed way reached the caller"
        );
        assert!(world.sides[0].machine.view().unwrap().since.is_some(), "the talking's start told its screen");
        assert_eq!(world.sides[0].courses, [Course::Ringing, Course::Accepted], "the log's words of the caller");
        assert_eq!(world.sides[1].courses, [Course::Ringing, Course::Accepted], "and of the callee");
        assert!(world.sides.iter().all(|side| side.is_sounding && !side.is_muted), "both sides' sound runs");
        let packets: Vec<Packet> = (0..5u8)
            .map(|number| Packet {
                opus: alloc::vec![number; 240],
                sampled: Duration::from_millis(60) * u32::from(number),
                level: 30,
                is_voice: true,
            })
            .collect();
        world.sides[0].machine.take_sound(&world.crypto, packets.clone(), world.now);
        world.order(1, Order::Mute(true));
        world.run(Duration::from_secs(1));
        let heard = &world.sides[1].heard;
        assert_eq!(
            heard.iter().map(|(member, _, payload)| (*member, payload)).collect::<Vec<_>>(),
            packets.iter().map(|packet| (1000, &packet.opus)).collect::<Vec<_>>(),
            "the caller's sound reaches the callee's speaker, packet for packet"
        );
        assert!(heard.windows(2).all(|pair| pair[1].1 == pair[0].1 + 1), "numbered one after another");
        assert!(world.sides[1].is_muted && world.sides[0].heard.is_empty(), "the callee's microphone muted");
        world.order(0, Order::Video(true));
        world.run(Duration::from_secs(1));
        let (caller, callee) = (world.sides[0].machine.view().unwrap(), world.sides[1].machine.view().unwrap());
        assert!(caller.is_video_on && callee.has_far_video, "the caller's picture is told the callee over the way");
        world.run(Duration::from_secs(20));
        assert_eq!(world.sides[1].phase(), Some(Phase::Talking), "consent kept and nothing times out while they talk");
        world.sides[1].ip = "203.0.113.77".parse().unwrap();
        world.sides[1].machine.take_down();
        world.run(Duration::from_secs(45));
        assert_eq!(
            (world.sides[0].phase(), world.sides[1].phase()),
            (Some(Phase::Talking), Some(Phase::Talking)),
            "the callee's stay ended and another began: the way is found anew from its new address"
        );
        let moved = &world.sides[1];
        assert!(!moved.bound.is_empty() && moved.bound.iter().all(|(_, address)| address.ip() == moved.ip));
        world.order(0, Order::End);
        world.run(Duration::from_secs(1));
        assert_eq!(
            (world.sides[0].phase(), world.sides[1].phase()),
            (Some(Phase::Ended(Ending::HungUp)), Some(Phase::Ended(Ending::FarHungUp))),
            "hung up here, and told there"
        );
        for side in &world.sides {
            assert!(
                !side.machine.is_standing() && side.is_released && side.bound.is_empty() && !side.is_sounding,
                "no call stands: the hold let go, every socket closed, the sound stopped"
            );
        }
        assert_eq!(world.sides[0].courses.last(), Some(&Course::Hangup));
        assert_eq!(world.sides[1].courses.last(), Some(&Course::RemoteHangup));
        world.order(0, Order::End);
        assert!(world.sides[0].machine.view().is_none(), "the ended call let go of by the owner");
    }

    #[test]
    fn pictures_to_far_side() {
        let mut world = World::new();
        world.order(0, Order::Call { peer: Peer::Contact(BEN), name: None, is_video: true });
        world.run(Duration::from_secs(3));
        assert_eq!(world.sides[0].machine.video_rate(), None, "no picture goes before the call is talked in");
        world.order(1, Order::Accept);
        world.run(Duration::from_secs(1));
        let room = (240, 141);
        world.sides[1].state.want(room);
        let caller = &mut world.sides[0].machine;
        assert!(
            caller.video_rate().is_some_and(|rate| (LEAST_SEND..=MOST_SEND).contains(&rate)),
            "talked in: a rate for the encoder within the call's bounds"
        );
        assert!(caller.take_key_wish() && !caller.take_key_wish(), "a picture begins at a key frame, asked once");
        let clip = ivf::frames(CALL);
        let size = Decoder::default().decode(clip[0]).unwrap().map(|picture| (picture.width, picture.height)).unwrap();
        let frame = |bytes: &[u8], sampled: Duration| Frame {
            bytes: bytes.to_vec(),
            is_key: is_key_frame(bytes),
            size: (size.0 as u16, size.1 as u16),
            frame_rate: 30,
            sampled,
            orientation: Orientation { turn: Turn::Quarter, is_flipped: false, is_back: false },
            rate: FIRST_SEND,
            quantizer: 0,
        };
        for bytes in &clip {
            let frames = alloc::vec![frame(bytes, world.now)];
            world.sides[0].machine.take_frames(&world.crypto, frames, world.now);
            world.run(Duration::from_millis(33));
        }
        world.run(Duration::from_millis(200));
        let shown = world.sides[1].pictures.last().expect("the caller's picture on the callee's screen");
        assert_eq!(
            (shown.width, shown.height()),
            to_fill((size.1, size.0), room),
            "turned as its sender asks and as large as the room takes it: {size:?} in {room:?}"
        );
        assert!(world.sides[0].pictures.is_empty(), "the callee sends none");
        let broken = alloc::vec![Frame { is_key: false, ..frame(&[0x30, 0x01, 0x00, 0x9D], world.now) }];
        world.sides[0].machine.take_frames(&world.crypto, broken, world.now);
        world.run(Duration::from_secs(1));
        assert!(
            world.sides[0].machine.take_key_wish(),
            "a frame that did not decode there: a key frame asked of the encoder here"
        );
        world.order(0, Order::Video(false));
        let count = world.sides[1].pictures.len();
        let frames = alloc::vec![frame(clip[0], world.now)];
        world.sides[0].machine.take_frames(&world.crypto, frames, world.now);
        world.run(Duration::from_millis(300));
        assert!(
            world.sides[0].machine.video_rate().is_none() && world.sides[1].pictures.len() == count,
            "the picture switched off: no rate and no frame goes"
        );
        world.order(0, Order::Video(true));
        assert!(world.sides[0].machine.take_key_wish(), "switched on again: from a key frame");
    }

    #[test]
    fn declined_and_busy() {
        let mut world = World::new();
        world.order(0, Order::Call { peer: Peer::Contact(BEN), name: None, is_video: true });
        world.run(Duration::from_secs(3));
        assert_eq!(world.sides[1].phase(), Some(Phase::Incoming));
        let offer = Signal::Offer {
            call: 9,
            is_video: false,
            parameters: Some(Parameters {
                public_key: [5; 32],
                ufrag: "Xy4+".into(),
                pwd: "0123456789abcdefghijkl".into(),
                max_bitrate: None,
                encodes: alloc::vec![Codec::Vp8],
                decodes: alloc::vec![Codec::Vp8],
            }),
        };
        world.deliver(1, (CAT, 1, [3; 32]), offer);
        let commands = world.sides[1].machine.take_commands();
        assert!(
            matches!(
                &commands[..],
                [
                    Command::Signal(Outbound {
                        target: Target::Contact(CAT),
                        device: None,
                        signal: Signal::Busy { call: 9 }
                    }),
                    Command::Course(Note { call: 9, is_outgoing: false, .. }, Course::Missed)
                ]
            ),
            "an offer from another while one rings: busy, and a missed call in the log"
        );
        assert_eq!(world.sides[1].phase(), Some(Phase::Incoming), "the call that rings rings on");
        world.order(1, Order::Decline);
        world.run(Duration::from_secs(1));
        assert_eq!(
            (world.sides[0].phase(), world.sides[1].phase()),
            (Some(Phase::Ended(Ending::Declined)), Some(Phase::Ended(Ending::HungUp))),
            "sent away there"
        );
        assert_eq!(world.sides[0].courses.last(), Some(&Course::RemoteHangup));
        assert_eq!(world.sides[1].courses.last(), Some(&Course::Hangup));
    }

    #[test]
    fn calls_begun_on_both_sides() {
        let mut world = World::new();
        world.order(0, Order::Call { peer: Peer::Contact(BEN), name: None, is_video: false });
        world.order(1, Order::Call { peer: Peer::Contact(ANN), name: None, is_video: false });
        world.run(Duration::from_secs(3));
        let callee = usize::from(world.sides[1].phase() == Some(Phase::Incoming));
        let caller = 1 - callee;
        assert_eq!(
            (world.sides[caller].phase(), world.sides[callee].phase()),
            (Some(Phase::Ringing), Some(Phase::Incoming)),
            "one of the two calls stands on both sides: the other gave way to its offer"
        );
        assert_eq!(
            (&world.sides[caller].courses[..], &world.sides[caller].asked[..]),
            (&[Course::Ringing][..], &[Command::Relays][..]),
            "the call that stays goes its course"
        );
        assert_eq!(
            (&world.sides[callee].courses[..], &world.sides[callee].asked[..]),
            (&[Course::Missed, Course::Ringing][..], &[Command::Relays, Command::Relays][..]),
            "the one that gave way is missed in the log, and the offer's call asks its relays anew and rings"
        );
        assert!(
            world.sides[callee].is_released && !world.sides[caller].is_released,
            "the hold of the call that gave way is let go of, and the offer's call holds anew"
        );
        world.order(callee, Order::Accept);
        world.run(Duration::from_secs(1));
        assert_eq!(
            (world.sides[0].phase(), world.sides[1].phase()),
            (Some(Phase::Talking), Some(Phase::Talking)),
            "and it is taken as any call"
        );
    }

    #[test]
    fn news_left_at_a_wait() {
        let (mut machine, mut state) = (Machine::default(), State::new(PixelFormat::XRGB8888));
        assert_eq!(machine.leave(&mut state), (false, false), "no call: news for nobody");
        let order = Order::Call { peer: Peer::Contact(BEN), name: None, is_video: false };
        let wishes = Wishes { orders: alloc::vec![order], ..Wishes::default() };
        machine.take_wishes(&Standin::default(), wishes, Duration::from_secs(100), WALL);
        assert_eq!(
            machine.leave(&mut state),
            (false, true),
            "a call begun and its word for Signal's thread are the main loop's news alone"
        );
        assert_eq!(state.take_report(&mut Picture::default()).commands, [Command::Relays]);
        assert_eq!(machine.leave(&mut state), (false, false), "told once");
        let mut world = World::new();
        world.order(0, Order::Call { peer: Peer::Contact(BEN), name: None, is_video: true });
        world.run(Duration::from_secs(3));
        world.order(1, Order::Accept);
        world.run(Duration::from_secs(1));
        let caller = &mut world.sides[0];
        assert_eq!(
            caller.machine.leave(&mut caller.state),
            (true, true),
            "a picture that begins to go is news for the pictures' threads, and for the main loop, whose camera makes it"
        );
        assert!(caller.state.wants_picture() && caller.machine.leave(&mut caller.state) == (false, false));
    }

    #[test]
    fn nobody_answers() {
        let mut world = World::new();
        world.order(0, Order::Call { peer: Peer::Contact(CAT), name: None, is_video: false });
        world.run(Duration::from_secs(59));
        assert_eq!(world.sides[0].phase(), Some(Phase::Calling));
        world.run(Duration::from_secs(2));
        assert_eq!(world.sides[0].phase(), Some(Phase::Ended(Ending::Unanswered)), "a minute and no answer");
        assert!(world.sides[0].is_released && world.sides[0].courses == [Course::Missed]);
    }

    const GROUP: Peer = Peer::Group([7; 32]);
    // The demux ids the made-up server gives this device and Ben's.
    const HERE: u32 = 0x1000;
    const THERE: u32 = 0x2000;
    const ERA: &str = "1122334455667788";
    const MOVED: &str = "https://region.example/v2/conference/participants";
    const UPRIGHT: Orientation = Orientation { turn: Turn::None, is_flipped: false, is_back: false };

    fn head(text: &str) -> Head {
        Head::parse(format!("HTTP/1.1 {text}\r\n\r\n").as_bytes()).unwrap().unwrap().0
    }

    fn membership() -> Membership {
        let member =
            |service, opaque: &str, name: &str| GroupMember { service, opaque: opaque.into(), name: Some(name.into()) };
        Membership {
            token: "2:own:group:1700000000:1:mac".into(),
            members: alloc::vec![member(ANN, "a1", "Ann"), member(BEN, "b2", "Ben")],
            own: ANN,
        }
    }

    // A made-up calling server with Ben's device in the call behind it: its end of the way, the session that carries Ben's streams to this side and opens this side's, the keys of Ben's frames and of this side's, who began the call, and what came from this side.
    struct Server {
        agent: Agent,
        socket: Socket,
        address: SocketAddr,
        pair: Pair,
        session: Option<Session>,
        ben: frame::Outbound,
        own: frame::Inbound,
        creator: &'static str,
        taken: Vec<Taken>,
    }

    // What this side sent through the server, opened under its keys: its sound, its pictures, its words to the devices and its messages to the server.
    #[derive(Default)]
    struct Sent {
        sounds: Vec<Option<Vec<u8>>>,
        pictures: Vec<Option<Vec<u8>>>,
        words: Vec<Option<Relayed>>,
        told: Vec<Vec<u8>>,
    }

    impl Server {
        fn new(crypto: &Standin) -> Server {
            let mut agent = Agent::new(crypto, Role::Controlled, false).unwrap();
            let address: SocketAddr = "192.0.2.80:10000".parse().unwrap();
            agent.local(address.ip());
            let Some(ice::Step::Bind { socket, .. }) = agent.next(crypto, Duration::ZERO) else {
                panic!("an agent given an address asks for a socket on it")
            };
            agent.take_bound(crypto, socket, Some(address), Duration::ZERO);
            Server {
                agent,
                socket,
                address,
                pair: Pair::new(crypto).unwrap(),
                session: None,
                ben: frame::Outbound::new(crypto, [0xB2; 32]).unwrap(),
                own: frame::Inbound::default(),
                creator: "a1",
                taken: Vec::new(),
            }
        }

        // Lets the device of a join's request in: its ICE credentials taken, the SRTP keys agreed, and the join's answer.
        fn join(&mut self, crypto: &Standin, request: &Request<'_>) -> String {
            let body = core::str::from_utf8(&request.body).unwrap();
            let text = |name: &str| json::text_at(body, &[name]).unwrap();
            self.agent.far_credentials(SERVER, text("iceUfrag"), text("icePwd"));
            let public: [u8; 32] = unhex(text("dhePublicKey")).unwrap().try_into().unwrap();
            let (device, server) = sfu::negotiate(crypto, &self.pair, &public).unwrap();
            let streams = Streams::Group { demux: THERE };
            self.session = Session::new(streams, crypto, &server, &device, FAR_STREAMS, BOUNDS, 5);
            format!(
                r#"{{"demuxId":{HERE},"udpAddresses":["{}"],"tcpAddresses":[],"tlsAddresses":[],"iceUfrag":"{}","icePwd":"{}","dhePublicKey":"{}","callCreator":"{}","conferenceId":"{ERA}","clientStatus":"ACTIVE"}}"#,
                self.address,
                self.agent.ufrag(),
                self.agent.pwd(),
                hex(&self.pair.public),
                self.creator
            )
        }

        fn session(&mut self) -> &mut Session {
            self.session.as_mut().expect("a device joined")
        }

        // Sends what Ben's streams owe and does the agent's steps: its datagrams go on the wire, and what came from this side over the way is taken.
        fn pump(&mut self, crypto: &Standin, now: Duration, wire: &mut Wire) -> Option<Duration> {
            if let Some(session) = self.session.as_mut() {
                while let Some(datagram) = session.poll(crypto, now) {
                    self.agent.send(crypto, SERVER, datagram, now);
                }
            }
            while let Some(step) = self.agent.next(crypto, now) {
                match step {
                    ice::Step::Send { to, datagram, .. } => wire.push((self.address, to, datagram)),
                    ice::Step::Deliver { mut packet, .. } => {
                        let taken = self.session.as_mut().map(|session| session.take(crypto, &mut packet, now));
                        self.taken.extend(taken.filter(|taken| *taken != Taken::Nothing));
                    }
                    _ => {}
                }
            }
            self.agent.next_wait(now)
        }

        // What came from this side since the last look, opened under the keys it sent Ben.
        fn take_sent(&mut self, crypto: &Standin) -> Sent {
            let mut sent = Sent::default();
            for taken in core::mem::take(&mut self.taken) {
                match taken {
                    Taken::Audio { from: HERE, payload, .. } => sent.sounds.push(self.own.open(crypto, &payload)),
                    Taken::Video { from: HERE, frames } => {
                        sent.pictures.extend(frames.iter().map(|frame| self.own.open(crypto, &frame.join())));
                    }
                    Taken::Data { from: HERE, payload, .. } => {
                        sent.words.push(self.own.open(crypto, &payload).and_then(|plain| Relayed::read(&plain)));
                    }
                    Taken::Server { payload } => sent.told.push(payload),
                    _ => {}
                }
            }
            sent
        }
    }

    // A device in a group's call through the made-up server, with Signal's thread played: the token and the members handed over when asked, the server's answers to its requests, and its media keys given to Ben.
    struct GroupWorld {
        side: Side,
        server: Server,
        crypto: Standin,
        now: Duration,
        wire: Wire,
        // The devices a peek tells, and whether the join was sent on to another address yet.
        devices: Vec<(u32, &'static str)>,
        is_moved: bool,
    }

    impl GroupWorld {
        fn new() -> GroupWorld {
            let crypto = Standin::default();
            GroupWorld {
                side: Side::new(ANN, 1, "198.51.100.10"),
                server: Server::new(&crypto),
                crypto,
                now: Duration::from_secs(100),
                wire: Vec::new(),
                devices: alloc::vec![(HERE, "a1")],
                is_moved: false,
            }
        }

        fn order(&mut self, order: Order) {
            let wishes = Wishes { orders: alloc::vec![order], ..Wishes::default() };
            self.side.machine.take_wishes(&self.crypto, wishes, self.now, WALL);
        }

        // A group's call message from a member, as Signal's gate hands it on.
        fn deliver(&mut self, from: ServiceId, signal: GroupSignal) {
            let incoming = Incoming {
                sender: address(from, 1),
                name: Some("Far".into()),
                group_name: Some("Friends".into()),
                signals: alloc::vec![Signal::Group { signal, is_urgent: false }],
                age: 0,
                is_accepted: true,
                identity: [9; 32],
                own_identity: self.side.identity(),
                own_device: self.side.device,
            };
            let wishes = Wishes { signals: alloc::vec![incoming], ..Wishes::default() };
            self.side.machine.take_wishes(&self.crypto, wishes, self.now, WALL);
        }

        // Answers a request as the calling server does: a join sent on to another address once and let in there, a peek told the devices.
        fn answer(&mut self, number: u32, request: &Request<'static>) -> Answer {
            let is_join = request.method == "PUT";
            let answer = if is_join && !core::mem::replace(&mut self.is_moved, true) {
                (head(&format!("307 Temporary Redirect\r\nLocation: {MOVED}")), Vec::new())
            } else if is_join {
                assert_eq!(request.url, MOVED, "the join made again where the server sent it on to");
                (head("200 OK"), self.server.join(&self.crypto, request).into_bytes())
            } else {
                let device =
                    |(demux_id, user): &(u32, &str)| format!(r#"{{"opaqueUserId":"{user}","demuxId":{demux_id}}}"#);
                let devices: Vec<String> = self.devices.iter().map(device).collect();
                let body = format!(
                    r#"{{"conferenceId":"{ERA}","maxDevices":8,"participants":[{}],"creator":"{}"}}"#,
                    devices.join(","),
                    self.server.creator
                );
                (head("200 OK"), body.into_bytes())
            };
            Answer::Fetched(number, Some(answer))
        }

        // Runs the device, Signal's thread, the server and the network on for a span.
        fn run(&mut self, span: Duration) {
            let until = self.now + span;
            while self.now < until {
                let (wait, commands) = self.side.pump(&self.crypto, self.now, &mut self.wire);
                let mut wishes = Wishes::default();
                for command in commands {
                    match command {
                        Command::Token(peer) => wishes.tokens.push((peer, Some(membership()))),
                        Command::Course(_, course) => self.side.courses.push(course),
                        command => {
                            if let Command::Signal(Outbound {
                                signal: Signal::Group { signal: GroupSignal::MediaKey { ratchet, secret, .. }, .. },
                                ..
                            }) = &command
                            {
                                self.server.own.add(&self.crypto, u8::try_from(*ratchet).unwrap(), *secret);
                            }
                            self.side.asked.push(command);
                        }
                    }
                }
                for (number, request) in core::mem::take(&mut self.side.fetches) {
                    let answer = self.answer(number, &request);
                    wishes.answers.push(answer);
                }
                let is_asked = !wishes.tokens.is_empty() || !wishes.answers.is_empty();
                if is_asked {
                    self.side.machine.take_wishes(&self.crypto, wishes, self.now, WALL);
                }
                let server_wait = self.server.pump(&self.crypto, self.now, &mut self.wire);
                let wire = core::mem::take(&mut self.wire);
                let is_idle = wire.is_empty() && !is_asked;
                for (from, to, bytes) in wire {
                    if to == self.server.address {
                        self.server.agent.take_datagram(&self.crypto, self.server.socket, from, &bytes, self.now);
                    } else if let Some((socket, _)) = self.side.bound.iter().find(|(_, address)| *address == to) {
                        self.side.machine.take_datagram(&self.crypto, *socket, from, &bytes, self.now);
                    }
                }
                if is_idle {
                    let wait = [wait, server_wait].into_iter().flatten().min().unwrap_or(span);
                    self.now += wait.clamp(Duration::from_millis(1), Duration::from_millis(20));
                }
            }
        }
    }

    fn group_signal(target: Target, signal: GroupSignal, is_urgent: bool) -> Command {
        Command::Signal(Outbound { target, device: None, signal: Signal::Group { signal, is_urgent } })
    }

    #[test]
    fn group_call_through_server() {
        let mut world = GroupWorld::new();
        world.order(Order::Call { peer: GROUP, name: Some("Friends".into()), is_video: true });
        assert_eq!(world.side.phase(), Some(Phase::Connecting), "a group's call is joined first");
        world.run(Duration::from_secs(2));
        assert_eq!(
            world.side.phase(),
            Some(Phase::Ringing),
            "joined where the server sent the join on to, the way to it stands, and alone after its ring"
        );
        let Some(Command::Signal(Outbound { signal: Signal::Group { signal: rang, .. }, .. })) =
            world.side.asked.first()
        else {
            panic!("a ring, not {:?}", world.side.asked)
        };
        let GroupSignal::Ring { ring, is_cancelled: false, .. } = *rang else { panic!("a ring, not {rang:?}") };
        assert_eq!(
            (&world.side.asked[..], &world.side.courses[..]),
            (&[group_signal(Target::Group(GROUP), rang.clone(), true)][..], &[Course::Ringing][..]),
            "the creator rings the group, and the log keeps an outgoing ring"
        );
        assert!(!world.side.is_sounding && world.side.machine.video_rate().is_none(), "nobody to talk with yet");
        world.devices.push((THERE, "b2"));
        let now = world.now;
        world.server.session().send_data(&numbered(joined_or_left(), 1, None), true, now);
        world.run(Duration::from_secs(1));
        let key = world.side.asked.last().cloned();
        assert!(
            matches!(
                key,
                Some(Command::Signal(Outbound {
                    target: Target::Contact(BEN),
                    signal: Signal::Group { signal: GroupSignal::MediaKey { demux_id: HERE, ratchet: 1, .. }, .. },
                    ..
                }))
            ),
            "the server told of a device that joined: the devices asked anew, and this side's key sent Ben"
        );
        assert_eq!(world.side.phase(), Some(Phase::Talking), "talked in once Ben's device is in the call");
        assert!(world.side.is_sounding && world.side.machine.view().unwrap().since.is_some(), "the sound runs");
        world.deliver(BEN, GroupSignal::MediaKey { group: GROUP, demux_id: THERE, ratchet: 0, secret: [0xB2; 32] });
        let beat = Heartbeat { is_audio_muted: Some(false), is_video_muted: Some(false), ..Heartbeat::default() };
        let sealed = world.server.ben.seal(&world.crypto, &Relayed::Heartbeat(beat).write()).unwrap();
        world.server.session().send_data(&sealed, false, now);
        world.server.session().send_data(&numbered(speaker(THERE), 2, None), true, now);
        let sounds = [alloc::vec![1u8; 80], alloc::vec![2u8; 80]];
        for (number, opus) in sounds.iter().enumerate() {
            let sealed = world.server.ben.seal(&world.crypto, opus).unwrap();
            let (sampled, level) =
                (Duration::from_millis(60) * number as u32, AudioLevel { level: 30, is_voice: true });
            world.server.session().send_audio(&sealed, sampled, level, world.now);
        }
        world.run(Duration::from_millis(500));
        assert_eq!(
            world.side.heard.iter().map(|(member, _, payload)| (*member, payload)).collect::<Vec<_>>(),
            sounds.iter().map(|opus| (THERE, opus)).collect::<Vec<_>>(),
            "Ben's sound opened under his key, for the speaker by his device"
        );
        let view = world.side.machine.view().unwrap();
        assert_eq!(
            view.members,
            [Member { name: Some("Ben".into()), is_speaking: true, is_muted: false, has_video: true }],
            "Ben by his name, as his heartbeat and the server's word tell him"
        );
        assert!(view.is_video && view.is_video_on && view.has_far_video, "a video call, with Ben's picture to show");
        let room = (240, 141);
        world.side.state.want(room);
        let clip = ivf::frames(CALL);
        let size = Decoder::default().decode(clip[0]).unwrap().map(|picture| (picture.width, picture.height)).unwrap();
        for bytes in &clip {
            let sealed = world.server.ben.seal(&world.crypto, bytes).unwrap();
            let encoded = Encoded {
                bytes: &sealed,
                is_key: is_key_frame(bytes),
                width: size.0 as u16,
                height: size.1 as u16,
                frame_rate: 30,
                sampled: world.now,
                orientation: UPRIGHT,
            };
            assert!(world.server.session().send_video(encoded, world.now), "Ben's frame is queued");
            world.run(Duration::from_millis(33));
        }
        world.run(Duration::from_millis(200));
        let shown = world.side.pictures.last().expect("Ben's picture on the screen");
        assert_eq!((shown.width, shown.height()), to_fill(size, room), "opened, decoded and made the room's size");
        let caller = &mut world.side.machine;
        assert!(caller.video_rate().is_some() && caller.take_key_wish(), "this side's picture goes, from a key frame");
        let packet = Packet { opus: alloc::vec![7; 240], sampled: Duration::ZERO, level: 30, is_voice: true };
        caller.take_sound(&world.crypto, alloc::vec![packet], world.now);
        let frame = Frame {
            bytes: clip[0].to_vec(),
            is_key: true,
            size: (size.0 as u16, size.1 as u16),
            frame_rate: 30,
            sampled: world.now,
            orientation: UPRIGHT,
            rate: FIRST_SEND,
            quantizer: 0,
        };
        caller.take_frames(&world.crypto, alloc::vec![frame], world.now);
        world.run(Duration::from_millis(1500));
        let sent = world.server.take_sent(&world.crypto);
        assert_eq!(sent.sounds, [Some(alloc::vec![7; 240])], "this side's sound opens under the key it sent Ben");
        assert_eq!(sent.pictures, [Some(clip[0].to_vec())], "and so does its picture");
        let beat = Heartbeat { is_audio_muted: Some(false), is_video_muted: Some(false), ..Heartbeat::default() };
        assert!(
            sent.words.contains(&Some(Relayed::Heartbeat(beat))),
            "its heartbeat through the server: heard, with a picture"
        );
        assert!(
            sent.told.contains(&alloc::vec![0x42, 0x02, 0x10, 0x03]),
            "the server's two messages acknowledged on the server's stream"
        );
        world.order(Order::Mute(true));
        world.run(Duration::from_millis(100));
        let muted = Heartbeat { is_audio_muted: Some(true), ..beat };
        assert!(
            world.side.is_muted
                && world.server.take_sent(&world.crypto).words.contains(&Some(Relayed::Heartbeat(muted))),
            "muted: the microphone heard no more, and the others told at once"
        );
        world.order(Order::End);
        world.run(Duration::from_secs(1));
        assert_eq!(world.side.phase(), Some(Phase::Ended(Ending::HungUp)), "left");
        let sent = world.server.take_sent(&world.crypto);
        assert!(
            sent.words.contains(&Some(Relayed::Leaving)) && sent.told == [sfu::write_leave(), sfu::write_leave()],
            "the leave told the devices through the server, and the server twice"
        );
        let leaving = GroupSignal::Leaving { group: GROUP, demux_id: HERE };
        assert_eq!(
            world.side.asked[world.side.asked.len() - 2..],
            [group_signal(Target::Contact(BEN), leaving.clone(), false), group_signal(Target::Own, leaving, false)],
            "and over Signal Ben and the account's own devices"
        );
        let side = &world.side;
        assert!(
            !side.machine.is_standing() && side.is_released && side.bound.is_empty() && !side.is_sounding,
            "no call stands: the hold let go, every socket closed, the sound stopped"
        );
        assert!(ring != 0 && world.side.courses == [Course::Ringing], "the log told the ring alone");
    }

    #[test]
    fn group_ring_taken_or_sent_away() {
        let ring = |group, ring| GroupSignal::Ring { group, ring, is_cancelled: false };
        let answered = |group, ring, kind| group_signal(Target::Own, GroupSignal::Answered { group, ring, kind }, true);
        let mut world = GroupWorld::new();
        world.server.creator = "b2";
        world.deliver(BEN, ring(GROUP, 77));
        world.run(Duration::from_secs(1));
        let view = world.side.machine.view().unwrap();
        assert_eq!(
            (view.phase, view.name.as_deref(), view.is_outgoing, world.side.is_held),
            (Phase::Incoming, Some("Friends"), false, false),
            "a ring rings here under its group's name, and holds no network"
        );
        let other = Peer::Group([8; 32]);
        world.deliver(CAT, ring(other, 78));
        world.run(Duration::from_millis(100));
        assert_eq!(
            (&world.side.asked[..], &world.side.courses[..]),
            (&[answered(other, 78, BUSY_RESPONSE)][..], &[Course::Ringing][..]),
            "a ring of another group meanwhile: the account's other devices told this one is busy"
        );
        world.order(Order::Accept);
        world.run(Duration::from_secs(2));
        assert_eq!(world.side.asked[1], answered(GROUP, 77, ACCEPTED), "taken: the account's other devices told");
        let view = world.side.machine.view().unwrap();
        assert_eq!(
            (view.phase, view.is_video, view.is_video_on, world.side.is_held),
            (Phase::Talking, false, false, true),
            "the network held from the accept on, and Ben's call joined without this side's picture"
        );
        assert_eq!(
            world.side.courses,
            [Course::Ringing, Course::Accepted, Course::Accepted],
            "the log told the ring, its accept and the join"
        );
        let mut world = GroupWorld::new();
        world.deliver(BEN, ring(GROUP, 77));
        world.order(Order::Decline);
        world.run(Duration::from_secs(1));
        assert_eq!(
            (world.side.phase(), &world.side.asked[..], &world.side.courses[..]),
            (
                Some(Phase::Ended(Ending::HungUp)),
                &[answered(GROUP, 77, DECLINED)][..],
                &[Course::Ringing, Course::Missed][..]
            ),
            "sent away: the account's other devices told, and missed in the log"
        );
        let side = &world.side;
        assert!(
            !side.machine.is_standing() && !side.is_held && !side.is_released && side.fetches.is_empty(),
            "a ring sent away never held the network nor asked the server anything"
        );
    }
}

use super::candidate::Candidate;
use super::{Role, Socket, State, Step, to_state_line};
use crate::net::rtp::is_version;
use crate::net::stun::{Class, Crypto, Frames, Message, Transaction, is_message, prefixed};
use crate::net::turn::{self, Client};
use alloc::borrow::Cow;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};
use core::time::Duration;

// The credentials RingRTC makes for Signal's calling server, 4 and 22 letters and digits (`group_call.rs`), the least RFC 8839 allows (5.4); each character is drawn from four random bytes.
const UFRAG_BYTES: usize = 4;
const PWD_BYTES: usize = 22;
const ICE_CHARS: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const DRAW_BYTES: usize = 4;
// No more candidates of this side, nor of the far sides together, nor pairs of all far sides, than ICE checks at the most over all its check lists (RFC 8445, 6.1.2.5).
pub(super) const MOST_PAIRS: usize = 100;
pub(super) const MOST_CANDIDATES: usize = MOST_PAIRS;
// Each pair's check, and the one a triggered check cancelled (RFC 8445, 7.3.1.4).
pub(super) const MOST_CHECKS: usize = 2 * MOST_PAIRS;
// No more far sides than pairs: a side checks nothing without a pair of its own.
pub(super) const MOST_SIDES: usize = MOST_PAIRS;

// How a message reaches a far side and comes from it: a socket of this side, a relay, or a TCP stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Via {
    Udp(Socket),
    Relay(u32),
    Stream(Socket),
}

// What a candidate of this side is sent from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Base {
    // The UDP socket of a host candidate, which its reflexive candidates share.
    Udp(Socket),
    // A relay, by its number.
    Relay(u32),
    // TCP connections from this address, one for each far candidate.
    Tcp(IpAddr),
}

// A candidate of this side.
pub(super) struct Own {
    pub(super) id: u32,
    pub(super) candidate: Candidate,
    pub(super) base: Base,
    pub(super) is_told: bool,
}

// A candidate of a far side, with the name its certificate is checked by where it is reached under TLS.
pub(super) struct Far {
    pub(super) id: u32,
    pub(super) side: u32,
    pub(super) candidate: Candidate,
    pub(super) name: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Status {
    Frozen,
    Waiting,
    Progress,
    Succeeded,
    Failed,
}

pub(super) struct Pair {
    pub(super) id: u32,
    pub(super) side: u32,
    pub(super) local: u32,
    pub(super) far: u32,
    pub(super) priority: u64,
    pub(super) foundation: (String, String),
    pub(super) status: Status,
    // Once a check of it succeeded, the priority of its valid pair's local candidate (RFC 8445, 7.2.5.3.2).
    pub(super) valid: Option<u32>,
    // The next check of it nominates it: this side controls.
    pub(super) nominate: bool,
    // The order of the far side's latest nomination of it: this side is controlled.
    pub(super) heard: Option<u32>,
    // The NOMINATION the far side sent last for it.
    pub(super) seen: u32,
    // When an answer last gave consent (RFC 7675, 5.1).
    pub(super) consent: Option<Duration>,
    pub(super) consent_next: Duration,
    // Consent lost: its credentials are never used on it again (RFC 7675, 5.1).
    pub(super) is_spent: bool,
    pub(super) stream: Option<Socket>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Purpose {
    Pair(u32),
    Consent(u32),
    Gather { local: IpAddr, server: SocketAddr },
}

// A transaction of this side's: a check, a check of consent, or a request to a STUN server.
pub(super) struct Check {
    pub(super) transaction: Transaction,
    pub(super) purpose: Purpose,
    pub(super) bytes: Vec<u8>,
    pub(super) via: Via,
    pub(super) to: SocketAddr,
    pub(super) first: Duration,
    pub(super) next: Duration,
    pub(super) sent: u32,
    pub(super) rto: Duration,
    // The PRIORITY it carried, a peer-reflexive candidate's that its answer finds (RFC 8445, 7.2.5.3.1).
    pub(super) priority: u32,
    // The role it was sent under, which an answer of a role conflict has this side leave (RFC 8445, 7.2.5.1).
    pub(super) role: Role,
    pub(super) is_nominating: bool,
    pub(super) is_cancelled: bool,
}

// An address of this side, its host socket and where that is bound.
pub(super) struct Local {
    pub(super) ip: IpAddr,
    pub(super) socket: Option<Socket>,
    pub(super) candidate: Option<u32>,
    pub(super) preference: u32,
    pub(super) refresh: Option<Duration>,
}

pub(super) struct Relay {
    pub(super) id: u32,
    pub(super) local: IpAddr,
    pub(super) socket: Socket,
    pub(super) client: Client,
    pub(super) frames: Option<Frames>,
    pub(super) is_open: bool,
    pub(super) candidate: Option<u32>,
    pub(super) preference: u32,
    pub(super) permitted: Vec<IpAddr>,
    // The peers of its channels: the far candidates of the pairs the far sides selected through it.
    pub(super) bound: Vec<SocketAddr>,
}

// A TCP connection of a pair with a far side's passive candidate.
pub(super) struct Stream {
    pub(super) socket: Socket,
    pub(super) local: IpAddr,
    pub(super) far: SocketAddr,
    pub(super) frames: Frames,
    pub(super) is_open: bool,
}

// A far side, which checks this side's candidates under its own credentials (a device that answered the offer, a server): the owner's number for it, none while only checks that named its user fragment made it known; its credentials, its triggered checks, its nominations, its selected pair and how it stands.
pub(super) struct Side {
    pub(super) id: u32,
    pub(super) number: Option<u32>,
    pub(super) ufrag: Option<String>,
    pub(super) pwd: Option<String>,
    pub(super) triggered: VecDeque<u32>,
    pub(super) selected: Option<u32>,
    pub(super) selected_at: Duration,
    // The count of the nominations sent to it while this side controls, or the order of those heard from it.
    pub(super) nominations: u32,
    pub(super) state: State,
    pub(super) first_valid: Option<Duration>,
    pub(super) idle_since: Option<Duration>,
}

impl Side {
    fn new(id: u32, number: Option<u32>, ufrag: Option<String>) -> Side {
        Side {
            id,
            number,
            ufrag,
            pwd: None,
            triggered: VecDeque::new(),
            selected: None,
            selected_at: Duration::ZERO,
            nominations: 0,
            state: State::Connecting,
            first_valid: None,
            idle_since: None,
        }
    }
}

/// An ICE agent of one stream and one component: its credentials, its candidates, and toward each far side the check list and the selected pair; it has no `Debug`, as it holds its password.
pub struct Agent {
    pub(super) role: Role,
    pub(super) tiebreaker: u64,
    pub(super) ufrag: String,
    pub(super) pwd: String,
    pub(super) is_relayed: bool,
    pub(super) stuns: Vec<SocketAddr>,
    pub(super) servers: Vec<turn::Server>,
    pub(super) locals: Vec<Local>,
    pub(super) relays: Vec<Relay>,
    pub(super) streams: Vec<Stream>,
    pub(super) owns: Vec<Own>,
    pub(super) sides: Vec<Side>,
    // The user fragments of the far sides let go, whose checks get no answer: the latest, as many as there may be sides.
    pub(super) dropped: VecDeque<String>,
    pub(super) fars: Vec<Far>,
    pub(super) pairs: Vec<Pair>,
    pub(super) checks: Vec<Check>,
    pub(super) gathers: VecDeque<(IpAddr, SocketAddr)>,
    pub(super) paced: Duration,
    // Where the round of the sides' paced checks goes on (RFC 8445, 6.1.4.2).
    pub(super) turn: usize,
    ids: u32,
    sockets: u32,
    pub(super) is_closed: bool,
    pub(super) steps: VecDeque<Step>,
}

// A credential of random letters and digits; none where the library gives no random bytes.
fn credential(crypto: &impl Crypto, length: usize) -> Option<String> {
    let mut bytes = alloc::vec![0u8; DRAW_BYTES * length];
    crypto.random(&mut bytes).then_some(())?;
    let draw = |chunk: &[u8]| {
        ICE_CHARS[(u32::from_le_bytes(chunk.try_into().unwrap_or_default()) % ICE_CHARS.len() as u32) as usize]
    };
    Some(bytes.chunks(DRAW_BYTES).map(|chunk| char::from(draw(chunk))).collect())
}

impl Agent {
    /// An agent of a role, its credentials made of random bytes; `is_relayed` uses and tells relayed candidates alone. None where the library gives no random bytes.
    pub fn new(crypto: &impl Crypto, role: Role, is_relayed: bool) -> Option<Agent> {
        let mut tiebreaker = [0u8; 8];
        crypto.random(&mut tiebreaker).then_some(())?;
        Some(Agent {
            role,
            tiebreaker: u64::from_be_bytes(tiebreaker),
            ufrag: credential(crypto, UFRAG_BYTES)?,
            pwd: credential(crypto, PWD_BYTES)?,
            is_relayed,
            stuns: Vec::new(),
            servers: Vec::new(),
            locals: Vec::new(),
            relays: Vec::new(),
            streams: Vec::new(),
            owns: Vec::new(),
            sides: Vec::new(),
            dropped: VecDeque::new(),
            fars: Vec::new(),
            pairs: Vec::new(),
            checks: Vec::new(),
            gathers: VecDeque::new(),
            paced: Duration::ZERO,
            turn: 0,
            ids: 0,
            sockets: 0,
            is_closed: false,
            steps: VecDeque::new(),
        })
    }

    // This side's user fragment, for every far side.
    pub(crate) fn ufrag(&self) -> &str {
        &self.ufrag
    }

    // This side's password, for every far side.
    pub(crate) fn pwd(&self) -> &str {
        &self.pwd
    }

    // How a far side stands; none for a side the agent does not know.
    #[cfg(test)]
    pub(super) fn state(&self, side: u32) -> Option<State> {
        self.sides.iter().find(|entry| entry.number == Some(side)).map(|entry| entry.state)
    }

    pub(super) fn id(&mut self) -> u32 {
        self.ids += 1;
        self.ids
    }

    pub(super) fn socket(&mut self) -> Socket {
        self.sockets += 1;
        Socket(self.sockets)
    }

    pub(super) fn log(&mut self, line: String) {
        self.steps.push_back(Step::Log(line));
    }

    pub(super) fn side(&self, id: u32) -> Option<&Side> {
        self.sides.iter().find(|side| side.id == id)
    }

    pub(super) fn side_mut(&mut self, id: u32) -> Option<&mut Side> {
        self.sides.iter_mut().find(|side| side.id == id)
    }

    // The far side the owner numbers so, made where it is new and there is room for it.
    pub(super) fn side_of(&mut self, number: u32) -> Option<u32> {
        if let Some(side) = self.sides.iter().find(|side| side.number == Some(number)) {
            return Some(side.id);
        }
        if self.is_closed || self.sides.len() >= MOST_SIDES {
            return None;
        }
        let id = self.id();
        self.sides.push(Side::new(id, Some(number), None));
        Some(id)
    }

    // A far side known by a user fragment its checks named alone, until credentials come with it.
    pub(super) fn add_named(&mut self, ufrag: &str) -> Option<u32> {
        if self.sides.len() >= MOST_SIDES {
            return None;
        }
        let id = self.id();
        self.sides.push(Side::new(id, None, Some(String::from(ufrag))));
        Some(id)
    }

    // Has every far side take up anew whether nothing is left to check, after what may bring it pairs.
    pub(super) fn clear_idle(&mut self) {
        self.sides.iter_mut().for_each(|side| side.idle_since = None);
    }

    pub(super) fn set_state(&mut self, id: u32, state: State) {
        let pairs = self.pairs.iter().filter(|pair| pair.side == id).count();
        let Some(side) = self.side_mut(id) else { return };
        let Some(number) = side.number else { return };
        if core::mem::replace(&mut side.state, state) != state {
            self.steps.push_back(Step::State { side: number, state });
            self.log(to_state_line(number, state, pairs));
        }
    }

    pub(super) fn own(&self, id: u32) -> Option<&Own> {
        self.owns.iter().find(|own| own.id == id)
    }

    pub(super) fn far(&self, id: u32) -> Option<&Far> {
        self.fars.iter().find(|far| far.id == id)
    }

    pub(super) fn pair(&self, id: u32) -> Option<&Pair> {
        self.pairs.iter().find(|pair| pair.id == id)
    }

    pub(super) fn pair_mut(&mut self, id: u32) -> Option<&mut Pair> {
        self.pairs.iter_mut().find(|pair| pair.id == id)
    }

    // True for a pair its far side has selected.
    pub(super) fn is_selected(&self, pair: &Pair) -> bool {
        self.side(pair.side).is_some_and(|side| side.selected == Some(pair.id))
    }

    // The candidate of this side that messages through a way arrive at.
    fn base_of(&self, via: Via) -> Option<u32> {
        match via {
            Via::Udp(socket) => self.locals.iter().find(|local| local.socket == Some(socket))?.candidate,
            Via::Relay(id) => self.relays.iter().find(|relay| relay.id == id)?.candidate,
            Via::Stream(socket) => {
                let stream = self.streams.iter().find(|stream| stream.socket == socket)?;
                let is_tcp = |own: &&Own| own.base == Base::Tcp(stream.local);
                self.owns.iter().find(is_tcp).map(|own| own.id)
            }
        }
    }

    // The way to a far side from a candidate of this side, over the stream where it is TCP.
    pub(super) fn via_of(&self, local: u32, stream: Option<Socket>) -> Option<Via> {
        match self.own(local)?.base {
            Base::Udp(socket) => Some(Via::Udp(socket)),
            Base::Relay(id) => Some(Via::Relay(id)),
            Base::Tcp(_) => stream.map(Via::Stream),
        }
    }

    // Sends a datagram to a far side's address by a way; over a stream the frame is prefixed with its length (RFC 4571, 2).
    pub(super) fn emit(&mut self, crypto: &impl Crypto, via: Via, to: SocketAddr, bytes: Vec<u8>, now: Duration) {
        match via {
            Via::Udp(socket) => self.steps.push_back(Step::Send { socket, to, datagram: bytes }),
            Via::Relay(id) => {
                let Some(at) = self.relays.iter().position(|relay| relay.id == id) else { return };
                let events = self.relays[at].client.send(crypto, &to, &bytes, now);
                self.carry(at, events);
            }
            Via::Stream(socket) => {
                if self.streams.iter().any(|stream| stream.socket == socket && stream.is_open)
                    && let Some(bytes) = prefixed(&bytes)
                {
                    self.steps.push_back(Step::Write { socket, bytes });
                }
            }
        }
    }

    // Has the bytes a relay's client sends go to the relay: datagrams to its address, or written to its stream.
    pub(super) fn carry(&mut self, at: usize, events: Vec<turn::Event>) {
        let relay = &self.relays[at];
        let (socket, address, is_stream) = (relay.socket, relay.client.address(), relay.frames.is_some());
        for event in events {
            if let turn::Event::Send(bytes) = event {
                self.steps.push_back(if is_stream {
                    Step::Write { socket, bytes }
                } else {
                    Step::Send { socket, to: address, datagram: bytes }
                });
            }
        }
    }

    // Takes a datagram that came to a socket from an address.
    pub(crate) fn take_datagram(
        &mut self,
        crypto: &impl Crypto,
        socket: Socket,
        from: SocketAddr,
        datagram: &[u8],
        now: Duration,
    ) {
        if self.is_closed {
            return;
        }
        if let Some(at) = self.relays.iter().position(|relay| relay.socket == socket) {
            if from == self.relays[at].client.address() {
                let events = self.relays[at].client.take(crypto, datagram, now);
                self.relayed(crypto, at, events, now);
            }
            return;
        }
        self.arrive(crypto, Via::Udp(socket), from, Cow::Borrowed(datagram), now);
    }

    // Takes bytes that came on a stream, cut wherever the stream cut them.
    pub(crate) fn take_stream(&mut self, crypto: &impl Crypto, socket: Socket, bytes: &[u8], now: Duration) {
        if self.is_closed {
            return;
        }
        if let Some(at) = self.relays.iter().position(|relay| relay.socket == socket) {
            let Some(messages) = self.relays[at].frames.as_mut().and_then(|frames| frames.take(bytes)) else {
                return self.take_closed(socket);
            };
            for message in messages {
                let Some(at) = self.relays.iter().position(|relay| relay.socket == socket) else { return };
                let events = self.relays[at].client.take(crypto, &message, now);
                self.relayed(crypto, at, events, now);
            }
            return;
        }
        let Some(stream) = self.streams.iter_mut().find(|stream| stream.socket == socket) else { return };
        let far = stream.far;
        let Some(frames) = stream.frames.take(bytes) else { return self.take_closed(socket) };
        for frame in frames {
            self.arrive(crypto, Via::Stream(socket), far, Cow::Owned(frame), now);
        }
    }

    // A datagram from a far side by a way: STUN to the agent, media to the owner for the side with a pair of its source and the way's candidate, its selected pair first, then the one an answer gave consent last (RFC 7983, 7); media its caller owns is handed on as it is.
    pub(super) fn arrive(
        &mut self,
        crypto: &impl Crypto,
        via: Via,
        from: SocketAddr,
        bytes: Cow<'_, [u8]>,
        now: Duration,
    ) {
        let Some(base) = self.base_of(via) else { return };
        if is_message(&bytes) {
            let Some(message) = Message::parse(&bytes) else { return };
            match message.class {
                Class::Request => self.serve(crypto, via, base, from, &message, now),
                Class::Success | Class::Error => self.answer(crypto, via, from, &message, now),
                Class::Indication => {}
            }
        } else if bytes.first().is_some_and(|first| is_version(*first)) {
            let source = self
                .pairs
                .iter()
                .filter(|pair| {
                    pair.local == base && self.far(pair.far).is_some_and(|far| far.candidate.address == from)
                })
                .filter_map(|pair| Some((self.is_selected(pair), pair.consent, self.side(pair.side)?.number?)))
                .max_by_key(|(is_selected, consent, _)| (*is_selected, *consent));
            if let Some((_, _, side)) = source {
                self.steps.push_back(Step::Deliver { side, packet: bytes.into_owned() });
            }
        }
    }

    /// The agent's next step at `now`, what is due done first; none while nothing is to be done.
    pub fn next(&mut self, crypto: &impl Crypto, now: Duration) -> Option<Step> {
        if self.steps.is_empty() && !self.is_closed {
            self.tick(crypto, now);
        }
        self.steps.pop_front()
    }

    // Ends the agent: each allocation let go at its relay (RFC 8656, 8.1) and every socket closed.
    pub(crate) fn close(&mut self, crypto: &impl Crypto, now: Duration) {
        if self.is_closed {
            return;
        }
        for at in 0..self.relays.len() {
            if self.relays[at].is_open {
                let events = self.relays[at].client.close(crypto, now);
                self.carry(at, events);
            }
        }
        let mut sockets: Vec<Socket> = self.locals.iter().filter_map(|local| local.socket).collect();
        sockets.extend(self.relays.iter().map(|relay| relay.socket));
        sockets.extend(self.streams.iter().map(|stream| stream.socket));
        self.steps.extend(sockets.into_iter().map(Step::Close));
        for side in &mut self.sides {
            (side.selected, side.triggered) = (None, VecDeque::new());
        }
        (self.is_closed, self.checks, self.gathers) = (true, Vec::new(), VecDeque::new());
    }
}

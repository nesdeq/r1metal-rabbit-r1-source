use super::agent::{
    Agent, Base, Check, Far, MOST_CANDIDATES, MOST_CHECKS, MOST_PAIRS, MOST_SIDES, Pair, Purpose, Side, Status, Via,
};
use super::candidate::{
    Candidate, Direction, Kind, PRIORITIES, foundation, is_ice_char, local_preference, priority, tcp_preference,
    type_preference,
};
use super::gather::{IPV4_PREFERENCE, IPV6_PREFERENCE, KEEPALIVE};
use super::{Path, Role, Socket, State, Step, to_path_line};
use crate::net::stun::{
    Attribute, BAD_REQUEST, BINDING, Class, Crypto, Integrity, Message, ROLE_CONFLICT, RTO, STREAM_WAIT, TRANSMISSIONS,
    Transport, UNAUTHENTICATED, UNKNOWN_ATTRIBUTE, Writer, new_transaction, resend_at,
};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};
use core::time::Duration;

// Checks are paced at Ta (RFC 8445, 14.2).
const TA: Duration = Duration::from_millis(50);
// Consent lasts 30 s after the last answer, and is asked for every 4 to 6 s (RFC 7675, 5.1).
const CONSENT: Duration = Duration::from_secs(30);
const CONSENT_LEAST_MS: u64 = 4000;
const CONSENT_SPREAD_MS: u64 = 2000;
// How long the controlling side waits after its first valid pair for a better one before it nominates: the stopping criterion is the agent's own (RFC 8445, 8.1.1), and this the time selected pairs may still change in (8.3.1).
const NOMINATION_WAIT: Duration = Duration::from_secs(3);
// A far side fails after nothing was left to check toward it for as long as consent lasts.
const FAIL_WAIT: Duration = CONSENT;
// The longest frame Signal's calling server takes on a stream (`packet_server/epoll.rs`, `MAX_RTP_LENGTH`).
const STREAM_MOST: usize = 1500;
// A user fragment of 4 to 256 characters and a password of 22 to 256 (RFC 8839, 5.4).
const UFRAG_LEAST: usize = 4;
const PWD_LEAST: usize = 22;
const CREDENTIAL_MOST: usize = 256;

fn is_credential(text: &str, least: usize) -> bool {
    (least..=CREDENTIAL_MOST).contains(&text.len()) && text.chars().all(is_ice_char)
}

// IPv6 link-local addresses pair with link-local ones alone (RFC 8445, 6.1.2.2).
fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V6(ip) => ip.is_unicast_link_local(),
        IpAddr::V4(_) => false,
    }
}

impl Agent {
    // RFC 8445, 6.1.2.3: G the controlling side's candidate's priority, D the controlled side's.
    fn pair_priority(&self, local: u32, far: u32) -> u64 {
        let (g, d) = match self.role {
            Role::Controlling => (u64::from(local), u64::from(far)),
            Role::Controlled => (u64::from(far), u64::from(local)),
        };
        (1 << 32) * g.min(d) + 2 * g.max(d) + u64::from(g > d)
    }

    // The priority of a pair's valid pair, by the priority of its local candidate as the check found it.
    fn valid_priority(&self, pair: &Pair) -> Option<u64> {
        Some(self.pair_priority(pair.valid?, self.far(pair.far)?.candidate.priority))
    }

    // True for a far side whose checks can go: its password is known.
    fn can_check(&self, side: u32) -> bool {
        self.side(side).is_some_and(|side| side.pwd.is_some())
    }

    // Takes a far side's user fragment and password, once: no side restarts. The checks that named its fragment before become its own.
    pub(crate) fn far_credentials(&mut self, side: u32, ufrag: &str, pwd: &str) {
        if !is_credential(ufrag, UFRAG_LEAST) || !is_credential(pwd, PWD_LEAST) {
            return;
        }
        let Some(id) = self.side_of(side) else { return };
        let Some(entry) = self.side_mut(id).filter(|entry| entry.ufrag.is_none()) else { return };
        (entry.ufrag, entry.pwd, entry.idle_since) = (Some(ufrag.to_string()), Some(pwd.to_string()), None);
        let named = self.sides.iter().position(|other| other.number.is_none() && other.ufrag.as_deref() == Some(ufrag));
        if let Some(at) = named {
            let named = self.sides.remove(at);
            self.claim(named, id);
        }
        let ufrag = ufrag.to_string();
        self.remove_fars(|far| far.side == id && far.candidate.ufrag.as_ref().is_some_and(|other| *other != ufrag));
    }

    // Makes what a side known by its checks alone holds the side's whose credentials came with that user fragment: its candidates where the side has none at their addresses, their pairs, its queued checks, and the order of the nominations it heard.
    fn claim(&mut self, named: Side, id: u32) {
        let is_known = |far: &Far| {
            self.fars.iter().any(|other| {
                other.side == id
                    && other.candidate.address == far.candidate.address
                    && other.candidate.transport == far.candidate.transport
            })
        };
        let doubles: Vec<u32> =
            self.fars.iter().filter(|far| far.side == named.id && is_known(far)).map(|far| far.id).collect();
        self.remove_fars(|far| doubles.contains(&far.id));
        self.fars.iter_mut().filter(|far| far.side == named.id).for_each(|far| far.side = id);
        self.pairs.iter_mut().filter(|pair| pair.side == named.id).for_each(|pair| pair.side = id);
        let queued: Vec<u32> =
            named.triggered.into_iter().filter(|pair| self.pairs.iter().any(|other| other.id == *pair)).collect();
        let Some(side) = self.side_mut(id) else { return };
        side.nominations = side.nominations.max(named.nominations);
        for pair in queued {
            if !side.triggered.contains(&pair) {
                side.triggered.push_back(pair);
            }
        }
    }

    // Takes a candidate's attribute line from a far side, at any time.
    pub(crate) fn far_candidate(&mut self, side: u32, line: &str) {
        let Some(candidate) = Candidate::parse(line) else { return };
        if candidate.direction.is_some_and(|way| way != Direction::Passive) {
            return;
        }
        let Some(id) = self.side_of(side) else { return };
        let known = self.side(id).and_then(|entry| entry.ufrag.as_ref());
        if candidate.ufrag.as_ref().is_some_and(|ufrag| known.is_some_and(|known| known != ufrag)) {
            return;
        }
        self.add_far(id, candidate, None);
    }

    // Takes an address of a server, Signal's calling server, as a host candidate of a far side: under TLS checked by `name`.
    pub(crate) fn far_server(&mut self, side: u32, address: SocketAddr, transport: Transport, name: Option<&str>) {
        let family = if address.is_ipv6() { IPV6_PREFERENCE } else { IPV4_PREFERENCE };
        let preference = if transport == Transport::Udp { family } else { tcp_preference(false, family) };
        let candidate = Candidate {
            kind: Kind::Host,
            transport,
            address,
            priority: priority(type_preference(Kind::Host, transport), preference),
            foundation: foundation(Kind::Host, address.ip(), None, transport),
            related: None,
            direction: (transport != Transport::Udp).then_some(Direction::Passive),
            ufrag: None,
        };
        let Some(id) = self.side_of(side) else { return };
        self.add_far(id, candidate, name.map(String::from));
    }

    // Lets a candidate of a far side go, by its address.
    pub(crate) fn drop_far(&mut self, side: u32, address: SocketAddr) {
        let Some(id) = self.sides.iter().find(|entry| entry.number == Some(side)).map(|entry| entry.id) else {
            return;
        };
        self.remove_fars(|far| far.side == id && far.candidate.address == address);
    }

    // Lets a far side go with all that is its own: its pairs and their streams closed, its peers no longer wanted at the relays, its checks unanswered, and nothing told of it after.
    pub(crate) fn drop_side(&mut self, side: u32) {
        let Some(at) = self.sides.iter().position(|entry| entry.number == Some(side)) else { return };
        let entry = self.sides.remove(at);
        if let Some(ufrag) = entry.ufrag {
            if self.dropped.len() >= MOST_SIDES {
                self.dropped.pop_front();
            }
            self.dropped.push_back(ufrag);
        }
        self.remove_fars(|far| far.side == entry.id);
    }

    fn add_far(&mut self, side: u32, candidate: Candidate, name: Option<String>) {
        if self.is_closed {
            return;
        }
        let same = |far: &Far| {
            far.side == side
                && far.candidate.address == candidate.address
                && far.candidate.transport == candidate.transport
        };
        if let Some(at) = self.fars.iter().position(same) {
            // A trickled candidate the far side's checks made known first takes its signalled place (RFC 8838, 11.4).
            if self.fars[at].candidate.kind == Kind::Prflx && candidate.kind != Kind::Prflx {
                self.fars[at].candidate = candidate;
                self.reprioritize();
            }
            return;
        }
        if !self.make_far_room(side, candidate.priority) {
            return;
        }
        let id = self.id();
        self.fars.push(Far { id, side, candidate, name });
        let owns: Vec<u32> = self.owns.iter().map(|own| own.id).collect();
        for own in owns {
            self.pair_up(own, id);
        }
        if let Some(entry) = self.side_mut(side) {
            entry.idle_since = None;
        }
    }

    // Room in a full table for a far side's candidate of a priority, so that each side keeps its share: made of the lowest candidate no selected pair holds of a side with the most, or of this side's own below this one where it has as many; false where there is none.
    fn make_far_room(&mut self, side: u32, priority: u32) -> bool {
        if self.fars.len() < MOST_CANDIDATES {
            return true;
        }
        let count = |id: u32| self.fars.iter().filter(|far| far.side == id).count();
        let (own, most) = (count(side), self.fars.iter().map(|far| count(far.side)).max().unwrap_or(0));
        let is_held = |far: &Far| self.pairs.iter().any(|pair| pair.far == far.id && self.is_selected(pair));
        let is_spare = |far: &&Far| {
            let is_share = if own >= most {
                far.side == side && far.candidate.priority < priority
            } else {
                count(far.side) == most
            };
            is_share && !is_held(far)
        };
        let spare = self.fars.iter().filter(is_spare).min_by_key(|far| far.candidate.priority).map(|far| far.id);
        let Some(spare) = spare else { return false };
        self.remove_fars(|far| far.id == spare);
        true
    }

    fn remove_fars(&mut self, is_gone: impl Fn(&Far) -> bool) {
        let ids: Vec<u32> = self.fars.iter().filter(|far| is_gone(far)).map(|far| far.id).collect();
        self.fars.retain(|far| !is_gone(far));
        self.remove_pairs(|pair| ids.contains(&pair.far));
    }

    // Pairs a new candidate of this side with every far side's.
    pub(super) fn pair_own(&mut self, own: u32) {
        let fars: Vec<u32> = self.fars.iter().map(|far| far.id).collect();
        for far in fars {
            self.pair_up(own, far);
        }
        self.clear_idle();
    }

    // Forms a pair where the two candidates pair (RFC 8445, 6.1.2.2; RFC 6544, 6.2), in the state trickle gives it (RFC 8838, 12); none where they do not, or the pairs of all sides fill the table with better ones.
    fn pair_up(&mut self, local: u32, far: u32) -> Option<u32> {
        if let Some(pair) = self.pairs.iter().find(|pair| pair.local == local && pair.far == far) {
            return Some(pair.id);
        }
        let (own, other) = (self.own(local)?, self.far(far)?);
        let (ours, theirs, side) = (&own.candidate, &other.candidate, other.side);
        let is_family = ours.address.is_ipv6() == theirs.address.is_ipv6()
            && is_link_local(ours.address.ip()) == is_link_local(theirs.address.ip());
        let is_transport = match own.base {
            Base::Udp(_) | Base::Relay(_) => theirs.transport == Transport::Udp,
            Base::Tcp(_) => theirs.transport != Transport::Udp && theirs.direction == Some(Direction::Passive),
        };
        let is_kind = matches!(ours.kind, Kind::Host | Kind::Relay) && (!self.is_relayed || ours.kind == Kind::Relay);
        if !is_family || !is_transport || !is_kind {
            return None;
        }
        let priority = self.pair_priority(ours.priority, theirs.priority);
        let foundation = (ours.foundation.clone(), theirs.foundation.clone());
        if self.pairs.len() >= MOST_PAIRS {
            // Room is made so that each side keeps its share (RFC 8445, 6.1.2.5): in a side with the most pairs, or in this side where it has as many, of a failed pair, else of one not yet checked, below this one where it is this side's.
            let count = |id: u32| self.pairs.iter().filter(|pair| pair.side == id).count();
            let (own, most) = (count(side), self.pairs.iter().map(|pair| count(pair.side)).max().unwrap_or(0));
            let is_spare = |pair: &&Pair| {
                let is_share = if own >= most { pair.side == side } else { count(pair.side) == most };
                let is_unchecked = matches!(pair.status, Status::Waiting | Status::Frozen)
                    && (pair.side != side || pair.priority < priority);
                is_share && ((pair.status == Status::Failed && !self.is_selected(pair)) || is_unchecked)
            };
            let spare = self
                .pairs
                .iter()
                .filter(is_spare)
                .min_by_key(|pair| (pair.status != Status::Failed, pair.priority))
                .map(|pair| pair.id)?;
            self.remove_pairs(|pair| pair.id == spare);
        }
        let same = self.pairs.iter().filter(|pair| pair.foundation == foundation);
        let is_waiting = same.clone().any(|pair| pair.status == Status::Succeeded)
            || same.clone().all(|pair| pair.priority < priority);
        let id = self.id();
        self.pairs.push(Pair {
            id,
            side,
            local,
            far,
            priority,
            foundation,
            status: if is_waiting { Status::Waiting } else { Status::Frozen },
            valid: None,
            nominate: false,
            heard: None,
            seen: 0,
            consent: None,
            consent_next: Duration::ZERO,
            is_spent: false,
            stream: None,
        });
        Some(id)
    }

    // Recomputes every pair's priority, after the role changed or a candidate's priority did.
    fn reprioritize(&mut self) {
        for at in 0..self.pairs.len() {
            let pair = &self.pairs[at];
            let local = self.own(pair.local).map(|own| own.candidate.priority);
            let far = self.far(pair.far).map(|far| far.candidate.priority);
            if let (Some(local), Some(far)) = (local, far) {
                self.pairs[at].priority = self.pair_priority(local, far);
            }
        }
    }

    pub(super) fn remove_pairs(&mut self, is_gone: impl Fn(&Pair) -> bool) {
        let ids: Vec<u32> = self.pairs.iter().filter(|pair| is_gone(pair)).map(|pair| pair.id).collect();
        if ids.is_empty() {
            return;
        }
        self.pairs.retain(|pair| !ids.contains(&pair.id));
        self.checks.retain(|check| match check.purpose {
            Purpose::Pair(id) | Purpose::Consent(id) => !ids.contains(&id),
            Purpose::Gather { .. } => true,
        });
        let mut deselected = Vec::new();
        for side in &mut self.sides {
            side.triggered.retain(|id| !ids.contains(id));
            if side.selected.is_some_and(|id| ids.contains(&id)) {
                deselected.push(side.id);
            }
        }
        for side in deselected {
            self.deselect(side);
        }
        let unused: Vec<Socket> = self
            .streams
            .iter()
            .filter(|stream| !self.pairs.iter().any(|pair| pair.stream == Some(stream.socket)))
            .map(|stream| stream.socket)
            .collect();
        self.streams.retain(|stream| !unused.contains(&stream.socket));
        self.steps.extend(unused.into_iter().map(Step::Close));
    }

    fn switch_role(&mut self, role: Role) {
        self.role = role;
        self.reprioritize();
        self.pairs.iter_mut().for_each(|pair| pair.nominate = false);
    }

    // The way a pair's checks go, connecting its stream first where it is TCP (RFC 6544, 7.1); none while that stream is not open.
    fn way_of(&mut self, id: u32) -> Option<Via> {
        let pair = self.pair(id)?;
        let (local, far, stream) = (pair.local, pair.far, pair.stream);
        let Base::Tcp(ip) = self.own(local)?.base else { return self.via_of(local, stream) };
        let far = self.far(far)?;
        let (address, name) = (far.candidate.address, far.name.clone());
        let socket = match stream.or_else(|| {
            self.streams.iter().find(|stream| stream.local == ip && stream.far == address).map(|stream| stream.socket)
        }) {
            Some(socket) => socket,
            None => {
                let socket = self.socket();
                self.streams.push(super::agent::Stream {
                    socket,
                    local: ip,
                    far: address,
                    frames: crate::net::stun::Frames::new(crate::net::stun::Framing::Prefixed),
                    is_open: false,
                });
                self.steps.push_back(Step::Connect { socket, local: ip, far: address, name });
                socket
            }
        };
        if let Some(pair) = self.pair_mut(id) {
            pair.stream = Some(socket);
        }
        self.streams.iter().any(|stream| stream.socket == socket && stream.is_open).then_some(Via::Stream(socket))
    }

    // Sends a check of a pair now under its far side's credentials (RFC 8445, 7.2.4), nominating it where this side nominates it; or a check of consent, sent once and changing no state (RFC 7675, 5.1).
    pub(super) fn check(&mut self, crypto: &impl Crypto, id: u32, is_consent: bool, now: Duration) {
        let Some(pair) = self.pair(id) else { return };
        let (local, far, side, is_spent) = (pair.local, pair.far, pair.side, pair.is_spent);
        let Some((far_ufrag, far_pwd)) =
            self.side(side).and_then(|entry| Some((entry.ufrag.clone()?, entry.pwd.clone()?)))
        else {
            return;
        };
        if is_spent {
            return;
        }
        let is_nominating = self.role == Role::Controlling && pair.nominate && !is_consent;
        // A pair that succeeded stays so while it is checked again (RFC 8445, 6.1.2.6).
        if !is_consent
            && let Some(pair) = self.pair_mut(id)
            && pair.status != Status::Succeeded
        {
            pair.status = Status::Progress;
        }
        let Some(via) = self.way_of(id) else { return };
        let (Some(own), Some(other)) = (self.own(local), self.far(far)) else { return };
        let to = other.candidate.address;
        let ours = &own.candidate;
        let reflexive = priority(type_preference(Kind::Prflx, ours.transport), local_preference(ours.priority));
        let Some(transaction) = new_transaction(crypto) else { return };
        let count = match self.side_mut(side) {
            Some(entry) if is_nominating => {
                entry.nominations += 1;
                Some(entry.nominations)
            }
            _ => None,
        };
        let username = format!("{far_ufrag}:{}", self.ufrag);
        let mut writer = Writer::new(BINDING, Class::Request, &transaction);
        writer.push(Attribute::Username(username.as_bytes())).push(Attribute::Priority(reflexive));
        writer.push(match self.role {
            Role::Controlling => Attribute::Controlling(self.tiebreaker),
            Role::Controlled => Attribute::Controlled(self.tiebreaker),
        });
        if let Some(count) = count {
            writer.push(Attribute::UseCandidate).push(Attribute::Nomination(count));
        }
        writer.seal(crypto, Integrity::Sha1(far_pwd.as_bytes()));
        let bytes = writer.finish(true);
        self.track(Check {
            transaction,
            purpose: if is_consent { Purpose::Consent(id) } else { Purpose::Pair(id) },
            bytes: bytes.clone(),
            via,
            to,
            first: now,
            next: now,
            sent: 1,
            rto: self.rto(),
            priority: reflexive,
            role: self.role,
            is_nominating,
            is_cancelled: false,
        });
        self.emit(crypto, via, to, bytes, now);
    }

    // RFC 8445, 14.3, over the pairs of all far sides.
    fn rto(&self) -> Duration {
        let busy = self.pairs.iter().filter(|pair| matches!(pair.status, Status::Waiting | Status::Progress)).count();
        RTO.max(TA * busy as u32)
    }

    // Keeps a transaction to answer, making room in a full table of the oldest cancelled one or of a check of consent.
    fn track(&mut self, mut check: Check) {
        if self.checks.len() >= MOST_CHECKS {
            let spare =
                self.checks.iter().position(|check| check.is_cancelled || matches!(check.purpose, Purpose::Consent(_)));
            match spare {
                Some(at) => _ = self.checks.remove(at),
                None => return,
            }
        }
        // A transaction over a stream, and a check of consent, are sent once and wait out the stream's time.
        let is_once = matches!(check.via, Via::Stream(_)) || matches!(check.purpose, Purpose::Consent(_));
        check.next = check.first + if is_once { STREAM_WAIT } else { check.rto };
        self.checks.push(check);
    }

    // Answers a request back the way it came: an error where `attributes` begins with one, under this side's password where `is_sealed`.
    fn reply(
        &mut self,
        crypto: &impl Crypto,
        (via, to): (Via, SocketAddr),
        message: &Message<'_>,
        attributes: &[Attribute<'_>],
        is_sealed: bool,
        now: Duration,
    ) {
        let class = if matches!(attributes.first(), Some(Attribute::Error(_))) { Class::Error } else { Class::Success };
        let mut writer = Writer::new(BINDING, class, &message.transaction);
        for attribute in attributes {
            writer.push(*attribute);
        }
        if is_sealed {
            writer.seal(crypto, Integrity::Sha1(self.pwd.as_bytes()));
        }
        let bytes = writer.finish(true);
        self.emit(crypto, via, to, bytes, now);
    }

    // The far side a check is for, by the far user fragment its USERNAME names after this side's, as libwebrtc tells apart the transports that share a port (`p2p_transport_channel.cc`, `OnUnknownAddress`): of the sides that hold it, the one that knows the source; where none holds it, a side known by that fragment alone.
    fn side_named(&mut self, ufrag: &str, from: SocketAddr) -> Option<u32> {
        let holders: Vec<u32> =
            self.sides.iter().filter(|side| side.ufrag.as_deref() == Some(ufrag)).map(|side| side.id).collect();
        let knows = |side: &u32| self.fars.iter().any(|far| far.side == *side && far.candidate.address == from);
        if let Some(side) = holders.iter().find(|side| knows(side)).or(holders.first()) {
            return Some(*side);
        }
        if !is_credential(ufrag, UFRAG_LEAST) {
            return None;
        }
        self.add_named(ufrag)
    }

    // Answers a check of a far side's (RFC 8445, 7.3; RFC 8489, 6.3.1, 9.1.3), learns its peer-reflexive candidate, triggers a check back and takes a nomination.
    pub(super) fn serve(
        &mut self,
        crypto: &impl Crypto,
        via: Via,
        base: u32,
        from: SocketAddr,
        message: &Message<'_>,
        now: Duration,
    ) {
        if message.method != BINDING || !message.has_fingerprint {
            return;
        }
        let back = (via, from);
        let Some(username) = message.username.filter(|_| message.is_sealed()) else {
            return self.reply(crypto, back, message, &[Attribute::Error(BAD_REQUEST)], false, now);
        };
        let ours = self.ufrag.len();
        let is_ours = username.get(..ours) == Some(self.ufrag.as_bytes()) && username.get(ours) == Some(&b':');
        if !is_ours || !message.verify(crypto, Integrity::Sha1(self.pwd.as_bytes())) {
            return self.reply(crypto, back, message, &[Attribute::Error(UNAUTHENTICATED)], false, now);
        }
        let Ok(fragment) = core::str::from_utf8(&username[ours + 1..]) else { return };
        // A side let go gets no answer, as libwebrtc answers no check whose fragment no transport sharing the port knows (`p2p_transport_channel.cc`, `OnUnknownAddress`): its consent ends (RFC 7675, 5.1).
        let is_held = self.sides.iter().any(|side| side.ufrag.as_deref() == Some(fragment));
        if !is_held && self.dropped.iter().any(|dropped| dropped == fragment) {
            return;
        }
        if !message.required.is_empty() {
            let unknown = message.required.clone();
            let attributes = [Attribute::Error(UNKNOWN_ATTRIBUTE), Attribute::Unknown(&unknown)];
            return self.reply(crypto, back, message, &attributes, true, now);
        }
        // RFC 8445, 7.3.1.1.
        match (self.role, message.controlling, message.controlled) {
            (Role::Controlling, Some(theirs), _) if self.tiebreaker >= theirs => {
                return self.reply(crypto, back, message, &[Attribute::Error(ROLE_CONFLICT)], true, now);
            }
            (Role::Controlling, Some(_), _) => self.switch_role(Role::Controlled),
            (Role::Controlled, _, Some(theirs)) if self.tiebreaker >= theirs => self.switch_role(Role::Controlling),
            (Role::Controlled, _, Some(_)) => {
                return self.reply(crypto, back, message, &[Attribute::Error(ROLE_CONFLICT)], true, now);
            }
            _ => {}
        }
        self.reply(crypto, back, message, &[Attribute::XorMapped(from)], true, now);
        let Some(side) = self.side_named(fragment, from) else { return };
        let is_stream = matches!(via, Via::Stream(_));
        let known = self.fars.iter().find(|far| {
            far.side == side
                && far.candidate.address == from
                && (far.candidate.transport != Transport::Udp) == is_stream
        });
        let far = match known {
            Some(far) => far.id,
            // RFC 8445, 7.3.1.3: a request without PRIORITY makes no candidate.
            None => {
                let Some(priority) = message.priority.filter(|priority| !is_stream && PRIORITIES.contains(priority))
                else {
                    return;
                };
                if !self.make_far_room(side, priority) {
                    return;
                }
                let id = self.id();
                let candidate = Candidate {
                    kind: Kind::Prflx,
                    transport: Transport::Udp,
                    address: from,
                    priority,
                    foundation: format!("prflx{id}"),
                    related: None,
                    direction: None,
                    ufrag: Some(fragment.to_string()),
                };
                self.fars.push(Far { id, side, candidate, name: None });
                id
            }
        };
        let Some(id) = self.pair_up(base, far) else { return };
        let Some(status) = self.pair(id).filter(|pair| !pair.is_spent).map(|pair| pair.status) else { return };
        // RFC 8445, 7.3.1.4.
        if status != Status::Succeeded {
            if let Some(pair) = self.pair_mut(id) {
                pair.status = Status::Waiting;
            }
            if status == Status::Progress {
                for check in self.checks.iter_mut().filter(|check| check.purpose == Purpose::Pair(id)) {
                    check.is_cancelled = true;
                    check.next = check.first + STREAM_WAIT;
                }
            }
            if let Some(entry) = self.side_mut(side)
                && !entry.triggered.contains(&id)
            {
                entry.triggered.push_back(id);
            }
        }
        // The far side's nomination, which libwebrtc repeats and renews: the last one wins.
        if self.role == Role::Controlled {
            let Some(pair) = self.pair_mut(id) else { return };
            let is_renewed = message.nomination.is_some_and(|count| count > pair.seen);
            pair.seen = pair.seen.max(message.nomination.unwrap_or(0));
            if message.use_candidate || is_renewed {
                let Some(entry) = self.side_mut(side) else { return };
                entry.nominations += 1;
                let order = entry.nominations;
                let Some(pair) = self.pair_mut(id) else { return };
                pair.heard = Some(order);
                if pair.status == Status::Succeeded && !pair.is_spent {
                    self.select(crypto, side, id, now);
                }
            }
        }
    }

    // Takes an answer to a transaction of this side: a STUN server's, or one to a check, under its far side's password (RFC 8445, 7.2.5).
    pub(super) fn answer(
        &mut self,
        crypto: &impl Crypto,
        via: Via,
        from: SocketAddr,
        message: &Message<'_>,
        now: Duration,
    ) {
        let Some(at) = self.checks.iter().position(|check| check.transaction == message.transaction) else { return };
        let (Purpose::Pair(id) | Purpose::Consent(id)) = self.checks[at].purpose else {
            if let Purpose::Gather { local, server } = self.checks[at].purpose
                && from == server
                && self.checks[at].via == via
            {
                self.checks.remove(at);
                if let (Class::Success, Some(mapped)) = (message.class, message.mapped) {
                    self.reflexive(local, server, mapped);
                }
            }
            return;
        };
        let Some(pair) = self.pair(id).filter(|pair| !pair.is_spent) else { return };
        let (local, side) = (pair.local, pair.side);
        let Some(pwd) = self.side(side).and_then(|entry| entry.pwd.clone()) else { return };
        if !message.has_fingerprint || !message.verify(crypto, Integrity::Sha1(pwd.as_bytes())) {
            return;
        }
        let check = self.checks.remove(at);
        let is_consent = matches!(check.purpose, Purpose::Consent(_));
        // RFC 8445, 7.2.5.2.1.
        if check.via != via || check.to != from {
            if !is_consent {
                self.fail_pair(id);
            }
            return;
        }
        if message.class == Class::Error {
            if message.error == Some(ROLE_CONFLICT) && !is_consent {
                // RFC 8445, 7.2.5.1: the other role than the check was sent under, which a check of the far side's may have brought already, a new tie breaker, and the check again.
                let role = match check.role {
                    Role::Controlling => Role::Controlled,
                    Role::Controlled => Role::Controlling,
                };
                if self.role != role {
                    self.switch_role(role);
                }
                let mut tiebreaker = [0u8; 8];
                if crypto.random(&mut tiebreaker) {
                    self.tiebreaker = u64::from_be_bytes(tiebreaker);
                }
                if let Some(pair) = self.pair_mut(id) {
                    pair.status = Status::Waiting;
                }
                if let Some(entry) = self.side_mut(side)
                    && !entry.triggered.contains(&id)
                {
                    entry.triggered.push_back(id);
                }
            } else if !is_consent {
                self.fail_pair(id);
            }
            return;
        }
        let Some(own) = self.own(local) else { return };
        let (address, base, transport, ip) =
            (own.candidate.address, own.base, own.candidate.transport, own.candidate.address.ip());
        // Over a stream Signal's calling server tells no mapped address (`candidate_selector.rs`).
        let mapped = match (message.mapped, via) {
            (Some(mapped), _) => mapped,
            (None, Via::Stream(_)) => address,
            (None, _) => return self.fail_pair(id),
        };
        let found = self
            .owns
            .iter()
            .find(|own| own.candidate.address == mapped && own.base == base)
            .map(|own| own.candidate.priority);
        let valid = match found {
            Some(priority) => priority,
            // RFC 8445, 7.2.5.3.1: a peer-reflexive candidate of this side, told to nobody.
            None => {
                let candidate = Candidate {
                    kind: Kind::Prflx,
                    transport,
                    address: mapped,
                    priority: check.priority,
                    foundation: foundation(Kind::Prflx, ip, None, transport),
                    related: None,
                    direction: None,
                    ufrag: None,
                };
                if self.owns.len() < MOST_CANDIDATES {
                    let id = self.id();
                    self.owns.push(super::agent::Own { id, candidate, base, is_told: false });
                }
                check.priority
            }
        };
        let Some(pair) = self.pair_mut(id) else { return };
        (pair.status, pair.valid, pair.consent) = (Status::Succeeded, Some(valid), Some(now));
        if check.is_nominating {
            pair.nominate = false;
        }
        let (foundation, heard) = (pair.foundation.clone(), pair.heard);
        let Some(entry) = self.side_mut(side) else { return };
        entry.first_valid.get_or_insert(now);
        let is_latest = heard.is_some() && heard == Some(entry.nominations);
        // RFC 8445, 7.2.5.3.3.
        for pair in self.pairs.iter_mut().filter(|pair| pair.foundation == foundation && pair.status == Status::Frozen)
        {
            pair.status = Status::Waiting;
        }
        if check.is_nominating || (self.role == Role::Controlled && is_latest) {
            self.select(crypto, side, id, now);
        }
    }

    fn fail_pair(&mut self, id: u32) {
        if let Some(pair) = self.pair_mut(id) {
            (pair.status, pair.nominate) = (Status::Failed, false);
        }
    }

    // The interval to the next check of consent: 0.8 to 1.2 of five seconds (RFC 7675, 5.1).
    fn consent_interval(crypto: &impl Crypto) -> Duration {
        let mut bytes = [0u8; 8];
        let draw = if crypto.random(&mut bytes) {
            u64::from_le_bytes(bytes) % (CONSENT_SPREAD_MS + 1)
        } else {
            CONSENT_SPREAD_MS / 2
        };
        Duration::from_millis(CONSENT_LEAST_MS + draw)
    }

    fn path_of(&self, pair: &Pair) -> Option<Path> {
        let (own, far) = (self.own(pair.local)?, self.far(pair.far)?);
        let transport = match own.base {
            Base::Relay(id) => self.relays.iter().find(|relay| relay.id == id)?.client.transport(),
            Base::Udp(_) => Transport::Udp,
            Base::Tcp(_) => far.candidate.transport,
        };
        Some(Path { local: own.candidate.kind, far: far.candidate.kind, transport })
    }

    // Makes a valid pair the one media goes over to its far side.
    fn select(&mut self, crypto: &impl Crypto, side: u32, id: u32, now: Duration) {
        let Some(entry) = self.side_mut(side) else { return };
        if entry.selected != Some(id) {
            (entry.selected, entry.selected_at) = (Some(id), now);
            let number = entry.number;
            let interval = Self::consent_interval(crypto);
            let Some(pair) = self.pair_mut(id) else { return };
            // A pair whose consent is old asks for it at once; media waits for it (RFC 7675, 5.1).
            let is_fresh = pair.consent.is_some_and(|consent| now < consent + interval);
            pair.consent_next = if is_fresh { now + interval } else { now };
            let path = self.pair(id).and_then(|pair| self.path_of(pair));
            if let Some(number) = number {
                self.steps.push_back(Step::Selected { side: number, path });
                self.log(to_path_line(number, path));
            }
        }
        self.set_state(side, State::Connected);
    }

    fn deselect(&mut self, side: u32) {
        let Some(entry) = self.side_mut(side) else { return };
        if entry.selected.take().is_some() {
            let (number, state) = (entry.number, entry.state);
            if let Some(number) = number {
                self.steps.push_back(Step::Selected { side: number, path: None });
                self.log(to_path_line(number, None));
            }
            if state == State::Connected {
                self.set_state(side, State::Disconnected);
            }
        }
    }

    // A stream of pairs ended: their transactions are failed, and a selected pair's stream is opened anew by a triggered check (RFC 6544, 11.1).
    pub(super) fn stream_closed(&mut self, socket: Socket) {
        self.checks.retain(|check| check.via != Via::Stream(socket));
        let selected: Vec<u32> = self.sides.iter().filter_map(|side| side.selected).collect();
        let (mut queued, mut failed) = (Vec::new(), Vec::new());
        for pair in self.pairs.iter_mut().filter(|pair| pair.stream == Some(socket)) {
            pair.stream = None;
            if selected.contains(&pair.id) {
                pair.status = Status::Waiting;
                queued.push((pair.side, pair.id));
            } else if matches!(pair.status, Status::Progress | Status::Succeeded) {
                failed.push(pair.id);
            }
        }
        failed.into_iter().for_each(|id| self.fail_pair(id));
        for (side, id) in queued {
            if let Some(entry) = self.side_mut(side)
                && !entry.triggered.contains(&id)
            {
                entry.triggered.push_back(id);
            }
        }
    }

    /// Sends a datagram of media to a far side over its selected pair while it has consent; nothing goes otherwise.
    pub fn send(&mut self, crypto: &impl Crypto, side: u32, packet: Vec<u8>, now: Duration) {
        let selected = self.sides.iter().find(|entry| entry.number == Some(side)).and_then(|entry| entry.selected);
        let Some(pair) = selected.and_then(|id| self.pair(id)) else { return };
        if self.is_closed || pair.consent.is_none_or(|consent| now >= consent + CONSENT) {
            return;
        }
        let (local, stream, far) = (pair.local, pair.stream, pair.far);
        let (Some(via), Some(far)) = (self.via_of(local, stream), self.far(far)) else { return };
        if matches!(via, Via::Stream(_)) && packet.len() > STREAM_MOST {
            return;
        }
        let to = far.candidate.address;
        self.emit(crypto, via, to, packet, now);
    }

    // True while a far side may still check the reflexive bindings: before any side is known, or while one has no pair selected (RFC 8445, 11).
    fn is_unsettled(&self) -> bool {
        let mut numbered = self.sides.iter().filter(|side| side.number.is_some()).peekable();
        numbered.peek().is_none() || numbered.any(|side| side.selected.is_none())
    }

    // Does what is due at `now`: the relays' upkeep, retransmissions and timeouts, consent, keepalives of reflexive bindings, nomination, the next paced transaction, and failure.
    pub(super) fn tick(&mut self, crypto: &impl Crypto, now: Duration) {
        self.keep_relays(crypto, now);
        self.retransmit(crypto, now);
        self.keep_consent(crypto, now);
        if self.is_unsettled() {
            let servers = self.stun_servers();
            for local in self.locals.iter_mut().filter(|local| local.refresh.is_some_and(|due| now >= due)) {
                local.refresh = Some(now + KEEPALIVE);
                for server in servers.iter().filter(|server| server.is_ipv6() == local.ip.is_ipv6()) {
                    if !self.gathers.contains(&(local.ip, *server)) {
                        self.gathers.push_back((local.ip, *server));
                    }
                }
            }
        }
        self.nominate(now);
        if now >= self.paced && self.pace(crypto, now) {
            self.paced = now + TA;
        }
        self.judge(now);
    }

    // The relays' timers, the permissions the pairs of all far sides want (RFC 8445, 7.2.1), and a channel for each pair a side selected through it (RFC 8445, 12.1).
    fn keep_relays(&mut self, crypto: &impl Crypto, now: Duration) {
        let selected: Vec<(u32, u32)> = self
            .sides
            .iter()
            .filter_map(|side| side.selected)
            .filter_map(|id| self.pair(id))
            .map(|pair| (pair.local, pair.far))
            .collect();
        for at in 0..self.relays.len() {
            let Some(relay) = self.relays.get(at) else { break };
            if !relay.is_open {
                continue;
            }
            let id = relay.id;
            let candidate = relay.candidate;
            let mut wanted: Vec<IpAddr> = Vec::new();
            for pair in self
                .pairs
                .iter()
                .filter(|pair| Some(pair.local) == candidate && pair.status != Status::Failed && !pair.is_spent)
            {
                if let Some(far) = self.far(pair.far)
                    && !wanted.contains(&far.candidate.address.ip())
                {
                    wanted.push(far.candidate.address.ip());
                }
            }
            let channels: Vec<SocketAddr> = selected
                .iter()
                .filter(|(local, _)| Some(*local) == candidate)
                .filter_map(|(_, far)| self.far(*far))
                .map(|far| far.candidate.address)
                .collect();
            let mut events = self.relays[at].client.tick(crypto, now);
            let relay = &mut self.relays[at];
            for ip in &wanted {
                if !relay.permitted.contains(ip) {
                    relay.permitted.push(*ip);
                    events.extend(relay.client.permit(crypto, *ip, now));
                }
            }
            for ip in relay.permitted.iter().filter(|ip| !wanted.contains(ip)) {
                relay.client.forget(*ip);
            }
            relay.permitted.retain(|ip| wanted.contains(ip));
            for peer in relay.bound.iter().filter(|peer| !channels.contains(peer)) {
                relay.client.unbind(*peer);
            }
            relay.bound.retain(|peer| channels.contains(peer));
            for peer in channels {
                if !relay.bound.contains(&peer) {
                    relay.bound.push(peer);
                    events.extend(relay.client.bind(crypto, peer, now));
                }
            }
            self.relayed(crypto, at, events, now);
            if self.relays.get(at).is_none_or(|relay| relay.id != id) {
                break;
            }
        }
    }

    fn retransmit(&mut self, crypto: &impl Crypto, now: Duration) {
        let mut at = 0;
        while at < self.checks.len() {
            let check = &mut self.checks[at];
            let is_resent = !check.is_cancelled
                && !matches!(check.via, Via::Stream(_))
                && !matches!(check.purpose, Purpose::Consent(_))
                && check.sent < TRANSMISSIONS;
            if now < check.next {
                at += 1;
            } else if is_resent {
                check.sent += 1;
                check.next = check.first + resend_at(check.rto, check.sent);
                let (via, to, bytes) = (check.via, check.to, check.bytes.clone());
                self.emit(crypto, via, to, bytes, now);
                at += 1;
            } else {
                let check = self.checks.remove(at);
                // A nomination that times out leaves its pair failed (RFC 8445, 7.2.5.3.4).
                if let (Purpose::Pair(id), false) = (check.purpose, check.is_cancelled)
                    && (check.is_nominating || self.pair(id).is_some_and(|pair| pair.status == Status::Progress))
                {
                    self.fail_pair(id);
                }
            }
        }
    }

    // RFC 7675, 5.1: a check of consent on each far side's selected pair every 4 to 6 s, sent once, and the pair let go 30 s after its last answer.
    fn keep_consent(&mut self, crypto: &impl Crypto, now: Duration) {
        let selected: Vec<(u32, u32, Duration)> =
            self.sides.iter().filter_map(|side| Some((side.id, side.selected?, side.selected_at))).collect();
        for (side, id, selected_at) in selected {
            let Some(pair) = self.pair(id) else { continue };
            // Consent is counted from the last answer, or from the selection of a pair it was old for.
            let since = pair.consent.map_or(selected_at, |consent| consent.max(selected_at));
            if now >= since + CONSENT {
                if let Some(pair) = self.pair_mut(id) {
                    (pair.is_spent, pair.status, pair.nominate) = (true, Status::Failed, false);
                }
                self.checks.retain(
                    |check| !matches!(check.purpose, Purpose::Pair(pair) | Purpose::Consent(pair) if pair == id),
                );
                if let Some(entry) = self.side_mut(side) {
                    entry.triggered.retain(|pair| *pair != id);
                }
                self.deselect(side);
                continue;
            }
            if now < pair.consent_next {
                continue;
            }
            let interval = Self::consent_interval(crypto);
            if let Some(pair) = self.pair_mut(id) {
                pair.consent_next = now + interval;
            }
            self.check(crypto, id, true, now);
        }
    }

    // RFC 8445, 8.1.1: toward each far side the controlling side nominates its best valid pair once no better one may still succeed, or a while after its first; and a better one that succeeds later.
    fn nominate(&mut self, now: Duration) {
        if self.role != Role::Controlling {
            return;
        }
        let sides: Vec<(u32, Option<u32>, Option<Duration>)> = self
            .sides
            .iter()
            .filter(|side| side.pwd.is_some())
            .map(|side| (side.id, side.selected, side.first_valid))
            .collect();
        for (side, selected, first_valid) in sides {
            let own = |pair: &&Pair| pair.side == side;
            if self.pairs.iter().filter(own).any(|pair| pair.nominate) {
                continue;
            }
            let valid = self.pairs.iter().filter(own).filter(|pair| pair.status == Status::Succeeded && !pair.is_spent);
            let Some((best, priority)) = valid
                .filter_map(|pair| Some((pair.id, self.valid_priority(pair)?)))
                .max_by_key(|(_, priority)| *priority)
            else {
                continue;
            };
            match selected.and_then(|id| self.pair(id)) {
                Some(selected)
                    if selected.id == best || self.valid_priority(selected).is_some_and(|other| other >= priority) =>
                {
                    continue;
                }
                Some(_) => {}
                None => {
                    let may_better = self.pairs.iter().filter(own).any(|pair| {
                        matches!(pair.status, Status::Waiting | Status::Frozen | Status::Progress)
                            && !pair.is_spent
                            && pair.priority > priority
                    });
                    if may_better && first_valid.is_some_and(|first| now < first + NOMINATION_WAIT) {
                        continue;
                    }
                }
            }
            if let Some(pair) = self.pair_mut(best) {
                pair.nominate = true;
            }
            if let Some(entry) = self.side_mut(side) {
                entry.triggered.retain(|id| *id != best);
                entry.triggered.push_front(best);
            }
        }
    }

    // True for a queued pair still to be checked: a pair that succeeded since it was queued is checked again only to be nominated.
    fn is_triggerable(&self, id: u32) -> bool {
        self.pair(id).is_some_and(|pair| !pair.is_spent && (pair.status != Status::Succeeded || pair.nominate))
    }

    // True for a frozen pair no pair of its foundation that can be checked stands before: it may be unfrozen (RFC 8445, 6.1.4.2).
    fn is_unfrozen(&self, pair: &Pair) -> bool {
        !self.pairs.iter().any(|other| {
            other.foundation == pair.foundation
                && matches!(other.status, Status::Waiting | Status::Progress)
                && self.can_check(other.side)
        })
    }

    // True where `pace` has a transaction to send: what it picks from, as it picks.
    fn has_paced_work(&self) -> bool {
        let is_queued = self
            .sides
            .iter()
            .any(|side| side.pwd.is_some() && side.triggered.iter().any(|id| self.is_triggerable(*id)));
        let is_checkable = self.pairs.iter().any(|pair| {
            self.can_check(pair.side)
                && !pair.is_spent
                && (pair.status == Status::Waiting || pair.status == Status::Frozen && self.is_unfrozen(pair))
        });
        !self.gathers.is_empty() || is_queued || is_checkable
    }

    // One paced transaction for all far sides: a triggered check, else a request to a STUN server, else an ordinary check, the sides taken in turn (RFC 8445, 6.1.4.2); false where there was none.
    fn pace(&mut self, crypto: &impl Crypto, now: Duration) -> bool {
        let count = self.sides.len();
        let order: Vec<(usize, u32)> = (0..count)
            .map(|step| (self.turn + step) % count)
            .filter(|at| self.sides[*at].pwd.is_some())
            .map(|at| (at, self.sides[at].id))
            .collect();
        for &(at, side) in &order {
            while let Some(id) = self.side_mut(side).and_then(|entry| entry.triggered.pop_front()) {
                if self.is_triggerable(id) {
                    self.turn = at + 1;
                    self.check(crypto, id, false, now);
                    return true;
                }
            }
        }
        if let Some((local, server)) = self.gathers.pop_front() {
            return self.gather(crypto, local, server, now);
        }
        for &(at, side) in &order {
            let own = |pair: &&Pair| pair.side == side && !pair.is_spent;
            if !self.pairs.iter().filter(own).any(|pair| pair.status == Status::Waiting) {
                let mut foundations: Vec<(String, String)> = Vec::new();
                for pair in self.pairs.iter().filter(own) {
                    if pair.status == Status::Frozen
                        && self.is_unfrozen(pair)
                        && !foundations.contains(&pair.foundation)
                    {
                        foundations.push(pair.foundation.clone());
                    }
                }
                for foundation in foundations {
                    let frozen = self.pairs.iter_mut().filter(|pair| {
                        pair.side == side && pair.foundation == foundation && pair.status == Status::Frozen
                    });
                    if let Some(pair) = frozen.max_by_key(|pair| pair.priority) {
                        pair.status = Status::Waiting;
                    }
                }
            }
            let waiting = self.pairs.iter().filter(own).filter(|pair| pair.status == Status::Waiting);
            if let Some(id) = waiting.max_by_key(|pair| pair.priority).map(|pair| pair.id) {
                self.turn = at + 1;
                self.check(crypto, id, false, now);
                return true;
            }
        }
        false
    }

    // A Binding request to a STUN server from a host candidate (RFC 8445, 5.1.1.2), unauthenticated.
    fn gather(&mut self, crypto: &impl Crypto, local: IpAddr, server: SocketAddr, now: Duration) -> bool {
        let Some(socket) = self.locals.iter().find(|candidate| candidate.ip == local).and_then(|local| local.socket)
        else {
            return false;
        };
        let Some(transaction) = new_transaction(crypto) else { return false };
        let bytes = Writer::new(BINDING, Class::Request, &transaction).finish(false);
        let candidates = self.gathers.len() + 1;
        self.track(Check {
            transaction,
            purpose: Purpose::Gather { local, server },
            bytes: bytes.clone(),
            via: Via::Udp(socket),
            to: server,
            first: now,
            next: now,
            sent: 1,
            // RFC 8445, 14.3, while gathering.
            rto: RTO.max(TA * candidates as u32),
            priority: 0,
            role: self.role,
            is_nominating: false,
            is_cancelled: false,
        });
        self.emit(crypto, Via::Udp(socket), server, bytes, now);
        true
    }

    // Fails a far side once nothing was left to check toward it for `FAIL_WAIT`, its credentials and a pair of its known.
    fn judge(&mut self, now: Duration) {
        let is_gathering = !self.gathers.is_empty() || self.relays.iter().any(|relay| relay.candidate.is_none());
        let ids: Vec<u32> = self.sides.iter().map(|side| side.id).collect();
        for side in ids {
            let Some(entry) = self.side(side) else { continue };
            let own = |pair: &&Pair| pair.side == side;
            let is_judged = matches!(entry.state, State::Connecting | State::Disconnected)
                && entry.pwd.is_some()
                && self.pairs.iter().any(|pair| own(&pair));
            let is_busy = is_gathering
                || entry.triggered.iter().any(|id| self.is_triggerable(*id))
                || self.pairs.iter().filter(own).any(|pair| {
                    !pair.is_spent && matches!(pair.status, Status::Frozen | Status::Waiting | Status::Progress)
                });
            let Some(entry) = self.side_mut(side) else { continue };
            if !is_judged || is_busy {
                entry.idle_since = None;
            } else if now >= *entry.idle_since.get_or_insert(now) + FAIL_WAIT {
                self.set_state(side, State::Failed);
            }
        }
    }

    // How long from `now` until something is due; none while nothing is.
    pub(crate) fn next_wait(&self, now: Duration) -> Option<Duration> {
        if !self.steps.is_empty() {
            return Some(Duration::ZERO);
        }
        if self.is_closed {
            return None;
        }
        let pacing = self.has_paced_work().then_some(self.paced);
        let checks = self.checks.iter().map(|check| check.next);
        let relays = self
            .relays
            .iter()
            .filter(|relay| relay.is_open)
            .filter_map(|relay| relay.client.next_wait(now).map(|wait| now + wait));
        let consent = self.sides.iter().filter_map(|side| {
            let pair = self.pair(side.selected?)?;
            let since = pair.consent.map_or(side.selected_at, |consent| consent.max(side.selected_at));
            Some((since + CONSENT).min(pair.consent_next))
        });
        let is_unsettled = self.is_unsettled();
        let refresh = self.locals.iter().filter(|_| is_unsettled).filter_map(|local| local.refresh);
        let nomination = self
            .sides
            .iter()
            .filter(|side| self.role == Role::Controlling && side.selected.is_none())
            .filter_map(|side| side.first_valid.map(|first| first + NOMINATION_WAIT))
            .filter(|due| *due > now);
        let failure = self.sides.iter().filter_map(|side| side.idle_since.map(|idle| idle + FAIL_WAIT));
        pacing
            .into_iter()
            .chain(checks)
            .chain(relays)
            .chain(consent)
            .chain(refresh)
            .chain(nomination)
            .chain(failure)
            .min()
            .map(|due| due.saturating_sub(now))
    }
}

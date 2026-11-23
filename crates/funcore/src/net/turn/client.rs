use super::Server;
use super::auth::{Auth, is_downgraded};
use crate::base::bytes::be16;
use crate::net::stun::{
    ALLOCATE, ALLOCATION_MISMATCH, Attribute, BAD_REQUEST, CHANNEL_BIND, CHANNEL_HEAD, CREATE_PERMISSION, Class,
    Crypto, DATA_METHOD, DONT_FRAGMENT, FAMILY_UNSUPPORTED, INSUFFICIENT_CAPACITY, IPV4, Message, REFRESH, RTO, SEND,
    SERVER_ERROR, STALE_NONCE, STREAM_WAIT, TRANSMISSIONS, TRY_ALTERNATE, Transaction, Transport, UDP, UNAUTHENTICATED,
    UNKNOWN_ATTRIBUTE, WORD, Writer, family_of, new_transaction, resend_at,
};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};
use core::time::Duration;

// An allocation's lifetime where the server tells none (RFC 8656, 6).
const LIFETIME: Duration = Duration::from_secs(600);
// What is refreshed is refreshed a minute before it runs out (RFC 8656, 8).
const AHEAD: Duration = Duration::from_secs(60);
const PERMISSION_LIFE: Duration = Duration::from_secs(300);
const CHANNEL_LIFE: Duration = Duration::from_secs(600);
// A channel's number and its peer are bound to no other until this long after it ran out (RFC 8656, 12).
const QUARANTINE: Duration = Duration::from_secs(300);
const FIRST_CHANNEL: u16 = 0x4000;
const LAST_CHANNEL: u16 = 0x4FFF;
// A request asked again after a server's error or a stale nonce, at the most (RFC 8489, 6.3.4).
const MOST_RETRIES: u8 = 4;
// The codes of a server's own errors, after which the request is asked again (RFC 8489, 6.3.4); 508, a server without room, is not asked again and ends the allocation as refused (RFC 8656, 7.4).
const SERVER_ERRORS: core::ops::Range<u16> = SERVER_ERROR..600;
// Client transport addresses tried, and servers a redirect leads to, at the most (RFC 8656, 7.4).
const MOST_TRIES: u8 = 3;
// As many permissions as ICE has pairs (RFC 8445, 6.1.2.5).
const MOST_PERMISSIONS: usize = 100;

// What the client has its owner do, or tells it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Event {
    // Bytes for the server: a datagram, or written to the stream as they are.
    Send(Vec<u8>),
    // The allocation stands: the relayed address, and the address the server saw this side at.
    Allocated { relayed: SocketAddr, mapped: Option<SocketAddr> },
    // What a peer sent through the relay.
    Data(SocketAddr, Vec<u8>),
    // The server sent this side to another (`Client::address`); the client is started there anew, over a new connection on a stream.
    Moved,
    // The server holds an allocation for this side's transport address; the client is started anew from another (RFC 8656, 7.4).
    Rebind,
    // An answer passed over as if never received, by its error code (none for a success) and why; the request waits on.
    Discarded { code: Option<u16>, reason: Discard },
    Failed(Failure),
}

// Why an answer is passed over (RFC 8489, 9.2.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Discard {
    // No integrity: on a 400, or where an answer is checked for one.
    Unsealed,
    // An integrity that does not hold under the credential.
    Unverified,
    // A nonce that asks for password algorithms the answer does not list.
    Downgraded,
}

// Why an allocation is none.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Failure {
    // No answer in time.
    Timeout,
    // The server answered with this error code.
    Refused(u16),
    // The credential ran out before the allocation was asked for.
    Expired,
    // A refresh found no allocation, its time ran out, or its connection closed.
    Lost,
    // The integrity of an answer over a stream did not hold, or of none of the answers over UDP (RFC 8489, 9.2.5).
    Unverified,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ask {
    Allocate,
    // A refresh; true for the one that ends the allocation.
    Refresh(bool),
    Permission(IpAddr),
    Bind(u16, SocketAddr),
}

struct Request {
    transaction: Transaction,
    ask: Ask,
    bytes: Vec<u8>,
    is_signed: bool,
    first: Duration,
    next: Duration,
    sent: u32,
    retries: u8,
    // An answer to it was discarded, as its integrity did not hold.
    is_forged: bool,
    // Why the last answer to it was passed over, so that one repeated is told once.
    discarded: Option<Discard>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Asking,
    Allocated { relayed: SocketAddr, expires: Duration },
    Ended,
}

struct Permission {
    ip: IpAddr,
    until: Option<Duration>,
    is_wanted: bool,
    is_asked: bool,
    is_refused: bool,
    // The latest datagram that waits for the permission.
    held: Option<(SocketAddr, Vec<u8>)>,
}

struct Channel {
    number: u16,
    peer: SocketAddr,
    bound: Option<Duration>,
    is_wanted: bool,
    is_asked: bool,
    is_refused: bool,
    // When its binding ran out, for its quarantine.
    lapsed: Option<Duration>,
}

impl Channel {
    fn is_bound(&self, now: Duration) -> bool {
        self.bound.is_some_and(|bound| now < bound + CHANNEL_LIFE)
    }
}

// An allocation on one relay over one transport address of this side.
pub(crate) struct Client {
    server: Server,
    address: SocketAddr,
    name: Option<String>,
    family: u8,
    auth: Option<Auth>,
    takes_fragment: bool,
    state: State,
    requests: Vec<Request>,
    permissions: Vec<Permission>,
    channels: Vec<Channel>,
    tried: Vec<SocketAddr>,
    rebinds: u8,
}

impl Client {
    // A client of a relay that asks for a relayed address of the family `family` (RFC 8656, 18.6).
    pub(crate) fn new(server: Server, family: u8) -> Client {
        Client {
            address: server.address,
            name: server.name.clone(),
            server,
            family,
            auth: None,
            takes_fragment: true,
            state: State::Idle,
            requests: Vec::new(),
            permissions: Vec::new(),
            channels: Vec::new(),
            tried: Vec::new(),
            rebinds: 0,
        }
    }

    // Where the client's messages go, which a redirect moves.
    pub(crate) const fn address(&self) -> SocketAddr {
        self.address
    }

    // The name the server's certificate is checked by under TLS.
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) const fn transport(&self) -> Transport {
        self.server.transport
    }

    // The relayed address while the allocation stands.
    #[cfg(test)]
    pub(super) const fn relayed(&self) -> Option<SocketAddr> {
        match self.state {
            State::Allocated { relayed, .. } => Some(relayed),
            _ => None,
        }
    }

    const fn is_stream(&self) -> bool {
        !matches!(self.server.transport, Transport::Udp)
    }

    fn method_of(ask: Ask) -> u16 {
        match ask {
            Ask::Allocate => ALLOCATE,
            Ask::Refresh(_) => REFRESH,
            Ask::Permission(_) => CREATE_PERMISSION,
            Ask::Bind(..) => CHANNEL_BIND,
        }
    }

    fn ask(&mut self, crypto: &impl Crypto, ask: Ask, retries: u8, now: Duration, events: &mut Vec<Event>) {
        let Some(transaction) = new_transaction(crypto) else {
            return self.fail(Failure::Timeout, events);
        };
        let mut writer = Writer::new(Self::method_of(ask), Class::Request, &transaction);
        match ask {
            Ask::Allocate => {
                writer.push(Attribute::RequestedTransport(UDP));
                // Without it a server allocates IPv4 (RFC 8656, 7.2), as libwebrtc asks, and one without RFC 6156, as Cloudflare's, refuses it.
                if self.family != IPV4 {
                    writer.push(Attribute::RequestedFamily(self.family));
                }
                if self.takes_fragment {
                    writer.push(Attribute::DontFragment);
                }
            }
            Ask::Refresh(true) => _ = writer.push(Attribute::Lifetime(0)),
            Ask::Refresh(false) => {}
            Ask::Permission(ip) => _ = writer.push(Attribute::Peer(SocketAddr::new(ip, 0))),
            Ask::Bind(number, peer) => _ = writer.push(Attribute::Channel(number)).push(Attribute::Peer(peer)),
        }
        if let Some(auth) = &self.auth {
            auth.sign(&self.server, &mut writer);
            writer.seal(crypto, auth.integrity());
        }
        let bytes = writer.finish(true);
        events.push(Event::Send(bytes.clone()));
        let next = now + if self.is_stream() { STREAM_WAIT } else { RTO };
        let is_signed = self.auth.is_some();
        self.requests.push(Request {
            transaction,
            ask,
            bytes,
            is_signed,
            first: now,
            next,
            sent: 1,
            retries,
            is_forged: false,
            discarded: None,
        });
    }

    fn fail(&mut self, failure: Failure, events: &mut Vec<Event>) {
        self.state = State::Ended;
        self.requests.clear();
        events.push(Event::Failed(failure));
    }

    // Asks the relay for the allocation, or again after `Moved` or `Rebind`.
    pub(crate) fn start(&mut self, crypto: &impl Crypto, now: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        if now >= self.server.expires {
            self.fail(Failure::Expired, &mut events);
        } else if matches!(self.state, State::Idle) {
            self.state = State::Asking;
            self.ask(crypto, Ask::Allocate, 0, now, &mut events);
        }
        events
    }

    // Takes a message from the relay: a STUN message, or ChannelData.
    pub(crate) fn take(&mut self, crypto: &impl Crypto, bytes: &[u8], now: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(number) = be16(bytes, 0).filter(|number| (FIRST_CHANNEL..=LAST_CHANNEL).contains(number)) {
            let Some(length) = be16(bytes, 2) else { return events };
            // Data may come on a channel as soon as its binding was asked for (RFC 8656, 12).
            let peer = self
                .channels
                .iter()
                .find(|channel| channel.number == number && (channel.is_bound(now) || channel.is_asked));
            if let (Some(channel), Some(data)) = (peer, bytes.get(CHANNEL_HEAD..CHANNEL_HEAD + usize::from(length))) {
                events.push(Event::Data(channel.peer, data.to_vec()));
            }
            return events;
        }
        let Some(message) = Message::parse(bytes) else { return events };
        match message.class {
            Class::Indication if message.method == DATA_METHOD => {
                if let (Some(peer), Some(data)) = (message.peers.first(), message.data)
                    && self.is_permitted(peer.ip(), now)
                {
                    events.push(Event::Data(*peer, data.to_vec()));
                }
            }
            Class::Success | Class::Error => self.answer(crypto, &message, now, &mut events),
            _ => {}
        }
        events
    }

    fn is_permitted(&self, ip: IpAddr, now: Duration) -> bool {
        self.permissions
            .iter()
            .any(|permission| permission.ip == ip && permission.until.is_some_and(|until| now < until))
    }

    fn answer(&mut self, crypto: &impl Crypto, message: &Message<'_>, now: Duration, events: &mut Vec<Event>) {
        let Some(index) = self.requests.iter().position(|request| request.transaction == message.transaction) else {
            return;
        };
        let mut request = self.requests.remove(index);
        if Self::method_of(request.ask) != message.method {
            return self.requests.push(request);
        }
        let error = message.error.filter(|_| message.class == Class::Error);
        if matches!(error, Some(UNAUTHENTICATED | STALE_NONCE)) {
            return self.challenged(crypto, message, request, now, events);
        }
        // RFC 8489, 9.2.5: an answer whose nonce asks for algorithms it does not list is as if never received, and so is a 400 without integrity to any request; the client passes such a 400 over only where its request is without the credential, as libwebrtc takes one to a signed request as its refusal, as every other error (`StunRequestManager::CheckResponse`).
        let reason = if is_downgraded(message) {
            Some(Discard::Downgraded)
        } else {
            let is_unsealed = error == Some(BAD_REQUEST) && !message.is_sealed() && !request.is_signed;
            is_unsealed.then_some(Discard::Unsealed)
        };
        if let Some(reason) = reason {
            return self.discard(request, error, reason, events);
        }
        // As libwebrtc, which Signal's relays serve, the integrity of a success and of a redirect alone is checked: they send other errors without one (`StunRequestManager::CheckResponse`).
        let is_checked = error.is_none_or(|code| code == TRY_ALTERNATE);
        let holds = match &self.auth {
            Some(auth) if request.is_signed && is_checked => message.verify(crypto, auth.integrity()),
            _ => true,
        };
        if !holds {
            if self.is_stream() {
                return self.fail(Failure::Unverified, events);
            }
            request.is_forged = true;
            let reason = if message.is_sealed() { Discard::Unverified } else { Discard::Unsealed };
            return self.discard(request, error, reason, events);
        }
        if !message.required.is_empty() {
            return self.settle(request, Some(BAD_REQUEST), message, now, crypto, events);
        }
        self.settle(request, error, message, now, crypto, events);
    }

    // Passes an answer over: the request waits on, and why is told where it differs from the last answer's passed over.
    fn discard(&mut self, mut request: Request, code: Option<u16>, reason: Discard, events: &mut Vec<Event>) {
        if request.discarded.replace(reason) != Some(reason) {
            events.push(Event::Discarded { code, reason });
        }
        self.requests.push(request);
    }

    // A 401 or a 438: the credential is answered or renewed, and the request asked again (RFC 8489, 9.2.5).
    fn challenged(
        &mut self,
        crypto: &impl Crypto,
        message: &Message<'_>,
        request: Request,
        now: Duration,
        events: &mut Vec<Event>,
    ) {
        let code = message.error.unwrap_or(UNAUTHENTICATED);
        let renewed = match &mut self.auth {
            Some(auth) if code == STALE_NONCE => !is_downgraded(message) && auth.renew(message),
            Some(auth) if request.is_signed && auth.is_of(message) => false,
            _ => match Auth::new(crypto, &self.server, message) {
                Some(auth) => {
                    self.auth = Some(auth);
                    true
                }
                None => false,
            },
        };
        // The first challenge, to a request without the credential, is the mechanism's own step and no retry.
        if renewed && request.retries < MOST_RETRIES {
            self.ask(crypto, request.ask, request.retries + u8::from(request.is_signed), now, events);
        } else {
            self.refused(request.ask, code, events);
        }
    }

    // What an error that ends a request means for what it asked.
    fn refused(&mut self, ask: Ask, code: u16, events: &mut Vec<Event>) {
        match ask {
            Ask::Allocate => self.fail(Failure::Refused(code), events),
            Ask::Refresh(false) if code == ALLOCATION_MISMATCH => self.fail(Failure::Lost, events),
            Ask::Refresh(false) => self.fail(Failure::Refused(code), events),
            Ask::Refresh(true) => {}
            Ask::Permission(ip) => {
                if let Some(permission) = self.permissions.iter_mut().find(|permission| permission.ip == ip) {
                    (permission.is_asked, permission.is_refused, permission.held) = (false, true, None);
                }
            }
            // A binding refused is not asked again: the peer is reached by Send indications (RFC 8656, 12.3).
            Ask::Bind(number, _) => {
                if let Some(channel) = self.channels.iter_mut().find(|channel| channel.number == number) {
                    (channel.is_asked, channel.is_refused) = (false, true);
                }
            }
        }
    }

    fn settle(
        &mut self,
        request: Request,
        error: Option<u16>,
        message: &Message<'_>,
        now: Duration,
        crypto: &impl Crypto,
        events: &mut Vec<Event>,
    ) {
        if let Some(code) = error {
            let is_server_error = SERVER_ERRORS.contains(&code) && code != INSUFFICIENT_CAPACITY;
            if is_server_error && request.retries < MOST_RETRIES {
                return self.ask(crypto, request.ask, request.retries + 1, now, events);
            }
            if request.ask == Ask::Allocate {
                return self.allocate_refused(crypto, code, message, request.is_signed, now, events);
            }
            return self.refused(request.ask, code, events);
        }
        match request.ask {
            Ask::Allocate => self.allocated(crypto, message, now, events),
            Ask::Refresh(false) => {
                if let State::Allocated { expires, .. } = &mut self.state {
                    *expires =
                        now + message.lifetime.map_or(LIFETIME, |seconds| Duration::from_secs(u64::from(seconds)));
                }
            }
            Ask::Refresh(true) => {}
            Ask::Permission(ip) => self.permitted(crypto, ip, now, events),
            Ask::Bind(number, peer) => {
                if let Some(channel) = self.channels.iter_mut().find(|channel| channel.number == number) {
                    (channel.bound, channel.is_asked, channel.lapsed) = (Some(now), false, None);
                }
                self.permitted(crypto, peer.ip(), now, events);
            }
        }
    }

    // The place of an address's permission in the table, made where there is none and room for it.
    fn permission_at(&mut self, ip: IpAddr) -> Option<usize> {
        match self.permissions.iter().position(|permission| permission.ip == ip) {
            Some(at) => Some(at),
            None if self.permissions.len() < MOST_PERMISSIONS => {
                self.permissions.push(Permission {
                    ip,
                    until: None,
                    is_wanted: false,
                    is_asked: false,
                    is_refused: false,
                    held: None,
                });
                Some(self.permissions.len() - 1)
            }
            None => None,
        }
    }

    fn permitted(&mut self, crypto: &impl Crypto, ip: IpAddr, now: Duration, events: &mut Vec<Event>) {
        let Some(at) = self.permission_at(ip) else { return };
        let permission = &mut self.permissions[at];
        (permission.until, permission.is_asked) = (Some(now + PERMISSION_LIFE), false);
        if let Some((peer, data)) = permission.held.take() {
            events.extend(self.send(crypto, &peer, &data, now));
        }
    }

    fn allocated(&mut self, crypto: &impl Crypto, message: &Message<'_>, now: Duration, events: &mut Vec<Event>) {
        let Some(relayed) = message.relayed.filter(|relayed| family_of(relayed.ip()) == self.family) else {
            // An address of a family this side did not ask for is given back (RFC 8656, 7.3).
            self.ask(crypto, Ask::Refresh(true), 0, now, events);
            return self.fail(Failure::Refused(FAMILY_UNSUPPORTED), events);
        };
        let lifetime = message.lifetime.map_or(LIFETIME, |seconds| Duration::from_secs(u64::from(seconds)));
        self.state = State::Allocated { relayed, expires: now + lifetime };
        events.push(Event::Allocated { relayed, mapped: message.mapped });
        events.extend(self.tick(crypto, now));
    }

    fn allocate_refused(
        &mut self,
        crypto: &impl Crypto,
        code: u16,
        message: &Message<'_>,
        is_signed: bool,
        now: Duration,
        events: &mut Vec<Event>,
    ) {
        match code {
            // A redirect is followed only where its integrity held (RFC 8489, 14.8), to a server not tried yet (10).
            TRY_ALTERNATE if is_signed => match message.alternate {
                Some(alternate) if !self.tried.contains(&alternate) && self.tried.len() < usize::from(MOST_TRIES) => {
                    self.tried.push(self.address);
                    self.address = alternate;
                    if let Some(domain) = message.domain.and_then(|domain| core::str::from_utf8(domain).ok()) {
                        self.name = Some(domain.to_string());
                    }
                    (self.auth, self.state) = (None, State::Idle);
                    events.push(Event::Moved);
                }
                _ => self.fail(Failure::Refused(code), events),
            },
            UNKNOWN_ATTRIBUTE if self.takes_fragment && message.unknown.contains(&DONT_FRAGMENT) => {
                self.takes_fragment = false;
                self.ask(crypto, Ask::Allocate, 0, now, events);
            }
            ALLOCATION_MISMATCH if self.rebinds < MOST_TRIES => {
                self.rebinds += 1;
                self.state = State::Idle;
                events.push(Event::Rebind);
            }
            _ => self.fail(Failure::Refused(code), events),
        }
    }

    // Has a permission for a peer's address asked for and kept while wanted.
    pub(crate) fn permit(&mut self, crypto: &impl Crypto, ip: IpAddr, now: Duration) -> Vec<Event> {
        if let Some(at) = self.permission_at(ip) {
            self.permissions[at].is_wanted = true;
        }
        self.tick(crypto, now)
    }

    // Lets a permission and the channels of that address run out.
    pub(crate) fn forget(&mut self, ip: IpAddr) {
        self.permissions.iter_mut().filter(|permission| permission.ip == ip).for_each(|permission| {
            (permission.is_wanted, permission.held) = (false, None);
        });
        self.channels
            .iter_mut()
            .filter(|channel| channel.peer.ip() == ip)
            .for_each(|channel| channel.is_wanted = false);
    }

    // Has a channel bound to a peer and kept while wanted.
    pub(crate) fn bind(&mut self, crypto: &impl Crypto, peer: SocketAddr, now: Duration) -> Vec<Event> {
        match self.channels.iter_mut().find(|channel| channel.peer == peer) {
            Some(channel) => channel.is_wanted = true,
            None => {
                let number = (FIRST_CHANNEL..=LAST_CHANNEL)
                    .find(|number| self.channels.iter().all(|channel| channel.number != *number));
                if let Some(number) = number {
                    self.channels.push(Channel {
                        number,
                        peer,
                        bound: None,
                        is_wanted: true,
                        is_asked: false,
                        is_refused: false,
                        lapsed: None,
                    });
                }
            }
        }
        self.permit(crypto, peer.ip(), now)
    }

    // Lets the channel of a peer run out.
    pub(crate) fn unbind(&mut self, peer: SocketAddr) {
        self.channels.iter_mut().filter(|channel| channel.peer == peer).for_each(|channel| channel.is_wanted = false);
    }

    // Sends a datagram to a peer: on its channel where one is bound, else in a Send indication, which waits for its permission where none stands yet.
    pub(crate) fn send(&mut self, crypto: &impl Crypto, peer: &SocketAddr, data: &[u8], now: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        if !matches!(self.state, State::Allocated { .. }) {
            return events;
        }
        if let Some(channel) = self.channels.iter().find(|channel| channel.peer == *peer && channel.is_bound(now))
            && let Ok(length) = u16::try_from(data.len())
        {
            let mut bytes = Vec::with_capacity(CHANNEL_HEAD + data.len() + WORD);
            bytes.extend(channel.number.to_be_bytes());
            bytes.extend(length.to_be_bytes());
            bytes.extend(data);
            if self.is_stream() {
                bytes.resize(bytes.len().next_multiple_of(WORD), 0);
            }
            events.push(Event::Send(bytes));
            return events;
        }
        if self.is_permitted(peer.ip(), now) {
            let Some(transaction) = new_transaction(crypto) else { return events };
            let mut writer = Writer::new(SEND, Class::Indication, &transaction);
            writer.push(Attribute::Peer(*peer)).push(Attribute::Data(data));
            if self.takes_fragment {
                writer.push(Attribute::DontFragment);
            }
            events.push(Event::Send(writer.finish(false)));
        } else if let Some(permission) = self.permissions.iter_mut().find(|permission| permission.ip == peer.ip())
            && permission.is_wanted
            && !permission.is_refused
        {
            permission.held = Some((*peer, data.to_vec()));
        }
        events
    }

    // Does what is due: retransmissions, timeouts, refreshes, and permissions and channels kept or let go.
    pub(crate) fn tick(&mut self, crypto: &impl Crypto, now: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        let is_stream = self.is_stream();
        let mut index = 0;
        while index < self.requests.len() {
            let request = &mut self.requests[index];
            if now < request.next {
                index += 1;
            } else if is_stream || request.sent >= TRANSMISSIONS {
                let request = self.requests.remove(index);
                self.timed_out(request.ask, request.is_forged, &mut events);
                if self.state == State::Ended {
                    return events;
                }
            } else {
                events.push(Event::Send(request.bytes.clone()));
                request.sent += 1;
                request.next = request.first + resend_at(RTO, request.sent);
                index += 1;
            }
        }
        let State::Allocated { expires, .. } = self.state else { return events };
        if now >= expires {
            self.fail(Failure::Lost, &mut events);
            return events;
        }
        let is_refreshing = self.requests.iter().any(|request| request.ask == Ask::Refresh(false));
        if now + AHEAD >= expires && !is_refreshing {
            self.ask(crypto, Ask::Refresh(false), 0, now, &mut events);
        }
        self.keep_permissions(crypto, now, &mut events);
        self.keep_channels(crypto, now, &mut events);
        events
    }

    // A request that ended unanswered: where every answer was discarded for its integrity, that is told instead of the silence (RFC 8489, 9.2.5).
    fn timed_out(&mut self, ask: Ask, is_forged: bool, events: &mut Vec<Event>) {
        let failure = if is_forged { Failure::Unverified } else { Failure::Timeout };
        match ask {
            Ask::Allocate | Ask::Refresh(false) => self.fail(failure, events),
            Ask::Refresh(true) => {}
            Ask::Permission(ip) => {
                if let Some(permission) = self.permissions.iter_mut().find(|permission| permission.ip == ip) {
                    (permission.is_asked, permission.held) = (false, None);
                }
            }
            Ask::Bind(number, _) => {
                if let Some(channel) = self.channels.iter_mut().find(|channel| channel.number == number) {
                    channel.is_asked = false;
                }
            }
        }
    }

    fn keep_permissions(&mut self, crypto: &impl Crypto, now: Duration, events: &mut Vec<Event>) {
        let mut asks = Vec::new();
        for permission in &mut self.permissions {
            if permission.until.is_some_and(|until| now >= until) {
                permission.until = None;
            }
            let is_due = permission.until.is_none_or(|until| now + AHEAD >= until);
            if permission.is_wanted && !permission.is_asked && !permission.is_refused && is_due {
                permission.is_asked = true;
                asks.push(permission.ip);
            }
        }
        self.permissions.retain(|permission| permission.is_wanted || permission.until.is_some() || permission.is_asked);
        for ip in asks {
            self.ask(crypto, Ask::Permission(ip), 0, now, events);
        }
    }

    fn keep_channels(&mut self, crypto: &impl Crypto, now: Duration, events: &mut Vec<Event>) {
        let mut asks = Vec::new();
        for channel in &mut self.channels {
            if let Some(bound) = channel.bound
                && now >= bound + CHANNEL_LIFE
            {
                (channel.bound, channel.lapsed) = (None, Some(bound + CHANNEL_LIFE));
            }
            // A binding is refreshed as its permission is, which lasts the shorter.
            let is_due = channel.bound.is_none_or(|bound| now + AHEAD >= bound + PERMISSION_LIFE);
            if channel.is_wanted && !channel.is_asked && !channel.is_refused && is_due {
                channel.is_asked = true;
                asks.push((channel.number, channel.peer));
            }
        }
        self.channels.retain(|channel| {
            channel.is_wanted
                || channel.is_asked
                || channel.bound.is_some()
                || channel.lapsed.is_some_and(|lapsed| now < lapsed + QUARANTINE)
        });
        for (number, peer) in asks {
            self.ask(crypto, Ask::Bind(number, peer), 0, now, events);
        }
    }

    // How long from `now` until something is due; none while nothing is.
    pub(crate) fn next_wait(&self, now: Duration) -> Option<Duration> {
        let State::Allocated { expires, .. } = self.state else {
            return self.requests.iter().map(|request| request.next).min().map(|due| due.saturating_sub(now));
        };
        let refresh = (!self.requests.iter().any(|request| request.ask == Ask::Refresh(false)))
            .then(|| expires.saturating_sub(AHEAD));
        let permissions = self.permissions.iter().filter_map(|permission| match permission.until {
            Some(until) if permission.is_wanted && !permission.is_asked && !permission.is_refused => {
                Some(until.saturating_sub(AHEAD))
            }
            Some(until) => Some(until),
            None => None,
        });
        let channels = self.channels.iter().filter_map(|channel| match channel.bound {
            Some(bound) if channel.is_wanted && !channel.is_asked && !channel.is_refused => {
                Some((bound + PERMISSION_LIFE).saturating_sub(AHEAD))
            }
            Some(bound) => Some(bound + CHANNEL_LIFE),
            None => channel.lapsed.map(|lapsed| lapsed + QUARANTINE),
        });
        self.requests
            .iter()
            .map(|request| request.next)
            .chain(refresh)
            .chain(Some(expires))
            .chain(permissions)
            .chain(channels)
            .min()
            .map(|due| due.saturating_sub(now))
    }

    // Ends the allocation at the relay: a refresh of lifetime nought, sent once (RFC 8656, 8.1).
    pub(crate) fn close(&mut self, crypto: &impl Crypto, now: Duration) -> Vec<Event> {
        let mut events = Vec::new();
        if matches!(self.state, State::Allocated { .. }) {
            self.ask(crypto, Ask::Refresh(true), 0, now, &mut events);
        }
        (self.state, self.requests) = (State::Ended, Vec::new());
        events
    }
}

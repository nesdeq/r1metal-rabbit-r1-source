use super::Crypto;
use super::noise::{self, COOKIE_BYTES, Handshake, Keys, Kind};
use crate::crypto::KEY_BYTES;
use crate::net::to_whole_ipv4;
use crate::net::window::Window;
use alloc::vec::Vec;
use core::time::Duration;

// The whitepaper's times and counts (6.1).
const REKEY_AFTER_TIME: Duration = Duration::from_secs(120);
const REJECT_AFTER_TIME: Duration = Duration::from_secs(180);
const REKEY_ATTEMPT_TIME: Duration = Duration::from_secs(90);
pub(crate) const REKEY_TIMEOUT: Duration = Duration::from_secs(5);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);
const REKEY_AFTER_MESSAGES: u64 = 1 << 60;
const REJECT_AFTER_MESSAGES: u64 = u64::MAX - (1 << 13);
// As long as the peer takes a cookie it gave (5.4.7).
const COOKIE_LIFE: Duration = Duration::from_secs(120);
const STAMP_STEP: Duration = Duration::from_nanos(1);

/// What a peer has the tunnel's owner do, in this order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    // A datagram for the peer.
    Send(Vec<u8>),
    // A packet that came out of the tunnel.
    Deliver(Vec<u8>),
    // The peer answered no handshake for as long as one is tried: its address is looked up anew.
    Lost,
}

struct Pending {
    handshake: Handshake,
    sent: Duration,
    since: Duration,
}

struct Session {
    sending: [u8; KEY_BYTES],
    receiving: [u8; KEY_BYTES],
    index: u32,
    peer_index: u32,
    counter: u64,
    window: Window,
    born: Duration,
    has_asked_anew: bool,
}

impl Session {
    fn age(&self, now: Duration) -> Duration {
        now.saturating_sub(self.born)
    }

    fn is_alive(&self, now: Duration) -> bool {
        self.age(now) < REJECT_AFTER_TIME
    }

    fn can_send(&self, now: Duration) -> bool {
        self.is_alive(now) && self.counter < REJECT_AFTER_MESSAGES
    }

    fn is_aged(&self, now: Duration) -> bool {
        self.age(now) >= REKEY_AFTER_TIME || self.counter >= REKEY_AFTER_MESSAGES
    }
}

/// The tunnel's one peer: the handshake under way, the sessions and their timers.
pub struct Peer {
    keys: Keys,
    keepalive: Option<Duration>,
    most: usize,
    pending: Option<Pending>,
    cookie: Option<([u8; COOKIE_BYTES], Duration)>,
    current: Option<Session>,
    previous: Option<Session>,
    stamp: Duration,
    sent: Option<Duration>,
    // A packet went and nothing has come since.
    unanswered: Option<Duration>,
    // A packet came and nothing has gone since.
    unconfirmed: Option<Duration>,
}

impl Peer {
    /// A peer by its keys, kept alive every `keepalive` where the config asks it, its packets of `most` bytes at the most.
    pub fn new(keys: Keys, keepalive: Option<Duration>, most: usize) -> Peer {
        Peer {
            keys,
            keepalive,
            most,
            pending: None,
            cookie: None,
            current: None,
            previous: None,
            stamp: Duration::ZERO,
            sent: None,
            unanswered: None,
            unconfirmed: None,
        }
    }

    /// True while a session stands that packets go out under.
    pub fn is_up(&self, now: Duration) -> bool {
        self.current.as_ref().is_some_and(|session| session.can_send(now))
    }

    // Begins a handshake at `now`, the clock at `utc`, unless one was begun less than its timeout ago.
    fn initiate(&mut self, crypto: &impl Crypto, now: Duration, utc: Duration, steps: &mut Vec<Step>) {
        if self.pending.as_ref().is_some_and(|pending| now.saturating_sub(pending.sent) < REKEY_TIMEOUT) {
            return;
        }
        let since = self.pending.as_ref().map_or(now, |pending| pending.since);
        let mut index = [0u8; 4];
        if !crypto.random(&mut index) {
            return;
        }
        // The peer takes a stamp only past the last it took from these keys.
        self.stamp = utc.max(self.stamp + STAMP_STEP);
        let cookie = self.cookie.filter(|(_, at)| now.saturating_sub(*at) < COOKIE_LIFE).map(|(cookie, _)| cookie);
        let stamp = noise::to_stamp(self.stamp);
        let begun = noise::initiate(crypto, &self.keys, u32::from_le_bytes(index), &stamp, cookie.as_ref());
        if let Some((handshake, message)) = begun {
            self.pending = Some(Pending { handshake, sent: now, since });
            self.sent = Some(now);
            steps.push(Step::Send(message));
        }
    }

    // Sends a packet under the session that stands, an empty one to keep it alive, and asks for a new session behind either once this one is aged (6.2); false where none stands.
    fn send_under(
        &mut self,
        crypto: &impl Crypto,
        packet: &[u8],
        now: Duration,
        utc: Duration,
        steps: &mut Vec<Step>,
    ) -> bool {
        let Some(session) = self.current.as_mut().filter(|session| session.can_send(now)) else {
            return false;
        };
        let message =
            noise::seal_data(crypto, &session.sending, session.peer_index, session.counter, packet, self.most);
        if let Some(message) = message {
            session.counter += 1;
            (self.sent, self.unconfirmed) = (Some(now), None);
            if !packet.is_empty() {
                self.unanswered.get_or_insert(now);
            }
            steps.push(Step::Send(message));
        }
        if session.is_aged(now) {
            self.initiate(crypto, now, utc, steps);
        }
        true
    }

    /// Has a session made where none stands and no handshake is under way.
    pub fn begin(&mut self, crypto: &impl Crypto, now: Duration, utc: Duration) -> Vec<Step> {
        let mut steps = Vec::new();
        if !self.is_up(now) {
            self.initiate(crypto, now, utc, &mut steps);
        }
        steps
    }

    /// Takes a packet for the tunnel: it goes under the session that stands, or is dropped and a session made.
    pub fn send(&mut self, crypto: &impl Crypto, packet: &[u8], now: Duration, utc: Duration) -> Vec<Step> {
        let mut steps = Vec::new();
        if !self.send_under(crypto, packet, now, utc, &mut steps) {
            self.initiate(crypto, now, utc, &mut steps);
        }
        steps
    }

    /// Takes a datagram from the peer.
    pub fn take(&mut self, crypto: &impl Crypto, message: &[u8], now: Duration, utc: Duration) -> Vec<Step> {
        let mut steps = Vec::new();
        match noise::kind_of(message) {
            Some(Kind::Response) => self.take_response(crypto, message, now, utc, &mut steps),
            Some(Kind::CookieReply) => {
                let pending = self.pending.as_ref();
                let cookie = pending.and_then(|pending| pending.handshake.read_cookie(crypto, &self.keys, message));
                self.cookie = cookie.map(|cookie| (cookie, now)).or(self.cookie);
            }
            Some(Kind::Data) => self.take_data(crypto, message, now, utc, &mut steps),
            // This side alone begins handshakes.
            Some(Kind::Initiation) | None => {}
        }
        steps
    }

    fn take_response(
        &mut self,
        crypto: &impl Crypto,
        message: &[u8],
        now: Duration,
        utc: Duration,
        steps: &mut Vec<Step>,
    ) {
        let Some(pending) = &self.pending else {
            return;
        };
        let Some(agreed) = pending.handshake.complete(crypto, &self.keys, message) else {
            return;
        };
        let session = Session {
            sending: agreed.sending,
            receiving: agreed.receiving,
            index: pending.handshake.index,
            peer_index: agreed.peer_index,
            counter: 0,
            window: Window::default(),
            born: now,
            has_asked_anew: false,
        };
        self.previous = self.current.replace(session);
        (self.pending, self.unanswered) = (None, None);
        // The peer sends under the session only once it has heard this side under it.
        self.send_under(crypto, &[], now, utc, steps);
    }

    fn take_data(&mut self, crypto: &impl Crypto, message: &[u8], now: Duration, utc: Duration, steps: &mut Vec<Step>) {
        let Some((index, counter)) = noise::read_data_head(message) else {
            return;
        };
        let (is_current, session) = match (&mut self.current, &mut self.previous) {
            (Some(session), _) if session.index == index => (true, session),
            (_, Some(session)) if session.index == index => (false, session),
            _ => return,
        };
        if !session.is_alive(now) || counter >= REJECT_AFTER_MESSAGES {
            return;
        }
        let Some(mut packet) = noise::open_data(crypto, &session.receiving, counter, message) else {
            return;
        };
        if !session.window.take(counter) {
            return;
        }
        let is_late = now.saturating_sub(session.born) >= REJECT_AFTER_TIME - KEEPALIVE_TIMEOUT - REKEY_TIMEOUT;
        let asks_anew = is_current && is_late && !core::mem::replace(&mut session.has_asked_anew, true);
        self.unanswered = None;
        if !packet.is_empty() {
            self.unconfirmed.get_or_insert(now);
            if let Some(length) = to_whole_ipv4(&packet).map(<[u8]>::len) {
                packet.truncate(length);
                steps.push(Step::Deliver(packet));
            }
        }
        if asks_anew {
            self.initiate(crypto, now, utc, steps);
        }
    }

    /// Does what is due at `now`.
    pub fn tick(&mut self, crypto: &impl Crypto, now: Duration, utc: Duration) -> Vec<Step> {
        let mut steps = Vec::new();
        self.current.take_if(|session| !session.is_alive(now));
        self.previous.take_if(|session| !session.is_alive(now));
        let is_due = |at: Option<Duration>, after: Duration| at.is_some_and(|at| now.saturating_sub(at) >= after);
        if let Some(pending) = &self.pending
            && now.saturating_sub(pending.sent) >= REKEY_TIMEOUT
        {
            if now.saturating_sub(pending.since) >= REKEY_ATTEMPT_TIME {
                self.pending = None;
                steps.push(Step::Lost);
            } else {
                self.initiate(crypto, now, utc, &mut steps);
            }
        }
        if is_due(self.unanswered, KEEPALIVE_TIMEOUT + REKEY_TIMEOUT) {
            self.unanswered = None;
            self.initiate(crypto, now, utc, &mut steps);
        }
        if is_due(self.unconfirmed, KEEPALIVE_TIMEOUT) {
            self.unconfirmed = None;
            self.send_under(crypto, &[], now, utc, &mut steps);
        }
        if let Some(every) = self.keepalive
            && self.pending.is_none()
            && self.sent.is_none_or(|sent| now.saturating_sub(sent) >= every)
        {
            // Counted as sent whatever comes of it, so that a library that gives nothing is asked again in its time.
            self.sent = Some(now);
            if !self.send_under(crypto, &[], now, utc, &mut steps) {
                self.initiate(crypto, now, utc, &mut steps);
            }
        }
        steps
    }

    // How long from `now` until something is due; none while nothing is.
    pub(crate) fn next_wait(&self, now: Duration) -> Option<Duration> {
        let after = |at: Option<Duration>, wait: Duration| at.map(|at| at + wait);
        let keepalive = self.keepalive.filter(|_| self.pending.is_none());
        [
            after(self.pending.as_ref().map(|pending| pending.sent), REKEY_TIMEOUT),
            after(self.unanswered, KEEPALIVE_TIMEOUT + REKEY_TIMEOUT),
            after(self.unconfirmed, KEEPALIVE_TIMEOUT),
            keepalive.map(|every| self.sent.map_or(now, |sent| sent + every)),
            after(self.current.as_ref().map(|session| session.born), REJECT_AFTER_TIME),
        ]
        .into_iter()
        .flatten()
        .min()
        .map(|due| due.saturating_sub(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::wireguard::noise::tests::{Responder, pair};
    use crate::net::wireguard::tests::Toy;
    use alloc::vec;

    const UTC: Duration = Duration::from_secs(1_800_000_000);
    const MOST: usize = 1384;

    fn second(count: u64) -> Duration {
        Duration::from_secs(count)
    }

    fn packet(length: u16) -> Vec<u8> {
        let mut packet = vec![0x45, 0];
        packet.extend(length.to_be_bytes());
        packet.resize(usize::from(length), 7);
        packet
    }

    fn sent(steps: &[Step]) -> Vec<u8> {
        match steps {
            [Step::Send(message)] => message.clone(),
            other => panic!("not one datagram: {other:?}"),
        }
    }

    // A peer with its session, made at `now`, and the other side's keys for it.
    fn joined(crypto: &Toy, keepalive: Option<Duration>, now: Duration) -> (Peer, Responder, noise::Agreed) {
        let (keys, mut responder) = pair(crypto);
        let mut peer = Peer::new(keys, keepalive, MOST);
        let first = sent(&peer.begin(crypto, now, UTC + now));
        let (answer, theirs) = responder.answer(crypto, &first).unwrap();
        let alive = sent(&peer.take(crypto, &answer, now, UTC + now));
        assert_eq!(
            noise::open_data(crypto, &theirs.receiving, 0, &alive),
            Some(Vec::new()),
            "the session's first word"
        );
        (peer, responder, theirs)
    }

    #[test]
    fn handshake_then_packets_both_ways() {
        let crypto = Toy::default();
        let (mut peer, _, theirs) = joined(&crypto, None, second(1));
        assert!(peer.is_up(second(1)));
        assert_eq!(peer.next_wait(second(1)), Some(second(180)), "nothing is due but the session's end");
        let out = sent(&peer.send(&crypto, &packet(40), second(2), UTC));
        assert_eq!(
            noise::read_data_head(&out),
            Some((0x0B0B_0B0C, 1)),
            "under the peer's number, the count after the first word's"
        );
        let opened = noise::open_data(&crypto, &theirs.receiving, 1, &out).unwrap();
        assert_eq!((opened.len(), &opened[..40]), (48, &packet(40)[..]), "filled up to whole blocks of sixteen");
        let back = noise::seal_data(&crypto, &theirs.sending, theirs.peer_index, 0, &packet(61), MOST).unwrap();
        assert_eq!(peer.take(&crypto, &back, second(2), UTC), [Step::Deliver(packet(61))], "the filling cut off");
        assert_eq!(peer.take(&crypto, &back, second(2), UTC), [], "a datagram that comes again is dropped");
        let alive = noise::seal_data(&crypto, &theirs.sending, theirs.peer_index, 1, &[], MOST).unwrap();
        assert_eq!(
            peer.take(&crypto, &alive, second(2), UTC),
            [],
            "a word that keeps the session alive carries nothing"
        );
        let other = noise::seal_data(&crypto, &theirs.sending, theirs.peer_index ^ 1, 2, &packet(40), MOST).unwrap();
        assert_eq!(peer.take(&crypto, &other, second(2), UTC), [], "a session this side has none of");
        let no_packet = noise::seal_data(&crypto, &theirs.sending, theirs.peer_index, 3, &[0x60; 40], MOST).unwrap();
        assert_eq!(peer.take(&crypto, &no_packet, second(2), UTC), [], "what is no packet of IPv4 is not delivered");
    }

    #[test]
    fn no_session_drops_and_begins() {
        let crypto = Toy::default();
        let (keys, mut responder) = pair(&crypto);
        let mut peer = Peer::new(keys, None, MOST);
        assert!(!peer.is_up(second(0)) && peer.next_wait(second(0)).is_none());
        let first = sent(&peer.send(&crypto, &packet(40), second(0), UTC));
        assert_eq!(first.len(), 148, "the packet is dropped and a handshake begun");
        assert_eq!(peer.send(&crypto, &packet(40), second(1), UTC), [], "one handshake at a time");
        assert_eq!(peer.next_wait(second(1)), Some(second(4)));
        assert_eq!(peer.tick(&crypto, second(4), UTC), []);
        let again = sent(&peer.tick(&crypto, second(5), UTC));
        assert!(again.len() == 148 && again != first, "begun anew after five seconds, with a number of its own");
        let (stale, _) = responder.answer(&crypto, &first).unwrap();
        assert_eq!(peer.take(&crypto, &stale, second(6), UTC), [], "the answer to the handshake given up is none");
        let (answer, _) = responder.answer(&crypto, &again).expect("each stamp past the one before");
        assert_eq!(peer.take(&crypto, &answer, second(6), UTC).len(), 1);
        assert!(peer.is_up(second(6)));
    }

    #[test]
    fn attempts_end_lost() {
        let crypto = Toy::default();
        let (keys, _) = pair(&crypto);
        let mut peer = Peer::new(keys, None, MOST);
        peer.begin(&crypto, second(0), UTC);
        let tries: usize = (1..18).map(|count| peer.tick(&crypto, second(5 * count), UTC).len()).sum();
        assert_eq!(tries, 17, "every five seconds for ninety");
        assert_eq!(peer.tick(&crypto, second(90), UTC), [Step::Lost]);
        assert_eq!(
            (peer.next_wait(second(90)), peer.tick(&crypto, second(95), UTC)),
            (None, vec![]),
            "then nothing by itself"
        );
        assert_eq!(sent(&peer.begin(&crypto, second(96), UTC)).len(), 148, "until it is asked again");
    }

    #[test]
    fn session_ages() {
        let crypto = Toy::default();
        let (mut peer, mut responder, theirs) = joined(&crypto, None, second(0));
        assert_eq!(sent(&peer.send(&crypto, &packet(40), second(119), UTC)).len(), 80);
        let steps = peer.send(&crypto, &packet(40), second(120), UTC);
        assert!(
            matches!(&steps[..], [Step::Send(data), Step::Send(begun)] if data.len() == 80 && begun.len() == 148),
            "past two minutes the packet goes, and a new session is asked for"
        );
        let Step::Send(begun) = &steps[1] else { unreachable!() };
        let (answer, new) = responder.answer(&crypto, begun).unwrap();
        peer.take(&crypto, &answer, second(121), UTC + second(121));
        let late = noise::seal_data(&crypto, &theirs.sending, theirs.peer_index, 0, &packet(40), MOST).unwrap();
        assert_eq!(
            peer.take(&crypto, &late, second(121), UTC).len(),
            1,
            "what the session before still brings is taken"
        );
        let out = sent(&peer.send(&crypto, &packet(40), second(122), UTC));
        assert!(noise::open_data(&crypto, &new.receiving, 1, &out).is_some(), "what goes, goes under the new one");
        assert_eq!(peer.take(&crypto, &late, second(180), UTC), [], "a session of three minutes is none");
        assert!(
            peer.is_up(second(300)) && !peer.is_up(second(301)),
            "nor is the new one, three minutes after it was made"
        );
    }

    #[test]
    fn late_packet_asks_anew_once() {
        let crypto = Toy::default();
        let (mut peer, _, theirs) = joined(&crypto, None, second(0));
        let data = |counter: u64| {
            noise::seal_data(&crypto, &theirs.sending, theirs.peer_index, counter, &packet(40), MOST).unwrap()
        };
        assert_eq!(peer.take(&crypto, &data(0), second(164), UTC).len(), 1);
        let steps = peer.take(&crypto, &data(1), second(165), UTC);
        assert!(matches!(&steps[..], [Step::Deliver(_), Step::Send(begun)] if begun.len() == 148));
        assert_eq!(peer.take(&crypto, &data(2), second(171), UTC).len(), 1, "asked for once");
    }

    #[test]
    fn silence_and_keepalives() {
        let crypto = Toy::default();
        let (mut peer, ..) = joined(&crypto, None, second(0));
        peer.send(&crypto, &packet(40), second(1), UTC);
        assert_eq!(peer.next_wait(second(1)), Some(second(15)));
        assert_eq!(peer.tick(&crypto, second(15), UTC), []);
        assert_eq!(
            sent(&peer.tick(&crypto, second(16), UTC)).len(),
            148,
            "a packet unanswered for fifteen seconds: a new handshake"
        );
        let (mut peer, _, theirs) = joined(&crypto, None, second(0));
        let data = noise::seal_data(&crypto, &theirs.sending, theirs.peer_index, 0, &packet(40), MOST).unwrap();
        peer.take(&crypto, &data, second(3), UTC);
        assert_eq!(peer.next_wait(second(3)), Some(second(10)));
        let alive = sent(&peer.tick(&crypto, second(13), UTC));
        assert_eq!(alive.len(), 32, "a packet taken and nothing sent for ten seconds: a word that carries nothing");
        assert_eq!(peer.tick(&crypto, second(30), UTC), [], "once");
        let (mut kept, ..) = joined(&crypto, Some(second(25)), second(0));
        assert_eq!(kept.next_wait(second(1)), Some(second(24)));
        assert_eq!(sent(&kept.tick(&crypto, second(25), UTC)).len(), 32, "kept alive as the config asks");
        kept.send(&crypto, &packet(40), second(30), UTC);
        assert_eq!(kept.tick(&crypto, second(50), UTC).len(), 1, "the silence's handshake alone, fifteen seconds on");
        assert_eq!(kept.tick(&crypto, second(54), UTC), [], "counted from what went last");
    }

    #[test]
    fn keepalive_of_aged_session() {
        let crypto = Toy::default();
        let (mut kept, ..) = joined(&crypto, Some(second(25)), second(0));
        for count in 1..=4 {
            assert_eq!(sent(&kept.tick(&crypto, second(25 * count), UTC)).len(), 32, "a session under two minutes");
        }
        let steps = kept.tick(&crypto, second(125), UTC);
        assert!(
            matches!(&steps[..], [Step::Send(alive), Step::Send(begun)] if alive.len() == 32 && begun.len() == 148),
            "a word that keeps a session of two minutes alive asks for a new one, as a packet does: {steps:?}"
        );
        let (mut heard, _, theirs) = joined(&crypto, None, second(0));
        let data = noise::seal_data(&crypto, &theirs.sending, theirs.peer_index, 0, &packet(40), MOST).unwrap();
        heard.take(&crypto, &data, second(115), UTC);
        let steps = heard.tick(&crypto, second(125), UTC);
        assert!(
            matches!(&steps[..], [Step::Send(alive), Step::Send(begun)] if alive.len() == 32 && begun.len() == 148),
            "and so does the word behind a packet that came: {steps:?}"
        );
    }

    #[test]
    fn cookie_kept_for_retry() {
        let crypto = Toy::default();
        let (keys, responder) = pair(&crypto);
        let mut peer = Peer::new(keys, None, MOST);
        let first = sent(&peer.begin(&crypto, second(0), UTC));
        assert_eq!(peer.take(&crypto, &responder.refuse(&first, [0x66; 16]), second(1), UTC), []);
        let again = sent(&peer.tick(&crypto, second(5), UTC));
        assert_ne!(again[132..], [0; 16], "the next try is signed with the cookie");
        let much_later = sent(&peer.tick(&crypto, second(60), UTC));
        assert_ne!(much_later[132..], [0; 16]);
        peer.tick(&crypto, second(90), UTC);
        let stale = sent(&peer.begin(&crypto, second(121), UTC));
        assert_eq!(stale[132..], [0; 16], "for two minutes");
    }
}

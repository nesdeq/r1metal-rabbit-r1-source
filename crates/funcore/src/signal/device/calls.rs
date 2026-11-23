use super::outbox::padding;
use super::{Address, Device, Event, File, Order, Peer, ServiceId};
use crate::base::code::hex;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::base::time::MILLIS_PER_SECOND;
use crate::signal::Crypto;
use crate::signal::call::{
    ACCEPTED, CLEAR, GroupSignal, Hangup, Kind, LogChange, Logged, MARKED_AS_READ, MARKED_AS_READ_IN_CONVERSATION,
    Outcome, RINGING, Signal,
};
use crate::signal::command::{Outbound, Target};
use crate::signal::group;
use crate::signal::service::Relay;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

// RingRTC's `MAX_MESSAGE_AGE`: an offer or a group's ring older is expired, by the whole seconds Signal-Desktop counts a message's age in.
const MAX_AGE_S: u64 = 60;
// The newest calls kept, as many as a conversation's view holds of its messages (`domain::signal::list::MAX_MESSAGES`).
const MAX_CALLS: usize = 200;
// An era RingRTC reads as a number: sixteen hex digits (`RingId::from_era_id`).
const ERA_DIGITS: usize = 16;
const HEX: u32 = 16;
const CALL_BYTES: usize = 8;

/// How a call of the log stands (Signal-Desktop's `CallStatusValue`, those its one-to-one and group calls come to).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    // A one-to-one call that rings or is under way unanswered.
    Pending = 0,
    Accepted = 1,
    Missed = 2,
    Declined = 3,
    // Deleted on another device, kept so that nothing brings it back, and shown nowhere.
    Deleted = 4,
    // A group's call seen begun.
    Generic = 5,
    OutgoingRing = 6,
    Ringing = 7,
    Joined = 8,
}

const STATUSES: [Status; 9] = [
    Status::Pending,
    Status::Accepted,
    Status::Missed,
    Status::Declined,
    Status::Deleted,
    Status::Generic,
    Status::OutgoingRing,
    Status::Ringing,
    Status::Joined,
];

/// What became of a call, as the call layer tells it (Signal-Desktop's `LocalCallEvent`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Course {
    // A group's call seen begun, or one the owner joined that rings nobody.
    Started,
    // Ringing, in or out: a one-to-one call's offer, a group's ring.
    Ringing,
    // Answered, here or on another device; a group's call joined.
    Accepted,
    Declined,
    // Hung up here: an incoming call declined, an outgoing one missed.
    Hangup,
    // Hung up by the far side: an incoming call missed, an outgoing one declined.
    RemoteHangup,
    Missed,
}

/// A call as the log knows it: its conversation and id, its kind, whether the owner began it, who rang or began a group's where known, and when, ms of the wall clock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Note {
    pub(crate) peer: Peer,
    pub(crate) call: u64,
    pub(crate) kind: Kind,
    pub(crate) is_outgoing: bool,
    pub(crate) ringer: Option<ServiceId>,
    pub(crate) at: u64,
}

impl Note {
    const fn incoming(peer: Peer, (call, is_video): (u64, bool), caller: ServiceId, at: u64) -> Note {
        let kind = if is_video { Kind::Video } else { Kind::Audio };
        Note { peer, call, kind, is_outgoing: false, ringer: Some(caller), at }
    }
}

// A call of the log, and whether the owner has seen it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Call {
    id: u64,
    pub(crate) peer: Peer,
    pub(crate) kind: Kind,
    pub(crate) is_outgoing: bool,
    ringer: Option<ServiceId>,
    pub(crate) status: Status,
    pub(crate) at: u64,
    pub(crate) is_seen: bool,
}

impl Call {
    fn write(&self) -> Writer {
        Writer::new()
            .varint(1, self.id)
            .bytes(2, &self.peer.to_bytes())
            .varint(3, self.kind as u64)
            .bool(4, self.is_outgoing)
            .varint(5, self.at)
            .varint(6, self.status as u64)
            .bool(7, self.is_seen)
            .maybe(self.ringer.map(|ringer| move |w: Writer| w.bytes(8, &ringer.to_binary())))
    }

    fn read(message: &Message) -> Option<Call> {
        let kind = message.varint(3)?;
        let status = message.varint(6)?;
        Some(Call {
            id: message.varint(1)?,
            peer: Peer::from_bytes(message.bytes(2)?)?,
            kind: Kind::read(kind)?,
            is_outgoing: message.bool(4).unwrap_or(false),
            ringer: message.bytes(8).and_then(ServiceId::from_binary),
            status: STATUSES.into_iter().find(|known| *known as u64 == status)?,
            at: message.varint(5)?,
            is_seen: message.bool(7).unwrap_or(true),
        })
    }
}

pub(super) fn write(calls: &[Call]) -> Vec<u8> {
    calls.iter().fold(Writer::new(), |writer, call| writer.message(1, call.write())).finish()
}

pub(super) fn read(bytes: &[u8]) -> Vec<Call> {
    let Some(message) = Message::parse(bytes) else { return Vec::new() };
    message.repeated(1).filter_map(|call| Call::read(&Message::parse(call)?)).collect()
}

/// A call message that passed the gate, for the call layer: who sent it, by the name the list gives its conversation, the name of the group a group's signal is of, its signals, how long the service held it, ms, whether the sender's conversation is accepted, the identity keys a one-to-one call's keys are drawn under, the sender's as held and the account's own ACI's, and the number of this device of the account's, which a hang-up of its own doing names.
#[derive(Clone, PartialEq, Eq)]
pub struct Incoming {
    pub(crate) sender: Address,
    pub(crate) name: Option<String>,
    pub(crate) group_name: Option<String>,
    pub(crate) signals: Vec<Signal>,
    pub(crate) age: u64,
    pub(crate) is_accepted: bool,
    pub(crate) identity: [u8; 32],
    pub(crate) own_identity: [u8; 32],
    pub(crate) own_device: u32,
}

impl core::fmt::Debug for Incoming {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Incoming")
            .field("signals", &self.signals.len())
            .field("age", &self.age)
            .field("accepted", &self.is_accepted)
            .finish_non_exhaustive()
    }
}

/// A member of a group as Signal's calling server names it in a call, the SHA-256 of its id's ciphertext under the group's key in hex (RingRTC's `MemberMap`), with the name the list gives it.
#[derive(Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub(crate) service: ServiceId,
    pub(crate) opaque: String,
    pub(crate) name: Option<String>,
}

/// What a group's call is joined with: the token, the group's members as the calling server names them, and which of them the owner is.
#[derive(Clone, PartialEq, Eq)]
pub struct Membership {
    pub(crate) token: String,
    pub(crate) members: Vec<GroupMember>,
    pub(crate) own: ServiceId,
}

impl core::fmt::Debug for Membership {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Membership").field("members", &self.members.len()).finish_non_exhaustive()
    }
}

// What happened to a call: told here, or by another device of the account.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Happened {
    Local(Course),
    Remote(Outcome),
}

// Signal-Desktop's `transitionDirectCallStatus`.
const fn direct_status(status: Option<Status>, happened: Happened, is_outgoing: bool) -> Status {
    match (status, happened) {
        (Some(Status::Deleted), _) | (_, Happened::Remote(Outcome::Deleted)) => Status::Deleted,
        (_, Happened::Remote(Outcome::Accepted) | Happened::Local(Course::Accepted)) | (Some(Status::Accepted), _) => {
            Status::Accepted
        }
        (Some(Status::Declined), Happened::Remote(Outcome::NotAccepted)) => Status::Declined,
        (_, Happened::Remote(Outcome::NotAccepted) | Happened::Local(Course::Missed)) => Status::Missed,
        (_, Happened::Local(Course::Declined)) => Status::Declined,
        (_, Happened::Local(Course::Hangup)) if is_outgoing => Status::Missed,
        (_, Happened::Local(Course::Hangup)) => Status::Declined,
        (_, Happened::Local(Course::RemoteHangup)) if is_outgoing => Status::Declined,
        (_, Happened::Local(Course::RemoteHangup)) => Status::Missed,
        (_, Happened::Local(Course::Started | Course::Ringing)) => Status::Pending,
    }
}

// Signal-Desktop's `transitionGroupCallStatus`; none where it refuses the event.
const fn group_status(status: Option<Status>, happened: Happened, is_outgoing: bool) -> Option<Status> {
    Some(match (status, happened) {
        (Some(Status::Deleted), _) | (_, Happened::Remote(Outcome::Deleted)) => Status::Deleted,
        (Some(Status::Accepted), _) => Status::Accepted,
        (_, Happened::Remote(Outcome::NotAccepted)) => return None,
        (_, Happened::Remote(Outcome::Accepted) | Happened::Local(Course::Accepted)) => match status {
            None | Some(Status::Generic) => Status::Joined,
            Some(Status::Ringing | Status::Missed | Status::Declined) => Status::Accepted,
            Some(held @ (Status::Joined | Status::OutgoingRing)) => held,
            Some(Status::Pending | Status::Accepted | Status::Deleted) => return None,
        },
        (_, Happened::Local(Course::Started)) => Status::Generic,
        (_, Happened::Local(Course::Ringing)) if is_outgoing => Status::OutgoingRing,
        (_, Happened::Local(Course::Ringing)) => Status::Ringing,
        (_, Happened::Local(Course::Missed)) => Status::Missed,
        (_, Happened::Local(Course::Declined)) => Status::Declined,
        (_, Happened::Local(Course::Hangup)) if is_outgoing => Status::Missed,
        (_, Happened::Local(Course::Hangup)) => Status::Declined,
        (_, Happened::Local(Course::RemoteHangup)) if is_outgoing => Status::Declined,
        (_, Happened::Local(Course::RemoteHangup)) => Status::Missed,
    })
}

// Signal-Desktop's `transitionTimestamp`: the later time where the entry's status holds none worth keeping, else the entry's, but the later of a remote word that agrees with it.
fn transition_time(held: Option<&Call>, happened: Happened, at: u64) -> u64 {
    let Some(held) = held else { return at };
    let later = at.max(held.at);
    match held.status {
        Status::Deleted => held.at,
        Status::Accepted | Status::Joined | Status::OutgoingRing if happened == Happened::Remote(Outcome::Accepted) => {
            later
        }
        Status::Declined if happened == Happened::Remote(Outcome::NotAccepted) => later,
        Status::Accepted | Status::Joined | Status::OutgoingRing | Status::Declined => held.at,
        Status::Pending | Status::Generic | Status::Ringing | Status::Missed => later,
    }
}

// Signal-Desktop's unseen call (`saveCallHistory`): one that came in and was missed, or rings or is under way unanswered.
fn is_unseen(kind: Kind, is_outgoing: bool, status: Status) -> bool {
    let unseen: &[Status] = match kind {
        Kind::Group => &[Status::Ringing, Status::Generic, Status::Missed],
        Kind::Audio | Kind::Video => &[Status::Missed, Status::Pending],
    };
    !is_outgoing && unseen.contains(&status)
}

// The status Signal-Desktop syncs a call by (`statusToProto`); none for one it does not sync.
const fn outcome_of(status: Status) -> Option<Outcome> {
    match status {
        Status::Accepted | Status::Joined | Status::OutgoingRing => Some(Outcome::Accepted),
        Status::Declined => Some(Outcome::NotAccepted),
        Status::Deleted => Some(Outcome::Deleted),
        Status::Pending | Status::Missed | Status::Generic | Status::Ringing => None,
    }
}

// A group call's id by its era, as RingRTC reads one (`RingId::from_era_id`): sixteen hex digits as the number they write, nought taken as the ring of all ones, any other era the first eight bytes of its SHA-256, the lowest first.
pub(crate) fn call_of_era(crypto: &impl Crypto, era: &str) -> u64 {
    if era.len() == ERA_DIGITS
        && let Ok(id) = u64::from_str_radix(era, HEX)
    {
        return if id == 0 { u64::MAX } else { id };
    }
    let hash = crypto.sha256(&[era.as_bytes()]);
    u64::from_le_bytes(hash[..CALL_BYTES].try_into().expect("eight of thirty-two"))
}

impl Device {
    pub(crate) fn calls(&self) -> &[Call] {
        &self.calls
    }

    // A call message's gate, rule after rule in the order Signal-Desktop and RingRTC apply them to the caller, its timestamp, how long the service held it, ms, and whether it came to the phone number's identity; what passes goes to the call layer, and what the log took of the rest is told.
    pub(super) fn gate_call(
        &mut self,
        sender: Address,
        (signals, device): (Vec<Signal>, Option<u32>),
        (timestamp, age, is_to_pni): (u64, u64, bool),
        now: u64,
    ) -> Vec<Event> {
        // Signal-Desktop takes nothing of a sender whose identity key it does not hold (`handleCallingMessage`).
        let Some(identity) = self.identity_key(&sender.service) else { return Vec::new() };
        let contact = Peer::Contact(sender.service);
        let is_accepted = self.is_accepted(&contact, false);
        let offer = signals.iter().find_map(|signal| match signal {
            Signal::Offer { call, is_video, .. } => Some((*call, *is_video)),
            _ => None,
        });
        // An offer from a conversation not accepted is answered that it needs permission and missed, but one to the phone number's identity, which the answer would tell of, is passed over (`handleCallingMessage`).
        if let Some(offer) = offer.filter(|_| !is_accepted) {
            if is_to_pni {
                return Vec::new();
            }
            self.owed_hangups.push((sender, offer.0));
            let note = Note::incoming(contact, offer, sender.service, timestamp);
            return self.log(&note, Course::Missed).then_some(Event::Logged).into_iter().collect();
        }
        // What is for another device of the account is dropped (RingRTC's `handleCallingMessage`).
        if device.is_some_and(|device| device != self.account.device) {
            return Vec::new();
        }
        let (mut passed, mut is_logged) = (Vec::new(), false);
        for signal in signals {
            let is_passed = match &signal {
                Signal::Offer { call, is_video, .. } => match self.missed_offer_at(sender.service, age, now) {
                    Some(at) => {
                        is_logged |=
                            self.log(&Note::incoming(contact, (*call, *is_video), sender.service, at), Course::Missed);
                        false
                    }
                    None => true,
                },
                Signal::Group { signal: group, .. } => {
                    let (is_passed, is_changed) = self.gate_group(sender.service, group, age, now);
                    is_logged |= is_changed;
                    is_passed
                }
                Signal::Answer { .. } | Signal::Candidates { .. } | Signal::Hangup { .. } | Signal::Busy { .. } => true,
            };
            if is_passed {
                passed.push(signal);
            }
        }
        let own_identity = self.account.aci_identity.public;
        let group = passed.iter().find_map(|signal| match signal {
            Signal::Group { signal, .. } => Some(signal.group()),
            _ => None,
        });
        let (name, group_name) = (self.name_of(&contact), group.and_then(|group| self.name_of(&group)));
        let own_device = self.account.device;
        let incoming = Incoming {
            sender,
            name,
            group_name,
            signals: passed,
            age,
            is_accepted,
            identity,
            own_identity,
            own_device,
        };
        let called = (!incoming.signals.is_empty()).then(|| Event::Called(Box::new(incoming)));
        called.into_iter().chain(is_logged.then_some(Event::Logged)).collect()
    }

    // When an offer is missed: one older than RingRTC takes at the time it was made (`validate_offer`, `handleRejectedIncomingCallRequest`), one of a muted conversation that does not tell calls or from an identity left unverified now (`handleIncomingCall`); none for one that rings.
    fn missed_offer_at(&self, caller: ServiceId, age: u64, now: u64) -> Option<u64> {
        let age_s = age / MILLIS_PER_SECOND;
        if age_s > MAX_AGE_S {
            return Some(now.saturating_sub(age_s * MILLIS_PER_SECOND));
        }
        (self.is_call_muted(&Peer::Contact(caller), now) || self.is_unverified(&caller)).then_some(now)
    }

    // A group's signal as RingRTC (`handle_received_call_message`) and Signal-Desktop (`handleGroupCallRingUpdate`) take one: whether it goes to the call layer, and whether the log changed.
    fn gate_group(&mut self, sender: ServiceId, signal: &GroupSignal, age: u64, now: u64) -> (bool, bool) {
        let own = self.account.aci();
        match *signal {
            GroupSignal::Ring { group, ring, is_cancelled } => {
                if !self.may_ring(&group, sender) {
                    return (false, false);
                }
                let note = Note {
                    peer: group,
                    call: ring as u64,
                    kind: Kind::Group,
                    is_outgoing: sender == own,
                    ringer: Some(sender),
                    at: now,
                };
                if is_cancelled {
                    return (true, self.log(&note, Course::Missed));
                }
                if age / MILLIS_PER_SECOND > MAX_AGE_S {
                    return (false, self.log(&note, Course::Missed));
                }
                if self.is_call_muted(&group, now) {
                    return (false, self.log(&note, Course::Started));
                }
                (true, false)
            }
            GroupSignal::Answered { group, ring, kind } => {
                if sender != own || kind == RINGING || !self.may_ring(&group, sender) {
                    return (false, false);
                }
                let course = if kind == ACCEPTED { Course::Accepted } else { Course::Missed };
                let note = Note {
                    peer: group,
                    call: ring as u64,
                    kind: Kind::Group,
                    is_outgoing: true,
                    ringer: Some(own),
                    at: now,
                };
                (true, self.log(&note, course))
            }
            GroupSignal::MediaKey { group, .. } | GroupSignal::Leaving { group, .. } => {
                let members = self.members(&group);
                (members.contains(&own) && members.contains(&sender), false)
            }
        }
    }

    // Whether a ring of the group, or its answer, from `ringer` is taken (`handleGroupCallRingUpdate`): of a group known, not blocked and accepted, of which the owner and the ringer are members, not ended, and where only administrators send, from an administrator.
    fn may_ring(&self, peer: &Peer, ringer: ServiceId) -> bool {
        let own = self.account.aci();
        let is_open = self.group(peer).is_some_and(|group| {
            group.members.contains(&own)
                && group.members.contains(&ringer)
                && !group.terminated
                && (!group.announcements_only || group.admins.contains(&ringer))
        });
        is_open && !self.is_blocked(peer) && self.is_accepted(peer, false)
    }

    // Takes what happened to a call into the log by Signal-Desktop's transitions (`transitionCallHistory`), refused where the call's kind, direction or ringer disagrees with its entry's; the status before and the entry, where the entry changed.
    fn change_log(&mut self, note: &Note, happened: Happened) -> Option<(Option<Status>, Call)> {
        let place = self.calls.iter().position(|call| (call.id, call.peer) == (note.call, note.peer));
        let held = place.map(|place| self.calls[place]);
        if let Some(held) = held {
            let is_unknown_creator = held.kind == Kind::Group
                && !held.is_outgoing
                && held.status == Status::Generic
                && held.ringer.is_none();
            let is_same_ringer = note.ringer.is_none() || held.ringer.is_none() || note.ringer == held.ringer;
            if !is_same_ringer
                || held.kind != note.kind
                || (held.is_outgoing != note.is_outgoing && !is_unknown_creator)
            {
                return None;
            }
        }
        let before = held.map(|call| call.status);
        let status = match note.kind {
            Kind::Group => group_status(before, happened, note.is_outgoing)?,
            Kind::Audio | Kind::Video => direct_status(before, happened, note.is_outgoing),
        };
        let entry = Call {
            id: note.call,
            peer: note.peer,
            kind: note.kind,
            is_outgoing: note.is_outgoing,
            ringer: note.ringer,
            status,
            at: transition_time(held.as_ref(), happened, note.at),
            is_seen: held.is_some_and(|call| call.is_seen) || !is_unseen(note.kind, note.is_outgoing, status),
        };
        if held == Some(entry) {
            return None;
        }
        match place {
            Some(place) => self.calls[place] = entry,
            None => self.calls.push(entry),
        }
        let excess = self.calls.len().saturating_sub(MAX_CALLS);
        self.calls.drain(..excess);
        self.dirty.insert(File::Calls);
        Some((before, entry))
    }

    fn log(&mut self, note: &Note, course: Course) -> bool {
        self.change_log(note, Happened::Local(course)).is_some()
    }

    // Takes what the call layer tells of a call into the log; the sync that tells the account's other devices of it where the status changed to one Signal-Desktop syncs (`updateCallHistoryFromLocalEvent`, `updateRemoteCallHistory`).
    pub(in crate::signal) fn take_course(
        &mut self,
        crypto: &impl Crypto,
        (note, course): (&Note, Course),
        now: u64,
    ) -> Option<Order> {
        let (before, entry) = self.change_log(note, Happened::Local(course))?;
        let outcome = outcome_of(entry.status).filter(|_| before != Some(entry.status))?;
        let logged = Logged {
            peer: entry.peer,
            call: entry.id,
            timestamp: entry.at,
            kind: entry.kind,
            is_outgoing: entry.is_outgoing,
            outcome,
        };
        let content = logged.write(&padding(crypto)?);
        Some(Order::new(self.account.aci(), content, self.stamp(now), false))
    }

    // Whether the conversation is one the R1 knows: a group's, or a contact's it holds or names.
    fn is_known(&self, peer: &Peer) -> bool {
        match peer {
            Peer::Contact(contact) => self.contacts.contains_key(contact) || self.conversations.contains_key(peer),
            Peer::Group(_) => self.group(peer).is_some(),
        }
    }

    // Takes a call another device of the account tells of, where its conversation is known (`onCallEventSync`); true where the log changed.
    pub(super) fn take_logged(&mut self, logged: &Logged) -> bool {
        if !self.is_known(&logged.peer) {
            return false;
        }
        let note = Note {
            peer: logged.peer,
            call: logged.call,
            kind: logged.kind,
            is_outgoing: logged.is_outgoing,
            ringer: None,
            at: logged.timestamp,
        };
        self.change_log(&note, Happened::Remote(logged.outcome)).is_some()
    }

    // The place of the call a change of the log names (`getCallHistoryForCallLogEventTarget`): by its conversation and id, else the newest no later than the change's time.
    fn log_target(&self, change: &LogChange) -> Option<usize> {
        let calls = self.calls.iter().enumerate();
        match change.target {
            Some(target) => self.calls.iter().position(|call| (call.peer, call.id) == target),
            None => {
                calls.filter(|(_, call)| call.at <= change.timestamp).max_by_key(|(_, call)| call.at).map(|(at, _)| at)
            }
        }
    }

    // Takes a change of the log made on another device of the account (`onCallLogEventSync`): the calls up to its target deleted, or seen, of its conversation alone where it says so; true where the log changed.
    pub(super) fn take_log_change(&mut self, change: &LogChange, now: u64) -> bool {
        let Some(target) = self.log_target(change) else { return false };
        let Call { at, peer, status, .. } = self.calls[target];
        let mut is_changed = false;
        match change.kind {
            // Every call no later than the target, deleted ones among them (`clearCallHistory`).
            CLEAR => {
                for call in self.calls.iter_mut().filter(|call| call.at <= at) {
                    is_changed |= (call.status, call.at) != (Status::Deleted, now);
                    (call.status, call.at) = (Status::Deleted, now);
                }
            }
            // Every call logged no later than the target, as Signal-Desktop reads up to its call's message, which a deleted call has none of (`getUnreadCallMessagesAndMarkRead`).
            MARKED_AS_READ | MARKED_AS_READ_IN_CONVERSATION if status != Status::Deleted => {
                let is_everywhere = change.kind == MARKED_AS_READ;
                for call in self.calls[..=target].iter_mut().filter(|call| !call.is_seen) {
                    if is_everywhere || call.peer == peer {
                        (call.is_seen, is_changed) = (true, true);
                    }
                }
            }
            // Clearing a conversation's calls, which Signal-Desktop does not do either, and reading up to a deleted call.
            _ => {}
        }
        if is_changed {
            self.dirty.insert(File::Calls);
        }
        is_changed
    }

    // The log shows (Signal-Desktop's `markCallsTabViewed`): the calls up to the newest count as seen, and the account's other devices are told so.
    pub(in crate::signal) fn see_calls(&mut self, crypto: &impl Crypto, now: u64) -> Option<Order> {
        let newest = *self.calls.iter().filter(|call| call.status != Status::Deleted).max_by_key(|call| call.at)?;
        let change = LogChange { kind: MARKED_AS_READ, timestamp: newest.at, target: Some((newest.peer, newest.id)) };
        self.take_log_change(&change, now);
        let content = change.write(&padding(crypto)?);
        Some(Order::new(self.account.aci(), content, self.stamp(now), false))
    }

    // A group's call seen begun, of a member's update (`handleGroupCallUpdateMessage`, `updateLocalHistoryFromGroupCallUpdateMessage`): an entry made where the group is accepted and, where only administrators send, the member is one; one held takes an earlier time. True where the log changed.
    pub(super) fn take_group_call(
        &mut self,
        crypto: &impl Crypto,
        peer: Peer,
        (sender, era): (ServiceId, &str),
        timestamp: u64,
    ) -> bool {
        let own = self.account.aci();
        let Some(group) = self.group(&peer) else { return false };
        if group.terminated {
            return false;
        }
        let may_make = sender != own && (!group.announcements_only || group.admins.contains(&sender));
        let call = call_of_era(crypto, era);
        if let Some(held) = self.calls.iter_mut().find(|held| (held.id, held.peer) == (call, peer)) {
            if timestamp >= held.at {
                return false;
            }
            held.at = timestamp;
            self.dirty.insert(File::Calls);
            return true;
        }
        let note = Note { peer, call, kind: Kind::Group, is_outgoing: false, ringer: None, at: timestamp };
        may_make && self.is_accepted(&peer, false) && self.log(&note, Course::Started)
    }

    // The orders of a call's signal: an offer to a contact behind the owner's profile key, the profile shared with the contact from then on (`startOutgoingDirectCall`).
    pub(in crate::signal) fn signal_orders(&mut self, outbound: Outbound, now: u64) -> Vec<Order> {
        let mut orders = Vec::new();
        if let (Target::Contact(contact), Signal::Offer { .. }) = (&outbound.target, &outbound.signal) {
            self.set_sharing(Peer::Contact(*contact), true);
            let timestamp = self.stamp(now);
            orders.push(self.profile_key_order(*contact, timestamp));
        }
        orders.push(self.signal_order(outbound, now));
        orders
    }

    // A call's signal to its target: a contact's devices and a group's members as the peer's messages go to them, named members among those, or the account's other devices.
    fn signal_order(&mut self, outbound: Outbound, now: u64) -> Order {
        let own = self.account.aci();
        let (peer, recipients) = match &outbound.target {
            Target::Contact(contact) => (Peer::Contact(*contact), None),
            Target::Group(group) => (*group, None),
            Target::Members(group, named) => {
                let members = self.recipients(group, now).into_iter().filter(|member| named.contains(member));
                (*group, Some(members.collect()))
            }
            Target::Own => (Peer::Contact(own), Some(alloc::vec![own])),
        };
        let content = outbound.signal.write(outbound.device);
        Order::call(peer, recipients, (content, self.stamp(now)), outbound)
    }

    // The hang-ups owed the callers of offers from conversations not accepted, each naming the caller's device (`handleCallingMessage`).
    pub(in crate::signal) fn take_hangups(&mut self, now: u64) -> Vec<Order> {
        let owed = core::mem::take(&mut self.owed_hangups);
        owed.into_iter()
            .map(|(caller, call)| {
                let signal = Signal::Hangup { call, kind: Hangup::NeedPermission, device: Some(caller.device) };
                self.signal_order(Outbound { target: Target::Contact(caller.service), device: None, signal }, now)
            })
            .collect()
    }

    // What a group's call is joined with: its token, and the group's members as the calling server names them, the owner among them; none for a group not known.
    pub(in crate::signal) fn membership(&self, crypto: &impl Crypto, peer: &Peer, token: String) -> Option<Membership> {
        let group = self.group(peer)?;
        let key = group::Keys::new(crypto, &group.master_key).uid;
        let members = group.members.iter().map(|member| GroupMember {
            service: *member,
            opaque: hex(&crypto.sha256(&[&key.encrypt_id(crypto, member)])),
            name: self.name_of(&Peer::Contact(*member)),
        });
        Some(Membership { token, members: members.collect(), own: self.account.aci() })
    }

    // The relays the service handed out, while they may still be used at `now` (Signal-Desktop's `#getIceServers`).
    pub(in crate::signal) fn cached_relays(&self, now: u64) -> Option<Vec<Relay>> {
        self.relays.as_ref().filter(|(_, until)| now < *until).map(|(relays, _)| relays.clone())
    }

    // Keeps the relays the service handed out at `now` for the least time any of them may be used.
    pub(in crate::signal) fn keep_relays(&mut self, relays: &[Relay], now: u64) {
        if let Some(least) = relays.iter().map(|relay| relay.ttl.unwrap_or(0)).min() {
            self.relays = Some((relays.to_vec(), now.saturating_add(least.saturating_mul(MILLIS_PER_SECOND))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Reception;
    use super::super::identities::UNVERIFIED;
    use super::*;
    use crate::signal::call;
    use crate::signal::content::{self, ACCEPT, Content};
    use crate::signal::group::Context;
    use crate::signal::sealed;
    use crate::signal::storage::ListState;
    use crate::signal::tests::{Exact, Standin};
    use alloc::vec;

    const ALICE: ServiceId = ServiceId::Aci([4; 16]);
    const BORIS: ServiceId = ServiceId::Aci([5; 16]);
    const CAROL: ServiceId = ServiceId::Aci([6; 16]);
    const CALLER: Address = Address { service: ALICE, device: 3 };
    const NOW: u64 = 1_800_000_000_000;
    const SENT: u64 = NOW - 5_000;
    const ALICE_CHAT: Peer = Peer::Contact(ALICE);

    impl Device {
        // A call into the log by what became of it here, or with none, deleted on another device.
        pub(crate) fn add_call(&mut self, note: &Note, course: Option<Course>) {
            self.change_log(note, course.map_or(Happened::Remote(Outcome::Deleted), Happened::Local));
        }
    }

    fn offer(call: u64) -> Signal {
        Signal::Offer { call, is_video: true, parameters: None }
    }

    // A device that holds the identity keys of Alice, Boris and Carol, Alice's conversation accepted where `is_accepted`.
    fn with_alice(is_accepted: bool) -> Device {
        let mut device = Device::unkeyed();
        for (service, key) in [(ALICE, [7; 32]), (BORIS, [8; 32]), (CAROL, [9; 32])] {
            device.save_identity(service, key, None, 0);
        }
        if is_accepted {
            device.apply_response(ALICE_CHAT, ACCEPT, false);
        }
        device
    }

    fn gate(device: &mut Device, signals: Vec<Signal>, (to, age, is_to_pni): (Option<u32>, u64, bool)) -> Vec<Event> {
        device.gate_call(CALLER, (signals, to), (SENT, age, is_to_pni), NOW)
    }

    fn missed(peer: Peer, (call, kind): (u64, Kind), at: u64) -> Call {
        Call {
            id: call,
            peer,
            kind,
            is_outgoing: false,
            ringer: Some(ALICE),
            status: Status::Missed,
            at,
            is_seen: false,
        }
    }

    #[test]
    fn signals_handed_on() {
        let mut device = with_alice(false);
        device.add_named_contact(ALICE, "Alice", false);
        device.apply_response(ALICE_CHAT, ACCEPT, false);
        device.take_dirty();
        let own = device.account.device;
        let answer = Signal::Answer { call: 9, parameters: None };
        let events = gate(&mut device, vec![offer(9), answer.clone()], (Some(own), 2_500, false));
        let incoming = Incoming {
            sender: CALLER,
            name: Some("Alice".into()),
            group_name: None,
            signals: vec![offer(9), answer],
            age: 2_500,
            is_accepted: true,
            identity: [7; 32],
            own_identity: device.account.aci_identity.public,
            own_device: own,
        };
        assert_eq!(
            events,
            [Event::Called(Box::new(incoming))],
            "the caller's device and name, the signals, their age, the conversation accepted and both identity keys"
        );
        assert!(
            device.calls().is_empty() && device.take_hangups(NOW).is_empty() && device.take_dirty().is_empty(),
            "nothing logged, owed or kept: the call layer rings"
        );
    }

    #[test]
    fn other_device_and_no_identity() {
        let mut device = with_alice(true);
        let other = device.account.device + 1;
        let answer = Signal::Answer { call: 9, parameters: None };
        assert!(
            gate(&mut device, vec![answer], (Some(other), 0, false)).is_empty(),
            "for another device of the account"
        );
        let mut stranger = Device::unkeyed();
        stranger.apply_response(ALICE_CHAT, ACCEPT, false);
        assert!(
            gate(&mut stranger, vec![offer(9)], (None, 0, false)).is_empty() && stranger.calls().is_empty(),
            "from a sender whose identity key is not held"
        );
    }

    #[test]
    fn blocked_and_kept_nowhere() {
        let crypto = Standin::default();
        let mut device = with_alice(true);
        device.take_dirty();
        let content = offer(9).write(None);
        device.set_blocked(ALICE_CHAT, true);
        assert_eq!(
            device.receive_content(&crypto, &content, CALLER, (SENT, true, 0), NOW, false).err(),
            Some(super::super::DropReason::Blocked),
            "from a blocked account, before anything else"
        );
        device.set_blocked(ALICE_CHAT, false);
        device.take_dirty();
        let reception = device.receive_content(&crypto, &content, CALLER, (SENT, true, 0), NOW, false);
        assert!(
            matches!(reception, Ok(Reception::Call(ref signals, None)) if *signals == [offer(9)]),
            "handed to the delivery's gate"
        );
        assert!(
            device.conversation(&ALICE_CHAT).is_none()
                && device.early_contents.is_empty()
                && device.take_dirty().is_empty(),
            "written to no conversation, held for nothing, kept in no file"
        );
    }

    #[test]
    fn offer_needs_permission() {
        let mut device = with_alice(false);
        assert!(
            device.is_accepted(&ALICE_CHAT, true) && !device.is_accepted(&ALICE_CHAT, false),
            "an empty conversation counts as accepted for a message, not for a call (`ignoreEmptyConvo`)"
        );
        let candidates = Signal::Candidates { call: 9, candidates: vec![] };
        assert_eq!(
            gate(&mut device, vec![offer(9), candidates], (None, 0, false)),
            [Event::Logged],
            "an offer from a conversation not accepted: nothing of its message rings"
        );
        assert_eq!(device.calls(), [missed(ALICE_CHAT, (9, Kind::Video), SENT)], "missed at the offer's timestamp");
        let hangups = device.take_hangups(NOW);
        let [order] = &hangups[..] else { panic!("one hang-up") };
        assert_eq!(
            call::read(&Message::parse(&order.content).unwrap()),
            Some((vec![Signal::Hangup { call: 9, kind: Hangup::NeedPermission, device: Some(3) }], None)),
            "naming the caller's device, to all of the caller's"
        );
        assert_eq!(
            (order.peer, order.is_addressed, order.is_urgent, order.hint, order.is_logged),
            (ALICE_CHAT, false, false, sealed::DEFAULT, false),
            "not urgent, of no content hint, kept in no send log"
        );
        assert!(device.take_hangups(NOW).is_empty(), "owed once");
        let mut pni = with_alice(false);
        assert!(
            gate(&mut pni, vec![offer(9)], (None, 0, true)).is_empty()
                && pni.calls().is_empty()
                && pni.take_hangups(NOW).is_empty(),
            "to the phone number's identity: nothing, which would tell the account"
        );
        let mut accepted = with_alice(true);
        assert!(
            matches!(gate(&mut accepted, vec![offer(9)], (None, 0, true))[..], [Event::Called(_)]),
            "where accepted, an offer to the phone number's identity rings as any"
        );
    }

    #[test]
    fn offer_too_old() {
        let mut device = with_alice(true);
        assert!(
            matches!(gate(&mut device, vec![offer(1)], (None, 60_999, false))[..], [Event::Called(_)]),
            "sixty whole seconds old, it rings"
        );
        assert_eq!(
            gate(&mut device, vec![offer(2), Signal::Busy { call: 2 }], (None, 61_000, false)).len(),
            2,
            "sixty-one: the busy signal handed on, the log told"
        );
        assert_eq!(
            device.calls(),
            [missed(ALICE_CHAT, (2, Kind::Video), NOW - 61_000)],
            "missed at the time it was made; what came beside it handed on"
        );
        assert!(device.take_hangups(NOW).is_empty(), "nothing sent");
    }

    #[test]
    fn offer_muted_or_unverified() {
        let mut device = with_alice(true);
        device.set_list_state(ALICE_CHAT, ListState { muted_until: NOW + 1, ..ListState::default() });
        assert_eq!(gate(&mut device, vec![offer(1)], (None, 0, false)), [Event::Logged], "muted, calls not told");
        assert_eq!(device.calls(), [missed(ALICE_CHAT, (1, Kind::Video), NOW)], "missed now");
        device.account.listing.notify.2 = true;
        assert!(
            matches!(gate(&mut device, vec![offer(2)], (None, 0, false))[..], [Event::Called(_)]),
            "the account tells them"
        );
        device.conversation_mut(ALICE_CHAT).notify.2 = Some(false);
        assert_eq!(
            gate(&mut device, vec![offer(3)], (None, 0, false)),
            [Event::Logged],
            "the conversation's word first"
        );
        let mut unverified = with_alice(true);
        unverified.identities.get_mut(&ALICE).unwrap().verified = UNVERIFIED;
        assert_eq!(
            gate(&mut unverified, vec![offer(4)], (None, 0, false)),
            [Event::Logged],
            "from an identity left unverified"
        );
        assert_eq!(unverified.calls()[0].status, Status::Missed, "missed");
    }

    // A group of the owner, Alice and Boris that the owner accepted.
    fn with_group(device: &mut Device) -> Peer {
        let own = device.account.aci();
        let group = device.add_group(&Standin::default(), &[8; 32], &[own, ALICE, BORIS]);
        device.group_mut(&group).unwrap().response = ACCEPT;
        group
    }

    fn group_gate(device: &mut Device, (sender, signal): (ServiceId, GroupSignal), age: u64) -> Vec<Event> {
        let from = Address { service: sender, device: 1 };
        device.gate_call(from, (vec![Signal::Group { signal, is_urgent: false }], None), (SENT, age, false), NOW)
    }

    #[test]
    fn group_rings() {
        let mut device = with_alice(false);
        let group = with_group(&mut device);
        let ring = |is_cancelled| GroupSignal::Ring { group, ring: -2, is_cancelled };
        assert!(matches!(group_gate(&mut device, (ALICE, ring(false)), 0)[..], [Event::Called(_)]), "a member rings");
        assert!(device.calls().is_empty(), "the call layer tells the log whether it rang");
        assert_eq!(group_gate(&mut device, (ALICE, ring(false)), 61_000), [Event::Logged], "expired");
        assert_eq!(
            device.calls(),
            [missed(group, (u64::MAX - 1, Kind::Group), NOW)],
            "missed by the ring's id, from the ringer, now"
        );
        let events = group_gate(&mut device, (ALICE, ring(true)), 0);
        assert!(matches!(events[..], [Event::Called(_)]), "a ring cancelled is handed on, its call missed already");
        let mut muted = with_alice(false);
        let quiet = with_group(&mut muted);
        muted.set_list_state(quiet, ListState { muted_until: NOW + 1, ..ListState::default() });
        let ring = GroupSignal::Ring { group: quiet, ring: 4, is_cancelled: false };
        assert_eq!(group_gate(&mut muted, (ALICE, ring), 0), [Event::Logged], "muted, calls not told: no ring");
        assert_eq!(muted.calls()[0].status, Status::Generic, "logged as begun (`handleGroupCallRingUpdate`)");
    }

    #[test]
    fn group_ring_refusals() {
        let refused = |change: &dyn Fn(&mut Device, Peer), ringer: ServiceId, why: &str| {
            let mut device = with_alice(false);
            let group = with_group(&mut device);
            change(&mut device, group);
            let ring = GroupSignal::Ring { group, ring: 4, is_cancelled: false };
            assert!(group_gate(&mut device, (ringer, ring), 0).is_empty() && device.calls().is_empty(), "{why}");
        };
        refused(&|_, _| {}, CAROL, "from no member");
        refused(&|device, group| device.set_blocked(group, true), ALICE, "to a blocked group");
        refused(&|device, group| device.group_mut(&group).unwrap().response = 0, ALICE, "to a group not accepted");
        let without_owner = |device: &mut Device, _| {
            device.add_group(&Standin::default(), &[8; 32], &[ALICE, BORIS]);
        };
        refused(&without_owner, ALICE, "the owner no member");
        refused(&|device, group| device.group_mut(&group).unwrap().terminated = true, ALICE, "to an ended group");
        let announcing = |device: &mut Device, group: Peer| {
            let held = device.group_mut(&group).unwrap();
            (held.announcements_only, held.admins) = (true, vec![BORIS]);
        };
        refused(&announcing, ALICE, "where only administrators send, from none");
        let mut device = with_alice(false);
        let group = with_group(&mut device);
        announcing(&mut device, group);
        let ring = GroupSignal::Ring { group, ring: 4, is_cancelled: false };
        assert_eq!(group_gate(&mut device, (BORIS, ring), 0).len(), 1, "from its administrator");
    }

    #[test]
    fn group_answers_keys_and_leaves() {
        let mut device = with_alice(false);
        let group = with_group(&mut device);
        let own = device.account.aci();
        let answered = |kind| GroupSignal::Answered { group, ring: 4, kind };
        assert!(group_gate(&mut device, (ALICE, answered(ACCEPTED)), 0).is_empty(), "from another account");
        assert!(
            group_gate(&mut device, (own, answered(RINGING)), 0).is_empty(),
            "ringing on another device of the account"
        );
        assert_eq!(group_gate(&mut device, (own, answered(ACCEPTED)), 0).len(), 2, "accepted on another device");
        let joined = Call {
            id: 4,
            peer: group,
            kind: Kind::Group,
            is_outgoing: true,
            ringer: Some(own),
            status: Status::Joined,
            at: NOW,
            is_seen: true,
        };
        assert_eq!(device.calls(), [joined], "as Signal-Desktop logs it, of the account's own ring");
        let key = GroupSignal::MediaKey { group, demux_id: 16, ratchet: 0, secret: [1; 32] };
        assert_eq!(group_gate(&mut device, (BORIS, key.clone()), 0).len(), 1, "a member's media key");
        assert!(group_gate(&mut device, (CAROL, key), 0).is_empty(), "no one else's");
        let leaving = GroupSignal::Leaving { group, demux_id: 16 };
        assert!(group_gate(&mut device, (CAROL, leaving.clone()), 0).is_empty(), "no one else's leave");
        assert_eq!(group_gate(&mut device, (ALICE, leaving), 0).len(), 1, "a member's leave");
    }

    #[test]
    fn direct_transitions() {
        use Course::*;
        let local = Happened::Local;
        let remote = Happened::Remote;
        let cases = [
            (None, local(Ringing), false, Status::Pending),
            (Some(Status::Pending), local(Accepted), false, Status::Accepted),
            (Some(Status::Accepted), local(Hangup), false, Status::Accepted),
            (Some(Status::Accepted), remote(Outcome::Deleted), false, Status::Deleted),
            (Some(Status::Deleted), local(Accepted), false, Status::Deleted),
            (Some(Status::Pending), local(Hangup), false, Status::Declined),
            (Some(Status::Pending), local(Hangup), true, Status::Missed),
            (Some(Status::Pending), local(RemoteHangup), false, Status::Missed),
            (Some(Status::Pending), local(RemoteHangup), true, Status::Declined),
            (Some(Status::Declined), remote(Outcome::NotAccepted), false, Status::Declined),
            (Some(Status::Pending), remote(Outcome::NotAccepted), false, Status::Missed),
            (None, remote(Outcome::Accepted), true, Status::Accepted),
            (Some(Status::Missed), local(Declined), false, Status::Declined),
            (Some(Status::Declined), local(Missed), false, Status::Missed),
            (None, local(Started), true, Status::Pending),
        ];
        for (before, happened, is_outgoing, after) in cases {
            assert_eq!(
                direct_status(before, happened, is_outgoing),
                after,
                "transitionDirectCallStatus: {before:?}, outgoing {is_outgoing}"
            );
        }
    }

    #[test]
    fn group_transitions() {
        use Course::*;
        let local = Happened::Local;
        let cases = [
            (None, local(Started), false, Some(Status::Generic)),
            (None, local(Ringing), false, Some(Status::Ringing)),
            (None, local(Ringing), true, Some(Status::OutgoingRing)),
            (None, local(Accepted), false, Some(Status::Joined)),
            (Some(Status::Generic), local(Accepted), false, Some(Status::Joined)),
            (Some(Status::Ringing), local(Accepted), false, Some(Status::Accepted)),
            (Some(Status::Missed), Happened::Remote(Outcome::Accepted), false, Some(Status::Accepted)),
            (Some(Status::OutgoingRing), local(Accepted), true, Some(Status::OutgoingRing)),
            (Some(Status::Accepted), local(Missed), false, Some(Status::Accepted)),
            (Some(Status::Ringing), local(Hangup), false, Some(Status::Declined)),
            (Some(Status::Ringing), local(RemoteHangup), false, Some(Status::Missed)),
            (Some(Status::Joined), Happened::Remote(Outcome::Deleted), false, Some(Status::Deleted)),
            (Some(Status::Ringing), Happened::Remote(Outcome::NotAccepted), false, None),
        ];
        for (before, happened, is_outgoing, after) in cases {
            assert_eq!(
                group_status(before, happened, is_outgoing),
                after,
                "transitionGroupCallStatus: {before:?}, outgoing {is_outgoing}"
            );
        }
    }

    #[test]
    fn transition_times() {
        let held = |status| Call { status, ..missed(ALICE_CHAT, (1, Kind::Audio), 100) };
        let accepted = Happened::Remote(Outcome::Accepted);
        assert_eq!(transition_time(None, accepted, 50), 50, "nothing held: the event's");
        assert_eq!(transition_time(Some(&held(Status::Missed)), Happened::Local(Course::Missed), 50), 100, "the later");
        assert_eq!(transition_time(Some(&held(Status::Deleted)), accepted, 200), 100, "a deleted call keeps its own");
        assert_eq!(
            transition_time(Some(&held(Status::Accepted)), Happened::Local(Course::Hangup), 200),
            100,
            "an answered call keeps its own"
        );
        assert_eq!(transition_time(Some(&held(Status::Accepted)), accepted, 200), 200, "a remote word that agrees");
        let not_accepted = Happened::Remote(Outcome::NotAccepted);
        assert_eq!(
            transition_time(Some(&held(Status::Declined)), not_accepted, 200),
            200,
            "a declined call and a remote word that agrees"
        );
        assert_eq!(transition_time(Some(&held(Status::Declined)), accepted, 200), 100, "and one that does not");
    }

    #[test]
    fn unseen_calls() {
        assert!(
            is_unseen(Kind::Audio, false, Status::Missed) && is_unseen(Kind::Video, false, Status::Pending),
            "one that came in missed or rings"
        );
        assert!(!is_unseen(Kind::Audio, true, Status::Missed), "the owner's own");
        assert!(
            !is_unseen(Kind::Audio, false, Status::Declined) && !is_unseen(Kind::Audio, false, Status::Accepted),
            "one the owner declined or answered"
        );
        let group =
            [Status::Ringing, Status::Generic, Status::Missed].map(|status| is_unseen(Kind::Group, false, status));
        assert_eq!(group, [true; 3], "a group's that rings, was seen begun, or was missed");
        assert!(!is_unseen(Kind::Group, false, Status::Joined), "a group's the owner joined");
        let mut device = Device::unkeyed();
        let note =
            Note { peer: ALICE_CHAT, call: 1, kind: Kind::Audio, is_outgoing: false, ringer: Some(ALICE), at: 1 };
        device.log(&note, Course::Ringing);
        assert!(!device.calls()[0].is_seen, "ringing");
        device.log(&note, Course::Declined);
        device.log(&note, Course::Missed);
        assert_eq!(
            (device.calls()[0].status, device.calls()[0].is_seen),
            (Status::Missed, true),
            "seen once, seen for good (`maxSeenStatus`)"
        );
    }

    #[test]
    fn course_syncs() {
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let own = device.account.aci();
        let note = Note { peer: ALICE_CHAT, call: 5, kind: Kind::Audio, is_outgoing: true, ringer: None, at: NOW };
        assert!(device.take_course(&crypto, (&note, Course::Ringing), NOW).is_none(), "pending is not synced");
        let order = device.take_course(&crypto, (&note, Course::Accepted), NOW + 1).expect("accepted is");
        let logged = Logged {
            peer: ALICE_CHAT,
            call: 5,
            timestamp: NOW,
            kind: Kind::Audio,
            is_outgoing: true,
            outcome: Outcome::Accepted,
        };
        assert_eq!(
            content::parse_content(&order.content, order.timestamp, true),
            Some(Content::Logged(logged)),
            "the call by its conversation, id, time, kind and direction"
        );
        assert_eq!(
            (order.recipients.clone(), order.is_urgent, order.call.is_none()),
            (vec![own], false, true),
            "to the account, not urgent"
        );
        assert!(device.take_course(&crypto, (&note, Course::Hangup), NOW + 2).is_none(), "it stays accepted");
        let declined = Note { call: 6, ..note };
        device.take_course(&crypto, (&declined, Course::Ringing), NOW);
        let order = device.take_course(&crypto, (&declined, Course::RemoteHangup), NOW).expect("declined is synced");
        assert!(
            matches!(
                content::parse_content(&order.content, order.timestamp, true),
                Some(Content::Logged(Logged { outcome: Outcome::NotAccepted, .. }))
            ),
            "as not accepted"
        );
        let missed = Note { call: 7, ..note };
        device.take_course(&crypto, (&missed, Course::Ringing), NOW);
        assert!(device.take_course(&crypto, (&missed, Course::Hangup), NOW).is_none(), "missed is not synced");
        let group = Note { peer: Peer::Group([1; 32]), kind: Kind::Group, call: 8, ..note };
        assert!(
            device.take_course(&crypto, (&group, Course::Ringing), NOW).is_some(),
            "an outgoing ring is, as accepted"
        );
    }

    #[test]
    fn syncs_of_other_devices() {
        let mut device = Device::unkeyed();
        let logged = Logged {
            peer: ALICE_CHAT,
            call: 1,
            timestamp: 900,
            kind: Kind::Video,
            is_outgoing: false,
            outcome: Outcome::Accepted,
        };
        assert!(!device.take_logged(&logged) && device.calls().is_empty(), "of a conversation not known");
        device.add_named_contact(ALICE, "Alice", false);
        assert!(device.take_logged(&logged), "accepted on another device");
        assert_eq!((device.calls()[0].status, device.calls()[0].at), (Status::Accepted, 900), "at the time it tells");
        let outgoing = Logged { is_outgoing: true, outcome: Outcome::Deleted, ..logged };
        assert!(!device.take_logged(&outgoing), "a direction that disagrees with the entry's is refused");
        assert!(device.take_logged(&Logged { outcome: Outcome::Deleted, ..logged }), "deleted there");
        let group = Logged { peer: Peer::Group([1; 32]), kind: Kind::Group, ..logged };
        assert!(!device.take_logged(&group), "a group not known");
    }

    // Three missed calls: Alice's at 1, the group's at 2, Alice's at 3.
    fn three_calls(device: &mut Device) -> Peer {
        let group = with_group(device);
        for (peer, call, kind, at) in
            [(ALICE_CHAT, 1, Kind::Audio, 1), (group, 2, Kind::Group, 2), (ALICE_CHAT, 3, Kind::Audio, 3)]
        {
            device.log(&Note { peer, call, kind, is_outgoing: false, ringer: Some(ALICE), at }, Course::Missed);
        }
        group
    }

    #[test]
    fn log_changes_of_other_devices() {
        let mut device = with_alice(true);
        let group = three_calls(&mut device);
        let seen = |device: &Device| device.calls().iter().map(|call| call.is_seen).collect::<Vec<_>>();
        let change = |kind, timestamp, target| LogChange { kind, timestamp, target };
        assert!(
            device.take_log_change(&change(MARKED_AS_READ_IN_CONVERSATION, 0, Some((ALICE_CHAT, 3))), NOW),
            "a call named"
        );
        assert_eq!(seen(&device), [true, false, true], "read up to it in its conversation");
        assert!(
            device.take_log_change(&change(MARKED_AS_READ, 2, None), NOW),
            "up to the newest no later than its time"
        );
        assert_eq!(seen(&device), [true, true, true], "in every conversation");
        assert!(!device.take_log_change(&change(MARKED_AS_READ, 2, None), NOW), "once");
        assert!(!device.take_log_change(&change(3, 3, None), NOW), "clearing a conversation is not done");
        assert!(!device.take_log_change(&change(CLEAR, 3, Some((group, 9))), NOW), "a call not held");
        assert!(device.take_log_change(&change(CLEAR, 0, Some((group, 2))), NOW), "a call held");
        let statuses: Vec<(Status, u64)> = device.calls().iter().map(|call| (call.status, call.at)).collect();
        assert_eq!(
            statuses,
            [(Status::Deleted, NOW), (Status::Deleted, NOW), (Status::Missed, 3)],
            "cleared up to its time (`clearCallHistory`)"
        );
        device.calls[2].is_seen = false;
        assert!(
            !device.take_log_change(&change(MARKED_AS_READ, 0, Some((ALICE_CHAT, 1))), NOW),
            "read up to a deleted call: nothing"
        );
        assert!(device.take_log_change(&change(CLEAR, 0, Some((ALICE_CHAT, 1))), NOW + 1), "cleared again");
        assert_eq!(device.calls()[1].at, NOW + 1, "a deleted call stamped anew, as Signal-Desktop stamps it");
    }

    #[test]
    fn log_opened() {
        let crypto = Standin::default();
        let mut device = with_alice(true);
        assert!(device.see_calls(&crypto, NOW).is_none(), "no call: nothing to tell");
        three_calls(&mut device);
        let order = device.see_calls(&crypto, NOW).expect("the account told");
        assert!(device.calls().iter().all(|call| call.is_seen), "every call seen");
        assert_eq!(
            content::parse_content(&order.content, order.timestamp, true),
            Some(Content::LogChange(LogChange { kind: MARKED_AS_READ, timestamp: 3, target: Some((ALICE_CHAT, 3)) })),
            "read up to the newest (`markCallsTabViewed`)"
        );
        assert_eq!(
            (order.recipients.clone(), order.is_urgent),
            (vec![device.account.aci()], false),
            "to the account, not urgent"
        );
    }

    #[test]
    fn group_call_updates() {
        let crypto = Standin::default();
        let mut device = with_alice(false);
        let group = with_group(&mut device);
        let context = |revision| Context { master_key: [8; 32], revision };
        let update = |era: Option<&str>, timestamp| Content::GroupCall {
            group: context(4),
            era: era.map(Into::into),
            timestamp,
        };
        let from = |service| Address { service, device: 1 };
        let alice = from(ALICE);
        assert_eq!(device.apply(&crypto, update(Some("1122334455667788"), 1_000), &alice, 2_000), Some(Event::Logged));
        let begun = Call {
            id: 0x1122_3344_5566_7788,
            peer: group,
            kind: Kind::Group,
            is_outgoing: false,
            ringer: None,
            status: Status::Generic,
            at: 1_000,
            is_seen: false,
        };
        assert_eq!(device.calls(), [begun], "seen begun, by its era, at the message's time");
        assert_eq!(device.group_due(&group), (Some(4), false), "the revision it names noted, as before");
        assert_eq!(
            device.apply(&crypto, update(Some("1122334455667788"), 1_500), &alice, 2_000),
            Some(Event::Changed(group)),
            "a later time kept out"
        );
        assert_eq!(
            device.apply(&crypto, update(Some("1122334455667788"), 500), &alice, 2_000),
            Some(Event::Logged),
            "an earlier one taken"
        );
        assert_eq!(device.calls()[0].at, 500, "an earlier time taken");
        let own = from(device.account.aci());
        assert_eq!(
            device.apply(&crypto, update(Some("aa"), 1_000), &own, 2_000),
            Some(Event::Changed(group)),
            "the owner's own"
        );
        assert_eq!(device.apply(&crypto, update(None, 1_000), &alice, 2_000), Some(Event::Changed(group)), "no era");
        let held = device.group_mut(&group).unwrap();
        (held.announcements_only, held.admins) = (true, vec![BORIS]);
        assert_eq!(
            device.apply(&crypto, update(Some("bb"), 1_000), &alice, 2_000),
            Some(Event::Changed(group)),
            "where only administrators send, a member's"
        );
        assert_eq!(
            device.apply(&crypto, update(Some("bb"), 1_000), &from(BORIS), 2_000),
            Some(Event::Logged),
            "its administrator's"
        );
        device.group_mut(&group).unwrap().response = 0;
        assert_eq!(
            device.apply(&crypto, update(Some("cc"), 1_000), &from(BORIS), 2_000),
            Some(Event::Changed(group)),
            "not accepted"
        );
        device.group_mut(&group).unwrap().response = ACCEPT;
        device.group_mut(&group).unwrap().terminated = true;
        assert_eq!(
            device.apply(&crypto, update(Some("cc"), 1_000), &from(BORIS), 2_000),
            Some(Event::Changed(group)),
            "ended"
        );
        assert_eq!(device.calls().len(), 2, "two entries made, one by each era");
    }

    #[test]
    fn group_call_update_delivered() {
        let crypto = Standin::default();
        let mut device = with_alice(false);
        let group = with_group(&mut device);
        let held = device.group_mut(&group).unwrap();
        (held.announcements_only, held.admins) = (true, vec![BORIS]);
        let update = |revision| {
            let data = crate::base::proto::Writer::new()
                .message(19, crate::base::proto::Writer::new().text(1, "1122334455667788"))
                .varint(7, 1_000)
                .message(15, Context::write(&[8; 32], revision));
            content::write_message(data)
        };
        let alice = Address { service: ALICE, device: 1 };
        let taken = device.receive_content(&crypto, &update(0), alice, (1_000, true, 1_000), 1_000, false);
        assert!(
            matches!(taken, Ok(Reception::Event(Event::Changed(_)))),
            "a member who is no administrator: not dropped, as a message would be, though it makes no entry"
        );
        assert!(device.owed_receipts.is_empty(), "and owed no receipt");
        let carol = Address { service: CAROL, device: 1 };
        assert_eq!(
            device.receive_content(&crypto, &update(0), carol, (1_000, true, 1_000), 1_000, false).err(),
            Some(super::super::DropReason::NotMember),
            "from no member"
        );
        assert!(
            matches!(
                device.receive_content(&crypto, &update(5), alice, (1_000, true, 1_000), 1_000, false),
                Ok(Reception::Queued)
            ),
            "a later revision than held waits for the group's state"
        );
    }

    #[test]
    fn era_ids() {
        let crypto = Exact::default();
        assert_eq!(call_of_era(&crypto, "1122334455667788"), 0x1122_3344_5566_7788, "sixteen hex digits as they stand");
        assert_eq!(call_of_era(&crypto, "0000000000000000"), u64::MAX, "nought as the ring of all ones");
        let hash = crypto.sha256(&[b"mesozoic"]);
        assert_eq!(
            call_of_era(&crypto, "mesozoic"),
            u64::from_le_bytes(hash[..8].try_into().unwrap()),
            "any other era by the first eight bytes of its SHA-256, the lowest first"
        );
        assert_ne!(call_of_era(&crypto, "112233445566778"), 0x0112_2334_4556_6778, "fifteen digits are no number");
    }

    #[test]
    fn log_bounded() {
        let mut device = Device::unkeyed();
        for call in 0..=MAX_CALLS as u64 {
            device.log(
                &Note { peer: ALICE_CHAT, call, kind: Kind::Audio, is_outgoing: true, ringer: None, at: call },
                Course::Started,
            );
        }
        assert_eq!(
            (device.calls().len(), device.calls()[0].id),
            (MAX_CALLS, 1),
            "the newest kept, the first logged going first"
        );
    }

    #[test]
    fn calls_round_trip() {
        let mut device = with_alice(true);
        let group = three_calls(&mut device);
        let own = device.account.aci();
        for (call, status) in STATUSES.into_iter().enumerate() {
            let (id, is_seen) = (10 + call as u64, call % 2 == 0);
            let ringer = Some(own);
            device.calls.push(Call {
                id,
                peer: group,
                kind: Kind::Group,
                is_outgoing: true,
                ringer,
                status,
                at: 4,
                is_seen,
            });
        }
        assert_eq!(read(&write(&device.calls)), device.calls, "every status, a ringer or none, seen or not");
        let (directory, name) = File::Calls.to_path();
        assert_eq!(
            (directory, name.as_str(), File::from_path(directory, &name)),
            (super::super::DIRECTORY, "calls", Some(File::Calls)),
            "the file `calls` beside the account"
        );
        let files = [File::Account, File::Calls]
            .into_iter()
            .map(|file| (file.clone(), device.file_bytes(&file).unwrap()))
            .collect();
        assert_eq!(Device::read(&files).unwrap().calls, device.calls, "read back with the device");
        assert_eq!(read(b"\xFF"), [], "a file that does not read holds none");
    }

    #[test]
    fn signal_orders_by_target() {
        let mut device = with_alice(true);
        let own = device.account.aci();
        let group = with_group(&mut device);
        let outbound = |target, signal| Outbound { target, device: None, signal };
        let orders = device.signal_orders(outbound(Target::Contact(ALICE), offer(1)), NOW);
        let [profile, call] = &orders[..] else { panic!("the profile key, then the offer") };
        assert!(
            device.is_sharing(&ALICE_CHAT)
                && Message::parse(&profile.content).unwrap().message(1).unwrap().varint(4) == Some(4),
            "the profile shared and its key sent first (`startOutgoingDirectCall`)"
        );
        assert_eq!(
            (call.peer, call.is_addressed, call.is_urgent, call.hint, call.is_logged, call.is_group_send),
            (ALICE_CHAT, false, true, sealed::DEFAULT, false, false),
            "found as a message's recipients, urgent as it rings, of no content hint, in no send log"
        );
        assert!(call.timestamp > profile.timestamp, "each under a timestamp of its own");
        let candidates = Signal::Candidates { call: 1, candidates: vec![] };
        let [order] = &device.signal_orders(outbound(Target::Contact(ALICE), candidates), NOW)[..] else {
            panic!("one")
        };
        assert!(!order.is_urgent && order.call.is_some(), "a candidate rings nothing");
        let ring = Signal::Group { signal: GroupSignal::Ring { group, ring: 1, is_cancelled: false }, is_urgent: true };
        let [order] = &device.signal_orders(outbound(Target::Group(group), ring), NOW)[..] else { panic!("one") };
        assert_eq!(
            (order.is_addressed, order.is_group_send, order.endorsing_group, order.is_urgent, order.uses_sender_key()),
            (false, true, Some(group), true, true),
            "to the group's members as its messages go, under the sender key where it may"
        );
        let key = Signal::Group { signal: GroupSignal::Leaving { group, demux_id: 16 }, is_urgent: false };
        let [order] = &device.signal_orders(outbound(Target::Members(group, vec![BORIS, CAROL]), key.clone()), NOW)[..]
        else {
            panic!("one")
        };
        assert_eq!(
            (order.recipients.clone(), order.is_addressed),
            (vec![BORIS], true),
            "the members named that are members"
        );
        let [order] = &device.signal_orders(Outbound { target: Target::Own, device: Some(1), signal: key }, NOW)[..]
        else {
            panic!("one")
        };
        assert_eq!(
            (order.peer, order.recipients.clone(), order.is_group_send),
            (Peer::Contact(own), vec![own], false),
            "to the account's other devices"
        );
        assert_eq!(
            call::read(&Message::parse(&order.content).unwrap()).map(|(_, to)| to),
            Some(Some(1)),
            "for its one device"
        );
    }

    #[test]
    fn members_as_the_server_names() {
        let crypto = Exact::default();
        let mut device = with_alice(false);
        device.add_named_contact(ALICE, "Alice", false);
        let own = device.account.aci();
        let group = device.add_group(&crypto, &[8; 32], &[own, ALICE]);
        let membership = device.membership(&crypto, &group, "2:user:group".into()).expect("of a group known");
        assert_eq!((membership.token.as_str(), membership.own), ("2:user:group", own));
        let key = group::Keys::new(&crypto, &[8; 32]).uid;
        let named: Vec<(ServiceId, Option<&str>)> =
            membership.members.iter().map(|member| (member.service, member.name.as_deref())).collect();
        assert_eq!(named[1], (ALICE, Some("Alice")), "each member with the name the list gives it");
        assert_eq!(named[0].0, own, "the owner among them, as the server lists the owner's devices too");
        for member in &membership.members {
            let ciphertext = key.encrypt_id(&crypto, &member.service);
            assert_eq!(
                (ciphertext.len(), key.decrypt_id(&crypto, &ciphertext)),
                (65, Some(member.service)),
                "the id's ciphertext under the group's key, as the group's state holds it"
            );
            assert_eq!(member.opaque, hex(&crypto.sha256(&[&ciphertext])), "named by its SHA-256 in hex");
        }
        assert_ne!(membership.members[0].opaque, membership.members[1].opaque);
        assert!(device.membership(&crypto, &Peer::Group([9; 32]), "t".into()).is_none(), "a group not known");
        assert_eq!(alloc::format!("{membership:?}"), "Membership { members: 2, .. }", "no token and no member told");
    }

    #[test]
    fn relays_kept() {
        let relay = |ttl| Relay {
            username: "u".into(),
            password: "p".into(),
            urls: vec![],
            urls_with_ips: vec![],
            hostname: None,
            ttl,
        };
        let mut device = Device::unkeyed();
        device.keep_relays(&[], NOW);
        assert_eq!(device.cached_relays(NOW), None, "none handed out");
        device.keep_relays(&[relay(Some(120)), relay(Some(60))], NOW);
        assert_eq!(device.cached_relays(NOW + 59_999).map(|relays| relays.len()), Some(2), "for the least time");
        assert_eq!(device.cached_relays(NOW + 60_000), None, "and no longer");
        device.keep_relays(&[relay(Some(120)), relay(None)], NOW);
        assert_eq!(device.cached_relays(NOW), None, "one that names no time: none kept");
    }
}

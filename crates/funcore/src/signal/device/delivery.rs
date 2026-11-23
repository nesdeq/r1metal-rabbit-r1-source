use super::calls::{Incoming, Membership};
use super::{Address, Device, File, Identity, Peer, ServiceId};
use crate::signal::call::Signal;
use crate::signal::command::Outbound;
use crate::signal::content::{self, Content, DecryptionError, Envelope};
use crate::signal::conversation::QueuedMessage;
use crate::signal::group;
use crate::signal::message::{PreKeyMessage, SignalMessage};
use crate::signal::service::Relay;
use crate::signal::session::{Error, PreKeys, State};
use crate::signal::{Crypto, sealed};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

pub(super) const MAX_GUIDS: usize = 1_000;
// As many contents are held for a message not come yet as deliveries are remembered, the oldest going first.
pub(super) const MAX_EARLY: usize = MAX_GUIDS;

/// What a delivery or a stay did, for the screens, the call layer and the log.
#[derive(Clone, PartialEq, Eq)]
pub enum Event {
    // A contact's new message, as the screens show it.
    Arrived { peer: Peer, timestamp: u64, text: String, is_noticed: bool },
    // The owner's message from another device.
    Wrote(Peer),
    // An edit, a deletion, reads elsewhere, a group's change.
    Changed(Peer),
    // A contact's identity key is not the one held before.
    SafetyChanged(Peer),
    // A call message that passed the gate, for the call layer; boxed, as it is many times the size of the rest.
    Called(Box<Incoming>),
    // The call log changed.
    Logged,
    // The relays of a one-to-one call, none where none could be had.
    Relays(Vec<Relay>),
    // What a group's call is joined with, none where its token could not be had.
    Token(Peer, Option<Box<Membership>>),
    // A call's signal that was not sent and goes no more; boxed as a call message is.
    Unsent(Box<Outbound>),
}

impl core::fmt::Debug for Event {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Event::Arrived { timestamp, is_noticed, .. } => f
                .debug_struct("Arrived")
                .field("timestamp", timestamp)
                .field("noticed", is_noticed)
                .finish_non_exhaustive(),
            Event::Wrote(_) => f.write_str("Wrote"),
            Event::Changed(_) => f.write_str("Changed"),
            Event::SafetyChanged(_) => f.write_str("SafetyChanged"),
            Event::Called(incoming) => f.debug_tuple("Called").field(incoming).finish(),
            Event::Logged => f.write_str("Logged"),
            Event::Relays(relays) => f.debug_tuple("Relays").field(&relays.len()).finish(),
            Event::Token(_, token) => f.debug_struct("Token").field("given", &token.is_some()).finish_non_exhaustive(),
            Event::Unsent(outbound) => f.debug_tuple("Unsent").field(&outbound.target).finish_non_exhaustive(),
        }
    }
}

/// Why a delivery was dropped; it is answered all the same, or it would come again.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    Malformed,
    Duplicate,
    // Not for an identity of the account's.
    WrongDestination,
    // A kind the R1 does not take: a server's delivery receipt.
    Kind,
    Sealed(sealed::Error),
    // From the R1 itself.
    OwnDevice,
    // Unsealed from a phone number's identity, or to it other than a PreKey message, as Signal-Desktop drops them.
    Pni,
    Session(Error),
    // A message that did not open, with what its sending device is asked to send it again by.
    Unopened { sender: Address, timestamp: u64, ratchet: Option<[u8; 32]>, group: Option<[u8; 32]>, reason: Error },
    // The pre-key it names is not held, or a last-resort key is used again with the same base key.
    PreKey,
    // Plaintext content that asks nothing.
    Plaintext,
    // From an account the owner blocked, or to a group the owner blocked.
    Blocked,
    // To a group the owner or its sender is no member of, or where only administrators may send and its sender is none.
    NotMember,
    // To a group that was ended.
    Ended,
    // It held nothing the conversations show.
    Nothing,
}

impl core::fmt::Debug for DropReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DropReason::Malformed => f.write_str("Malformed"),
            DropReason::Duplicate => f.write_str("Twice"),
            DropReason::WrongDestination => f.write_str("NotOurs"),
            DropReason::Kind => f.write_str("Kind"),
            DropReason::Sealed(reason) => f.debug_tuple("Sealed").field(reason).finish(),
            DropReason::OwnDevice => f.write_str("Ourselves"),
            DropReason::Pni => f.write_str("Pni"),
            DropReason::Session(reason) => f.debug_tuple("Session").field(reason).finish(),
            DropReason::Unopened { sender, timestamp, reason, .. } => f
                .debug_struct("Unopened")
                .field("from", sender)
                .field("timestamp", timestamp)
                .field("why", reason)
                .finish_non_exhaustive(),
            DropReason::PreKey => f.write_str("PreKey"),
            DropReason::Plaintext => f.write_str("Plaintext"),
            DropReason::Blocked => f.write_str("Blocked"),
            DropReason::NotMember => f.write_str("NotMember"),
            DropReason::Ended => f.write_str("Ended"),
            DropReason::Nothing => f.write_str("Nothing"),
        }
    }
}

// How long the service held a message, ms: the time it delivered it at less the time it took it at, nought where either is not told or the first is not the later (Signal-Desktop's `calculateMessageAge`).
fn age_of(server: u64, delivered: Option<u64>) -> u64 {
    delivered.filter(|_| server > 0).map_or(0, |delivered| delivered.saturating_sub(server))
}

pub(super) enum Reception {
    Event(Event),
    Quiet,
    Queued,
    // A call message's signals and the device they are for, which the delivery gates.
    Call(Vec<Signal>, Option<u32>),
}

pub(in crate::signal) struct Delivery {
    pub(in crate::signal) message_id: Option<(Address, u64)>,
    pub(in crate::signal) result: Result<Effects, DropReason>,
}

#[derive(Default)]
pub(in crate::signal) struct Effects {
    pub(in crate::signal) events: Vec<Event>,
    pub(in crate::signal) resend: Option<(Address, DecryptionError, Option<[u8; 32]>)>,
}

impl Device {
    // Takes a delivery at `now` by the wall clock, which the service delivered at `delivered` where it tells it.
    pub(in crate::signal) fn deliver(
        &mut self,
        crypto: &impl Crypto,
        bytes: &[u8],
        (now, delivered): (u64, Option<u64>),
    ) -> Delivery {
        let mut message_id = None;
        let result = self.open_delivery(crypto, bytes, (now, delivered), &mut message_id);
        Delivery { message_id, result }
    }

    fn open_delivery(
        &mut self,
        crypto: &impl Crypto,
        bytes: &[u8],
        (now, delivered): (u64, Option<u64>),
        message_id: &mut Option<(Address, u64)>,
    ) -> Result<Effects, DropReason> {
        let envelope = Envelope::read(bytes).ok_or(DropReason::Malformed)?;
        if !envelope.guid.is_empty() {
            if self.guids.contains(&envelope.guid) {
                return Err(DropReason::Duplicate);
            }
            self.guids.push_back(envelope.guid);
            if self.guids.len() > MAX_GUIDS {
                self.guids.pop_front();
            }
            self.dirty.insert(File::Seen);
        }
        let destination = envelope.destination.unwrap_or(self.account.aci());
        let identity = self.account.identity_of(&destination).ok_or(DropReason::WrongDestination)?;
        let (sender, kind, inner, sealed_identity, group) = match envelope.kind {
            content::UNIDENTIFIED_SENDER => {
                if identity != Identity::Aci {
                    return Err(DropReason::WrongDestination);
                }
                let contents =
                    sealed::open(crypto, &self.account.aci_identity, &envelope.content, envelope.server_timestamp)
                        .map_err(DropReason::Sealed)?;
                let kind = sealed::envelope_kind(u32::from(contents.kind)).ok_or(DropReason::Kind)?;
                let sender = Address { service: contents.sender, device: contents.device };
                (sender, kind, contents.content, Some(contents.identity), contents.group)
            }
            content::PREKEY_MESSAGE | content::DOUBLE_RATCHET | content::PLAINTEXT => {
                let source = envelope.source.ok_or(DropReason::Malformed)?;
                if matches!(source, ServiceId::Pni(_)) {
                    return Err(DropReason::Pni);
                }
                let sender = Address { service: source, device: envelope.source_device };
                (sender, envelope.kind, envelope.content, None, None)
            }
            _ => return Err(DropReason::Kind),
        };
        if sender.service == self.account.aci() && sender.device == self.account.device {
            return Err(DropReason::OwnDevice);
        }
        if identity == Identity::Pni && kind != content::PREKEY_MESSAGE {
            return Err(DropReason::Pni);
        }
        let local = self.account.address(identity).ok_or(DropReason::WrongDestination)?;
        let is_blocked = self.blocklist.acis.contains(&sender.service);
        if kind == content::PLAINTEXT {
            *message_id = Some((sender, envelope.timestamp));
            let error = content::parse_decryption_error(&inner).ok_or(DropReason::Plaintext)?;
            if is_blocked {
                return Err(DropReason::Blocked);
            }
            return Ok(Effects { events: Vec::new(), resend: Some((sender, error, group)) });
        }
        // A message that does not open is asked again of its device, as Signal-Desktop asks it (`requestResend`), but for one taken before, one from a blocked account, one to the phone number's identity, and one whose ratchet key cannot be read, for which libsignal makes no request.
        let to_unopened = |reason: DropReason| {
            let error = match reason {
                DropReason::Session(Error::Duplicate) => return reason,
                DropReason::Session(error) => error,
                DropReason::PreKey => Error::BadKey,
                _ => return reason,
            };
            let ratchet = match kind {
                content::SENDER_KEY => None,
                content::PREKEY_MESSAGE => {
                    match PreKeyMessage::read(&inner).and_then(|prekey| SignalMessage::read(&prekey.message)) {
                        Some(message) => Some(message.ratchet_key),
                        None => return DropReason::Malformed,
                    }
                }
                _ => match SignalMessage::read(&inner) {
                    Some(message) => Some(message.ratchet_key),
                    None => return DropReason::Malformed,
                },
            };
            if identity == Identity::Pni || is_blocked {
                return DropReason::Session(error);
            }
            DropReason::Unopened { sender, timestamp: envelope.timestamp, ratchet, group, reason: error }
        };
        let is_own = self.is_own_id(&sender.service);
        let (plain, remote_identity) = match kind {
            content::PREKEY_MESSAGE => {
                let (plain, identity) =
                    self.open_prekey(crypto, identity, &sender, &local, &inner).map_err(to_unopened)?;
                (plain, Some(identity))
            }
            content::SENDER_KEY => (
                self.open_by_sender_key(crypto, &sender, &inner).map_err(DropReason::Session).map_err(to_unopened)?,
                None,
            ),
            _ => {
                let record =
                    self.sessions.get_mut(&sender).ok_or(DropReason::Session(Error::NoSession)).map_err(to_unopened)?;
                // libsignal keeps what opened only once the session's identity key is trusted (`message_decrypt_signal`), which only a device of the account's own may fail.
                let previous = is_own.then(|| record.clone());
                let plain =
                    record.open(crypto, &inner, &sender, &local).map_err(DropReason::Session).map_err(to_unopened)?;
                let remote = record.current.as_ref().map(|state| state.remote_identity);
                let remote = remote.ok_or(DropReason::Session(Error::NoSession))?;
                if !self.is_trusted_receiving(&sender.service, &remote) {
                    self.sessions.extend(previous.map(|record| (sender, record)));
                    return Err(to_unopened(DropReason::Session(Error::Untrusted)));
                }
                self.dirty.insert(File::Session(sender));
                (plain, Some(remote))
            }
        };
        *message_id = Some((sender, envelope.timestamp));
        let mut events = Vec::new();
        if let Some(key) = remote_identity
            && self.save_identity(sender.service, key, Some(sender.device), now)
        {
            events.push(Event::SafetyChanged(Peer::Contact(sender.service)));
        }
        let plain = content::unpad(&plain).ok_or(DropReason::Malformed)?;
        // A new number of the account's own is taken before anything else, as the deliveries after it need its keys (`handlePNIChangeNumber`).
        if sender.service == self.account.aci()
            && let Some(change) = content::parse_pni_change(plain)
        {
            self.take_pni_change(crypto, change, envelope.updated_pni, now);
            return Ok(Effects { events, resend: None });
        }
        if let Some((pni, signature)) = content::parse_pni_signature(plain)
            && self.take_pni_signature(crypto, sender.service, pni, &signature)
        {
            events.push(Event::Changed(Peer::Contact(sender.service)));
        }
        let has_distribution = content::parse_distribution(plain)
            .is_some_and(|distribution| self.take_distribution(&sender, &distribution));
        let meta = (envelope.timestamp, sealed_identity.is_some(), envelope.server_timestamp);
        let taken_at = self.entry_time(now);
        match self.receive_content(crypto, plain, sender, meta, taken_at, false)? {
            Reception::Event(event) => events.push(event),
            Reception::Call(signals, device) => {
                let age = age_of(envelope.server_timestamp, delivered);
                let facts = (envelope.timestamp, age, identity == Identity::Pni);
                events.extend(self.gate_call(sender, (signals, device), facts, now));
            }
            Reception::Quiet if events.is_empty() && !has_distribution => return Err(DropReason::Nothing),
            Reception::Quiet | Reception::Queued => {}
        }
        Ok(Effects { events, resend: None })
    }

    pub(super) fn receive_content(
        &mut self,
        crypto: &impl Crypto,
        plain: &[u8],
        sender: Address,
        (timestamp, is_sealed, server): (u64, bool, u64),
        now: u64,
        is_requeued: bool,
    ) -> Result<Reception, DropReason> {
        if self.blocklist.acis.contains(&sender.service) {
            return Err(DropReason::Blocked);
        }
        let account = self.account.aci();
        let content =
            content::parse_content(plain, timestamp, sender.service == account).ok_or(DropReason::Malformed)?;
        // A call message is gated by its delivery, and nothing of it is kept or held (`handleCallingMessage`).
        if let Content::Call { signals, device } = content {
            return Ok(Reception::Call(signals, device));
        }
        // A group call's update is held to its group's membership and end, but sent no receipt and dropped by no announcements' rule (`handleDataMessage`).
        let is_call_update = matches!(content, Content::GroupCall { .. });
        if sender.service != account {
            let is_to_blocked = content::parse_group_context(plain)
                .is_some_and(|context| self.blocklist.groups.contains(&group::id_of(crypto, &context.master_key)));
            if is_to_blocked {
                if let Content::Text(content::Text { profile_key: Some(key), .. }) = &content {
                    self.take_profile_key(sender.service, *key);
                }
                return Err(DropReason::Blocked);
            }
            // A contact the owner trusts whose message came unsealed is owed the owner's profile key (`onMessageReceived`).
            if !is_sealed && content::is_dispatched(plain) && self.is_trusted_contact(&sender.service) {
                self.owed_profile_keys.insert(sender.service);
            }
            if let Some(context) = content::parse_handled_group(plain) {
                self.note_registered(sender.service);
                let peer = match &context {
                    Some(context) => {
                        let peer = self.ensure_group(crypto, &context.master_key);
                        let is_behind = self.group(&peer).is_some_and(|group| group.is_behind(context.revision));
                        if !is_requeued && is_behind {
                            let message =
                                QueuedMessage { content: plain.to_vec(), timestamp, sender, is_sealed, at: now };
                            let conversation = self.conversation_mut(peer);
                            if let Some(group) = &mut conversation.group {
                                group.note_revision(context.revision);
                            }
                            conversation.queue.push(message);
                            return Ok(Reception::Queued);
                        }
                        let members = self.members(&peer);
                        if !members.contains(&account) || !members.contains(&sender.service) {
                            return Err(DropReason::NotMember);
                        }
                        let is_admin_only = self
                            .group(&peer)
                            .is_some_and(|group| group.announcements_only && !group.admins.contains(&sender.service));
                        if !is_call_update && is_admin_only {
                            return Err(DropReason::NotMember);
                        }
                        peer
                    }
                    None => Peer::Contact(sender.service),
                };
                let author = context.as_ref().map(|_| sender.service);
                let has_message = self
                    .conversations
                    .get(&peer)
                    .is_some_and(|conversation| conversation.has_message(author, timestamp));
                if is_sealed && !has_message && !is_call_update && self.is_accepted(&peer, true) {
                    self.owed_receipts.push((sender.service, peer, timestamp));
                }
                if self.group(&peer).is_some_and(|group| group.terminated) {
                    return Err(DropReason::Ended);
                }
            }
        }
        match self.apply_content(crypto, content, &sender, (server, now)) {
            Ok(Some(event @ (Event::Arrived { .. } | Event::Wrote(_)))) => {
                Ok(Reception::Event(self.finish_new_message(crypto, event, &sender, now)))
            }
            Ok(event) => Ok(event.map_or(Reception::Quiet, Reception::Event)),
            Err(missing) => {
                let content = missing.rest.unwrap_or_else(|| plain.to_vec());
                self.early_contents.push(super::EarlyContent { content, sender, timestamp, server, at: now });
                let excess = self.early_contents.len().saturating_sub(MAX_EARLY);
                self.early_contents.drain(..excess);
                self.dirty.insert(File::Seen);
                Ok(missing.event.map_or(Reception::Quiet, Reception::Event))
            }
        }
    }

    fn finish_new_message(&mut self, crypto: &impl Crypto, event: Event, sender: &Address, now: u64) -> Event {
        self.retry_early(crypto, now);
        let Event::Arrived { peer, timestamp, is_noticed, .. } = event else { return event };
        let author = matches!(peer, Peer::Group(_)).then_some(sender.service);
        match self.conversations.get(&peer).and_then(|conversation| conversation.entry(timestamp, (false, author))) {
            Some(entry) if entry.deleter.is_none() => {
                Event::Arrived { peer, timestamp, text: self.resolved_body(entry), is_noticed }
            }
            _ => Event::Changed(peer),
        }
    }

    pub(super) fn open_prekey(
        &mut self,
        crypto: &impl Crypto,
        identity: Identity,
        sender: &Address,
        local: &Address,
        bytes: &[u8],
    ) -> Result<(Vec<u8>, [u8; 32]), DropReason> {
        let prekey = PreKeyMessage::read(bytes).ok_or(DropReason::Malformed)?;
        if !self.is_trusted_receiving(&sender.service, &prekey.identity) {
            return Err(DropReason::Session(Error::Untrusted));
        }
        let mut record = self.sessions.get(sender).cloned().unwrap_or_default();
        let mut used_keys: Option<(Option<u32>, u32, bool)> = None;
        if !record.promote_prekey_state(&prekey).map_err(DropReason::Session)? {
            let ring = self.rings.get(&identity).ok_or(DropReason::PreKey)?;
            let signed = ring.signed_key(prekey.signed_pre_key).ok_or(DropReason::PreKey)?;
            let (kyber, one_time) = ring.kyber_key(prekey.kyber_pre_key).ok_or(DropReason::PreKey)?;
            if !one_time && ring.uses.contains(&(kyber.id, signed.id, prekey.base_key)) {
                return Err(DropReason::PreKey);
            }
            let pre_key = match prekey.pre_key {
                Some(id) => Some(&ring.one_time_key(id).ok_or(DropReason::PreKey)?.pair),
                None => None,
            };
            let pair = self.account.identity(identity).ok_or(DropReason::WrongDestination)?;
            let registration = match identity {
                Identity::Aci => self.account.registration,
                Identity::Pni => self.account.pni_registration,
            };
            // libsignal's `is_same_account`: the sender is the very identity the message came to, its key and its id.
            let is_own = sender.service == local.service && prekey.identity == pair.public;
            let keys = PreKeys { signed_pre_key: &signed.pair, kyber_pre_key: &kyber.private, pre_key };
            let state =
                State::accept(crypto, pair, registration, &keys, &prekey, is_own).map_err(DropReason::Session)?;
            record.promote(state);
            used_keys = Some((prekey.pre_key, kyber.id, one_time));
        }
        let plain = record.open_prekey(crypto, &prekey, sender, local).map_err(DropReason::Session)?;
        if let (Some((pre_key, kyber, one_time)), Some(ring)) = (used_keys, self.rings.get_mut(&identity)) {
            if let Some(pre_key) = pre_key {
                ring.one_time.retain(|key| key.id != pre_key);
            }
            if one_time {
                ring.kyber.retain(|key| key.id != kyber);
            } else {
                ring.uses.push((kyber, prekey.signed_pre_key, prekey.base_key));
            }
            self.dirty.insert(File::Ring(identity));
        }
        self.sessions.insert(*sender, record);
        self.dirty.insert(File::Session(*sender));
        Ok((plain, prekey.identity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_age() {
        assert_eq!(age_of(1_000, Some(62_000)), 61_000, "the service's time of delivery less the time it took it");
        assert_eq!(
            [age_of(1_000, None), age_of(0, Some(62_000)), age_of(62_000, Some(1_000))],
            [0; 3],
            "nought where a time is not told, or the delivery is not the later (`calculateMessageAge`)"
        );
    }
}

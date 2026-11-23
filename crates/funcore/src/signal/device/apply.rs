use super::{Address, Device, Event, File, Peer, ServiceId};
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::Crypto;
use crate::signal::account::derive_master_key;
use crate::signal::content::{self, Content, DeleteForMe, Identifier};
use crate::signal::conversation::{Deleter, Entry, Status};
use crate::signal::group::Context;
use crate::signal::keys::QUEUE_TIME;
use alloc::vec::Vec;

// How long after its message a deletion for everyone is taken, and how much longer for its way (Signal-Desktop's `DEFAULT_DELETE_MAX_AGE_MS` and `MESSAGE_SEND_GRACE_PERIOD`).
pub(super) const DELETE_WINDOW: u64 = 24 * 60 * 60 * 1000;
pub(super) const DELETE_GRACE: u64 = 24 * 60 * 60 * 1000;
const EARLY_AGE: u64 = QUEUE_TIME;

#[derive(Default)]
pub(super) struct MissingTarget {
    pub(super) event: Option<Event>,
    pub(super) rest: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct EarlyContent {
    pub(super) content: Vec<u8>,
    pub(super) sender: Address,
    pub(super) timestamp: u64,
    pub(super) server: u64,
    pub(super) at: u64,
}

impl core::fmt::Debug for EarlyContent {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Early").field("timestamp", &self.timestamp).finish_non_exhaustive()
    }
}

impl EarlyContent {
    pub(super) fn write(&self) -> Writer {
        Writer::new()
            .bytes(1, &self.content)
            .bytes(2, &self.sender.service.to_binary())
            .varint(3, u64::from(self.sender.device))
            .varint(4, self.timestamp)
            .varint(5, self.server)
            .varint(6, self.at)
    }

    pub(super) fn read(message: &Message) -> Option<EarlyContent> {
        Some(EarlyContent {
            content: message.bytes(1)?.to_vec(),
            sender: Address { service: ServiceId::from_binary(message.bytes(2)?)?, device: message.u32(3)? },
            timestamp: message.varint(4)?,
            server: message.varint(5).unwrap_or(0),
            at: message.varint(6)?,
        })
    }
}

impl Device {
    pub(super) fn retry_early(&mut self, crypto: &impl Crypto, now: u64) {
        for mut early in core::mem::take(&mut self.early_contents) {
            let content =
                content::parse_content(&early.content, early.timestamp, early.sender.service == self.account.aci());
            let result = content
                .filter(|_| early.at + EARLY_AGE > now)
                .map(|content| self.apply_content(crypto, content, &early.sender, (early.server, now)));
            match result {
                Some(Err(missing)) => {
                    if let Some(rest) = missing.rest {
                        early.content = rest;
                        self.dirty.insert(File::Seen);
                    }
                    self.early_contents.push(early);
                }
                Some(Ok(_)) | None => {
                    self.dirty.insert(File::Seen);
                }
            }
        }
    }

    pub(super) fn apply_content(
        &mut self,
        crypto: &impl Crypto,
        content: Content,
        sender: &Address,
        (server, now): (u64, u64),
    ) -> Result<Option<Event>, MissingTarget> {
        let service = sender.service;
        let own = self.account.aci();
        // Where the service's time is not known, as for a message that waited for its group's state, it is when the R1 took it in.
        let server = if server == 0 { now } else { server };
        // The conversation a contact's message is in, and who of it wrote the message.
        let peer_and_author = |device: &mut Device, group: Option<&Context>| match group {
            Some(context) => (device.note_group(crypto, context), Some(service)),
            None => (Peer::Contact(service), None),
        };
        // A contact's conversation takes the timer every message to it carries; a group's timer is the group's state's, as Signal-Desktop has it (`handleDataMessage`).
        let take_timer = |device: &mut Device, peer: Peer, text: &content::Text| {
            matches!(peer, Peer::Contact(_)) && device.conversation_mut(peer).take_timer(text)
        };
        // An edit names a message the R1 may not hold yet, and is then held for it.
        let to_edit_result = |is_changed: bool, peer: Peer| {
            if is_changed { Ok(Some(Event::Changed(peer))) } else { Err(MissingTarget::default()) }
        };
        Ok(match content {
            Content::Text(text) => {
                if let Some(key) = text.profile_key {
                    self.take_profile_key(service, key);
                }
                let (peer, author) = peer_and_author(self, text.group.as_ref());
                take_timer(self, peer, &text);
                let is_noticed = self.is_noticed(&peer, &text, now);
                let entry = Entry::incoming(&text, now, author);
                let (timestamp, body) = (text.timestamp, self.resolved_body(&entry));
                self.conversation_mut(peer).add(entry).then_some(Event::Arrived {
                    peer,
                    timestamp,
                    text: body,
                    is_noticed,
                })
            }
            Content::Sent { to, text, started } => {
                let Some(peer) = self.peer_of_sent(crypto, to, text.group.as_ref()) else { return Ok(None) };
                // The owner's message carries the profile key where the owner shares it there (`handleDataMessage`).
                if text.profile_key.is_some() && peer != Peer::Contact(own) {
                    self.set_sharing(peer, true);
                }
                take_timer(self, peer, &text);
                let entry =
                    Entry { started: Some(started.unwrap_or(now)), ..Entry::outgoing(&text, now, Status::Sent) };
                self.conversation_mut(peer).add(entry).then_some(Event::Wrote(peer))
            }
            Content::SentProfileKey { to, group, key } => {
                // The owner shares the profile there, and the key is the owner's newest (`handleMessageSentProfileUpdate`).
                let Some(peer) = self.peer_of_sent(crypto, to, group.as_ref()) else { return Ok(None) };
                self.set_sharing(peer, true);
                if self.account.profile_key != key {
                    self.account.profile_key = key;
                    self.dirty.insert(File::Account);
                }
                None
            }
            Content::Blocked { acis, groups } => {
                self.take_blocked_lists(acis, groups);
                None
            }
            Content::Configuration { read_receipts } => {
                self.account.read_receipts = read_receipts;
                self.dirty.insert(File::Account);
                None
            }
            Content::Response { thread, kind } => {
                self.apply_response(thread, kind, false);
                self.conversations.contains_key(&thread).then_some(Event::Changed(thread))
            }
            Content::FetchStorage => {
                self.is_storage_due = true;
                None
            }
            Content::Keys { master, pool } => {
                self.take_account_keys(crypto, master, pool.as_deref());
                None
            }
            Content::Timer(text) => {
                let (peer, _) = peer_and_author(self, text.group.as_ref());
                (take_timer(self, peer, &text) || text.group.is_some()).then_some(Event::Changed(peer))
            }
            Content::SentTimer { to, text } => {
                let Some(peer) = self.peer_of_sent(crypto, to, text.group.as_ref()) else { return Ok(None) };
                (take_timer(self, peer, &text) || text.group.is_some()).then_some(Event::Changed(peer))
            }
            Content::ProfileKey(key) => {
                self.take_profile_key(service, key);
                None
            }
            Content::Edit { target, text } => {
                let (peer, author) = peer_and_author(self, text.group.as_ref());
                let is_edited =
                    self.change_conversation(peer, |conversation| conversation.edit(target, (false, author), &text));
                self.is_dropped |= is_edited;
                return to_edit_result(is_edited, peer);
            }
            Content::SentEdit { to, target, text } => {
                let Some(peer) = self.peer_of_sent(crypto, to, text.group.as_ref()) else { return Ok(None) };
                let is_edited =
                    self.change_conversation(peer, |conversation| conversation.edit(target, (true, None), &text));
                self.is_dropped |= is_edited;
                return to_edit_result(is_edited, peer);
            }
            Content::Delete { target, group } => {
                let (peer, author) = peer_and_author(self, group.as_ref());
                return self.erase(peer, (target, (false, author)), (service, Deleter::Author), server);
            }
            Content::SentDelete { to, target, group } => {
                let Some(peer) = self.peer_of_sent(crypto, to, group.as_ref()) else { return Ok(None) };
                return self.erase(peer, (target, (true, None)), (own, Deleter::Author), server);
            }
            Content::AdminDelete { author, target, group: Some(context) } => {
                let peer = self.note_group(crypto, &context);
                let target_author = if author == own { (true, None) } else { (false, Some(author)) };
                return self.erase(peer, (target, target_author), (service, Deleter::Admin), server);
            }
            Content::SentAdminDelete { author, target, group: Some(context) } => {
                let peer = self.note_group(crypto, &context);
                let target_author = if author == own { (true, None) } else { (false, Some(author)) };
                return self.erase(peer, (target, target_author), (own, Deleter::Admin), server);
            }
            Content::AdminDelete { .. } | Content::SentAdminDelete { .. } => None,
            Content::Reaction(reaction) => {
                let (peer, _) = peer_and_author(self, reaction.group.as_ref());
                return self.react(peer, service, &reaction);
            }
            Content::SentReaction { to, reaction } => {
                let Some(peer) = self.peer_of_sent(crypto, to, reaction.group.as_ref()) else { return Ok(None) };
                return self.react(peer, own, &reaction);
            }
            Content::Receipt { is_read, timestamps } => return self.take_receipts(service, is_read, &timestamps),
            Content::DeleteForMe(parts) => return self.apply_delete_for_me(parts),
            Content::GroupChange(context) => Some(Event::Changed(self.note_group(crypto, &context))),
            Content::ReadElsewhere { reads, at } => {
                // Read on another device at `at`, as that device stamped it, which begins the messages' timers then.
                let at = at.min(now);
                let mut event = None;
                for (sender, timestamp) in reads {
                    // The message read is the sender's own conversation's, or a group's that holds it from the sender.
                    let groups = self.conversations.iter().filter(|(peer, conversation)| {
                        matches!(peer, Peer::Group(_)) && conversation.has_message(Some(sender), timestamp)
                    });
                    let mut peers: Vec<Peer> = groups.map(|(peer, _)| *peer).collect();
                    if peers.is_empty() {
                        peers.push(Peer::Contact(sender));
                    }
                    for peer in peers {
                        if self.change_conversation(peer, |conversation| conversation.read_up_to(timestamp, at)) {
                            event = Some(Event::Changed(peer));
                        }
                    }
                }
                event
            }
            Content::EndSession => {
                self.archive_session(*sender);
                None
            }
            // The group's revision is noted as a change of the group's is (`handleDataMessage`).
            Content::GroupCall { group, era, timestamp } => {
                let peer = self.note_group(crypto, &group);
                let is_logged = era.is_some_and(|era| self.take_group_call(crypto, peer, (service, &era), timestamp));
                Some(if is_logged { Event::Logged } else { Event::Changed(peer) })
            }
            Content::Logged(logged) => self.take_logged(&logged).then_some(Event::Logged),
            Content::LogChange(change) => self.take_log_change(&change, now).then_some(Event::Logged),
            // A call message is gated as its delivery is taken (`receive_content`).
            Content::Call { .. } | Content::Nothing => None,
        })
    }

    fn peer_of_sent(
        &mut self,
        crypto: &impl Crypto,
        recipient: Option<ServiceId>,
        group: Option<&Context>,
    ) -> Option<Peer> {
        match (group, recipient) {
            (Some(context), _) => Some(self.note_group(crypto, context)),
            (None, recipient) => recipient.map(Peer::Contact),
        }
    }

    fn erase(
        &mut self,
        peer: Peer,
        (target, author): (u64, (bool, Option<ServiceId>)),
        (deleter_id, deleter): (ServiceId, Deleter),
        server: u64,
    ) -> Result<Option<Event>, MissingTarget> {
        if peer == Peer::Contact(self.account.aci()) {
            return Ok(None);
        }
        let Some(conversation) = self.conversations.get(&peer) else { return Err(MissingTarget::default()) };
        let group = conversation.group.as_ref();
        if group.is_some_and(|group| group.terminated) {
            return Ok(None);
        }
        let Some(entry) = conversation.entry(target, author) else { return Err(MissingTarget::default()) };
        let is_allowed = deleter == Deleter::Author || group.is_some_and(|group| group.admins.contains(&deleter_id));
        if entry.deleter.is_some() || !is_allowed || target.abs_diff(server) > DELETE_WINDOW + DELETE_GRACE {
            return Ok(None);
        }
        self.conversation_mut(peer).erase(target, author, deleter);
        self.forget_gone();
        Ok(Some(Event::Changed(peer)))
    }

    // A reaction of `from`'s to a message of the conversation, as Signal-Desktop takes one (`isMessageAMatchForReaction`, `handleReaction`): from one who is of the conversation, to a message its author wrote there that is not deleted; one that names a message not held yet waits for it.
    fn react(
        &mut self,
        peer: Peer,
        from: ServiceId,
        reaction: &content::Reacted,
    ) -> Result<Option<Event>, MissingTarget> {
        let own = self.account.aci();
        let author = match peer {
            _ if reaction.author == own => (true, None),
            Peer::Group(_) => (false, Some(reaction.author)),
            Peer::Contact(contact) if reaction.author == contact => (false, None),
            Peer::Contact(_) => return Ok(None),
        };
        let is_of_it = self
            .group(&peer)
            .is_none_or(|group| !group.terminated && group.members.contains(&own) && group.members.contains(&from));
        if !is_of_it {
            return Ok(None);
        }
        let given = (from, reaction.emoji.as_deref(), reaction.timestamp);
        let held = self.conversations.get_mut(&peer);
        match held.and_then(|conversation| conversation.react((reaction.target, author), given, from == own)) {
            Some(true) => {
                self.dirty.insert(File::Conversation(peer));
                Ok(Some(Event::Changed(peer)))
            }
            Some(false) => Ok(None),
            None => Err(MissingTarget::default()),
        }
    }

    fn take_receipts(
        &mut self,
        sender: ServiceId,
        is_read: bool,
        timestamps: &[u64],
    ) -> Result<Option<Event>, MissingTarget> {
        if is_read && !self.account.read_receipts {
            return Ok(None);
        }
        let status = if is_read { Status::Read } else { Status::Delivered };
        let (mut event, mut has_missing) = (None, false);
        for timestamp in timestamps {
            let peers: Vec<Peer> = self
                .conversations
                .iter()
                .filter(|(peer, conversation)| {
                    let is_recipient = **peer == Peer::Contact(sender)
                        || conversation.group.as_ref().is_some_and(|group| group.members.contains(&sender));
                    is_recipient && conversation.has_sent(*timestamp)
                })
                .map(|(peer, _)| *peer)
                .collect();
            has_missing |= peers.is_empty();
            for peer in peers {
                if self.change_conversation(peer, |conversation| conversation.raise_status(*timestamp, status)) {
                    event = Some(Event::Changed(peer));
                }
            }
        }
        if has_missing { Err(MissingTarget { event, rest: None }) } else { Ok(event) }
    }

    fn peer_of_thread(&self, thread: &Identifier) -> Option<Peer> {
        let peer = match thread {
            Identifier::Group(id) => Peer::Group(*id),
            Identifier::Service(service) => Peer::Contact(self.aci_of(service)),
            Identifier::Number(number) => Peer::Contact(self.aci_of_number(number)?),
        };
        self.conversations.contains_key(&peer).then_some(peer)
    }

    fn author_of(&self, peer: &Peer, author: &Identifier) -> Option<(bool, Option<ServiceId>)> {
        let service = match author {
            Identifier::Service(service) => self.aci_of(service),
            Identifier::Number(number) => self.aci_of_number(number)?,
            Identifier::Group(_) => return None,
        };
        if service == self.account.aci() {
            return Some((true, None));
        }
        Some((false, matches!(peer, Peer::Group(_)).then_some(service)))
    }

    fn newest_of(&self, peer: &Peer, messages: &[(Identifier, u64)]) -> Option<u64> {
        let conversation = self.conversations.get(peer)?;
        let entry =
            |(author, timestamp): &(Identifier, u64)| conversation.entry(*timestamp, self.author_of(peer, author)?);
        messages.iter().filter_map(entry).map(|entry| entry.at).max()
    }

    fn apply_delete_for_me(&mut self, parts: Vec<DeleteForMe>) -> Result<Option<Event>, MissingTarget> {
        let (mut event, mut rest, mut has_removed) = (None, Vec::new(), false);
        for part in parts {
            match part {
                DeleteForMe::Messages { thread, messages } => {
                    let Some(peer) = self.peer_of_thread(&thread) else {
                        rest.push(DeleteForMe::Messages { thread, messages });
                        continue;
                    };
                    let mut missing = Vec::new();
                    for (author, timestamp) in messages {
                        match self.author_of(&peer, &author) {
                            Some(owner)
                                if self.change_conversation(peer, |conversation| {
                                    conversation.forget(timestamp, owner)
                                }) =>
                            {
                                (event, has_removed) = (Some(Event::Changed(peer)), true);
                            }
                            _ => missing.push((author, timestamp)),
                        }
                    }
                    if !missing.is_empty() {
                        rest.push(DeleteForMe::Messages { thread, messages: missing });
                    }
                }
                DeleteForMe::Conversation { thread, newest, non_expiring, is_full } => {
                    let Some(peer) = self.peer_of_thread(&thread) else { continue };
                    let up_to = self.newest_of(&peer, &newest).or_else(|| self.newest_of(&peer, &non_expiring));
                    if up_to
                        .is_some_and(|at| self.change_conversation(peer, |conversation| conversation.forget_up_to(at)))
                    {
                        (event, has_removed) = (Some(Event::Changed(peer)), true);
                    }
                    if is_full && self.forget_local(peer) {
                        event = Some(Event::Changed(peer));
                    }
                }
                DeleteForMe::Local { thread } => {
                    if let Some(peer) = self.peer_of_thread(&thread)
                        && self.forget_local(peer)
                    {
                        event = Some(Event::Changed(peer));
                    }
                }
            }
        }
        if has_removed {
            self.forget_gone();
        }
        if rest.is_empty() {
            Ok(event)
        } else {
            Err(MissingTarget { event, rest: Some(content::write_delete_for_me(&rest)) })
        }
    }

    fn forget_local(&mut self, peer: Peer) -> bool {
        let is_shown = self
            .conversations
            .get(&peer)
            .is_some_and(|conversation| conversation.entries.is_empty() && conversation.is_safety_changed);
        if is_shown {
            self.conversation_mut(peer).is_safety_changed = false;
        }
        is_shown
    }

    fn take_account_keys(&mut self, crypto: &impl Crypto, master: Option<[u8; 32]>, pool: Option<&str>) {
        let key = master.or_else(|| derive_master_key(crypto, pool?));
        if let Some(key) = key.filter(|key| *key != self.account.master_key) {
            self.account.master_key = key;
            self.is_storage_due = true;
            self.dirty.insert(File::Account);
        }
    }
}

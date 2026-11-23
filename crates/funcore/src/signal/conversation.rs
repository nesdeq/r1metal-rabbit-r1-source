//! A conversation's messages and state, read and written one message a line.

use super::address::{Address, Peer, ServiceId};
use super::attachment::Attachment;
use super::content::{Mention, Placeholder, Quote, Text};
use super::group::Group;
use crate::base::code::base64_onto;
use crate::base::code::unbase64;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::base::time::MILLIS_PER_SECOND;
use alloc::string::String;
use alloc::vec::Vec;

/// How far an owner's message got, the best any recipient told (Signal-Desktop's `getHighestSuccessfulRecipientStatus`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Status {
    Received,
    // Waiting in the outbox.
    Sending,
    // The service refused it, or it could not be sealed.
    Failed,
    Sent,
    Delivered,
    Read,
}

/// Who deleted a message for everyone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Deleter {
    Author,
    Admin,
}

/// A reaction to a message: who gave it, its emoji, and its own timestamp, ms.
#[derive(Clone, PartialEq, Eq)]
pub struct Reaction {
    pub from: ServiceId,
    pub emoji: String,
    pub timestamp: u64,
}

impl core::fmt::Debug for Reaction {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Reaction").field("timestamp", &self.timestamp).finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    /// The sender's timestamp, the message's id, ms.
    pub timestamp: u64,
    /// When the R1 took it in or wrote it, ms.
    pub at: u64,
    pub is_owner: bool,
    pub body: String,
    pub placeholders: Vec<Placeholder>,
    /// What it carries that can be had: a contact's to be fetched, the owner's to go up before the message goes.
    pub attachments: Vec<Attachment>,
    /// The words' mentions; `Device::resolve_mentions` puts the names in their place.
    pub mentions: Vec<Mention>,
    /// The message it replies to.
    pub quote: Option<Quote>,
    /// Who deleted it for everyone, its words gone with it.
    pub deleter: Option<Deleter>,
    pub status: Status,
    /// Seconds after its timer began the message disappears; nought for never.
    pub expire: u32,
    /// When its timer began, ms: an owner's message once sent, a contact's once read.
    pub started: Option<u64>,
    pub is_read: bool,
    pub is_edited: bool,
    /// True if the robot wrote it in auto mode.
    pub is_robot: bool,
    /// The group member who wrote it.
    pub sender: Option<ServiceId>,
    /// The reactions to it, one of each who gave one, in the order they came.
    pub reactions: Vec<Reaction>,
}

impl core::fmt::Debug for Entry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Entry")
            .field("timestamp", &self.timestamp)
            .field("at", &self.at)
            .field("owner", &self.is_owner)
            .field("placeholders", &self.placeholders)
            .field("deleted", &self.deleter)
            .field("status", &self.status)
            .field("expire", &self.expire)
            .field("started", &self.started)
            .field("read", &self.is_read)
            .field("edited", &self.is_edited)
            .field("robot", &self.is_robot)
            .finish_non_exhaustive()
    }
}

const STATUSES: [Status; 6] =
    [Status::Received, Status::Sending, Status::Sent, Status::Failed, Status::Delivered, Status::Read];
const PLACEHOLDERS: [Placeholder; 8] = [
    Placeholder::Photo,
    Placeholder::Video,
    Placeholder::VoiceNote,
    Placeholder::File,
    Placeholder::Sticker,
    Placeholder::Contact,
    Placeholder::Poll,
    Placeholder::Unsupported,
];
const DELETERS: [Deleter; 2] = [Deleter::Author, Deleter::Admin];
// Who an owner's message is found by: the owner, and no group member.
pub(super) const OWN: (bool, Option<ServiceId>) = (true, None);

fn to_number<T: PartialEq>(list: &[T], value: &T) -> u64 {
    list.iter().position(|item| item == value).expect("every one is listed") as u64
}

fn from_number<T: Copy>(list: &[T], number: u64) -> Option<T> {
    list.get(usize::try_from(number).ok()?).copied()
}

impl Entry {
    pub(super) fn incoming(text: &Text, at: u64, sender: Option<ServiceId>) -> Entry {
        Entry {
            timestamp: text.timestamp,
            at,
            is_owner: false,
            body: text.body.clone(),
            placeholders: text.placeholders.clone(),
            attachments: text.attachments.clone(),
            mentions: text.mentions.clone(),
            quote: text.quote.clone(),
            deleter: None,
            status: Status::Received,
            expire: text.expire,
            started: None,
            is_read: false,
            is_edited: false,
            is_robot: false,
            sender,
            reactions: Vec::new(),
        }
    }

    pub(super) fn outgoing(text: &Text, at: u64, status: Status) -> Entry {
        Entry { is_owner: true, status, is_read: true, ..Entry::incoming(text, at, None) }
    }

    fn matches(&self, timestamp: u64, is_owner: bool, sender: Option<ServiceId>) -> bool {
        self.timestamp == timestamp && self.is_owner == is_owner && self.sender == sender
    }

    pub(super) fn quote_author(&self) -> Option<ServiceId> {
        self.quote.as_ref().map(|quote| quote.author)
    }

    fn expiry(&self) -> Option<u64> {
        (self.expire > 0).then_some(self.started?.saturating_add(u64::from(self.expire) * MILLIS_PER_SECOND))
    }

    fn has_expired(&self, now: u64) -> bool {
        self.expiry().is_some_and(|at| at <= now)
    }

    fn erase(&mut self, deleter: Deleter) {
        (self.body, self.placeholders, self.mentions, self.quote) = (String::new(), Vec::new(), Vec::new(), None);
        (self.attachments, self.deleter, self.is_edited) = (Vec::new(), Some(deleter), false);
        self.reactions.clear();
    }

    // Takes a reaction of `from`'s, an emoji or none for one taken back, in place of the one `from` gave before: Signal-Desktop's `handleReaction`. One of the owner's from another device gives way to a later one held. True if the reactions changed.
    fn react(&mut self, from: ServiceId, emoji: Option<&str>, timestamp: u64, is_owner: bool) -> bool {
        let held = self.reactions.iter().position(|reaction| reaction.from == from);
        if self.deleter.is_some() || is_owner && held.is_some_and(|at| self.reactions[at].timestamp > timestamp) {
            return false;
        }
        let taken = held.map(|at| self.reactions.remove(at));
        self.reactions.extend(emoji.map(|emoji| Reaction { from, emoji: emoji.into(), timestamp }));
        taken.is_some() || emoji.is_some()
    }

    // Its reactions as its side tells them: each emoji once, in the order it first came, with how many gave it.
    pub(crate) fn reaction_counts(&self) -> Vec<(&str, usize)> {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for reaction in &self.reactions {
            match counts.iter_mut().find(|(emoji, _)| *emoji == reaction.emoji) {
                Some((_, count)) => *count += 1,
                None => counts.push((&reaction.emoji, 1)),
            }
        }
        counts
    }

    // The files that hold the plaintexts of its attachments, where the R1 holds them.
    pub(super) fn files(&self) -> impl Iterator<Item = &str> {
        self.attachments.iter().filter_map(|attachment| attachment.file.as_deref())
    }

    fn start(&mut self, at: u64) {
        self.started = Some(self.started.map_or(at, |started| started.min(at)));
    }

    fn write_line(&self, text: &mut String) {
        let mut writer = Writer::new()
            .varint(1, self.timestamp)
            .varint(2, self.at)
            .bool(3, self.is_owner)
            .text(4, &self.body)
            .varint(6, to_number(&STATUSES, &self.status))
            .varint(7, u64::from(self.expire))
            .bool(8, self.is_read)
            .bool(9, self.is_edited)
            .bool(10, self.is_robot)
            .maybe(self.sender.map(|sender| move |w: Writer| w.bytes(11, &sender.to_binary())))
            .maybe(self.started.map(|started| move |w: Writer| w.varint(12, started)))
            .maybe(self.quote.as_ref().map(|quote| {
                move |w: Writer| {
                    let message = Writer::new()
                        .varint(1, quote.timestamp)
                        .bytes(2, &quote.author.to_binary())
                        .text(3, &quote.text);
                    w.message(14, message)
                }
            }))
            .maybe(self.deleter.map(|deleter| move |w: Writer| w.varint(15, to_number(&DELETERS, &deleter))));
        for placeholder in &self.placeholders {
            writer = writer.varint(5, to_number(&PLACEHOLDERS, placeholder));
        }
        for attachment in &self.attachments {
            writer = writer.message(16, attachment.write_line());
        }
        for mention in &self.mentions {
            let message = Writer::new()
                .varint(1, u64::from(mention.start))
                .varint(2, u64::from(mention.length))
                .bytes(3, &mention.aci.to_binary());
            writer = writer.message(13, message);
        }
        for reaction in &self.reactions {
            let message = Writer::new()
                .bytes(1, &reaction.from.to_binary())
                .text(2, &reaction.emoji)
                .varint(3, reaction.timestamp);
            writer = writer.message(17, message);
        }
        base64_onto(&writer.finish(), text);
        text.push('\n');
    }

    fn from_line(line: &str) -> Option<Entry> {
        let bytes = unbase64(line.trim())?;
        let message = Message::parse(&bytes)?;
        let (at, expire) = (message.varint(2)?, message.u32(7).unwrap_or(0));
        Some(Entry {
            timestamp: message.varint(1)?,
            at,
            is_owner: message.bool(3)?,
            body: String::from(message.text(4).unwrap_or_default()),
            placeholders: message
                .varints(5)
                .into_iter()
                .filter_map(|number| from_number(&PLACEHOLDERS, number))
                .collect(),
            attachments: message
                .repeated(16)
                .filter_map(|line| Attachment::from_line(&Message::parse(line)?))
                .collect(),
            mentions: message
                .repeated(13)
                .filter_map(Message::parse)
                .filter_map(|mention| {
                    Some(Mention {
                        start: mention.u32(1).unwrap_or(0),
                        length: mention.u32(2).unwrap_or(0),
                        aci: ServiceId::from_binary(mention.bytes(3)?)?,
                    })
                })
                .collect(),
            quote: message.message(14).and_then(|quote| {
                Some(Quote {
                    timestamp: quote.varint(1)?,
                    author: ServiceId::from_binary(quote.bytes(2)?)?,
                    text: String::from(quote.text(3).unwrap_or_default()),
                })
            }),
            deleter: message.varint(15).and_then(|number| from_number(&DELETERS, number)),
            status: from_number(&STATUSES, message.varint(6)?)?,
            expire,
            // A line that names no start was kept counting from when the message was taken in.
            started: message.varint(12).or((expire > 0).then_some(at)),
            is_read: message.bool(8).unwrap_or(true),
            is_edited: message.bool(9).unwrap_or(false),
            is_robot: message.bool(10).unwrap_or(false),
            sender: message.bytes(11).and_then(ServiceId::from_binary),
            reactions: message
                .repeated(17)
                .filter_map(Message::parse)
                .filter_map(|reaction| {
                    Some(Reaction {
                        from: ServiceId::from_binary(reaction.bytes(1)?)?,
                        emoji: String::from(reaction.text(2)?),
                        timestamp: reaction.varint(3).unwrap_or(0),
                    })
                })
                .collect(),
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct QueuedMessage {
    pub(super) content: Vec<u8>,
    pub(super) timestamp: u64,
    pub(super) sender: Address,
    pub(super) is_sealed: bool,
    pub(super) at: u64,
}

impl core::fmt::Debug for QueuedMessage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Waiting").field("timestamp", &self.timestamp).finish_non_exhaustive()
    }
}

impl QueuedMessage {
    fn write(&self) -> Writer {
        Writer::new()
            .bytes(1, &self.content)
            .varint(2, self.timestamp)
            .bytes(3, &self.sender.service.to_binary())
            .varint(4, u64::from(self.sender.device))
            .bool(5, self.is_sealed)
            .varint(6, self.at)
    }

    fn read(message: &Message) -> Option<QueuedMessage> {
        Some(QueuedMessage {
            content: message.bytes(1)?.to_vec(),
            timestamp: message.varint(2)?,
            sender: Address { service: ServiceId::from_binary(message.bytes(3)?)?, device: message.u32(4)? },
            is_sealed: message.bool(5).unwrap_or(false),
            at: message.varint(6)?,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Mark {
    pub(super) is_set: bool,
    is_recorded: bool,
}

impl Mark {
    pub(super) fn apply_record(&mut self, is_recorded: bool) -> bool {
        if is_recorded == self.is_recorded {
            return false;
        }
        self.is_recorded = is_recorded;
        core::mem::replace(&mut self.is_set, is_recorded) != is_recorded
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Conversation {
    pub(crate) peer: Peer,
    pub(crate) entries: Vec<Entry>,
    pub(super) expire: u32,
    pub(super) expire_version: u32,
    pub(crate) is_safety_changed: bool,
    pub(super) group: Option<Group>,
    pub(super) is_sent_to: bool,
    pub(super) queue: Vec<QueuedMessage>,
    pub(super) archived: Mark,
    pub(super) marked_unread: Mark,
    pub(super) muted_until: u64,
    // Whether it tells mentions, replies and calls while muted, where its record says.
    pub(super) notify: (Option<bool>, Option<bool>, Option<bool>),
}

impl Conversation {
    pub(super) fn new(peer: Peer) -> Conversation {
        Conversation {
            peer,
            entries: Vec::new(),
            expire: 0,
            expire_version: 0,
            is_safety_changed: false,
            group: None,
            is_sent_to: false,
            queue: Vec::new(),
            archived: Mark::default(),
            marked_unread: Mark::default(),
            muted_until: 0,
            notify: (None, None, None),
        }
    }

    pub(crate) fn read(peer: Peer, text: &str) -> Conversation {
        let mut conversation = Conversation::new(peer);
        let mut lines = text.lines();
        if let Some(bytes) = lines.next().and_then(unbase64)
            && let Some(head) = Message::parse(&bytes)
        {
            conversation.expire = head.u32(1).unwrap_or(0);
            conversation.expire_version = head.u32(2).unwrap_or(0);
            conversation.is_safety_changed = head.bool(3).unwrap_or(false);
            conversation.group = head.message(4).and_then(|group| Group::read(&group));
            conversation.is_sent_to = head.bool(5).unwrap_or(false);
            conversation.queue =
                head.repeated(6).filter_map(|queued| QueuedMessage::read(&Message::parse(queued)?)).collect();
            let mark = |set, record| Mark {
                is_set: head.bool(set).unwrap_or(false),
                is_recorded: head.bool(record).unwrap_or(false),
            };
            (conversation.archived, conversation.marked_unread) = (mark(7, 8), mark(9, 10));
            conversation.muted_until = head.varint(11).unwrap_or(0);
            conversation.notify = (head.bool(12), head.bool(13), head.bool(14));
        }
        conversation.entries = lines.filter_map(Entry::from_line).collect();
        conversation
    }

    pub(crate) fn write(&self) -> String {
        let head = self
            .queue
            .iter()
            .fold(
                Writer::new()
                    .varint(1, u64::from(self.expire))
                    .varint(2, u64::from(self.expire_version))
                    .bool(3, self.is_safety_changed)
                    .maybe(self.group.as_ref().map(|group| move |w: Writer| w.message(4, group.write())))
                    .bool(5, self.is_sent_to)
                    .bool(7, self.archived.is_set)
                    .bool(8, self.archived.is_recorded)
                    .bool(9, self.marked_unread.is_set)
                    .bool(10, self.marked_unread.is_recorded)
                    .varint(11, self.muted_until)
                    .maybe(self.notify.0.map(|mentions| move |w: Writer| w.bool(12, mentions)))
                    .maybe(self.notify.1.map(|replies| move |w: Writer| w.bool(13, replies)))
                    .maybe(self.notify.2.map(|calls| move |w: Writer| w.bool(14, calls))),
                |writer, queued| writer.message(6, queued.write()),
            )
            .finish();
        let mut text = String::new();
        base64_onto(&head, &mut text);
        text.push('\n');
        self.entries.iter().for_each(|entry| entry.write_line(&mut text));
        text
    }

    pub(super) fn take_timer(&mut self, text: &Text) -> bool {
        if text.expire_version == 0 || (self.expire_version > 0 && text.expire_version < self.expire_version) {
            return false;
        }
        self.expire_version = self.expire_version.max(text.expire_version);
        core::mem::replace(&mut self.expire, text.expire) != text.expire
    }

    pub(super) fn start_sent_timer(&mut self, timestamp: u64, at: u64) {
        if let Some(entry) = self.entry_mut(timestamp, OWN) {
            entry.start(at);
        }
    }

    pub(super) fn own_timestamps(&self) -> impl Iterator<Item = u64> + '_ {
        self.entries.iter().filter(|entry| entry.is_owner && entry.deleter.is_none()).map(|entry| entry.timestamp)
    }

    pub(super) fn entry(&self, target: u64, (is_owner, sender): (bool, Option<ServiceId>)) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.matches(target, is_owner, sender))
    }

    pub(super) fn entry_mut(
        &mut self,
        target: u64,
        (is_owner, sender): (bool, Option<ServiceId>),
    ) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|entry| entry.matches(target, is_owner, sender))
    }

    pub(super) fn next_expiry(&self) -> Option<u64> {
        self.entries.iter().filter_map(Entry::expiry).min()
    }

    pub(crate) fn add(&mut self, entry: Entry) -> bool {
        if self.entry(entry.timestamp, (entry.is_owner, entry.sender)).is_some() {
            return false;
        }
        let at = self.entries.partition_point(|existing| existing.at <= entry.at);
        self.entries.insert(at, entry);
        true
    }

    pub(super) fn edit(&mut self, target: u64, author: (bool, Option<ServiceId>), text: &Text) -> bool {
        let Some(entry) = self.entry_mut(target, author).filter(|entry| entry.deleter.is_none()) else { return false };
        (entry.body, entry.placeholders, entry.is_edited) = (text.body.clone(), text.placeholders.clone(), true);
        let attachments = text.attachments.iter().map(|new| new.clone().inheriting(&entry.attachments)).collect();
        (entry.attachments, entry.mentions, entry.quote) = (attachments, text.mentions.clone(), text.quote.clone());
        true
    }

    pub(super) fn erase(&mut self, target: u64, author: (bool, Option<ServiceId>), deleter: Deleter) -> bool {
        let Some(entry) = self.entry_mut(target, author) else { return false };
        entry.erase(deleter);
        true
    }

    // Takes a reaction to the message `author` wrote at `target`; none where the message is not held, else whether its reactions changed.
    pub(super) fn react(
        &mut self,
        (target, author): (u64, (bool, Option<ServiceId>)),
        (from, emoji, timestamp): (ServiceId, Option<&str>, u64),
        is_owner: bool,
    ) -> Option<bool> {
        Some(self.entry_mut(target, author)?.react(from, emoji, timestamp, is_owner))
    }

    pub(super) fn forget(&mut self, target: u64, (is_owner, sender): (bool, Option<ServiceId>)) -> bool {
        let count = self.entries.len();
        self.entries.retain(|entry| !entry.matches(target, is_owner, sender));
        self.entries.len() != count
    }

    pub(super) fn forget_up_to(&mut self, at: u64) -> bool {
        let count = self.entries.len();
        self.entries.retain(|entry| entry.at > at);
        self.entries.len() != count
    }

    pub(super) fn raise_status(&mut self, timestamp: u64, status: Status) -> bool {
        let entry = self.entry_mut(timestamp, OWN);
        match entry.filter(|entry| entry.status >= Status::Sent && entry.status < status) {
            Some(entry) => {
                entry.status = status;
                true
            }
            None => false,
        }
    }

    pub(super) fn has_sent(&self, timestamp: u64) -> bool {
        self.entry(timestamp, OWN).is_some_and(|entry| entry.status >= Status::Sent)
    }

    pub(super) fn has_message(&self, sender: Option<ServiceId>, timestamp: u64) -> bool {
        self.entry(timestamp, (false, sender)).is_some()
    }

    pub(super) fn expire(&mut self, now: u64) -> bool {
        let count = self.entries.len();
        self.entries.retain(|entry| !entry.has_expired(now));
        self.entries.len() != count
    }

    pub(super) fn read_all(&mut self, now: u64) -> Vec<(Option<ServiceId>, u64)> {
        self.entries
            .iter_mut()
            .filter(|entry| !entry.is_owner && !entry.is_read)
            .map(|entry| {
                entry.is_read = true;
                entry.start(now);
                (entry.sender, entry.timestamp)
            })
            .collect()
    }

    pub(super) fn read_up_to(&mut self, timestamp: u64, at: u64) -> bool {
        let mut is_changed = false;
        for entry in
            self.entries.iter_mut().filter(|entry| !entry.is_owner && !entry.is_read && entry.timestamp <= timestamp)
        {
            entry.is_read = true;
            entry.start(at);
            is_changed = true;
        }
        is_changed
    }

    pub(crate) fn unread(&self) -> usize {
        self.entries.iter().filter(|entry| !entry.is_owner && !entry.is_read).count()
    }

    pub(super) fn set_status(&mut self, timestamp: u64, status: Status) {
        if let Some(entry) = self.entry_mut(timestamp, OWN) {
            entry.status = status;
        }
    }

    pub(super) const fn is_muted(&self, now: u64) -> bool {
        self.muted_until > now
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::group::{GroupUpdate, State};
    use super::*;

    fn text(timestamp: u64, body: &str) -> Text {
        Text { timestamp, body: body.into(), ..Text::default() }
    }

    pub(crate) fn entry(timestamp: u64, is_owner: bool, body: &str) -> Entry {
        let text = text(timestamp, body);
        if is_owner { Entry::outgoing(&text, timestamp, Status::Sent) } else { Entry::incoming(&text, timestamp, None) }
    }

    #[test]
    fn round_trip_and_duplicates() {
        let mut conversation = Conversation::new(Peer::Contact(ServiceId::Aci([1; 16])));
        conversation.expire = 3600;
        assert!(conversation.add(Entry::incoming(&text(10, "hello"), 100, None)));
        assert!(!conversation.add(Entry::incoming(&text(10, "hello"), 101, None)), "the same message again");
        assert!(conversation.add(Entry::outgoing(&text(20, "hi"), 90, Status::Sent)));
        assert_eq!(conversation.entries[0].body, "hi", "in the order they were taken in");
        let photos = alloc::vec![Placeholder::Photo, Placeholder::Sticker];
        assert!(conversation.edit(10, (false, None), &Text { placeholders: photos, ..text(11, "hello there") }));
        let parsed = Conversation::read(conversation.peer, &conversation.write());
        assert_eq!(parsed, conversation);
        assert_eq!(parsed.unread(), 1);
        let mut corrupt = conversation.write();
        corrupt.push_str("not a message\n");
        assert_eq!(Conversation::read(conversation.peer, &corrupt).entries.len(), 2);
    }

    #[test]
    fn attachments_with_their_message() {
        use super::super::attachment::{Enclosure, KEYS};
        let mut conversation = Conversation::new(Peer::Contact(ServiceId::Aci([1; 16])));
        let photo = Enclosure::photo("a.jpg".into(), (4, 3)).attach([3; KEYS]);
        let looking = Text { attachments: alloc::vec![photo.clone()], ..text(5, "look") };
        conversation.add(Entry::outgoing(&looking, 50, Status::Sending));
        let read = Conversation::read(conversation.peer, &conversation.write());
        assert_eq!(
            (read.entries[0].attachments.clone(), read.entries[0].files().collect::<Vec<_>>()),
            (alloc::vec![photo], alloc::vec!["a.jpg"]),
            "with the file that holds it"
        );
        let again = Enclosure::photo("b.jpg".into(), (4, 3)).attach([3; KEYS]);
        let edited = Text { attachments: alloc::vec![again], ..text(6, "look again") };
        assert!(conversation.edit(5, (true, None), &edited));
        assert_eq!(
            conversation.entries[0].files().collect::<Vec<_>>(),
            ["a.jpg"],
            "an edit that carries the attachment again keeps the file it is held in"
        );
        assert!(conversation.erase(5, (true, None), Deleter::Author) && conversation.entries[0].attachments.is_empty());
    }

    #[test]
    fn group_conversation() {
        let (anna, boris) = (ServiceId::Aci([1; 16]), ServiceId::Aci([2; 16]));
        let mut conversation = Conversation::new(Peer::Group([7; 32]));
        let mut group = Group::new([4; 32]);
        group.apply(GroupUpdate::State(State {
            revision: 3,
            title: Some("Climbing".into()),
            members: alloc::vec![anna, boris],
            ..State::default()
        }));
        conversation.group = Some(group);
        assert!(conversation.add(Entry::incoming(&text(10, "hi"), 100, Some(anna))));
        assert!(conversation.add(Entry::incoming(&text(10, "hey"), 100, Some(boris))), "another wrote in that ms");
        assert!(!conversation.edit(10, (false, None), &text(11, "lost")), "a message is found by who wrote it");
        assert!(conversation.edit(10, (false, Some(boris)), &text(11, "hey all")));
        assert!(conversation.has_message(Some(anna), 10) && !conversation.has_message(None, 10));
        conversation.is_sent_to = true;
        let sender = Address { service: anna, device: 2 };
        conversation.queue.push(QueuedMessage {
            content: alloc::vec![1, 2],
            timestamp: 12,
            sender,
            is_sealed: true,
            at: 130,
        });
        let parsed = Conversation::read(conversation.peer, &conversation.write());
        assert_eq!(
            parsed, conversation,
            "the group's state, who wrote each message, the owner's sending and what waits for the group read back"
        );
        assert_eq!(conversation.read_all(200), [(Some(anna), 10), (Some(boris), 10)]);
        assert!(conversation.forget(10, (false, Some(anna))) && conversation.entries.len() == 1);
    }

    #[test]
    fn disappearing_messages() {
        let mut conversation = Conversation::new(Peer::Contact(ServiceId::Aci([1; 16])));
        let with_timer = |timestamp, body| Entry { expire: 5, ..Entry::incoming(&text(timestamp, body), 1_000, None) };
        conversation.add(with_timer(1, "gone soon"));
        conversation.add(Entry::incoming(&text(2, "stays"), 2_000, None));
        conversation.add(Entry { expire: 5, ..Entry::outgoing(&text(3, "mine"), 1_000, Status::Sending) });
        assert!(!conversation.expire(100_000), "unread and unsent, no timer has begun");
        conversation.start_sent_timer(3, 4_000);
        assert_eq!(conversation.read_all(6_000), [(None, 1), (None, 2)], "read at 6 s");
        assert!(!conversation.expire(8_999));
        assert!(conversation.expire(9_000), "the owner's 5 s after it was sent");
        assert!(!conversation.expire(10_999));
        assert!(conversation.expire(11_000), "the contact's 5 s after it was read");
        assert_eq!(conversation.entries.len(), 1);
        assert!(conversation.read_all(12_000).is_empty(), "read once");
        conversation.add(with_timer(4, "seen on the phone"));
        assert!(conversation.read_up_to(4, 20_000) && !conversation.read_up_to(4, 30_000));
        assert_eq!(conversation.entries[0].started, Some(20_000), "from when the phone read it");
        let parsed = Conversation::read(conversation.peer, &conversation.write());
        assert_eq!(parsed, conversation, "when each timer began reads back");
        let mut file = conversation.write();
        let old = Writer::new().varint(1, 5).varint(2, 50_000).bool(3, false).varint(6, 0).varint(7, 5).finish();
        crate::base::code::base64_onto(&old, &mut file);
        assert_eq!(
            Conversation::read(conversation.peer, &file).entries[2].started,
            Some(50_000),
            "a line kept without a start counts from when its message was taken in, as it did"
        );
        assert!(conversation.forget(2, (false, None)) && conversation.entries.len() == 1);
    }

    #[test]
    fn message_fields_round_trip() {
        let (anna, own) = (ServiceId::Aci([1; 16]), ServiceId::Aci([3; 16]));
        let mut conversation = Conversation::new(Peer::Contact(anna));
        let reply = Text {
            mentions: alloc::vec![Mention { start: 0, length: 1, aci: own }],
            quote: Some(Quote { timestamp: 5, author: own, text: "Lunch?".into() }),
            ..text(10, "\u{FFFC} yes")
        };
        conversation.add(Entry::incoming(&reply, 100, None));
        conversation.add(Entry::outgoing(&text(20, "mine"), 110, Status::Sending));
        assert!(!conversation.raise_status(20, Status::Delivered), "not before it is sent");
        conversation.set_status(20, Status::Sent);
        assert!(
            conversation.raise_status(20, Status::Read) && !conversation.raise_status(20, Status::Delivered),
            "only ever better"
        );
        assert_eq!(conversation.entries[1].status, Status::Read);
        assert!(conversation.erase(20, (true, None), Deleter::Admin));
        let entry = &conversation.entries[1];
        assert_eq!((entry.body.as_str(), entry.deleter), ("", Some(Deleter::Admin)), "its words gone");
        assert!(!conversation.edit(20, (true, None), &text(21, "again")), "a message deleted takes no edit");
        assert_eq!(conversation.own_timestamps().count(), 0, "nor counts as the owner's to send again");
        conversation.archived.apply_record(true);
        conversation.marked_unread.apply_record(true);
        (conversation.muted_until, conversation.notify) = (5_000, (Some(false), None, Some(true)));
        let parsed = Conversation::read(conversation.peer, &conversation.write());
        assert_eq!(
            parsed, conversation,
            "mentions, quote, deletion, receipts, the list's marks and what it tells while muted, calls among it, read back"
        );
        assert!(parsed.is_muted(4_999) && !parsed.is_muted(5_000), "muted until its time");
    }

    #[test]
    fn reactions_of_a_message() {
        let (anna, boris, own) = (ServiceId::Aci([1; 16]), ServiceId::Aci([2; 16]), ServiceId::Aci([3; 16]));
        let mut conversation = Conversation::new(Peer::Group([7; 32]));
        conversation.add(Entry::incoming(&text(10, "hi"), 100, Some(anna)));
        let at = (10, (false, Some(anna)));
        assert_eq!(conversation.react((11, (false, Some(anna))), (boris, Some("\u{1F44D}"), 20), false), None);
        assert_eq!(conversation.react(at, (boris, Some("\u{1F44D}"), 20), false), Some(true));
        assert_eq!(conversation.react(at, (own, Some("\u{2764}\u{FE0F}"), 30), true), Some(true));
        assert_eq!(conversation.react(at, (anna, Some("\u{1F44D}"), 40), false), Some(true));
        assert_eq!(
            conversation.entries[0].reaction_counts(),
            [("\u{1F44D}", 2), ("\u{2764}\u{FE0F}", 1)],
            "each emoji once, in the order it first came, with how many gave it"
        );
        assert_eq!(
            conversation.react(at, (own, Some("\u{1F389}"), 25), true),
            Some(false),
            "the owner's from another device gives way to a later one held"
        );
        assert_eq!(conversation.react(at, (boris, None, 5), false), Some(true), "another's goes whatever its time");
        assert_eq!(conversation.react(at, (boris, None, 50), false), Some(false), "nothing of his to take back");
        let parsed = Conversation::read(conversation.peer, &conversation.write());
        assert_eq!(parsed, conversation, "who gave each, its emoji and its time read back");
        assert_eq!(alloc::format!("{:?}", parsed.entries[0].reactions[0]), "Reaction { timestamp: 30, .. }");
        assert!(conversation.erase(10, (false, Some(anna)), Deleter::Author));
        assert!(conversation.entries[0].reactions.is_empty(), "a message deleted for everyone loses them");
        assert_eq!(conversation.react(at, (boris, Some("\u{1F44D}"), 60), false), Some(false), "and takes none");
    }

    #[test]
    fn mark_follows_record() {
        let mut mark = Mark::default();
        assert!(mark.apply_record(true) && mark.is_set, "set on the phone");
        mark.is_set = false;
        assert!(!mark.apply_record(true) && !mark.is_set, "the record that said so before changes nothing");
        assert!(!mark.apply_record(false) && !mark.is_set);
        assert!(mark.apply_record(true) && mark.is_set, "set anew on the phone");
    }

    #[test]
    fn forget_and_next_expiry() {
        let mut conversation = Conversation::new(Peer::Group([7; 32]));
        let anna = Some(ServiceId::Aci([1; 16]));
        conversation.add(Entry { expire: 60, started: Some(1_000), ..Entry::incoming(&text(1, "a"), 100, anna) });
        conversation.add(Entry { expire: 10, started: Some(2_000), ..Entry::incoming(&text(2, "b"), 200, anna) });
        conversation.add(Entry::outgoing(&text(3, "c"), 300, Status::Sent));
        assert_eq!(conversation.next_expiry(), Some(12_000), "the second's, ten seconds on");
        assert!(!conversation.forget(2, (false, None)), "by who wrote it in a group");
        assert!(conversation.forget(2, (false, anna)) && conversation.next_expiry() == Some(61_000));
        assert!(conversation.forget_up_to(100) && conversation.entries.len() == 1 && !conversation.forget_up_to(2));
        let far = Entry { expire: 10, started: Some(u64::MAX - 1), ..Entry::incoming(&text(4, "d"), 400, anna) };
        assert_eq!(far.expiry(), Some(u64::MAX), "a start at the far end of time wraps into no early end");
    }

    #[test]
    fn timer_version() {
        let mut conversation = Conversation::new(Peer::Contact(ServiceId::Aci([1; 16])));
        let timer = |expire, expire_version| Text { expire, expire_version, ..text(1, "") };
        assert!(!conversation.take_timer(&timer(60, 0)), "version nought changes nothing");
        assert!(
            conversation.take_timer(&timer(60, 2)) && (conversation.expire, conversation.expire_version) == (60, 2)
        );
        assert!(!conversation.take_timer(&timer(30, 1)), "an earlier version");
        assert!(conversation.take_timer(&timer(30, 2)), "the same version takes the timer");
        assert!(conversation.take_timer(&timer(0, 3)) && (conversation.expire, conversation.expire_version) == (0, 3));
        assert!(
            !conversation.take_timer(&timer(0, 4)) && conversation.expire_version == 4,
            "the version moves on alone"
        );
    }
}

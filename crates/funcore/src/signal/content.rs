// The Envelope and Content protobufs, read as far as the R1 needs and checked as Signal-Desktop checks them.

use super::address::{GROUP_ID_BYTES, Peer, ServiceId, parse_uuid};
use super::attachment::{Attachment, read_kind};
use super::call::{self, LogChange, Logged, Signal};
use super::curve::Pair;
use super::group::Context;
use super::message::{decode_key, decode_kyber, encode_key};
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::gfx::glyph;
use alloc::string::String;
use alloc::vec::Vec;

pub(super) const DOUBLE_RATCHET: u32 = 1;
pub(super) const PREKEY_MESSAGE: u32 = 3;
pub(super) const UNIDENTIFIED_SENDER: u32 = 6;
pub(super) const SENDER_KEY: u32 = 7;
pub(super) const PLAINTEXT: u32 = 8;
const END_SESSION: u64 = 1;
const EXPIRATION_TIMER_UPDATE: u64 = 2;
const PROFILE_KEY_UPDATE: u64 = 4;
const DELIVERY: u64 = 0;
const READ: u64 = 1;
const VIEWED: u64 = 2;
const PROTOCOL_VERSION: u64 = 8;
const SHARED_CONTACT: u32 = 9;
const REACTION: u32 = 16;
const POLL: u32 = 24;
const ADMIN_DELETE: u32 = 29;
const SEPARATE_FIELDS: [u32; 7] = [REACTION, 17, 25, 26, 27, 28, ADMIN_DELETE];
const GROUP_CALL_UPDATE: u32 = 19;
const ERA: u32 = 1;
const STORAGE_MANIFEST: u64 = 2;
pub(super) const BLOCKED: u64 = 3;
pub(super) const CONFIGURATION: u64 = 4;
const PLAINTEXT_MARK: u8 = 0xC0;
const PLAINTEXT_END: u8 = 0x80;

pub(super) struct Envelope {
    pub(super) kind: u32,
    pub(super) source: Option<ServiceId>,
    pub(super) source_device: u32,
    pub(super) destination: Option<ServiceId>,
    pub(super) timestamp: u64,
    pub(super) content: Vec<u8>,
    pub(super) guid: Vec<u8>,
    pub(super) server_timestamp: u64,
    pub(super) updated_pni: Option<[u8; 16]>,
}

fn read_service_id(message: &Message, binary: u32, text: u32) -> Option<ServiceId> {
    match message.bytes(binary).filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => ServiceId::from_binary(bytes),
        None => ServiceId::from_text(message.text(text)?),
    }
}

impl Envelope {
    pub(crate) fn read(bytes: &[u8]) -> Option<Envelope> {
        let message = Message::parse(bytes)?;
        let guid = match message.bytes(21).filter(|bytes| !bytes.is_empty()) {
            Some(bytes) => bytes.to_vec(),
            None => message.bytes(9).unwrap_or_default().to_vec(),
        };
        Some(Envelope {
            kind: message.u32(1)?,
            source: read_service_id(&message, 19, 11),
            source_device: message.u32(7).unwrap_or(1),
            destination: read_service_id(&message, 20, 13),
            timestamp: message.varint(5).unwrap_or(0),
            content: message.bytes(8).unwrap_or_default().to_vec(),
            guid,
            server_timestamp: message.varint(10).unwrap_or(0),
            updated_pni: match message.bytes(22).filter(|bytes| !bytes.is_empty()) {
                Some(bytes) => bytes.try_into().ok(),
                None => message.text(15).and_then(parse_uuid),
            },
        })
    }
}

/// A conversation's stand-in for what the R1 does not show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Placeholder {
    Photo,
    Video,
    VoiceNote,
    File,
    Sticker,
    Contact,
    Poll,
    Unsupported,
}

/// A mention in a message's words (`BodyRange`): its start and length in UTF-16 units, and the account.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Mention {
    pub(crate) start: u32,
    pub(super) length: u32,
    pub(super) aci: ServiceId,
}

/// A reply's quote: the message by timestamp and author, and the quoted words.
#[derive(Clone, PartialEq, Eq)]
pub struct Quote {
    pub(super) timestamp: u64,
    pub(crate) author: ServiceId,
    pub(crate) text: String,
}

impl core::fmt::Debug for Quote {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Quote").field("timestamp", &self.timestamp).finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Default)]
pub(super) struct Text {
    pub(super) timestamp: u64,
    pub(super) body: String,
    pub(super) placeholders: Vec<Placeholder>,
    // The attachments its pointers name that can be had and checked.
    pub(super) attachments: Vec<Attachment>,
    pub(super) mentions: Vec<Mention>,
    pub(super) quote: Option<Quote>,
    pub(super) expire: u32,
    pub(super) expire_version: u32,
    pub(super) profile_key: Option<[u8; 32]>,
    pub(super) group: Option<Context>,
}

impl core::fmt::Debug for Text {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Text")
            .field("timestamp", &self.timestamp)
            .field("placeholders", &self.placeholders)
            .field("expire", &self.expire)
            .field("expire_version", &self.expire_version)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Identifier {
    Service(ServiceId),
    Group([u8; GROUP_ID_BYTES]),
    Number(String),
}

impl core::fmt::Debug for Identifier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Identifier::Service(_) => "Service",
            Identifier::Group(_) => "Group",
            Identifier::Number(_) => "Number",
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum DeleteForMe {
    Messages {
        thread: Identifier,
        messages: Vec<(Identifier, u64)>,
    },
    Conversation {
        thread: Identifier,
        newest: Vec<(Identifier, u64)>,
        non_expiring: Vec<(Identifier, u64)>,
        is_full: bool,
    },
    Local {
        thread: Identifier,
    },
}

type LastResortKey = (u32, Vec<u8>, Vec<u8>, [u8; 64], u64);

#[derive(Clone, PartialEq, Eq)]
pub(super) struct PniChange {
    pub(super) identity: Pair,
    pub(super) signed_pre_key: (u32, Pair, [u8; 64], u64),
    pub(super) last_resort: Option<LastResortKey>,
    pub(super) registration: u32,
    pub(super) number: String,
}

impl core::fmt::Debug for PniChange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NumberChanged").field("registration", &self.registration).finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct DecryptionError {
    pub(super) ratchet: Option<[u8; 32]>,
    pub(super) timestamp: u64,
    pub(super) device: u32,
}

// A reaction to a message: the message by its timestamp and author, the emoji, none for one taken back, and the reaction's own timestamp.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Reacted {
    pub(super) target: u64,
    pub(super) author: ServiceId,
    pub(super) emoji: Option<String>,
    pub(super) timestamp: u64,
    pub(super) group: Option<Context>,
}

impl core::fmt::Debug for Reacted {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Reacted")
            .field("target", &self.target)
            .field("is_taken_back", &self.emoji.is_none())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum Content {
    Text(Text),
    Sent { to: Option<ServiceId>, text: Text, started: Option<u64> },
    Reaction(Reacted),
    SentReaction { to: Option<ServiceId>, reaction: Reacted },
    Edit { target: u64, text: Text },
    SentEdit { to: Option<ServiceId>, target: u64, text: Text },
    Delete { target: u64, group: Option<Context> },
    SentDelete { to: Option<ServiceId>, target: u64, group: Option<Context> },
    AdminDelete { author: ServiceId, target: u64, group: Option<Context> },
    SentAdminDelete { author: ServiceId, target: u64, group: Option<Context> },
    Receipt { is_read: bool, timestamps: Vec<u64> },
    Keys { master: Option<[u8; 32]>, pool: Option<String> },
    DeleteForMe(Vec<DeleteForMe>),
    Timer(Text),
    SentTimer { to: Option<ServiceId>, text: Text },
    ProfileKey([u8; 32]),
    SentProfileKey { to: Option<ServiceId>, group: Option<Context>, key: [u8; 32] },
    Blocked { acis: Option<Vec<ServiceId>>, groups: Vec<[u8; GROUP_ID_BYTES]> },
    Configuration { read_receipts: bool },
    Response { thread: Peer, kind: u32 },
    FetchStorage,
    GroupChange(Context),
    ReadElsewhere { reads: Vec<(ServiceId, u64)>, at: u64 },
    EndSession,
    // A call message's signals, and the one device of the account they are for where it names one.
    Call { signals: Vec<Signal>, device: Option<u32> },
    // A group's call seen begun or left (`GroupCallUpdate`), by its era where it names one.
    GroupCall { group: Context, era: Option<String>, timestamp: u64 },
    // A call as another device of the account tells it for the log.
    Logged(Logged),
    LogChange(LogChange),
    Nothing,
}

// As a derived one tells it, but for the keys, the groups' ids and a group call's era, which are left out, and a call's signals, told by their count.
impl core::fmt::Debug for Content {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Content::Text(text) => f.debug_tuple("Text").field(text).finish(),
            Content::Sent { to, text, started } => {
                f.debug_struct("Sent").field("to", to).field("text", text).field("started", started).finish()
            }
            Content::Reaction(reaction) => f.debug_tuple("Reaction").field(reaction).finish(),
            Content::SentReaction { to, reaction } => {
                f.debug_struct("SentReaction").field("to", to).field("reaction", reaction).finish()
            }
            Content::Edit { target, text } => {
                f.debug_struct("Edit").field("target", target).field("text", text).finish()
            }
            Content::SentEdit { to, target, text } => {
                f.debug_struct("SentEdit").field("to", to).field("target", target).field("text", text).finish()
            }
            Content::Delete { target, group } => {
                f.debug_struct("Delete").field("target", target).field("group", group).finish()
            }
            Content::SentDelete { to, target, group } => {
                f.debug_struct("SentDelete").field("to", to).field("target", target).field("group", group).finish()
            }
            Content::AdminDelete { author, target, group } => f
                .debug_struct("AdminDelete")
                .field("author", author)
                .field("target", target)
                .field("group", group)
                .finish(),
            Content::SentAdminDelete { author, target, group } => f
                .debug_struct("SentAdminDelete")
                .field("author", author)
                .field("target", target)
                .field("group", group)
                .finish(),
            Content::Receipt { is_read, timestamps } => {
                f.debug_struct("Receipt").field("is_read", is_read).field("timestamps", timestamps).finish()
            }
            Content::Keys { .. } => f.write_str("Keys"),
            Content::DeleteForMe(parts) => f.debug_tuple("DeleteForMe").field(parts).finish(),
            Content::Timer(text) => f.debug_tuple("Timer").field(text).finish(),
            Content::SentTimer { to, text } => f.debug_struct("SentTimer").field("to", to).field("text", text).finish(),
            Content::ProfileKey(_) => f.write_str("ProfileKey"),
            Content::SentProfileKey { to, group, .. } => {
                f.debug_struct("SentProfileKey").field("to", to).field("group", group).finish_non_exhaustive()
            }
            Content::Blocked { acis, groups } => {
                f.debug_struct("Blocked").field("acis", acis).field("groups", &groups.len()).finish()
            }
            Content::Configuration { read_receipts } => {
                f.debug_struct("Configuration").field("read_receipts", read_receipts).finish()
            }
            Content::Response { thread, kind } => {
                f.debug_struct("Response").field("thread", thread).field("kind", kind).finish()
            }
            Content::FetchStorage => f.write_str("FetchStorage"),
            Content::GroupChange(group) => f.debug_tuple("GroupChange").field(group).finish(),
            Content::ReadElsewhere { reads, at } => {
                f.debug_struct("ReadElsewhere").field("reads", reads).field("at", at).finish()
            }
            Content::EndSession => f.write_str("EndSession"),
            Content::Call { signals, device } => {
                f.debug_struct("Call").field("signals", &signals.len()).field("device", device).finish()
            }
            Content::GroupCall { group, timestamp, .. } => {
                f.debug_struct("GroupCall").field("group", group).field("timestamp", timestamp).finish_non_exhaustive()
            }
            Content::Logged(logged) => f.debug_tuple("Logged").field(logged).finish(),
            Content::LogChange(change) => f.debug_tuple("LogChange").field(change).finish(),
            Content::Nothing => f.write_str("Nothing"),
        }
    }
}

pub(super) fn placeholder_of(kind: &str, is_voice: bool) -> Placeholder {
    if is_voice {
        Placeholder::VoiceNote
    } else if kind.starts_with("image/") {
        Placeholder::Photo
    } else if kind.starts_with("video/") {
        Placeholder::Video
    } else if kind.starts_with("audio/") {
        Placeholder::VoiceNote
    } else {
        Placeholder::File
    }
}

fn read_placeholders(data: &Message) -> Vec<Placeholder> {
    let mut placeholders: Vec<Placeholder> = data
        .repeated(2)
        .filter_map(Message::parse)
        .map(|pointer| {
            let (kind, is_voice) = read_kind(&pointer);
            placeholder_of(kind, is_voice)
        })
        .collect();
    if data.has(11) {
        placeholders.push(Placeholder::Sticker);
    }
    if data.repeated(SHARED_CONTACT).next().is_some() {
        placeholders.push(Placeholder::Contact);
    }
    if data.has(POLL) {
        placeholders.push(Placeholder::Poll);
    }
    placeholders
}

fn read_attachments(data: &Message) -> Vec<Attachment> {
    data.repeated(2).filter_map(Message::parse).filter_map(|pointer| Attachment::read(&pointer)).collect()
}

fn read_mentions(data: &Message) -> Vec<Mention> {
    let mention = |range: Message| {
        let aci = match range.bytes(5).filter(|bytes| !bytes.is_empty()) {
            Some(bytes) => parse_aci(bytes)?,
            None => ServiceId::from_text(range.text(3)?).filter(|id| matches!(id, ServiceId::Aci(_)))?,
        };
        Some(Mention { start: range.u32(1).unwrap_or(0), length: range.u32(2).unwrap_or(0), aci })
    };
    data.repeated(18).filter_map(Message::parse).filter_map(mention).collect()
}

fn read_quote(data: &Message) -> Option<Quote> {
    let quote = data.message(8)?;
    let author = match quote.bytes(8).filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => parse_aci(bytes)?,
        None => ServiceId::from_text(quote.text(5)?)?,
    };
    Some(Quote { timestamp: quote.varint(1)?, author, text: String::from(quote.text(3).unwrap_or_default()) })
}

// The group a DataMessage is to (`GroupContextV2`).
fn read_group(data: &Message) -> Option<Context> {
    Context::read(&data.message(15)?)
}

fn read_text(data: &Message, timestamp: u64) -> Option<Text> {
    (data.varint(7)? == timestamp).then_some(())?;
    let group = read_group(data);
    let (expire, expire_version) = (data.u32(5).unwrap_or(0), data.u32(23).unwrap_or(0));
    let profile_key = data.bytes(6).and_then(|key| key.try_into().ok());
    let unsupported = Text {
        timestamp,
        placeholders: alloc::vec![Placeholder::Unsupported],
        expire,
        expire_version,
        profile_key,
        group,
        ..Text::default()
    };
    if data.varint(12).unwrap_or(0) > PROTOCOL_VERSION {
        return Some(unsupported);
    }
    let question = data.message(POLL).and_then(|poll| poll.text(1).map(String::from));
    let body = data.text(1).filter(|body| !body.is_empty()).map(String::from).or(question).unwrap_or_default();
    Some(Text {
        body,
        placeholders: read_placeholders(data),
        attachments: read_attachments(data),
        mentions: read_mentions(data),
        quote: read_quote(data),
        ..unsupported
    })
}

// A reaction names its message's author and timestamp, and unless it takes one back, one emoji the screen draws.
fn read_reaction(reaction: &Message, timestamp: u64, group: Option<Context>) -> Option<Reacted> {
    let author = match reaction.bytes(6).filter(|bytes| !bytes.is_empty()) {
        Some(bytes) => parse_aci(bytes)?,
        None => ServiceId::from_text(reaction.text(4)?).filter(|id| matches!(id, ServiceId::Aci(_)))?,
    };
    let emoji = match reaction.bool(2) {
        Some(true) => None,
        _ => Some(String::from(reaction.text(1).filter(|emoji| glyph::is_emoji(emoji))?)),
    };
    Some(Reacted { target: reaction.varint(5)?, author, emoji, timestamp, group })
}

fn parse_data(data: &Message, timestamp: u64) -> Option<Content> {
    let text = read_text(data, timestamp)?;
    let flags = data.varint(4).unwrap_or(0);
    if flags & END_SESSION != 0 {
        return Some(Content::EndSession);
    }
    if flags & EXPIRATION_TIMER_UPDATE != 0 {
        return Some(Content::Timer(text));
    }
    if flags & PROFILE_KEY_UPDATE != 0 {
        return Some(text.profile_key.map_or(Content::Nothing, Content::ProfileKey));
    }
    if flags != 0 {
        return None;
    }
    // A group call's update to no group is dropped (`handleGroupCallUpdateMessage`).
    if let Some(update) = data.message(GROUP_CALL_UPDATE) {
        let era = update.text(ERA).map(String::from);
        return Some(text.group.map_or(Content::Nothing, |group| Content::GroupCall { group, era, timestamp }));
    }
    if let Some(reaction) = data.message(REACTION) {
        return Some(read_reaction(&reaction, timestamp, text.group).map_or(Content::Nothing, Content::Reaction));
    }
    if let Some(delete) = data.message(17) {
        return Some(delete.varint(1).map_or(Content::Nothing, |target| Content::Delete { target, group: text.group }));
    }
    // An administrator's deletion names the message's author in binary and its timestamp, or is refused (`processAdminDelete`).
    if let Some(delete) = data.message(ADMIN_DELETE) {
        let author = parse_aci(delete.bytes(1)?)?;
        return Some(Content::AdminDelete { author, target: delete.varint(2)?, group: text.group });
    }
    if text.body.is_empty() && text.placeholders.is_empty() {
        return Some(text.group.map_or(Content::Nothing, Content::GroupChange));
    }
    Some(Content::Text(text))
}

pub(super) fn parse_content(content: &[u8], timestamp: u64, is_own_account: bool) -> Option<Content> {
    let content = Message::parse(content)?;
    if let Some(data) = content.message(1) {
        return parse_data(&data, timestamp);
    }
    if let Some(edit) = content.message(11) {
        let (target, data) = (edit.varint(1)?, edit.message(2)?);
        return Some(match parse_data(&data, timestamp)? {
            Content::Text(text) => Content::Edit { target, text },
            _ => Content::Nothing,
        });
    }
    if let Some(sync) = content.message(2) {
        if !is_own_account {
            return Some(Content::Nothing);
        }
        if let Some(sent) = sync.message(1) {
            let to = read_service_id(&sent, 12, 7);
            let started = sent.varint(4).filter(|started| *started > 0);
            // A message the account sent goes to a contact or a group: one that names neither is none of the R1's.
            return Some(match (sent.message(3), sent.message(10)) {
                (Some(data), _) => match parse_data(&data, timestamp)? {
                    Content::Text(text) if to.is_some() || text.group.is_some() => Content::Sent { to, text, started },
                    Content::Reaction(reaction) if to.is_some() || reaction.group.is_some() => {
                        Content::SentReaction { to, reaction }
                    }
                    Content::Delete { target, group } if to.is_some() || group.is_some() => {
                        Content::SentDelete { to, target, group }
                    }
                    Content::AdminDelete { author, target, group } => {
                        Content::SentAdminDelete { author, target, group }
                    }
                    Content::Timer(text) if to.is_some() || text.group.is_some() => Content::SentTimer { to, text },
                    change @ (Content::GroupChange(_) | Content::GroupCall { .. }) => change,
                    Content::ProfileKey(key) => {
                        let group = read_group(&data);
                        if to.is_some() || group.is_some() {
                            Content::SentProfileKey { to, group, key }
                        } else {
                            Content::Nothing
                        }
                    }
                    _ => Content::Nothing,
                },
                (None, Some(edit)) => {
                    let target = edit.varint(1)?;
                    match edit.message(2).map(|data| parse_data(&data, timestamp)) {
                        Some(Some(Content::Text(text))) if to.is_some() || text.group.is_some() => {
                            Content::SentEdit { to, target, text }
                        }
                        Some(None) => return None,
                        _ => Content::Nothing,
                    }
                }
                _ => Content::Nothing,
            });
        }
        if let Some(blocked) = sync.message(6) {
            return Some(parse_blocked(&blocked));
        }
        let reads: Vec<(ServiceId, u64)> = sync
            .repeated(5)
            .filter_map(Message::parse)
            .filter_map(|read| Some((read_service_id(&read, 4, 3)?, read.varint(2)?)))
            .collect();
        if !reads.is_empty() {
            return Some(Content::ReadElsewhere { reads, at: timestamp });
        }
        // A Verified sync, which Signal-Desktop drops, says nothing either.
        if let Some(configuration) = sync.message(9) {
            return Some(Content::Configuration { read_receipts: configuration.bool(1).unwrap_or(false) });
        }
        if let Some(response) = sync.message(14) {
            return Some(parse_response(&response));
        }
        if let Some(fetch) = sync.message(12) {
            return Some(if fetch.varint(1) == Some(STORAGE_MANIFEST) {
                Content::FetchStorage
            } else {
                Content::Nothing
            });
        }
        if let Some(keys) = sync.message(13) {
            let master = keys.bytes(2).and_then(|key| key.try_into().ok());
            let pool = keys.text(3).filter(|pool| !pool.is_empty()).map(String::from);
            return Some(Content::Keys { master, pool });
        }
        if let Some(logged) = call::read_sync(&sync) {
            return Some(logged.map_or_else(Content::LogChange, Content::Logged));
        }
        if let Some(delete) = sync.message(22) {
            return Some(parse_delete_for_me(&delete));
        }
        return Some(Content::Nothing);
    }
    // After a sync and a DataMessage, before a receipt, as Signal-Desktop takes a Content (`innerHandleContentMessage`).
    if let Some((signals, device)) = call::read(&content) {
        return Some(Content::Call { signals, device });
    }
    if let Some(receipt) = content.message(5) {
        let is_read = matches!(receipt.varint(1).unwrap_or(DELIVERY), READ | VIEWED);
        return Some(Content::Receipt { is_read, timestamps: receipt.varints(2) });
    }
    Some(Content::Nothing)
}

fn read_identifier(
    message: &Message,
    (binary, text, number, group): (u32, u32, u32, Option<u32>),
) -> Option<Identifier> {
    if let Some(id) = group.and_then(|field| message.bytes(field)) {
        return Some(Identifier::Group(id.try_into().ok()?));
    }
    if let Some(service) = read_service_id(message, binary, text) {
        return Some(Identifier::Service(service));
    }
    message.text(number).filter(|number| !number.is_empty()).map(|number| Identifier::Number(String::from(number)))
}

const THREAD: (u32, u32, u32, Option<u32>) = (4, 1, 3, Some(2));
const AUTHOR: (u32, u32, u32, Option<u32>) = (4, 1, 2, None);
const SENT_TIMESTAMP: u32 = 3;

fn read_addressable(message: &Message, field: u32) -> Vec<(Identifier, u64)> {
    let read_message =
        |addressable: Message| Some((read_identifier(&addressable, AUTHOR)?, addressable.varint(SENT_TIMESTAMP)?));
    message.repeated(field).filter_map(Message::parse).filter_map(read_message).collect()
}

fn write_identifier(identifier: &Identifier, (binary, _, number, group): (u32, u32, u32, Option<u32>)) -> Writer {
    match (identifier, group) {
        (Identifier::Service(service), _) => Writer::new().bytes(binary, &service.to_binary()),
        (Identifier::Number(e164), _) => Writer::new().text(number, e164),
        (Identifier::Group(id), Some(group)) => Writer::new().bytes(group, id),
        (Identifier::Group(_), None) => Writer::new(),
    }
}

pub(super) fn write_delete_for_me(parts: &[DeleteForMe]) -> Vec<u8> {
    let addressable = |writer: Writer, field: u32, messages: &[(Identifier, u64)]| {
        messages.iter().fold(writer, |writer, (author, timestamp)| {
            writer.message(field, write_identifier(author, AUTHOR).varint(SENT_TIMESTAMP, *timestamp))
        })
    };
    let conversation = |thread: &Identifier| Writer::new().message(1, write_identifier(thread, THREAD));
    let delete = parts.iter().fold(Writer::new(), |delete, part| match part {
        DeleteForMe::Messages { thread, messages } => delete.message(1, addressable(conversation(thread), 2, messages)),
        DeleteForMe::Conversation { thread, newest, non_expiring, is_full } => {
            let part = addressable(addressable(conversation(thread), 2, newest), 4, non_expiring).bool(3, *is_full);
            delete.message(2, part)
        }
        DeleteForMe::Local { thread } => delete.message(3, conversation(thread)),
    });
    Writer::new().message(2, Writer::new().message(22, delete)).finish()
}

fn parse_delete_for_me(delete: &Message) -> Content {
    let mut parts = Vec::new();
    let read_thread = |part: &Message| part.message(1).and_then(|thread| read_identifier(&thread, THREAD));
    for part in delete.repeated(1).filter_map(Message::parse) {
        let (thread, messages) = (read_thread(&part), read_addressable(&part, 2));
        if let Some(thread) = thread.filter(|_| !messages.is_empty()) {
            parts.push(DeleteForMe::Messages { thread, messages });
        }
    }
    for part in delete.repeated(2).filter_map(Message::parse) {
        let (newest, non_expiring) = (read_addressable(&part, 2), read_addressable(&part, 4));
        let is_full = part.bool(3).unwrap_or(false);
        if let Some(thread) = read_thread(&part).filter(|_| !newest.is_empty()) {
            parts.push(DeleteForMe::Conversation { thread, newest, non_expiring, is_full });
        }
    }
    for local in delete.repeated(3).filter_map(Message::parse) {
        if let Some(thread) = read_thread(&local) {
            parts.push(DeleteForMe::Local { thread });
        }
    }
    if parts.is_empty() { Content::Nothing } else { Content::DeleteForMe(parts) }
}

pub(super) fn parse_pni_change(content: &[u8]) -> Option<PniChange> {
    let change = Message::parse(content)?.message(2)?.message(18)?;
    let pair = |public: &[u8], private: &[u8]| -> Option<Pair> {
        Some(Pair { private: private.try_into().ok()?, public: decode_key(public)? })
    };
    let identity = Message::parse(change.bytes(1)?)?;
    let identity = pair(identity.bytes(1)?, identity.bytes(2)?)?;
    let record = Message::parse(change.bytes(2)?)?;
    let signature = |record: &Message| -> Option<[u8; 64]> { record.bytes(4)?.try_into().ok() };
    let signed_pre_key = (
        record.u32(1)?,
        pair(record.bytes(2)?, record.bytes(3)?)?,
        signature(&record)?,
        record.fixed64(5).unwrap_or(0),
    );
    let last_resort = change.bytes(5).and_then(Message::parse).and_then(|record| {
        let public = decode_kyber(record.bytes(2)?)?.to_vec();
        let private = decode_kyber(record.bytes(3)?)?.to_vec();
        Some((record.u32(1)?, private, public, signature(&record)?, record.fixed64(5).unwrap_or(0)))
    });
    let number = String::from(change.text(4).filter(|number| !number.is_empty())?);
    Some(PniChange { identity, signed_pre_key, last_resort, registration: change.u32(3)?, number })
}

pub(super) fn parse_pni_signature(content: &[u8]) -> Option<([u8; 16], [u8; 64])> {
    let message = Message::parse(content)?.message(10)?;
    Some((message.bytes(1)?.try_into().ok()?, message.bytes(2)?.try_into().ok()?))
}

fn parse_aci(bytes: &[u8]) -> Option<ServiceId> {
    Some(ServiceId::Aci(bytes.try_into().ok()?))
}

fn parse_blocked(blocked: &Message) -> Content {
    let has_any = |fields: &[u32]| fields.iter().any(|field| blocked.repeated(*field).next().is_some());
    let groups =
        |field| -> Vec<[u8; GROUP_ID_BYTES]> { blocked.repeated(field).filter_map(|id| id.try_into().ok()).collect() };
    if has_any(&[5, 6, 7]) || !has_any(&[1, 2, 3, 4]) {
        let acis =
            blocked.repeated(6).filter_map(Message::parse).filter_map(|item| parse_aci(item.bytes(1)?)).collect();
        let groups =
            blocked.repeated(7).filter_map(Message::parse).filter_map(|item| item.bytes(1)?.try_into().ok()).collect();
        return Content::Blocked { acis: Some(acis), groups };
    }
    let acis = if has_any(&[4]) {
        Some(blocked.repeated(4).filter_map(parse_aci).collect())
    } else if has_any(&[3]) {
        let texts = blocked.repeated(3).filter_map(|text| ServiceId::from_text(core::str::from_utf8(text).ok()?));
        Some(texts.filter(|id| matches!(id, ServiceId::Aci(_))).collect())
    } else {
        None
    };
    Content::Blocked { acis, groups: groups(2) }
}

fn parse_response(response: &Message) -> Content {
    let kind = response.u32(4).filter(|kind| *kind <= BLOCK_AND_SPAM).unwrap_or(0);
    let thread = match response.bytes(3).filter(|id| !id.is_empty()) {
        Some(id) => id.try_into().ok().map(Peer::Group),
        None => response
            .bytes(5)
            .and_then(parse_aci)
            .or_else(|| ServiceId::from_text(response.text(2)?).filter(|id| matches!(id, ServiceId::Aci(_))))
            .map(Peer::Contact),
    };
    thread.map_or(Content::Nothing, |thread| Content::Response { thread, kind })
}

pub(super) const ACCEPT: u32 = 1;
pub(super) const DELETE: u32 = 2;
pub(super) const BLOCK: u32 = 3;
pub(super) const BLOCK_AND_DELETE: u32 = 4;
pub(super) const BLOCK_AND_SPAM: u32 = 6;

pub(super) fn parse_handled_group(content: &[u8]) -> Option<Option<Context>> {
    let data = Message::parse(content)?.message(1)?;
    let group = read_group(&data);
    let is_separate = SEPARATE_FIELDS.iter().any(|field| data.has(*field));
    let is_profile_key_update = data.varint(4).unwrap_or(0) & PROFILE_KEY_UPDATE != 0;
    (!is_separate && !is_profile_key_update).then_some(group)
}

pub(super) fn is_dispatched(content: &[u8]) -> bool {
    let Some(content) = Message::parse(content) else { return false };
    content.has(11) || content.message(1).is_some_and(|data| data.varint(4).unwrap_or(0) & PROFILE_KEY_UPDATE == 0)
}

pub(super) fn write_profile_key_update(key: &[u8; 32], timestamp: u64) -> Vec<u8> {
    let data =
        Writer::new().varint(4, PROFILE_KEY_UPDATE).bytes(6, key).varint(7, timestamp).varint(12, 0).bool(14, false);
    write_message(data)
}

pub(super) fn parse_group_context(content: &[u8]) -> Option<Context> {
    let content = Message::parse(content)?;
    let data = content.message(1).or_else(|| content.message(11)?.message(2))?;
    read_group(&data)
}

pub(super) fn parse_distribution(content: &[u8]) -> Option<Vec<u8>> {
    Message::parse(content)?.bytes(7).map(<[u8]>::to_vec)
}

pub(super) fn write_distribution(distribution: &[u8]) -> Vec<u8> {
    Writer::new().bytes(7, distribution).finish()
}

pub(super) fn with_distribution(content: &[u8], distribution: &[u8]) -> Vec<u8> {
    [content, &write_distribution(distribution)].concat()
}

// A Content padded as Signal's clients pad one: the end byte, then noughts to one short of a multiple of 80 bytes.
pub(super) fn pad(plain: &[u8]) -> Vec<u8> {
    let length = (plain.len() + 2).div_ceil(80) * 80 - 1;
    let mut out = plain.to_vec();
    out.push(PLAINTEXT_END);
    out.resize(length, 0);
    out
}

pub(super) fn unpad(padded: &[u8]) -> Option<&[u8]> {
    for at in (0..padded.len()).rev() {
        match padded[at] {
            0 => continue,
            PLAINTEXT_END => return Some(&padded[..at]),
            _ => return None,
        }
    }
    Some(padded)
}

pub(super) fn parse_decryption_error(bytes: &[u8]) -> Option<DecryptionError> {
    let body = bytes.strip_prefix(&[PLAINTEXT_MARK])?;
    let content = Message::parse(unpad(body)?)?;
    let error = Message::parse(content.bytes(8)?)?;
    let ratchet = match error.bytes(1) {
        Some(key) => Some(decode_key(key)?),
        None => None,
    };
    Some(DecryptionError { ratchet, timestamp: error.varint(2)?, device: error.u32(3).unwrap_or(0) })
}

pub(super) fn write_data(
    body: &str,
    timestamp: u64,
    profile_key: Option<&[u8; 32]>,
    (expire, expire_version): (u32, u32),
    group: Option<(&[u8; 32], u32)>,
) -> Writer {
    Writer::new()
        .text(1, body)
        .varint(4, 0)
        .maybe((expire > 0).then_some(move |w: Writer| w.varint(5, u64::from(expire))))
        .maybe(group.is_none().then_some(move |w: Writer| w.varint(23, u64::from(expire_version))))
        .maybe(profile_key.map(|key| move |w: Writer| w.bytes(6, key)))
        .varint(7, timestamp)
        .varint(12, 0)
        .bool(14, false)
        .maybe(group.map(|(key, revision)| move |w: Writer| w.message(15, Context::write(key, revision))))
}

// A DataMessage with the pointers of the attachments a CDN holds, in their order.
pub(super) fn write_attached(data: Writer, attachments: &[Attachment]) -> Writer {
    attachments.iter().filter_map(Attachment::pointer).fold(data, |data, pointer| data.message(2, pointer))
}

pub(super) fn write_message(data: Writer) -> Vec<u8> {
    Writer::new().message(1, data).finish()
}

pub(super) fn write_transcript(
    to: Option<&ServiceId>,
    recipients: &[(ServiceId, bool)],
    (timestamp, started): (u64, u64),
    data: Writer,
    padding: &[u8],
) -> Vec<u8> {
    let mut sent = Writer::new().varint(2, timestamp).message(3, data).varint(4, started);
    for (recipient, is_sealed) in recipients {
        sent = sent.message(5, Writer::new().bool(2, *is_sealed).bytes(6, &recipient.to_binary()));
    }
    let sent = sent.bool(6, false).maybe(to.map(|to| move |w: Writer| w.bytes(12, &to.to_binary())));
    Writer::new().message(2, Writer::new().message(1, sent).bytes(8, padding)).finish()
}

pub(super) fn write_decryption_error(error: &DecryptionError) -> Vec<u8> {
    let message = Writer::new()
        .maybe(error.ratchet.map(|key| move |w: Writer| w.bytes(1, &encode_key(&key))))
        .varint(2, error.timestamp)
        .varint(3, u64::from(error.device))
        .finish();
    let content = Writer::new().bytes(8, &message).finish();
    [&[PLAINTEXT_MARK][..], &content, &[PLAINTEXT_END]].concat()
}

pub(super) fn write_null(padding: &[u8]) -> Vec<u8> {
    Writer::new().message(4, Writer::new().bytes(1, padding)).finish()
}

pub(super) fn write_receipt(is_read: bool, timestamps: &[u64]) -> Vec<u8> {
    let kind = if is_read { READ } else { DELIVERY };
    let receipt =
        timestamps.iter().fold(Writer::new().varint(1, kind), |receipt, &timestamp| receipt.varint(2, timestamp));
    Writer::new().message(5, receipt).finish()
}

pub(super) fn write_sync_request(kind: u64, padding: &[u8]) -> Vec<u8> {
    let sync = Writer::new().message(4, Writer::new().varint(1, kind)).bytes(8, padding);
    Writer::new().message(2, sync).finish()
}

pub(super) fn write_read_sync(reads: &[(ServiceId, u64)], padding: &[u8]) -> Vec<u8> {
    let sync = reads.iter().fold(Writer::new(), |sync, (sender, timestamp)| {
        sync.message(5, Writer::new().varint(2, *timestamp).bytes(4, &sender.to_binary()))
    });
    Writer::new().message(2, sync.bytes(8, padding)).finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const ALICE: ServiceId = ServiceId::Aci([0xA1; 16]);

    #[test]
    fn envelope_ids() {
        let bytes = Writer::new()
            .varint(1, 6)
            .text(11, "a1a1a1a1-a1a1-a1a1-a1a1-a1a1a1a1a1a1")
            .varint(7, 3)
            .bytes(20, &ALICE.to_binary())
            .varint(5, 1700)
            .bytes(8, b"sealed")
            .bytes(21, &[9; 16])
            .varint(10, 1701)
            .finish();
        let envelope = Envelope::read(&bytes).unwrap();
        assert_eq!(envelope.kind, UNIDENTIFIED_SENDER);
        assert_eq!(envelope.source, Some(ALICE));
        assert_eq!((envelope.source_device, envelope.destination), (3, Some(ALICE)));
        assert_eq!((envelope.timestamp, envelope.server_timestamp, envelope.guid.len()), (1700, 1701, 16));
    }

    fn fields(data: &Message) -> Vec<(u32, Vec<u8>)> {
        let mut pairs = Vec::new();
        for number in 1..=30 {
            if let Some(value) = data.varint(number) {
                pairs.push((number, value.to_le_bytes().to_vec()));
            } else if let Some(bytes) = data.bytes(number) {
                pairs.push((number, bytes.to_vec()));
            }
        }
        pairs
    }

    #[test]
    fn contact_message_fields() {
        let key = [7u8; 32];
        let content = write_message(write_data("hello", 1700, Some(&key), (0, 3), None));
        let data = Message::parse(&content).unwrap().message(1).unwrap();
        let number = |value: u64| value.to_le_bytes().to_vec();
        assert_eq!(
            fields(&data),
            vec![
                (1, b"hello".to_vec()),
                (4, number(0)),
                (6, key.to_vec()),
                (7, number(1700)),
                (12, number(0)),
                (14, number(0)),
                (23, number(3)),
            ],
            "body, no flags, the profile key, the timestamp, the protocol version and view-once as SendMessage's toProto writes them; no timer, but its version"
        );
        let unshared = write_message(write_data("hello", 1700, None, (3600, 4), None));
        let unshared = Message::parse(&unshared).unwrap().message(1).unwrap();
        assert_eq!(
            (unshared.bytes(6), unshared.varint(5), unshared.varint(23)),
            (None, Some(3600), Some(4)),
            "no profile key where it is not shared"
        );
        let to_group = write_message(write_data("hi all", 1700, Some(&key), (3600, 4), Some((&[3; 32], 9))));
        let to_group = Message::parse(&to_group).unwrap().message(1).unwrap();
        assert_eq!(
            (to_group.varint(5), to_group.varint(23), to_group.message(15).and_then(|g| Context::read(&g))),
            (Some(3600), None, Context::read(&Message::parse(&Context::write(&[3; 32], 9).finish()).unwrap())),
            "a group's carries the timer but never its version"
        );
    }

    #[test]
    fn transcript_fields() {
        let key = [7u8; 32];
        let own = ServiceId::Aci([3; 16]);
        let to_self = write_transcript(
            Some(&own),
            &[],
            (1700, 1705),
            write_data("note", 1700, Some(&key), (0, 1), None),
            &[0; 3],
        );
        let content = Message::parse(&to_self).unwrap();
        assert!(content.message(1).is_none(), "no DataMessage of its own: the transcript alone");
        let sent = content.message(2).unwrap().message(1).unwrap();
        assert_eq!(
            (sent.varint(2), sent.varint(4), sent.bytes(12), sent.repeated(5).count(), sent.bool(6)),
            (Some(1700), Some(1705), Some(&own.to_binary()[..]), 0, Some(false)),
            "the timestamp, the timer's start, the account itself as the destination and no recipient's status"
        );
        assert_eq!(
            parse_content(&to_self, 1700, true),
            Some(Content::Sent {
                to: Some(own),
                text: Text {
                    timestamp: 1700,
                    body: "note".into(),
                    profile_key: Some(key),
                    expire_version: 1,
                    ..Text::default()
                },
                started: Some(1705)
            }),
            "read back by another device as the owner's own message"
        );
        assert_eq!(
            parse_content(&to_self, 1700, false),
            Some(Content::Nothing),
            "a transcript counts from the account alone"
        );
        let copy = write_transcript(
            Some(&ALICE),
            &[(ALICE, true)],
            (1700, 1800),
            write_data("hello", 1700, None, (0, 1), None),
            &[0; 3],
        );
        let sent = Message::parse(&copy).unwrap().message(2).unwrap().message(1).unwrap();
        let statuses: Vec<(ServiceId, Option<bool>)> = sent
            .repeated(5)
            .filter_map(Message::parse)
            .filter_map(|status| Some((ServiceId::from_binary(status.bytes(6)?)?, status.bool(2))))
            .collect();
        assert_eq!(statuses, vec![(ALICE, Some(true))], "it names whom the message went to, and that it went sealed");
        let read = write_read_sync(&[(ALICE, 1690)], &[]);
        assert_eq!(
            parse_content(&read, 1750, true),
            Some(Content::ReadElsewhere { reads: vec![(ALICE, 1690)], at: 1750 })
        );
    }

    #[test]
    fn data_message_refusals() {
        let sent = write_message(write_data("hello", 1700, None, (0, 1), None));
        assert!(matches!(parse_content(&sent, 1700, false), Some(Content::Text(_))));
        assert_eq!(parse_content(&sent, 1701, false), None, "another timestamp than the envelope's");
        let no_timestamp = Writer::new().message(1, Writer::new().text(1, "hello")).finish();
        assert_eq!(parse_content(&no_timestamp, 1700, false), None, "no timestamp");
        let with_flags = |flags: u64| {
            let data = Writer::new().varint(4, flags).varint(5, 60).varint(23, 2).bytes(6, &[5; 32]).varint(7, 1700);
            parse_content(&write_message(data), 1700, false)
        };
        assert!(
            matches!(with_flags(2), Some(Content::Timer(ref text)) if (text.expire, text.expire_version) == (60, 2))
        );
        assert_eq!(with_flags(4), Some(Content::ProfileKey([5; 32])));
        assert_eq!(with_flags(1), Some(Content::EndSession));
        assert_eq!(with_flags(8), None, "a flag Signal-Desktop does not know");
        let transcript_of_timer = write_transcript(
            Some(&ALICE),
            &[(ALICE, false)],
            (1700, 1700),
            Writer::new().varint(4, 2).varint(23, 2).varint(7, 1700),
            &[],
        );
        assert!(
            matches!(parse_content(&transcript_of_timer, 1700, true), Some(Content::SentTimer { to: Some(ALICE), ref text }) if text.expire == 0),
            "the owner turned the timer off on the phone"
        );
    }

    #[test]
    fn group_message_fields() {
        const BORIS: ServiceId = ServiceId::Aci([0xB0; 16]);
        let to_group = || write_data("hi all", 1700, Some(&[7; 32]), (0, 1), Some((&[3; 32], 4)));
        let Some(Content::Text(text)) = parse_content(&write_message(to_group()), 1700, false) else {
            panic!("a text")
        };
        let group = text.group.expect("to a group");
        assert_eq!((group.master_key, group.revision, text.body.as_str()), ([3; 32], 4, "hi all"));
        let copy = write_transcript(None, &[(ALICE, false), (BORIS, true)], (1700, 1700), to_group(), &[0; 3]);
        let Some(Content::Sent { to: None, text, .. }) = parse_content(&copy, 1700, true) else {
            panic!("the owner's message to the group")
        };
        assert_eq!(text.group.map(|group| group.revision), Some(4));
        let no_recipient =
            write_transcript(None, &[(ALICE, false)], (1700, 1700), write_data("lost", 1700, None, (0, 1), None), &[]);
        assert_eq!(
            parse_content(&no_recipient, 1700, true),
            Some(Content::Nothing),
            "a message to nobody is none of the R1's"
        );
        let change =
            Writer::new().message(1, Writer::new().varint(7, 1700).message(15, Context::write(&[3; 32], 5))).finish();
        assert!(
            matches!(parse_content(&change, 1700, false), Some(Content::GroupChange(ref group)) if group.revision == 5),
            "a message to a group with nothing to show changes it"
        );
        let content = Writer::new().message(1, to_group()).bytes(7, b"distribution").finish();
        assert_eq!(parse_distribution(&content).as_deref(), Some(&b"distribution"[..]), "beside what it says");
        let again = with_distribution(&write_message(to_group()), b"the R1's");
        assert!(
            parse_distribution(&again).as_deref() == Some(&b"the R1's"[..])
                && matches!(parse_content(&again, 1700, false), Some(Content::Text(_))),
            "sent again with the R1's distribution, it says what it said"
        );
        let alone = write_distribution(b"the R1's");
        assert_eq!(
            (parse_distribution(&alone).as_deref(), parse_content(&alone, 1, false)),
            (Some(&b"the R1's"[..]), Some(Content::Nothing))
        );
    }

    #[test]
    fn decryption_error_round_trip() {
        let again = DecryptionError { ratchet: Some([4; 32]), timestamp: 1700, device: 3 };
        let bytes = write_decryption_error(&again);
        let mut error = alloc::vec![0x0A, 33, 0x05];
        error.extend_from_slice(&[4; 32]);
        error.extend_from_slice(&[0x10, 0xA4, 0x0D, 0x18, 0x03]);
        let mut expected = alloc::vec![0xC0, 0x42, error.len() as u8];
        expected.extend_from_slice(&error);
        expected.push(0x80);
        assert_eq!(
            bytes, expected,
            "0xC0, Content of field 8 holding the ratchet key with its type byte, the timestamp and the device in libsignal's order, 0x80"
        );
        assert_eq!(parse_decryption_error(&bytes), Some(again));
        let sender_key = DecryptionError { ratchet: None, ..again };
        assert_eq!(
            parse_decryption_error(&write_decryption_error(&sender_key)),
            Some(sender_key),
            "a sender key message's names no key"
        );
        assert_eq!(parse_decryption_error(&bytes[1..]), None, "not marked as plaintext content");
        let empty =
            [&[PLAINTEXT_MARK][..], &write_message(write_data("hi", 1, None, (0, 1), None)), &[PLAINTEXT_END]].concat();
        assert_eq!(
            parse_decryption_error(&empty),
            None,
            "plaintext content without a DecryptionErrorMessage is dropped"
        );
        let nothing = write_null(&[1, 2, 3]);
        let padding = Message::parse(&nothing).unwrap().message(4).and_then(|null| null.bytes(1).map(<[u8]>::to_vec));
        assert_eq!(padding, Some(vec![1, 2, 3]));
        assert_eq!(parse_content(&write_null(&[1]), 5, false), Some(Content::Nothing), "a null message says nothing");
    }

    #[test]
    fn owner_sync_messages() {
        const BORIS: ServiceId = ServiceId::Aci([0xB0; 16]);
        let sync = |field: u32, inner: Writer| Writer::new().message(2, Writer::new().message(field, inner)).finish();
        let modern = Writer::new()
            .message(6, Writer::new().bytes(1, &ALICE.to_binary()).varint(2, 5))
            .message(6, Writer::new().bytes(1, &[1; 15]))
            .message(7, Writer::new().bytes(1, &[3; 32]))
            .message(7, Writer::new().bytes(1, &[4; 16]))
            .bytes(4, &BORIS.to_binary());
        assert_eq!(
            parse_content(&sync(6, modern), 1, true),
            Some(Content::Blocked { acis: Some(vec![ALICE]), groups: vec![[3; 32]] }),
            "the fields of today where any is used, a malformed ACI and a group id of 16 bytes passed over"
        );
        let legacy = Writer::new().bytes(4, &BORIS.to_binary()).text(3, &ALICE.to_text()).bytes(2, &[3; 32]);
        assert_eq!(
            parse_content(&sync(6, legacy), 1, true),
            Some(Content::Blocked { acis: Some(vec![BORIS]), groups: vec![[3; 32]] }),
            "else the deprecated ones, the ACIs in binary first"
        );
        let texts = Writer::new().text(3, &ALICE.to_text()).text(3, "PNI:a1a1a1a1-a1a1-a1a1-a1a1-a1a1a1a1a1a1");
        assert_eq!(
            parse_content(&sync(6, texts), 1, true),
            Some(Content::Blocked { acis: Some(vec![ALICE]), groups: vec![] })
        );
        let numbers = Writer::new().text(1, "+15550000000");
        assert_eq!(
            parse_content(&sync(6, numbers), 1, true),
            Some(Content::Blocked { acis: None, groups: vec![] }),
            "numbers alone leave the accounts as they are"
        );
        assert_eq!(
            parse_content(&sync(6, Writer::new()), 1, true),
            Some(Content::Blocked { acis: Some(vec![]), groups: vec![] }),
            "an empty list unblocks all"
        );
        assert_eq!(parse_content(&sync(6, Writer::new()), 1, false), Some(Content::Nothing), "from the owner alone");
        let configuration = Writer::new().bool(1, true).bool(3, true);
        assert_eq!(
            parse_content(&sync(9, configuration), 1, true),
            Some(Content::Configuration { read_receipts: true })
        );
        assert_eq!(
            parse_content(&sync(9, Writer::new()), 1, true),
            Some(Content::Configuration { read_receipts: false })
        );
        let response = |inner: Writer| parse_content(&sync(14, inner), 1, true);
        assert_eq!(
            response(Writer::new().bytes(5, &ALICE.to_binary()).varint(4, 3)),
            Some(Content::Response { thread: Peer::Contact(ALICE), kind: BLOCK })
        );
        assert_eq!(
            response(Writer::new().text(2, &ALICE.to_text()).bytes(3, &[3; 32]).varint(4, 9)),
            Some(Content::Response { thread: Peer::Group([3; 32]), kind: 0 }),
            "a group's before a contact's, a kind not known as nought"
        );
        assert_eq!(
            response(Writer::new().bytes(3, &[3; 16]).varint(4, 1)),
            Some(Content::Nothing),
            "a group id of 16 bytes"
        );
        assert_eq!(parse_content(&sync(12, Writer::new().varint(1, 2)), 1, true), Some(Content::FetchStorage));
        assert_eq!(
            parse_content(&sync(12, Writer::new().varint(1, 1)), 1, true),
            Some(Content::Nothing),
            "the local profile"
        );
        assert_eq!(
            parse_content(&sync(7, Writer::new().bytes(2, &[5; 33])), 1, true),
            Some(Content::Nothing),
            "a Verified sync, dropped"
        );
        let bytes = write_sync_request(BLOCKED, &[0; 2]);
        let request = Message::parse(&bytes).unwrap().message(2).unwrap();
        assert_eq!((request.message(4).unwrap().varint(1), request.bytes(8)), (Some(3), Some(&[0u8, 0][..])));
        let profile_key_update = Writer::new().varint(4, 4).bytes(6, &[6; 32]).varint(7, 1700);
        let profile = write_transcript(Some(&ALICE), &[(ALICE, false)], (1700, 1700), profile_key_update, &[]);
        assert_eq!(
            parse_content(&profile, 1700, true),
            Some(Content::SentProfileKey { to: Some(ALICE), group: None, key: [6; 32] }),
            "the owner's profile key sent alone from the phone"
        );
    }

    #[test]
    fn handled_group() {
        let group = Context::write(&[3; 32], 4);
        let to_group = |extra: Writer| write_message(extra.varint(7, 1700).message(15, Context::write(&[3; 32], 4)));
        let context = Context::read(&Message::parse(&group.finish()).unwrap());
        assert_eq!(
            parse_handled_group(&write_message(write_data("hi", 1700, None, (0, 1), None))),
            Some(None),
            "a contact's text"
        );
        assert_eq!(parse_handled_group(&to_group(Writer::new().text(1, "hi"))), Some(context.clone()), "a group's");
        assert_eq!(parse_handled_group(&to_group(Writer::new())), Some(context.clone()), "a group's change, empty");
        for field in [16, 17, 25, 26, 27, 28, 29] {
            assert_eq!(
                parse_handled_group(&to_group(Writer::new().message(field, Writer::new()))),
                None,
                "field {field} goes apart"
            );
        }
        assert_eq!(
            parse_handled_group(&to_group(Writer::new().message(19, Writer::new()))),
            Some(context.clone()),
            "a group call's update to a group is held to its group's rules, as handleDataMessage holds it"
        );
        let direct_call = write_message(Writer::new().message(19, Writer::new()).varint(7, 1700));
        assert_eq!(parse_handled_group(&direct_call), Some(None), "and one to a contact as a contact's");
        assert_eq!(parse_handled_group(&to_group(Writer::new().varint(4, 4))), None, "a profile key's update");
        let edit = Writer::new()
            .message(
                11,
                Writer::new()
                    .varint(1, 1600)
                    .message(2, Writer::new().varint(7, 1700).message(15, Context::write(&[3; 32], 4))),
            )
            .finish();
        assert_eq!(
            (parse_handled_group(&edit), parse_group_context(&edit)),
            (None, context),
            "an edit goes apart, to its group"
        );
        let receipt = write_receipt(false, &[1, 2]);
        let receipt = Message::parse(&receipt).unwrap().message(5).unwrap();
        assert_eq!((receipt.varint(1), receipt.varints(2)), (Some(0), vec![1, 2]), "a delivery receipt");
    }

    #[test]
    fn call_contents() {
        use crate::signal::call::{Kind, Outcome};
        let offer = Signal::Offer { call: 7, is_video: true, parameters: None };
        assert_eq!(
            parse_content(&offer.write(Some(3)), 1700, false),
            Some(Content::Call { signals: alloc::vec![offer.clone()], device: Some(3) }),
            "a call message's signals and the device they are for"
        );
        let with_data = [write_message(write_data("hi", 1700, None, (0, 1), None)), offer.write(None)].concat();
        assert!(
            matches!(parse_content(&with_data, 1700, false), Some(Content::Text(_))),
            "a DataMessage before a call message, as Signal-Desktop takes a Content"
        );
        let update = |era: Option<&str>| {
            let call = Writer::new().maybe(era.map(|era| move |w: Writer| w.text(ERA, era)));
            Writer::new().message(GROUP_CALL_UPDATE, call).varint(7, 1700)
        };
        let to_group = |data: Writer| write_message(data.message(15, Context::write(&[3; 32], 4)));
        let context = Context::read(&Message::parse(&Context::write(&[3; 32], 4).finish()).unwrap()).unwrap();
        assert_eq!(
            parse_content(&to_group(update(Some("1122334455667788"))), 1700, false),
            Some(Content::GroupCall { group: context.clone(), era: Some("1122334455667788".into()), timestamp: 1700 }),
            "a group call seen by its era, with the group's revision"
        );
        assert_eq!(
            parse_content(&to_group(update(None)), 1700, false),
            Some(Content::GroupCall { group: context, era: None, timestamp: 1700 }),
            "one that names no era, its revision still noted"
        );
        assert_eq!(
            parse_content(&write_message(update(Some("e"))), 1700, false),
            Some(Content::Nothing),
            "to no group"
        );
        let sent =
            write_transcript(None, &[], (1700, 1700), update(Some("e")).message(15, Context::write(&[3; 32], 4)), &[]);
        assert!(
            matches!(parse_content(&sent, 1700, true), Some(Content::GroupCall { ref era, .. }) if era.as_deref() == Some("e")),
            "the owner's from another device"
        );
        let logged = Logged {
            peer: Peer::Contact(ALICE),
            call: 9,
            timestamp: 1,
            kind: Kind::Audio,
            is_outgoing: false,
            outcome: Outcome::Accepted,
        };
        assert_eq!(parse_content(&logged.write(&[]), 5, true), Some(Content::Logged(logged)), "a call's sync");
        assert_eq!(parse_content(&logged.write(&[]), 5, false), Some(Content::Nothing), "from the account alone");
        let change = LogChange { kind: 1, timestamp: 4, target: None };
        assert_eq!(parse_content(&change.write(&[]), 5, true), Some(Content::LogChange(change)), "a change of the log");
        assert_eq!(
            alloc::format!("{:?}", parse_content(&offer.write(Some(3)), 1700, false).unwrap()),
            "Call { signals: 1, device: Some(3) }",
            "told by its count of signals"
        );
    }

    #[test]
    fn edits_deletions_attachments() {
        let edit = Writer::new()
            .message(11, Writer::new().varint(1, 1600).message(2, write_data("fixed", 1700, None, (0, 1), None)))
            .finish();
        assert!(
            matches!(parse_content(&edit, 1700, false), Some(Content::Edit { target: 1600, ref text }) if text.body == "fixed")
        );
        let delete =
            Writer::new().message(1, Writer::new().message(17, Writer::new().varint(1, 1600)).varint(7, 1700)).finish();
        assert_eq!(parse_content(&delete, 1700, false), Some(Content::Delete { target: 1600, group: None }));
        for (kind, flags, placeholder) in
            [("image/jpeg", 0, Placeholder::Photo), ("audio/aac", 1, Placeholder::VoiceNote)]
        {
            let pointer = Writer::new().text(2, kind).varint(8, flags);
            let content = Writer::new().message(1, Writer::new().message(2, pointer).varint(7, 1700)).finish();
            assert!(
                matches!(parse_content(&content, 1700, false), Some(Content::Text(ref text)) if text.placeholders == [placeholder])
            );
        }
        let reaction = Writer::new()
            .message(
                1,
                Writer::new().varint(7, 1700).message(15, Context::write(&[0; 32], 1)).message(16, Writer::new()),
            )
            .finish();
        assert_eq!(parse_content(&reaction, 1700, false), Some(Content::Nothing), "a reaction that names nothing");
        let given = |fields: Writer| {
            parse_content(&write_message(Writer::new().message(REACTION, fields).varint(7, 1700)), 1700, false)
        };
        let named = || Writer::new().varint(5, 1600).bytes(6, &ALICE.to_binary());
        let reacted = |emoji: Option<&str>| {
            Some(Content::Reaction(Reacted {
                target: 1600,
                author: ALICE,
                emoji: emoji.map(String::from),
                timestamp: 1700,
                group: None,
            }))
        };
        assert_eq!(given(named().text(1, "\u{2764}\u{FE0F}")), reacted(Some("\u{2764}\u{FE0F}")));
        assert_eq!(
            given(Writer::new().varint(5, 1600).text(4, &ALICE.to_text()).text(1, "\u{1F44D}\u{1F3FD}")),
            reacted(Some("\u{1F44D}\u{1F3FD}")),
            "its message's author as text, the emoji with its tone"
        );
        assert_eq!(given(named().text(1, "\u{2764}").bool(2, true)), reacted(None), "one taken back");
        for (fields, why) in [
            (named().text(1, "ok"), "no emoji"),
            (named().text(1, "\u{1F44D}\u{1F44D}"), "two"),
            (named(), "none"),
            (Writer::new().varint(5, 1600).text(1, "\u{1F44D}"), "no author"),
            (Writer::new().bytes(6, &ALICE.to_binary()).text(1, "\u{1F44D}"), "no message"),
        ] {
            assert_eq!(given(fields), Some(Content::Nothing), "{why}");
        }
        let sent = write_transcript(
            Some(&ALICE),
            &[],
            (1700, 1700),
            Writer::new().message(REACTION, named().text(1, "\u{1F44D}")).varint(7, 1700),
            &[],
        );
        assert!(
            matches!(parse_content(&sent, 1700, true), Some(Content::SentReaction { to: Some(ALICE), ref reaction }) if reaction.emoji.as_deref() == Some("\u{1F44D}")),
            "the owner's, from another device"
        );
        assert_eq!(
            alloc::format!("{:?}", reacted(Some("\u{1F44D}"))),
            "Some(Reaction(Reacted { target: 1600, is_taken_back: false, .. }))",
            "its emoji is told nowhere"
        );
        let receipt = write_receipt(true, &[1, 2]);
        assert_eq!(parse_content(&receipt, 0, false), Some(Content::Receipt { is_read: true, timestamps: vec![1, 2] }));
        assert_eq!(
            parse_content(&write_receipt(false, &[3]), 0, false),
            Some(Content::Receipt { is_read: false, timestamps: vec![3] })
        );
        let viewed = Writer::new().message(5, Writer::new().varint(1, VIEWED).varint(2, 4)).finish();
        assert_eq!(
            parse_content(&viewed, 0, false),
            Some(Content::Receipt { is_read: true, timestamps: vec![4] }),
            "viewed as read"
        );
    }

    #[test]
    fn attachment_pointers() {
        use crate::signal::attachment::{Enclosure, KEYS};
        let mut photo = Enclosure::photo("a.jpg".into(), (4, 3)).attach([3; KEYS]);
        let unsent = Enclosure::said(b"sound".to_vec()).attach([4; KEYS]);
        photo.note_uploaded(2, "key".into(), [5; 32], 1_600);
        let content =
            write_message(write_attached(write_data("look", 1700, None, (0, 1), None), &[photo.clone(), unsent]));
        let Some(Content::Text(text)) = parse_content(&content, 1700, false) else { panic!("a text") };
        let read: Vec<(Option<alloc::string::String>, Option<&str>)> =
            text.attachments.iter().map(|attachment| (attachment.address(), attachment.file.as_deref())).collect();
        assert_eq!(
            (read, text.placeholders),
            (vec![(photo.address(), None)], vec![Placeholder::Photo]),
            "the one a CDN holds goes, and reads back as its pointer names it"
        );
        let unnamed = Writer::new().text(2, "image/jpeg").bytes(3, &[3; 64]).varint(4, 9).bytes(6, &[6; 32]);
        let content = write_message(Writer::new().message(2, unnamed).varint(7, 1700));
        let Some(Content::Text(text)) = parse_content(&content, 1700, false) else { panic!("a text") };
        assert_eq!(
            (text.attachments.len(), text.placeholders),
            (0, vec![Placeholder::Photo]),
            "a pointer that names no place is a word alone"
        );
        assert_eq!(placeholder_of("audio/mp4", false), Placeholder::VoiceNote);
        assert_eq!(placeholder_of("image/png", true), Placeholder::VoiceNote, "the flag first");
    }

    #[test]
    fn mentions_quotes_and_later_versions() {
        let mention = Writer::new().varint(2, 1).bytes(5, &ALICE.to_binary());
        let style = Writer::new().varint(1, 2).varint(2, 3).varint(4, 1);
        let quoted = Writer::new().varint(1, 1600).bytes(8, &ALICE.to_binary()).text(3, "Lunch?");
        let reply = Writer::new().text(1, "\u{FFFC} yes").message(18, mention).message(18, style).message(8, quoted);
        let Some(Content::Text(text)) = parse_content(&write_message(reply.varint(7, 1700)), 1700, false) else {
            panic!("a text")
        };
        assert_eq!(
            (text.mentions, text.quote),
            (
                vec![Mention { start: 0, length: 1, aci: ALICE }],
                Some(Quote { timestamp: 1600, author: ALICE, text: "Lunch?".into() })
            ),
            "the mention at the first letter, a style passed over, and the message quoted"
        );
        let shared = Writer::new().message(SHARED_CONTACT, Writer::new()).varint(7, 1700);
        assert!(
            matches!(parse_content(&write_message(shared), 1700, false), Some(Content::Text(ref text)) if text.placeholders == [Placeholder::Contact])
        );
        let poll = Writer::new().message(POLL, Writer::new().text(1, "Pizza?").text(3, "Yes")).varint(7, 1700);
        assert!(
            matches!(parse_content(&write_message(poll), 1700, false), Some(Content::Text(ref text)) if text.body == "Pizza?" && text.placeholders == [Placeholder::Poll]),
            "a poll by its question"
        );
        let later = |version| Writer::new().text(1, "secret").varint(12, version).varint(7, 1700);
        let Some(Content::Text(newer)) = parse_content(&write_message(later(PROTOCOL_VERSION + 1)), 1700, false) else {
            panic!("a text")
        };
        assert_eq!(
            (newer.body.as_str(), newer.placeholders),
            ("", vec![Placeholder::Unsupported]),
            "a later version: nothing but that it cannot be shown"
        );
        assert!(
            matches!(parse_content(&write_message(later(PROTOCOL_VERSION)), 1700, false), Some(Content::Text(ref text)) if text.body == "secret")
        );
    }

    #[test]
    fn admin_delete() {
        let group = Context::write(&[3; 32], 4);
        let context = Context::read(&Message::parse(&group.finish()).unwrap());
        let deletion = |author: &[u8]| {
            Writer::new()
                .message(ADMIN_DELETE, Writer::new().bytes(1, author).varint(2, 1600))
                .varint(7, 1700)
                .message(15, Context::write(&[3; 32], 4))
        };
        assert_eq!(
            parse_content(&write_message(deletion(&ALICE.to_binary())), 1700, false),
            Some(Content::AdminDelete { author: ALICE, target: 1600, group: context.clone() })
        );
        assert_eq!(
            parse_content(&write_message(deletion(&[1; 3])), 1700, false),
            None,
            "one that names no author is refused"
        );
        let sent = write_transcript(None, &[], (1700, 1700), deletion(&ALICE.to_binary()), &[]);
        assert_eq!(
            parse_content(&sent, 1700, true),
            Some(Content::SentAdminDelete { author: ALICE, target: 1600, group: context }),
            "the owner's, from the phone"
        );
        assert_eq!(
            parse_handled_group(&write_message(deletion(&ALICE.to_binary()))),
            None,
            "taken apart from handleDataMessage"
        );
    }

    #[test]
    fn keys_deletions_and_number_change() {
        let sync = |field: u32, inner: Writer| Writer::new().message(2, Writer::new().message(field, inner)).finish();
        assert_eq!(
            parse_content(&sync(13, Writer::new().bytes(2, &[5; 32])), 1, true),
            Some(Content::Keys { master: Some([5; 32]), pool: None })
        );
        assert_eq!(
            parse_content(&sync(13, Writer::new().text(3, "pool")), 1, true),
            Some(Content::Keys { master: None, pool: Some("pool".into()) })
        );
        let keyed = [
            Content::Keys { master: Some([5; 32]), pool: Some("pool".into()) },
            Content::ProfileKey([6; 32]),
            Content::SentProfileKey { to: Some(ALICE), group: None, key: [6; 32] },
        ];
        assert_eq!(
            alloc::format!("{keyed:?}"),
            "[Keys, ProfileKey, SentProfileKey { to: Some(Aci), group: None, .. }]",
            "no key is told where formatted"
        );
        assert_eq!(
            alloc::format!("{:?}", Content::Blocked { acis: Some(alloc::vec![ALICE]), groups: alloc::vec![[3; 32]] }),
            "Blocked { acis: Some([Aci]), groups: 1 }",
            "nor a group's id"
        );
        let addressable =
            |author: &ServiceId, timestamp| Writer::new().bytes(4, &author.to_binary()).varint(3, timestamp);
        let group = || Writer::new().bytes(2, &[3; 32]);
        let delete = Writer::new()
            .message(
                1,
                Writer::new()
                    .message(1, Writer::new().bytes(4, &ALICE.to_binary()))
                    .message(2, addressable(&ALICE, 1600)),
            )
            .message(1, Writer::new().message(2, addressable(&ALICE, 1601)))
            .message(2, Writer::new().message(1, group()).message(2, addressable(&ALICE, 1700)).bool(3, true))
            .message(3, Writer::new().message(1, Writer::new().text(3, "+15550000000")))
            .message(4, Writer::new().message(1, group()));
        assert_eq!(
            parse_content(&sync(22, delete.clone()), 1, true),
            Some(Content::DeleteForMe(vec![
                DeleteForMe::Messages {
                    thread: Identifier::Service(ALICE),
                    messages: vec![(Identifier::Service(ALICE), 1600)]
                },
                DeleteForMe::Conversation {
                    thread: Identifier::Group([3; 32]),
                    newest: vec![(Identifier::Service(ALICE), 1700)],
                    non_expiring: Vec::new(),
                    is_full: true
                },
                DeleteForMe::Local { thread: Identifier::Number("+15550000000".into()) },
            ])),
            "each part that names its conversation, a deletion of attachments passed over"
        );
        let Some(Content::DeleteForMe(parts)) = parse_content(&sync(22, delete), 1, true) else { panic!("deletions") };
        assert_eq!(
            parse_content(&write_delete_for_me(&parts), 1, true),
            Some(Content::DeleteForMe(parts)),
            "written as it reads"
        );
        assert_eq!(
            parse_content(&sync(22, Writer::new()), 1, true),
            Some(Content::Nothing),
            "a sync that names nothing"
        );
        let pair =
            |public: &[u8; 32], private: &[u8; 32]| Writer::new().bytes(1, &encode_key(public)).bytes(2, private);
        let signed = Writer::new().varint(1, 7).bytes(2, &encode_key(&[2; 32])).bytes(3, &[3; 32]).bytes(4, &[4; 64]);
        let last_resort = |key_type: u8| {
            Writer::new()
                .varint(1, 8)
                .bytes(2, &[&[key_type][..], &[5; 4]].concat())
                .bytes(3, &[&[0x08][..], &[6; 4]].concat())
                .bytes(4, &[7; 64])
        };
        let to_typed_change = |number: Option<&str>, key_type: u8| {
            let change = Writer::new()
                .bytes(1, &pair(&[1; 32], &[9; 32]).finish())
                .bytes(2, &signed.clone().finish())
                .bytes(5, &last_resort(key_type).finish())
                .varint(3, 77)
                .maybe(number.map(|number| move |w: Writer| w.text(4, number)));
            sync(18, change)
        };
        let to_change = |number: Option<&str>| to_typed_change(number, 0x08);
        assert_eq!(
            parse_pni_change(&to_typed_change(Some("+15550000001"), 0x05)).map(|change| change.last_resort),
            Some(None),
            "a last-resort key that is no Kyber key is left out"
        );
        assert_eq!(
            parse_pni_change(&to_change(Some("+15550000001"))),
            Some(PniChange {
                identity: Pair { private: [9; 32], public: [1; 32] },
                signed_pre_key: (7, Pair { private: [3; 32], public: [2; 32] }, [4; 64], 0),
                last_resort: Some((8, vec![6; 4], vec![5; 4], [7; 64], 0)),
                registration: 77,
                number: "+15550000001".into(),
            }),
            "the keys without their type bytes"
        );
        assert_eq!(parse_pni_change(&to_change(None)), None, "without the number, none");
        let proof = Writer::new().message(10, Writer::new().bytes(1, &[6; 16]).bytes(2, &[8; 64])).finish();
        assert_eq!(parse_pni_signature(&proof), Some(([6; 16], [8; 64])));
    }
}

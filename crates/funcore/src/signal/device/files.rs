use super::apply::EarlyContent;
use super::calls;
use super::contacts::{Blocklist, Contact};
use super::identities::KnownIdentity;
use super::outbox::SentRecord;
use super::sealing::Certificates;
use super::{Account, Address, Device, Identity, Peer, Record, Ring, SenderKeys, ServiceId};
use crate::base::code::hex;
use crate::base::code::unhex;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::attachment::ATTACHMENTS;
use crate::signal::conversation::Conversation;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Display;

/// Signal's directory, in the private directory USB hosts are not shown.
pub const DIRECTORY: &str = ".r1metal/signal";
pub const SESSIONS: &str = ".r1metal/signal/sessions";
pub const CONVERSATIONS: &str = ".r1metal/signal/conversations";
pub const SENDER_KEYS: &str = ".r1metal/signal/sender-keys";
/// The folders of `DIRECTORY`, the one that holds the plaintexts of attachments among them.
pub const FOLDERS: [&str; 4] = [SESSIONS, SENDER_KEYS, CONVERSATIONS, ATTACHMENTS];

/// The names of a directory an unlink removes: all but `skip`.
pub fn to_removed<'a>(names: &'a [String], skip: Option<&str>) -> Vec<&'a str> {
    names.iter().map(String::as_str).filter(|name| Some(*name) != skip).collect()
}

/// The log line of an unlink that left `count` things behind.
pub fn to_unforgotten_line(count: usize, first: &impl Display) -> String {
    format!("signal: {count} of what the account kept is not taken away, which the next start tries again: {first}")
}

/// What the partition holds of Signal.
pub enum LinkState {
    Linked(Box<Device>),
    Unlinked,
    // Files without an account, as an unlink cut short or a link that ended left them.
    Leftovers,
}

/// A file the device keeps.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum File {
    Account,
    Ring(Identity),
    Session(Address),
    // A sender's chains of one distribution of its sender key.
    SenderKey(Address, [u8; 16]),
    Identities,
    Contacts,
    Conversation(Peer),
    Seen,
    // What the R1 sent in the last day, to be sent again where asked.
    Sent,
    Blocked,
    Certificates,
    // The call log.
    Calls,
}

impl File {
    /// The directory and name of the file.
    pub fn to_path(&self) -> (&'static str, String) {
        match self {
            File::Account => (DIRECTORY, String::from("account")),
            File::Ring(identity) => (DIRECTORY, format!("keys-{}", identity.name())),
            File::Session(address) => (SESSIONS, format!("{}.{}", hex(&address.service.to_binary()), address.device)),
            File::SenderKey(address, distribution) => {
                let service = hex(&address.service.to_binary());
                (SENDER_KEYS, format!("{service}.{}.{}", address.device, hex(distribution)))
            }
            File::Identities => (DIRECTORY, String::from("identities")),
            File::Contacts => (DIRECTORY, String::from("contacts")),
            File::Conversation(peer) => (CONVERSATIONS, hex(&peer.to_bytes())),
            File::Seen => (DIRECTORY, String::from("seen")),
            File::Sent => (DIRECTORY, String::from("sent")),
            File::Blocked => (DIRECTORY, String::from("blocked")),
            File::Certificates => (DIRECTORY, String::from("certificates")),
            File::Calls => (DIRECTORY, String::from("calls")),
        }
    }

    /// The file of that name in that directory; none for a file the device does not keep.
    pub fn from_path(directory: &str, name: &str) -> Option<File> {
        match directory {
            SESSIONS => {
                let (service, device) = name.split_once('.')?;
                let service = ServiceId::from_binary(&unhex(service)?)?;
                Some(File::Session(Address { service, device: device.parse().ok()? }))
            }
            SENDER_KEYS => {
                let mut parts = name.split('.');
                let service = ServiceId::from_binary(&unhex(parts.next()?)?)?;
                let device = parts.next()?.parse().ok()?;
                let distribution = unhex(parts.next()?)?.try_into().ok()?;
                parts.next().is_none().then_some(File::SenderKey(Address { service, device }, distribution))
            }
            CONVERSATIONS => Some(File::Conversation(Peer::from_bytes(&unhex(name)?)?)),
            DIRECTORY => [
                File::Account,
                File::Identities,
                File::Contacts,
                File::Seen,
                File::Sent,
                File::Blocked,
                File::Certificates,
                File::Calls,
            ]
            .into_iter()
            .chain([Identity::Aci, Identity::Pni].map(File::Ring))
            .find(|file| file.to_path().1 == name),
            _ => None,
        }
    }
}

impl Device {
    /// The device as its files hold it, damaged sessions, sender keys and conversations passed over; none without a readable account.
    pub fn read(files: &BTreeMap<File, Vec<u8>>) -> Option<Device> {
        let file = |file: File| files.get(&file).map(Vec::as_slice);
        let account = Account::read(file(File::Account)?)?;
        let (mut rings, mut sessions, mut conversations) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
        let mut sender_keys = BTreeMap::new();
        for (file, bytes) in files {
            match *file {
                File::Ring(identity) => rings.extend(Ring::read(bytes).map(|ring| (identity, ring))),
                File::Session(address) => sessions.extend(Record::read(bytes).map(|record| (address, record))),
                File::SenderKey(address, distribution) => {
                    sender_keys.extend(SenderKeys::read(bytes).map(|record| ((address, distribution), record)));
                }
                File::Conversation(peer) => {
                    conversations.insert(peer, Conversation::read(peer, &String::from_utf8_lossy(bytes)));
                }
                _ => {}
            }
        }
        let identities = file(File::Identities).and_then(Message::parse).map_or_else(BTreeMap::new, |message| {
            let entries = message.repeated(1).filter_map(Message::parse);
            entries
                .filter_map(|entry| Some((ServiceId::from_binary(entry.bytes(1)?)?, KnownIdentity::read(&entry)?)))
                .collect()
        });
        let contacts = file(File::Contacts).and_then(Message::parse).map_or_else(BTreeMap::new, |message| {
            let entries = message.repeated(1).filter_map(Message::parse);
            entries
                .filter_map(|entry| {
                    let aci = ServiceId::from_binary(entry.bytes(1)?)?;
                    Some((aci, Contact::read(aci, entry.bytes(2)?)?))
                })
                .collect()
        });
        let (mut guids, mut storage_version, mut storage_read_at, mut has_listed_groups) =
            (VecDeque::new(), 0, 0, false);
        let (mut is_synced, mut early_contents) = (false, Vec::new());
        if let Some(message) = file(File::Seen).and_then(Message::parse) {
            guids = message.repeated(1).map(<[u8]>::to_vec).collect();
            storage_version = message.varint(2).unwrap_or(0);
            storage_read_at = message.varint(3).unwrap_or(0);
            has_listed_groups = message.bool(4).unwrap_or(false);
            is_synced = message.bool(5).unwrap_or(false);
            early_contents =
                message.repeated(6).filter_map(|bytes| EarlyContent::read(&Message::parse(bytes)?)).collect();
        }
        let sent_records = file(File::Sent).and_then(Message::parse).map_or_else(Vec::new, |message| {
            message.repeated(1).filter_map(|record| SentRecord::read(&Message::parse(record)?)).collect()
        });
        let held = conversations.values().flat_map(|conversation: &Conversation| {
            conversation.entries.iter().map(|entry| entry.at).chain(conversation.queue.iter().map(|message| message.at))
        });
        let last_entry_time = held.max().unwrap_or(0);
        let mut device = Device {
            account,
            rings,
            sessions,
            sender_keys,
            identities,
            contacts,
            conversations,
            guids,
            storage_version,
            storage_read_at,
            has_listed_groups,
            last_stamp: 0,
            last_entry_time,
            dirty: BTreeSet::new(),
            deferred_orders: Vec::new(),
            sent_records,
            retry_counts: BTreeMap::new(),
            blocklist: file(File::Blocked).and_then(Blocklist::read).unwrap_or_default(),
            certificates: file(File::Certificates).and_then(Certificates::read).unwrap_or_default(),
            owed_receipts: Vec::new(),
            owed_profile_keys: BTreeSet::new(),
            is_synced,
            is_storage_due: false,
            early_contents,
            is_credential_stale: false,
            is_dropped: false,
            calls: file(File::Calls).map_or_else(Vec::new, calls::read),
            relays: None,
            owed_hangups: Vec::new(),
        };
        device.add_own_identities();
        Some(device)
    }

    /// What those files come to, `has_any` where the partition held any.
    pub fn to_link_state(files: &BTreeMap<File, Vec<u8>>, has_any: bool) -> LinkState {
        match Device::read(files) {
            Some(device) => LinkState::Linked(Box::new(device)),
            None if has_any => LinkState::Leftovers,
            None => LinkState::Unlinked,
        }
    }

    /// The content of a file, for writing.
    pub fn file_bytes(&self, file: &File) -> Option<Vec<u8>> {
        Some(match file {
            File::Account => self.account.write(),
            File::Ring(identity) => self.rings.get(identity)?.write(),
            File::Session(address) => self.sessions.get(address)?.write(),
            File::SenderKey(address, distribution) => self.sender_keys.get(&(*address, *distribution))?.write(),
            File::Identities => self
                .identities
                .iter()
                .fold(Writer::new(), |writer, (id, known)| writer.message(1, known.write(id)))
                .finish(),
            File::Contacts => self
                .contacts
                .iter()
                .fold(Writer::new(), |writer, (id, contact)| {
                    writer.message(1, Writer::new().bytes(1, &id.to_binary()).bytes(2, &contact.write()))
                })
                .finish(),
            File::Conversation(peer) => self.conversations.get(peer)?.write().into_bytes(),
            File::Seen => {
                let writer = self
                    .guids
                    .iter()
                    .fold(Writer::new(), |writer, guid| writer.bytes(1, guid))
                    .varint(2, self.storage_version)
                    .varint(3, self.storage_read_at)
                    .bool(4, self.has_listed_groups)
                    .bool(5, self.is_synced);
                self.early_contents.iter().fold(writer, |writer, early| writer.message(6, early.write())).finish()
            }
            File::Sent => self
                .sent_records
                .iter()
                .fold(Writer::new(), |writer, record| writer.message(1, record.write()))
                .finish(),
            File::Blocked => self.blocklist.write(),
            File::Certificates => self.certificates.write(),
            File::Calls => calls::write(&self.calls),
        })
    }

    /// The files changed since this was last asked, to be written.
    pub fn take_dirty(&mut self) -> Vec<File> {
        core::mem::take(&mut self.dirty).into_iter().collect()
    }

    /// Marks a file to be written again, as after a failed write.
    pub fn mark_dirty(&mut self, file: File) {
        self.dirty.insert(file);
    }
}

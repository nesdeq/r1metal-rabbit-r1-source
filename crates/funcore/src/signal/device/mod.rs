//! The R1 as a device of the account: what it keeps, each change marking its file.

mod apply;
mod attachments;
mod calls;
mod contacts;
mod delivery;
mod files;
mod groups;
mod identities;
mod outbox;
mod prekeys;
mod records;
mod sealing;
mod sessions;
#[cfg(test)]
pub(super) mod tests;

use super::account::{Account, Identity};
use super::address::{Address, Peer, ServiceId};
use super::conversation::{Conversation, Entry};
use super::group::Group;
use super::keys::Ring;
use super::sender_keys::Record as SenderKeys;
use super::service::Relay;
use super::session::Record;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use apply::EarlyContent;
use contacts::{Blocklist, Contact};
use delivery::Reception;
use outbox::SentRecord;
use sealing::Certificates;

pub(super) use calls::Call;
pub(crate) use calls::call_of_era;
pub use calls::{Course, GroupMember, Incoming, Membership, Note, Status};
pub(crate) use contacts::UNNAMED;
pub use delivery::{DropReason, Event};
pub use files::{
    CONVERSATIONS, DIRECTORY, FOLDERS, File, LinkState, SENDER_KEYS, SESSIONS, to_removed, to_unforgotten_line,
};
pub(super) use identities::{KnownIdentity, Trust};
pub(super) use outbox::Order;
pub(super) use sealing::Access;

/// What the device keeps.
#[derive(Clone)]
pub struct Device {
    pub account: Account,
    rings: BTreeMap<Identity, Ring>,
    sessions: BTreeMap<Address, Record>,
    sender_keys: BTreeMap<(Address, [u8; 16]), SenderKeys>,
    identities: BTreeMap<ServiceId, KnownIdentity>,
    contacts: BTreeMap<ServiceId, Contact>,
    conversations: BTreeMap<Peer, Conversation>,
    guids: VecDeque<Vec<u8>>,
    pub(super) storage_version: u64,
    pub(super) storage_read_at: u64,
    pub(super) has_listed_groups: bool,
    dirty: BTreeSet<File>,
    last_stamp: u64,
    last_entry_time: u64,
    // What a stay put off, each with the paced time it goes again at; kept in memory alone.
    pub(super) deferred_orders: Vec<(u64, Order)>,
    sent_records: Vec<SentRecord>,
    // How often a message of a device was asked or sent again, and when the first time.
    retry_counts: BTreeMap<(ServiceId, u32, u64), (u32, u64)>,
    blocklist: Blocklist,
    certificates: Certificates,
    pub(super) owed_receipts: Vec<(ServiceId, Peer, u64)>,
    owed_profile_keys: BTreeSet<ServiceId>,
    pub(super) is_synced: bool,
    pub(super) is_storage_due: bool,
    early_contents: Vec<EarlyContent>,
    pub(super) is_credential_stale: bool,
    // Messages went since this was last asked, and with them maybe the files of their attachments.
    is_dropped: bool,
    // The call log, each call where it was first logged.
    calls: Vec<Call>,
    // The relays the service handed out, until the wall clock's time they may be used to; kept in memory alone.
    relays: Option<(Vec<Relay>, u64)>,
    // The callers of offers from conversations not accepted, each with its call, owed a hang-up.
    owed_hangups: Vec<(Address, u64)>,
}

// The clock's time where it lies past `last`, else the millisecond after it: a time that neither goes back nor repeats.
fn rise(last: &mut u64, now: u64) -> u64 {
    *last = now.max(*last + 1);
    *last
}

impl core::fmt::Debug for Device {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Device")
    }
}

impl Device {
    pub(super) fn new(account: Account, rings: BTreeMap<Identity, Ring>) -> Device {
        let mut device = Device {
            account,
            rings,
            sessions: BTreeMap::new(),
            sender_keys: BTreeMap::new(),
            identities: BTreeMap::new(),
            contacts: BTreeMap::new(),
            conversations: BTreeMap::new(),
            guids: VecDeque::new(),
            storage_version: 0,
            storage_read_at: 0,
            has_listed_groups: false,
            last_stamp: 0,
            last_entry_time: 0,
            dirty: BTreeSet::new(),
            deferred_orders: Vec::new(),
            sent_records: Vec::new(),
            retry_counts: BTreeMap::new(),
            blocklist: Blocklist::default(),
            certificates: Certificates::default(),
            owed_receipts: Vec::new(),
            owed_profile_keys: BTreeSet::new(),
            is_synced: false,
            is_storage_due: false,
            early_contents: Vec::new(),
            is_credential_stale: false,
            is_dropped: false,
            calls: Vec::new(),
            relays: None,
            owed_hangups: Vec::new(),
        };
        device.dirty.insert(File::Account);
        device.dirty.extend(device.rings.keys().map(|identity| File::Ring(*identity)));
        device.add_own_identities();
        device
    }

    pub(super) const fn rings(&self) -> &BTreeMap<Identity, Ring> {
        &self.rings
    }

    fn own_address(&self) -> Address {
        Address { service: self.account.aci(), device: self.account.device }
    }

    fn is_own_id(&self, service: &ServiceId) -> bool {
        self.account.identity_of(service).is_some()
    }

    fn conversation_mut(&mut self, peer: Peer) -> &mut Conversation {
        self.dirty.insert(File::Conversation(peer));
        self.conversations.entry(peer).or_insert_with(|| Conversation::new(peer))
    }

    // Changes a conversation held, its file marked only where the change says it changed anything.
    fn change_conversation(&mut self, peer: Peer, change: impl FnOnce(&mut Conversation) -> bool) -> bool {
        let is_changed = self.conversations.get_mut(&peer).is_some_and(change);
        if is_changed {
            self.dirty.insert(File::Conversation(peer));
        }
        is_changed
    }

    fn contact_mut(&mut self, aci: ServiceId) -> &mut Contact {
        self.dirty.insert(File::Contacts);
        self.contacts.entry(aci).or_insert_with(|| Contact::new(aci))
    }

    fn group(&self, peer: &Peer) -> Option<&Group> {
        self.conversations.get(peer)?.group.as_ref()
    }

    fn group_mut(&mut self, peer: &Peer) -> Option<&mut Group> {
        let group = self.conversations.get_mut(peer)?.group.as_mut()?;
        self.dirty.insert(File::Conversation(*peer));
        Some(group)
    }

    pub(crate) fn conversations(&self) -> impl Iterator<Item = &Conversation> {
        self.conversations.values()
    }

    pub(crate) fn conversation(&self, peer: &Peer) -> Option<&Conversation> {
        self.conversations.get(peer)
    }

    pub(crate) fn entries(&self, peer: &Peer) -> &[Entry] {
        self.conversation(peer).map_or(&[][..], |conversation| &conversation.entries[..])
    }

    pub(super) fn stamp(&mut self, now: u64) -> u64 {
        rise(&mut self.last_stamp, now)
    }

    // The time an entry taken in or written at `now` is kept at: entries and conversations stand in the order they came in, whatever the clock does meanwhile.
    fn entry_time(&mut self, now: u64) -> u64 {
        rise(&mut self.last_entry_time, now)
    }

    pub(super) fn next_expiry(&self, now: u64) -> Option<u64> {
        let next = self.conversations.values().filter_map(Conversation::next_expiry).min()?;
        Some(next.saturating_sub(now))
    }

    pub(super) fn expire(&mut self, now: u64) -> Vec<Peer> {
        let peers: Vec<Peer> = self
            .conversations
            .iter_mut()
            .filter_map(|(peer, conversation)| conversation.expire(now).then_some(*peer))
            .collect();
        peers.iter().for_each(|peer| {
            self.dirty.insert(File::Conversation(*peer));
        });
        if !peers.is_empty() {
            self.forget_gone();
        }
        peers
    }
}

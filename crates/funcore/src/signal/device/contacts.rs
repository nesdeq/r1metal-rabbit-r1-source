use super::sealing::UNKNOWN;
use super::{Device, File, Peer, ServiceId};
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::address::GROUP_ID_BYTES;
use crate::signal::content::{self, ACCEPT, BLOCK, BLOCK_AND_DELETE, BLOCK_AND_SPAM, DELETE};
use crate::signal::conversation::Entry;
use crate::signal::storage::ListState;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const NOTE_TO_SELF: &str = "Note to Self";
pub(crate) const UNNAMED: &str = "Unknown";
const MENTION: char = '@';
// How long a contact found not to be on Signal counts as off it (Signal-Desktop's `isConversationUnregistered`, `SIX_HOURS`).
pub(super) const UNREGISTERED_AGE: u64 = 6 * 60 * 60 * 1000;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct ListStatus {
    pub(crate) pinned: Option<usize>,
    pub(crate) is_archived: bool,
    pub(crate) is_marked: bool,
    pub(crate) is_muted: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(in crate::signal) struct Contact {
    pub(super) aci: ServiceId,
    pub(super) name: Option<String>,
    pub(super) number: Option<String>,
    pub(super) username: Option<String>,
    pub(super) pni: Option<ServiceId>,
    pub(super) profile_key: Option<[u8; 32]>,
    pub(super) is_system: bool,
    pub(super) is_sharing: bool,
    pub(super) is_hidden: bool,
    pub(super) response: u32,
    pub(super) unregistered: Option<u64>,
    pub(super) access_mode: u32,
}

impl core::fmt::Debug for Contact {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Contact")
    }
}

impl Contact {
    pub(super) fn new(aci: ServiceId) -> Contact {
        Contact {
            aci,
            name: None,
            number: None,
            username: None,
            pni: None,
            profile_key: None,
            is_system: false,
            is_sharing: false,
            is_hidden: false,
            response: 0,
            unregistered: None,
            access_mode: UNKNOWN,
        }
    }

    pub(super) fn read(aci: ServiceId, bytes: &[u8]) -> Option<Contact> {
        let contact = Message::parse(bytes)?;
        Some(Contact {
            aci,
            name: contact.text(1).map(String::from),
            number: contact.text(2).map(String::from),
            username: contact.text(10).map(String::from),
            pni: contact.bytes(11).and_then(ServiceId::from_binary),
            profile_key: contact.bytes(3).and_then(|key| key.try_into().ok()),
            is_system: contact.bool(4).unwrap_or(false),
            is_sharing: contact.bool(5).unwrap_or(false),
            is_hidden: contact.bool(6).unwrap_or(false),
            response: contact.u32(7).unwrap_or(0),
            unregistered: contact.varint(8),
            access_mode: contact.u32(9).unwrap_or(UNKNOWN),
        })
    }

    pub(super) fn write(&self) -> Vec<u8> {
        Writer::new()
            .maybe(self.name.as_deref().map(|name| move |w: Writer| w.text(1, name)))
            .maybe(self.number.as_deref().map(|number| move |w: Writer| w.text(2, number)))
            .maybe(self.profile_key.map(|key| move |w: Writer| w.bytes(3, &key)))
            .bool(4, self.is_system)
            .bool(5, self.is_sharing)
            .bool(6, self.is_hidden)
            .varint(7, u64::from(self.response))
            .maybe(self.unregistered.map(|at| move |w: Writer| w.varint(8, at)))
            .varint(9, u64::from(self.access_mode))
            .maybe(self.username.as_deref().map(|username| move |w: Writer| w.text(10, username)))
            .maybe(self.pni.map(|pni| move |w: Writer| w.bytes(11, &pni.to_binary())))
            .finish()
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Blocklist {
    pub(super) acis: BTreeSet<ServiceId>,
    pub(super) groups: BTreeSet<[u8; GROUP_ID_BYTES]>,
}

impl Blocklist {
    pub(super) fn write(&self) -> Vec<u8> {
        let acis = self.acis.iter().fold(Writer::new(), |writer, aci| writer.bytes(1, &aci.to_binary()));
        self.groups.iter().fold(acis, |writer, group| writer.bytes(2, group)).finish()
    }

    pub(super) fn read(bytes: &[u8]) -> Option<Blocklist> {
        let message = Message::parse(bytes)?;
        Some(Blocklist {
            acis: message.repeated(1).filter_map(ServiceId::from_binary).collect(),
            groups: message.repeated(2).filter_map(|id| id.try_into().ok()).collect(),
        })
    }
}

impl Device {
    fn contact_of(&self, service: &ServiceId) -> Option<&Contact> {
        self.contacts.get(service).or_else(|| self.contacts.values().find(|contact| contact.pni == Some(*service)))
    }

    // The ACI of the account a service id names: the contact's whose ACI or PNI it is, else the id itself.
    pub(super) fn aci_of(&self, service: &ServiceId) -> ServiceId {
        self.contact_of(service).map_or(*service, |contact| contact.aci)
    }

    pub(super) fn aci_of_number(&self, number: &str) -> Option<ServiceId> {
        Some(self.contacts.values().find(|contact| contact.number.as_deref() == Some(number))?.aci)
    }

    pub(crate) fn name_of(&self, peer: &Peer) -> Option<String> {
        let contact = |service: &ServiceId| {
            let contact = self.contact_of(service)?;
            contact.name.clone().or_else(|| contact.number.clone()).or_else(|| contact.username.clone())
        };
        match peer {
            Peer::Contact(service) if *service == self.account.aci() => Some(NOTE_TO_SELF.into()),
            Peer::Contact(service) => contact(service),
            Peer::Group(_) => {
                let group = self.group(peer)?;
                if let Some(title) = &group.title {
                    return Some(title.clone());
                }
                let others = group.members.iter().filter(|member| **member != self.account.aci());
                let names: Vec<String> = others.filter_map(contact).collect();
                (!names.is_empty()).then(|| names.join(", "))
            }
        }
    }

    // The contacts a new message may go to at `now`, each by its conversation and the name it goes by, sorted by that name whatever its letters' case: the owner's Note to Self, and every other contact that goes by a name and is neither hidden nor blocked nor found not to be on Signal in the last six hours.
    pub(crate) fn contactable(&self, now: u64) -> Vec<(Peer, String)> {
        let own = self.account.aci();
        let is_open = |contact: &&Contact| {
            let peer = Peer::Contact(contact.aci);
            contact.aci != own
                && !contact.is_hidden
                && !self.is_blocked(&peer)
                && !self.is_off_signal(&contact.aci, now)
        };
        let named = |contact: &Contact| {
            let peer = Peer::Contact(contact.aci);
            Some((peer, self.name_of(&peer)?))
        };
        let mut listed: Vec<(Peer, String)> = self.contacts.values().filter(is_open).filter_map(named).collect();
        listed.push((Peer::Contact(own), String::from(NOTE_TO_SELF)));
        listed.sort_by_cached_key(|(_, name)| name.to_lowercase());
        listed
    }

    // The contacts and groups a call may go to at `now`, sorted by name as `contactable` sorts: the contacts a message may go to but the owner's Note to Self, and each group that goes by a name whose messages go to anyone.
    pub(crate) fn callable(&self, now: u64) -> Vec<(Peer, String)> {
        let own = Peer::Contact(self.account.aci());
        let contacts = self.contactable(now).into_iter().filter(|(peer, _)| *peer != own);
        let groups = self.conversations.values().filter(|conversation| conversation.group.is_some());
        let groups = groups.map(|conversation| conversation.peer).filter(|peer| !self.recipients(peer, now).is_empty());
        let mut listed: Vec<(Peer, String)> =
            contacts.chain(groups.filter_map(|peer| Some((peer, self.name_of(&peer)?)))).collect();
        listed.sort_by_cached_key(|(_, name)| name.to_lowercase());
        listed
    }

    // The names of those who wrote `entries` and of those their replies quote.
    pub(crate) fn writers_of(&self, entries: &[Entry]) -> BTreeMap<ServiceId, String> {
        let writers = entries.iter().flat_map(|entry| entry.sender.into_iter().chain(entry.quote_author()));
        writers.filter_map(|writer| Some((writer, self.name_of(&Peer::Contact(writer))?))).collect()
    }

    // An entry's words with the name of whom each mention names in the place of its mark.
    pub(super) fn resolved_body(&self, entry: &Entry) -> String {
        if entry.mentions.is_empty() {
            return entry.body.clone();
        }
        let name = |mention: &content::Mention| {
            let name = self.name_of(&Peer::Contact(mention.aci)).unwrap_or_else(|| String::from(UNNAMED));
            format!("{MENTION}{name}")
        };
        let mut mentions: Vec<&content::Mention> = entry.mentions.iter().collect();
        mentions.sort_by_key(|mention| mention.start);
        let (mut body, mut at, mut skip_to) = (String::new(), 0u32, 0u32);
        let mut next = mentions.into_iter().peekable();
        for letter in entry.body.chars() {
            while let Some(mention) = next.next_if(|mention| mention.start <= at) {
                body.push_str(&name(mention));
                skip_to = skip_to.max(mention.start + mention.length);
            }
            if at >= skip_to {
                body.push(letter);
            }
            at += letter.len_utf16() as u32;
        }
        next.for_each(|mention| body.push_str(&name(mention)));
        body
    }

    pub(crate) fn resolve_mentions(&self, entry: &Entry) -> Entry {
        Entry { body: self.resolved_body(entry), mentions: Vec::new(), ..entry.clone() }
    }

    pub(crate) fn resolved_entries<'a>(&self, entries: impl IntoIterator<Item = &'a Entry>) -> Vec<Entry> {
        entries.into_iter().map(|entry| self.resolve_mentions(entry)).collect()
    }

    // Whether a muted conversation tells mentions, replies and calls: its own word, else the account's (`getNotifyWhileMuted`).
    fn notify_while_muted(&self, peer: &Peer) -> (bool, bool, bool) {
        let own = self.conversations.get(peer).map_or((None, None, None), |conversation| conversation.notify);
        let account = self.account.listing.notify;
        (own.0.unwrap_or(account.0), own.1.unwrap_or(account.1), own.2.unwrap_or(account.2))
    }

    fn is_muted(&self, peer: &Peer, now: u64) -> bool {
        self.conversations.get(peer).is_some_and(|conversation| conversation.is_muted(now))
    }

    pub(super) fn is_noticed(&self, peer: &Peer, text: &content::Text, now: u64) -> bool {
        let own = self.account.aci();
        if !self.is_muted(peer, now) {
            return true;
        }
        let (mentions, replies, _) = self.notify_while_muted(peer);
        let is_mention = text.mentions.iter().any(|mention| mention.aci == own);
        let is_reply = text.quote.as_ref().is_some_and(|quote| quote.author == own);
        (is_mention && mentions) || (is_reply && replies)
    }

    // A call of a muted conversation that does not tell calls tells itself not (`handleIncomingCall`, `handleGroupCallRingUpdate`).
    pub(super) fn is_call_muted(&self, peer: &Peer, now: u64) -> bool {
        self.is_muted(peer, now) && !self.notify_while_muted(peer).2
    }

    pub(crate) fn list_status(&self, peer: &Peer, now: u64) -> ListStatus {
        let conversation = self.conversations.get(peer);
        ListStatus {
            pinned: self.account.listing.pinned.iter().position(|pinned| pinned == peer),
            is_archived: conversation.is_some_and(|conversation| conversation.archived.is_set),
            is_marked: conversation.is_some_and(|conversation| conversation.marked_unread.is_set),
            is_muted: conversation.is_some_and(|conversation| conversation.is_muted(now)),
        }
    }

    pub(super) fn apply_list_state(&mut self, peer: Peer, state: ListState) {
        let held = self.conversations.get(&peer);
        let held =
            held.map(|conversation| (conversation.archived, conversation.marked_unread, conversation.muted_until));
        if held.is_none() && state == ListState::default() {
            return;
        }
        let (mut archived, mut marked_unread, _) = held.unwrap_or_default();
        archived.apply_record(state.archived);
        marked_unread.apply_record(state.marked_unread);
        let listed = (archived, marked_unread, state.muted_until);
        if held != Some(listed) {
            let conversation = self.conversation_mut(peer);
            (conversation.archived, conversation.marked_unread, conversation.muted_until) = listed;
        }
    }

    pub(super) fn take_profile_key(&mut self, service: ServiceId, key: [u8; 32]) {
        let contact = self.contacts.entry(service).or_insert_with(|| Contact::new(service));
        if contact.profile_key != Some(key) {
            (contact.profile_key, contact.access_mode) = (Some(key), UNKNOWN);
            self.dirty.insert(File::Contacts);
        }
    }

    pub(super) fn is_sharing(&self, peer: &Peer) -> bool {
        match peer {
            Peer::Contact(contact) => self.contacts.get(contact).is_some_and(|contact| contact.is_sharing),
            Peer::Group(_) => self.group(peer).is_some_and(|group| group.is_sharing),
        }
    }

    pub(super) fn set_sharing(&mut self, peer: Peer, is_sharing: bool) {
        if self.is_sharing(&peer) == is_sharing {
            return;
        }
        match peer {
            Peer::Contact(contact) => self.contact_mut(contact).is_sharing = is_sharing,
            Peer::Group(_) => {
                if let Some(group) = self.group_mut(&peer) {
                    group.is_sharing = is_sharing;
                }
            }
        }
    }

    pub(super) fn is_blocked(&self, peer: &Peer) -> bool {
        match peer {
            Peer::Contact(contact) => self.blocklist.acis.contains(contact),
            Peer::Group(id) => self.blocklist.groups.contains(id),
        }
    }

    pub(super) fn set_blocked(&mut self, peer: Peer, is_blocked: bool) {
        if peer == Peer::Contact(self.account.aci()) || self.is_blocked(&peer) == is_blocked {
            return;
        }
        match (peer, is_blocked) {
            (Peer::Contact(contact), true) => self.blocklist.acis.insert(contact),
            (Peer::Contact(contact), false) => self.blocklist.acis.remove(&contact),
            (Peer::Group(id), true) => self.blocklist.groups.insert(id),
            (Peer::Group(id), false) => self.blocklist.groups.remove(&id),
        };
        self.dirty.insert(File::Blocked);
    }

    pub(super) fn apply_response(&mut self, peer: Peer, kind: u32, is_from_storage: bool) {
        match peer {
            Peer::Contact(contact) if contact == self.account.aci() => return,
            Peer::Contact(contact) => self.contact_mut(contact).response = kind,
            Peer::Group(_) => {
                let Some(held) = self.group(&peer).map(|group| group.response) else { return };
                if held != kind
                    && let Some(group) = self.group_mut(&peer)
                {
                    group.response = kind;
                }
            }
        }
        let is_block = matches!(kind, BLOCK | BLOCK_AND_DELETE | BLOCK_AND_SPAM);
        let is_delete = matches!(kind, DELETE | BLOCK_AND_DELETE);
        if kind == ACCEPT {
            self.set_blocked(peer, false);
            if let (Peer::Contact(contact), false) = (peer, is_from_storage) {
                self.contact_mut(contact).is_hidden = false;
            }
            self.set_sharing(peer, true);
        }
        if is_block {
            self.set_blocked(peer, true);
        }
        if is_block || is_delete {
            self.set_sharing(peer, false);
        }
        if is_delete && self.conversations.contains_key(&peer) {
            let conversation = self.conversation_mut(peer);
            conversation.entries.clear();
            conversation.queue.clear();
            self.forget_gone();
        }
    }

    pub(super) fn take_blocked_lists(&mut self, acis: Option<Vec<ServiceId>>, groups: Vec<[u8; GROUP_ID_BYTES]>) {
        let mut changes: Vec<(Vec<Peer>, Vec<Peer>)> = Vec::new();
        if let Some(acis) = acis {
            let acis: BTreeSet<ServiceId> = acis.into_iter().collect();
            let removed = self.blocklist.acis.difference(&acis).map(|aci| Peer::Contact(*aci)).collect();
            let added = acis.difference(&self.blocklist.acis).map(|aci| Peer::Contact(*aci)).collect();
            changes.push((removed, added));
            self.blocklist.acis = acis;
        }
        let groups: BTreeSet<[u8; GROUP_ID_BYTES]> = groups.into_iter().collect();
        let removed = self.blocklist.groups.difference(&groups).map(|id| Peer::Group(*id)).collect();
        let added = groups.difference(&self.blocklist.groups).map(|id| Peer::Group(*id)).collect();
        changes.push((removed, added));
        self.blocklist.groups = groups;
        self.dirty.insert(File::Blocked);
        for (removed, added) in changes {
            for peer in removed {
                self.apply_response(peer, ACCEPT, false);
            }
            for peer in added {
                self.apply_response(peer, BLOCK, false);
            }
        }
    }

    pub(super) fn note_registered(&mut self, contact: ServiceId) {
        if self.contacts.get(&contact).is_some_and(|known| known.unregistered.is_some()) {
            self.contact_mut(contact).unregistered = None;
        }
    }

    pub(in crate::signal) fn note_unregistered(&mut self, contact: ServiceId, now: u64) {
        if contact != self.account.aci() {
            let unregistered = &mut self.contact_mut(contact).unregistered;
            *unregistered = Some(unregistered.map_or(now, |at| at.max(now)));
        }
    }

    pub(in crate::signal) fn is_off_signal(&self, contact: &ServiceId, now: u64) -> bool {
        self.contacts.get(contact).and_then(|known| known.unregistered).is_some_and(|at| at + UNREGISTERED_AGE > now)
    }

    // Whether the conversation is accepted, a contact's that holds nothing counting where `is_empty_accepted`, as Signal-Desktop's `ignoreEmptyConvo` says otherwise for a call.
    pub(in crate::signal) fn is_accepted(&self, peer: &Peer, is_empty_accepted: bool) -> bool {
        let conversation = self.conversations.get(peer);
        let is_sent_to = conversation.is_some_and(|conversation| conversation.is_sent_to);
        let is_empty = conversation.is_none_or(|conversation| conversation.entries.is_empty());
        match peer {
            Peer::Contact(contact) if *contact == self.account.aci() => true,
            Peer::Contact(contact) => {
                let is_trusted = |known: &Contact| known.response == ACCEPT || known.is_system || known.is_sharing;
                match self.contacts.get(contact) {
                    Some(known) if known.is_hidden => false,
                    known => known.is_some_and(is_trusted) || is_sent_to || (is_empty && is_empty_accepted),
                }
            }
            Peer::Group(_) => {
                let group = conversation.and_then(|conversation| conversation.group.as_ref());
                group.is_some_and(|group| group.response == ACCEPT) || is_sent_to || (is_empty && self.is_sharing(peer))
            }
        }
    }

    pub(super) fn takes_receipts(&self, contact: ServiceId, now: u64) -> bool {
        let peer = Peer::Contact(contact);
        self.is_accepted(&peer, true) && !self.is_blocked(&peer) && !self.is_off_signal(&contact, now)
    }

    pub(super) fn is_trusted_contact(&self, contact: &ServiceId) -> bool {
        self.contacts.get(contact).is_some_and(|known| known.is_system || known.is_sharing)
    }
}

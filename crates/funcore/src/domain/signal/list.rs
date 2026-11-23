//! The conversation list, the open conversation and the call log for the screens.

use super::auto::unmarked;
use crate::signal::address::{Peer, ServiceId};
use crate::signal::call::Kind;
use crate::signal::conversation::Entry;
use crate::signal::device::{Device, Status};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Reverse;

// The messages of a conversation its view holds, the last of them.
pub(super) const MAX_MESSAGES: usize = 200;
// What stands in place of a message deleted for everyone.
const DELETED_WORD: &str = "This message was deleted.";

// The words a message stands for: those in place of one deleted for everyone, else its own without auto mode's mark.
pub(crate) fn entry_words(entry: &Entry) -> &str {
    if entry.deleter.is_some() { DELETED_WORD } else { unmarked(entry) }
}

/// A contact a new message may go to, or a contact or a group a call may go to.
#[derive(Clone, PartialEq, Eq)]
pub struct Contactable {
    pub peer: Peer,
    pub name: String,
    // True where its conversation holds messages already.
    pub has_talked: bool,
}

impl core::fmt::Debug for Contactable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Contactable").field("has_talked", &self.has_talked).finish_non_exhaustive()
    }
}

/// A conversation's list row.
#[derive(Clone, PartialEq, Eq)]
pub struct Summary {
    pub peer: Peer,
    pub name: Option<String>,
    pub last: Option<Entry>,
    pub unread: usize,
    pub is_marked: bool,
    pub is_pinned: bool,
    pub is_archived: bool,
    /// Its unread messages count among all (`_canCountConversation`).
    pub is_counted: bool,
}

impl Summary {
    pub(super) fn unread_count(&self) -> usize {
        match (self.is_counted, self.unread) {
            (false, _) => 0,
            (true, 0) => usize::from(self.is_marked),
            (true, unread) => unread,
        }
    }
}

impl core::fmt::Debug for Summary {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Summary").field("last", &self.last).field("unread", &self.unread).finish_non_exhaustive()
    }
}

/// The open conversation, whole.
#[derive(Clone, PartialEq, Eq)]
pub struct ConversationView {
    pub peer: Peer,
    pub name: Option<String>,
    pub entries: Vec<Entry>,
    pub is_safety_changed: bool,
    /// The names of its writers and of those its replies quote.
    pub writers: BTreeMap<ServiceId, String>,
    /// The owner's own account, whose quoted messages are the owner's.
    pub owner: ServiceId,
}

impl core::fmt::Debug for ConversationView {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Opened")
            .field("entries", &self.entries)
            .field("is_safety_changed", &self.is_safety_changed)
            .finish_non_exhaustive()
    }
}

fn to_summary(device: &Device, peer: Peer, now_ms: u64) -> Option<(Option<usize>, Summary)> {
    let conversation = device.conversation(&peer)?;
    let status = device.list_status(&peer, now_ms);
    let summary = Summary {
        peer,
        name: device.name_of(&peer),
        last: conversation.entries.last().map(|entry| device.resolve_mentions(entry)),
        unread: conversation.unread(),
        is_marked: status.is_marked,
        is_pinned: status.pinned.is_some(),
        is_archived: status.is_archived,
        is_counted: !status.is_archived && (!status.is_muted || device.account.listing.count_muted),
    };
    Some((status.pinned, summary))
}

pub(super) fn to_summaries(device: &Device, now_ms: u64) -> Vec<Summary> {
    let pinned = device.account.listing.pinned.iter().copied();
    let active = device.conversations().filter(|conversation| !conversation.entries.is_empty());
    let mut peers: Vec<Peer> = pinned.chain(active.map(|conversation| conversation.peer)).collect();
    peers.sort();
    peers.dedup();
    let mut rows: Vec<(Option<usize>, Summary)> =
        peers.into_iter().filter_map(|peer| to_summary(device, peer, now_ms)).collect();
    rows.sort_by_key(|(pinned, summary)| {
        (pinned.unwrap_or(usize::MAX), Reverse(summary.last.as_ref().map_or(0, |entry| entry.at)))
    });
    rows.into_iter().map(|row| row.1).collect()
}

// The last `count` of a conversation's messages.
pub(super) fn last_of(entries: &[Entry], count: usize) -> &[Entry] {
    &entries[entries.len().saturating_sub(count)..]
}

pub(super) fn to_conversation_view(device: &Device, peer: &Peer) -> ConversationView {
    let shown = last_of(device.entries(peer), MAX_MESSAGES);
    ConversationView {
        peer: *peer,
        name: device.name_of(peer),
        entries: device.resolved_entries(shown),
        is_safety_changed: device.conversation(peer).is_some_and(|conversation| conversation.is_safety_changed),
        writers: device.writers_of(shown),
        owner: device.account.aci(),
    }
}

fn to_contactables(device: &Device, listed: Vec<(Peer, String)>) -> Vec<Contactable> {
    let has_talked =
        |peer: &Peer| device.conversation(peer).is_some_and(|conversation| !conversation.entries.is_empty());
    listed.into_iter().map(|(peer, name)| Contactable { has_talked: has_talked(&peer), peer, name }).collect()
}

pub(super) fn to_contacts(device: &Device, now_ms: u64) -> Vec<Contactable> {
    to_contactables(device, device.contactable(now_ms))
}

// The contacts and groups a call may go to.
pub(super) fn to_callees(device: &Device, now_ms: u64) -> Vec<Contactable> {
    to_contactables(device, device.callable(now_ms))
}

/// A call of the log as the screens show it: its conversation by the name the list gives it, its kind, whether the owner began it, how it stands, when, ms, and whether the owner has seen it.
#[derive(Clone, PartialEq, Eq)]
pub struct CallRow {
    pub peer: Peer,
    pub name: Option<String>,
    pub kind: Kind,
    pub is_outgoing: bool,
    pub status: Status,
    pub at: u64,
    pub is_seen: bool,
}

impl core::fmt::Debug for CallRow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CallRow")
            .field("kind", &self.kind)
            .field("outgoing", &self.is_outgoing)
            .field("status", &self.status)
            .field("at", &self.at)
            .field("seen", &self.is_seen)
            .finish_non_exhaustive()
    }
}

// The calls of the log the screens show, the newest first; one deleted on another device is none of them.
pub(super) fn to_call_rows(device: &Device) -> Vec<CallRow> {
    let shown = device.calls().iter().filter(|call| call.status != Status::Deleted);
    let mut rows: Vec<CallRow> = shown
        .map(|call| CallRow {
            peer: call.peer,
            name: device.name_of(&call.peer),
            kind: call.kind,
            is_outgoing: call.is_outgoing,
            status: call.status,
            at: call.at,
            is_seen: call.is_seen,
        })
        .collect();
    rows.sort_by_key(|row| Reverse(row.at));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::tests::Standin;

    const ALICE: ServiceId = ServiceId::Aci([1; 16]);
    const PEER_ALICE: Peer = Peer::Contact(ALICE);

    #[test]
    fn contacts_by_name() {
        let mut device = Device::unkeyed();
        device.add_named_contact(ServiceId::Aci([2; 16]), "boris", false);
        device.add_named_contact(ALICE, "Alice", false);
        device.add_incoming(ALICE, ("hi", &[]), 5, 1_000);
        let listed: Vec<(String, bool)> =
            to_contacts(&device, 2_000).into_iter().map(|contact| (contact.name, contact.has_talked)).collect();
        assert_eq!(
            listed,
            [("Alice".into(), true), ("boris".into(), false), ("Note to Self".into(), false)],
            "by name whatever its letters' case, each told whether its conversation holds messages"
        );
        let contact = Contactable { peer: PEER_ALICE, name: "Alice".into(), has_talked: false };
        assert_eq!(alloc::format!("{contact:?}"), "Contactable { has_talked: false, .. }");
    }

    #[test]
    fn listed_from_first_message() {
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let own = device.account.aci();
        let member = device.add_group(&crypto, &[8; 32], &[own, ALICE]);
        device.add_group(&crypto, &[7; 32], &[ALICE]);
        device.note_group_revision(&crypto, &[9; 32], 2);
        let peers = |device: &Device| to_summaries(device, 0).iter().map(|summary| summary.peer).collect::<Vec<Peer>>();
        assert!(peers(&device).is_empty(), "groups known, of the storage service or of a change, hold no message yet");
        device.compose(&crypto, member, ("hi", &[]), 1_000, false);
        device.compose(&crypto, PEER_ALICE, ("hi", &[]), 2_000, false);
        assert_eq!(peers(&device), [PEER_ALICE, member], "each from its first message on, the newest first");
    }

    #[test]
    fn order_survives_clock() {
        use crate::signal::device::File;
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let boris = Peer::Contact(ServiceId::Aci([5; 16]));
        let first = |device: &Device| to_summaries(device, 0).first().map(|summary| summary.peer);
        device.compose(&crypto, PEER_ALICE, ("first", &[]), 10_000, false);
        device.compose(&crypto, boris, ("later", &[]), 7_000, false);
        assert_eq!(first(&device), Some(boris), "written after the clock went back, and still the newest");
        device.compose(&crypto, PEER_ALICE, ("last", &[]), 7_000, false);
        let summaries = to_summaries(&device, 0);
        assert_eq!(
            summaries.first().and_then(|summary| Some((summary.peer, summary.last.as_ref()?.body.as_str()))),
            Some((PEER_ALICE, "last")),
            "two written in one millisecond stand in the order they were written in, the later its conversation's last"
        );
        let files: BTreeMap<File, Vec<u8>> = (device.take_dirty().into_iter())
            .filter_map(|file| Some((file.clone(), device.file_bytes(&file)?)))
            .collect();
        let mut read = Device::read(&files).unwrap();
        read.compose(&crypto, boris, ("after a start", &[]), 1_000, false);
        assert_eq!(first(&read), Some(boris), "and so after the files are read anew");
    }

    #[test]
    fn pinned_order_and_unread() {
        use crate::signal::storage::ListState;
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let [boris, carol, dana] = [5, 6, 7].map(|id| Peer::Contact(ServiceId::Aci([id; 16])));
        for (at, peer) in [(1_000, PEER_ALICE), (2_000, boris), (3_000, carol), (4_000, dana)] {
            device.compose(&crypto, peer, ("hi", &[]), at, false);
        }
        device.account.listing.pinned = alloc::vec![carol, PEER_ALICE];
        device.set_list_state(boris, ListState { archived: true, marked_unread: true, ..ListState::default() });
        device.set_list_state(dana, ListState { marked_unread: true, muted_until: 10_000, ..ListState::default() });
        let summaries = to_summaries(&device, 5_000);
        assert_eq!(
            summaries.iter().map(|summary| summary.peer).collect::<Vec<_>>(),
            [carol, PEER_ALICE, dana, boris],
            "the pinned first in their order, then the newest first"
        );
        assert_eq!(
            summaries
                .iter()
                .map(|summary| (summary.is_pinned, summary.is_archived, summary.is_marked, summary.is_counted))
                .collect::<Vec<_>>(),
            [
                (true, false, false, true),
                (true, false, false, true),
                (false, false, true, false),
                (false, true, true, false)
            ]
        );
        let unread = |device: &Device, now| to_summaries(device, now).iter().map(Summary::unread_count).sum::<usize>();
        assert_eq!(unread(&device, 5_000), 0, "marked unread, but archived or muted");
        device.account.listing.count_muted = true;
        assert_eq!(unread(&device, 5_000), 1, "a muted one marked unread counts one where muted ones count");
        device.account.listing.count_muted = false;
        assert_eq!(unread(&device, 10_000), 1, "as it does once its muting ran out");
        let summary = |unread, marked, counted| Summary {
            peer: PEER_ALICE,
            name: None,
            last: None,
            unread,
            is_marked: marked,
            is_pinned: false,
            is_archived: false,
            is_counted: counted,
        };
        assert_eq!(
            [summary(2, true, true), summary(0, true, true), summary(0, false, true), summary(2, false, false)]
                .map(|summary| summary.unread_count()),
            [2, 1, 0, 0],
            "its unread messages, or one where it is marked unread and holds none, as Signal-Desktop counts"
        );
    }

    #[test]
    fn words_of_an_entry() {
        use super::super::auto::MARK;
        use crate::signal::conversation::Deleter;
        use crate::signal::conversation::tests::entry;
        let marked = alloc::format!("{MARK}On my way");
        let robot = Entry { is_robot: true, ..entry(1, true, &marked) };
        assert_eq!(
            [entry_words(&entry(1, false, "Lunch?")), entry_words(&robot), entry_words(&entry(1, false, &marked))],
            ["Lunch?", "On my way", marked.as_str()],
            "its own words, auto mode's mark left out of a reply of auto mode's alone"
        );
        let deleted = Entry { deleter: Some(Deleter::Author), ..robot };
        assert_eq!(entry_words(&deleted), DELETED_WORD, "one deleted for everyone tells nothing of what it said");
    }

    #[test]
    fn call_log_rows() {
        use crate::signal::device::{Course, Note};
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        device.add_named_contact(ALICE, "Alice", false);
        let group = device.add_group(&crypto, &[8; 32], &[device.account.aci(), ALICE]);
        let note = |peer, call, kind, at| Note { peer, call, kind, is_outgoing: false, ringer: Some(ALICE), at };
        device.add_call(&note(PEER_ALICE, 1, Kind::Audio, 30), Some(Course::Missed));
        device.add_call(&note(group, 2, Kind::Group, 40), Some(Course::Started));
        device.add_call(&note(PEER_ALICE, 3, Kind::Video, 10), Some(Course::Accepted));
        device.add_call(&note(PEER_ALICE, 3, Kind::Video, 10), None);
        let rows: Vec<(Option<String>, Kind, Status, u64, bool)> = to_call_rows(&device)
            .into_iter()
            .map(|row| (row.name, row.kind, row.status, row.at, row.is_seen))
            .collect();
        let alice = Some(String::from("Alice"));
        assert_eq!(
            rows,
            [(alice.clone(), Kind::Group, Status::Generic, 40, false), (alice, Kind::Audio, Status::Missed, 30, false)],
            "the newest first, each by the name the list gives its conversation, one deleted on another device none"
        );
        let row = to_call_rows(&device).remove(0);
        assert_eq!(
            alloc::format!("{row:?}"),
            "CallRow { kind: Group, outgoing: false, status: Generic, at: 40, seen: false, .. }",
            "no name told"
        );
    }

    #[test]
    fn callees_by_name() {
        let crypto = Standin::default();
        let mut device = Device::unkeyed();
        let own = device.account.aci();
        device.add_named_contact(ALICE, "anna", false);
        device.add_named_contact(ServiceId::Aci([2; 16]), "Boris", false);
        let climbing = device.add_group(&crypto, &[8; 32], &[own, ALICE]);
        device.add_group(&crypto, &[7; 32], &[ALICE]);
        let callees: Vec<(Peer, String)> =
            to_callees(&device, 0).into_iter().map(|callee| (callee.peer, callee.name)).collect();
        let boris = Peer::Contact(ServiceId::Aci([2; 16]));
        assert_eq!(
            callees,
            [(PEER_ALICE, "anna".into()), (climbing, "anna".into()), (boris, "Boris".into())],
            "the contacts a message may go to but Note to Self, and a group whose messages go to anyone, the owner's alone, by name"
        );
    }

    #[test]
    fn debug_hides_names() {
        let summary = Summary {
            peer: PEER_ALICE,
            name: Some("Alice".into()),
            last: None,
            unread: 2,
            is_marked: false,
            is_pinned: false,
            is_archived: false,
            is_counted: true,
        };
        assert_eq!(alloc::format!("{summary:?}"), "Summary { last: None, unread: 2, .. }");
        let view = ConversationView {
            peer: PEER_ALICE,
            name: Some("Alice".into()),
            entries: Vec::new(),
            is_safety_changed: true,
            writers: BTreeMap::new(),
            owner: ALICE,
        };
        assert_eq!(alloc::format!("{view:?}"), "Opened { entries: [], is_safety_changed: true, .. }");
    }
}

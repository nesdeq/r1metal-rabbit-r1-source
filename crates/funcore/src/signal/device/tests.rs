use super::apply::{DELETE_GRACE, DELETE_WINDOW};
use super::contacts::{ListStatus, UNREGISTERED_AGE};
use super::delivery::MAX_EARLY;
use super::groups::SENDER_KEY_AGE;
use super::identities::{ALTERNATE_FIRST, ALTERNATE_SECOND, DEFAULT, TRUST_DELAY, UNVERIFIED, VERIFIED};
use super::outbox::{MAX_RETRY_COUNTS, RESEND_AGE, RETRY_LIMIT, SENT_AGE};
use super::sealing::{DISABLED, ENABLED, UNKNOWN, UNRESTRICTED, access_key};
use super::*;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::Crypto;
use crate::signal::attachment::Attachment;
use crate::signal::content::{
    self, ACCEPT, BLOCK, BLOCK_AND_DELETE, Content, DELETE, DecryptionError, DeleteForMe, Identifier, PniChange, Text,
};
use crate::signal::conversation::{Deleter, Entry, Status};
use crate::signal::curve::{self, Pair};
use crate::signal::errand::{Errand, Outcome, Step};
use crate::signal::group::{self, Context, GroupUpdate};
use crate::signal::keys::{CHECK_PERIOD, KyberKey, MAX_CONFIRMED_AGE, RENEW_AGE};
use crate::signal::message::{encode_key, encode_kyber};
use crate::signal::sender_keys::tests::{distribution, seal};
use crate::signal::session::{Bundle, ChainKey, Error, State};
use crate::signal::storage::samples::contact_record;
use crate::signal::storage::{AccountRecord, ContactRecord, GroupRecord, ListState, Pin};
use crate::signal::tests::Standin;
use alloc::format;
use alloc::string::String;
use alloc::vec;

impl Device {
    pub(crate) fn unkeyed() -> Device {
        let account = Account {
            aci: [3; 16],
            pni: None,
            number: None,
            device: 2,
            password: String::from("secret"),
            aci_identity: Pair {
                private: [1; 32],
                public: curve::tests::x25519(&[1; 32], None).expect("the key of the private one"),
            },
            pni_identity: None,
            registration: 1,
            pni_registration: 1,
            profile_key: [0; 32],
            master_key: [0; 32],
            read_receipts: false,
            credential_salt: None,
            is_number_shared: false,
            listing: crate::signal::account::Listing::default(),
        };
        Device::new(account, BTreeMap::new())
    }

    pub(crate) fn set_list_state(&mut self, peer: Peer, state: ListState) {
        self.apply_list_state(peer, state);
    }

    fn add_contact(&mut self, contact: Contact) {
        self.contacts.insert(contact.aci, contact);
        self.dirty.insert(File::Contacts);
    }

    pub(crate) fn add_named_contact(&mut self, aci: ServiceId, name: &str, is_hidden: bool) {
        self.add_contact(Contact { name: Some(name.into()), is_hidden, ..Contact::new(aci) });
    }

    pub(crate) fn add_incoming(
        &mut self,
        from: ServiceId,
        (body, placeholders): (&str, &[content::Placeholder]),
        timestamp: u64,
        now: u64,
    ) {
        let text = Text { timestamp, body: body.into(), placeholders: placeholders.to_vec(), ..Text::default() };
        self.apply(&Standin::default(), Content::Text(text), &Address { service: from, device: 1 }, now);
    }

    // A JPEG of a contact's message, which the CDN holds under the key `k`, its digest `[digest; 32]`.
    pub(crate) fn add_incoming_photo(&mut self, from: ServiceId, (timestamp, digest): (u64, u8), now: u64) {
        let pointer = Writer::new()
            .text(2, "image/jpeg")
            .bytes(3, &[3; 64])
            .varint(4, 15)
            .bytes(6, &[digest; 32])
            .text(15, "k")
            .finish();
        let attachment = Attachment::read(&Message::parse(&pointer).expect("a pointer")).expect("an attachment");
        let text = Text { timestamp, attachments: vec![attachment], ..Text::default() };
        self.apply(&Standin::default(), Content::Text(text), &Address { service: from, device: 1 }, now);
    }

    pub(in crate::signal) fn keyed(crypto: &impl Crypto, at: u64) -> Device {
        let mut device = Device::unkeyed();
        let mut ring = Ring::generate(crypto, &device.account.aci_identity, at).expect("keys");
        ring.confirm((Some(ring.signed_keys[0].id), Some(ring.last_resort[0].id)), at);
        device.rings.insert(Identity::Aci, ring);
        device
    }

    pub(in crate::signal) const fn contacts(&self) -> &BTreeMap<ServiceId, Contact> {
        &self.contacts
    }

    pub(in crate::signal) fn sent_content(&self, recipient: ServiceId, timestamp: u64) -> Option<(Vec<u8>, bool)> {
        let record =
            self.sent_records.iter().find(|record| record.recipient == recipient && record.timestamp == timestamp)?;
        Some((record.content.clone(), record.is_urgent))
    }

    pub(in crate::signal) fn has_session(&self, address: &Address) -> bool {
        self.sessions.contains_key(address)
    }

    pub(crate) fn note_group_revision(&mut self, crypto: &impl Crypto, master_key: &[u8; 32], revision: u32) -> Peer {
        self.note_group(crypto, &Context { master_key: *master_key, revision })
    }

    pub(in crate::signal) fn add_endorsed_group(
        &mut self,
        crypto: &Standin,
        master_key: &[u8; 32],
        members: &[ServiceId],
        (server, now): (&crate::signal::zk::credential::tests::Server, u64),
    ) -> Peer {
        let peer = self.add_group(crypto, master_key, members);
        let response = group::tests::endorsements_of(crypto, server, master_key, members, now);
        let key = group::Keys::new(crypto, master_key).uid;
        let root = server.issuer.endorsement_root;
        let seconds = now / crate::base::time::MILLIS_PER_SECOND;
        let endorsements =
            crate::signal::zk::endorsement::parse_endorsements(crypto, &response, members, (&key, &root), seconds);
        self.group_mut(&peer).expect("the group").endorsements = endorsements;
        peer
    }

    pub(crate) fn add_group(&mut self, crypto: &impl Crypto, master_key: &[u8; 32], members: &[ServiceId]) -> Peer {
        let peer = self.ensure_group(crypto, master_key);
        let group = self.group_mut(&peer).expect("the group");
        let revision = group.latest_revision();
        group.apply(if members.is_empty() {
            GroupUpdate::Gone
        } else {
            GroupUpdate::State(group::State { revision, members: members.to_vec(), ..group::State::default() })
        });
        peer
    }

    pub(in crate::signal) fn apply(
        &mut self,
        crypto: &impl Crypto,
        content: Content,
        sender: &Address,
        now: u64,
    ) -> Option<Event> {
        self.apply_content(crypto, content, sender, (now, now)).ok().flatten()
    }
}

const ALICE: ServiceId = ServiceId::Aci([4; 16]);
const BORIS: ServiceId = ServiceId::Aci([5; 16]);

fn climbing(revision: u32) -> Context {
    let bytes = Context::write(&[8; 32], revision).finish();
    Context::read(&Message::parse(&bytes).unwrap()).unwrap()
}

// A session with a device of an account, under the account's own key where the account is the R1's.
pub(in crate::signal) fn session_with(device: &mut Device, crypto: &Standin, address: Address, registration: u32) {
    let is_own = address.service == device.account.aci();
    let identity = if is_own { device.account.aci_identity } else { Pair::new(crypto).unwrap() };
    session_under(device, crypto, (address, registration), &identity, 0);
}

fn session_under(
    device: &mut Device,
    crypto: &Standin,
    (address, registration): (Address, u32),
    identity: &Pair,
    now: u64,
) {
    let is_own = address.service == device.account.aci();
    let signed = Pair::new(crypto).unwrap();
    let (_, kyber) = crypto.lattice_pair().unwrap();
    let signature = |key: &[u8]| curve::sign(crypto, &identity.private, &[key], &[0; 64]);
    let bundle = Bundle {
        registration,
        identity: identity.public,
        pre_key: None,
        signed_pre_key: (2, signed.public, signature(&encode_key(&signed.public))),
        kyber_pre_key: (3, kyber.clone(), signature(&encode_kyber(&kyber))),
    };
    let state = State::begin(crypto, &device.account.aci_identity, 11, &bundle, is_own, now).unwrap();
    device.begin_session(address, state);
}

// An account with sessions to its devices, each by its id and registration, under one identity key, which is held for it.
pub(in crate::signal) fn member(
    device: &mut Device,
    crypto: &Standin,
    service: ServiceId,
    devices: &[(u32, u32)],
    now: u64,
) -> Pair {
    let identity = Pair::new(crypto).unwrap();
    for (id, registration) in devices {
        session_under(device, crypto, (Address { service, device: *id }, *registration), &identity, now);
    }
    device.save_identity(service, identity.public, None, 0);
    identity
}

#[test]
fn access_key_kat() {
    // libsignal v0.105.1, rust/zkgroup/src/api/profiles/profile_key.rs, `access_key_kat`.
    use crate::base::code::hex;
    use crate::base::code::unhex;
    for (profile_key, expected) in [
        ("b95042a2c2d9e5b3bb09300ee408a172facd96e91b504e043a5a023dc4cff359", "24fb96d4a5e333e9d4451205b9e2faed"),
        ("26197b17e5a2c36d8c9518c35358f123c476000db6da7565c0d41f6674462c4d", "e895c30cf780757d22f7a179708b14a1"),
    ] {
        let profile_key = unhex(profile_key).unwrap().try_into().unwrap();
        assert_eq!(
            access_key(&crate::signal::tests::Exact::default(), &profile_key).map(|key| hex(&key)),
            Some(expected.into())
        );
    }
}

#[test]
fn files_round_trip() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    device.compose(&crypto, Peer::Contact(ALICE), ("hi", &[]), 1_000, false);
    device.log_sent(ALICE, (1_000, true), b"hi", Some(Peer::Contact(ALICE)), 1_000);
    let group = device.add_group(&crypto, &[8; 32], &[ALICE, BORIS]);
    device.note_group(&crypto, &climbing(2));
    let known =
        KnownIdentity { key: [5; 32], verified: UNVERIFIED, first_use: false, timestamp: 900, is_approved: true };
    device.identities.insert(ALICE, known);
    let alice = Contact {
        name: Some("Alice".into()),
        is_system: true,
        is_sharing: true,
        response: ACCEPT,
        unregistered: Some(4_000),
        ..Contact::new(ALICE)
    };
    device.add_contact(alice);
    device.add_contact(Contact { is_hidden: true, ..Contact::new(BORIS) });
    (device.guids, device.storage_version, device.storage_read_at, device.has_listed_groups, device.is_synced) =
        (VecDeque::from([vec![7; 16]]), 9, 77, true, true);
    device.set_blocked(Peer::Contact(BORIS), true);
    device.set_blocked(group, true);
    let sender = Address { service: ALICE, device: 3 };
    let signing = Pair::new(&crypto).unwrap();
    device.take_distribution(&sender, &distribution([6; 16], 1, &ChainKey { key: [1; 32], index: 0 }, &signing));
    let names = [
        File::Account,
        File::Identities,
        File::Contacts,
        File::Conversation(Peer::Contact(ALICE)),
        File::Conversation(group),
        File::SenderKey(sender, [6; 16]),
        File::Seen,
        File::Sent,
        File::Blocked,
    ];
    let session = File::Session(Address { service: ALICE, device: 3 });
    for file in [File::Ring(Identity::Pni), session].iter().chain(&names) {
        let (directory, name) = file.to_path();
        assert_eq!(File::from_path(directory, &name).as_ref(), Some(file), "known by the name it is written under");
    }
    assert_eq!((File::from_path(DIRECTORY, "sessions"), File::from_path(SESSIONS, "stray")), (None, None));
    let files = names.iter().map(|file| (file.clone(), device.file_bytes(file).expect("written"))).collect();
    let copy = Device::read(&files).expect("a device");
    assert_eq!(
        names.clone().map(|file| copy.file_bytes(&file)),
        names.map(|file| device.file_bytes(&file)),
        "read back, each file is written again to the same bytes"
    );
}

#[test]
fn contact_message_once() {
    let mut device = Device::unkeyed();
    let sender = Address { service: ALICE, device: 1 };
    let text = Text { timestamp: 7, body: "Lunch?".into(), ..Text::default() };
    let event = device.apply(&Standin::default(), Content::Text(text.clone()), &sender, 1_000);
    let expected = Event::Arrived { peer: Peer::Contact(ALICE), timestamp: 7, text: "Lunch?".into(), is_noticed: true };
    assert_eq!(event, Some(expected));
    assert_eq!(
        device.apply(&Standin::default(), Content::Text(text), &sender, 2_000),
        None,
        "the same message again is none"
    );
}

#[test]
fn group_messages_and_revisions() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    device.add_contact(Contact { name: Some("Alice".into()), ..Contact::new(ALICE) });
    let text = |timestamp, body: &str, revision| Text {
        timestamp,
        body: body.into(),
        group: Some(climbing(revision)),
        ..Text::default()
    };
    let (alice, boris) = (Address { service: ALICE, device: 1 }, Address { service: BORIS, device: 1 });
    let Some(Event::Arrived { peer, .. }) = device.apply(&crypto, Content::Text(text(7, "Up?", 2)), &alice, 1_000)
    else {
        panic!("a group's message")
    };
    assert_eq!(peer, Peer::Group(group::id_of(&crypto, &[8; 32])), "by Signal's id of the group");
    assert_eq!(device.groups_due(), [(peer, [8; 32], Some(2), false)], "a group whose state the R1 has not got is due");
    assert_eq!((device.name_of(&peer), device.recipients(&peer, 0)), (None, Vec::new()), "nobody known");
    device.add_group(&crypto, &[8; 32], &[device.account.aci(), ALICE, BORIS]);
    assert_eq!(device.group_due(&peer), (None, false), "its state is had");
    assert!(device.apply(&crypto, Content::Text(text(7, "Yes", 2)), &boris, 1_001).is_some(), "another's of that ms");
    assert_eq!(
        device.entries(&peer).iter().map(|entry| (entry.sender, entry.body.as_str())).collect::<Vec<_>>(),
        [(Some(ALICE), "Up?"), (Some(BORIS), "Yes")]
    );
    assert_eq!(
        (device.name_of(&peer), device.writers_of(device.entries(&peer))),
        (Some("Alice".into()), BTreeMap::from([(ALICE, "Alice".into())])),
        "untitled, it goes by the names of its members but the owner"
    );
    assert_eq!(device.recipients(&peer, 0), [ALICE, BORIS], "a reply goes to every member but the owner");
    let invited = device.add_group(&crypto, &[7; 32], &[ALICE]);
    assert_eq!(device.recipients(&invited, 0), Vec::new(), "nothing goes to a group the owner is no member of yet");
    let sent = Content::Sent { to: None, text: text(9, "Sure", 3), started: Some(2_900) };
    assert_eq!(
        device.apply(&crypto, sent, &Address { service: device.account.aci(), device: 1 }, 3_000),
        Some(Event::Wrote(peer))
    );
    assert_eq!(device.group_due(&peer), (Some(3), false), "the owner's message from the phone names a later revision");
    assert_eq!(device.recipients(&peer, 0), [ALICE, BORIS], "which changes nobody until the server tells");
    device.apply(&crypto, Content::Text(text_with_timer(5, 60, 2)), &alice, 3_100);
    let direct = Peer::Contact(ALICE);
    assert_eq!(
        device.apply(&crypto, Content::ReadElsewhere { reads: vec![(ALICE, 7)], at: 3_500 }, &alice, 4_000),
        Some(Event::Changed(peer)),
        "a read elsewhere reaches the group that holds the message"
    );
    let entry = &device.entries(&direct)[0];
    assert_eq!(
        (entry.is_read, entry.started),
        (false, None),
        "and leaves her earlier message to the owner alone unread, its timer not begun"
    );
    assert_eq!(
        device.apply(&crypto, Content::ReadElsewhere { reads: vec![(ALICE, 5)], at: 3_600 }, &alice, 4_100),
        Some(Event::Changed(direct)),
        "a message no group holds is her own conversation's"
    );
    assert_eq!(device.entries(&direct)[0].started, Some(3_600), "read there, its timer begins as it was read");
}

fn text_with_timer(timestamp: u64, expire: u32, expire_version: u32) -> Text {
    Text { timestamp, body: "hi".into(), expire, expire_version, ..Text::default() }
}

#[test]
fn timer_by_version() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let alice = Address { service: ALICE, device: 1 };
    let own = Address { service: device.account.aci(), device: 1 };
    let timer = Content::Timer(Text { body: String::new(), ..text_with_timer(7, 60, 2) });
    let event = device.apply(&crypto, timer, &alice, 1_000);
    let peer = Peer::Contact(ALICE);
    assert_eq!(event, Some(Event::Changed(peer)), "a contact turned the timer on");
    assert_eq!(device.timer_of(&peer), (60, 2), "the R1's messages to the contact go under it");
    device.apply(&crypto, Content::Text(text_with_timer(8, 0, 1)), &alice, 1_100);
    assert_eq!(device.timer_of(&peer), (60, 2), "a message of an earlier version changes nothing");
    device.apply(&crypto, Content::SentTimer { to: Some(ALICE), text: text_with_timer(9, 0, 3) }, &own, 1_200);
    assert_eq!(device.timer_of(&peer), (0, 3), "the owner turned it off on the phone");
    let other = Peer::Contact(BORIS);
    assert_eq!(device.timer_of(&other), (0, 1), "a conversation never timed goes at Signal-Desktop's first version");
    let group = device.add_group(&crypto, &[8; 32], &[device.account.aci(), ALICE]);
    let to_group = Text { group: Some(climbing(0)), ..text_with_timer(10, 30, 9) };
    device.apply(&crypto, Content::Text(to_group), &alice, 1_300);
    assert_eq!(device.timer_of(&group), (0, 0), "a group's timer is its state's, its messages' none of it");
    assert_eq!(device.entries(&group)[0].expire, 30, "though each message keeps its own");
}

#[test]
fn note_to_self_transcript() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let own = Peer::Contact(device.account.aci());
    let order = device.compose(&crypto, own, ("milk", &[]), 5_000, false).expect("an order");
    let entry = &device.entries(&own)[0];
    assert_eq!((entry.status, entry.started), (Status::Sending, Some(5_000)), "its timer begun as it is written");
    let Some(Content::Sent { to, text, started }) = content::parse_content(&order.content, 5_000, true) else {
        panic!("a transcript")
    };
    assert_eq!(
        (to, text.body.as_str(), started, order.is_urgent),
        (Some(device.account.aci()), "milk", Some(5_000), false),
        "the owner's own message, which another device shows as sent"
    );
    assert!(!content::parse_content(&order.content, 5_000, false).is_some_and(|content| content != Content::Nothing));
    assert_eq!(device.note_sent(&crypto, own, (5_000, &[]), Status::Sent, 5_100), None, "no second transcript");
    assert_eq!(device.entries(&own)[0].status, Status::Sent);
}

#[test]
fn sent_timer_start() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let peer = Peer::Contact(ALICE);
    device.apply(&crypto, Content::Timer(text_with_timer(1, 60, 2)), &Address { service: ALICE, device: 1 }, 500);
    device.compose(&crypto, peer, ("hi", &[]), 1_000, false).expect("an order");
    assert_eq!(device.entries(&peer)[0].started, None, "not before it is sent");
    let transcript = device.note_sent(&crypto, peer, (1_000, &[ALICE]), Status::Sent, 1_400).expect("a transcript");
    assert_eq!(device.entries(&peer)[0].started, Some(1_400));
    let Some(Content::Sent { started, text, .. }) = content::parse_content(&transcript.content, 1_000, true) else {
        panic!("a transcript")
    };
    assert_eq!((started, text.expire, text.expire_version), (Some(1_400), 60, 2));
}

#[test]
fn read_receipt_chunks() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    device.account.read_receipts = true;
    device.add_contact(Contact { is_system: true, ..Contact::new(ALICE) });
    let alice = Address { service: ALICE, device: 1 };
    for timestamp in 1..=150 {
        device.apply(&crypto, Content::Text(text_with_timer(timestamp, 0, 1)), &alice, timestamp);
    }
    let orders = device.mark_read(&crypto, Peer::Contact(ALICE), 10_000);
    let receipts: Vec<usize> = orders[..orders.len() - 1]
        .iter()
        .map(|order| Message::parse(&order.content).unwrap().message(5).unwrap().varints(2).len())
        .collect();
    assert_eq!(receipts, [100, 50], "a receipt of at most 100 timestamps, as Signal-Desktop chunks them");
    assert!(orders.last().unwrap().is_urgent, "the read sync goes urgent, as Signal-Desktop sends it");
}

fn content_of(timestamp: u64, body: &str, revision: Option<u32>) -> Vec<u8> {
    content::write_message(content::write_data(
        body,
        timestamp,
        Some(&[2; 32]),
        (0, 1),
        revision.map(|revision| (&[8; 32], revision)),
    ))
}

#[test]
fn later_revision_waits() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let own = device.account.aci();
    let (alice, boris) = (Address { service: ALICE, device: 1 }, Address { service: BORIS, device: 1 });
    let group = Peer::Group(group::id_of(&crypto, &[8; 32]));
    let reception = device.receive_content(&crypto, &content_of(7, "Up?", Some(2)), alice, (7, true, 0), 1_000, false);
    assert!(matches!(reception, Ok(Reception::Queued)), "a group not fetched yet");
    assert_eq!((device.group_due(&group), device.entries(&group).len()), ((Some(2), false), 0), "due, nothing shown");
    let copy = Device::read(
        &[
            (File::Account, device.file_bytes(&File::Account).unwrap()),
            (File::Conversation(group), device.file_bytes(&File::Conversation(group)).unwrap()),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    assert_eq!(copy.conversation(&group).map(|conversation| conversation.queue.len()), Some(1), "what waits is kept");
    device.group_mut(&group).unwrap().is_sharing = true;
    let reception = device.receive_content(&crypto, &content_of(6, "Me", Some(2)), boris, (6, true, 0), 1_050, false);
    assert!(matches!(reception, Ok(Reception::Queued)), "another's waits with it");
    let state = |members: &[ServiceId], admins: &[ServiceId], announcements_only, terminated| group::State {
        revision: 2,
        members: members.to_vec(),
        admins: admins.to_vec(),
        announcements_only,
        terminated,
        ..group::State::default()
    };
    let (events, drops) =
        device.apply_group_updates(&crypto, vec![(group, GroupUpdate::State(state(&[own, ALICE], &[], false, false)))]);
    assert_eq!(
        events,
        [Event::Arrived { peer: group, timestamp: 7, text: "Up?".into(), is_noticed: true }],
        "taken once its state came"
    );
    assert_eq!(drops, [DropReason::NotMember], "and the one of no member dropped then, with why for the log");
    assert_eq!(
        device.owed_receipts,
        [(ALICE, group, 7)],
        "sealed to a group shared with and empty: a delivery receipt"
    );
    assert_eq!(
        device.receive_content(&crypto, &content_of(8, "Me too", Some(2)), boris, (8, true, 0), 1_100, false).err(),
        Some(DropReason::NotMember),
        "from no member"
    );
    device.apply_group_updates(
        &crypto,
        vec![(group, GroupUpdate::State(state(&[own, ALICE, BORIS], &[ALICE], true, false)))],
    );
    assert_eq!(
        device.receive_content(&crypto, &content_of(9, "Me too", Some(2)), boris, (9, true, 0), 1_200, false).err(),
        Some(DropReason::NotMember),
        "where only administrators send"
    );
    assert!(device.recipients(&group, 0).is_empty(), "where the owner is none either, nothing of the owner's goes");
    device.apply_group_updates(&crypto, vec![(group, GroupUpdate::State(state(&[own, ALICE], &[], false, true)))]);
    device.owed_receipts.clear();
    device.group_mut(&group).unwrap().response = ACCEPT;
    assert_eq!(
        device.receive_content(&crypto, &content_of(10, "Bye", Some(2)), alice, (10, true, 0), 1_300, false).err(),
        Some(DropReason::Ended),
        "to an ended group"
    );
    assert_eq!(device.owed_receipts, [(ALICE, group, 10)], "after its delivery receipt");
    assert!(device.recipients(&group, 0).is_empty(), "and nothing of the owner's goes to an ended group");
    device.apply_group_updates(&crypto, vec![(group, GroupUpdate::Gone)]);
    assert_eq!(
        device.receive_content(&crypto, &content_of(11, "Hello?", Some(2)), alice, (11, false, 0), 1_400, false).err(),
        Some(DropReason::NotMember),
        "the owner is no member any more"
    );
    device.set_blocked(group, true);
    assert_eq!(
        device.receive_content(&crypto, &content_of(12, "Hi", Some(9)), boris, (12, false, 0), 1_500, false).err(),
        Some(DropReason::Blocked),
        "to a blocked group, which waits for nothing"
    );
    assert_eq!(
        device.contacts.get(&BORIS).and_then(|contact| contact.profile_key),
        Some([2; 32]),
        "its profile key taken first"
    );
    device.set_blocked(Peer::Contact(ALICE), true);
    assert_eq!(
        device.receive_content(&crypto, &content_of(13, "Hi", None), alice, (13, true, 0), 1_600, false).err(),
        Some(DropReason::Blocked),
        "from a blocked account"
    );
}

#[test]
fn delivery_receipts() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let alice = Address { service: ALICE, device: 1 };
    device.note_unregistered(ALICE, 0);
    device.receive_content(&crypto, &content_of(1, "Hi", None), alice, (1, true, 0), 10, false).unwrap();
    assert_eq!(
        device.owed_receipts,
        [(ALICE, Peer::Contact(ALICE), 1)],
        "a stranger's first message, as Signal-Desktop's empty conversation is accepted"
    );
    assert!(!device.is_off_signal(&ALICE, 10), "a message marks its sender on Signal");
    device.receive_content(&crypto, &content_of(2, "Hi?", None), alice, (2, true, 0), 20, false).unwrap();
    device.receive_content(&crypto, &content_of(3, "Hey", None), alice, (3, false, 0), 30, false).unwrap();
    assert_eq!(device.owed_receipts.len(), 1, "none for a message request's next, nor for an unsealed message");
    device.add_contact(Contact { is_sharing: true, ..Contact::new(ALICE) });
    for timestamp in 4..=104 {
        device
            .receive_content(&crypto, &content_of(timestamp, "more", None), alice, (timestamp, true, 0), 40, false)
            .unwrap();
    }
    device.receive_content(&crypto, &content_of(104, "more", None), alice, (104, true, 0), 41, false).unwrap();
    let orders = device.take_delivery_receipts(50);
    let receipts: Vec<(u64, usize)> = orders
        .iter()
        .map(|order| {
            let receipt = Message::parse(&order.content).unwrap().message(5).unwrap();
            (receipt.varint(1).unwrap(), receipt.varints(2).len())
        })
        .collect();
    assert_eq!(
        receipts,
        [(0, 100), (0, 2)],
        "delivery receipts of 100 timestamps at the most, one for a message taken twice none"
    );
    assert!(orders.iter().all(|order| !order.is_urgent && order.recipients == [ALICE]));
    assert!(device.owed_receipts.is_empty(), "sent once");
    device.receive_content(&crypto, &content_of(105, "last", None), alice, (105, true, 0), 60, false).unwrap();
    device.set_blocked(Peer::Contact(ALICE), true);
    assert!(device.take_delivery_receipts(70).is_empty(), "none to an account blocked since");
}

#[test]
fn profile_key_responses() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let (alice, boris) = (Address { service: ALICE, device: 1 }, Address { service: BORIS, device: 1 });
    device.add_contact(Contact { is_sharing: true, ..Contact::new(ALICE) });
    device.receive_content(&crypto, &content_of(1, "Hi", None), alice, (1, true, 0), 10, false).unwrap();
    device.receive_content(&crypto, &content_of(2, "Hi", None), boris, (2, false, 0), 10, false).unwrap();
    assert!(device.owed_profile_keys.is_empty(), "sealed, or from a stranger: nothing owed");
    device.receive_content(&crypto, &content_of(3, "Hi", None), alice, (3, false, 0), 10, false).unwrap();
    device.receive_content(&crypto, &content_of(4, "Hi", None), alice, (4, false, 0), 10, false).unwrap();
    let responses = device.take_profile_key_responses(20);
    assert_eq!(responses.len(), 1, "one for the contact, however many messages");
    let data = Message::parse(&responses[0].content).unwrap().message(1).unwrap();
    assert_eq!(
        (data.varint(4), data.bytes(6), data.varint(7), responses[0].is_urgent),
        (Some(4), Some(&device.account.profile_key[..]), Some(20), false),
        "the profile key alone, not urgent"
    );
    assert!(device.take_profile_key_responses(30).is_empty(), "sent once");
}

#[test]
fn owner_responses_and_records() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let own = Address { service: device.account.aci(), device: 1 };
    let group = device.add_group(&crypto, &[8; 32], &[own.service, ALICE]);
    let unknown = [3; 32];
    let blocked = Content::Blocked { acis: Some(vec![ALICE]), groups: vec![group::id_of(&crypto, &[8; 32]), unknown] };
    device.apply(&crypto, blocked, &own, 1);
    assert!(device.is_blocked(&Peer::Contact(ALICE)) && device.is_blocked(&Peer::Group(unknown)), "the lists as told");
    assert_eq!(
        (device.contacts[&ALICE].response, device.conversations[&group].group.as_ref().map(|group| group.response)),
        (BLOCK, Some(BLOCK)),
        "each newly blocked, the group known"
    );
    device.apply(&crypto, Content::Blocked { acis: None, groups: vec![] }, &own, 2);
    assert!(
        device.is_blocked(&Peer::Contact(ALICE)) && !device.is_blocked(&Peer::Group(unknown)),
        "accounts named in no field stay as they were"
    );
    device.apply(&crypto, Content::Blocked { acis: Some(vec![]), groups: vec![] }, &own, 3);
    let alice = &device.contacts[&ALICE];
    assert!(
        !device.is_blocked(&Peer::Contact(ALICE)) && alice.is_sharing && alice.response == ACCEPT,
        "no longer blocked: accepted"
    );
    device.apply(&crypto, Content::Text(text_with_timer(4, 0, 1)), &Address { service: ALICE, device: 1 }, 4);
    assert_eq!(
        device.apply(&crypto, Content::Response { thread: Peer::Contact(ALICE), kind: DELETE }, &own, 5),
        Some(Event::Changed(Peer::Contact(ALICE)))
    );
    assert!(
        device.entries(&Peer::Contact(ALICE)).is_empty() && !device.contacts[&ALICE].is_sharing,
        "deleted on the phone"
    );
    device.apply(&crypto, Content::Response { thread: group, kind: BLOCK_AND_DELETE }, &own, 6);
    assert!(device.is_blocked(&group) && !device.is_sharing(&group), "a group blocked and deleted from the phone");
    device.apply(&crypto, Content::Configuration { read_receipts: true }, &own, 7);
    device.apply(&crypto, Content::FetchStorage, &own, 8);
    assert!(device.account.read_receipts && device.is_storage_due);
    let record = |blocked, whitelisted, hidden| ContactRecord {
        profile_key: Some([5; 32]),
        is_system: true,
        blocked,
        whitelisted,
        hidden,
        ..contact_record(BORIS)
    };
    device.add_contact(Contact { profile_key: Some([4; 32]), unregistered: Some(9), ..Contact::new(BORIS) });
    device.merge_contact(record(false, true, false), false, 10);
    let boris = &device.contacts[&BORIS];
    assert_eq!(
        (boris.profile_key, boris.is_system, boris.is_sharing, boris.response, boris.unregistered),
        (Some([4; 32]), true, true, ACCEPT, None),
        "whitelisted: accepted; the profile key held kept after the first reading; on Signal"
    );
    device.merge_contact(record(true, false, false), true, 10);
    assert!(device.is_blocked(&Peer::Contact(BORIS)) && !device.contacts[&BORIS].is_sharing);
    assert_eq!(device.contacts[&BORIS].profile_key, Some([5; 32]), "at the first reading the record's profile key");
    device.merge_contact(record(false, false, true), false, 10);
    let boris = &device.contacts[&BORIS];
    assert!(!device.is_blocked(&Peer::Contact(BORIS)) && boris.is_hidden && boris.response == 0);
    assert!(
        !device.is_accepted(&Peer::Contact(BORIS), true),
        "a removed contact is not accepted, though the address book names it"
    );
    let own_record = ContactRecord { aci: own.service, ..record(true, false, false) };
    device.merge_contact(own_record, false, 10);
    assert!(!device.is_blocked(&Peer::Contact(own.service)), "the account's own record is passed over");
}

#[test]
fn identity_trust() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let own = device.account.aci();
    assert_eq!(device.trust(&own, &device.account.aci_identity.public, 0), Trust::Trusted, "the account's own key");
    assert_eq!(device.trust(&own, &[7; 32], 0), Trust::Never, "and no other for it");
    assert!(!device.is_trusted_receiving(&own, &[7; 32]) && device.is_trusted_receiving(&ALICE, &[7; 32]));
    assert!(
        !device.save_identity(own, [7; 32], Some(1), 0) && device.trust(&own, &[7; 32], 0) == Trust::Never,
        "never changed"
    );
    assert_eq!(device.trust(&ALICE, &[5; 32], 0), Trust::Trusted, "none held yet");
    assert!(!device.save_identity(ALICE, [5; 32], Some(1), 1_000), "the first is no change");
    assert_eq!(
        device.identities[&ALICE],
        KnownIdentity { key: [5; 32], verified: DEFAULT, first_use: true, timestamp: 1_000, is_approved: false }
    );
    assert_eq!(device.trust(&ALICE, &[5; 32], 1_000), Trust::Trusted, "a first key waits for nothing");
    for id in [1, 2, 3] {
        session_with(&mut device, &crypto, Address { service: ALICE, device: id }, id);
    }
    assert!(device.save_identity(ALICE, [6; 32], Some(2), 2_000), "a change");
    assert!(
        device.conversation(&Peer::Contact(ALICE)).is_some_and(|conversation| conversation.is_safety_changed),
        "told"
    );
    assert_eq!(
        [1, 2, 3].map(|id| device.sends_to(&Address { service: ALICE, device: id }, 2_000)),
        [false, true, false],
        "the sessions with the account's other devices set aside"
    );
    assert_eq!(device.trust(&ALICE, &[5; 32], 2_000), Trust::Never, "the key replaced");
    assert_eq!(device.trust(&ALICE, &[6; 32], 2_000 + TRUST_DELAY - 1), Trust::Soon, "a change waits");
    assert_eq!(device.trust(&ALICE, &[6; 32], 2_000 + TRUST_DELAY), Trust::Trusted, "five seconds");
    device.identities.get_mut(&ALICE).unwrap().verified = VERIFIED;
    device.save_identity(ALICE, [7; 32], Some(1), 9_000);
    assert_eq!(
        (device.identities[&ALICE].verified, device.trust(&ALICE, &[7; 32], 99_000)),
        (UNVERIFIED, Trust::Never),
        "a verified key's change is left unverified until the phone verifies it"
    );
    device.take_record_identity(ALICE, ([7; 32], VERIFIED), false, 10_000);
    assert_eq!(
        (
            device.identities[&ALICE].verified,
            device.identities[&ALICE].is_approved,
            device.trust(&ALICE, &[7; 32], 10_000)
        ),
        (VERIFIED, true, Trust::Trusted),
        "verified again on the phone, the storage service's record tells it, approved"
    );
    device.conversation_mut(Peer::Contact(ALICE)).is_safety_changed = false;
    device.take_record_identity(ALICE, ([8; 32], DEFAULT), false, 11_000);
    assert_eq!(device.identities[&ALICE].key, [7; 32], "another key is taken from a record only at the first reading");
    device.take_record_identity(ALICE, ([8; 32], DEFAULT), true, 12_000);
    assert_eq!(
        device.identities[&ALICE],
        KnownIdentity { key: [8; 32], verified: DEFAULT, first_use: false, timestamp: 12_000, is_approved: true }
    );
    assert!(device.conversation(&Peer::Contact(ALICE)).is_some_and(|conversation| conversation.is_safety_changed));
    device.take_record_identity(BORIS, ([9; 32], 5), false, 13_000);
    assert_eq!(
        device.identities[&BORIS],
        KnownIdentity { key: [9; 32], verified: DEFAULT, first_use: true, timestamp: 13_000, is_approved: true },
        "a state not known is the default"
    );
    let old = Writer::new().message(1, Writer::new().bytes(1, &BORIS.to_binary()).bytes(2, &[9; 32])).finish();
    let copy = Device::read(
        &[(File::Account, device.file_bytes(&File::Account).unwrap()), (File::Identities, old)].into_iter().collect(),
    )
    .unwrap();
    assert_eq!(
        copy.identities[&BORIS],
        KnownIdentity { key: [9; 32], verified: DEFAULT, first_use: false, timestamp: 0, is_approved: false },
        "a key kept before keys had a state"
    );
    assert_eq!(copy.trust(&BORIS, &[9; 32], 1_800_000_000_000), Trust::Trusted, "which is trusted");
    assert_eq!(
        copy.identities[&own].verified, VERIFIED,
        "the account's own key held where a device linked before is read"
    );
}

pub(in crate::signal) fn keys_document(crypto: &Standin, identity: &Pair) -> String {
    use crate::base::code::base64;
    let (signed, (_, kyber)) = (Pair::new(crypto).unwrap(), crypto.lattice_pair().unwrap());
    let signature = |key: &[u8]| base64(&curve::sign(crypto, &identity.private, &[key], &[0; 64]));
    format!(
        r#"{{"identityKey":"{}","devices":[{{"deviceId":1,"registrationId":7,"preKey":null,"signedPreKey":{{"keyId":2,"publicKey":"{}","signature":"{}"}},"pqPreKey":{{"keyId":4,"publicKey":"{}","signature":"{}"}}}}]}}"#,
        base64(&encode_key(&identity.public)),
        base64(&encode_key(&signed.public)),
        signature(&encode_key(&signed.public)),
        base64(&encode_kyber(&kyber)),
        signature(&encode_kyber(&kyber))
    )
}

#[test]
fn changed_identity_holds_errands() {
    use crate::signal::service;
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let own = device.account.aci();
    let (old, new) = (Pair::new(&crypto).unwrap(), Pair::new(&crypto).unwrap());
    device.save_identity(ALICE, old.public, None, 0);
    let mut errand = Errand::begin(Order::message(Peer::Contact(ALICE), vec![1], 5, true));
    assert!(matches!(errand.next(&mut device, &crypto, 1_000), Step::Keys { .. }), "no session: the pre-keys");
    errand.take_answer(&mut device, &crypto, 200, keys_document(&crypto, &new).as_bytes(), 1_000);
    assert_eq!(errand.next(&mut device, &crypto, 1_000), Step::Done(Outcome::Later), "their new key saved, it waits");
    assert!(!device.has_session(&Address { service: ALICE, device: 1 }), "no session begun under it yet");
    let mut again = Errand::begin(errand.order);
    assert!(matches!(again.next(&mut device, &crypto, 1_000 + TRUST_DELAY), Step::Keys { .. }));
    again.take_answer(&mut device, &crypto, 200, keys_document(&crypto, &new).as_bytes(), 1_000 + TRUST_DELAY);
    assert!(
        matches!(again.next(&mut device, &crypto, 1_000 + TRUST_DELAY), Step::Submit { .. }),
        "five seconds on, it goes"
    );
    let group = device.add_group(&crypto, &[8; 32], &[own, ALICE, BORIS]);
    device.take_record_identity(BORIS, (old.public, VERIFIED), true, 0);
    let mut message = Errand::begin(Order::message(group, vec![1], 9, true));
    let mut step = message.next(&mut device, &crypto, 50_000);
    while !matches!(step, Step::Done(_)) {
        let answer = match &step {
            Step::Keys { path, .. } if *path == service::bundles_path(&BORIS) => keys_document(&crypto, &new),
            _ => String::new(),
        };
        message.take_answer(&mut device, &crypto, 200, answer.as_bytes(), 50_000);
        step = message.next(&mut device, &crypto, 50_000);
    }
    assert_eq!(step, Step::Done(Outcome::Sent), "Alice took it");
    assert_eq!(
        (device.identities[&BORIS].verified, message.rest().is_none()),
        (UNVERIFIED, true),
        "Boris's verified key changed: left unverified, nothing goes to him until the phone verifies it"
    );
    device.take_record_identity(BORIS, (new.public, DEFAULT), true, 60_000);
    device.save_identity(BORIS, old.public, Some(1), 60_000);
    let mut message = Errand::begin(Order::message(group, vec![1], 10, true));
    let mut step = message.next(&mut device, &crypto, 60_000);
    while !matches!(step, Step::Done(_)) {
        message.take_answer(&mut device, &crypto, 200, keys_document(&crypto, &old).as_bytes(), 60_000);
        step = message.next(&mut device, &crypto, 60_000);
    }
    let rest = message.rest().expect("the rest");
    assert_eq!(
        (step, rest.recipients, rest.is_message),
        (Step::Done(Outcome::Sent), alloc::vec![BORIS], false),
        "Boris's key changed a moment ago: the message goes to him on its own once it is trusted"
    );
    session_with(&mut device, &crypto, Address { service: BORIS, device: 2 }, 3);
    device.take_record_identity(BORIS, (new.public, VERIFIED), true, 70_000);
    let mut receipt = Errand::begin(Order::new(BORIS, vec![1], 11, false));
    assert!(
        matches!(receipt.next(&mut device, &crypto, 70_000), Step::Keys { .. }),
        "a session under a key no longer held is set aside, and the pre-keys asked anew"
    );
    assert!(!device.sends_to(&Address { service: BORIS, device: 2 }, 70_000));
}

pub(in crate::signal) fn certificate_answer(expires: u64, has_number: bool) -> String {
    let inner = Writer::new().maybe(has_number.then_some(|w: Writer| w.text(1, "+15550000000"))).finish();
    // The time it runs out at, a fixed64, which the writer has no call for.
    let inner = [&inner[..], &[0x19], &expires.to_le_bytes()].concat();
    let certificate = Writer::new().bytes(1, &inner).bytes(2, &[0; 64]).finish();
    format!(r#"{{"certificate":"{}"}}"#, crate::base::code::base64(&certificate))
}

#[test]
fn seal_and_access_modes() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let hour = 60 * 60 * 1000;
    assert!(device.seal_of(&crypto, &ALICE, None, 0).is_none(), "no sender certificate held");
    assert!(!device.take_certificate(false, &certificate_answer(10 * hour, true), 0), "a number where none was asked");
    assert!(!device.take_certificate(false, &certificate_answer(hour, false), 0), "about to run out");
    assert!(device.take_certificate(false, &certificate_answer(10 * hour, false), 0));
    assert!(device.take_certificate(true, &certificate_answer(10 * hour, true), 0));
    assert_eq!(device.certificate(false, 9 * hour - 1).map(<[u8]>::len), device.certificate(false, 0).map(<[u8]>::len));
    assert!(device.certificate(false, 9 * hour).is_none(), "used no later than an hour before it runs out");
    let seal = device.seal_of(&crypto, &ALICE, None, 0).expect("sealed");
    assert_eq!(seal.access, Access::Key([0; 16]), "no profile key held: the access key of noughts");
    assert_eq!(seal.certificate, device.certificate(false, 0).unwrap(), "the certificate without the number");
    device.account.is_number_shared = true;
    assert_eq!(
        device.seal_of(&crypto, &ALICE, None, 0).unwrap().certificate,
        device.certificate(true, 0).unwrap(),
        "shared with everybody, the number goes"
    );
    device.take_profile_key(ALICE, [7; 32]);
    assert_eq!(
        device.seal_of(&crypto, &ALICE, None, 0).unwrap().access,
        Access::Key(access_key(&crypto, &[7; 32]).unwrap()),
        "the contact's access key"
    );
    assert!(device.seal_of(&crypto, &device.account.aci(), None, 0).is_none(), "never to the owner's own devices");
    device.note_sealed_delivered(ALICE);
    assert_eq!(device.contacts[&ALICE].access_mode, ENABLED, "delivered under its access key");
    device.take_profile_key(ALICE, [8; 32]);
    assert_eq!(device.contacts[&ALICE].access_mode, UNKNOWN, "a new profile key leaves it unknown again");
    device.note_sealed_delivered(BORIS);
    assert_eq!(
        device.contacts[&BORIS].access_mode, UNRESTRICTED,
        "no profile key: it takes sealed messages from anybody"
    );
    assert_eq!(device.seal_of(&crypto, &BORIS, None, 0).unwrap().access, Access::Key([0; 16]));
    device.note_sealed_refused(ALICE);
    assert!(device.seal_of(&crypto, &ALICE, None, 0).is_none(), "refused: unsealed from then on");
    assert_eq!(
        (
            device.member_access(&crypto, &ALICE),
            device.member_access(&crypto, &BORIS),
            device.member_access(&crypto, &ServiceId::Aci([9; 16]))
        ),
        (None, Some([0; 16]), Some([0; 16])),
        "a group's members' pre-keys are asked under no access key where it was refused, else under theirs or noughts"
    );
}

#[test]
fn sealed_failover() {
    use crate::signal::service;
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let hour = 60 * 60 * 1000;
    device.take_certificate(false, &certificate_answer(10 * hour, false), 0);
    device.take_profile_key(ALICE, [7; 32]);
    let access = Some(Access::Key(access_key(&crypto, &[7; 32]).unwrap()));
    let alice = Pair::new(&crypto).unwrap();
    let mut errand = Errand::begin(Order::message(Peer::Contact(ALICE), alloc::vec![1], 5, true));
    assert_eq!(
        errand.next(&mut device, &crypto, 0),
        Step::Keys { path: service::bundles_path(&ALICE), access: access.clone() },
        "the pre-keys asked unauthenticated under the access key"
    );
    errand.take_answer(&mut device, &crypto, 401, b"", 0);
    assert_eq!(
        errand.next(&mut device, &crypto, 0),
        Step::Keys { path: service::bundles_path(&ALICE), access: None },
        "refused: asked under the R1's name"
    );
    errand.take_answer(&mut device, &crypto, 200, keys_document(&crypto, &alice).as_bytes(), 0);
    let Step::Submit { body, access: None, .. } = errand.next(&mut device, &crypto, 0) else { panic!("unsealed") };
    assert!(body.contains(r#""type":3"#), "a PreKey message under the R1's name");
    errand.take_answer(&mut device, &crypto, 200, b"", 0);
    assert_eq!(errand.next(&mut device, &crypto, 0), Step::Done(Outcome::Sent));
    assert_eq!(device.contacts[&ALICE].access_mode, DISABLED, "its sealing refused from then on");
    // A session with a device of an account, whose identity key is held as the session's.
    let session = |device: &mut Device, service: ServiceId| {
        let address = Address { service, device: 1 };
        session_with(device, &crypto, address, 7);
        let key = device.sessions[&address].current.as_ref().unwrap().remote_identity;
        device.save_identity(service, key, Some(1), 0);
    };
    session(&mut device, BORIS);
    let mut receipt = Errand::begin(Order::new(BORIS, alloc::vec![1], 5, false));
    let Step::Submit { body, access: Some(Access::Key(key)), .. } = receipt.next(&mut device, &crypto, 0) else {
        panic!("sealed")
    };
    assert_eq!(key, [0; 16], "under the access key of noughts, no profile key held");
    let sealed = body.split(r#""content":""#).nth(1).and_then(|rest| rest.split('"').next()).unwrap();
    assert!(
        body.contains(r#""type":6"#) && crate::base::code::unbase64(sealed).unwrap()[0] == 0x11,
        "sealed in version 1"
    );
    receipt.take_answer(&mut device, &crypto, 401, b"", 0);
    let Step::Submit { access: None, .. } = receipt.next(&mut device, &crypto, 0) else {
        panic!("sealed anew under the R1's name")
    };
    receipt.take_answer(&mut device, &crypto, 200, b"", 0);
    assert!(receipt.sealed_recipients.is_empty() && device.contacts[&BORIS].access_mode == DISABLED);
    let carol = ServiceId::Aci([6; 16]);
    session(&mut device, carol);
    let mut receipt = Errand::begin(Order::new(carol, alloc::vec![1], 6, false));
    let Step::Submit { access: Some(_), .. } = receipt.next(&mut device, &crypto, 0) else { panic!("sealed") };
    receipt.take_answer(&mut device, &crypto, 200, b"", 0);
    assert_eq!(
        (receipt.sealed_recipients.clone(), device.contacts[&carol].access_mode),
        (alloc::vec![carol], UNRESTRICTED),
        "taken sealed under noughts: it takes sealed messages from anybody"
    );
}

#[test]
fn own_prekey_impostor() {
    let crypto = Standin::default();
    let mut device = Device::keyed(&crypto, 0);
    let impostor = Pair::new(&crypto).unwrap();
    let ring = &device.rings[&Identity::Aci];
    let prekey = crate::signal::message::PreKeyMessage {
        registration: 7,
        pre_key: None,
        signed_pre_key: ring.signed_keys[0].id,
        base_key: [3; 32],
        identity: impostor.public,
        message: alloc::vec![0x44],
        kyber_pre_key: ring.last_resort[0].id,
        kyber_ciphertext: alloc::vec![1],
    };
    let own = Address { service: device.account.aci(), device: 1 };
    let local = device.account.address(Identity::Aci).unwrap();
    assert_eq!(
        device.open_prekey(&crypto, Identity::Aci, &own, &local, &prekey.write()).err(),
        Some(DropReason::Session(Error::Untrusted)),
        "before any key is touched"
    );
    assert!(!device.has_session(&own), "no session begun");
}

#[test]
fn read_receipt_rules() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let alice = Address { service: ALICE, device: 1 };
    let receipts = |device: &mut Device, at: u64| {
        device.apply(&Standin::default(), Content::Text(text_with_timer(at, 0, 1)), &alice, at);
        let orders = device.mark_read(&Standin::default(), Peer::Contact(ALICE), at);
        orders.iter().filter(|order| order.recipients == [ALICE]).count()
    };
    assert_eq!(receipts(&mut device, 1), 0, "the setting is off");
    device.account.read_receipts = true;
    assert_eq!(receipts(&mut device, 2), 0, "a stranger's message request is not accepted");
    device.apply(&crypto, Content::Response { thread: Peer::Contact(ALICE), kind: ACCEPT }, &alice, 3);
    assert_eq!(receipts(&mut device, 4), 1, "accepted on the phone");
    device.note_unregistered(ALICE, 5);
    assert_eq!(receipts(&mut device, 6), 0, "found off Signal a moment ago");
    assert_eq!(receipts(&mut device, 7 + UNREGISTERED_AGE), 1, "six hours on it counts as on Signal again");
    device.set_blocked(Peer::Contact(ALICE), true);
    assert_eq!(receipts(&mut device, 8 + UNREGISTERED_AGE), 0, "blocked");
    let mut other = Device::unkeyed();
    other.account.read_receipts = true;
    other.compose(&crypto, Peer::Contact(ALICE), ("hi", &[]), 1, false);
    assert_eq!(receipts(&mut other, 2), 1, "a contact the owner wrote to is accepted, its profile shared");
}

#[test]
fn resend_archives_session() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let anna = Address { service: ALICE, device: 2 };
    session_with(&mut device, &crypto, anna, 7);
    device.log_sent(ALICE, (5_000, true), b"hello", Some(Peer::Contact(ALICE)), 5_000);
    let ratchet = device.sessions[&anna].sending_ratchet().unwrap();
    let local = device.account.device;
    let error = |ratchet, timestamp| DecryptionError { ratchet: Some(ratchet), timestamp, device: local };
    let resent = device.resend(&crypto, anna, &error(ratchet, 5_000), None, 6_000).expect("sent again");
    assert_eq!(
        (resent.content, resent.timestamp, resent.is_urgent),
        (b"hello".to_vec(), 5_000, true),
        "as it was sent"
    );
    assert!(!device.sends_to(&anna, 6_000), "its session set aside: the next message begins one anew");
    session_with(&mut device, &crypto, anna, 7);
    let ratchet = device.sessions[&anna].sending_ratchet().unwrap();
    assert!(device.resend(&crypto, anna, &error([9; 32], 5_000), None, 6_000).is_some(), "asked again, sent again");
    assert!(device.sends_to(&anna, 6_000), "under another key, nothing set aside");
    assert_eq!(
        device.resend(&crypto, anna, &error([9; 32], 4_000), None, 6_000),
        None,
        "nothing kept, nothing set aside"
    );
    let null = device.resend(&crypto, anna, &error(ratchet, 4_000), None, 6_000).expect("a null message");
    assert!(Message::parse(&null.content).unwrap().has(4), "nothing kept of that timestamp, and the session set aside");
    let too_old = 6_000 + SENT_AGE;
    assert_eq!(device.resend(&crypto, anna, &error([9; 32], 5_000), None, too_old), None, "let go after a day");
    device.log_sent(ALICE, (7_000, true), b"later", None, 7_000);
    for _ in 0..RETRY_LIMIT {
        assert!(device.resend(&crypto, anna, &error([9; 32], 7_000), None, 7_000).is_some());
    }
    assert_eq!(
        device.resend(&crypto, anna, &error([9; 32], 7_000), None, 7_000),
        None,
        "no more than Signal-Desktop's limit of times a message of a device"
    );
}

#[test]
fn resend_request_limit() {
    let mut device = Device::unkeyed();
    let anna = Address { service: ALICE, device: 2 };
    let error = |timestamp| DecryptionError { ratchet: None, timestamp, device: 2 };
    for _ in 0..RETRY_LIMIT {
        assert!(device.request_resend(anna, &error(7_000), None, 8_000).is_some());
    }
    assert_eq!(device.request_resend(anna, &error(7_000), None, 8_000 + RESEND_AGE - 1), None, "asked the most times");
    assert!(device.request_resend(anna, &error(9_000), None, 8_000 + RESEND_AGE - 1).is_some(), "another message");
    assert!(
        device.request_resend(anna, &error(7_000), None, 8_000 + RESEND_AGE).is_some(),
        "a count begun 14 days ago is let go, and the message counted anew"
    );
    assert_eq!(device.retry_counts.len(), 2, "one count a message, the old one gone");
}

#[test]
fn network_fed_lists_bounded() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let alice = Address { service: ALICE, device: 1 };
    for at in 0..=MAX_EARLY as u64 {
        let receipt = content::write_receipt(false, &[at]);
        device.receive_content(&crypto, &receipt, alice, (at, true, at), at, false).unwrap();
    }
    assert_eq!(
        (device.early_contents.len(), device.early_contents[0].timestamp),
        (MAX_EARLY, 1),
        "receipts of messages never sent are held to a bound, the oldest going first"
    );
    for timestamp in 0..=MAX_RETRY_COUNTS as u64 {
        let error = DecryptionError { ratchet: None, timestamp, device: 2 };
        device.request_resend(alice, &error, None, timestamp);
    }
    assert_eq!(
        (device.retry_counts.len(), device.retry_counts.contains_key(&(ALICE, 1, 0))),
        (MAX_RETRY_COUNTS, false),
        "and so are the counts of requests, the one begun first going first"
    );
}

#[test]
fn sender_key_lifetime() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let group = device.add_group(&crypto, &[8; 32], &[device.account.aci(), ALICE]);
    assert_eq!(device.sender_key(&crypto, &Peer::Contact(ALICE), 0), None, "a contact's conversation has none");
    let key = device.sender_key(&crypto, &group, 1_000).expect("a sender key");
    assert_eq!(
        (key.distribution[6] >> 4, key.distribution[8] >> 6, key.created, key.devices.len()),
        (4, 2, 1_000, 0),
        "its distribution a version 4 UUID, given to no device yet"
    );
    device.set_sender_devices(&group, vec![(ALICE, 1, 7), (ALICE, 2, 8)]);
    assert!(
        !device.owns_distribution(&key.distribution) && device.own_distribution(&crypto, &key.distribution).is_some()
    );
    assert!(
        device.owns_distribution(&key.distribution),
        "the R1's own record made with its first distribution message"
    );
    let existing = device.sender_key(&crypto, &group, 1_000 + SENDER_KEY_AGE - 1).expect("the same");
    assert_eq!(existing.devices, [(ALICE, 1, 7), (ALICE, 2, 8)], "kept until 90 days on");
    device.drop_sender_devices(&group, &[(ALICE, 2)]);
    assert_eq!(
        device.sender_key(&crypto, &group, 1_000).unwrap().devices,
        [(ALICE, 1, 7)],
        "a device let go by its id"
    );
    device.take_dirty();
    let new_key = device.sender_key(&crypto, &group, 1_000 + SENDER_KEY_AGE).expect("made anew");
    assert_eq!(
        (new_key.distribution, new_key.created, new_key.devices.len(), device.owns_distribution(&key.distribution)),
        (key.distribution, 1_000 + SENDER_KEY_AGE, 0, false),
        "90 days on: the distribution kept, given to no device, the R1's record let go"
    );
    assert!(device.take_dirty().contains(&File::SenderKey(device.own_address(), key.distribution)));
}

#[test]
fn group_resend_distribution() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let group = device.add_group(&crypto, &[8; 32], &[device.account.aci(), ALICE]);
    let Peer::Group(id) = group else { panic!("a group") };
    let (alice, local) = (Address { service: ALICE, device: 2 }, device.account.device);
    let error = |timestamp| DecryptionError { ratchet: None, timestamp, device: local };
    let hello = content::write_message(content::write_data("hello", 5_000, None, (0, 0), Some((&[8; 32], 1))));
    device.log_sent(ALICE, (5_000, true), &hello, Some(group), 5_000);
    let resent = device.resend(&crypto, alice, &error(5_000), Some(id), 6_000).expect("sent again");
    assert_eq!(resent.content, hello, "no sender key of the group: as it was");
    let key = device.sender_key(&crypto, &group, 6_000).unwrap();
    assert_eq!(
        device.resend(&crypto, alice, &error(5_000), Some(id), 6_000),
        None,
        "the R1's own record of the distribution gone: nothing, as Signal-Desktop refuses then"
    );
    let distribution = device.own_distribution(&crypto, &key.distribution).unwrap();
    let resent = device.resend(&crypto, alice, &error(5_000), Some(id), 6_000).expect("sent again");
    assert_eq!(
        (content::parse_distribution(&resent.content), resent.timestamp, resent.is_urgent),
        (Some(distribution.clone()), 5_000, true),
        "as it was, with the R1's distribution beside it"
    );
    let alone = device.resend(&crypto, alice, &error(4_000), Some(id), 6_000).expect("the distribution alone");
    assert_eq!(
        (alone.content, alone.timestamp, alone.is_urgent),
        (content::write_distribution(&distribution), 6_000, false),
        "nothing kept of that message: the distribution alone, now, not urgent"
    );
    let boris = Address { service: BORIS, device: 1 };
    assert_eq!(device.resend(&crypto, boris, &error(4_000), Some(id), 6_000), None, "to no member, nothing");
}

#[test]
fn refused_seal_group_token() {
    use crate::signal::group::tests::endorsement_server;
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let (server, now) = endorsement_server(&crypto);
    device.take_certificate(false, &certificate_answer(now + 10 * 60 * 60 * 1000, false), now);
    let own = device.account.aci();
    let group = device.add_group(&crypto, &[8; 32], &[own, ALICE]);
    device.note_sealed_refused(ALICE);
    assert!(device.seal_of(&crypto, &ALICE, Some(&group), now).is_none(), "no endorsements held");
    assert!(device.wants_endorsements(&ALICE, &group, now), "the group's state is asked anew for them");
    device.add_endorsed_group(&crypto, &[8; 32], &[own, ALICE], (&server, now));
    assert!(!device.wants_endorsements(&ALICE, &group, now));
    assert_eq!(
        device.seal_of(&crypto, &ALICE, Some(&group), now).map(|seal| seal.access),
        device.group_token(&crypto, &group, &[ALICE], now).map(Access::Token),
        "the group's token for her"
    );
    assert!(device.seal_of(&crypto, &ALICE, None, now).is_none(), "of no group, unsealed");
    assert!(device.seal_of(&crypto, &ALICE, Some(&Peer::Contact(ALICE)), now).is_none());
    let later = now + 23 * 60 * 60 * 1000;
    assert!(device.wants_endorsements(&ALICE, &group, later), "endorsements about to run out are asked anew");
    device.set_blocked(group, true);
    assert!(!device.wants_endorsements(&ALICE, &group, later), "a blocked group's are not");
}

#[test]
fn sent_log_expiry() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let peer = Peer::Contact(ALICE);
    device.apply(&crypto, Content::Timer(text_with_timer(1, 1, 2)), &Address { service: ALICE, device: 1 }, 0);
    device.compose(&crypto, peer, ("brief", &[]), 1_000, false);
    device.log_sent(ALICE, (1_000, true), b"brief", Some(peer), 1_000);
    device.log_sent(ALICE, (1_001, false), b"receipt", None, 1_001);
    device.note_sent(&crypto, peer, (1_000, &[]), Status::Sent, 1_000);
    assert_eq!(device.expire(2_000), [peer], "its timer of a second ran out");
    assert_eq!(
        (device.sent_content(ALICE, 1_000), device.sent_content(ALICE, 1_001).is_some()),
        (None, true),
        "the message's content let go with it, the receipt's kept"
    );
}

#[test]
fn pni_deliveries() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let pni = ServiceId::Pni([6; 16]);
    device.account.pni = Some([6; 16]);
    let envelope = |kind: u64, source: &ServiceId, destination: &ServiceId| {
        Writer::new()
            .varint(1, kind)
            .bytes(19, &source.to_binary())
            .varint(7, 1)
            .bytes(20, &destination.to_binary())
            .varint(5, 7)
            .bytes(8, b"ciphertext")
            .finish()
    };
    let aci = device.account.aci();
    let delivered = |device: &mut Device, envelope: &[u8]| device.deliver(&crypto, envelope, (0, None)).result.err();
    assert_eq!(delivered(&mut device, &envelope(1, &pni, &aci)), Some(DropReason::Pni), "from one");
    assert_eq!(delivered(&mut device, &envelope(1, &ALICE, &pni)), Some(DropReason::Pni), "to one");
    assert_eq!(
        delivered(&mut device, &envelope(1, &ALICE, &aci)),
        Some(DropReason::Malformed),
        "to the ACI it is tried, and this one is no message"
    );
}

#[test]
fn plaintext_and_unopened() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let error = DecryptionError { ratchet: Some([4; 32]), timestamp: 7, device: device.account.device };
    let plain = Writer::new()
        .varint(1, u64::from(content::PLAINTEXT))
        .bytes(19, &ALICE.to_binary())
        .varint(7, 3)
        .varint(5, 9)
        .bytes(8, &content::write_decryption_error(&error))
        .finish();
    let delivery = device.deliver(&crypto, &plain, (0, None));
    let alice = Address { service: ALICE, device: 3 };
    assert_eq!(delivery.message_id, Some((alice, 9)));
    assert_eq!(delivery.result.map(|effects| effects.resend), Ok(Some((alice, error, None))));
    let empty = Writer::new()
        .varint(1, u64::from(content::PLAINTEXT))
        .bytes(19, &ALICE.to_binary())
        .varint(7, 3)
        .bytes(8, &[0xC0, 0x80])
        .finish();
    assert_eq!(device.deliver(&crypto, &empty, (0, None)).result.err(), Some(DropReason::Plaintext));
    let ratchet = Pair::new(&crypto).unwrap();
    let message = crate::signal::message::write_message(
        &crypto,
        &crate::signal::message::Outgoing {
            ratchet_key: &ratchet.public,
            counter: 0,
            previous_counter: 0,
            ciphertext: &[1; 16],
            ratchet: &[],
            addresses: None,
        },
        &[1; 32],
        &[2; 32],
        &[3; 32],
    );
    let whisper = Writer::new()
        .varint(1, u64::from(content::DOUBLE_RATCHET))
        .bytes(19, &ALICE.to_binary())
        .varint(7, 3)
        .varint(5, 11)
        .bytes(8, &message)
        .finish();
    assert_eq!(
        device.deliver(&crypto, &whisper, (0, None)).result.err(),
        Some(DropReason::Unopened {
            sender: alice,
            timestamp: 11,
            ratchet: Some(ratchet.public),
            group: None,
            reason: Error::NoSession
        }),
        "no session: its device is owed a request under the message's own ratchet key"
    );
}

#[test]
fn key_upkeep() {
    let crypto = Standin::default();
    let mut device = Device::keyed(&crypto, 0);
    assert_eq!((device.upkeep_due(CHECK_PERIOD - 1), device.upkeep_due(CHECK_PERIOD)), (vec![], vec![Identity::Aci]));
    assert_eq!(device.renew_keys(&crypto, Identity::Aci, (10, 200), RENEW_AGE - 1), None, "nothing new");
    assert!(device.upkeep_due(RENEW_AGE - 2 + CHECK_PERIOD).is_empty(), "the check ended whole");
    let now = RENEW_AGE;
    let (body, (signed, last_resort)) = device.renew_keys(&crypto, Identity::Aci, (201, 10), now).expect("an upload");
    assert!(body.contains(r#""preKeys""#) && !body.contains(r#""pqPreKeys""#), "too many EC keys, Kyber's enough");
    assert!(signed.is_some() && last_resort.is_some(), "both a day and a half old");
    assert!(!device.keys_out_of_date(now), "the earlier keys confirmed still");
    let (_, new_ids) = device.renew_keys(&crypto, Identity::Aci, (100, 100), now).expect("the upload not taken");
    assert_ne!(new_ids.0, signed, "a key never confirmed is renewed anew");
    device.note_uploaded(Identity::Aci, new_ids, now + 1);
    assert_eq!(device.rings[&Identity::Aci].signed_keys[0].confirmed, Some(now + 1), "confirmed once taken");
    assert!(device.upkeep_due(now + 1).is_empty());
    assert!(!device.keys_out_of_date(now + MAX_CONFIRMED_AGE) && device.keys_out_of_date(now + 1 + MAX_CONFIRMED_AGE));
    let late = 31 * 24 * 60 * 60 * 1000;
    let ring = device.rings.get_mut(&Identity::Aci).unwrap();
    let template = ring.last_resort[0].clone();
    ring.last_resort = (0..6)
        .map(|at| KyberKey { id: 100 + at, created: if at < 5 { late } else { 0 }, ..template.clone() })
        .collect();
    ring.uses = vec![(105, 1, [0; 32]), (100, 1, [1; 32])];
    device.note_uploaded(Identity::Aci, (None, None), late);
    assert_eq!(device.rings[&Identity::Aci].uses, [(100, 1, [1; 32])], "a last-resort key let go takes its uses");
}

#[test]
fn sender_key_open() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let sender = Address { service: ALICE, device: 1 };
    let signing = Pair::new(&crypto).unwrap();
    let mut key = ChainKey { key: [1; 32], index: 0 };
    let bytes = distribution([6; 16], 1, &key, &signing);
    let message = seal(&crypto, ([6; 16], 1), &mut key, &signing, b"padded");
    assert_eq!(device.open_by_sender_key(&crypto, &sender, &message), Err(Error::NoSession));
    assert!(
        device.take_distribution(&sender, &bytes) && device.take_dirty().contains(&File::SenderKey(sender, [6; 16]))
    );
    assert_eq!(device.open_by_sender_key(&crypto, &sender, &message).as_deref(), Ok(&b"padded"[..]));
    assert_eq!(
        device.open_by_sender_key(&crypto, &Address { device: 2, ..sender }, &message),
        Err(Error::NoSession),
        "a distribution is the sending device's alone"
    );
}

fn content_with_action(timestamp: u64, action: (u32, Writer), revision: Option<u32>) -> Vec<u8> {
    let data = Writer::new().message(action.0, action.1).varint(7, timestamp);
    content::write_message(
        data.maybe(revision.map(|revision| move |w: Writer| w.message(15, Context::write(&[8; 32], revision)))),
    )
}

#[test]
fn delete_for_everyone() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let alice = Address { service: ALICE, device: 1 };
    let delete = |target: u64, timestamp| content_with_action(timestamp, (17, Writer::new().varint(1, target)), None);
    let reception = device.receive_content(&crypto, &delete(1_000, 2_000), alice, (2_000, true, 2_000), 2_000, false);
    assert!(
        matches!(reception, Ok(Reception::Quiet)) && device.early_contents.len() == 1,
        "held for the message it names"
    );
    let copy = Device::read(
        &[
            (File::Account, device.file_bytes(&File::Account).unwrap()),
            (File::Seen, device.file_bytes(&File::Seen).unwrap()),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    assert_eq!(copy.early_contents, device.early_contents, "and kept with the deliveries seen");
    let reception =
        device.receive_content(&crypto, &content_of(1_000, "Oops", None), alice, (1_000, true, 1_000), 2_100, false);
    assert!(
        matches!(reception, Ok(Reception::Event(Event::Changed(_)))) && device.early_contents.is_empty(),
        "taken in deleted, told as a change and not as a message come"
    );
    let peer = Peer::Contact(ALICE);
    assert_eq!((device.entries(&peer)[0].deleter, device.entries(&peer)[0].body.as_str()), (Some(Deleter::Author), ""));
    device.receive_content(&crypto, &content_of(5_000, "Hi", None), alice, (5_000, true, 5_000), 5_000, false).unwrap();
    let late = 5_000 + DELETE_WINDOW + DELETE_GRACE + 1;
    device.receive_content(&crypto, &delete(5_000, late), alice, (late, true, late), late, false).unwrap();
    assert_eq!(device.entries(&peer)[1].deleter, None, "two days after its message, a deletion is refused");
    let own = Address { service: device.account.aci(), device: 1 };
    let note = Peer::Contact(own.service);
    device.compose(&crypto, note, ("milk", &[]), 6_000, false);
    let stamp = device.entries(&note)[0].timestamp;
    let deletion = Content::SentDelete { to: Some(own.service), target: stamp, group: None };
    assert_eq!(
        device.apply(&crypto, deletion, &own, 6_100),
        None,
        "nothing is deleted for everyone in the Note to Self"
    );
}

#[test]
fn admin_delete() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let own = device.account.aci();
    let group = Peer::Group(group::id_of(&crypto, &[8; 32]));
    let state =
        group::State { revision: 2, members: vec![own, ALICE, BORIS], admins: vec![BORIS], ..group::State::default() };
    device.note_group_revision(&crypto, &[8; 32], 2);
    device.apply_group_updates(&crypto, vec![(group, GroupUpdate::State(state))]);
    let (alice, boris) = (Address { service: ALICE, device: 1 }, Address { service: BORIS, device: 1 });
    device
        .receive_content(&crypto, &content_of(1_000, "Up?", Some(2)), alice, (1_000, true, 1_000), 1_000, false)
        .unwrap();
    device
        .receive_content(&crypto, &content_of(1_100, "Me", Some(2)), boris, (1_100, true, 1_100), 1_100, false)
        .unwrap();
    let admin = |author: &ServiceId, target: u64, timestamp| {
        content_with_action(timestamp, (29, Writer::new().bytes(1, &author.to_binary()).varint(2, target)), Some(2))
    };
    device.receive_content(&crypto, &admin(&BORIS, 1_100, 1_200), alice, (1_200, true, 1_200), 1_200, false).unwrap();
    assert_eq!(
        device.entries(&group)[1].deleter,
        None,
        "a member who is no administrator deletes nothing of another's"
    );
    device.receive_content(&crypto, &admin(&ALICE, 1_000, 1_300), boris, (1_300, true, 1_300), 1_300, false).unwrap();
    assert_eq!(device.entries(&group)[0].deleter, Some(Deleter::Admin), "the administrator does");
}

#[test]
fn owner_syncs() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let own = Address { service: device.account.aci(), device: 1 };
    let peer = Peer::Contact(ALICE);
    device.take_dirty();
    let shared = || Content::SentProfileKey { to: Some(ALICE), group: None, key: [9; 32] };
    assert_eq!(device.apply(&crypto, shared(), &own, 1_000), None);
    assert!(
        device.is_sharing(&peer)
            && device.account.profile_key == [9; 32]
            && device.take_dirty().contains(&File::Account),
        "the owner shares the profile there, under the newest key, which is kept"
    );
    device.apply(&crypto, shared(), &own, 1_100);
    assert!(!device.take_dirty().contains(&File::Account), "the key held is not kept anew");
    device.compose(&crypto, peer, ("Lunch?", &[]), 2_000, false);
    let stamp = device.entries(&peer)[0].timestamp;
    let edit = |target| Content::SentEdit {
        to: Some(ALICE),
        target,
        text: Text { timestamp: 2_500, body: "Lunch at one?".into(), ..Text::default() },
    };
    assert!(
        device.apply_content(&crypto, edit(stamp + 1), &own, (2_500, 2_500)).is_err(),
        "an edit of a message not held waits for it"
    );
    assert_eq!(device.apply(&crypto, edit(stamp), &own, 2_600), Some(Event::Changed(peer)));
    let entry = &device.entries(&peer)[0];
    assert_eq!((entry.body.as_str(), entry.is_edited), ("Lunch at one?", true), "the owner's edit from the phone");
    assert!(device.take_dropped(), "what the words before carried may be held no more");
    let delete = |target| Content::SentDelete { to: Some(ALICE), target, group: None };
    let late = stamp + DELETE_WINDOW + DELETE_GRACE + 1;
    assert_eq!(device.apply(&crypto, delete(stamp), &own, late), None, "two days after its message, refused");
    assert_eq!(device.apply(&crypto, delete(stamp), &own, 3_000), Some(Event::Changed(peer)));
    assert_eq!(device.entries(&peer)[0].deleter, Some(Deleter::Author), "the owner's deletion from the phone");
    let group = device.note_group_revision(&crypto, &[8; 32], 2);
    let state = |admins: &[ServiceId]| group::State {
        revision: 2,
        members: vec![own.service, ALICE],
        admins: admins.to_vec(),
        ..group::State::default()
    };
    device.apply_group_updates(&crypto, vec![(group, GroupUpdate::State(state(&[])))]);
    let said = Text { timestamp: 4_000, body: "Up?".into(), group: Some(climbing(2)), ..Text::default() };
    device.apply(&crypto, Content::Text(said), &Address { service: ALICE, device: 1 }, 4_000);
    let admin_delete = || Content::SentAdminDelete { author: ALICE, target: 4_000, group: Some(climbing(2)) };
    assert_eq!(device.apply(&crypto, admin_delete(), &own, 4_100), None, "the owner is no administrator");
    device.apply_group_updates(&crypto, vec![(group, GroupUpdate::State(state(&[own.service])))]);
    assert_eq!(device.apply(&crypto, admin_delete(), &own, 4_200), Some(Event::Changed(group)));
    assert_eq!(
        device.entries(&group)[0].deleter,
        Some(Deleter::Admin),
        "as its administrator the owner deletes a member's message from the phone"
    );
}

#[test]
fn group_change_and_session_end() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let group = device.add_group(&crypto, &[8; 32], &[device.account.aci(), ALICE]);
    let phone = Address { service: ALICE, device: 2 };
    assert_eq!(device.apply(&crypto, Content::GroupChange(climbing(5)), &phone, 1_000), Some(Event::Changed(group)));
    assert_eq!(
        device.group_due(&group),
        (Some(5), false),
        "a change of the group names a later revision, which is due"
    );
    session_with(&mut device, &crypto, phone, 7);
    assert!(device.sends_to(&phone, 2_000));
    assert_eq!(device.apply(&crypto, Content::EndSession, &phone, 2_000), None);
    assert!(!device.sends_to(&phone, 2_000), "a session its device ended is set aside");
}

#[test]
fn early_edit() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let alice = Address { service: ALICE, device: 1 };
    let edit = Writer::new()
        .message(
            11,
            Writer::new().varint(1, 1_000).message(2, content::write_data("Lunch at one?", 1_500, None, (0, 1), None)),
        )
        .finish();
    device.receive_content(&crypto, &edit, alice, (1_500, true, 1_500), 1_500, false).unwrap();
    assert_eq!(device.early_contents.len(), 1);
    device
        .receive_content(&crypto, &content_of(1_000, "Lunch?", None), alice, (1_000, true, 1_000), 1_600, false)
        .unwrap();
    let entry = &device.entries(&Peer::Contact(ALICE))[0];
    assert_eq!((entry.body.as_str(), entry.is_edited, device.early_contents.len()), ("Lunch at one?", true, 0));
}

#[test]
fn reactions() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let own = Address { service: device.account.aci(), device: 1 };
    let (alice, boris) = (Address { service: ALICE, device: 1 }, Address { service: BORIS, device: 1 });
    let peer = Peer::Contact(ALICE);
    let reaction = |author: &ServiceId, target: u64, emoji: Option<&str>| {
        let named = Writer::new().bytes(6, &author.to_binary()).varint(5, target);
        match emoji {
            Some(emoji) => named.text(1, emoji),
            None => named.text(1, "\u{1F44D}").bool(2, true),
        }
    };
    let react = |device: &mut Device, from: Address, given: Writer, at: u64, revision: Option<u32>| {
        let content = content_with_action(at, (16, given), revision);
        device.receive_content(&crypto, &content, from, (at, true, at), at, false)
    };
    let counts = |device: &Device, peer: &Peer, at: usize| -> Vec<(String, usize)> {
        let counts = device.entries(peer)[at].reaction_counts();
        counts.into_iter().map(|(emoji, count)| (String::from(emoji), count)).collect()
    };
    let thumb = || reaction(&ALICE, 1_000, Some("\u{1F44D}"));
    assert!(
        matches!(react(&mut device, alice, thumb(), 1_500, None), Ok(Reception::Quiet))
            && device.early_contents.len() == 1,
        "held for the message it names"
    );
    device
        .receive_content(&crypto, &content_of(1_000, "Lunch?", None), alice, (1_000, true, 1_000), 1_600, false)
        .unwrap();
    assert_eq!(
        (counts(&device, &peer, 0), device.early_contents.len()),
        (vec![("\u{1F44D}".into(), 1)], 0),
        "taken as its message comes"
    );
    let sent = |emoji: &str, at: u64| {
        let data = Writer::new().message(16, reaction(&ALICE, 1_000, Some(emoji))).varint(7, at);
        content::write_transcript(Some(&ALICE), &[], (at, at), data, &[])
    };
    let event = device.receive_content(&crypto, &sent("\u{1F44D}", 1_700), own, (1_700, true, 1_700), 1_700, false);
    assert!(matches!(event, Ok(Reception::Event(Event::Changed(changed))) if changed == peer));
    assert_eq!(counts(&device, &peer, 0), [("\u{1F44D}".into(), 2)], "the owner's from the phone beside the contact's");
    react(&mut device, alice, reaction(&ALICE, 1_000, Some("\u{2764}\u{FE0F}")), 1_800, None).unwrap();
    assert_eq!(
        counts(&device, &peer, 0),
        [("\u{1F44D}".into(), 1), ("\u{2764}\u{FE0F}".into(), 1)],
        "another of the same giver takes the place of the one before"
    );
    device.receive_content(&crypto, &sent("\u{1F389}", 1_650), own, (1_650, true, 1_650), 1_900, false).unwrap();
    assert_eq!(counts(&device, &peer, 0).len(), 2, "one of the owner's earlier than the one held changes nothing");
    react(&mut device, alice, reaction(&ALICE, 1_000, None), 2_000, None).unwrap();
    assert_eq!(counts(&device, &peer, 0), [("\u{1F44D}".into(), 1)], "taken back");
    let copy = Device::read(
        &[
            (File::Account, device.file_bytes(&File::Account).unwrap()),
            (File::Conversation(peer), device.file_bytes(&File::Conversation(peer)).unwrap()),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    assert_eq!(copy.entries(&peer), device.entries(&peer), "kept with its message");
    assert!(
        matches!(react(&mut device, boris, thumb(), 2_100, None), Ok(Reception::Quiet))
            && device.early_contents.is_empty(),
        "one that names another's message than the conversation's own is dropped, and waits for nothing"
    );
    for (given, what) in [(Some("ok"), "words"), (Some("\u{1F44D}\u{1F44D}"), "two emoji"), (Some(""), "nothing")] {
        react(&mut device, alice, reaction(&ALICE, 1_000, given), 2_200, None).unwrap();
        assert_eq!(counts(&device, &peer, 0).len(), 1, "{what} is no reaction");
    }
    let delete = content_with_action(2_300, (17, Writer::new().varint(1, 1_000)), None);
    device.receive_content(&crypto, &delete, alice, (2_300, true, 2_300), 2_300, false).unwrap();
    react(&mut device, alice, thumb(), 2_400, None).unwrap();
    assert!(
        device.entries(&peer)[0].reactions.is_empty() && device.early_contents.is_empty(),
        "a message deleted for everyone loses its reactions and takes none"
    );
    let group = device.add_group(&crypto, &[8; 32], &[own.service, ALICE]);
    device
        .receive_content(&crypto, &content_of(3_000, "Up?", Some(0)), alice, (3_000, true, 3_000), 3_000, false)
        .unwrap();
    react(&mut device, boris, reaction(&ALICE, 3_000, Some("\u{1F44D}")), 3_100, Some(0)).unwrap();
    assert!(device.entries(&group)[0].reactions.is_empty(), "from no member of the group");
    react(&mut device, alice, reaction(&ALICE, 3_000, Some("\u{1F44D}")), 3_200, Some(0)).unwrap();
    assert_eq!(counts(&device, &group, 0), [("\u{1F44D}".into(), 1)], "a member's to a member's message");
}

#[test]
fn receipt_status() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    device.account.read_receipts = true;
    let peer = Peer::Contact(ALICE);
    device.compose(&crypto, peer, ("Lunch?", &[]), 1_000, false);
    let stamp = device.entries(&peer)[0].timestamp;
    let (alice, boris) = (Address { service: ALICE, device: 1 }, Address { service: BORIS, device: 1 });
    let delivery = content::write_receipt(false, &[stamp]);
    device.receive_content(&crypto, &delivery, alice, (2_000, true, 2_000), 2_000, false).unwrap();
    assert_eq!(
        (device.entries(&peer)[0].status, device.early_contents.len()),
        (Status::Sending, 1),
        "not sent yet: held"
    );
    device.note_sent(&crypto, peer, (stamp, &[]), Status::Sent, 2_100);
    assert_eq!(
        (device.entries(&peer)[0].status, device.early_contents.len()),
        (Status::Delivered, 0),
        "sent, it is taken"
    );
    let read = content::write_receipt(true, &[stamp]);
    device.receive_content(&crypto, &read, boris, (2_200, true, 2_200), 2_200, false).unwrap();
    assert_eq!(device.entries(&peer)[0].status, Status::Delivered, "Boris received nothing of it");
    device.receive_content(&crypto, &read, alice, (2_300, true, 2_300), 2_300, false).unwrap();
    assert_eq!(device.entries(&peer)[0].status, Status::Read);
    device.receive_content(&crypto, &delivery, alice, (2_400, true, 2_400), 2_400, false).unwrap();
    assert_eq!(device.entries(&peer)[0].status, Status::Read, "never back");
    device.account.read_receipts = false;
    device.compose(&crypto, peer, ("And?", &[]), 3_000, false);
    let next = device.entries(&peer)[1].timestamp;
    device.note_sent(&crypto, peer, (next, &[]), Status::Sent, 3_100);
    device.early_contents.clear();
    device
        .receive_content(&crypto, &content::write_receipt(true, &[next]), alice, (3_200, true, 3_200), 3_200, false)
        .unwrap();
    assert_eq!(
        (device.entries(&peer)[1].status, device.early_contents.len()),
        (Status::Sent, 0),
        "a read receipt counts only where the owner sends read receipts"
    );
}

#[test]
fn muted_notices() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let (alice, own) = (Address { service: ALICE, device: 1 }, device.account.aci());
    let peer = Peer::Contact(ALICE);
    device.apply_list_state(peer, ListState { muted_until: 10_000, ..ListState::default() });
    let noticed = |device: &mut Device, text: Text, now| match device.apply(&crypto, Content::Text(text), &alice, now) {
        Some(Event::Arrived { is_noticed, .. }) => is_noticed,
        _ => panic!("a message come"),
    };
    assert!(!noticed(&mut device, text_with_timer(1, 0, 1), 5_000), "muted");
    let mention = content::Mention { start: 0, length: 1, aci: own };
    let text = Text { mentions: vec![mention.clone()], ..text_with_timer(2, 0, 1) };
    assert!(noticed(&mut device, text, 5_000), "a mention of the owner is told, as a new device has it");
    let reply = Text {
        quote: Some(content::Quote { timestamp: 9, author: own, text: "?".into() }),
        ..text_with_timer(3, 0, 1)
    };
    device.account.listing.notify = (true, false, false);
    assert!(!noticed(&mut device, reply, 5_000), "a reply is not where the owner says so");
    device.account.listing.notify = (false, true, false);
    assert!(!noticed(&mut device, Text { mentions: vec![mention], ..text_with_timer(4, 0, 1) }, 5_000));
    assert!(noticed(&mut device, text_with_timer(5, 0, 1), 10_000), "and all is once the muting ran out");
}

#[test]
fn list_state_merge() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let peer = Peer::Contact(ALICE);
    let record = |list_state| ContactRecord { whitelisted: true, list_state, ..contact_record(ALICE) };
    let list_state = ListState { archived: true, marked_unread: true, muted_until: 9_000 };
    device.merge_contact(record(list_state), true, 0);
    assert_eq!(
        device.list_status(&peer, 0),
        ListStatus { pinned: None, is_archived: true, is_marked: true, is_muted: true },
        "archived, marked unread and muted"
    );
    device.mark_read(&crypto, peer, 1);
    device.compose(&crypto, peer, ("hi", &[]), 2, false);
    assert_eq!(
        device.list_status(&peer, 0),
        ListStatus { is_muted: true, ..ListStatus::default() },
        "opened and written to on the R1"
    );
    device.merge_contact(record(list_state), false, 3);
    assert!(!device.list_status(&peer, 3).is_archived, "the phone's record that said so before changes nothing");
    device.merge_contact(record(ListState::default()), false, 4);
    device.merge_contact(record(ListState { archived: true, ..list_state }), false, 5);
    assert!(device.list_status(&peer, 5).is_archived, "archived anew on the phone");
    device.account.listing.keep_muted_archived = true;
    device.compose(&crypto, peer, ("still", &[]), 6, false);
    assert!(device.list_status(&peer, 6).is_archived, "muted, it stays archived where the owner keeps muted ones so");
    let account = AccountRecord {
        profile_key: None,
        read_receipts: false,
        is_number_shared: false,
        note: ListState { marked_unread: true, ..ListState::default() },
        pins: vec![Pin::Group([8; 32]), Pin::Contact(ALICE), Pin::Number("+15550000000".into())],
        keep_muted_archived: false,
        count_muted: true,
        notify: (false, true, true),
    };
    device.merge_account(&crypto, account);
    let group = Peer::Group(group::id_of(&crypto, &[8; 32]));
    assert_eq!(device.account.listing.pinned, [group, peer], "in their order, a number nobody has passed over");
    assert!(
        !device.list_status(&peer, 7).is_archived && device.list_status(&group, 7).pinned == Some(0),
        "pinned, not archived"
    );
    let own = Peer::Contact(device.account.aci());
    assert!(device.list_status(&own, 7).is_marked && device.account.listing.count_muted);
    let copy =
        Device::read(&[(File::Account, device.file_bytes(&File::Account).unwrap())].into_iter().collect()).unwrap();
    assert_eq!(copy.account.listing, device.account.listing, "how the list goes is kept with the account");
}

#[test]
fn conversation_kept_on_change() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let (alice, own) = (Address { service: ALICE, device: 1 }, Address { service: device.account.aci(), device: 1 });
    let peer = Peer::Contact(ALICE);
    let group = device.add_group(&crypto, &[8; 32], &[own.service, ALICE]);
    device.apply(&crypto, Content::Text(text_with_timer(1, 0, 1)), &alice, 100);
    device.compose(&crypto, peer, ("hi", &[]), 1_000, false);
    let stamp = device.entries(&peer)[1].timestamp;
    device.note_sent(&crypto, peer, (stamp, &[]), Status::Sent, 1_100);
    let kept = |device: &mut Device| -> BTreeSet<File> {
        device.take_dirty().into_iter().filter(|file| matches!(file, File::Conversation(_))).collect()
    };
    let both = BTreeSet::from([File::Conversation(peer), File::Conversation(group)]);
    let alone = BTreeSet::from([File::Conversation(peer)]);
    let read_records = |device: &mut Device| {
        let archived = ListState { archived: true, muted_until: 9_000, ..ListState::default() };
        device.merge_contact(
            ContactRecord { whitelisted: true, list_state: archived, ..contact_record(ALICE) },
            false,
            2,
        );
        device.merge_group(
            &crypto,
            GroupRecord {
                master_key: [8; 32],
                blocked: false,
                whitelisted: true,
                list_state: archived,
                notify: (Some(true), None, None),
            },
        );
        let account = AccountRecord {
            profile_key: None,
            read_receipts: true,
            is_number_shared: false,
            note: ListState::default(),
            pins: vec![Pin::Group([8; 32]), Pin::Contact(ALICE)],
            keep_muted_archived: false,
            count_muted: false,
            notify: (true, true, false),
        };
        device.merge_account(&crypto, account);
    };
    kept(&mut device);
    read_records(&mut device);
    assert_eq!(kept(&mut device), both, "what the records changed is kept");
    read_records(&mut device);
    assert!(kept(&mut device).is_empty(), "records that say what is held have no conversation written again");
    let receipt = content::write_receipt(false, &[stamp]);
    device.receive_content(&crypto, &receipt, alice, (2_000, true, 2_000), 2_000, false).unwrap();
    assert_eq!(kept(&mut device), alone, "a receipt that raises a message's status");
    device.receive_content(&crypto, &receipt, alice, (2_100, true, 2_100), 2_100, false).unwrap();
    assert!(kept(&mut device).is_empty(), "the same from another of her devices raises nothing");
    let read = || Content::ReadElsewhere { reads: vec![(ALICE, 1)], at: 2_200 };
    assert_eq!(device.apply(&crypto, read(), &own, 2_300), Some(Event::Changed(peer)));
    assert_eq!((kept(&mut device), device.apply(&crypto, read(), &own, 2_400)), (alone.clone(), None));
    assert!(kept(&mut device).is_empty(), "read there once");
    let edit = Content::Edit { target: 77, text: text_with_timer(2_500, 0, 1) };
    assert!(device.apply_content(&crypto, edit, &alice, (2_500, 2_500)).is_err(), "an edit of a message not held");
    let unknown = vec![DeleteForMe::Messages {
        thread: Identifier::Service(ALICE),
        messages: vec![(Identifier::Service(ALICE), 77)],
    }];
    assert!(device.apply_content(&crypto, Content::DeleteForMe(unknown), &own, (2_600, 2_600)).is_err());
    assert!(kept(&mut device).is_empty(), "what waits for its message changes no conversation");
    device.mark_read(&crypto, peer, 2_700);
    assert!(kept(&mut device).is_empty(), "nor does opening one that holds nothing unread");
}

#[test]
fn mention_names() {
    let mut device = Device::unkeyed();
    device.add_contact(Contact { name: Some("Boris".into()), ..Contact::new(BORIS) });
    device.add_contact(Contact { username: Some("anna.01".into()), ..Contact::new(ALICE) });
    let mention = |start, aci| content::Mention { start, length: 1, aci };
    let carol = ServiceId::Aci([6; 16]);
    let text =
        Text { mentions: vec![mention(4, BORIS), mention(0, ALICE), mention(6, carol)], ..text_with_timer(1, 0, 1) };
    let entry = Entry::incoming(&Text { body: "\u{FFFC} \u{1F600}\u{FFFC} \u{FFFC}!".into(), ..text }, 1, None);
    assert_eq!(
        device.resolve_mentions(&entry).body,
        "@anna.01 \u{1F600}@Boris @Unknown!",
        "each in its place, counted in UTF-16 as the emoji's two units"
    );
    device.add_contact(Contact { number: Some("+15550000000".into()), ..device.contacts[&ALICE].clone() });
    assert_eq!(
        device.name_of(&Peer::Contact(ALICE)).as_deref(),
        Some("+15550000000"),
        "the number before the username"
    );
}

#[test]
fn account_keys_sync() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let own = Address { service: device.account.aci(), device: 1 };
    device.apply(&crypto, Content::Keys { master: Some([0; 32]), pool: None }, &own, 1);
    assert!(!device.is_storage_due, "the key held: nothing");
    device.apply(&crypto, Content::Keys { master: Some([7; 32]), pool: None }, &own, 1);
    assert!(device.account.master_key == [7; 32] && core::mem::take(&mut device.is_storage_due));
    device.apply(&crypto, Content::Keys { master: None, pool: Some("pool".into()) }, &own, 1);
    assert_eq!(
        (Some(device.account.master_key), device.is_storage_due),
        (crate::signal::account::derive_master_key(&crypto, "pool"), true),
        "from the account entropy pool"
    );
}

#[test]
fn pni_change() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let old = ServiceId::Pni([4; 16]);
    (device.account.pni, device.account.pni_identity) =
        (Some([4; 16]), Some(Pair { private: [1; 32], public: [2; 32] }));
    device.add_own_identities();
    device.take_certificate(true, &certificate_answer(10 * 60 * 60 * 1000, true), 0);
    let identity = Pair::new(&crypto).unwrap();
    let change = PniChange {
        identity,
        signed_pre_key: (7, Pair::new(&crypto).unwrap(), [4; 64], 100),
        last_resort: Some((8, vec![6; 4], vec![5; 4], [7; 64], 100)),
        registration: 77,
        number: "+15550000001".into(),
    };
    device.take_pni_change(&crypto, change.clone(), None, 1_000);
    assert_eq!(device.account.pni, Some([4; 16]), "without the new identity's id nothing changes");
    let now = CHECK_PERIOD;
    device.take_pni_change(&crypto, change, Some([5; 16]), now);
    let account = &device.account;
    assert_eq!(
        (account.pni, account.pni_identity, account.pni_registration, account.number.as_deref()),
        (Some([5; 16]), Some(identity), 77, Some("+15550000001"))
    );
    let ring = &device.rings[&Identity::Pni];
    assert_eq!(
        (ring.signed_keys[0].id, ring.last_resort[0].id, ring.signed_keys[0].confirmed, ring.one_time.len()),
        (7, 8, Some(now), 0),
        "the keys the phone gave the service, taken"
    );
    assert_eq!(device.upkeep_due(now), [Identity::Pni], "the new identity's never checked: at once");
    assert!(
        !device.identities.contains_key(&old) && device.identities[&ServiceId::Pni([5; 16])].key == identity.public
    );
    assert!(device.certificate(true, 0).is_none() && device.is_credential_stale, "what named the old number let go");
}

#[test]
fn pni_signature_merge() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let (pni, number) = (ServiceId::Pni([6; 16]), Pair::new(&crypto).unwrap());
    device.save_identity(pni, number.public, None, 0);
    device.apply(&crypto, Content::Text(text_with_timer(1, 0, 1)), &Address { service: pni, device: 1 }, 10);
    let signed_parts = [&ALTERNATE_FIRST[..], ALTERNATE_SECOND, &encode_key(&[5; 32])];
    let signature = curve::sign(&crypto, &number.private, &signed_parts, &[0; 64]);
    assert!(
        !device.take_pni_signature(&crypto, ALICE, [6; 16], &signature),
        "without the contact's own key held, nothing"
    );
    device.save_identity(ALICE, [5; 32], None, 0);
    assert!(
        !device.take_pni_signature(&crypto, ALICE, [6; 16], &[0; 64]),
        "nor by a signature the number's key did not make"
    );
    assert!(device.take_pni_signature(&crypto, ALICE, [6; 16], &signature));
    assert_eq!(
        (device.entries(&Peer::Contact(ALICE)).len(), device.conversation(&Peer::Contact(pni)).is_none()),
        (1, true),
        "the number's conversation is the contact's"
    );
    assert_eq!(device.contacts[&ALICE].pni, Some(pni));
    assert!(device.take_dirty().contains(&File::Conversation(Peer::Contact(pni))), "its file to go");
    assert!(!device.take_pni_signature(&crypto, ALICE, [6; 16], &signature), "told again, nothing more");
}

#[test]
fn delete_for_me() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    let (alice, own) = (Address { service: ALICE, device: 1 }, Address { service: device.account.aci(), device: 1 });
    let peer = Peer::Contact(ALICE);
    for timestamp in [1, 2, 3, 4] {
        device.apply(&crypto, Content::Text(text_with_timer(timestamp, 0, 1)), &alice, timestamp * 100);
    }
    let message_id = |timestamp| (Identifier::Service(ALICE), timestamp);
    let messages = |messages| vec![DeleteForMe::Messages { thread: Identifier::Service(ALICE), messages }];
    let sync = content::write_delete_for_me(&messages(vec![message_id(1), message_id(9)]));
    device.receive_content(&crypto, &sync, own, (500, true, 500), 500, false).unwrap();
    assert_eq!(
        (device.entries(&peer).len(), device.early_contents.len()),
        (3, 1),
        "the one held goes, the one to come waits"
    );
    assert_eq!(
        content::parse_content(&device.early_contents[0].content, 500, true),
        Some(Content::DeleteForMe(messages(vec![message_id(9)]))),
        "held for it alone"
    );
    device.receive_content(&crypto, &content_of(9, "Later", None), alice, (9, true, 600), 600, false).unwrap();
    assert_eq!((device.entries(&peer).len(), device.early_contents.len()), (3, 0), "and goes as it comes");
    let partial = Content::DeleteForMe(vec![DeleteForMe::Conversation {
        thread: Identifier::Service(ALICE),
        newest: vec![message_id(7)],
        non_expiring: vec![message_id(3)],
        is_full: false,
    }]);
    device.apply(&crypto, partial, &own, 700);
    assert_eq!(
        device.entries(&peer).iter().map(|entry| entry.timestamp).collect::<Vec<_>>(),
        [4],
        "cleared up to the newest of those that do not disappear, none of the first held"
    );
    device.conversation_mut(peer).is_safety_changed = true;
    let full = Content::DeleteForMe(vec![DeleteForMe::Conversation {
        thread: Identifier::Service(ALICE),
        newest: vec![message_id(4)],
        non_expiring: Vec::new(),
        is_full: true,
    }]);
    assert_eq!(device.apply(&crypto, full, &own, 800), Some(Event::Changed(peer)));
    let conversation = device.conversation(&peer).unwrap();
    assert!(conversation.entries.is_empty() && !conversation.is_safety_changed, "deleted whole: nothing of it shows");
    assert!(device.take_dropped(), "messages went: the files of their attachments are swept");
    let unknown = content::write_delete_for_me(&messages(vec![message_id(77)]));
    device.receive_content(&crypto, &unknown, own, (900, true, 900), 900, false).unwrap();
    device.receive_content(&crypto, &content_of(20, "Hi", None), alice, (20, true, 1_000), 1_000, false).unwrap();
    assert_eq!(
        (device.early_contents.len(), device.take_dropped()),
        (1, false),
        "a deletion held for a message that has not come takes nothing away as it is tried again, and nothing is swept"
    );
}

#[test]
fn expiry_wait() {
    let crypto = Standin::default();
    let mut device = Device::unkeyed();
    assert_eq!(device.next_expiry(0), None, "nothing runs");
    device.apply(&crypto, Content::Text(text_with_timer(1, 30, 2)), &Address { service: ALICE, device: 1 }, 1_000);
    device.mark_read(&crypto, Peer::Contact(ALICE), 2_000);
    assert_eq!(device.next_expiry(10_000), Some(22_000), "thirty seconds after it was read");
    assert_eq!(device.next_expiry(40_000), Some(0), "and at once where its time has come");
}

#[test]
fn contactable_contacts() {
    let mut device = Device::unkeyed();
    let (own, carol, dana, eve) =
        (device.account.aci(), ServiceId::Aci([6; 16]), ServiceId::Aci([7; 16]), ServiceId::Aci([8; 16]));
    for (aci, name) in [(BORIS, "boris"), (ALICE, "Alice"), (dana, "Dana"), (eve, "Eve"), (own, "Me")] {
        device.add_contact(Contact { name: Some(name.into()), ..Contact::new(aci) });
    }
    device.add_contact(Contact { name: Some("Carol".into()), is_hidden: true, ..Contact::new(carol) });
    device.add_contact(Contact::new(ServiceId::Aci([9; 16])));
    device.set_blocked(Peer::Contact(dana), true);
    device.note_unregistered(eve, 1_000);
    let names = |now| device.contactable(now).into_iter().map(|(_, name)| name).collect::<Vec<_>>();
    assert_eq!(
        names(2_000),
        ["Alice", "boris", "Note to Self"],
        "by name whatever its letters' case; the owner's own conversation is Note to Self, and a hidden contact, a blocked one, one found off Signal and one with no name are none"
    );
    assert_eq!(
        names(1_000 + UNREGISTERED_AGE),
        ["Alice", "boris", "Eve", "Note to Self"],
        "six hours on, on Signal again"
    );
}

#[test]
fn attachments_folder() {
    use crate::signal::attachment::ATTACHMENTS;
    assert!(FOLDERS.contains(&ATTACHMENTS) && ATTACHMENTS.starts_with(DIRECTORY));
    assert_eq!(File::from_path(ATTACHMENTS, "0a.jpg"), None, "not a file the device keeps");
}

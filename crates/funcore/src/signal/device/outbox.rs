use super::{Address, Device, File, Peer, ServiceId};
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::Crypto;
use crate::signal::attachment::{Attachment, Enclosure, KEYS};
use crate::signal::command::{Outbound, Outgoing};
use crate::signal::content::{self, DecryptionError};
use crate::signal::conversation::{Entry, OWN, Status};
use crate::signal::sealed;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

pub(super) const SENT_AGE: u64 = 24 * 60 * 60 * 1000;
pub(super) const RESEND_AGE: u64 = 14 * 24 * 60 * 60 * 1000;
pub(super) const RETRY_LIMIT: u32 = 5;
// As many counts are kept as deliveries are remembered, the one begun first going first.
pub(super) const MAX_RETRY_COUNTS: usize = super::delivery::MAX_GUIDS;
const MAX_RECEIPT_TIMESTAMPS: usize = 100;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SentRecord {
    pub(super) timestamp: u64,
    pub(super) recipient: ServiceId,
    pub(super) content: Vec<u8>,
    pub(super) is_urgent: bool,
    peer: Option<Peer>,
}

impl SentRecord {
    pub(super) fn write(&self) -> Writer {
        Writer::new()
            .varint(1, self.timestamp)
            .bytes(2, &self.recipient.to_binary())
            .bytes(3, &self.content)
            .bool(4, self.is_urgent)
            .maybe(self.peer.map(|peer| move |w: Writer| w.bytes(5, &peer.to_bytes())))
    }

    pub(super) fn read(message: &Message) -> Option<SentRecord> {
        let peer = match message.bytes(5) {
            Some(bytes) => Some(Peer::from_bytes(bytes)?),
            None => None,
        };
        Some(SentRecord {
            timestamp: message.varint(1)?,
            recipient: ServiceId::from_binary(message.bytes(2)?)?,
            content: message.bytes(3)?.to_vec(),
            is_urgent: message.bool(4).unwrap_or(true),
            peer,
        })
    }
}

// What is to be sent and to whom, kept whole where its sending begins anew and where its rest goes on its own.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Order {
    pub(in crate::signal) peer: Peer,
    pub(in crate::signal) recipients: Vec<ServiceId>,
    // False where the recipients are found as its sending begins, those the peer's messages go to then.
    pub(in crate::signal) is_addressed: bool,
    pub(in crate::signal) content: Vec<u8>,
    pub(in crate::signal) timestamp: u64,
    pub(in crate::signal) is_urgent: bool,
    pub(in crate::signal) is_plain: bool,
    pub(in crate::signal) group: Option<[u8; 32]>,
    pub(in crate::signal) endorsing_group: Option<Peer>,
    pub(in crate::signal) is_sync_request: bool,
    pub(in crate::signal) hint: u32,
    // A content for a group's members, which goes under the R1's sender key where enough of them take it (`sendToGroup`).
    pub(in crate::signal) is_group_send: bool,
    // A conversation's message, whose status is kept and which waits for its group's state where a later revision is due.
    pub(in crate::signal) is_message: bool,
    pub(in crate::signal) is_logged: bool,
    pub(in crate::signal) logged_peer: Option<Peer>,
    // The call's signal it carries, which goes now or never.
    pub(in crate::signal) call: Option<Outbound>,
}

impl core::fmt::Debug for Order {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Order")
            .field("timestamp", &self.timestamp)
            .field("urgent", &self.is_urgent)
            .field("message", &self.is_message)
            .finish_non_exhaustive()
    }
}

impl Order {
    fn to_peer(peer: Peer, recipients: Vec<ServiceId>, content: Vec<u8>, (timestamp, is_urgent): (u64, bool)) -> Order {
        Order {
            peer,
            recipients,
            is_addressed: true,
            content,
            timestamp,
            is_urgent,
            is_plain: false,
            group: None,
            endorsing_group: None,
            is_sync_request: false,
            hint: sealed::RESENDABLE,
            is_group_send: false,
            is_message: false,
            is_logged: true,
            logged_peer: None,
            call: None,
        }
    }

    pub(in crate::signal) fn uses_sender_key(&self) -> bool {
        self.is_group_send && !self.is_plain && matches!(self.peer, Peer::Group(_))
    }

    pub(in crate::signal) fn new(recipient: ServiceId, content: Vec<u8>, timestamp: u64, is_urgent: bool) -> Order {
        Order::to_peer(Peer::Contact(recipient), alloc::vec![recipient], content, (timestamp, is_urgent))
    }

    fn with_logged_peer(mut self, peer: Peer) -> Order {
        self.logged_peer = Some(peer);
        self
    }

    pub(in crate::signal) fn with_endorsing_group(mut self, peer: Peer) -> Order {
        self.endorsing_group = matches!(peer, Peer::Group(_)).then_some(peer);
        self
    }

    pub(in crate::signal) fn message(peer: Peer, content: Vec<u8>, timestamp: u64, is_urgent: bool) -> Order {
        let order = Order::to_peer(peer, Vec::new(), content, (timestamp, is_urgent));
        let is_group_send = matches!(peer, Peer::Group(_));
        Order { is_addressed: false, is_group_send, is_message: true, logged_peer: Some(peer), ..order }
    }

    // A call's signal as Signal-Desktop sends one (`sendCallingMessage`): to the recipients named, else to those the peer's messages go to; urgent alone where it rings, of no content hint, kept in no send log, a group's under the R1's sender key where enough members take it and else sealed under the group's endorsements.
    pub(in crate::signal) fn call(
        peer: Peer,
        recipients: Option<Vec<ServiceId>>,
        (content, timestamp): (Vec<u8>, u64),
        outbound: Outbound,
    ) -> Order {
        let is_addressed = recipients.is_some();
        let order =
            Order::to_peer(peer, recipients.unwrap_or_default(), content, (timestamp, outbound.signal.is_urgent()));
        Order {
            is_addressed,
            hint: sealed::DEFAULT,
            is_group_send: matches!(peer, Peer::Group(_)),
            is_logged: false,
            call: Some(outbound),
            ..order.with_endorsing_group(peer)
        }
    }

    pub(in crate::signal) fn resend_request(
        recipient: ServiceId,
        error: &DecryptionError,
        group: Option<[u8; 32]>,
    ) -> Order {
        let content = content::write_decryption_error(error);
        Order {
            is_plain: true,
            group,
            endorsing_group: group.map(Peer::Group),
            hint: sealed::DEFAULT,
            is_logged: false,
            ..Order::new(recipient, content, error.timestamp, false)
        }
    }

    fn sync_request(own: ServiceId, content: Vec<u8>, now: u64) -> Order {
        Order { is_sync_request: true, ..Order::new(own, content, now, false) }
    }

    fn resend(
        recipient: ServiceId,
        content: Vec<u8>,
        (timestamp, is_urgent): (u64, bool),
        group: Option<[u8; 32]>,
    ) -> Order {
        Order {
            is_logged: false,
            group,
            endorsing_group: group.map(Peer::Group),
            ..Order::new(recipient, content, timestamp, is_urgent)
        }
    }

    pub(in crate::signal) fn distribution(
        peer: Peer,
        recipients: Vec<ServiceId>,
        content: Vec<u8>,
        timestamp: u64,
        (is_urgent, hint): (bool, u32),
    ) -> Order {
        let group = match peer {
            Peer::Group(id) => Some(id),
            Peer::Contact(_) => None,
        };
        let is_logged = recipients.len() > 1;
        Order { group, hint, is_logged, ..Order::to_peer(peer, recipients, content, (timestamp, is_urgent)) }
    }
}

// The bits of a random number that make a padding's length less one: 1 to 512 bytes (Signal-Desktop's `getRandomPadding`).
const PADDING_MASK: u16 = 0x1FF;

pub(super) fn padding(crypto: &impl Crypto) -> Option<Vec<u8>> {
    let mut length = [0u8; 2];
    crypto.random(&mut length).then_some(())?;
    let mut padding = alloc::vec![0u8; usize::from(u16::from_le_bytes(length) & PADDING_MASK) + 1];
    crypto.random(&mut padding).then_some(padding)
}

impl Device {
    pub(in crate::signal) fn recipients(&self, peer: &Peer, now: u64) -> Vec<ServiceId> {
        let own = self.account.aci();
        match peer {
            Peer::Contact(contact) if *contact == own => alloc::vec![own],
            Peer::Contact(contact) if self.is_blocked(peer) || self.is_off_signal(contact, now) => Vec::new(),
            Peer::Contact(contact) => alloc::vec![*contact],
            Peer::Group(_) => self
                .group(peer)
                .filter(|group| group.members.contains(&own) && !group.terminated && !self.is_blocked(peer))
                .filter(|group| !group.announcements_only || group.admins.contains(&own))
                .map_or_else(Vec::new, |group| {
                    let others = group.members.iter().copied().filter(|member| *member != own);
                    others.filter(|member| !self.blocklist.acis.contains(member)).collect()
                }),
        }
    }

    pub(super) fn timer_of(&self, peer: &Peer) -> (u32, u32) {
        let conversation = self.conversations.get(peer);
        match conversation.and_then(|conversation| conversation.group.as_ref()) {
            Some(group) => (group.timer, 0),
            None => {
                conversation.map_or((0, 1), |conversation| (conversation.expire, conversation.expire_version.max(1)))
            }
        }
    }

    fn write_data(&self, peer: &Peer, (body, attachments): (&str, &[Attachment]), timestamp: u64) -> Writer {
        let group = self.group(peer).map(|group| (&group.master_key, group.latest_revision()));
        let data = content::write_data(body, timestamp, Some(&self.account.profile_key), self.timer_of(peer), group);
        content::write_attached(data, attachments)
    }

    fn write_content(
        &self,
        crypto: &impl Crypto,
        peer: &Peer,
        said: (&str, &[Attachment]),
        (timestamp, started): (u64, u64),
    ) -> Option<Vec<u8>> {
        let data = self.write_data(peer, said, timestamp);
        if *peer != Peer::Contact(self.account.aci()) {
            return Some(content::write_message(data));
        }
        Some(content::write_transcript(peer.contact().as_ref(), &[], (timestamp, started), data, &padding(crypto)?))
    }

    // Takes an owner's message into its conversation as being sent, with a key of its own for each file it carries; the order that sends it, once each file went up. Nothing is taken in where the library gives no keys.
    pub(crate) fn compose(
        &mut self,
        crypto: &impl Crypto,
        peer: Peer,
        (body, enclosures): (&str, &[Enclosure]),
        now: u64,
        is_robot: bool,
    ) -> Option<Order> {
        let attachments: Vec<Attachment> = enclosures
            .iter()
            .map(|enclosure| {
                let mut keys = [0u8; KEYS];
                crypto.random(&mut keys).then(|| enclosure.attach(keys))
            })
            .collect::<Option<_>>()?;
        let timestamp = self.stamp(now);
        let text = content::Text {
            timestamp,
            body: String::from(body),
            placeholders: attachments.iter().map(Attachment::placeholder).collect(),
            attachments: attachments.clone(),
            ..content::Text::default()
        };
        let mut entry = Entry::outgoing(&text, self.entry_time(now), Status::Sending);
        entry.is_robot = is_robot;
        entry.expire = self.timer_of(&peer).0;
        if peer == Peer::Contact(self.account.aci()) {
            entry.started = Some(now);
        } else {
            self.set_sharing(peer, true);
        }
        let keeps_muted = self.account.listing.keep_muted_archived;
        let sent = entry.clone();
        let conversation = self.conversation_mut(peer);
        conversation.is_sent_to = true;
        conversation.add(entry);
        if !(keeps_muted && conversation.is_muted(now)) {
            conversation.archived.is_set = false;
        }
        if !attachments.iter().all(Attachment::is_located) {
            return None;
        }
        self.message_order(crypto, peer, &sent, now)
    }

    // Takes a message to send into its conversation as `compose` does; with its order, the voice notes just said among its files, each by the name to be kept under in the attachments' directory.
    pub(in crate::signal) fn keep_send(
        &mut self,
        crypto: &impl Crypto,
        mut send: Outgoing,
        now: u64,
    ) -> (Option<Order>, Vec<(String, Vec<u8>)>) {
        let sounds = send.enclosures.iter_mut().filter_map(|enclosure| enclosure.take_sound(crypto)).collect();
        (self.compose(crypto, send.peer, (&send.body, &send.enclosures), now, send.is_robot), sounds)
    }

    /// Takes the messages to send into their conversations before a stay stands, which sends them; the voice notes just said among their files, to be kept (`keep_send`).
    pub fn keep_sends(&mut self, crypto: &impl Crypto, sends: Vec<Outgoing>, now: u64) -> Vec<(String, Vec<u8>)> {
        sends.into_iter().flat_map(|send| self.keep_send(crypto, send, now).1).collect()
    }

    pub(super) fn message_order(&self, crypto: &impl Crypto, peer: Peer, entry: &Entry, now: u64) -> Option<Order> {
        let said = (&entry.body[..], &entry.attachments[..]);
        let content = self.write_content(crypto, &peer, said, (entry.timestamp, entry.started.unwrap_or(now)))?;
        Some(Order::message(peer, content, entry.timestamp, self.is_urgent(&peer)))
    }

    fn is_urgent(&self, peer: &Peer) -> bool {
        *peer != Peer::Contact(self.account.aci())
    }

    // The orders of the owner's messages that wait to be sent, those whose files all went up.
    pub(crate) fn unsent_orders(&self, crypto: &impl Crypto, now: u64) -> Vec<Order> {
        self.conversations
            .values()
            .flat_map(|conversation| {
                let unsent = conversation.entries.iter().filter(|entry| {
                    entry.is_owner
                        && entry.status == Status::Sending
                        && entry.attachments.iter().all(Attachment::is_located)
                });
                unsent.filter_map(|entry| self.message_order(crypto, conversation.peer, entry, now))
            })
            .collect()
    }

    // Takes how far the owner's message got as its sending ended; one that was sent is owed its transcript to the owner's other devices.
    pub(in crate::signal) fn note_sent(
        &mut self,
        crypto: &impl Crypto,
        peer: Peer,
        (timestamp, sealed_recipients): (u64, &[ServiceId]),
        status: Status,
        now: u64,
    ) -> Option<Order> {
        let entry = self.conversations.get(&peer)?.entry(timestamp, OWN)?;
        let (body, attachments) = (entry.body.clone(), entry.attachments.clone());
        let is_own = peer == Peer::Contact(self.account.aci());
        let conversation = self.conversation_mut(peer);
        conversation.set_status(timestamp, status);
        if status != Status::Sent || is_own {
            return None;
        }
        conversation.start_sent_timer(timestamp, now);
        // A receipt that came before the service's answer finds its message now.
        self.retry_early(crypto, now);
        let padding = padding(crypto)?;
        let data = self.write_data(&peer, (&body, &attachments), timestamp);
        let recipients: Vec<(ServiceId, bool)> = self
            .recipients(&peer, now)
            .into_iter()
            .map(|recipient| (recipient, sealed_recipients.contains(&recipient)))
            .collect();
        let transcript =
            content::write_transcript(peer.contact().as_ref(), &recipients, (timestamp, now), data, &padding);
        Some(Order::new(self.account.aci(), transcript, timestamp, false).with_logged_peer(peer))
    }

    pub(in crate::signal) fn mark_read(&mut self, crypto: &impl Crypto, peer: Peer, now: u64) -> Vec<Order> {
        let Some(conversation) = self.conversations.get_mut(&peer) else { return Vec::new() };
        // A conversation marked unread is so no more once the owner opens it, as on Signal-Desktop.
        let was_flagged = conversation.is_safety_changed || conversation.marked_unread.is_set;
        (conversation.is_safety_changed, conversation.marked_unread.is_set) = (false, false);
        let reads = conversation.read_all(now);
        if was_flagged || !reads.is_empty() {
            self.dirty.insert(File::Conversation(peer));
        }
        let reads: Vec<(ServiceId, u64)> =
            reads.into_iter().filter_map(|(sender, timestamp)| Some((sender.or(peer.contact())?, timestamp))).collect();
        if reads.is_empty() {
            return Vec::new();
        }
        let mut orders = Vec::new();
        if self.account.read_receipts && self.is_accepted(&peer, true) {
            let mut by_sender: BTreeMap<ServiceId, Vec<u64>> = BTreeMap::new();
            reads.iter().for_each(|(sender, timestamp)| by_sender.entry(*sender).or_default().push(*timestamp));
            let receipts = by_sender.into_iter().map(|(sender, timestamps)| ((peer, sender), timestamps));
            orders = self.receipt_orders(receipts, true, now);
        }
        if let Some(padding) = padding(crypto) {
            orders.push(Order::new(self.account.aci(), content::write_read_sync(&reads, &padding), now, true));
        }
        orders
    }

    pub(in crate::signal) fn take_delivery_receipts(&mut self, now: u64) -> Vec<Order> {
        let mut receipts: Vec<((Peer, ServiceId), Vec<u64>)> = Vec::new();
        for (sender, peer, timestamp) in core::mem::take(&mut self.owed_receipts) {
            match receipts.iter_mut().find(|(key, _)| *key == (peer, sender)) {
                Some((_, timestamps)) => timestamps.push(timestamp),
                None => receipts.push(((peer, sender), alloc::vec![timestamp])),
            }
        }
        self.receipt_orders(receipts, false, now)
    }

    // The receipts, of reading or of delivery, of messages by their conversation and writer, in their order, each of so many timestamps at the most, to the writers that take receipts.
    fn receipt_orders(
        &self,
        receipts: impl IntoIterator<Item = ((Peer, ServiceId), Vec<u64>)>,
        is_read: bool,
        now: u64,
    ) -> Vec<Order> {
        let mut orders = Vec::new();
        for ((peer, sender), timestamps) in receipts {
            if self.takes_receipts(sender, now) {
                let chunks = timestamps.chunks(MAX_RECEIPT_TIMESTAMPS);
                orders.extend(chunks.map(|chunk| {
                    let receipt = content::write_receipt(is_read, chunk);
                    Order::new(sender, receipt, now, false).with_endorsing_group(peer)
                }));
            }
        }
        orders
    }

    pub(in crate::signal) fn take_profile_key_responses(&mut self, now: u64) -> Vec<Order> {
        let contacts = core::mem::take(&mut self.owed_profile_keys);
        let contacts = contacts
            .into_iter()
            .filter(|contact| self.is_trusted_contact(contact) && self.takes_receipts(*contact, now));
        contacts.map(|contact| self.profile_key_order(contact, now)).collect()
    }

    // The owner's profile key alone to a contact, not urgent (Signal-Desktop's `sendProfileKey`).
    pub(super) fn profile_key_order(&self, contact: ServiceId, timestamp: u64) -> Order {
        Order::new(contact, content::write_profile_key_update(&self.account.profile_key, timestamp), timestamp, false)
    }

    pub(in crate::signal) fn sync_requests(&self, crypto: &impl Crypto, now: u64) -> Vec<Order> {
        [content::CONFIGURATION, content::BLOCKED]
            .into_iter()
            .filter_map(|kind| {
                Some(Order::sync_request(self.account.aci(), content::write_sync_request(kind, &padding(crypto)?), now))
            })
            .collect()
    }

    pub(in crate::signal) fn note_synced(&mut self) {
        self.is_synced = true;
        self.dirty.insert(File::Seen);
    }

    pub(in crate::signal) fn log_sent(
        &mut self,
        recipient: ServiceId,
        (timestamp, is_urgent): (u64, bool),
        content: &[u8],
        peer: Option<Peer>,
        now: u64,
    ) {
        self.forget_sent(now);
        self.sent_records.push(SentRecord { timestamp, recipient, content: content.to_vec(), is_urgent, peer });
        self.dirty.insert(File::Sent);
    }

    fn forget_sent(&mut self, now: u64) {
        let count = self.sent_records.len();
        self.sent_records.retain(|record| record.timestamp.saturating_add(SENT_AGE) >= now);
        if self.sent_records.len() != count {
            self.dirty.insert(File::Sent);
        }
    }

    pub(super) fn forget_gone(&mut self) {
        self.is_dropped = true;
        let conversations = &self.conversations;
        let count = self.sent_records.len();
        self.sent_records.retain(|record| {
            record.peer.is_none_or(|peer| {
                conversations
                    .get(&peer)
                    .is_some_and(|conversation| conversation.own_timestamps().any(|at| at == record.timestamp))
            })
        });
        if self.sent_records.len() != count {
            self.dirty.insert(File::Sent);
        }
    }

    fn may_retry(&mut self, service: ServiceId, device: u32, timestamp: u64, now: u64) -> bool {
        self.retry_counts.retain(|_, (_, began)| began.saturating_add(RESEND_AGE) > now);
        let (count, _) = self.retry_counts.entry((service, device, timestamp)).or_insert((0, now));
        *count = count.saturating_add(1);
        let may_retry = *count <= RETRY_LIMIT;
        let first = self.retry_counts.iter().min_by_key(|(_, (_, began))| *began).map(|(key, _)| *key);
        if let Some(first) = first.filter(|_| self.retry_counts.len() > MAX_RETRY_COUNTS) {
            self.retry_counts.remove(&first);
        }
        may_retry
    }

    pub(in crate::signal) fn request_resend(
        &mut self,
        sender: Address,
        error: &DecryptionError,
        group: Option<[u8; 32]>,
        now: u64,
    ) -> Option<Order> {
        self.may_retry(sender.service, sender.device, error.timestamp, now)
            .then(|| Order::resend_request(sender.service, error, group))
    }

    pub(in crate::signal) fn resend(
        &mut self,
        crypto: &impl Crypto,
        requester: Address,
        error: &DecryptionError,
        group: Option<[u8; 32]>,
        now: u64,
    ) -> Option<Order> {
        if !self.may_retry(requester.service, requester.device, error.timestamp, now) {
            return None;
        }
        let is_local = error.device == self.account.device;
        let is_current_ratchet = error
            .ratchet
            .is_some_and(|ratchet| self.sessions.get(&requester).is_some_and(|record| record.sends_under(&ratchet)));
        let is_archived = is_local && is_current_ratchet;
        if is_archived {
            self.archive_session(requester);
        }
        self.forget_sent(now);
        let is_recent = error.timestamp.saturating_add(RESEND_AGE) > now;
        let record = self
            .sent_records
            .iter()
            .find(|record| is_recent && record.recipient == requester.service && record.timestamp == error.timestamp)
            .cloned();
        if let Some(record) = record {
            let (mut content, mut group) = (record.content, None);
            if let Some(peer @ Peer::Group(id)) = record.peer {
                group = Some(id);
                if let Some(distribution) = self.distribution_for(crypto, &peer, &requester.service)? {
                    content = content::with_distribution(&content, &distribution);
                }
            }
            return Some(Order::resend(requester.service, content, (record.timestamp, record.is_urgent), group));
        }
        if let Some(id) = group.filter(|id| self.group(&Peer::Group(*id)).is_some()) {
            let peer = Peer::Group(id);
            match self.distribution_for(crypto, &peer, &requester.service)? {
                Some(distribution) => {
                    let content = content::write_distribution(&distribution);
                    let timestamp = self.stamp(now);
                    return Some(Order::distribution(
                        peer,
                        alloc::vec![requester.service],
                        content,
                        timestamp,
                        (false, sealed::IMPLICIT),
                    ));
                }
                None if !is_archived => return None,
                None => {}
            }
        }
        is_archived
            .then(|| Some(Order::new(requester.service, content::write_null(&padding(crypto)?), now, false)))
            .flatten()
    }

    fn distribution_for(
        &mut self,
        crypto: &impl Crypto,
        peer: &Peer,
        requester: &ServiceId,
    ) -> Option<Option<Vec<u8>>> {
        let Some(group) = self.group(peer) else { return Some(None) };
        if !group.members.contains(requester) {
            return None;
        }
        let Some(key) = group.sender_key.clone() else { return Some(None) };
        if !self.owns_distribution(&key.distribution) {
            return None;
        }
        Some(self.own_distribution(crypto, &key.distribution))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::attachment::{ATTACHMENTS, Finish, Job};
    use crate::signal::content::Placeholder;
    use crate::signal::tests::Standin;

    const PEER_ALICE: Peer = Peer::Contact(ServiceId::Aci([1; 16]));

    fn say(body: &str, is_robot: bool) -> Outgoing {
        Outgoing { peer: PEER_ALICE, body: body.into(), is_robot, enclosures: Vec::new() }
    }

    #[test]
    fn kept_sends() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        assert!(device.keep_sends(&crypto, Vec::new(), 1_000).is_empty(), "none");
        let sends = alloc::vec![say("hi", false), say("\u{1F916} Soon", true)];
        assert!(device.keep_sends(&crypto, sends, 1_000).is_empty(), "no voice note said among them");
        assert_eq!(
            device
                .entries(&PEER_ALICE)
                .iter()
                .map(|entry| (entry.timestamp, entry.status, entry.is_robot))
                .collect::<Vec<_>>(),
            [(1_000, Status::Sending, false), (1_001, Status::Sending, true)],
            "a millisecond apart, auto mode's marked"
        );
        assert!(device.take_dirty().contains(&File::Conversation(PEER_ALICE)), "kept");
        assert_eq!(device.unsent_orders(&crypto, 2_000).len(), 2, "the next stay sends them");
    }

    #[test]
    fn kept_sends_with_files() {
        let (mut device, crypto) = (Device::unkeyed(), Standin::default());
        let note = Enclosure::said(b"sound".to_vec());
        let photo = Enclosure::photo("camera/pictures/a.jpg".into(), (4, 3));
        let send = Outgoing { enclosures: alloc::vec![note, photo], ..say("", false) };
        let sounds = device.keep_sends(&crypto, alloc::vec![send], 1_000);
        let name = alloc::format!("{}.m4a", crate::base::code::hex(&crypto.sha256(&[b"sound"])));
        assert_eq!(sounds, [(name.clone(), b"sound".to_vec())], "the sound to be kept");
        let uploads = device.uploads_due();
        let kept: Vec<String> = uploads.iter().map(|upload| String::from(Job::Up(upload.clone()).writes())).collect();
        assert_eq!(
            kept,
            [alloc::format!("{ATTACHMENTS}/{name}"), alloc::format!("{ATTACHMENTS}/1000-1.jpg")],
            "each file waits to go up, to be kept beside the others"
        );
        assert_eq!(device.entries(&PEER_ALICE)[0].placeholders, [Placeholder::VoiceNote, Placeholder::Photo]);
        let finished = alloc::vec![(Job::Up(uploads[0].clone()), Finish::Refused)];
        assert!(device.take_finished(&crypto, finished, 2_000).1, "a message with a file refused changed");
        assert!(!device.take_finished(&crypto, Vec::new(), 3_000).1);
    }
}

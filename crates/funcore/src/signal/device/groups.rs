use super::{Address, Device, DropReason, Event, File, Peer, Reception, SenderKeys, ServiceId};
use crate::base::code::{UUID_RANDOM, to_uuid};
use crate::signal::Crypto;
use crate::signal::group::{self, Context, Group, GroupFetch, GroupUpdate, SenderKey};
use crate::signal::sender_keys::{self, Distribution};
use crate::signal::session::Error;
use alloc::boxed::Box;
use alloc::collections::btree_map;
use alloc::string::String;
use alloc::vec::Vec;

pub(super) const SENDER_KEY_AGE: u64 = 90 * 24 * 60 * 60 * 1000;

impl Device {
    // The conversation of the group of that master key, made, and its file marked, only where none holds the group.
    pub(super) fn ensure_group(&mut self, crypto: &impl Crypto, master_key: &[u8; 32]) -> Peer {
        let peer = Peer::Group(group::id_of(crypto, master_key));
        if self.group(&peer).is_none() {
            self.conversation_mut(peer).group = Some(Group::new(*master_key));
        }
        peer
    }

    pub(super) fn note_group(&mut self, crypto: &impl Crypto, context: &Context) -> Peer {
        let peer = self.ensure_group(crypto, &context.master_key);
        if let Some(group) = self.group_mut(&peer) {
            group.note_revision(context.revision);
        }
        peer
    }

    pub(in crate::signal) fn groups_due(&self) -> Vec<(Peer, [u8; 32], Option<u32>, bool)> {
        self.conversations
            .iter()
            .filter_map(|(peer, conversation)| {
                let group = conversation.group.as_ref()?;
                let due = group.due_revision();
                (due.is_some() || group.needs_refresh).then_some((*peer, group.master_key, due, group.needs_refresh))
            })
            .collect()
    }

    pub(in crate::signal) fn group_due(&self, peer: &Peer) -> (Option<u32>, bool) {
        self.group(peer).map_or((None, false), |group| (group.due_revision(), group.needs_refresh))
    }

    pub(in crate::signal) fn can_refresh(&self, peer: &Peer) -> bool {
        self.group(peer).is_some_and(|group| !group.terminated) && !self.is_blocked(peer)
    }

    pub(in crate::signal) fn refresh_group(&mut self, peer: &Peer) {
        if let Some(group) = self.group_mut(peer) {
            group.needs_refresh = true;
        }
    }

    pub(in crate::signal) fn has_endorsements(&self, peer: &Peer, now: u64) -> bool {
        self.group(peer).is_some_and(|group| group.usable_endorsements(now).is_some())
    }

    pub(in crate::signal) fn endorses(&self, peer: &Peer, member: &ServiceId, now: u64) -> bool {
        self.group(peer)
            .and_then(|group| group.usable_endorsements(now))
            .is_some_and(|endorsements| endorsements.has(member))
    }

    pub(in crate::signal) fn group_token(
        &self,
        crypto: &impl Crypto,
        peer: &Peer,
        members: &[ServiceId],
        now: u64,
    ) -> Option<Vec<u8>> {
        let group = self.group(peer)?;
        let endorsements = group.usable_endorsements(now)?;
        endorsements.token(crypto, members, &group::Keys::new(crypto, &group.master_key).uid)
    }

    pub(in crate::signal) fn members(&self, peer: &Peer) -> &[ServiceId] {
        self.group(peer).map_or(&[][..], |group| &group.members[..])
    }

    pub(in crate::signal) fn master_key(&self, peer: &Peer) -> Option<[u8; 32]> {
        self.group(peer).map(|group| group.master_key)
    }

    pub(in crate::signal) fn sender_key(&mut self, crypto: &impl Crypto, peer: &Peer, now: u64) -> Option<SenderKey> {
        let existing = self.group(peer)?.sender_key.clone();
        match existing {
            Some(key) if key.created.saturating_add(SENDER_KEY_AGE) > now => Some(key),
            Some(_) => {
                self.reset_sender_key(peer, now);
                self.group(peer)?.sender_key.clone()
            }
            None => {
                let mut distribution = [0u8; 16];
                crypto.random(&mut distribution).then_some(())?;
                let distribution = to_uuid(distribution, UUID_RANDOM);
                let key = SenderKey { created: now, distribution, devices: Vec::new() };
                self.group_mut(peer)?.sender_key = Some(key.clone());
                Some(key)
            }
        }
    }

    pub(in crate::signal) fn reset_sender_key(&mut self, peer: &Peer, now: u64) {
        let Some(key) = self.group_mut(peer).and_then(|group| group.sender_key.as_mut()) else { return };
        (key.created, key.devices) = (now, Vec::new());
        let distribution = key.distribution;
        let own = self.own_address();
        if self.sender_keys.remove(&(own, distribution)).is_some() {
            self.dirty.insert(File::SenderKey(own, distribution));
        }
    }

    pub(in crate::signal) fn set_sender_devices(&mut self, peer: &Peer, devices: Vec<(ServiceId, u32, u32)>) {
        if let Some(key) = self.group_mut(peer).and_then(|group| group.sender_key.as_mut()) {
            key.devices = devices;
        }
    }

    pub(in crate::signal) fn drop_sender_devices(&mut self, peer: &Peer, stale: &[(ServiceId, u32)]) {
        if stale.is_empty() {
            return;
        }
        if let Some(key) = self.group_mut(peer).and_then(|group| group.sender_key.as_mut()) {
            key.devices.retain(|(account, device, _)| !stale.contains(&(*account, *device)));
        }
    }

    pub(in crate::signal) fn own_distribution(
        &mut self,
        crypto: &impl Crypto,
        distribution: &[u8; 16],
    ) -> Option<Vec<u8>> {
        let own = self.own_address();
        if let btree_map::Entry::Vacant(vacant) = self.sender_keys.entry((own, *distribution)) {
            vacant.insert(SenderKeys::generate(crypto)?);
            self.dirty.insert(File::SenderKey(own, *distribution));
        }
        self.sender_keys.get(&(own, *distribution))?.distribution(distribution)
    }

    pub(in crate::signal) fn owns_distribution(&self, distribution: &[u8; 16]) -> bool {
        self.sender_keys
            .get(&(self.own_address(), *distribution))
            .is_some_and(|record| record.distribution(distribution).is_some())
    }

    pub(in crate::signal) fn group_encrypt(
        &mut self,
        crypto: &impl Crypto,
        distribution: &[u8; 16],
        plain: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let own = self.own_address();
        let record = self.sender_keys.get_mut(&(own, *distribution)).ok_or(Error::NoSession)?;
        let ciphertext = record.encrypt(crypto, distribution, plain)?;
        self.dirty.insert(File::SenderKey(own, *distribution));
        Ok(ciphertext)
    }

    pub(super) fn open_by_sender_key(
        &mut self,
        crypto: &impl Crypto,
        sender: &Address,
        bytes: &[u8],
    ) -> Result<Vec<u8>, Error> {
        let distribution = sender_keys::parse_distribution_id(bytes).ok_or(Error::Malformed)?;
        let record = self.sender_keys.get_mut(&(*sender, distribution)).ok_or(Error::NoSession)?;
        let plain = record.open(crypto, bytes)?;
        self.dirty.insert(File::SenderKey(*sender, distribution));
        Ok(plain)
    }

    pub(super) fn take_distribution(&mut self, sender: &Address, bytes: &[u8]) -> bool {
        let Some(distribution) = Distribution::read(bytes) else { return false };
        self.sender_keys.entry((*sender, distribution.id)).or_default().add_distribution(&distribution);
        self.dirty.insert(File::SenderKey(*sender, distribution.id));
        true
    }

    /// Takes in a fetch from the groups server, then the messages that waited for each state (`handleDataMessage`), and hands on the tokens of groups' calls it asked; the log line, the events, and why each message that waited was dropped.
    pub fn take_group_fetch(
        &mut self,
        crypto: &impl Crypto,
        fetch: GroupFetch,
    ) -> (String, Vec<Event>, Vec<DropReason>) {
        let (updates, tokens, line) = fetch.finish();
        let (mut events, drops) = self.apply_group_updates(crypto, updates);
        for (peer, token) in tokens {
            let membership = token.and_then(|token| self.membership(crypto, &peer, token));
            events.push(Event::Token(peer, membership.map(Box::new)));
        }
        (line, events, drops)
    }

    pub(super) fn apply_group_updates(
        &mut self,
        crypto: &impl Crypto,
        updates: Vec<(Peer, GroupUpdate)>,
    ) -> (Vec<Event>, Vec<DropReason>) {
        let (mut events, mut drops) = (Vec::new(), Vec::new());
        for (peer, update) in updates {
            let Some(conversation) = self.conversations.get_mut(&peer) else { continue };
            let Some(group) = conversation.group.as_mut() else { continue };
            group.apply(update);
            let queue = core::mem::take(&mut conversation.queue);
            self.dirty.insert(File::Conversation(peer));
            for message in queue {
                let meta = (message.timestamp, message.is_sealed, message.at);
                match self.receive_content(crypto, &message.content, message.sender, meta, message.at, true) {
                    Ok(Reception::Event(event)) => events.push(event),
                    Ok(_) => {}
                    Err(reason) => drops.push(reason),
                }
            }
        }
        (events, drops)
    }
}

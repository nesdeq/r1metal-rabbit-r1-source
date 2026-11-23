use super::{Device, File, Peer, Ring, ServiceId};
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::account::Identity;
use crate::signal::address::Address;
use crate::signal::content::PniChange;
use crate::signal::keys::{KyberKey, SignedKey};
use crate::signal::message::encode_key;
use crate::signal::{Crypto, curve};
use alloc::collections::btree_map;
use alloc::vec::Vec;

pub(super) const DEFAULT: u32 = 0;
pub(super) const VERIFIED: u32 = 1;
pub(super) const UNVERIFIED: u32 = 2;
pub(super) const TRUST_DELAY: u64 = 5 * 1000;
pub(super) const ALTERNATE_FIRST: [u8; 32] = [0xFF; 32];
pub(super) const ALTERNATE_SECOND: &[u8] = b"Signal_PNI_Signature";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::signal) struct KnownIdentity {
    pub(in crate::signal) key: [u8; 32],
    pub(in crate::signal) verified: u32,
    pub(in crate::signal) first_use: bool,
    pub(in crate::signal) timestamp: u64,
    pub(in crate::signal) is_approved: bool,
}

impl core::fmt::Debug for KnownIdentity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Known")
            .field("verified", &self.verified)
            .field("first_use", &self.first_use)
            .field("timestamp", &self.timestamp)
            .field("approved", &self.is_approved)
            .finish_non_exhaustive()
    }
}

impl KnownIdentity {
    pub(super) fn write(&self, id: &ServiceId) -> Writer {
        Writer::new()
            .bytes(1, &id.to_binary())
            .bytes(2, &self.key)
            .varint(3, u64::from(self.verified))
            .bool(4, self.first_use)
            .varint(5, self.timestamp)
            .bool(6, self.is_approved)
    }

    pub(super) fn read(entry: &Message) -> Option<KnownIdentity> {
        Some(KnownIdentity {
            key: entry.bytes(2)?.try_into().ok()?,
            verified: entry.u32(3).unwrap_or(DEFAULT),
            first_use: entry.bool(4).unwrap_or(false),
            timestamp: entry.varint(5).unwrap_or(0),
            is_approved: entry.bool(6).unwrap_or(false),
        })
    }

    const fn is_waiting(&self, now: u64) -> bool {
        !self.first_use && self.timestamp + TRUST_DELAY > now && !self.is_approved
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(in crate::signal) enum Trust {
    Trusted,
    Soon,
    Never,
}

impl Device {
    pub(super) fn add_own_identities(&mut self) {
        let pni = self.account.pni.map(ServiceId::Pni).zip(self.account.pni_identity.map(|pair| pair.public));
        for (id, key) in [(self.account.aci(), self.account.aci_identity.public)].into_iter().chain(pni) {
            if let btree_map::Entry::Vacant(vacant) = self.identities.entry(id) {
                vacant.insert(KnownIdentity {
                    key,
                    verified: VERIFIED,
                    first_use: true,
                    timestamp: 0,
                    is_approved: true,
                });
                self.dirty.insert(File::Identities);
            }
        }
    }

    pub(super) fn is_trusted_receiving(&self, service: &ServiceId, key: &[u8; 32]) -> bool {
        !self.is_own_id(service) || self.identities.get(service).is_none_or(|known| known.key == *key)
    }

    pub(in crate::signal) fn trust(&self, service: &ServiceId, key: &[u8; 32], now: u64) -> Trust {
        let Some(known) = self.identities.get(service) else { return Trust::Trusted };
        if known.key != *key {
            return Trust::Never;
        }
        if self.is_own_id(service) {
            return Trust::Trusted;
        }
        if known.verified == UNVERIFIED {
            return Trust::Never;
        }
        if known.is_waiting(now) { Trust::Soon } else { Trust::Trusted }
    }

    pub(in crate::signal) fn session_trust(&self, address: &Address, now: u64) -> Trust {
        let remote =
            self.sessions.get(address).and_then(|record| record.current.as_ref()).map(|state| state.remote_identity);
        remote.map_or(Trust::Trusted, |key| self.trust(&address.service, &key, now))
    }

    pub(in crate::signal) fn identity_key(&self, service: &ServiceId) -> Option<[u8; 32]> {
        self.identities.get(service).map(|known| known.key)
    }

    // A verified key that changed is left so until the phone verifies it anew.
    pub(super) fn is_unverified(&self, service: &ServiceId) -> bool {
        self.identities.get(service).is_some_and(|known| known.verified == UNVERIFIED)
    }

    pub(in crate::signal) fn save_identity(
        &mut self,
        service: ServiceId,
        key: [u8; 32],
        device: Option<u32>,
        now: u64,
    ) -> bool {
        let Some(known) = self.identities.get_mut(&service) else {
            let known = KnownIdentity { key, verified: DEFAULT, first_use: true, timestamp: now, is_approved: false };
            self.identities.insert(service, known);
            self.dirty.insert(File::Identities);
            return false;
        };
        if known.key == key || self.account.identity_of(&service).is_some() {
            return false;
        }
        let verified = if known.verified == DEFAULT { DEFAULT } else { UNVERIFIED };
        *known = KnownIdentity { key, verified, first_use: false, timestamp: now, is_approved: false };
        self.dirty.insert(File::Identities);
        let siblings: Vec<Address> = self
            .sessions
            .keys()
            .filter(|address| address.service == service && Some(address.device) != device)
            .copied()
            .collect();
        siblings.into_iter().for_each(|address| self.archive_session(address));
        self.note_key_changed(service);
        true
    }

    fn note_key_changed(&mut self, service: ServiceId) {
        if !self.is_own_id(&service) {
            self.conversation_mut(Peer::Contact(service)).is_safety_changed = true;
        }
    }

    pub(super) fn take_record_identity(
        &mut self,
        service: ServiceId,
        (key, state): ([u8; 32], u32),
        is_first: bool,
        now: u64,
    ) {
        let existing = self.identities.get(&service).copied();
        if !(is_first || existing.is_none_or(|known| known.key == key)) {
            return;
        }
        let verified = match state {
            VERIFIED | UNVERIFIED => state,
            _ => DEFAULT,
        };
        if existing.is_none_or(|known| known.key != key || known.verified != verified) {
            let known =
                KnownIdentity { key, verified, first_use: existing.is_none(), timestamp: now, is_approved: true };
            self.identities.insert(service, known);
            self.dirty.insert(File::Identities);
        }
        if existing.is_some_and(|known| known.key != key) {
            self.note_key_changed(service);
        }
    }

    pub(super) fn take_pni_change(&mut self, crypto: &impl Crypto, change: PniChange, pni: Option<[u8; 16]>, now: u64) {
        let Some(pni) = pni else { return };
        let PniChange { identity, signed_pre_key, last_resort, registration, number } = change;
        let (id, pair, signature, created) = signed_pre_key;
        let signed_key = SignedKey { id, pair, signature, created, confirmed: Some(now) };
        let last_resort = last_resort.map(|(id, private, public, signature, created)| KyberKey {
            id,
            private,
            public,
            signature,
            created,
            confirmed: Some(now),
        });
        let Some(ring) = Ring::from_keys(crypto, signed_key, last_resort) else { return };
        if let Some(old) = self.account.pni.map(ServiceId::Pni)
            && self.identities.remove(&old).is_some()
        {
            self.dirty.insert(File::Identities);
        }
        self.account.pni = Some(pni);
        self.account.pni_identity = Some(identity);
        (self.account.pni_registration, self.account.number) = (registration, Some(number));
        self.rings.insert(Identity::Pni, ring);
        self.certificates = super::Certificates::default();
        self.is_credential_stale = true;
        self.dirty.extend([File::Account, File::Ring(Identity::Pni), File::Certificates]);
        self.add_own_identities();
    }

    pub(super) fn take_pni_signature(
        &mut self,
        crypto: &impl Crypto,
        aci: ServiceId,
        pni: [u8; 16],
        signature: &[u8; 64],
    ) -> bool {
        let pni = ServiceId::Pni(pni);
        let (Some(pni_key), Some(aci_key)) = (self.identity_key(&pni), self.identity_key(&aci)) else { return false };
        let signed_parts = [&ALTERNATE_FIRST[..], ALTERNATE_SECOND, &encode_key(&aci_key)];
        if !matches!(aci, ServiceId::Aci(_)) || !curve::verify(crypto, &pni_key, &signed_parts, signature) {
            return false;
        }
        let is_known = self.contacts.get(&aci).is_some_and(|contact| contact.pni == Some(pni));
        let pni_conversation = self.conversations.remove(&Peer::Contact(pni));
        if is_known && pni_conversation.is_none() {
            return false;
        }
        self.contact_mut(aci).pni = Some(pni);
        if let Some(pni_conversation) = pni_conversation {
            self.dirty.insert(File::Conversation(Peer::Contact(pni)));
            let conversation = self.conversation_mut(Peer::Contact(aci));
            for entry in pni_conversation.entries {
                conversation.add(entry);
            }
        }
        true
    }
}

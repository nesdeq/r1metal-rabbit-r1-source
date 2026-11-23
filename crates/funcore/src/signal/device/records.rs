use super::sealing::UNKNOWN;
use super::{Device, File, Peer};
use crate::signal::Crypto;
use crate::signal::content::{ACCEPT, BLOCK};
use crate::signal::storage::{AccountRecord, ContactRecord, GroupRecord, Pin, StorageRead, StorageRecords};
use alloc::string::String;
use alloc::vec::Vec;

impl Device {
    pub(super) fn merge_contact(&mut self, record: ContactRecord, is_first: bool, now: u64) {
        let aci = record.aci;
        if aci == self.account.aci() {
            return;
        }
        self.apply_list_state(Peer::Contact(aci), record.list_state);
        let contact = self.contact_mut(aci);
        contact.name = record.name.or(contact.name.take());
        contact.number = record.number.or(contact.number.take());
        contact.username = record.username.or(contact.username.take());
        contact.pni = record.pni.or(contact.pni);
        let key = record.profile_key.or(contact.profile_key);
        if (is_first || contact.profile_key.is_none()) && key != contact.profile_key {
            (contact.profile_key, contact.access_mode) = (key, UNKNOWN);
        }
        contact.is_system = record.is_system;
        contact.unregistered = match record.unregistered {
            0 => None,
            at => Some(contact.unregistered.map_or(at, |existing| existing.max(at))),
        };
        if let Some(identity) = record.identity {
            self.take_record_identity(aci, identity, is_first, now);
        }
        let peer = Peer::Contact(aci);
        let held = self.conversations.get(&peer).map(|conversation| conversation.notify.2);
        if held.map_or(record.notify_calls.is_some(), |held| held != record.notify_calls) {
            self.conversation_mut(peer).notify.2 = record.notify_calls;
        }
        self.apply_request_state(peer, record.blocked, record.whitelisted);
        let contact = self.contact_mut(aci);
        if record.hidden && !contact.is_hidden {
            (contact.is_hidden, contact.response) = (true, 0);
            self.set_sharing(peer, false);
        } else if !record.hidden {
            contact.is_hidden = false;
        }
    }

    fn apply_request_state(&mut self, peer: Peer, blocked: bool, whitelisted: bool) {
        if blocked {
            self.apply_response(peer, BLOCK, true);
        } else if whitelisted {
            self.apply_response(peer, ACCEPT, true);
        } else {
            self.set_blocked(peer, false);
        }
        if !whitelisted {
            self.set_sharing(peer, false);
        }
    }

    pub(super) fn merge_group(&mut self, crypto: &impl Crypto, record: GroupRecord) {
        let peer = self.ensure_group(crypto, &record.master_key);
        self.apply_request_state(peer, record.blocked, record.whitelisted);
        self.apply_list_state(peer, record.list_state);
        self.change_conversation(peer, |conversation| {
            core::mem::replace(&mut conversation.notify, record.notify) != record.notify
        });
    }

    pub(super) fn merge_account(&mut self, crypto: &impl Crypto, record: AccountRecord) {
        self.account.read_receipts = record.read_receipts;
        self.account.is_number_shared = record.is_number_shared;
        if let Some(key) = record.profile_key {
            self.account.profile_key = key;
        }
        self.apply_list_state(Peer::Contact(self.account.aci()), record.note);
        let peers: Vec<Peer> = record
            .pins
            .into_iter()
            .filter_map(|pin| match pin {
                Pin::Contact(service) => Some(Peer::Contact(self.aci_of(&service))),
                Pin::Number(number) => Some(Peer::Contact(self.aci_of_number(&number)?)),
                Pin::Group(master_key) => Some(self.ensure_group(crypto, &master_key)),
            })
            .collect();
        for peer in &peers {
            if self.conversations.get(peer).is_none_or(|conversation| conversation.archived.is_set) {
                self.conversation_mut(*peer).archived.is_set = false;
            }
        }
        let listing = &mut self.account.listing;
        (listing.pinned, listing.keep_muted_archived) = (peers, record.keep_muted_archived);
        (listing.count_muted, listing.notify) = (record.count_muted, record.notify);
        self.dirty.insert(File::Account);
    }

    /// Takes in a reading of the storage service, as it ended at `now`, a first one where none listed the groups (`storageFetchComplete`); the log line.
    pub fn take_storage_read(&mut self, crypto: &impl Crypto, read: StorageRead, now: u64) -> String {
        let (records, line) = read.finish();
        if let Some(StorageRecords { version, contacts, groups, account }) = records {
            let is_first = !self.has_listed_groups;
            for record in contacts {
                self.merge_contact(record, is_first, now);
            }
            for record in groups {
                self.merge_group(crypto, record);
            }
            if let Some(record) = account {
                self.merge_account(crypto, record);
            }
            (self.storage_version, self.has_listed_groups) = (version, true);
        }
        self.storage_read_at = now;
        self.dirty.insert(File::Seen);
        line
    }
}

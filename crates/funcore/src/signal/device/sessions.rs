use super::{Address, Device, File, ServiceId};
use crate::signal::Crypto;
use crate::signal::session::{Ciphertext, State};
use alloc::vec::Vec;

impl Device {
    pub(in crate::signal) fn sendable_devices(&self, recipient: ServiceId, now: u64) -> Vec<Address> {
        let is_own = recipient == self.account.aci();
        self.sessions
            .iter()
            .filter(|(address, record)| {
                address.service == recipient
                    && !(is_own && address.device == self.account.device)
                    && record.can_send(now)
            })
            .map(|(address, _)| *address)
            .collect()
    }

    pub(in crate::signal) fn sends_to(&self, address: &Address, now: u64) -> bool {
        self.sessions.get(address).is_some_and(|record| record.can_send(now))
    }

    pub(in crate::signal) fn registration(&self, address: &Address) -> Option<u32> {
        self.sessions.get(address)?.current.as_ref().map(|state| state.remote_registration)
    }

    pub(in crate::signal) fn encrypt_to(
        &mut self,
        crypto: &impl Crypto,
        plain: &[u8],
        local: &Address,
        target: &Address,
        now: u64,
    ) -> Option<(u32, Ciphertext)> {
        let record = self.sessions.get_mut(target)?;
        let registration = record.current.as_ref().map_or(0, |state| state.remote_registration);
        let ciphertext = record.seal(crypto, plain, local, target, now).ok()?;
        self.dirty.insert(File::Session(*target));
        Some((registration, ciphertext))
    }

    pub(in crate::signal) fn archive_session(&mut self, address: Address) {
        if let Some(record) = self.sessions.get_mut(&address) {
            record.archive();
            self.dirty.insert(File::Session(address));
        }
    }

    pub(in crate::signal) fn archive_sessions(&mut self, service: ServiceId) {
        let addresses: Vec<Address> =
            self.sessions.keys().filter(|address| address.service == service).copied().collect();
        addresses.into_iter().for_each(|address| self.archive_session(address));
    }

    pub(in crate::signal) fn begin_session(&mut self, address: Address, state: State) {
        self.sessions.entry(address).or_default().promote(state);
        self.dirty.insert(File::Session(address));
    }
}

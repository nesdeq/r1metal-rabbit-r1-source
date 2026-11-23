use super::{Device, File, Identity, Ring};
use crate::signal::Crypto;
use crate::signal::keys::{
    CHECK_PERIOD, KyberKey, MAX_COUNT, MIN_COUNT, RENEW_AGE, SignedKey, drop_replaced, drop_unused, is_out_of_date,
    write_upload,
};
use alloc::string::String;
use alloc::vec::Vec;

type UploadedIds = (Option<u32>, Option<u32>);

impl Device {
    pub(in crate::signal) fn upkeep_due(&self, now: u64) -> Vec<Identity> {
        self.rings
            .iter()
            .filter(|(_, ring)| ring.checked + CHECK_PERIOD <= now)
            .map(|(identity, _)| *identity)
            .collect()
    }

    pub(in crate::signal) fn keys_out_of_date(&self, now: u64) -> bool {
        self.rings.get(&Identity::Aci).is_some_and(|ring| {
            is_out_of_date(ring.signed_keys.iter().map(|key| key.confirmed), now)
                || is_out_of_date(ring.last_resort.iter().map(|key| key.confirmed), now)
        })
    }

    pub(in crate::signal) fn renew_keys(
        &mut self,
        crypto: &impl Crypto,
        identity: Identity,
        (one_time_count, kyber_count): (u32, u32),
        now: u64,
    ) -> Option<(String, UploadedIds)> {
        let pair = *self.account.identity(identity)?;
        let ring = self.rings.get_mut(&identity)?;
        self.dirty.insert(File::Ring(identity));
        let needs_refill = |count: u32| !(MIN_COUNT..=MAX_COUNT).contains(&count);
        let one_time = if needs_refill(one_time_count) { Some(ring.top_up_one_time(crypto, now)?) } else { None };
        let kyber = if needs_refill(kyber_count) { Some(ring.top_up_kyber(crypto, &pair, now)?) } else { None };
        let is_fresh = |created: Option<u64>| created.is_some_and(|created| created + RENEW_AGE > now);
        let signed_key =
            match is_fresh(ring.signed_keys.iter().find(|key| key.confirmed.is_some()).map(|key| key.created)) {
                true => None,
                false => Some(SignedKey::new(crypto, &pair, Ring::take_id(&mut ring.next_signed), now)?),
            };
        let last_resort =
            match is_fresh(ring.last_resort.iter().find(|key| key.confirmed.is_some()).map(|key| key.created)) {
                true => None,
                false => Some(KyberKey::new(crypto, &pair, Ring::take_id(&mut ring.next_kyber), now)?),
            };
        let ids = (signed_key.as_ref().map(|key| key.id), last_resort.as_ref().map(|key| key.id));
        if let Some(key) = signed_key {
            ring.signed_keys.insert(0, key);
        }
        if let Some(key) = last_resort {
            ring.last_resort.insert(0, key);
        }
        if one_time.is_none() && kyber.is_none() && ids == (None, None) {
            ring.checked = now;
            return None;
        }
        let one_time = one_time.map_or(&[][..], |start| &ring.one_time[start..]);
        let kyber = kyber.map_or(&[][..], |start| &ring.kyber[start..]);
        let body = write_upload(
            one_time,
            kyber,
            ring.signed_keys.first().filter(|_| ids.0.is_some()),
            ring.last_resort.first().filter(|_| ids.1.is_some()),
        );
        Some((body, ids))
    }

    pub(in crate::signal) fn note_uploaded(&mut self, identity: Identity, ids: UploadedIds, now: u64) {
        let Some(ring) = self.rings.get_mut(&identity) else { return };
        ring.confirm(ids, now);
        drop_replaced(&mut ring.signed_keys, |key| key.created, now);
        drop_replaced(&mut ring.last_resort, |key| key.created, now);
        let last_resort = &ring.last_resort;
        ring.uses.retain(|(kyber, ..)| last_resort.iter().any(|key| key.id == *kyber));
        drop_unused(&mut ring.one_time, |key| key.created, now);
        drop_unused(&mut ring.kyber, |key| key.created, now);
        ring.checked = now;
        self.dirty.insert(File::Ring(identity));
    }
}

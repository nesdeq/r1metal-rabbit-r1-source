use super::{Device, File, Peer, ServiceId};
use crate::base::code::base64;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::signal::{Crypto, sealed, service};
use alloc::string::String;
use alloc::vec::Vec;

pub(super) const UNKNOWN: u32 = 0;
pub(super) const ENABLED: u32 = 1;
pub(super) const DISABLED: u32 = 2;
pub(super) const UNRESTRICTED: u32 = 3;
const CERTIFICATE_MARGIN: u64 = 60 * 60 * 1000;
const ACCESS_COUNTER: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];

pub(in crate::signal) fn access_key(crypto: &impl Crypto, profile_key: &[u8; 32]) -> Option<[u8; 16]> {
    crypto.ctr(profile_key, &ACCESS_COUNTER, &[0; 16])?.try_into().ok()
}

// What a sealed request goes under in place of the R1's name: an account's access key, or a group's token for its members.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::signal) enum Access {
    Key([u8; 16]),
    Token(Vec<u8>),
}

impl core::fmt::Debug for Access {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Access::Key(_) => "Key",
            Access::Token(_) => "Token",
        })
    }
}

impl Access {
    pub(in crate::signal) fn header(&self) -> (&'static str, String) {
        match self {
            Access::Key(key) => (service::ACCESS_KEY, base64(key)),
            Access::Token(token) => (service::GROUP_SEND_TOKEN, base64(token)),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(in crate::signal) struct Seal {
    pub(in crate::signal) access: Access,
    pub(in crate::signal) certificate: Vec<u8>,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Certificates {
    with_number: Option<Vec<u8>>,
    without_number: Option<Vec<u8>>,
}

impl Certificates {
    pub(super) fn write(&self) -> Vec<u8> {
        Writer::new()
            .maybe(self.with_number.as_deref().map(|bytes| move |w: Writer| w.bytes(1, bytes)))
            .maybe(self.without_number.as_deref().map(|bytes| move |w: Writer| w.bytes(2, bytes)))
            .finish()
    }

    pub(super) fn read(bytes: &[u8]) -> Option<Certificates> {
        let message = Message::parse(bytes)?;
        Some(Certificates {
            with_number: message.bytes(1).map(<[u8]>::to_vec),
            without_number: message.bytes(2).map(<[u8]>::to_vec),
        })
    }

    fn slot(&mut self, has_number: bool) -> &mut Option<Vec<u8>> {
        if has_number { &mut self.with_number } else { &mut self.without_number }
    }
}

impl Device {
    pub(in crate::signal) fn certificate(&self, has_number: bool, now: u64) -> Option<&[u8]> {
        let slot = if has_number { &self.certificates.with_number } else { &self.certificates.without_number };
        let certificate = slot.as_deref()?;
        let expiry = sealed::parse_expiry(certificate);
        expiry.is_some_and(|(expires, _)| expires.saturating_sub(CERTIFICATE_MARGIN) > now).then_some(certificate)
    }

    pub(in crate::signal) fn take_certificate(&mut self, has_number: bool, document: &str, now: u64) -> bool {
        let Some(certificate) =
            crate::base::json::text_at(document, &["certificate"]).and_then(crate::base::code::unbase64)
        else {
            return false;
        };
        let is_usable = sealed::parse_expiry(&certificate).is_some_and(|(expires, names_number)| {
            expires.saturating_sub(CERTIFICATE_MARGIN) > now && (has_number || !names_number)
        });
        if is_usable {
            *self.certificates.slot(has_number) = Some(certificate);
            self.dirty.insert(File::Certificates);
        }
        is_usable
    }

    pub(in crate::signal) fn seal_of(
        &self,
        crypto: &impl Crypto,
        recipient: &ServiceId,
        group: Option<&Peer>,
        now: u64,
    ) -> Option<Seal> {
        if self.is_own_id(recipient) {
            return None;
        }
        let mode = self.contacts.get(recipient).map_or(UNKNOWN, |contact| contact.access_mode);
        let certificate = self.certificate(self.account.is_number_shared, now)?.to_vec();
        let access = match mode {
            DISABLED => Access::Token(self.group_token(crypto, group?, &[*recipient], now)?),
            UNRESTRICTED => Access::Key([0; 16]),
            _ => Access::Key(self.contact_access_key(crypto, recipient).unwrap_or([0; 16])),
        };
        Some(Seal { access, certificate })
    }

    fn contact_access_key(&self, crypto: &impl Crypto, contact: &ServiceId) -> Option<[u8; 16]> {
        access_key(crypto, &self.contacts.get(contact)?.profile_key?)
    }

    pub(in crate::signal) fn member_access(&self, crypto: &impl Crypto, member: &ServiceId) -> Option<[u8; 16]> {
        match self.contacts.get(member).map_or(UNKNOWN, |contact| contact.access_mode) {
            DISABLED => None,
            UNRESTRICTED => Some([0; 16]),
            ENABLED => self.contact_access_key(crypto, member),
            _ => Some(self.contact_access_key(crypto, member).unwrap_or([0; 16])),
        }
    }

    pub(in crate::signal) fn wants_endorsements(&self, recipient: &ServiceId, group: &Peer, now: u64) -> bool {
        let is_refused = self.contacts.get(recipient).is_some_and(|contact| contact.access_mode == DISABLED);
        is_refused && !self.is_own_id(recipient) && self.can_refresh(group) && !self.has_endorsements(group, now)
    }

    pub(in crate::signal) fn note_sealed_delivered(&mut self, recipient: ServiceId) {
        let Some(contact) = self.contacts.get(&recipient) else {
            self.contact_mut(recipient).access_mode = UNRESTRICTED;
            return;
        };
        if contact.access_mode == UNKNOWN {
            let access_mode = if contact.profile_key.is_some() { ENABLED } else { UNRESTRICTED };
            self.contact_mut(recipient).access_mode = access_mode;
        }
    }

    pub(in crate::signal) fn note_sealed_refused(&mut self, recipient: ServiceId) {
        if self.contacts.get(&recipient).is_none_or(|contact| contact.access_mode != DISABLED) {
            self.contact_mut(recipient).access_mode = DISABLED;
        }
    }
}

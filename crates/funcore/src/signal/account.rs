use super::address::{Address, Peer, ServiceId, uuid_to_text};
use super::curve::Pair;
use super::{Crypto, hkdf};
use crate::base::proto::Message;
use crate::base::proto::Writer;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// An identity of the account: its ACI, or its PNI, its phone number's.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Identity {
    Aci,
    Pni,
}

impl Identity {
    pub(super) fn name(self) -> &'static str {
        match self {
            Identity::Aci => "aci",
            Identity::Pni => "pni",
        }
    }
}

/// The account and this device of it.
#[derive(Clone, PartialEq, Eq)]
pub struct Account {
    pub(super) aci: [u8; 16],
    pub(super) pni: Option<[u8; 16]>,
    pub(super) number: Option<String>,
    pub(super) device: u32,
    pub(super) password: String,
    pub(super) aci_identity: Pair,
    pub(super) pni_identity: Option<Pair>,
    pub(super) registration: u32,
    pub(super) pni_registration: u32,
    pub(super) profile_key: [u8; 32],
    pub(super) master_key: [u8; 32],
    pub(super) read_receipts: bool,
    pub(super) credential_salt: Option<Vec<u8>>,
    pub(super) is_number_shared: bool,
    pub(crate) listing: Listing,
}

// Whether a muted conversation tells mentions, replies and calls where neither it nor the account's record says (Signal-Desktop's `DEFAULT_NOTIFY_IF_MUTED`).
pub(super) const NOTIFY_IF_MUTED: (bool, bool, bool) = (true, true, false);
const MASTER_FROM_POOL: &[u8] = b"20240801_SIGNAL_SVR_MASTER_KEY";

pub(super) fn derive_master_key(crypto: &impl Crypto, pool: &str) -> Option<[u8; 32]> {
    hkdf(crypto, None, pool.as_bytes(), &[MASTER_FROM_POOL])
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Listing {
    pub(crate) pinned: Vec<Peer>,
    pub(super) keep_muted_archived: bool,
    pub(crate) count_muted: bool,
    pub(super) notify: (bool, bool, bool),
}

impl Default for Listing {
    fn default() -> Listing {
        Listing { pinned: Vec::new(), keep_muted_archived: false, count_muted: false, notify: NOTIFY_IF_MUTED }
    }
}

impl core::fmt::Debug for Account {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Account").field("device", &self.device).finish_non_exhaustive()
    }
}

impl Account {
    pub(crate) fn aci(&self) -> ServiceId {
        ServiceId::Aci(self.aci)
    }

    pub(super) fn address(&self, identity: Identity) -> Option<Address> {
        let service = match identity {
            Identity::Aci => ServiceId::Aci(self.aci),
            Identity::Pni => ServiceId::Pni(self.pni?),
        };
        Some(Address { service, device: self.device })
    }

    pub(super) fn identity(&self, identity: Identity) -> Option<&Pair> {
        match identity {
            Identity::Aci => Some(&self.aci_identity),
            Identity::Pni => self.pni_identity.as_ref(),
        }
    }

    pub(super) fn identity_of(&self, service: &ServiceId) -> Option<Identity> {
        match service {
            ServiceId::Aci(uuid) if *uuid == self.aci => Some(Identity::Aci),
            ServiceId::Pni(uuid) if Some(*uuid) == self.pni => Some(Identity::Pni),
            _ => None,
        }
    }

    pub(super) fn username(&self) -> String {
        format!("{}.{}", uuid_to_text(&self.aci), self.device)
    }

    pub(crate) fn write(&self) -> Vec<u8> {
        let pair = |pair: &Pair| Writer::new().bytes(1, &pair.private).bytes(2, &pair.public);
        let listing = &self.listing;
        let pinned = listing.pinned.iter().fold(Writer::new(), |writer, peer| writer.bytes(15, &peer.to_bytes()));
        pinned
            .bytes(1, &self.aci)
            .maybe(self.pni.map(|pni| move |w: Writer| w.bytes(2, &pni)))
            .maybe(self.number.as_deref().map(|number| move |w: Writer| w.text(3, number)))
            .varint(4, u64::from(self.device))
            .text(5, &self.password)
            .message(6, pair(&self.aci_identity))
            .maybe(self.pni_identity.as_ref().map(|identity| move |w: Writer| w.message(7, pair(identity))))
            .varint(8, u64::from(self.registration))
            .varint(9, u64::from(self.pni_registration))
            .bytes(10, &self.profile_key)
            .bytes(11, &self.master_key)
            .bool(12, self.read_receipts)
            .maybe(self.credential_salt.as_deref().map(|salt| move |w: Writer| w.bytes(13, salt)))
            .bool(14, self.is_number_shared)
            .bool(16, listing.keep_muted_archived)
            .bool(17, listing.count_muted)
            .bool(18, listing.notify.0)
            .bool(19, listing.notify.1)
            // Written only where calls are told, so that an account kept before reads and is written back alike.
            .maybe(listing.notify.2.then_some(|w: Writer| w.bool(20, true)))
            .finish()
    }

    pub(crate) fn read(bytes: &[u8]) -> Option<Account> {
        let message = Message::parse(bytes)?;
        let pair = |number| -> Option<Pair> {
            let pair = message.message(number)?;
            Some(Pair { private: pair.bytes(1)?.try_into().ok()?, public: pair.bytes(2)?.try_into().ok()? })
        };
        Some(Account {
            aci: message.bytes(1)?.try_into().ok()?,
            pni: message.bytes(2).and_then(|pni| pni.try_into().ok()),
            number: message.text(3).map(String::from),
            device: message.u32(4)?,
            password: String::from(message.text(5)?),
            aci_identity: pair(6)?,
            pni_identity: pair(7),
            registration: message.u32(8)?,
            pni_registration: message.u32(9)?,
            profile_key: message.bytes(10)?.try_into().ok()?,
            master_key: message.bytes(11)?.try_into().ok()?,
            read_receipts: message.bool(12).unwrap_or(false),
            credential_salt: message.bytes(13).map(<[u8]>::to_vec),
            is_number_shared: message.bool(14).unwrap_or(false),
            listing: Listing {
                pinned: message.repeated(15).filter_map(Peer::from_bytes).collect(),
                keep_muted_archived: message.bool(16).unwrap_or(false),
                count_muted: message.bool(17).unwrap_or(false),
                notify: (
                    message.bool(18).unwrap_or(NOTIFY_IF_MUTED.0),
                    message.bool(19).unwrap_or(NOTIFY_IF_MUTED.1),
                    message.bool(20).unwrap_or(NOTIFY_IF_MUTED.2),
                ),
            },
        })
    }
}

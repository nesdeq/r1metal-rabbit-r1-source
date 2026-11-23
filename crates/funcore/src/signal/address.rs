//! The ids of accounts, devices and conversations.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub(super) const GROUP_ID_BYTES: usize = 32;

/// An account's identifier: its ACI, or its PNI, the identifier of its phone number.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServiceId {
    Aci([u8; 16]),
    Pni([u8; 16]),
}

impl core::fmt::Debug for ServiceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            ServiceId::Aci(_) => "Aci",
            ServiceId::Pni(_) => "Pni",
        })
    }
}

impl ServiceId {
    pub(super) fn to_fixed(self) -> [u8; 17] {
        let (ServiceId::Aci(uuid) | ServiceId::Pni(uuid)) = self;
        let mut bytes = [0u8; 17];
        bytes[0] = u8::from(matches!(self, ServiceId::Pni(_)));
        bytes[1..].copy_from_slice(&uuid);
        bytes
    }

    pub(super) fn to_binary(self) -> Vec<u8> {
        match self {
            ServiceId::Aci(uuid) => uuid.to_vec(),
            ServiceId::Pni(_) => self.to_fixed().to_vec(),
        }
    }

    pub(super) fn from_binary(bytes: &[u8]) -> Option<ServiceId> {
        match bytes.len() {
            16 => Some(ServiceId::Aci(bytes.try_into().ok()?)),
            17 if bytes[0] == 0 => Some(ServiceId::Aci(bytes[1..].try_into().ok()?)),
            17 if bytes[0] == 1 => Some(ServiceId::Pni(bytes[1..].try_into().ok()?)),
            _ => None,
        }
    }

    pub(super) fn to_text(self) -> String {
        match self {
            ServiceId::Aci(uuid) => uuid_to_text(&uuid),
            ServiceId::Pni(uuid) => format!("PNI:{}", uuid_to_text(&uuid)),
        }
    }

    pub(super) fn from_text(text: &str) -> Option<ServiceId> {
        match text.strip_prefix("PNI:") {
            Some(uuid) => Some(ServiceId::Pni(parse_uuid(uuid)?)),
            None => Some(ServiceId::Aci(parse_uuid(text)?)),
        }
    }
}

/// A conversation's other side: a contact, or a group by Signal's id of it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Peer {
    Contact(ServiceId),
    Group([u8; GROUP_ID_BYTES]),
}

impl core::fmt::Debug for Peer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Peer::Contact(contact) => core::fmt::Debug::fmt(contact, f),
            Peer::Group(_) => f.write_str("Group"),
        }
    }
}

impl Peer {
    pub(crate) const fn contact(self) -> Option<ServiceId> {
        match self {
            Peer::Contact(contact) => Some(contact),
            Peer::Group(_) => None,
        }
    }

    pub(super) fn to_bytes(self) -> Vec<u8> {
        match self {
            Peer::Contact(contact) => contact.to_binary(),
            Peer::Group(id) => id.to_vec(),
        }
    }

    pub(super) fn from_bytes(bytes: &[u8]) -> Option<Peer> {
        match <[u8; GROUP_ID_BYTES]>::try_from(bytes) {
            Ok(id) => Some(Peer::Group(id)),
            Err(_) => ServiceId::from_binary(bytes).map(Peer::Contact),
        }
    }
}

pub(super) fn uuid_to_text(uuid: &[u8; 16]) -> String {
    let hex = crate::base::code::hex(uuid);
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

pub(super) fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    if !text.split('-').map(str::len).eq([8, 4, 4, 4, 12]) {
        return None;
    }
    crate::base::code::unhex(&text.replace('-', ""))?.try_into().ok()
}

/// A device of an account.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Address {
    pub(super) service: ServiceId,
    pub(super) device: u32,
}

impl Address {
    pub(crate) const fn service(&self) -> ServiceId {
        self.service
    }

    pub(crate) const fn device(&self) -> u32 {
        self.device
    }

    pub(super) fn pair(sender: &Address, recipient: &Address) -> Option<Vec<u8>> {
        let (sender_device, recipient_device) =
            (u8::try_from(sender.device).ok()?, u8::try_from(recipient.device).ok()?);
        Some(
            [&sender.service.to_fixed()[..], &[sender_device], &recipient.service.to_fixed(), &[recipient_device]]
                .concat(),
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const fn address(service: ServiceId, device: u32) -> Address {
        Address { service, device }
    }

    #[test]
    fn id_round_trips() {
        let uuid = *b"\x9d\x0f\x6a\x1c\x4e\x2b\x4f\x10\x8a\x55\x0b\x1e\x7c\x3d\x2a\x91";
        let aci = ServiceId::Aci(uuid);
        let pni = ServiceId::Pni(uuid);
        assert_eq!(aci.to_text(), "9d0f6a1c-4e2b-4f10-8a55-0b1e7c3d2a91");
        assert_eq!(pni.to_text(), "PNI:9d0f6a1c-4e2b-4f10-8a55-0b1e7c3d2a91");
        assert_eq!(ServiceId::from_text(&aci.to_text()), Some(aci));
        assert_eq!(ServiceId::from_text(&pni.to_text().to_uppercase()), Some(pni), "in capitals too");
        assert_eq!(aci.to_binary().len(), 16);
        assert_eq!(pni.to_binary()[0], 1);
        assert_eq!(ServiceId::from_binary(&aci.to_binary()), Some(aci));
        assert_eq!(ServiceId::from_binary(&pni.to_binary()), Some(pni));
        assert_eq!(ServiceId::from_binary(&[0; 15]), None);
        let pair = Address::pair(&Address { service: aci, device: 1 }, &Address { service: pni, device: 3 }).unwrap();
        assert_eq!(pair.len(), 36);
        assert_eq!((pair[17], pair[35]), (1, 3));
    }
}

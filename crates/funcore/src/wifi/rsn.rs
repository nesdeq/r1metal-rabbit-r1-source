//! The RSN element and its extension: a beacon's offer, the firmware's choice, and the elements it answers with.

use crate::base::bytes::le16;
use crate::wifi::elements;
use alloc::vec::Vec;

pub(super) const ELEMENT: u8 = 48;
pub(super) const EXTENSION: u8 = 244;
const SAE_H2E: u8 = 1 << 5;
const VERSION: u16 = 1;
const OUI: [u8; 3] = [0x00, 0x0F, 0xAC];
const TKIP: u8 = 2;
const CCMP: u8 = 4;
const BIP: u8 = 6;
const PSK: u8 = 2;
const PSK_SHA256: u8 = 6;
const SAE: u8 = 8;
const MFP_REQUIRED: u16 = 1 << 6;
const MFP_CAPABLE: u16 = 1 << 7;
const SUITE: usize = 4;
const GROUP_AT: usize = 2;
const PMKID: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Akm {
    Psk,
    PskSha256,
    Sae { is_hashed: bool },
}

impl Akm {
    const fn kind(self) -> u8 {
        match self {
            Akm::Psk => PSK,
            Akm::PskSha256 => PSK_SHA256,
            Akm::Sae { .. } => SAE,
        }
    }

    pub(super) const fn suite(self) -> u32 {
        selector(self.kind())
    }
}

/// The group cipher.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Tkip,
    Ccmp,
}

impl Group {
    const fn kind(self) -> u8 {
        match self {
            Group::Tkip => TKIP,
            Group::Ccmp => CCMP,
        }
    }

    pub const fn suite(self) -> u32 {
        selector(self.kind())
    }

    pub(super) const fn key_length(self) -> usize {
        match self {
            Group::Tkip => 32,
            Group::Ccmp => 16,
        }
    }
}

const fn selector(kind: u8) -> u32 {
    u32::from_be_bytes([OUI[0], OUI[1], OUI[2], kind])
}

pub(super) const PAIRWISE: u32 = selector(CCMP);
pub const MANAGEMENT: u32 = selector(BIP);

/// The suites the firmware takes of a network's offer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Choice {
    pub(super) group: Group,
    pub(super) akm: Akm,
    /// True if management frames are protected.
    pub is_protected: bool,
    pub(super) is_required: bool,
}

fn read_suites(body: &[u8], at: usize) -> Option<(impl Iterator<Item = &[u8; SUITE]>, usize)> {
    let count = usize::from(le16(body, at)?);
    let suites = body.get(at + 2..at + 2 + SUITE * count)?;
    Some((suites.as_chunks::<SUITE>().0.iter(), at + 2 + SUITE * count))
}

pub(super) fn is_suite(suite: &[u8], kind: u8) -> bool {
    suite[..3] == OUI && suite[3] == kind
}

pub(super) fn find_element(bytes: &[u8], number: u8) -> Option<&[u8]> {
    elements(bytes).find(|element| element[0] == number)
}

pub(super) fn find_rsn(bytes: &[u8]) -> Option<Vec<u8>> {
    let element = find_element(bytes, ELEMENT)?;
    Some([element, find_element(bytes, EXTENSION).unwrap_or_default()].concat())
}

/// The suites to take of a beacon's elements; none without CCMP, a known group cipher, a password AKM and an MFP this firmware speaks.
pub fn choose(elements: &[u8]) -> Option<Choice> {
    let body = &find_element(elements, ELEMENT)?[2..];
    let is_hashed = find_element(elements, EXTENSION)
        .and_then(|extension| extension.get(2))
        .is_some_and(|capabilities| capabilities & SAE_H2E != 0);
    if le16(body, 0)? != VERSION {
        return None;
    }
    let group = body.get(GROUP_AT..GROUP_AT + SUITE)?;
    let group = if is_suite(group, CCMP) {
        Group::Ccmp
    } else if is_suite(group, TKIP) {
        Group::Tkip
    } else {
        return None;
    };
    let (mut pairwise, akm_at) = read_suites(body, GROUP_AT + SUITE)?;
    if !pairwise.any(|suite| is_suite(suite, CCMP)) {
        return None;
    }
    let (akms, capabilities_at) = read_suites(body, akm_at)?;
    let akms: Vec<&[u8; SUITE]> = akms.collect();
    let has_akm = |kind: u8| akms.iter().any(|&suite| is_suite(suite, kind));
    // The capabilities and all after them may be left out; a network that leaves them out protects nothing.
    let capabilities = le16(body, capabilities_at).unwrap_or(0);
    let (is_capable, is_required) = (capabilities & MFP_CAPABLE != 0, capabilities & MFP_REQUIRED != 0);
    // After the capabilities come a counted list of PMKIDs and then the cipher for management frames to all, BIP if it is left out.
    let pmkids = le16(body, capabilities_at + 2).map_or(0, usize::from);
    let management = body.get(capabilities_at + 4 + PMKID * pmkids..).and_then(|rest| rest.get(..SUITE));
    let is_bip = management.is_none_or(|suite| is_suite(suite, BIP));
    if is_required && !(is_capable && is_bip) {
        return None;
    }
    let is_protected = is_capable && is_bip;
    let akm = match (has_akm(SAE), has_akm(PSK), has_akm(PSK_SHA256)) {
        // SAE wherever the network offers it and frames are protected, which WPA3 has go together.
        (true, _, _) if is_protected => Akm::Sae { is_hashed },
        // SHA-256 where frames are protected and the network offers it.
        (_, _, true) if is_protected => Akm::PskSha256,
        // SHA-1 wherever else the network offers it.
        (_, true, _) => Akm::Psk,
        // SHA-256 without that protection where it is the one proof left: its keys and signatures do not hang on the protection.
        (_, false, true) => Akm::PskSha256,
        (_, false, false) => return None,
    };
    Some(Choice { group, akm, is_protected, is_required })
}

impl Choice {
    fn element(&self) -> Vec<u8> {
        let suite = |kind: u8| selector(kind).to_be_bytes();
        let mut body = Vec::new();
        body.extend(VERSION.to_le_bytes());
        body.extend(suite(self.group.kind()));
        body.extend(1u16.to_le_bytes());
        body.extend(suite(CCMP));
        body.extend(1u16.to_le_bytes());
        body.extend(suite(self.akm.kind()));
        let capabilities =
            if self.is_protected { MFP_CAPABLE } else { 0 } | if self.is_required { MFP_REQUIRED } else { 0 };
        body.extend(capabilities.to_le_bytes());
        if self.is_protected {
            // No PMKIDs, and the cipher for management frames to all.
            body.extend(0u16.to_le_bytes());
            body.extend(suite(BIP));
        }
        let mut element = alloc::vec![ELEMENT, body.len() as u8];
        element.extend(body);
        element
    }

    fn extension(&self) -> Vec<u8> {
        if self.akm == (Akm::Sae { is_hashed: true }) { alloc::vec![EXTENSION, 1, SAE_H2E] } else { Vec::new() }
    }

    pub(super) fn elements(&self) -> Vec<u8> {
        [self.element(), self.extension()].concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn element(body: &[u8]) -> Vec<u8> {
        [&[ELEMENT, body.len() as u8][..], body].concat()
    }

    fn offer(group: u8, pairwise: &[u8], akms: &[u8], rest: &[u8]) -> Vec<u8> {
        let mut body = vec![1, 0, 0x00, 0x0F, 0xAC, group, pairwise.len() as u8, 0];
        pairwise.iter().for_each(|kind| body.extend([0x00, 0x0F, 0xAC, *kind]));
        body.extend([akms.len() as u8, 0]);
        akms.iter().for_each(|kind| body.extend([0x00, 0x0F, 0xAC, *kind]));
        body.extend(rest);
        element(&body)
    }

    fn ccmp(akms: &[u8], rest: &[u8]) -> Vec<u8> {
        offer(CCMP, &[CCMP], akms, rest)
    }

    #[test]
    fn plain_wpa2() {
        let choice = choose(&ccmp(&[PSK], &[0, 0])).unwrap();
        assert_eq!(choice, Choice { group: Group::Ccmp, akm: Akm::Psk, is_protected: false, is_required: false });
        assert_eq!(
            choice.element(),
            [48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 2, 0, 0],
            "the element says the same back: one cipher, one proof, nothing protected"
        );
        assert_eq!(choose(&ccmp(&[PSK], &[])), Some(choice), "a network may leave its capabilities out");
        assert_eq!(
            (Akm::Psk.suite(), Group::Ccmp.suite(), PAIRWISE, MANAGEMENT),
            (0x000F_AC02, 0x000F_AC04, 0x000F_AC04, 0x000F_AC06)
        );
    }

    #[test]
    fn tkip_group_with_ccmp() {
        let choice = choose(&offer(TKIP, &[TKIP, CCMP], &[PSK], &[0, 0])).unwrap();
        assert_eq!((choice.group, choice.akm), (Group::Tkip, Akm::Psk));
        assert_eq!(choice.element()[2..8], [1, 0, 0x00, 0x0F, 0xAC, 2], "the group cipher is the network's to choose");
        assert_eq!(choose(&offer(TKIP, &[TKIP], &[PSK], &[0, 0])), None, "TKIP between the two is not spoken");
        assert_eq!(choose(&offer(1, &[CCMP], &[PSK], &[0, 0])), None, "nor a group cipher of before TKIP");
    }

    #[test]
    fn management_frame_protection() {
        let capable = choose(&ccmp(&[PSK, PSK_SHA256], &[0x80, 0])).unwrap();
        assert_eq!(capable, Choice { group: Group::Ccmp, akm: Akm::PskSha256, is_protected: true, is_required: false });
        assert_eq!(
            capable.element()[20..],
            [0x80, 0, 0, 0, 0x00, 0x0F, 0xAC, 6],
            "capable, no key names, and BIP for what goes to all"
        );
        let required = choose(&ccmp(&[PSK_SHA256], &[0xC0, 0, 0, 0, 0x00, 0x0F, 0xAC, 6])).unwrap();
        assert_eq!(
            (required.akm, required.is_protected, required.is_required, required.element()[20]),
            (Akm::PskSha256, true, true, 0xC0)
        );
        assert_eq!(
            choose(&ccmp(&[PSK], &[0x80, 0])).map(|choice| choice.akm),
            Some(Akm::Psk),
            "protection without the newer proof keeps the older"
        );
        let other = [0x80, 0, 0, 0, 0x00, 0x0F, 0xAC, 12];
        assert_eq!(
            choose(&ccmp(&[PSK], &other)).map(|choice| choice.is_protected),
            Some(false),
            "a protection this firmware does not speak is left alone"
        );
        assert_eq!(
            choose(&ccmp(&[PSK], &[0xC0, 0, 0, 0, 0x00, 0x0F, 0xAC, 12])),
            None,
            "unless the network insists on it"
        );
        assert_eq!(choose(&ccmp(&[PSK], &[0x40, 0])), None, "a network that insists without being able to is refused");
    }

    #[test]
    fn sha256_without_protection() {
        let sha256 = choose(&ccmp(&[PSK_SHA256], &[0, 0])).unwrap();
        assert_eq!(sha256, Choice { group: Group::Ccmp, akm: Akm::PskSha256, is_protected: false, is_required: false });
        assert_eq!(
            sha256.element()[14..],
            [1, 0, 0x00, 0x0F, 0xAC, 6, 0, 0],
            "the element names the proof, protects nothing and names no cipher for management frames"
        );
        assert_eq!(
            choose(&ccmp(&[PSK_SHA256], &[])),
            Some(sha256),
            "the same where the network leaves its capabilities out"
        );
        assert_eq!(
            choose(&ccmp(&[PSK_SHA256], &[0x80, 0, 0, 0, 0x00, 0x0F, 0xAC, 12])),
            Some(sha256),
            "and where it protects management frames in a way this firmware does not speak"
        );
        assert_eq!(
            choose(&ccmp(&[SAE, PSK_SHA256], &[0, 0])),
            Some(sha256),
            "SAE without that protection is passed over for it"
        );
        assert_eq!(
            choose(&ccmp(&[PSK, PSK_SHA256], &[0, 0])).map(|choice| choice.akm),
            Some(Akm::Psk),
            "beside SHA-1 and without the protection, SHA-1 is taken"
        );
    }

    #[test]
    fn sae_with_protection() {
        let wpa3 = choose(&ccmp(&[SAE], &[0xC0, 0])).unwrap();
        assert_eq!(
            wpa3,
            Choice { group: Group::Ccmp, akm: Akm::Sae { is_hashed: false }, is_protected: true, is_required: true },
            "WPA3 alone"
        );
        assert_eq!(
            (wpa3.element()[14..20].to_vec(), wpa3.element()[20], wpa3.extension()),
            (vec![1, 0, 0x00, 0x0F, 0xAC, 8], 0xC0, vec![]),
            "the element names SAE, and nothing is said of hashing where the network says nothing of it"
        );
        assert_eq!(Akm::Sae { is_hashed: true }.suite(), 0x000F_AC08);
        let both = ccmp(&[PSK, SAE], &[0x80, 0]);
        assert_eq!(
            choose(&both).map(|choice| (choice.akm, choice.is_required)),
            Some((Akm::Sae { is_hashed: false }, false)),
            "a network of both generations is joined by the newer"
        );
        let hashing = choose(&[&both[..], &[EXTENSION, 1, 0x20]].concat()).unwrap();
        assert_eq!(
            (hashing.akm, hashing.extension()),
            (Akm::Sae { is_hashed: true }, vec![244, 1, 0x20]),
            "a network that hashes to the password's point is answered in kind"
        );
        assert_eq!(
            choose(&[&both[..], &[EXTENSION, 2, 0x11, 0x20]].concat()).map(|choice| choice.akm),
            Some(Akm::Sae { is_hashed: false }),
            "the bit counts in the first byte only"
        );
        assert_eq!(
            choose(&[&both[..], &[EXTENSION, 0]].concat()).map(|choice| choice.akm),
            Some(Akm::Sae { is_hashed: false }),
            "an extension that says nothing"
        );
        let plain = choose(&[&ccmp(&[PSK], &[0x80, 0])[..], &[EXTENSION, 1, 0x20]].concat()).unwrap();
        assert_eq!(
            (plain.akm, plain.extension()),
            (Akm::Psk, vec![]),
            "without SAE an extension changes nothing and none is sent"
        );
        assert_eq!(
            choose(&ccmp(&[PSK, SAE], &[0, 0])).map(|choice| choice.akm),
            Some(Akm::Psk),
            "SAE without protected management frames is no WPA3, and the older proof is taken"
        );
        assert_eq!(choose(&ccmp(&[SAE], &[0, 0])), None, "and none where there is no older one");
    }

    #[test]
    fn refused_offers() {
        assert_eq!(choose(&ccmp(&[1], &[0, 0])), None, "a login");
        let offer = ccmp(&[PSK], &[0, 0]);
        for length in 0..offer.len() {
            assert_eq!(choose(&offer[..length]), None, "an element that lacks its end, {length} bytes");
        }
        for length in 0..offer.len() - 4 {
            assert_eq!(choose(&element(&offer[2..2 + length])), None, "a body of {length} bytes");
        }
        assert_eq!(
            choose(&[&[EXTENSION, 1, 0x20][..], &offer].concat()).map(|choice| choice.akm),
            Some(Akm::Psk),
            "the elements are found in any order"
        );
        assert_eq!(choose(&[EXTENSION, 1, 0x20]), None, "an extension alone offers nothing");
        assert_eq!(
            choose(&element(&[2, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 2])),
            None,
            "another version"
        );
        assert_eq!(
            choose(&element(&[1, 0, 0x00, 0x50, 0xF2, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 2])),
            None,
            "a vendor's group cipher"
        );
    }

    #[test]
    fn find_element_and_rsn() {
        let elements = [0, 2, b'h', b'i', 48, 3, 1, 2, 3, 244, 1, 0x20, 48, 1, 9];
        assert_eq!(
            (find_element(&elements, 48), find_element(&elements, 244), find_element(&elements, 0)),
            (Some(&[48, 3, 1, 2, 3][..]), Some(&[244, 1, 0x20][..]), Some(&[0, 2, b'h', b'i'][..])),
            "the first of its number, with its number and length"
        );
        assert_eq!(
            (
                find_element(&elements, 7),
                find_element(&elements[..8], 48),
                find_element(&[], 48),
                find_element(&[48], 48)
            ),
            (None, None, None, None),
            "none that is not there or lacks its end"
        );
        assert_eq!(
            (find_rsn(&elements), find_rsn(&elements[..9]), find_rsn(&elements[9..12])),
            (Some(vec![48, 3, 1, 2, 3, 244, 1, 0x20]), Some(vec![48, 3, 1, 2, 3]), None),
            "the element with its extension after it, the element alone, and an extension of no element"
        );
    }
}

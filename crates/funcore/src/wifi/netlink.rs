//! nl80211 over generic netlink: messages, attributes and the requests a station sends.

use crate::base::bytes::le16;
use crate::base::bytes::le32;
use crate::wifi::rsn;
use crate::wifi::wpa::Step;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const ERROR: u16 = 2;
const DONE: u16 = 3;
pub const CONTROL: u16 = 16;
const REQUEST: u16 = 1;
pub const ACKNOWLEDGE: u16 = 4;
pub const DUMP: u16 = 0x300;
pub const GET_FAMILY: u8 = 3;
pub const CONTROL_VERSION: u8 = 1;
const FAMILY_ID: u16 = 1;
pub const FAMILY_NAME: u16 = 2;
const FAMILY_GROUPS: u16 = 7;
const GROUP_NAME: u16 = 1;
const GROUP_ID: u16 = 2;
/// The family of the wireless stack.
pub const WIRELESS: &[u8] = b"nl80211\0";
const SCAN_GROUP: &[u8] = b"scan";
const LINK_GROUP: &[u8] = b"mlme";
pub const CONNECT: u8 = 46;
pub const DISCONNECT: u8 = 48;
pub(super) const NEW_KEY: u8 = 11;
const SET_KEY: u8 = 10;
pub const GET_STATION: u8 = 17;
pub const REGISTER_FRAME: u8 = 58;
pub const FRAME: u8 = 59;
pub const EXTERNAL_AUTH: u8 = 127;
pub const GET_WIPHY: u8 = 1;
const NEW_WIPHY: u8 = 3;
const MAC: u16 = 6;
const KEY_DATA: u16 = 7;
const KEY_INDEX: u16 = 8;
const KEY_CIPHER: u16 = 9;
const KEY_SEQUENCE: u16 = 10;
const STA_INFO: u16 = 21;
const WIPHY_FREQ: u16 = 38;
const IE: u16 = 42;
const SSID: u16 = 52;
const AUTH_TYPE: u16 = 53;
pub(super) const REASON_CODE: u16 = 54;
const KEY_TYPE: u16 = 55;
pub(super) const TIMED_OUT: u16 = 65;
const USE_MFP: u16 = 66;
const CONTROL_PORT: u16 = 68;
const PRIVACY: u16 = 70;
pub(super) const STATUS_CODE: u16 = 72;
const CIPHER_SUITES_PAIRWISE: u16 = 73;
const CIPHER_SUITE_GROUP: u16 = 74;
const WPA_VERSIONS: u16 = 75;
const AKM_SUITES: u16 = 76;
const KEY_DEFAULT: u16 = 11;
const KEY_DEFAULT_TYPES: u16 = 110;
const ATTR_FRAME: u16 = 51;
pub(super) const REQ_IE: u16 = 77;
const FRAME_MATCH: u16 = 91;
const FRAME_TYPE: u16 = 101;
const DONT_WAIT_FOR_ACK: u16 = 142;
const SOCKET_OWNER: u16 = 204;
pub(super) const BSSID: u16 = 245;
pub(super) const EXTERNAL_AUTH_ACTION: u16 = 260;
const EXTERNAL_AUTH_SUPPORT: u16 = 261;
const OPEN_SYSTEM: u32 = 0;
const SAE: u32 = 4;
const EXTERNAL_AUTH_START: u32 = 0;
const WPA2: u32 = 2;
const MFP_REQUIRED: u32 = 1;
const KEYTYPE_GROUP: u32 = 0;
const KEYTYPE_PAIRWISE: u32 = 1;
const DEFAULT_UNICAST: u16 = 1;
const STA_INFO_SIGNAL: u16 = 7;
const STA_INFO_TX_BITRATE: u16 = 8;
const RATE_INFO_BITRATE: u16 = 1;
const SPEED_UNITS_PER_MBIT: u32 = 10;
const DEAUTH_LEAVING: u16 = 3;
pub(super) const SUCCESS: u16 = 0;
const FAILURE: u16 = 1;
const IFINDEX: u16 = 3;
/// The attribute that has `GET_WIPHY`'s list come in parts, without which it is cut short.
pub const SPLIT_WIPHY_DUMP: u16 = 174;
const WIPHY_BANDS: u16 = 22;
const BAND_ATTR_FREQS: u16 = 1;
const FREQUENCY_ATTR_FREQ: u16 = 1;
const FREQUENCY_ATTR_DISABLED: u16 = 2;
const FREQUENCY_ATTR_NO_IR: u16 = 3;

const HEAD: usize = 16;
const FAMILY_HEAD: usize = 4;
const ALIGNMENT: usize = 4;
const KIND_BITS: u16 = 0x3FFF;

fn to_aligned(length: usize) -> usize {
    length.next_multiple_of(ALIGNMENT)
}

pub fn add_attribute(buffer: &mut Vec<u8>, kind: u16, payload: &[u8]) {
    buffer.extend(((ALIGNMENT + payload.len()) as u16).to_le_bytes());
    buffer.extend(kind.to_le_bytes());
    buffer.extend(payload);
    buffer.resize(to_aligned(buffer.len()), 0);
}

/// The attributes of a request about an interface: the interface, then each kind with its payload.
pub fn interface_attributes(interface: u32, attributes: &[(u16, &[u8])]) -> Vec<u8> {
    let mut buffer = Vec::new();
    add_attribute(&mut buffer, IFINDEX, &interface.to_le_bytes());
    for (kind, payload) in attributes {
        add_attribute(&mut buffer, *kind, payload);
    }
    buffer
}

/// A request to a family: its type, flags, a number its answers carry, the family's command and version, and attributes.
pub fn request(family: u16, flags: u16, sequence: u32, command: u8, version: u8, attributes: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(HEAD + FAMILY_HEAD + attributes.len());
    message.extend(((HEAD + FAMILY_HEAD + attributes.len()) as u32).to_le_bytes());
    message.extend(family.to_le_bytes());
    message.extend((REQUEST | flags).to_le_bytes());
    message.extend(sequence.to_le_bytes());
    message.extend(0u32.to_le_bytes());
    message.extend([command, version, 0, 0]);
    message.extend(attributes);
    message
}

pub(super) fn attributes(mut bytes: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    core::iter::from_fn(move || {
        let length = usize::from(le16(bytes, 0)?);
        let kind = le16(bytes, 2)? & KIND_BITS;
        let payload = bytes.get(ALIGNMENT..length)?;
        bytes = bytes.get(to_aligned(length)..).unwrap_or(&[]);
        Some((kind, payload))
    })
}

pub(super) fn find_attribute(bytes: &[u8], kind: u16) -> Option<&[u8]> {
    attributes(bytes).find(|(other, _)| *other == kind).map(|(_, payload)| payload)
}

pub(super) fn nest(kind: u16, inner: &[u8]) -> Vec<u8> {
    let mut buffer = Vec::new();
    add_attribute(&mut buffer, kind, inner);
    buffer
}

/// A netlink message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Message<'a> {
    kind: u16,
    sequence: u32,
    body: &'a [u8],
}

impl<'a> Message<'a> {
    pub(super) fn ack_status(&self) -> Option<i32> {
        (self.kind == ERROR).then(|| le32(self.body, 0).map(|code| (code as i32).saturating_neg())).flatten()
    }

    /// The error number of a request the kernel refused; none for an acknowledgement or any other message.
    pub fn error(&self) -> Option<i32> {
        self.ack_status().filter(|code| *code != 0)
    }

    /// True for the kernel's acknowledgement of the request of that number.
    pub fn is_ack_of(&self, sequence: u32) -> bool {
        self.sequence == sequence && self.ack_status() == Some(0)
    }

    /// True for the message that ends the answers to the dump request of that number.
    pub const fn is_end_of(&self, sequence: u32) -> bool {
        self.sequence == sequence && self.kind == DONE
    }

    /// True for a message that answers the request of that number.
    pub const fn is_answer_of(&self, sequence: u32) -> bool {
        self.sequence == sequence
    }

    /// The command of a family's message and its attributes; none for the kernel's own messages.
    pub fn command(&self) -> Option<(u8, &'a [u8])> {
        (self.kind >= CONTROL).then(|| Some((*self.body.first()?, self.body.get(FAMILY_HEAD..)?))).flatten()
    }

    /// The command and attributes of a family's message that answers the request of that number.
    pub fn command_of(&self, sequence: u32) -> Option<(u8, &'a [u8])> {
        self.command().filter(|_| self.is_answer_of(sequence))
    }
}

/// The messages of one datagram; one that does not fit ends them.
pub fn messages(mut datagram: &[u8]) -> impl Iterator<Item = Message<'_>> {
    core::iter::from_fn(move || {
        let length = le32(datagram, 0)? as usize;
        let (kind, sequence) = (le16(datagram, 4)?, le32(datagram, 8)?);
        let body = datagram.get(HEAD..length)?;
        datagram = datagram.get(to_aligned(length)..).unwrap_or(&[]);
        Some(Message { kind, sequence, body })
    })
}

/// The wireless family on this system: its message type and the groups that tell of scans and of links.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Family {
    pub id: u16,
    pub scans: u32,
    pub links: u32,
}

/// Reads the answer to `GET_FAMILY`.
pub fn parse_family(answer: &[u8]) -> Option<Family> {
    let id = le16(find_attribute(answer, FAMILY_ID)?, 0)?;
    let group = |target: &[u8]| {
        attributes(find_attribute(answer, FAMILY_GROUPS)?).find_map(|(_, group)| {
            let name = find_attribute(group, GROUP_NAME)?;
            let id = find_attribute(group, GROUP_ID)?;
            (name.strip_suffix(&[0]).unwrap_or(name) == target).then(|| le32(id, 0)).flatten()
        })
    };
    Some(Family { id, scans: group(SCAN_GROUP)?, links: group(LINK_GROUP)? })
}

/// A connect request: SSID, BSSID, channel in MHz, RSN element and extension, and the cipher and AKM suites.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConnectRequest<'a> {
    pub(super) ssid: &'a [u8],
    pub(super) bssid: [u8; 6],
    pub(super) frequency: u32,
    pub(super) ie: &'a [u8],
    pub(super) group: u32,
    pub(super) pairwise: u32,
    pub(super) akm: u32,
    pub(super) mfp_required: bool,
    pub(super) is_sae: bool,
}

impl ConnectRequest<'_> {
    /// The request's attributes on an interface; the asking socket owns the link, and an element makes the handshake the station's own.
    pub fn to_attributes(&self, interface: u32) -> Vec<u8> {
        let mut buffer = interface_attributes(interface, &[]);
        add_attribute(&mut buffer, SSID, self.ssid);
        add_attribute(&mut buffer, MAC, &self.bssid);
        add_attribute(&mut buffer, WIPHY_FREQ, &self.frequency.to_le_bytes());
        add_attribute(&mut buffer, AUTH_TYPE, &(if self.is_sae { SAE } else { OPEN_SYSTEM }).to_le_bytes());
        add_attribute(&mut buffer, SOCKET_OWNER, &[]);
        if self.is_sae {
            add_attribute(&mut buffer, EXTERNAL_AUTH_SUPPORT, &[]);
        }
        if !self.ie.is_empty() {
            add_attribute(&mut buffer, IE, self.ie);
            add_attribute(&mut buffer, PRIVACY, &[]);
            add_attribute(&mut buffer, WPA_VERSIONS, &WPA2.to_le_bytes());
            add_attribute(&mut buffer, CIPHER_SUITES_PAIRWISE, &self.pairwise.to_le_bytes());
            add_attribute(&mut buffer, CIPHER_SUITE_GROUP, &self.group.to_le_bytes());
            add_attribute(&mut buffer, AKM_SUITES, &self.akm.to_le_bytes());
            add_attribute(&mut buffer, CONTROL_PORT, &[]);
            if self.mfp_required {
                add_attribute(&mut buffer, USE_MFP, &MFP_REQUIRED.to_le_bytes());
            }
        }
        buffer
    }
}

pub(super) fn parse_connect_status(message: &[u8]) -> Option<u16> {
    if find_attribute(message, TIMED_OUT).is_some() {
        return None;
    }
    le16(find_attribute(message, STATUS_CODE)?, 0)
}

pub(super) fn parse_req_ie(message: &[u8]) -> &[u8] {
    find_attribute(message, REQ_IE).unwrap_or(&[])
}

pub(super) fn parse_reason(message: &[u8]) -> u16 {
    find_attribute(message, REASON_CODE).and_then(|reason| le16(reason, 0)).unwrap_or(0)
}

pub(super) fn register_frame_attributes(interface: u32, kind: u16, prefix: &[u8]) -> Vec<u8> {
    interface_attributes(interface, &[(FRAME_TYPE, &kind.to_le_bytes()), (FRAME_MATCH, prefix)])
}

/// The attributes of the request to send a management frame, head included, with no acknowledgement.
pub fn frame_attributes(interface: u32, frame: &[u8]) -> Vec<u8> {
    interface_attributes(interface, &[(DONT_WAIT_FOR_ACK, &[]), (ATTR_FRAME, frame)])
}

pub(super) fn parse_frame(message: &[u8]) -> Option<&[u8]> {
    find_attribute(message, ATTR_FRAME)
}

pub(super) fn parse_external_auth(message: &[u8]) -> Option<[u8; 6]> {
    let action = find_attribute(message, EXTERNAL_AUTH_ACTION).and_then(|action| le32(action, 0));
    find_attribute(message, BSSID).filter(|_| action == Some(EXTERNAL_AUTH_START))?.try_into().ok()
}

/// The attributes of the outcome of SAE with an access point of a network.
pub fn external_auth_attributes(interface: u32, name: &[u8], access: &[u8; 6], is_proved: bool) -> Vec<u8> {
    let status = if is_proved { SUCCESS } else { FAILURE };
    interface_attributes(interface, &[(SSID, name), (BSSID, access), (STATUS_CODE, &status.to_le_bytes())])
}

/// The attributes of the request to leave the network an interface has joined.
pub fn disconnect_attributes(interface: u32) -> Vec<u8> {
    interface_attributes(interface, &[(REASON_CODE, &DEAUTH_LEAVING.to_le_bytes())])
}

fn new_key_attributes(
    interface: u32,
    index: u8,
    cipher: u32,
    key: &[u8],
    sequence: Option<&[u8]>,
    peer: Option<&[u8; 6]>,
) -> Vec<u8> {
    let mut buffer = interface_attributes(interface, &[]);
    add_attribute(&mut buffer, KEY_DATA, key);
    add_attribute(&mut buffer, KEY_CIPHER, &cipher.to_le_bytes());
    add_attribute(&mut buffer, KEY_INDEX, &[index]);
    if let Some(sequence) = sequence {
        add_attribute(&mut buffer, KEY_SEQUENCE, sequence);
    }
    if let Some(peer) = peer {
        add_attribute(&mut buffer, MAC, peer);
    }
    add_attribute(
        &mut buffer,
        KEY_TYPE,
        &(if peer.is_some() { KEYTYPE_PAIRWISE } else { KEYTYPE_GROUP }).to_le_bytes(),
    );
    buffer
}

fn set_key_attributes(interface: u32) -> Vec<u8> {
    interface_attributes(
        interface,
        &[(KEY_INDEX, &[0]), (KEY_DEFAULT, &[]), (KEY_DEFAULT_TYPES, &nest(DEFAULT_UNICAST, &[]))],
    )
}

/// The requests that install the key of a handshake step, each a command with its attributes; none for a frame to send.
pub fn key_requests(step: &Step, interface: u32, network: &[u8; 6]) -> Vec<(u8, Vec<u8>)> {
    match step {
        Step::Send(_) => Vec::new(),
        Step::Pairwise(tk) => alloc::vec![
            (NEW_KEY, new_key_attributes(interface, 0, rsn::PAIRWISE, tk, None, Some(network))),
            (SET_KEY, set_key_attributes(interface)),
        ],
        // The driver refuses a station that asks for a default management key: a station only receives with it.
        Step::Group(group) | Step::Management(group) => {
            let attributes =
                new_key_attributes(interface, group.index, group.cipher, &group.key, Some(&group.sequence), None);
            alloc::vec![(NEW_KEY, attributes)]
        }
    }
}

/// The attributes of the request for the station at an address, which for a station that has joined is the access point.
pub fn get_station_attributes(interface: u32, address: &[u8; 6]) -> Vec<u8> {
    interface_attributes(interface, &[(MAC, address)])
}

pub const NO_QUALITY_LINE: &str = "wifi: the driver tells nothing of the link";

/// The quality of the link to an access point: the signal last heard, and the speed frames go out at.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Quality {
    signal: i32,
    speed: u32,
}

impl Quality {
    /// The log line of the link.
    pub fn to_log_line(&self) -> String {
        format!(
            "wifi: the access point is heard at {} dBm, frames go out at {}.{} Mbit/s",
            self.signal,
            self.speed / SPEED_UNITS_PER_MBIT,
            self.speed % SPEED_UNITS_PER_MBIT
        )
    }
}

/// Reads the answer to `GET_STATION`; what it leaves out is 0.
pub fn parse_quality(message: &[u8]) -> Option<Quality> {
    let facts = find_attribute(message, STA_INFO)?;
    let signal = find_attribute(facts, STA_INFO_SIGNAL)
        .and_then(|field| field.first())
        .map_or(0, |level| i32::from(*level as i8));
    let speed = find_attribute(facts, STA_INFO_TX_BITRATE)
        .and_then(|rate| find_attribute(rate, RATE_INFO_BITRATE))
        .and_then(|field| le16(field, 0))
        .map_or(0, u32::from);
    Some(Quality { signal, speed })
}

/// A channel of a radio, as the kernel's rules mark it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Channel {
    pub(super) frequency: u32,
    pub(super) disabled: bool,
    pub(super) no_ir: bool,
}

impl Channel {
    pub(super) const fn allows_probe(&self) -> bool {
        !self.disabled && !self.no_ir
    }
}

/// The channels of a `GET_WIPHY` dump message, by its command and attributes; a message of a list that comes in parts holds one, or none.
pub fn to_channels(command: u8, message: &[u8]) -> Vec<Channel> {
    if command == NEW_WIPHY { parse_channels(message) } else { Vec::new() }
}

fn parse_channels(message: &[u8]) -> Vec<Channel> {
    let bands = find_attribute(message, WIPHY_BANDS).unwrap_or(&[]);
    let channels =
        attributes(bands).flat_map(|(_, band)| attributes(find_attribute(band, BAND_ATTR_FREQS).unwrap_or(&[])));
    channels
        .filter_map(|(_, channel)| {
            let mark = |kind| find_attribute(channel, kind).is_some();
            Some(Channel {
                frequency: le32(find_attribute(channel, FREQUENCY_ATTR_FREQ)?, 0)?,
                disabled: mark(FREQUENCY_ATTR_DISABLED),
                no_ir: mark(FREQUENCY_ATTR_NO_IR),
            })
        })
        .collect()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::wifi::scan::NEW_SCAN_RESULTS;
    use crate::wifi::scan::TRIGGER_SCAN;

    pub(crate) fn answer(sequence: u32, error: i32) -> Vec<u8> {
        [20u32.to_le_bytes(), [2, 0, 0, 0], sequence.to_le_bytes(), [0; 4], error.to_le_bytes()].concat()
    }

    #[test]
    fn request_bytes() {
        let mut interface = Vec::new();
        add_attribute(&mut interface, IFINDEX, &30u32.to_le_bytes());
        assert_eq!(interface, [8, 0, 3, 0, 30, 0, 0, 0]);
        let bytes = request(21, ACKNOWLEDGE, 7, TRIGGER_SCAN, 0, &interface);
        assert_eq!(bytes, [28, 0, 0, 0, 21, 0, 5, 0, 7, 0, 0, 0, 0, 0, 0, 0, 33, 0, 0, 0, 8, 0, 3, 0, 30, 0, 0, 0]);
        let mut name = Vec::new();
        add_attribute(&mut name, FAMILY_NAME, WIRELESS);
        assert_eq!(name.len(), 12);
        add_attribute(&mut name, 9, b"abcde");
        assert_eq!(
            (name.len(), &name[12..14]),
            (24, &[9u8, 0][..]),
            "an attribute says its own length and is filled up to four"
        );
        assert_eq!(attributes(&name).collect::<Vec<_>>(), [(FAMILY_NAME, WIRELESS), (9, &b"abcde"[..])]);
        assert_eq!(
            (attributes(&name[..21]).count(), attributes(&name[..20]).count()),
            (2, 1),
            "one that has lost only its filling counts, one that is cut short is left out"
        );
    }

    #[test]
    fn datagram_messages() {
        let (ack, refusal) = (answer(7, 0), answer(8, -16));
        let done = [16u32.to_le_bytes(), [3, 0, 2, 0], 9u32.to_le_bytes(), [0; 4]].concat();
        let event = request(21, 0, 9, NEW_SCAN_RESULTS, 1, &interface_attributes(30, &[]));
        let datagram = [ack, refusal, event.clone(), done].concat();
        let list: Vec<Message> = messages(&datagram).collect();
        assert_eq!(
            list.iter().map(|message| (message.kind, message.sequence)).collect::<Vec<_>>(),
            [(ERROR, 7), (ERROR, 8), (21, 9), (DONE, 9)]
        );
        assert_eq!(list.iter().map(Message::ack_status).collect::<Vec<_>>(), [Some(0), Some(16), None, None]);
        assert_eq!(
            list.iter().map(Message::error).collect::<Vec<_>>(),
            [None, Some(16), None, None],
            "an acknowledgement refuses nothing"
        );
        assert!(
            list[0].is_ack_of(7) && !list[0].is_ack_of(8) && !list[1].is_ack_of(8),
            "the acknowledgement of the request of that number, not a refusal"
        );
        assert!(
            list[3].is_end_of(9) && !list[3].is_end_of(8) && !list[2].is_end_of(9),
            "the end of the answers to the request of that number, not one of them"
        );
        assert_eq!(
            list[2].command().map(|(command, attributes)| (command, attributes.len())),
            Some((NEW_SCAN_RESULTS, event.len() - 20))
        );
        assert_eq!((list[0].command(), list[3].command()), (None, None));
        assert_eq!(messages(&datagram[..datagram.len() - 1]).count(), 3, "a message that is cut short is left out");
        assert_eq!(
            messages(&answer(9, i32::MIN)).next().map(|message| message.ack_status()),
            Some(Some(i32::MAX)),
            "an error number no kernel sends is read as the farthest there is"
        );
    }

    #[test]
    fn command_sequence() {
        let datagram = request(21, 0, 9, NEW_SCAN_RESULTS, 1, &interface_attributes(30, &[]));
        let message = messages(&datagram).next().unwrap();
        assert!(message.is_answer_of(9) && !message.is_answer_of(8));
        assert_eq!(message.command_of(9), message.command());
        assert_eq!(message.command_of(8), None);
        assert_eq!(messages(&answer(9, 0)).next().unwrap().command_of(9), None);
    }

    #[test]
    fn channels_command() {
        let channel = nest(0, &nest(FREQUENCY_ATTR_FREQ, &2412u32.to_le_bytes()));
        let band = nest(0, &nest(BAND_ATTR_FREQS, &channel));
        let bands = nest(WIPHY_BANDS, &band);
        assert_eq!(to_channels(NEW_WIPHY, &bands), parse_channels(&bands));
        assert_eq!(to_channels(NEW_WIPHY, &bands).len(), 1);
        assert!(to_channels(GET_WIPHY, &bands).is_empty());
    }

    #[test]
    fn family_answer() {
        let group = |name: &[u8], id: u32| {
            let mut group = Vec::new();
            add_attribute(&mut group, GROUP_NAME, name);
            add_attribute(&mut group, GROUP_ID, &id.to_le_bytes());
            group
        };
        let family = |groups: &[u8]| {
            let mut answer = Vec::new();
            add_attribute(&mut answer, FAMILY_NAME, WIRELESS);
            add_attribute(&mut answer, FAMILY_ID, &21u16.to_le_bytes());
            add_attribute(&mut answer, FAMILY_GROUPS, groups);
            answer
        };
        let mut groups = Vec::new();
        add_attribute(&mut groups, 1, &group(b"config\0", 4));
        add_attribute(&mut groups, 2, &group(b"scan\0", 5));
        assert_eq!(
            parse_family(&family(&groups)),
            None,
            "a family that lacks the group of joins is not the wireless one"
        );
        add_attribute(&mut groups, 3, &group(b"mlme\0", 7));
        assert_eq!(parse_family(&family(&groups)), Some(Family { id: 21, scans: 5, links: 7 }));
        assert_eq!(parse_family(&family(&groups)[..16]), None);
    }

    #[test]
    fn connect_attributes() {
        let element = [48, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 2, 0, 0];
        let join = ConnectRequest {
            ssid: b"home",
            bssid: [2, 0, 0, 0, 0, 9],
            frequency: 5180,
            ie: &element,
            group: 0x000F_AC04,
            pairwise: 0x000F_AC04,
            akm: 0x000F_AC02,
            mfp_required: false,
            is_sae: false,
        };
        assert_eq!(
            attributes(&join.to_attributes(30)).collect::<Vec<_>>(),
            [
                (IFINDEX, &[30, 0, 0, 0][..]),
                (SSID, b"home"),
                (MAC, &[2, 0, 0, 0, 0, 9]),
                (WIPHY_FREQ, &5180u32.to_le_bytes()),
                (AUTH_TYPE, &[0, 0, 0, 0]),
                (SOCKET_OWNER, &[]),
                (IE, &element),
                (PRIVACY, &[]),
                (WPA_VERSIONS, &[2, 0, 0, 0]),
                (CIPHER_SUITES_PAIRWISE, &[4, 0xAC, 0x0F, 0]),
                (CIPHER_SUITE_GROUP, &[4, 0xAC, 0x0F, 0]),
                (AKM_SUITES, &[2, 0xAC, 0x0F, 0]),
                (CONTROL_PORT, &[]),
            ],
            "the socket that asks owns the link, which proves nothing itself where the handshake does; suites as numbers of this machine's byte order"
        );
        assert_eq!(
            find_attribute(&ConnectRequest { mfp_required: true, ..join.clone() }.to_attributes(30), USE_MFP),
            Some(&[1, 0, 0, 0][..])
        );
        let sae = ConnectRequest { is_sae: true, akm: 0x000F_AC08, ..join.clone() }.to_attributes(30);
        assert_eq!(
            (
                find_attribute(&sae, AUTH_TYPE),
                find_attribute(&sae, EXTERNAL_AUTH_SUPPORT),
                find_attribute(&sae, WPA_VERSIONS)
            ),
            (Some(&[4, 0, 0, 0][..]), Some(&[][..]), Some(&[2, 0, 0, 0][..])),
            "by SAE the station proves the password itself, under the same version"
        );
        assert_eq!(
            attributes(&ConnectRequest { ie: &[], ..join }.to_attributes(30)).map(|(kind, _)| kind).collect::<Vec<_>>(),
            [IFINDEX, SSID, MAC, WIPHY_FREQ, AUTH_TYPE, SOCKET_OWNER],
            "a network that asks for nothing is joined with no word of keys"
        );
        assert_eq!(
            (parse_connect_status(&[]), parse_req_ie(&[]), parse_reason(&[])),
            (None, &[][..], 0),
            "a word of a join with no status, of an association request with no elements, of a loss with no reason"
        );
        assert_eq!(disconnect_attributes(30), [8, 0, 3, 0, 30, 0, 0, 0, 6, 0, 54, 0, 3, 0, 0, 0]);
    }

    #[test]
    fn key_requests_by_step() {
        use crate::wifi::rsn::Group;
        use crate::wifi::wpa::GroupKey;
        let network = [2, 0, 0, 0, 0, 9];
        assert!(
            key_requests(&Step::Send(alloc::vec![1, 2]), 30, &network).is_empty(),
            "a frame to send puts no key to use"
        );
        let pairwise = key_requests(&Step::Pairwise([7; 16]), 30, &network);
        let [(NEW_KEY, new_key), (SET_KEY, set_key)] = &pairwise[..] else {
            panic!("not the key between the two, then the word to send with it: {pairwise:?}")
        };
        assert_eq!(
            (
                find_attribute(new_key, KEY_DATA),
                find_attribute(new_key, KEY_INDEX),
                find_attribute(new_key, KEY_CIPHER)
            ),
            (Some(&[7u8; 16][..]), Some(&[0u8][..]), Some(&[4, 0xAC, 0x0F, 0][..])),
            "at number 0, as CCMP"
        );
        assert_eq!(
            (find_attribute(new_key, MAC), find_attribute(new_key, KEY_TYPE), find_attribute(new_key, KEY_SEQUENCE)),
            (Some(&network[..]), Some(&[1, 0, 0, 0][..]), None),
            "for the access point, with no packet number"
        );
        let kinds = find_attribute(set_key, KEY_DEFAULT_TYPES);
        assert!(
            find_attribute(set_key, KEY_DEFAULT).is_some()
                && kinds.is_some_and(|kinds| find_attribute(kinds, DEFAULT_UNICAST).is_some())
                && find_attribute(set_key, KEY_INDEX) == Some(&[0u8][..])
        );
        let sequence = [1, 2, 3, 4, 5, 6];
        let group = GroupKey { index: 2, cipher: Group::Tkip.suite(), key: alloc::vec![8; 32], sequence };
        let requests = key_requests(&Step::Group(group), 30, &network);
        let [(NEW_KEY, group)] = &requests[..] else { panic!("not one new key: {requests:?}") };
        assert_eq!(
            (find_attribute(group, MAC), find_attribute(group, KEY_TYPE), find_attribute(group, KEY_SEQUENCE)),
            (None, Some(&[0, 0, 0, 0][..]), Some(&sequence[..])),
            "a key for what goes to all names nobody and says how far the network has counted"
        );
        assert_eq!(
            (find_attribute(group, KEY_INDEX), find_attribute(group, KEY_CIPHER), find_attribute(group, KEY_DATA)),
            (Some(&[2u8][..]), Some(&[2, 0xAC, 0x0F, 0][..]), Some(&[8u8; 32][..])),
            "with its own number and cipher"
        );
        let management = GroupKey { index: 4, cipher: rsn::MANAGEMENT, key: alloc::vec![9; 16], sequence };
        let management = key_requests(&Step::Management(management), 30, &network);
        assert_eq!(
            management,
            [(NEW_KEY, new_key_attributes(30, 4, rsn::MANAGEMENT, &[9; 16], Some(&sequence), None))],
            "the management key is put to use, and made nobody's default"
        );
        assert!(
            [group, &management[0].1].iter().all(|request| find_attribute(request, KEY_DEFAULT).is_none()),
            "only the key between the two is the one to send with"
        );
        assert_eq!(find_attribute(&get_station_attributes(30, &network), MAC), Some(&network[..]));
    }

    #[test]
    fn external_auth_messages() {
        let registration = register_frame_attributes(30, 0x00B0, &[3, 0]);
        assert_eq!(
            registration,
            [8, 0, 3, 0, 30, 0, 0, 0, 6, 0, 101, 0, 0xB0, 0, 0, 0, 6, 0, 91, 0, 3, 0, 0, 0],
            "the kind of frame as its first two bytes, and how its body begins"
        );
        let frame = frame_attributes(30, &[0xB0, 0, 1, 2, 3]);
        assert_eq!(
            (
                find_attribute(&frame, IFINDEX),
                find_attribute(&frame, DONT_WAIT_FOR_ACK),
                find_attribute(&frame, ATTR_FRAME),
                parse_frame(&frame)
            ),
            (Some(&[30, 0, 0, 0][..]), Some(&[][..]), Some(&[0xB0, 0, 1, 2, 3][..]), Some(&[0xB0, 0, 1, 2, 3][..])),
            "a frame goes out, and comes up, whole in one attribute"
        );
        assert_eq!(parse_frame(&registration), None);
        let event = |action: u32| {
            let mut attributes = Vec::new();
            add_attribute(&mut attributes, AKM_SUITES, &0x000F_AC08u32.to_le_bytes());
            add_attribute(&mut attributes, EXTERNAL_AUTH_ACTION, &action.to_le_bytes());
            add_attribute(&mut attributes, BSSID, &[2, 0, 0, 0, 0, 9]);
            add_attribute(&mut attributes, SSID, b"home");
            attributes
        };
        assert_eq!(
            (parse_external_auth(&event(0)), parse_external_auth(&event(1)), parse_external_auth(&[])),
            (Some([2, 0, 0, 0, 0, 9]), None, None),
            "the proof is due at the access point the driver names, and not where it asks for its end"
        );
        let report = external_auth_attributes(30, b"home", &[2, 0, 0, 0, 0, 9], false);
        assert_eq!(
            (
                find_attribute(&report, IFINDEX),
                find_attribute(&report, SSID),
                find_attribute(&report, BSSID),
                find_attribute(&report, STATUS_CODE)
            ),
            (Some(&[30, 0, 0, 0][..]), Some(&b"home"[..]), Some(&[2, 0, 0, 0, 0, 9][..]), Some(&[1, 0][..])),
            "a proof that failed, of no named kind"
        );
        assert_eq!(
            find_attribute(&external_auth_attributes(30, b"home", &[2, 0, 0, 0, 0, 9], true), STATUS_CODE),
            Some(&[0, 0][..]),
            "a password proved"
        );
    }

    #[test]
    fn link_quality() {
        let mut rate = Vec::new();
        add_attribute(&mut rate, RATE_INFO_BITRATE, &1300u16.to_le_bytes());
        let mut facts = Vec::new();
        add_attribute(&mut facts, STA_INFO_SIGNAL, &[(-57i8) as u8]);
        add_attribute(&mut facts, STA_INFO_TX_BITRATE, &rate);
        assert_eq!(parse_quality(&nest(STA_INFO, &facts)), Some(Quality { signal: -57, speed: 1300 }));
        assert_eq!((parse_quality(&nest(STA_INFO, &[])), parse_quality(&[])), (Some(Quality::default()), None));
        assert_eq!(
            Quality { signal: -57, speed: 1305 }.to_log_line(),
            "wifi: the access point is heard at -57 dBm, frames go out at 130.5 Mbit/s",
            "in Mbit/s, from the driver's hundred kbit/s"
        );
    }

    #[test]
    fn channel_list() {
        let channel = |frequency: u32, marks: &[u16]| {
            let mut channel = Vec::new();
            add_attribute(&mut channel, FREQUENCY_ATTR_FREQ, &frequency.to_le_bytes());
            marks.iter().for_each(|mark| add_attribute(&mut channel, *mark, &[]));
            channel
        };
        let band = |channels: &[Vec<u8>]| {
            let mut list = Vec::new();
            channels.iter().enumerate().for_each(|(index, channel)| add_attribute(&mut list, index as u16, channel));
            nest(BAND_ATTR_FREQS, &list)
        };
        let mut bands = Vec::new();
        add_attribute(&mut bands, 0, &band(&[channel(2412, &[]), channel(2467, &[FREQUENCY_ATTR_NO_IR])]));
        add_attribute(
            &mut bands,
            1,
            &band(&[channel(5260, &[FREQUENCY_ATTR_NO_IR]), channel(5885, &[FREQUENCY_ATTR_DISABLED])]),
        );
        let plain = Channel { frequency: 2412, disabled: false, no_ir: false };
        let channels = [
            plain,
            Channel { frequency: 2467, no_ir: true, ..plain },
            Channel { frequency: 5260, no_ir: true, ..plain },
            Channel { frequency: 5885, disabled: true, ..plain },
        ];
        assert_eq!(parse_channels(&nest(WIPHY_BANDS, &bands)), channels);
        assert_eq!(
            channels.map(|channel| channel.allows_probe()),
            [true, false, false, false],
            "the radio sends first only where the rules leave a channel unmarked"
        );
        assert!(
            parse_channels(&nest(WIPHY_BANDS, &nest(0, &[9, 0, 2, 0, 1, 2, 3, 4, 5, 0, 0, 0]))).is_empty(),
            "a part of the list that tells of a band's rates only"
        );
        assert!(
            parse_channels(&nest(WIPHY_BANDS, &nest(1, &band(&[Vec::new()])))).is_empty(),
            "a channel without its frequency is left out"
        );
        assert!(parse_channels(&[]).is_empty());
    }
}

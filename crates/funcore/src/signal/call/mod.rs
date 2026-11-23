//! A call's signalling: the call messages devices exchange, an offer's and an answer's parameters, candidates, the SRTP keys of a one-to-one call, what the two sides of one tell each other over the media path (`data.rs`), a group call's messages, the encryption of its frames (`frame.rs`) and what its devices and Signal's calling server tell each other (`sfu.rs`), and the syncs of the call log.

pub(crate) mod data;
pub(crate) mod frame;
pub(crate) mod sfu;

use super::Crypto;
use super::address::Peer;
use super::curve::Pair;
use crate::base::proto::{Message, Writer};
use crate::crypto::KEY_BYTES;
use crate::net::rtp::AES256_BYTES;
use crate::net::rtp::srtp::{Master, SALT_BYTES};
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};

// Content's field of a CallMessage, and CallMessage's own (SignalService.proto).
const CALL_MESSAGE: u32 = 3;
const OFFER: u32 = 1;
const ANSWER: u32 = 2;
const ICE_UPDATE: u32 = 3;
const BUSY: u32 = 5;
const HANGUP: u32 = 7;
const DESTINATION_DEVICE: u32 = 9;
const OPAQUE: u32 = 10;
const ID: u32 = 1;
const OFFER_TYPE: u32 = 3;
const OFFER_OPAQUE: u32 = 4;
const ANSWER_OPAQUE: u32 = 3;
const ICE_OPAQUE: u32 = 5;
const HANGUP_TYPE: u32 = 2;
const HANGUP_DEVICE: u32 = 3;
const OPAQUE_DATA: u32 = 1;
const OPAQUE_URGENCY: u32 = 2;
const VIDEO_CALL: u64 = 1;
const HANDLE_IMMEDIATELY: u64 = 1;
// RingRTC's signaling.proto: the slot of an offer's and an answer's parameters, and their fields.
const V4: u32 = 4;
const PUBLIC_KEY: u32 = 1;
const UFRAG: u32 = 2;
const PWD: u32 = 3;
const MAX_BITRATE: u32 = 5;
const ENCODE_CODECS: u32 = 6;
const DECODE_CODECS: u32 = 7;
const CODEC_TYPE: u32 = 1;
const ADDED: u32 = 2;
const REMOVED: u32 = 3;
const SDP: u32 = 1;
const IP: u32 = 1;
const PORT: u32 = 2;
// RingRTC's CallMessage, which an Opaque carries, and group_call.proto's DeviceToDevice.
const GROUP_MESSAGE: u32 = 1;
const RING_INTENTION: u32 = 2;
const RING_RESPONSE: u32 = 3;
const GROUP_ID: u32 = 1;
const RING_TYPE: u32 = 2;
const RING_ID: u32 = 3;
const MEDIA_KEY: u32 = 2;
const LEAVING: u32 = 4;
const RATCHET_COUNTER: u32 = 1;
const SECRET: u32 = 2;
const DEMUX_ID: u32 = 3;
const LEAVING_DEMUX_ID: u32 = 1;
const RING: u64 = 0;
const CANCELLED: u64 = 1;
// `RingResponse.Type`'s numbers: ringing, which RingRTC passes over from the account's own devices, accepted, declined, and busy, the last.
pub(crate) const RINGING: u64 = 0;
pub(crate) const ACCEPTED: u64 = 1;
pub(crate) const DECLINED: u64 = 2;
pub(crate) const BUSY_RESPONSE: u64 = 3;
// SyncMessage's fields of the call log, and theirs.
const SYNC: u32 = 2;
const SYNC_PADDING: u32 = 8;
const CALL_EVENT: u32 = 19;
const CALL_LOG_EVENT: u32 = 21;
const EVENT_CONVERSATION: u32 = 1;
const EVENT_CALL: u32 = 2;
const EVENT_TIMESTAMP: u32 = 3;
const EVENT_TYPE: u32 = 4;
const EVENT_DIRECTION: u32 = 5;
const EVENT_EVENT: u32 = 6;
const OUTGOING: u64 = 2;
const INCOMING: u64 = 1;
const LOG_TYPE: u32 = 1;
// `SyncMessage.CallLogEvent.Type`'s numbers the R1 acts on: the log cleared, read, and read in one conversation.
pub(super) const CLEAR: u64 = 0;
pub(super) const MARKED_AS_READ: u64 = 1;
pub(super) const MARKED_AS_READ_IN_CONVERSATION: u64 = 2;
const LOG_TIMESTAMP: u32 = 2;
const LOG_CONVERSATION: u32 = 3;
const LOG_CALL: u32 = 4;
// The HKDF info of a one-to-one call's SRTP keys, before the two identity keys (RingRTC's `negotiate_srtp_keys`).
const SRTP_INFO: &[u8] = b"Signal_Calling_20200807_SignallingDH_SRTPKey_KDF";
// A direction's master key and salt under AEAD_AES_256_GCM, the suite of a one-to-one call.
const SRTP_BYTES: usize = AES256_BYTES + SALT_BYTES;
// The bytes of a group call's media secret.
pub(crate) const SECRET_BYTES: usize = 32;
// What RingRTC puts beside a removal, for clients that read an added candidate from every update.
const REMOVAL_FILLER: &str = "candidate:FAKE 1 tcp 0 127.0.0.1 0 typ host";

// A video codec a side names in its parameters, by RingRTC's number of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Codec {
    Vp8 = 8,
    Vp9 = 9,
    H265 = 47,
}

impl Codec {
    fn read(number: u64) -> Option<Codec> {
        [Codec::Vp8, Codec::Vp9, Codec::H265].into_iter().find(|codec| *codec as u64 == number)
    }
}

// What an offer and an answer tell of their side's media path (`ConnectionParametersV4`).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Parameters {
    pub(crate) public_key: [u8; KEY_BYTES],
    pub(crate) ufrag: String,
    pub(crate) pwd: String,
    // The most the side wants sent to it, in bits a second.
    pub(crate) max_bitrate: Option<u64>,
    pub(crate) encodes: Vec<Codec>,
    pub(crate) decodes: Vec<Codec>,
}

impl core::fmt::Debug for Parameters {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Parameters")
            .field("max_bitrate", &self.max_bitrate)
            .field("encodes", &self.encodes)
            .field("decodes", &self.decodes)
            .finish_non_exhaustive()
    }
}

impl Parameters {
    fn read(opaque: &[u8]) -> Option<Parameters> {
        let v4 = Message::parse(opaque)?.message(V4)?;
        let codecs = |field| -> Vec<Codec> {
            let types = v4.repeated(field).filter_map(Message::parse).filter_map(|codec| codec.varint(CODEC_TYPE));
            types.filter_map(Codec::read).collect()
        };
        Some(Parameters {
            public_key: v4.bytes(PUBLIC_KEY)?.try_into().ok()?,
            ufrag: String::from(v4.text(UFRAG)?),
            pwd: String::from(v4.text(PWD)?),
            max_bitrate: v4.varint(MAX_BITRATE),
            encodes: codecs(ENCODE_CODECS),
            decodes: codecs(DECODE_CODECS),
        })
    }

    fn write(&self) -> Vec<u8> {
        let codecs = |v4: Writer, field, codecs: &[Codec]| {
            codecs.iter().fold(v4, |v4, codec| v4.message(field, Writer::new().varint(CODEC_TYPE, *codec as u64)))
        };
        let v4 = Writer::new()
            .bytes(PUBLIC_KEY, &self.public_key)
            .text(UFRAG, &self.ufrag)
            .text(PWD, &self.pwd)
            .maybe(self.max_bitrate.map(|rate| move |v4: Writer| v4.varint(MAX_BITRATE, rate)));
        Writer::new()
            .message(V4, codecs(codecs(v4, ENCODE_CODECS, &self.encodes), DECODE_CODECS, &self.decodes))
            .finish()
    }
}

// A candidate a side found, as its attribute line, or one it lost, by its address.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Candidate {
    Added(String),
    Removed(SocketAddr),
}

impl core::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Candidate::Added(_) => "Added",
            Candidate::Removed(_) => "Removed",
        })
    }
}

impl Candidate {
    // A removal counts before what stands beside it: RingRTC fills an added line in there that names nothing.
    fn read(opaque: &[u8]) -> Option<Candidate> {
        let update = Message::parse(opaque)?;
        if let Some(removed) = update.message(REMOVED) {
            let port = u16::try_from(removed.varint(PORT)?).ok()?;
            let ip = match removed.bytes(IP)? {
                bytes if bytes.len() == 4 => IpAddr::from(<[u8; 4]>::try_from(bytes).ok()?),
                bytes => IpAddr::from(<[u8; 16]>::try_from(bytes).ok()?),
            };
            return Some(Candidate::Removed(SocketAddr::new(ip, port)));
        }
        Some(Candidate::Added(String::from(update.message(ADDED)?.text(SDP)?)))
    }

    fn write(&self) -> Vec<u8> {
        let added = |line: &str| Writer::new().message(ADDED, Writer::new().text(SDP, line));
        match self {
            Candidate::Added(line) => added(line).finish(),
            Candidate::Removed(address) => {
                let ip = match address.ip() {
                    IpAddr::V4(ip) => ip.octets().to_vec(),
                    IpAddr::V6(ip) => ip.octets().to_vec(),
                };
                let removed = Writer::new().bytes(IP, &ip).varint(PORT, u64::from(address.port()));
                added(REMOVAL_FILLER).message(REMOVED, removed).finish()
            }
        }
    }
}

// How a call ended for the device a hang-up reaches, by `Hangup.Type`'s numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Hangup {
    Normal = 0,
    // Another device of the callee's account took the call, declined it, or was busy; the device is named beside it.
    Accepted = 1,
    Declined = 2,
    Busy = 3,
    NeedPermission = 4,
}

impl Hangup {
    fn read(number: u64) -> Hangup {
        [Hangup::Accepted, Hangup::Declined, Hangup::Busy, Hangup::NeedPermission]
            .into_iter()
            .find(|hangup| *hangup as u64 == number)
            .unwrap_or(Hangup::Normal)
    }
}

// A ring of a group, its answer on another device of the account, or a member's media key or leave (RingRTC's `CallMessage`).
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum GroupSignal {
    Ring { group: Peer, ring: i64, is_cancelled: bool },
    // By `RingResponse.Type`'s numbers, `RINGING` to `BUSY_RESPONSE`.
    Answered { group: Peer, ring: i64, kind: u64 },
    MediaKey { group: Peer, demux_id: u32, ratchet: u32, secret: [u8; SECRET_BYTES] },
    Leaving { group: Peer, demux_id: u32 },
}

impl core::fmt::Debug for GroupSignal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GroupSignal::Ring { is_cancelled, .. } => {
                f.debug_struct("Ring").field("is_cancelled", is_cancelled).finish_non_exhaustive()
            }
            GroupSignal::Answered { kind, .. } => {
                f.debug_struct("Answered").field("kind", kind).finish_non_exhaustive()
            }
            GroupSignal::MediaKey { ratchet, .. } => {
                f.debug_struct("MediaKey").field("ratchet", ratchet).finish_non_exhaustive()
            }
            GroupSignal::Leaving { .. } => f.write_str("Leaving"),
        }
    }
}

fn read_group(message: &Message) -> Option<Peer> {
    message.bytes(GROUP_ID)?.try_into().ok().map(Peer::Group)
}

fn write_group(group: &Peer) -> Writer {
    Writer::new().bytes(GROUP_ID, &group.to_bytes())
}

impl GroupSignal {
    // The group the signal is of.
    pub(crate) const fn group(&self) -> Peer {
        match self {
            GroupSignal::Ring { group, .. }
            | GroupSignal::Answered { group, .. }
            | GroupSignal::MediaKey { group, .. }
            | GroupSignal::Leaving { group, .. } => *group,
        }
    }

    // A ring or its answer without a type, or of a type not known, is malformed, as RingRTC reads them (`handle_received_call_message`).
    fn read(data: &[u8]) -> Option<GroupSignal> {
        let message = Message::parse(data)?;
        let ring_id = |ring: &Message| ring.fixed64(RING_ID).map(|id| id as i64);
        if let Some(ring) = message.message(RING_INTENTION) {
            let is_cancelled = match ring.varint(RING_TYPE)? {
                RING => false,
                CANCELLED => true,
                _ => return None,
            };
            return Some(GroupSignal::Ring { group: read_group(&ring)?, ring: ring_id(&ring)?, is_cancelled });
        }
        if let Some(ring) = message.message(RING_RESPONSE) {
            let kind = ring.varint(RING_TYPE).filter(|kind| *kind <= BUSY_RESPONSE)?;
            return Some(GroupSignal::Answered { group: read_group(&ring)?, ring: ring_id(&ring)?, kind });
        }
        let device = message.message(GROUP_MESSAGE)?;
        let group = read_group(&device)?;
        if let Some(key) = device.message(MEDIA_KEY) {
            return Some(GroupSignal::MediaKey {
                group,
                demux_id: key.u32(DEMUX_ID)?,
                ratchet: key.u32(RATCHET_COUNTER).unwrap_or(0),
                secret: key.bytes(SECRET)?.try_into().ok()?,
            });
        }
        let leaving = device.message(LEAVING)?;
        Some(GroupSignal::Leaving { group, demux_id: leaving.u32(LEAVING_DEMUX_ID)? })
    }

    fn write(&self) -> Vec<u8> {
        let ring =
            |group: &Peer, kind: u64, id: i64| write_group(group).varint(RING_TYPE, kind).fixed64(RING_ID, id as u64);
        let message = match self {
            GroupSignal::Ring { group, ring: id, is_cancelled } => {
                Writer::new().message(RING_INTENTION, ring(group, u64::from(*is_cancelled), *id))
            }
            GroupSignal::Answered { group, ring: id, kind } => {
                Writer::new().message(RING_RESPONSE, ring(group, *kind, *id))
            }
            GroupSignal::MediaKey { group, demux_id, ratchet, secret } => {
                let key = Writer::new()
                    .varint(RATCHET_COUNTER, u64::from(*ratchet))
                    .bytes(SECRET, secret)
                    .varint(DEMUX_ID, u64::from(*demux_id));
                Writer::new().message(GROUP_MESSAGE, write_group(group).message(MEDIA_KEY, key))
            }
            GroupSignal::Leaving { group, demux_id } => {
                let leaving = Writer::new().varint(LEAVING_DEMUX_ID, u64::from(*demux_id));
                Writer::new().message(GROUP_MESSAGE, write_group(group).message(LEAVING, leaving))
            }
        };
        message.finish()
    }
}

// What a device tells another of a call (`CallMessage`), by the call's id where it is of a one-to-one call.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Signal {
    // An offer whose parameters cannot be read is none the call can be taken by.
    Offer { call: u64, is_video: bool, parameters: Option<Parameters> },
    Answer { call: u64, parameters: Option<Parameters> },
    Candidates { call: u64, candidates: Vec<Candidate> },
    Hangup { call: u64, kind: Hangup, device: Option<u32> },
    Busy { call: u64 },
    Group { signal: GroupSignal, is_urgent: bool },
}

impl Signal {
    /// The Content that carries the signal, for one device of its recipient's alone where one is named.
    pub fn write(&self, device: Option<u32>) -> Vec<u8> {
        let with_id = |call: &u64| Writer::new().varint(ID, *call);
        let message = match self {
            Signal::Offer { call, is_video, parameters } => {
                let offer = with_id(call).varint(OFFER_TYPE, u64::from(*is_video));
                let opaque = parameters.as_ref().map(Parameters::write);
                Writer::new()
                    .message(OFFER, offer.maybe(opaque.map(|opaque| move |w: Writer| w.bytes(OFFER_OPAQUE, &opaque))))
            }
            Signal::Answer { call, parameters } => {
                let opaque = parameters.as_ref().map(Parameters::write);
                let answer = with_id(call).maybe(opaque.map(|opaque| move |w: Writer| w.bytes(ANSWER_OPAQUE, &opaque)));
                Writer::new().message(ANSWER, answer)
            }
            Signal::Candidates { call, candidates } => candidates.iter().fold(Writer::new(), |message, candidate| {
                message.message(ICE_UPDATE, with_id(call).bytes(ICE_OPAQUE, &candidate.write()))
            }),
            Signal::Hangup { call, kind, device } => {
                let hangup = with_id(call).varint(HANGUP_TYPE, *kind as u64);
                let named = device.map(|device| move |w: Writer| w.varint(HANGUP_DEVICE, u64::from(device)));
                Writer::new().message(HANGUP, hangup.maybe(named))
            }
            Signal::Busy { call } => Writer::new().message(BUSY, with_id(call)),
            Signal::Group { signal, is_urgent } => {
                let opaque = Writer::new().bytes(OPAQUE_DATA, &signal.write());
                let urgency = is_urgent.then_some(|w: Writer| w.varint(OPAQUE_URGENCY, HANDLE_IMMEDIATELY));
                Writer::new().message(OPAQUE, opaque.maybe(urgency))
            }
        };
        let named = device.map(|device| move |w: Writer| w.varint(DESTINATION_DEVICE, u64::from(device)));
        Writer::new().message(CALL_MESSAGE, message.maybe(named)).finish()
    }

    /// The one-to-one call the signal is of; none for a group's.
    pub const fn call(&self) -> Option<u64> {
        match self {
            Signal::Offer { call, .. }
            | Signal::Answer { call, .. }
            | Signal::Candidates { call, .. }
            | Signal::Hangup { call, .. }
            | Signal::Busy { call } => Some(*call),
            Signal::Group { .. } => None,
        }
    }

    // True for the one signal Signal's apps send as urgent beside a group's own: an offer, which rings.
    pub(super) const fn is_urgent(&self) -> bool {
        matches!(self, Signal::Offer { .. } | Signal::Group { is_urgent: true, .. })
    }
}

// The signals of a Content's call message, in its fields' order, and the one device of the account they are for where it names one; none for a Content that carries no call message.
pub(super) fn read(content: &Message) -> Option<(Vec<Signal>, Option<u32>)> {
    let message = content.message(CALL_MESSAGE)?;
    let id = |part: &Message| part.varint(ID);
    let mut signals = Vec::new();
    if let Some(offer) = message.message(OFFER)
        && let Some(call) = id(&offer)
    {
        let is_video = offer.varint(OFFER_TYPE) == Some(VIDEO_CALL);
        signals.push(Signal::Offer {
            call,
            is_video,
            parameters: offer.bytes(OFFER_OPAQUE).and_then(Parameters::read),
        });
    }
    if let Some(answer) = message.message(ANSWER)
        && let Some(call) = id(&answer)
    {
        signals.push(Signal::Answer { call, parameters: answer.bytes(ANSWER_OPAQUE).and_then(Parameters::read) });
    }
    for update in message.repeated(ICE_UPDATE).filter_map(Message::parse) {
        let Some((call, candidate)) = id(&update).zip(update.bytes(ICE_OPAQUE).and_then(Candidate::read)) else {
            continue;
        };
        match signals.last_mut() {
            Some(Signal::Candidates { call: last, candidates }) if *last == call => candidates.push(candidate),
            _ => signals.push(Signal::Candidates { call, candidates: alloc::vec![candidate] }),
        }
    }
    if let Some(hangup) = message.message(HANGUP)
        && let Some(call) = id(&hangup)
    {
        let kind = Hangup::read(hangup.varint(HANGUP_TYPE).unwrap_or(0));
        signals.push(Signal::Hangup { call, kind, device: hangup.u32(HANGUP_DEVICE) });
    }
    if let Some(call) = message.message(BUSY).and_then(|busy| id(&busy)) {
        signals.push(Signal::Busy { call });
    }
    if let Some(opaque) = message.message(OPAQUE)
        && let Some(signal) = opaque.bytes(OPAQUE_DATA).and_then(GroupSignal::read)
    {
        signals.push(Signal::Group { signal, is_urgent: opaque.varint(OPAQUE_URGENCY) == Some(HANDLE_IMMEDIATELY) });
    }
    Some((signals, message.u32(DESTINATION_DEVICE)))
}

// The SRTP master keys of a one-to-one call both sides come to from this side's pair and the far side's public key, under both identity keys: the offer's, under which the caller sends, then the answer's; none where the far key agrees on nothing or the library fails.
pub(crate) fn negotiate(
    crypto: &impl Crypto,
    own: &Pair,
    far: &[u8; KEY_BYTES],
    caller_identity: &[u8; KEY_BYTES],
    callee_identity: &[u8; KEY_BYTES],
) -> Option<(Master, Master)> {
    let secret = crypto.agree(&own.private, far)?;
    let mut keys = [0u8; 2 * SRTP_BYTES];
    let info = [SRTP_INFO, caller_identity, callee_identity];
    crypto.hkdf_sha256(&[0; crate::crypto::SHA256_BYTES], &secret, &info, &mut keys).then_some(())?;
    let (offer, answer) = keys.split_at(SRTP_BYTES);
    let master = |half: &[u8]| Master::new(&half[..AES256_BYTES], &half[AES256_BYTES..]);
    master(offer).zip(master(answer))
}

/// The kind of a call in the log, by `CallEvent.Type`'s numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Audio = 1,
    Video = 2,
    Group = 3,
}

impl Kind {
    // None for a call by a link, which the log keeps none of.
    pub(super) fn read(number: u64) -> Option<Kind> {
        [Kind::Audio, Kind::Video, Kind::Group].into_iter().find(|kind| *kind as u64 == number)
    }
}

// What a device of the account did with a call, by `CallEvent.Event`'s numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Outcome {
    Accepted = 1,
    NotAccepted = 2,
    Deleted = 3,
}

// A call as another device of the account tells it for the log (`SyncMessage.CallEvent`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Logged {
    pub(super) peer: Peer,
    pub(super) call: u64,
    pub(super) timestamp: u64,
    pub(super) kind: Kind,
    pub(super) is_outgoing: bool,
    pub(super) outcome: Outcome,
}

impl Logged {
    // A call by a link, an event the R1 does not know and a conversation that is neither a contact's nor a group's are none of the log's.
    pub(super) fn read(event: &Message) -> Option<Logged> {
        let kind = event.varint(EVENT_TYPE).and_then(Kind::read)?;
        let outcome = [Outcome::Accepted, Outcome::NotAccepted, Outcome::Deleted]
            .into_iter()
            .find(|outcome| event.varint(EVENT_EVENT) == Some(*outcome as u64))?;
        let is_outgoing = match event.varint(EVENT_DIRECTION)? {
            OUTGOING => true,
            INCOMING => false,
            _ => return None,
        };
        let peer = Peer::from_bytes(event.bytes(EVENT_CONVERSATION)?)?;
        if matches!(peer, Peer::Group(_)) != (kind == Kind::Group) {
            return None;
        }
        Some(Logged {
            peer,
            call: event.varint(EVENT_CALL)?,
            timestamp: event.varint(EVENT_TIMESTAMP)?,
            kind,
            is_outgoing,
            outcome,
        })
    }

    // The sync that tells the account's other devices of the call, with the padding every sync carries.
    pub(super) fn write(&self, padding: &[u8]) -> Vec<u8> {
        let event = Writer::new()
            .bytes(EVENT_CONVERSATION, &self.peer.to_bytes())
            .varint(EVENT_CALL, self.call)
            .varint(EVENT_TIMESTAMP, self.timestamp)
            .varint(EVENT_TYPE, self.kind as u64)
            .varint(EVENT_DIRECTION, if self.is_outgoing { OUTGOING } else { INCOMING })
            .varint(EVENT_EVENT, self.outcome as u64);
        Writer::new().message(SYNC, Writer::new().message(CALL_EVENT, event).bytes(SYNC_PADDING, padding)).finish()
    }
}

// What the owner did with the log on another device (`SyncMessage.CallLogEvent`), by its `Type`'s numbers, up to the call it names or the time it tells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct LogChange {
    // `CLEAR`, `MARKED_AS_READ`, `MARKED_AS_READ_IN_CONVERSATION`, or a type the R1 does not act on.
    pub(super) kind: u64,
    pub(super) timestamp: u64,
    pub(super) target: Option<(Peer, u64)>,
}

impl LogChange {
    pub(super) fn read(event: &Message) -> Option<LogChange> {
        let peer = event.bytes(LOG_CONVERSATION).and_then(Peer::from_bytes);
        Some(LogChange {
            kind: event.varint(LOG_TYPE).unwrap_or(0),
            timestamp: event.varint(LOG_TIMESTAMP)?,
            target: peer.zip(event.varint(LOG_CALL)),
        })
    }

    // The sync that tells the account's other devices of the change, with the padding every sync carries (Signal-Desktop's `markAllCallHistoryReadAndSync`).
    pub(super) fn write(&self, padding: &[u8]) -> Vec<u8> {
        let target = self
            .target
            .map(|(peer, call)| move |w: Writer| w.bytes(LOG_CONVERSATION, &peer.to_bytes()).varint(LOG_CALL, call));
        let event = Writer::new().varint(LOG_TYPE, self.kind).varint(LOG_TIMESTAMP, self.timestamp).maybe(target);
        Writer::new().message(SYNC, Writer::new().message(CALL_LOG_EVENT, event).bytes(SYNC_PADDING, padding)).finish()
    }
}

// The log's syncs a sync message carries: a call another device tells of, or a change of the log.
pub(super) fn read_sync(sync: &Message) -> Option<Result<Logged, LogChange>> {
    if let Some(event) = sync.message(CALL_EVENT) {
        return Logged::read(&event).map(Ok);
    }
    sync.message(CALL_LOG_EVENT).and_then(|event| LogChange::read(&event)).map(Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::X25519;
    use crate::crypto::keccak;
    use crate::signal::address::ServiceId;
    use crate::signal::tests::Standin;
    use alloc::format;
    use alloc::vec;

    const GROUP: Peer = Peer::Group([7; 32]);
    const ALICE: Peer = Peer::Contact(ServiceId::Aci([0xA1; 16]));

    fn parameters() -> Parameters {
        Parameters {
            public_key: [5; 32],
            ufrag: "Xy4+".into(),
            pwd: "0123456789abcdefghijkl".into(),
            max_bitrate: Some(2_000_000),
            encodes: vec![Codec::Vp8],
            decodes: vec![Codec::Vp9, Codec::Vp8],
        }
    }

    fn round_trip(signal: &Signal, device: Option<u32>) -> (Vec<Signal>, Option<u32>) {
        let content = signal.write(device);
        read(&Message::parse(&content).unwrap()).unwrap()
    }

    #[test]
    fn signals_round_trip() {
        let candidates = vec![
            Candidate::Added("candidate:1 1 udp 2122260223 192.0.2.7 50000 typ host generation 0".into()),
            Candidate::Removed("192.0.2.7:50000".parse().unwrap()),
            Candidate::Removed("[2001:db8::7]:443".parse().unwrap()),
        ];
        let signals = [
            Signal::Offer { call: 0xFFFF_FFFF_FFFF_FFFE, is_video: true, parameters: Some(parameters()) },
            Signal::Offer { call: 9, is_video: false, parameters: None },
            Signal::Answer { call: 9, parameters: Some(Parameters { max_bitrate: None, ..parameters() }) },
            Signal::Candidates { call: 9, candidates },
            Signal::Hangup { call: 9, kind: Hangup::Normal, device: None },
            Signal::Hangup { call: 9, kind: Hangup::Accepted, device: Some(3) },
            Signal::Hangup { call: 9, kind: Hangup::NeedPermission, device: Some(1) },
            Signal::Busy { call: 9 },
            Signal::Group {
                signal: GroupSignal::Ring { group: GROUP, ring: -5, is_cancelled: false },
                is_urgent: true,
            },
            Signal::Group { signal: GroupSignal::Ring { group: GROUP, ring: 6, is_cancelled: true }, is_urgent: false },
            Signal::Group { signal: GroupSignal::Answered { group: GROUP, ring: 6, kind: 2 }, is_urgent: true },
            Signal::Group {
                signal: GroupSignal::MediaKey { group: GROUP, demux_id: 0x1230, ratchet: 4, secret: [9; 32] },
                is_urgent: false,
            },
            Signal::Group { signal: GroupSignal::Leaving { group: GROUP, demux_id: 0x1230 }, is_urgent: false },
        ];
        for signal in signals {
            assert_eq!(round_trip(&signal, None), (vec![signal.clone()], None), "{signal:?}");
            assert_eq!(round_trip(&signal, Some(2)).1, Some(2), "for one device of the account");
        }
    }

    #[test]
    fn offer_bytes() {
        let offer = Signal::Offer { call: 300, is_video: true, parameters: None };
        assert_eq!(
            offer.write(Some(2)),
            [0x1A, 0x09, 0x0A, 0x05, 0x08, 0xAC, 0x02, 0x18, 0x01, 0x48, 0x02],
            "Content's field 3, the offer in its field 1 with the id and the type, the device in field 9"
        );
        let v4 = Parameters { ufrag: "u".into(), pwd: "p".into(), max_bitrate: None, ..parameters() }.write();
        let mut expected = vec![0x22, 52, 0x0A, 32];
        expected.extend([5; 32]);
        expected.extend([0x12, 1, b'u', 0x1A, 1, b'p', 0x32, 2, 0x08, 8, 0x3A, 2, 0x08, 9, 0x3A, 2, 0x08, 8]);
        assert_eq!(v4, expected, "the parameters in slot 4: key, ufrag, pwd, what it encodes, what it decodes");
        assert!(offer.is_urgent() && !Signal::Busy { call: 1 }.is_urgent(), "an offer rings");
    }

    #[test]
    fn several_in_one_message() {
        let update =
            |call: u64, candidate: &Candidate| Writer::new().varint(ID, call).bytes(ICE_OPAQUE, &candidate.write());
        let (first, second) = (Candidate::Added("candidate:a".into()), Candidate::Added("candidate:b".into()));
        let message = Writer::new()
            .message(ICE_UPDATE, update(4, &first))
            .message(ICE_UPDATE, update(4, &second))
            .message(ICE_UPDATE, Writer::new().varint(ID, 4).bytes(ICE_OPAQUE, b"\xFF"))
            .message(ICE_UPDATE, update(5, &first))
            .message(HANGUP, Writer::new().varint(ID, 4).varint(HANGUP_TYPE, 9));
        let content = Writer::new().message(CALL_MESSAGE, message).finish();
        assert_eq!(
            read(&Message::parse(&content).unwrap()),
            Some((
                vec![
                    Signal::Candidates { call: 4, candidates: vec![first.clone(), second] },
                    Signal::Candidates { call: 5, candidates: vec![first] },
                    Signal::Hangup { call: 4, kind: Hangup::Normal, device: None },
                ],
                None
            )),
            "a call's candidates together, one that cannot be read passed over, a hang-up of a kind not known a plain one"
        );
        let other = Writer::new().message(1, Writer::new().text(1, "hello")).finish();
        assert_eq!(read(&Message::parse(&other).unwrap()), None, "a Content without a call message");
        let empty = Writer::new().message(CALL_MESSAGE, Writer::new()).finish();
        assert_eq!(read(&Message::parse(&empty).unwrap()), Some((vec![], None)));
    }

    #[test]
    fn unreadable_parts() {
        let offer = |opaque: &[u8]| {
            let offer = Writer::new().varint(ID, 1).bytes(OFFER_OPAQUE, opaque);
            let content = Writer::new().message(CALL_MESSAGE, Writer::new().message(OFFER, offer)).finish();
            read(&Message::parse(&content).unwrap()).unwrap().0
        };
        let short_key =
            Writer::new().message(V4, Writer::new().bytes(PUBLIC_KEY, &[5; 31]).text(UFRAG, "u").text(PWD, "p"));
        for opaque in [&b""[..], &[0xFF], &short_key.finish()] {
            assert_eq!(
                offer(opaque),
                [Signal::Offer { call: 1, is_video: false, parameters: None }],
                "an offer without parameters this side reads"
            );
        }
        let unknown = Writer::new().message(
            V4,
            Writer::new()
                .bytes(PUBLIC_KEY, &[5; 32])
                .text(UFRAG, "u")
                .text(PWD, "p")
                .message(ENCODE_CODECS, Writer::new().varint(CODEC_TYPE, 40))
                .message(ENCODE_CODECS, Writer::new().varint(CODEC_TYPE, 47)),
        );
        let Some(Signal::Offer { parameters: Some(read), .. }) = offer(&unknown.finish()).pop() else {
            panic!("an offer")
        };
        assert_eq!((read.encodes, read.decodes), (vec![Codec::H265], vec![]), "a codec not known is passed over");
        let group = |data: Writer| GroupSignal::read(&data.finish());
        assert_eq!(group(Writer::new()), None);
        let short_group = Writer::new().bytes(GROUP_ID, &[7; 16]).fixed64(RING_ID, 1);
        assert_eq!(group(Writer::new().message(RING_INTENTION, short_group)), None, "a group id of 16 bytes");
        let short_secret = Writer::new().bytes(SECRET, &[9; 31]).varint(DEMUX_ID, 16);
        assert_eq!(
            group(Writer::new().message(GROUP_MESSAGE, write_group(&GROUP).message(MEDIA_KEY, short_secret))),
            None,
            "a secret of 31 bytes"
        );
        assert_eq!(
            Candidate::read(&Writer::new().message(REMOVED, Writer::new().bytes(IP, &[1; 5]).varint(PORT, 9)).finish()),
            None
        );
        assert_eq!(
            Candidate::read(
                &Writer::new().message(REMOVED, Writer::new().bytes(IP, &[1; 4]).varint(PORT, 70_000)).finish()
            ),
            None
        );
    }

    #[test]
    fn secrets_untold() {
        let key = GroupSignal::MediaKey { group: GROUP, demux_id: 16, ratchet: 1, secret: [9; 32] };
        let told = format!(
            "{:?} {:?} {key:?} {:?}",
            Signal::Offer { call: 1, is_video: false, parameters: Some(parameters()) },
            Candidate::Removed("192.0.2.7:1".parse().unwrap()),
            GroupSignal::Ring { group: GROUP, ring: 77, is_cancelled: false },
        );
        assert_eq!(
            told,
            "Offer { call: 1, is_video: false, parameters: Some(Parameters { max_bitrate: Some(2000000), encodes: [Vp8], decodes: [Vp9, Vp8], .. }) } Removed MediaKey { ratchet: 1, .. } Ring { is_cancelled: false, .. }",
            "no key, password, address, secret or group is told where formatted"
        );
    }

    #[test]
    fn srtp_keys_of_both_sides() {
        let crypto = Standin::default();
        let (caller, callee) = (Pair::new(&crypto).unwrap(), Pair::new(&crypto).unwrap());
        let (caller_identity, callee_identity) = ([1u8; 32], [2u8; 32]);
        let ours = negotiate(&crypto, &caller, &callee.public, &caller_identity, &callee_identity).unwrap();
        let theirs = negotiate(&crypto, &callee, &caller.public, &caller_identity, &callee_identity).unwrap();
        assert!(ours == theirs, "both sides come to the same keys");
        let secret = crypto.agree(&caller.private, &callee.public).unwrap();
        let mut expected = [0u8; 88];
        keccak::shake256(&[&[0; 32], &secret, SRTP_INFO, &caller_identity, &callee_identity], &mut expected);
        let cut = Master::new(&expected[..32], &expected[32..44]).zip(Master::new(&expected[44..76], &expected[76..]));
        assert!(
            cut.as_ref() == Some(&ours),
            "88 bytes under a salt of noughts and the label with the caller's identity key before the callee's: the offer's key and salt, then the answer's"
        );
        let swapped = negotiate(&crypto, &caller, &callee.public, &callee_identity, &caller_identity).unwrap();
        assert!(swapped != ours, "the identity keys in the other order give other keys");
        assert!(
            negotiate(&crypto, &caller, &[0; 32], &caller_identity, &callee_identity).is_none(),
            "a key that agrees on nothing"
        );
    }

    #[test]
    fn log_syncs() {
        let logged = Logged {
            peer: ALICE,
            call: u64::MAX,
            timestamp: 1_800_000_000_000,
            kind: Kind::Video,
            is_outgoing: true,
            outcome: Outcome::Accepted,
        };
        let content = logged.write(&[0; 3]);
        let sync = Message::parse(&content).unwrap().message(SYNC).unwrap();
        assert_eq!((read_sync(&sync), sync.bytes(SYNC_PADDING)), (Some(Ok(logged)), Some(&[0u8; 3][..])));
        let event = sync.message(CALL_EVENT).unwrap();
        assert_eq!(
            (
                event.bytes(EVENT_CONVERSATION),
                event.varint(EVENT_TYPE),
                event.varint(EVENT_DIRECTION),
                event.varint(EVENT_EVENT)
            ),
            (Some(&[0xA1; 16][..]), Some(2), Some(2), Some(1)),
            "a contact by the 16 bytes of its ACI, a video call, outgoing, accepted"
        );
        let group =
            Logged { peer: GROUP, kind: Kind::Group, is_outgoing: false, outcome: Outcome::NotAccepted, ..logged };
        let content = group.write(&[]);
        assert_eq!(read_sync(&Message::parse(&content).unwrap().message(SYNC).unwrap()), Some(Ok(group)));
        let event =
            |fields: Writer| read_sync(&Message::parse(&Writer::new().message(CALL_EVENT, fields).finish()).unwrap());
        let base =
            || Writer::new().bytes(EVENT_CONVERSATION, &[0xA1; 16]).varint(EVENT_CALL, 1).varint(EVENT_TIMESTAMP, 2);
        assert_eq!(
            event(base().varint(EVENT_TYPE, 1).varint(EVENT_DIRECTION, 1).varint(EVENT_EVENT, 3)),
            Some(Ok(Logged {
                peer: ALICE,
                call: 1,
                timestamp: 2,
                kind: Kind::Audio,
                is_outgoing: false,
                outcome: Outcome::Deleted
            }))
        );
        for (fields, why) in [
            (base().varint(EVENT_TYPE, 4).varint(EVENT_DIRECTION, 1).varint(EVENT_EVENT, 1), "a call by a link"),
            (base().varint(EVENT_TYPE, 1).varint(EVENT_DIRECTION, 1).varint(EVENT_EVENT, 4), "an event not known"),
            (base().varint(EVENT_TYPE, 1).varint(EVENT_DIRECTION, 0).varint(EVENT_EVENT, 1), "no direction"),
            (
                base().varint(EVENT_TYPE, 3).varint(EVENT_DIRECTION, 1).varint(EVENT_EVENT, 1),
                "a group's call with a contact",
            ),
        ] {
            assert_eq!(event(fields), None, "{why}");
        }
        let change = Writer::new()
            .varint(LOG_TYPE, 1)
            .varint(LOG_TIMESTAMP, 9)
            .bytes(LOG_CONVERSATION, &[7; 32])
            .varint(LOG_CALL, 4);
        assert_eq!(
            read_sync(&Message::parse(&Writer::new().message(CALL_LOG_EVENT, change).finish()).unwrap()),
            Some(Err(LogChange { kind: 1, timestamp: 9, target: Some((GROUP, 4)) })),
            "the log read up to a call"
        );
        let clear = Writer::new().varint(LOG_TIMESTAMP, 9);
        assert_eq!(
            read_sync(&Message::parse(&Writer::new().message(CALL_LOG_EVENT, clear).finish()).unwrap()),
            Some(Err(LogChange { kind: 0, timestamp: 9, target: None })),
            "the log cleared up to a time"
        );
        assert_eq!(read_sync(&Message::parse(&[]).unwrap()), None);
        for change in [
            LogChange { kind: 1, timestamp: 1_800_000_000_000, target: Some((ALICE, u64::MAX)) },
            LogChange { kind: 1, timestamp: 9, target: None },
        ] {
            let content = change.write(&[0; 2]);
            let sync = Message::parse(&content).unwrap().message(SYNC).unwrap();
            assert_eq!(
                (read_sync(&sync), sync.bytes(SYNC_PADDING)),
                (Some(Err(change)), Some(&[0u8; 2][..])),
                "a read mark written as it reads, padded"
            );
        }
    }

    #[test]
    fn rings_malformed() {
        let ring = |kind: Option<u64>| {
            let ring = write_group(&GROUP).fixed64(RING_ID, 6);
            let ring = ring.maybe(kind.map(|kind| move |w: Writer| w.varint(RING_TYPE, kind)));
            GroupSignal::read(&Writer::new().message(RING_INTENTION, ring).finish())
        };
        assert_eq!(
            ring(Some(RING)),
            Some(GroupSignal::Ring { group: GROUP, ring: 6, is_cancelled: false }),
            "a ring of its type"
        );
        assert_eq!((ring(None), ring(Some(2))), (None, None), "a ring without a type, or of one not known");
        let answered = |kind: Option<u64>| {
            let ring = write_group(&GROUP).fixed64(RING_ID, 6);
            let ring = ring.maybe(kind.map(|kind| move |w: Writer| w.varint(RING_TYPE, kind)));
            GroupSignal::read(&Writer::new().message(RING_RESPONSE, ring).finish())
        };
        assert_eq!(answered(Some(3)), Some(GroupSignal::Answered { group: GROUP, ring: 6, kind: 3 }), "busy");
        assert_eq!((answered(None), answered(Some(4))), (None, None), "an answer without a type, or of one not known");
    }
}

use crate::base::code::{hex, unhex};
use crate::base::json;
use crate::base::proto::{Message, Writer};
use crate::codec::http::{GET, PUT, USER_AGENT};
use crate::crypto::KEY_BYTES;
use crate::net::rtp::AES128_BYTES;
use crate::net::rtp::srtp::{Master, SALT_BYTES};
use crate::signal::curve::Pair;
use crate::signal::service::{Request, basic_auth};
use crate::signal::{Crypto, hkdf};
use alloc::borrow::Cow;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::SocketAddr;

// Signal's calling server, as Signal Desktop's production configuration names it (`sfuUrl`).
const SERVER: &str = "https://sfu.voip.signal.org";
// Where a call is peeked at and joined (the calling server's `frontend/src/api.rs`).
const PARTICIPANTS: &str = "/v2/conference/participants";
const ACTIVE: &str = "ACTIVE";
// The devices a call takes by the calling server's default (`backend/src/config.rs`, `max_clients_per_call`).
pub(crate) const CALL_DEVICES: usize = 8;
// The HKDF info of a group call's SRTP keys, before the extra info, of which Signal's apps send none (the calling server's `sfu.rs`).
const SRTP_INFO: &[u8] = b"Signal_Group_Call_20211105_SignallingDH_SRTPKey_KDF";
// A direction's master key and salt under AEAD_AES_128_GCM, the suite of a group call.
const SRTP_BYTES: usize = AES128_BYTES + SALT_BYTES;
// RingRTC's group_call.proto: `MRPHeader`, `DeviceToSfu`, `SfuToDevice` and `DeviceToDevice` by their fields.
const SEQNUM: u32 = 1;
const ACK_NUM: u32 = 2;
const NUM_PACKETS: u32 = 3;
const VIDEO_REQUEST: u32 = 1;
const LEAVE: u32 = 2;
const TO_SFU_HEADER: u32 = 8;
const REQUESTS: u32 = 1;
const MAX_KBPS: u32 = 3;
const SPEAKER_HEIGHT: u32 = 4;
const REQUEST_HEIGHT: u32 = 2;
const REQUEST_DEMUX_ID: u32 = 3;
const SPEAKER: u32 = 4;
const SPEAKER_DEMUX_ID: u32 = 2;
const JOINED_OR_LEFT: u32 = 6;
const REMOVED: u32 = 9;
const FROM_SFU_HEADER: u32 = 11;
const CONTENT: u32 = 12;
const HEARTBEAT: u32 = 3;
const LEAVING: u32 = 4;
const MUTE_REQUEST: u32 = 6;
const AUDIO_MUTED: u32 = 1;
const VIDEO_MUTED: u32 = 2;
const PRESENTING: u32 = 3;
const SHARING_SCREEN: u32 = 4;
const MUTED_BY: u32 = 5;
const MUTE_TARGET: u32 = 1;
// RingRTC's window of reliable messages (`group_call.rs`, `RELIABLE_RTP_BUFFER_SIZE`).
const WINDOW: u64 = 64;

fn headers(token: &str) -> Vec<(String, String)> {
    let user = token.split(':').next().unwrap_or_default();
    alloc::vec![
        (String::from("Authorization"), basic_auth(user, token)),
        (String::from("User-Agent"), String::from(USER_AGENT)),
    ]
}

// Where a group's call is peeked at and joined, on the server a request is first made of.
pub(crate) fn participants_url() -> String {
    format!("{SERVER}{PARTICIPANTS}")
}

// Asks who is in a group's call, under the group's token, at the URL given: the server's own, or the one a 307 named.
pub(crate) fn peek_request(url: &str, token: &str) -> Request<'static> {
    Request::new(GET, String::from(url), headers(token), Cow::Borrowed(&[]))
}

// Joins a group's call with this side's ICE credentials and the public key the SRTP keys are agreed by.
pub(crate) fn join_request(
    url: &str,
    token: &str,
    ufrag: &str,
    pwd: &str,
    public_key: &[u8; KEY_BYTES],
) -> Request<'static> {
    let body = format!(
        r#"{{"iceUfrag":"{}","icePwd":"{}","dhePublicKey":"{}","hkdfExtraInfo":"","requiresSvc":false}}"#,
        json::escape(ufrag),
        json::escape(pwd),
        hex(public_key)
    );
    let mut headers = headers(token);
    headers.push((String::from("Content-Type"), String::from("application/json")));
    Request::new(PUT, String::from(url), headers, Cow::Owned(body.into_bytes()))
}

// A device in a call as a peek tells it: its user by the hash of the id's ciphertext in the group, where the server tells one; its `Debug` leaves the user out, as nothing tells with whom a call is.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) demux_id: u32,
    pub(crate) user: Option<String>,
}

impl core::fmt::Debug for Member {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Member").field("demux_id", &self.demux_id).finish_non_exhaustive()
    }
}

// Reads the devices a peek's answer tells; none for an answer without the call's era and the most devices it takes, which is no peek's.
pub(crate) fn parse_peek(document: &str) -> Option<Vec<Member>> {
    json::text_at(document, &["conferenceId"])?;
    json::number_at::<u32>(document, &["maxDevices"])?;
    let devices = json::items(document, &["participants"]).filter_map(|device| {
        Some(Member {
            demux_id: json::number_at(device, &["demuxId"])?,
            user: json::text_at(device, &["opaqueUserId"]).map(json::unescape),
        })
    });
    Some(devices.collect())
}

// What the server answers a join with: this device's place in the call and the server's side of the media path; it has no `Debug`, as it holds the server's ICE password.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Joined {
    pub(crate) demux_id: u32,
    pub(crate) udp: Vec<SocketAddr>,
    pub(crate) tcp: Vec<SocketAddr>,
    pub(crate) tls: Vec<SocketAddr>,
    // The name the addresses of `tls` are reached under.
    pub(crate) hostname: Option<String>,
    pub(crate) ufrag: String,
    pub(crate) pwd: String,
    pub(crate) public_key: [u8; KEY_BYTES],
    pub(crate) creator: String,
    pub(crate) era: String,
    // False while the join waits to be let in.
    pub(crate) is_active: bool,
}

// Reads a join's answer; none for what is none.
pub(crate) fn parse_join(document: &str) -> Option<Joined> {
    let text = |name: &str| json::text_at(document, &[name]).map(json::unescape);
    let addresses = |name: &str| -> Vec<SocketAddr> {
        let path = [name];
        let items = json::items(document, &path).filter_map(|item| item.strip_prefix('"')?.strip_suffix('"'));
        items.filter_map(|address| address.parse().ok()).collect()
    };
    Some(Joined {
        demux_id: json::number_at(document, &["demuxId"])?,
        udp: addresses("udpAddresses"),
        tcp: addresses("tcpAddresses"),
        tls: addresses("tlsAddresses"),
        hostname: text("hostname"),
        ufrag: text("iceUfrag")?,
        pwd: text("icePwd")?,
        public_key: unhex(json::text_at(document, &["dhePublicKey"])?)?.try_into().ok()?,
        creator: text("callCreator")?,
        era: text("conferenceId")?,
        is_active: json::text_at(document, &["clientStatus"]) == Some(ACTIVE),
    })
}

// The SRTP master keys of a group call from this side's pair and the server's public key: the one this device sends under, then the server's; none where the far key agrees on nothing or the library fails.
pub(crate) fn negotiate(crypto: &impl Crypto, own: &Pair, far: &[u8; KEY_BYTES]) -> Option<(Master, Master)> {
    let secret = crypto.agree(&own.private, far)?;
    let keys: [u8; 2 * SRTP_BYTES] = hkdf(crypto, None, &secret, &[SRTP_INFO])?;
    let (client, server) = keys.split_at(SRTP_BYTES);
    let master = |half: &[u8]| Master::new(&half[..AES128_BYTES], &half[AES128_BYTES..]);
    master(client).zip(master(server))
}

// Asks the server for each device's video at the height wanted, nought for none of it, with the most it may send in kbit/s and the height wanted of whoever speaks (`DeviceToSfu.video_request`).
pub(crate) fn write_video_request(requests: &[(u32, u32)], max_kbps: u32, speaker_height: u32) -> Vec<u8> {
    let request = requests.iter().fold(Writer::new(), |request, (demux_id, height)| {
        let one = Writer::new().varint(REQUEST_HEIGHT, u64::from(*height)).fixed32(REQUEST_DEMUX_ID, *demux_id);
        request.message(REQUESTS, one)
    });
    let request = request.varint(MAX_KBPS, u64::from(max_kbps)).varint(SPEAKER_HEIGHT, u64::from(speaker_height));
    Writer::new().message(VIDEO_REQUEST, request).finish()
}

// Tells the server this device leaves (`DeviceToSfu.leave`).
pub(crate) fn write_leave() -> Vec<u8> {
    Writer::new().message(LEAVE, Writer::new()).finish()
}

// What a message of the server's tells this device: of the devices whose video it sends and of its rates (`SfuToDevice.current_devices`, `stats`) nothing is read.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct Update {
    // The device that speaks.
    pub(crate) speaker: Option<u32>,
    // A device joined or left: who is there is to be peeked at.
    pub(crate) is_changed: bool,
    // An administrator removed this device.
    pub(crate) is_removed: bool,
}

impl Update {
    fn read(message: &Message<'_>) -> Update {
        Update {
            speaker: message.message(SPEAKER).and_then(|speaker| speaker.fixed32(SPEAKER_DEMUX_ID)),
            is_changed: message.has(JOINED_OR_LEFT),
            is_removed: message.has(REMOVED),
        }
    }
}

// A message begun that came in several packets: what is there of it, and the packets it still lacks.
struct Parts {
    content: Vec<u8>,
    lacking: u32,
}

// The receiving side of the server's reliable messages (RingRTC's MRP, `mrp/src/stream.rs`): each taken once and in order, and what is owed the server for them.
#[derive(Default)]
pub(crate) struct Inbox {
    // The packets taken in order; the next has this number and one.
    taken: u64,
    // Packets that came before one that lacks, by their numbers: their counts of packets and contents.
    held: BTreeMap<u64, (u32, Vec<u8>, Update)>,
    owes_ack: bool,
    parts: Option<Parts>,
    // The last number of the latest message of several packets, begun or let go.
    parts_end: Option<u64>,
}

impl Inbox {
    // Takes a message of the server's (`SfuToDevice`): what it and the messages it completes tell, in order; nothing for what does not read, came before, or lies past the window.
    pub(crate) fn take(&mut self, bytes: &[u8]) -> Vec<Update> {
        let Some(message) = Message::parse(bytes) else { return Vec::new() };
        let header = message.message(FROM_SFU_HEADER).unwrap_or_default();
        // An acknowledgement carries nothing else (`receive_and_merge`); this device sends nothing reliably.
        if header.has(ACK_NUM) {
            return Vec::new();
        }
        let Some(number) = header.varint(SEQNUM) else { return alloc::vec![Update::read(&message)] };
        if number <= self.taken {
            self.owes_ack = true;
            return Vec::new();
        }
        if number > self.taken + WINDOW {
            return Vec::new();
        }
        let packets = header.u32(NUM_PACKETS).unwrap_or(0);
        let content = message.bytes(CONTENT).unwrap_or_default().to_vec();
        self.held.entry(number).or_insert((packets, content, Update::read(&message)));
        let mut updates = Vec::new();
        while let Some((packets, content, update)) = self.held.remove(&(self.taken + 1)) {
            (self.taken, self.owes_ack) = (self.taken + 1, true);
            updates.extend(self.join(self.taken, packets, content, update));
        }
        updates
    }

    // A packet in its order: a whole message, or a part of one that is given once its last part came (`merge_packets`).
    fn join(&mut self, number: u64, packets: u32, content: Vec<u8>, update: Update) -> Option<Update> {
        if self.parts.is_none() && self.parts_end.is_some_and(|end| number <= end) {
            return None;
        }
        match &mut self.parts {
            // A part that begins a message of its own breaks the one begun, whose rest is passed over.
            Some(_) if packets != 0 => {
                self.parts = None;
                None
            }
            Some(parts) => {
                parts.content.extend(content);
                parts.lacking -= 1;
                let whole = (parts.lacking == 0).then(|| self.parts.take()).flatten()?;
                Message::parse(&whole.content).map(|message| Update::read(&message))
            }
            None if packets <= 1 => Some(update),
            None if u64::from(packets) > WINDOW => None,
            None => {
                self.parts = Some(Parts { content, lacking: packets - 1 });
                self.parts_end = Some(number + u64::from(packets) - 1);
                None
            }
        }
    }

    // The acknowledgement owed the server, once: the number it next expects (`DeviceToSfu.mrp_header`).
    pub(crate) fn take_ack(&mut self) -> Option<Vec<u8>> {
        core::mem::take(&mut self.owes_ack)
            .then(|| Writer::new().message(TO_SFU_HEADER, Writer::new().varint(ACK_NUM, self.taken + 1)).finish())
    }
}

// How a device stands, as it tells the others every second and at each change (`DeviceToDevice.heartbeat`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Heartbeat {
    pub(crate) is_audio_muted: Option<bool>,
    pub(crate) is_video_muted: Option<bool>,
    pub(crate) is_presenting: Option<bool>,
    pub(crate) is_sharing_screen: Option<bool>,
    // The device that asked this one to mute.
    pub(crate) muted_by: Option<u32>,
}

// What a device tells the others through the server, in a frame encrypted as its media is (`DeviceToDevice`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Relayed {
    Heartbeat(Heartbeat),
    Leaving,
    // Asks the device of that demux id to mute its microphone.
    Mute(u32),
}

impl Relayed {
    // None for what tells none of these: a reaction, or no protobuf.
    pub(crate) fn read(bytes: &[u8]) -> Option<Relayed> {
        let message = Message::parse(bytes)?;
        if let Some(heartbeat) = message.message(HEARTBEAT) {
            return Some(Relayed::Heartbeat(Heartbeat {
                is_audio_muted: heartbeat.bool(AUDIO_MUTED),
                is_video_muted: heartbeat.bool(VIDEO_MUTED),
                is_presenting: heartbeat.bool(PRESENTING),
                is_sharing_screen: heartbeat.bool(SHARING_SCREEN),
                muted_by: heartbeat.u32(MUTED_BY),
            }));
        }
        if message.has(LEAVING) {
            return Some(Relayed::Leaving);
        }
        message.message(MUTE_REQUEST)?.u32(MUTE_TARGET).map(Relayed::Mute)
    }

    pub(crate) fn write(&self) -> Vec<u8> {
        let flag = |field: u32, value: Option<bool>| value.map(|value| move |w: Writer| w.bool(field, value));
        let message = match self {
            Relayed::Heartbeat(heartbeat) => {
                let muted_by = heartbeat.muted_by.map(|by| move |w: Writer| w.varint(MUTED_BY, u64::from(by)));
                let fields = Writer::new()
                    .maybe(flag(AUDIO_MUTED, heartbeat.is_audio_muted))
                    .maybe(flag(VIDEO_MUTED, heartbeat.is_video_muted))
                    .maybe(flag(PRESENTING, heartbeat.is_presenting))
                    .maybe(flag(SHARING_SCREEN, heartbeat.is_sharing_screen))
                    .maybe(muted_by);
                Writer::new().message(HEARTBEAT, fields)
            }
            Relayed::Leaving => Writer::new().message(LEAVING, Writer::new()),
            Relayed::Mute(target) => {
                Writer::new().message(MUTE_REQUEST, Writer::new().varint(MUTE_TARGET, u64::from(*target)))
            }
        };
        message.finish()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::crypto::X25519;
    use crate::crypto::keccak;
    use crate::signal::tests::Standin;
    use alloc::vec;

    const TOKEN: &str = "2:user:group:1700000000:1:mac";

    #[test]
    fn peek_and_join_requests() {
        let peek = peek_request(&participants_url(), TOKEN);
        assert_eq!(
            (peek.method, peek.url.as_str(), peek.body.len()),
            ("GET", "https://sfu.voip.signal.org/v2/conference/participants", 0)
        );
        assert_eq!(
            peek.headers,
            [
                (
                    "Authorization".into(),
                    format!("Basic {}", crate::base::code::base64(format!("2:{TOKEN}").as_bytes()))
                ),
                ("User-Agent".into(), USER_AGENT.into())
            ],
            "the token as the password under its first part as the user, and the agent the server asks for"
        );
        let join = join_request(
            "https://region.example/v2/conference/participants?region=x",
            TOKEN,
            "Ab12",
            "p\"w",
            &[0xA5; 32],
        );
        assert_eq!(
            (join.method, join.url.as_str()),
            ("PUT", "https://region.example/v2/conference/participants?region=x")
        );
        assert_eq!(
            core::str::from_utf8(&join.body).unwrap(),
            format!(
                r#"{{"iceUfrag":"Ab12","icePwd":"p\"w","dhePublicKey":"{}","hkdfExtraInfo":"","requiresSvc":false}}"#,
                "a5".repeat(32)
            )
        );
        assert_eq!(join.headers[2], ("Content-Type".into(), "application/json".into()));
    }

    #[test]
    fn peek_answer() {
        let document = r#"{"conferenceId":"1122334455667788","maxDevices":75,"participants":[{"opaqueUserId":"0a0b","demuxId":1234567888,"requiresSvc":false},{"demuxId":48,"requiresSvc":true},{"opaqueUserId":"x"}],"creator":"0a0b"}"#;
        let devices = parse_peek(document).expect("a peek's answer");
        assert_eq!(
            devices,
            [Member { demux_id: 1_234_567_888, user: Some("0a0b".into()) }, Member { demux_id: 48, user: None }],
            "each device with its demux id, one without a user among them"
        );
        assert_eq!(format!("{:?}", devices[0]), "Member { demux_id: 1234567888, .. }", "told without its user");
        let empty = r#"{"conferenceId":"e","maxDevices":8,"participants":[],"creator":null}"#;
        assert_eq!(parse_peek(empty), Some(Vec::new()));
        assert_eq!(
            (parse_peek("{}"), parse_peek(""), parse_peek(r#"{"conferenceId":"e","participants":[]}"#)),
            (None, None, None),
            "an answer without the call's era or the most devices it takes is no peek's"
        );
    }

    #[test]
    fn join_answer() {
        let key = "5a".repeat(32);
        let document = format!(
            r#"{{"demuxId":3456789008,"port":10000,"portTcp":10000,"ips":["192.0.2.80"],"udpAddresses":["192.0.2.80:10000","[2001:db8::80]:10000"],"tcpAddresses":["192.0.2.80:10000"],"tlsAddresses":["192.0.2.80:443","nonsense"],"hostname":"sfu.example.org","iceUfrag":"Sf4u","icePwd":"SfuPasswordOf32CharactersInBase6","dhePublicKey":"{key}","callCreator":"0a0b","conferenceId":"1122334455667788","clientStatus":"ACTIVE"}}"#
        );
        let joined = parse_join(&document).expect("a join's answer");
        assert_eq!(
            (joined.demux_id, joined.udp.len(), joined.tcp.len(), joined.tls.clone(), joined.hostname.as_deref()),
            (3_456_789_008, 2, 1, vec!["192.0.2.80:443".parse().unwrap()], Some("sfu.example.org")),
            "the server's addresses by transport, one that is none passed over"
        );
        assert_eq!(
            (joined.ufrag.as_str(), joined.pwd.len(), joined.public_key, joined.creator.as_str(), joined.is_active),
            ("Sf4u", 32, [0x5A; 32], "0a0b", true)
        );
        let pending = document.replace("ACTIVE", "PENDING");
        assert_eq!(parse_join(&pending).map(|joined| joined.is_active), Some(false), "not yet let in");
        assert!(parse_join(&document.replace(&key, "5a5a")).is_none(), "a public key of two bytes");
        assert!(parse_join(&document.replace("iceUfrag", "ufrag")).is_none());
    }

    #[test]
    fn srtp_keys_of_a_group() {
        let crypto = Standin::default();
        let (own, server) = (Pair::new(&crypto).unwrap(), Pair::new(&crypto).unwrap());
        let ours = negotiate(&crypto, &own, &server.public).unwrap();
        let theirs = negotiate(&crypto, &server, &own.public).unwrap();
        assert!(ours == theirs, "both sides come to the same keys");
        let secret = crypto.agree(&own.private, &server.public).unwrap();
        let mut expected = [0u8; 56];
        keccak::shake256(&[&[0; 32], &secret, SRTP_INFO], &mut expected);
        let cut = Master::new(&expected[..16], &expected[16..28]).zip(Master::new(&expected[28..44], &expected[44..]));
        assert!(
            cut.as_ref() == Some(&ours),
            "56 bytes under a salt of noughts and the label alone: the client's key and salt, then the server's"
        );
        assert!(negotiate(&crypto, &own, &[0; 32]).is_none(), "a key that agrees on nothing");
    }

    #[test]
    fn messages_to_the_server() {
        assert_eq!(
            write_video_request(&[(0x0102_0300, 240), (0x10, 0)], 500, 240),
            [
                0x0A, 0x19, 0x0A, 0x08, 0x10, 0xF0, 0x01, 0x1D, 0x00, 0x03, 0x02, 0x01, 0x0A, 0x07, 0x10, 0x00, 0x1D,
                0x10, 0x00, 0x00, 0x00, 0x18, 0xF4, 0x03, 0x20, 0xF0, 0x01
            ],
            "each device's height and its demux id in four fixed bytes, then the most kbit/s and the speaker's height"
        );
        assert_eq!(write_leave(), [0x12, 0x00]);
    }

    pub(crate) fn speaker(demux_id: u32) -> Writer {
        Writer::new().message(SPEAKER, Writer::new().fixed32(SPEAKER_DEMUX_ID, demux_id))
    }

    // A message that tells a device joined or left.
    pub(crate) fn joined_or_left() -> Writer {
        Writer::new().message(JOINED_OR_LEFT, Writer::new())
    }

    pub(crate) fn numbered(message: Writer, number: u64, packets: Option<u32>) -> Vec<u8> {
        let header = Writer::new().varint(SEQNUM, number);
        let header = header.maybe(packets.map(|packets| move |w: Writer| w.varint(NUM_PACKETS, u64::from(packets))));
        message.message(FROM_SFU_HEADER, header).finish()
    }

    fn speakers(updates: &[Update]) -> Vec<Option<u32>> {
        updates.iter().map(|update| update.speaker).collect()
    }

    #[test]
    fn server_updates() {
        let whole = speaker(0x30).message(JOINED_OR_LEFT, Writer::new()).message(REMOVED, Writer::new());
        let mut inbox = Inbox::default();
        assert_eq!(
            inbox.take(&whole.finish()),
            [Update { speaker: Some(0x30), is_changed: true, is_removed: true }],
            "a message without a number is taken at once"
        );
        assert_eq!(inbox.take_ack(), None, "and owed nothing");
        assert_eq!(inbox.take(&[0xFF]), [], "what is no protobuf");
        // `SfuToDevice.stats`, field 8, with its target rate.
        let only_stats = Writer::new().message(8, Writer::new().varint(1, 1)).finish();
        assert_eq!(inbox.take(&only_stats), [Update::default()], "the server's rates tell this device nothing");
    }

    #[test]
    fn reliable_in_order_once() {
        let mut inbox = Inbox::default();
        assert_eq!(speakers(&inbox.take(&numbered(speaker(1), 1, None))), [Some(1)]);
        assert_eq!(inbox.take_ack(), Some(vec![0x42, 0x02, 0x10, 0x02]), "the next number expected, in field 8");
        assert_eq!(inbox.take_ack(), None, "owed once");
        assert_eq!(inbox.take(&numbered(speaker(3), 3, None)), [], "held: the second lacks");
        assert_eq!(inbox.take_ack(), None, "nothing new to acknowledge");
        assert_eq!(
            speakers(&inbox.take(&numbered(speaker(2), 2, None))),
            [Some(2), Some(3)],
            "the one that lacked, then the one held"
        );
        assert_eq!(inbox.take_ack(), Some(vec![0x42, 0x02, 0x10, 0x04]));
        assert_eq!(inbox.take(&numbered(speaker(2), 2, None)), [], "one that came before is taken no more");
        assert_eq!(inbox.take_ack(), Some(vec![0x42, 0x02, 0x10, 0x04]), "and acknowledged again");
        assert_eq!(inbox.take(&numbered(speaker(9), 4 + WINDOW, None)), [], "past the window");
        assert!(inbox.held.is_empty() && inbox.take_ack().is_none(), "and not held");
        let ack = Writer::new().message(FROM_SFU_HEADER, Writer::new().varint(ACK_NUM, 7).varint(SEQNUM, 4));
        assert_eq!(inbox.take(&speaker(5).message(FROM_SFU_HEADER, Writer::new().varint(ACK_NUM, 7)).finish()), []);
        assert_eq!(inbox.take(&ack.finish()), [], "an acknowledgement carries nothing else");
        assert_eq!(inbox.taken, 3);
    }

    #[test]
    fn messages_of_several_packets() {
        let whole = speaker(0x50).message(JOINED_OR_LEFT, Writer::new()).finish();
        let (first, rest) = whole.split_at(3);
        let part = |content: &[u8]| Writer::new().bytes(CONTENT, content);
        let mut inbox = Inbox::default();
        assert_eq!(inbox.take(&numbered(part(first), 1, Some(2))), [], "the first of two");
        assert_eq!(
            inbox.take(&numbered(part(rest), 2, None)),
            [Update { speaker: Some(0x50), is_changed: true, ..Update::default() }],
            "whole with the second: the contents joined and read as one message"
        );
        assert_eq!(
            speakers(&inbox.take(&numbered(speaker(1), 3, Some(1)))),
            [Some(1)],
            "a count of one is a whole one"
        );
        assert_eq!(inbox.take(&numbered(part(first), 4, Some(3))), []);
        assert_eq!(inbox.take(&numbered(part(first), 5, Some(2))), [], "a new beginning breaks the message begun");
        assert_eq!(inbox.take(&numbered(part(rest), 6, None)), [], "whose rest is passed over");
        assert_eq!(speakers(&inbox.take(&numbered(speaker(7), 7, None))), [Some(7)], "the next after it is taken");
        assert_eq!(inbox.take(&numbered(part(first), 8, Some(WINDOW as u32 + 1))), [], "more parts than the window");
        assert_eq!(speakers(&inbox.take(&numbered(speaker(9), 9, None))), [Some(9)]);
    }

    #[test]
    fn relayed_round_trip() {
        let heartbeat = Heartbeat {
            is_audio_muted: Some(true),
            is_video_muted: Some(false),
            is_presenting: None,
            is_sharing_screen: Some(false),
            muted_by: Some(0x30),
        };
        for relayed in [
            Relayed::Heartbeat(heartbeat),
            Relayed::Heartbeat(Heartbeat::default()),
            Relayed::Leaving,
            Relayed::Mute(0x40),
        ] {
            assert_eq!(Relayed::read(&relayed.write()), Some(relayed), "{relayed:?}");
        }
        assert_eq!(
            Relayed::Heartbeat(Heartbeat { is_audio_muted: Some(true), ..Heartbeat::default() }).write(),
            [0x1A, 0x02, 0x08, 0x01],
            "the heartbeat in field 3"
        );
        let reaction = Writer::new().message(5, Writer::new().text(1, "+")).finish();
        assert_eq!((Relayed::read(&reaction), Relayed::read(&[0xFF])), (None, None), "a reaction, and no protobuf");
    }

    #[test]
    fn strangers_messages() {
        use crate::base::math::xorshift64;
        let mut inbox = Inbox::default();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..5_000 {
            state = xorshift64(state);
            let mut bytes: Vec<u8> = (0..state % 48).map(|at| (state >> (at % 57)) as u8).collect();
            if state.is_multiple_of(3) {
                bytes =
                    numbered(Writer::new().bytes(CONTENT, &bytes), state >> 20 & 0xFF, Some((state >> 30) as u32 % 80));
            }
            inbox.take(&bytes);
            inbox.take_ack();
            Relayed::read(&bytes);
            assert!(inbox.held.len() as u64 <= WINDOW, "no more held than the window");
        }
    }
}

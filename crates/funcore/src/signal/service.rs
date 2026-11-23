//! The service's WebSocket of `chat.signal.org`: its URLs, headers, frames, and the requests and answers the R1 makes of it; and a request over HTTPS to another of Signal's hosts.

use super::account::Account;
use super::address::ServiceId;
use super::session::{Bundle, Ciphertext};
use crate::base::code::base64;
use crate::base::code::unbase64_unpadded;
use crate::base::json;
use crate::base::proto::Message;
use crate::base::proto::Writer;
use crate::codec::http::{FORBIDDEN, OK, UNAUTHORIZED, USER_AGENT};
use crate::net::tls::Purpose;
use crate::net::websocket;
use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

/// Signal's two root certificates, as libsignal ships them: RSA, valid to 24 January 2032, and Ed25519, valid to 16 June 2036.
pub const ROOTS: [&[u8]; 2] =
    [include_bytes!("../../assets/signal/root-rsa.cer"), include_bytes!("../../assets/signal/root-ed25519.cer")];

const HOST: &str = "chat.signal.org";
const STORAGE_HOST: &str = "storage.signal.org";
const PROTOBUF: &str = "application/x-protobuf";
// The CDNs by their numbers, as Signal Desktop's production configuration has them (`cdn`); a number it does not know is the first's.
pub(super) const CDNS: [(u32, &str); 3] =
    [(0, "https://cdn.signal.org"), (2, "https://cdn2.signal.org"), (3, "https://cdn3.signal.org")];
pub const PROVISIONING_PATH: &str = "/v1/websocket/provisioning/";
pub const SOCKET_PATH: &str = "/v1/websocket/";
/// How long the service is given for each piece of an answer.
pub const TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const ADDRESS: &str = "/v1/address";
pub(super) const PROVISIONING_MESSAGE: &str = "/v1/message";
pub(super) const MESSAGE: &str = "/api/v1/message";
pub(super) const QUEUE_EMPTY: &str = "/api/v1/queue/empty";
pub(super) const LINK: &str = "/v1/devices/link";
pub(super) const KEEPALIVE: &str = "/v1/keepalive";
pub(super) const STORAGE_AUTH: &str = "/v1/storage/auth";
pub(super) const CAPABILITIES: &str = "/v1/devices/capabilities";
// Signal-Desktop's `getIceServers`.
pub(super) const RELAYS: &str = "/v2/calling/relays";
// The header of the service's time, by which libsignal tells when a message was delivered (`TIMESTAMP_HEADER_NAME`) and Signal's own servers tell themselves.
pub(super) const TIMESTAMP_HEADER: &str = "x-signal-timestamp";
/// Why a request is not made.
pub const NO_URL: &str = "no URL";
pub const NOT_A_FRAME_LINE: &str = "signal: a frame that is none of the service's";
const TEXT_FRAME_LINE: &str = "signal: a text frame, which the service does not send";
pub(super) const ANONYMOUS_REQUEST_LINE: &str = "signal: a request on the unauthenticated socket";
const TOO_LARGE: &str = "an answer larger than the attachment it is for";

/// A request over HTTPS on a connection of its own, to the storage service, the groups server or a CDN, and the most of its answer's body that is taken.
#[derive(Clone, PartialEq, Eq)]
pub struct Request<'a> {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Cow<'a, [u8]>,
    pub(super) most: u64,
}

impl<'a> Request<'a> {
    /// A request whose answer's body is taken whole.
    pub const fn new(
        method: &'static str,
        url: String,
        headers: Vec<(String, String)>,
        body: Cow<'a, [u8]>,
    ) -> Request<'a> {
        Request { method, url, headers, body, most: u64::MAX }
    }

    /// Refuses an answer's body that grew past the most taken, by its length so far.
    pub const fn check_size(&self, bytes: usize) -> Result<(), &'static str> {
        if bytes as u64 > self.most { Err(TOO_LARGE) } else { Ok(()) }
    }
}

impl core::fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Request").field("method", &self.method).field("body", &self.body.len()).finish_non_exhaustive()
    }
}

/// The reason of a socket the service closed, with the status it gave.
pub fn to_closed_text(status: Option<u16>) -> String {
    let suffix = status.map_or_else(String::new, |status| format!(" with {status}"));
    format!("the service closed the socket{suffix}")
}

/// The URL of a path on `HOST`.
pub fn url(path: &str) -> String {
    format!("https://{HOST}{path}")
}

pub(super) fn storage_url(path: &str) -> String {
    format!("https://{STORAGE_HOST}{path}")
}

// The storage service and the groups server, one host, take the same headers under an authorization.
pub(super) fn storage_headers(authorization: String) -> Vec<(String, String)> {
    alloc::vec![(String::from("Authorization"), authorization), (String::from("Content-Type"), String::from(PROTOBUF))]
}

// True for a host of Signal's, its CDNs among them, whatever the case of its letters.
fn is_pinned_host(host: &str) -> bool {
    let cdns = CDNS.iter().filter_map(|(_, base)| base.strip_prefix("https://"));
    [HOST, STORAGE_HOST].into_iter().chain(cdns).any(|pinned| host.eq_ignore_ascii_case(pinned))
}

/// The purpose of a TLS connection to `host`: Signal's for a host of Signal's, whoever makes it, so that it trusts Signal's roots alone; else the one asked for.
pub fn to_purpose(host: &str, purpose: Purpose) -> Purpose {
    if is_pinned_host(host) { Purpose::Signal } else { purpose }
}

/// The headers a socket of the service's opens with: for the account's socket the device's credentials and no stories, and for every socket the firmware's name.
pub fn socket_headers(account: Option<&Account>) -> Vec<(&'static str, String)> {
    let credentials = account.map(|account| {
        [
            ("Authorization", basic_auth(&account.username(), &account.password)),
            ("x-signal-receive-stories", String::from("false")),
        ]
    });
    credentials.into_iter().flatten().chain([("User-Agent", String::from(USER_AGENT))]).collect()
}

/// True if the status refusing the account's socket says the phone unlinked the device.
pub fn is_unlinked(status: u16) -> bool {
    matches!(status, UNAUTHORIZED | FORBIDDEN)
}

pub(super) fn keys_path(identity: &str) -> String {
    format!("/v2/keys?identity={identity}")
}

pub(super) fn bundles_path(recipient: &ServiceId) -> String {
    format!("/v2/keys/{}/*", recipient.to_text())
}

pub(super) fn messages_path(recipient: &ServiceId) -> String {
    format!("/v1/messages/{}", recipient.to_text())
}

pub(super) const fn certificate_path(has_number: bool) -> &'static str {
    if has_number { "/v1/certificate/delivery" } else { "/v1/certificate/delivery?includeE164=false" }
}

pub(super) const ACCESS_KEY: &str = "unidentified-access-key";
pub(super) const GROUP_SEND_TOKEN: &str = "group-send-token";

pub(super) fn multi_recipient_path(timestamp: u64, is_urgent: bool) -> String {
    format!("/v1/messages/multi_recipient?ts={timestamp}&online=false&urgent={is_urgent}")
}

pub(super) const MULTI_RECIPIENT: (&str, &str) = ("content-type", "application/vnd.signal-messenger.mrm");

pub(super) fn group_credentials_path(day: u64) -> String {
    format!(
        "/v1/certificate/auth/group?redemptionStartSeconds={day}&redemptionEndSeconds={day}&v101=true&zkcCredential=true"
    )
}

pub(super) fn write_capabilities(has_number: bool) -> String {
    format!(
        r#"{{"attachmentBackfill":true,"spqr":true,"usernameChangeSyncMessage":true,"optionalPhoneNumber":{}}}"#,
        !has_number
    )
}

/// A frame the service sent; a request with the time the service delivered it at, ms, where its header tells one.
#[derive(Clone, PartialEq, Eq)]
pub enum Frame {
    Request { id: u64, verb: String, path: String, body: Vec<u8>, delivered: Option<u64> },
    Response { id: u64, status: u16, body: Vec<u8> },
}

impl core::fmt::Debug for Frame {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Frame::Request { id, verb, path, .. } => f
                .debug_struct("Request")
                .field("id", id)
                .field("verb", verb)
                .field("path", path)
                .finish_non_exhaustive(),
            Frame::Response { id, status, .. } => {
                f.debug_struct("Response").field("id", id).field("status", status).finish_non_exhaustive()
            }
        }
    }
}

/// The bytes of a frame, of the WebSocket message that carries one; the log's line for a message of any other kind.
pub fn to_binary(message: websocket::Message) -> Result<Vec<u8>, &'static str> {
    match message {
        websocket::Message::Binary(bytes) => Ok(bytes),
        _ => Err(TEXT_FRAME_LINE),
    }
}

impl Frame {
    pub fn read(bytes: &[u8]) -> Option<Frame> {
        let frame = Message::parse(bytes)?;
        match frame.varint(1)? {
            1 => {
                let request = frame.message(2)?;
                Some(Frame::Request {
                    id: request.varint(4)?,
                    verb: String::from(request.text(1)?),
                    path: String::from(request.text(2)?),
                    body: request.bytes(3).unwrap_or_default().to_vec(),
                    delivered: read_delivered(&request),
                })
            }
            2 => {
                let response = frame.message(3)?;
                Some(Frame::Response {
                    id: response.varint(1)?,
                    status: u16::try_from(response.varint(2)?).ok()?,
                    body: response.bytes(4).unwrap_or_default().to_vec(),
                })
            }
            _ => None,
        }
    }
}

// The last header of the service's time that reads as a number, whatever the case of its name, as libsignal reads it (`convert_received_message`).
fn read_delivered(request: &Message) -> Option<u64> {
    let headers = request.repeated(5).filter_map(|line| core::str::from_utf8(line).ok());
    let times = headers.filter_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case(TIMESTAMP_HEADER).then(|| value.trim().parse().ok()).flatten()
    });
    times.last()
}

pub(super) fn write_request(id: u64, verb: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut request = Writer::new().text(1, verb).text(2, path);
    if !body.is_empty() {
        request = request.bytes(3, body);
    }
    request = request.varint(4, id);
    for (name, value) in headers {
        request = request.text(5, &format!("{}: {value}", name.to_ascii_lowercase()));
    }
    Writer::new().varint(1, 1).message(2, request).finish()
}

pub(super) fn write_ok(id: u64) -> Vec<u8> {
    let response = Writer::new().varint(1, id).varint(2, u64::from(OK)).text(3, "OK");
    Writer::new().varint(1, 2).message(3, response).finish()
}

pub(super) fn basic_auth(username: &str, password: &str) -> String {
    format!("Basic {}", base64(format!("{username}:{password}").as_bytes()))
}

pub(super) const JSON: (&str, &str) = ("content-type", "application/json");

fn parse_key(text: &str) -> Option<[u8; 32]> {
    super::message::decode_key(&unbase64_unpadded(text)?)
}

pub(super) fn parse_devices(document: &str) -> Vec<(u32, Bundle)> {
    let Some(identity) = json::text_at(document, &["identityKey"]).and_then(parse_key) else { return Vec::new() };
    json::items(document, &["devices"])
        .filter_map(|device| {
            let signed = json::value_at(device, &["signedPreKey"])?;
            let kyber = json::value_at(device, &["pqPreKey"])?;
            let signature = |key: &str| -> Option<[u8; 64]> {
                unbase64_unpadded(json::text_at(key, &["signature"])?)?.try_into().ok()
            };
            let kyber_public = unbase64_unpadded(json::text_at(kyber, &["publicKey"])?)?;
            let pre_key = json::value_at(device, &["preKey"]).filter(|value| *value != "null").and_then(|value| {
                Some((json::number_at(value, &["keyId"])?, parse_key(json::text_at(value, &["publicKey"])?)?))
            });
            let bundle = Bundle {
                registration: json::number_at(device, &["registrationId"])?,
                identity,
                pre_key,
                signed_pre_key: (
                    json::number_at(signed, &["keyId"])?,
                    parse_key(json::text_at(signed, &["publicKey"])?)?,
                    signature(signed)?,
                ),
                kyber_pre_key: (
                    json::number_at(kyber, &["keyId"])?,
                    super::message::decode_kyber(&kyber_public)?.to_vec(),
                    signature(kyber)?,
                ),
            };
            Some((json::number_at(device, &["deviceId"])?, bundle))
        })
        .collect()
}

pub(super) fn write_submission(ciphertexts: &[(u32, u32, Ciphertext)], timestamp: u64, is_urgent: bool) -> String {
    let messages: Vec<String> = ciphertexts
        .iter()
        .map(|(device, registration, ciphertext)| {
            format!(
                r#"{{"type":{},"destinationDeviceId":{device},"destinationRegistrationId":{registration},"content":"{}"}}"#,
                ciphertext.kind,
                base64(&ciphertext.bytes)
            )
        })
        .collect();
    format!(r#"{{"messages":[{}],"online":false,"urgent":{is_urgent},"timestamp":{timestamp}}}"#, messages.join(","))
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(super) struct Mismatch {
    pub(super) missing: Vec<u32>,
    pub(super) extra: Vec<u32>,
    pub(super) stale: Vec<u32>,
}

pub(super) fn parse_mismatch(document: &str) -> Mismatch {
    let numbers = |name| json::items(document, &[name]).filter_map(|item| item.parse().ok()).collect();
    Mismatch { missing: numbers("missingDevices"), extra: numbers("extraDevices"), stale: numbers("staleDevices") }
}

pub(super) fn parse_mismatches(document: &str) -> Vec<(ServiceId, Mismatch)> {
    json::items(document, &[])
        .filter_map(|item| {
            let id = ServiceId::from_text(json::text_at(item, &["uuid"])?)?;
            Some((id, parse_mismatch(json::value_at(item, &["devices"])?)))
        })
        .collect()
}

pub(super) fn parse_not_on_signal(document: &str) -> Vec<ServiceId> {
    let texts = json::items(document, &["uuids404"]).filter_map(|item| item.strip_prefix('"')?.strip_suffix('"'));
    texts.filter_map(ServiceId::from_text).collect()
}

pub(super) fn parse_counts(document: &str) -> Option<(u32, u32)> {
    Some((json::number_at(document, &["count"])?, json::number_at(document, &["pqCount"])?))
}

/// A relay a one-to-one call may go through, as the service hands it out (Signal-Desktop's `IceServerGroupType`).
#[derive(Clone, PartialEq, Eq)]
pub struct Relay {
    pub(crate) username: String,
    pub(crate) password: String,
    pub(crate) urls: Vec<String>,
    pub(crate) urls_with_ips: Vec<String>,
    // The name the addresses of `urls_with_ips` are reached under over TLS.
    pub(crate) hostname: Option<String>,
    // The seconds it may be used for; none where the service names none.
    pub(crate) ttl: Option<u64>,
}

impl core::fmt::Debug for Relay {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Relay")
            .field("urls", &self.urls.len())
            .field("urls_with_ips", &self.urls_with_ips.len())
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

// The relays of `GET v2/calling/relays`, each that names its credentials.
pub(super) fn parse_relays(document: &str) -> Vec<Relay> {
    let texts = |relay: &str, name: &str| -> Vec<String> {
        let path = [name];
        let items = json::items(relay, &path).filter_map(|item| item.strip_prefix('"')?.strip_suffix('"'));
        items.map(json::unescape).collect()
    };
    json::items(document, &["relays"])
        .filter_map(|relay| {
            Some(Relay {
                username: json::unescape(json::text_at(relay, &["username"])?),
                password: json::unescape(json::text_at(relay, &["password"])?),
                urls: texts(relay, "urls"),
                urls_with_ips: texts(relay, "urlsWithIps"),
                hostname: json::text_at(relay, &["hostname"]).map(json::unescape),
                ttl: json::number_at(relay, &["ttl"]),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn closed_text() {
        assert_eq!(to_closed_text(None), "the service closed the socket");
        assert_eq!(to_closed_text(Some(1006)), "the service closed the socket with 1006");
    }

    #[test]
    fn answer_size_limit() {
        let whole = Request::new("PUT", "https://x".into(), Vec::new(), Cow::Borrowed(b"abc"));
        assert_eq!(whole.check_size(usize::MAX), Ok(()), "an answer taken whole");
        let request = Request { most: 9, ..whole };
        assert_eq!(
            (request.check_size(9), request.check_size(10)),
            (Ok(()), Err(TOO_LARGE)),
            "a body is taken up to the most and refused past it"
        );
        assert_eq!(format!("{request:?}"), "Request { method: \"PUT\", body: 3, .. }", "its place is not told");
    }

    #[test]
    fn request_fields() {
        let bytes = write_request(7, "PUT", LINK, &[("Content-Type", "application/json")], b"{}");
        let frame = Message::parse(&bytes).unwrap();
        assert_eq!(frame.varint(1), Some(1));
        let request = frame.message(2).unwrap();
        assert_eq!((request.text(1), request.text(2), request.varint(4)), (Some("PUT"), Some(LINK), Some(7)));
        assert_eq!(request.repeated(5).collect::<Vec<_>>(), vec![&b"content-type: application/json"[..]]);
    }

    #[test]
    fn frame_kinds() {
        let service_request = Writer::new()
            .varint(1, 1)
            .message(
                2,
                Writer::new()
                    .text(1, "PUT")
                    .text(2, MESSAGE)
                    .bytes(3, b"envelope")
                    .varint(4, 99)
                    .text(5, "X-Signal-Timestamp: 1700"),
            )
            .finish();
        let frame = Frame::read(&service_request).unwrap();
        assert!(
            matches!(frame, Frame::Request { id: 99, ref path, ref body, delivered: Some(1700), .. } if path == MESSAGE && body == b"envelope"),
            "with the time the service delivered it at"
        );
        let response = Frame::read(&write_ok(99)).unwrap();
        assert_eq!(response, Frame::Response { id: 99, status: 200, body: vec![] });
        assert_eq!(Frame::read(&Writer::new().varint(1, 0).finish()), None, "neither");
        assert_eq!(to_binary(websocket::Message::Binary(vec![1, 2])), Ok(vec![1, 2]), "a frame's bytes");
        assert_eq!(
            to_binary(websocket::Message::Text("hello".into())),
            Err("signal: a text frame, which the service does not send")
        );
    }

    #[test]
    fn delivery_time_header() {
        let delivered = |headers: &[&str]| {
            let request = headers
                .iter()
                .fold(Writer::new().text(1, "PUT").text(2, MESSAGE).varint(4, 1), |w, line| w.text(5, line));
            match Frame::read(&Writer::new().varint(1, 1).message(2, request).finish()) {
                Some(Frame::Request { delivered, .. }) => delivered,
                _ => panic!("a request"),
            }
        };
        assert_eq!(delivered(&[]), None, "none told");
        assert_eq!(delivered(&["x-signal-timestamp:1000"]), Some(1000), "libsignal's own, without a space");
        assert_eq!(
            delivered(&["X-Signal-Timestamp: 1000", "content-type: x", "x-signal-timestamp: 2000 "]),
            Some(2000),
            "whatever the case of its name, the last"
        );
        assert_eq!(
            delivered(&["x-signal-timestamp: 1000", "x-signal-timestamp: soon"]),
            Some(1000),
            "the last that reads as a number"
        );
    }

    #[test]
    fn relay_answer() {
        let document = r#"{"relays":[{"username":"u1","password":"p\/1","urls":["turn:turn3.voip.signal.org","turns:turn3.voip.signal.org:443"],"urlsWithIps":["turn:[2001:db8::1]"],"hostname":"turn3.voip.signal.org","ttl":86400},{"username":"u2","password":"p2"},{"password":"p3"}]}"#;
        let relays = parse_relays(document);
        assert_eq!(
            relays,
            [
                Relay {
                    username: "u1".into(),
                    password: "p/1".into(),
                    urls: vec!["turn:turn3.voip.signal.org".into(), "turns:turn3.voip.signal.org:443".into()],
                    urls_with_ips: vec!["turn:[2001:db8::1]".into()],
                    hostname: Some("turn3.voip.signal.org".into()),
                    ttl: Some(86_400),
                },
                Relay {
                    username: "u2".into(),
                    password: "p2".into(),
                    urls: vec![],
                    urls_with_ips: vec![],
                    hostname: None,
                    ttl: None
                },
            ],
            "each that names its credentials, their escapes read"
        );
        assert_eq!(parse_relays("{}"), [], "none");
        assert_eq!(
            format!("{:?}", relays[0]),
            "Relay { urls: 2, urls_with_ips: 1, ttl: Some(86400), .. }",
            "no credential or address told where formatted"
        );
    }

    #[test]
    fn pre_key_padding() {
        for write in [base64, crate::base::code::base64_unpadded] {
            let key = |byte: u8| write(&[[5].as_slice(), &[byte; 32]].concat());
            let kyber = write(&[[8].as_slice(), &[3; 1568]].concat());
            let document = format!(
                r#"{{"identityKey":"{}","devices":[{{"deviceId":1,"registrationId":7,"preKey":null,"signedPreKey":{{"keyId":2,"publicKey":"{}","signature":"{}"}},"pqPreKey":{{"keyId":4,"publicKey":"{}","signature":"{}"}}}}]}}"#,
                key(1),
                key(2),
                write(&[6; 64]),
                kyber,
                write(&[9; 64])
            );
            let devices = parse_devices(&document);
            assert_eq!(devices.len(), 1, "the device is read: {}", write(&[6; 64]));
            let (id, bundle) = &devices[0];
            assert_eq!(
                (*id, bundle.registration, bundle.identity, bundle.signed_pre_key, bundle.kyber_pre_key.0),
                (1, 7, [1; 32], (2, [2; 32], [6; 64]), 4)
            );
        }
    }

    #[test]
    fn submission_and_mismatch() {
        let body = write_submission(&[(1, 1234, Ciphertext { kind: 3, bytes: vec![1, 2] })], 1761234567890, true);
        assert_eq!(
            body,
            r#"{"messages":[{"type":3,"destinationDeviceId":1,"destinationRegistrationId":1234,"content":"AQI="}],"online":false,"urgent":true,"timestamp":1761234567890}"#
        );
        let mismatch = parse_mismatch(r#"{"missingDevices":[2,3],"extraDevices":[4]}"#);
        assert_eq!(mismatch, Mismatch { missing: vec![2, 3], extra: vec![4], stale: vec![] });
        assert_eq!(parse_mismatch(r#"{"staleDevices":[1]}"#).stale, vec![1]);
        assert_eq!(parse_counts(r#"{"count":42,"pqCount":7}"#), Some((42, 7)));
        assert_eq!(basic_auth("a", "b"), "Basic YTpi");
    }

    #[test]
    fn multi_recipient_answers() {
        assert_eq!(
            multi_recipient_path(1_700_000_000_000, false),
            "/v1/messages/multi_recipient?ts=1700000000000&online=false&urgent=false",
            "libsignal's path of a group's message"
        );
        let aci = "9d0652a3-dcc3-4d11-975f-74d61598733f";
        let pni = "796abedb-ca4e-4f18-8803-1fde5b921f9f";
        let (aci_id, pni_id) =
            (ServiceId::from_text(aci).unwrap(), ServiceId::from_text(&format!("PNI:{pni}")).unwrap());
        assert_eq!(
            parse_not_on_signal(&format!(r#"{{"uuids404":["{aci}", "PNI:{pni}", "garbage"]}}"#)),
            [aci_id, pni_id]
        );
        assert_eq!(parse_not_on_signal("{}"), []);
        let document = format!(
            r#"[{{"uuid":"{aci}","devices":{{"missingDevices":[50,60]}}}},{{"uuid":"PNI:{pni}","devices":{{"missingDevices":[],"extraDevices":[4,5]}}}}]"#
        );
        assert_eq!(
            parse_mismatches(&document),
            [
                (aci_id, Mismatch { missing: vec![50, 60], ..Mismatch::default() }),
                (pni_id, Mismatch { extra: vec![4, 5], ..Mismatch::default() })
            ],
            "libsignal's own example of a 409"
        );
        let stale = format!(r#"[{{"uuid":"{aci}","devices":{{"staleDevices":[4,5]}}}}]"#);
        assert_eq!(parse_mismatches(&stale), [(aci_id, Mismatch { stale: vec![4, 5], ..Mismatch::default() })]);
    }

    #[test]
    fn socket_credentials() {
        assert_eq!(url(SOCKET_PATH), "https://chat.signal.org/v1/websocket/");
        let account = super::super::device::Device::unkeyed().account;
        assert_eq!(
            socket_headers(Some(&account)),
            [
                ("Authorization", basic_auth("03030303-0303-0303-0303-030303030303.2", "secret")),
                ("x-signal-receive-stories", String::from("false")),
                ("User-Agent", String::from(USER_AGENT))
            ]
        );
        assert_eq!(
            socket_headers(None),
            [("User-Agent", String::from(USER_AGENT))],
            "a socket without an account tells the firmware's name alone"
        );
        assert!(is_unlinked(401) && is_unlinked(403));
        assert!(!is_unlinked(404) && !is_unlinked(500), "any other refusal is a stay that broke");
    }

    #[test]
    fn pinned_hosts() {
        assert!(
            is_pinned_host(HOST) && is_pinned_host(STORAGE_HOST) && is_pinned_host("Chat.Signal.Org"),
            "a name is no matter of case"
        );
        assert!(
            ["cdn.signal.org", "cdn2.signal.org", "CDN3.signal.org"].into_iter().all(is_pinned_host),
            "its CDNs, as Signal Desktop trusts them under its own roots alone"
        );
        assert!(
            !is_pinned_host("signal.org")
                && !is_pinned_host("chat.signal.org.example")
                && !is_pinned_host("api.openai.com")
                && !is_pinned_host("cdn4.signal.org")
                && !is_pinned_host("cdn.signal.org.example")
        );
        assert_eq!(
            [to_purpose("cdn2.signal.org", Purpose::Http), to_purpose("api.openai.com", Purpose::Http)],
            [Purpose::Signal, Purpose::Http],
            "a host of Signal's is reached under its roots, whoever asks; any other as asked"
        );
    }
}

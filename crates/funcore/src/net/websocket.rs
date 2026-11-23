//! WebSocket as a client speaks it (RFC 6455): the opening, masked frames, and the server's frames taken apart.

use crate::base::bytes::be16;
use crate::base::code;
use crate::codec::http;
use crate::codec::http::Head;
use crate::codec::http::Target;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const ACCEPT_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const VERSION: &str = "13";
/// The random bytes of a client's key, new for every opening.
pub const KEY_BYTES: usize = 16;
const FIN: u8 = 0x80;
const RESERVED: u8 = 0x70;
const OPCODE: u8 = 0x0F;
const MASK_BIT: u8 = 0x80;
const LENGTH_BITS: u8 = 0x7F;
const LENGTH_16: u8 = 126;
const LENGTH_64: u8 = 127;
const MAX_SHORT: usize = 125;
const FIRST_CONTROL: u8 = 8;
const HEAD_BYTES: usize = 14;
/// The status of a normal closure (RFC 6455, 7.4.1).
pub const NORMAL_CLOSURE: u16 = 1000;
const PROTOCOL_ERROR: u16 = 1002;
const MESSAGE_TOO_BIG: u16 = 1009;
/// The longest message taken from a server.
pub const MESSAGE_BYTES: usize = 1 << 20;

/// A frame's opcode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Continuation = 0,
    Text = 1,
    Binary = 2,
    Close = 8,
    Ping = 9,
    Pong = 10,
}

impl Kind {
    fn from_bits(bits: u8) -> Option<Kind> {
        [Kind::Continuation, Kind::Text, Kind::Binary, Kind::Close, Kind::Ping, Kind::Pong]
            .into_iter()
            .find(|kind| *kind as u8 == bits)
    }
}

/// The client's key, its random bytes in base64.
pub fn client_key(random: &[u8; KEY_BYTES]) -> String {
    code::base64(random)
}

/// The request that opens a WebSocket with the client's key and further headers; none if a header would break out of its line.
pub fn opening_request(target: &Target, key: &str, headers: &[(&str, &str)]) -> Option<Vec<u8>> {
    let upgrade = [
        ("Upgrade", "websocket"),
        ("Connection", "Upgrade"),
        ("Sec-WebSocket-Key", key),
        ("Sec-WebSocket-Version", VERSION),
    ];
    http::write_request(http::GET, target, &[&upgrade[..], headers].concat(), &[])
}

/// The `Sec-WebSocket-Accept` a server owes the client's key: the SHA-1 hash of the key and the standard's GUID, in base64.
pub fn accept_value(key: &str, sha1: impl FnOnce(&[u8]) -> [u8; 20]) -> String {
    code::base64(&sha1([key, ACCEPT_GUID].concat().as_bytes()))
}

/// True if an answer opens the WebSocket; one that adds an extension does not, as none was offered.
pub fn is_opened(head: &Head, accept: &str) -> bool {
    head.status == http::SWITCHING_PROTOCOLS
        && head.has_token("upgrade", "websocket")
        && head.has_token("connection", "upgrade")
        && head.header("sec-websocket-accept") == Some(accept)
        && head.header("sec-websocket-extensions").is_none()
}

/// Writes a client's frame in place of what `frame` held: a whole message, its payload masked with four new random bytes.
pub fn write_frame(kind: Kind, payload: &[u8], mask: [u8; 4], frame: &mut Vec<u8>) {
    frame.clear();
    frame.reserve(HEAD_BYTES + payload.len());
    frame.push(FIN | kind as u8);
    match payload.len() {
        length if length <= MAX_SHORT => frame.push(MASK_BIT | length as u8),
        length if length <= usize::from(u16::MAX) => {
            frame.push(MASK_BIT | LENGTH_16);
            frame.extend((length as u16).to_be_bytes());
        }
        length => {
            frame.push(MASK_BIT | LENGTH_64);
            frame.extend((length as u64).to_be_bytes());
        }
    }
    frame.extend(mask);
    frame.extend(payload.iter().zip(mask.iter().cycle()).map(|(byte, mask)| byte ^ mask));
}

/// The payload of a frame that closes: its status.
pub fn close_payload(status: u16) -> [u8; 2] {
    status.to_be_bytes()
}

/// A server's message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    // To be answered with a pong of the same payload.
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    // The server ends the conversation, with this status if it gives one.
    Close(Option<u16>),
}

impl Message {
    /// The text of a text message; none for any other.
    pub fn text(self) -> Option<String> {
        match self {
            Message::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// Why what a server sent is no WebSocket, or more than was asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Violation {
    // A server's frame that is masked, as only a client's are.
    Masked,
    // A reserved bit is set, or the opcode is none.
    Unknown,
    // A control frame that is long or comes in parts, or a close with half a status.
    Control,
    // A part of a message where none was begun, or a new message inside one.
    OutOfTurn,
    // A text that is no UTF-8.
    NotText,
    // A frame or a message longer than the receiver takes.
    TooLong,
}

impl Violation {
    /// The status the client closes the WebSocket with.
    pub const fn status(self) -> u16 {
        if matches!(self, Violation::TooLong) { MESSAGE_TOO_BIG } else { PROTOCOL_ERROR }
    }

    /// The log text of the violation.
    pub fn to_text(self) -> String {
        format!("the server speaks no WebSocket: {self:?}")
    }
}

/// The log text of an answer of that status that opens nothing.
pub fn to_unopened_text(status: u16) -> String {
    format!("the server answers {status} and opens no WebSocket")
}

/// What a client does with what a server's frame completed: hands a message on, answers a ping or a close (RFC 6455, 5.5.1), or goes on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Message(Message),
    Pong(Vec<u8>),
    // The server has closed, with this status if it gave one; `echo` is the payload of the close that answers it.
    Close { status: Option<u16>, echo: Vec<u8> },
    Nothing,
}

impl Action {
    /// What a client does with the message a frame completed, if any.
    pub fn from_message(message: Option<Message>) -> Action {
        match message {
            Some(Message::Ping(payload)) => Action::Pong(payload),
            Some(Message::Close(status)) => {
                Action::Close { status, echo: status.map_or_else(Vec::new, |status| close_payload(status).to_vec()) }
            }
            Some(Message::Pong(_)) | None => Action::Nothing,
            Some(message) => Action::Message(message),
        }
    }
}

/// Takes what a server sends apart into messages.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Receiver {
    limit: usize,
    parts: Option<(Kind, Vec<u8>)>,
    buffer: Vec<u8>,
}

impl Receiver {
    pub fn new(limit: usize) -> Receiver {
        Receiver { limit, parts: None, buffer: Vec::new() }
    }

    /// Takes back the buffer of a message that has been read, for the next message.
    pub fn give_back(&mut self, mut buffer: Vec<u8>) {
        buffer.clear();
        self.buffer = buffer;
    }

    /// Takes the next frame off the start of `input`: none while it is not all there, else its bytes and the message it completes, if any.
    pub fn take_frame(&mut self, input: &[u8]) -> Result<Option<(usize, Option<Message>)>, Violation> {
        let (Some(&first), Some(&second)) = (input.first(), input.get(1)) else {
            return Ok(None);
        };
        if second & MASK_BIT != 0 {
            return Err(Violation::Masked);
        }
        let kind = Kind::from_bits(first & OPCODE).filter(|_| first & RESERVED == 0).ok_or(Violation::Unknown)?;
        let (is_fin, is_control) = (first & FIN != 0, kind as u8 >= FIRST_CONTROL);
        let (head, length) = match second & LENGTH_BITS {
            LENGTH_16 => match be16(input, 2) {
                Some(length) => (4, u64::from(length)),
                None => return Ok(None),
            },
            LENGTH_64 => match input.get(2..10).and_then(|field| <[u8; 8]>::try_from(field).ok()) {
                Some(field) => (10, u64::from_be_bytes(field)),
                None => return Ok(None),
            },
            length => (2, u64::from(length)),
        };
        if is_control && (!is_fin || second & LENGTH_BITS > MAX_SHORT as u8) {
            return Err(Violation::Control);
        }
        let message_bytes = self.parts.as_ref().map_or(0, |(_, parts)| parts.len());
        let length = usize::try_from(length)
            .ok()
            .filter(|length| is_control || *length <= self.limit.saturating_sub(message_bytes))
            .ok_or(Violation::TooLong)?;
        let Some(payload) = input.get(head..).and_then(|rest| rest.get(..length)) else {
            return Ok(None);
        };
        let message = match kind {
            Kind::Ping => Some(Message::Ping(payload.to_vec())),
            Kind::Pong => Some(Message::Pong(payload.to_vec())),
            Kind::Close => match payload {
                [] => Some(Message::Close(None)),
                [high, low, ..] => Some(Message::Close(Some(u16::from_be_bytes([*high, *low])))),
                [_] => return Err(Violation::Control),
            },
            Kind::Text | Kind::Binary | Kind::Continuation => {
                let (message_kind, mut parts) = match (kind, self.parts.take()) {
                    (Kind::Continuation, Some(parts)) => parts,
                    (Kind::Text | Kind::Binary, None) => (kind, core::mem::take(&mut self.buffer)),
                    _ => return Err(Violation::OutOfTurn),
                };
                parts.extend(payload);
                if !is_fin {
                    self.parts = Some((message_kind, parts));
                    None
                } else if message_kind == Kind::Text {
                    Some(Message::Text(String::from_utf8(parts).map_err(|_| Violation::NotText)?))
                } else {
                    Some(Message::Binary(parts))
                }
            }
        };
        Ok(Some((head + length, message)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn frame(kind: Kind, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
        let mut frame = Vec::new();
        write_frame(kind, payload, mask, &mut frame);
        frame
    }

    #[test]
    fn opening_handshake() {
        let target = Target::parse("wss://server.example.com/chat").unwrap();
        let request =
            opening_request(&target, "dGhlIHNhbXBsZSBub25jZQ==", &[("Origin", "http://example.com")]).unwrap();
        assert_eq!(request, &b"GET /chat HTTP/1.1\r\nHost: server.example.com\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: http://example.com\r\n\r\n"[..]);
        assert_eq!(
            client_key(b"the sample nonce"),
            "dGhlIHNhbXBsZSBub25jZQ==",
            "RFC 6455, 4.1: the random bytes in base64"
        );
        let hash = [
            0xB3, 0x7A, 0x4F, 0x2C, 0xC0, 0x62, 0x4F, 0x16, 0x90, 0xF6, 0x46, 0x06, 0xCF, 0x38, 0x59, 0x45, 0xB2, 0xBE,
            0xC4, 0xEA,
        ];
        let value = accept_value("dGhlIHNhbXBsZSBub25jZQ==", |input| {
            assert_eq!(
                input, b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11",
                "the key and the addition"
            );
            hash
        });
        assert_eq!(value, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", "RFC 6455, 1.3: their hash in base64");
        let head = |text: &str| Head::parse(text.as_bytes()).unwrap().unwrap().0;
        let accept = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
        let answer = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: WebSocket\r\nConnection: keep-alive, upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
        assert!(is_opened(&head(answer), accept));
        assert!(!is_opened(&head(answer), "another"), "an answer that proves another key opens nothing");
        assert!(
            !is_opened(&head(&answer.replace("101", "200")), accept)
                && !is_opened(&head(&answer.replace("Upgrade: WebSocket\r\n", "")), accept)
                && !is_opened(&head(&answer.replace(", upgrade", "")), accept)
        );
        assert!(
            !is_opened(
                &head(&answer.replace("\r\n\r\n", "\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n")),
                accept
            ),
            "an extension nobody offered"
        );
    }

    #[test]
    fn masked_frames() {
        assert_eq!(
            frame(Kind::Text, b"Hello", [0x37, 0xFA, 0x21, 0x3D]),
            [0x81, 0x85, 0x37, 0xFA, 0x21, 0x3D, 0x7F, 0x9F, 0x4D, 0x51, 0x58],
            "RFC 6455, 5.7"
        );
        assert_eq!(
            frame(Kind::Pong, b"Hello", [0x37, 0xFA, 0x21, 0x3D]),
            [0x8A, 0x85, 0x37, 0xFA, 0x21, 0x3D, 0x7F, 0x9F, 0x4D, 0x51, 0x58]
        );
        assert_eq!(frame(Kind::Close, &close_payload(1000), [0; 4]), [0x88, 0x82, 0, 0, 0, 0, 0x03, 0xE8]);
        let (short, two, eight) = (
            frame(Kind::Binary, &[7; 125], [0; 4]),
            frame(Kind::Binary, &[7; 126], [0; 4]),
            frame(Kind::Binary, &vec![7; 65536], [0; 4]),
        );
        assert_eq!((&short[..2], short.len()), (&[0x82, 0xFD][..], 2 + 4 + 125));
        assert_eq!((&two[..4], two.len()), (&[0x82, 0xFE, 0, 126][..], 4 + 4 + 126));
        assert_eq!((&eight[..10], eight.len()), (&[0x82, 0xFF, 0, 0, 0, 0, 0, 1, 0, 0][..], 10 + 4 + 65536));
        assert_eq!(
            frame(Kind::Binary, &[1, 2, 3, 4, 5], [0xFF, 0, 0x0F, 0xF0])[6..],
            [0xFE, 2, 0x0C, 0xF4, 0xFA],
            "the mask goes round"
        );
        let mut reused = vec![9u8; 300];
        let buffer = reused.as_ptr();
        write_frame(Kind::Text, b"Hello", [0x37, 0xFA, 0x21, 0x3D], &mut reused);
        assert_eq!(
            (&reused[..], reused.as_ptr()),
            (&[0x81, 0x85, 0x37, 0xFA, 0x21, 0x3D, 0x7F, 0x9F, 0x4D, 0x51, 0x58][..], buffer),
            "a frame written into what held the one before takes its place and no memory"
        );
    }

    #[test]
    fn buffer_reuse() {
        let mut receiver = Receiver::new(1 << 16);
        let text = |receiver: &mut Receiver, frame: &[u8]| match receiver.take_frame(frame) {
            Ok(Some((_, Some(Message::Text(text))))) => text,
            other => panic!("no text: {other:?}"),
        };
        let first = text(&mut receiver, &[0x81, 0x05, 0x48, 0x65, 0x6C, 0x6C, 0x6F]);
        let (buffer, capacity) = (first.as_ptr(), first.capacity());
        receiver.give_back(first.into_bytes());
        let second = text(&mut receiver, &[0x81, 0x02, 0x48, 0x69]);
        assert_eq!((second.as_str(), second.as_ptr(), second.capacity()), ("Hi", buffer, capacity));
        assert_eq!(text(&mut receiver, &[0x81, 0x02, 0x48, 0x6F]), "Ho", "what was not given back is not waited for");
    }

    #[test]
    fn server_messages() {
        let mut receiver = Receiver::new(1 << 16);
        assert_eq!(
            receiver.take_frame(&[0x81, 0x05, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x89]),
            Ok(Some((7, Some(Message::Text("Hello".into()))))),
            "RFC 6455, 5.7; what follows the frame is left alone"
        );
        for length in 0..7 {
            assert_eq!(
                receiver.take_frame(&[0x81, 0x05, 0x48, 0x65, 0x6C, 0x6C, 0x6F][..length]),
                Ok(None),
                "{length} bytes"
            );
        }
        assert_eq!(
            receiver.take_frame(&[0x01, 0x03, 0x48, 0x65, 0x6C]),
            Ok(Some((5, None))),
            "the first part of a message"
        );
        assert_eq!(
            receiver.take_frame(&[0x89, 0x05, 0x48, 0x65, 0x6C, 0x6C, 0x6F]),
            Ok(Some((7, Some(Message::Ping(b"Hello".to_vec()))))),
            "a ping between the parts"
        );
        assert_eq!(
            receiver.take_frame(&[0x80, 0x02, 0x6C, 0x6F]),
            Ok(Some((4, Some(Message::Text("Hello".into()))))),
            "and the part that ends it"
        );
        assert_eq!(receiver.take_frame(&[0x8A, 0x00]), Ok(Some((2, Some(Message::Pong(Vec::new()))))));
        assert_eq!(receiver.take_frame(&[0x88, 0x02, 0x03, 0xE8]), Ok(Some((4, Some(Message::Close(Some(1000)))))));
        assert_eq!(receiver.take_frame(&[0x88, 0x00]), Ok(Some((2, Some(Message::Close(None))))));
        let long = [&[0x82, 0x7E, 0x01, 0x00][..], &[9; 256]].concat();
        assert_eq!(
            (receiver.take_frame(&long), receiver.take_frame(&long[..259])),
            (Ok(Some((260, Some(Message::Binary(vec![9; 256]))))), Ok(None))
        );
        let longer = [&[0x82, 0x7F, 0, 0, 0, 0, 0, 1, 0, 0][..], &vec![9; 65536]].concat();
        assert_eq!(
            receiver.take_frame(&longer).map(|taken| taken.map(|(length, _)| length)),
            Ok(Some(65546)),
            "a message as long as the receiver takes"
        );
    }

    #[test]
    fn frame_actions() {
        assert_eq!(Action::from_message(Some(Message::Ping(b"hi".to_vec()))), Action::Pong(b"hi".to_vec()));
        assert_eq!(
            Action::from_message(Some(Message::Close(Some(1001)))),
            Action::Close { status: Some(1001), echo: close_payload(1001).to_vec() },
            "a close is answered with its own status"
        );
        assert_eq!(
            Action::from_message(Some(Message::Close(None))),
            Action::Close { status: None, echo: Vec::new() },
            "a close of no status, as it came"
        );
        assert_eq!(
            [Action::from_message(Some(Message::Pong(b"hi".to_vec()))), Action::from_message(None)],
            [Action::Nothing, Action::Nothing],
            "a pong and a part of a message ask nothing"
        );
        assert_eq!(
            Action::from_message(Some(Message::Text("Hello".into()))),
            Action::Message(Message::Text("Hello".into()))
        );
        assert_eq!([Message::Text("Hi".into()).text(), Message::Binary(vec![1]).text()], [Some("Hi".into()), None]);
        assert_eq!(
            [Violation::TooLong, Violation::Masked, Violation::NotText].map(Violation::status),
            [1009, 1002, 1002],
            "a message too long is told apart from what is no WebSocket"
        );
        assert_eq!(close_payload(NORMAL_CLOSURE), [0x03, 0xE8]);
        assert_eq!(
            (Violation::Masked.to_text(), to_unopened_text(403)),
            ("the server speaks no WebSocket: Masked".into(), "the server answers 403 and opens no WebSocket".into())
        );
    }

    #[test]
    fn violations() {
        let refused = |frames: &[&[u8]]| {
            let mut receiver = Receiver::new(8);
            frames.iter().map(|frame| receiver.take_frame(frame)).find_map(Result::err)
        };
        assert_eq!(
            refused(&[&[0x81, 0x85, 0x37, 0xFA, 0x21, 0x3D, 0x7F, 0x9F, 0x4D, 0x51, 0x58]]),
            Some(Violation::Masked)
        );
        assert_eq!(
            (refused(&[&[0xC1, 0x00]]), refused(&[&[0x83, 0x00]]), refused(&[&[0x8B, 0x00]])),
            (Some(Violation::Unknown), Some(Violation::Unknown), Some(Violation::Unknown))
        );
        assert_eq!(
            (refused(&[&[0x09, 0x00]]), refused(&[&[0x89, 0x7E, 0x00, 0x7E]]), refused(&[&[0x88, 0x01, 0x03]])),
            (Some(Violation::Control), Some(Violation::Control), Some(Violation::Control))
        );
        assert_eq!(
            (refused(&[&[0x80, 0x00]]), refused(&[&[0x01, 0x00], &[0x81, 0x00]])),
            (Some(Violation::OutOfTurn), Some(Violation::OutOfTurn))
        );
        assert_eq!(refused(&[&[0x81, 0x02, 0xC3, 0x28]]), Some(Violation::NotText));
        assert_eq!(refused(&[&[0x82, 0x09]]), Some(Violation::TooLong), "refused by its length, before it has come");
        assert_eq!(
            refused(&[&[0x02, 0x05, 1, 2, 3, 4, 5], &[0x80, 0x04]]),
            Some(Violation::TooLong),
            "parts that add up to more than is taken"
        );
        assert_eq!(refused(&[&[0x82, 0x7F, 0xFF, 0, 0, 0, 0, 0, 0, 0]]), Some(Violation::TooLong));
        assert_eq!(
            refused(&[&[0x02, 0x05, 1, 2, 3, 4, 5], &[0x80, 0x7F, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]]),
            Some(Violation::TooLong),
            "a length that would wrap the count of what has come"
        );
        assert_eq!(
            refused(&[&[0x82, 0x08, 1, 2, 3, 4, 5, 6, 7, 8], &[0x89, 0x7D]]),
            None,
            "a ping is not counted against the limit"
        );
    }
}

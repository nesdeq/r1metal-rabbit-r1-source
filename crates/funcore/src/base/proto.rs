use alloc::vec::Vec;

const VARINT_MAX_BYTES: usize = 10;
const VARINT_PAYLOAD_BITS: u32 = 7;
const VARINT_PAYLOAD: u8 = 0x7F;
const VARINT_MORE: u8 = 0x80;
const FIELD_NUMBER_SHIFT: u32 = 3;
const WIRE_TYPE_MASK: u64 = 7;
const WIRE_VARINT: u64 = 0;
const WIRE_FIXED64: u64 = 1;
const WIRE_BYTES: u64 = 2;
const WIRE_FIXED32: u64 = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Value<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

pub(crate) fn read_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (index, byte) in bytes.iter().take(VARINT_MAX_BYTES).enumerate() {
        value |= u64::from(byte & VARINT_PAYLOAD) << (VARINT_PAYLOAD_BITS * index as u32);
        if byte & VARINT_MORE == 0 {
            return Some((value, index + 1));
        }
    }
    None
}

pub(crate) fn write_varint(out: &mut Vec<u8>, value: u64) {
    let mut rest = value;
    loop {
        let low = rest as u8 & VARINT_PAYLOAD;
        rest >>= VARINT_PAYLOAD_BITS;
        if rest == 0 {
            out.push(low);
            return;
        }
        out.push(low | VARINT_MORE);
    }
}

fn take_field<'a>(bytes: &mut &'a [u8]) -> Option<(u32, Value<'a>)> {
    let (key, read) = read_varint(bytes)?;
    *bytes = &bytes[read..];
    let number = u32::try_from(key >> FIELD_NUMBER_SHIFT).ok().filter(|number| *number != 0)?;
    let value = match key & WIRE_TYPE_MASK {
        WIRE_VARINT => {
            let (value, read) = read_varint(bytes)?;
            *bytes = &bytes[read..];
            Value::Varint(value)
        }
        WIRE_FIXED64 => {
            let (head, rest) = bytes.split_first_chunk::<8>()?;
            *bytes = rest;
            Value::Fixed64(u64::from_le_bytes(*head))
        }
        WIRE_BYTES => {
            let (length, read) = read_varint(bytes)?;
            let body = bytes.get(read..)?;
            let length = usize::try_from(length).ok().filter(|length| *length <= body.len())?;
            let (value, rest) = body.split_at(length);
            *bytes = rest;
            Value::Bytes(value)
        }
        WIRE_FIXED32 => {
            let (head, rest) = bytes.split_first_chunk::<4>()?;
            *bytes = rest;
            Value::Fixed32(u32::from_le_bytes(*head))
        }
        _ => return None,
    };
    Some((number, value))
}

#[derive(Clone, Default)]
pub(crate) struct Message<'a> {
    fields: Vec<(u32, Value<'a>)>,
}

impl core::fmt::Debug for Message<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Message").field("fields", &self.fields.len()).finish_non_exhaustive()
    }
}

impl<'a> Message<'a> {
    pub(crate) fn parse(bytes: &'a [u8]) -> Option<Message<'a>> {
        let mut rest = bytes;
        let mut fields = Vec::new();
        while !rest.is_empty() {
            fields.push(take_field(&mut rest)?);
        }
        Some(Message { fields })
    }

    fn values(&self, number: u32) -> impl DoubleEndedIterator<Item = Value<'a>> + '_ {
        self.fields.iter().filter(move |field| field.0 == number).map(|field| field.1)
    }

    fn last(&self, number: u32) -> Option<Value<'a>> {
        self.values(number).next_back()
    }

    pub(crate) fn has(&self, number: u32) -> bool {
        self.values(number).next().is_some()
    }

    pub(crate) fn varint(&self, number: u32) -> Option<u64> {
        match self.last(number)? {
            Value::Varint(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn u32(&self, number: u32) -> Option<u32> {
        self.varint(number)?.try_into().ok()
    }

    pub(crate) fn bool(&self, number: u32) -> Option<bool> {
        Some(self.varint(number)? != 0)
    }

    pub(crate) fn fixed64(&self, number: u32) -> Option<u64> {
        match self.last(number)? {
            Value::Fixed64(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn fixed32(&self, number: u32) -> Option<u32> {
        match self.last(number)? {
            Value::Fixed32(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn bytes(&self, number: u32) -> Option<&'a [u8]> {
        match self.last(number)? {
            Value::Bytes(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn text(&self, number: u32) -> Option<&'a str> {
        self.bytes(number).and_then(|bytes| core::str::from_utf8(bytes).ok())
    }

    pub(crate) fn message(&self, number: u32) -> Option<Message<'a>> {
        self.bytes(number).and_then(Message::parse)
    }

    pub(crate) fn repeated(&self, number: u32) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.values(number).filter_map(|value| match value {
            Value::Bytes(bytes) => Some(bytes),
            _ => None,
        })
    }

    pub(crate) fn varints(&self, number: u32) -> Vec<u64> {
        let mut varints = Vec::new();
        self.values(number).for_each(|value| match value {
            Value::Varint(single) => varints.push(single),
            Value::Bytes(packed) => varints.extend(unpack_varints(packed)),
            _ => {}
        });
        varints
    }
}

fn unpack_varints(packed: &[u8]) -> impl Iterator<Item = u64> + '_ {
    let mut rest = packed;
    core::iter::from_fn(move || {
        let (value, read) = read_varint(rest)?;
        rest = &rest[read..];
        Some(value)
    })
}

/// Builds a message field by field.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Writer {
    bytes: Vec<u8>,
}

impl core::fmt::Debug for Writer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Writer").field("bytes", &self.bytes.len()).finish_non_exhaustive()
    }
}

impl Writer {
    pub(crate) fn new() -> Writer {
        Writer::default()
    }

    fn write_key(&mut self, number: u32, wire_type: u64) {
        write_varint(&mut self.bytes, u64::from(number) << FIELD_NUMBER_SHIFT | wire_type);
    }

    pub(crate) fn varint(mut self, number: u32, value: u64) -> Writer {
        self.write_key(number, WIRE_VARINT);
        write_varint(&mut self.bytes, value);
        self
    }

    pub(crate) fn bool(self, number: u32, value: bool) -> Writer {
        self.varint(number, value.into())
    }

    pub(crate) fn fixed64(mut self, number: u32, value: u64) -> Writer {
        self.write_key(number, WIRE_FIXED64);
        self.bytes.extend(value.to_le_bytes());
        self
    }

    pub(crate) fn fixed32(mut self, number: u32, value: u32) -> Writer {
        self.write_key(number, WIRE_FIXED32);
        self.bytes.extend(value.to_le_bytes());
        self
    }

    pub(crate) fn bytes(mut self, number: u32, value: &[u8]) -> Writer {
        self.write_key(number, WIRE_BYTES);
        write_varint(&mut self.bytes, value.len() as u64);
        self.bytes.extend(value);
        self
    }

    pub(crate) fn text(self, number: u32, value: &str) -> Writer {
        self.bytes(number, value.as_bytes())
    }

    pub(crate) fn message(self, number: u32, value: Writer) -> Writer {
        self.bytes(number, &value.bytes)
    }

    pub(crate) fn maybe(self, write: Option<impl FnOnce(Writer) -> Writer>) -> Writer {
        if let Some(write) = write { write(self) } else { self }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn varint_round_trip() {
        let mut out = Vec::new();
        write_varint(&mut out, 300);
        assert_eq!(out, [0xAC, 0x02]);
        assert_eq!(read_varint(&out), Some((300, 2)));
        let mut out = Vec::new();
        write_varint(&mut out, u64::MAX);
        assert_eq!(out.len(), 10);
        assert_eq!(read_varint(&out), Some((u64::MAX, 10)));
        assert_eq!(read_varint(&[0x80, 0x80]), None);
        assert_eq!(read_varint(&[0xFF; 11]), None);
    }

    #[test]
    fn message_round_trip() {
        let inner = Writer::new().text(1, "inner");
        let bytes = Writer::new()
            .varint(1, 150)
            .bytes(2, b"testing")
            .fixed64(3, 0x0102_0304_0506_0708)
            .message(4, inner)
            .bool(5, true)
            .bytes(6, b"one")
            .bytes(6, b"two")
            .fixed32(7, 0x0102_0304)
            .finish();
        assert_eq!(&bytes[..3], [0x08, 0x96, 0x01]);
        assert_eq!(bytes[12..21], [3 << 3 | 1, 8, 7, 6, 5, 4, 3, 2, 1], "a fixed number, its lowest byte first");
        let message = Message::parse(&bytes).unwrap();
        assert_eq!(message.varint(1), Some(150));
        assert_eq!(message.text(2), Some("testing"));
        assert_eq!(message.fixed64(3), Some(0x0102_0304_0506_0708));
        assert_eq!(message.message(4).unwrap().text(1), Some("inner"));
        assert_eq!(message.bool(5), Some(true));
        assert_eq!(message.bytes(6), Some(&b"two"[..]));
        assert_eq!(message.repeated(6).collect::<Vec<_>>(), [&b"one"[..], b"two"]);
        assert_eq!(message.varint(2), None);
        assert_eq!(bytes[bytes.len() - 5..], [7 << 3 | 5, 4, 3, 2, 1], "four fixed bytes, the lowest first");
        assert_eq!((message.fixed32(7), message.fixed32(3)), (Some(0x0102_0304), None));
        assert!(!message.has(8));
    }

    #[test]
    fn repeated_varints_packed_or_not() {
        let bytes = Writer::new().varint(4, 1).bytes(4, &[3, 0x8E, 0x02]).finish();
        assert_eq!(Message::parse(&bytes).unwrap().varints(4), vec![1, 3, 270]);
    }

    #[test]
    fn malformed_message_is_none() {
        assert!(Message::parse(&[0x12, 0x05, b'a']).is_none());
        assert!(Message::parse(&[0x0B]).is_none());
        assert!(Message::parse(&[0x00, 0x01]).is_none());
        assert!(Message::parse(&[0x09, 1, 2, 3]).is_none());
        assert!(Message::parse(&[]).is_some());
    }

    #[test]
    fn debug_prints_size_only() {
        let writer = Writer::new().text(1, "secret").varint(2, 7);
        assert_eq!(alloc::format!("{writer:?}"), "Writer { bytes: 10, .. }");
        let bytes = writer.finish();
        assert_eq!(alloc::format!("{:?}", Message::parse(&bytes).unwrap()), "Message { fields: 2, .. }");
    }
}

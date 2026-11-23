use super::US_ENGLISH;
use alloc::vec::Vec;

const DESCRIPTORS_MAGIC: u32 = 3;
const STRINGS_MAGIC: u32 = 2;
const FULL_AND_HIGH_SPEED: u32 = 1 | 2;
const SPEED_DESCRIPTORS: u32 = 2;
const STRING_COUNT: u32 = 1;
const LANGUAGE_COUNT: u32 = 1;
const DT_INTERFACE: u8 = 4;
const INTERFACE_BYTES: u8 = 9;
const DT_ENDPOINT: u8 = 5;
const ENDPOINT_BYTES: u8 = 7;
const DIR_IN: u8 = 0x80;
const BULK: u8 = 2;
const INTERFACE_NUMBER: u8 = 0;
const ALTERNATE_SETTING: u8 = 0;
const ENDPOINT_COUNT: u8 = 1;
const NAME_STRING: u8 = 1;
const ENDPOINT_NUMBER: u8 = 1;
const NO_INTERVAL: u8 = 0;
const FULL_SPEED_PACKET: u16 = 64;
const HIGH_SPEED_PACKET: u16 = 512;
/// The size of a struct usb_functionfs_event.
pub const EVENT_BYTES: usize = 12;
const EVENT_TYPE_AT: usize = 8;
const SETUP: u8 = 4;
/// The most events the kernel queues for the function.
pub const MAX_EVENTS: usize = 4;

fn le_words(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn with_length(header: &[u32], body: &[u8]) -> Vec<u8> {
    let mut block = le_words(header);
    block.extend_from_slice(body);
    let length = (block.len() as u32).to_le_bytes();
    block[size_of::<u32>()..][..length.len()].copy_from_slice(&length);
    block
}

/// A function of one interface of that class, subclass and protocol with one bulk IN endpoint, at full and high speed.
pub fn descriptors(class: u8, subclass: u8, protocol: u8) -> Vec<u8> {
    let mut speeds = Vec::new();
    for packet in [FULL_SPEED_PACKET, HIGH_SPEED_PACKET] {
        speeds.extend_from_slice(&[INTERFACE_BYTES, DT_INTERFACE, INTERFACE_NUMBER, ALTERNATE_SETTING, ENDPOINT_COUNT]);
        speeds.extend_from_slice(&[class, subclass, protocol, NAME_STRING]);
        speeds.extend_from_slice(&[ENDPOINT_BYTES, DT_ENDPOINT, DIR_IN | ENDPOINT_NUMBER, BULK]);
        speeds.extend_from_slice(&packet.to_le_bytes());
        speeds.push(NO_INTERVAL);
    }
    with_length(&[DESCRIPTORS_MAGIC, 0, FULL_AND_HIGH_SPEED, SPEED_DESCRIPTORS, SPEED_DESCRIPTORS], &speeds)
}

/// The kernel's strings block naming the interface.
pub fn strings(name: &str) -> Vec<u8> {
    let mut table = US_ENGLISH.to_le_bytes().to_vec();
    table.extend(name.bytes().chain([0]));
    with_length(&[STRINGS_MAGIC, 0, STRING_COUNT, LANGUAGE_COUNT], &table)
}

/// How the kernel stalls a host's request: one that asks for an answer by a read of the control file, others by a write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stall {
    Read,
    Write,
}

/// The stall an event of the kernel's asks for; none for an event that is no request of the host's.
pub fn stall(event: &[u8; EVENT_BYTES]) -> Option<Stall> {
    (event[EVENT_TYPE_AT] == SETUP).then_some(if event[0] & DIR_IN != 0 { Stall::Read } else { Stall::Write })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_bytes() {
        let bytes = descriptors(0xff, b'R', 1);
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(
            [word(0), word(4), word(8), word(12), word(16)],
            [3, bytes.len() as u32, 3, 2, 2],
            "magic, length, full and high speed, two descriptors each"
        );
        let full = [9, 4, 0, 0, 1, 0xff, b'R', 1, 1, 7, 5, 0x81, 2, 64, 0, 0];
        let high = [9, 4, 0, 0, 1, 0xff, b'R', 1, 1, 7, 5, 0x81, 2, 0, 2, 0];
        assert_eq!(bytes[20..], [full, high].concat());
        let table = strings("r1metal frames");
        assert_eq!(table[..16], le_words(&[2, table.len() as u32, 1, 1])[..]);
        assert_eq!(table[16..], *b"\x09\x04r1metal frames\0", "US English, and the name ended by a zero");
    }

    #[test]
    fn setup_stalls() {
        let event = |request_type: u8, kind: u8| {
            let mut event = [0; EVENT_BYTES];
            (event[0], event[EVENT_TYPE_AT]) = (request_type, kind);
            event
        };
        assert_eq!(stall(&event(0x80, SETUP)), Some(Stall::Read), "one that asks for an answer");
        assert_eq!(stall(&event(0x00, SETUP)), Some(Stall::Write), "one that brings data");
        assert_eq!(stall(&event(0x80, 2)), None, "the function was enabled");
    }
}

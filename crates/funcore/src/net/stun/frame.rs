use super::attribute::WORD;
use super::message::HEAD;
use crate::base::bytes::be16;
use alloc::vec::Vec;

const PREFIX: usize = 2;
// The bytes of ChannelData's head: its channel's number and its data's length (RFC 8656, 12.4).
pub(crate) const CHANNEL_HEAD: usize = 4;
// A ChannelData message's first two bits are 01, a STUN message's 00 (RFC 8656, 12; RFC 8489, 5).
const KIND_BITS: u8 = 0xC0;
const CHANNEL_BITS: u8 = 0x40;

// How messages lie one after another on a stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Framing {
    // Each behind its length in two bytes (RFC 4571, 2), as ICE over TCP frames them (RFC 6544, 7.1).
    Prefixed,
    // STUN messages and ChannelData padded to whole words, each telling its own length (RFC 8656, 12.5).
    Messages,
}

// The bytes of a stream not yet a whole message.
pub(crate) struct Frames {
    framing: Framing,
    held: Vec<u8>,
}

impl Frames {
    pub(crate) const fn new(framing: Framing) -> Frames {
        Frames { framing, held: Vec::new() }
    }

    // The length of the frame at the start of `bytes`, its head included; none until enough is there to tell, and an error for what frames nothing.
    fn length(&self, bytes: &[u8]) -> Result<Option<usize>, ()> {
        match self.framing {
            Framing::Prefixed => Ok(be16(bytes, 0).map(|length| PREFIX + usize::from(length))),
            Framing::Messages => match bytes.first() {
                None => Ok(None),
                Some(first) if first & KIND_BITS == 0 => Ok(be16(bytes, 2).map(|length| HEAD + usize::from(length))),
                Some(first) if first & KIND_BITS == CHANNEL_BITS => {
                    Ok(be16(bytes, 2).map(|length| (CHANNEL_HEAD + usize::from(length)).next_multiple_of(WORD)))
                }
                Some(_) => Err(()),
            },
        }
    }

    // Takes bytes off the stream: the messages they complete, a prefixed one without its prefix; none where the stream frames nothing, and is broken.
    pub(crate) fn take(&mut self, bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
        self.held.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut at = 0;
        while let Some(length) = self.length(&self.held[at..]).ok()? {
            let Some(frame) = self.held.get(at..at + length) else { break };
            let skip = if self.framing == Framing::Prefixed { PREFIX } else { 0 };
            frames.push(frame[skip..].to_vec());
            at += length;
        }
        self.held.drain(..at);
        Some(frames)
    }
}

// A frame for a stream of RFC 4571: its length in two bytes, then the frame; none for one longer than two bytes tell.
pub(crate) fn prefixed(frame: &[u8]) -> Option<Vec<u8>> {
    let length = u16::try_from(frame.len()).ok()?;
    let mut bytes = Vec::with_capacity(PREFIX + frame.len());
    bytes.extend(length.to_be_bytes());
    bytes.extend(frame);
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn prefixed_splits() {
        let mut stream = prefixed(b"first").unwrap();
        stream.extend(prefixed(&[]).unwrap());
        stream.extend(prefixed(&[7; 300]).unwrap());
        for cut in [1, 2, 3, 7, 300] {
            let mut frames = Frames::new(Framing::Prefixed);
            let taken: Vec<Vec<u8>> = stream.chunks(cut).flat_map(|chunk| frames.take(chunk).unwrap()).collect();
            assert_eq!(taken, [b"first".to_vec(), vec![], vec![7; 300]], "in pieces of {cut}");
        }
        assert_eq!(prefixed(&[0; 65_536]), None, "longer than its prefix tells");
    }

    #[test]
    fn message_splits() {
        let message = [0, 1, 0, 4, 0x21, 0x12, 0xA4, 0x42, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 0, 0x24, 0, 0];
        let channel = [0x40, 0x01, 0, 5, 1, 2, 3, 4, 5, 0, 0, 0];
        let stream = [&message[..], &channel, &message].concat();
        for cut in [1, 3, 4, 5, 13, 24] {
            let mut frames = Frames::new(Framing::Messages);
            let taken: Vec<Vec<u8>> = stream.chunks(cut).flat_map(|chunk| frames.take(chunk).unwrap()).collect();
            assert_eq!(taken, [message.to_vec(), channel.to_vec(), message.to_vec()], "in pieces of {cut}");
        }
        let mut frames = Frames::new(Framing::Messages);
        assert_eq!(frames.take(&[0x80, 0, 0, 0]), None, "what is neither a message nor ChannelData breaks the stream");
    }
}

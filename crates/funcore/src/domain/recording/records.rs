use super::{HEAD_BYTES, Head, MAGIC};
use alloc::vec::Vec;

/// Splits a byte stream into records, passing over bytes before a magic.
#[derive(Default)]
pub struct Records {
    pending: Vec<u8>,
}

impl Records {
    /// Adds bytes of the stream.
    pub fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// The next whole record's head and body; none until all of it has come.
    pub fn take(&mut self) -> Option<(Head, Vec<u8>)> {
        loop {
            let start = self.pending.windows(MAGIC.len()).position(|window| window == MAGIC);
            let Some(start) = start else {
                // A magic cut in two at the end keeps its first part.
                let tail = self.pending.len().min(MAGIC.len() - 1);
                self.pending.drain(..self.pending.len() - tail);
                return None;
            };
            self.pending.drain(..start);
            if self.pending.len() < HEAD_BYTES {
                return None;
            }
            let Some(head) = Head::read(&self.pending) else {
                // A magic that begins no valid record was a chance match.
                self.pending.drain(..1);
                continue;
            };
            let end = HEAD_BYTES + head.length as usize;
            if self.pending.len() < end {
                return None;
            }
            let body = self.pending[HEAD_BYTES..end].to_vec();
            self.pending.drain(..end);
            return Some((head, body));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::support::{SIDE, frame, records};
    use super::super::{BLOCK, Decoder, ENTRY_BYTES, Encoder, FRAME_HEAD_BYTES, Kind, Malformed};
    use super::*;
    use crate::gfx::support::RGBX;

    #[test]
    fn splits_stream_into_records() {
        let mut encoder = Encoder::new(SIDE, SIDE, RGBX).unwrap();
        let pixels = frame(RGBX, |_, _| (1, 2, 3));
        let mut stream = b"left over from before R1R".to_vec();
        stream.extend_from_slice(encoder.frame(&pixels, SIDE, 5, 0).unwrap());
        stream.extend_from_slice(b"R1RCX");
        stream.extend_from_slice(encoder.end(9, 0));
        stream.extend_from_slice(b"R1RC\x09");
        stream.extend_from_slice(&[0; HEAD_BYTES]);
        stream.extend_from_slice(encoder.start(12));
        let mut cut = Records::default();
        let mut parsed = Vec::new();
        for piece in stream.chunks(7) {
            cut.push(piece);
            while let Some(record) = cut.take() {
                parsed.push(record);
            }
        }
        assert_eq!(
            parsed.iter().map(|(head, _)| (head.kind, head.time_us)).collect::<Vec<_>>(),
            [(Kind::Frame, 5), (Kind::End, 9), (Kind::Start, 12)],
            "what comes before a record, a magic cut short and one that begins no record are passed over"
        );
        let mut decoder = Decoder::new(SIDE, SIDE).unwrap();
        assert_eq!(decoder.apply(&parsed[0].1), Ok(()));
        assert_eq!(Decoder::new(SIDE, BLOCK).unwrap().apply(&parsed[0].1), Err(Malformed), "a frame of another size");
        assert_eq!(decoder.apply(&parsed[0].1[..5]), Err(Malformed), "a body cut short in its head");
        let mut short = parsed[0].1.clone();
        short.pop();
        assert_eq!(decoder.apply(&short), Err(Malformed), "a block cut short");
        let mut outside = parsed[0].1.clone();
        outside[8] = 9;
        assert_eq!(decoder.apply(&outside), Err(Malformed), "a block outside the frame");
        let mut chance = b"R1RC\x02\0\0\0".to_vec();
        chance.extend_from_slice(&[0; 8]);
        chance.extend_from_slice(&u32::MAX.to_le_bytes());
        chance.extend_from_slice(&[0; 4]);
        chance.extend_from_slice(encoder.start(70));
        assert_eq!(
            records(&chance).iter().map(|(head, _)| (head.kind, head.time_us)).collect::<Vec<_>>(),
            [(Kind::Start, 70)],
            "a head whose length no record of its kind has is a chance match, and the start behind it is not waited for"
        );
        for (kind, length) in [(Kind::Start, 7), (Kind::End, 1), (Kind::Frame, 7), (Kind::Frame, FRAME_HEAD_BYTES + 1)]
        {
            assert!(!kind.fits(length), "{kind:?} of {length} bytes");
        }
        assert!(Kind::Frame.fits(FRAME_HEAD_BYTES + u16::MAX as usize * ENTRY_BYTES), "every block of a frame");
    }
}

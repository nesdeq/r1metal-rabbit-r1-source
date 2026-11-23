use crate::codec::Flaw;

// The rate every frame size is counted at, and the decoder's output.
pub(super) const RATE: u32 = 48_000;
// The most bytes of a frame (3.2.1).
pub(super) const LARGEST_FRAME: usize = 1275;
// 120 ms, the longest a packet lasts (3.2.5), and the frames that many of 2.5 ms are.
const LONGEST_PACKET: usize = 5760;
const MOST_FRAMES: usize = 48;
const CONFIG_SHIFT: u32 = 3;
const STEREO: u8 = 0x04;
const CODE_MASK: u8 = 0x03;
const VBR: u8 = 0x80;
const PADDING: u8 = 0x40;
const COUNT_MASK: u8 = 0x3F;
// A length byte from here on takes a second byte, which counts fourfold (3.2.1).
const TWO_BYTE_LENGTH: usize = 252;
// A padding byte of 255 stands for 254 bytes and another padding byte (3.2.5).
const PADDING_MORE: u8 = 255;
pub(super) const SILK_FRAMES: [usize; 4] = [480, 960, 1920, 2880];
const HYBRID_FRAMES: [usize; 2] = [480, 960];
const CELT_SHORTEST: usize = 120;
// The configurations a first byte tells (3.1), and the bytes before the frames of a packet of code 3, its first and the count (3.2.5).
const CONFIGS: u8 = 32;
const COUNTED_HEAD: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    Silk,
    Hybrid,
    Celt,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Bandwidth {
    Narrow,
    Medium,
    Wide,
    SuperWide,
    Full,
}

// What a packet's first byte tells of its frames (3.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Toc {
    pub(super) mode: Mode,
    pub(super) bandwidth: Bandwidth,
    // A frame's samples at 48 kHz.
    pub(super) frame: usize,
    pub(super) is_stereo: bool,
}

impl Toc {
    pub(super) fn read(byte: u8) -> Toc {
        use Bandwidth::{Full, Medium, Narrow, SuperWide, Wide};
        let config = usize::from(byte >> CONFIG_SHIFT);
        let (mode, bandwidth, frame) = match config {
            0..=11 => (Mode::Silk, [Narrow, Medium, Wide][config / 4], SILK_FRAMES[config % 4]),
            12..=15 => (Mode::Hybrid, [SuperWide, Full][(config - 12) / 2], HYBRID_FRAMES[config % 2]),
            _ => (Mode::Celt, [Narrow, Wide, SuperWide, Full][(config - 16) / 4], CELT_SHORTEST << (config % 4)),
        };
        Toc { mode, bandwidth, frame, is_stereo: byte & STEREO != 0 }
    }

    // The first byte of a packet of such frames, which `code` counts (3.1); none for frames no configuration has.
    fn byte(self, code: u8) -> Option<u8> {
        let config =
            (0..CONFIGS).find(|&config| Toc::read(config << CONFIG_SHIFT) == Toc { is_stereo: false, ..self })?;
        Some(config << CONFIG_SHIFT | if self.is_stereo { STEREO } else { 0 } | code & CODE_MASK)
    }
}

// Lays out a packet of `count` frames of one size, `least` bytes at the least, that fills `packet` to its last byte: code 0 where one frame fills it, else code 3 with the padding that makes it up (3.2.1, 3.2.5), noughts in the padding. Where the first frame begins and the frames' size.
pub(super) fn lay_out(toc: Toc, count: usize, least: usize, packet: &mut [u8]) -> Result<(usize, usize), Flaw> {
    if count == 0 || count * toc.frame > LONGEST_PACKET {
        return Err(Flaw::Invalid("a count of frames"));
    }
    let byte = |code: u8| toc.byte(code).ok_or(Flaw::Invalid("frames no configuration has"));
    let len = packet.len();
    if count == 1 && (least + 1..=LARGEST_FRAME + 1).contains(&len) {
        packet[0] = byte(0)?;
        return Ok((1, len - 1));
    }
    let size = (len.saturating_sub(COUNTED_HEAD) / count).min(LARGEST_FRAME);
    if size < least {
        return Err(Flaw::Short);
    }
    let padding = len - COUNTED_HEAD - count * size;
    packet[0] = byte(CODE_MASK)?;
    packet[1] = count as u8 | if padding > 0 { PADDING } else { 0 };
    let mut at = COUNTED_HEAD;
    if padding > 0 {
        let more = (padding - 1) / usize::from(PADDING_MORE);
        packet[at..at + more].fill(PADDING_MORE);
        packet[at + more] = (padding - 1 - more * usize::from(PADDING_MORE)) as u8;
        at += more + 1;
    }
    packet[at + count * size..].fill(0);
    Ok((at, size))
}

// A packet's frames (3.2); a frame of no bytes was not sent.
pub(super) struct Packet<'a> {
    pub(super) toc: Toc,
    frames: [&'a [u8]; MOST_FRAMES],
    count: usize,
}

impl<'a> Packet<'a> {
    pub(super) fn frames(&self) -> &[&'a [u8]] {
        &self.frames[..self.count]
    }

    // The samples the packet lasts at 48 kHz.
    pub(super) const fn samples(&self) -> usize {
        self.count * self.toc.frame
    }

    pub(super) fn read(packet: &'a [u8]) -> Result<Packet<'a>, Flaw> {
        let (&first, rest) = packet.split_first().ok_or(Flaw::Short)?;
        let mut read = Packet { toc: Toc::read(first), frames: [&[]; MOST_FRAMES], count: 0 };
        match first & CODE_MASK {
            0 => read.push(rest)?,
            1 => {
                if !rest.len().is_multiple_of(2) {
                    return Err(Flaw::Invalid("two frames of one size in an odd count of bytes"));
                }
                let (first, second) = rest.split_at(rest.len() / 2);
                read.push(first)?;
                read.push(second)?;
            }
            2 => {
                let (length, rest) = split_length(rest)?;
                let (first, second) = rest.split_at_checked(length).ok_or(Flaw::Short)?;
                read.push(first)?;
                read.push(second)?;
            }
            _ => read.read_counted(rest)?,
        }
        Ok(read)
    }

    fn read_counted(&mut self, bytes: &'a [u8]) -> Result<(), Flaw> {
        let (&head, mut rest) = bytes.split_first().ok_or(Flaw::Short)?;
        let count = usize::from(head & COUNT_MASK);
        if count == 0 || count * self.toc.frame > LONGEST_PACKET {
            return Err(Flaw::Invalid("a count of frames"));
        }
        if head & PADDING != 0 {
            let mut padding = 0;
            loop {
                let (&byte, after) = rest.split_first().ok_or(Flaw::Short)?;
                rest = after;
                padding += usize::from(byte) - usize::from(byte == PADDING_MORE);
                if byte != PADDING_MORE {
                    break;
                }
            }
            rest = rest.get(..rest.len().checked_sub(padding).ok_or(Flaw::Short)?).ok_or(Flaw::Short)?;
        }
        if head & VBR == 0 {
            if !rest.len().is_multiple_of(count) {
                return Err(Flaw::Invalid("frames of one size in bytes they do not divide"));
            }
            let size = rest.len() / count;
            return (0..count).try_for_each(|at| self.push(&rest[at * size..][..size]));
        }
        let mut lengths = [0; MOST_FRAMES];
        for length in &mut lengths[..count - 1] {
            (*length, rest) = split_length(rest)?;
        }
        for &length in &lengths[..count - 1] {
            let (frame, after) = rest.split_at_checked(length).ok_or(Flaw::Short)?;
            rest = after;
            self.push(frame)?;
        }
        self.push(rest)
    }

    fn push(&mut self, frame: &'a [u8]) -> Result<(), Flaw> {
        if frame.len() > LARGEST_FRAME {
            return Err(Flaw::Invalid("a frame's size"));
        }
        self.frames[self.count] = frame;
        self.count += 1;
        Ok(())
    }
}

// A frame's length as one byte or two say it, and what follows them.
fn split_length(bytes: &[u8]) -> Result<(usize, &[u8]), Flaw> {
    let (&first, rest) = bytes.split_first().ok_or(Flaw::Short)?;
    let first = usize::from(first);
    if first < TWO_BYTE_LENGTH {
        return Ok((first, rest));
    }
    let (&second, rest) = rest.split_first().ok_or(Flaw::Short)?;
    Ok((usize::from(second) * 4 + first, rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn lengths(packet: &[u8]) -> Result<Vec<usize>, Flaw> {
        Packet::read(packet).map(|packet| packet.frames().iter().map(|frame| frame.len()).collect())
    }

    #[test]
    fn toc_configurations() {
        let toc = |config: u8| Toc::read(config << CONFIG_SHIFT);
        assert_eq!(
            [0, 3, 4, 9, 11].map(|config| (toc(config).mode, toc(config).bandwidth, toc(config).frame)),
            [
                (Mode::Silk, Bandwidth::Narrow, 480),
                (Mode::Silk, Bandwidth::Narrow, 2880),
                (Mode::Silk, Bandwidth::Medium, 480),
                (Mode::Silk, Bandwidth::Wide, 960),
                (Mode::Silk, Bandwidth::Wide, 2880),
            ]
        );
        assert_eq!(
            [12, 13, 14, 15].map(|config| (toc(config).mode, toc(config).bandwidth, toc(config).frame)),
            [
                (Mode::Hybrid, Bandwidth::SuperWide, 480),
                (Mode::Hybrid, Bandwidth::SuperWide, 960),
                (Mode::Hybrid, Bandwidth::Full, 480),
                (Mode::Hybrid, Bandwidth::Full, 960),
            ]
        );
        assert_eq!(
            [16, 19, 20, 27, 28, 31].map(|config| (toc(config).mode, toc(config).bandwidth, toc(config).frame)),
            [
                (Mode::Celt, Bandwidth::Narrow, 120),
                (Mode::Celt, Bandwidth::Narrow, 960),
                (Mode::Celt, Bandwidth::Wide, 120),
                (Mode::Celt, Bandwidth::SuperWide, 960),
                (Mode::Celt, Bandwidth::Full, 120),
                (Mode::Celt, Bandwidth::Full, 960),
            ]
        );
        assert!(Toc::read(0xFC).is_stereo && !Toc::read(0xF8).is_stereo);
    }

    #[test]
    fn frames_of_each_code() {
        let toc = 31 << CONFIG_SHIFT;
        assert_eq!(lengths(&[toc]), Ok(vec![0]), "a packet of its first byte alone holds one frame not sent");
        assert_eq!(lengths(&[toc, 1, 2, 3]), Ok(vec![3]));
        assert_eq!(lengths(&[toc | 1, 1, 2, 3, 4]), Ok(vec![2, 2]));
        assert_eq!(lengths(&[toc | 1]), Ok(vec![0, 0]));
        assert_eq!(lengths(&[toc | 2, 1, 9, 8, 7]), Ok(vec![1, 2]));
        assert_eq!(lengths(&[toc | 2, 0]), Ok(vec![0, 0]));
        assert_eq!(lengths(&[toc | 3, 3, 1, 2, 3, 4, 5, 6]), Ok(vec![2, 2, 2]), "three frames of one size");
        assert_eq!(lengths(&[toc | 3, VBR | 3, 1, 0, 9, 8, 7]), Ok(vec![1, 0, 2]), "three of their own sizes");
        assert_eq!(lengths(&[toc | 3, 2]), Ok(vec![0, 0]));
        let bytes = [toc | 3, 3, 1, 2, 3];
        let packet = Packet::read(&bytes).unwrap();
        assert_eq!((packet.samples(), packet.frames()[1]), (2880, &[2][..]));
    }

    #[test]
    fn two_byte_lengths_and_padding() {
        let toc = 31 << CONFIG_SHIFT;
        let mut packet = vec![toc | 2, 252, 1];
        packet.extend([0; 256 + 3]);
        assert_eq!(lengths(&packet), Ok(vec![256, 3]), "252 and a second byte of 1 are 256 bytes");
        let mut largest = vec![toc | 2, 255, 255];
        largest.extend([0; LARGEST_FRAME]);
        assert_eq!(lengths(&largest), Ok(vec![LARGEST_FRAME, 0]));
        assert_eq!(lengths(&[toc | 3, PADDING | 1, 2, 7, 0, 0]), Ok(vec![1]), "two bytes of padding at the end");
        let mut padded = vec![toc | 3, PADDING | 2, 255, 1, 5, 6];
        padded.extend([0; 255]);
        assert_eq!(lengths(&padded), Ok(vec![1, 1]), "a padding byte of 255 counts 254 and has another follow");
        assert_eq!(lengths(&[toc | 3, PADDING | 1, 0, 7]), Ok(vec![1]), "padding of no bytes");
    }

    #[test]
    fn malformed_packets_refused() {
        let toc = 31 << CONFIG_SHIFT;
        assert_eq!(lengths(&[]), Err(Flaw::Short));
        assert_eq!(lengths(&vec![toc; LARGEST_FRAME + 2]).unwrap_err(), Flaw::Invalid("a frame's size"));
        assert_eq!(
            lengths(&[toc | 1, 1, 2, 3]).unwrap_err(),
            Flaw::Invalid("two frames of one size in an odd count of bytes")
        );
        assert_eq!(lengths(&[toc | 2]), Err(Flaw::Short));
        assert_eq!(lengths(&[toc | 2, 5, 1, 2]), Err(Flaw::Short), "a first frame longer than the packet");
        assert_eq!(lengths(&[toc | 2, 252]), Err(Flaw::Short));
        assert_eq!(lengths(&[toc | 3]), Err(Flaw::Short));
        assert_eq!(lengths(&[toc | 3, 0]).unwrap_err(), Flaw::Invalid("a count of frames"));
        assert_eq!(lengths(&[toc | 3, 7, 0, 0, 0, 0, 0, 0, 0]).unwrap_err(), Flaw::Invalid("a count of frames"));
        assert_eq!(
            lengths(&[toc | 3, 3, 1, 2, 3, 4]).unwrap_err(),
            Flaw::Invalid("frames of one size in bytes they do not divide")
        );
        assert_eq!(lengths(&[toc | 3, VBR | 2, 4, 1, 2]), Err(Flaw::Short));
        assert_eq!(lengths(&[toc | 3, PADDING | 1, 9, 1, 2]), Err(Flaw::Short), "more padding than bytes");
        assert_eq!(lengths(&[toc | 3, PADDING | 1, 255]), Err(Flaw::Short));
        let short = 16 << CONFIG_SHIFT;
        assert_eq!(lengths(&[short | 3, 48]), Ok(vec![0; 48]), "120 ms of the shortest frames");
        assert_eq!(lengths(&[short | 3, 49]).unwrap_err(), Flaw::Invalid("a count of frames"));
    }

    #[test]
    fn first_byte_written() {
        for byte in 0..=u8::MAX {
            assert_eq!(Toc::read(byte).byte(byte & CODE_MASK), Some(byte), "the byte of {byte:#04x} as read");
        }
        let medium = Toc { mode: Mode::Celt, bandwidth: Bandwidth::Medium, frame: 960, is_stereo: false };
        assert_eq!(medium.byte(0), None, "CELT has no medium band");
        let odd = Toc { mode: Mode::Celt, bandwidth: Bandwidth::Wide, frame: 1000, is_stereo: false };
        assert_eq!(odd.byte(0), None, "nor frames of 1000 samples");
    }

    #[test]
    fn packets_laid_out_to_their_length() {
        let toc = Toc { mode: Mode::Celt, bandwidth: Bandwidth::Wide, frame: 960, is_stereo: false };
        for count in 1..=6 {
            for len in [3, 4, 2 * count + 1, 2 * count + 2, 50, 80, 237, 239, 240, 241, 242, 1276, 1277, 1500, 7700] {
                let mut packet = vec![0xA5; len];
                let Ok((at, size)) = lay_out(toc, count, 2, &mut packet) else {
                    assert!(len < 2 * count + 2 && (count > 1 || len < 3), "{count} frames in {len} bytes: too few");
                    continue;
                };
                let head = if count == 1 && len <= LARGEST_FRAME + 1 { 1 } else { COUNTED_HEAD };
                assert_eq!(
                    size,
                    ((len - head) / count).min(LARGEST_FRAME),
                    "{count} frames in {len} bytes: the largest"
                );
                packet[at..at + count * size].fill(0x5A);
                let read = Packet::read(&packet).expect("a packet that parses");
                assert_eq!(read.toc, toc);
                assert_eq!(read.frames().len(), count, "{count} frames in {len} bytes");
                for (number, frame) in read.frames().iter().enumerate() {
                    assert_eq!(frame.len(), size);
                    let place = frame.as_ptr() as usize - packet.as_ptr() as usize;
                    assert_eq!(place, at + number * size, "{count} frames in {len} bytes: frame {number} where laid");
                }
                assert!(packet[at + count * size..].iter().all(|&byte| byte == 0), "the padding all noughts");
            }
        }
        let mut packet = [0; 100];
        assert_eq!(lay_out(toc, 7, 2, &mut packet), Err(Flaw::Invalid("a count of frames")), "140 ms");
        assert_eq!(lay_out(toc, 0, 2, &mut packet), Err(Flaw::Invalid("a count of frames")));
    }
}

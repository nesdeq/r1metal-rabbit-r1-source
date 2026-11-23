use super::{SEQUENCE_BYTES, WORD};
use alloc::vec::Vec;
use core::ops::Range;

// The local identifiers Signal's calls negotiate (RingRTC's constants.h).
pub(super) const TRANSPORT_SEQUENCE: u8 = 1;
const ORIENTATION: u8 = 4;
const AUDIO_LEVEL: u8 = 5;
pub(super) const DEPENDENCY_DESCRIPTOR: u8 = 6;
pub(super) const LAYERS_ALLOCATION: u8 = 14;
// RFC 8285, 4.2 and 4.3: the profiles of the two forms, the second with four bits the application keeps.
const ONE_BYTE: u16 = 0xBEDE;
const TWO_BYTE: u16 = 0x1000;
const APPBITS: u16 = 0x000F;
const ID_SHIFT: u8 = 4;
const LENGTH_BITS: u8 = 0x0F;
const PADDING: u8 = 0;
// RFC 8285, 4.2: identifier 15 ends the one-byte form.
const RESERVED: u8 = 15;
const ONE_BYTE_LONGEST: usize = 16;
const TWO_BYTE_LONGEST: usize = u8::MAX as usize;
// RFC 3550, 5.3.1: the profile's 16 bits and the length in words.
pub(super) const HEAD: usize = 4;
pub(super) const LENGTH_AT: usize = 2;
// 3GPP TS 26.114, 7.4.5: the camera, the flip and the rotation in the low four bits.
const CAMERA_BIT: u8 = 0x08;
const FLIP_BIT: u8 = 0x04;
const TURN_BITS: u8 = 0x03;
// RFC 6464, 3: the voice flag over the level in -dBov.
const VOICE_BIT: u8 = 0x80;
const LEVEL_BITS: u8 = 0x7F;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Form {
    OneByte,
    TwoByte,
}

// A packet's header extension read in place.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Extensions<'a> {
    form: Option<Form>,
    bytes: &'a [u8],
}

// The quarter turns clockwise a picture is to be turned by to be shown (the calling server's `VideoRotation`, rtp.rs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Turn {
    None,
    Quarter,
    Half,
    ThreeQuarters,
}

// Coordination of video orientation (3GPP TS 26.114, 7.4.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Orientation {
    pub(crate) turn: Turn,
    pub(crate) is_flipped: bool,
    pub(crate) is_back: bool,
}

// RFC 6464, 3: the level in -dBov, 0 to 127, 127 for silence, and whether the sender heard a voice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct AudioLevel {
    pub(crate) level: u8,
    pub(crate) is_voice: bool,
}

// An element to write, by the identifiers calls negotiate, or by any other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Element<'a> {
    TransportSequence(u16),
    Orientation(Orientation),
    AudioLevel(AudioLevel),
    Other { id: u8, data: &'a [u8] },
}

impl<'a> Extensions<'a> {
    pub(super) fn new(profile: u16, bytes: &'a [u8]) -> Extensions<'a> {
        let form = match profile {
            ONE_BYTE => Some(Form::OneByte),
            two_byte if two_byte & !APPBITS == TWO_BYTE => Some(Form::TwoByte),
            _ => None,
        };
        Extensions { form, bytes }
    }

    // None where the packet has no extension, or one of another profile.
    #[cfg(test)]
    pub(super) fn form(&self) -> Option<Form> {
        self.form
    }

    pub(super) fn elements(&self) -> Elements<'a> {
        Elements { form: self.form, bytes: self.bytes, at: 0 }
    }

    // The data of the first element of an identifier.
    pub(crate) fn get(&self, id: u8) -> Option<&'a [u8]> {
        self.elements().find(|(found, _)| *found == id).map(|(_, data)| data)
    }

    // Where the first element of an identifier lies among the extension's bytes.
    pub(super) fn place(&self, id: u8) -> Option<Range<usize>> {
        let mut elements = self.elements();
        core::iter::from_fn(|| elements.next_place()).find(|(found, _)| *found == id).map(|(_, place)| place)
    }

    pub(super) fn transport_sequence(&self) -> Option<u16> {
        Some(u16::from_be_bytes(*<&[u8; SEQUENCE_BYTES]>::try_from(self.get(TRANSPORT_SEQUENCE)?).ok()?))
    }

    pub(super) fn orientation(&self) -> Option<Orientation> {
        let [byte] = *<&[u8; 1]>::try_from(self.get(ORIENTATION)?).ok()?;
        let turn = [Turn::None, Turn::Quarter, Turn::Half, Turn::ThreeQuarters][usize::from(byte & TURN_BITS)];
        Some(Orientation { turn, is_flipped: byte & FLIP_BIT != 0, is_back: byte & CAMERA_BIT != 0 })
    }

    pub(super) fn audio_level(&self) -> Option<AudioLevel> {
        let [byte] = *<&[u8; 1]>::try_from(self.get(AUDIO_LEVEL)?).ok()?;
        Some(AudioLevel { level: byte & LEVEL_BITS, is_voice: byte & VOICE_BIT != 0 })
    }
}

// The elements of a header extension in order: each identifier and data.
pub(super) struct Elements<'a> {
    form: Option<Form>,
    bytes: &'a [u8],
    at: usize,
}

impl Elements<'_> {
    // The next element's identifier and where its data lies; none at the end, at identifier 15 or a nought with a length in the one-byte form, or at an element cut short (RFC 8285, 4.1.2 and 4.2).
    fn next_place(&mut self) -> Option<(u8, Range<usize>)> {
        let form = self.form?;
        loop {
            let first = *self.bytes.get(self.at)?;
            if first == PADDING {
                self.at += 1;
                continue;
            }
            let (id, length, start) = match form {
                Form::OneByte => (first >> ID_SHIFT, usize::from(first & LENGTH_BITS) + 1, self.at + 1),
                Form::TwoByte => (first, usize::from(*self.bytes.get(self.at + 1)?), self.at + 2),
            };
            let end = start + length;
            if form == Form::OneByte && (id == PADDING || id == RESERVED) || end > self.bytes.len() {
                self.at = self.bytes.len();
                return None;
            }
            self.at = end;
            return Some((id, start..end));
        }
    }
}

impl<'a> Iterator for Elements<'a> {
    type Item = (u8, &'a [u8]);

    fn next(&mut self) -> Option<(u8, &'a [u8])> {
        let (id, place) = self.next_place()?;
        Some((id, &self.bytes[place]))
    }
}

impl Orientation {
    fn to_byte(self) -> u8 {
        let camera = if self.is_back { CAMERA_BIT } else { 0 };
        let flip = if self.is_flipped { FLIP_BIT } else { 0 };
        camera | flip | self.turn as u8
    }
}

// An element's identifier and data, a typed value's written into `inline`; none for a value its element cannot carry.
fn encode<'s>(element: &'s Element<'_>, inline: &'s mut [u8; SEQUENCE_BYTES]) -> Option<(u8, &'s [u8])> {
    match *element {
        Element::TransportSequence(sequence) => {
            inline[..SEQUENCE_BYTES].copy_from_slice(&sequence.to_be_bytes());
            Some((TRANSPORT_SEQUENCE, &inline[..SEQUENCE_BYTES]))
        }
        Element::Orientation(orientation) => {
            inline[0] = orientation.to_byte();
            Some((ORIENTATION, &inline[..1]))
        }
        Element::AudioLevel(AudioLevel { level, is_voice }) => {
            inline[0] = if is_voice { VOICE_BIT } else { 0 } | level;
            (level <= LEVEL_BITS).then_some((AUDIO_LEVEL, &inline[..1]))
        }
        Element::Other { id, data } => Some((id, data)),
    }
}

// Appends a header extension of elements: its profile, its length in words and its elements filled to a whole word, in the one-byte form wherever every element fits it (RFC 8285, 4.1.2); false, appending nothing, where one fits neither form.
pub(super) fn write(elements: &[Element], out: &mut Vec<u8>) -> bool {
    let mut inline = [0u8; SEQUENCE_BYTES];
    let (mut fits_one, mut fits_two) = (true, true);
    for element in elements {
        let Some((id, data)) = encode(element, &mut inline) else {
            return false;
        };
        fits_one &= id != PADDING && id < RESERVED && (1..=ONE_BYTE_LONGEST).contains(&data.len());
        fits_two &= id != PADDING && data.len() <= TWO_BYTE_LONGEST;
    }
    if !fits_one && !fits_two {
        return false;
    }
    let start = out.len();
    out.extend(if fits_one { ONE_BYTE } else { TWO_BYTE }.to_be_bytes());
    out.extend([0; HEAD - LENGTH_AT]);
    for element in elements {
        let Some((id, data)) = encode(element, &mut inline) else {
            continue;
        };
        if fits_one {
            out.push(id << ID_SHIFT | (data.len() - 1) as u8);
        } else {
            out.extend([id, data.len() as u8]);
        }
        out.extend_from_slice(data);
    }
    let body = (out.len() - start - HEAD).next_multiple_of(WORD);
    out.resize(start + HEAD + body, PADDING);
    let Ok(words) = u16::try_from(body / WORD) else {
        out.truncate(start);
        return false;
    };
    out[start + LENGTH_AT..start + HEAD].copy_from_slice(&words.to_be_bytes());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn elements(profile: u16, bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        Extensions::new(profile, bytes).elements().map(|(id, data)| (id, data.to_vec())).collect()
    }

    #[test]
    fn one_byte_form() {
        let bytes = [0x10, 0xAA, 0x21, 0xBB, 0xCC, 0x00, 0x00, 0x33, 0x01, 0x02, 0x03, 0x04];
        assert_eq!(
            elements(ONE_BYTE, &bytes),
            vec![(1, vec![0xAA]), (2, vec![0xBB, 0xCC]), (3, vec![1, 2, 3, 4])],
            "RFC 8285's figure in 4.2: three elements of one, two and four bytes, padding between"
        );
        assert_eq!(elements(ONE_BYTE, &[0x10, 1, 0xF0, 0x20, 2]), vec![(1, vec![1])], "identifier 15 ends it");
        assert_eq!(
            elements(ONE_BYTE, &[0x10, 1, 0x02, 0x20, 2, 3, 4]),
            vec![(1, vec![1])],
            "a nought with a length ends it"
        );
        assert_eq!(elements(ONE_BYTE, &[0x10, 1, 0x23, 2, 3]), vec![(1, vec![1])], "an element cut short ends it");
        assert_eq!(elements(ONE_BYTE, &[0x1F; 1]), vec![], "sixteen bytes announced, none there");
        let sixteen: Vec<u8> = [0x1F].into_iter().chain(0..16).collect();
        assert_eq!(elements(ONE_BYTE, &sixteen), vec![(1, (0..16).collect())], "the longest element");
        assert_eq!(elements(0x1234, &bytes), vec![], "another profile's extension has no elements");
    }

    #[test]
    fn two_byte_form() {
        let bytes = [0x01, 0x00, 0x02, 0x01, 0xAA, 0x00, 0x03, 0x04, 1, 2, 3, 4];
        assert_eq!(
            elements(0x1000, &bytes),
            vec![(1, vec![]), (2, vec![0xAA]), (3, vec![1, 2, 3, 4])],
            "RFC 8285's figure in 4.3: an empty element, padding between"
        );
        assert_eq!(elements(0x100F, &bytes).len(), 3, "the application's four bits are no matter");
        assert_eq!(elements(0x1000, &[0xFF, 0x02, 9]), vec![], "cut short");
        assert_eq!(elements(0x1000, &[0x0F, 0x01, 9, 0x10, 0x00]), vec![(15, vec![9]), (16, vec![])], "any identifier");
    }

    #[test]
    fn typed_elements() {
        let extensions = Extensions::new(ONE_BYTE, &[0x11, 0x0B, 0x0C, 0x40, 0x0D, 0x50, 0xFF, 0xC2, 0x12, 0x34, 0x56]);
        assert_eq!(extensions.transport_sequence(), Some(0x0B0C));
        assert_eq!(
            extensions.orientation(),
            Some(Orientation { turn: Turn::Quarter, is_flipped: true, is_back: true }),
            "C, F and R of 3GPP's byte"
        );
        assert_eq!(extensions.audio_level(), Some(AudioLevel { level: 127, is_voice: true }));
        assert_eq!(extensions.get(9), None);
        let wrong = Extensions::new(ONE_BYTE, &[0x10, 0x0B, 0x41, 1, 2]);
        assert_eq!((wrong.transport_sequence(), wrong.orientation()), (None, None), "elements of another length");
    }

    #[test]
    fn written_and_read_back() {
        let orientation = Orientation { turn: Turn::ThreeQuarters, is_flipped: false, is_back: true };
        let level = AudioLevel { level: 30, is_voice: false };
        let small = [
            Element::TransportSequence(0xBEEF),
            Element::Orientation(orientation),
            Element::AudioLevel(level),
            Element::Other { id: 6, data: &[1, 2, 3] },
        ];
        let mut out = vec![0xEE];
        assert!(write(&small, &mut out));
        assert_eq!(
            out,
            [0xEE, 0xBE, 0xDE, 0, 3, 0x11, 0xBE, 0xEF, 0x40, 0x0B, 0x50, 30, 0x62, 1, 2, 3, 0],
            "the one-byte form, 11 bytes filled to three words"
        );
        let read = Extensions::new(ONE_BYTE, &out[5..]);
        assert_eq!(
            (read.transport_sequence(), read.orientation(), read.audio_level(), read.get(6)),
            (Some(0xBEEF), Some(orientation), Some(level), Some(&[1u8, 2, 3][..]))
        );
        let long = [0x5A; 17];
        let wide = [Element::TransportSequence(7), Element::Other { id: DEPENDENCY_DESCRIPTOR, data: &long }];
        let mut out = Vec::new();
        assert!(write(&wide, &mut out));
        assert_eq!(out[..8], [0x10, 0x00, 0, 6, 1, 2, 0, 7], "seventeen bytes take the two-byte form");
        assert_eq!(out.len(), 4 + 24, "4 + 19 bytes filled to six words");
        let read = Extensions::new(0x1000, &out[4..]);
        assert_eq!((read.transport_sequence(), read.get(DEPENDENCY_DESCRIPTOR)), (Some(7), Some(&long[..])));
        let empty = [Element::Other { id: 20, data: &[] }];
        let mut out = Vec::new();
        assert!(write(&empty, &mut out) && out == [0x10, 0, 0, 1, 20, 0, 0, 0], "identifier 20, empty");
        for refused in [
            Element::Other { id: 0, data: &[1] },
            Element::Other { id: 1, data: &[0; 256] },
            Element::AudioLevel(AudioLevel { level: 128, is_voice: false }),
        ] {
            let mut out = vec![9];
            assert!(!write(&[refused], &mut out) && out == [9], "{refused:?} fits no form");
        }
    }
}

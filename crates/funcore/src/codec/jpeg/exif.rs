use crate::base::bytes::{be16, be32, le16, le32};
use crate::base::sampling::{Turn, turn_words};
use alloc::vec::Vec;
use core::ops::RangeInclusive;

// The segment EXIF lies in, and what its data begins with (CIPA DC-008, 4.5.4).
pub(super) const APP1: u8 = 0xE1;
const EXIF: &[u8] = b"Exif\0\0";
// The byte orders of a TIFF header, the number after them, and the tag of the orientation and its type, a short.
const LITTLE: &[u8] = b"II";
const BIG: &[u8] = b"MM";
const TIFF: u16 = 42;
const ORIENTATION: u16 = 0x0112;
const SHORT: u16 = 3;
const ENTRY_BYTES: usize = 12;
const VALUE_AT: usize = 8;
pub(crate) const UPRIGHT: u8 = 1;
pub(crate) const MIRRORED: u8 = 2;
// The orientations, 1 to 8 (CIPA DC-008, 4.6.4 A).
const ORIENTATIONS: RangeInclusive<u8> = UPRIGHT..=8;

fn first_orientation(tiff: &[u8]) -> Option<u8> {
    let is_little = match tiff.get(..2)? {
        LITTLE => true,
        BIG => false,
        _ => return None,
    };
    let short = |at: usize| if is_little { le16(tiff, at) } else { be16(tiff, at) };
    let long = |at: usize| if is_little { le32(tiff, at) } else { be32(tiff, at) };
    if short(2)? != TIFF {
        return None;
    }
    let first = usize::try_from(long(4)?).ok()?;
    let entries = usize::from(short(first)?);
    let value = (0..entries).map(|entry| first + 2 + entry * ENTRY_BYTES).find_map(|at| {
        (short(at)? == ORIENTATION && short(at + 2)? == SHORT).then(|| short(at + VALUE_AT)).flatten()
    })?;
    u8::try_from(value).ok()
}

// The orientation the EXIF of an APP1 segment tells, 1 to 8, upright where it tells none or another; none where the segment holds no EXIF.
pub(super) fn orientation(body: &[u8]) -> Option<u8> {
    let tiff = body.strip_prefix(EXIF)?;
    Some(first_orientation(tiff).filter(|told| ORIENTATIONS.contains(told)).unwrap_or(UPRIGHT))
}

// The turn that stands a picture stored in that orientation upright (CIPA DC-008, 4.6.4 A).
const fn to_turn(orientation: u8) -> Turn {
    let (quarters, is_mirrored) = match orientation {
        MIRRORED => (0, true),
        3 => (2, false),
        4 => (2, true),
        5 => (1, true),
        6 => (1, false),
        7 => (3, true),
        8 => (3, false),
        _ => (0, false),
    };
    Turn { quarters, is_mirrored }
}

pub(crate) fn shown_size((width, height): (usize, usize), orientation: u8) -> (usize, usize) {
    if to_turn(orientation).is_sideways() { (height, width) } else { (width, height) }
}

/// The words of a picture `width` across as it stands once oriented, mirrored or turned as the tag says, and how many to a line.
pub fn oriented(words: Vec<u32>, width: usize, orientation: u8) -> (Vec<u32>, usize) {
    if orientation == UPRIGHT {
        return (words, width);
    }
    let mut turned = Vec::with_capacity(words.len());
    let across = turn_words(&words, width, to_turn(orientation), &mut turned);
    (turned, across)
}

#[cfg(test)]
pub(super) mod samples {
    use super::*;

    // An APP1 segment's body: EXIF whose first IFD holds a make and the orientation.
    pub(in crate::codec::jpeg) fn exif_body(orientation: u16, is_little: bool) -> Vec<u8> {
        let short = |value: u16| if is_little { value.to_le_bytes() } else { value.to_be_bytes() };
        let long = |value: u32| if is_little { value.to_le_bytes() } else { value.to_be_bytes() };
        let entry = |tag: u16, kind: u16, value: [u8; 4]| [&short(tag)[..], &short(kind), &long(1), &value].concat();
        let order = if is_little { LITTLE } else { BIG };
        let value = [short(orientation), [0, 0]].concat().try_into().unwrap();
        [
            EXIF,
            order,
            &short(TIFF),
            &long(8),
            &short(2),
            &entry(0x010F, 2, [b'R', b'1', 0, 0]),
            &entry(ORIENTATION, SHORT, value),
        ]
        .concat()
    }
}

#[cfg(test)]
mod tests {
    use super::samples::exif_body;
    use super::*;
    use alloc::vec;

    #[test]
    fn orientation_in_either_byte_order() {
        assert_eq!([orientation(&exif_body(6, true)), orientation(&exif_body(8, false))], [Some(6), Some(8)]);
        assert_eq!(orientation(&exif_body(9, true)), Some(UPRIGHT), "a value that is none");
        assert_eq!(orientation(&exif_body(0, false)), Some(UPRIGHT), "nought is none");
        assert_eq!(orientation(b"http://ns.adobe.com/xap/1.0/\0"), None, "a segment of no EXIF");
        assert_eq!(orientation(&exif_body(6, true)[..12]), Some(UPRIGHT), "EXIF cut short tells none");
    }

    #[test]
    fn eight_orientations() {
        let stored = [1, 2, 3, 4, 5, 6];
        let stands = |orientation| oriented(stored.to_vec(), 3, orientation);
        assert_eq!(stands(1), (vec![1, 2, 3, 4, 5, 6], 3));
        assert_eq!(stands(2), (vec![3, 2, 1, 6, 5, 4], 3), "mirrored");
        assert_eq!(stands(3), (vec![6, 5, 4, 3, 2, 1], 3), "turned a half");
        assert_eq!(stands(4), (vec![4, 5, 6, 1, 2, 3], 3), "mirrored top to bottom");
        assert_eq!(stands(5), (vec![1, 4, 2, 5, 3, 6], 2), "about its diagonal");
        assert_eq!(stands(6), (vec![4, 1, 5, 2, 6, 3], 2), "turned a quarter clockwise");
        assert_eq!(stands(7), (vec![6, 3, 5, 2, 4, 1], 2), "about its other diagonal");
        assert_eq!(stands(8), (vec![3, 6, 2, 5, 1, 4], 2), "turned a quarter back");
        assert_eq!([shown_size((3, 2), 6), shown_size((3, 2), 3)], [(2, 3), (3, 2)]);
    }
}

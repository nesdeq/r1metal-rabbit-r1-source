use super::fse::OFFSETS;
use alloc::vec;
use alloc::vec::Vec;

const TABLE_LOG: u32 = 17;
const LEAST_MATCH: usize = 5;
const SKIP_SHIFT: u32 = 8;
const PRIME: u64 = 889_523_592_379;
const NOWHERE: u32 = u32::MAX;
pub(super) const REACH: usize = NOWHERE as usize;
const OFFSET_BIAS: u32 = 3;
const LAST_OFFSET: u32 = 1;
const FARTHEST: usize = (1 << OFFSETS.len()) - OFFSET_BIAS as usize - 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Sequence {
    pub(super) literals: u32,
    pub(super) offset: u32,
    pub(super) length: u32,
}

fn hash(bytes: &[u8]) -> usize {
    let value = bytes.iter().rev().fold(0u64, |value, &byte| value << 8 | u64::from(byte));
    ((value << 24).wrapping_mul(PRIME) >> (64 - TABLE_LOG)) as usize
}

fn match_length(input: &[u8], at: usize, source: usize, end: usize) -> usize {
    input[at..end].iter().zip(&input[source..]).take_while(|(a, b)| a == b).count()
}

pub(super) struct Finder {
    table: Vec<u32>,
}

impl Finder {
    pub(super) fn new() -> Finder {
        Finder { table: vec![NOWHERE; 1 << TABLE_LOG] }
    }

    fn index_places(&mut self, input: &[u8], start: usize, stop: usize, end: usize) {
        for at in start..stop.min(end.saturating_sub(LEAST_MATCH - 1)) {
            self.table[hash(&input[at..at + LEAST_MATCH])] = at as u32;
        }
    }

    pub(super) fn find(&mut self, input: &[u8], start: usize, end: usize, offsets: &mut [u32; 3]) -> Vec<Sequence> {
        let mut sequences = Vec::new();
        let (mut anchor, mut at) = (start, start);
        while at + LEAST_MATCH <= end {
            let candidate = core::mem::replace(&mut self.table[hash(&input[at..at + LEAST_MATCH])], at as u32);
            let last = offsets[0] as usize;
            if at > anchor && last <= at && input[at - last..at - last + LEAST_MATCH] == input[at..at + LEAST_MATCH] {
                let length = LEAST_MATCH + match_length(input, at + LEAST_MATCH, at - last + LEAST_MATCH, end);
                sequences.push(Sequence { literals: (at - anchor) as u32, offset: LAST_OFFSET, length: length as u32 });
                self.index_places(input, at + 1, at + length, end);
                (at, anchor) = (at + length, at + length);
                continue;
            }
            if candidate != NOWHERE
                && at - candidate as usize <= FARTHEST
                && input[candidate as usize..candidate as usize + LEAST_MATCH] == input[at..at + LEAST_MATCH]
            {
                let (mut position, mut source) = (at, candidate as usize);
                let mut length = LEAST_MATCH + match_length(input, at + LEAST_MATCH, source + LEAST_MATCH, end);
                while position > anchor && source > 0 && input[position - 1] == input[source - 1] {
                    (position, source, length) = (position - 1, source - 1, length + 1);
                }
                let offset = (position - source) as u32;
                sequences.push(Sequence {
                    literals: (position - anchor) as u32,
                    offset: offset + OFFSET_BIAS,
                    length: length as u32,
                });
                *offsets = [offset, offsets[0], offsets[1]];
                self.index_places(input, at + 1, position + length, end);
                (at, anchor) = (position + length, position + length);
                continue;
            }
            at += ((at - anchor) >> SKIP_SHIFT) + 1;
        }
        sequences
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_all(input: &[u8]) -> (Vec<Sequence>, [u32; 3]) {
        let mut offsets = [1, 4, 8];
        (Finder::new().find(input, 0, input.len(), &mut offsets), offsets)
    }

    #[test]
    fn repeats_and_last_offset() {
        assert_eq!(
            find_all(b"abcdefghij_abcdefghij_abcdefghij_xyz"),
            (vec![Sequence { literals: 11, offset: 11 + OFFSET_BIAS, length: 22 }], [11, 1, 4]),
            "the second and third copies are one match of the first at its distance, the text that overlaps it included, its offset first and the others shifted back"
        );
        let (sequences, offsets) = find_all(b"0123456789-0123456789+0123456789");
        assert_eq!(
            sequences,
            [
                Sequence { literals: 11, offset: 11 + OFFSET_BIAS, length: 10 },
                Sequence { literals: 1, offset: LAST_OFFSET, length: 10 }
            ],
            "after a literal the same offset again is the last one, told as 1"
        );
        assert_eq!(offsets, [11, 1, 4], "which leaves the offsets as they were");
        assert!(find_all(b"abcd").0.is_empty(), "nothing shorter than five bytes matches");
    }

    #[test]
    fn farthest_offset_code() {
        let code = |distance: usize| 31 - (distance as u32 + OFFSET_BIAS).leading_zeros();
        assert_eq!(code(FARTHEST), OFFSETS.len() as u32 - 1, "the last code there is");
        assert_eq!(code(FARTHEST + 1), OFFSETS.len() as u32, "one further has none");
    }
}

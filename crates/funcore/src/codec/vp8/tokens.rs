use super::frame::{CHROMA_ACROSS, CHROMA_SUBBLOCKS, SUBBLOCKS, SUBBLOCKS_ACROSS};
use super::tables::{
    BLOCK_TYPES, COEFF_BANDS, COEFF_BANDS_OF, CoeffProbs, DCT_CAT_BASE, ENTROPY_NODES, PCAT1, PCAT2, PCAT3, PCAT4,
    PCAT5, PCAT6, PREV_COEFF_CONTEXTS, ZIGZAG,
};
use crate::codec::boolean::{BoolDecoder, BoolEncoder, EVEN};

// The block types that index the coefficient probabilities (RFC 6386, 13.3).
const Y_AFTER_Y2: usize = 0;
const Y2: usize = 1;
const CHROMA: usize = 2;
const Y_WITH_DC: usize = 3;
// The nodes of the coefficient tree, by the probability each is read with (RFC 6386, 13.2).
const EOB_NODE: usize = 0;
const ZERO_NODE: usize = 1;
const ONE_NODE: usize = 2;
const LOW_VALUE_NODE: usize = 3;
const TWO_NODE: usize = 4;
const THREE_NODE: usize = 5;
const HIGH_LOW_NODE: usize = 6;
const CAT_ONE_NODE: usize = 7;
const CAT_THREE_FOUR_NODE: usize = 8;
const CAT_THREE_NODE: usize = 9;
const CAT_FIVE_NODE: usize = 10;
// The contexts after a token: a zero, a one, or more (RFC 6386, 13.3).
const AFTER_ONE: usize = 1;
const AFTER_MORE: usize = 2;
pub(super) const COEFFICIENTS: usize = 16;
const LAST: usize = COEFFICIENTS - 1;
// A macroblock's blocks in their order: the luma subblocks, those of U and of V, and the second-order Y2.
pub(super) const U_FIRST: usize = SUBBLOCKS;
pub(super) const V_FIRST: usize = U_FIRST + CHROMA_SUBBLOCKS;
pub(super) const Y2_BLOCK: usize = V_FIRST + CHROMA_SUBBLOCKS;
const BLOCKS: usize = Y2_BLOCK + 1;
// The places of a macroblock's "has coefficients" contexts: four luma, two each of U and V, and Y2 (RFC 6386, 13.3).
const U_CONTEXT: usize = SUBBLOCKS_ACROSS;
const V_CONTEXT: usize = U_CONTEXT + CHROMA_ACROSS;
const Y2_CONTEXT: usize = V_CONTEXT + CHROMA_ACROSS;
pub(super) const CONTEXTS: usize = Y2_CONTEXT + 1;

// The dequantisation factors of a segment, of the DC and of the other coefficients (RFC 6386, 14.1).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Factors {
    pub(super) y1: [i32; 2],
    pub(super) y2: [i32; 2],
    pub(super) uv: [i32; 2],
}

// A macroblock's dequantised coefficients in raster order, and the place after each block's last that is not nought.
#[derive(Default)]
pub(super) struct Residue {
    pub(super) blocks: [[i16; COEFFICIENTS]; BLOCKS],
    pub(super) ends: [u8; BLOCKS],
}

impl Residue {
    // Clears what the last macroblock left, block by block.
    pub(super) fn clear(&mut self) {
        for (block, end) in self.blocks.iter_mut().zip(&mut self.ends) {
            if *end != 0 {
                *block = [0; COEFFICIENTS];
                *end = 0;
            }
        }
    }
}

#[inline]
fn extra(decoder: &mut BoolDecoder<'_>, probabilities: &[u8]) -> i32 {
    probabilities.iter().fold(0, |value, &probability| value << 1 | i32::from(decoder.read(probability)))
}

// The magnitude of a token that is not zero, and the context it leaves (RFC 6386, 13.2).
#[inline]
fn read_value(decoder: &mut BoolDecoder<'_>, probabilities: &[u8; ENTROPY_NODES]) -> (i32, usize) {
    if !decoder.read(probabilities[ONE_NODE]) {
        return (1, AFTER_ONE);
    }
    let value = if !decoder.read(probabilities[LOW_VALUE_NODE]) {
        if !decoder.read(probabilities[TWO_NODE]) {
            2
        } else if !decoder.read(probabilities[THREE_NODE]) {
            3
        } else {
            4
        }
    } else if !decoder.read(probabilities[HIGH_LOW_NODE]) {
        if !decoder.read(probabilities[CAT_ONE_NODE]) {
            DCT_CAT_BASE[0] + extra(decoder, &PCAT1)
        } else {
            DCT_CAT_BASE[1] + extra(decoder, &PCAT2)
        }
    } else if !decoder.read(probabilities[CAT_THREE_FOUR_NODE]) {
        if !decoder.read(probabilities[CAT_THREE_NODE]) {
            DCT_CAT_BASE[2] + extra(decoder, &PCAT3)
        } else {
            DCT_CAT_BASE[3] + extra(decoder, &PCAT4)
        }
    } else if !decoder.read(probabilities[CAT_FIVE_NODE]) {
        DCT_CAT_BASE[4] + extra(decoder, &PCAT5)
    } else {
        DCT_CAT_BASE[5] + extra(decoder, &PCAT6)
    };
    (value, AFTER_MORE)
}

// A block's tokens from `first` on, dequantised: whether any came before its end, and that end (RFC 6386, 13).
#[inline]
fn read_block(
    decoder: &mut BoolDecoder<'_>,
    probabilities: &[[[u8; ENTROPY_NODES]; PREV_COEFF_CONTEXTS]; COEFF_BANDS],
    context: usize,
    first: usize,
    factors: [i32; 2],
    block: &mut [i16; COEFFICIENTS],
) -> (bool, u8) {
    let mut place = first;
    let mut node = &probabilities[usize::from(COEFF_BANDS_OF[place])][context];
    if !decoder.read(node[EOB_NODE]) {
        return (false, 0);
    }
    let mut end = 0;
    loop {
        // A zero cannot be followed by the end of the block: the next token is read without asking for it.
        if !decoder.read(node[ZERO_NODE]) {
            if place == LAST {
                return (true, end);
            }
            place += 1;
            node = &probabilities[usize::from(COEFF_BANDS_OF[place])][0];
            continue;
        }
        let (magnitude, next) = read_value(decoder, node);
        let value = if decoder.flag() { -magnitude } else { magnitude };
        // The product is a 16-bit word, which a damaged frame's values overflow and wrap (RFC 6386, 14.1).
        block[usize::from(ZIGZAG[place])] = (value * factors[usize::from(place > 0)]) as i16;
        end = place as u8 + 1;
        if place == LAST {
            return (true, end);
        }
        place += 1;
        node = &probabilities[usize::from(COEFF_BANDS_OF[place])][next];
        if !decoder.read(node[EOB_NODE]) {
            return (true, end);
        }
    }
}

// A macroblock's blocks in the order their tokens go, each with its block type, the place its tokens begin at, and its places in the contexts left of and above it (RFC 6386, 13.3).
fn blocks(has_y2: bool) -> impl Iterator<Item = (usize, usize, usize, (usize, usize))> {
    let (kind, first) = if has_y2 { (Y_AFTER_Y2, 1) } else { (Y_WITH_DC, 0) };
    let y2 = has_y2.then_some((Y2_BLOCK, Y2, 0, (Y2_CONTEXT, Y2_CONTEXT)));
    let luma =
        (0..SUBBLOCKS).map(move |block| (block, kind, first, (block / SUBBLOCKS_ACROSS, block % SUBBLOCKS_ACROSS)));
    let chroma = [(U_CONTEXT, U_FIRST), (V_CONTEXT, V_FIRST)].into_iter().flat_map(|(context, start)| {
        (0..CHROMA_SUBBLOCKS).map(move |block| {
            (start + block, CHROMA, 0, (context + block / CHROMA_ACROSS, context + block % CHROMA_ACROSS))
        })
    });
    y2.into_iter().chain(luma).chain(chroma)
}

// A macroblock's tokens in the contexts left of and above it, which it updates: whether any block had a token.
pub(super) fn read_macroblock(
    decoder: &mut BoolDecoder<'_>,
    probabilities: &CoeffProbs,
    has_y2: bool,
    factors: &Factors,
    (left, above): (&mut [u8; CONTEXTS], &mut [u8; CONTEXTS]),
    residue: &mut Residue,
) -> bool {
    let mut is_coded = false;
    for (block, kind, first, places) in blocks(has_y2) {
        let factors = match kind {
            Y2 => factors.y2,
            CHROMA => factors.uv,
            _ => factors.y1,
        };
        let context = usize::from(left[places.0] + above[places.1]);
        let (has_tokens, end) =
            read_block(decoder, &probabilities[kind], context, first, factors, &mut residue.blocks[block]);
        left[places.0] = u8::from(has_tokens);
        above[places.1] = u8::from(has_tokens);
        residue.ends[block] = end;
        is_coded |= has_tokens;
    }
    is_coded
}

// The contexts a coded macroblock of `levels` leaves, as its tokens leave them: each block's, whether it has a token past its first place.
pub(super) fn contexts(has_y2: bool, levels: &Levels, (left, above): (&mut [u8; CONTEXTS], &mut [u8; CONTEXTS])) {
    for (block, _, first, places) in blocks(has_y2) {
        let has_tokens = u8::from(usize::from(levels.ends[block]) > first);
        (left[places.0], above[places.1]) = (has_tokens, has_tokens);
    }
}

// The contexts a skipped macroblock leaves: no coefficients, and Y2's left as it was where the macroblock has none.
pub(super) fn skip(has_y2: bool, (left, above): (&mut [u8; CONTEXTS], &mut [u8; CONTEXTS])) {
    left[..Y2_CONTEXT].fill(0);
    above[..Y2_CONTEXT].fill(0);
    if has_y2 {
        left[Y2_CONTEXT] = 0;
        above[Y2_CONTEXT] = 0;
    }
}

// A macroblock's quantised levels in zigzag order by block, and the place after each block's last that is not nought.
#[derive(Clone, Default)]
pub(super) struct Levels {
    pub(super) blocks: [[i16; COEFFICIENTS]; BLOCKS],
    pub(super) ends: [u8; BLOCKS],
}

impl Levels {
    // Whether a block has a level to code: the macroblock is skipped where none has.
    pub(super) fn is_coded(&self, has_y2: bool) -> bool {
        self.ends[..Y2_BLOCK].iter().any(|&end| end > 0) || has_y2 && self.ends[Y2_BLOCK] > 0
    }
}

// Where the bools of tokens go: into a partition, or counted by the node of the coefficient tree they decide.
pub(super) trait Sink {
    // A bool at `node` of the probabilities of block type `kind`, band `band` and context `context`.
    fn node(&mut self, kind: usize, band: usize, context: usize, node: usize, bit: bool);

    // A bool of a fixed probability: an extra bit or a sign.
    fn fixed(&mut self, probability: u8, bit: bool);
}

// Writes the bools of tokens with the frame's coefficient probabilities.
pub(super) struct Writer<'a> {
    pub(super) encoder: &'a mut BoolEncoder,
    pub(super) probabilities: &'a CoeffProbs,
}

impl Sink for Writer<'_> {
    #[inline]
    fn node(&mut self, kind: usize, band: usize, context: usize, node: usize, bit: bool) {
        self.encoder.write(self.probabilities[kind][band][context][node], bit);
    }

    #[inline]
    fn fixed(&mut self, probability: u8, bit: bool) {
        self.encoder.write(probability, bit);
    }
}

// How often each node of the coefficient tree took each branch, by block type, band and context.
pub(super) type Counts = [[[[[u32; 2]; ENTROPY_NODES]; PREV_COEFF_CONTEXTS]; COEFF_BANDS]; BLOCK_TYPES];

impl Sink for Counts {
    #[inline]
    fn node(&mut self, kind: usize, band: usize, context: usize, node: usize, bit: bool) {
        self[kind][band][context][node][usize::from(bit)] += 1;
    }

    fn fixed(&mut self, _: u8, _: bool) {}
}

// The tokens of a magnitude past nought: the branches of read_value, then its category's extra bits (RFC 6386, 13.2).
#[inline]
fn write_value<S: Sink>(sink: &mut S, (kind, band, context): (usize, usize, usize), magnitude: i32) {
    let mut node = |node: usize, bit: bool| sink.node(kind, band, context, node, bit);
    node(ONE_NODE, magnitude > 1);
    if magnitude == 1 {
        return;
    }
    node(LOW_VALUE_NODE, magnitude > DCT_CAT_BASE[0] - 1);
    if magnitude < DCT_CAT_BASE[0] {
        node(TWO_NODE, magnitude > 2);
        if magnitude > 2 {
            node(THREE_NODE, magnitude > 3);
        }
        return;
    }
    let category = DCT_CAT_BASE.iter().rposition(|&base| magnitude >= base).unwrap_or(0);
    node(HIGH_LOW_NODE, category >= 2);
    match category {
        0 | 1 => node(CAT_ONE_NODE, category == 1),
        2 | 3 => {
            node(CAT_THREE_FOUR_NODE, false);
            node(CAT_THREE_NODE, category == 3);
        }
        _ => {
            node(CAT_THREE_FOUR_NODE, true);
            node(CAT_FIVE_NODE, category == 5);
        }
    }
    let extra: &[u8] = [&PCAT1[..], &PCAT2, &PCAT3, &PCAT4, &PCAT5, &PCAT6][category];
    let value = magnitude - DCT_CAT_BASE[category];
    for (bit, &probability) in extra.iter().enumerate() {
        sink.fixed(probability, value >> (extra.len() - 1 - bit) & 1 == 1);
    }
}

// A block's tokens from `first` on, as read_block reads them: whether it had any before its end.
fn write_block<S: Sink>(
    sink: &mut S,
    kind: usize,
    context: usize,
    first: usize,
    levels: &[i16; COEFFICIENTS],
    end: u8,
) -> bool {
    let end = usize::from(end);
    let band = |place: usize| usize::from(COEFF_BANDS_OF[place]);
    sink.node(kind, band(first), context, EOB_NODE, end > first);
    if end <= first {
        return false;
    }
    let (mut place, mut context) = (first, context);
    loop {
        let level = i32::from(levels[place]);
        // A zero is never the last token: the one after it is read without asking for the end.
        sink.node(kind, band(place), context, ZERO_NODE, level != 0);
        if level == 0 {
            (place, context) = (place + 1, 0);
            continue;
        }
        write_value(sink, (kind, band(place), context), level.abs());
        sink.fixed(EVEN, level < 0);
        context = if level.abs() == 1 { AFTER_ONE } else { AFTER_MORE };
        place += 1;
        if place == COEFFICIENTS {
            return true;
        }
        sink.node(kind, band(place), context, EOB_NODE, place < end);
        if place == end {
            return true;
        }
    }
}

// A macroblock's tokens in the contexts left of and above it, which it updates as read_macroblock does.
pub(super) fn write_macroblock<S: Sink>(
    sink: &mut S,
    has_y2: bool,
    levels: &Levels,
    (left, above): (&mut [u8; CONTEXTS], &mut [u8; CONTEXTS]),
) {
    for (block, kind, first, places) in blocks(has_y2) {
        let context = usize::from(left[places.0] + above[places.1]);
        let has_tokens = write_block(sink, kind, context, first, &levels.blocks[block], levels.ends[block]);
        left[places.0] = u8::from(has_tokens);
        above[places.1] = u8::from(has_tokens);
    }
}

#[cfg(test)]
mod tests {
    use super::super::tables::DEFAULT_COEFF_PROBS;
    use super::*;

    const FACTORS: Factors = Factors { y1: [3, 5], y2: [7, 11], uv: [2, 13] };

    fn write_value(encoder: &mut BoolEncoder, node: &[u8; ENTROPY_NODES], value: i32) {
        let magnitude = value.unsigned_abs();
        encoder.write(node[ONE_NODE], magnitude > 1);
        if magnitude > 1 {
            // Values 2 to 4 alone are written here: the low-value branch.
            encoder.write(node[LOW_VALUE_NODE], false);
            encoder.write(node[TWO_NODE], magnitude > 2);
            if magnitude > 2 {
                encoder.write(node[THREE_NODE], magnitude > 3);
            }
        }
        encoder.write(128, value < 0);
    }

    #[test]
    fn block_tokens_and_contexts() {
        let probabilities = &DEFAULT_COEFF_PROBS[Y_WITH_DC];
        let band = |place: usize| usize::from(COEFF_BANDS_OF[place]);
        let mut encoder = BoolEncoder::new();
        // A 3 at place 0 with no neighbours, a zero, a -1 at place 2, then the end of the block.
        encoder.write(probabilities[band(0)][0][EOB_NODE], true);
        encoder.write(probabilities[band(0)][0][ZERO_NODE], true);
        write_value(&mut encoder, &probabilities[band(0)][0], 3);
        encoder.write(probabilities[band(1)][AFTER_MORE][EOB_NODE], true);
        encoder.write(probabilities[band(1)][AFTER_MORE][ZERO_NODE], false);
        encoder.write(probabilities[band(2)][0][ZERO_NODE], true);
        write_value(&mut encoder, &probabilities[band(2)][0], -1);
        encoder.write(probabilities[band(3)][AFTER_ONE][EOB_NODE], false);
        // A second block that ends at once.
        encoder.write(probabilities[band(0)][1][EOB_NODE], false);
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        let mut block = [0i16; COEFFICIENTS];
        assert_eq!(
            read_block(&mut decoder, probabilities, 0, 0, [3, 5], &mut block),
            (true, 3),
            "tokens up to place 2"
        );
        let mut expected = [0i16; COEFFICIENTS];
        expected[0] = 9;
        expected[usize::from(ZIGZAG[2])] = -5;
        assert_eq!(block, expected, "3 times the DC factor, and -1 times the other factor at the zigzag's third place");
        let mut empty = [0i16; COEFFICIENTS];
        assert_eq!(read_block(&mut decoder, probabilities, 1, 0, [3, 5], &mut empty), (false, 0), "an end at once");
        assert!(!decoder.is_past_end(), "both blocks within the bytes written");
    }

    #[test]
    fn last_place_ends_block() {
        let probabilities = &DEFAULT_COEFF_PROBS[Y_AFTER_Y2];
        let band = |place: usize| usize::from(COEFF_BANDS_OF[place]);
        let mut encoder = BoolEncoder::new();
        encoder.write(probabilities[band(1)][2][EOB_NODE], true);
        for place in 1..LAST {
            let context = if place == 1 { 2 } else { 0 };
            encoder.write(probabilities[band(place)][context][ZERO_NODE], false);
        }
        encoder.write(probabilities[band(LAST)][0][ZERO_NODE], true);
        write_value(&mut encoder, &probabilities[band(LAST)][0], 2);
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        let mut block = [0i16; COEFFICIENTS];
        assert_eq!(
            read_block(&mut decoder, probabilities, 2, 1, [3, 5], &mut block),
            (true, 16),
            "zeros from place 1 and a 2 at place 15, after which no end of block is read"
        );
        assert_eq!(block[15], 10, "place 15 is the last of the zigzag");
    }

    #[test]
    fn macroblock_contexts() {
        let mut encoder = BoolEncoder::new();
        // Y2 with the contexts 1 + 1 ends at once; every other block of a macroblock with Y2 ends at once too.
        encoder.write(DEFAULT_COEFF_PROBS[Y2][0][2][EOB_NODE], false);
        for block in 0..16 {
            let context = if block % 4 == 0 { 1 } else { 0 };
            encoder.write(DEFAULT_COEFF_PROBS[Y_AFTER_Y2][1][context][EOB_NODE], false);
        }
        for _ in 0..8 {
            encoder.write(DEFAULT_COEFF_PROBS[CHROMA][0][0][EOB_NODE], false);
        }
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        let mut left = [1, 1, 1, 1, 0, 0, 0, 0, 1];
        let mut above = [0, 0, 0, 0, 0, 0, 0, 0, 1];
        let mut residue = Residue::default();
        let is_coded =
            read_macroblock(&mut decoder, &DEFAULT_COEFF_PROBS, true, &FACTORS, (&mut left, &mut above), &mut residue);
        assert!(!is_coded, "no block had a token");
        assert_eq!((left, above), ([0; CONTEXTS], [0; CONTEXTS]), "every context taken over by the blocks read");
        assert!(!decoder.is_past_end(), "25 blocks within the bytes written");
        let (mut kept_left, mut kept_above) = ([1; CONTEXTS], [1; CONTEXTS]);
        skip(false, (&mut kept_left, &mut kept_above));
        assert_eq!(kept_left, [0, 0, 0, 0, 0, 0, 0, 0, 1], "a skipped macroblock without Y2 keeps Y2's context");
        skip(true, (&mut kept_left, &mut kept_above));
        assert_eq!(kept_above, [0; CONTEXTS], "one with Y2 clears it");
    }

    #[test]
    fn written_tokens_read_back() {
        let mut state = 0x7F4A_7C15u32;
        let mut next = |bound: u32| {
            state = crate::base::math::xorshift32(state);
            state % bound
        };
        let mut macroblocks = alloc::vec::Vec::new();
        for index in 0..300 {
            let mut levels = Levels::default();
            for (block, end) in levels.ends.iter_mut().enumerate() {
                let last = next(17) as usize;
                for place in 0..last {
                    // Small values mostly, every category now and then, and the largest a token codes.
                    let magnitude = match next(16) {
                        0..=5 => 0,
                        6..=11 => 1 + next(4) as i16,
                        12..=14 => 1 + next(70) as i16,
                        _ => [2114, 67, 66, 35, 34, 19, 18, 11, 10, 7, 6, 5][next(12) as usize],
                    };
                    levels.blocks[block][place] = if next(2) == 0 { magnitude } else { -magnitude };
                }
                if last > 0 && levels.blocks[block][last - 1] == 0 {
                    levels.blocks[block][last - 1] = 1;
                }
                *end = last as u8;
            }
            macroblocks.push((levels, index % 3 != 0));
        }
        let mut encoder = BoolEncoder::new();
        let (mut left, mut above) = ([0; CONTEXTS], [0; CONTEXTS]);
        for (levels, has_y2) in &mut macroblocks {
            if *has_y2 {
                for block in 0..SUBBLOCKS {
                    levels.blocks[block][0] = 0;
                    let end = &mut levels.ends[block];
                    *end = if levels.blocks[block][1..].iter().all(|&level| level == 0) { 0 } else { (*end).max(2) };
                }
            }
            let mut writer = Writer { encoder: &mut encoder, probabilities: &DEFAULT_COEFF_PROBS };
            write_macroblock(&mut writer, *has_y2, levels, (&mut left, &mut above));
        }
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        let (mut left, mut above) = ([0; CONTEXTS], [0; CONTEXTS]);
        let ones = Factors { y1: [1, 1], y2: [1, 1], uv: [1, 1] };
        for (index, (levels, has_y2)) in macroblocks.iter().enumerate() {
            let mut residue = Residue::default();
            let is_coded = read_macroblock(
                &mut decoder,
                &DEFAULT_COEFF_PROBS,
                *has_y2,
                &ones,
                (&mut left, &mut above),
                &mut residue,
            );
            assert_eq!(is_coded, levels.is_coded(*has_y2), "macroblock {index}: whether any block has a token");
            for block in 0..BLOCKS {
                if !*has_y2 && block == Y2_BLOCK {
                    continue;
                }
                let raster: [i16; COEFFICIENTS] = core::array::from_fn(|at| {
                    levels.blocks[block][ZIGZAG.iter().position(|&z| usize::from(z) == at).unwrap()]
                });
                assert_eq!(
                    residue.blocks[block], raster,
                    "macroblock {index}, block {block}: the levels in raster order"
                );
                assert_eq!(residue.ends[block], levels.ends[block], "macroblock {index}, block {block}: its end");
            }
        }
        assert!(!decoder.is_past_end(), "300 macroblocks within the bytes written");
    }
}

use super::probs::{BANDS, CONTEXTS, EOB_TOKEN, MoreCounts, ONE_TOKEN, TWO_TOKEN, TokenCounts, ZERO_TOKEN};
use super::tables::{
    ADST_DCT, CAT_BASE, CAT1_PROBS, CAT2_PROBS, CAT3_PROBS, CAT4_PROBS, CAT5_PROBS, CAT6_PROBS, COEFBAND_4X4,
    COEFBAND_8X8PLUS, COL_SCAN_4X4, COL_SCAN_8X8, COL_SCAN_16X16, DCT_ADST, DEFAULT_SCAN_4X4, DEFAULT_SCAN_8X8,
    DEFAULT_SCAN_16X16, DEFAULT_SCAN_32X32, LAST_BAND, MOST_SIDE, PARETO_TABLE, ROW_SCAN_4X4, ROW_SCAN_8X8,
    ROW_SCAN_16X16, TX_4X4, TX_8X8, TX_16X16, TX_32X32,
};
use crate::codec::boolean::BoolDecoder;

// The context a token leaves for the coefficients after it, by its token, as energy_class gives it (VP9 10.2).
const CACHE_ONE: u8 = 1;
const CACHE_TWO: u8 = 2;
const CACHE_THREE_FOUR: u8 = 3;
const CACHE_CAT1_CAT2: u8 = 4;
const CACHE_CAT3_UP: u8 = 5;
// The value of TWO_TOKEN and THREE_TOKEN (VP9 9.3, token).
const TWO: i32 = 2;
const THREE: i32 = 3;
// The node of a token's model whose probability picks the row of the Pareto table (VP9 9.3.2, token).
const PIVOT_NODE: usize = 2;

// A transform size's, plane type's and reference's probabilities by band, context and node.
type BandProbs = [[[u8; 3]; CONTEXTS]; BANDS];

// The scan of a transform: a column scan for DCT_ADST, a row scan for ADST_DCT, and 32x32, whose kind comes as DCT_DCT, has the default alone (10.1).
fn scan(size: u8, kind: u8) -> &'static [u16] {
    match (size, kind) {
        (TX_4X4, DCT_ADST) => &COL_SCAN_4X4,
        (TX_4X4, ADST_DCT) => &ROW_SCAN_4X4,
        (TX_4X4, _) => &DEFAULT_SCAN_4X4,
        (TX_8X8, DCT_ADST) => &COL_SCAN_8X8,
        (TX_8X8, ADST_DCT) => &ROW_SCAN_8X8,
        (TX_8X8, _) => &DEFAULT_SCAN_8X8,
        (TX_16X16, DCT_ADST) => &COL_SCAN_16X16,
        (TX_16X16, ADST_DCT) => &ROW_SCAN_16X16,
        (TX_16X16, _) => &DEFAULT_SCAN_16X16,
        _ => &DEFAULT_SCAN_32X32,
    }
}

// The probability of a node after the pivot's: the Pareto table's row of an odd pivot, the mean of the two rows about an even one (VP9 9.3.2).
fn pareto(pivot: u8, node: usize) -> u8 {
    let row = usize::from(pivot - 1) / 2;
    if pivot & 1 == 1 {
        PARETO_TABLE[row][node]
    } else {
        ((u16::from(PARETO_TABLE[row][node]) + u16::from(PARETO_TABLE[row + 1][node])) >> 1) as u8
    }
}

// The tokens of one transform block, dequantized into `block` in raster order; the count of places read (VP9 6.4.24).
pub(super) struct Tokens<'a> {
    pub(super) probs: &'a BandProbs,
    pub(super) counts: &'a mut TokenCounts,
    pub(super) more: &'a mut MoreCounts,
    pub(super) cache: &'a mut [u8; MOST_SIDE * MOST_SIDE],
}

impl Tokens<'_> {
    pub(super) fn read(
        &mut self,
        decoder: &mut BoolDecoder<'_>,
        (size, kind): (u8, u8),
        mut context: usize,
        dequant: [i32; 2],
        block: &mut [i32],
    ) -> usize {
        let scan = scan(size, kind);
        let log2 = u32::from(size) + 2;
        let most = 1usize << (2 * log2);
        let shift = u32::from(size == TX_32X32);
        let band = |place: usize| {
            usize::from(if size == TX_4X4 {
                COEFBAND_4X4[place]
            } else {
                COEFBAND_8X8PLUS.get(place).copied().unwrap_or(LAST_BAND)
            })
        };
        let mut place = 0;
        let mut step = dequant[0];
        while place < most {
            let mut probs = &self.probs[band(place)][context];
            self.more[band(place)][context] += 1;
            if !decoder.read(probs[0]) {
                self.counts[band(place)][context][EOB_TOKEN] += 1;
                break;
            }
            while !decoder.read(probs[1]) {
                self.counts[band(place)][context][ZERO_TOKEN] += 1;
                step = dequant[1];
                self.cache[usize::from(scan[place])] = 0;
                place += 1;
                if place >= most {
                    return place;
                }
                context = self.context(scan, log2, kind, place);
                probs = &self.probs[band(place)][context];
            }
            let value = if !decoder.read(probs[2]) {
                self.counts[band(place)][context][ONE_TOKEN] += 1;
                self.cache[usize::from(scan[place])] = CACHE_ONE;
                1
            } else {
                self.counts[band(place)][context][TWO_TOKEN] += 1;
                let (value, cache) = Self::large(decoder, probs[PIVOT_NODE]);
                self.cache[usize::from(scan[place])] = cache;
                value
            };
            let magnitude = (value * step) >> shift;
            // A coefficient's sign is a bool of even odds (VP9 6.4.24, sign_bit).
            block[usize::from(scan[place])] = if decoder.flag() { -magnitude } else { magnitude };
            place += 1;
            context = self.context(scan, log2, kind, place);
            step = dequant[1];
        }
        place
    }

    // A token of TWO or more: its nodes after the third by the Pareto table, then a category's extra bits (VP9 9.3).
    fn large(decoder: &mut BoolDecoder<'_>, pivot: u8) -> (i32, u8) {
        let pareto = |node: usize| pareto(pivot, node);
        let extra = |decoder: &mut BoolDecoder<'_>, probs: &[u8]| {
            probs.iter().fold(0, |value, &probability| value << 1 | i32::from(decoder.read(probability)))
        };
        if !decoder.read(pareto(0)) {
            if !decoder.read(pareto(1)) {
                return (TWO, CACHE_TWO);
            }
            return (THREE + i32::from(decoder.read(pareto(2))), CACHE_THREE_FOUR);
        }
        if !decoder.read(pareto(3)) {
            if decoder.read(pareto(4)) {
                return (CAT_BASE[1] + extra(decoder, &CAT2_PROBS), CACHE_CAT1_CAT2);
            }
            return (CAT_BASE[0] + extra(decoder, &CAT1_PROBS), CACHE_CAT1_CAT2);
        }
        let value = if decoder.read(pareto(5)) {
            if decoder.read(pareto(7)) {
                CAT_BASE[5] + extra(decoder, &CAT6_PROBS)
            } else {
                CAT_BASE[4] + extra(decoder, &CAT5_PROBS)
            }
        } else if decoder.read(pareto(6)) {
            CAT_BASE[3] + extra(decoder, &CAT4_PROBS)
        } else {
            CAT_BASE[2] + extra(decoder, &CAT3_PROBS)
        };
        (value, CACHE_CAT3_UP)
    }

    // The context of the token at a place after the first: its neighbours above and to the left (VP9 9.3.2).
    fn context(&self, scan: &[u16], log2: u32, kind: u8, place: usize) -> usize {
        let Some(&position) = scan.get(place) else {
            return 0;
        };
        let position = usize::from(position);
        let side = 1usize << log2;
        let (row, col) = (position >> log2, position & (side - 1));
        let (above, left) = (position.wrapping_sub(side), position.wrapping_sub(1));
        let (first, second) = if row > 0 && col > 0 {
            match kind {
                DCT_ADST => (above, above),
                ADST_DCT => (left, left),
                _ => (above, left),
            }
        } else if row > 0 {
            (above, above)
        } else {
            (left, left)
        };
        (1 + usize::from(self.cache[first]) + usize::from(self.cache[second])) >> 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::boolean::BoolEncoder;
    use crate::codec::vp9::tables::DEFAULT_COEF_PROBS;

    #[test]
    fn tokens_of_a_block() {
        let probs = &DEFAULT_COEF_PROBS[0][0][0];
        let mut encoder = BoolEncoder::new();
        // DC: more_coefs, not zero, ONE, positive.
        encoder.write(probs[0][0][0], true);
        encoder.write(probs[0][0][1], true);
        encoder.write(probs[0][0][2], false);
        encoder.flag(false);
        // Places 1 and 2, below and right of the DC, have band 1 and context (1 + 1 + 1) >> 1: a ZERO, a negative TWO.
        let context = (1 + 2 * usize::from(CACHE_ONE)) >> 1;
        encoder.write(probs[1][context][0], true);
        encoder.write(probs[1][context][1], false);
        encoder.write(probs[1][context][1], true);
        encoder.write(probs[1][context][2], true);
        let pivot = probs[1][context][PIVOT_NODE];
        encoder.write(pareto(pivot, 0), false);
        encoder.write(pareto(pivot, 1), false);
        encoder.flag(true);
        // Place 3, raster 5, band 2, between the TWO above and the ZERO on its left: no more coefficients.
        let place3 = (1 + usize::from(CACHE_TWO)) >> 1;
        encoder.write(probs[2][place3][0], false);
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        let mut counts = TokenCounts::default();
        let mut more = MoreCounts::default();
        let mut cache = [0u8; MOST_SIDE * MOST_SIDE];
        let mut tokens = Tokens { probs, counts: &mut counts, more: &mut more, cache: &mut cache };
        let mut block = [0i32; 16];
        let read = tokens.read(&mut decoder, (TX_4X4, 0), 0, [8, 10], &mut block);
        assert_eq!(read, 3, "three places before the end of the block");
        // The default 4x4 scan visits raster places 0, 4 and 1.
        assert_eq!((block[0], block[4], block[1]), (8, 0, -20), "the DC by its step, a ZERO, a TWO by the AC step");
        assert_eq!(counts[0][0][ONE_TOKEN], 1, "the DC counted as a ONE");
        assert_eq!(counts[1][context][ZERO_TOKEN], 1, "the zero counted at its band and context");
        assert_eq!(counts[1][context][TWO_TOKEN], 1, "the TWO counted with the larger tokens");
        assert_eq!(counts[2][place3][EOB_TOKEN], 1, "the end counted where it was read");
        assert_eq!(more[1][context], 1, "after a zero no end is read, so band 1 asked once");
    }
}

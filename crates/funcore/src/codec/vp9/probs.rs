use super::tables::{
    CoefProbs, DEFAULT_COEF_PROBS, DEFAULT_COMP_MODE_PROBS, DEFAULT_COMP_REF_PROBS, DEFAULT_INTER_MODE_PROBS,
    DEFAULT_INTERP_FILTER_PROBS, DEFAULT_IS_INTER_PROBS, DEFAULT_MV_BITS_PROBS, DEFAULT_MV_CLASS_PROBS,
    DEFAULT_MV_CLASS0_BIT_PROBS, DEFAULT_MV_CLASS0_FR_PROBS, DEFAULT_MV_CLASS0_HP_PROB, DEFAULT_MV_FR_PROBS,
    DEFAULT_MV_HP_PROB, DEFAULT_MV_JOINT_PROBS, DEFAULT_MV_SIGN_PROB, DEFAULT_PARTITION_PROBS,
    DEFAULT_SINGLE_REF_PROBS, DEFAULT_SKIP_PROBS, DEFAULT_TX_8_PROBS, DEFAULT_TX_16_PROBS, DEFAULT_TX_32_PROBS,
    DEFAULT_UV_MODE_PROBS, DEFAULT_Y_MODE_PROBS, INTER_MODE_TREE, INTERP_FILTER_TREE, INTRA_MODE_TREE, INTRA_MODES,
    INV_MAP_TABLE, MV_CLASS_TREE, MV_FR_TREE, MV_JOINT_TREE, PARTITION_TREE, TX_SIZE_8_TREE, TX_SIZE_16_TREE,
    TX_SIZE_32_TREE, TX_SIZES,
};
use crate::codec::boolean::{BoolDecoder, probability_of};

// The probability of a flag that an update follows (VP9 6.3.3, 6.3.17).
const UPDATE_PROB: u8 = 252;
// A motion vector probability sent anew has seven bits and its lowest set (VP9 6.3.17).
const MV_PROB_BITS: u32 = 7;
// The sizes of decode_term_subexp's ranges, and where its last range doubles (VP9 6.3.4).
const SUBEXP_LOW_BITS: u32 = 4;
const SUBEXP_MIDDLE_BITS: u32 = 5;
const SUBEXP_HIGH_BITS: u32 = 7;
const SUBEXP_SECOND: u32 = 16;
const SUBEXP_THIRD: u32 = 32;
const SUBEXP_FOURTH: u32 = 64;
const SUBEXP_DOUBLED: u32 = 65;
const MAX_PROB: u32 = 255;
// Backward adaptation: the counts of full adaptation and the largest factors, of modes and coefficients (VP9 3, 8.4).
const COUNT_SAT: u32 = 20;
const MAX_UPDATE_FACTOR: u32 = 128;
const COEF_COUNT_SAT: u32 = 24;
const PROB_SCALE: u32 = 256;
const PROB_SHIFT: u32 = 8;
// The nodes and the tokens counted of a coefficient's model: more_coefs, zero, one and the rest (VP9 8.4.3).
const MODEL_NODES: usize = 3;
pub(super) const ZERO_TOKEN: usize = 0;
pub(super) const ONE_TOKEN: usize = 1;
pub(super) const TWO_TOKEN: usize = 2;
pub(super) const EOB_TOKEN: usize = 3;
pub(super) const BANDS: usize = 6;
pub(super) const CONTEXTS: usize = 6;
pub(super) const BAND_0_CONTEXTS: usize = 3;
const MV_CLASSES: usize = 11;
const MV_OFFSET_BITS: usize = 10;

// One component's motion vector probabilities (VP9 6.3.16).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct MvProbs {
    pub(super) sign: u8,
    pub(super) classes: [u8; MV_CLASSES - 1],
    pub(super) class0_bit: u8,
    pub(super) bits: [u8; MV_OFFSET_BITS],
    pub(super) class0_fr: [[u8; 3]; 2],
    pub(super) fr: [u8; 3],
    pub(super) class0_hp: u8,
    pub(super) hp: u8,
}

// A frame context: every probability a frame decodes by, kept in four slots (VP9 7.2).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Probs {
    pub(super) tx8: [[u8; 1]; 2],
    pub(super) tx16: [[u8; 2]; 2],
    pub(super) tx32: [[u8; 3]; 2],
    pub(super) coef: CoefProbs,
    pub(super) skip: [u8; 3],
    pub(super) inter_mode: [[u8; 3]; 7],
    pub(super) interp_filter: [[u8; 2]; 4],
    pub(super) is_inter: [u8; 4],
    pub(super) comp_mode: [u8; 5],
    pub(super) single_ref: [[u8; 2]; 5],
    pub(super) comp_ref: [u8; 5],
    pub(super) y_mode: [[u8; 9]; 4],
    pub(super) uv_mode: [[u8; 9]; INTRA_MODES],
    pub(super) partition: [[u8; 3]; 16],
    pub(super) mv_joint: [u8; 3],
    pub(super) mv: [MvProbs; 2],
}

impl Default for Probs {
    fn default() -> Probs {
        let mv = |component: usize| MvProbs {
            sign: DEFAULT_MV_SIGN_PROB,
            classes: DEFAULT_MV_CLASS_PROBS[component],
            class0_bit: DEFAULT_MV_CLASS0_BIT_PROBS[component],
            bits: DEFAULT_MV_BITS_PROBS,
            class0_fr: DEFAULT_MV_CLASS0_FR_PROBS,
            fr: DEFAULT_MV_FR_PROBS,
            class0_hp: DEFAULT_MV_CLASS0_HP_PROB,
            hp: DEFAULT_MV_HP_PROB,
        };
        Probs {
            tx8: DEFAULT_TX_8_PROBS,
            tx16: DEFAULT_TX_16_PROBS,
            tx32: DEFAULT_TX_32_PROBS,
            coef: DEFAULT_COEF_PROBS,
            skip: DEFAULT_SKIP_PROBS,
            inter_mode: DEFAULT_INTER_MODE_PROBS,
            interp_filter: DEFAULT_INTERP_FILTER_PROBS,
            is_inter: DEFAULT_IS_INTER_PROBS,
            comp_mode: DEFAULT_COMP_MODE_PROBS,
            single_ref: DEFAULT_SINGLE_REF_PROBS,
            comp_ref: DEFAULT_COMP_REF_PROBS,
            y_mode: DEFAULT_Y_MODE_PROBS,
            uv_mode: DEFAULT_UV_MODE_PROBS,
            partition: DEFAULT_PARTITION_PROBS,
            mv_joint: DEFAULT_MV_JOINT_PROBS,
            mv: [mv(0), mv(1)],
        }
    }
}

// One component's motion vector counts (VP9 8.3).
#[derive(Clone, Copy, Default)]
pub(super) struct MvCounts {
    pub(super) sign: [u32; 2],
    pub(super) classes: [u32; MV_CLASSES],
    pub(super) class0_bit: [u32; 2],
    pub(super) bits: [[u32; 2]; MV_OFFSET_BITS],
    pub(super) class0_fr: [[u32; 4]; 2],
    pub(super) fr: [u32; 4],
    pub(super) class0_hp: [u32; 2],
    pub(super) hp: [u32; 2],
}

// How often each value of each syntax element was decoded in a frame, for the backward adaptation (VP9 8.3).
#[derive(Clone, Default)]
pub(super) struct Counts {
    pub(super) y_mode: [[u32; INTRA_MODES]; 4],
    pub(super) uv_mode: [[u32; INTRA_MODES]; INTRA_MODES],
    pub(super) partition: [[u32; 4]; 16],
    pub(super) interp_filter: [[u32; 3]; 4],
    pub(super) inter_mode: [[u32; 4]; 7],
    pub(super) tx8: [[u32; 2]; 2],
    pub(super) tx16: [[u32; 3]; 2],
    pub(super) tx32: [[u32; 4]; 2],
    pub(super) is_inter: [[u32; 2]; 4],
    pub(super) comp_mode: [[u32; 2]; 5],
    pub(super) single_ref: [[[u32; 2]; 2]; 5],
    pub(super) comp_ref: [[u32; 2]; 5],
    pub(super) skip: [[u32; 2]; 3],
    pub(super) mv_joint: [u32; 4],
    pub(super) mv: [MvCounts; 2],
    pub(super) tokens: [[[TokenCounts; 2]; 2]; TX_SIZES],
    pub(super) more_coefs: [[[MoreCounts; 2]; 2]; TX_SIZES],
}

// By band and context: tokens of zero, one and more, and more_coefs decoded as nought; then more_coefs decoded at all.
pub(super) type TokenCounts = [[[u32; 4]; CONTEXTS]; BANDS];
pub(super) type MoreCounts = [[u32; CONTEXTS]; BANDS];

// diff_update_prob: a probability, changed where a flag says a coded delta follows (VP9 6.3.3 to 6.3.6).
pub(super) fn update(decoder: &mut BoolDecoder<'_>, probability: &mut u8) {
    if !decoder.read(UPDATE_PROB) {
        return;
    }
    let delta = if !decoder.flag() {
        decoder.literal(SUBEXP_LOW_BITS)
    } else if !decoder.flag() {
        decoder.literal(SUBEXP_LOW_BITS) + SUBEXP_SECOND
    } else if !decoder.flag() {
        decoder.literal(SUBEXP_MIDDLE_BITS) + SUBEXP_THIRD
    } else {
        let value = decoder.literal(SUBEXP_HIGH_BITS);
        if value < SUBEXP_DOUBLED { value + SUBEXP_FOURTH } else { (value << 1) - 1 + u32::from(decoder.flag()) }
    };
    let remapped = u32::from(INV_MAP_TABLE[delta as usize]);
    let previous = u32::from(*probability) - 1;
    *probability = if previous << 1 <= MAX_PROB {
        1 + recenter(remapped, previous)
    } else {
        MAX_PROB - recenter(remapped, MAX_PROB - 1 - previous)
    } as u8;
}

// inv_recenter_nonneg (VP9 6.3.6).
const fn recenter(value: u32, middle: u32) -> u32 {
    if value > 2 * middle {
        value
    } else if value & 1 == 1 {
        middle - ((value + 1) >> 1)
    } else {
        middle + (value >> 1)
    }
}

// update_mv_prob: seven bits and a set lowest bit where a flag says so (VP9 6.3.17).
pub(super) fn update_mv(decoder: &mut BoolDecoder<'_>, probability: &mut u8) {
    if decoder.read(UPDATE_PROB) {
        *probability = (decoder.literal(MV_PROB_BITS) << 1 | 1) as u8;
    }
}

// merge_prob (VP9 8.4.1).
fn merge(previous: u8, (zeros, ones): (u32, u32), saturation: u32, most: u32) -> u8 {
    let total = zeros + ones;
    let probability = u32::from(probability_of([zeros, ones]));
    let factor = most * total.min(saturation) / saturation;
    let weighted = u32::from(previous) * (PROB_SCALE - factor) + probability * factor;
    ((weighted + (1 << (PROB_SHIFT - 1))) >> PROB_SHIFT) as u8
}

// merge_probs over a tree, from node `at` down: the times its branch was taken (VP9 8.4.2).
fn merge_tree(tree: &[i8], at: usize, previous: &[u8], counts: &[u32], out: &mut [u8]) -> u32 {
    let count = |out: &mut [u8], child: i8| {
        if child <= 0 {
            counts[usize::from(child.unsigned_abs())]
        } else {
            merge_tree(tree, child as usize, previous, counts, out)
        }
    };
    let left = count(out, tree[at]);
    let right = count(out, tree[at + 1]);
    out[at / 2] = adapt(previous[at / 2], [left, right]);
    left + right
}

fn adapt_tree(tree: &[i8], previous: &[u8], counts: &[u32], out: &mut [u8]) {
    merge_tree(tree, 0, previous, counts, out);
}

fn adapt(previous: u8, counts: [u32; 2]) -> u8 {
    merge(previous, (counts[0], counts[1]), COUNT_SAT, MAX_UPDATE_FACTOR)
}

impl Probs {
    // The coefficient probabilities adapted from the frame's context to its counts (VP9 8.4.3).
    pub(super) fn adapt_coef(&mut self, previous: &Probs, counts: &Counts, most: u32) {
        for size in 0..TX_SIZES {
            for plane in 0..2 {
                for reference in 0..2 {
                    for band in 0..BANDS {
                        let contexts = if band == 0 { BAND_0_CONTEXTS } else { CONTEXTS };
                        for context in 0..contexts {
                            let tokens = &counts.tokens[size][plane][reference][band][context];
                            let more = counts.more_coefs[size][plane][reference][band][context];
                            let before = &previous.coef[size][plane][reference][band][context];
                            let branches = [
                                (tokens[EOB_TOKEN], more - tokens[EOB_TOKEN]),
                                (tokens[ZERO_TOKEN], tokens[ONE_TOKEN] + tokens[TWO_TOKEN]),
                                (tokens[ONE_TOKEN], tokens[TWO_TOKEN]),
                            ];
                            let after = &mut self.coef[size][plane][reference][band][context];
                            for node in 0..MODEL_NODES {
                                after[node] = merge(before[node], branches[node], COEF_COUNT_SAT, most);
                            }
                        }
                    }
                }
            }
        }
    }

    // Every probability but the coefficients' adapted from the frame's context to its counts (VP9 8.4.4).
    pub(super) fn adapt_modes(
        &mut self,
        previous: &Probs,
        counts: &Counts,
        (is_switchable, is_tx_select, allow_hp): (bool, bool, bool),
    ) {
        for (context, counts) in counts.is_inter.iter().enumerate() {
            self.is_inter[context] = adapt(previous.is_inter[context], *counts);
        }
        for (context, counts) in counts.comp_mode.iter().enumerate() {
            self.comp_mode[context] = adapt(previous.comp_mode[context], *counts);
        }
        for (context, counts) in counts.comp_ref.iter().enumerate() {
            self.comp_ref[context] = adapt(previous.comp_ref[context], *counts);
        }
        for (context, counts) in counts.single_ref.iter().enumerate() {
            for (which, counts) in counts.iter().enumerate() {
                self.single_ref[context][which] = adapt(previous.single_ref[context][which], *counts);
            }
        }
        for (context, counts) in counts.inter_mode.iter().enumerate() {
            adapt_tree(&INTER_MODE_TREE, &previous.inter_mode[context], counts, &mut self.inter_mode[context]);
        }
        for (group, counts) in counts.y_mode.iter().enumerate() {
            adapt_tree(&INTRA_MODE_TREE, &previous.y_mode[group], counts, &mut self.y_mode[group]);
        }
        for (mode, counts) in counts.uv_mode.iter().enumerate() {
            adapt_tree(&INTRA_MODE_TREE, &previous.uv_mode[mode], counts, &mut self.uv_mode[mode]);
        }
        for (context, counts) in counts.partition.iter().enumerate() {
            adapt_tree(&PARTITION_TREE, &previous.partition[context], counts, &mut self.partition[context]);
        }
        if is_switchable {
            for (context, counts) in counts.interp_filter.iter().enumerate() {
                let (before, after) = (&previous.interp_filter[context], &mut self.interp_filter[context]);
                adapt_tree(&INTERP_FILTER_TREE, before, counts, after);
            }
        }
        if is_tx_select {
            for context in 0..2 {
                adapt_tree(&TX_SIZE_8_TREE, &previous.tx8[context], &counts.tx8[context], &mut self.tx8[context]);
                adapt_tree(&TX_SIZE_16_TREE, &previous.tx16[context], &counts.tx16[context], &mut self.tx16[context]);
                adapt_tree(&TX_SIZE_32_TREE, &previous.tx32[context], &counts.tx32[context], &mut self.tx32[context]);
            }
        }
        for (context, counts) in counts.skip.iter().enumerate() {
            self.skip[context] = adapt(previous.skip[context], *counts);
        }
        adapt_tree(&MV_JOINT_TREE, &previous.mv_joint, &counts.mv_joint, &mut self.mv_joint);
        for ((after, before), counts) in self.mv.iter_mut().zip(&previous.mv).zip(&counts.mv) {
            after.sign = adapt(before.sign, counts.sign);
            adapt_tree(&MV_CLASS_TREE, &before.classes, &counts.classes, &mut after.classes);
            after.class0_bit = adapt(before.class0_bit, counts.class0_bit);
            for bit in 0..MV_OFFSET_BITS {
                after.bits[bit] = adapt(before.bits[bit], counts.bits[bit]);
            }
            for which in 0..2 {
                adapt_tree(
                    &MV_FR_TREE,
                    &before.class0_fr[which],
                    &counts.class0_fr[which],
                    &mut after.class0_fr[which],
                );
            }
            adapt_tree(&MV_FR_TREE, &before.fr, &counts.fr, &mut after.fr);
            if allow_hp {
                after.class0_hp = adapt(before.class0_hp, counts.class0_hp);
                after.hp = adapt(before.hp, counts.hp);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::boolean::BoolEncoder;

    #[test]
    fn merged_probabilities() {
        assert_eq!(merge(100, (0, 0), COUNT_SAT, MAX_UPDATE_FACTOR), 100, "no counts keep the probability");
        // 30 zeros of 40: 192; saturated at 20, the factor is 128, so the result is half way: (100 + 192 + 1) / 2.
        assert_eq!(merge(100, (30, 10), COUNT_SAT, MAX_UPDATE_FACTOR), 146, "counts past saturation weigh half");
        // 3 of 4: 192, the factor 128 * 4 / 20 = 25: (100 * 231 + 192 * 25 + 128) >> 8.
        assert_eq!(merge(100, (3, 1), COUNT_SAT, MAX_UPDATE_FACTOR), 109, "counts below saturation weigh less");
        // All ones: 1, at a factor of 112 * 9 / 24 = 42: (200 * 214 + 42 + 128) >> 8.
        assert_eq!(merge(200, (0, 9), COEF_COUNT_SAT, 112), 167, "all ones: the probability seen held at 1");
        let mut probabilities = [128, 128, 128];
        let previous = probabilities;
        adapt_tree(&MV_JOINT_TREE, &previous, &[20, 0, 0, 0], &mut probabilities);
        assert_eq!(probabilities, [192, 128, 128], "a tree's first node moves, the nodes never reached stay");
    }

    #[test]
    fn coded_updates() {
        let mut encoder = BoolEncoder::new();
        // No update; an update of delta 3 (first range); an update of delta 70 (last range, below the doubling).
        encoder.write(UPDATE_PROB, false);
        encoder.write(UPDATE_PROB, true);
        encoder.literal(0, 1);
        encoder.literal(3, SUBEXP_LOW_BITS);
        encoder.write(UPDATE_PROB, true);
        encoder.literal(0b111, 3);
        encoder.literal(70 - SUBEXP_FOURTH, SUBEXP_HIGH_BITS);
        encoder.write(UPDATE_PROB, true);
        encoder.literal(37, MV_PROB_BITS);
        let bytes = encoder.finish();
        let mut decoder = BoolDecoder::new(&bytes);
        let mut probability = 100;
        update(&mut decoder, &mut probability);
        assert_eq!(probability, 100, "no update");
        update(&mut decoder, &mut probability);
        // inv_map_table[3] is 46; m = 99: 46 is even, so 99 + 23 = 122, and 1 + 122.
        assert_eq!(probability, 123, "a small delta recentred on the old probability");
        update(&mut decoder, &mut probability);
        // inv_map_table[70] is 55; m = 122: 55 is odd, so 28 below 122 is 94, and 1 + 94.
        assert_eq!(probability, 95, "a delta of the last range");
        let mut mv = 0;
        update_mv(&mut decoder, &mut mv);
        assert_eq!(mv, 75, "seven bits, 37, and a set lowest bit");
    }
}

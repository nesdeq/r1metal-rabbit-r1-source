use crate::codec::boolean::{Path, path};

// Costs count 256ths of a bit, the precision of a probability (RFC 6386, 7.1).
const COST_BITS: u32 = 8;
pub(super) const ONE_BIT: u32 = 1 << COST_BITS;
// The fraction bits of a logarithm worked out by repeated squaring, and the fixed point it squares in.
const LOG_FRACTION_BITS: u32 = 16;
const SQUARE_POINT: u32 = 30;

// log2(n) in 65536ths, by its whole part and its fraction's bits, one a squaring.
const fn log2(n: u32) -> u32 {
    let whole = u32::BITS - 1 - n.leading_zeros();
    let mut mantissa = (n as u64) << (SQUARE_POINT - whole);
    let mut fraction = 0;
    let mut bit = 0;
    while bit < LOG_FRACTION_BITS {
        mantissa = (mantissa * mantissa) >> SQUARE_POINT;
        fraction <<= 1;
        if mantissa >> (SQUARE_POINT + 1) != 0 {
            mantissa >>= 1;
            fraction |= 1;
        }
        bit += 1;
    }
    whole << LOG_FRACTION_BITS | fraction
}

// The cost of an event of `n` chances in 256: -log2(n / 256) in 256ths of a bit, rounded.
const COSTS: [u16; 257] = {
    let mut costs = [0u16; 257];
    let whole = (COST_BITS << LOG_FRACTION_BITS) + (1 << (LOG_FRACTION_BITS - COST_BITS - 1));
    let mut n = 1;
    while n < costs.len() {
        costs[n] = ((whole - log2(n as u32)) >> (LOG_FRACTION_BITS - COST_BITS)) as u16;
        n += 1;
    }
    costs
};

// The cost of a bool whose chance of being false is `probability` in 256.
#[inline]
pub(super) const fn bool_cost(probability: u8, bit: bool) -> u32 {
    let chances = if bit { 256 - probability as usize } else { probability as usize };
    COSTS[chances] as u32
}

// The cost of `zeros` false and `ones` true bools at `probability`.
pub(super) fn counts_cost(probability: u8, [zeros, ones]: [u32; 2]) -> u32 {
    zeros * bool_cost(probability, false) + ones * bool_cost(probability, true)
}

// The cost of a leaf of a tree laid out as RFC 6386, 8.1 lays trees out.
pub(super) const fn tree_cost(tree: &[i8], probabilities: &[u8], value: u8) -> u32 {
    path_cost(&path(tree, value), probabilities)
}

// The cost of the bools down a path of a tree.
pub(super) const fn path_cost((steps, depth): &Path, probabilities: &[u8]) -> u32 {
    let (mut sum, mut step) = (0, 0);
    while step < *depth {
        let (node, bit) = steps[step];
        sum += bool_cost(probabilities[node / 2], bit);
        step += 1;
    }
    sum
}

// The costs of each leaf of a tree of `LEAVES` leaves numbered from nought.
pub(super) const fn leaf_costs<const LEAVES: usize>(tree: &[i8], probabilities: &[u8]) -> [u32; LEAVES] {
    let mut costs = [0; LEAVES];
    let mut leaf = 0;
    while leaf < LEAVES {
        costs[leaf] = tree_cost(tree, probabilities, leaf as u8);
        leaf += 1;
    }
    costs
}

// What a cost is worth in differences of pixels at `lambda`, the worth of a bit in 256ths of such a difference.
pub(super) const fn weigh(lambda: u32, cost: u32) -> u32 {
    (lambda * cost) >> (2 * COST_BITS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_of_chances() {
        assert_eq!(COSTS[256], 0, "a certain event costs nothing");
        assert_eq!(COSTS[128], 256, "an even one a bit");
        assert_eq!(COSTS[64], 512, "one in four two bits");
        assert_eq!(COSTS[1], 2048, "one in 256 eight bits");
        assert_eq!(COSTS[192], 106, "three in four: 0.415 bits");
        assert_eq!(bool_cost(255, true), 2048, "a true bool at the largest probability of false");
        assert_eq!(bool_cost(1, true), 1, "and at the smallest: 0.0056 bits");
    }

    #[test]
    fn counts_cost_least_at_fit() {
        assert!(counts_cost(192, [3, 1]) < counts_cost(128, [3, 1]), "the fitting probability costs less");
    }
}

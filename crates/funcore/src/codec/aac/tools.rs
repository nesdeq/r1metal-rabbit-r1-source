use super::transform::WINDOWS;
use crate::base::math::{next_random, sine_cosine, sqrt};

// FFmpeg's seed, so that the two decoders make the same noise.
pub(super) const SEED: u32 = 0x1F2E_3D4C;
// The most coefficients a filter of temporal noise shaping has in a long window and in a short one, and the most filters a window has (4.6.9.3, Table 4.48).
pub(super) const MAX_ORDER: [usize; 2] = [12, 7];
pub(super) const MAX_FILTERS: usize = 3;

#[derive(Clone, Copy, Default)]
pub(super) struct Filter {
    pub(super) length: usize,
    pub(super) order: usize,
    pub(super) down: bool,
    pub(super) lpc: [f32; MAX_ORDER[0]],
}

#[derive(Clone, Copy, Default)]
pub(super) struct Tns {
    pub(super) counts: [usize; WINDOWS],
    pub(super) filters: [[Filter; MAX_FILTERS]; WINDOWS],
}

// 4.6.13: a number for every line, scaled to the energy `gain` squared.
pub(super) fn noise(state: &mut u32, gain: f32, lines: &mut [f32]) {
    let mut energy = 0.0f32;
    for line in lines.iter_mut() {
        *line = next_random(state) as i32 as f32;
        energy += *line * *line;
    }
    let scale = if energy > 0.0 { gain / sqrt(energy) } else { 0.0 };
    lines.iter_mut().for_each(|line| *line *= scale);
}

// 4.6.8.1
pub(super) fn mid_side(left: &mut [f32], right: &mut [f32]) {
    for (left, right) in left.iter_mut().zip(right) {
        (*left, *right) = (*left + *right, *left - *right);
    }
}

// 4.6.8.2
pub(super) fn intensity(left: &[f32], right: &mut [f32], factor: f32) {
    for (right, &left) in right.iter_mut().zip(left) {
        *right = left * factor;
    }
}

// 4.6.9.3: a reflection coefficient is the sine of its index over the steps in a quarter turn, half a step fewer from an index of none up than below none.
pub(super) fn design(indices: &[i32], resolution: u32) -> [f32; MAX_ORDER[0]] {
    let steps = f64::from(1u32 << (resolution - 1));
    let mut lpc = [0.0f32; MAX_ORDER[0] + 1];
    lpc[0] = 1.0;
    for (at, &index) in indices.iter().enumerate() {
        let quarter = if index >= 0 { steps - 0.5 } else { steps + 0.5 };
        let reflection = sine_cosine(f64::from(index) / (4.0 * quarter)).0 as f32;
        let order = at + 1;
        lpc[order] = reflection;
        let before = lpc;
        for place in 1..order {
            lpc[place] = before[place] + reflection * before[order - place];
        }
    }
    core::array::from_fn(|at| lpc[at + 1])
}

pub(super) fn filter(lines: &mut [f32], filter: &Filter) {
    let last = lines.len() - 1;
    for step in 0..=last {
        let at = if filter.down { last - step } else { step };
        let sum: f32 = (1..=filter.order.min(step))
            .map(|back| filter.lpc[back - 1] * lines[if filter.down { at + back } else { at - back }])
            .sum();
        lines[at] -= sum;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_energy_and_continuation() {
        let (mut state, mut lines) = (SEED, [0.0f32; 32]);
        noise(&mut state, 3.0, &mut lines);
        let energy: f32 = lines.iter().map(|line| line * line).sum();
        assert!((energy - 9.0).abs() < 1.0e-4, "{energy}");
        let after = state;
        let mut more = [0.0f32; 32];
        noise(&mut state, 3.0, &mut more);
        assert_ne!(lines, more, "the generator goes on");
        let mut again = SEED;
        noise(&mut again, 3.0, &mut more);
        assert_eq!((again, more), (after, lines), "and from the same seed gives the same noise");
        noise(&mut state, 3.0, &mut []);
    }

    #[test]
    fn stereo_tools() {
        let (mut left, mut right) = ([3.0, 1.0, -2.0], [1.0, -1.0, 0.5]);
        mid_side(&mut left, &mut right);
        assert_eq!((left, right), ([4.0, 0.0, -1.5], [2.0, 2.0, -2.5]));
        intensity(&left, &mut right, -0.5);
        assert_eq!(right, [-2.0, 0.0, 0.75]);
    }

    #[test]
    fn reflection_to_filter() {
        let none = design(&[0, 0, 0], 3);
        assert_eq!(none, [0.0; MAX_ORDER[0]]);
        let one = design(&[1], 3);
        assert!((one[0] - 0.433_883_74).abs() < 1.0e-6, "the sine of 1 over 3.5 steps of a quarter turn");
        let negative = design(&[-4], 3);
        assert!((negative[0] + 0.984_807_7).abs() < 1.0e-6, "the sine of -4 over 4.5 steps");
        let two = design(&[1, 2], 4);
        let (first, second) = (0.207_911_69f32, 0.406_736_64f32);
        assert!((two[1] - second).abs() < 1.0e-6 && (two[0] - first * (1.0 + second)).abs() < 1.0e-6, "{two:?}");
    }

    #[test]
    fn filter_inverts_its_inverse() {
        let lpc = design(&[1, -2, 3], 4);
        let original: [f32; 9] = [4.0, -3.0, 2.0, 9.0, -1.0, 0.5, 7.0, -8.0, 2.0];
        for down in [false, true] {
            let tap = Filter { length: 0, order: 3, down, lpc };
            let mut lines = original;
            filter(&mut lines, &tap);
            let mut made = [0.0f32; 9];
            for step in 0..original.len() {
                let at = if down { original.len() - 1 - step } else { step };
                let back = |count: usize| if down { lines[at + count] } else { lines[at - count] };
                made[at] = lines[at] + (1..=3.min(step)).map(|count| lpc[count - 1] * back(count)).sum::<f32>();
            }
            for (actual, expected) in made.iter().zip(original) {
                assert!((actual - expected).abs() < 1.0e-4, "down {down}: {made:?}");
            }
        }
        let mut few = [1.0f32];
        filter(&mut few, &Filter { length: 0, order: 3, down: false, lpc });
        assert_eq!(few, [1.0], "a line has no lines behind it to follow");
    }
}

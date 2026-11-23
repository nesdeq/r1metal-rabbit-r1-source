use super::tables::{
    D45_PRED, D63_PRED, D117_PRED, D135_PRED, D153_PRED, D207_PRED, DC_PRED, H_PRED, MOST_SIDE, TM_PRED, V_PRED,
};

// The edges a missing row above or column to the left stands in with, and a DC of neither (VP9 8.5.1.1).
const MISSING_ABOVE: u8 = 127;
const MISSING_LEFT: u8 = 129;
const MISSING_BOTH: u8 = 128;

// Where a transform block lies and which of its edges are decoded (VP9 8.5.1).
#[derive(Clone, Copy)]
pub(super) struct Block {
    pub(super) x: usize,
    pub(super) y: usize,
    pub(super) size: u8,
    pub(super) has_above: bool,
    pub(super) has_left: bool,
    // Whether the 4 pixels above and to the right of a 4x4 block lie in its own block.
    pub(super) has_right: bool,
}

// A prediction's room: the largest block, a line of samples to each of its lines.
pub(super) type Scratch = [[u8; MOST_SIDE]; MOST_SIDE];

// Predicts a block of the plane from its edges, the plane's width and height being those of its 8x8 blocks, through `out`, whatever it holds (VP9 8.5.1).
pub(super) fn predict(
    plane: &mut [u8],
    stride: usize,
    (width, height): (usize, usize),
    block: Block,
    mode: u8,
    out: &mut Scratch,
) {
    let side = 4usize << block.size;
    let at = block.y * stride + block.x;
    let needs_left = !matches!(mode, V_PRED | D45_PRED | D63_PRED);
    let needs_above = !matches!(mode, H_PRED | D207_PRED);
    let needs_right = matches!(mode, D45_PRED | D63_PRED);
    let mut left = [MISSING_LEFT; MOST_SIDE];
    if needs_left && block.has_left {
        let count = side.min(height.saturating_sub(block.y)).max(1);
        for (row, value) in left.iter_mut().take(side).enumerate() {
            *value = plane[at + row.min(count - 1) * stride - 1];
        }
    }
    // The row above from its pixel above-left, then twice the block's width.
    let mut above = [MISSING_ABOVE; 2 * MOST_SIDE + 1];
    if (needs_above || needs_right) && block.has_above {
        let row = at - stride;
        let limit = if needs_right && side == 4 && block.has_right { 2 * side } else { side };
        let count = limit.min(width.saturating_sub(block.x)).max(1);
        for column in 0..2 * side {
            above[1 + column] = plane[row + column.min(count - 1)];
        }
        above[0] = if block.has_left { plane[row - 1] } else { MISSING_LEFT };
    }
    let a = |index: isize| i32::from(above[(index + 1) as usize]);
    let l = |index: usize| i32::from(left[index]);
    let average2 = |first: i32, second: i32| ((first + second + 1) >> 1) as u8;
    let average3 = |first: i32, second: i32, third: i32| ((first + 2 * second + third + 2) >> 2) as u8;
    match mode {
        DC_PRED => {
            let value = match (block.has_above, block.has_left) {
                (true, true) => {
                    let sum: i32 = (0..side).map(|index| a(index as isize) + l(index)).sum();
                    (sum + side as i32) / (2 * side as i32)
                }
                (true, false) => {
                    ((0..side).map(|index| a(index as isize)).sum::<i32>() + (side as i32 >> 1)) / side as i32
                }
                (false, true) => ((0..side).map(l).sum::<i32>() + (side as i32 >> 1)) / side as i32,
                (false, false) => i32::from(MISSING_BOTH),
            };
            out.iter_mut().take(side).for_each(|line| line[..side].fill(value as u8));
        }
        V_PRED => out.iter_mut().take(side).for_each(|line| {
            for (column, value) in line.iter_mut().take(side).enumerate() {
                *value = a(column as isize) as u8;
            }
        }),
        H_PRED => out.iter_mut().take(side).enumerate().for_each(|(row, line)| line[..side].fill(l(row) as u8)),
        TM_PRED => {
            for (row, line) in out.iter_mut().take(side).enumerate() {
                for (column, value) in line.iter_mut().take(side).enumerate() {
                    *value = (l(row) + a(column as isize) - a(-1)).clamp(0, 255) as u8;
                }
            }
        }
        D45_PRED => {
            for (row, line) in out.iter_mut().take(side).enumerate() {
                for (column, value) in line.iter_mut().take(side).enumerate() {
                    let index = (row + column) as isize;
                    *value = if row + column + 2 < 2 * side {
                        average3(a(index), a(index + 1), a(index + 2))
                    } else {
                        a(2 * side as isize - 1) as u8
                    };
                }
            }
        }
        D63_PRED => {
            for (row, line) in out.iter_mut().take(side).enumerate() {
                for (column, value) in line.iter_mut().take(side).enumerate() {
                    let index = (row / 2 + column) as isize;
                    *value = if row & 1 == 1 {
                        average3(a(index), a(index + 1), a(index + 2))
                    } else {
                        average2(a(index), a(index + 1))
                    };
                }
            }
        }
        D207_PRED => {
            // Two columns from the left edge, the last row its last sample, the rest two columns back a row down.
            let last = l(side - 1) as u8;
            for (row, line) in out.iter_mut().enumerate().take(side) {
                line[0] = if row + 1 < side { average2(l(row), l(row + 1)) } else { last };
                line[1] = match side - row {
                    1 => last,
                    2 => average3(l(row), l(row + 1), l(row + 1)),
                    _ => average3(l(row), l(row + 1), l(row + 2)),
                };
            }
            out[side - 1][2..side].fill(last);
            for row in (0..side - 1).rev() {
                let (line, below) = out.split_at_mut(row + 1);
                line[row][2..side].copy_from_slice(&below[0][..side - 2]);
            }
        }
        D117_PRED => {
            // The first two rows and the first column from the edges, the rest one column back two rows up.
            for (column, value) in out[0].iter_mut().enumerate().take(side) {
                *value = average2(a(column as isize - 1), a(column as isize));
            }
            for (column, value) in out[1].iter_mut().enumerate().take(side) {
                let index = column as isize;
                *value = if column == 0 {
                    average3(l(0), a(-1), a(0))
                } else {
                    average3(a(index - 2), a(index - 1), a(index))
                };
            }
            out[2][0] = average3(a(-1), l(0), l(1));
            for (row, line) in out.iter_mut().enumerate().take(side).skip(3) {
                line[0] = average3(l(row - 3), l(row - 2), l(row - 1));
            }
            for row in 2..side {
                let (above, line) = out.split_at_mut(row);
                line[0][1..side].copy_from_slice(&above[row - 2][..side - 1]);
            }
        }
        D135_PRED => {
            // The outer border from the bottom left to the top right, each row a step along it.
            let mut border = [0u8; 2 * MOST_SIDE];
            for (index, value) in border.iter_mut().enumerate().take(side - 2) {
                *value = average3(l(side - 3 - index), l(side - 2 - index), l(side - 1 - index));
            }
            border[side - 2] = average3(a(-1), l(0), l(1));
            border[side - 1] = average3(l(0), a(-1), a(0));
            border[side] = average3(a(-1), a(0), a(1));
            for (index, value) in border[side + 1..].iter_mut().enumerate().take(side - 2) {
                let at = index as isize;
                *value = average3(a(at), a(at + 1), a(at + 2));
            }
            for (row, line) in out.iter_mut().take(side).enumerate() {
                line[..side].copy_from_slice(&border[side - 1 - row..2 * side - 1 - row]);
            }
        }
        D153_PRED => {
            // The first two columns and the first row from the edges, the rest two columns back a row up.
            for (row, line) in out.iter_mut().enumerate().take(side) {
                line[0] = if row == 0 { average2(a(-1), l(0)) } else { average2(l(row - 1), l(row)) };
                line[1] = match row {
                    0 => average3(l(0), a(-1), a(0)),
                    1 => average3(a(-1), l(0), l(1)),
                    _ => average3(l(row - 2), l(row - 1), l(row)),
                };
            }
            for (column, value) in out[0][2..side].iter_mut().enumerate() {
                let index = column as isize;
                *value = average3(a(index - 1), a(index), a(index + 1));
            }
            for row in 1..side {
                let (above, line) = out.split_at_mut(row);
                line[0][2..side].copy_from_slice(&above[row - 1][..side - 2]);
            }
        }
        _ => out.iter_mut().take(side).for_each(|line| line[..side].fill(0)),
    }
    for (row, line) in out.iter().take(side).enumerate() {
        plane[at + row * stride..at + row * stride + side].copy_from_slice(&line[..side]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // A plane of 16 by 16 whose pixel at x, y is 10 * x + y, with a 4x4 block at 4, 4.
    fn plane() -> alloc::vec::Vec<u8> {
        (0..256).map(|place| (10 * (place % 16) + place / 16) as u8).collect()
    }

    fn block(has_above: bool, has_left: bool) -> Block {
        Block { x: 4, y: 4, size: 0, has_above, has_left, has_right: false }
    }

    #[test]
    fn edges_and_their_stand_ins() {
        // Room that comes filled: nothing of it reaches the plane.
        let mut out = [[255; MOST_SIDE]; MOST_SIDE];
        let mut pixels = plane();
        predict(&mut pixels, 16, (16, 16), block(false, false), DC_PRED, &mut out);
        assert_eq!(pixels[4 * 16 + 4], 128, "no edges: the middle grey");
        let mut pixels = plane();
        predict(&mut pixels, 16, (16, 16), block(true, false), V_PRED, &mut out);
        assert_eq!(&pixels[7 * 16 + 4..7 * 16 + 8], &[43, 53, 63, 73], "the row above repeated down");
        let mut pixels = plane();
        predict(&mut pixels, 16, (16, 16), block(false, true), H_PRED, &mut out);
        assert_eq!(&pixels[5 * 16 + 4..5 * 16 + 8], &[35; 4], "the column to the left repeated across");
        let mut pixels = plane();
        predict(&mut pixels, 16, (16, 16), block(false, false), TM_PRED, &mut out);
        assert_eq!(
            pixels[4 * 16 + 4],
            129,
            "TM of stand ins: the left's 129, plus the row's 127, less the corner's 127"
        );
    }

    #[test]
    fn above_right_replicated() {
        let mut out = [[0; MOST_SIDE]; MOST_SIDE];
        let mut pixels = plane();
        predict(&mut pixels, 16, (16, 16), block(true, true), D45_PRED, &mut out);
        // Without the pixels above right, they repeat the last above: 73. The corner is that value.
        assert_eq!(pixels[7 * 16 + 7], 73, "the bottom right corner takes the last of the row above");
        let mut narrow = plane();
        predict(&mut narrow, 16, (6, 16), block(true, true), V_PRED, &mut out);
        assert_eq!(&narrow[4 * 16 + 4..4 * 16 + 8], &[43, 53, 53, 53], "past the plane's width the edge repeats");
        let mut whole = vec![0u8; 256];
        whole.copy_from_slice(&plane());
        predict(&mut whole, 16, (16, 16), Block { has_right: true, ..block(true, true) }, D45_PRED, &mut out);
        assert_eq!(whole[7 * 16 + 7], 113, "with the pixels above right, the corner is the eighth above");
    }
}

use super::frame::SUBBLOCK;
use super::tables::{
    B_DC_PRED, B_HD_PRED, B_HE_PRED, B_HU_PRED, B_LD_PRED, B_RD_PRED, B_TM_PRED, B_VE_PRED, B_VL_PRED, B_VR_PRED,
    DC_PRED, H_PRED, TM_PRED, V_PRED,
};

// The value of a missing edge's pixels: above the frame, and left of it (RFC 6386, 12.2).
pub(super) const ABOVE_EDGE: u8 = 127;
pub(super) const LEFT_EDGE: u8 = 129;
// The DC of a block with neither edge (RFC 6386, 12.2).
const NO_EDGE_DC: u8 = 128;
// A subblock reads four pixels above and to its right besides the four above it (RFC 6386, 12.3).
pub(super) const ABOVE_RIGHT: usize = 4;
const SUBBLOCK_PIXELS: usize = SUBBLOCK * SUBBLOCK;

// The pixels around a block that predict it: the row above, the column to the left and the pixel above the column.
pub(super) struct Edges<'a> {
    pub(super) above: &'a [u8],
    pub(super) left: &'a [u8],
    pub(super) corner: u8,
    pub(super) has_above: bool,
    pub(super) has_left: bool,
}

#[inline]
fn sum(pixels: &[u8]) -> u32 {
    pixels.iter().map(|&pixel| u32::from(pixel)).sum()
}

// DC averages the edges the frame has: both, one, or neither (RFC 6386, 12.2).
fn dc(edges: &Edges<'_>, size: usize) -> u8 {
    let shift = size.trailing_zeros();
    let (above, left) = (&edges.above[..size], &edges.left[..size]);
    let value = match (edges.has_above, edges.has_left) {
        (true, true) => (sum(above) + sum(left) + size as u32) >> (shift + 1),
        (true, false) => (sum(above) + size as u32 / 2) >> shift,
        (false, true) => (sum(left) + size as u32 / 2) >> shift,
        (false, false) => u32::from(NO_EDGE_DC),
    };
    value as u8
}

// Predicts a square block in DC_PRED, V_PRED, H_PRED or TM_PRED into the plane at `at` (RFC 6386, 12.2).
pub(super) fn predict(mode: u8, size: usize, edges: &Edges<'_>, plane: &mut [u8], at: usize, stride: usize) {
    let above = &edges.above[..size];
    let value = if mode == DC_PRED { dc(edges, size) } else { NO_EDGE_DC };
    for (row, &left) in plane[at..].chunks_mut(stride).take(size).zip(edges.left) {
        let row = &mut row[..size];
        match mode {
            DC_PRED => row.fill(value),
            V_PRED => row.copy_from_slice(above),
            H_PRED => row.fill(left),
            TM_PRED => {
                let base = i32::from(left) - i32::from(edges.corner);
                for (pixel, &top) in row.iter_mut().zip(above) {
                    *pixel = (base + i32::from(top)).clamp(0, 255) as u8;
                }
            }
            _ => {}
        }
    }
}

#[inline]
const fn average3(first: u8, second: u8, third: u8) -> u8 {
    ((first as u32 + 2 * second as u32 + third as u32 + 2) >> 2) as u8
}

#[inline]
const fn average2(first: u8, second: u8) -> u8 {
    ((first as u32 + second as u32 + 1) >> 1) as u8
}

// A subblock in raster order from the eight pixels above, the four to the left and the corner (RFC 6386, 12.3).
pub(super) fn predict_subblock(
    mode: u8,
    above: &[u8; SUBBLOCK + ABOVE_RIGHT],
    left: &[u8; SUBBLOCK],
    corner: u8,
) -> [u8; SUBBLOCK_PIXELS] {
    let (a, l, p) = (above, left, corner);
    let mut block = [0u8; SUBBLOCK_PIXELS];
    match mode {
        B_DC_PRED | B_TM_PRED => {
            let edges = Edges { above: &a[..SUBBLOCK], left: l, corner, has_above: true, has_left: true };
            let mode = if mode == B_DC_PRED { DC_PRED } else { TM_PRED };
            predict(mode, SUBBLOCK, &edges, &mut block, 0, SUBBLOCK);
        }
        B_VE_PRED => {
            let row = [
                average3(p, a[0], a[1]),
                average3(a[0], a[1], a[2]),
                average3(a[1], a[2], a[3]),
                average3(a[2], a[3], a[4]),
            ];
            for line in block.as_chunks_mut::<SUBBLOCK>().0 {
                *line = row;
            }
        }
        B_HE_PRED => {
            let column = [
                average3(p, l[0], l[1]),
                average3(l[0], l[1], l[2]),
                average3(l[1], l[2], l[3]),
                average3(l[2], l[3], l[3]),
            ];
            for (line, value) in block.as_chunks_mut::<SUBBLOCK>().0.iter_mut().zip(column) {
                *line = [value; SUBBLOCK];
            }
        }
        B_LD_PRED => {
            let d = [
                average3(a[0], a[1], a[2]),
                average3(a[1], a[2], a[3]),
                average3(a[2], a[3], a[4]),
                average3(a[3], a[4], a[5]),
                average3(a[4], a[5], a[6]),
                average3(a[5], a[6], a[7]),
                average3(a[6], a[7], a[7]),
            ];
            block = [d[0], d[1], d[2], d[3], d[1], d[2], d[3], d[4], d[2], d[3], d[4], d[5], d[3], d[4], d[5], d[6]];
        }
        B_RD_PRED => {
            let d = [
                average3(l[3], l[2], l[1]),
                average3(l[2], l[1], l[0]),
                average3(l[1], l[0], p),
                average3(l[0], p, a[0]),
                average3(p, a[0], a[1]),
                average3(a[0], a[1], a[2]),
                average3(a[1], a[2], a[3]),
            ];
            block = [d[3], d[4], d[5], d[6], d[2], d[3], d[4], d[5], d[1], d[2], d[3], d[4], d[0], d[1], d[2], d[3]];
        }
        B_VR_PRED => {
            block = [
                average2(p, a[0]),
                average2(a[0], a[1]),
                average2(a[1], a[2]),
                average2(a[2], a[3]),
                average3(l[0], p, a[0]),
                average3(p, a[0], a[1]),
                average3(a[0], a[1], a[2]),
                average3(a[1], a[2], a[3]),
                average3(l[1], l[0], p),
                average2(p, a[0]),
                average2(a[0], a[1]),
                average2(a[1], a[2]),
                average3(l[2], l[1], l[0]),
                average3(l[0], p, a[0]),
                average3(p, a[0], a[1]),
                average3(a[0], a[1], a[2]),
            ];
        }
        B_VL_PRED => {
            block = [
                average2(a[0], a[1]),
                average2(a[1], a[2]),
                average2(a[2], a[3]),
                average2(a[3], a[4]),
                average3(a[0], a[1], a[2]),
                average3(a[1], a[2], a[3]),
                average3(a[2], a[3], a[4]),
                average3(a[3], a[4], a[5]),
                average2(a[1], a[2]),
                average2(a[2], a[3]),
                average2(a[3], a[4]),
                average3(a[4], a[5], a[6]),
                average3(a[1], a[2], a[3]),
                average3(a[2], a[3], a[4]),
                average3(a[3], a[4], a[5]),
                average3(a[5], a[6], a[7]),
            ];
        }
        B_HD_PRED => {
            block = [
                average2(l[0], p),
                average3(l[0], p, a[0]),
                average3(p, a[0], a[1]),
                average3(a[0], a[1], a[2]),
                average2(l[1], l[0]),
                average3(l[1], l[0], p),
                average2(l[0], p),
                average3(l[0], p, a[0]),
                average2(l[2], l[1]),
                average3(l[2], l[1], l[0]),
                average2(l[1], l[0]),
                average3(l[1], l[0], p),
                average2(l[3], l[2]),
                average3(l[3], l[2], l[1]),
                average2(l[2], l[1]),
                average3(l[2], l[1], l[0]),
            ];
        }
        B_HU_PRED => {
            block = [
                average2(l[0], l[1]),
                average3(l[0], l[1], l[2]),
                average2(l[1], l[2]),
                average3(l[1], l[2], l[3]),
                average2(l[1], l[2]),
                average3(l[1], l[2], l[3]),
                average2(l[2], l[3]),
                average3(l[2], l[3], l[3]),
                average2(l[2], l[3]),
                average3(l[2], l[3], l[3]),
                l[3],
                l[3],
                l[3],
                l[3],
                l[3],
                l[3],
            ];
        }
        _ => {}
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn edges<'a>(above: &'a [u8], left: &'a [u8], corner: u8) -> Edges<'a> {
        Edges { above, left, corner, has_above: true, has_left: true }
    }

    fn block(mode: u8, size: usize, edges: &Edges<'_>) -> alloc::vec::Vec<u8> {
        let mut plane = vec![0u8; size * size];
        predict(mode, size, edges, &mut plane, 0, size);
        plane
    }

    #[test]
    fn macroblock_predictors() {
        let above: [u8; 8] = [10, 20, 30, 40, 50, 60, 70, 80];
        let left: [u8; 8] = [100, 110, 120, 130, 140, 150, 160, 170];
        let full = edges(&above, &left, 5);
        assert_eq!(block(DC_PRED, 8, &full), [90; 64], "(360 + 1080 + 8) >> 4");
        let without_left = Edges { has_left: false, ..edges(&above, &left, 5) };
        assert_eq!(block(DC_PRED, 8, &without_left), [45; 64], "the top row of the frame's left edge: (360 + 4) >> 3");
        let without_above = Edges { has_above: false, ..edges(&above, &left, 5) };
        assert_eq!(block(DC_PRED, 8, &without_above), [135; 64], "(1080 + 4) >> 3");
        let neither = Edges { has_above: false, has_left: false, ..edges(&above, &left, 5) };
        assert_eq!(block(DC_PRED, 8, &neither), [128; 64], "the top left macroblock");
        let vertical = block(V_PRED, 8, &full);
        assert!(vertical.chunks(8).all(|row| row == above), "every row is the row above");
        let horizontal = block(H_PRED, 8, &full);
        assert!(
            horizontal.chunks(8).zip(left).all(|(row, value)| row.iter().all(|&pixel| pixel == value)),
            "every column is the column to the left"
        );
        let true_motion = block(TM_PRED, 8, &edges(&above, &left, 150));
        assert_eq!(true_motion[..8], [0, 0, 0, 0, 0, 10, 20, 30], "100 + above - 150, held at 0");
        assert_eq!(true_motion[56..], [30, 40, 50, 60, 70, 80, 90, 100], "170 + above - 150");
        let bright = block(TM_PRED, 8, &edges(&[250; 8], &[250; 8], 10));
        assert_eq!(bright, [255; 64], "250 + 250 - 10, held at 255");
    }

    #[test]
    fn subblock_predictors() {
        let above = [10, 20, 30, 40, 50, 60, 70, 80];
        let left = [90, 100, 110, 120];
        let corner = 0;
        let rows = |mode| predict_subblock(mode, &above, &left, corner);
        assert_eq!(rows(B_DC_PRED), [65; 16], "(100 + 420 + 4) >> 3");
        assert_eq!(rows(B_TM_PRED)[..4], [100, 110, 120, 130], "90 + above - 0");
        assert_eq!(rows(B_VE_PRED)[..4], [10, 20, 30, 40], "the row above smoothed, its first pixel with the corner");
        assert_eq!(rows(B_VE_PRED)[12..], [10, 20, 30, 40], "the same in every row");
        assert_eq!(
            [rows(B_HE_PRED)[0], rows(B_HE_PRED)[4], rows(B_HE_PRED)[8], rows(B_HE_PRED)[12]],
            [70, 100, 110, 118],
            "(0 + 180 + 100 + 2) >> 2 and so on, the last with the bottom pixel twice"
        );
        assert_eq!(
            rows(B_LD_PRED),
            [20, 30, 40, 50, 30, 40, 50, 60, 40, 50, 60, 70, 50, 60, 70, 78],
            "down and to the left; the last diagonal has no pixel past the eighth"
        );
        assert_eq!(rows(B_RD_PRED)[0], 25, "(90 + 0 + 10 + 2) >> 2 lies on the main diagonal");
        assert_eq!(rows(B_RD_PRED)[12], 110, "(120 + 220 + 100 + 2) >> 2 at the bottom left");
        assert_eq!(rows(B_RD_PRED)[3], 30, "(20 + 60 + 40 + 2) >> 2 at the top right");
        assert_eq!(rows(B_VR_PRED)[..4], [5, 15, 25, 35], "the averages of pairs along the row above");
        assert_eq!(rows(B_VL_PRED)[15], 70, "(60 + 140 + 80 + 2) >> 2");
        assert_eq!(rows(B_HD_PRED)[12], 115, "(120 + 110 + 1) >> 1");
        assert_eq!(
            rows(B_HU_PRED),
            [95, 100, 105, 110, 105, 110, 115, 118, 115, 118, 120, 120, 120, 120, 120, 120],
            "up the left column, the bottom pixel repeated where nothing lies on the diagonal"
        );
    }

    #[test]
    fn edge_values() {
        let row = [ABOVE_EDGE; 8];
        let column = [LEFT_EDGE; 4];
        assert_eq!(
            predict_subblock(B_DC_PRED, &row, &column, ABOVE_EDGE),
            [128; 16],
            "(4 * 127 + 4 * 129 + 4) >> 3 at the top left of the frame"
        );
        assert_eq!(
            predict_subblock(B_TM_PRED, &row, &column, ABOVE_EDGE),
            [129; 16],
            "129 + 127 - 127: the frame's top left takes the left column"
        );
    }
}

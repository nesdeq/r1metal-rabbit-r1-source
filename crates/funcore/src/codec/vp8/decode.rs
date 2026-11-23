use super::filter::{self, Kind, Strength};
use super::frame::{CHROMA_BLOCK, MACROBLOCK};
use super::header::{self, COPY_LAST, COPY_OTHER, Header, KEY_BYTES, MOST_PARTITIONS, SEGMENTS, State, TAG_BYTES};
use super::modes::{Grid, INTRA_FRAME, Macroblock};
use super::reconstruct::{self, Lines};
use super::tables::{AC_QLOOKUP, B_PRED, DC_QLOOKUP, QINDEX_RANGE, SPLITMV, ZEROMV};
use super::tokens::{self, CONTEXTS, Factors, Residue};
use crate::codec::Flaw;
use crate::codec::boolean::BoolDecoder;
use crate::codec::picture::{self, Frame, Geometry, Picture};
use alloc::vec::Vec;

// The frame being decoded and the last, golden and altref frames it may be predicted from (RFC 6386, 5).
const FRAMES: usize = 4;
// A luma border: the macroblock a clamped vector reaches past an edge, and one more for the taps (16.3, 18.3).
pub(super) const BORDER: usize = 2 * MACROBLOCK;
// The scaling and the bounds of Y2's and chroma's factors (the reference's dequant_init).
const Y2_DC_SCALE: i32 = 2;
const Y2_AC_SCALE: (i32, i32) = (155, 100);
const Y2_AC_FLOOR: i32 = 8;
const UV_DC_CEILING: i32 = 132;
// The loop filter's mode deltas: B_PRED, ZEROMV, the other whole-macroblock vectors, SPLITMV (RFC 6386, 9.4).
const B_PRED_DELTA: usize = 0;
const ZEROMV_DELTA: usize = 1;
const MV_DELTA: usize = 2;
const SPLITMV_DELTA: usize = 3;

// A decoder of a VP8 stream (RFC 6386): it keeps the reference frames from one frame to the next.
#[derive(Default)]
pub(crate) struct Decoder {
    frames: [Frame; FRAMES],
    last: usize,
    golden: usize,
    altref: usize,
    shown: usize,
    has_reference: bool,
    width: usize,
    height: usize,
    columns: usize,
    rows: usize,
    geometries: [Geometry; 3],
    state: State,
    grid: Grid,
    contexts: Vec<[u8; CONTEXTS]>,
    strengths: Vec<Strength>,
    lines: Lines,
    residue: Residue,
}

// Whether a frame is a key frame, told from its first six bytes alone.
#[cfg(test)]
pub(crate) fn is_key_frame(frame: &[u8]) -> bool {
    header::is_key(frame)
}

pub(super) fn dequantization(header: &Header, state: &State) -> [Factors; SEGMENTS] {
    let segmentation = &state.segmentation;
    let quantizers = &header.quantizers;
    core::array::from_fn(|segment| {
        let mut index = quantizers.index;
        if segmentation.is_enabled {
            let value = segmentation.quantizer[segment];
            index = if segmentation.is_absolute { value } else { index + value };
        }
        // An index is held to the table after its delta is added, as the reference's dc_q and ac_q do (14.1).
        let step =
            |table: &[i32; QINDEX_RANGE], delta: i32| table[(index + delta).clamp(0, QINDEX_RANGE as i32 - 1) as usize];
        Factors {
            y1: [step(&DC_QLOOKUP, quantizers.y_dc), step(&AC_QLOOKUP, 0)],
            y2: [
                step(&DC_QLOOKUP, quantizers.y2_dc) * Y2_DC_SCALE,
                (step(&AC_QLOOKUP, quantizers.y2_ac) * Y2_AC_SCALE.0 / Y2_AC_SCALE.1).max(Y2_AC_FLOOR),
            ],
            uv: [step(&DC_QLOOKUP, quantizers.uv_dc).min(UV_DC_CEILING), step(&AC_QLOOKUP, quantizers.uv_ac)],
        }
    })
}

// A macroblock's filter level: the frame's, the segment's, then its reference's and mode's deltas (15.4, dixie).
pub(super) fn filter_level(header: &Header, state: &State, mb: &Macroblock) -> i32 {
    let (segmentation, deltas) = (&state.segmentation, &state.deltas);
    let mut level = header.level;
    if segmentation.is_enabled {
        let value = segmentation.level[usize::from(mb.segment)];
        level = if segmentation.is_absolute { value } else { level + value }.clamp(0, filter::MAX_LEVEL);
    }
    if deltas.is_enabled {
        level += deltas.reference[usize::from(mb.reference)];
        let mode = match (mb.reference, mb.y_mode) {
            (INTRA_FRAME, B_PRED) => Some(B_PRED_DELTA),
            (INTRA_FRAME, _) => None,
            (_, ZEROMV) => Some(ZEROMV_DELTA),
            (_, SPLITMV) => Some(SPLITMV_DELTA),
            _ => Some(MV_DELTA),
        };
        level += mode.map_or(0, |mode| deltas.mode[mode]);
    }
    level
}

impl Decoder {
    // Decodes one frame: the picture to show, or none for a frame not shown. After an error it waits for a key frame.
    pub(crate) fn decode(&mut self, frame: &[u8]) -> Result<Option<Picture<'_>>, Flaw> {
        match self.decode_frame(frame) {
            Ok(is_shown) => Ok(is_shown.then(|| self.picture())),
            Err(flaw) => {
                self.has_reference = false;
                Err(flaw)
            }
        }
    }

    // Whether the decoder holds no reference frame, before its first key frame or after a frame it could not decode.
    #[cfg(test)]
    pub(super) fn needs_key_frame(&self) -> bool {
        !self.has_reference
    }

    fn picture(&self) -> Picture<'_> {
        self.frames[self.shown].picture(&self.geometries, (self.width, self.height))
    }

    // Sets the buffers to a key frame's size, allocating them only where its macroblocks differ in number.
    fn resize(&mut self, (width, height): (usize, usize)) -> Result<(), Flaw> {
        picture::check_size(width, height)?;
        let (columns, rows) = (width.div_ceil(MACROBLOCK), height.div_ceil(MACROBLOCK));
        (self.width, self.height) = (width, height);
        if (columns, rows) == (self.columns, self.rows) && !self.frames[0].planes[0].is_empty() {
            return Ok(());
        }
        (self.columns, self.rows) = (columns, rows);
        let luma = Geometry::new(columns * MACROBLOCK, rows * MACROBLOCK, BORDER);
        let chroma = Geometry::new(columns * CHROMA_BLOCK, rows * CHROMA_BLOCK, BORDER / 2);
        self.geometries = [luma, chroma, chroma];
        for frame in &mut self.frames {
            frame.allocate(&self.geometries);
        }
        self.grid.resize(columns, rows);
        self.contexts.clear();
        self.contexts.resize(columns, [0; CONTEXTS]);
        self.strengths.clear();
        self.strengths.resize(columns * rows, Strength::default());
        Ok(())
    }

    fn decode_frame(&mut self, frame: &[u8]) -> Result<bool, Flaw> {
        let tag = header::read_tag(frame)?;
        let start = if tag.is_key {
            self.resize(header::read_size(frame)?)?;
            KEY_BYTES
        } else if self.has_reference {
            TAG_BYTES
        } else {
            return Err(Flaw::Missing("key frame"));
        };
        let rest = frame.get(start..).ok_or(Flaw::Short)?;
        let (first, after) = rest.split_at_checked(tag.first_size).ok_or(Flaw::Short)?;
        let mut decoder = BoolDecoder::new(first);
        let header = header::read(&mut decoder, tag, &mut self.state)?;
        let partitions = header::split_partitions(after, header.partitions)?;
        let references = [self.last, self.golden, self.altref];
        let index = (0..FRAMES).find(|index| !references.contains(index)).unwrap_or(0);
        let mut current = core::mem::take(&mut self.frames[index]);
        let result = self.reconstruct(&header, &mut decoder, partitions, &mut current);
        self.frames[index] = current;
        result?;
        if let Some(saved) = self.state.saved.take() {
            self.state.probabilities = saved;
        }
        self.update_references(&header, index);
        self.shown = index;
        self.has_reference = true;
        Ok(tag.is_shown)
    }

    // The buffer copies, then the refreshes (RFC 6386, 9.7 and 9.8, in the reference's order: altref's copy first).
    fn update_references(&mut self, header: &Header, current: usize) {
        match header.copy_to_altref {
            COPY_LAST => self.altref = self.last,
            COPY_OTHER => self.altref = self.golden,
            _ => {}
        }
        match header.copy_to_golden {
            COPY_LAST => self.golden = self.last,
            COPY_OTHER => self.golden = self.altref,
            _ => {}
        }
        if header.refresh_golden {
            self.golden = current;
        }
        if header.refresh_altref {
            self.altref = current;
        }
        if header.refresh_last {
            self.last = current;
        }
    }

    // Decodes the macroblocks row by row into `current`, and filters each row once the row under it is decoded.
    fn reconstruct(
        &mut self,
        header: &Header,
        first: &mut BoolDecoder<'_>,
        partitions: [&[u8]; MOST_PARTITIONS],
        current: &mut Frame,
    ) -> Result<(), Flaw> {
        let Decoder {
            frames,
            last,
            golden,
            altref,
            columns,
            rows,
            geometries,
            state,
            grid,
            contexts,
            strengths,
            lines,
            residue,
            ..
        } = self;
        let (columns, rows, geometries) = (*columns, *rows, &*geometries);
        // By reference frame, of which the first, the current frame, is never read.
        let references = [&frames[*last], &frames[*last], &frames[*golden], &frames[*altref]];
        let factors = dequantization(header, state);
        let mut tokens = partitions.map(BoolDecoder::new);
        contexts.fill([0; CONTEXTS]);
        lines.reset(geometries);
        for row in 0..rows {
            let partition = &mut tokens[row % header.partitions];
            let mut left = [0u8; CONTEXTS];
            for (column, above) in contexts.iter_mut().enumerate() {
                grid.read(first, header, state, (row, column));
                let mb = grid.get(row, column);
                let has_y2 = mb.y_mode != B_PRED && mb.y_mode != SPLITMV;
                let is_coded = if mb.skip {
                    tokens::skip(has_y2, (&mut left, above));
                    false
                } else {
                    let factors = &factors[usize::from(mb.segment)];
                    tokens::read_macroblock(
                        partition,
                        &state.probabilities.coeff,
                        has_y2,
                        factors,
                        (&mut left, above),
                        residue,
                    )
                };
                let inner = is_coded || !has_y2;
                strengths[row * columns + column] =
                    Strength::new(filter_level(header, state, mb), header.sharpness, header.is_key, inner);
                let (luma, chroma) = (geometries[0], geometries[1]);
                let [y, u, v] = &mut current.planes;
                let at = luma.at(column * MACROBLOCK, row * MACROBLOCK);
                let chroma_at = chroma.at(column * CHROMA_BLOCK, row * CHROMA_BLOCK);
                if mb.reference == INTRA_FRAME {
                    let targets = ((&mut y[..], at, luma.stride), ([&mut u[..], &mut v[..]], chroma_at, chroma.stride));
                    reconstruct::intra(mb, residue, targets, lines.above(column), (column > 0, row > 0));
                } else {
                    let reference = references[usize::from(mb.reference)];
                    let source = (&reference.planes[0][..], luma);
                    reconstruct::predict_luma(mb, source, (row, column), header.filter, (&mut y[..], at, luma.stride));
                    let targets = ((&mut y[..], at, luma.stride), ([&mut u[..], &mut v[..]], chroma_at, chroma.stride));
                    let options = (header.filter, header.is_full_pixel);
                    reconstruct::inter(mb, residue, targets, (reference, geometries), (row, column), options);
                }
                residue.clear();
            }
            if first.is_past_end() || partition.is_past_end() {
                return Err(Flaw::Short);
            }
            lines.keep(current, geometries, row);
            if row > 0 && header.level > 0 {
                filter_row(current, geometries, row - 1, &strengths[(row - 1) * columns..row * columns], header.kind);
            }
        }
        if header.level > 0 && rows > 0 {
            filter_row(current, geometries, rows - 1, &strengths[(rows - 1) * columns..rows * columns], header.kind);
        }
        current.extend(geometries);
        Ok(())
    }
}

fn filter_row(frame: &mut Frame, geometries: &[Geometry; 3], row: usize, strengths: &[Strength], kind: Kind) {
    let [luma, u, v] = &mut frame.planes;
    let planes = [(&mut luma[..], geometries[0]), (&mut u[..], geometries[1]), (&mut v[..], geometries[2])];
    filter::filter_row(planes, row, strengths, kind);
}

#[cfg(test)]
mod tests {
    use super::super::modes::{GOLDEN_FRAME, LAST_FRAME};
    use super::super::tables::NEWMV;
    use super::*;

    #[test]
    fn references_after_copies() {
        let mut decoder = Decoder { last: 0, golden: 1, altref: 2, ..Decoder::default() };
        let header = |copy_to_golden, copy_to_altref, refresh_last| Header {
            copy_to_golden,
            copy_to_altref,
            refresh_last,
            ..Header::default()
        };
        decoder.update_references(&header(COPY_OTHER, COPY_OTHER, false), 3);
        assert_eq!(
            (decoder.last, decoder.golden, decoder.altref),
            (0, 1, 1),
            "altref takes golden first, and golden then takes the altref it became"
        );
        decoder.update_references(&header(COPY_LAST, 0, true), 3);
        assert_eq!(
            (decoder.last, decoder.golden, decoder.altref),
            (3, 0, 1),
            "golden takes the last frame before it is replaced"
        );
    }

    #[test]
    fn quantizer_indices_held() {
        let mut state = State::default();
        let header = |index, y2_ac| Header {
            quantizers: header::Quantizers { index, y2_ac, uv_dc: -3, ..header::Quantizers::default() },
            ..Header::default()
        };
        let factors = dequantization(&header(0, 0), &state)[0];
        assert_eq!(factors, Factors { y1: [4, 4], y2: [8, 8], uv: [4, 4] }, "index 0: 4 * 155 / 100 is 6, raised to 8");
        let factors = dequantization(&header(127, 5), &state)[0];
        assert_eq!(factors.y2[1], 284 * 155 / 100, "an index of 127 plus 5 is held at 127");
        assert_eq!(factors.uv[0], 132, "chroma's DC is held at 132: index 124 gives 148");
        state.segmentation =
            header::Segmentation { is_enabled: true, quantizer: [-20, 0, 0, 10], ..header::Segmentation::default() };
        let factors = dequantization(&header(10, 0), &state);
        assert_eq!((factors[0].y1, factors[3].y1), ([4, 4], [21, 24]), "deltas of a segment below 0 and to 20");
        state.segmentation.is_absolute = true;
        assert_eq!(dequantization(&header(10, 0), &state)[3].y1, [13, 14], "an absolute index of 10");
    }

    #[test]
    fn filter_levels_by_segment_and_deltas() {
        let mut state = State::default();
        let header = Header { level: 60, ..Header::default() };
        state.segmentation =
            header::Segmentation { is_enabled: true, level: [10, 0, 0, 0], ..header::Segmentation::default() };
        state.deltas = header::Deltas { is_enabled: true, reference: [0, -10, 0, 0], mode: [4, -2, 2, 4] };
        let zero = Macroblock { reference: LAST_FRAME, y_mode: ZEROMV, ..Macroblock::default() };
        assert_eq!(filter_level(&header, &state, &zero), 51, "60 + 10 held at 63, then -10 and -2");
        let bpred = Macroblock { reference: INTRA_FRAME, y_mode: B_PRED, ..Macroblock::default() };
        assert_eq!(filter_level(&header, &state, &bpred), 67, "63 + 4: the strength holds it to 63 in its turn");
        let golden = Macroblock { reference: GOLDEN_FRAME, y_mode: NEWMV, segment: 1, ..zero };
        assert_eq!(filter_level(&header, &state, &golden), 62, "60, no reference delta for golden, + 2");
    }
}

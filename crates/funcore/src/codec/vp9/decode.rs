use super::filter::{self, Levels};
use super::header::{self, Compressed, Header, Kept, NUM_REF_FRAMES, Parsed};
use super::inter::{self, Scale, Source, Target};
use super::intra::{self, Block};
use super::modes::{self, Edges, Info, Motion, Mv, Place, Scope};
use super::probs::{Counts, Probs};
use super::tables::{
    AC_QLOOKUP, B_HEIGHT_LOG2, B_WIDTH_LOG2, BLOCK_8X8, BLOCK_64X64, DC_QLOOKUP, DCT_DCT, KF_PARTITION_PROBS,
    LAST_FRAME, MAX_SEGMENTS, MI_SIZE, MI_WIDTH_LOG2, MODE_TO_TX_TYPE, MOST_SIDE, NUM_8X8_HIGH, NUM_8X8_WIDE,
    PARTITION_HORZ, PARTITION_NONE, PARTITION_SPLIT, PARTITION_TREE, PARTITION_VERT, QINDEX_RANGE, SEG_LVL_ALT_Q,
    SUBSIZE, SUPERBLOCK_MI, SWITCHABLE, TX_32X32, TX_MODE_SELECT,
};
use super::tokens::Tokens;
use super::transform;
use crate::codec::Flaw;
use crate::codec::boolean::BoolDecoder;
use crate::codec::picture::{self, Frame, Geometry, Picture};
use alloc::vec::Vec;

// The frame contexts kept, and the buffers: one for each reference slot and one for the frame decoded (VP9 3).
const FRAME_CONTEXTS: usize = 4;
const BUFFERS: usize = NUM_REF_FRAMES + 1;
// A superframe's index: a marker byte at either end of the frames' sizes of 1 to 4 bytes each (VP9 B.3).
const SUPERFRAME_MASK: u8 = 0xE0;
const SUPERFRAME_MARKER: u8 = 0xC0;
const FRAMES_MASK: u8 = 0x07;
const SIZE_BYTES_SHIFT: u32 = 3;
const SIZE_BYTES_MASK: u8 = 0x03;
const MARKER_BYTES: usize = 2;
// A shown existing frame is one byte in profile 0 (VP9 6.2).
const EXISTING_BYTES: usize = 1;
// 4x4 units of 4 samples (VP9 3).
const UNIT: usize = 4;
const UNITS_PER_MI: usize = MI_SIZE / UNIT;
// A block's distance to an edge in eighths of a sample, as 4x4 units of luma: 32 eighths each (VP9 6.4.22).
const EIGHTHS_PER_UNIT_LOG2: u32 = 5;
// The four subblocks of a block under 8x8, whose vectors a chroma block's is the mean of (VP9 8.5.2.1).
const SUBBLOCKS: i32 = 4;
// The partition contexts of each block size: above and left, two bits (VP9 9.3.2).
const PARTITION_CONTEXTS: usize = 4;
// Each tile but the last is preceded by its size in 4 bytes (VP9 6.4).
const TILE_SIZE_BYTES: usize = 4;
// reset_frame_context: the context the header names, or all four (VP9 7.2).
const RESET_ONE: u8 = 2;
const RESET_ALL: u8 = 3;
// The coefficient adaptation's largest factor, larger just after a key frame (VP9 8.4.3).
const COEF_MAX_UPDATE_FACTOR: u32 = 112;
const COEF_MAX_UPDATE_FACTOR_AFTER_KEY: u32 = 128;
// The coefficients of the largest transform.
const MOST_COEFFICIENTS: usize = MOST_SIDE * MOST_SIDE;
// The left nonzero contexts of a superblock's luma: 16 rows of 4x4 units.
const LEFT_UNITS: usize = 16;
// A partition's context bits, the highest for the smallest block (VP9 9.3.2, as libvpx keeps them).
const PARTITION_BITS: u8 = 15;

// A decoder of a VP9 profile 0 stream: it keeps the eight reference slots and the four frame contexts.
#[derive(Default)]
pub(crate) struct Decoder {
    buffers: Vec<Buffer>,
    slots: [Option<usize>; NUM_REF_FRAMES],
    is_synced: bool,
    kept: Kept,
    contexts: [Probs; FRAME_CONTEXTS],
    // The type of the last frame whose header was read, and the last frame decoded: whether shown, its size.
    was_key: bool,
    was_intra_only: bool,
    was_shown: bool,
    last_size: (usize, usize),
    size: (usize, usize),
    grid: Vec<Info>,
    segments: Vec<u8>,
    previous_segments: Vec<u8>,
    motion: Vec<Motion>,
    previous_motion: Vec<Motion>,
    counts: Counts,
    above_nonzero: [Vec<u8>; 3],
    above_partition: Vec<u8>,
}

// A frame buffer and the size of the frame in it.
#[derive(Default)]
struct Buffer {
    frame: Frame,
    width: usize,
    height: usize,
    geometries: [Geometry; 3],
}

// The frames' sizes of a superframe index at the end of the data, none where it has no index (VP9 B.3).
pub(super) fn superframe_sizes(data: &[u8]) -> Result<Option<Vec<usize>>, Flaw> {
    let Some(&marker) = data.last() else {
        return Err(Flaw::Short);
    };
    if marker & SUPERFRAME_MASK != SUPERFRAME_MARKER {
        return Ok(None);
    }
    let frames = usize::from(marker & FRAMES_MASK) + 1;
    let size_bytes = usize::from((marker >> SIZE_BYTES_SHIFT) & SIZE_BYTES_MASK) + 1;
    let index_size = MARKER_BYTES + size_bytes * frames;
    let Some(index) = data.len().checked_sub(index_size).map(|start| &data[start..]) else {
        return Err(Flaw::Invalid("superframe index"));
    };
    if index[0] != marker {
        return Err(Flaw::Invalid("superframe index"));
    }
    let sizes = index[1..1 + size_bytes * frames]
        .chunks_exact(size_bytes)
        .map(|bytes| bytes.iter().rev().fold(0usize, |size, &byte| size << 8 | usize::from(byte)))
        .collect();
    Ok(Some(sizes))
}

// The 8x8 blocks across and down of a frame, and its planes' geometries: whole superblocks, chroma of half the size.
fn layout(width: usize, height: usize) -> ((usize, usize), [Geometry; 3]) {
    let (mi_cols, mi_rows) = (width.div_ceil(MI_SIZE), height.div_ceil(MI_SIZE));
    let (columns, rows) = (mi_cols.div_ceil(SUPERBLOCK_MI), mi_rows.div_ceil(SUPERBLOCK_MI));
    let luma = Geometry::new(columns * SUPERBLOCK_MI * MI_SIZE, rows * SUPERBLOCK_MI * MI_SIZE, 0);
    let chroma = Geometry::new(columns * SUPERBLOCK_MI * MI_SIZE / 2, rows * SUPERBLOCK_MI * MI_SIZE / 2, 0);
    ((mi_cols, mi_rows), [luma, chroma, chroma])
}

// The start of each tile's columns or rows in 8x8 blocks, and its end (VP9 6.4.1, get_tile_offset).
fn tile_bounds(index: usize, mis: usize, log2: u32) -> (usize, usize) {
    let superblocks = mis.div_ceil(SUPERBLOCK_MI);
    let offset = |index: usize| (((index * superblocks) >> log2) * SUPERBLOCK_MI).min(mis);
    (offset(index), offset(index + 1))
}

// The dequantization factors of each segment: luma's DC and AC, then chroma's (VP9 8.6.1).
fn dequantization(header: &Header) -> [[[i32; 2]; 2]; MAX_SEGMENTS] {
    let step = |table: &[i32; QINDEX_RANGE], index: i32| table[index.clamp(0, QINDEX_RANGE as i32 - 1) as usize];
    core::array::from_fn(|segment| {
        let index = match header.segmentation.feature(segment as u8, SEG_LVL_ALT_Q) {
            Some(data) if header.segmentation.is_absolute => data,
            Some(data) => header.base_q + data,
            None => header.base_q,
        }
        .clamp(0, QINDEX_RANGE as i32 - 1);
        let [y_dc, uv_dc, uv_ac] = header.delta_q;
        [
            [step(&DC_QLOOKUP, index + y_dc), step(&AC_QLOOKUP, index)],
            [step(&DC_QLOOKUP, index + uv_dc), step(&AC_QLOOKUP, index + uv_ac)],
        ]
    })
}

impl Decoder {
    // Decodes a frame, a superframe or frames one after another: the picture of the last if shown.
    pub(crate) fn decode(&mut self, data: &[u8]) -> Result<Option<Picture<'_>>, Flaw> {
        let shown = match self.decode_all(data) {
            Ok(shown) => shown,
            Err(flaw) => {
                self.is_synced = false;
                return Err(flaw);
            }
        };
        Ok(shown.map(|index| {
            let buffer = &self.buffers[index];
            buffer.frame.picture(&buffer.geometries, (buffer.width, buffer.height))
        }))
    }

    // Whether the next frame must be a key frame or an intra-only frame: before the first, or after an error.
    #[cfg(test)]
    pub(super) fn needs_key_frame(&self) -> bool {
        !self.is_synced
    }

    // A superframe's frames by its index; frames without one back to back, the zeros after each passed over (B.3): the buffer the last one shows.
    fn decode_all(&mut self, data: &[u8]) -> Result<Option<usize>, Flaw> {
        let mut shown = None;
        if let Some(sizes) = superframe_sizes(data)? {
            let mut start = 0;
            for size in sizes {
                let frame = data.get(start..start + size).ok_or(Flaw::Invalid("superframe index"))?;
                (_, shown) = self.decode_frame(frame)?;
                start += size;
            }
            return Ok(shown);
        }
        let mut start = 0;
        while start < data.len() {
            let used;
            (used, shown) = self.decode_frame(&data[start..])?;
            start += used;
            while data.get(start) == Some(&0) {
                start += 1;
            }
        }
        Ok(shown)
    }

    // Decodes one frame: the bytes it took, and the buffer it shows.
    fn decode_frame(&mut self, frame: &[u8]) -> Result<(usize, Option<usize>), Flaw> {
        let sizes = self.slots.map(|slot| slot.map(|index| (self.buffers[index].width, self.buffers[index].height)));
        let header = match header::read(frame, &mut self.kept, (&sizes, !self.is_synced), picture::check_size)? {
            Parsed::Existing(slot) => {
                let index = self.slots[slot].ok_or(Flaw::Missing("reference frame"))?;
                return Ok((EXISTING_BYTES, self.is_synced.then_some(index)));
            }
            Parsed::Frame(header) => *header,
        };
        let (was_key, was_intra_only) = (self.was_key, self.was_intra_only);
        (self.was_key, self.was_intra_only) = (header.is_key, header.is_intra_only);
        if !self.is_synced {
            self.slots = [None; NUM_REF_FRAMES];
        }
        self.is_synced = true;
        let (width, height) = (header.width, header.height);
        let mut references: [Option<(usize, Scale)>; 3] = [None; 3];
        if !header.is_intra() {
            let mut has_valid = false;
            for (reference, &slot) in references.iter_mut().zip(&header.refs) {
                let index = self.slots[slot].ok_or(Flaw::Missing("reference frame"))?;
                let buffer = &self.buffers[index];
                if let Some(scale) = Scale::of((buffer.width, buffer.height), (width, height)) {
                    *reference = Some((index, scale));
                    has_valid = true;
                }
            }
            if !has_valid {
                return Err(Flaw::Invalid("reference frame size"));
            }
        }
        let ((mi_cols, mi_rows), geometries) = layout(width, height);
        let cells = mi_cols * mi_rows;
        // A new size forgets the last segment map, as libvpx's resize_context_buffers does.
        if (width, height) != self.size {
            self.size = (width, height);
            self.previous_segments.clear();
        }
        for map in [&mut self.segments, &mut self.previous_segments] {
            map.resize(cells, 0);
        }
        self.grid.clear();
        self.grid.resize(cells, Info::default());
        self.motion.resize(cells, Motion::default());
        let is_independent = header.is_intra() || header.is_error_resilient;
        if is_independent {
            self.segments.fill(0);
            self.previous_segments.fill(0);
            if header.is_key || header.is_error_resilient || header.reset_context == RESET_ALL {
                self.contexts = core::array::from_fn(|_| Probs::default());
            } else if header.reset_context == RESET_ONE {
                self.contexts[header.context] = Probs::default();
            }
        }
        let context = if is_independent { 0 } else { header.context };
        let mut probs = self.contexts[context].clone();
        let previous = probs.clone();
        let compressed_data = frame.get(header.size..header.size + header.compressed_size).ok_or(Flaw::Short)?;
        let mut decoder = BoolDecoder::open(compressed_data)?;
        let compressed = header::read_compressed(&mut decoder, &header, &mut probs);
        if decoder.is_past_end() {
            return Err(Flaw::Short);
        }
        if !decoder.is_padding_zero() {
            return Err(Flaw::Invalid("padding"));
        }
        let tiles = &frame[header.size + header.compressed_size..];
        let index = (0..BUFFERS).find(|index| !self.slots.contains(&Some(*index))).unwrap_or(0);
        if index >= self.buffers.len() {
            self.buffers.resize_with(index + 1, Buffer::default);
        }
        let mut buffer = core::mem::take(&mut self.buffers[index]);
        buffer.frame.allocate(&geometries);
        (buffer.width, buffer.height, buffer.geometries) = (width, height, geometries);
        let use_previous = !header.is_error_resilient
            && (width, height) == self.last_size
            && !was_intra_only
            && self.was_shown
            && !was_key;
        let result = self.decode_tiles(
            &header,
            (&compressed, &probs),
            (&mut buffer, references, use_previous),
            (mi_rows, mi_cols),
            tiles,
        );
        self.buffers[index] = buffer;
        let used = result?;
        if header.loop_filter.level > 0 {
            let levels = Levels::of(&header.loop_filter, &header.segmentation);
            let buffer = &mut self.buffers[index];
            filter::filter_frame(&mut buffer.frame.planes, &geometries, &self.grid, (mi_rows, mi_cols), &levels);
        }
        if !header.is_error_resilient && !header.is_parallel {
            let factor =
                if !header.is_intra() && was_key { COEF_MAX_UPDATE_FACTOR_AFTER_KEY } else { COEF_MAX_UPDATE_FACTOR };
            probs.adapt_coef(&previous, &self.counts, factor);
            if !header.is_intra() {
                let choices = (header.filter == SWITCHABLE, compressed.tx_mode == TX_MODE_SELECT, header.allow_hp);
                probs.adapt_modes(&previous, &self.counts, choices);
            }
        }
        if header.refresh_context {
            self.contexts[context] = probs;
        }
        for (bit, slot) in self.slots.iter_mut().enumerate() {
            if header.refresh >> bit & 1 == 1 {
                *slot = Some(index);
            }
        }
        (self.was_shown, self.last_size) = (header.is_shown, (width, height));
        core::mem::swap(&mut self.motion, &mut self.previous_motion);
        if header.segmentation.is_enabled {
            core::mem::swap(&mut self.segments, &mut self.previous_segments);
        }
        Ok((header.size + header.compressed_size + used, header.is_shown.then_some(index)))
    }

    // Decodes every tile into the buffer: the bytes of the tiles taken, to the end of the last tile's bits (VP9 6.4).
    fn decode_tiles(
        &mut self,
        header: &Header,
        (compressed, probs): (&Compressed, &Probs),
        (buffer, references, use_previous): (&mut Buffer, [Option<(usize, Scale)>; 3], bool),
        (mi_rows, mi_cols): (usize, usize),
        tiles: &[u8],
    ) -> Result<usize, Flaw> {
        let Decoder {
            buffers,
            grid,
            segments,
            previous_segments,
            motion,
            previous_motion,
            counts,
            above_nonzero,
            above_partition,
            ..
        } = self;
        *counts = Counts::default();
        let superblock_cols = mi_cols.div_ceil(SUPERBLOCK_MI);
        for (plane, above) in above_nonzero.iter_mut().enumerate() {
            above.clear();
            above.resize(superblock_cols * (LEFT_UNITS >> usize::from(plane > 0)), 0);
        }
        above_partition.clear();
        above_partition.resize(superblock_cols * SUPERBLOCK_MI, 0);
        let sources = references.map(|reference| {
            reference.map(|(index, scale)| {
                let source = &buffers[index];
                let planes: [Source<'_>; 3] = core::array::from_fn(|plane| {
                    let (width, height) = if plane == 0 {
                        (source.width, source.height)
                    } else {
                        (source.width.div_ceil(2), source.height.div_ceil(2))
                    };
                    Source {
                        plane: &source.frame.planes[plane],
                        stride: source.geometries[plane].stride,
                        width,
                        height,
                    }
                });
                (planes, scale)
            })
        });
        let scope = Scope {
            header,
            compressed,
            probs,
            previous: use_previous.then_some(&previous_motion[..]),
            previous_segments: &previous_segments[..],
            mi_rows,
            mi_cols,
            tile: (0, 0),
        };
        let mut decoding = Decoding {
            scope,
            sources,
            dequant: dequantization(header),
            grid,
            segments,
            motion,
            planes: &mut buffer.frame.planes,
            strides: buffer.geometries.map(|geometry| geometry.stride),
            counts,
            above_nonzero,
            above_partition,
            left_nonzero: [[0; LEFT_UNITS]; 3],
            left_partition: [0; SUPERBLOCK_MI],
            coefficients: [0; MOST_COEFFICIENTS],
            cache: [0; MOST_COEFFICIENTS],
            middle: [0; MOST_COEFFICIENTS],
            inter_scratch: inter::Scratch::new(),
            intra_scratch: [[0; MOST_SIDE]; MOST_SIDE],
        };
        let (tile_rows, tile_cols) = (1usize << header.tile_rows_log2, 1usize << header.tile_cols_log2);
        let mut rest = tiles;
        let mut used = 0;
        for tile_row in 0..tile_rows {
            let rows = tile_bounds(tile_row, mi_rows, header.tile_rows_log2);
            for tile_col in 0..tile_cols {
                let is_last = tile_row == tile_rows - 1 && tile_col == tile_cols - 1;
                let size = if is_last {
                    rest.len()
                } else {
                    let (head, after) = rest.split_first_chunk::<TILE_SIZE_BYTES>().ok_or(Flaw::Short)?;
                    rest = after;
                    used += TILE_SIZE_BYTES;
                    let size = u32::from_be_bytes(*head) as usize;
                    if size > rest.len() {
                        return Err(Flaw::Short);
                    }
                    size
                };
                let (data, after) = rest.split_at(size);
                let mut decoder = BoolDecoder::open(data)?;
                let columns = tile_bounds(tile_col, mi_cols, header.tile_cols_log2);
                decoding.scope.tile = columns;
                for row in (rows.0..rows.1).step_by(SUPERBLOCK_MI) {
                    decoding.left_nonzero = [[0; LEFT_UNITS]; 3];
                    decoding.left_partition = [0; SUPERBLOCK_MI];
                    for col in (columns.0..columns.1).step_by(SUPERBLOCK_MI) {
                        decoding.partition(&mut decoder, (row, col), BLOCK_64X64)?;
                    }
                }
                if decoder.is_past_end() {
                    return Err(Flaw::Short);
                }
                if is_last {
                    used += decoder.end();
                } else {
                    if !decoder.is_padding_zero() {
                        return Err(Flaw::Invalid("padding"));
                    }
                    used += size;
                }
                rest = after;
            }
        }
        Ok(used)
    }
}

// A frame being decoded: what its blocks read, and what they write.
struct Decoding<'a> {
    scope: Scope<'a>,
    // By reference frame from LAST_FRAME: each plane of its buffer and its scale.
    sources: [Option<([Source<'a>; 3], Scale)>; 3],
    dequant: [[[i32; 2]; 2]; MAX_SEGMENTS],
    grid: &'a mut [Info],
    segments: &'a mut [u8],
    motion: &'a mut [Motion],
    planes: &'a mut [Vec<u8>; 3],
    strides: [usize; 3],
    counts: &'a mut Counts,
    above_nonzero: &'a mut [Vec<u8>; 3],
    above_partition: &'a mut Vec<u8>,
    left_nonzero: [[u8; LEFT_UNITS]; 3],
    left_partition: [u8; SUPERBLOCK_MI],
    coefficients: [i32; MOST_COEFFICIENTS],
    cache: [u8; MOST_COEFFICIENTS],
    // The room the transform and the predictions work in, kept for the frame so that no block clears it whole.
    middle: [i64; MOST_COEFFICIENTS],
    inter_scratch: inter::Scratch,
    intra_scratch: intra::Scratch,
}

impl Decoding<'_> {
    // decode_partition: a block's partition, then its blocks or the four quarters (VP9 6.4.2, 6.4.3).
    fn partition(&mut self, decoder: &mut BoolDecoder<'_>, (row, col): (usize, usize), size: u8) -> Result<(), Flaw> {
        let Scope { header, probs, mi_rows, mi_cols, .. } = self.scope;
        if row >= mi_rows || col >= mi_cols {
            return Ok(());
        }
        let width = usize::from(NUM_8X8_WIDE[usize::from(size)]);
        let half = width / 2;
        let has_rows = row + half < mi_rows;
        let has_cols = col + half < mi_cols;
        let level = MI_WIDTH_LOG2[usize::from(size)];
        let above = (self.above_partition[col] >> level) & 1;
        let left = (self.left_partition[row % SUPERBLOCK_MI] >> level) & 1;
        let context = usize::from(left * 2 + above) + usize::from(level) * PARTITION_CONTEXTS;
        let probs = if header.is_intra() { &KF_PARTITION_PROBS[context] } else { &probs.partition[context] };
        let partition = match (has_rows, has_cols) {
            (true, true) => decoder.tree(&PARTITION_TREE, probs),
            (false, true) => {
                if decoder.read(probs[1]) {
                    PARTITION_SPLIT
                } else {
                    PARTITION_HORZ
                }
            }
            (true, false) => {
                if decoder.read(probs[2]) {
                    PARTITION_SPLIT
                } else {
                    PARTITION_VERT
                }
            }
            (false, false) => PARTITION_SPLIT,
        };
        self.counts.partition[context][usize::from(partition)] += 1;
        let subsize = SUBSIZE[usize::from(partition)][usize::from(size)];
        if half == 0 {
            self.block(decoder, Place { row, col, size: subsize })?;
        } else {
            match partition {
                PARTITION_NONE => self.block(decoder, Place { row, col, size: subsize })?,
                PARTITION_HORZ => {
                    self.block(decoder, Place { row, col, size: subsize })?;
                    if has_rows {
                        self.block(decoder, Place { row: row + half, col, size: subsize })?;
                    }
                }
                PARTITION_VERT => {
                    self.block(decoder, Place { row, col, size: subsize })?;
                    if has_cols {
                        self.block(decoder, Place { row, col: col + half, size: subsize })?;
                    }
                }
                _ => {
                    for (down, across) in [(0, 0), (0, half), (half, 0), (half, half)] {
                        self.partition(decoder, (row + down, col + across), subsize)?;
                    }
                }
            }
        }
        if size >= BLOCK_8X8 && (size == BLOCK_8X8 || partition != PARTITION_SPLIT) {
            let above = (PARTITION_BITS << B_WIDTH_LOG2[usize::from(subsize)]) & PARTITION_BITS;
            let left = (PARTITION_BITS << B_HEIGHT_LOG2[usize::from(subsize)]) & PARTITION_BITS;
            self.above_partition[col..col + width].fill(above);
            let start = row % SUPERBLOCK_MI;
            self.left_partition[start..(start + width).min(SUPERBLOCK_MI)].fill(left);
        }
        Ok(())
    }

    // decode_block: a block's mode info, its prediction and its residual (VP9 6.4.3, 8.5, 8.6).
    fn block(&mut self, decoder: &mut BoolDecoder<'_>, place: Place) -> Result<(), Flaw> {
        let (mi_rows, mi_cols) = (self.scope.mi_rows, self.scope.mi_cols);
        let info = modes::read(&self.scope, self.grid, self.segments, decoder, self.counts, place)?;
        self.spread(place, |cell| *cell = info);
        if !self.scope.header.is_intra() {
            let motion = Motion { refs: info.refs, mvs: info.mvs[3] };
            for cells in place.rows(mi_rows, mi_cols) {
                self.motion[cells].fill(motion);
            }
        }
        let edges = Edges::of(place, mi_rows, mi_cols);
        if info.is_skip {
            for plane in 0..3 {
                let (units_wide, units_high) = Self::units(place.size, plane);
                let (x, y) = Self::unit_place(place, plane);
                self.above_nonzero[plane][x..x + units_wide].fill(0);
                self.left_nonzero[plane][y..(y + units_high).min(LEFT_UNITS)].fill(0);
            }
        }
        if !info.is_inter {
            for plane in 0..3 {
                self.intra_plane(decoder, place, &info, plane, edges);
            }
        } else {
            self.predict(place, &info, edges)?;
            if !info.is_skip {
                let mut total = 0;
                for plane in 0..3 {
                    total += self.residual_plane(decoder, place, &info, plane, edges);
                }
                // An inter block of 8x8 or more with no coefficient counts as skipped from here on, as in libvpx.
                if place.size >= BLOCK_8X8 && total == 0 {
                    self.spread(place, |cell| cell.is_skip = true);
                }
            }
        }
        if decoder.is_past_end() {
            return Err(Flaw::Short);
        }
        Ok(())
    }

    // Writes to the mode info of each 8x8 place of a block inside the frame.
    fn spread(&mut self, place: Place, mut write: impl FnMut(&mut Info)) {
        for cells in place.rows(self.scope.mi_rows, self.scope.mi_cols) {
            self.grid[cells].iter_mut().for_each(&mut write);
        }
    }

    // The 4x4 units of a block in a plane, across and down: a block under 8x8 counts as 8x8 (VP9 6.4.22).
    fn units(size: u8, plane: usize) -> (usize, usize) {
        let sub = usize::from(plane > 0);
        let size = usize::from(size);
        (
            (usize::from(NUM_8X8_WIDE[size]) * UNITS_PER_MI) >> sub,
            (usize::from(NUM_8X8_HIGH[size]) * UNITS_PER_MI) >> sub,
        )
    }

    // The block's first 4x4 unit in the plane's above contexts and in the superblock's left ones.
    fn unit_place(place: Place, plane: usize) -> (usize, usize) {
        let sub = usize::from(plane > 0);
        ((place.col * UNITS_PER_MI) >> sub, ((place.row % SUPERBLOCK_MI) * UNITS_PER_MI) >> sub)
    }

    // The 4x4 units of a block a plane has inside the frame, across and down (VP9 6.4.22, max_blocks_wide).
    fn visible_units(place: Place, plane: usize, edges: Edges) -> (usize, usize) {
        let sub = plane > 0;
        let (units_wide, units_high) = Self::units(place.size, plane);
        let shift = EIGHTHS_PER_UNIT_LOG2 + u32::from(sub);
        let wide = if edges.right < 0 { units_wide as i32 + (edges.right >> shift) } else { units_wide as i32 };
        let high = if edges.bottom < 0 { units_high as i32 + (edges.bottom >> shift) } else { units_high as i32 };
        (wide.max(0) as usize, high.max(0) as usize)
    }

    // Intra prediction and reconstruction of each transform block of a plane in raster order (VP9 8.5.1, 8.6.2).
    fn intra_plane(&mut self, decoder: &mut BoolDecoder<'_>, place: Place, info: &Info, plane: usize, edges: Edges) {
        let sub = usize::from(plane > 0);
        let tx_size = info.plane_tx_size(plane);
        let step = 1usize << tx_size;
        let (wide, high) = Self::visible_units(place, plane, edges);
        let block_log2 = usize::from(B_WIDTH_LOG2[usize::from(place.size.max(BLOCK_8X8))]) - sub;
        let (x, y) = place.origin(plane);
        let dims = ((self.scope.mi_cols * MI_SIZE) >> sub, (self.scope.mi_rows * MI_SIZE) >> sub);
        // A 32x32 block has the DCT alone, whatever its intra mode (VP9 8.7.2).
        let is_dct = plane > 0 || self.scope.header.is_lossless || tx_size == TX_32X32;
        for row in (0..high).step_by(step) {
            for col in (0..wide).step_by(step) {
                let mode = if plane > 0 {
                    info.uv_mode
                } else if place.size < BLOCK_8X8 {
                    info.sub_modes[(row << 1) + col]
                } else {
                    info.mode
                };
                let block = Block {
                    x: x + col * UNIT,
                    y: y + row * UNIT,
                    size: tx_size,
                    has_above: row > 0 || place.row > 0,
                    has_left: col > 0 || place.col > self.scope.tile.0,
                    has_right: col + step < 1 << block_log2,
                };
                let scratch = &mut self.intra_scratch;
                intra::predict(&mut self.planes[plane], self.strides[plane], dims, block, mode, scratch);
                if !info.is_skip {
                    let kind = if is_dct { DCT_DCT } else { MODE_TO_TX_TYPE[usize::from(mode)] };
                    self.residual(decoder, (place, info, plane), (row, col), (tx_size, kind), (wide, high));
                }
            }
        }
    }

    // The residual of each transform block of an inter block's plane: the count of coefficients read (VP9 8.6.2).
    fn residual_plane(
        &mut self,
        decoder: &mut BoolDecoder<'_>,
        place: Place,
        info: &Info,
        plane: usize,
        edges: Edges,
    ) -> usize {
        let tx_size = info.plane_tx_size(plane);
        let step = 1usize << tx_size;
        let (wide, high) = Self::visible_units(place, plane, edges);
        let mut total = 0;
        for row in (0..high).step_by(step) {
            for col in (0..wide).step_by(step) {
                total += self.residual(decoder, (place, info, plane), (row, col), (tx_size, DCT_DCT), (wide, high));
            }
        }
        total
    }

    // The tokens of one transform block by its neighbours' contexts, and their inverse transform (VP9 6.4.24, 8.6.2).
    fn residual(
        &mut self,
        decoder: &mut BoolDecoder<'_>,
        (place, info, plane): (Place, &Info, usize),
        (row, col): (usize, usize),
        (tx_size, kind): (u8, u8),
        (wide, high): (usize, usize),
    ) -> usize {
        let units = 1usize << tx_size;
        let (above_x, left_y) = Self::unit_place(place, plane);
        let (above_x, left_y) = (above_x + col, left_y + row);
        let above = &mut self.above_nonzero[plane];
        let left = &mut self.left_nonzero[plane];
        let has_above = above[above_x..above_x + units].iter().any(|&value| value != 0);
        let has_left = left[left_y..(left_y + units).min(LEFT_UNITS)].iter().any(|&value| value != 0);
        let context = usize::from(has_above) + usize::from(has_left);
        let plane_type = usize::from(plane > 0);
        let reference = usize::from(info.is_inter);
        let size = usize::from(tx_size);
        let mut tokens = Tokens {
            probs: &self.scope.probs.coef[size][plane_type][reference],
            counts: &mut self.counts.tokens[size][plane_type][reference],
            more: &mut self.counts.more_coefs[size][plane_type][reference],
            cache: &mut self.cache,
        };
        let dequant = self.dequant[usize::from(info.segment)][plane_type];
        let count = tokens.read(decoder, (tx_size, kind), context, dequant, &mut self.coefficients);
        let is_coded = u8::from(count > 0);
        for (index, value) in above[above_x..above_x + units].iter_mut().enumerate() {
            *value = if col + index < wide { is_coded } else { 0 };
        }
        for (index, value) in left[left_y..(left_y + units).min(LEFT_UNITS)].iter_mut().enumerate() {
            *value = if row + index < high { is_coded } else { 0 };
        }
        if count > 0 {
            let (x, y) = place.origin(plane);
            let (x, y) = (x + col * UNIT, y + row * UNIT);
            let stride = self.strides[plane];
            let side = UNIT << tx_size;
            let block = &mut self.coefficients[..side * side];
            transform::add(
                block,
                &mut self.middle,
                (tx_size, kind, self.scope.header.is_lossless),
                count,
                &mut self.planes[plane],
                y * stride + x,
                stride,
            );
            block.fill(0);
        }
        count
    }

    // Inter prediction of each plane from one reference, or two averaged (VP9 8.5.2).
    fn predict(&mut self, place: Place, info: &Info, edges: Edges) -> Result<(), Flaw> {
        let references = if info.is_compound() { 2 } else { 1 };
        for which in 0..references {
            let frame = (info.refs[which] - LAST_FRAME) as usize;
            let Some(Some((sources, scale))) = self.sources.get(frame) else {
                return Err(Flaw::Invalid("reference frame size"));
            };
            for (plane, source) in sources.iter().enumerate() {
                let (units_wide, units_high) = Self::units(place.size, plane);
                let (x, y) = place.origin(plane);
                let target = Target {
                    x,
                    y,
                    luma_x: place.col * MI_SIZE,
                    luma_y: place.row * MI_SIZE,
                    width: units_wide * UNIT,
                    height: units_high * UNIT,
                    block_width: units_wide * UNIT,
                    block_height: units_high * UNIT,
                    edges,
                    is_chroma: plane > 0,
                };
                let output = (&mut self.planes[plane][..], self.strides[plane]);
                let (scratch, is_average) = (&mut self.inter_scratch, which == 1);
                if place.size >= BLOCK_8X8 {
                    let motion = (info.mvs[3][which], *scale, info.filter);
                    inter::predict(source, output, motion, target, is_average, scratch);
                    continue;
                }
                let (output, stride) = output;
                for down in 0..units_high {
                    for across in 0..units_wide {
                        let mv = if plane == 0 {
                            info.mvs[down * UNITS_PER_MI + across][which]
                        } else {
                            average(info, which)
                        };
                        let target = Target {
                            x: x + across * UNIT,
                            y: y + down * UNIT,
                            luma_x: target.luma_x + across * UNIT,
                            luma_y: target.luma_y + down * UNIT,
                            width: UNIT,
                            height: UNIT,
                            ..target
                        };
                        let motion = (mv, *scale, info.filter);
                        inter::predict(source, (&mut *output, stride), motion, target, is_average, scratch);
                    }
                }
            }
        }
        Ok(())
    }
}

// The vector of a chroma block under 8x8: the four luma vectors' mean, rounded away from nought (VP9 8.5.2.1).
fn average(info: &Info, which: usize) -> Mv {
    let half = SUBBLOCKS / 2;
    let round = |sum: i32| ((if sum < 0 { sum - half } else { sum + half }) / SUBBLOCKS) as i16;
    let sum = |component: fn(&Mv) -> i16| info.mvs.iter().map(|mvs| i32::from(component(&mvs[which]))).sum::<i32>();
    Mv { row: round(sum(|mv| mv.row)), col: round(sum(|mv| mv.col)) }
}

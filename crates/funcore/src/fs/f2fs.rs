//! An empty f2fs as mkfs.f2fs 1.17 writes it, and a superblock's identity and encrypt feature.

use super::BuildError;
use crate::base::checksum::crc32_from;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

pub const BLOCK_SIZE: usize = 1 << LOG_BLOCK_SIZE;
/// The numbers of the blocks that hold the superblock's copies.
pub const SUPERBLOCKS: [u64; 2] = [0, 1];
pub const BLOCKS_PER_SEGMENT: u32 = 1 << LOG_BLOCKS_PER_SEGMENT;

const BLOCK: u64 = BLOCK_SIZE as u64;
const SECTOR: u64 = 1 << LOG_SECTOR_SIZE;
const LOG_SECTOR_SIZE: u32 = 9;
const LOG_BLOCK_SIZE: u32 = 12;
const LOG_BLOCKS_PER_SEGMENT: u32 = 9;
const SEGMENT_BYTES: u64 = BLOCKS_PER_SEGMENT as u64 * BLOCK;
const MAX_SEGMENTS: u64 = (1 << 32) / BLOCKS_PER_SEGMENT as u64;
const MIN_SEGMENTS: u32 = 9;
const LOGS: u32 = 6;
const HOT_DATA: usize = 0;
const WARM_DATA: usize = 1;
const COLD_DATA: usize = 2;
const HOT_NODE: usize = 3;
const WARM_NODE: usize = 4;
const COLD_NODE: usize = 5;
const LOG_SLOTS: usize = 8;
const UNUSED_LOG: u32 = u32::MAX;

const SUPER_OFFSET: usize = 1024;
const SUPER_SIZE: usize = 3072;
const MAGIC: u32 = 0xf2f5_2010;
const TOOLS_VERSION: (u16, u16) = (1, 17);
const UUID_AT: usize = 108;
pub const UUID_SIZE: usize = 16;
const LABEL_AT: usize = UUID_AT + UUID_SIZE;
const LABEL_UNITS: usize = 512;
const EXTENSION_SLOTS: usize = 64;
const EXTENSION_SIZE: usize = 8;
const VERSION_AT: usize = 1668;
const VERSION_SIZE: usize = 256;
const VERSION_LENGTH: usize = VERSION_SIZE - 4;
const HOT_EXTENSION_COUNT_AT: usize = 2757;
const FEATURES_AT: usize = 2180;
const ENCRYPT: u32 = 0x0001;
const SUPERBLOCK_CHECKSUM: u32 = 0x0800;
const ROOT_INODE: u32 = 3;
const NODE_INODE: u32 = 1;
const META_INODE: u32 = 2;
const COLD_EXTENSIONS: [&str; 36] = [
    "mp", "wm", "og", "jp", "avi", "m4v", "m4p", "mkv", "mov", "webm", "wav", "m4a", "3gp", "opus", "flac", "gif",
    "png", "svg", "webp", "jar", "deb", "iso", "gz", "xz", "zst", "pdf", "pyc", "ttc", "ttf", "exe", "apk", "cnt",
    "exo", "odex", "vdex", "so",
];
const HOT_EXTENSIONS: [&str; 4] = ["db", "vmdk", "vdi", "qcow2"];

const SIT_ENTRY_SIZE: usize = 74;
const SIT_ENTRIES_PER_BLOCK: u32 = (BLOCK_SIZE / SIT_ENTRY_SIZE) as u32;
const NAT_ENTRY_SIZE: usize = 9;
const NAT_ENTRIES_PER_BLOCK: u32 = (BLOCK_SIZE / NAT_ENTRY_SIZE) as u32;
const BITMAP_BYTES_PER_SEGMENT: u32 = BLOCKS_PER_SEGMENT / 8;

const CHECKPOINT_PACKS: u32 = 2;
const CHECKPOINT_HEADER: usize = 192;
const CHECKSUM_AT: usize = BLOCK_SIZE - 4;
const MIN_NAT_BITMAP: u32 = 64;
const MAX_BITMAP_IN_CHECKPOINT: u32 = (CHECKSUM_AT - CHECKPOINT_HEADER) as u32;
const MAX_SIT_BITMAP_IN_CHECKPOINT: u32 = MAX_BITMAP_IN_CHECKPOINT - MIN_NAT_BITMAP;
const MAX_SIT_BITMAP: u32 = (MAX_SEGMENTS.div_ceil(SIT_ENTRIES_PER_BLOCK as u64) as u32).div_ceil(BLOCKS_PER_SEGMENT)
    * BITMAP_BYTES_PER_SEGMENT;
const PACK_BLOCKS: u32 = 6;
const NAT_BITS_HEADER: usize = 8;
const COARSE_UNDER: u32 = 256;
const COARSE_SEARCH: (f64, f64, f64) = (10.0, 95.0, 5.0);
const FINE_SEARCH: (f64, f64, f64) = (0.01, 10.0, 0.01);
const FLAG_UNMOUNTED: u32 = 0x001;
const FLAG_COMPACT_SUMMARY: u32 = 0x004;
const FLAG_NAT_BITS: u32 = 0x080;
const FLAG_TRIMMED: u32 = 0x100;
const FIRST_FREE_NODE: u32 = 4;

const SUMMARY_SIZE: usize = 7;
const SUMMARY_FOOTER_AT: usize = BLOCK_SIZE - 5;
const SUMMARY_JOURNAL_AT: usize = SUMMARY_SIZE * BLOCKS_PER_SEGMENT as usize;
const JOURNAL_SIZE: usize = SUMMARY_FOOTER_AT - SUMMARY_JOURNAL_AT;
const NODE_SUMMARY: u8 = 1;
const SIT_JOURNAL_ENTRY_SIZE: usize = 4 + SIT_ENTRY_SIZE;
const SIT_TYPE_SHIFT: u32 = 10;

const INODE_ADDRESSES_AT: usize = 360;
const NODE_FOOTER_AT: usize = BLOCK_SIZE - 24;
const ROOT_MODE: u16 = 0o040755;
const DIRECTORY: u8 = 2;
const DENTRIES_PER_BLOCK: usize = 214;
const DENTRY_SIZE: usize = 11;
const NAME_SLOT_SIZE: usize = 8;
const DENTRIES_AT: usize = BLOCK_SIZE - DENTRIES_PER_BLOCK * (DENTRY_SIZE + NAME_SLOT_SIZE);
const NAMES_AT: usize = DENTRIES_AT + DENTRIES_PER_BLOCK * DENTRY_SIZE;

const TOO_SMALL: BuildError = BuildError("partition too small for f2fs");
const TOO_LARGE: BuildError = BuildError("partition larger than f2fs can address");

/// The parameters of a format.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    /// Size of the partition in bytes.
    pub bytes: u64,
    /// The partition's first sector on its disk, in 512 byte sectors.
    pub start_sector: u64,
    pub label: &'a str,
    pub uuid: [u8; UUID_SIZE],
    /// Version of the first checkpoint.
    pub checkpoint: u64,
    /// The kernel's version line, written to the superblock.
    pub kernel: &'a str,
    /// The root directory's timestamps, in seconds since 1970.
    pub time: u64,
    /// True if the partition was discarded, so the filesystem's free space needs no discard.
    pub is_trimmed: bool,
}

/// An empty filesystem as ordered writes to its partition.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Image {
    /// Runs of blocks to fill with zeros.
    pub zero_runs: Vec<Range<u64>>,
    /// Blocks by their number.
    pub blocks: Vec<(u64, Vec<u8>)>,
    /// The block for each of `SUPERBLOCKS`, written last so that a format cut short leaves no filesystem.
    pub superblock: Vec<u8>,
}

/// Splits runs of blocks into writes of at most a segment, each its first block and count.
pub fn zero_writes(runs: &[Range<u64>]) -> impl Iterator<Item = (u64, u64)> + '_ {
    let segment = u64::from(BLOCKS_PER_SEGMENT);
    runs.iter().flat_map(move |run| {
        run.clone().step_by(BLOCKS_PER_SEGMENT as usize).map(move |start| (start, (run.end - start).min(segment)))
    })
}

/// The label and uuid of a filesystem.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Identity {
    pub(crate) label: String,
    pub uuid: [u8; UUID_SIZE],
}

#[derive(Clone, PartialEq, Debug)]
struct Layout {
    block_count: u64,
    segment0: u32,
    segments: u32,
    sit_segments: u32,
    nat_segments: u32,
    ssa_segments: u32,
    main_segments: u32,
    sit: u32,
    nat: u32,
    ssa: u32,
    main: u32,
    payload: u32,
    overprovision: f64,
    reserved_segments: u32,
    logs: [u32; LOGS as usize],
}

struct Fields<'a> {
    block: &'a mut [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn new(block: &'a mut [u8], at: usize) -> Self {
        Fields { block, at }
    }

    fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.block[self.at..self.at + bytes.len()].copy_from_slice(bytes);
        self.at += bytes.len();
        self
    }

    fn u8(&mut self, value: u8) -> &mut Self {
        self.bytes(&[value])
    }

    fn u16(&mut self, value: u16) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    fn u64(&mut self, value: u64) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    fn fixed(&mut self, bytes: &[u8], width: usize) -> &mut Self {
        let length = bytes.len().min(width);
        self.block[self.at..self.at + length].copy_from_slice(&bytes[..length]);
        self.at += width;
        self
    }

    fn skip(&mut self, count: usize) -> &mut Self {
        self.at += count;
        self
    }
}

fn reserved_segments(percent: f64) -> u32 {
    (100.0 / percent + 1.0 + LOGS as f64) as u32
}

fn best_overprovision(main_segments: u32) -> f64 {
    let (mut candidate, end, step) = if main_segments < COARSE_UNDER { COARSE_SEARCH } else { FINE_SEARCH };
    let (mut best, mut largest) = (0.0, 0.0);
    while candidate <= end {
        let reserved = reserved_segments(candidate);
        let overprovision = main_segments.wrapping_sub(reserved) as f64 * candidate / 100.0;
        if overprovision > 0.0 {
            let unusable = if reserved as f64 > overprovision { reserved as f64 } else { overprovision };
            let space = main_segments as f64 - unusable - LOGS as f64;
            if largest < space {
                (largest, best) = (space, candidate);
            }
        }
        candidate += step;
    }
    best
}

fn lay_out(bytes: u64, start_sector: u64) -> Result<Layout, BuildError> {
    let bytes = bytes / SECTOR * SECTOR;
    if bytes > MAX_SEGMENTS * SEGMENT_BYTES {
        return Err(TOO_LARGE);
    }
    let start = start_sector * SECTOR;
    let offset = (start + SUPERBLOCKS.len() as u64 * BLOCK).next_multiple_of(SEGMENT_BYTES) - start;
    if bytes < offset {
        return Err(TOO_SMALL);
    }
    let segment0 = (offset / BLOCK) as u32;
    let segments = ((bytes - offset) / SEGMENT_BYTES) as u32;

    let sit_segments = segments.div_ceil(SIT_ENTRIES_PER_BLOCK).div_ceil(BLOCKS_PER_SEGMENT);
    let sit = segment0 + CHECKPOINT_PACKS * BLOCKS_PER_SEGMENT;
    let nat = sit + 2 * sit_segments * BLOCKS_PER_SEGMENT;
    let segments_to_nat = CHECKPOINT_PACKS + 2 * sit_segments;
    if segments <= segments_to_nat {
        return Err(TOO_SMALL);
    }
    let nodes = (segments - segments_to_nat) as u64 * BLOCKS_PER_SEGMENT as u64;
    let nat_demand = (nodes.div_ceil(NAT_ENTRIES_PER_BLOCK as u64) as u32).div_ceil(BLOCKS_PER_SEGMENT);

    let sit_bitmap = (sit_segments * BITMAP_BYTES_PER_SEGMENT).min(MAX_SIT_BITMAP);
    let (nat_bitmap, payload) = if sit_bitmap > MAX_SIT_BITMAP_IN_CHECKPOINT {
        (MAX_BITMAP_IN_CHECKPOINT, sit_bitmap.div_ceil(BLOCK_SIZE as u32))
    } else {
        (MAX_BITMAP_IN_CHECKPOINT - sit_bitmap, 0)
    };
    let nat_segments = nat_demand.min(nat_bitmap / BITMAP_BYTES_PER_SEGMENT);
    let ssa = nat + 2 * nat_segments * BLOCKS_PER_SEGMENT;
    let segments_to_ssa = segments_to_nat + 2 * nat_segments;
    if segments <= segments_to_ssa {
        return Err(TOO_SMALL);
    }
    let ssa_segments = (segments - segments_to_ssa).div_ceil(BLOCKS_PER_SEGMENT);
    let meta_segments = segments_to_ssa + ssa_segments;
    if segments <= meta_segments {
        return Err(TOO_SMALL);
    }
    let main = segment0 + meta_segments * BLOCKS_PER_SEGMENT;
    let main_segments = segments - meta_segments;

    let overprovision = best_overprovision(main_segments);
    let reserved = reserved_segments(overprovision);
    if overprovision == 0.0 || segments < MIN_SEGMENTS || main_segments <= LOGS || main_segments - LOGS < reserved {
        return Err(TOO_SMALL);
    }

    let mut logs = [0; LOGS as usize];
    logs[HOT_NODE] = 0;
    logs[WARM_NODE] = logs[HOT_NODE] + 1;
    logs[COLD_NODE] = logs[WARM_NODE] + 1;
    logs[HOT_DATA] = logs[COLD_NODE] + 1;
    logs[COLD_DATA] = (main_segments / 4 - 1).max(logs[HOT_DATA] + 1);
    logs[WARM_DATA] = (main_segments / 2 - 1).max(logs[COLD_DATA] + 1);

    let layout = Layout {
        block_count: bytes / BLOCK,
        segment0,
        segments,
        sit_segments,
        nat_segments,
        ssa_segments,
        main_segments,
        sit,
        nat,
        ssa,
        main,
        payload,
        overprovision,
        reserved_segments: reserved,
        logs,
    };
    if main_segments <= layout.overprovision_segments() {
        return Err(TOO_SMALL);
    }
    Ok(layout)
}

impl Layout {
    fn log_start(&self, log: usize) -> u32 {
        self.main + self.logs[log] * BLOCKS_PER_SEGMENT
    }

    fn overprovision_segments(&self) -> u32 {
        let share = ((self.main_segments - self.reserved_segments) as f64 * self.overprovision / 100.0) as u32;
        share.max(self.reserved_segments) + LOGS
    }

    fn pack_blocks(&self) -> u32 {
        PACK_BLOCKS + self.payload
    }

    fn nat_bits_bytes(&self) -> u32 {
        self.nat_segments * BITMAP_BYTES_PER_SEGMENT
    }

    fn nat_bits_blocks(&self) -> u32 {
        (2 * self.nat_bits_bytes() + NAT_BITS_HEADER as u32).div_ceil(BLOCK_SIZE as u32)
    }

    fn has_nat_bits(&self) -> bool {
        self.pack_blocks() <= BLOCKS_PER_SEGMENT - self.nat_bits_blocks()
    }
}

fn superblock(layout: &Layout, request: &Request<'_>) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK_SIZE];
    let label: Vec<u8> = request.label.encode_utf16().take(LABEL_UNITS - 1).flat_map(u16::to_le_bytes).collect();
    let version = request.kernel.lines().next().unwrap_or_default().as_bytes();
    let version = &version[..version.len().min(VERSION_LENGTH)];

    let mut fields = Fields::new(&mut block, SUPER_OFFSET);
    fields.u32(MAGIC).u16(TOOLS_VERSION.0).u16(TOOLS_VERSION.1);
    fields.u32(LOG_SECTOR_SIZE).u32(LOG_BLOCK_SIZE - LOG_SECTOR_SIZE).u32(LOG_BLOCK_SIZE).u32(LOG_BLOCKS_PER_SEGMENT);
    // A segment a section, a section a zone, and no checksum of the superblock.
    fields.u32(1).u32(1).u32(0);
    fields.u64(layout.block_count).u32(layout.main_segments).u32(layout.segments);
    fields.u32(CHECKPOINT_PACKS).u32(2 * layout.sit_segments).u32(2 * layout.nat_segments);
    fields.u32(layout.ssa_segments).u32(layout.main_segments);
    fields.u32(layout.segment0).u32(layout.segment0).u32(layout.sit).u32(layout.nat).u32(layout.ssa).u32(layout.main);
    fields.u32(ROOT_INODE).u32(NODE_INODE).u32(META_INODE);
    debug_assert_eq!(fields.at, SUPER_OFFSET + UUID_AT);
    fields.bytes(&request.uuid).fixed(&label, 2 * LABEL_UNITS);
    fields.u32(COLD_EXTENSIONS.len() as u32);
    for extension in COLD_EXTENSIONS.iter().chain(&HOT_EXTENSIONS) {
        fields.fixed(extension.as_bytes(), EXTENSION_SIZE);
    }
    fields.skip((EXTENSION_SLOTS - COLD_EXTENSIONS.len() - HOT_EXTENSIONS.len()) * EXTENSION_SIZE);
    fields.u32(layout.payload);
    debug_assert_eq!(fields.at, SUPER_OFFSET + VERSION_AT);
    fields.fixed(version, VERSION_SIZE).fixed(version, VERSION_SIZE);
    block[SUPER_OFFSET + HOT_EXTENSION_COUNT_AT] = HOT_EXTENSIONS.len() as u8;
    block
}

fn root_entries() -> Vec<u8> {
    let mut block = vec![0u8; BLOCK_SIZE];
    for (slot, name) in [".", ".."].into_iter().enumerate() {
        block[slot / 8] |= 1 << (slot % 8);
        Fields::new(&mut block, DENTRIES_AT + slot * DENTRY_SIZE)
            .u32(0)
            .u32(ROOT_INODE)
            .u16(name.len() as u16)
            .u8(DIRECTORY);
        Fields::new(&mut block, NAMES_AT + slot * NAME_SLOT_SIZE).bytes(name.as_bytes());
    }
    block
}

fn root_inode(layout: &Layout, time: u64) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK_SIZE];
    // mkfs.f2fs keeps the low 32 bits of the time.
    let time = time as u32 as u64;
    let mut fields = Fields::new(&mut block, 0);
    // Its mode, no advice and nothing inline, the root user's and group's, two links.
    fields.u16(ROOT_MODE).u8(0).u8(0).u32(0).u32(0).u32(2);
    // A block long, two blocks held (the inode and its entries), read, changed and modified at one time.
    fields.u64(BLOCK).u64(2).u64(time).u64(time).u64(time);
    // No nanoseconds to those times, generation 0, a depth of one level, then where its entries lie.
    fields.u32(0).u32(0).u32(0).u32(0).u32(1);
    Fields::new(&mut block, INODE_ADDRESSES_AT).u32(layout.log_start(HOT_DATA));
    // The footer: its node and its inode, no flags, the checkpoint version it is of, and the next block of its log.
    Fields::new(&mut block, NODE_FOOTER_AT)
        .u32(ROOT_INODE)
        .u32(ROOT_INODE)
        .u32(0)
        .u64(1)
        .u32(layout.log_start(HOT_NODE) + 1);
    block
}

fn first_nat_block() -> Vec<u8> {
    let mut block = vec![0u8; BLOCK_SIZE];
    for inode in [NODE_INODE, META_INODE] {
        // A `struct f2fs_nat_entry`: version 0, the inode, the placeholder address 1.
        Fields::new(&mut block, inode as usize * NAT_ENTRY_SIZE).u8(0).u32(inode).u32(1);
    }
    block
}

fn checkpoint(layout: &Layout, version: u64, is_trimmed: bool) -> (Vec<u8>, u32) {
    let mut block = vec![0u8; BLOCK_SIZE];
    let overprovision = layout.overprovision_segments();
    let mut flags = FLAG_UNMOUNTED | FLAG_COMPACT_SUMMARY;
    if layout.has_nat_bits() {
        flags |= FLAG_NAT_BITS;
    }
    if is_trimmed {
        flags |= FLAG_TRIMMED;
    }

    let mut fields = Fields::new(&mut block, 0);
    fields.u64(version);
    // The blocks the user may fill and the two in use; the segments reserved, overprovisioned and free.
    fields.u64((layout.main_segments - overprovision) as u64 * BLOCKS_PER_SEGMENT as u64).u64(2);
    fields.u32(layout.reserved_segments).u32(overprovision).u32(layout.main_segments - LOGS);
    // The segment each node log and each data log writes to, and the block it writes next: the first of each hot log is taken.
    for logs in [[HOT_NODE, WARM_NODE, COLD_NODE], [HOT_DATA, WARM_DATA, COLD_DATA]] {
        for slot in 0..LOG_SLOTS {
            fields.u32(logs.get(slot).map_or(UNUSED_LOG, |&log| layout.logs[log]));
        }
        fields.u16(1).skip((LOG_SLOTS - 1) * 2);
    }
    // Its flags, the blocks of its pack, the block the summaries begin at, one node and one inode in use, the first free node; the bitmaps' sizes and where the checksum lies.
    fields.u32(flags).u32(layout.pack_blocks()).u32(1 + layout.payload).u32(1).u32(1).u32(FIRST_FREE_NODE);
    fields.u32(layout.sit_segments * BITMAP_BYTES_PER_SEGMENT).u32(layout.nat_segments * BITMAP_BYTES_PER_SEGMENT);
    fields.u32(CHECKSUM_AT as u32);
    debug_assert!(fields.at <= CHECKPOINT_HEADER);
    let checksum = crc32_from(MAGIC, &block[..CHECKSUM_AT]);
    Fields::new(&mut block, CHECKSUM_AT).u32(checksum);
    (block, checksum)
}

fn compact_summary(layout: &Layout) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK_SIZE];
    // The NAT journal: one entry, the root's node, of version 0, at the first block of the hot node log.
    Fields::new(&mut block, 0).u16(1).u32(ROOT_INODE).u8(0).u32(ROOT_INODE).u32(layout.log_start(HOT_NODE));
    // The SIT journal: every log's segment, its kind over the count of its blocks in use, and the bit of its first block.
    Fields::new(&mut block, JOURNAL_SIZE).u16(LOGS as u16);
    for (log, &segment) in layout.logs.iter().enumerate() {
        let is_used = matches!(log, HOT_DATA | HOT_NODE);
        let mut entry = Fields::new(&mut block, JOURNAL_SIZE + 2 + log * SIT_JOURNAL_ENTRY_SIZE);
        entry.u32(segment).u16((log as u16) << SIT_TYPE_SHIFT | is_used as u16).u8(if is_used { 0x80 } else { 0 });
    }
    // The hot data log's summary: its first block belongs to the root.
    Fields::new(&mut block, 2 * JOURNAL_SIZE).u32(ROOT_INODE);
    block
}

fn node_summary(owner: Option<u32>) -> Vec<u8> {
    let mut block = vec![0u8; BLOCK_SIZE];
    if let Some(node) = owner {
        Fields::new(&mut block, 0).u32(node);
    }
    block[SUMMARY_FOOTER_AT] = NODE_SUMMARY;
    block
}

fn nat_bits(layout: &Layout, version: u64, checksum: u32) -> Vec<u8> {
    let bytes = layout.nat_bits_bytes() as usize;
    let mut bits = vec![0u8; layout.nat_bits_blocks() as usize * BLOCK_SIZE];
    Fields::new(&mut bits, 0).u64(version | (checksum as u64) << 32);
    bits[NAT_BITS_HEADER + bytes..NAT_BITS_HEADER + 2 * bytes].fill(0xff);
    bits[NAT_BITS_HEADER + bytes] &= !1;
    bits
}

/// The writes of an empty filesystem on the partition `request` describes.
pub fn format(request: &Request<'_>) -> Result<Image, BuildError> {
    let layout = lay_out(request.bytes, request.start_sector)?;
    let segment = BLOCKS_PER_SEGMENT as u64;
    let (pack1, pack2) = (layout.segment0 as u64, layout.segment0 as u64 + segment);
    let (payload, last) = (layout.payload as u64, layout.pack_blocks() as u64 - 1);

    let (sit, warm) = (layout.sit as u64, layout.log_start(WARM_NODE) as u64);
    let nat_copies = (0..layout.nat_segments as u64)
        .map(|pair| layout.nat as u64 + 2 * pair * segment)
        .map(|start| start..start + segment);
    let mut zero_runs =
        Vec::from_iter(core::iter::once(sit..sit + layout.sit_segments as u64 * segment).chain(nat_copies));
    zero_runs.push(warm..warm + 1);
    if payload > 0 {
        zero_runs.extend([pack1 + 1..pack1 + 1 + payload, pack2 + 1..pack2 + 1 + payload]);
    }

    let (first, checksum) = checkpoint(&layout, request.checkpoint, request.is_trimmed);
    let (second, _) = checkpoint(&layout, 0, request.is_trimmed);
    let mut blocks = vec![
        (layout.log_start(HOT_DATA) as u64, root_entries()),
        (layout.log_start(HOT_NODE) as u64, root_inode(&layout, request.time)),
        (layout.nat as u64, first_nat_block()),
        (pack1, first.clone()),
        (pack1 + payload + 1, compact_summary(&layout)),
        (pack1 + payload + 2, node_summary(Some(ROOT_INODE))),
        (pack1 + payload + 3, node_summary(None)),
        (pack1 + payload + 4, node_summary(None)),
        (pack1 + last, first),
    ];
    if layout.has_nat_bits() {
        let at = pack1 + segment - layout.nat_bits_blocks() as u64;
        let bits = nat_bits(&layout, request.checkpoint, checksum);
        blocks.extend(bits.chunks(BLOCK_SIZE).enumerate().map(|(index, block)| (at + index as u64, block.to_vec())));
    }
    blocks.extend([(pack2, second.clone()), (pack2 + last, second)]);

    Ok(Image { zero_runs, blocks, superblock: superblock(&layout, request) })
}

pub(crate) fn identify(block: &[u8]) -> Option<Identity> {
    let superblock = block.get(SUPER_OFFSET..SUPER_OFFSET + SUPER_SIZE)?;
    if superblock[..4] != MAGIC.to_le_bytes() {
        return None;
    }
    let units =
        superblock[LABEL_AT..LABEL_AT + 2 * LABEL_UNITS].as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes);
    let label = char::decode_utf16(units.take_while(|&unit| unit != 0))
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    let uuid = superblock[UUID_AT..UUID_AT + UUID_SIZE].try_into().ok()?;
    Some(Identity { label, uuid })
}

/// Sets the encrypt feature as `mkfs.f2fs -O encrypt` does: true if it was unset; none for no superblock, or one with a checksum.
pub fn allow_encryption(block: &mut [u8]) -> Option<bool> {
    identify(block)?;
    let at = SUPER_OFFSET + FEATURES_AT;
    let features = u32::from_le_bytes(block.get(at..at + 4)?.try_into().ok()?);
    if features & SUPERBLOCK_CHECKSUM != 0 {
        return None;
    }
    block[at..at + 4].copy_from_slice(&(features | ENCRYPT).to_le_bytes());
    Some(features & ENCRYPT == 0)
}

/// The first checkpoint's version from a random number: odd and under 2^31, as mkfs.f2fs makes it.
pub fn first_checkpoint(random: u32) -> u64 {
    (random & 0x7fff_ffff | 1) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::checksum::fnv;

    const R1_BYTES: u64 = 115_361_153_024;
    const R1_START_SECTOR: u64 = 18_841_600;
    const MKFS_KERNEL: &str = "Linux version 7.2.9-1-cachyos (linux-cachyos@cachyos) (clang version 23.1.1, LLD 23.1.1) #1 SMP PREEMPT_DYNAMIC Sat, 03 Oct 2026 18:34:55 +0000\n";
    const MKFS_BLOCKS: [(u64, u64); 15] = [
        (0, 0x9fc8336fa49e6813),
        (1, 0x9fc8336fa49e6813),
        (512, 0xe81a34d6ffec9d81),
        (513, 0xe24975b3bfee47a7),
        (514, 0x280c0d32ee9b0d89),
        (515, 0xac8b295950ebf8c2),
        (516, 0xac8b295950ebf8c2),
        (517, 0xe81a34d6ffec9d81),
        (1022, 0x99d6fdb9abc01818),
        (1023, 0x059ef8431e11e79d),
        (1024, 0x6c1e80d791531c78),
        (1029, 0x6c1e80d791531c78),
        (3584, 0x0c03a755ed19b0fc),
        (118272, 0xd9925f9d9b4b6033),
        (119808, 0x2638e748d7b81a25),
    ];
    const MKFS_CHECKPOINT: u64 = 0x60f8_493d;
    const MKFS_UUID: [u8; 16] =
        [0x6f, 0x1d, 0x8a, 0x52, 0x3c, 0x4b, 0x4e, 0x7a, 0x9b, 0x21, 0x0f, 0x5e, 0x6d, 0x7c, 0x8a, 0x90];
    const MIB: u64 = 1 << 20;

    fn request() -> Request<'static> {
        Request {
            bytes: R1_BYTES,
            start_sector: R1_START_SECTOR,
            label: "r1metal",
            uuid: MKFS_UUID,
            checkpoint: MKFS_CHECKPOINT,
            kernel: MKFS_KERNEL,
            time: 1_791_547_200,
            is_trimmed: true,
        }
    }

    #[test]
    fn r1_layout() {
        let layout = lay_out(R1_BYTES, R1_START_SECTOR).unwrap();
        assert_eq!((layout.block_count, layout.segment0, layout.segments), (28_164_344, 512, 55_007));
        assert_eq!(
            (layout.sit_segments, layout.nat_segments, layout.ssa_segments, layout.main_segments),
            (2, 58, 108, 54_777)
        );
        assert_eq!((layout.sit, layout.nat, layout.ssa, layout.main), (1536, 3584, 62_976, 118_272));
        assert_eq!((layout.payload, layout.reserved_segments, layout.overprovision_segments()), (0, 239, 245));
        assert_eq!(layout.logs, [3, 27_387, 13_693, 0, 1, 2]);
        assert!((layout.overprovision - 0.43).abs() < 1e-9);
    }

    #[test]
    fn mkfs_blocks() {
        let image = format(&request()).unwrap();
        let superblocks = SUPERBLOCKS.map(|number| (number, image.superblock.clone()));
        let mut actual: Vec<(u64, u64)> =
            superblocks.iter().chain(&image.blocks).map(|(number, block)| (*number, fnv(block))).collect();
        actual.sort();
        assert_eq!(actual, MKFS_BLOCKS);
        assert!(image.blocks.iter().all(|(_, block)| block.len() == BLOCK_SIZE));
    }

    #[test]
    fn mkfs_zero_runs() {
        let image = format(&request()).unwrap();
        let nat_copies = (0..58).map(|pair| 3584 + pair * 1024).map(|start| start..start + 512);
        let warm_node = core::iter::once(118_784..118_785);
        let expected: Vec<Range<u64>> = core::iter::once(1536..2560).chain(nat_copies).chain(warm_node).collect();
        assert_eq!(image.zero_runs, expected);
        let segment = u64::from(BLOCKS_PER_SEGMENT);
        assert_eq!(
            zero_writes(&[1..3, segment - 1..2 * segment + 5]).collect::<Vec<_>>(),
            [(1, 2), (segment - 1, segment), (2 * segment - 1, 6)],
            "a segment at a write, the last of a run as long as what is left of it"
        );
        assert_eq!(zero_writes(&[]).count(), 0);
    }

    #[test]
    fn segment0_alignment() {
        let aligned = lay_out(R1_BYTES, 0).unwrap();
        assert_eq!(aligned, lay_out(R1_BYTES, R1_START_SECTOR).unwrap());
        let shifted = lay_out(R1_BYTES, MIB / SECTOR).unwrap();
        assert_eq!(shifted.segment0, 256);
        assert_eq!((shifted.sit, shifted.main), (aligned.sit - 256, aligned.main - 256));
    }

    #[test]
    fn ssa_growth() {
        let sized =
            |mib| lay_out(mib * MIB, 0).map(|layout| (layout.segments, layout.ssa_segments, layout.main_segments));
        assert_eq!(
            [1040, 1042, 1044].map(sized),
            [Ok((519, 1, 510)), Ok((520, 1, 511)), Ok((521, 2, 511))],
            "as mkfs.f2fs 1.17.0 laid out files of these sizes: 512 segments to summarise still fit one segment"
        );
    }

    #[test]
    fn refused_sizes() {
        let sized = |bytes| format(&Request { bytes, ..request() }).map(|image| image.blocks.len());
        assert_eq!(sized(20 * MIB), Err(TOO_SMALL));
        assert_eq!(sized(40 * MIB), Err(TOO_SMALL));
        assert_eq!(sized(50 * MIB), Ok(12));
        assert_eq!(sized((MAX_SEGMENTS + 1) * SEGMENT_BYTES), Err(TOO_LARGE));
        assert_eq!(sized(0), Err(TOO_SMALL));
    }

    #[test]
    fn long_kernel_line() {
        let line = "k".repeat(300);
        let image = format(&Request { kernel: &line, ..request() }).unwrap();
        let version = &image.superblock[SUPER_OFFSET + VERSION_AT..][..VERSION_SIZE];
        assert!(version[..VERSION_LENGTH].iter().all(|&byte| byte == b'k'));
        assert_eq!(version[VERSION_LENGTH..], [0; VERSION_SIZE - VERSION_LENGTH]);
    }

    #[test]
    fn identify_superblock() {
        let image = format(&request()).unwrap();
        assert_eq!(identify(&image.superblock), Some(Identity { label: "r1metal".into(), uuid: MKFS_UUID }));
        let renamed = format(&Request { label: "größer", ..request() }).unwrap();
        assert_eq!(identify(&renamed.superblock).map(|identity| identity.label).as_deref(), Some("größer"));
        assert_eq!(identify(&[0u8; BLOCK_SIZE]), None);
        let mut ext4 = [0u8; BLOCK_SIZE];
        ext4[SUPER_OFFSET + 0x38..SUPER_OFFSET + 0x3a].copy_from_slice(&0xef53u16.to_le_bytes());
        assert_eq!(identify(&ext4), None);
        assert_eq!(identify(&image.superblock[..BLOCK_SIZE - 1]), None);
    }

    #[test]
    fn encrypt_feature() {
        let image = format(&request()).unwrap();
        let mut block = image.superblock.clone();
        assert_eq!(allow_encryption(&mut block), Some(true), "mkfs gives it none");
        let changes: Vec<usize> = (0..BLOCK_SIZE).filter(|&at| block[at] != image.superblock[at]).collect();
        assert_eq!(changes, [SUPER_OFFSET + FEATURES_AT], "the lowest bit of the features alone");
        assert_eq!(block[SUPER_OFFSET + FEATURES_AT], 1);
        assert_eq!(identify(&block), identify(&image.superblock));
        assert_eq!(allow_encryption(&mut block), Some(false), "it has it now");
        let mut with_checksum = image.superblock.clone();
        with_checksum[SUPER_OFFSET + FEATURES_AT + 1] = 0x08;
        assert_eq!(allow_encryption(&mut with_checksum), None, "a superblock with a checksum is left alone");
        assert_eq!(allow_encryption(&mut [0u8; BLOCK_SIZE]), None);
    }

    #[test]
    fn first_checkpoint_odd() {
        assert_eq!(first_checkpoint(0), 1);
        assert_eq!(first_checkpoint(u32::MAX), 0x7fff_ffff);
        assert_eq!(first_checkpoint(0x60f8_493c), MKFS_CHECKPOINT);
    }
}

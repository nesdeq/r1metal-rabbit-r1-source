//! A read only exFAT volume built in memory from a directory tree; file contents are fetched as they are read.

use super::BuildError;
use crate::base::time::Moment;
use crate::base::time::offset::to_local;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

const SECTOR_SIZE: u64 = 512;
const SECTOR_SHIFT: u8 = 9;
const MAX_CLUSTER_BYTES: u64 = 32 << 20;
const MIN_VOLUME_BYTES: u64 = 1 << 20;
const MAX_CLUSTERS: u64 = (1 << 32) - 11;
const FIRST_CLUSTER: u64 = 2;
const BOOT_SECTORS: u64 = 12;
const BOOT_COPIES: u64 = 2;
const EXTENDED_BOOT_SECTORS: core::ops::Range<usize> = 1..9;
const CHECKSUM_SECTOR: usize = 11;
const UNCHECKED_BOOT_BYTES: [usize; 3] = [106, 107, 112];
const BOOT_SIGNATURE: [u8; 2] = [0x55, 0xaa];
const EXTENDED_BOOT_SIGNATURE: [u8; 4] = [0x00, 0x00, 0x55, 0xaa];
const HALT: u8 = 0xf4;
const BOOT_CODE: core::ops::Range<usize> = 120..510;
const REVISION: u16 = 0x0100;
const FIRST_HARD_DISK: u8 = 0x80;
const FATS: u8 = 1;
const FAT_ENTRY_BYTES: u64 = 4;
const FAT_MEDIA: u32 = 0xffff_fff8;
const FAT_END: u32 = 0xffff_ffff;

const PARTITION_TABLE_AT: usize = 446;
const DISK_SIGNATURE_AT: usize = 440;
const PARTITION_TYPE: u8 = 0x07;
const MAX_CHS: [u8; 3] = [0xfe, 0xff, 0xff];

const ENTRY_SIZE: usize = 32;
const ROOT: usize = 0;
const LABEL_ENTRY: u8 = 0x83;
const BITMAP_ENTRY: u8 = 0x81;
const UPCASE_ENTRY: u8 = 0x82;
const FILE_ENTRY: u8 = 0x85;
const STREAM_ENTRY: u8 = 0xc0;
const NAME_ENTRY: u8 = 0xc1;
const ROOT_SYSTEM_ENTRIES: usize = 3;
const MAX_LABEL_UNITS: usize = 11;
const MAX_NAME_UNITS: usize = 255;
const NAME_UNITS_PER_ENTRY: usize = 15;
const MAX_DIRECTORY_BYTES: u64 = 256 << 20;
const DIRECTORY: u16 = 0x10;
const ARCHIVE: u16 = 0x20;
const ALLOCATION_POSSIBLE: u8 = 0x01;
const NO_FAT_CHAIN: u8 = 0x02;
const ILLEGAL_CHARS: [char; 9] = ['"', '*', '/', ':', '<', '>', '?', '\\', '|'];
const FIRST_YEAR: u32 = 1980;
const LAST_YEAR: u32 = 2107;
const QUARTER_HOUR: i16 = 15;
const OFFSET_VALID: u8 = 0x80;
const OFFSET_BITS: u8 = 0x7f;
const QUARTER_HOURS: core::ops::RangeInclusive<i16> = -64..=63;

static UPCASE: &[u8] = include_bytes!("../../assets/exfat/upcase.bin");
const UPCASE_RUN: u16 = 0xffff;

const BAD_CLUSTER_SIZE: BuildError = BuildError("cluster size is no power of two that exFAT allows");
const TOO_LARGE: BuildError = BuildError("more than an exFAT volume behind a partition table holds");

/// An entry of a volume's tree.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Node {
    pub name: String,
    /// In seconds of UTC since 1970.
    pub modified: u64,
    pub content: Content,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Content {
    Directory(Vec<Node>),
    // `id` is the owner's key for the file, handed to `Files::fetch`.
    File { id: usize, size: u64 },
    // Neither a file nor a directory, or an entry that could not be read: skipped and counted.
    Other,
}

/// The parameters of a volume.
#[derive(Clone, Copy, Debug)]
pub struct Spec<'a> {
    pub label: &'a str,
    pub serial: u32,
    /// In bytes.
    pub cluster_size: u32,
    /// The size the volume shows, in bytes; it grows if the tree needs more.
    pub capacity: u64,
    /// Minutes local time is ahead of UTC; with none, times are written as UTC.
    pub offset: Option<i16>,
}

/// Reads the files of a tree by their ids.
pub trait Files {
    fn fetch(&self, id: usize, offset: u64, buffer: &mut [u8]);
}

enum Source {
    Bitmap,
    Bytes(Vec<u8>),
    File { id: usize, size: u64 },
}

struct Extent {
    first: u64,
    clusters: u64,
    source: Source,
}

/// A medium with a partition table and one exFAT volume, readable at any byte.
pub struct Volume {
    size: u64,
    cluster: u64,
    table: Vec<u8>,
    volume_at: u64,
    fat_at: u64,
    heap_at: u64,
    boot: Vec<u8>,
    fat: Vec<u8>,
    used_clusters: u64,
    extents: Vec<Extent>,
    skip_count: usize,
}

#[derive(Clone, Copy)]
struct Stamp {
    timestamp: u32,
    hundredths: u8,
    zone: u8,
}

impl Stamp {
    fn new(seconds: u64, offset: Option<i16>) -> Stamp {
        let moment = Moment::from_seconds(to_local(seconds, offset.unwrap_or(0)));
        let quarters = offset
            .filter(|offset| offset % QUARTER_HOUR == 0)
            .map(|offset| offset / QUARTER_HOUR)
            .filter(|quarters| QUARTER_HOURS.contains(quarters));
        let zone = quarters.map_or(0, |quarters| OFFSET_VALID | (quarters as u8 & OFFSET_BITS));
        let moment = match moment.year {
            year if year < FIRST_YEAR => {
                Moment { year: FIRST_YEAR, month: 1, day: 1, hour: 0, minute: 0, second: 0, ..moment }
            }
            year if year > LAST_YEAR => {
                Moment { year: LAST_YEAR, month: 12, day: 31, hour: 23, minute: 59, second: 59, ..moment }
            }
            _ => moment,
        };
        // A DOS date over a DOS time (exFAT specification, 7.4.8): years since 1980, month, day; hours, minutes, two seconds.
        let date = (moment.year - FIRST_YEAR) << 9 | (moment.month as u32) << 5 | moment.day as u32;
        let time = (moment.hour as u32) << 11 | (moment.minute as u32) << 5 | (moment.second as u32 / 2);
        Stamp { timestamp: date << 16 | time, hundredths: moment.second % 2 * 100, zone }
    }
}

enum Target {
    Directory(usize),
    File { id: usize, size: u64 },
}

struct PlanEntry {
    name: Vec<u16>,
    hash: u16,
    stamp: Stamp,
    target: Target,
}

struct Plan {
    directories: Vec<Vec<PlanEntry>>,
    skip_count: usize,
    offset: Option<i16>,
}

struct Geometry {
    first_sector: u64,
    sectors: u64,
    sectors_per_cluster: u64,
    fat_at: u64,
    fat_sectors: u64,
    heap_at: u64,
    clusters: u64,
}

impl Geometry {
    const fn new(clusters: u64, cluster: u64) -> Geometry {
        let sectors_per_cluster = cluster / SECTOR_SIZE;
        let fat_at = BOOT_SECTORS * BOOT_COPIES;
        let fat_sectors = ((clusters + FIRST_CLUSTER) * FAT_ENTRY_BYTES).div_ceil(SECTOR_SIZE);
        let heap_at = (fat_at + fat_sectors).next_multiple_of(sectors_per_cluster);
        let sectors = heap_at + clusters * sectors_per_cluster;
        Geometry {
            first_sector: sectors_per_cluster,
            sectors,
            sectors_per_cluster,
            fat_at,
            fat_sectors,
            heap_at,
            clusters,
        }
    }
}

fn checksum32(bytes: impl Iterator<Item = u8>) -> u32 {
    bytes.fold(0, |sum: u32, byte| sum.rotate_right(1).wrapping_add(byte as u32))
}

fn checksum16(bytes: impl Iterator<Item = u8>) -> u16 {
    bytes.fold(0, |sum: u16, byte| sum.rotate_right(1).wrapping_add(byte as u16))
}

fn upcase_table() -> Vec<u16> {
    let mut table: Vec<u16> = (0..=u16::MAX).collect();
    let mut words = UPCASE.as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes);
    let mut index = 0;
    while let Some(word) = words.next() {
        if index >= table.len() {
            break;
        }
        if word as usize == index {
            index += 1;
        } else if word == UPCASE_RUN {
            index += words.next().unwrap_or(0) as usize;
        } else {
            table[index] = word;
            index += 1;
        }
    }
    table
}

fn is_legal(unit: u16) -> bool {
    unit >= ' ' as u16 && !ILLEGAL_CHARS.iter().any(|&illegal| illegal as u16 == unit)
}

fn name_units(name: &str) -> Option<Vec<u16>> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let has_length = !units.is_empty() && units.len() <= MAX_NAME_UNITS;
    (has_length && !name.ends_with(['.', ' ']) && units.iter().all(|&unit| is_legal(unit))).then_some(units)
}

fn entry_count(units: usize) -> usize {
    2 + units.div_ceil(NAME_UNITS_PER_ENTRY)
}

fn reserved_entries(index: usize) -> usize {
    if index == ROOT { ROOT_SYSTEM_ENTRIES } else { 0 }
}

impl Plan {
    fn add_directory(&mut self, nodes: Vec<Node>, upcase: &[u16]) -> usize {
        let index = self.directories.len();
        self.directories.push(Vec::new());
        let (mut entries, mut count, mut names) = (Vec::new(), reserved_entries(index), BTreeSet::new());
        for node in nodes {
            let Some(name) = name_units(&node.name) else {
                self.skip_count += 1;
                continue;
            };
            let upper_name: Vec<u16> = name.iter().map(|&unit| upcase[unit as usize]).collect();
            let hash = checksum16(upper_name.iter().flat_map(|unit| unit.to_le_bytes()));
            let set_entries = entry_count(name.len());
            if ((count + set_entries) * ENTRY_SIZE) as u64 > MAX_DIRECTORY_BYTES || names.contains(&upper_name) {
                self.skip_count += 1;
                continue;
            }
            let target = match node.content {
                Content::Directory(children) => Target::Directory(self.add_directory(children, upcase)),
                Content::File { id, size } => Target::File { id, size },
                // What is neither takes no entry, and leaves its name to another.
                Content::Other => {
                    self.skip_count += 1;
                    continue;
                }
            };
            count += set_entries;
            names.insert(upper_name);
            entries.push(PlanEntry { name, hash, stamp: Stamp::new(node.modified, self.offset), target });
        }
        self.directories[index] = entries;
        index
    }

    fn directory_bytes(&self, index: usize) -> u64 {
        let entries: usize = self.directories[index].iter().map(|entry| entry_count(entry.name.len())).sum();
        ((reserved_entries(index) + entries) * ENTRY_SIZE) as u64
    }
}

fn blank_entry(kind: u8) -> [u8; ENTRY_SIZE] {
    let mut entry = [0; ENTRY_SIZE];
    entry[0] = kind;
    entry
}

fn set_clusters(entry: &mut [u8; ENTRY_SIZE], first_cluster: u64, bytes: u64) {
    entry[20..24].copy_from_slice(&(first_cluster as u32).to_le_bytes());
    entry[24..].copy_from_slice(&bytes.to_le_bytes());
}

fn set_units(field: &mut [u8], units: &[u16]) {
    for (slot, unit) in field.as_chunks_mut::<2>().0.iter_mut().zip(units) {
        *slot = unit.to_le_bytes();
    }
}

fn entry_set(entry: &PlanEntry, first_cluster: u64, bytes: u64) -> Vec<u8> {
    let is_directory = matches!(entry.target, Target::Directory(_));
    // The file entry (exFAT specification, 7.4): the entries after it, its attributes, three stamps, their hundredths and their offsets.
    let mut file = blank_entry(FILE_ENTRY);
    file[1] = (entry_count(entry.name.len()) - 1) as u8;
    file[4..6].copy_from_slice(&if is_directory { DIRECTORY } else { ARCHIVE }.to_le_bytes());
    for stamp in [8, 12, 16] {
        file[stamp..stamp + 4].copy_from_slice(&entry.stamp.timestamp.to_le_bytes());
    }
    file[20..22].fill(entry.stamp.hundredths);
    // Created, modified and accessed share the node's one time, each stamp with its offset from UTC.
    file[22..25].fill(entry.stamp.zone);

    // The stream extension (7.6): its flags, the name's length and hash, the valid length, then the first cluster and the length.
    let mut stream = blank_entry(STREAM_ENTRY);
    stream[1] = if bytes > 0 { ALLOCATION_POSSIBLE | NO_FAT_CHAIN } else { ALLOCATION_POSSIBLE };
    stream[3] = entry.name.len() as u8;
    stream[4..6].copy_from_slice(&entry.hash.to_le_bytes());
    stream[8..16].copy_from_slice(&bytes.to_le_bytes());
    set_clusters(&mut stream, if bytes > 0 { first_cluster } else { 0 }, bytes);

    let mut set: Vec<u8> = file.into_iter().chain(stream).collect();
    for units in entry.name.chunks(NAME_UNITS_PER_ENTRY) {
        let mut name = blank_entry(NAME_ENTRY);
        set_units(&mut name[2..], units);
        set.extend(name);
    }
    // The set's checksum over all its bytes but the two it stands in (7.4.2).
    let checksum =
        checksum16(set.iter().enumerate().filter(|(index, _)| !matches!(index, 2 | 3)).map(|(_, &byte)| byte));
    set[2..4].copy_from_slice(&checksum.to_le_bytes());
    set
}

fn system_entries(label: &str, cluster_count: u64, upcase_first: u64) -> Vec<u8> {
    let units: Vec<u16> = name_units(label).unwrap_or_default().into_iter().take(MAX_LABEL_UNITS).collect();
    let mut name = blank_entry(LABEL_ENTRY);
    name[1] = units.len() as u8;
    set_units(&mut name[2..], &units);
    let mut bitmap = blank_entry(BITMAP_ENTRY);
    set_clusters(&mut bitmap, FIRST_CLUSTER, cluster_count.div_ceil(8));
    let mut table = blank_entry(UPCASE_ENTRY);
    table[4..8].copy_from_slice(&checksum32(UPCASE.iter().copied()).to_le_bytes());
    set_clusters(&mut table, upcase_first, UPCASE.len() as u64);
    [name, bitmap, table].concat()
}

fn claim(next: &mut u64, clusters: u64) -> u64 {
    let first = *next;
    *next += clusters;
    first
}

fn sort_by_name(nodes: &mut [Node]) {
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    for node in nodes {
        if let Content::Directory(children) = &mut node.content {
            sort_by_name(children);
        }
    }
}

fn partition_table(serial: u32, first_sector: u64, sectors: u64) -> Vec<u8> {
    let mut sector = vec![0u8; SECTOR_SIZE as usize];
    sector[DISK_SIGNATURE_AT..DISK_SIGNATURE_AT + 4].copy_from_slice(&serial.to_le_bytes());
    // The first entry: where it begins and ends as cylinder, head and sector around its type, then its first sector and how many it has.
    let partition = &mut sector[PARTITION_TABLE_AT..];
    partition[1..4].copy_from_slice(&MAX_CHS);
    partition[4] = PARTITION_TYPE;
    partition[5..8].copy_from_slice(&MAX_CHS);
    partition[8..12].copy_from_slice(&(first_sector as u32).to_le_bytes());
    partition[12..16].copy_from_slice(&(sectors as u32).to_le_bytes());
    sector[SECTOR_SIZE as usize - 2..].copy_from_slice(&BOOT_SIGNATURE);
    sector
}

fn boot_region(geometry: &Geometry, root_cluster: u64, used_clusters: u64, serial: u32) -> Vec<u8> {
    // The main boot sector at the places the exFAT specification gives its fields (3.1).
    let mut boot = vec![0u8; (BOOT_SECTORS * SECTOR_SIZE) as usize];
    boot[..3].copy_from_slice(&[0xeb, 0x76, 0x90]);
    boot[3..11].copy_from_slice(b"EXFAT   ");
    boot[64..72].copy_from_slice(&geometry.first_sector.to_le_bytes());
    boot[72..80].copy_from_slice(&geometry.sectors.to_le_bytes());
    let words = [
        (80, geometry.fat_at),
        (84, geometry.fat_sectors),
        (88, geometry.heap_at),
        (92, geometry.clusters),
        (96, root_cluster),
    ];
    for (at, value) in words {
        boot[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }
    boot[100..104].copy_from_slice(&serial.to_le_bytes());
    boot[104..106].copy_from_slice(&REVISION.to_le_bytes());
    boot[108..113].copy_from_slice(&[
        SECTOR_SHIFT,
        geometry.sectors_per_cluster.trailing_zeros() as u8,
        FATS,
        FIRST_HARD_DISK,
        (used_clusters * 100 / geometry.clusters) as u8,
    ]);
    boot[BOOT_CODE].fill(HALT);
    boot[BOOT_CODE.end..BOOT_CODE.end + 2].copy_from_slice(&BOOT_SIGNATURE);
    for sector in EXTENDED_BOOT_SECTORS {
        let end = (sector + 1) * SECTOR_SIZE as usize;
        boot[end - 4..end].copy_from_slice(&EXTENDED_BOOT_SIGNATURE);
    }
    let checksum_at = CHECKSUM_SECTOR * SECTOR_SIZE as usize;
    let checksum = checksum32(
        boot[..checksum_at]
            .iter()
            .enumerate()
            .filter(|(index, _)| !UNCHECKED_BOOT_BYTES.contains(index))
            .map(|(_, &byte)| byte),
    );
    boot[checksum_at..].as_chunks_mut::<4>().0.iter_mut().for_each(|word| *word = checksum.to_le_bytes());
    boot.extend_from_within(..);
    boot
}

fn fat_bytes(chains: [u64; 3]) -> Vec<u8> {
    let mut fat: Vec<u32> = vec![FAT_MEDIA, FAT_END];
    for clusters in chains {
        let end = fat.len() as u32 + clusters as u32;
        fat.extend((fat.len() as u32 + 1..end).chain([FAT_END]));
    }
    fat.into_iter().flat_map(u32::to_le_bytes).collect()
}

fn overlap(at: u64, length: u64, offset: u64, end: u64) -> Option<(u64, usize, usize)> {
    let (start, stop) = (at.max(offset), (at + length).min(end));
    (start < stop).then(|| (start - at, (start - offset) as usize, (stop - start) as usize))
}

impl Volume {
    /// Lays out a volume of `nodes`, each directory's entries by name; entries exFAT cannot hold and `Content::Other` are skipped and counted.
    pub fn build(spec: &Spec<'_>, mut nodes: Vec<Node>) -> Result<Volume, BuildError> {
        let cluster = spec.cluster_size as u64;
        if !cluster.is_power_of_two() || !(SECTOR_SIZE..=MAX_CLUSTER_BYTES).contains(&cluster) {
            return Err(BAD_CLUSTER_SIZE);
        }
        sort_by_name(&mut nodes);
        let upcase = upcase_table();
        let mut plan = Plan { directories: Vec::new(), skip_count: 0, offset: spec.offset };
        plan.add_directory(nodes, &upcase);

        let to_clusters = |bytes: u64| bytes.div_ceil(cluster);
        let directory_clusters: Vec<u64> =
            (0..plan.directories.len()).map(|index| to_clusters(plan.directory_bytes(index)).max(1)).collect();
        let file_clusters = plan.directories.iter().flatten().map(|entry| match entry.target {
            Target::File { size, .. } => to_clusters(size),
            Target::Directory(_) => 0,
        });
        let upcase_clusters = to_clusters(UPCASE.len() as u64);
        let content = upcase_clusters + directory_clusters.iter().sum::<u64>() + file_clusters.sum::<u64>();

        let mut count = (spec.capacity / cluster).max(to_clusters(MIN_VOLUME_BYTES));
        let (bitmap_clusters, used_clusters) = loop {
            let bitmap_clusters = to_clusters(count.div_ceil(8));
            if bitmap_clusters + content <= count {
                break (bitmap_clusters, bitmap_clusters + content);
            }
            count = bitmap_clusters + content;
        };
        let geometry = Geometry::new(count, cluster);
        if count > MAX_CLUSTERS || geometry.first_sector + geometry.sectors > u32::MAX as u64 {
            return Err(TOO_LARGE);
        }

        let mut next_cluster = bitmap_clusters + upcase_clusters;
        let directory_first: Vec<u64> =
            directory_clusters.iter().map(|&clusters| claim(&mut next_cluster, clusters)).collect();
        let mut extents = vec![
            Extent { first: 0, clusters: bitmap_clusters, source: Source::Bitmap },
            Extent { first: bitmap_clusters, clusters: upcase_clusters, source: Source::Bytes(UPCASE.to_vec()) },
        ];
        let mut file_extents = Vec::new();
        for (index, entries) in plan.directories.iter().enumerate() {
            let mut bytes = if index == ROOT {
                system_entries(spec.label, count, FIRST_CLUSTER + bitmap_clusters)
            } else {
                Vec::new()
            };
            for entry in entries {
                let (first, length) = match entry.target {
                    Target::Directory(child) => (directory_first[child], directory_clusters[child] * cluster),
                    Target::File { id, size } => {
                        let clusters = to_clusters(size);
                        let first = claim(&mut next_cluster, clusters);
                        if size > 0 {
                            file_extents.push(Extent { first, clusters, source: Source::File { id, size } });
                        }
                        (first, size)
                    }
                };
                bytes.extend(entry_set(entry, FIRST_CLUSTER + first, length));
            }
            extents.push(Extent {
                first: directory_first[index],
                clusters: directory_clusters[index],
                source: Source::Bytes(bytes),
            });
        }
        extents.extend(file_extents);

        let volume_at = geometry.first_sector * SECTOR_SIZE;
        Ok(Volume {
            size: volume_at + geometry.sectors * SECTOR_SIZE,
            cluster,
            table: partition_table(spec.serial, geometry.first_sector, geometry.sectors),
            volume_at,
            fat_at: volume_at + geometry.fat_at * SECTOR_SIZE,
            heap_at: volume_at + geometry.heap_at * SECTOR_SIZE,
            boot: boot_region(&geometry, FIRST_CLUSTER + directory_first[ROOT], used_clusters, spec.serial),
            fat: fat_bytes([bitmap_clusters, upcase_clusters, directory_clusters[ROOT]]),
            used_clusters,
            extents,
            skip_count: plan.skip_count,
        })
    }

    /// Bytes of the whole medium.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Bytes of the clusters in use, metadata included.
    pub fn used_bytes(&self) -> u64 {
        self.used_clusters * self.cluster
    }

    /// Entries of the tree the volume does not show.
    pub fn skip_count(&self) -> usize {
        self.skip_count
    }

    /// Fills `buffer` with the medium's bytes from `offset` on, reading file contents through `files`.
    pub fn read(&self, offset: u64, buffer: &mut [u8], files: &dyn Files) {
        buffer.fill(0);
        let end = offset + buffer.len() as u64;
        for (at, bytes) in [(0, &self.table), (self.volume_at, &self.boot), (self.fat_at, &self.fat)] {
            if let Some((skip, start, length)) = overlap(at, bytes.len() as u64, offset, end) {
                buffer[start..start + length].copy_from_slice(&bytes[skip as usize..skip as usize + length]);
            }
        }
        let first_cluster = offset.saturating_sub(self.heap_at) / self.cluster;
        let first_extent = self.extents.partition_point(|extent| extent.first + extent.clusters <= first_cluster);
        for extent in &self.extents[first_extent..] {
            let at = self.heap_at + extent.first * self.cluster;
            if at >= end {
                break;
            }
            let length = match &extent.source {
                Source::Bitmap => self.used_clusters.div_ceil(8),
                Source::Bytes(bytes) => bytes.len() as u64,
                Source::File { size, .. } => *size,
            };
            let Some((skip, start, length)) = overlap(at, length, offset, end) else {
                continue;
            };
            let target = &mut buffer[start..start + length];
            match &extent.source {
                Source::Bitmap => {
                    for (index, byte) in target.iter_mut().enumerate() {
                        let cluster = (skip + index as u64) * 8;
                        *byte = if cluster + 8 <= self.used_clusters {
                            0xff
                        } else {
                            (1 << (self.used_clusters - cluster)) - 1
                        };
                    }
                }
                Source::Bytes(bytes) => target.copy_from_slice(&bytes[skip as usize..skip as usize + length]),
                Source::File { id, .. } => files.fetch(*id, skip, target),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::checksum::fnv;
    use alloc::format;
    use alloc::string::ToString;

    const CLUSTER: usize = 4096;
    const SECTOR: usize = SECTOR_SIZE as usize;
    const MODIFIED: u64 = 1_747_392_631;
    const RECOMMENDED_UPCASE_CHECKSUM: u32 = 0xe619_d30d;
    const REFERENCE_HASH: u64 = 0x17e6_4cf3_0ac0_4915;

    fn spec() -> Spec<'static> {
        Spec { label: "r1metal", serial: 0x6f1d_8a52, cluster_size: CLUSTER as u32, capacity: 4 << 20, offset: None }
    }

    fn file(name: &str, id: usize, size: u64) -> Node {
        Node { name: name.to_string(), modified: MODIFIED + id as u64, content: Content::File { id, size } }
    }

    fn directory(name: &str, nodes: Vec<Node>) -> Node {
        Node { name: name.to_string(), modified: MODIFIED, content: Content::Directory(nodes) }
    }

    const REFERENCE_FILES: [(&str, usize, u64); 5] = [
        ("camera/clip.mp4", 0, 10_000),
        ("audio/note.opus", 1, 4096),
        ("files/Ünïcödé 日本語 🐇.txt", 2, 1),
        ("files/empty", 3, 0),
        ("files/a-name-longer-than-fifteen-units.txt", 4, 4097),
    ];

    fn reference() -> Vec<Node> {
        let files = |parent: &str| -> Vec<Node> {
            let inside = REFERENCE_FILES
                .iter()
                .filter_map(|&(path, id, size)| Some((path.strip_prefix(parent)?.strip_prefix('/')?, id, size)));
            inside.map(|(name, id, size)| file(name, id, size)).collect()
        };
        ["camera", "audio", "files", "chats"].map(|name| directory(name, files(name))).into()
    }

    fn content(id: usize, offset: u64, buffer: &mut [u8]) {
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = ((offset + index as u64) as u8).wrapping_mul(31).wrapping_add(id as u8);
        }
    }

    struct Contents;

    impl Files for Contents {
        fn fetch(&self, id: usize, offset: u64, buffer: &mut [u8]) {
            content(id, offset, buffer);
        }
    }

    fn image(volume: &Volume) -> Vec<u8> {
        let mut image = vec![0xaa; volume.size() as usize];
        volume.read(0, &mut image, &Contents);
        image
    }

    fn paths(volume: &Volume) -> Vec<String> {
        Host::open(&image(volume)).files().into_iter().map(|(path, _)| path).collect()
    }

    fn u32_at(bytes: &[u8], at: usize) -> usize {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
    }

    fn u64_at(bytes: &[u8], at: usize) -> usize {
        u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize
    }

    struct Host<'a> {
        volume: &'a [u8],
        fat: usize,
        heap: usize,
        cluster: usize,
        clusters: usize,
        root: usize,
    }

    type Listing = (String, bool, u8, usize, usize);

    impl<'a> Host<'a> {
        fn open(medium: &'a [u8]) -> Host<'a> {
            assert_eq!(medium[SECTOR - 2..SECTOR], BOOT_SIGNATURE);
            assert_eq!(medium[PARTITION_TABLE_AT + 4], PARTITION_TYPE);
            let first = u32_at(medium, PARTITION_TABLE_AT + 8) * SECTOR;
            let volume = &medium[first..first + u32_at(medium, PARTITION_TABLE_AT + 12) * SECTOR];
            assert_eq!(volume[3..11], *b"EXFAT   ");
            assert_eq!(u64_at(volume, 72) * SECTOR, volume.len());
            let (sector, cluster) = (1 << volume[108], 1 << (volume[108] + volume[109]));
            Host {
                volume,
                fat: u32_at(volume, 80) * sector,
                heap: u32_at(volume, 88) * sector,
                cluster,
                clusters: u32_at(volume, 92),
                root: u32_at(volume, 96),
            }
        }

        fn clusters(&self, first: usize, count: usize) -> &'a [u8] {
            assert!(first >= 2 && first + count <= self.clusters + 2, "clusters {first}+{count} lie outside the heap");
            &self.volume[self.heap + (first - 2) * self.cluster..][..count * self.cluster]
        }

        fn chain(&self, first: usize) -> Vec<u8> {
            let (mut bytes, mut cluster) = (Vec::new(), first);
            while cluster != FAT_END as usize {
                bytes.extend(self.clusters(cluster, 1));
                cluster = u32_at(self.volume, self.fat + cluster * 4);
            }
            bytes
        }

        fn list(&self, directory: &[u8]) -> Vec<Listing> {
            let upcase = upcase_table();
            let mut entries = directory.as_chunks::<ENTRY_SIZE>().0.iter().take_while(|entry| entry[0] != 0);
            let mut listings = Vec::new();
            while let Some(entry) = entries.next() {
                if entry[0] != FILE_ENTRY {
                    continue;
                }
                let secondary: Vec<&[u8; ENTRY_SIZE]> = entries.by_ref().take(entry[1] as usize).collect();
                let set: Vec<u8> =
                    core::iter::once(entry).chain(secondary.iter().copied()).flatten().copied().collect();
                let checksum = checksum16(
                    set.iter().enumerate().filter(|(index, _)| !matches!(index, 2 | 3)).map(|(_, &byte)| byte),
                );
                assert_eq!(u16::from_le_bytes([entry[2], entry[3]]), checksum);
                let stream = secondary[0];
                assert_eq!(stream[0], STREAM_ENTRY);
                assert!(secondary[1..].iter().all(|name| name[0] == NAME_ENTRY && name[1] == 0));
                let units: Vec<u16> = secondary[1..]
                    .iter()
                    .flat_map(|name| name[2..].as_chunks::<2>().0)
                    .copied()
                    .map(u16::from_le_bytes)
                    .collect();
                let units = &units[..stream[3] as usize];
                assert_eq!(secondary.len() - 1, units.len().div_ceil(NAME_UNITS_PER_ENTRY));
                let hash = checksum16(units.iter().flat_map(|&unit| upcase[unit as usize].to_le_bytes()));
                assert_eq!(u16::from_le_bytes([stream[4], stream[5]]), hash);
                assert_eq!(u64_at(stream, 8), u64_at(stream, 24), "all of a file's length is valid data");
                let is_directory = u16::from_le_bytes([entry[4], entry[5]]) & DIRECTORY != 0;
                listings.push((
                    String::from_utf16(units).unwrap(),
                    is_directory,
                    stream[1],
                    u32_at(stream, 20),
                    u64_at(stream, 24),
                ));
            }
            listings
        }

        fn walk(&self, directory: &[u8], prefix: &str, files: &mut Vec<(String, Vec<u8>)>) {
            for (name, is_directory, flags, first, length) in self.list(directory) {
                let path = format!("{prefix}{name}");
                let bytes = match length {
                    0 => {
                        assert_eq!((flags, first), (ALLOCATION_POSSIBLE, 0), "{path} owns nothing");
                        &[][..]
                    }
                    _ => {
                        assert_eq!(flags, ALLOCATION_POSSIBLE | NO_FAT_CHAIN, "{path}");
                        &self.clusters(first, length.div_ceil(self.cluster))[..length]
                    }
                };
                if is_directory {
                    assert!(length > 0 && length % self.cluster == 0, "{path}");
                    files.push((format!("{path}/"), Vec::new()));
                    self.walk(bytes, &format!("{path}/"), files);
                } else {
                    files.push((path, bytes.to_vec()));
                }
            }
        }

        fn files(&self) -> Vec<(String, Vec<u8>)> {
            let mut files = Vec::new();
            self.walk(&self.chain(self.root), "", &mut files);
            files
        }
    }

    #[test]
    fn recommended_upcase_table() {
        assert_eq!(checksum32(UPCASE.iter().copied()), RECOMMENDED_UPCASE_CHECKSUM);
        let table = upcase_table();
        assert_eq!(table.len(), 1 << 16);
        let upper = |unit: char| char::from_u32(table[unit as usize] as u32).unwrap();
        assert_eq!(
            ['a', 'z', 'A', '1', '{', 'ä', 'ÿ', 'α', 'я', 'ß'].map(upper),
            ['A', 'Z', 'A', '1', '{', 'Ä', 'Ÿ', 'Α', 'Я', 'ß']
        );
        assert_eq!((table[0xd800], table[0xffff]), (0xd800, 0xffff));
    }

    #[test]
    fn host_reads_files() {
        let volume = Volume::build(&spec(), reference()).unwrap();
        let medium = image(&volume);
        let mut expected: Vec<(String, Vec<u8>)> = REFERENCE_FILES
            .iter()
            .map(|&(path, id, size)| {
                let mut bytes = vec![0; size as usize];
                content(id, 0, &mut bytes);
                (path.to_string(), bytes)
            })
            .chain(["camera/", "audio/", "files/", "chats/"].map(|path| (path.to_string(), Vec::new())))
            .collect();
        let mut actual = Host::open(&medium).files();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
        assert_eq!(volume.skip_count(), 0);
    }

    #[test]
    fn reference_medium_hash() {
        assert_eq!(fnv(&image(&Volume::build(&spec(), reference()).unwrap())), REFERENCE_HASH);
    }

    #[test]
    fn boot_sector_areas() {
        let volume = Volume::build(&spec(), reference()).unwrap();
        let medium = image(&volume);
        let host = Host::open(&medium);
        let boot = host.volume;
        assert_eq!((boot[..3].to_vec(), boot[510..512].to_vec()), (vec![0xeb, 0x76, 0x90], BOOT_SIGNATURE.to_vec()));
        assert!(boot[11..64].iter().all(|&byte| byte == 0) && boot[BOOT_CODE].iter().all(|&byte| byte == HALT));
        let (fat, fat_sectors, heap, count) = (u32_at(boot, 80), u32_at(boot, 84), u32_at(boot, 88), u32_at(boot, 92));
        assert_eq!((u64_at(boot, 64), fat, count), (CLUSTER / SECTOR, 24, 1024));
        assert_eq!(fat_sectors, ((count + 2) * 4).div_ceil(SECTOR));
        assert_eq!(heap, (fat + fat_sectors).next_multiple_of(CLUSTER / SECTOR));
        assert_eq!(u64_at(boot, 72), heap + count * (CLUSTER / SECTOR));
        assert_eq!(boot[104..112], [0x00, 0x01, 0, 0, 9, 3, 1, 0x80]);
        assert_eq!(volume.size() as usize, CLUSTER + boot.len());
        assert_eq!(medium[DISK_SIGNATURE_AT..DISK_SIGNATURE_AT + 4], boot[100..104]);
    }

    #[test]
    fn boot_region_checksum() {
        let medium = image(&Volume::build(&spec(), reference()).unwrap());
        let volume = Host::open(&medium).volume;
        let region = BOOT_SECTORS as usize * SECTOR;
        assert_eq!(volume[..region], volume[region..2 * region]);
        for sector in EXTENDED_BOOT_SECTORS {
            let (code, signature) = volume[sector * SECTOR..][..SECTOR].split_at(SECTOR - 4);
            assert!(code.iter().all(|&byte| byte == 0) && signature == EXTENDED_BOOT_SIGNATURE);
        }
        let checked = volume[..CHECKSUM_SECTOR * SECTOR]
            .iter()
            .enumerate()
            .filter(|(index, _)| !UNCHECKED_BOOT_BYTES.contains(index));
        let checksum = checksum32(checked.map(|(_, &byte)| byte)).to_le_bytes();
        assert!(volume[CHECKSUM_SECTOR * SECTOR..region].as_chunks::<4>().0.iter().all(|&word| word == checksum));
    }

    #[test]
    fn root_system_entries() {
        let volume = Volume::build(&spec(), reference()).unwrap();
        let medium = image(&volume);
        let host = Host::open(&medium);
        let root = host.chain(host.root);
        let (label, bitmap, table) = (&root[..32], &root[32..64], &root[64..96]);
        assert_eq!((label[0], label[1]), (LABEL_ENTRY, 7));
        assert_eq!(
            String::from_utf16_lossy(
                &label[2..16].as_chunks::<2>().0.iter().copied().map(u16::from_le_bytes).collect::<Vec<_>>()
            ),
            "r1metal"
        );
        assert_eq!((bitmap[0], u32_at(bitmap, 20), u64_at(bitmap, 24)), (BITMAP_ENTRY, 2, host.clusters.div_ceil(8)));
        assert_eq!(
            (table[0], u32_at(table, 4), u64_at(table, 24)),
            (UPCASE_ENTRY, RECOMMENDED_UPCASE_CHECKSUM as usize, UPCASE.len())
        );
        assert_eq!(host.chain(u32_at(table, 20))[..UPCASE.len()], *UPCASE);
        assert_eq!(host.root, u32_at(table, 20) + UPCASE.len().div_ceil(CLUSTER), "the root follows the table");
        assert_eq!(u32_at(host.volume, host.fat), FAT_MEDIA as usize);
    }

    #[test]
    fn bitmap_marks_used_clusters() {
        let volume = Volume::build(&spec(), reference()).unwrap();
        let medium = image(&volume);
        let host = Host::open(&medium);
        let bitmap = &host.chain(2)[..host.clusters.div_ceil(8)];
        let used = (volume.used_bytes() / CLUSTER as u64) as usize;
        let marked = |cluster: usize| bitmap[cluster / 8] >> (cluster % 8) & 1 == 1;
        assert!((0..used).all(marked) && !(used..host.clusters).any(marked));
        let content: usize = host
            .files()
            .iter()
            .map(|(path, bytes)| if path.ends_with('/') { 1 } else { bytes.len().div_ceil(CLUSTER) })
            .sum();
        assert_eq!(
            used,
            1 + UPCASE.len().div_ceil(CLUSTER) + 1 + content,
            "bitmap, table, root and what the tree holds"
        );
        assert_eq!(host.volume[112] as usize, used * 100 / host.clusters);
    }

    #[test]
    fn read_at_any_offset() {
        let volume = Volume::build(&spec(), reference()).unwrap();
        let medium = image(&volume);
        for (step, length) in [1usize, 7, 511, 512, 513, 4095, 4097, 70_000].into_iter().cycle().take(600).enumerate() {
            let offset = step * 104_729 % (medium.len() - length);
            let mut piece = vec![0xaa; length];
            volume.read(offset as u64, &mut piece, &Contents);
            assert!(piece == medium[offset..offset + length], "{length} bytes at {offset}");
        }
    }

    #[test]
    fn illegal_names_skipped() {
        let longest = "n".repeat(MAX_NAME_UNITS);
        let names = [
            "fine.txt",
            "Fine.TXT",
            "FINE.txt",
            "a:b",
            "what?",
            "tab\there",
            "dot.",
            "space ",
            ".",
            "..",
            "",
            &longest,
            &format!("{longest}n"),
            ".hidden",
            "ÄRGER",
            "ärger",
        ];
        let nodes = names.iter().enumerate().map(|(id, name)| file(name, id, 1)).collect();
        let volume = Volume::build(&spec(), nodes).unwrap();
        let actual = paths(&volume);
        assert_eq!(actual, [".hidden", "FINE.txt", &longest, "ÄRGER"], "of names that differ in case alone, the first");
        assert_eq!(volume.skip_count(), names.len() - actual.len());
    }

    #[test]
    fn entries_sorted_by_name() {
        let other = |name: &str| Node { name: name.to_string(), modified: 0, content: Content::Other };
        let nodes = vec![
            other("link"),
            directory("b", vec![file("z", 0, 1), other("socket"), file("y", 1, 2)]),
            file("a.txt", 2, 3),
            other("A.TXT"),
        ];
        let volume = Volume::build(&spec(), nodes).unwrap();
        assert_eq!(
            (paths(&volume), volume.skip_count()),
            (["a.txt", "b/", "b/y", "b/z"].map(String::from).to_vec(), 3),
            "what is left out takes no name from a file, here one that differs in case alone"
        );
    }

    #[test]
    fn volume_grows() {
        let empty = Volume::build(&spec(), Vec::new()).unwrap();
        assert_eq!(empty.used_bytes(), 4 * CLUSTER as u64, "bitmap, two clusters of table, root");
        let large = 3 * spec().capacity;
        let full = Volume::build(&spec(), vec![file("large", 0, large)]).unwrap();
        assert_eq!(full.used_bytes(), empty.used_bytes() + large);
        assert!(full.size() > empty.size() + large - spec().capacity);
        let medium = image(&full);
        assert_eq!(Host::open(&medium).clusters as u64, full.used_bytes() / CLUSTER as u64);
    }

    #[test]
    fn smallest_volume() {
        let volume = Volume::build(&Spec { capacity: 0, ..spec() }, Vec::new()).unwrap();
        let medium = image(&volume);
        assert_eq!(Host::open(&medium).clusters * CLUSTER, MIN_VOLUME_BYTES as usize);
    }

    #[test]
    fn two_second_stamps() {
        let stamp = Stamp::new(MODIFIED, None);
        assert_eq!(stamp.timestamp, 45 << 25 | 5 << 21 | 16 << 16 | 10 << 11 | 50 << 5 | 15);
        assert_eq!((stamp.hundredths, stamp.zone), (100, 0));
        assert_eq!(Stamp::new(MODIFIED - 1, None).hundredths, 0);
        let (early, late) = (Stamp::new(0, None), Stamp::new(4_500_000_000, None));
        assert_eq!((early.timestamp, early.hundredths), (1 << 21 | 1 << 16, 0));
        assert_eq!((late.timestamp, late.hundredths), (127 << 25 | 12 << 21 | 31 << 16 | 23 << 11 | 59 << 5 | 29, 100));
    }

    #[test]
    fn local_stamps() {
        let at = |offset: Option<i16>| Stamp::new(MODIFIED, offset);
        let ahead = at(Some(120));
        assert_eq!(
            (ahead.timestamp, ahead.zone),
            (45 << 25 | 5 << 21 | 16 << 16 | 12 << 11 | 50 << 5 | 15, 0x88),
            "10:50:31 UTC is 12:50:31 two hours east, eight quarter hours ahead"
        );
        let behind = at(Some(-210));
        assert_eq!(
            (behind.timestamp, behind.zone),
            (45 << 25 | 5 << 21 | 16 << 16 | 7 << 11 | 20 << 5 | 15, 0xf2),
            "and 07:20:31 three and a half hours west, fourteen quarter hours behind"
        );
        assert_eq!(
            (at(Some(0)).timestamp, at(Some(0)).zone),
            (at(None).timestamp, 0x80),
            "UTC itself is an offset that is known"
        );
        assert_eq!(
            (at(Some(840)).zone, at(Some(-660)).zone, at(Some(765)).zone),
            (0x80 | 56, 0x80 | (-44i8 as u8 & 0x7f), 0x80 | 51)
        );
        assert_eq!(
            (at(Some(7)).zone, at(Some(7)).timestamp),
            (0, 45 << 25 | 5 << 21 | 16 << 16 | 10 << 11 | 57 << 5 | 15),
            "an offset that is no quarter hour moves the time and cannot be told"
        );
        let volume = Volume::build(&Spec { offset: Some(120), ..spec() }, reference()).unwrap();
        let medium = image(&volume);
        let host = Host::open(&medium);
        let root = host.chain(host.root);
        let entry = root
            .as_chunks::<ENTRY_SIZE>()
            .0
            .iter()
            .find(|entry| entry[0] == FILE_ENTRY)
            .expect("a directory of the tree");
        assert_eq!(
            (u32_at(entry, 8), u32_at(entry, 12), u32_at(entry, 16), &entry[20..25]),
            (
                ahead.timestamp as usize,
                ahead.timestamp as usize,
                ahead.timestamp as usize,
                &[100, 100, 0x88, 0x88, 0x88][..]
            ),
            "made, changed and read, each with its offset"
        );
        assert_eq!(
            host.files().len(),
            REFERENCE_FILES.len() + 4,
            "and a host still finds every file, the checksums of the entries right"
        );
    }

    #[test]
    fn refused_sizes() {
        let sized =
            |cluster_size| Volume::build(&Spec { cluster_size, ..spec() }, Vec::new()).map(|volume| volume.size());
        assert_eq!(sized(0).err(), Some(BAD_CLUSTER_SIZE));
        assert_eq!(sized(256).err(), Some(BAD_CLUSTER_SIZE));
        assert_eq!(sized(3000).err(), Some(BAD_CLUSTER_SIZE));
        assert_eq!(sized(64 << 20).err(), Some(BAD_CLUSTER_SIZE));
        assert!(sized(512).is_ok() && sized(32 << 20).is_ok());
        let huge = Volume::build(&Spec { capacity: 3 << 40, ..spec() }, Vec::new()).map(|volume| volume.size());
        assert_eq!(huge.err(), Some(TOO_LARGE));
    }
}

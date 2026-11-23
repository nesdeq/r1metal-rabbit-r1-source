//! Android sparse images, read in place: any part of the image one stands for, without unpacking it, and the image cut into sparse images that each fit one download of a bootloader.

use crate::{ReadAt, half, invalid, word};
use std::io::{self, Read, Seek, SeekFrom};

const MAGIC: u32 = 0xED26_FF3A;
const MAJOR_VERSION: u16 = 1;
const HEADER_BYTES: usize = 28;
const CHUNK_HEADER_BYTES: usize = 12;
const CHUNK_HEAD: u64 = CHUNK_HEADER_BYTES as u64;
const FILL_BYTES: u64 = 4;
// The kinds of chunk: bytes as they are, four bytes over and over, nothing (which reads as zeros), and a checksum over what came before.
const RAW: u16 = 0xCAC1;
const FILL: u16 = 0xCAC2;
const SKIP: u16 = 0xCAC3;
const CHECKSUM: u16 = 0xCAC4;

#[derive(Clone, Copy)]
enum Data {
    // Where in the sparse file the bytes lie.
    Raw(u64),
    Fill([u8; 4]),
    Skip,
}

// A run of the image: where it begins there, how long it is, and what it holds.
#[derive(Clone, Copy)]
struct Chunk {
    start: u64,
    bytes: u64,
    data: Data,
}

pub struct Sparse<R> {
    source: R,
    // The bytes of a block, of which every chunk holds a whole number.
    block: u64,
    // The bytes of the image it stands for.
    size: u64,
    chunks: Vec<Chunk>,
}

/// A run of the image, from one block to another, that a sparse image of its own writes while it skips the rest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Part {
    from: u64,
    to: u64,
}

// The bytes a chunk of `bytes` takes in a sparse image, its header with them.
fn encoded(data: Data, bytes: u64) -> u64 {
    CHUNK_HEAD
        + match data {
            Data::Raw(_) => bytes,
            Data::Fill(_) => FILL_BYTES,
            Data::Skip => 0,
        }
}

fn chunk_head(out: &mut Vec<u8>, kind: u16, blocks: u64, payload: u64) {
    out.extend(kind.to_le_bytes());
    out.extend([0, 0]);
    out.extend((blocks as u32).to_le_bytes());
    out.extend(((CHUNK_HEAD + payload) as u32).to_le_bytes());
}

// A sparse image's header, with the sizes of headers every bootloader reads, before `count` chunks that stand for `blocks` blocks.
fn header(block: u64, blocks: u64, count: u32) -> Vec<u8> {
    let mut image = Vec::new();
    image.extend(MAGIC.to_le_bytes());
    image.extend(MAJOR_VERSION.to_le_bytes());
    image.extend(0u16.to_le_bytes());
    image.extend((HEADER_BYTES as u16).to_le_bytes());
    image.extend((CHUNK_HEADER_BYTES as u16).to_le_bytes());
    image.extend((block as u32).to_le_bytes());
    image.extend((blocks as u32).to_le_bytes());
    image.extend(count.to_le_bytes());
    image.extend(0u32.to_le_bytes());
    image
}

impl<R> Sparse<R> {
    /// The bytes of the image it stands for.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The image cut into runs that each fit a sparse image of at most `limit` bytes: written one after the other, they write all it writes. A raw chunk too long for one run is cut between two blocks; a run that would only skip is left out.
    pub fn parts(&self, limit: usize) -> io::Result<Vec<Part>> {
        let too_small = || invalid("a limit too small for a sparse image of one block");
        // A run's chunks have the room its header and the skips before and after it leave.
        let room = (limit as u64).checked_sub(HEADER_BYTES as u64 + 2 * CHUNK_HEAD).ok_or_else(too_small)?;
        let (mut parts, mut from, mut used, mut writes) = (Vec::new(), 0, 0, false);
        for chunk in &self.chunks {
            let (mut at, end) = (chunk.start, chunk.start + chunk.bytes);
            while at < end {
                let left = room - used;
                let fits = match chunk.data {
                    Data::Raw(_) => left.saturating_sub(CHUNK_HEAD) / self.block * self.block,
                    data if left >= encoded(data, 0) => end - at,
                    _ => 0,
                };
                let take = fits.min(end - at);
                if take > 0 {
                    used += encoded(chunk.data, take);
                    writes |= !matches!(chunk.data, Data::Skip);
                    at += take;
                } else if at > from {
                    if writes {
                        parts.push(Part { from, to: at });
                    }
                    (from, used, writes) = (at, 0, false);
                } else {
                    return Err(too_small());
                }
            }
        }
        if writes {
            parts.push(Part { from, to: self.size });
        }
        Ok(parts)
    }
}

impl<R: Read + Seek> Sparse<R> {
    /// Reads the list of chunks of a sparse image.
    pub fn open(mut source: R) -> io::Result<Sparse<R>> {
        let mut header = [0u8; HEADER_BYTES];
        source.seek(SeekFrom::Start(0))?;
        source.read_exact(&mut header)?;
        if word(&header, 0) != MAGIC || half(&header, 4) != MAJOR_VERSION {
            return Err(invalid("not an Android sparse image of version 1"));
        }
        let (file_header, chunk_header) = (u64::from(half(&header, 8)), u64::from(half(&header, 10)));
        let (block, blocks, count) = (u64::from(word(&header, 12)), u64::from(word(&header, 16)), word(&header, 20));
        if file_header < HEADER_BYTES as u64 || chunk_header < CHUNK_HEADER_BYTES as u64 || block == 0 {
            return Err(invalid("a sparse image with impossible header sizes"));
        }
        // Every place and length the chunks add up to is held within a file's reach here, so that no read of them overflows.
        let beyond = || invalid("a sparse image larger than any file");
        let (mut at, mut start, mut chunks) = (file_header, 0u64, Vec::new());
        for _ in 0..count {
            let mut head = [0u8; CHUNK_HEADER_BYTES];
            source.seek(SeekFrom::Start(at))?;
            source.read_exact(&mut head)?;
            let kind = half(&head, 0);
            let bytes = u64::from(word(&head, 4)) * block;
            let total = u64::from(word(&head, 8));
            let body = at.checked_add(chunk_header).ok_or_else(beyond)?;
            at = at.checked_add(total.max(chunk_header)).ok_or_else(beyond)?;
            let data = match kind {
                RAW => {
                    body.checked_add(bytes).ok_or_else(beyond)?;
                    Data::Raw(body)
                }
                FILL => {
                    let mut value = [0u8; 4];
                    source.seek(SeekFrom::Start(body))?;
                    source.read_exact(&mut value)?;
                    Data::Fill(value)
                }
                SKIP => Data::Skip,
                CHECKSUM => continue,
                _ => return Err(invalid("a sparse image with a chunk of an unknown kind")),
            };
            chunks.push(Chunk { start, bytes, data });
            start = start.checked_add(bytes).ok_or_else(beyond)?;
        }
        if start != blocks * block {
            return Err(invalid("a sparse image whose chunks do not add up to its size"));
        }
        Ok(Sparse { source, block, size: start, chunks })
    }

    /// The sparse image that writes `part` of the image and skips all else.
    pub fn part(&mut self, part: Part) -> io::Result<Vec<u8>> {
        let (mut body, mut count) = (Vec::new(), 0u32);
        if part.from > 0 {
            chunk_head(&mut body, SKIP, part.from / self.block, 0);
            count += 1;
        }
        let first = self.chunks.partition_point(|chunk| chunk.start + chunk.bytes <= part.from);
        for chunk in self.chunks[first..].iter().take_while(|chunk| chunk.start < part.to) {
            let (start, end) = (chunk.start.max(part.from), (chunk.start + chunk.bytes).min(part.to));
            if start >= end {
                continue;
            }
            let blocks = (end - start) / self.block;
            match chunk.data {
                Data::Raw(at) => {
                    chunk_head(&mut body, RAW, blocks, end - start);
                    let from = body.len();
                    body.resize(from + (end - start) as usize, 0);
                    self.source.seek(SeekFrom::Start(at + (start - chunk.start)))?;
                    self.source.read_exact(&mut body[from..])?;
                }
                Data::Fill(value) => {
                    chunk_head(&mut body, FILL, blocks, FILL_BYTES);
                    body.extend(value);
                }
                Data::Skip => chunk_head(&mut body, SKIP, blocks, 0),
            }
            count += 1;
        }
        if part.to < self.size {
            chunk_head(&mut body, SKIP, (self.size - part.to) / self.block, 0);
            count += 1;
        }
        let mut image = header(self.block, self.size / self.block, count);
        image.extend(body);
        Ok(image)
    }
}

impl<R: Read + Seek> ReadAt for Sparse<R> {
    fn read_at(&mut self, mut offset: u64, mut out: &mut [u8]) -> io::Result<()> {
        while !out.is_empty() {
            let index = self.chunks.partition_point(|chunk| chunk.start + chunk.bytes <= offset);
            let chunk = *self.chunks.get(index).ok_or_else(|| invalid("a read past the end of a sparse image"))?;
            let within = offset - chunk.start;
            let take = (chunk.bytes - within).min(out.len() as u64) as usize;
            let (now, rest) = out.split_at_mut(take);
            match chunk.data {
                Data::Raw(at) => {
                    self.source.seek(SeekFrom::Start(at + within))?;
                    self.source.read_exact(now)?;
                }
                Data::Fill(value) => now
                    .iter_mut()
                    .enumerate()
                    .for_each(|(place, byte)| *byte = value[(within as usize + place) % value.len()]),
                Data::Skip => now.fill(0),
            }
            (offset, out) = (offset + take as u64, rest);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const BLOCK: u32 = 8;

    fn chunk(kind: u16, blocks: u32, body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        chunk_head(&mut bytes, kind, blocks.into(), body.len() as u64);
        bytes.extend(body);
        bytes
    }

    // A sparse image of `blocks` blocks made of those chunks.
    fn image(blocks: u32, chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = header(BLOCK.into(), blocks.into(), chunks.len() as u32);
        chunks.iter().for_each(|chunk| bytes.extend(chunk));
        bytes
    }

    #[test]
    fn read_through_chunks() {
        let raw: Vec<u8> = (1..=16).collect();
        let chunks = [
            chunk(RAW, 2, &raw),
            chunk(FILL, 1, &[0xA, 0xB, 0xC, 0xD]),
            chunk(CHECKSUM, 0, &[0; 4]),
            chunk(SKIP, 3, &[]),
            chunk(RAW, 1, &[9; 8]),
        ];
        let mut sparse = Sparse::open(Cursor::new(image(7, &chunks))).unwrap();
        let mut whole = [0xFFu8; 56];
        sparse.read_at(0, &mut whole).unwrap();
        let mut expected = raw.clone();
        expected.extend([0xA, 0xB, 0xC, 0xD, 0xA, 0xB, 0xC, 0xD]);
        expected.extend([0; 24]);
        expected.extend([9; 8]);
        assert_eq!(&whole[..], &expected[..]);
        let mut part = [0u8; 9];
        sparse.read_at(14, &mut part).unwrap();
        assert_eq!(
            part,
            [15, 16, 0xA, 0xB, 0xC, 0xD, 0xA, 0xB, 0xC],
            "across the border of two chunks, and from the middle of a filled one"
        );
        sparse.read_at(22, &mut part[..3]).unwrap();
        assert_eq!(part[..3], [0xC, 0xD, 0], "a fill keeps its beat wherever a read begins");
        assert!(sparse.read_at(50, &mut part).is_err(), "nothing lies past the end");
    }

    // What writing a sparse image does to a disk: its raw and filled blocks written, its skipped ones left as they were.
    fn written(image: &[u8], disk: &mut [u8]) {
        let mut sparse = Sparse::open(Cursor::new(image)).unwrap();
        for chunk in sparse.chunks.clone().iter().filter(|chunk| !matches!(chunk.data, Data::Skip)) {
            let (start, end) = (chunk.start as usize, (chunk.start + chunk.bytes) as usize);
            sparse.read_at(chunk.start, &mut disk[start..end]).unwrap();
        }
    }

    #[test]
    fn parts_within_limit() {
        let raw: Vec<u8> = (1..=40).collect();
        let chunks = [
            chunk(SKIP, 2, &[]),
            chunk(RAW, 5, &raw),
            chunk(FILL, 3, &[0xA, 0xB, 0xC, 0xD]),
            chunk(CHECKSUM, 0, &[0; 4]),
            chunk(RAW, 1, &[9; 8]),
            chunk(SKIP, 4, &[]),
        ];
        let whole = image(15, &chunks);
        let mut sparse = Sparse::open(Cursor::new(whole.clone())).unwrap();
        // Room beside the header and the two skips for one raw chunk of two blocks.
        let limit = HEADER_BYTES + 3 * CHUNK_HEADER_BYTES + 2 * BLOCK as usize;
        let parts = sparse.parts(limit).unwrap();
        assert_eq!(parts.len(), 5, "the five raw blocks in three runs, the fill, the last raw block");
        let (mut expected, mut disk) = (vec![0xEE; 120], vec![0xEE; 120]);
        written(&whole, &mut expected);
        for part in parts {
            let bytes = sparse.part(part).unwrap();
            assert!(bytes.len() <= limit, "{} bytes", bytes.len());
            assert_eq!(
                Sparse::open(Cursor::new(&bytes[..])).unwrap().size(),
                120,
                "each part stands for the whole image"
            );
            written(&bytes, &mut disk);
        }
        assert_eq!(disk, expected, "skipped blocks keep what the disk held");
        assert_eq!(sparse.parts(whole.len()).unwrap().len(), 1, "an image that fits is one part");
        let below_a_block = HEADER_BYTES + 3 * CHUNK_HEADER_BYTES + BLOCK as usize - 1;
        assert!(sparse.parts(below_a_block).is_err(), "a limit that cannot take one raw block");
        let skips = image(4, &[chunk(SKIP, 4, &[])]);
        assert!(Sparse::open(Cursor::new(skips)).unwrap().parts(limit).unwrap().is_empty(), "nothing to write");
    }

    // The runs a sparse image writes, those that touch made one.
    fn runs(sparse: &Sparse<impl Read + Seek>) -> Vec<(u64, u64)> {
        let mut runs: Vec<(u64, u64)> = Vec::new();
        for chunk in sparse.chunks.iter().filter(|chunk| !matches!(chunk.data, Data::Skip)) {
            match runs.last_mut() {
                Some((_, end)) if *end == chunk.start => *end += chunk.bytes,
                _ => runs.push((chunk.start, chunk.start + chunk.bytes)),
            }
        }
        runs
    }

    #[test]
    #[ignore = "needs the stock firmware under stock/"]
    fn stock_super_parts() {
        const LIMIT: usize = 0x800_0000;
        let mut whole = Sparse::open(std::fs::File::open(crate::stock("super.img")).unwrap()).unwrap();
        let mut written = Vec::new();
        for part in whole.parts(LIMIT).unwrap() {
            let bytes = whole.part(part).unwrap();
            assert!(bytes.len() <= LIMIT, "{} bytes", bytes.len());
            let mut piece = Sparse::open(Cursor::new(bytes)).unwrap();
            assert_eq!(piece.size(), whole.size(), "each part stands for the whole image");
            for (start, end) in runs(&piece) {
                let (mut theirs, mut ours) = (vec![0; (end - start) as usize], vec![0; (end - start) as usize]);
                piece.read_at(start, &mut ours).unwrap();
                whole.read_at(start, &mut theirs).unwrap();
                assert!(ours == theirs, "the bytes at {start:#x}");
                written.push((start, end));
            }
        }
        written.dedup_by(|next, last| {
            let touches = last.1 == next.0;
            if touches {
                last.1 = next.1;
            }
            touches
        });
        assert_eq!(written, runs(&whole), "the parts write every run of the image, once");
    }

    #[test]
    fn foreign_sparse_refused() {
        assert!(Sparse::open(Cursor::new(vec![0u8; 64])).is_err());
        assert!(Sparse::open(Cursor::new(vec![0u8; 3])).is_err());
        assert!(Sparse::open(Cursor::new(image(3, &[chunk(RAW, 2, &[0; 16])]))).is_err(), "chunks that do not add up");
        assert!(Sparse::open(Cursor::new(image(1, &[chunk(0xCAC9, 1, &[])]))).is_err(), "a chunk of no known kind");
        let mut cut = image(2, &[chunk(RAW, 2, &[7; 16])]);
        cut.truncate(cut.len() - 4);
        let mut sparse = Sparse::open(Cursor::new(cut)).unwrap();
        assert!(sparse.read_at(0, &mut [0u8; 16]).is_err(), "a file that ends before its last chunk does");
        let mut huge = image(0, &[chunk(SKIP, u32::MAX, &[]), chunk(SKIP, u32::MAX, &[])]);
        huge[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Sparse::open(Cursor::new(huge)).is_err(), "chunks that add up past any file");
    }
}

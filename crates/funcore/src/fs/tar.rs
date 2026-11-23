//! POSIX ustar tape archives: the header and padding of a file, and the files of a tape.

use crate::base::bytes::until_zero;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

/// The bytes of a block of a tape.
pub const BLOCK: usize = 512;
/// The two empty blocks that end a tape.
pub const END: [u8; 2 * BLOCK] = [0; 2 * BLOCK];
const NAME: (usize, usize) = (0, 100);
const MODE: (usize, usize) = (100, 8);
const OWNER: (usize, usize) = (108, 8);
const GROUP: (usize, usize) = (116, 8);
const SIZE: (usize, usize) = (124, 12);
const MODIFIED: (usize, usize) = (136, 12);
const SUM: (usize, usize) = (148, 8);
const KIND: usize = 156;
const MAGIC: usize = 257;
const FILE: u8 = b'0';
const USTAR: &[u8] = b"ustar\x0000";
const FILE_MODE: u64 = 0o644;

fn write_octal(field: &mut [u8], value: u64) -> bool {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    if text.len() != digits {
        return false;
    }
    field[..digits].copy_from_slice(text.as_bytes());
    field[digits] = 0;
    true
}

/// The header block of a file, `modified` in seconds since 1970; none if a field does not fit.
pub fn header(name: &str, size: u64, modified: u64) -> Option<[u8; BLOCK]> {
    let mut block = [0u8; BLOCK];
    let field = |span: (usize, usize)| span.0..span.0 + span.1;
    if name.is_empty() || name.len() > NAME.1 {
        return None;
    }
    block[..name.len()].copy_from_slice(name.as_bytes());
    let has_room = write_octal(&mut block[field(MODE)], FILE_MODE)
        && write_octal(&mut block[field(OWNER)], 0)
        && write_octal(&mut block[field(GROUP)], 0)
        && write_octal(&mut block[field(SIZE)], size)
        && write_octal(&mut block[field(MODIFIED)], modified);
    if !has_room {
        return None;
    }
    block[KIND] = FILE;
    block[MAGIC..MAGIC + USTAR.len()].copy_from_slice(USTAR);
    // The sum counts its own field as blanks, and is written as six digits, a zero byte and a blank.
    block[field(SUM)].fill(b' ');
    let sum: u64 = block.iter().map(|byte| u64::from(*byte)).sum();
    write_octal(&mut block[SUM.0..SUM.0 + SUM.1 - 1], sum).then_some(block)
}

/// The zero bytes that fill the last block of a file of `size` bytes.
pub fn padding(size: u64) -> usize {
    (BLOCK - (size % BLOCK as u64) as usize) % BLOCK
}

/// The error text of a file `header` has no room for.
pub fn to_no_room_text(name: &str) -> String {
    format!("{name}: a tape archive has no room for it")
}

/// A file of a tape: `modified` in seconds since 1970, `at` the place of its bytes in the tape.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct File {
    pub name: String,
    pub modified: u64,
    pub at: Range<usize>,
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let digits = core::str::from_utf8(field).ok()?.trim_matches(|padding: char| padding == '\0' || padding == ' ');
    u64::from_str_radix(digits, 8).ok()
}

/// What a header block of a tape tells: a file's name, its bytes and `modified` in seconds since 1970, or the tape's end.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Head {
    File { name: String, size: u64, modified: u64 },
    End,
}

/// A header block read; none for a bad sum or a file of another kind.
pub fn parse_head(block: &[u8; BLOCK]) -> Option<Head> {
    if block.iter().all(|byte| *byte == 0) {
        return Some(Head::End);
    }
    let field = |span: (usize, usize)| &block[span.0..span.0 + span.1];
    let sum_field = SUM.0..SUM.0 + SUM.1;
    let sum: u64 = block
        .iter()
        .enumerate()
        .map(|(index, byte)| if sum_field.contains(&index) { u64::from(b' ') } else { u64::from(*byte) })
        .sum();
    if parse_octal(field(SUM))? != sum || block[KIND] != FILE {
        return None;
    }
    let name = String::from(core::str::from_utf8(until_zero(field(NAME))).ok()?);
    Some(Head::File { name, size: parse_octal(field(SIZE))?, modified: parse_octal(field(MODIFIED))? })
}

/// The files of a tape; none for a bad sum, a file of another kind, or a tape cut short.
pub fn parse_files(tape: &[u8]) -> Option<Vec<File>> {
    let (mut files, mut at) = (Vec::new(), 0);
    loop {
        let Head::File { name, size, modified } = parse_head(tape.get(at..at + BLOCK)?.try_into().ok()?)? else {
            return Some(files);
        };
        let size = usize::try_from(size).ok()?;
        let start = at + BLOCK;
        let end = start.checked_add(size).filter(|end| *end <= tape.len())?;
        files.push(File { name, modified, at: start..end });
        at = end + padding(size as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_fields() {
        let block = header("20261011/log", 550_616, 1_791_697_759).unwrap();
        assert_eq!(&block[..13], b"20261011/log\0");
        assert_eq!(&block[100..108], b"0000644\0");
        assert_eq!((&block[108..116], &block[116..124]), (&b"0000000\0"[..], &b"0000000\0"[..]));
        assert_eq!(&block[124..136], b"00002063330\0");
        assert_eq!(&block[136..148], b"15262621537\0");
        assert_eq!((block[156], &block[257..265]), (b'0', &b"ustar\x0000"[..]));
        let sum: u64 = block
            .iter()
            .enumerate()
            .map(|(index, byte)| if (148..156).contains(&index) { 32 } else { u64::from(*byte) })
            .sum();
        assert_eq!(&block[148..156], format!("{sum:06o}\0 ").as_bytes(), "the sum is taken with its own field blank");
        assert!(block[265..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn oversize_fields() {
        let longest = "n".repeat(100);
        assert!(header(&longest, 0, 0).is_some_and(|block| block[..100] == *longest.as_bytes() && block[100] == b'0'));
        assert!(header(&"n".repeat(101), 0, 0).is_none() && header("", 0, 0).is_none());
        assert!(header("log", 0o77_777_777_777, 0).is_some() && header("log", 0o100_000_000_000, 0).is_none());
        assert!(header("log", 0, u64::MAX).is_none());
        assert_eq!(to_no_room_text("20261011/log"), "20261011/log: a tape archive has no room for it");
        assert_eq!([0, 1, 511, 512, 513, 550_616].map(padding), [0, 511, 1, 0, 511, 296]);
        assert_eq!((END.len(), END.iter().all(|byte| *byte == 0)), (1024, true));
    }

    #[test]
    fn tape_round_trip() {
        let mut tape = Vec::new();
        for (name, content, modified) in [("20261011/log", &b"a line\n"[..], 1_791_697_759), ("20261011/power", b"", 7)]
        {
            tape.extend_from_slice(&header(name, content.len() as u64, modified).unwrap());
            tape.extend_from_slice(content);
            tape.extend(core::iter::repeat_n(0, padding(content.len() as u64)));
        }
        tape.extend_from_slice(&END);
        let files = parse_files(&tape).unwrap();
        assert_eq!(
            files.iter().map(|file| (file.name.as_str(), &tape[file.at.clone()], file.modified)).collect::<Vec<_>>(),
            [("20261011/log", &b"a line\n"[..], 1_791_697_759), ("20261011/power", b"", 7)],
            "each file by its name, its bytes and its time"
        );
        assert_eq!(parse_files(&END), Some(Vec::new()), "a tape of nothing");
        let head = header("20261011/log", 7, 1_791_697_759).unwrap();
        assert_eq!(
            (parse_head(&head), parse_head(&[0; BLOCK])),
            (Some(Head::File { name: "20261011/log".into(), size: 7, modified: 1_791_697_759 }), Some(Head::End)),
            "a block by itself, as a tape read block by block has it"
        );
        let mut corrupt = tape.clone();
        corrupt[3] = b'x';
        assert_eq!(parse_files(&corrupt), None, "a head whose sum does not hold");
        assert_eq!(parse_files(&tape[..BLOCK + 3]), None, "a file cut short");
        assert_eq!(parse_files(&tape[..2 * BLOCK]), None, "a tape without its end");
        let mut folder = header("20261011", 0, 0).unwrap();
        folder[156] = b'5';
        folder[148..156].fill(b' ');
        let sum: u64 = folder.iter().map(|byte| u64::from(*byte)).sum();
        folder[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        assert_eq!(parse_files(&[&folder[..], &END].concat()), None, "a folder, whose sum holds, is no file");
    }
}

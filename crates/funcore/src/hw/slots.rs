//! The A/B slot control block (AOSP `bootloader_control`) the bootloader keeps in its misc partition, and the update marker beside it.

use crate::base::bytes::le32;
use crate::base::checksum::crc32;
use alloc::format;
use alloc::string::String;

/// The bootloader's misc partition by its name in the partition table.
pub const MISC_PARTITION: &str = "para";
/// Bytes to the control block in the misc partition.
pub const CONTROL_OFFSET: u64 = 0x800;
/// Bytes to the word the bootloader reads before it shows a saved dm-verity warning.
pub const MARKER_OFFSET: usize = 0x1000;
/// A value of that word: any nonzero word reads as a finished update.
pub const MARKER: u32 = 1;
pub const CONTROL_SIZE: usize = 32;
const MAGIC: u32 = 0x4241_4342;
const MAGIC_AT: usize = 4;
const SLOT_COUNT_AT: usize = 9;
const SLOT_COUNT_MASK: u8 = 0b111;
const SLOT_INFO_AT: usize = 12;
const SLOT_INFO_SIZE: usize = 2;
const CRC_AT: usize = 28;
const PRIORITY_MASK: u8 = 0x0f;
const TRIES_SHIFT: u8 = 4;
const SUCCESSFUL: u8 = 0x80;
const MARKED_TRIES: u8 = 1;
const SUFFIXES: [&str; 4] = ["_a", "_b", "_c", "_d"];
const SUFFIX_KEY: &str = "androidboot.slot_suffix=";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    Clean,
    Dirty,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ControlError {
    BadMagic,
    BadChecksum,
    NoSuchSlot,
}

impl ControlError {
    pub const fn message(self) -> &'static str {
        match self {
            ControlError::BadMagic => "no slot control block",
            ControlError::BadChecksum => "slot control block checksum mismatch",
            ControlError::NoSuchSlot => "slot not in the control block",
        }
    }
}

/// The error text of a command line without a slot suffix.
pub const NO_SUFFIX: &str = "no slot suffix on the command line";
/// The log line of a slot marked now.
pub const MARKED_LINE: &str = "boot slot marked successful";
const WAITING_LINE: &str = "boot slot: waiting for the partition with the slot control block";

/// Whether the booted slot is marked.
pub struct Marking {
    is_done: bool,
}

impl Marking {
    /// A marking after the first try.
    pub const fn with(is_done: bool) -> Marking {
        Marking { is_done }
    }

    /// True where a block device came and the slot is not marked.
    pub const fn is_due(&self, has_device: bool) -> bool {
        has_device && !self.is_done
    }

    pub const fn note_try(&mut self, is_done: bool) {
        self.is_done = is_done;
    }

    /// The log line while the slot waits.
    pub const fn to_waiting_line(&self) -> Option<&'static str> {
        if self.is_done { None } else { Some(WAITING_LINE) }
    }
}

/// The error text of a suffix no slot has.
pub fn to_unknown_text(suffix: &str) -> String {
    format!("unknown slot suffix {suffix}")
}

/// The booted slot's suffix from the kernel command line.
pub fn suffix(cmdline: &str) -> Option<&str> {
    cmdline.split_whitespace().find_map(|word| word.strip_prefix(SUFFIX_KEY))
}

/// The slot number of a suffix such as `_b`.
pub fn index(suffix: &str) -> Option<usize> {
    SUFFIXES.iter().position(|entry| *entry == suffix)
}

/// Marks `slot` successful, keeping its priority.
pub fn mark_successful(block: &mut [u8; CONTROL_SIZE], slot: usize) -> Result<Mark, ControlError> {
    if le32(block, MAGIC_AT) != Some(MAGIC) {
        return Err(ControlError::BadMagic);
    }
    if le32(block, CRC_AT) != Some(crc32(&block[..CRC_AT])) {
        return Err(ControlError::BadChecksum);
    }
    if slot >= (block[SLOT_COUNT_AT] & SLOT_COUNT_MASK) as usize {
        return Err(ControlError::NoSuchSlot);
    }
    let info = &mut block[SLOT_INFO_AT + slot * SLOT_INFO_SIZE];
    if *info & SUCCESSFUL != 0 {
        return Ok(Mark::Clean);
    }
    *info = (*info & PRIORITY_MASK) | (MARKED_TRIES << TRIES_SHIFT) | SUCCESSFUL;
    let crc = crc32(&block[..CRC_AT]);
    block[CRC_AT..].copy_from_slice(&crc.to_le_bytes());
    Ok(Mark::Dirty)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOCK: [u8; CONTROL_SIZE] = [
        b'_', b'b', 0, 0, b'B', b'C', b'A', b'B', 1, 2, 0, 0, 0, 0, 0x9f, 0, //
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x0c, 0x76, 0xa9, 0xdf,
    ];

    fn with_slot_b(info: u8) -> [u8; CONTROL_SIZE] {
        let mut block = STOCK;
        block[SLOT_INFO_AT + SLOT_INFO_SIZE] = info;
        let crc = crc32(&block[..CRC_AT]);
        block[CRC_AT..].copy_from_slice(&crc.to_le_bytes());
        block
    }

    #[test]
    fn already_successful() {
        let mut block = STOCK;
        assert_eq!(mark_successful(&mut block, 1), Ok(Mark::Clean));
        assert_eq!(block, STOCK);
    }

    #[test]
    fn marks_as_android() {
        let mut block = with_slot_b(0x5f);
        assert_eq!(mark_successful(&mut block, 1), Ok(Mark::Dirty));
        assert_eq!(block, STOCK);
    }

    #[test]
    fn refusals() {
        let mut blank = [0u8; CONTROL_SIZE];
        assert_eq!(mark_successful(&mut blank, 1), Err(ControlError::BadMagic));
        let mut corrupt = with_slot_b(0x5f);
        corrupt[SLOT_INFO_AT] = 1;
        assert_eq!(mark_successful(&mut corrupt, 1), Err(ControlError::BadChecksum));
        let mut block = STOCK;
        assert_eq!(mark_successful(&mut block, 2), Err(ControlError::NoSuchSlot));
        assert_eq!(
            [ControlError::BadMagic, ControlError::BadChecksum, ControlError::NoSuchSlot].map(ControlError::message),
            ["no slot control block", "slot control block checksum mismatch", "slot not in the control block"]
        );
    }

    #[test]
    fn suffix_index() {
        assert_eq!(index("_a"), Some(0));
        assert_eq!(index("_b"), Some(1));
        assert_eq!(index("b"), None);
        assert_eq!(to_unknown_text("b"), "unknown slot suffix b");
    }

    #[test]
    fn marking_tries() {
        let mut marking = Marking::with(false);
        assert_eq!(marking.to_waiting_line(), Some(WAITING_LINE));
        assert!(marking.is_due(true) && !marking.is_due(false));
        marking.note_try(false);
        assert!(marking.is_due(true));
        marking.note_try(true);
        assert_eq!((marking.is_due(true), marking.to_waiting_line()), (false, None));
    }

    #[test]
    fn suffix_from_cmdline() {
        assert_eq!(suffix("root=/dev/ram androidboot.slot_suffix=_b androidboot.slot=b"), Some("_b"));
        assert_eq!(suffix("root=/dev/ram"), None);
    }
}

//! The table MediaTek's bootloader writes at the head of the `expdb` partition after a dead boot.

use crate::base::bytes::le32;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The name of the partition the bootloader saves to.
pub const PARTITION: &str = "expdb";
const MAGIC: u32 = 0xaee0_dead;
const LENGTH_AT: usize = 8;
const FIRST: usize = 0x30;
const ENTRY: usize = 0x40;
const OFFSET_AT: usize = 8;
const SIZE_AT: usize = 12;
const NAME_AT: usize = 0x20;
/// The bytes `table_length` reads.
pub const HEAD: usize = LENGTH_AT + 4;

/// A part of a saved boot: its offset and size on the partition, in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Part {
    offset: u64,
    size: u64,
}

/// The length of the table at the partition's start; none if it holds no table.
pub fn table_length(head: &[u8]) -> Option<usize> {
    let length = le32(head, LENGTH_AT)? as usize;
    (le32(head, 0)? == MAGIC && length >= FIRST).then_some(length)
}

/// The parts of a whole table, in its order, up to the first entry without a name.
pub fn parts(table: &[u8]) -> Vec<Part> {
    let mut parts = Vec::new();
    for entry in table.get(FIRST..).unwrap_or_default().as_chunks::<ENTRY>().0 {
        if entry[NAME_AT] == 0 {
            break;
        }
        let (Some(offset), Some(size)) = (le32(entry, OFFSET_AT), le32(entry, SIZE_AT)) else {
            break;
        };
        parts.push(Part { offset: offset.into(), size: size.into() });
    }
    parts
}

/// A table that does not fit a partition of `room` bytes, itself or with the parts it names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overflow {
    Table { length: usize, room: u64 },
    Parts { extent: u64, room: u64 },
}

impl Overflow {
    /// The error text of the overflow.
    pub fn message(self) -> String {
        match self {
            Overflow::Table { length, room } => format!("its table is {length} bytes long, on a partition of {room}"),
            Overflow::Parts { extent, room } => format!("its table names {extent} bytes of a partition of {room}"),
        }
    }
}

/// A table's length, if a partition of `room` bytes holds it.
pub const fn check_table(length: usize, room: u64) -> Result<usize, Overflow> {
    if length as u64 > room { Err(Overflow::Table { length, room }) } else { Ok(length) }
}

/// The bytes from the partition's start that hold a table and its parts, if `room` holds them.
pub fn check_parts(length: usize, parts: &[Part], room: u64) -> Result<u64, Overflow> {
    let extent = parts.iter().map(|part| part.offset + part.size).fold(length as u64, u64::max);
    if extent > room { Err(Overflow::Parts { extent, room }) } else { Ok(extent) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn table(parts: &[(&str, u32, u32)]) -> Vec<u8> {
        let mut table = vec![0u8; 0x1030];
        table[..4].copy_from_slice(&MAGIC.to_le_bytes());
        table[4..8].copy_from_slice(&0x11u32.to_le_bytes());
        table[LENGTH_AT..HEAD].copy_from_slice(&0x1030u32.to_le_bytes());
        table[0x14..0x18].copy_from_slice(&0x200u32.to_le_bytes());
        for (index, (name, offset, size)) in parts.iter().enumerate() {
            let entry = &mut table[FIRST + index * ENTRY..][..ENTRY];
            entry[4..8].copy_from_slice(&1u32.to_le_bytes());
            for (at, value) in [(OFFSET_AT, *offset), (SIZE_AT, *size), (SIZE_AT + 4, *size)] {
                entry[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            entry[NAME_AT..NAME_AT + name.len()].copy_from_slice(name.as_bytes());
        }
        table
    }

    #[test]
    fn saved_boot() {
        let saved = table(&[
            ("SYS_RAMCONSOLE_RAW", 0x1200, 0x540),
            ("SYS_PSTORE_RAW", 0x1740, 0xe0000),
            ("KEDUMP_CRC", 0xe1740, 8),
            ("SYS_LOG_DUR_LKDUMP", 0x1b_a4fa, 0xc209),
        ]);
        assert_eq!(table_length(&saved[..HEAD]), Some(0x1030));
        let parts = parts(&saved);
        assert_eq!(
            parts.iter().map(|part| (part.offset, part.size)).collect::<Vec<_>>(),
            [(0x1200, 0x540), (0x1740, 0xe0000), (0xe1740, 8), (0x1b_a4fa, 0xc209)],
            "every named entry, and the one without a name ends them"
        );
        assert_eq!(check_parts(0x1030, &[], 0x1030), Ok(0x1030), "a table that names nothing is its own length");
        assert_eq!(
            (check_table(0x1030, 0x4000), check_table(0x1030, 0x1030), check_table(0x1031, 0x1030)),
            (Ok(0x1030), Ok(0x1030), Err(Overflow::Table { length: 0x1031, room: 0x1030 })),
            "a table is read only where the partition holds it whole"
        );
        assert_eq!(
            (check_parts(0x1030, &parts, 0x1c_6703), check_parts(0x1030, &parts, 0x1c_6702)),
            (Ok(0x1c_6703), Err(Overflow::Parts { extent: 0x1c_6703, room: 0x1c_6702 })),
            "and what it names, up to the end of the dump's log, only where the partition holds that"
        );
        assert_eq!(
            [Overflow::Table { length: 4, room: 2 }.message(), Overflow::Parts { extent: 4, room: 2 }.message()],
            ["its table is 4 bytes long, on a partition of 2", "its table names 4 bytes of a partition of 2"]
        );
    }

    #[test]
    fn no_saved_boot() {
        assert_eq!(table_length(&[0; HEAD]), None, "what an R1 that never crashed holds");
        assert_eq!(table_length(&[0xff; HEAD]), None);
        assert_eq!(table_length(&table(&[])[..HEAD - 1]), None, "too short to tell");
        let mut short = table(&[]);
        short[LENGTH_AT..HEAD].copy_from_slice(&0x10u32.to_le_bytes());
        assert_eq!(table_length(&short), None, "a table shorter than its head");
        assert_eq!(parts(&table(&[])), Vec::new());
        assert_eq!(parts(&[0; 0x20]), Vec::new());
    }
}

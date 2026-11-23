//! The table of device trees that a boot image's dtb section and the dtbo partition hold, as Android lays it out and LK checks it.

use crate::{be32, invalid};
use std::io;

const MAGIC: u32 = 0xd7b7_ab1e;
// The sizes of the header and of an entry, which LK refuses to boot without.
const HEADER_BYTES: u32 = 32;
const ENTRY_BYTES: u32 = 32;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub tree: Vec<u8>,
    id: u32,
    rev: u32,
    custom: [u32; 4],
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Table {
    page_size: u32,
    version: u32,
    pub entries: Vec<Entry>,
}

impl Table {
    /// Reads a table from the start of `bytes`; what lies past its total size, as the zeros of a partition, is no part of it.
    pub fn parse(bytes: &[u8]) -> io::Result<Table> {
        let field = |index: usize| be32(bytes, 4 * index);
        if field(0)? != MAGIC {
            return Err(invalid("no table of device trees"));
        }
        if field(2)? != HEADER_BYTES || field(3)? != ENTRY_BYTES {
            return Err(invalid("a table of device trees with a header or entries of another size"));
        }
        let total = field(1)? as usize;
        let bytes = bytes.get(..total).ok_or_else(|| invalid("a table of device trees shorter than it says"))?;
        let (count, entries_at) = (field(4)? as usize, field(5)? as usize);
        let entries = (0..count)
            .map(|index| {
                let at = entries_at + index * ENTRY_BYTES as usize;
                let (size, offset) = (be32(bytes, at)? as usize, be32(bytes, at + 4)? as usize);
                let tree =
                    bytes.get(offset..offset + size).ok_or_else(|| invalid("a device tree past the table's end"))?;
                let custom =
                    [be32(bytes, at + 16)?, be32(bytes, at + 20)?, be32(bytes, at + 24)?, be32(bytes, at + 28)?];
                Ok(Entry { tree: tree.to_vec(), id: be32(bytes, at + 8)?, rev: be32(bytes, at + 12)?, custom })
            })
            .collect::<io::Result<Vec<Entry>>>()?;
        Ok(Table { page_size: field(6)?, version: field(7)?, entries })
    }

    /// The header, the entries right after it, and the trees one after the other behind them.
    pub fn to_bytes(&self) -> Vec<u8> {
        let entries_at = HEADER_BYTES;
        let mut offset = entries_at + ENTRY_BYTES * self.entries.len() as u32;
        let total = offset + self.entries.iter().map(|entry| entry.tree.len() as u32).sum::<u32>();
        let mut out = Vec::with_capacity(total as usize);
        for value in [
            MAGIC,
            total,
            HEADER_BYTES,
            ENTRY_BYTES,
            self.entries.len() as u32,
            entries_at,
            self.page_size,
            self.version,
        ] {
            out.extend(value.to_be_bytes());
        }
        for entry in &self.entries {
            for value in [entry.tree.len() as u32, offset, entry.id, entry.rev].into_iter().chain(entry.custom) {
                out.extend(value.to_be_bytes());
            }
            offset += entry.tree.len() as u32;
        }
        for entry in &self.entries {
            out.extend(&entry.tree);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Table {
        let entry = |byte: u8, length: usize| Entry { tree: vec![byte; length], id: 0, rev: 0, custom: [0; 4] };
        Table { page_size: 2048, version: 0, entries: vec![entry(1, 100), entry(2, 37)] }
    }

    #[test]
    fn table_round_trip() {
        let table = sample();
        let bytes = table.to_bytes();
        assert_eq!(bytes.len(), 32 + 2 * 32 + 137);
        assert_eq!(be32(&bytes, 32 + 4).unwrap(), 96, "the first tree right behind the entries");
        assert_eq!(be32(&bytes, 64 + 4).unwrap(), 196);
        let mut padded = bytes.clone();
        padded.resize(4096, 0);
        assert_eq!(Table::parse(&padded).unwrap(), table, "the zeros of a partition behind it are no part of it");
    }

    #[test]
    fn foreign_table_refused() {
        assert!(Table::parse(&[0u8; 64]).is_err());
        let mut bytes = sample().to_bytes();
        bytes[11] = 40;
        assert!(Table::parse(&bytes).is_err(), "a header of another size");
        let bytes = sample().to_bytes();
        assert!(Table::parse(&bytes[..bytes.len() - 1]).is_err(), "a table cut short");
    }

    #[test]
    #[cfg(feature = "boot")]
    #[ignore = "needs the stock firmware under stock/"]
    fn stock_trees_round_trip() {
        use crate::fdt::Tree;
        let stock = |name: &str| std::fs::read(crate::stock(name)).unwrap();
        let boot = crate::bootimg::BootImage::parse(&stock("boot.img")).unwrap();
        let overlay = stock("dtbo.img");
        for bytes in [&boot.dtb[..], &overlay[..]] {
            let table = Table::parse(bytes).unwrap();
            assert_eq!(table.entries.len(), 1);
            let tree = Tree::parse(&table.entries[0].tree).unwrap();
            assert!(tree.to_bytes() == table.entries[0].tree, "the tree");
            let written = table.to_bytes();
            assert!(written[..] == bytes[..written.len()], "the table");
        }
    }
}

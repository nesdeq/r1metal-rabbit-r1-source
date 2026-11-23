//! Container formats of the R1's boot chain (boot image v2, newc cpio, legacy LZ4, vbmeta, device trees and their table) and readers for Rabbit's firmware (sparse image, super partition table, ext4).

#[cfg(feature = "boot")]
pub mod bootimg;
pub mod cpio;
pub mod dt_table;
pub mod ext4;
pub mod fdt;
pub mod lp;
#[cfg(feature = "boot")]
pub mod lz4_legacy;
pub mod sparse;
pub mod vbmeta;

use std::io;

/// Where the workspace keeps the release of Rabbit's firmware these readers are written for, as `setupcodebase.sh` unpacks it.
pub const STOCK: &str = "stock/v0.8.293/rabbit_OS_v0.8.293_20250516110545";

#[cfg(test)]
fn stock(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(STOCK).join(name)
}

// What of the release a hand unpacked beside it, for the tests that hold a reader to it.
#[cfg(test)]
fn unpacked(path: &str) -> std::path::PathBuf {
    stock("../unpacked").join(path)
}

/// Something bytes can be read from at any place.
pub trait ReadAt {
    // Fills `out` with the bytes from `offset` on; what does not hold that many is an error.
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> io::Result<()>;
}

#[cfg(test)]
impl ReadAt for &[u8] {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        let from = usize::try_from(offset).ok().and_then(|start| self.get(start..)?.get(..out.len()));
        out.copy_from_slice(from.ok_or_else(|| invalid("a read past the end"))?);
        Ok(())
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

// For a header or an entry whose length its reader has checked: a place past it is the reader's fault, and panics.
fn half(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

// For what a file gave at the length it says of itself: a place past its end is an error.
fn field<const N: usize>(bytes: &[u8], at: usize) -> io::Result<[u8; N]> {
    let field = bytes.get(at..).and_then(<[u8]>::first_chunk);
    field.copied().ok_or_else(|| invalid("a field past the end"))
}

fn le32(bytes: &[u8], at: usize) -> io::Result<u32> {
    field(bytes, at).map(u32::from_le_bytes)
}

fn le64(bytes: &[u8], at: usize) -> io::Result<u64> {
    field(bytes, at).map(u64::from_le_bytes)
}

fn be32(bytes: &[u8], at: usize) -> io::Result<u32> {
    field(bytes, at).map(u32::from_be_bytes)
}

fn be64(bytes: &[u8], at: usize) -> io::Result<u64> {
    field(bytes, at).map(u64::from_be_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn super_partition_file_read() {
        let files = ext4::tests::image();
        let mut device = lp::tests::image(128 * 1024, &[("vendor_a", &[(64, Some(64)), (64, Some(160))])]);
        device[64 * 512..128 * 512].copy_from_slice(&files[..64 * 512]);
        device[160 * 512..224 * 512].copy_from_slice(&files[64 * 512..]);
        let mut device = &device[..];
        let table = lp::read(&mut device).unwrap();
        let mut vendor = ext4::Ext4::open(table.partition("vendor_a", &mut device).unwrap()).unwrap();
        assert_eq!(vendor.read("notes").unwrap(), b"hey");
        assert_eq!(vendor.read("firmware/a.bin").unwrap().len(), 1500);
    }

    #[test]
    #[ignore = "needs the stock firmware under stock/ and its vendor firmware unpacked beside it into unpacked/vendor/firmware"]
    fn stock_vendor_files() {
        let mut image = sparse::Sparse::open(File::open(stock("super.img")).unwrap()).unwrap();
        let table = lp::read(&mut image).unwrap();
        let mut vendor = ext4::Ext4::open(table.partition("vendor_a", &mut image).unwrap()).unwrap();
        let unpacked = unpacked("vendor/firmware");
        let mut names: Vec<String> = vendor.list("firmware").unwrap().into_iter().map(|(name, _)| name).collect();
        names.sort();
        let mut expected: Vec<String> = std::fs::read_dir(&unpacked)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        expected.sort();
        assert_eq!(names, expected);
        for name in names {
            assert!(
                vendor.read(&format!("firmware/{name}")).unwrap() == std::fs::read(unpacked.join(&name)).unwrap(),
                "{name}"
            );
        }
    }
}

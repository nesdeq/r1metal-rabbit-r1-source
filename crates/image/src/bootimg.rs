//! Android boot image, header version 2, as the R1's LK loads it.

use crate::{invalid, word};
use sha1::{Digest, Sha1};
use std::io;

const MAGIC: &[u8; 8] = b"ANDROID!";
const HEADER_VERSION: u32 = 2;
const HEADER_SIZE: usize = 1660;
const NAME_LEN: usize = 16;
const CMDLINE_LEN: usize = 512;
const ID_LEN: usize = 32;
const EXTRA_CMDLINE_LEN: usize = 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct BootImage {
    pub kernel: Vec<u8>,
    pub ramdisk: Vec<u8>,
    pub second: Vec<u8>,
    pub recovery_dtbo: Vec<u8>,
    pub dtb: Vec<u8>,
    pub kernel_addr: u32,
    pub ramdisk_addr: u32,
    pub second_addr: u32,
    pub tags_addr: u32,
    pub dtb_addr: u64,
    pub page_size: u32,
    pub os_version: u32,
    pub name: [u8; NAME_LEN],
    pub cmdline: [u8; CMDLINE_LEN],
    pub extra_cmdline: [u8; EXTRA_CMDLINE_LEN],
}

fn page_aligned(len: usize, page: usize) -> usize {
    len.div_ceil(page) * page
}

impl BootImage {
    /// Reads an image whose header's fields lie where boot_img_hdr_v2 has them.
    pub fn parse(image: &[u8]) -> io::Result<Self> {
        if image.len() < HEADER_SIZE || !image.starts_with(MAGIC) {
            return Err(invalid("missing ANDROID! magic"));
        }
        let field = |at: usize| word(image, at);
        let (page_size, header_version, header_size) = (field(36), field(40), field(1644));
        if header_version != HEADER_VERSION {
            return Err(invalid(&format!("boot header version {header_version}, only {HEADER_VERSION} is supported")));
        }
        if header_size as usize != HEADER_SIZE {
            return Err(invalid(&format!("header size field is {header_size}, expected {HEADER_SIZE}")));
        }
        if !page_size.is_power_of_two() {
            return Err(invalid(&format!("page size {page_size} is not a power of two")));
        }
        let page = page_size as usize;
        // The sections follow the header in the order they are read here, each from a page of its own on and as long as the field at `size_at` says.
        let mut offset = page_aligned(HEADER_SIZE, page);
        let mut section = |size_at: usize, what: &str| {
            let size = field(size_at) as usize;
            let data = offset.checked_add(size).and_then(|end| image.get(offset..end));
            let data = data.ok_or_else(|| invalid(&format!("image ends inside the {what} section")))?.to_vec();
            offset += page_aligned(size, page);
            Ok::<_, io::Error>(data)
        };
        Ok(BootImage {
            kernel: section(8, "kernel")?,
            ramdisk: section(16, "ramdisk")?,
            second: section(24, "second")?,
            recovery_dtbo: section(1632, "recovery dtbo")?,
            dtb: section(1648, "dtb")?,
            kernel_addr: field(12),
            ramdisk_addr: field(20),
            second_addr: field(28),
            tags_addr: field(32),
            dtb_addr: u64::from(field(1652)) | u64::from(field(1656)) << 32,
            page_size,
            os_version: field(44),
            name: *image[48..].first_chunk().unwrap(),
            cmdline: *image[64..].first_chunk().unwrap(),
            extra_cmdline: *image[608..].first_chunk().unwrap(),
        })
    }

    /// Puts a parameter at the end of the command line, after a blank where it holds one already; an error where the line would leave no room for its closing zero.
    pub fn append_cmdline(&mut self, parameter: &str) -> io::Result<()> {
        let end = self.cmdline.iter().position(|&byte| byte == 0).unwrap_or(CMDLINE_LEN);
        let blank = usize::from(end > 0);
        let stop = end + blank + parameter.len();
        if stop >= CMDLINE_LEN {
            return Err(invalid("the command line has no room for another parameter"));
        }
        if blank == 1 {
            self.cmdline[end] = b' ';
        }
        self.cmdline[end + blank..stop].copy_from_slice(parameter.as_bytes());
        self.cmdline[stop] = 0;
        Ok(())
    }

    // SHA1 over each section followed by its little endian length, as mkbootimg computes it.
    fn id(&self) -> [u8; ID_LEN] {
        let mut sha = Sha1::new();
        for part in [&self.kernel, &self.ramdisk, &self.second, &self.recovery_dtbo, &self.dtb] {
            sha.update(part);
            sha.update((part.len() as u32).to_le_bytes());
        }
        let (mut id, digest) = ([0u8; ID_LEN], sha.finalize());
        id[..digest.len()].copy_from_slice(&digest);
        id
    }

    /// The image as mkbootimg lays it out, for a page size that is a power of two, as `parse` holds it to, and sections of a 32 bit length.
    pub fn to_bytes(&self) -> Vec<u8> {
        let page = self.page_size as usize;
        let recovery_dtbo_offset = if self.recovery_dtbo.is_empty() {
            0
        } else {
            (page_aligned(HEADER_SIZE, page)
                + page_aligned(self.kernel.len(), page)
                + page_aligned(self.ramdisk.len(), page)
                + page_aligned(self.second.len(), page)) as u64
        };

        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.kernel.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.kernel_addr.to_le_bytes());
        out.extend_from_slice(&(self.ramdisk.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.ramdisk_addr.to_le_bytes());
        out.extend_from_slice(&(self.second.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.second_addr.to_le_bytes());
        out.extend_from_slice(&self.tags_addr.to_le_bytes());
        out.extend_from_slice(&self.page_size.to_le_bytes());
        out.extend_from_slice(&HEADER_VERSION.to_le_bytes());
        out.extend_from_slice(&self.os_version.to_le_bytes());
        out.extend_from_slice(&self.name);
        out.extend_from_slice(&self.cmdline);
        out.extend_from_slice(&self.id());
        out.extend_from_slice(&self.extra_cmdline);
        out.extend_from_slice(&(self.recovery_dtbo.len() as u32).to_le_bytes());
        out.extend_from_slice(&recovery_dtbo_offset.to_le_bytes());
        out.extend_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        out.extend_from_slice(&(self.dtb.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.dtb_addr.to_le_bytes());
        debug_assert_eq!(out.len(), HEADER_SIZE);

        for part in [&[][..], &self.kernel, &self.ramdisk, &self.second, &self.recovery_dtbo, &self.dtb] {
            out.extend_from_slice(part);
            out.resize(page_aligned(out.len(), page), 0);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BootImage {
        let mut cmdline = [0u8; CMDLINE_LEN];
        cmdline[..9].copy_from_slice(b"bootopt=x");
        BootImage {
            kernel: vec![0xAA; 5000],
            ramdisk: vec![0xBB; 2048],
            second: Vec::new(),
            recovery_dtbo: Vec::new(),
            dtb: vec![0xCC; 77],
            kernel_addr: 0x4008_0000,
            ramdisk_addr: 0x51b0_0000,
            second_addr: 0,
            tags_addr: 0x4788_0000,
            dtb_addr: 0x4788_0000,
            page_size: 2048,
            os_version: 0x1800_0167,
            name: [0; NAME_LEN],
            cmdline,
            extra_cmdline: [0; EXTRA_CMDLINE_LEN],
        }
    }

    #[test]
    fn boot_image_round_trip() {
        let image = sample();
        let bytes = image.to_bytes();
        assert_eq!(bytes.len(), 2048 + 6144 + 2048 + 2048);
        assert_eq!((bytes[2048], bytes[2048 + 6144], bytes[2048 + 6144 + 2048]), (0xAA, 0xBB, 0xCC));
        assert!(BootImage::parse(&bytes).unwrap() == image);
    }

    #[test]
    fn foreign_header_refused() {
        let refusal = |bytes: &[u8]| BootImage::parse(bytes).err().map(|e| e.to_string());
        let mut bytes = sample().to_bytes();
        assert_eq!(refusal(&bytes[..4096]).as_deref(), Some("image ends inside the kernel section"));
        bytes[40] = 3;
        assert_eq!(refusal(&bytes).as_deref(), Some("boot header version 3, only 2 is supported"));
    }

    #[test]
    fn cmdline_append() {
        let mut image = sample();
        image.append_cmdline("printk.silent=1").unwrap();
        assert_eq!(&image.cmdline[..26], b"bootopt=x printk.silent=1\0");
        let mut empty = BootImage { cmdline: [0; CMDLINE_LEN], ..sample() };
        empty.append_cmdline("a").unwrap();
        assert_eq!(&empty.cmdline[..2], b"a\0", "an empty line takes no blank");
        let mut nearly = BootImage { cmdline: [0; CMDLINE_LEN], ..sample() };
        nearly.cmdline[..CMDLINE_LEN - 3].fill(b'x');
        assert!(nearly.append_cmdline("a").is_ok(), "a blank, the parameter and the zero fill the line");
        assert_eq!(nearly.cmdline[CMDLINE_LEN - 3..], *b" a\0");
        assert!(nearly.append_cmdline("b").is_err());
        let mut full = BootImage { cmdline: [b'x'; CMDLINE_LEN], ..sample() };
        assert!(full.append_cmdline("a").is_err(), "a line without its zero has no room");
    }

    #[test]
    #[ignore = "needs the stock firmware under stock/"]
    fn reproduces_stock_image_byte_for_byte() {
        let stock = std::fs::read(crate::stock("boot.img")).unwrap();
        let rebuilt = BootImage::parse(&stock).unwrap().to_bytes();
        assert!(stock[..rebuilt.len()] == rebuilt[..]);
    }
}

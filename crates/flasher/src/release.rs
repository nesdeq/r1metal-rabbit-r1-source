//! Rabbit's release as `r1` takes it: its files by name, what each of its images and of r1metal's own is held to be, and its firmware read for `r1 stock` with every image held to be what it is named for before anything is erased.

use crate::{Result, of, read};
use r1_image::bootimg::BootImage;
use r1_image::sparse::{Part, Sparse};
use r1_image::{dt_table, vbmeta};
use std::fs::File;
use std::io;
use std::path::Path;

/// The partitions of the release's boot image, its overlay and its super image, each in the file `image` names; and its logo's file.
pub const BOOT: &str = "boot";
pub const DTBO: &str = "dtbo";
pub const SUPER: &str = "super";
const LOGO: &str = "logo.bin";
// What MediaTek's partition header begins with: its magic, little endian.
const MEDIATEK_HEADER: [u8; 4] = 0x5888_1688u32.to_le_bytes();

/// What an image is held to be before anything is erased.
#[derive(Clone, Copy)]
pub enum Holds {
    Boot,
    Overlay,
    Vbmeta,
    // An image behind MediaTek's partition header, which the preloader and the bootloader read.
    MediaTek,
}

// Rabbit's images of a slot, by partition, each in the file of its partition's name, in the order its scatter file lists them.
const SLOT: [(&str, Holds); 12] = [
    ("md1img", Holds::MediaTek),
    ("spmfw", Holds::MediaTek),
    ("scp", Holds::MediaTek),
    ("sspm", Holds::MediaTek),
    ("gz", Holds::MediaTek),
    ("lk", Holds::MediaTek),
    (BOOT, Holds::Boot),
    (DTBO, Holds::Overlay),
    ("tee", Holds::MediaTek),
    ("vbmeta", Holds::Vbmeta),
    ("vbmeta_system", Holds::Vbmeta),
    ("vbmeta_vendor", Holds::Vbmeta),
];

/// The release's file of a partition's image: the partition's name, then `.img`.
pub fn image(partition: &str) -> String {
    format!("{partition}.img")
}

/// An image read from `path` and held to be what it is named for.
pub fn checked(path: &Path, holds: Holds) -> Result<Vec<u8>> {
    let image = read(path)?;
    match holds {
        Holds::Boot => BootImage::parse(&image).map(drop).map_err(|e| of(path, e))?,
        Holds::Overlay => dt_table::Table::parse(&image).map(drop).map_err(|e| of(path, e))?,
        Holds::Vbmeta => vbmeta::flags(&image).map(drop).ok_or_else(|| of(path, "no vbmeta"))?,
        Holds::MediaTek if image.starts_with(&MEDIATEK_HEADER) => {}
        Holds::MediaTek => return Err(of(path, "no MediaTek partition image")),
    }
    Ok(image)
}

/// Rabbit's firmware as `r1 stock` writes it: all of it but the preloader, which is never written, and user data, which is erased.
pub struct Stock {
    slot: Vec<(&'static str, Vec<u8>)>,
    logo: Vec<u8>,
    dynamic: Sparse<File>,
}

impl Stock {
    /// Rabbit's firmware out of the release in `dir`, each image held to be what it is named for before anything is erased.
    pub fn open(dir: &Path) -> Result<Stock> {
        let mut slot = Vec::new();
        for (partition, holds) in SLOT {
            slot.push((partition, checked(&dir.join(image(partition)), holds)?));
        }
        let logo = checked(&dir.join(LOGO), Holds::MediaTek)?;
        let dynamic = dir.join(image(SUPER));
        let dynamic = File::open(&dynamic).and_then(Sparse::open).map_err(|e| of(&dynamic, e))?;
        Ok(Stock { slot, logo, dynamic })
    }

    /// Each image of a slot by its partition, in the order Rabbit's scatter file lists them; its `vbmeta_system` and `vbmeta_vendor` hold the hashes of `super`'s partitions.
    pub fn slot(&self) -> Vec<(&'static str, &[u8])> {
        self.slot.iter().map(|(partition, image)| (*partition, &image[..])).collect()
    }

    pub fn logo(&self) -> &[u8] {
        &self.logo
    }

    /// The bytes of `super` its sparse image stands for: system, product and vendor of slot a.
    pub fn dynamic_size(&self) -> u64 {
        self.dynamic.size()
    }

    /// That sparse image cut into runs that each fit a download of at most `limit` bytes (`Sparse::parts`).
    pub fn dynamic_parts(&self, limit: usize) -> io::Result<Vec<Part>> {
        self.dynamic.parts(limit)
    }

    /// The sparse image that writes a run of `super` and skips all else.
    pub fn dynamic_part(&mut self, part: Part) -> io::Result<Vec<u8>> {
        self.dynamic.part(part)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs the stock firmware under stock/"]
    fn stock_release_images() {
        let release = crate::workspace(r1_image::STOCK);
        let stock = Stock::open(&release).unwrap();
        let partitions: Vec<&str> = stock.slot.iter().map(|(partition, _)| *partition).collect();
        assert_eq!(partitions, SLOT.map(|(partition, _)| partition));
        assert!(stock.slot.iter().all(|(_, image)| image.len() <= 0x800_0000), "each fits one download");
        assert_eq!(stock.dynamic.size(), 0x2_0c0c_4000, "the size of the super partition");
        assert!(checked(&release.join(image(BOOT)), Holds::MediaTek).is_err());
    }
}

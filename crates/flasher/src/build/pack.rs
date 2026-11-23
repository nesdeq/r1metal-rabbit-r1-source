//! Assembles an r1metal boot image: our kernel and ramdisk, Rabbit's device tree with our changes.

use crate::Result;
use r1_funcore::domain::flash::{self, Flash};
use r1_funcore::wifi::wmt::FIRMWARE;
use r1_image::bootimg::BootImage;
use r1_image::{cpio, lz4_legacy};

// Size of boot_a and boot_b in the stock partition table (MT6765_Android_scatter.txt).
const BOOT_PARTITION_SIZE: usize = 0x200_0000;
const MOUNT_POINTS: [&str; 4] = ["dev", "proc", "sys", "config"];
// (path, permissions, major, minor) of the nodes init needs before it has mounted /dev.
const EARLY_NODES: [(&str, u32, u32, u32); 3] =
    [("dev/console", 0o600, 5, 1), ("dev/null", 0o666, 1, 3), ("dev/kmsg", 0o600, 1, 11)];

// The initramfs: the init binary, the directories it mounts on, three device nodes, and the files drivers ask the kernel for, where the kernel looks for them.
fn ramdisk(init: &[u8], files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut archive = cpio::Archive::default();
    for dir in MOUNT_POINTS {
        archive.dir(dir, 0o755);
    }
    for (path, perm, major, minor) in EARLY_NODES {
        archive.char_device(path, perm, major, minor);
    }
    archive.file("init", 0o755, init);
    for (end, _) in FIRMWARE.match_indices('/').chain([(FIRMWARE.len(), "")]) {
        archive.dir(&FIRMWARE[..end], 0o755);
    }
    for (name, bytes) in files {
        archive.file(&format!("{FIRMWARE}/{name}"), 0o644, bytes);
    }
    lz4_legacy::compress(&archive.finish())
}

// Takes `stock_boot` and swaps in `kernel`, the dtb section `trees` and a ramdisk holding `init` and `files`, keeping everything else.
pub(super) fn boot_image(
    stock_boot: &[u8],
    kernel: &[u8],
    trees: &[u8],
    init: &[u8],
    files: &[(String, Vec<u8>)],
) -> Result<Vec<u8>> {
    let mut image = BootImage::parse(stock_boot)?;
    image.kernel = kernel.to_vec();
    image.dtb = trees.to_vec();
    image.ramdisk = ramdisk(init, files);
    fitting(image.to_bytes())
}

/// A boot image opened for the file of a flash: its sections, and its ramdisk's archive open behind its entries.
pub struct Unflashed {
    image: BootImage,
    archive: cpio::Archive,
}

impl Unflashed {
    /// Opens `boot`; an error for what is no boot image whose ramdisk is a cpio archive.
    pub fn open(boot: &[u8]) -> Result<Unflashed> {
        let image = BootImage::parse(boot)?;
        let archive = cpio::reopen(&lz4_legacy::decompress(&image.ramdisk)?)
            .ok_or("the boot image's ramdisk is no cpio archive")?;
        Ok(Unflashed { image, archive })
    }

    /// The boot image with the file of a flash at the root of its ramdisk, which tells the device what that flash chose, and the kernel's parameter for its logging on the command line.
    pub fn flashed(self, flash: &Flash) -> Result<Vec<u8>> {
        let Unflashed { mut image, mut archive } = self;
        archive.file(flash::FILE, 0o644, flash.to_text().as_bytes());
        image.ramdisk = lz4_legacy::compress(&archive.finish());
        if let Some(parameter) = flash.logging.to_parameter() {
            image.append_cmdline(parameter)?;
        }
        fitting(image.to_bytes())
    }
}

// A boot image that fits the boot partition.
fn fitting(bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.len() > BOOT_PARTITION_SIZE {
        return Err(format!("boot image is {} bytes, the partition holds {BOOT_PARTITION_SIZE}", bytes.len()).into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::logs::Logging;

    // A boot image of these sections, laid out as Rabbit's is, with its command line.
    fn image(kernel: Vec<u8>, ramdisk: Vec<u8>, dtb: Vec<u8>) -> BootImage {
        let (mut cmdline, stock) = ([0; 512], b"bootopt=64S3,32N2,64N2 buildvariant=user");
        cmdline[..stock.len()].copy_from_slice(stock);
        BootImage {
            kernel,
            ramdisk,
            second: Vec::new(),
            recovery_dtbo: Vec::new(),
            dtb,
            kernel_addr: 0x4008_0000,
            ramdisk_addr: 0x51b0_0000,
            second_addr: 0,
            tags_addr: 0x4788_0000,
            dtb_addr: 0x4788_0000,
            page_size: 2048,
            os_version: 0x1800_0167,
            name: [0; 16],
            cmdline,
            extra_cmdline: [0; 1024],
        }
    }

    #[test]
    fn ramdisk_contents() {
        let init = b"\x7fELF fake init";
        let archive =
            lz4_legacy::decompress(&ramdisk(init, &[("some.bin".into(), b"what a driver asks for".to_vec())])).unwrap();
        assert_eq!(&archive[..6], b"070701");
        let has = |needle: &[u8]| archive.windows(needle.len()).any(|window| window == needle);
        assert!(has(b"init\0") && has(init) && has(b"dev/kmsg\0") && has(b"TRAILER!!!\0"));
        let named = |path: &str| format!("{path}\0").into_bytes();
        let (parent, directory, file) = (named("vendor"), named(FIRMWARE), named(&format!("{FIRMWARE}/some.bin")));
        assert!(
            has(&parent) && has(&directory) && has(&file) && has(b"what a driver asks for"),
            "the files for drivers lie where the kernel looks first, every directory above them made"
        );
        let bare = lz4_legacy::decompress(&ramdisk(init, &[])).unwrap();
        assert!(
            bare.windows(directory.len()).any(|window| window == directory),
            "the directory is there without files too"
        );
    }

    #[test]
    fn only_kernel_trees_and_ramdisk_change() {
        let stock = image(vec![1; 3000], vec![2; 10], vec![3; 500]);
        let (kernel, trees) = (vec![4; 2000], vec![5; 600]);
        let files = [("a.cfg".to_string(), b"a=1\n".to_vec())];
        let packed = boot_image(&stock.to_bytes(), &kernel, &trees, b"init", &files).unwrap();
        let ours = BootImage::parse(&packed).unwrap();
        assert!(ours.kernel == kernel && ours.dtb == trees);
        assert!(ours.ramdisk == ramdisk(b"init", &files));
        let kept =
            BootImage { kernel: stock.kernel.clone(), ramdisk: stock.ramdisk.clone(), dtb: stock.dtb.clone(), ..ours };
        assert!(kept == stock);
    }

    #[test]
    fn flash_file_and_cmdline() {
        let ours = image(vec![4; 2000], ramdisk(b"init", &[("a.cfg".to_string(), b"a=1\n".to_vec())]), vec![5; 600]);
        for (logging, line) in [
            (Logging::Off, &b"bootopt=64S3,32N2,64N2 buildvariant=user printk.silent=1\0"[..]),
            (Logging::On, b"bootopt=64S3,32N2,64N2 buildvariant=user\0"),
            (Logging::Debug, b"bootopt=64S3,32N2,64N2 buildvariant=user printk.debug=1\0"),
        ] {
            let choice = Flash { stamp: 1_792_000_000, logging, key: Some("sk-proj-Fake0Key_-9".into()) };
            let flashed = Unflashed::open(&ours.to_bytes()).unwrap().flashed(&choice).unwrap();
            let made = BootImage::parse(&flashed).unwrap();
            let archive = lz4_legacy::decompress(&made.ramdisk).unwrap();
            let mut expected = cpio::reopen(&lz4_legacy::decompress(&ours.ramdisk).unwrap()).unwrap();
            expected.file(flash::FILE, 0o644, choice.to_text().as_bytes());
            assert!(
                archive == expected.finish(),
                "{logging:?}: the ramdisk as it was, with the flash's file before the trailer"
            );
            assert!(
                made.cmdline.starts_with(line),
                "{logging:?}: the kernel's parameter for the logging ends the command line"
            );
            assert!(
                BootImage { ramdisk: ours.ramdisk.clone(), cmdline: ours.cmdline, ..made } == ours,
                "{logging:?}: nothing else changes"
            );
        }
        assert!(Unflashed::open(b"no boot image").is_err(), "what is no boot image is refused before a key is asked");
        let unpacked = image(vec![4; 2000], b"no lz4".to_vec(), vec![5; 600]).to_bytes();
        assert!(Unflashed::open(&unpacked).is_err(), "and so is one whose ramdisk is no archive");
    }
}

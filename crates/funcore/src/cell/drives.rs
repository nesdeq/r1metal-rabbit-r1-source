//! The modem's drives: their letters, their folders in the private directory, and their first filling from the factory's partitions.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// Directory of the modem's NVRAM, one folder per drive the modem names.
pub(crate) const NVRAM_DIRECTORY: &str = ".r1metal/cell/nvram";
// Directory of the NVRAM as the factory's partitions held it when the modem's folders were first filled, which `Restore` reads.
pub(crate) const FACTORY_DIRECTORY: &str = ".r1metal/cell/factory";
/// The record of the factory drives filled whole so far, in `cell::DIRECTORY`, a letter a line.
pub const SEEDED_FILE: &str = "seeded";
// The drives MediaTek's `ccci_fsd` and Rabbit OS's add to it name; `W`, `T` and `R` are folders of the vendor partition Rabbit OS's has not.
pub(super) const DRIVES: &[u8] = b"ZXYWVUTSRQ";
// The drives made empty: the modem's low-power log (`/data/vendor/mdlpm`) and `/data/vendor_de/md`.
pub(super) const EMPTY_DRIVES: [u8; 2] = *b"UQ";
// The drives filled from the factory's partitions, each with the partition and the folder there, as Rabbit OS's `fstab` mounts them.
const FACTORY_DRIVES: [(u8, &str, &str); 5] = [
    (b'Z', "nvdata", "md"),
    (b'V', "nvdata", "md_cmn"),
    (b'X', "protect1", "md"),
    (b'Y', "protect2", "md"),
    (b'S', "nvcfg", ""),
];

/// The drives the record of filled ones names.
pub fn parse_seeded(text: &str) -> Vec<u8> {
    let drive = |line: &str| match line.trim().as_bytes() {
        [drive] if DRIVES.contains(drive) => Some(*drive),
        _ => None,
    };
    text.lines().filter_map(drive).collect()
}

pub fn to_seeded_text(drives: &[u8]) -> String {
    drives.iter().map(|drive| format!("{}\n", char::from(*drive))).collect()
}

/// The factory drives the record does not name, each with its partition and its folder there.
pub fn to_seeds(seeded: &[u8]) -> Vec<(u8, &'static str, &'static str)> {
    FACTORY_DRIVES.into_iter().filter(|(drive, ..)| !seeded.contains(drive)).collect()
}

/// A drive's two folders, the modem's and the factory's copy, each as its directory and its name there.
pub fn to_drive_folders(drive: u8) -> [(&'static str, String); 2] {
    [NVRAM_DIRECTORY, FACTORY_DIRECTORY].map(|root| (root, String::from(char::from(drive))))
}

/// The path of `name` in `folder`: `name` alone where `folder` is empty, `folder` alone where `name` is.
pub fn join_path(folder: &str, name: &str) -> String {
    match (folder.is_empty(), name.is_empty()) {
        (true, _) => String::from(name),
        (false, true) => String::from(folder),
        (false, false) => format!("{folder}/{name}"),
    }
}

/// The directory under a drive's folder in `root` that a folder `under` its partition's is copied to.
pub fn to_seed_directory(root: &str, drive: u8, under: &str) -> String {
    join_path(&to_drive_folder(root, drive), under)
}

/// The log line of a drive filled from the factory's partition.
pub fn to_seeded_line(drive: u8, partition: &str, folder: &str, files: usize) -> String {
    format!("cell: drive {}: filled with {files} files from {partition}/{folder}", char::from(drive))
}

pub(super) fn to_drive_folder(root: &str, drive: u8) -> String {
    join_path(root, &String::from(char::from(drive)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeding_round_trip() {
        let text = to_seeded_text(b"ZV");
        assert_eq!(text, "Z\nV\n");
        assert_eq!(parse_seeded(&text), [b'Z', b'V']);
        assert_eq!(
            parse_seeded("Z\nA\nXY\n\n S \n"),
            [b'Z', b'S'],
            "a letter of no drive, or more than one, is passed over"
        );
        let drives: Vec<u8> = to_seeds(b"ZX").iter().map(|seed| seed.0).collect();
        assert_eq!(drives, [b'V', b'Y', b'S'], "the drives not filled yet");
        assert_eq!(to_drive_folders(b'Z'), [(NVRAM_DIRECTORY, "Z".into()), (FACTORY_DIRECTORY, "Z".into())]);
        assert_eq!(to_seed_directory(NVRAM_DIRECTORY, b'Z', ""), format!("{NVRAM_DIRECTORY}/Z"));
        assert_eq!(
            to_seed_directory(FACTORY_DIRECTORY, b'Z', "NVRAM/NVD_DATA"),
            format!("{FACTORY_DIRECTORY}/Z/NVRAM/NVD_DATA")
        );
        assert_eq!(to_seeded_line(b'Z', "nvdata", "md", 12), "cell: drive Z: filled with 12 files from nvdata/md");
        assert_eq!(
            (join_path("md", "NVRAM"), join_path("", "APCFG"), join_path("md", "")),
            ("md/NVRAM".into(), "APCFG".into(), "md".into())
        );
    }
}

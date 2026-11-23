use super::Shared;
use super::folder::days;
use crate::log;
use crate::storage::files::{Folder, move_into_place};
use flate2::Compression;
use flate2::write::GzEncoder;
use r1_funcore::base::file_names;
use r1_funcore::fs::{tar, to_modified};
use r1_funcore::logs;
use r1_funcore::storage::paths;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(super) fn list_files(folder: &Path, archive_path: &str, files: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let archive_path = format!("{archive_path}/{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            list_files(&entry.path(), &archive_path, files)?;
        } else {
            files.push((archive_path, entry.path()));
        }
    }
    Ok(())
}

// Writes an archive's file: the tape `fill` writes, packed and committed.
pub(super) fn write_packed<T>(
    path: &Path,
    fill: impl FnOnce(&mut GzEncoder<File>) -> io::Result<T>,
) -> io::Result<(File, T)> {
    let mut packer = GzEncoder::new(File::create(path)?, Compression::default());
    let filled = fill(&mut packer)?;
    let archive = packer.finish()?;
    archive.sync_all()?;
    Ok((archive, filled))
}

fn write_tape<'a, R: Read>(
    path: &Path,
    files: impl Iterator<Item = io::Result<(&'a str, u64, u64, R)>>,
) -> io::Result<(File, u64)> {
    write_packed(path, |packer| {
        let mut total = 0;
        for file in files {
            let (name, size, modified, content) = file?;
            let header =
                tar::header(name, size, modified).ok_or_else(|| io::Error::other(tar::to_no_room_text(name)))?;
            packer.write_all(&header)?;
            if io::copy(&mut content.take(size), packer)? != size {
                return Err(io::Error::other(format!("{name}: shorter than it said")));
            }
            packer.write_all(&[0u8; tar::BLOCK][..tar::padding(size)])?;
            total += size;
        }
        packer.write_all(&tar::END)?;
        Ok(total)
    })
}

fn pack(root: &Path, day: &str) -> io::Result<(String, u64, u64)> {
    let folder = root.join(day);
    let name = file_names::first_free(|count| logs::archive_name(day, count), Folder(root))?;
    let temporary = root.join(paths::new_name(&name));
    let mut files = Vec::new();
    list_files(&folder, day, &mut files)?;
    let files = logs::to_tape_order(files);
    let entries = files.iter().map(|(archive_path, path)| {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        Ok((archive_path.as_str(), metadata.len(), to_modified(metadata.mtime()), file))
    });
    let (archive, total) = write_tape(&temporary, entries)?;
    let packed_bytes = archive.metadata()?.len();
    move_into_place(&temporary, &root.join(&name), root)?;
    fs::remove_dir_all(&folder)?;
    Ok((name, total, packed_bytes))
}

fn pack_past(shared: &Shared, root: &Path, today: &str) {
    let days = match days(root) {
        Ok(days) => days,
        Err(e) => return log!("logs: the days under {}: {e}", root.display()),
    };
    for day in days.iter().filter(|day| shared.board.lock().days.begin_past(day, today)) {
        let start = Instant::now();
        match pack(root, day) {
            Ok((name, total, packed_bytes)) => {
                log!("{}", logs::to_packed_line(day, &name, total, packed_bytes, start.elapsed().as_millis()))
            }
            Err(e) => log!("logs: packing {day}: {e}"),
        }
        shared.end_packing();
    }
}

pub(super) fn work(shared: &Shared) -> ! {
    loop {
        let (root, today) = shared.await_due_day();
        pack_past(shared, &root, &today);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::scratch;
    use flate2::read::GzDecoder;
    use r1_funcore::logs::{CRASH, FRAMES, LOG};

    fn unpack(archive: &Path) -> Vec<(String, Vec<u8>)> {
        let mut tape = Vec::new();
        GzDecoder::new(File::open(archive).unwrap()).read_to_end(&mut tape).unwrap();
        let (mut files, mut at) = (Vec::new(), 0);
        while tape[at..at + tar::BLOCK].iter().any(|byte| *byte != 0) {
            let head = &tape[at..at + tar::BLOCK];
            let text = |field: &[u8]| {
                String::from_utf8(field.iter().copied().take_while(|byte| *byte != 0).collect()).unwrap()
            };
            let size = usize::from_str_radix(&text(&head[124..136]), 8).unwrap();
            files.push((text(&head[..100]), tape[at + tar::BLOCK..at + tar::BLOCK + size].to_vec()));
            at += tar::BLOCK + size + tar::padding(size as u64);
        }
        assert_eq!(&tape[at..], &tar::END[..], "two empty blocks end the tape");
        files
    }

    #[test]
    fn pack_past_day() {
        let root = scratch("pack");
        let text =
            b"19:29:42.031 3,130401,7564029328,-; (4)[4294:kworker/u16:3][HYN]hyn_esd_check_func: Enter\n".repeat(700);
        for day in ["20261010", "20261011"] {
            fs::create_dir_all(root.join(day).join(CRASH)).unwrap();
            fs::write(root.join(day).join(LOG), &text).unwrap();
            fs::write(root.join(day).join(FRAMES), b"").unwrap();
            fs::write(root.join(day).join(CRASH).join("081200-console-ramoops-0"), b"panic").unwrap();
        }
        fs::write(root.join("boot"), b"a boot").unwrap();
        pack_past(&Shared::new().unwrap(), &root, "20261011");
        let mut names: Vec<String> =
            fs::read_dir(&root).unwrap().flatten().map(|entry| entry.file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(
            names,
            ["20261010.tar.gz", "20261011", "boot"],
            "today's folder stays, and nothing is left of the packing"
        );
        let files = unpack(&root.join("20261010.tar.gz"));
        assert_eq!(
            files.iter().map(|(name, content)| (name.as_str(), content.len())).collect::<Vec<_>>(),
            [("20261010/crash/081200-console-ramoops-0", 5), ("20261010/frames", 0), ("20261010/log", text.len())]
        );
        assert!(files[2].1 == text && files[0].1 == b"panic");
        assert!(
            fs::metadata(root.join("20261010.tar.gz")).unwrap().len() < text.len() as u64 / 10,
            "a log packs to a small part of itself"
        );
        fs::create_dir_all(root.join("20261010")).unwrap();
        fs::write(root.join("20261010").join(LOG), b"a day the clock came back to\n").unwrap();
        let (name, total, _) = pack(&root, "20261010").unwrap();
        assert_eq!((name.as_str(), total), ("20261010_1.tar.gz", 29), "an archive that is there is never written over");
        assert_eq!(unpack(&root.join("20261010.tar.gz")).len(), 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn open_day_not_packed() {
        let root = scratch("turns");
        for day in ["20261010", "20261011"] {
            fs::create_dir_all(root.join(day)).unwrap();
            fs::write(root.join(day).join(LOG), b"19:29:42.031 6,1,2,-;a record\n").unwrap();
        }
        let shared = std::sync::Arc::new(Shared::new().unwrap());
        shared.open_day("20261010");
        pack_past(&shared, &root, "20261011");
        assert!(
            root.join("20261010").is_dir() && !root.join("20261010.tar.gz").exists(),
            "a clock that went back has the writer in a day that is not today's: its folder stays"
        );
        shared.open_day("20261011");
        shared.pack_past(&root, "20261011");
        let (due_root, today) = shared.await_due_day();
        pack_past(&shared, &due_root, &today);
        assert!(!root.join("20261010").exists() && root.join("20261010.tar.gz").exists(), "left alone, it is packed");
        assert!(root.join("20261011").is_dir(), "today's stays");
        assert!(shared.board.lock().days.begin_packing("20261009"));
        let writer = std::sync::Arc::clone(&shared);
        let opening = std::thread::spawn(move || writer.open_day("20261009"));
        for _ in 0..100 {
            std::thread::yield_now();
        }
        assert!(!opening.is_finished(), "the writer does not have the day while it is being packed");
        shared.end_packing();
        opening.join().unwrap();
        assert!(!shared.board.lock().days.begin_packing("20261009"), "and has it once it is packed");
        fs::remove_dir_all(root).unwrap();
    }
}

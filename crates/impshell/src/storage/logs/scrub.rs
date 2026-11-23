use super::archive::{list_files, write_packed};
use crate::log;
use crate::storage::files::{move_into_place, write_over_with};
use flate2::read::GzDecoder;
use r1_funcore::fs::tar::{self, Head};
use r1_funcore::logs::scrub::{Entry, Holding, LogScrub, blank_signal, to_entry, to_holding};
use r1_funcore::logs::to_unreadable_line;
use r1_funcore::signal::THREADS;
use r1_funcore::storage::paths;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

fn is_unreadable(error: &io::Error) -> bool {
    use io::ErrorKind::{InvalidData, InvalidInput, UnexpectedEof};
    matches!(error.kind(), InvalidData | InvalidInput | UnexpectedEof)
}

pub(super) fn scrub(root: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        entries => entries?,
    };
    let mut result = Ok(());
    for entry in entries {
        let outcome = entry.and_then(|entry| {
            let (name, path) = (entry.file_name().to_string_lossy().into_owned(), entry.path());
            match to_entry(&name) {
                Entry::HalfWritten => fs::remove_file(path),
                Entry::Day if entry.file_type()?.is_dir() => scrub_day(&path, &name),
                Entry::Archive => scrub_archive(root, &name),
                Entry::Day | Entry::Other => Ok(()),
            }
        });
        result = result.and(outcome);
    }
    result
}

// A log on its way to `out`, which gets it without the traces of Signal.
struct Scrubbed<W> {
    scrub: LogScrub,
    kept: Vec<u8>,
    written: u64,
    out: W,
}

impl<W: Write> Scrubbed<W> {
    fn put_kept(&mut self) -> io::Result<()> {
        self.out.write_all(&self.kept)?;
        self.written += self.kept.len() as u64;
        self.kept.clear();
        Ok(())
    }
}

impl<W: Write> Write for Scrubbed<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.scrub.add(bytes, &THREADS, &mut self.kept);
        self.put_kept().map(|()| bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

// A log copied to `out` without the traces of Signal, no more of it in memory than a block and a line; whether it lost anything, and the bytes written.
fn copy_log(mut log: impl Read, out: impl Write) -> io::Result<(bool, u64)> {
    let mut scrubbed = Scrubbed { scrub: LogScrub::default(), kept: Vec::new(), written: 0, out };
    io::copy(&mut log, &mut scrubbed)?;
    scrubbed.scrub.finish(&THREADS, &mut scrubbed.kept);
    scrubbed.put_kept()?;
    Ok((scrubbed.scrub.is_changed(), scrubbed.written))
}

// A crash's copy, read whole as it is no longer than the partition the bootloader saves it on, copied to `out` with Signal's lines blanked; whether any was, and the bytes written.
fn copy_blanked(mut copy: impl Read, mut out: impl Write) -> io::Result<(bool, u64)> {
    let mut bytes = Vec::new();
    copy.read_to_end(&mut bytes)?;
    let is_changed = blank_signal(&mut bytes, &THREADS);
    out.write_all(&bytes)?;
    Ok((is_changed, bytes.len() as u64))
}

// Writes a day's file anew in its place where `copy` changes it: read once to tell, and once more to write.
fn rewrite(
    directory: &Path,
    path: &Path,
    copy: impl Fn(File, &mut dyn Write) -> io::Result<(bool, u64)>,
) -> io::Result<()> {
    if !copy(File::open(path)?, &mut io::sink())?.0 {
        return Ok(());
    }
    write_over_with(directory, path, |file| {
        let mut out = BufWriter::new(file);
        copy(File::open(path)?, &mut out)?;
        out.flush()
    })
}

fn scrub_day(folder: &Path, day: &str) -> io::Result<()> {
    let mut files = Vec::new();
    list_files(folder, day, &mut files)?;
    let mut result = Ok(());
    for (name, path) in files {
        let directory = path.parent().unwrap_or(folder);
        let outcome = match to_holding(&name) {
            Holding::HalfWritten => fs::remove_file(&path),
            Holding::Log => rewrite(directory, &path, |file, out| copy_log(file, out)),
            Holding::CrashCopy => rewrite(directory, &path, |file, out| copy_blanked(file, out)),
            Holding::Nothing => Ok(()),
        };
        result = result.and(outcome);
    }
    result
}

// A tape's files copied to `out` as a scrub leaves them, each behind a header of its size in `sizes`, or behind none where no sizes are given; whether any changed, and the size each came to.
fn copy_tape(tape: &mut GzDecoder<File>, sizes: Option<&[u64]>, mut out: impl Write) -> io::Result<(bool, Vec<u64>)> {
    let (mut is_changed, mut written, mut block) = (false, Vec::new(), [0u8; tar::BLOCK]);
    loop {
        tape.read_exact(&mut block)?;
        let (name, size, modified) = match tar::parse_head(&block) {
            Some(Head::File { name, size, modified }) => (name, size, modified),
            Some(Head::End) => break,
            None => return Err(io::ErrorKind::InvalidData.into()),
        };
        if let Some(sizes) = sizes {
            let size = *sizes.get(written.len()).ok_or(io::ErrorKind::InvalidData)?;
            let header =
                tar::header(&name, size, modified).ok_or_else(|| io::Error::other(tar::to_no_room_text(&name)))?;
            out.write_all(&header)?;
        }
        let mut file = Read::take(&mut *tape, size);
        let (changed, bytes) = match to_holding(&name) {
            Holding::Log => copy_log(&mut file, &mut out)?,
            Holding::CrashCopy => copy_blanked(&mut file, &mut out)?,
            Holding::HalfWritten | Holding::Nothing => (false, io::copy(&mut file, &mut out)?),
        };
        if file.limit() > 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        out.write_all(&[0u8; tar::BLOCK][..tar::padding(bytes)])?;
        tape.read_exact(&mut block[..tar::padding(size)])?;
        is_changed |= changed;
        written.push(bytes);
    }
    out.write_all(&tar::END)?;
    // What a tape ends on is read too, so that an archive whose packing does not hold to its end is told.
    io::copy(tape, &mut io::sink())?;
    Ok((is_changed, written))
}

// Packs an archive anew where a scrub changes it, a file of its tape at a time: read once to tell, with the size each file comes to, and once more to write.
fn scrub_archive(root: &Path, name: &str) -> io::Result<()> {
    let path = root.join(name);
    let open = || File::open(&path).map(GzDecoder::new);
    let sizes = match copy_tape(&mut open()?, None, io::sink()) {
        Ok((false, _)) => return Ok(()),
        Ok((true, sizes)) => sizes,
        Err(e) if is_unreadable(&e) => {
            log!("{}", to_unreadable_line(name));
            return fs::remove_file(path);
        }
        Err(e) => return Err(e),
    };
    let temporary = root.join(paths::new_name(name));
    write_packed(&temporary, |packer| copy_tape(&mut open()?, Some(&sizes), packer))?;
    move_into_place(&temporary, &path, root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::scratch;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use r1_funcore::logs::scrub::blank_signal;
    use r1_funcore::logs::{CRASH, LOG, POWER};
    use std::io::Write;

    const SIGNALS: &[u8] =
        b"10:00:00.000 6,1,5,-;usb: up\n10:00:01.000 14,2,9,-; (2)[412:signal]r1metal: signal: linked as device 3\n";
    const CLEAN: &[u8] = b"10:00:00.000 6,1,5,-;usb: up\n10:00:01.000 14,2,9,-;\n";
    const COPY: &[u8] = b"<6>[ 9.1] (2)[412:signal]r1metal: signal: sent\n<6>[ 9.2] (1)[1:init]end\n";

    fn unpack(archive: &Path) -> Vec<(String, Vec<u8>, u64)> {
        let mut tape = Vec::new();
        GzDecoder::new(File::open(archive).unwrap()).read_to_end(&mut tape).unwrap();
        let files = tar::parse_files(&tape).unwrap();
        files.into_iter().map(|file| (file.name, tape[file.at].to_vec(), file.modified)).collect()
    }

    fn pack(root: &Path, name: &str, files: &[(&str, &[u8])], modified: u64) {
        let mut packer = GzEncoder::new(File::create(root.join(name)).unwrap(), Compression::default());
        for (path, content) in files {
            packer.write_all(&tar::header(path, content.len() as u64, modified).unwrap()).unwrap();
            packer.write_all(content).unwrap();
            packer.write_all(&[0u8; tar::BLOCK][..tar::padding(content.len() as u64)]).unwrap();
        }
        packer.write_all(&tar::END).unwrap();
        packer.finish().unwrap();
    }

    #[test]
    fn scrub_folders_and_archives() {
        let root = scratch("scrub");
        let today = root.join("20261011");
        fs::create_dir_all(today.join(CRASH)).unwrap();
        fs::write(today.join(LOG), SIGNALS).unwrap();
        fs::write(today.join(POWER), b"10:00:00.000 4396 mV\n").unwrap();
        fs::write(today.join(CRASH).join("081200-dmesg-ramoops-0"), COPY).unwrap();
        fs::write(today.join(paths::new_name(LOG)), SIGNALS).unwrap();
        let crash = format!("20261010/{CRASH}/081200-dmesg-ramoops-0");
        let files = [("20261010/log", SIGNALS), (crash.as_str(), COPY), ("20261010/power", b"4396 mV\n")];
        pack(&root, "20261010.tar.gz", &files, 1_791_600_000);
        pack(&root, "20261009.tar.gz", &[("20261009/log", CLEAN)], 1_791_500_000);
        fs::write(root.join("20261008.tar.gz"), b"no archive at all").unwrap();
        fs::write(root.join(paths::new_name("20261011.tar.gz")), SIGNALS).unwrap();
        fs::write(root.join("boot"), b"1f2e").unwrap();
        let modified = fs::metadata(root.join("20261009.tar.gz")).unwrap().modified().unwrap();
        scrub(&root).unwrap();
        assert_eq!(fs::read(today.join(LOG)).unwrap(), CLEAN, "the day the writer is at");
        assert_eq!(fs::read(today.join(POWER)).unwrap(), b"10:00:00.000 4396 mV\n");
        let copy = fs::read(today.join(CRASH).join("081200-dmesg-ramoops-0")).unwrap();
        assert!(copy.len() == COPY.len() && copy.ends_with(b"\n<6>[ 9.2] (1)[1:init]end\n"), "blanked in place");
        assert!(copy[..COPY.len() - 26].iter().all(|byte| *byte == b' '));
        let mut expected = COPY.to_vec();
        blank_signal(&mut expected, &THREADS);
        assert_eq!(
            unpack(&root.join("20261010.tar.gz")),
            [
                ("20261010/log".into(), CLEAN.to_vec(), 1_791_600_000),
                (crash.clone(), expected, 1_791_600_000),
                ("20261010/power".into(), b"4396 mV\n".to_vec(), 1_791_600_000)
            ],
            "an archive packed anew, each file in its place with its time"
        );
        assert_eq!(
            fs::metadata(root.join("20261009.tar.gz")).unwrap().modified().unwrap(),
            modified,
            "one that holds nothing of Signal's is left as it is"
        );
        let mut names: Vec<String> =
            fs::read_dir(&root).unwrap().flatten().map(|entry| entry.file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(
            names,
            ["20261009.tar.gz", "20261010.tar.gz", "20261011", "boot"],
            "what is no archive goes, and so does what was half written"
        );
        assert!(!today.join(paths::new_name(LOG)).exists());
        scrub(&root).unwrap();
        assert_eq!(fs::read(today.join(LOG)).unwrap(), CLEAN, "a second scrub finds nothing more");
        fs::remove_dir_all(&root).unwrap();
        assert!(scrub(&root).is_ok(), "logs that were never written have nothing to scrub");
    }
}

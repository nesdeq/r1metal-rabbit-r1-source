//! `r1 backup` and `r1 restore`: what a running R1 keeps, copied into an age file under its safe's passphrase, and a backup's files staged on an R1, which puts them in its partition's place at its next start.

mod age;
mod records;

use crate::link::console::{Console, Refusal};
use crate::link::{Choice, Transport};
use crate::prompt::{self, Attempt};
use crate::{Result, interrupt, of, part, remote, to_hex};
use aws_lc_rs::digest;
use r1_funcore::domain::backup::{PUBLIC_ROOTS, is_backed_up, staging};
use r1_funcore::domain::console::command::Command;
use r1_funcore::domain::console::shell::{EntryKind, WRONG, parse_listing_line};
use r1_funcore::domain::console::wire::ReplyLine;
use r1_funcore::storage::safe::Passphrase;
use r1_funcore::storage::{MOUNT_POINT, PRIVATE};
use records::Record;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, IsTerminal, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const OF_SAFE: &str = "Passphrase of the R1's safe, not shown: ";
const OF_BACKUP: &str = "Passphrase of the backup, not shown: ";
const OF_THIS_SAFE: &str = "Passphrase of this R1's safe, not shown (Enter where it is the backup's): ";

// True for the R1's word that a passphrase is not its safe's.
fn is_wrong(refused: &(dyn std::error::Error + 'static)) -> bool {
    refused.downcast_ref::<Refusal>().is_some_and(|refusal| refusal.says(WRONG))
}

// Opens the R1's gate with a passphrase asked for, `given` where none is typed; at a terminal it is asked again for as long as the R1 calls one wrong and takes another.
fn unlock(console: &mut Console, question: &str, given: Option<&Passphrase>) -> Result<Passphrase> {
    prompt::ask(question, |typed| {
        let typed = typed.map(|typed| Passphrase::from(typed.as_str()));
        let passphrase = typed.or_else(|| given.cloned()).ok_or("no passphrase given")?;
        match console.run(&Command::Unlock(passphrase.clone()), remote::hush) {
            Ok(()) => Ok(Attempt::Taken(passphrase)),
            Err(e) if is_wrong(e.as_ref()) => Ok(Attempt::Refused(e)),
            Err(e) => Err(e),
        }
    })
}

// Has the first Ctrl-C end the work at its next part of a file, so that the R1's gate is shut behind it, and a second end `r1` at once, for as long as what is returned beside the flag is held.
fn watch_interrupts() -> Result<(Arc<AtomicBool>, interrupt::Taken)> {
    let stopped = Arc::new(AtomicBool::new(false));
    let heard = Arc::clone(&stopped);
    let taken = interrupt::take(move || {
        if heard.swap(true, Ordering::Relaxed) {
            std::process::exit(interrupt::ENDED);
        }
    })?;
    Ok((stopped, taken))
}

fn check(stopped: &AtomicBool) -> Result<()> {
    if stopped.load(Ordering::Relaxed) { Err("interrupted".into()) } else { Ok(()) }
}

// How many files of how many are done, told in place where that goes to a terminal; its line is ended before anything else is told and as it is let go.
#[derive(Default)]
struct Progress {
    is_open: bool,
}

impl Progress {
    fn tell(&mut self, done: usize, all: usize) {
        if io::stderr().is_terminal() {
            eprint!("\r{done} of {all} files");
            self.is_open = true;
        }
    }

    fn end(&mut self) {
        if std::mem::take(&mut self.is_open) {
            eprintln!();
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.end();
    }
}

// Where a path from the partition's root lies on the R1.
fn on_device(path: &str) -> String {
    format!("{MOUNT_POINT}/{path}")
}

// What the R1 lists of what a backup takes: its files by their paths from the partition's root, and the directories it listed and then refused to list, as one that went meanwhile is.
#[derive(Default)]
struct Listing {
    files: Vec<String>,
    refused: HashSet<String>,
}

impl Listing {
    fn take(console: &mut Console<impl Transport>) -> Result<Listing> {
        let mut listing = Listing::default();
        for root in PUBLIC_ROOTS.into_iter().chain([PRIVATE]) {
            let names = remote::entries(console, &on_device(root), remote::hush)?;
            listing.add(console, root, &names)?;
        }
        Ok(listing)
    }

    fn add(&mut self, console: &mut Console<impl Transport>, directory: &str, names: &[String]) -> Result<()> {
        for name in names {
            let (inner, kind) = parse_listing_line(name);
            let path = format!("{directory}/{inner}");
            if !is_backed_up(&path) {
                continue;
            }
            match kind {
                EntryKind::File => self.files.push(path),
                EntryKind::Link => {}
                EntryKind::Directory => match remote::entries(console, &on_device(&path), remote::hush) {
                    Ok(names) => self.add(console, &path, &names)?,
                    Err(e) if e.is::<Refusal>() => drop(self.refused.insert(path)),
                    Err(e) => return Err(e),
                },
            }
        }
        Ok(())
    }
}

// The SHA-256 the R1 tells of a file of its own; none where it refuses, as for a file that went.
fn device_sum(console: &mut Console<impl Transport>, device_path: &str) -> Result<Option<String>> {
    let mut sum = None;
    let told = console.run(&Command::Sum(device_path.into()), |line| {
        if let ReplyLine::Data(text) = line {
            sum = Some(text.to_string());
        }
    });
    match told {
        Ok(()) => Ok(Some(sum.ok_or("the device told no sum")?)),
        Err(e) if e.is::<Refusal>() => Ok(None),
        Err(e) => Err(e),
    }
}

// Copies a file of the R1 into the records, and tells its SHA-256 and its length; none where the R1 refuses a read of it, as of a file that went, which its records then say.
fn copy<W: Write>(
    console: &mut Console<impl Transport>,
    path: &str,
    records: &mut records::Writer<W>,
    stopped: &AtomicBool,
) -> Result<Option<(String, u64)>> {
    records.begin(path)?;
    let mut sum = digest::Context::new(&digest::SHA256);
    let pulled = remote::pulled(console, &on_device(path), remote::hush, |part| {
        check(stopped)?;
        sum.update(part);
        records.part(part)
    });
    records.end()?;
    match pulled {
        Ok(bytes) => Ok(Some((to_hex(sum), bytes))),
        Err(e) if e.is::<Refusal>() => {
            records.gone(path)?;
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

// Copies what a backup takes into the records, and how many files and bytes that is. The R1 runs on meanwhile: the files are listed and held against the R1's own sums again, and what changed, came or went is copied or recorded, until one whole pass finds every file as it is recorded, so that the records hold the files as they all stood at one moment. What the R1 lists and refuses to read in two passes running ends the backup.
fn copy_all<W: Write>(
    console: &mut Console<impl Transport>,
    records: &mut records::Writer<W>,
    stopped: &AtomicBool,
) -> Result<(usize, u64)> {
    let mut held: BTreeMap<String, (String, u64)> = BTreeMap::new();
    let (mut refused_before, mut progress) = (HashSet::new(), Progress::default());
    loop {
        let Listing { files, mut refused } = Listing::take(console)?;
        let listed: HashSet<&str> = files.iter().map(String::as_str).collect();
        let went: Vec<String> = held.keys().filter(|path| !listed.contains(path.as_str())).cloned().collect();
        let mut changed = went.len();
        for path in went {
            records.gone(&path)?;
            held.remove(&path);
        }
        for (done, path) in files.iter().enumerate() {
            check(stopped)?;
            progress.tell(done, files.len());
            let recorded = held.get(path).map(|(sum, _)| sum);
            if recorded.is_some() && device_sum(console, &on_device(path))?.as_ref() == recorded {
                continue;
            }
            changed += 1;
            match copy(console, path, records, stopped)? {
                Some(copied) => drop(held.insert(path.clone(), copied)),
                None => {
                    held.remove(path);
                    refused.insert(path.clone());
                }
            }
        }
        progress.tell(files.len(), files.len());
        progress.end();
        if let Some(path) = refused.iter().find(|path| refused_before.contains(*path)) {
            return Err(format!("{path}: the R1 lists it and refuses to read it").into());
        }
        if changed == 0 && refused.is_empty() {
            return Ok((held.len(), held.values().map(|(_, bytes)| bytes).sum()));
        }
        eprintln!("{changed} files copied or gone; holding all against the R1's sums");
        refused_before = refused;
    }
}

fn write_backup(
    console: &mut Console,
    passphrase: &Passphrase,
    file: &Path,
    stopped: &AtomicBool,
) -> Result<(usize, u64)> {
    let out = BufWriter::new(File::create(file).map_err(|e| of(file, e))?);
    let mut records = records::Writer::new(age::Writer::new(out, passphrase.as_str())?)?;
    let copied = copy_all(console, &mut records, stopped)?;
    let written = records.finish()?.into_inner().map_err(|e| of(file, e.into_error()))?;
    written.sync_all().map_err(|e| of(file, e))?;
    Ok(copied)
}

/// `r1 backup`: copies what the chosen R1 keeps, but what is the device's own, into `file`, an age file under the passphrase of the R1's safe, which is asked for and opens the R1's gate.
pub fn backup(choice: &Choice, file: &Path) -> Result<()> {
    let mut console = remote::running_console(choice)?;
    let passphrase = unlock(&mut console, OF_SAFE, None)?;
    let partial = part(file);
    let (stopped, interrupts) = watch_interrupts()?;
    let copied = write_backup(&mut console, &passphrase, &partial, &stopped);
    let shut = console.run(&Command::Lock, remote::hush);
    interrupts.give_back()?;
    let (files, bytes) = copied.inspect_err(|_| drop(fs::remove_file(&partial)))?;
    shut?;
    fs::rename(&partial, file).map_err(|e| of(file, e))?;
    eprintln!("{}: {files} files, {bytes} bytes, under the passphrase of the R1's safe", file.display());
    Ok(())
}

fn open(file: &Path, passphrase: &Passphrase) -> Result<records::Reader<BufReader<File>>> {
    let input = BufReader::new(File::open(file).map_err(|e| of(file, e))?);
    records::Reader::new(age::Reader::open(input, passphrase.as_str())?)
}

// A backup opened under its passphrase, which is asked for, at a terminal for as long as the one given is another's.
fn open_asked(file: &Path) -> Result<(Passphrase, records::Reader<BufReader<File>>)> {
    prompt::ask(OF_BACKUP, |typed| {
        let passphrase = Passphrase::from(typed.ok_or("no passphrase given")?.as_str());
        match open(file, &passphrase) {
            Ok(records) => Ok(Attempt::Taken((passphrase, records))),
            Err(e) if e.is::<age::WrongPassphrase>() => Ok(Attempt::Refused(e)),
            Err(e) => Err(e),
        }
    })
}

// The number of the record each file of a backup is last recorded by, counted among its files' records, for every file the backup holds at its end. Read to its end, the backup is whole.
fn last_records<R: Read>(mut records: records::Reader<R>) -> Result<HashMap<String, usize>> {
    let (mut last, mut number) = (HashMap::new(), 0);
    loop {
        match records.next()? {
            Record::File(path) => {
                last.insert(path, number);
                number += 1;
            }
            Record::Gone(path) => drop(last.remove(&path)),
            Record::Part(_) | Record::FileEnd => {}
            Record::End => return Ok(last),
        }
    }
}

// A file as it is staged on the R1: where, the sum of what went there, and whether anything did.
struct Staged {
    device_path: String,
    sum: digest::Context,
    is_begun: bool,
}

// Stages each file of a backup on the R1 by its last record, and holds each to the sum the R1 tells of it; how many files and bytes were staged.
fn stage_all<R: Read>(
    console: &mut Console<impl Transport>,
    mut records: records::Reader<R>,
    last: &HashMap<String, usize>,
    stopped: &AtomicBool,
) -> Result<(usize, u64)> {
    console.run(&Command::Stage, remote::hush)?;
    let (mut number, mut bytes, mut made) = (0, 0u64, HashSet::new());
    let (mut staged, mut done, mut progress): (Option<Staged>, _, _) = (None, 0, Progress::default());
    loop {
        match records.next()? {
            Record::File(path) => {
                check(stopped)?;
                if last.get(&path) == Some(&number) {
                    progress.tell(done, last.len());
                    done += 1;
                    let device_path = on_device(&format!("{}/{path}", staging()));
                    let directory = device_path.rsplit_once('/').map_or(MOUNT_POINT, |(directory, _)| directory);
                    if made.insert(directory.to_string()) {
                        console.run(&Command::MakeDirectory(directory.into()), remote::hush)?;
                    }
                    staged = Some(Staged { device_path, sum: digest::Context::new(&digest::SHA256), is_begun: false });
                }
                number += 1;
            }
            Record::Part(part) => {
                if let Some(file) = &mut staged {
                    check(stopped)?;
                    remote::append(console, &file.device_path, &part, remote::hush)?;
                    file.sum.update(&part);
                    file.is_begun = true;
                    bytes += part.len() as u64;
                }
            }
            Record::FileEnd => {
                if let Some(file) = staged.take() {
                    if !file.is_begun {
                        remote::append(console, &file.device_path, &[], remote::hush)?;
                    }
                    if device_sum(console, &file.device_path)? != Some(to_hex(file.sum)) {
                        return Err(format!("{}: the R1 holds other bytes than the backup", file.device_path).into());
                    }
                }
            }
            Record::Gone(_) => {}
            Record::End => {
                progress.tell(done, last.len());
                return Ok((last.len(), bytes));
            }
        }
    }
}

/// `r1 restore`: stages the files of the backup in `file` on the chosen R1 and restarts it; the R1 puts them in its partition's place as its safe is next opened, keeping what is the device's own. The backup is read whole before the R1 is asked anything.
pub fn restore(choice: &Choice, file: &Path) -> Result<()> {
    let (passphrase, records) = open_asked(file)?;
    let last = last_records(records)?;
    let mut console = remote::running_console(choice)?;
    unlock(&mut console, OF_THIS_SAFE, Some(&passphrase))?;
    let (stopped, interrupts) = watch_interrupts()?;
    let staged = open(file, &passphrase)
        .and_then(|records| stage_all(&mut console, records, &last, &stopped))
        .and_then(|staged| console.run(&Command::Commit, remote::hush).map(|()| staged));
    let shut = console.run(&Command::Lock, remote::hush);
    interrupts.give_back()?;
    let (files, bytes) = staged?;
    shut?;
    console.send(&Command::Reboot)?;
    eprintln!(
        "{files} files, {bytes} bytes staged: the R1 restarts, and puts them in place once its passphrase is typed"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::age::samples;
    use super::*;
    use crate::link::console::samples::{console, device};
    use crate::remote::samples::{Device, Files};
    use r1_funcore::domain::console::command::READ_MOST;

    // The records of a backup's file, and the record each of its files is last recorded by.
    fn read_back(file: &[u8]) -> (records::Reader<&[u8]>, HashMap<String, usize>) {
        let records = || records::Reader::new(samples::reader(file).unwrap()).unwrap();
        (records(), last_records(records()).unwrap())
    }

    #[test]
    fn backup_of_changing_files() {
        let long: Vec<u8> = (0..READ_MOST as usize + 5).map(|at| (at % 251) as u8).collect();
        let mut keeping = Device::keeping(&[
            ("/data/camera/pictures/a.jpg", b"first"),
            ("/data/camera/pictures/b.jpg.new", b"half written"),
            ("/data/camera/videos/v.mp4", &long),
            ("/data/chat/core.md", b"core"),
            ("/data/news/empty.md", b""),
            ("/data/.r1metal/openai/key", b"sk"),
            ("/data/.r1metal/battery/gauge", b"the device's own"),
        ]);
        keeping.links.insert("/data/camera/latest".into());
        let mut passes = 0;
        keeping.meanwhile = Some(Box::new(move |command, files| {
            passes += usize::from(*command == Command::Ls("/data/camera".into()));
            if passes == 2 {
                files.insert("/data/camera/pictures/a.jpg".into(), b"second".to_vec());
                files.remove("/data/chat/core.md");
                files.insert("/data/audio/notes/n.m4a".into(), b"note".to_vec());
            }
        }));
        let (mut source, stopped) = (console(keeping, None), AtomicBool::new(false));
        let mut records = records::Writer::new(samples::writer()).unwrap();
        let copied = copy_all(&mut source, &mut records, &stopped).unwrap();
        let held: [(&str, &[u8]); 5] = [
            ("camera/pictures/a.jpg", b"second"),
            ("camera/videos/v.mp4", &long),
            ("audio/notes/n.m4a", b"note"),
            ("news/empty.md", b""),
            (".r1metal/openai/key", b"sk"),
        ];
        assert_eq!(copied, (5, held.iter().map(|(_, bytes)| bytes.len() as u64).sum()));
        let listings = device(&mut source).taken.iter().filter(|line| *line == "ls /data/camera").count();
        assert_eq!(listings, 3, "the backup ends with the pass that finds every file as it is recorded");
        let file = records.finish().unwrap();
        let (records, last) = read_back(&file);
        let mut target = console(Device::default(), None);
        assert_eq!(stage_all(&mut target, records, &last, &stopped).unwrap(), copied);
        let staged = device(&mut target);
        let expected: Files =
            held.iter().map(|(path, bytes)| (format!("/data/.restore.new/{path}"), bytes.to_vec())).collect();
        assert!(
            staged.files == expected,
            "the files as the last pass found them: none that went, none half written, none of the device's own"
        );
        let made: Vec<&str> = staged.taken.iter().filter_map(|line| line.strip_prefix("mkdir ")).collect();
        assert_eq!(staged.taken[0], "stage", "the staging is made before a file goes into it");
        assert_eq!(
            made,
            [
                "/data/.restore.new/camera/videos",
                "/data/.restore.new/news",
                "/data/.restore.new/.r1metal/openai",
                "/data/.restore.new/camera/pictures",
                "/data/.restore.new/audio/notes"
            ],
            "each file is staged once, by its last record, its directory made before it"
        );
    }

    #[test]
    fn refused_twice_ends_backup() {
        let backed_up = |device: Device| {
            let mut records = records::Writer::new(samples::writer()).unwrap();
            copy_all(&mut console(device, None), &mut records, &AtomicBool::new(false)).map_err(|e| e.to_string())
        };
        let files: [(&str, &[u8]); 2] = [("/data/camera/a.jpg", b"a"), ("/data/chat/sealed/core.md", b"core")];
        for (refused, path) in [("/data/camera/a.jpg", "camera/a.jpg"), ("/data/chat/sealed", "chat/sealed")] {
            let mut device = Device::keeping(&files);
            device.refused.insert(refused.into());
            let ended = Err(format!("{path}: the R1 lists it and refuses to read it"));
            assert_eq!(backed_up(device), ended, "a file it does not read, a directory it does not list");
        }
        let mut device = Device::keeping(&files);
        device.refused.insert("/data/camera/a.jpg".into());
        let mut passes = 0;
        device.meanwhile = Some(Box::new(move |command, files| {
            passes += usize::from(*command == Command::Ls("/data/camera".into()));
            if passes == 2 {
                files.remove("/data/camera/a.jpg");
            }
        }));
        assert_eq!(backed_up(device), Ok((1, 4)), "a file refused once that the next pass lists no more went");
    }

    #[test]
    fn stale_staged_file_refused() {
        let mut writer = records::Writer::new(samples::writer()).unwrap();
        writer.begin("chat/core.md").unwrap();
        writer.part(b"core").unwrap();
        writer.end().unwrap();
        let file = writer.finish().unwrap();
        let (records, last) = read_back(&file);
        let stale = Device::keeping(&[("/data/.restore.new/chat/core.md", b"stale")]);
        let refused = stage_all(&mut console(stale, None), records, &last, &AtomicBool::new(false)).unwrap_err();
        assert_eq!(refused.to_string(), "/data/.restore.new/chat/core.md: the R1 holds other bytes than the backup");
    }

    #[test]
    fn last_record_counts() {
        let mut writer = records::Writer::new(samples::writer()).unwrap();
        for (path, content) in [("chat/core.md", "first"), ("camera/a.jpg", "a"), ("chat/core.md", "second")] {
            writer.begin(path).unwrap();
            writer.part(content.as_bytes()).unwrap();
            writer.end().unwrap();
        }
        writer.gone("camera/a.jpg").unwrap();
        writer.begin("news/x.md").unwrap();
        writer.end().unwrap();
        let file = writer.finish().unwrap();
        let last = last_records(records::Reader::new(samples::reader(&file).unwrap()).unwrap()).unwrap();
        let mut numbered: Vec<(&str, usize)> = last.iter().map(|(path, number)| (path.as_str(), *number)).collect();
        numbered.sort_unstable();
        assert_eq!(numbered, [("chat/core.md", 2), ("news/x.md", 3)]);
        let cut = &file[..file.len() - 1];
        assert!(
            samples::reader(cut).and_then(records::Reader::new).and_then(last_records).is_err(),
            "a backup cut short is found so before an R1 is asked anything"
        );
    }

    #[test]
    fn device_paths() {
        assert_eq!(on_device(".r1metal/signal/account"), "/data/.r1metal/signal/account");
        assert_eq!(on_device(&format!("{}/camera/a.jpg", staging())), "/data/.restore.new/camera/a.jpg");
    }
}

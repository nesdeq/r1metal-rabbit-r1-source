//! What `r1` asks of a running device over its console: commands, files both ways, the logs, the time.

use crate::link::console::{Console, Refusal};
use crate::link::{Choice, Guise, Transport, guise, plugged_in};
use crate::{Result, link, of, part, read};
use nusb::DeviceInfo;
use r1_funcore::base::code;
use r1_funcore::base::file_names;
use r1_funcore::domain::console::command::{Command, READ_MOST};
use r1_funcore::domain::console::shell::{EntryKind, parse_listing_line};
use r1_funcore::domain::console::wire::{LINE_BYTES, ReplyLine};
use std::fs;
use std::io::Write;
use std::path::Path;

/// `r1 sh -` reads commands from stdin.
pub const BATCH_ARGUMENT: &str = "-";
// The verb that, given nothing, sets the device's clocks to this computer's.
const CLOCK: &str = "clock";
const BATCH_HEADER: &str = "$ ";
const BATCH_FAILURE: &str = "! ";
// Bytes sent in one `append`: as base64, four characters for every three bytes, they take half the line the device takes.
const PUSH_CHUNK: usize = LINE_BYTES / 2 / 4 * 3;
/// Where on this computer a device's logs go unless told otherwise: a directory of the workspace git ignores, since they name networks, addresses and the device.
pub const DEFAULT_LOGS: &str = "logs/device";

/// Waits for the device that runs r1metal and is the one chosen, telling the user so where it is not there yet; several that could be it are refused.
pub fn running_device(choice: &Choice) -> Result<DeviceInfo> {
    let running = |guise: &Guise| matches!(guise, Guise::Running);
    if plugged_in(choice, running)?.0.is_none() {
        link::tell_what_macos_asks();
        eprintln!("waiting for an R1 running r1metal: plug it in");
    }
    let chosen = |device: &DeviceInfo| guise(device).as_ref().is_some_and(running) && choice.takes(device);
    let (info, ()) = link::wait_for(|device| chosen(device).then_some(()))?;
    Ok(info)
}

/// Waits for that device and opens its console.
pub fn running_console(choice: &Choice) -> Result<Console> {
    Console::open(&running_device(choice)?)
}

// What is done with a line of the device's that is no answer's verdict.
type Tell = fn(ReplyLine<'_>);

pub fn show(line: ReplyLine<'_>) {
    match line {
        ReplyLine::Data(text) => println!("{text}"),
        ReplyLine::Log(text) => eprintln!("# {text}"),
        ReplyLine::Unknown(text) if !text.is_empty() => eprintln!("? {text}"),
        _ => {}
    }
}

/// Tells nothing of a line: the device's own log lines come between the answers of every command, which a long run of commands would bury what it tells under.
pub fn hush(_: ReplyLine<'_>) {}

/// The command a line names, checked as the device checks it; `clock` alone sets the device's clocks to this computer's.
pub fn command(line: &str) -> Result<Command> {
    if line.trim() == CLOCK {
        let (utc, offset) = time_and_offset()?;
        return Ok(Command::Clock { utc, offset });
    }
    Command::parse(line).map_err(|e| e.message().into())
}

pub fn send_or_run(console: &mut Console, command: &Command) -> Result<()> {
    if command.ends_link() { console.send(command) } else { console.run(command, show) }
}

/// Runs every stdin line as a command over one connection, heading each reply with `$ line`; a failing link ends the batch.
pub fn run_batch(console: &mut Console) -> Result<()> {
    for line in std::io::stdin().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        println!("{BATCH_HEADER}{line}");
        let command = match command(&line) {
            Ok(command) => command,
            Err(e) => {
                println!("{BATCH_FAILURE}{e}");
                continue;
            }
        };
        match send_or_run(console, &command) {
            Ok(()) => {}
            Err(e) if e.is::<Refusal>() => println!("{BATCH_FAILURE}{e}"),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Hands the device's file part by part to `take` until a part comes back short; how many bytes it holds.
pub fn pulled(
    console: &mut Console<impl Transport>,
    device_path: &str,
    tell: Tell,
    mut take: impl FnMut(&[u8]) -> Result<()>,
) -> Result<u64> {
    let mut offset = 0;
    loop {
        let mut part = None;
        let request = Command::Read { path: device_path.into(), offset, length: READ_MOST };
        console.run(&request, |line| match line {
            ReplyLine::Data(text) => part = code::unbase64(text),
            other => tell(other),
        })?;
        let part = part.ok_or("the device answered with something that is no base64")?;
        take(&part)?;
        offset += part.len() as u64;
        if part.len() < READ_MOST as usize {
            return Ok(offset);
        }
    }
}

/// Copies the device's file or directory to `local`.
pub fn pull(console: &mut Console<impl Transport>, device_path: &str, local: &Path) -> Result<()> {
    match entries(console, device_path, show) {
        Ok(names) => pull_directory(console, device_path, &names, local),
        // A file is no directory to list; whatever else is wrong, reading it tells.
        Err(e) if e.is::<Refusal>() => pull_file(console, device_path, local),
        Err(e) => Err(e),
    }
}

/// The entries of a directory of the device, as `ls` names them.
pub fn entries(console: &mut Console<impl Transport>, device_path: &str, tell: Tell) -> Result<Vec<String>> {
    let mut names = Vec::new();
    console.run(&Command::Ls(device_path.into()), |line| match line {
        ReplyLine::Data(name) => names.push(name.to_string()),
        other => tell(other),
    })?;
    Ok(names)
}

// Copies the device's file to `local`, which takes its place once it has come whole; a file that does not come leaves `local` as it was.
fn pull_file(console: &mut Console<impl Transport>, device_path: &str, local: &Path) -> Result<()> {
    let partial = part(local);
    let came = fs::File::create(&partial).map_err(|e| of(&partial, e)).and_then(|mut file| {
        pulled(console, device_path, show, |part| file.write_all(part).map_err(|e| of(&partial, e)))
    });
    let bytes = came.inspect_err(|_| drop(fs::remove_file(&partial)))?;
    fs::rename(&partial, local).map_err(|e| of(local, e))?;
    eprintln!("{}: {bytes} bytes", local.display());
    Ok(())
}

// Copies a directory of the device, whose entries are `names`, to `local` with every file and directory in it; symbolic links are left out, and a name that would lead out of the directory, one `file_names::is_plain` does not take, is refused.
fn pull_directory(
    console: &mut Console<impl Transport>,
    device_path: &str,
    names: &[String],
    local: &Path,
) -> Result<()> {
    fs::create_dir_all(local).map_err(|e| of(local, e))?;
    for name in names {
        let (inner, kind) = parse_listing_line(name);
        if kind == EntryKind::Link {
            continue;
        }
        if !file_names::is_plain(inner) {
            return Err(format!("{device_path}: the device names {name:?}, which is no name of a thing in it").into());
        }
        let (there, here) = (format!("{device_path}/{inner}"), local.join(inner));
        if kind == EntryKind::Directory {
            let names = entries(console, &there, show)?;
            pull_directory(console, &there, &names, &here)?;
        } else {
            pull_file(console, &there, &here)?;
        }
    }
    Ok(())
}

/// Appends `bytes` to the device's file part by part, making the file where it is missing; none appended makes it too.
pub fn append(console: &mut Console<impl Transport>, device_path: &str, bytes: &[u8], tell: Tell) -> Result<()> {
    let empty: &[u8] = &[];
    for part in bytes.chunks(PUSH_CHUNK).chain(bytes.is_empty().then_some(empty)) {
        console.run(&Command::Append { path: device_path.into(), data: code::base64(part) }, tell)?;
    }
    Ok(())
}

/// Deletes the device's file, if there is one, and writes `local` in its place part by part.
pub fn push(console: &mut Console, local: &Path, device_path: &str) -> Result<()> {
    let bytes = read(local)?;
    match console.run(&Command::Remove(device_path.into()), show) {
        Err(e) if !e.is::<Refusal>() => return Err(e),
        _ => {}
    }
    append(console, device_path, &bytes, show)?;
    eprintln!("{device_path}: {} bytes", bytes.len());
    Ok(())
}

// The time in seconds of UTC since the start of 1970, the count an R1 keeps, and how many minutes this computer's local time is ahead of it now.
fn time_and_offset() -> Result<(u64, i16)> {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
    if unsafe { libc::localtime_r(&now, &mut local) }.is_null() {
        return Err("this computer's local time cannot be worked out".into());
    }
    Ok((u64::try_from(now)?, i16::try_from(local.tm_gmtoff / 60)?))
}

#[cfg(test)]
pub mod samples {
    use super::*;
    use aws_lc_rs::digest;
    use r1_funcore::domain::console::shell::{NOT_A_VERB, NOT_BASE64, to_listing};
    use r1_funcore::domain::console::wire::Reply;
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    const REFUSED: &str = "no such file or directory";

    /// The files a scripted R1 keeps, by their paths.
    pub type Files = BTreeMap<String, Vec<u8>>;
    // What a scripted R1 does to its files before it takes a command.
    type Meanwhile = Box<dyn FnMut(&Command, &mut Files)>;

    /// A running R1's debug shell as the host meets it: its files, what it lists and refuses, and every line it took.
    #[derive(Default)]
    pub struct Device {
        pub files: Files,
        pub links: BTreeSet<String>,
        /// The files and directories it lists and refuses to read, to sum or to list.
        pub refused: BTreeSet<String>,
        /// As an R1 that runs on while it is backed up changes them.
        pub meanwhile: Option<Meanwhile>,
        pub taken: Vec<String>,
        queued: Vec<u8>,
    }

    impl Device {
        /// A device that keeps those files.
        pub fn keeping(files: &[(&str, &[u8])]) -> Device {
            let files = files.iter().map(|(path, bytes)| (path.to_string(), bytes.to_vec())).collect();
            Device { files, ..Device::default() }
        }

        fn file(&self, path: &str) -> std::result::Result<&Vec<u8>, &'static str> {
            self.files.get(path).filter(|_| !self.refused.contains(path)).ok_or(REFUSED)
        }

        fn listing(&self, directory: &str) -> std::result::Result<Vec<String>, &'static str> {
            if self.refused.contains(directory) || self.files.contains_key(directory) {
                return Err(REFUSED);
            }
            let inside = format!("{directory}/");
            let files = self.files.keys().filter_map(|path| path.strip_prefix(&inside)).map(|rest| {
                match rest.split_once('/') {
                    Some((name, _)) => (name.to_string(), EntryKind::Directory),
                    None => (rest.to_string(), EntryKind::File),
                }
            });
            let links = self.links.iter().filter_map(|path| path.strip_prefix(&inside));
            let entries: BTreeMap<String, EntryKind> =
                files.chain(links.map(|name| (name.to_string(), EntryKind::Link))).collect();
            Ok(to_listing(entries))
        }

        // The lines of data a command is answered with, or its refusal.
        fn answer(&mut self, command: &Command) -> std::result::Result<Vec<String>, &'static str> {
            match command {
                Command::Ls(path) => self.listing(path),
                Command::Sum(path) => {
                    self.file(path).map(|bytes| vec![code::hex(digest::digest(&digest::SHA256, bytes).as_ref())])
                }
                Command::Read { path, offset, length } => self.file(path).map(|bytes| {
                    let from = usize::try_from(*offset).map_or(bytes.len(), |from| from.min(bytes.len()));
                    vec![code::base64(&bytes[from..(from + *length as usize).min(bytes.len())])]
                }),
                Command::Append { path, data } => {
                    let bytes = code::unbase64(data).ok_or(NOT_BASE64)?;
                    self.files.entry(path.clone()).or_default().extend(bytes);
                    Ok(Vec::new())
                }
                Command::MakeDirectory(_) | Command::Stage => Ok(Vec::new()),
                _ => Err(NOT_A_VERB),
            }
        }
    }

    impl Transport for Device {
        fn send(&mut self, data: &[u8]) -> Result<()> {
            let command = Command::parse(std::str::from_utf8(data)?).map_err(|e| e.message())?;
            if let Some(meanwhile) = &mut self.meanwhile {
                meanwhile(&command, &mut self.files);
            }
            self.taken.push(command.encode());
            let mut reply = Reply::default();
            match self.answer(&command) {
                Ok(lines) => {
                    lines.iter().for_each(|line| reply.data(line));
                    reply.ok();
                }
                Err(why) => drop(reply.err(why)),
            }
            self.queued.extend(reply.bytes());
            Ok(())
        }

        fn receive_within(&mut self, _: Duration) -> Result<Vec<u8>> {
            if self.queued.is_empty() {
                return Err("the device sent nothing".into());
            }
            Ok(std::mem::take(&mut self.queued))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::Device;
    use super::*;
    use crate::link::console::samples::console;

    #[test]
    fn append_fits_line() {
        let data = code::base64(&[0; PUSH_CHUNK]);
        assert_eq!(data.len(), LINE_BYTES / 2, "the part takes half the line");
        let line = Command::Append { path: "/data/camera/videos/20261018_120000_1.mp4".into(), data }.encode();
        assert!(line.len() < LINE_BYTES, "{} bytes", line.len());
    }

    #[test]
    fn directory_pull() {
        let root = std::env::temp_dir().join(format!("r1-pull-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let long: Vec<u8> = (0..READ_MOST as usize + 5).map(|at| (at % 251) as u8).collect();
        let full = vec![7u8; READ_MOST as usize];
        let mut device = Device::keeping(&[
            ("/data/camera/pictures/a.jpg", &long),
            ("/data/camera/full", &full),
            ("/data/camera/empty", b""),
        ]);
        device.links.insert("/data/camera/latest".into());
        let mut r1 = console(device, None);
        pull(&mut r1, "/data/camera", &root.join("camera")).unwrap();
        let read = |path: &str| fs::read(root.join(path)).unwrap();
        assert!(read("camera/pictures/a.jpg") == long, "a file of more than one read, in a directory of the directory");
        assert!(read("camera/full") == full, "a file that ends with a full read ends at the empty one behind it");
        assert!(read("camera/empty").is_empty() && !root.join("camera/latest").exists(), "a link is left out");
        pull(&mut r1, "/data/camera/full", &root.join("one")).unwrap();
        assert!(read("one") == full && !part(&root.join("one")).exists(), "a file, which is no directory to list");
        let mut kept = Device::keeping(&[("/data/safe", b"sealed")]);
        kept.refused.insert("/data/safe".into());
        let unread = pull(&mut console(kept, None), "/data/safe", &root.join("safe")).unwrap_err();
        assert!(unread.is::<Refusal>(), "{unread}");
        assert!(!root.join("safe").exists() && !part(&root.join("safe")).exists(), "and nothing of it is left here");
        let mut leading_out = console(Device::keeping(&[("/data/notes/../.ssh/keys", b"")]), None);
        let refused = pull(&mut leading_out, "/data/notes", &root.join("notes")).unwrap_err().to_string();
        assert_eq!(refused, "/data/notes: the device names \"../\", which is no name of a thing in it");
        assert!(!root.join(".ssh").exists(), "nothing is written beside the directory pulled into");
        fs::remove_dir_all(root).unwrap();
    }
}

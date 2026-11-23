//! The debug shell's denied paths, the gate the safe's passphrase opens, its path resolution and its listings.

use super::command::{Command, READ_MOST};
use super::wire::Reply;
use crate::base::handoff::Orders;
use crate::domain::{backup, router};
use crate::net::wireguard::config::{Config, Flaw};
use crate::signal::device;
use crate::storage::paths::new_name;
use crate::storage::safe::{ATTEMPTS, KEY_FILES, Refusal};
use crate::storage::{MOUNT_POINT, RESTORE};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const DIRECTORY_MARK: char = '/';
const LINK_MARK: char = '@';
/// The answer to a verb on a denied path.
pub const DENIAL: &str = "the shell reaches neither Signal's files, the tunnel's config, the safe's key files nor a restore's before the safe's passphrase is given, nor what is held in memory alone, nor climbs where nothing is yet";
/// The answer to a verb the gate keeps while it is shut.
pub const LOCKED: &str = "the safe's passphrase is not given";
/// The answer to a passphrase that is not the safe's.
pub const WRONG: &str = "a wrong passphrase";
// The answer to a passphrase once the wrong ones of this run are spent.
const SPENT: &str = "three wrong passphrases: no other is taken before the R1 starts anew";
/// The answer to a passphrase the safe could not be asked about.
pub const UNCHECKED: &str = "the passphrase could not be checked: the safe is not open, or no key file reads";
const PROCESSES: &str = "/proc/";
const ALL_MEMORY: &str = "/dev/mem";
const MEMORY_FILE: &str = "/memfd:";
// The kernel's record of the modem's ports, which holds every write to `ttyC0`, a PIN's among them.
const MODEM_RECORD: &str = "/proc/ccci_dump";
const CLIMB: &str = "..";
/// The answer to an append whose data is no base64.
pub const NOT_BASE64: &str = "not base64";
/// The answer to a command that is no shell verb.
pub const NOT_A_VERB: &str = "not a shell verb";

/// A relay of the shell's filesystem and of its path type.
pub trait Paths {
    type Path: Clone + Default + PartialEq;

    fn current_dir(&self) -> Self::Path;
    // None where the path is not there.
    fn canonical(&mut self, path: &Self::Path) -> Option<Self::Path>;
    // None for anything but a symbolic link.
    fn read_link(&mut self, path: &Self::Path) -> Option<Self::Path>;
    fn parent(&self, path: &Self::Path) -> Option<Self::Path>;
    fn join(&self, directory: &Self::Path, path: &Self::Path) -> Self::Path;
    fn parts(&self, path: &Self::Path) -> Vec<Self::Path>;
    fn join_parts(&self, parts: &[Self::Path]) -> Self::Path;
    fn to_text(&self, path: &Self::Path) -> String;
}

fn to_existing_prefix<P: Paths>(paths: &mut P, path: &P::Path) -> String {
    let path = paths.join(&paths.current_dir(), path);
    let parts = paths.parts(&path);
    let prefix =
        (1..=parts.len()).rev().find_map(|count| Some((paths.canonical(&paths.join_parts(&parts[..count]))?, count)));
    prefix.map_or_else(String::new, |(real, count)| {
        let rest: Vec<String> = parts[count..].iter().map(|part| paths.to_text(part)).collect();
        join_path(&paths.to_text(&real), rest.iter().map(String::as_str))
    })
}

fn to_real_path<P: Paths>(paths: &mut P, path: &P::Path) -> String {
    let (mut path, mut visited) = (paths.join(&paths.current_dir(), path), Vec::new());
    while !visited.contains(&path) {
        if let Some(real) = paths.canonical(&path) {
            return paths.to_text(&real);
        }
        let Some(target) = paths.read_link(&path) else { break };
        let directory = paths.parent(&path).and_then(|parent| paths.canonical(&parent)).unwrap_or_default();
        let next = paths.join(&directory, &target);
        visited.push(core::mem::replace(&mut path, next));
    }
    to_existing_prefix(paths, &path)
}

/// True for a path that no verb reaches, by any link; Signal's files, the safe's key files and a restore's are reached once the gate is open.
pub fn is_path_denied<P: Paths>(paths: &mut P, path: &P::Path, is_unlocked: bool) -> bool {
    is_denied(&to_real_path(paths, path), is_unlocked)
}

/// Whether the host has given the safe's passphrase, and how many wrong ones this run took.
#[derive(Default, Debug)]
pub struct Gate {
    wrong: u32,
    is_open: bool,
    is_checking: bool,
}

impl Gate {
    /// Begins the check of a passphrase; the answer where this run's wrong ones are spent.
    pub fn begin(&mut self) -> Result<(), &'static str> {
        self.is_checking = self.wrong < ATTEMPTS;
        if self.is_checking { Ok(()) } else { Err(SPENT) }
    }

    /// Ends the check with the safe's word on the passphrase: the gate opens where it is the safe's own and no lock came since the check began.
    pub fn end(&mut self, outcome: Result<(), Refusal>) -> Result<(), &'static str> {
        let was_checking = core::mem::take(&mut self.is_checking);
        match outcome {
            Ok(()) if was_checking => {
                self.is_open = true;
                Ok(())
            }
            Ok(()) => Err(LOCKED),
            Err(Refusal::Wrong) => {
                self.wrong += 1;
                Err(WRONG)
            }
            Err(Refusal::Failed) => Err(UNCHECKED),
        }
    }

    /// Shuts the gate, as the host asks or as it leaves; a check under way opens nothing.
    pub const fn lock(&mut self) {
        self.is_open = false;
        self.is_checking = false;
    }

    pub const fn is_open(&self) -> bool {
        self.is_open
    }
}

/// The state of the shell's thread and the main loop: the verbs to run, their replies in order, and the gate.
#[derive(Default)]
pub struct State {
    pub orders: Orders<Command>,
    pub report: Vec<Reply>,
    pub gate: Gate,
}

impl State {
    // True for a verb the gate keeps from running while it is shut.
    const fn is_kept(&self, command: &Command) -> bool {
        command.needs_gate() && !self.gate.is_open()
    }

    /// Whether a verb runs, `is_denied` where its path is one no verb reaches: the answer of a denied path before that of a verb the shut gate keeps.
    pub const fn check(&self, command: &Command, is_denied: bool) -> Result<(), &'static str> {
        if is_denied {
            Err(DENIAL)
        } else if self.is_kept(command) {
            Err(LOCKED)
        } else {
            Ok(())
        }
    }

    /// The config the verb that hands one over carries, where the gate is open and its text is one the router keeps; else the words of the failed answer.
    pub fn read_tunnel(&self, command: &Command) -> Result<Config, &'static str> {
        let Command::Tunnel(code) = command else {
            return Err(NOT_A_VERB);
        };
        self.check(command, false)?;
        router::read_handover(code).map_err(Flaw::message)
    }
}

/// The most bytes `cat` and `sum` take of a file: all of a file that ends, and of what has no end, a device or a pipe, one read's part of what it has at hand.
pub const fn most_read(is_regular: bool) -> u64 {
    if is_regular { u64::MAX } else { READ_MOST as u64 }
}

fn is_denied(path: &str, is_unlocked: bool) -> bool {
    let is_under = |directory: &str| {
        let directory = format!("{MOUNT_POINT}/{directory}");
        path.strip_prefix(directory.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    // A key file holds the safe's key sealed under the passphrase, which whoever reads it may try passphrases on.
    let is_key_file = KEY_FILES.iter().any(|file| is_under(file) || is_under(&new_name(file)));
    let is_private = is_under(device::DIRECTORY) || is_under(router::DIRECTORY);
    let is_gated = is_private || is_key_file || is_under(RESTORE) || is_under(&backup::staging());
    let is_number = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let is_process_memory =
        path.strip_prefix(PROCESSES).is_some_and(|rest| match rest.split('/').collect::<Vec<&str>>()[..] {
            [process, "mem"] => is_number(process),
            [process, "task", thread, "mem"] => is_number(process) && is_number(thread),
            _ => false,
        });
    let is_secret = path == ALL_MEMORY || path == MODEM_RECORD || path.starts_with(MEMORY_FILE);
    is_gated && !is_unlocked || is_process_memory || is_secret || path.split('/').any(|part| part == CLIMB)
}

// Missing parts stay as written, a climb among them, which `is_denied` refuses.
fn join_path<'a>(base: &'a str, rest: impl IntoIterator<Item = &'a str>) -> String {
    let parts: Vec<&str> = base.split('/').chain(rest).filter(|part| !part.is_empty() && *part != ".").collect();
    format!("/{}", parts.join("/"))
}

/// The kind of a directory entry, as `ls` marks it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    Directory,
    Link,
    File,
}

/// The sorted lines of `ls`: each name, with `/` behind a directory and `@` behind a symbolic link.
pub fn to_listing(entries: impl IntoIterator<Item = (String, EntryKind)>) -> Vec<String> {
    let mut lines: Vec<String> = entries
        .into_iter()
        .map(|(mut name, kind)| {
            match kind {
                EntryKind::Directory => name.push(DIRECTORY_MARK),
                EntryKind::Link => name.push(LINK_MARK),
                EntryKind::File => {}
            }
            name
        })
        .collect();
    lines.sort();
    lines
}

/// The name and kind in a line of `ls`.
pub fn parse_listing_line(line: &str) -> (&str, EntryKind) {
    match (line.strip_suffix(DIRECTORY_MARK), line.strip_suffix(LINK_MARK)) {
        (Some(name), _) => (name, EntryKind::Directory),
        (None, Some(name)) => (name, EntryKind::Link),
        (None, None) => (line, EntryKind::File),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    struct Tree {
        files: Vec<&'static str>,
        links: Vec<(&'static str, &'static str)>,
    }

    impl Paths for Tree {
        type Path = String;

        fn current_dir(&self) -> String {
            String::from("/here")
        }

        fn canonical(&mut self, path: &String) -> Option<String> {
            self.files.contains(&path.as_str()).then(|| path.clone())
        }

        fn read_link(&mut self, path: &String) -> Option<String> {
            self.links.iter().find(|link| link.0 == path).map(|link| String::from(link.1))
        }

        fn parent(&self, path: &String) -> Option<String> {
            path.rsplit_once('/').map(|(parent, _)| String::from(parent))
        }

        fn join(&self, directory: &String, path: &String) -> String {
            if path.starts_with('/') { path.clone() } else { format!("{directory}/{path}") }
        }

        fn parts(&self, path: &String) -> Vec<String> {
            path.split_inclusive('/').map(|part| String::from(part.trim_end_matches('/'))).collect()
        }

        fn join_parts(&self, parts: &[String]) -> String {
            parts.join("/")
        }

        fn to_text(&self, path: &String) -> String {
            path.clone()
        }
    }

    #[test]
    fn links_followed() {
        let mut tree = Tree {
            files: vec!["", "/here", "/data", "/data/.r1metal", "/data/.r1metal/signal", "/data/open"],
            links: vec![
                ("/here/dangling", "/data/.r1metal/signal/new"),
                ("/here/round", "/here/about"),
                ("/here/about", "/here/round"),
            ],
        };
        let denied = |tree: &mut Tree, path: &str| is_path_denied(tree, &String::from(path), false);
        assert!(denied(&mut tree, "/data/.r1metal/signal"), "as it stands");
        assert!(denied(&mut tree, "dangling"), "through a link to what is not there yet");
        assert!(!denied(&mut tree, "/data/open") && !denied(&mut tree, "/data/new"), "any other");
        assert!(!denied(&mut tree, "round"), "links that lead round end");
        assert!(
            !is_path_denied(&mut tree, &String::from("dangling"), true),
            "and reached through the link once the gate is open"
        );
    }

    #[test]
    fn passphrase_opens_gate() {
        let mut gate = Gate::default();
        assert!(!gate.is_open(), "shut until a passphrase is given");
        assert_eq!((gate.begin(), gate.end(Err(Refusal::Failed))), (Ok(()), Err(UNCHECKED)));
        assert_eq!((gate.begin(), gate.end(Ok(()))), (Ok(()), Ok(())));
        assert!(gate.is_open());
        gate.lock();
        assert!(!gate.is_open(), "the host shuts it again");
        assert_eq!(gate.begin(), Ok(()));
        gate.lock();
        assert_eq!(gate.end(Ok(())), Err(LOCKED), "a host that left while its passphrase was checked opens nothing");
        assert!(!gate.is_open());
        for _ in 0..ATTEMPTS {
            assert_eq!((gate.begin(), gate.end(Err(Refusal::Wrong))), (Ok(()), Err(WRONG)));
        }
        assert_eq!((ATTEMPTS, gate.begin()), (3, Err(SPENT)), "the answer names the count that ends the tries");
        assert_eq!(gate.end(Ok(())), Err(LOCKED), "and the right one is taken no more in this run");
        assert!(!gate.is_open());
        let mut unchecked = Gate::default();
        for _ in 0..=ATTEMPTS {
            assert_eq!((unchecked.begin(), unchecked.end(Err(Refusal::Failed))), (Ok(()), Err(UNCHECKED)));
        }
        assert_eq!(unchecked.begin(), Ok(()), "a passphrase the safe could not be asked about spends no try");
    }

    #[test]
    fn staging_verbs_gated() {
        let mut state = State::default();
        let (stage, read) = (Command::Stage, Command::Read { path: "/a".into(), offset: 0, length: 1 });
        let tunnel = Command::Tunnel("W0ludGVyZmFjZV0K".into());
        assert!(state.is_kept(&stage) && state.is_kept(&Command::Commit) && !state.is_kept(&read));
        assert!(state.is_kept(&tunnel), "and the one that hands over a tunnel's config");
        assert_eq!((state.gate.begin(), state.gate.end(Ok(()))), (Ok(()), Ok(())));
        assert!(!state.is_kept(&stage) && !state.is_kept(&Command::Commit), "run once the gate is open");
        assert!(!state.is_kept(&tunnel));
    }

    #[test]
    fn verb_verdicts() {
        let mut state = State::default();
        let (stage, read) = (Command::Stage, Command::Read { path: "/a".into(), offset: 0, length: 1 });
        assert_eq!(
            [state.check(&read, false), state.check(&read, true), state.check(&stage, false)],
            [Ok(()), Err(DENIAL), Err(LOCKED)]
        );
        assert_eq!(state.check(&stage, true), Err(DENIAL), "a denied path is told before the shut gate");
        assert_eq!((state.gate.begin(), state.gate.end(Ok(()))), (Ok(()), Ok(())));
        assert_eq!(
            [state.check(&stage, false), state.check(&read, true)],
            [Ok(()), Err(DENIAL)],
            "the open gate lets the kept verbs run, and a path still denied stays so"
        );
        assert_eq!(
            (most_read(true), most_read(false)),
            (u64::MAX, 49152),
            "a file is read to its end, a device or a pipe for one part"
        );
    }

    #[test]
    fn tunnel_read_behind_the_gate() {
        use crate::base::code::base64;
        use crate::net::wireguard::config::tests::TEXT;
        let mut state = State::default();
        let tunnel = Command::Tunnel(base64(TEXT.as_bytes()));
        assert_eq!(state.read_tunnel(&tunnel).err(), Some(LOCKED), "kept while the gate is shut");
        assert_eq!((state.gate.begin(), state.gate.end(Ok(()))), (Ok(()), Ok(())));
        assert!(state.read_tunnel(&tunnel).is_ok_and(|config| config.endpoint.port == 51820));
        assert_eq!(
            state.read_tunnel(&Command::Tunnel("W0ludGVyZmFjZV0K".into())).err(),
            Some("no config of one interface and one peer"),
            "a text the router keeps none of is told why"
        );
        assert_eq!(state.read_tunnel(&Command::Lock).err(), Some(NOT_A_VERB));
    }

    #[test]
    fn listing_round_trip() {
        let entries = [("log", EntryKind::Directory), ("ledger", EntryKind::File), ("current", EntryKind::Link)];
        let lines = to_listing(entries.map(|(name, kind)| (String::from(name), kind)));
        assert_eq!(lines, ["current@", "ledger", "log/"]);
        assert_eq!(
            lines.iter().map(|line| parse_listing_line(line)).collect::<Vec<_>>(),
            [("current", EntryKind::Link), ("ledger", EntryKind::File), ("log", EntryKind::Directory)]
        );
    }

    #[test]
    fn denied_paths() {
        assert_eq!(
            join_path("/tmp", ["gone", "..", "..", "data", ".", ".r1metal", "signal", "new"]),
            "/tmp/gone/../../data/.r1metal/signal/new",
            "parts that are not there yet are taken as written, a climb among them too"
        );
        assert_eq!(
            [join_path("/", [".."]), join_path("/data/notes", ["new", ".", "note"])],
            ["/..", "/data/notes/new/note"]
        );
        let denied = [
            "/data/.r1metal/signal",
            "/data/.r1metal/signal/account",
            "/proc/77/mem",
            "/proc/78/mem",
            "/proc/77/task/78/mem",
            "/proc/1/mem",
            "/dev/mem",
            "/proc/ccci_dump",
            "/memfd:recording (deleted)",
            "/tmp/gone/../../data/.r1metal/signal/new",
            "/..",
        ];
        for denied in denied {
            assert!(
                is_denied(denied, false),
                "{denied}: a thread is reached as a process too, physical memory is one node, the modem's record holds a PIN"
            );
        }
        let gated = [
            "/data/.r1metal/signal",
            "/data/.r1metal/signal/account",
            "/data/.r1metal/router",
            "/data/.r1metal/router/tunnel",
            "/data/.restore",
            "/data/.restore/.r1metal/signal/account",
            "/data/.restore.new",
            "/data/.restore.new/camera/pictures/a.jpg",
            "/data/.r1metal.key",
            "/data/.r1metal.key2",
            "/data/.r1metal.key2.new",
        ];
        for gated in gated {
            assert!(is_denied(gated, false) && !is_denied(gated, true), "{gated}: reached once the gate is open");
        }
        for kept in ["/proc/77/mem", "/dev/mem", "/proc/ccci_dump", "/memfd:recording (deleted)", "/.."] {
            assert!(is_denied(kept, true), "{kept}: the gate opens the way to files of the partition alone");
        }
        for open in [
            "/data/.r1metal/signals",
            "/data/.r1metal/logs/log",
            "/proc/77/maps",
            "/proc/77/task/78/maps",
            "/proc/meminfo",
            "/dev/fb0",
            "/data/notes/new/note",
            "/tmp/memfd:recording",
            "/tmp/a..b",
            "/data/.restored",
            "/data/.restore.newer",
            "/data/.r1metal.keys",
        ] {
            assert!(!is_denied(open, false), "{open}");
        }
    }
}

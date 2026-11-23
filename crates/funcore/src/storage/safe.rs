//! The safe's key, the files that hold it sealed under the passphrase, and passphrases.

use crate::base::code::{hex, unhex};
use crate::fs::f2fs::{BLOCK_SIZE, BLOCKS_PER_SEGMENT, UUID_SIZE};
use crate::fs::fscrypt::KEY_SIZE;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub(crate) const MIN_LENGTH: usize = 6;
pub(crate) const MAX_LENGTH: usize = 12;
const SCRYPT_SLACK: usize = 1 << 20;
// The salt of the key an earlier firmware drew straight from the passphrase, one for every R1 and every format.
const SALT_PREFIX: &[u8] = b"r1metal safe ";
const SALT_BYTES: usize = SALT_PREFIX.len() + UUID_SIZE;
/// The scrypt parameters a passphrase is stretched with.
pub const WORK: Work = Work { log_blocks: 16, block: 8, lanes: 1 };
/// The files at the partition's root that hold the safe's key sealed under the passphrase, two copies of one text: either opens the safe, USB hosts are shown neither, and no backup takes them.
pub const KEY_FILES: [&str; 2] = [".r1metal.key", ".r1metal.key2"];
const SLOT_MARK: &str = "r1metal safe 1";
const SLOT_SALT_SIZE: usize = 16;
const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;
/// The bytes of the safe's key sealed: the key and the seal's tag.
pub const SEALED_SIZE: usize = KEY_SIZE + TAG_SIZE;
/// The bytes of the key a passphrase is stretched to, AES-256's, which seals the safe's key.
pub const SEAL_KEY_SIZE: usize = 32;
pub const NO_SAFE: &str = "the private directory is no safe";
pub const OTHER_POLICY: &str = "the directory is encrypted under a policy of another version";
pub const OTHER_KEY: &str = "the private directory is under another key than the key files hold";
pub const NO_KEY: &str = "the key could not be drawn";
/// The log line of a passphrase for the open safe that no key file is there to try on.
pub const NO_KEY_FILE_LINE: &str = "safe: no key file reads";
pub const UNLIKE_COPY: &str = "the encrypted copy reads back otherwise than what it copies";
pub const NEW_KEY_LINE: &str = "safe: a new key, sealed under the passphrase in the key files";
pub const REMADE_LINE: &str =
    "safe: made anew under the key of the key files, the key drawn from the passphrase taken away";
pub const CLEARED_LINE: &str = "safe: what a start cut short left is gone";
// The wrong passphrases of one start that power the device off.
pub(crate) const ATTEMPTS: u32 = 3;
pub const THRICE_WRONG_LINE: &str = "safe: three wrong passphrases, powering off";

/// The log line of a safe as the app finds it; none once a passphrase was refused, as the safe is found anew for every passphrase.
pub const fn to_found_line(is_made: bool, has_refused: bool) -> Option<&'static str> {
    match (has_refused, is_made) {
        (true, _) => None,
        (false, true) => Some("safe: shut"),
        (false, false) => Some("safe: not made"),
    }
}

/// The log line of a safe opened.
pub fn to_open_line(derive_ms: u128) -> String {
    format!("safe: open, the key drawn in {derive_ms} ms")
}

/// The log line of a safe made.
pub fn to_made_line(file_count: usize) -> String {
    format!("safe: made, {file_count} files encrypted into it")
}

/// The log line of an entry the safe skips.
pub fn to_skipped_line(path: &str) -> String {
    format!("safe: {path} is neither a directory nor a file, and left out")
}

/// The log line of free blocks trimmed.
pub fn to_trim_line(bytes: u64) -> String {
    format!("safe: {bytes} bytes of free blocks handed back to the medium")
}

/// The log line of a passphrase change; none where the change failed.
pub const fn to_change_line(outcome: Result<(), Refusal>) -> Option<&'static str> {
    match outcome {
        Ok(()) => Some("safe: the passphrase in use is right, the key is sealed under the new one"),
        Err(Refusal::Wrong) => Some("safe: a wrong passphrase in use"),
        Err(Refusal::Failed) => None,
    }
}

/// The log line of a key file that holds no sealed key.
pub fn to_unread_line(name: &str) -> String {
    format!("safe: {name} holds no sealed key")
}

/// The log line of a public root's files moved under the safe's key.
pub fn to_enclosed_line(root: &str, file_count: usize) -> String {
    format!("safe: {root} under the key, {file_count} files encrypted into it")
}

/// The log line of a public root that is not under the safe's key whole.
pub fn to_unenclosed_line(root: &str, reason: &dyn core::fmt::Display) -> String {
    format!("safe: {root} is not under the key whole, the next start goes on: {reason}")
}

/// The safe's key as a key file holds it: the salt scrypt stretches the passphrase with at `WORK`, and the key sealed with AES-256-GCM under what that gives, the lines before the nonce bound into the seal.
#[derive(Clone, PartialEq, Eq)]
pub struct Slot {
    pub salt: [u8; SLOT_SALT_SIZE],
    pub nonce: [u8; NONCE_SIZE],
    pub sealed: [u8; SEALED_SIZE],
}

impl core::fmt::Debug for Slot {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Slot")
    }
}

impl Slot {
    /// The lines the seal binds: the file's mark, the work and the salt.
    pub fn to_bound(&self) -> String {
        let Work { log_blocks, block, lanes } = WORK;
        format!("{SLOT_MARK}\nwork {log_blocks} {block} {lanes}\nsalt {}\n", hex(&self.salt))
    }

    /// The key file's text.
    pub fn to_text(&self) -> String {
        format!("{}nonce {}\nkey {}\n", self.to_bound(), hex(&self.nonce), hex(&self.sealed))
    }

    /// The slot a key file's text holds; none for any text another slot would not be written as, one that asks for other work than `WORK` among them.
    pub fn parse(text: &str) -> Option<Slot> {
        let field = |line: &str, word: &str| unhex(line.strip_prefix(word)?.strip_prefix(' ')?);
        let mut lines = text.lines().skip(2);
        let slot = Slot {
            salt: field(lines.next()?, "salt")?.try_into().ok()?,
            nonce: field(lines.next()?, "nonce")?.try_into().ok()?,
            sealed: field(lines.next()?, "key")?.try_into().ok()?,
        };
        (slot.to_text() == text).then_some(slot)
    }
}

/// How the safe's key is come by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    // A key file reads: the key is the one the passphrase opens in it.
    Slots,
    // No key file reads and the private directory is encrypted: an earlier firmware's safe, opened by the key that firmware drew from the passphrase before a new key is sealed.
    Earlier,
    // No key file reads and nothing is encrypted: a new key is sealed.
    New,
}

impl Source {
    /// True where a safe is made already, which a passphrase then opens.
    pub const fn is_made(self) -> bool {
        !matches!(self, Source::New)
    }
}

/// How the safe's key is come by where `slot_count` key files read and the private directory is encrypted or not.
pub const fn to_source(slot_count: usize, is_encrypted: bool) -> Source {
    match (slot_count, is_encrypted) {
        (0, true) => Source::Earlier,
        (0, false) => Source::New,
        _ => Source::Slots,
    }
}

/// The slots a passphrase is tried on, in the key files' order: one like a slot before it is not tried again.
pub fn distinct(slots: &[Slot]) -> impl Iterator<Item = &Slot> {
    slots.iter().enumerate().filter(|(at, slot)| !slots[..*at].contains(slot)).map(|(_, slot)| slot)
}

/// True where the key files are written anew from the slot a passphrase opened: a copy is missing, or holds another seal.
pub fn needs_rewrite(slots: &[Slot], opened: &Slot) -> bool {
    slots.len() < KEY_FILES.len() || slots.iter().any(|other| other != opened)
}

/// Whether a passphrase for the open safe has a slot to be tried on: with none it is refused as `Failed`, which `NO_KEY_FILE_LINE` tells.
pub const fn check_slots(slots: &[Slot]) -> Result<(), Refusal> {
    if slots.is_empty() { Err(Refusal::Failed) } else { Ok(()) }
}

/// The kind of a tree's entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Directory,
    File,
    Other,
}

impl Kind {
    /// The kind of an entry the filesystem tells to be a directory, a file or neither.
    pub const fn from_type(is_directory: bool, is_file: bool) -> Kind {
        match (is_directory, is_file) {
            (true, _) => Kind::Directory,
            (false, true) => Kind::File,
            (false, false) => Kind::Other,
        }
    }
}

/// The bytes of a file compared with its copy at a time: a segment of the filesystem.
pub const COMPARED_BYTES: usize = BLOCKS_PER_SEGMENT as usize * BLOCK_SIZE;

/// The file trees `is_same_tree` compares, one effect per method.
pub trait Tree {
    type Path;
    type Error;
    fn names(&self, directory: &Self::Path) -> Result<Vec<Vec<u8>>, Self::Error>;
    fn join(&self, directory: &Self::Path, name: &[u8]) -> Self::Path;
    fn kind(&self, entry: &Self::Path) -> Result<Kind, Self::Error>;
    // The bytes of a file from `offset` on, `most` of them or fewer at its end.
    fn read_at(&self, file: &Self::Path, offset: u64, most: usize) -> Result<Vec<u8>, Self::Error>;
}

/// True if `copy` holds the bytes of the file `original`, compared `COMPARED_BYTES` at a time.
pub fn is_same_file<T: Tree>(tree: &T, original: &T::Path, copy: &T::Path) -> Result<bool, T::Error> {
    let mut offset = 0;
    loop {
        let bytes = tree.read_at(original, offset, COMPARED_BYTES)?;
        if bytes != tree.read_at(copy, offset, COMPARED_BYTES)? {
            return Ok(false);
        }
        if bytes.len() < COMPARED_BYTES {
            return Ok(true);
        }
        offset += COMPARED_BYTES as u64;
    }
}

/// True if `copy` holds the directories and files of `original`.
pub fn is_same_tree<T: Tree>(tree: &T, original: &T::Path, copy: &T::Path) -> Result<bool, T::Error> {
    let mut names = tree.names(original)?;
    names.sort();
    names.retain(|name| matches!(tree.kind(&tree.join(original, name)), Ok(Kind::Directory | Kind::File)));
    let mut copy_names = tree.names(copy)?;
    copy_names.sort();
    if names != copy_names {
        return Ok(false);
    }
    for name in &names {
        let (entry, copy_entry) = (tree.join(original, name), tree.join(copy, name));
        let is_same = if tree.kind(&entry)? == Kind::Directory {
            is_same_tree(tree, &entry, &copy_entry)?
        } else {
            is_same_file(tree, &entry, &copy_entry)?
        };
        if !is_same {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Scrypt's parameters: `log_blocks` is log2 of N, `block` is r, `lanes` is p.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Work {
    pub log_blocks: u32,
    pub block: u64,
    pub lanes: u64,
}

impl Work {
    pub const fn blocks(self) -> u64 {
        1 << self.log_blocks
    }

    const fn memory_bytes(self) -> usize {
        (128 * self.block as usize) << self.log_blocks
    }

    /// The memory limit handed to the library, in bytes.
    pub const fn limit_bytes(self) -> usize {
        self.memory_bytes() + SCRYPT_SLACK
    }
}

/// The salt of the key an earlier firmware drew from the passphrase for the safe on the filesystem `uuid`.
pub fn salt(uuid: &[u8; UUID_SIZE]) -> [u8; SALT_BYTES] {
    let mut bytes = [0; SALT_BYTES];
    let (prefix, id) = bytes.split_at_mut(SALT_PREFIX.len());
    prefix.copy_from_slice(SALT_PREFIX);
    id.copy_from_slice(uuid);
    bytes
}

/// A passphrase, zeroed on drop, whose `Debug` prints its type alone.
#[derive(Clone, PartialEq, Eq)]
pub struct Passphrase(String);

impl Passphrase {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Passphrase {
    fn from(text: String) -> Passphrase {
        Passphrase(text)
    }
}

impl From<&str> for Passphrase {
    fn from(text: &str) -> Passphrase {
        Passphrase(text.into())
    }
}

impl Drop for Passphrase {
    fn drop(&mut self) {
        let mut bytes = core::mem::take(&mut self.0).into_bytes();
        // The capacity past its end can still hold characters removed from the text.
        bytes.resize(bytes.capacity(), 0);
        bytes.fill(0);
        core::hint::black_box(&bytes);
    }
}

impl core::fmt::Debug for Passphrase {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("Passphrase")
    }
}

/// A request of the safe's window, borrowing the keys' text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Request<'a> {
    Open(&'a str),
    Change { current: &'a str, new: &'a str },
}

impl core::fmt::Debug for Request<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Request::Open(_) => "Open",
            Request::Change { .. } => "Change",
        })
    }
}

/// A request of the safe with passphrases of its own.
pub enum Order {
    Open(Passphrase),
    Change { current: Passphrase, new: Passphrase },
}

impl From<Request<'_>> for Order {
    fn from(request: Request<'_>) -> Order {
        match request {
            Request::Open(text) => Order::Open(text.into()),
            Request::Change { current, new } => Order::Change { current: current.into(), new: new.into() },
        }
    }
}

impl Order {
    /// The passphrase a shut safe opens or is made with; a change is dropped.
    pub fn into_open(self) -> Option<Passphrase> {
        if let Order::Open(passphrase) = self { Some(passphrase) } else { None }
    }

    /// The passphrase in use and its successor, for the open safe, whose key is sealed anew under the successor; an opening is dropped.
    pub fn into_change(self) -> Option<(Passphrase, Passphrase)> {
        if let Order::Change { current, new } = self { Some((current, new)) } else { None }
    }
}

/// Why a passphrase did not open the safe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    // The safe is made, and the passphrase is not its own.
    Wrong,
    // The safe could not be opened, made or given another passphrase.
    Failed,
}

impl Refusal {
    /// The outcome of a passphrase by whether its key is the safe's; `None` where that is unknown.
    pub fn check(is_right: Option<bool>) -> Result<(), Refusal> {
        match is_right {
            Some(true) => Ok(()),
            Some(false) => Err(Refusal::Wrong),
            None => Err(Refusal::Failed),
        }
    }

    /// The log line of the refusal; none for `Failed`.
    pub const fn to_log_line(self) -> Option<&'static str> {
        match self {
            Refusal::Wrong => Some("safe: a wrong passphrase"),
            Refusal::Failed => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;

    type Entry = (&'static str, Kind, &'static [u8]);

    // A tree in memory, each entry by its path with its kind and bytes; `broken` is an entry whose kind cannot be told.
    struct Memory {
        entries: BTreeMap<String, (Kind, Vec<u8>)>,
        broken: Option<&'static str>,
    }

    impl Tree for Memory {
        type Path = String;
        type Error = ();

        fn names(&self, directory: &String) -> Result<Vec<Vec<u8>>, ()> {
            let prefix = format!("{directory}/");
            let below = self.entries.keys().filter_map(|path| path.strip_prefix(&prefix));
            Ok(below.filter(|name| !name.contains('/')).map(|name| name.as_bytes().to_vec()).collect())
        }

        fn join(&self, directory: &String, name: &[u8]) -> String {
            format!("{directory}/{}", String::from_utf8_lossy(name))
        }

        fn kind(&self, entry: &String) -> Result<Kind, ()> {
            let held = self.entries.get(entry).filter(|_| self.broken != Some(entry.as_str()));
            held.map(|(kind, _)| *kind).ok_or(())
        }

        fn read_at(&self, file: &String, offset: u64, most: usize) -> Result<Vec<u8>, ()> {
            let (_, bytes) = self.entries.get(file).filter(|(kind, _)| *kind == Kind::File).ok_or(())?;
            let start = (offset as usize).min(bytes.len());
            Ok(bytes[start..bytes.len().min(start + most)].to_vec())
        }
    }

    fn compare(entries: &[Entry], broken: Option<&'static str>) -> Result<bool, ()> {
        let entries = entries.iter().map(|(path, kind, bytes)| (String::from(*path), (*kind, bytes.to_vec())));
        is_same_tree(&Memory { entries: entries.collect(), broken }, &"old".into(), &"new".into())
    }

    #[test]
    fn same_tree() {
        let file = |path: &'static str, bytes: &'static [u8]| (path, Kind::File, bytes);
        let directory = |path: &'static str| (path, Kind::Directory, &[][..]);
        let original = [directory("old/notes"), file("old/notes/1.txt", b"one"), file("old/key", b"k")];
        let with_copy = |copy: &[Entry]| [&original[..], copy].concat();
        let copy = [directory("new/notes"), file("new/notes/1.txt", b"one"), file("new/key", b"k")];
        assert_eq!(
            compare(&with_copy(&copy), None),
            Ok(true),
            "the same directories and files, each of the same bytes"
        );
        let socket = ("old/socket", Kind::Other, &[][..]);
        assert_eq!(
            compare(&[&with_copy(&copy)[..], &[socket, file("old/lost", b"")]].concat(), Some("old/lost")),
            Ok(true),
            "what of the original is neither a directory nor a file, or tells no kind, is left out"
        );
        let other = [directory("new/notes"), file("new/notes/1.txt", b"two"), file("new/key", b"k")];
        assert_eq!(compare(&with_copy(&other), None), Ok(false), "a file of other bytes, a directory down");
        assert_eq!(compare(&with_copy(&copy[..2]), None), Ok(false), "a file the copy lacks");
        assert_eq!(compare(&with_copy(&[&copy[..], &[file("new/more", b"")]].concat()), None), Ok(false));
        let unlike = [directory("new/notes"), file("new/notes/1.txt", b"one"), directory("new/key")];
        assert_eq!(compare(&with_copy(&unlike), None), Err(()), "an entry that cannot be read as its original is");
    }

    #[test]
    fn entry_kinds() {
        assert_eq!(
            [Kind::from_type(true, false), Kind::from_type(false, true), Kind::from_type(false, false)],
            [Kind::Directory, Kind::File, Kind::Other],
            "a link, a socket or a device is neither"
        );
    }

    #[test]
    fn same_file_by_chunks() {
        let long: Vec<u8> = (0..COMPARED_BYTES + 5).map(|at| at as u8).collect();
        let (mut other, mut longer) = (long.clone(), long.clone());
        other[COMPARED_BYTES + 2] ^= 1;
        longer.push(0);
        let whole = long[..COMPARED_BYTES].to_vec();
        let files = [("a", long.clone()), ("b", long), ("c", other), ("d", longer), ("e", whole.clone()), ("f", whole)];
        let entries = files.into_iter().map(|(path, bytes)| (String::from(path), (Kind::File, bytes)));
        let tree = Memory { entries: entries.collect(), broken: None };
        let same = |copy: &str| is_same_file(&tree, &"a".into(), &copy.into());
        assert_eq!(
            [same("b"), same("c"), same("d"), same("e")],
            [Ok(true), Ok(false), Ok(false), Ok(false)],
            "byte for byte past one comparison, and of one length"
        );
        assert_eq!(is_same_file(&tree, &"e".into(), &"f".into()), Ok(true), "a file that ends with a comparison");
        assert_eq!(same("nowhere"), Err(()), "a copy that does not read");
    }

    #[test]
    fn key_sources() {
        assert_eq!(
            [to_source(2, true), to_source(1, false), to_source(0, true), to_source(0, false)],
            [Source::Slots, Source::Slots, Source::Earlier, Source::New],
            "a key file that reads comes first; with none, an encrypted private directory is an earlier firmware's"
        );
        assert_eq!([Source::Slots, Source::Earlier, Source::New].map(Source::is_made), [true, true, false]);
        let slot = |byte| Slot { salt: [byte; SLOT_SALT_SIZE], nonce: [byte; NONCE_SIZE], sealed: [byte; SEALED_SIZE] };
        let (one, other) = (slot(1), slot(2));
        let slots = [one.clone(), one.clone(), other.clone(), one.clone()];
        assert_eq!(distinct(&slots).collect::<Vec<_>>(), [&one, &other], "each seal is tried once");
        assert!(
            !needs_rewrite(&[one.clone(), one.clone()], &one)
                && needs_rewrite(core::slice::from_ref(&one), &one)
                && needs_rewrite(&[one.clone(), other.clone()], &one)
                && needs_rewrite(&[other, one.clone()], &one),
            "written anew where a copy is missing or holds another seal than the one that opened"
        );
        assert_eq!(
            (check_slots(&[]), check_slots(core::slice::from_ref(&one)), NO_KEY_FILE_LINE),
            (Err(Refusal::Failed), Ok(()), "safe: no key file reads"),
            "with no key file a passphrase for the open safe is tried on nothing"
        );
    }

    #[test]
    fn salt_bytes() {
        let bytes = salt(&[7; UUID_SIZE]);
        assert_eq!(&bytes[..13], b"r1metal safe ");
        assert_eq!(bytes[13..], [7; UUID_SIZE]);
        assert_eq!(WORK.memory_bytes(), 64 << 20);
    }

    #[test]
    fn orders_by_state() {
        let open = || Order::from(Request::Open("opening"));
        let change = || Order::from(Request::Change { current: "in use", new: "new" });
        assert_eq!(open().into_open(), Some("opening".into()));
        assert_eq!(change().into_change(), Some(("in use".into(), "new".into())));
        assert_eq!((change().into_open(), open().into_change()), (None, None));
        assert_eq!(Passphrase::from("opening").as_str(), "opening");
        assert_eq!(alloc::format!("{:?}", Passphrase::from("opening")), "Passphrase");
        let text =
            alloc::format!("{:?} {:?}", Request::Open("opening"), Request::Change { current: "in use", new: "new" });
        assert_eq!(text, "Open Change");
    }

    #[test]
    fn safe_lines() {
        assert_eq!(
            [to_found_line(true, false), to_found_line(false, false), to_found_line(true, true)],
            [Some("safe: shut"), Some("safe: not made"), None],
            "told as the app finds it, and not again for each passphrase after a refusal"
        );
        assert_eq!(
            (ATTEMPTS, THRICE_WRONG_LINE),
            (3, "safe: three wrong passphrases, powering off"),
            "the line names the count that powers the device off"
        );
        assert_eq!(to_open_line(3), "safe: open, the key drawn in 3 ms");
        assert_eq!(
            [Ok(()), Err(Refusal::Wrong), Err(Refusal::Failed)].map(to_change_line),
            [
                Some("safe: the passphrase in use is right, the key is sealed under the new one"),
                Some("safe: a wrong passphrase in use"),
                None
            ]
        );
        assert_eq!(to_unread_line(".r1metal.key2"), "safe: .r1metal.key2 holds no sealed key");
        assert_eq!(to_enclosed_line("camera", 30), "safe: camera under the key, 30 files encrypted into it");
        assert_eq!(
            to_unenclosed_line("audio", &"no space left"),
            "safe: audio is not under the key whole, the next start goes on: no space left"
        );
        assert_eq!(
            (Refusal::Wrong.to_log_line(), Refusal::Failed.to_log_line()),
            (Some("safe: a wrong passphrase"), None)
        );
    }

    #[test]
    fn key_file_text() {
        let slot = Slot { salt: [0xA1; SLOT_SALT_SIZE], nonce: [0xB2; NONCE_SIZE], sealed: [0xC3; SEALED_SIZE] };
        let text = slot.to_text();
        assert_eq!(
            text,
            alloc::format!(
                "r1metal safe 1\nwork 16 8 1\nsalt {}\nnonce {}\nkey {}\n",
                "a1".repeat(16),
                "b2".repeat(12),
                "c3".repeat(80)
            ),
            "the mark, the work, the salt, and the key of 64 bytes with its tag of 16 under a nonce"
        );
        assert_eq!(slot.to_bound(), text[..text.find("nonce").unwrap()], "the seal binds what stands before the nonce");
        assert_eq!(alloc::format!("{slot:?}"), "Slot", "formatted, it tells none of what the key file holds");
        assert_eq!(Slot::parse(&text), Some(slot));
        for other in [
            text.replace("safe 1", "safe 2"),
            text.replace("work 16", "work 30"),
            text.replace("a1a1", "A1A1"),
            text.replace("c3c3", "c3"),
            text.trim_end().into(),
            alloc::format!("{text}more\n"),
            String::new(),
        ] {
            assert_eq!(Slot::parse(&other), None, "{other:?} is no key file's text");
        }
        assert_eq!(KEY_FILES, [".r1metal.key", ".r1metal.key2"]);
    }

    #[test]
    fn key_check() {
        assert_eq!(
            [Some(true), Some(false), None].map(Refusal::check),
            [Ok(()), Err(Refusal::Wrong), Err(Refusal::Failed)]
        );
    }
}

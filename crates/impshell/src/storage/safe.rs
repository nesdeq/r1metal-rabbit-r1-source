use super::Shared;
use super::files::{trim, write_over};
use crate::{crypto, log, sys};
use r1_funcore::domain::backup::PUBLIC_ROOTS;
use r1_funcore::fs::f2fs::UUID_SIZE;
use r1_funcore::fs::fscrypt::{self, KEY_SIZE, Key};
use r1_funcore::storage::progress::Progress;
use r1_funcore::storage::safe::{
    self, CLEARED_LINE, KEY_FILES, Kind, NEW_KEY_LINE, NO_KEY, NO_KEY_FILE_LINE, NO_SAFE, OTHER_KEY, OTHER_POLICY,
    Passphrase, REMADE_LINE, Refusal, Slot, Source, Tree, UNLIKE_COPY,
};
use r1_funcore::storage::{MOUNT_POINT, PRIVATE, paths};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

const SET_POLICY: u32 = sys::ioc(2, b'f', 19, 12);
const GET_POLICY: u32 = sys::ioc(3, b'f', 22, size_of::<u64>() + size_of::<u8>());
const ADD_KEY: u32 = sys::ioc(3, b'f', 23, size_of::<AddKey>());
const REMOVE_KEY: u32 = sys::ioc(3, b'f', 24, size_of::<RemoveKey>());
const IDENTIFIER_SIZE: usize = 16;
const BY_IDENTIFIER: u32 = 2;
const DROP_CACHES: &str = "/proc/sys/vm/drop_caches";
const PAGES_AND_NAMES: &[u8] = b"3";

type Identifier = [u8; IDENTIFIER_SIZE];

#[repr(C)]
#[derive(Default)]
struct PolicyV2 {
    version: u8,
    contents: u8,
    names: u8,
    flags: u8,
    reserved: [u8; 4],
    identifier: Identifier,
}

#[repr(C)]
struct GetPolicy {
    size: u64,
    policy: PolicyV2,
}

#[repr(C)]
struct KeySpecifier {
    kind: u32,
    reserved: u32,
    identifier: Identifier,
    rest_of_union: [u8; 16],
}

#[repr(C)]
struct AddKey {
    specifier: KeySpecifier,
    size: u32,
    keyring_key: u32,
    reserved: [u32; 7],
    flags: u32,
}

#[repr(C)]
struct AddKeyWithRaw {
    argument: AddKey,
    key: [u8; KEY_SIZE],
}

#[repr(C)]
struct RemoveKey {
    specifier: KeySpecifier,
    status: u32,
    reserved: [u32; 5],
}

fn to_policy(identifier: Identifier) -> PolicyV2 {
    let fscrypt::Policy { version, contents, names, flags } = fscrypt::POLICY;
    PolicyV2 { version, contents, names, flags, reserved: [0; 4], identifier }
}

fn get_policy_argument() -> GetPolicy {
    GetPolicy { size: size_of::<PolicyV2>() as u64, policy: PolicyV2::default() }
}

fn identifier_of(argument: &GetPolicy) -> Option<Identifier> {
    fscrypt::is_safe_policy(argument.size, argument.policy.version).then_some(argument.policy.identifier)
}

fn to_specifier(identifier: Identifier) -> KeySpecifier {
    KeySpecifier { kind: BY_IDENTIFIER, reserved: 0, identifier, rest_of_union: [0; 16] }
}

fn add_key_argument(key: &Key) -> AddKeyWithRaw {
    let specifier = to_specifier([0; IDENTIFIER_SIZE]);
    let argument = AddKey { specifier, size: KEY_SIZE as u32, keyring_key: 0, reserved: [0; 7], flags: 0 };
    AddKeyWithRaw { argument, key: *key.bytes() }
}

fn remove_key_argument(identifier: Identifier) -> RemoveKey {
    RemoveKey { specifier: to_specifier(identifier), status: 0, reserved: [0; 5] }
}

fn private_path(root: &Path) -> PathBuf {
    root.join(PRIVATE)
}

fn temporary_path(root: &Path) -> PathBuf {
    root.join(paths::new_name(PRIVATE))
}

// The key files' slots that read, in the files' order.
fn read_slots(root: &Path) -> Vec<Slot> {
    let read = |name: &&str| match fs::read_to_string(root.join(name)) {
        Ok(text) => Slot::parse(&text).or_else(|| {
            log!("{}", safe::to_unread_line(name));
            None
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            log!("safe: {name}: {e}");
            None
        }
    };
    KEY_FILES.iter().filter_map(read).collect()
}

// Writes a slot to every key file, each replaced at one stroke.
fn write_slots(root: &Path, slot: &Slot) -> io::Result<()> {
    let text = slot.to_text();
    KEY_FILES.iter().try_for_each(|name| write_over(root, &root.join(name), text.as_bytes()))
}

// The key files' slots for a passphrase of the open safe; with none that reads, the passphrase is refused and the log told.
fn read_kept_slots(root: &Path) -> Result<Vec<Slot>, Refusal> {
    let slots = read_slots(root);
    safe::check_slots(&slots).inspect_err(|_| log!("{NO_KEY_FILE_LINE}"))?;
    Ok(slots)
}

// The safe's key out of the first slot a passphrase opens, with that slot.
fn open_slots<'a>(slots: &'a [Slot], passphrase: &Passphrase) -> Option<(Key, &'a Slot)> {
    safe::distinct(slots).find_map(|slot| Some((crypto::open_safe_key(passphrase.as_str(), slot)?, slot)))
}

// True where the key an earlier firmware drew straight from the passphrase is the one the private directory is under; the kernel has it then.
fn open_earlier(root: &Path, uuid: &[u8; UUID_SIZE], passphrase: &Passphrase) -> io::Result<bool> {
    let key = crypto::safe_key(passphrase.as_str(), &safe::salt(uuid)).ok_or_else(|| io::Error::other(NO_KEY))?;
    open_with(root, &key)
}

// A new key, sealed under the passphrase into the key files before anything is encrypted under it.
fn seal_new_key(root: &Path, passphrase: &Passphrase) -> io::Result<Key> {
    let key = crypto::new_safe_key().ok_or_else(|| io::Error::other(NO_KEY))?;
    let slot = crypto::seal_safe_key(passphrase.as_str(), &key).ok_or_else(|| io::Error::other(NO_KEY))?;
    write_slots(root, &slot)?;
    log!("{NEW_KEY_LINE}");
    Ok(key)
}

// The safe's key by a passphrase, come by as `source` says; none for a wrong one.
fn take_key(
    root: &Path,
    uuid: &[u8; UUID_SIZE],
    (slots, source): (&[Slot], Source),
    passphrase: &Passphrase,
) -> io::Result<Option<Key>> {
    match source {
        Source::Slots => {
            let Some((key, slot)) = open_slots(slots, passphrase) else { return Ok(None) };
            if safe::needs_rewrite(slots, slot) {
                write_slots(root, slot).unwrap_or_else(|e| log!("safe: writing the key files alike: {e}"));
            }
            Ok(Some(key))
        }
        Source::Earlier if !open_earlier(root, uuid, passphrase)? => Ok(None),
        Source::Earlier | Source::New => seal_new_key(root, passphrase).map(Some),
    }
}

// Has the private directory under the safe's key: made under it where there is none or a plain one, and made anew under it where it is under the key an earlier firmware drew from the passphrase, which is taken away then.
fn settle_private(root: &Path, uuid: &[u8; UUID_SIZE], key: &Key, passphrase: &Passphrase) -> io::Result<()> {
    let filesystem = File::open(root)?;
    let identifier = add_key(&filesystem, key)?;
    match private_identifier(root)? {
        Some(current) if fscrypt::is_same_key(&identifier, &current) => Ok(()),
        Some(current) => {
            if !open_earlier(root, uuid, passphrase)? {
                return Err(io::Error::other(OTHER_KEY));
            }
            make_with(root, key)?;
            if let Err(e) = remove_key(&filesystem, &current) {
                log!("safe: taking the key drawn from the passphrase away: {e}");
            }
            log!("{REMADE_LINE}");
            Ok(())
        }
        None => make_with(root, key),
    }
}

pub(super) fn open(shared: &Shared, uuid: &[u8; UUID_SIZE]) -> io::Result<()> {
    let root = Path::new(MOUNT_POINT);
    clear_temporary(root)?;
    let mut refused = None;
    loop {
        // Looked at anew for every passphrase: a try that came to nothing may have written the key files.
        let slots = read_slots(root);
        let source = safe::to_source(slots.len(), private_identifier(root)?.is_some());
        if let Some(line) = safe::to_found_line(source.is_made(), refused.is_some()) {
            log!("{line}");
        }
        shared.report(Progress::Shut { is_made: source.is_made(), refused });
        let (number, order) = shared.await_order();
        let Some(passphrase) = order.into_open() else {
            continue;
        };
        let start = Instant::now();
        let key = take_key(root, uuid, (&slots, source), &passphrase);
        let key_ms = start.elapsed().as_millis();
        let is_right = key.and_then(|key| match key {
            Some(key) => settle_private(root, uuid, &key, &passphrase).map(|()| true),
            None => Ok(false),
        });
        drop(passphrase);
        let Err(refusal) = Refusal::check(is_right.inspect_err(|e| log!("safe: {e}")).ok()) else {
            log!("{}", safe::to_open_line(key_ms));
            return Ok(());
        };
        if let Some(line) = refusal.to_log_line() {
            log!("{line}");
        }
        refused = Some((number, refusal));
    }
}

pub(super) fn serve_changes(shared: &Shared) {
    let root = Path::new(MOUNT_POINT);
    loop {
        let (number, order) = shared.await_order();
        let Some((current, new)) = order.into_change() else {
            continue;
        };
        let outcome = change(root, current, new);
        // The blocks that held the key sealed under the passphrase given up are handed back.
        if outcome.is_ok() {
            trim(root);
        }
        if let Some(line) = safe::to_change_line(outcome) {
            log!("{line}");
        }
        shared.report_change(number, outcome);
    }
}

// Seals the safe's key under a new passphrase, once the one in use has opened it: nothing the safe holds is touched.
fn change(root: &Path, current: Passphrase, new: Passphrase) -> Result<(), Refusal> {
    let slots = read_kept_slots(root)?;
    let opened = open_slots(&slots, &current);
    drop(current);
    let (key, _) = opened.ok_or(Refusal::Wrong)?;
    let slot = crypto::seal_safe_key(new.as_str(), &key);
    drop((new, key));
    let written = slot.ok_or_else(|| io::Error::other(NO_KEY)).and_then(|slot| write_slots(root, &slot));
    written.map_err(|e| {
        log!("safe: sealing the key under the new passphrase: {e}");
        Refusal::Failed
    })
}

pub(super) fn check(root: &Path, passphrase: &Passphrase) -> Result<(), Refusal> {
    let slots = read_kept_slots(root)?;
    let outcome = Refusal::check(Some(open_slots(&slots, passphrase).is_some()));
    if let Some(line) = outcome.err().and_then(Refusal::to_log_line) {
        log!("{line}");
    }
    outcome
}

fn encrypt_alike(root: &Path, directory: &Path) -> io::Result<()> {
    let identifier = read_identifier(&File::open(private_path(root))?)?.ok_or_else(|| io::Error::other(NO_SAFE))?;
    let mut policy = to_policy(identifier);
    unsafe { sys::ioctl(File::open(directory)?.as_raw_fd(), SET_POLICY, &mut policy) }
}

pub(super) fn is_alike(root: &Path, directory: &Path) -> io::Result<Option<bool>> {
    let other = read_identifier(&File::open(directory)?)?;
    let own = read_identifier(&File::open(private_path(root))?)?;
    Ok(own.zip(other).map(|(own, other)| fscrypt::is_same_key(&other, &own)))
}

pub(super) fn make_enclosed(root: &Path, directory: &Path) -> io::Result<()> {
    fs::create_dir(directory)?;
    encrypt_alike(root, directory)
}

// Has every public root under the safe's key, once the safe is open: a missing one is made under it, and what a plain one holds, as an earlier firmware or a restore left it, is moved file by file into one made under it, which then takes its name. A root that does not go whole is told in the log, stays as far as it went, and is gone on with at the next start.
pub(super) fn enclose(shared: &Shared, root: &Path) {
    for name in PUBLIC_ROOTS {
        match enclose_root(root, name, &mut || shared.begin_encrypting()) {
            Ok(Some(count)) => log!("{}", safe::to_enclosed_line(name, count)),
            Ok(None) => {}
            Err(e) => log!("{}", safe::to_unenclosed_line(name, &e)),
        }
    }
    if shared.board.lock().progress().is_encrypting() {
        trim(root);
    }
}

// The number of files moved under the key; none where the root was under a key already, or was made.
fn enclose_root(root: &Path, name: &str, begin: &mut dyn FnMut()) -> io::Result<Option<usize>> {
    let (path, new) = (root.join(name), root.join(paths::new_name(name)));
    let is_plain = match File::open(&path) {
        Ok(directory) => Some(read_identifier(&directory)?.is_none()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if is_plain == Some(false) {
        return Ok(None);
    }
    // One made under the key stands there already where a start was cut short.
    if !new.try_exists()? {
        make_enclosed(root, &new)?;
    }
    let mut count = None;
    if is_plain == Some(true) {
        begin();
        count = Some(move_tree(&path, &new)?);
        fs::remove_dir(&path)?;
    }
    fs::rename(&new, &path)?;
    File::open(root)?.sync_all()?;
    Ok(count)
}

// Moves what a directory holds into another, which may hold a part of it already: each file is copied, synced with its directory, read back from the medium and compared, and only then removed where it came from.
fn move_tree(source: &Path, target: &Path) -> io::Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let (kind, from, to) = (entry.file_type()?, entry.path(), target.join(entry.file_name()));
        if kind.is_dir() {
            match fs::create_dir(&to) {
                Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                _ => {}
            }
            count += move_tree(&from, &to)?;
            fs::remove_dir(&from)?;
        } else if kind.is_file() {
            move_file(&from, &to, target)?;
            count += 1;
        } else {
            log!("{}", safe::to_skipped_line(&from.display().to_string()));
            fs::remove_file(&from)?;
        }
    }
    File::open(target)?.sync_all()?;
    Ok(count)
}

fn move_file(from: &Path, to: &Path, directory: &Path) -> io::Result<()> {
    let mut copy = File::create(to)?;
    io::copy(&mut File::open(from)?, &mut copy)?;
    copy.sync_all()?;
    File::open(directory)?.sync_all()?;
    // The copy's pages are let go of, so that the comparison reads it from the medium, decrypted.
    let result = unsafe { libc::posix_fadvise(copy.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if result != 0 {
        log!("safe: letting go of a copy's pages: {}", io::Error::from_raw_os_error(result));
    }
    drop(copy);
    if !safe::is_same_file(&Disk, &from.to_path_buf(), &to.to_path_buf())? {
        return Err(io::Error::other(UNLIKE_COPY));
    }
    fs::remove_file(from)
}

fn clear_temporary(root: &Path) -> io::Result<()> {
    match fs::remove_dir_all(temporary_path(root)) {
        Ok(()) => {
            log!("{CLEARED_LINE}");
            trim(root);
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// The identifier of the key the private directory is under; none where it is plain or not there.
fn private_identifier(root: &Path) -> io::Result<Option<Identifier>> {
    match File::open(private_path(root)) {
        Ok(directory) => read_identifier(&directory),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn read_identifier(directory: &File) -> io::Result<Option<Identifier>> {
    let mut argument = get_policy_argument();
    match unsafe { sys::ioctl(directory.as_raw_fd(), GET_POLICY, &mut argument) } {
        Ok(()) => identifier_of(&argument).map(Some).ok_or_else(|| io::Error::other(OTHER_POLICY)),
        Err(e) if e.raw_os_error() == Some(libc::ENODATA) => Ok(None),
        Err(e) => Err(e),
    }
}

fn add_key(file: &File, key: &Key) -> io::Result<Identifier> {
    let mut argument = add_key_argument(key);
    let result = unsafe { sys::ioctl(file.as_raw_fd(), ADD_KEY, &mut argument) };
    let identifier = argument.argument.specifier.identifier;
    argument.key.fill(0);
    std::hint::black_box(&argument);
    result.map(|()| identifier)
}

fn remove_key(file: &File, identifier: &Identifier) -> io::Result<()> {
    let mut argument = remove_key_argument(*identifier);
    unsafe { sys::ioctl(file.as_raw_fd(), REMOVE_KEY, &mut argument) }
}

fn open_with(root: &Path, key: &Key) -> io::Result<bool> {
    let directory = File::open(private_path(root))?;
    let expected = read_identifier(&directory)?.ok_or_else(|| io::Error::other(NO_SAFE))?;
    let identifier = add_key(&directory, key)?;
    if fscrypt::is_same_key(&identifier, &expected) {
        return Ok(true);
    }
    if let Err(e) = remove_key(&directory, &identifier) {
        log!("safe: taking a wrong key away: {e}");
    }
    Ok(false)
}

fn make_with(root: &Path, key: &Key) -> io::Result<()> {
    let (private, temporary) = (private_path(root), temporary_path(root));
    // A try before that came to nothing may have left a safe half made.
    clear_temporary(root)?;
    fs::create_dir(&temporary)?;
    let directory = File::open(&temporary)?;
    let mut policy = to_policy(add_key(&directory, key)?);
    unsafe { sys::ioctl(directory.as_raw_fd(), SET_POLICY, &mut policy) }?;
    let has_private = match fs::symlink_metadata(&private) {
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    let copy_count = if has_private { copy_tree(&private, &temporary)? } else { 0 };
    directory.sync_all()?;
    // The copy is read back from the medium, decrypted, before it takes the place of what it copies.
    if has_private {
        if let Err(e) = fs::write(DROP_CACHES, PAGES_AND_NAMES) {
            log!("safe: letting go of the caches: {e}");
        }
        if !safe::is_same_tree(&Disk, &private, &temporary)? {
            return Err(io::Error::other(UNLIKE_COPY));
        }
        exchange(&temporary, &private)?;
    } else {
        fs::rename(&temporary, &private)?;
    }
    File::open(root)?.sync_all()?;
    log!("{}", safe::to_made_line(copy_count));
    if has_private {
        match fs::remove_dir_all(&temporary) {
            Ok(()) => trim(root),
            Err(e) => log!("safe: the private directory it copied: {e}"),
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> io::Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let (kind, path) = (entry.file_type()?, target.join(entry.file_name()));
        if kind.is_dir() {
            fs::create_dir(&path)?;
            count += copy_tree(&entry.path(), &path)?;
            File::open(&path)?.sync_all()?;
        } else if kind.is_file() {
            let mut copy = File::create(&path)?;
            io::copy(&mut File::open(entry.path())?, &mut copy)?;
            copy.sync_all()?;
            count += 1;
        } else {
            log!("{}", safe::to_skipped_line(&entry.path().display().to_string()));
        }
    }
    Ok(count)
}

struct Disk;

impl Tree for Disk {
    type Path = PathBuf;
    type Error = io::Error;

    fn names(&self, directory: &PathBuf) -> io::Result<Vec<Vec<u8>>> {
        fs::read_dir(directory)?.map(|entry| entry.map(|entry| entry.file_name().into_vec())).collect()
    }

    fn join(&self, directory: &PathBuf, name: &[u8]) -> PathBuf {
        directory.join(OsStr::from_bytes(name))
    }

    fn kind(&self, entry: &PathBuf) -> io::Result<Kind> {
        let metadata = fs::symlink_metadata(entry)?;
        Ok(Kind::from_type(metadata.is_dir(), metadata.is_file()))
    }

    fn read_at(&self, file: &PathBuf, offset: u64, most: usize) -> io::Result<Vec<u8>> {
        let mut file = File::open(file)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(most as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

fn exchange(first: &Path, second: &Path) -> io::Result<()> {
    let (first, second) = (sys::c_path(first)?, sys::c_path(second)?);
    let (old_path, new_path) = (first.as_ptr(), second.as_ptr());
    let flags = libc::c_ulong::from(libc::RENAME_EXCHANGE);
    let result =
        unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, old_path, libc::AT_FDCWD, new_path, flags) };
    sys::check(result as libc::c_int).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::scratch;
    use std::mem::offset_of;

    #[test]
    fn ioctl_numbers() {
        assert_eq!([SET_POLICY, GET_POLICY, ADD_KEY, REMOVE_KEY], [0x800c_6613, 0xc009_6616, 0xc050_6617, 0xc040_6618]);
    }

    fn bytes<T>(value: &T) -> &[u8] {
        unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
    }

    fn bytes_mut<T>(value: &mut T) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut((value as *mut T).cast::<u8>(), size_of::<T>()) }
    }

    #[test]
    fn fscrypt_layouts() {
        assert_eq!(
            [size_of::<PolicyV2>(), size_of::<GetPolicy>(), size_of::<KeySpecifier>(), size_of::<AddKey>()],
            [24, 32, 40, 80]
        );
        assert_eq!((size_of::<AddKeyWithRaw>(), size_of::<RemoveKey>()), (144, 64));
        assert_eq!(
            [offset_of!(GetPolicy, policy), offset_of!(KeySpecifier, identifier), offset_of!(AddKey, size)],
            [8, 8, 40]
        );
        assert_eq!((offset_of!(AddKeyWithRaw, key), offset_of!(RemoveKey, status)), (80, 40));
        let identifier = [7; IDENTIFIER_SIZE];
        let policy = to_policy(identifier);
        assert_eq!(
            bytes(&policy),
            [&[2, 1, 4, 3, 0, 0, 0, 0][..], &[7; 16]].concat(),
            "version 2, XTS, CTS, padded to 32, reserved"
        );
        let mut argument = get_policy_argument();
        assert_eq!(bytes(&argument), [&24u64.to_le_bytes()[..], &[0; 24]].concat(), "room for a policy of version 2");
        bytes_mut(&mut argument)[8..].copy_from_slice(bytes(&policy));
        assert_eq!(identifier_of(&argument), Some(identifier));
        bytes_mut(&mut argument)[8] = 0;
        assert_eq!(identifier_of(&argument), None, "a policy of version 1 names its key otherwise");
        let mut short = get_policy_argument();
        bytes_mut(&mut short)[..8].copy_from_slice(&12u64.to_le_bytes());
        assert_eq!(identifier_of(&short), None, "the answer of a policy of version 1");
        let mut key = Key::zeroed();
        key.bytes_mut().fill(9);
        let mut adding = add_key_argument(&key);
        let expected = [&[2, 0, 0, 0][..], &[0; 36], &[64, 0, 0, 0], &[0; 36], &[9; KEY_SIZE]].concat();
        assert_eq!(
            bytes(&adding),
            expected,
            "by identifier, a key of 64 bytes, no key of the keyring, nothing reserved, no flags, the key"
        );
        bytes_mut(&mut adding)[8..24].copy_from_slice(&[5; IDENTIFIER_SIZE]);
        assert_eq!(
            adding.argument.specifier.identifier, [5; IDENTIFIER_SIZE],
            "the identifier the kernel gave the key"
        );
        let removing = remove_key_argument([5; IDENTIFIER_SIZE]);
        assert_eq!(
            bytes(&removing),
            [&[2, 0, 0, 0][..], &[0; 4], &[5; 16], &[0; 40]].concat(),
            "by identifier, nothing else"
        );
    }

    #[test]
    fn copy_tree_and_compare() {
        let root = scratch("safe-copy");
        let (source, target) = (root.join("from"), root.join("to"));
        fs::create_dir_all(source.join("signal/sessions")).unwrap();
        fs::write(source.join("signal/sessions/anna"), b"session").unwrap();
        fs::write(source.join("settings"), b"theme dial\n").unwrap();
        fs::create_dir_all(&target).unwrap();
        assert_eq!(copy_tree(&source, &target).unwrap(), 2);
        assert_eq!(fs::read(target.join("signal/sessions/anna")).unwrap(), b"session");
        assert_eq!(fs::read(target.join("settings")).unwrap(), b"theme dial\n");
        assert!(safe::is_same_tree(&Disk, &source, &target).unwrap(), "the copy reads back as what it copies");
        fs::write(target.join("signal/sessions/anna"), b"sessioN").unwrap();
        assert!(!safe::is_same_tree(&Disk, &source, &target).unwrap(), "a byte otherwise");
        fs::write(target.join("signal/sessions/anna"), b"session").unwrap();
        fs::write(target.join("more"), b"").unwrap();
        assert!(!safe::is_same_tree(&Disk, &source, &target).unwrap(), "a file more");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn exchange_swaps_names() {
        let root = scratch("safe-swap");
        fs::create_dir_all(root.join("one")).unwrap();
        fs::create_dir_all(root.join("other")).unwrap();
        fs::write(root.join("one/mark"), b"1").unwrap();
        exchange(&root.join("one"), &root.join("other")).unwrap();
        assert!(root.join("other/mark").exists() && !root.join("one/mark").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tree_moved_file_by_file() {
        let root = scratch("safe-move");
        let (source, target) = (root.join("camera"), root.join("camera.new"));
        let large: Vec<u8> = (0..safe::COMPARED_BYTES + 5).map(|at| at as u8).collect();
        fs::create_dir_all(source.join("pictures")).unwrap();
        fs::create_dir_all(source.join("videos")).unwrap();
        fs::write(source.join("pictures/a.jpg"), b"a").unwrap();
        fs::write(source.join("pictures/b.jpg"), b"b").unwrap();
        fs::write(source.join("videos/long.mp4"), &large).unwrap();
        std::os::unix::fs::symlink("a.jpg", source.join("pictures/link")).unwrap();
        // What a start cut short left: a file moved whole, and one copied in part.
        fs::create_dir_all(target.join("pictures")).unwrap();
        fs::write(target.join("pictures/moved.jpg"), b"moved").unwrap();
        fs::write(target.join("pictures/b.jpg"), b"").unwrap();
        assert_eq!(move_tree(&source, &target).unwrap(), 3, "each file that was still to move");
        assert_eq!(fs::read_dir(&source).unwrap().count(), 0, "nothing is left where they came from");
        assert_eq!(fs::read(target.join("pictures/b.jpg")).unwrap(), b"b", "a copy cut short is made anew");
        assert_eq!(fs::read(target.join("pictures/moved.jpg")).unwrap(), b"moved");
        assert_eq!(fs::read(target.join("videos/long.mp4")).unwrap(), large, "a file longer than one comparison");
        assert!(!target.join("pictures/link").exists(), "what is neither a directory nor a file is left out");
        fs::write(source.join("c.jpg"), b"same").unwrap();
        fs::write(target.join("c.jpg"), b"same").unwrap();
        fs::write(target.join("d.jpg"), b"sAme").unwrap();
        fs::write(target.join("e.jpg"), b"same and more").unwrap();
        assert_eq!(
            ["c.jpg", "d.jpg", "e.jpg"].map(|name| safe::is_same_file(
                &Disk,
                &source.join("c.jpg"),
                &target.join(name)
            )
            .unwrap()),
            [true, false, false],
            "byte for byte, and of one length"
        );
        fs::write(source.join("long.mp4"), &large[..large.len() - 1]).unwrap();
        assert!(
            !safe::is_same_file(&Disk, &source.join("long.mp4"), &target.join("videos/long.mp4")).unwrap(),
            "a byte more past one comparison"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn key_files_written_alike_and_read() {
        let root = scratch("safe-slots");
        assert!(read_slots(&root).is_empty(), "no key file yet");
        let key = crypto::new_safe_key().unwrap();
        let slot = crypto::seal_safe_key("opening", &key).unwrap();
        write_slots(&root, &slot).unwrap();
        let names: Vec<String> = super::super::files::names_in(&root, paths::Listed::Folders).unwrap();
        assert!(names.is_empty() && fs::read_dir(&root).unwrap().count() == KEY_FILES.len(), "two files, nothing else");
        assert_eq!(read_slots(&root), [slot.clone(), slot.clone()]);
        assert_eq!(check(&root, &"opening".into()), Ok(()));
        assert_eq!(check(&root, &"closing".into()), Err(Refusal::Wrong));
        assert_eq!(change(&root, "closing".into(), "another".into()), Err(Refusal::Wrong), "a wrong passphrase in use");
        assert_eq!(read_slots(&root), [slot.clone(), slot.clone()], "which leaves the key files as they are");
        assert_eq!(change(&root, "opening".into(), "another".into()), Ok(()));
        let changed = read_slots(&root);
        assert!(changed[0] == changed[1] && changed[0] != slot, "sealed anew, in both files alike");
        assert_eq!((check(&root, &"another".into()), check(&root, &"opening".into())), (Ok(()), Err(Refusal::Wrong)));
        let opened = open_slots(&changed, &"another".into()).map(|(opened, _)| *opened.bytes());
        assert_eq!(opened, Some(*key.bytes()), "the key itself is the one it was");
        fs::write(root.join(KEY_FILES[0]), b"not a key file").unwrap();
        assert_eq!(read_slots(&root), [changed[1].clone()], "a file that does not read leaves the other");
        assert_eq!(check(&root, &"another".into()), Ok(()));
        fs::remove_file(root.join(KEY_FILES[1])).unwrap();
        assert_eq!(check(&root, &"another".into()), Err(Refusal::Failed), "with no key file nothing is checked");
        assert_eq!(change(&root, "another".into(), "opening".into()), Err(Refusal::Failed));
        fs::remove_dir_all(&root).unwrap();
    }
}

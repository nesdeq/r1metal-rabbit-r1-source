use super::files::{self, Folder, names_in, write_over};
use super::{Shared, restore, safe};
use crate::{log, sys};
use r1_funcore::base::file_names::{self, Names};
use r1_funcore::fs::f2fs::UUID_SIZE;
use r1_funcore::storage::paths::{self, Listed, NO_DIRECTORY};
use r1_funcore::storage::safe::{Passphrase, Refusal};
use r1_funcore::storage::{Files, NOT_MOUNTED};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn replace(path: &Path, content: &[u8]) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| io::Error::other(NO_DIRECTORY))?;
    fs::create_dir_all(directory)?;
    write_over(directory, path, content)
}

fn replace_in_folder(path: &Path, content: &[u8]) -> io::Result<bool> {
    let folder = path.parent().ok_or_else(|| io::Error::other(NO_DIRECTORY))?;
    match write_over(folder, path, content) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The file names of a partition directory.
pub struct InDirectory<'a>(pub &'a Data, pub &'a str);

impl Names for InDirectory<'_> {
    type Error = io::Error;

    fn has(&mut self, name: &str) -> io::Result<bool> {
        self.0.has(self.1, name)
    }
}

/// The data partition as other threads see it, narrowed to a part's directories (`Storage::within`).
#[derive(Clone)]
pub struct Data {
    shared: Arc<Shared>,
    scope: Option<&'static [&'static str]>,
}

impl Data {
    pub(super) const fn new(shared: Arc<Shared>, scope: Option<&'static [&'static str]>) -> Data {
        Data { shared, scope }
    }

    /// True while the partition is mounted; does not wait.
    pub fn is_ready(&self) -> bool {
        self.shared.board.lock().progress().is_mounted()
    }

    /// True once the partition is no longer being brought up; does not wait.
    pub fn is_settled(&self) -> bool {
        !self.shared.board.lock().progress().is_underway()
    }

    /// Waits while the partition is brought up; true if it is mounted.
    pub fn wait_settled(&self) -> bool {
        self.shared.board.wait(|state| state.progress().settled())
    }

    /// Waits until the partition is mounted or given up, however late it appears; true if mounted.
    pub fn wait_decided(&self) -> bool {
        self.shared.board.wait(|state| state.progress().decided())
    }

    /// The mounted filesystem's id after `wait_decided`; none if not mounted.
    pub fn id(&self) -> Option<[u8; UUID_SIZE]> {
        self.wait_decided().then(|| self.shared.board.lock().id()).flatten()
    }

    /// A failed partition, for tests.
    #[cfg(test)]
    pub fn unusable() -> Data {
        let data = Data::new(Arc::new(Shared::new().expect("an eventfd")), None);
        data.shared.enter(r1_funcore::storage::progress::Progress::Failed(String::new()));
        data
    }

    /// A partition mounted at `root`, for tests.
    #[cfg(test)]
    pub fn with_root(root: &Path) -> Data {
        let data = Data::new(Arc::new(Shared::new().expect("an eventfd")), None);
        let _ = data.shared.scratch.set(Box::leak(root.to_path_buf().into_boxed_path()));
        data.shared.enter(r1_funcore::storage::progress::Progress::Mounted(0));
        data
    }

    #[cfg(test)]
    pub(super) fn within(&self, scope: &'static [&'static str]) -> Data {
        Data::new(Arc::clone(&self.shared), Some(scope))
    }

    fn root(&self) -> io::Result<&'static Path> {
        let mounted = self.shared.board.lock().progress().root().map(Path::new);
        #[cfg(test)]
        let mounted = self.shared.scratch.get().copied().or(mounted);
        mounted.ok_or_else(|| io::Error::other(NOT_MOUNTED))
    }

    pub(super) fn directory_path(&self, directory: &str) -> io::Result<PathBuf> {
        let directory = paths::to_directory_path(directory).map_err(sys::invalid)?;
        paths::check_scope(self.scope, directory).map_err(sys::invalid)?;
        Ok(self.root()?.join(directory))
    }

    fn file_path(&self, directory: &str, name: &str) -> io::Result<PathBuf> {
        let file = paths::to_file_path(directory, name).map_err(sys::invalid)?;
        paths::check_scope(self.scope, directory).map_err(sys::invalid)?;
        Ok(self.root()?.join(file))
    }

    fn new_file_directory(&self, directory: &str, stem: &str, extension: &str) -> io::Result<PathBuf> {
        paths::to_file_path(directory, &file_names::numbered_file(stem, extension, 0)).map_err(sys::invalid)?;
        self.directory_path(directory)
    }

    /// Makes a directory with the ones above it; returns where it lies.
    pub fn make(&self, directory: &str) -> io::Result<PathBuf> {
        let path = self.directory_path(directory)?;
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// A file's bytes; none if the file is missing.
    pub fn read(&self, directory: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.file_path(directory, name)?) {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The sorted names of what a directory holds of `listed`; a missing directory, or a path that leads through a file, holds none.
    pub fn names(&self, directory: &str, listed: Listed) -> io::Result<Vec<String>> {
        match names_in(&self.directory_path(directory)?, listed) {
            Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Ok(Vec::new()),
            names => names,
        }
    }

    /// True if a directory has a file of that name.
    pub fn has(&self, directory: &str, name: &str) -> io::Result<bool> {
        files::exists(&self.file_path(directory, name)?)
    }

    /// Saves `content` as a new file named `stem` and `extension`, with a count if taken; returns the name.
    pub fn save(&self, directory: &str, stem: &str, extension: &str, content: &[u8]) -> io::Result<String> {
        save_in(&self.new_file_directory(directory, stem, extension)?, stem, extension, content)
    }

    /// Makes a new empty file, named as `save` names one; returns it with its name.
    pub fn create(&self, directory: &str, stem: &str, extension: &str) -> io::Result<(File, String)> {
        create_in(&self.new_file_directory(directory, stem, extension)?, stem, extension)
    }

    /// The writable bytes left on the partition.
    pub fn free(&self) -> io::Result<u64> {
        sys::free_bytes(self.root()?)
    }

    /// Discards the partition's free blocks, so removed files do not stay readable on the medium.
    pub fn trim(&self) -> io::Result<()> {
        files::fitrim(self.root()?).map(drop)
    }

    /// The partition's total and writable bytes.
    pub fn room(&self) -> io::Result<(u64, u64)> {
        let root = self.root()?;
        Ok((sys::capacity(root)?.0, sys::free_bytes(root)?))
    }

    // The partition's total bytes and its block.
    fn blocks(&self) -> io::Result<(u64, u32)> {
        sys::capacity(self.root()?)
    }

    // A file's or folder's size in bytes and its times of change, access and status change, in seconds of UTC; none if missing.
    fn stat(&self, directory: &str, name: &str) -> io::Result<Option<(u64, [u64; 3])>> {
        let metadata = match fs::metadata(self.file_path(directory, name)?) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let seconds = |time: i64| u64::try_from(time).unwrap_or(0);
        let times = [seconds(metadata.mtime()), seconds(metadata.atime()), seconds(metadata.ctime())];
        Ok(Some((metadata.len(), times)))
    }

    /// The mount point, for the drive; a narrowed handle is refused (`paths::check_full_scope`).
    pub fn export_root(&self) -> io::Result<&'static Path> {
        paths::check_full_scope(self.scope).map_err(sys::invalid)?;
        self.root()
    }

    /// Whether a passphrase is the open safe's; one that could not be asked of the safe is told so, and a narrowed handle is refused (`paths::check_full_scope`).
    pub fn check_passphrase(&self, passphrase: &Passphrase) -> Result<(), Refusal> {
        match self.export_root() {
            Ok(root) => safe::check(root, passphrase),
            Err(e) => {
                log!("safe: checking a passphrase: {e}");
                Err(Refusal::Failed)
            }
        }
    }

    /// Makes the directory a restore's files are staged in, in place of what was staged or confirmed before, its private directory and public roots encrypted under the safe's key; a narrowed handle is refused.
    pub fn stage(&self) -> io::Result<()> {
        restore::stage(self.export_root()?)
    }

    /// Confirms the staged files, which the next start puts in the partition's place; a narrowed handle is refused.
    pub fn commit(&self) -> io::Result<()> {
        restore::commit(self.export_root()?)
    }

    /// Opens a file for reading.
    pub fn open(&self, directory: &str, name: &str) -> io::Result<File> {
        File::open(self.file_path(directory, name)?)
    }

    /// Replaces or makes a file atomically.
    pub fn put(&self, directory: &str, name: &str, content: &[u8]) -> io::Result<()> {
        replace(&self.file_path(directory, name)?, content)
    }

    /// `put` into an existing folder; false, with nothing written, if the folder is gone.
    pub fn put_into(&self, folder: &str, name: &str, content: &[u8]) -> io::Result<bool> {
        replace_in_folder(&self.file_path(folder, name)?, content)
    }

    /// Appends `content` to a file, making it if missing, and syncs it.
    pub fn append(&self, directory: &str, name: &str, content: &[u8]) -> io::Result<()> {
        let path = self.file_path(directory, name)?;
        fs::create_dir_all(self.directory_path(directory)?)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(content)?;
        file.sync_data()
    }

    /// A file as text; none if the file is missing.
    pub fn text(&self, directory: &str, name: &str) -> io::Result<Option<String>> {
        Ok(self.read(directory, name)?.map(|content| String::from_utf8_lossy(&content).into_owned()))
    }

    /// Removes a file; a missing one is no error.
    pub fn remove(&self, directory: &str, name: &str) -> io::Result<()> {
        match fs::remove_file(self.file_path(directory, name)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Removes a folder with its content; a missing one is no error, a name leaving the directory is refused.
    pub fn discard(&self, directory: &str, folder: &str) -> io::Result<()> {
        match fs::remove_dir_all(self.file_path(directory, folder)?) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

impl Files for &Data {
    type Error = io::Error;

    fn has(&mut self, directory: &str, name: &str) -> io::Result<bool> {
        Data::has(self, directory, name)
    }

    fn read(&mut self, directory: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        Data::read(self, directory, name)
    }

    fn remove(&mut self, directory: &str, name: &str) -> io::Result<()> {
        Data::remove(self, directory, name)
    }

    fn free(&mut self) -> io::Result<u64> {
        Data::free(self)
    }

    fn put(&mut self, directory: &str, name: &str, content: &[u8]) -> io::Result<()> {
        Data::put(self, directory, name, content)
    }

    fn put_into(&mut self, folder: &str, name: &str, content: &[u8]) -> io::Result<bool> {
        Data::put_into(self, folder, name, content)
    }

    fn save(&mut self, directory: &str, stem: &str, extension: &str, content: &[u8]) -> io::Result<String> {
        Data::save(self, directory, stem, extension, content)
    }

    fn to_error(&self, reason: &str) -> io::Error {
        io::Error::other(reason)
    }

    fn names(&mut self, directory: &str, listed: Listed) -> io::Result<Vec<String>> {
        Data::names(self, directory, listed)
    }

    fn make(&mut self, directory: &str) -> io::Result<()> {
        Data::make(self, directory).map(drop)
    }

    fn discard(&mut self, directory: &str, folder: &str) -> io::Result<()> {
        Data::discard(self, directory, folder)
    }

    fn blocks(&mut self) -> io::Result<(u64, u32)> {
        Data::blocks(self)
    }

    fn stat(&mut self, directory: &str, name: &str) -> io::Result<Option<(u64, [u64; 3])>> {
        Data::stat(self, directory, name)
    }
}

fn save_in(directory: &Path, stem: &str, extension: &str, content: &[u8]) -> io::Result<String> {
    let name = file_names::first_free(|count| file_names::numbered_file(stem, extension, count), Folder(directory))?;
    replace(&directory.join(&name), content)?;
    Ok(name)
}

fn create_in(directory: &Path, stem: &str, extension: &str) -> io::Result<(File, String)> {
    fs::create_dir_all(directory)?;
    for name in file_names::numbered_files(stem, extension) {
        match OpenOptions::new().write(true).create_new(true).open(directory.join(&name)) {
            Ok(file) => {
                File::open(directory)?.sync_all()?;
                return Ok((file, name));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    unreachable!("a directory holds fewer names than a u32 counts")
}

#[cfg(test)]
mod tests {
    use super::super::scratch;
    use super::*;
    use r1_funcore::storage::progress::Progress;
    use r1_funcore::storage::{NEWS, NOTES, PICTURE_EXTENSION, PICTURES, TALKS, VIDEO_EXTENSION};
    use std::thread;

    #[test]
    fn replace_file() {
        let root = scratch("replace");
        let path = root.join("battery/gauge");
        replace(&path, b"first").unwrap();
        replace(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        let names: Vec<_> =
            fs::read_dir(root.join("battery")).unwrap().flatten().map(|entry| entry.file_name()).collect();
        assert_eq!(names, ["gauge"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replace_in_existing_folder() {
        let root = scratch("replace-in");
        let folder = root.join("notes/20261012_190328");
        fs::create_dir_all(&folder).unwrap();
        let words = folder.join("words.md");
        assert!(replace_in_folder(&words, b"first").unwrap() && replace_in_folder(&words, b"second").unwrap());
        assert_eq!(fs::read(&words).unwrap(), b"second");
        fs::remove_dir_all(&folder).unwrap();
        assert!(!replace_in_folder(&words, b"third").unwrap(), "a folder that is gone takes nothing");
        assert!(!folder.exists() && root.join("notes").exists(), "and is not made anew");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn save_takes_free_name() {
        let root = scratch("save");
        let names: Vec<String> = [b"one", b"two", b"six"]
            .iter()
            .map(|content| save_in(&root, "20261009_193512", PICTURE_EXTENSION, *content).unwrap())
            .collect();
        assert_eq!(names, ["20261009_193512.jpg", "20261009_193512_1.jpg", "20261009_193512_2.jpg"]);
        assert_eq!(fs::read(root.join(&names[1])).unwrap(), b"two");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 3, "nothing is left of the writing");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_takes_free_name() {
        let root = scratch("create");
        let (mut first, name) = create_in(&root, "20261009_193512", VIDEO_EXTENSION).unwrap();
        first.write_all(b"head").unwrap();
        let (_, second) = create_in(&root, "20261009_193512", VIDEO_EXTENSION).unwrap();
        assert_eq!((name.as_str(), second.as_str()), ("20261009_193512.mp4", "20261009_193512_1.mp4"));
        assert_eq!(fs::read(root.join(&name)).unwrap(), b"head", "the file that was there is left as it is");
        assert_eq!(fs::read(root.join(&second)).unwrap(), b"");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn list_by_extension() {
        let root = scratch("list");
        fs::create_dir_all(root.join("20261009_100000.jpg")).unwrap();
        for name in [
            "20261009_193512.jpg",
            "20261008_080000.JPG",
            "20261009_193512_1.jpg",
            "20261009_200000.mp4",
            "notes",
            "20261009_193512.jpg.new",
        ] {
            fs::write(root.join(name), b"x").unwrap();
        }
        assert_eq!(
            names_in(&root, Listed::Extension(PICTURE_EXTENSION)).unwrap(),
            ["20261008_080000.JPG", "20261009_193512.jpg", "20261009_193512_1.jpg"],
            "no directory, no other kind, nothing half written"
        );
        assert_eq!(names_in(&root, Listed::Extension(VIDEO_EXTENSION)).unwrap(), ["20261009_200000.mp4"]);
        assert_eq!(
            names_in(&root, Listed::Files).unwrap(),
            ["20261008_080000.JPG", "20261009_193512.jpg", "20261009_193512_1.jpg", "20261009_200000.mp4", "notes"],
            "the files written whole, whatever their names"
        );
        assert!(names_in(&root, Listed::Extension("png")).unwrap().is_empty());
        assert!(names_in(&root.join("nowhere"), Listed::Extension(PICTURE_EXTENSION)).is_err());
        let data = Data::with_root(&root);
        assert_eq!(
            [Listed::Files, Listed::AllFiles, Listed::Extension(PICTURE_EXTENSION), Listed::Folders]
                .map(|listed| data.names("nowhere", listed).unwrap()),
            [const { Vec::<String>::new() }; 4],
            "a directory of the partition that is not there holds none, whatever is listed"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folders_and_discard() {
        let root = scratch("folders");
        for folder in ["20261012_190328", "20261011_083000", "20261012_190328_1"] {
            fs::create_dir_all(root.join("notes").join(folder)).unwrap();
            fs::write(root.join("notes").join(folder).join(format!("{folder}.m4a")), b"x").unwrap();
        }
        fs::write(root.join("notes/stray.md"), b"x").unwrap();
        fs::write(root.join("kept"), b"x").unwrap();
        assert_eq!(
            names_in(&root.join("notes"), Listed::Folders).unwrap(),
            ["20261011_083000", "20261012_190328", "20261012_190328_1"],
            "the folders, in the order of their names, and no file"
        );
        assert!(names_in(&root.join("nowhere"), Listed::Folders).is_err());
        let data = Data::with_root(&root);
        data.discard("notes", "20261012_190328").unwrap();
        data.discard("notes", "20261012_190328").unwrap();
        assert_eq!(
            names_in(&root.join("notes"), Listed::Folders).unwrap(),
            ["20261011_083000", "20261012_190328_1"],
            "gone with its file, and nothing is amiss when it is gone already"
        );
        for name in ["", ".", "..", "../kept", "20261011_083000/..", "/", "a/b"] {
            assert!(data.discard("notes", name).is_err(), "{name:?} is no folder of the directory");
        }
        assert!(root.join("kept").exists() && root.join("notes/stray.md").exists() && root.join("notes").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn needs_mounted_partition() {
        let data = Data::unusable();
        assert!(!data.is_ready() && !data.wait_settled() && !data.wait_decided() && data.id().is_none());
        assert!(
            data.put(PICTURES, "photo.jpg", b"x").is_err()
                && data.put_into(PICTURES, "photo.jpg", b"x").is_err()
                && data.names(PICTURES, Listed::Files).is_err()
                && data.names(PICTURES, Listed::AllFiles).is_err()
                && data.trim().is_err()
                && data.save(PICTURES, "photo", PICTURE_EXTENSION, b"x").is_err()
                && data.free().is_err()
                && data.room().is_err()
                && data.export_root().is_err(),
            "every way to a file goes through where the partition is mounted, which it is not"
        );
        let waiter = data.clone();
        data.shared.enter(Progress::Looking);
        assert!(!data.is_ready(), "a partition being brought up is not ready, and the asker does not wait for it");
        let waiting = thread::spawn(move || waiter.wait_settled());
        data.shared.enter(Progress::Mounted(1 << 30));
        assert!(waiting.join().unwrap() && data.is_ready(), "the wait ends when the partition is mounted");
        data.shared.enter(Progress::Waiting);
        assert!(!data.wait_settled(), "a partition that has not appeared is not waited for");
        let waiter = data.clone();
        let deciding = thread::spawn(move || waiter.wait_decided());
        data.shared.enter(Progress::Formatting);
        data.shared.board.lock().set_id([7; UUID_SIZE]);
        data.shared.enter(Progress::Mounted(1 << 30));
        assert!(deciding.join().unwrap(), "unless whoever asks wants it decided");
        assert_eq!(data.id(), Some([7; UUID_SIZE]), "a mounted partition tells its filesystem's id");
    }

    #[test]
    fn refuse_path_escape() {
        let data = Data::unusable();
        let is_refused = |e: io::Error| e.kind() == io::ErrorKind::InvalidData;
        let (bad_directory, bad_name) = ("news/../.r1metal/signal", "../x_news.md");
        let results = [
            ("read", data.read(bad_directory, "account").map(drop), data.read(NEWS, bad_name).map(drop)),
            ("text", data.text(bad_directory, "account").map(drop), data.text(NEWS, bad_name).map(drop)),
            ("has", data.has(bad_directory, "account").map(drop), data.has(NEWS, bad_name).map(drop)),
            ("open", data.open(bad_directory, "account").map(drop), data.open(NEWS, bad_name).map(drop)),
            ("put", data.put(bad_directory, "account", b"x"), data.put(NEWS, bad_name, b"x")),
            (
                "put_into",
                data.put_into(bad_directory, "account", b"x").map(drop),
                data.put_into(NEWS, bad_name, b"x").map(drop),
            ),
            ("append", data.append(bad_directory, "account", b"x"), data.append(NEWS, bad_name, b"x")),
            ("remove", data.remove(bad_directory, "account"), data.remove(NEWS, bad_name)),
            (
                "save",
                data.save(bad_directory, "photo", PICTURE_EXTENSION, b"x").map(drop),
                data.save(PICTURES, "../photo", PICTURE_EXTENSION, b"x").map(drop),
            ),
            (
                "create",
                data.create(bad_directory, "video", VIDEO_EXTENSION).map(drop),
                data.create(PICTURES, "../video", VIDEO_EXTENSION).map(drop),
            ),
            ("discard", data.discard(bad_directory, "20261016"), data.discard(NEWS, "..")),
            ("names", data.names(bad_directory, Listed::Files).map(drop), data.names("/news", Listed::Files).map(drop)),
            (
                "names of all files",
                data.names(bad_directory, Listed::AllFiles).map(drop),
                data.names("/news", Listed::AllFiles).map(drop),
            ),
            (
                "names by extension",
                data.names(bad_directory, Listed::Extension(PICTURE_EXTENSION)).map(drop),
                data.names("./news", Listed::Extension(PICTURE_EXTENSION)).map(drop),
            ),
            (
                "names of folders",
                data.names(bad_directory, Listed::Folders).map(drop),
                data.names("news/", Listed::Folders).map(drop),
            ),
        ];
        for (label, directory, name) in results {
            assert!(
                directory.is_err_and(is_refused) && name.is_err_and(is_refused),
                "{label}: a directory or a name that leads elsewhere names no file of the partition"
            );
        }
        assert!(
            data.text("news/20261016", "20261016_news.md").is_err_and(|e| !is_refused(e)),
            "a file of the partition is looked for, here on one that is not mounted"
        );
    }

    #[test]
    fn scope_refuses_other_directories() {
        let within = |scope: &'static [&'static str]| Data::unusable().within(scope);
        let notes = within(&[NOTES]);
        let is_refused = |e: io::Error| e.kind() == io::ErrorKind::InvalidData;
        assert!(
            notes.read(".r1metal/signal", "account").is_err_and(is_refused)
                && notes.names(PICTURES, Listed::Files).is_err_and(is_refused)
                && notes.put(NEWS, "x", b"x").is_err_and(is_refused)
                && notes.discard(TALKS, "20261012_190328").is_err_and(is_refused)
                && notes.save("audio", "x", PICTURE_EXTENSION, b"x").is_err_and(is_refused),
            "a directory it was not handed, or one above its own, names no file of its"
        );
        assert!(
            notes.names(NOTES, Listed::Folders).is_err_and(|e| !is_refused(e))
                && notes.has("audio/notes/20261012_190328", "20261012_190328.m4a").is_err_and(|e| !is_refused(e)),
            "what lies in or under its own is looked for, here on a partition that is not mounted"
        );
        assert!(
            within(&[]).room().is_err_and(|e| !is_refused(e)),
            "and what names no directory answers as it does for the whole"
        );
        assert!(
            notes.export_root().is_err_and(is_refused) && within(&[]).export_root().is_err_and(is_refused),
            "but where the partition is mounted, which a part handed it whole alone is told"
        );
    }

    #[test]
    fn files_lists_half_written() {
        let root = scratch("files");
        let data = Data::with_root(&root);
        assert_eq!(
            data.names(NEWS, Listed::AllFiles).unwrap(),
            Vec::<String>::new(),
            "a directory that is not there holds none"
        );
        data.put(NEWS, "edition", b"x").unwrap();
        fs::write(root.join(NEWS).join("edition.new"), b"cut").unwrap();
        fs::create_dir_all(root.join(NEWS).join("20261016")).unwrap();
        assert_eq!(data.names(NEWS, Listed::AllFiles).unwrap(), ["edition", "edition.new"]);
        assert_eq!(
            data.names(NEWS, Listed::Files).unwrap(),
            ["edition"],
            "where the files put there are only the whole ones"
        );
        assert_eq!(data.names(NEWS, Listed::Folders).unwrap(), ["20261016"]);
        assert_eq!(
            [Listed::Files, Listed::Folders].map(|listed| data.names("news/edition/deeper", listed).unwrap()),
            [const { Vec::<String>::new() }; 2],
            "nor does a path that leads through a file"
        );
        assert_eq!(
            data.stat(NEWS, "edition").unwrap().map(|(size, _)| size),
            Some(1),
            "a file's size comes with its times"
        );
        assert_eq!(data.stat(NEWS, "nothing").unwrap(), None);
        let result = data.trim();
        assert!(
            result.as_ref().is_ok() || result.as_ref().is_err_and(|e| e.raw_os_error().is_some()),
            "the kernel is asked to hand the free blocks back, which a filesystem may refuse: {result:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

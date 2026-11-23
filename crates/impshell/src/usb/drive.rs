use crate::storage::Data;
use crate::sync::Board;
use crate::sys::lock;
use crate::usb::fuse;
use crate::{clock, log, sys, usb};
use r1_funcore::fs::exfat::{Content, Files, Node, Spec, Volume};
use r1_funcore::fs::fuse::Source;
use r1_funcore::fs::to_modified;
use r1_funcore::hw::uevent::HostState;
use r1_funcore::hw::usb::{HostChanges, Medium, MediumStep, NO_MEDIUM, OpenFile, serial, to_snapshot_line};
use r1_funcore::storage::{LABEL, paths};
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

const MOUNT_POINT: &str = "/drive";
const FILE_NAME: &str = "volume";

struct Snapshot {
    volume: Volume,
    paths: Vec<PathBuf>,
    open_file: Mutex<OpenFile<File>>,
}

fn scan(directory: &Path, is_root: bool, paths: &mut Vec<PathBuf>) -> Vec<Node> {
    let mut nodes = Vec::new();
    for entry in fs::read_dir(directory).into_iter().flatten().flatten() {
        let name = entry.file_name();
        if !paths::is_shown_to_hosts(&name.to_string_lossy(), is_root) {
            continue;
        }
        let meta = entry.metadata();
        let content = match (name.to_str(), &meta) {
            (Some(_), Ok(meta)) if meta.is_dir() => Content::Directory(scan(&entry.path(), false, paths)),
            (Some(_), Ok(meta)) if meta.is_file() => {
                paths.push(entry.path());
                Content::File { id: paths.len() - 1, size: meta.len() }
            }
            _ => Content::Other,
        };
        let modified = meta.map_or(0, |meta| to_modified(meta.mtime()));
        nodes.push(Node { name: name.to_string_lossy().into_owned(), modified, content });
    }
    nodes
}

impl Snapshot {
    fn take(root: &Path, serial: u32) -> io::Result<Snapshot> {
        let (capacity, cluster_size) = sys::capacity(root)?;
        let mut paths = Vec::new();
        let nodes = scan(root, true, &mut paths);
        // The files' times are the system's, which keeps UTC: a host is shown them as the clock on screen tells them, with the offset that is set.
        let spec = Spec { label: LABEL, serial, cluster_size, capacity, offset: Some(clock::offset()) };
        let volume = Volume::build(&spec, nodes).map_err(|e| io::Error::other(e.message()))?;
        log!("{}", to_snapshot_line(paths.len(), volume.used_bytes(), volume.skip_count()));
        Ok(Snapshot { volume, paths, open_file: Mutex::default() })
    }
}

impl Files for Snapshot {
    fn fetch(&self, id: usize, offset: u64, buffer: &mut [u8]) {
        let (path, mut open_file) = (self.paths[id].display(), lock(&self.open_file));
        if !open_file.is_open(id) {
            let file = File::open(&self.paths[id]);
            if let Err(e) = &file
                && open_file.is_failure_new(id)
            {
                log!("drive: opening {path}: {e}");
            }
            open_file.set(id, file.ok());
        }
        match open_file.file().map(|file| sys::read_up_to(file, offset, buffer)) {
            Some(Err(e)) if open_file.is_failure_new(id) => log!("drive: reading {path}: {e}"),
            Some(Ok(count)) if count < buffer.len() && open_file.is_failure_new(id) => {
                log!("drive: {path} ends before what the host was shown of it");
            }
            _ => {}
        }
    }
}

impl Source for Snapshot {
    fn size(&self) -> u64 {
        self.volume.size()
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) {
        self.volume.read(offset, buffer, self);
    }
}

#[derive(Default)]
struct Slot(Mutex<Option<Arc<Snapshot>>>);

impl Slot {
    fn current(&self) -> Option<Arc<Snapshot>> {
        lock(&self.0).clone()
    }

    fn set(&self, snapshot: Option<Arc<Snapshot>>) {
        *lock(&self.0) = snapshot;
    }
}

impl Source for Slot {
    fn size(&self) -> u64 {
        self.current().as_deref().size()
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) {
        self.current().as_deref().read(offset, buffer);
    }
}

struct Host {
    board: Board<HostChanges>,
}

impl Host {
    fn new() -> io::Result<Host> {
        Ok(Host { board: Board::new(HostChanges::default())? })
    }

    fn note(&self, state: HostState) {
        self.board.lock().note(state);
        self.board.notify();
    }

    fn reset(&self) {
        self.board.lock().reset();
    }

    fn set_shown(&self, is_shown: bool) {
        if self.board.lock().set_shown(is_shown) {
            self.board.notify();
        }
    }

    fn is_offered(&self) -> bool {
        self.board.lock().is_offered(usb::is_configured())
    }

    fn wait_change(&self) -> bool {
        self.board.wait(HostChanges::take)
    }
}

fn run(data: &Data, serial: u32, host: &Host) -> io::Result<()> {
    let (mount_point, medium_file) = (Path::new(MOUNT_POINT), usb::medium_file());
    let volume_path = mount_point.join(FILE_NAME);
    // What the host did up to now shows in the state the drive reads first.
    host.reset();
    let _ = fs::write(&medium_file, NO_MEDIUM);
    let _ = sys::detach(mount_point);
    fs::create_dir_all(mount_point)?;
    let connection = fuse::mount(mount_point)?;
    let slot = Arc::new(Slot::default());
    let source = Arc::clone(&slot);
    thread::Builder::new()
        .name("drive fuse".into())
        .spawn(move || log!("drive: {}", fuse::serve(&connection, FILE_NAME, &*source)))?;

    let (mut medium, mut has_left) = (Medium::default(), false);
    loop {
        medium.begin_pass(host.is_offered(), has_left);
        loop {
            match medium.next_step() {
                MediumStep::Eject => match fs::write(&medium_file, NO_MEDIUM) {
                    Ok(()) => {
                        slot.set(None);
                        medium.set_loaded(false);
                    }
                    Err(e) => log!("drive: the host holds on to the medium: {e}"),
                },
                MediumStep::Load => {
                    let root = data.export_root();
                    let result = root.and_then(|root| Snapshot::take(root, serial)).and_then(|snapshot| {
                        slot.set(Some(Arc::new(snapshot)));
                        fs::write(&medium_file, volume_path.as_os_str().as_encoded_bytes())
                    });
                    match result {
                        Ok(()) => medium.set_loaded(true),
                        Err(e) => {
                            slot.set(None);
                            log!("drive: no medium: {e}");
                        }
                    }
                }
                MediumStep::Wait => break,
            }
        }
        has_left = host.wait_change();
    }
}

/// The USB drive; its thread loads and ejects the medium.
pub struct Drive {
    host: Arc<Host>,
}

impl Drive {
    /// Starts the thread, which offers the partition to hosts once mounted and ends if it is given up.
    pub fn start(data: Data) -> io::Result<Drive> {
        let host = Arc::new(Host::new()?);
        let worker = Arc::clone(&host);
        thread::Builder::new().name("drive".into()).spawn(move || {
            if let Some(id) = data.id()
                && let Err(e) = run(&data, serial(id), &worker)
            {
                log!("drive: {e}");
            }
        })?;
        Ok(Drive { host })
    }

    /// Takes the gadget's state as the kernel tells it.
    pub fn set_host_state(&self, state: HostState) {
        self.host.note(state);
    }

    /// Takes whether the owner has the drive shown to a host: off, no host is offered a medium, and one that is in goes.
    pub fn set_shown(&self, is_shown: bool) {
        self.host.set_shown(is_shown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("r1-drive-{test}-{}", std::process::id()));
        for directory in [".r1metal", "camera", "files/empty"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        for (path, content) in [
            (".r1metal/key", "secret"),
            ("camera/b.jpg", "picture"),
            ("camera/a.jpg", "another"),
            ("files/note.txt", "0123456789"),
        ] {
            fs::write(root.join(path), content).unwrap();
        }
        std::os::unix::fs::symlink("note.txt", root.join("files/link")).unwrap();
        root
    }

    fn add_lines(nodes: &[Node], paths: &[PathBuf], (root, prefix): (&Path, &str), lines: &mut Vec<String>) {
        for node in nodes {
            let path = format!("{prefix}{}", node.name);
            match &node.content {
                Content::Directory(children) => {
                    lines.push(format!("{path}/"));
                    add_lines(children, paths, (root, &format!("{path}/")), lines);
                }
                Content::File { id, size } => {
                    assert_eq!(paths[*id], root.join(&path));
                    lines.push(format!("{path} {size}"));
                }
                Content::Other => lines.push(format!("{path} ?")),
            }
        }
    }

    #[test]
    fn scan_skips_hidden_directory() {
        let root = tree("scan");
        let mut paths = Vec::new();
        let nodes = scan(&root, true, &mut paths);
        let mut lines = Vec::new();
        add_lines(&nodes, &paths, (&root, ""), &mut lines);
        lines.sort();
        assert_eq!(
            lines,
            [
                "camera/",
                "camera/a.jpg 7",
                "camera/b.jpg 7",
                "files/",
                "files/empty/",
                "files/link ?",
                "files/note.txt 10"
            ],
            "the symbolic link is neither file nor directory"
        );
        assert_eq!(paths.len(), 3);
        assert!(nodes.iter().all(|node| node.modified > 0));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fetch_reads_current_files() {
        let root = tree("fetch");
        let paths = vec![root.join("files/note.txt"), root.join("camera/a.jpg"), root.join("gone")];
        let snapshot = Snapshot {
            volume: Volume::build(
                &Spec { label: LABEL, serial: 1, cluster_size: 4096, capacity: 0, offset: None },
                Vec::new(),
            )
            .unwrap(),
            paths,
            open_file: Mutex::default(),
        };
        let read = |id: usize, offset: u64| {
            let mut buffer = [0u8; 6];
            snapshot.fetch(id, offset, &mut buffer);
            buffer
        };
        assert_eq!(&read(0, 2), b"234567");
        assert_eq!(&read(0, 8), b"89\0\0\0\0");
        assert_eq!(&read(1, 0), b"anothe");
        assert_eq!(&read(0, 0), b"012345");
        assert_eq!(&read(2, 0), &[0; 6]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reset_forgets_host_changes() {
        let host = Host::new().unwrap();
        host.note(HostState::Disconnected);
        assert!(host.wait_change(), "a host that went away is heard of");
        host.note(HostState::Disconnected);
        host.reset();
        host.note(HostState::Configured);
        assert!(!host.wait_change(), "what was said before the drive began counts for nothing");
        host.set_shown(true);
        assert!(!host.wait_change(), "the owner's switch wakes the thread, with no host gone for it");
        host.reset();
        assert!(host.board.lock().is_offered(true), "and what the owner switched stays through a reset");
    }
}

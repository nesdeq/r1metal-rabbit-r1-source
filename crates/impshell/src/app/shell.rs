use crate::crew::Crew;
use crate::crypto::Sum;
use crate::storage::Data;
use crate::sync::Board;
use crate::sys::klog;
use crate::sys::{self, PAGE};
use r1_funcore::base::code;
use r1_funcore::base::handoff::Step;
use r1_funcore::codec::vp8::Laps;
use r1_funcore::domain::console::bench::{self, Bench, Conversion, Place, Scene, Timing};
use r1_funcore::domain::console::command::Command;
use r1_funcore::domain::console::shell::{self, EntryKind, NOT_A_VERB, NOT_BASE64, Paths, State, to_listing};
use r1_funcore::domain::console::wire::Reply;
use r1_funcore::hw::{CORES, LARGE_CORES};
use r1_funcore::net::wireguard::config::Config;
use r1_funcore::storage::safe::Passphrase;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem;
use std::os::fd::RawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;

struct Kernel;

impl Paths for Kernel {
    type Path = PathBuf;

    fn current_dir(&self) -> PathBuf {
        PathBuf::from(".")
    }

    fn canonical(&mut self, path: &PathBuf) -> Option<PathBuf> {
        fs::canonicalize(path).ok()
    }

    fn read_link(&mut self, path: &PathBuf) -> Option<PathBuf> {
        fs::read_link(path).ok()
    }

    fn parent(&self, path: &PathBuf) -> Option<PathBuf> {
        path.parent().map(Path::to_path_buf)
    }

    fn join(&self, directory: &PathBuf, path: &PathBuf) -> PathBuf {
        directory.join(path)
    }

    fn parts(&self, path: &PathBuf) -> Vec<PathBuf> {
        path.components().map(|part| PathBuf::from(part.as_os_str())).collect()
    }

    fn join_parts(&self, parts: &[PathBuf]) -> PathBuf {
        parts.iter().collect()
    }

    fn to_text(&self, path: &PathBuf) -> String {
        path.to_string_lossy().into_owned()
    }
}

fn is_path_denied(path: &Path, is_unlocked: bool) -> bool {
    shell::is_path_denied(&mut Kernel, &path.to_path_buf(), is_unlocked)
}

fn open_nonblocking(path: &str) -> io::Result<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)
}

fn list(path: &str, is_unlocked: bool, reply: &mut Reply) -> io::Result<()> {
    let is_listed = |entry: &fs::DirEntry| !is_path_denied(&entry.path(), is_unlocked);
    let entries = fs::read_dir(path)?.flatten().filter(is_listed).map(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = match entry.file_type() {
            Ok(kind) if kind.is_dir() => EntryKind::Directory,
            Ok(kind) if kind.is_symlink() => EntryKind::Link,
            _ => EntryKind::File,
        };
        (name, kind)
    });
    to_listing(entries).iter().for_each(|line| reply.data(line));
    Ok(())
}

// Reads a file opened without blocking by pages, `most` bytes at the most, and hands each to `take`: to its end, or as far as it has bytes at hand, so that what has no end, a device or a pipe, holds the shell's thread for nothing.
fn read_at_hand(file: File, most: u64, mut take: impl FnMut(&[u8])) -> io::Result<()> {
    let (mut file, mut chunk) = (file.take(most), [0u8; PAGE]);
    loop {
        match file.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(count) => take(&chunk[..count]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

// A file as far as a verb that reads to its end takes it.
fn read_whole(path: &str, take: impl FnMut(&[u8])) -> io::Result<()> {
    let file = open_nonblocking(path)?;
    let most = shell::most_read(file.metadata()?.is_file());
    read_at_hand(file, most, take)
}

fn cat(path: &str, reply: &mut Reply) -> io::Result<()> {
    let mut bytes = Vec::new();
    read_whole(path, |part| bytes.extend_from_slice(part))?;
    reply.data_text(&bytes);
    Ok(())
}

fn dmesg(reply: &mut Reply) -> io::Result<()> {
    klog::records(&mut klog::reader()?, |record| reply.data_text(record))
}

fn read_part(path: &str, offset: u64, length: u32, reply: &mut Reply) -> io::Result<()> {
    let mut file = open_nonblocking(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut part = Vec::new();
    read_at_hand(file, u64::from(length), |bytes| part.extend_from_slice(bytes))?;
    reply.data(&code::base64(&part));
    Ok(())
}

fn append(path: &str, data: &str) -> io::Result<()> {
    let bytes = code::unbase64(data).ok_or_else(|| io::Error::other(NOT_BASE64))?;
    OpenOptions::new().create(true).append(true).open(path)?.write_all(&bytes)
}

fn sum(path: &str, reply: &mut Reply) -> io::Result<()> {
    let mut sum = Sum::new();
    read_whole(path, |part| sum.add(part))?;
    reply.data(&sum.hex());
    Ok(())
}

const CREW_TAG: &str = "bench";

// Where the calling thread is: its core, and the clocks of the large cores' cluster and of the small ones'.
fn place() -> Place {
    let core = usize::try_from(unsafe { libc::sched_getcpu() }).ok();
    Place { core, clocks: sys::CLOCKS.map(|[clock, _]| sys::read_number(clock).ok()) }
}

// Codes a scene's pictures as a call's pictures are coded, on the large cores, each step's work on the crew, each step timed; a first coding untimed has the crew's threads started and waiting, as a call's stand before its first picture.
fn code_on_crew(bench: &mut Bench, scene: Scene) -> (Timing, Laps) {
    let crew = Crew::on_cores(CREW_TAG, LARGE_CORES);
    bench.code_on(scene, |_, work| crew.run(work));
    let mut laps = Laps::default();
    let start = sys::monotonic();
    bench.code_on(scene, |step, work| {
        let begun = sys::monotonic();
        crew.run(work);
        laps.add(step, sys::monotonic() - begun);
    });
    (Timing { time: sys::monotonic() - start, place: place() }, laps)
}

// Times a parked thread woken, as a wave's row that waited is: the calling thread, kept on the first large core, and a thread of the large cores' crew take turns, each parking until the other has taken its turn and wakes it, `bench::WAKES` turns each.
fn wake_on_crew() -> Timing {
    let crew = Crew::on_cores(CREW_TAG, LARGE_CORES);
    let (turn, threads) = (AtomicUsize::new(0), [OnceLock::new(), OnceLock::new()]);
    let take_turns = |side: usize| {
        let _ = threads[side].set(thread::current());
        for _ in 0..bench::WAKES {
            while turn.load(Ordering::Acquire) % threads.len() != side {
                thread::park();
            }
            turn.fetch_add(1, Ordering::Release);
            if let Some(other) = threads[1 - side].get() {
                other.unpark();
            }
        }
    };
    let start = sys::monotonic();
    crew.for_each(&mut [0, 1], |_, side| take_turns(*side));
    Timing { time: sys::monotonic() - start, place: place() }
}

// Makes the bench's pictures of a conversion of raw frames as the camera makes them, in parts on a crew of every core beside a thread kept on the first.
fn make_on_crew(bench: &mut Bench, conversion: Conversion) -> Timing {
    let crew = Crew::on_cores(CREW_TAG, &CORES);
    let start = sys::monotonic();
    bench.make_on(conversion, |parts| crew.work(parts));
    Timing { time: sys::monotonic() - start, place: place() }
}

// Times each case of the bench by the monotonic clock on the shell's thread, then the coding and the making of pictures on crews, each on a thread of its own, which ends with it.
fn bench(reply: &mut Reply) -> io::Result<()> {
    let mut bench = Bench::default();
    let timings: Vec<Timing> = bench::cases()
        .map(|case| {
            let start = sys::monotonic();
            bench.run(case);
            Timing { time: sys::monotonic() - start, place: place() }
        })
        .collect();
    // The verb's threads and crews of their own, beside the console's rule: the bench's own lines, a coding on the shell's thread against the same coding on the large cores' crew, show that work times as the device's own only on the cores a call or the camera runs it on; each thread ends with its work.
    let crewed: Vec<(Timing, Laps)> = bench::SCENES
        .iter()
        .map(|&scene| {
            thread::scope(|scope| {
                let coded = scope.spawn(|| code_on_crew(&mut bench, scene)).join();
                coded.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
        })
        .collect();
    let parted: Vec<Timing> = bench::CONVERSIONS
        .iter()
        .map(|&conversion| {
            thread::scope(|scope| {
                let made = scope.spawn(|| make_on_crew(&mut bench, conversion)).join();
                made.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
        })
        .collect();
    let woken = thread::scope(|scope| {
        scope.spawn(wake_on_crew).join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    });
    bench.to_lines(&timings, &crewed, &parted, woken).iter().for_each(|line| reply.data(line));
    Ok(())
}

fn run(command: Command, data: &Data, is_unlocked: bool, verdict: Result<(), &'static str>) -> Reply {
    let mut output = Reply::default();
    let reply = &mut output;
    let allowed = verdict.map_err(|refusal| io::Error::new(io::ErrorKind::PermissionDenied, refusal));
    let result = allowed.and_then(|()| match command {
        Command::Ls(path) => list(&path, is_unlocked, reply),
        Command::Cat(path) => cat(&path, reply),
        Command::Write { path, text } => fs::write(path, text),
        Command::Remove(path) => fs::remove_file(path),
        Command::MakeDirectory(path) => fs::create_dir_all(path),
        Command::RemoveDirectory(path) => fs::remove_dir(path),
        Command::Read { path, offset, length } => read_part(&path, offset, length, reply),
        Command::Append { path, data } => append(&path, &data),
        Command::Sum(path) => sum(&path, reply),
        Command::Stage => data.stage(),
        Command::Commit => data.commit(),
        Command::Dmesg => dmesg(reply),
        Command::Bench => bench(reply),
        _ => Err(io::Error::other(NOT_A_VERB)),
    });
    match result {
        Ok(()) => reply.ok(),
        Err(e) => reply.err(&e.to_string()),
    };
    output
}

fn unlock(board: &Board<State>, data: &Data, passphrase: &Passphrase) -> Reply {
    let mut reply = Reply::default();
    let begun = board.lock().gate.begin();
    let opened = begun.and_then(|()| {
        let told = data.check_passphrase(passphrase);
        board.lock().gate.end(told)
    });
    match opened {
        Ok(()) => reply.ok(),
        Err(reason) => reply.err(reason),
    };
    reply
}

fn shut(board: &Board<State>) -> Reply {
    let mut reply = Reply::default();
    board.lock().gate.lock();
    reply.ok();
    reply
}

fn serve(board: &Board<State>, data: &Data, command: Command) -> Reply {
    match command {
        Command::Unlock(passphrase) => unlock(board, data, &passphrase),
        Command::Lock => shut(board),
        command => {
            let is_unlocked = board.lock().gate.is_open();
            let is_denied = command.path().is_some_and(|path| is_path_denied(Path::new(path), is_unlocked));
            let verdict = board.lock().check(&command, is_denied);
            run(command, data, is_unlocked, verdict)
        }
    }
}

pub(super) struct Shell {
    board: Arc<Board<State>>,
}

impl Shell {
    pub fn start(data: Data) -> io::Result<Shell> {
        let board = Arc::new(Board::new(State::default())?);
        let worker = Arc::clone(&board);
        thread::Builder::new().name("shell".into()).spawn(move || {
            while let Step::Serve(command) = worker.wait(|state| state.orders.next_step()) {
                let reply = serve(&worker, &data, command);
                worker.lock().report.push(reply);
                worker.raise();
            }
        })?;
        Ok(Shell { board })
    }

    // A host that left takes the passphrase it gave with it.
    pub(super) fn lock(&self) {
        self.board.lock().gate.lock();
    }

    pub(super) fn fd(&self) -> RawFd {
        self.board.fd()
    }

    // The config a `tunnel` verb hands over, as the gate and the router take it.
    pub(super) fn read_tunnel(&self, command: &Command) -> Result<Config, &'static str> {
        self.board.lock().read_tunnel(command)
    }

    pub(super) fn hand(&self, command: Command) {
        self.board.lock().orders.add(command);
        self.board.notify();
    }

    pub(super) fn latest(&self) -> Vec<Reply> {
        self.board.take(|state| mem::take(&mut state.report))
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.board.lock().orders.close();
        self.board.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use r1_funcore::domain::console::command::READ_MOST;
    use r1_funcore::domain::console::shell::{DENIAL, LOCKED};
    use r1_funcore::storage::{MOUNT_POINT, safe};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::sync::mpsc;
    use std::time::Duration;

    fn run_shut(command: Command) -> Reply {
        let board = Board::new(State::default()).unwrap();
        serve(&board, &Data::unusable(), command)
    }

    #[test]
    fn verbs_answered_in_order() {
        let shell = Shell::start(Data::unusable()).unwrap();
        let mut epoll = Epoll::new().unwrap();
        epoll.add(shell.fd(), 1).unwrap();
        shell.hand(Command::Ls("/proc/self".into()));
        shell.hand(Command::Cat("/nowhere/at/all".into()));
        let mut replies = Vec::new();
        while replies.len() < 2 {
            assert_eq!(epoll.wait(Some(Duration::from_secs(10))).unwrap(), vec![1], "a reply raises the shell's flag");
            replies.extend(shell.latest());
        }
        let text = |reply: &Reply| String::from_utf8_lossy(reply.bytes()).into_owned();
        let (listing, denial) = (text(&replies[0]), text(&replies[1]));
        assert!(listing.contains("> stat\r\n") && listing.ends_with("ok\r\n"), "{listing}");
        assert!(denial.starts_with("err ") && !denial.contains("ok\r\n"), "{denial}");
        assert!(shell.latest().is_empty(), "each reply is handed out once");
    }

    #[test]
    fn endless_device_read_for_one_part() {
        let text = |reply: Reply| String::from_utf8_lossy(reply.bytes()).into_owned();
        let mut zeros = Sum::new();
        zeros.add(&vec![0u8; READ_MOST as usize]);
        assert_eq!(
            text(run_shut(Command::Sum("/dev/zero".into()))),
            format!("> {}\r\nok\r\n", zeros.hex()),
            "a device that never ends is summed over one read's part"
        );
        let told = run_shut(Command::Cat("/dev/zero".into()));
        assert!(
            told.bytes().len() < 2 * READ_MOST as usize && told.bytes().ends_with(b"ok\r\n"),
            "and told for as much, where it ended before only as memory did"
        );
    }

    #[test]
    fn reads_of_own_memory_denied() {
        let text = |reply: Reply| String::from_utf8_lossy(reply.bytes()).into_owned();
        // A thread of the process other than its first, which the kernel shows as a process of its own too.
        let ((id_sender, id_receiver), (end_sender, end_receiver)) = (mpsc::channel(), mpsc::channel::<()>());
        let worker = thread::spawn(move || {
            id_sender.send(unsafe { libc::gettid() }).unwrap();
            let _ = end_receiver.recv();
        });
        let thread_memory = format!("/proc/{}/mem", id_receiver.recv().unwrap());
        for path in ["/proc/self/mem", "/proc/thread-self/mem", "/proc/self/task/../mem", thread_memory.as_str()] {
            let answer = text(run_shut(Command::Read { path: path.into(), offset: 0, length: 1 }));
            assert!(answer.starts_with("err ") && answer.contains(DENIAL), "{path}: {answer}");
            assert!(text(run_shut(Command::Cat(path.into()))).contains(DENIAL), "{path}");
        }
        drop(end_sender);
        worker.join().unwrap();
        let listing = text(run_shut(Command::Ls("/proc/self".into())));
        assert!(
            listing.ends_with("ok\r\n") && !listing.contains("> mem\r\n"),
            "the process is listed without its memory"
        );
    }

    struct TempDirectory(PathBuf);

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn writes_to_denied_paths_refused() {
        let text = |reply: Reply| String::from_utf8_lossy(reply.bytes()).into_owned();
        let directory = TempDirectory(std::env::temp_dir().join(format!("r1-shell-{}", std::process::id())));
        fs::create_dir_all(&directory.0).unwrap();
        let (link, chain) = (directory.0.join("dangling"), directory.0.join("chain"));
        std::os::unix::fs::symlink("/data/.r1metal/signal/new", &link).unwrap();
        std::os::unix::fs::symlink("dangling", &chain).unwrap();
        let missing = "/r1-not-there/../data/.r1metal/signal";
        let writes = [
            Command::Write { path: "/data/.r1metal/signal/account".into(), text: "x".into() },
            Command::Write { path: link.to_string_lossy().into(), text: "x".into() },
            Command::Write { path: chain.to_string_lossy().into(), text: "x".into() },
            Command::Write { path: "/proc/self/mem".into(), text: "x".into() },
            Command::Write { path: format!("{MOUNT_POINT}/{}", safe::KEY_FILES[0]), text: "x".into() },
            Command::Append { path: format!("{missing}/sessions/new"), data: "eA==".into() },
            Command::MakeDirectory(format!("{missing}/new")),
            Command::MakeDirectory(format!("{}/not-yet/../made", directory.0.display())),
            Command::Remove(format!("{missing}/contacts")),
            Command::RemoveDirectory(missing.into()),
        ];
        for command in writes {
            let answer = text(run_shut(command.clone()));
            assert!(answer.starts_with("err ") && answer.contains(DENIAL), "{command:?}: {answer}");
        }
        assert!(!directory.0.join("made").exists(), "nothing is made where a path climbs where nothing is yet");
        let note = directory.0.join("note");
        let answer = text(run_shut(Command::Write { path: note.to_string_lossy().into(), text: "kept".into() }));
        assert_eq!((answer.as_str(), fs::read(&note).unwrap()), ("ok\r\n", b"kept".to_vec()), "any other file is");
    }

    #[test]
    fn gate_keeps_staging() {
        let text = |reply: Reply| String::from_utf8_lossy(reply.bytes()).into_owned();
        let directory = TempDirectory(std::env::temp_dir().join(format!("r1-shell-gate-{}", std::process::id())));
        fs::create_dir_all(&directory.0).unwrap();
        let (board, data) = (Board::new(State::default()).unwrap(), Data::unusable());
        for kept in [Command::Stage, Command::Commit] {
            let answer = text(serve(&board, &data, kept.clone()));
            assert_eq!(answer, format!("err {LOCKED}\r\n"), "{kept:?}: kept while the gate is shut");
        }
        for staged in ["/data/.restore.new/camera/a.jpg", "/data/.restore/.r1metal/signal/account"] {
            let read = Command::Read { path: staged.into(), offset: 0, length: 1 };
            assert!(text(serve(&board, &data, read)).contains(DENIAL), "{staged}: a restore's files behind the gate");
        }
        let answer = text(serve(&board, &data, Command::Unlock("opening".into())));
        assert_eq!(answer, format!("err {}\r\n", shell::UNCHECKED), "a partition that is not mounted checks none");
        assert!(text(serve(&board, &data, Command::Stage)).contains(LOCKED), "and the gate stays shut");
        let opened = {
            let mut state = board.lock();
            state.gate.begin().and_then(|()| state.gate.end(Ok(())))
        };
        assert_eq!(opened, Ok(()), "as the safe's own passphrase opens it");
        let answer = text(serve(&board, &data, Command::Stage));
        assert!(
            answer.starts_with("err ") && !answer.contains(LOCKED),
            "with the gate open the verb reaches the partition, here one that is not mounted: {answer}"
        );
        assert_eq!(text(serve(&board, &data, Command::Lock)), "ok\r\n");
        assert!(text(serve(&board, &data, Command::Commit)).contains(LOCKED), "shut again at the host's word");
        let note = directory.0.join("note");
        fs::write(&note, b"abc").unwrap();
        assert_eq!(
            text(serve(&board, &data, Command::Sum(note.to_string_lossy().into()))),
            "> ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\r\nok\r\n",
            "a file's SHA-256, FIPS 180's"
        );
        assert!(text(serve(&board, &data, Command::Sum("/proc/self/mem".into()))).contains(DENIAL));
    }

    #[test]
    fn reads_of_memory_files_denied() {
        let text = |reply: Reply| String::from_utf8_lossy(reply.bytes()).into_owned();
        let fd = unsafe { OwnedFd::from_raw_fd(libc::memfd_create(c"r1-shell".as_ptr(), 0)) };
        let mut file = File::from(fd);
        file.write_all(b"said").unwrap();
        for path in
            [format!("/proc/self/fd/{}", file.as_raw_fd()), format!("/proc/thread-self/fd/{}", file.as_raw_fd())]
        {
            let answer = text(run_shut(Command::Read { path: path.clone(), offset: 0, length: 4 }));
            assert!(answer.starts_with("err ") && answer.contains(DENIAL), "{path}: {answer}");
            assert!(text(run_shut(Command::Cat(path.clone()))).contains(DENIAL), "{path}");
        }
        let listing = text(run_shut(Command::Ls("/proc/self/fd".into())));
        assert!(
            listing.ends_with("ok\r\n") && !listing.contains(&format!("> {}@\r\n", file.as_raw_fd())),
            "the process's files are listed without it: {listing}"
        );
    }
}

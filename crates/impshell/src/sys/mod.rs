pub mod devnodes;
mod epoll;
pub mod iface;
pub mod klog;
mod memory;
mod sched;
mod time;
pub mod uevent;

pub use epoll::{Epoll, Wake, poll, readable};
pub use memory::{Mapping, Registers, map};
pub use sched::{
    pin_to_cores, reset_scheduling, set_background_priority, set_clock_floor, set_drawing_priority, set_sound_priority,
    set_worker_priority, spawn_background,
};
pub use time::{
    boottime_ms, boottime_ns, boottime_seconds, cpu_ns, monotonic, monotonic_ns, realtime_ms, realtime_seconds,
    set_realtime,
};

use crate::log;
use r1_funcore::base::text::{parse_kernel_number, to_kernel_text, to_no_number_text};
use r1_funcore::domain::device::Sources;
use r1_funcore::domain::flash::{self, Flash};
use r1_funcore::hw::power::Power;
use r1_funcore::hw::{LARGE_CORES, SMALL_CORES};
use std::cell::UnsafeCell;
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard, TryLockError};

/// Bytes in a memory page.
pub const PAGE: usize = 4096;
pub const IOC_READ: u32 = 2;
pub const IOC_WRITE: u32 = 1;
pub const IOC_BOTH: u32 = IOC_READ | IOC_WRITE;
pub const CMDLINE: &str = "/proc/cmdline";
pub const OS_RELEASE: &str = "/proc/sys/kernel/osrelease";
pub const KERNEL_VERSION: &str = "/proc/version";
pub const MEMINFO: &str = "/proc/meminfo";
pub const CRASH_STORE: &str = "/sys/fs/pstore";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";
const TCP_SETTINGS: &str = "/proc/sys/net/ipv4";
const CPUINFO: &str = "/proc/cpuinfo";
const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";
const MAX_FREQ_FILE: &str = "cpuinfo_max_freq";
/// The large cores' cluster's and the small ones' clock in kHz and the most the kernel's policy lets it have, each policy named for its cluster's first core (`hw::LARGE_CORES`, `hw::SMALL_CORES`).
pub const CLOCKS: [[&str; 2]; 2] = [
    [
        "/sys/devices/system/cpu/cpufreq/policy0/scaling_cur_freq",
        "/sys/devices/system/cpu/cpufreq/policy0/scaling_max_freq",
    ],
    [
        "/sys/devices/system/cpu/cpufreq/policy4/scaling_cur_freq",
        "/sys/devices/system/cpu/cpufreq/policy4/scaling_max_freq",
    ],
];
const POLICY: &str = "/policy";
// Each clock file lies in the policy of its cluster's first core, a core of one digit.
const _: () = {
    let (firsts, prefixes) = ([LARGE_CORES[0], SMALL_CORES[0]], [CPUFREQ.as_bytes(), POLICY.as_bytes()]);
    let mut cluster = 0;
    while cluster < CLOCKS.len() {
        let mut file = 0;
        while file < CLOCKS[cluster].len() {
            let (path, mut at, mut part) = (CLOCKS[cluster][file].as_bytes(), 0, 0);
            while part < prefixes.len() {
                let mut place = 0;
                while place < prefixes[part].len() {
                    assert!(path[at] == prefixes[part][place], "a clock file outside the kernel's policies");
                    (at, place) = (at + 1, place + 1);
                }
                part += 1;
            }
            let first = firsts[cluster];
            assert!(first < 10 && path[at] == b'0' + first as u8 && path[at + 1] == b'/', "a policy of another core");
            file += 1;
        }
        cluster += 1;
    }
};
const NODE_MODE: libc::mode_t = 0o600;

/// An ioctl request number, as the kernel's `_IOC` macro builds it.
pub const fn ioc(direction: u32, kind: u8, number: u8, size: usize) -> u32 {
    direction << 30 | (size as u32) << 16 | (kind as u32) << 8 | number as u32
}

/// The result of a call, or its OS error.
pub fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result == -1 { Err(io::Error::last_os_error()) } else { Ok(result) }
}

/// The byte count of a call, or its OS error.
pub fn check_count(result: isize) -> io::Result<usize> {
    usize::try_from(result).map_err(|_| io::Error::last_os_error())
}

/// An `InvalidData` error with the given text.
pub fn invalid(text: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, text.into())
}

/// Takes a lock, poisoned or not.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The lock if free, poisoned or not; never waits.
pub fn try_lock<T>(mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

/// A lock whose holder runs at the priority of the highest thread waiting for it (POSIX's `PTHREAD_PRIO_INHERIT`, over the kernel's priority-inheriting futex), so that a real-time waiter waits on no lower holder another thread took off its core; a panicked holder is ignored.
pub struct PiMutex<T> {
    // Boxed: the kernel knows a priority-inheriting futex by its address.
    mutex: Box<UnsafeCell<libc::pthread_mutex_t>>,
    value: UnsafeCell<T>,
}

// The value passes between threads under the lock alone.
unsafe impl<T: Send> Send for PiMutex<T> {}
unsafe impl<T: Send> Sync for PiMutex<T> {}

impl<T> PiMutex<T> {
    pub fn new(value: T) -> io::Result<PiMutex<T>> {
        let check = |result| if result == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(result)) };
        let mut attributes = unsafe { std::mem::zeroed::<libc::pthread_mutexattr_t>() };
        check(unsafe { libc::pthread_mutexattr_init(&mut attributes) })?;
        let mutex = Box::new(UnsafeCell::new(unsafe { std::mem::zeroed::<libc::pthread_mutex_t>() }));
        let made = check(unsafe { libc::pthread_mutexattr_setprotocol(&mut attributes, libc::PTHREAD_PRIO_INHERIT) })
            .and_then(|()| check(unsafe { libc::pthread_mutex_init(mutex.get(), &attributes) }));
        unsafe { libc::pthread_mutexattr_destroy(&mut attributes) };
        made.map(|()| PiMutex { mutex, value: UnsafeCell::new(value) })
    }

    /// The value under the lock, waiting for it.
    pub fn lock(&self) -> PiGuard<'_, T> {
        // musl returns from a lock of a mutex of the normal kind only once it holds it (pthread_mutex_timedlock.c).
        unsafe { libc::pthread_mutex_lock(self.mutex.get()) };
        PiGuard { owner: self }
    }
}

impl<T> Drop for PiMutex<T> {
    fn drop(&mut self) {
        unsafe { libc::pthread_mutex_destroy(self.mutex.get()) };
    }
}

/// The value of a `PiMutex` while it is held; dropping it lets the lock go.
pub struct PiGuard<'a, T> {
    owner: &'a PiMutex<T>,
}

impl<T> Deref for PiGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.owner.value.get() }
    }
}

impl<T> DerefMut for PiGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.owner.value.get() }
    }
}

impl<T> Drop for PiGuard<'_, T> {
    fn drop(&mut self) {
        unsafe { libc::pthread_mutex_unlock(self.owner.mutex.get()) };
    }
}

/// The number in a kernel text file.
pub fn read_number<T: FromStr>(path: impl AsRef<Path>) -> io::Result<T> {
    let (path, text) = (path.as_ref(), fs::read_to_string(path.as_ref())?);
    parse_kernel_number(&text).ok_or_else(|| invalid(to_no_number_text(&path.display().to_string(), &text)))
}

/// The text of a kernel file without surrounding whitespace.
pub fn read_kernel_text(path: &str) -> io::Result<String> {
    fs::read_to_string(path).map(|text| to_kernel_text(&text))
}

/// The kernel's id of this boot.
pub fn boot_id() -> io::Result<String> {
    read_kernel_text(BOOT_ID)
}

/// The boot image's flash choices, from its root file; none for an unstamped image.
pub fn read_flash() -> Option<Flash> {
    fs::read_to_string(Path::new("/").join(flash::FILE)).ok().and_then(|text| Flash::parse(&text))
}

fn read_cluster_khz() -> Vec<u32> {
    let policies = fs::read_dir(CPUFREQ).into_iter().flatten().flatten();
    let texts = policies.filter_map(|policy| fs::read_to_string(policy.path().join(MAX_FREQ_FILE)).ok());
    texts.filter_map(|text| parse_kernel_number(&text)).collect()
}

/// The text of a kernel file; a failure goes to the log.
pub fn read_text(path: &str, label: &str) -> Option<String> {
    fs::read_to_string(path).inspect_err(|e| log!("about: {label}: {e}")).ok()
}

/// The kernel's, CPU's, memory's and partition's facts; each failure goes to the log.
pub fn read_sources(read_storage: &impl Fn() -> io::Result<(u64, u64)>) -> Sources {
    Sources {
        version: read_text(KERNEL_VERSION, "the kernel's version"),
        cpuinfo: read_text(CPUINFO, "the CPU"),
        clusters_khz: read_cluster_khz(),
        meminfo: read_text(MEMINFO, "the memory"),
        storage_bytes: read_storage().inspect_err(|e| log!("about: the room on the data partition: {e}")).ok(),
        ..Sources::default()
    }
}

// Each write of a kernel setting under `directory` that fails is logged as `protocol`'s under the log tag of the part it serves, and the rest are written regardless.
fn write_settings(directory: &Path, protocol: &str, tag: &str, writes: &[(&str, &str)]) {
    for (setting, value) in writes {
        let path = directory.join(setting);
        if let Err(e) = fs::write(&path, value) {
            log!("{tag}: {protocol} setting {setting}: {}: {e}", path.display());
        }
    }
}

/// Writes the kernel's TCP settings in order (`net::tcp`).
pub fn write_tcp(tag: &str, writes: &[(&str, &str)]) {
    write_settings(Path::new(TCP_SETTINGS), "TCP", tag, writes);
}

/// Opens a socket, closed on exec.
pub fn socket(domain: libc::c_int, kind: libc::c_int, protocol: libc::c_int) -> io::Result<OwnedFd> {
    let fd = check(unsafe { libc::socket(domain, kind | libc::SOCK_CLOEXEC, protocol) })?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Sets a socket option to `value`, read by the kernel as the option's type.
pub fn set_option<T>(socket: &OwnedFd, level: libc::c_int, option: libc::c_int, value: &T) -> io::Result<()> {
    let length = size_of::<T>() as libc::socklen_t;
    check(unsafe { libc::setsockopt(socket.as_raw_fd(), level, option, std::ptr::from_ref(value).cast(), length) })?;
    Ok(())
}

// An address and its length, as the kernel's socket calls take them.
fn socket_address(address: &SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let length = match address {
        SocketAddr::V4(v4) => {
            let inet = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes(v4.ip().octets()) },
                sin_zero: [0; 8],
            };
            unsafe { (&raw mut storage).cast::<libc::sockaddr_in>().write(inet) };
            size_of::<libc::sockaddr_in>()
        }
        SocketAddr::V6(v6) => {
            let inet = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: v6.flowinfo(),
                sin6_addr: libc::in6_addr { s6_addr: v6.ip().octets() },
                sin6_scope_id: v6.scope_id(),
            };
            unsafe { (&raw mut storage).cast::<libc::sockaddr_in6>().write(inet) };
            size_of::<libc::sockaddr_in6>()
        }
    };
    (storage, length as libc::socklen_t)
}

/// Sends a datagram over an unbound socket.
pub fn send_to(socket: &OwnedFd, address: &SocketAddr, datagram: &[u8]) -> io::Result<()> {
    let (peer, length) = socket_address(address);
    let count = unsafe {
        libc::sendto(socket.as_raw_fd(), datagram.as_ptr().cast(), datagram.len(), 0, (&raw const peer).cast(), length)
    };
    check_count(count).map(drop)
}

/// Begins a connection to `address` on a socket that does not block: it is writable once the connection has come or failed.
pub fn begin_connect(address: &SocketAddr) -> io::Result<OwnedFd> {
    let family = if address.is_ipv4() { libc::AF_INET } else { libc::AF_INET6 };
    let socket = socket(family, libc::SOCK_STREAM | libc::SOCK_NONBLOCK, 0)?;
    let (target, length) = socket_address(address);
    match unsafe { libc::connect(socket.as_raw_fd(), (&raw const target).cast(), length) } {
        0 => Ok(socket),
        _ if io::Error::last_os_error().raw_os_error() == Some(libc::EINPROGRESS) => Ok(socket),
        _ => Err(io::Error::last_os_error()),
    }
}

/// How the connection a socket began has ended.
pub fn connect_result(socket: RawFd) -> io::Result<()> {
    let (mut error, mut size): (libc::c_int, _) = (0, size_of::<libc::c_int>() as libc::socklen_t);
    check(unsafe { libc::getsockopt(socket, libc::SOL_SOCKET, libc::SO_ERROR, (&raw mut error).cast(), &mut size) })?;
    if error == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(error)) }
}

/// A path as a C string for the kernel's calls.
pub fn c_path(path: &Path) -> io::Result<CString> {
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

pub fn mount(source: &str, target: &Path, fstype: &str, options: &str) -> io::Result<()> {
    let (source, target, fstype, options) =
        (CString::new(source)?, c_path(target)?, CString::new(fstype)?, CString::new(options)?);
    check(unsafe { libc::mount(source.as_ptr(), target.as_ptr(), fstype.as_ptr(), 0, options.as_ptr().cast()) })?;
    Ok(())
}

/// Detaches a mount at once; files open on it stay usable until closed.
pub fn detach(target: &Path) -> io::Result<()> {
    let target = c_path(target)?;
    check(unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) })?;
    Ok(())
}

fn statvfs(path: &Path) -> io::Result<libc::statvfs> {
    let path = c_path(path)?;
    let mut stat = unsafe { std::mem::zeroed::<libc::statvfs>() };
    check(unsafe { libc::statvfs(path.as_ptr(), &mut stat) })?;
    Ok(stat)
}

/// The size in bytes of the filesystem at `path`, and its block size.
pub fn capacity(path: &Path) -> io::Result<(u64, u32)> {
    let stat = statvfs(path)?;
    Ok((stat.f_blocks as u64 * stat.f_frsize as u64, stat.f_bsize as u32))
}

/// The writable bytes left on the filesystem at `path`.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let stat = statvfs(path)?;
    Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
}

fn mknod(path: &Path, kind: libc::mode_t, major: u32, minor: u32) -> io::Result<()> {
    let path = c_path(path)?;
    check(unsafe { libc::mknod(path.as_ptr(), kind | NODE_MODE, libc::makedev(major, minor)) })?;
    Ok(())
}

/// Safety: `arg` must point to what `request` reads or writes.
pub unsafe fn ioctl<T>(fd: RawFd, request: u32, arg: *mut T) -> io::Result<()> {
    check(unsafe { libc::ioctl(fd, request as libc::Ioctl, arg) })?;
    Ok(())
}

/// Safety: `request` must take its argument as a number; returns the call's own result.
pub unsafe fn ioctl_value(fd: RawFd, request: u32, value: libc::c_ulong) -> io::Result<libc::c_int> {
    check(unsafe { libc::ioctl(fd, request as libc::Ioctl, value) })
}

/// Moves `fd` above `floor`; a dying process closes descriptors lowest first.
pub fn raise_above(fd: OwnedFd, floor: RawFd) -> io::Result<OwnedFd> {
    if fd.as_raw_fd() > floor {
        return Ok(fd);
    }
    let raised = check(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, floor + 1) })?;
    Ok(unsafe { OwnedFd::from_raw_fd(raised) })
}

/// Reads from `offset` until `buffer` is full or the file ends; returns the byte count.
pub fn read_up_to(file: &File, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buffer.len() {
        match file.read_at(&mut buffer[total..], offset + total as u64) {
            Ok(0) => break,
            Ok(count) => total += count,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// An in-memory file, named for the kernel's lists alone; closed on exec.
pub fn memory_file(name: &CStr) -> io::Result<File> {
    let fd = check(unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) })?;
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Opens a device node for reading and writing, with a descriptor above `floor`.
pub fn open_above(path: &str, floor: RawFd) -> io::Result<File> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    Ok(File::from(raise_above(file.into(), floor)?))
}

/// Grows the descriptor table to the limit; growing a shared table waits out an RCU grace period.
pub fn grow_descriptor_table() -> io::Result<()> {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    check(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) })?;
    let max_count =
        RawFd::try_from(limit.rlim_cur).map_err(|_| invalid(format!("a limit of {} descriptors", limit.rlim_cur)))?;
    let descriptor = unsafe { OwnedFd::from_raw_fd(check(libc::eventfd(0, libc::EFD_CLOEXEC))?) };
    let last_fd = check(unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_DUPFD_CLOEXEC, max_count - 1) })?;
    drop(unsafe { OwnedFd::from_raw_fd(last_fd) });
    Ok(())
}

pub fn set_raw_mode(fd: RawFd) -> io::Result<()> {
    let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
    check(unsafe { libc::tcgetattr(fd, &mut termios) })?;
    unsafe { libc::cfmakeraw(&mut termios) };
    check(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) })?;
    Ok(())
}

/// Reboots or powers off; returns only if the kernel refused.
pub fn reboot(power: Power) -> io::Error {
    unsafe { libc::sync() };
    let result = match power {
        Power::Reboot => unsafe { libc::reboot(libc::LINUX_REBOOT_CMD_RESTART) },
        Power::PowerOff => unsafe { libc::reboot(libc::LINUX_REBOOT_CMD_POWER_OFF) },
        Power::Bootloader => unsafe {
            libc::syscall(
                libc::SYS_reboot,
                libc::LINUX_REBOOT_MAGIC1,
                libc::LINUX_REBOOT_MAGIC2,
                libc::LINUX_REBOOT_CMD_RESTART2,
                c"bootloader".as_ptr(),
            ) as libc::c_int
        },
    };
    check(result).err().unwrap_or_else(|| io::Error::other("reboot returned"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_kernel_headers() {
        assert_eq!(ioc(IOC_READ, b'E', 0x06, 256), 0x8100_4506);
        assert_eq!(ioc(0, b'F', 0x06, 0), 0x4606);
    }

    #[test]
    fn read_number_and_path_in_error() {
        let path = std::env::temp_dir().join(format!("r1-number-{}", std::process::id()));
        std::fs::write(&path, "4711\n").unwrap();
        assert_eq!(read_number::<u32>(&path).unwrap(), 4711);
        std::fs::write(&path, "high\n").unwrap();
        let error = read_number::<u32>(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().starts_with(&format!("{}: ", path.display())), "{error}");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn raise_above_floor() {
        let low = unsafe { OwnedFd::from_raw_fd(check(libc::eventfd(0, libc::EFD_CLOEXEC)).unwrap()) };
        let number = low.as_raw_fd();
        let same = raise_above(low, number - 1).unwrap();
        assert_eq!(same.as_raw_fd(), number);
        let high = raise_above(same, number + 40).unwrap();
        assert!(high.as_raw_fd() > number + 40);
        assert_ne!(unsafe { libc::fcntl(high.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn grow_descriptor_table_to_limit() {
        grow_descriptor_table().unwrap();
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        check(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) }).unwrap();
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let size: u64 = status.lines().find_map(|line| line.strip_prefix("FDSize:")).unwrap().trim().parse().unwrap();
        assert!(size >= limit.rlim_cur, "a table of {size} for a limit of {}", limit.rlim_cur);
    }

    #[test]
    fn read_up_to_buffer_or_end() {
        let file = temp_file(b"0123456789");
        let mut buffer = [0u8; 6];
        assert_eq!((read_up_to(&file, 2, &mut buffer).unwrap(), &buffer), (6, b"234567"));
        assert_eq!(
            (read_up_to(&file, 8, &mut buffer).unwrap(), &buffer[..2]),
            (2, &b"89"[..]),
            "the rest is left as it was"
        );
        assert_eq!(read_up_to(&file, 20, &mut buffer).unwrap(), 0);
    }

    pub(super) fn temp_file(bytes: &[u8]) -> File {
        use std::io::Write;
        let mut file = memory_file(c"mapped").unwrap();
        file.write_all(bytes).unwrap();
        file
    }

    // A field of a thread's line in the kernel's `stat`, as proc(5) numbers them past the thread's name: 3, its state; 18, its priority.
    fn stat_field(thread: libc::pid_t, field: usize) -> String {
        let line = std::fs::read_to_string(format!("/proc/self/task/{thread}/stat")).unwrap();
        let fields = &line[line.rfind(')').unwrap() + 2..];
        fields.split(' ').nth(field - 3).unwrap().to_string()
    }

    #[test]
    fn pi_mutex_lends_priority() {
        let (mutex, (go, gone), (told, telling)) =
            (std::sync::Arc::new(PiMutex::new(()).unwrap()), std::sync::mpsc::channel(), std::sync::mpsc::channel());
        let held = std::sync::Arc::clone(&mutex);
        let holder = std::thread::spawn(move || {
            let thread = unsafe { libc::gettid() };
            set_background_priority();
            let guard = held.lock();
            let alone = stat_field(thread, 18);
            go.send(()).unwrap();
            let waiter = telling.recv().unwrap();
            while stat_field(waiter, 3) != "S" {
                std::thread::yield_now();
            }
            let lent = stat_field(thread, 18);
            drop(guard);
            (alone, lent, stat_field(thread, 18))
        });
        let waiter = std::thread::spawn(move || {
            let thread = unsafe { libc::gettid() };
            gone.recv().unwrap();
            let own = stat_field(thread, 18);
            told.send(thread).unwrap();
            drop(mutex.lock());
            own
        });
        let ((alone, lent, after), own) = (holder.join().unwrap(), waiter.join().unwrap());
        assert_eq!(lent, own, "a holder runs at the priority of the higher thread asleep on the lock");
        assert!(alone != own && after == alone, "and at its own priority, {alone}, alone and once it lets go: {after}");
    }

    #[test]
    fn capacity_and_free_bytes() {
        let root = std::env::temp_dir();
        let (bytes, block) = capacity(&root).unwrap();
        assert!(bytes > 0 && block > 0);
        assert!(free_bytes(&root).unwrap() <= bytes);
        assert!(free_bytes(Path::new("/nowhere/at/all")).is_err());
    }
}

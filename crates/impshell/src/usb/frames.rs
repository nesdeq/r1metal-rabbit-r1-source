use crate::sys;
use r1_funcore::domain::recording::{
    TRANSFER_BYTES, USB_CLASS, USB_INTERFACE_NAME, USB_PROTOCOL, USB_SUBCLASS, to_gone_text, to_reap_limit,
};
use r1_funcore::hw::usb::{EVENT_BYTES, MAX_EVENTS, READIED_TWICE, Stall, descriptors, stall, strings};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const FUNCTION: &str = "ffs.frames";
const INSTANCE: &str = "frames";
const FILESYSTEM: &str = "functionfs";
const MOUNT: &str = "/dev/usb-ffs/frames";
const CONTROL: &str = "ep0";
const STREAM: &str = "ep1";

static CONTROL_FILE: OnceLock<File> = OnceLock::new();

pub(super) fn function_path() -> PathBuf {
    super::function_path(FUNCTION)
}

/// Makes the function, mounts its files and describes it to the kernel; its control file stays open, since closing it takes the gadget off the bus.
pub fn prepare() -> io::Result<()> {
    fs::create_dir_all(function_path())?;
    let mount = Path::new(MOUNT);
    fs::create_dir_all(mount)?;
    sys::mount(INSTANCE, mount, FILESYSTEM, "")?;
    let mut control =
        OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(mount.join(CONTROL))?;
    control.write_all(&descriptors(USB_CLASS, USB_SUBCLASS, USB_PROTOCOL))?;
    control.write_all(&strings(USB_INTERFACE_NAME))?;
    CONTROL_FILE.set(control).map_err(|_| io::Error::other(READIED_TWICE))
}

/// True once `prepare` has run.
pub fn is_ready() -> bool {
    CONTROL_FILE.get().is_some()
}

/// The control file, readable while the kernel has events; none before `prepare`.
pub fn control() -> Option<RawFd> {
    CONTROL_FILE.get().map(AsRawFd::as_raw_fd)
}

/// Takes the kernel's events and stalls every request of the host.
pub fn serve() {
    let Some(mut control) = CONTROL_FILE.get() else {
        return;
    };
    let mut events = [0u8; EVENT_BYTES * MAX_EVENTS];
    loop {
        let count = match control.read(&mut events) {
            Ok(count) => count,
            // A request the host gave up on, or a signal, ends no reading.
            Err(e) if e.kind() == io::ErrorKind::Interrupted || e.raw_os_error() == Some(libc::EIDRM) => continue,
            Err(_) => return,
        };
        if count == 0 {
            return;
        }
        for event in events[..count].as_chunks::<EVENT_BYTES>().0 {
            let mut byte = [0u8; 1];
            let _ = match stall(event) {
                Some(Stall::Read) => control.read(&mut byte),
                Some(Stall::Write) => control.write(&byte),
                None => continue,
            };
        }
    }
}

#[repr(C)]
#[derive(Default)]
struct Iocb {
    data: u64,
    key: u32,
    rw_flags: i32,
    opcode: u16,
    reqprio: i16,
    fildes: u32,
    buf: u64,
    nbytes: u64,
    offset: i64,
    reserved2: u64,
    flags: u32,
    resfd: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IoEvent {
    data: u64,
    obj: u64,
    res: i64,
    res2: i64,
}

const WRITE: u16 = 1;

/// The endpoint toward the host, written through the kernel's asynchronous I/O, so its queue runs whatever the processors do.
pub struct Stream {
    file: File,
    context: u64,
    depth: usize,
    in_flight: usize,
    events: Vec<IoEvent>,
}

impl Stream {
    /// Opens the endpoint, which clears a disconnection's error, with room for `depth` parts in flight.
    pub fn open(depth: usize) -> io::Result<Stream> {
        let file = OpenOptions::new().write(true).open(Path::new(MOUNT).join(STREAM))?;
        let mut context = 0u64;
        sys::check(
            unsafe { libc::syscall(libc::SYS_io_setup, depth as libc::c_long, &raw mut context) } as libc::c_int
        )?;
        Ok(Stream { file, context, depth, in_flight: 0, events: vec![IoEvent::default(); depth] })
    }

    fn reap(&mut self, min_count: usize) -> io::Result<()> {
        let limit = to_reap_limit(min_count);
        let timeout = libc::timespec { tv_sec: limit.as_secs() as _, tv_nsec: limit.subsec_nanos() as _ };
        let count = loop {
            let result = unsafe {
                libc::syscall(
                    libc::SYS_io_getevents,
                    self.context,
                    min_count as libc::c_long,
                    self.depth as libc::c_long,
                    self.events.as_mut_ptr(),
                    &raw const timeout,
                )
            };
            match sys::check(result as libc::c_int) {
                Ok(count) => break count as usize,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        };
        self.in_flight -= count;
        if let Some(event) = self.events[..count].iter().find(|event| event.res < 0) {
            return Err(io::Error::from_raw_os_error(-event.res as i32));
        }
        if count < min_count {
            return Err(io::Error::new(io::ErrorKind::TimedOut, to_gone_text()));
        }
        Ok(())
    }

    /// Hands bytes to the kernel in whole transfers, waiting while the queue is full; fails if a part failed or the host takes none.
    pub fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.reap(0)?;
        for part in bytes.chunks(TRANSFER_BYTES) {
            while self.in_flight == self.depth {
                self.reap(1)?;
            }
            let mut iocb = Iocb {
                opcode: WRITE,
                fildes: self.file.as_raw_fd() as u32,
                buf: part.as_ptr() as u64,
                nbytes: part.len() as u64,
                ..Iocb::default()
            };
            let mut iocbs = [&raw mut iocb];
            let result =
                unsafe { libc::syscall(libc::SYS_io_submit, self.context, 1 as libc::c_long, iocbs.as_mut_ptr()) };
            sys::check(result as libc::c_int)?;
            self.in_flight += 1;
        }
        Ok(())
    }

    /// Waits until every part has gone to the host, or the host has taken none for `HOST_TIMEOUT`.
    pub fn drain(&mut self) -> io::Result<()> {
        while self.in_flight > 0 {
            self.reap(1)?;
        }
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe { libc::syscall(libc::SYS_io_destroy, self.context) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aio_layouts() {
        assert_eq!((size_of::<Iocb>(), size_of::<IoEvent>()), (64, 32));
        assert_eq!(std::mem::offset_of!(Iocb, opcode), 16);
        assert_eq!(std::mem::offset_of!(Iocb, fildes), 20);
        assert_eq!(std::mem::offset_of!(Iocb, buf), 24);
        assert_eq!(std::mem::offset_of!(Iocb, nbytes), 32);
        assert_eq!(std::mem::offset_of!(IoEvent, res), 16);
    }
}

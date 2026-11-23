use crate::sys;
use r1_funcore::domain::console::outbox::Outbox;
use r1_funcore::domain::console::wire::{Line, Reply, take_lines};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;

const TTY: &str = "/dev/ttyGS0";
const MAX_READ: usize = 4096;

/// The gadget's serial port; no call blocks.
pub struct Console {
    file: File,
    inbox: Vec<u8>,
    is_dropping: bool,
    outbox: Outbox,
}

impl Console {
    pub fn open() -> io::Result<Self> {
        let file =
            OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY).open(TTY)?;
        sys::set_raw_mode(file.as_raw_fd())?;
        Ok(Console { file, inbox: Vec::new(), is_dropping: false, outbox: Outbox::default() })
    }

    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Sends an unsolicited line, or drops it when the host is not keeping up.
    pub fn log(&mut self, text: &str) {
        self.flush();
        self.outbox.log(text);
    }

    /// Queues a whole answer.
    pub fn answer(&mut self, reply: &Reply) {
        self.outbox.answer(reply);
    }

    /// Writes what the port accepts.
    pub fn flush(&mut self) {
        while self.outbox.has_queued() {
            match (&self.file).write(self.outbox.next()) {
                Ok(0) => break,
                Ok(count) => self.outbox.take_sent(count),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => self.outbox.clear(),
            }
        }
    }

    /// Flushes; on a change, whether bytes stay queued, so the port is watched for room or no longer.
    pub fn backlog(&mut self) -> Option<bool> {
        self.flush();
        self.outbox.take_change()
    }

    /// The complete lines the host sent, in their order, one dropped for its length in its place; an error means the port hung up.
    pub fn read_lines(&mut self) -> io::Result<Vec<Line>> {
        let (mut chunk, mut lines) = ([0u8; MAX_READ], Vec::new());
        loop {
            match (&self.file).read(&mut chunk) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(count) => {
                    self.inbox.extend_from_slice(&chunk[..count]);
                    // Lines are taken out read by read, so that what waits never grows past a line and a read.
                    lines.extend(take_lines(&mut self.inbox, &mut self.is_dropping));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(lines),
                Err(e) => return Err(e),
            }
        }
    }
}

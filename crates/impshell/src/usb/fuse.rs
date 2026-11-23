use crate::{log, sys};
use r1_funcore::base::handoff::Latch;
use r1_funcore::fs::fuse::{self, MIN_READ_BUFFER, Source};
use std::fs::{File, OpenOptions};
use std::io::{self, IoSlice, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

const DEVICE: &str = "/dev/fuse";
const FILESYSTEM: &str = "fuse";

pub(super) fn mount(target: &Path) -> io::Result<File> {
    let connection = OpenOptions::new().read(true).write(true).open(DEVICE)?;
    sys::mount(FILESYSTEM, target, FILESYSTEM, &fuse::to_mount_options(connection.as_raw_fd()))?;
    Ok(connection)
}

pub(super) fn serve(mut connection: &File, name: &str, content: &dyn Source) -> io::Error {
    // A read's bytes go into one buffer from first to last: a host's large reads would otherwise map and unmap memory each.
    let (mut request, mut buffer) = (vec![0u8; MIN_READ_BUFFER], Vec::new());
    let mut is_answered = Latch::with(true);
    loop {
        let length = match connection.read(&mut request) {
            Ok(length) => length,
            Err(e) if matches!(e.raw_os_error(), Some(libc::EINTR | libc::EAGAIN | libc::ENOENT)) => continue,
            Err(e) => return e,
        };
        let Some((opcode, unique, node, body)) = fuse::parse_request(&request[..length]) else {
            continue;
        };
        let answer = fuse::answer(opcode, node, body, name, content, &mut buffer);
        let Some((header, data)) = fuse::reply(unique, &answer, &buffer) else {
            continue;
        };
        let written = connection.write_vectored(&[IoSlice::new(&header), IoSlice::new(data)]);
        // A reply to a request the kernel gave up meanwhile is refused with ENOENT (libfuse, `fuse_write_msg_dev`), which is no failure.
        let failure = written.err().filter(|e| e.raw_os_error() != Some(libc::ENOENT));
        if let (Some(false), Some(e)) = (is_answered.change(failure.is_none()), &failure) {
            log!("drive: answering the kernel: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_numbers() {
        assert_eq!((fuse::S_IFDIR, fuse::S_IFREG), (libc::S_IFDIR, libc::S_IFREG));
        assert_eq!((fuse::O_ACCMODE, fuse::O_RDONLY), (libc::O_ACCMODE as u32, libc::O_RDONLY as u32));
        assert_eq!((fuse::DT_DIR, fuse::DT_REG), (libc::DT_DIR, libc::DT_REG));
        assert_eq!(
            [fuse::ENOENT, fuse::EINVAL, fuse::EROFS, fuse::ENOSYS, fuse::EPROTO],
            [libc::ENOENT, libc::EINVAL, libc::EROFS, libc::ENOSYS, libc::EPROTO]
        );
    }
}

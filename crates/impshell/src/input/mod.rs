pub mod tilt;

use crate::{log, sys};
use r1_funcore::base::bytes::until_zero;
use r1_funcore::input::evdev::{self, Decoder, Held, KEY_STATE_BYTES, RECORD_SIZE};
use r1_funcore::input::{Devices, Event, to_open_order};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const DIRECTORY: &str = "/dev/input";
const MAX_NAME: usize = 256;
const EVIOCGNAME: u32 = sys::ioc(sys::IOC_READ, b'E', 0x06, MAX_NAME);
const EVIOCGKEY: u32 = sys::ioc(sys::IOC_READ, b'E', 0x18, KEY_STATE_BYTES);

fn device_name(file: &File) -> String {
    let mut buffer = [0u8; MAX_NAME];
    if let Err(e) = unsafe { sys::ioctl(file.as_raw_fd(), EVIOCGNAME, buffer.as_mut_ptr()) } {
        log!("input: asking a device's name: {e}");
        return String::new();
    }
    String::from_utf8_lossy(until_zero(&buffer)).into_owned()
}

// What every open device holds down now, asked after a queue dropped records; a device that does not tell counts as holding nothing.
fn held_keys(devices: &Devices<File>) -> Held {
    let mut held = Held::default();
    for (_, file) in devices.open_files() {
        let mut keys = [0u8; KEY_STATE_BYTES];
        match unsafe { sys::ioctl(file.as_raw_fd(), EVIOCGKEY, keys.as_mut_ptr()) } {
            Ok(()) => held.add(&keys),
            Err(e) => log!("input: asking a device's keys: {e}"),
        }
    }
    held
}

pub struct Inputs {
    devices: Devices<File>,
    decoder: Decoder,
}

impl Inputs {
    pub fn open() -> Self {
        let listing = fs::read_dir(DIRECTORY).inspect_err(|e| log!("input: listing {DIRECTORY}: {e}"));
        let entries = listing.into_iter().flatten().flatten();
        let paths = entries.map(|entry| entry.path().to_string_lossy().into_owned()).collect();
        let mut inputs = Inputs { devices: Devices::default(), decoder: Decoder::default() };
        for path in to_open_order(paths) {
            inputs.add(Path::new(&path));
        }
        inputs
    }

    /// Opens an event device, in its earlier place if it had one; returns its index and descriptor.
    pub fn add(&mut self, path: &Path) -> Option<(usize, RawFd)> {
        let text = path.to_string_lossy();
        let index = self.devices.open_place(&text)?;
        let opened = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path);
        let file = opened.inspect_err(|e| log!("input: opening {text}: {e}")).ok()?;
        let (fd, name) = (file.as_raw_fd(), device_name(&file));
        self.devices.open(index, &text, name, file);
        Some((index, fd))
    }

    /// The descriptor of an open device.
    pub fn fd(&self, index: usize) -> Option<RawFd> {
        self.devices.file(index).map(AsRawFd::as_raw_fd)
    }

    /// Closes a failed device, already unwatched.
    pub fn close(&mut self, index: usize) {
        self.devices.close(index);
    }

    pub fn name(&self, index: usize) -> &str {
        self.devices.name(index)
    }

    /// The names of the open devices.
    pub fn names(&self) -> Vec<&str> {
        self.devices.names()
    }

    /// The descriptors of the open devices, each with its index.
    pub fn fds(&self) -> Vec<(usize, RawFd)> {
        self.devices.open_files().map(|(index, file)| (index, file.as_raw_fd())).collect()
    }

    /// True, once, if a finger stayed in place since the last call, which made no event.
    pub fn take_finger_rest(&mut self) -> bool {
        self.decoder.take_rest()
    }

    /// Drains device `index` into `events`; a closed one adds none.
    pub fn read(&mut self, index: usize, events: &mut Vec<Event>) -> io::Result<()> {
        let mut record = [0u8; RECORD_SIZE];
        let Some(mut file) = self.devices.file(index) else {
            return Ok(());
        };
        loop {
            match file.read(&mut record) {
                Ok(RECORD_SIZE) => {
                    let (kind, code, value) = evdev::parse_record(&record);
                    events.extend(self.decoder.feed(kind, code, value));
                    if self.decoder.take_drop() {
                        events.extend(self.decoder.resync(&held_keys(&self.devices)).into_iter().flatten());
                    }
                }
                Ok(_) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_layout() {
        assert_eq!((EVIOCGNAME, EVIOCGKEY), (0x8100_4506, 0x8060_4518));
        assert_eq!(KEY_STATE_BYTES, (usize::from(libc::KEY_MAX) + 1) / 8, "a bit for every key code");
        assert_eq!(size_of::<libc::input_event>(), RECORD_SIZE);
        assert_eq!(std::mem::offset_of!(libc::input_event, type_), evdev::TYPE_OFFSET);
        assert_eq!(std::mem::offset_of!(libc::input_event, code), evdev::CODE_OFFSET);
        assert_eq!(std::mem::offset_of!(libc::input_event, value), evdev::VALUE_OFFSET);
    }
}

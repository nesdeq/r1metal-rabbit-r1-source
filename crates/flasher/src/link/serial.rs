//! A USB serial line as macOS gives it to a program: the terminal its own CDC ACM driver makes, which holds the device's interfaces and refuses others the requests that raise DTR.

use super::{RELIST_EVERY, Transport};
use crate::Result;
use core_foundation_sys::base::{CFGetTypeID, CFIndex, CFRelease, kCFAllocatorDefault};
use core_foundation_sys::string::{
    CFStringCreateWithCString, CFStringGetCString, CFStringGetTypeID, CFStringRef, kCFStringEncodingUTF8,
};
use io_kit_sys::keys::kIOServicePlane;
use io_kit_sys::serial::keys::kIOCalloutDeviceKey;
use io_kit_sys::{
    IOObjectRelease, IORegistryEntryIDMatching, IORegistryEntrySearchCFProperty, IOServiceGetMatchingService,
    kIORegistryIterateRecursively,
};
use nusb::DeviceInfo;
use std::ffi::{CStr, c_char};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::thread;
use std::time::{Duration, Instant};

// How long macOS is given to make the terminal of a device it has configured: it took 0.2 s for the preloader.
const MADE_WITHIN: Duration = Duration::from_secs(1);
// The default main port, as IOKit takes it.
const MAIN_PORT: u32 = 0;
// The longest path of a terminal read.
const PATH_BYTES: usize = 1024;
// The most bytes one read takes, a high speed bulk packet.
const PACKET: usize = 512;

// The terminal macOS made of a serial line of `device`, `/dev/cu.usbmodem…`: the first one under it in the registry.
fn callout(device: &DeviceInfo) -> Option<String> {
    unsafe {
        let service = IOServiceGetMatchingService(MAIN_PORT, IORegistryEntryIDMatching(device.registry_entry_id()));
        if service == 0 {
            return None;
        }
        let key = CFStringCreateWithCString(kCFAllocatorDefault, kIOCalloutDeviceKey, kCFStringEncodingUTF8);
        let found = IORegistryEntrySearchCFProperty(
            service,
            kIOServicePlane,
            key,
            kCFAllocatorDefault,
            kIORegistryIterateRecursively,
        );
        CFRelease(key.cast());
        IOObjectRelease(service);
        if found.is_null() {
            return None;
        }
        let mut path = [0 as c_char; PATH_BYTES];
        let named = CFGetTypeID(found) == CFStringGetTypeID()
            && CFStringGetCString(
                found as CFStringRef,
                path.as_mut_ptr(),
                PATH_BYTES as CFIndex,
                kCFStringEncodingUTF8,
            ) != 0;
        CFRelease(found);
        named.then(|| CStr::from_ptr(path.as_ptr()).to_string_lossy().into_owned())
    }
}

// What went wrong with a terminal, told with its path.
fn failed(path: &str, what: &str) -> Box<dyn std::error::Error> {
    format!("{what} {path}: {}", io::Error::last_os_error()).into()
}

// A device's serial line through its terminal, opened as a terminal program opens one, which raises DTR and RTS: raw, 115200 baud.
pub(super) struct Line {
    terminal: File,
}

impl Line {
    // Opens the terminal macOS makes of `device`'s serial line once it is there.
    pub(super) fn open(device: &DeviceInfo) -> Result<Line> {
        let until = Instant::now() + MADE_WITHIN;
        let path = loop {
            if let Some(path) = callout(device) {
                break path;
            }
            if Instant::now() >= until {
                return Err("macOS made no terminal of its serial line".into());
            }
            thread::sleep(RELIST_EVERY);
        };
        // Opened without waiting for a carrier, which a USB line never shows; the line then blocks, and reads wait in poll.
        let terminal = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|e| format!("cannot open {path}: {e}"))?;
        let fd = terminal.as_raw_fd();
        let mut settings = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut settings) } != 0 {
            return Err(failed(&path, "cannot read the settings of"));
        }
        unsafe {
            libc::cfmakeraw(&mut settings);
            libc::cfsetspeed(&mut settings, libc::B115200);
        }
        settings.c_cflag |= libc::CLOCAL | libc::CREAD;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &settings) } != 0 {
            return Err(failed(&path, "cannot set"));
        }
        if unsafe { libc::fcntl(fd, libc::F_SETFL, 0) } != 0 {
            return Err(failed(&path, "cannot block on"));
        }
        Ok(Line { terminal })
    }
}

impl Transport for Line {
    fn send(&mut self, data: &[u8]) -> Result<()> {
        self.terminal.write_all(data)?;
        if unsafe { libc::tcdrain(self.terminal.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(())
    }

    fn receive_within(&mut self, patience: Duration) -> Result<Vec<u8>> {
        let wait = if patience == Duration::MAX { -1 } else { patience.as_millis().min(i32::MAX as u128) as i32 };
        let mut ready = libc::pollfd { fd: self.terminal.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        match unsafe { libc::poll(&mut ready, 1, wait) } {
            0 => return Err("the serial line said nothing in time".into()),
            told if told < 0 => return Err(io::Error::last_os_error().into()),
            _ => {}
        }
        let mut packet = vec![0; PACKET];
        let read = self.terminal.read(&mut packet)?;
        if read == 0 {
            return Err("the serial line closed".into());
        }
        packet.truncate(read);
        Ok(packet)
    }
}

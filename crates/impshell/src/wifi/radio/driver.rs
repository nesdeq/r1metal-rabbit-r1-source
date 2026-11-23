use crate::log;
use crate::pool::ThreadPool;
use crate::storage::nvram;
use crate::sys::devnodes;
use crate::sys::{self, Epoll, Wake};
use crate::wifi::iface;
use r1_funcore::domain::device::NVRAM;
use r1_funcore::wifi::wmt::{self, Setup};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

const DETECT: &str = "/dev/wmtdetect";
const DRIVER: &str = "/dev/stpwmt";
const POWER: &str = "/dev/wmtWifi";
const ROOT: &str = "/";
const CALIBRATION: &str = "/sys/module/wmt_drv/parameters/restore_calibration";
const RESTORE: &str = "1";
const QUESTION: u64 = 0;
const WRITE_END: u64 = 1;
const HARDWARE_INIT: u32 = 0x8004_770A;
const ASK_CHIP: u32 = 0x8004_7703;
const NAME_CHIP: u32 = 0x4004_7701;
const CLEAN_UP: u32 = 0x8004_7705;
const START_DRIVERS: u32 = 0x8004_7704;
const SET_INTERFACE: u32 = 0x4004_A005;
const SET_ROM_PATCH: u32 = 0x4008_A01F;
const SET_PATCH_COUNT: u32 = 0x4004_A00E;
const SET_PATCH: u32 = 0x4008_A00F;
const NAME_BYTES: usize = 256;

static POWER_POOL: ThreadPool = ThreadPool::new("radio power");

#[repr(C)]
struct Record {
    number: u32,
    address: [u8; 4],
    name: [u8; NAME_BYTES],
}

impl Record {
    fn new(patch: wmt::Patch, name: &str) -> Record {
        let mut record = Record { number: patch.number, address: patch.address, name: [0; NAME_BYTES] };
        record.name[..name.len()].copy_from_slice(name.as_bytes());
        record
    }
}

fn read_head(name: &str, bytes: usize) -> io::Result<Vec<u8>> {
    let mut head = vec![0u8; bytes];
    File::open(Path::new(ROOT).join(wmt::FIRMWARE).join(name))
        .and_then(|mut file| file.read_exact(&mut head))
        .map_err(|e| io::Error::new(e.kind(), format!("{name}: {e}")))?;
    Ok(head)
}

fn start_drivers() -> io::Result<()> {
    let detect = OpenOptions::new().read(true).write(true).open(DETECT)?;
    let fd = detect.as_raw_fd();
    unsafe { sys::ioctl_value(fd, HARDWARE_INIT, 0) }?;
    let chip = unsafe { sys::ioctl_value(fd, ASK_CHIP, 0) }?;
    unsafe { sys::ioctl_value(fd, NAME_CHIP, chip as libc::c_ulong) }?;
    unsafe { sys::ioctl_value(fd, CLEAN_UP, 0) }?;
    // The start answers 0 once what is built has started (modules' patch 0014); the nodes the radio needs tell how it went.
    let result = unsafe { sys::ioctl_value(fd, START_DRIVERS, chip as libc::c_ulong) }.map(drop);
    devnodes::coldplug();
    if Path::new(DRIVER).exists() && Path::new(POWER).exists() {
        log!("{}", wmt::to_started_line(chip));
        Ok(())
    } else {
        Err(sys::invalid(format!("the drivers for chip {chip:#x} did not start: {result:?}")))
    }
}

fn hand_calibration() {
    let result = nvram::files(NVRAM).and_then(|mut files| files.read(wmt::WIFI_FILE)).and_then(|file| {
        let calibration = wmt::to_calibration(&file).ok_or_else(|| sys::invalid(wmt::to_unfit_text(file.len())))?;
        fs::write(POWER, calibration).map(|()| file.len())
    });
    match result {
        Ok(bytes) => log!("{}", wmt::to_handed_line(bytes)),
        Err(e) => log!("radio: the driver keeps its own settings, the factory's calibration is not handed over: {e}"),
    }
}

fn configure(driver: &File) -> io::Result<()> {
    let fd = driver.as_raw_fd();
    unsafe { sys::ioctl_value(fd, SET_INTERFACE, wmt::INTERFACE) }?;
    for name in wmt::ROM_PATCHES {
        let patch = wmt::parse_rom_patch(&read_head(name, wmt::ROM_HEAD_BYTES)?)
            .ok_or_else(|| sys::invalid(wmt::to_no_head_text(name)))?;
        let mut record = Record::new(patch, name);
        unsafe { sys::ioctl(fd, SET_ROM_PATCH, &raw mut record) }?;
    }
    let patch = wmt::parse_patch(&read_head(wmt::PATCH, wmt::PATCH_HEAD_BYTES)?)
        .map_err(|error| sys::invalid(error.to_file_text()))?;
    let mut record = Record::new(patch, wmt::PATCH);
    unsafe { sys::ioctl_value(fd, SET_PATCH_COUNT, wmt::PATCHES.into()) }?;
    unsafe { sys::ioctl(fd, SET_PATCH, &raw mut record) }?;
    Ok(())
}

fn answer(mut driver: &File) {
    let mut question = [0u8; wmt::QUESTION_BYTES];
    match driver.read(&mut question) {
        Ok(0) => {}
        Ok(count) => {
            log!("{}", wmt::to_question_line(&question[..count]));
            if let Err(e) = driver.write_all(wmt::DONE) {
                log!("radio: answering the driver: {e}");
            }
        }
        Err(e) => log!("radio: reading what the driver asks: {e}"),
    }
}

fn power_on(driver: &File) -> io::Result<()> {
    let write_end = Arc::new(Wake::new()?);
    let mut epoll = Epoll::new()?;
    epoll.add(driver.as_raw_fd(), QUESTION)?;
    epoll.add(write_end.fd(), WRITE_END)?;
    let worker_end = Arc::clone(&write_end);
    let write = POWER_POOL.run(move || {
        let result = fs::write(POWER, wmt::ON);
        worker_end.raise();
        result
    })?;
    loop {
        let ready = match epoll.wait(None) {
            Ok(ready) => ready,
            Err(e) => {
                // The core is as the write leaves it, which is waited for.
                drop(write.join());
                return Err(e);
            }
        };
        if ready.contains(&QUESTION) {
            answer(driver);
        }
        if ready.contains(&WRITE_END) {
            return write.join().unwrap_or_else(|| Err(io::Error::other("the write for power died")));
        }
    }
}

fn switch_on(setup: &mut Setup) -> io::Result<File> {
    if setup.is_failed() {
        return Err(io::Error::other(wmt::UNUSABLE));
    }
    let is_unconfigured = setup.is_unconfigured();
    let result: io::Result<File> = (|| {
        if is_unconfigured {
            start_drivers()?;
            // Kept calibration saves a power-on its second of calibrating; without it the radio works all the same.
            if let Err(e) = fs::write(CALIBRATION, RESTORE) {
                log!("radio: every power-on calibrates in full: {CALIBRATION}: {e}");
            }
            hand_calibration();
        }
        let driver = OpenOptions::new().read(true).write(true).open(DRIVER)?;
        if is_unconfigured {
            configure(&driver)?;
        }
        Ok(driver)
    })();
    *setup = setup.next(result.is_ok());
    let driver = result?;
    power_on(&driver)?;
    Ok(driver)
}

pub(super) fn power_up(setup: &mut Setup) -> Option<File> {
    let start = Instant::now();
    match switch_on(setup) {
        Ok(driver) => {
            log!("{}", wmt::to_on_line(start.elapsed().as_millis()));
            Some(driver)
        }
        Err(e) => {
            log!("radio: not on after {} ms: {e}", start.elapsed().as_millis());
            None
        }
    }
}

pub(super) fn power_down(driver: File) {
    let start = Instant::now();
    // The interface goes down first: only then does the driver end a scan for the kernel, which refuses every scan after one left running over a power-off.
    if let Err(e) = sys::iface::down(iface::NAME) {
        log!("radio: the interface would not go down: {e}");
    }
    match fs::write(POWER, wmt::OFF) {
        Ok(()) => log!("{}", wmt::to_off_line(start.elapsed().as_millis())),
        Err(e) => log!("radio: the core would not go off: {e}"),
    }
    drop(driver);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(record: &Record) -> &[u8] {
        unsafe { std::slice::from_raw_parts((&raw const *record).cast::<u8>(), size_of::<Record>()) }
    }

    #[test]
    fn ioctl_numbers() {
        let (int, pointer) = (size_of::<u32>(), size_of::<*const u8>());
        assert_eq!(
            [HARDWARE_INIT, ASK_CHIP, CLEAN_UP, START_DRIVERS],
            [10, 3, 5, 4].map(|number| sys::ioc(sys::IOC_READ, b'w', number, int))
        );
        assert_eq!(NAME_CHIP, sys::ioc(sys::IOC_WRITE, b'w', 1, int));
        assert_eq!([SET_INTERFACE, SET_PATCH_COUNT], [5, 14].map(|number| sys::ioc(sys::IOC_WRITE, 0xA0, number, int)));
        assert_eq!([SET_PATCH, SET_ROM_PATCH], [15, 31].map(|number| sys::ioc(sys::IOC_WRITE, 0xA0, number, pointer)));
    }

    #[test]
    fn record_layout() {
        assert_eq!(
            (size_of::<Record>(), std::mem::offset_of!(Record, address), std::mem::offset_of!(Record, name)),
            (264, 4, 8)
        );
        let name = wmt::ROM_PATCHES[2];
        let record = Record::new(wmt::Patch { number: 3, address: [0, 0, 0x14, 0xf0] }, name);
        let content = bytes(&record);
        assert_eq!(content[..8], [3, 0, 0, 0, 0, 0, 0x14, 0xf0], "the number in the machine's order, then the address");
        assert_eq!(&content[8..8 + name.len()], name.as_bytes());
        assert!(content[8 + name.len()..].iter().all(|&byte| byte == 0));
        assert!(wmt::ROM_PATCHES.iter().chain([&wmt::PATCH]).all(|name| name.len() < NAME_BYTES), "room for the zero");
    }

    #[test]
    fn failed_open_setup() {
        let mut setup = Setup::Configured;
        assert!(switch_on(&mut setup).is_err(), "here without the driver's node");
        assert_eq!(setup, Setup::Configured, "the next wish for power tries again");
        let mut setup = Setup::Failed;
        assert!(switch_on(&mut setup).is_err() && setup == Setup::Failed);
    }
}

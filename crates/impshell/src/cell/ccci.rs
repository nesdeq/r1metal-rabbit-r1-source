use crate::sys;
use r1_funcore::cell::ccci::{
    self, BOOT_WORDS, CCB_CONFIG_BYTES, CCB_OVERRUN, CCB_USERS, FLIGHT_STOP, MONITOR_BYTES, STATUS_BYTES,
};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;

pub(super) const BOOT_FILE: &str = "/sys/kernel/ccci/boot";
const MONITOR: &str = "/dev/ccci_monitor";
pub(super) const STATUS: &str = "/dev/ccci_md1_sta";
const CCB_CONTROL: &str = "/dev/ccci_ccb_ctrl";
pub(super) const FILES: &str = "/dev/ccci_fs";
pub(super) const RPC: &str = "/dev/ccci_rpc";
pub(super) const AT: &str = "/dev/ttyC0";
const MAGIC: u8 = b'C';
const WORD: usize = size_of::<u32>();
const DO_STOP_MD: u32 = sys::ioc(0, MAGIC, 12, 0);
const DO_START_MD: u32 = sys::ioc(0, MAGIC, 13, 0);
const SEND_BATTERY_INFO: u32 = sys::ioc(0, MAGIC, 21, 0);
const SET_BOOT_DATA: u32 = sys::ioc(sys::IOC_WRITE, MAGIC, 47, BOOT_WORDS * WORD);
const GET_CCB_CONFIG_LENGTH: u32 = sys::ioc(sys::IOC_READ, MAGIC, 63, WORD);
const GET_CCB_CONFIG: u32 = sys::ioc(sys::IOC_BOTH, MAGIC, 64, CCB_CONFIG_BYTES);
const CCB_CTRL_INFO: u32 = sys::ioc(sys::IOC_BOTH, MAGIC, 71, size_of::<ControlInfo>());

// The kernel's `struct ccb_ctrl_info` (`eccci/inc/ccci_core.h`).
#[repr(C)]
#[derive(Default)]
struct ControlInfo {
    user: u32,
    offset: u32,
    address: u32,
    length: u32,
}

// Opens the monitor node, whose close stops the modem: it is held for the app's life.
pub(super) fn open_monitor() -> io::Result<File> {
    File::open(MONITOR)
}

pub(super) fn open_port(path: &str) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

pub(super) fn read_message(mut monitor: &File) -> io::Result<[u8; MONITOR_BYTES]> {
    let mut message = [0u8; MONITOR_BYTES];
    monitor.read_exact(&mut message)?;
    Ok(message)
}

pub(super) fn read_event(mut status: &File) -> io::Result<[u8; STATUS_BYTES]> {
    let mut event = [0u8; STATUS_BYTES];
    status.read_exact(&mut event)?;
    Ok(event)
}

// Has the kernel hand the modem the battery's voltage it asked for.
pub(super) fn send_battery(monitor: &File) -> io::Result<()> {
    unsafe { sys::ioctl_value(monitor.as_raw_fd(), SEND_BATTERY_INFO, 0) }.map(drop)
}

fn ccb_configs(control: &File) -> io::Result<Vec<ccci::CcbConfig>> {
    let mut length = 0u32;
    unsafe { sys::ioctl(control.as_raw_fd(), GET_CCB_CONFIG_LENGTH, &raw mut length) }?;
    let mut configs = Vec::new();
    for index in 0..length {
        let mut record = [0u8; CCB_CONFIG_BYTES];
        record[..WORD].copy_from_slice(&index.to_ne_bytes());
        unsafe { sys::ioctl(control.as_raw_fd(), GET_CCB_CONFIG, record.as_mut_ptr()) }?;
        configs.extend(ccci::parse_ccb_config(&record));
    }
    Ok(configs)
}

// Writes each CCB user's descriptors into the control area, which the modem's buffer manager reads as it boots.
pub(super) fn init_ccb() -> io::Result<()> {
    let control = open_port(CCB_CONTROL)?;
    let configs = ccb_configs(&control)?;
    for user in CCB_USERS {
        let mut info = ControlInfo { user, ..ControlInfo::default() };
        unsafe { sys::ioctl(control.as_raw_fd(), CCB_CTRL_INFO, &raw mut info) }?;
        let words = ccci::ccb_words(&configs, user, info.offset, info.length);
        let words = words.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, CCB_OVERRUN))?;
        let mapping = sys::map(control.as_raw_fd(), 0, info.length as usize)?;
        for (at, word) in words {
            mapping.write(at, word);
        }
    }
    Ok(())
}

// Hands the kernel the boot data the modem's runtime data carries.
pub(super) fn set_boot_data(monitor: &File, words: &[u32; BOOT_WORDS]) -> io::Result<()> {
    let mut words = *words;
    unsafe { sys::ioctl(monitor.as_raw_fd(), SET_BOOT_DATA, words.as_mut_ptr()) }
}

// Starts the modem; the kernel's state machine carries it out after the call returns.
pub(super) fn start(monitor: &File) -> io::Result<()> {
    unsafe { sys::ioctl_value(monitor.as_raw_fd(), DO_START_MD, 0) }.map(drop)
}

// Stops the modem, as after `AT+EPOF` where asked; the kernel's state machine carries it out after the call returns.
pub(super) fn stop(monitor: &File, is_flight: bool) -> io::Result<()> {
    let mut flag = if is_flight { FLIGHT_STOP } else { 0 };
    unsafe { sys::ioctl(monitor.as_raw_fd(), DO_STOP_MD, &raw mut flag) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_kernel_headers() {
        assert_eq!([DO_STOP_MD, DO_START_MD, SEND_BATTERY_INFO, SET_BOOT_DATA], [0x430C, 0x430D, 0x4315, 0x4040_432F]);
        assert_eq!([GET_CCB_CONFIG_LENGTH, GET_CCB_CONFIG, CCB_CTRL_INFO], [0x8004_433F, 0xC018_4340, 0xC010_4347]);
        assert_eq!(size_of::<ControlInfo>(), 16, "four words, as the kernel's ccb_ctrl_info");
    }
}

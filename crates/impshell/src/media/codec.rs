use crate::media::ion::Ion;
use crate::sys;
use r1_funcore::base::handoff::Latch;
use r1_funcore::hw::LOCK_TIMEOUT_MS;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;

const DEVICE: &str = "/dev/Vcodec";
const VCODEC_WAITISR: u32 = sys::ioc(sys::IOC_WRITE, b'M', 0x0b, size_of::<u32>());
const VCODEC_LOCKHW: u32 = sys::ioc(sys::IOC_WRITE, b'M', 0x0d, size_of::<u32>());
const VCODEC_UNLOCKHW: u32 = sys::ioc(sys::IOC_WRITE, b'M', 0x33, size_of::<u32>());
const STATUS_WORDS: usize = 16;
const FIRST_STATUS: usize = 0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Driver {
    H264Encoder = 5,
    H264Decoder = 6,
    JpegEncoder = 23,
}

#[repr(C)]
#[derive(Default)]
struct HardwareLock {
    handle: u64,
    handle_size: u32,
    lock: u64,
    timeout_ms: u32,
    reserved: u64,
    reserved_size: u32,
    driver_type: u32,
    secure_instance: u8,
}

#[repr(C)]
#[derive(Default)]
struct Interrupt {
    handle: u64,
    handle_size: u32,
    driver_type: u32,
    isr_function: u64,
    reserved: u64,
    reserved_size: u32,
    timeout_ms: u32,
    status_count: u32,
    status: [u32; STATUS_WORDS],
}

#[derive(Clone, Default)]
pub(super) struct Owner(Arc<u64>);

impl Owner {
    fn address(&self) -> u64 {
        Arc::as_ptr(&self.0) as u64
    }
}

pub(super) struct Lock {
    file: File,
    owner: Owner,
    driver: Driver,
    is_held: Latch<bool>,
}

impl Lock {
    /// Takes the block for `driver` in the name of `owner`, through a node numbered above `floor`.
    pub fn take(owner: &Owner, driver: Driver, floor: RawFd) -> io::Result<Lock> {
        let file = sys::open_above(DEVICE, floor)?;
        let mut lock = Lock { file, owner: owner.clone(), driver, is_held: Latch::with(false) };
        lock.acquire()?;
        Ok(lock)
    }

    fn request(&self) -> HardwareLock {
        HardwareLock {
            handle: self.owner.address(),
            handle_size: size_of::<u64>() as u32,
            timeout_ms: LOCK_TIMEOUT_MS,
            driver_type: self.driver as u32,
            ..HardwareLock::default()
        }
    }

    pub(super) fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    pub(super) fn ion(&self) -> io::Result<Ion> {
        Ion::open(self.fd())
    }

    pub(super) fn is_held(&self) -> bool {
        self.is_held.is(&true)
    }

    pub(super) fn acquire(&mut self) -> io::Result<()> {
        if self.is_held.change(true).is_some() {
            let result = unsafe { sys::ioctl(self.file.as_raw_fd(), VCODEC_LOCKHW, &mut self.request()) };
            result.inspect_err(|_| self.is_held = Latch::with(false))?;
        }
        Ok(())
    }

    pub(super) fn unlock(&mut self) -> io::Result<()> {
        if self.is_held.change(false).is_none() {
            return Ok(());
        }
        unsafe { sys::ioctl(self.file.as_raw_fd(), VCODEC_UNLOCKHW, &mut self.request()) }
    }

    pub(super) fn wait_interrupt(&self, timeout_ms: u32) -> io::Result<Option<u32>> {
        let mut wait = Interrupt {
            handle: self.owner.address(),
            handle_size: size_of::<u64>() as u32,
            driver_type: self.driver as u32,
            timeout_ms,
            status_count: 1,
            ..Interrupt::default()
        };
        match unsafe { sys::ioctl(self.file.as_raw_fd(), VCODEC_WAITISR, &mut wait) } {
            Ok(()) => Ok(Some(wait.status[FIRST_STATUS])),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(super) fn release(mut self) -> io::Result<()> {
        self.unlock()
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_layouts() {
        assert_eq!(size_of::<HardwareLock>(), 56);
        assert_eq!(std::mem::offset_of!(HardwareLock, timeout_ms), 24);
        assert_eq!(std::mem::offset_of!(HardwareLock, driver_type), 44);
        assert_eq!(size_of::<Interrupt>(), 112);
        assert_eq!(std::mem::offset_of!(Interrupt, driver_type), 12);
        assert_eq!(std::mem::offset_of!(Interrupt, timeout_ms), 36);
        assert_eq!(std::mem::offset_of!(Interrupt, status), 44);
        assert_eq!(VCODEC_LOCKHW, 0x4004_4d0d);
        assert_eq!(VCODEC_UNLOCKHW, 0x4004_4d33);
        assert_eq!(VCODEC_WAITISR, 0x4004_4d0b);
        assert_eq!((Driver::H264Encoder as u32, Driver::H264Decoder as u32, Driver::JpegEncoder as u32), (5, 6, 23));
    }
}

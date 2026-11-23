use crate::sys::{self, IOC_BOTH, IOC_READ, IOC_WRITE, ioc};
use r1_funcore::hw::sound::card::{self, Direction};
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::zeroed;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

const PLAYBACK: &str = "/dev/snd/pcmC0D0p";
const CAPTURE: &str = "/dev/snd/pcmC0D1c";
const ACCESS_BY_CALLS: u32 = 3;
const FORMAT_S16_LE: u32 = 2;
const SUBFORMAT_PLAIN: u32 = 0;
const SAMPLE_BITS: u32 = 16;
const ACCESS: usize = 0;
const FORMAT: usize = 1;
const SUBFORMAT: usize = 2;
const BITS: usize = 0;
const FRAME_BITS: usize = 1;
const CHANNELS: usize = 2;
const RATE: usize = 3;
const PERIOD_SIZE: usize = 5;
const PERIODS: usize = 7;
const INTEGER: u32 = 1 << 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct Mask {
    bits: [u32; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Interval {
    min: u32,
    max: u32,
    flags: u32,
}

#[repr(C)]
struct HardwareParameters {
    flags: u32,
    masks: [Mask; 3],
    spare_masks: [Mask; 5],
    intervals: [Interval; 12],
    spare_intervals: [Interval; 9],
    rmask: u32,
    cmask: u32,
    info: u32,
    significant_bits: u32,
    rate_over: u32,
    rate_under: u32,
    fifo: u64,
    reserved: [u8; 64],
}

#[repr(C)]
struct SoftwareParameters {
    stamps: i32,
    period_step: u32,
    sleep_least: u32,
    wake_at: u64,
    align: u64,
    start_at: u64,
    stop_at: u64,
    silence_at: u64,
    silence: u64,
    boundary: u64,
    protocol: u32,
    stamp_kind: u32,
    reserved: [u8; 56],
}

#[repr(C)]
struct Transfer {
    result: i64,
    samples: *mut c_void,
    count: u64,
}

const HW_PARAMS: u32 = ioc(IOC_BOTH, b'A', 0x11, size_of::<HardwareParameters>());
const SW_PARAMS: u32 = ioc(IOC_BOTH, b'A', 0x13, size_of::<SoftwareParameters>());
const DELAY: u32 = ioc(IOC_READ, b'A', 0x21, size_of::<i64>());
const PREPARE: u32 = ioc(0, b'A', 0x40, 0);
const START: u32 = ioc(0, b'A', 0x42, 0);
const DROP: u32 = ioc(0, b'A', 0x43, 0);
const DRAIN: u32 = ioc(0, b'A', 0x44, 0);
const WRITE: u32 = ioc(IOC_WRITE, b'A', 0x50, size_of::<Transfer>());
const READ: u32 = ioc(IOC_READ, b'A', 0x51, size_of::<Transfer>());

pub(super) struct Pcm {
    file: File,
}

fn set_exact(interval: &mut Interval, value: u32) {
    *interval = Interval { min: value, max: value, flags: INTEGER };
}

fn set_only(mask: &mut Mask, bit: u32) {
    mask.bits = [1 << bit, 0, 0, 0, 0, 0, 0, 0];
}

fn hardware_parameters(rate: u32, period: usize, periods: usize) -> HardwareParameters {
    let mut parameters: HardwareParameters = unsafe { zeroed() };
    for interval in &mut parameters.intervals {
        interval.max = u32::MAX;
    }
    parameters.rmask = u32::MAX;
    parameters.info = u32::MAX;
    set_only(&mut parameters.masks[ACCESS], ACCESS_BY_CALLS);
    set_only(&mut parameters.masks[FORMAT], FORMAT_S16_LE);
    set_only(&mut parameters.masks[SUBFORMAT], SUBFORMAT_PLAIN);
    set_exact(&mut parameters.intervals[BITS], SAMPLE_BITS);
    set_exact(&mut parameters.intervals[FRAME_BITS], SAMPLE_BITS);
    set_exact(&mut parameters.intervals[CHANNELS], 1);
    set_exact(&mut parameters.intervals[RATE], rate);
    set_exact(&mut parameters.intervals[PERIOD_SIZE], period as u32);
    set_exact(&mut parameters.intervals[PERIODS], periods as u32);
    parameters
}

fn software_parameters(direction: Direction, periods: usize) -> SoftwareParameters {
    let thresholds = card::thresholds(direction, periods);
    let mut parameters: SoftwareParameters = unsafe { zeroed() };
    parameters.period_step = 1;
    (parameters.wake_at, parameters.start_at, parameters.stop_at) =
        (thresholds.wake_at, thresholds.start_at, thresholds.stop_at);
    (parameters.boundary, parameters.silence) = (thresholds.boundary, thresholds.silence);
    parameters
}

impl Pcm {
    pub(super) fn open(direction: Direction, rate: u32, periods: usize) -> io::Result<Pcm> {
        let node = if direction == Direction::Out { PLAYBACK } else { CAPTURE };
        let file = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(node)?;
        let flags = sys::check(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) })?;
        sys::check(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) })?;
        let pcm = Pcm { file };
        unsafe { sys::ioctl(pcm.file.as_raw_fd(), HW_PARAMS, &mut hardware_parameters(rate, card::PERIOD, periods)) }?;
        unsafe { sys::ioctl(pcm.file.as_raw_fd(), SW_PARAMS, &mut software_parameters(direction, periods)) }?;
        pcm.prepare()?;
        Ok(pcm)
    }

    fn control(&self, request: u32) -> io::Result<()> {
        unsafe { sys::ioctl_value(self.file.as_raw_fd(), request, 0) }.map(drop)
    }

    pub(super) fn prepare(&self) -> io::Result<()> {
        self.control(PREPARE)
    }

    pub(super) fn start(&self) -> io::Result<()> {
        self.control(START)
    }

    pub(super) fn drain(&self) -> io::Result<()> {
        self.control(DRAIN)
    }

    pub(super) fn delay(&self) -> io::Result<usize> {
        let mut samples = 0i64;
        unsafe { sys::ioctl(self.file.as_raw_fd(), DELAY, &mut samples) }?;
        Ok(samples.max(0) as usize)
    }

    /// Fills `samples` with the next recorded samples, waiting for them.
    pub fn read(&self, samples: &mut [i16]) -> io::Result<()> {
        let mut transfer = Transfer { result: 0, samples: samples.as_mut_ptr().cast(), count: samples.len() as u64 };
        unsafe { sys::ioctl(self.file.as_raw_fd(), READ, &mut transfer) }
    }

    /// Hands the kernel the next samples to play, waiting for room.
    pub fn write(&self, samples: &[i16]) -> io::Result<()> {
        let mut transfer =
            Transfer { result: 0, samples: samples.as_ptr().cast_mut().cast(), count: samples.len() as u64 };
        unsafe { sys::ioctl(self.file.as_raw_fd(), WRITE, &mut transfer) }
    }
}

impl Drop for Pcm {
    fn drop(&mut self) {
        let _ = self.control(DROP);
    }
}

pub(super) fn is_xrun(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EPIPE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_layouts() {
        assert_eq!(
            (size_of::<HardwareParameters>(), size_of::<SoftwareParameters>(), size_of::<Transfer>()),
            (608, 136, 24)
        );
        assert_eq!((HW_PARAMS, SW_PARAMS), (0xC260_4111, 0xC088_4113));
        assert_eq!((PREPARE, START, DROP, DRAIN), (0x4140, 0x4142, 0x4143, 0x4144));
        assert_eq!((WRITE, READ, DELAY), (0x4018_4150, 0x8018_4151, 0x8008_4121));
    }

    #[test]
    fn stream_parameters() {
        let parameters = hardware_parameters(48_000, 1024, 16);
        assert_eq!(
            (parameters.masks[ACCESS].bits[0], parameters.masks[FORMAT].bits[0], parameters.masks[SUBFORMAT].bits[0]),
            (8, 4, 1)
        );
        let interval =
            |at: usize| (parameters.intervals[at].min, parameters.intervals[at].max, parameters.intervals[at].flags);
        assert_eq!((interval(CHANNELS), interval(RATE)), ((1, 1, 4), (48_000, 48_000, 4)));
        assert_eq!(
            (interval(PERIOD_SIZE), interval(PERIODS), interval(FRAME_BITS)),
            ((1024, 1024, 4), (16, 16, 4), (16, 16, 4))
        );
        assert_eq!(
            (parameters.intervals[4].min, parameters.intervals[4].max),
            (0, u32::MAX),
            "what is not asked for is left open"
        );
        for (direction, periods) in [(Direction::Out, 4), (Direction::In, 8), (Direction::Out, card::CALL_PERIODS)] {
            let (software, set) = (software_parameters(direction, periods), card::thresholds(direction, periods));
            assert_eq!(
                (
                    software.wake_at,
                    software.start_at,
                    software.stop_at,
                    software.boundary,
                    software.silence,
                    software.silence_at
                ),
                (set.wake_at, set.start_at, set.stop_at, set.boundary, set.silence, 0),
                "{direction:?}: the kernel is asked for the card's thresholds"
            );
            assert_eq!(software.period_step, 1, "with every period counted");
        }
    }
}

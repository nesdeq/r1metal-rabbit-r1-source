use crate::sys;
use r1_funcore::base::handoff::Latch;
use r1_funcore::hw::camera::mode::{self, ExposureWrite, FRAME_LINES_REGISTER, ID_REGISTERS, Mode};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};

const DEVICE: &str = "/dev/kd_camera_hw";
const FEATURE_CONTROL: u32 = sys::ioc(sys::IOC_BOTH, b'i', 15, size_of::<FeatureCall>());
const CONTROL: u32 = sys::ioc(sys::IOC_BOTH, b'i', 20, size_of::<ControlCall>());
const SET_MCLK: u32 = sys::ioc(sys::IOC_BOTH, b'i', 60, size_of::<MasterClock>());
const MAIN: u32 = 0;
const MASTER_CLOCK_MHZ: u32 = 24;
const MASTER_CLOCK_OUTPUT: u8 = 0;
const SET_ESHUTTER: u32 = 3004;
const SET_GAIN: u32 = 3006;
const SET_REGISTER: u32 = 3011;
const GET_REGISTER: u32 = 3012;
const SET_STREAMING_SUSPEND: u32 = 3103;
const SET_STREAMING_RESUME: u32 = 3104;
const OPEN: u32 = 3105;
const CLOSE: u32 = 3106;
const SET_DRIVER: u32 = 3107;
// The bits of the value a register read leaves that are the register's: the sensor's registers are bytes.
const REGISTER_MASK: u32 = 0xFF;

#[repr(C)]
struct MasterClock {
    on: u8,
    freq: u32,
    tg: u8,
}

#[repr(C)]
struct FeatureCall {
    invoke_camera: u32,
    feature_id: u32,
    parameter: *mut u8,
    parameter_length: *mut u32,
}

// The parameter of a feature call that reads or writes a register of the sensor's.
#[repr(C)]
struct Register {
    address: u32,
    value: u32,
}

#[repr(C)]
struct ControlCall {
    invoke_camera: u32,
    scenario_id: u32,
    image_window: u64,
    sensor_config: u64,
}

pub(super) struct Sensor {
    file: File,
    is_clocked: Latch<bool>,
    is_powered: Latch<bool>,
    is_past_driver: Latch<bool>,
}

impl Sensor {
    pub(super) fn open(floor: RawFd) -> io::Result<Sensor> {
        Ok(Sensor {
            file: sys::open_above(DEVICE, floor)?,
            is_clocked: Latch::with(false),
            is_powered: Latch::with(false),
            is_past_driver: Latch::with(false),
        })
    }

    pub(super) fn master_clock(&mut self, is_on: bool) -> io::Result<()> {
        let mut clock = MasterClock { on: u8::from(is_on), freq: MASTER_CLOCK_MHZ, tg: MASTER_CLOCK_OUTPUT };
        unsafe { sys::ioctl(self.file.as_raw_fd(), SET_MCLK, &mut clock)? };
        self.is_clocked.change(is_on);
        Ok(())
    }

    // Makes a feature call with a parameter of the driver's layout, and hands back what the driver left in it.
    fn feature<P>(&self, id: u32, mut parameter: P) -> io::Result<P> {
        let mut length = size_of::<P>() as u32;
        let mut call = FeatureCall {
            invoke_camera: MAIN,
            feature_id: id,
            parameter: (&raw mut parameter).cast(),
            parameter_length: &mut length,
        };
        unsafe { sys::ioctl(self.file.as_raw_fd(), FEATURE_CONTROL, &mut call)? };
        Ok(parameter)
    }

    fn read_register(&self, address: u32) -> io::Result<u32> {
        Ok(self.feature(GET_REGISTER, Register { address, value: 0 })?.value & REGISTER_MASK)
    }

    pub(super) fn power_on(&mut self) -> io::Result<()> {
        self.feature(SET_DRIVER, [0u8; 4])?;
        self.feature(OPEN, [0u8; 8])?;
        self.is_powered.change(true);
        let [high, low] = ID_REGISTERS;
        if let Some(fault) = mode::to_sensor_fault(self.read_register(high)?, self.read_register(low)?) {
            return Err(io::Error::other(fault));
        }
        self.stream_off()
    }

    pub(super) fn mode(&mut self, mode: Mode) -> io::Result<()> {
        let mut call = ControlCall { invoke_camera: MAIN, scenario_id: mode as u32, image_window: 0, sensor_config: 0 };
        unsafe { sys::ioctl(self.file.as_raw_fd(), CONTROL, &mut call)? };
        self.is_past_driver.change(false);
        Ok(())
    }

    pub(super) fn frame_lines(&mut self, lines: u32) -> io::Result<()> {
        self.feature(SET_REGISTER, Register { address: FRAME_LINES_REGISTER, value: lines })?;
        self.is_past_driver.change(true);
        Ok(())
    }

    pub(super) fn expose(&self, lines: u32) -> io::Result<()> {
        match mode::to_exposure(self.is_past_driver.is(&true)) {
            ExposureWrite::Register(address) => {
                self.feature(SET_REGISTER, Register { address, value: lines }).map(drop)
            }
            ExposureWrite::Driver => self.feature(SET_ESHUTTER, u64::from(lines)).map(drop),
        }
    }

    pub(super) fn gain(&self, sixty_fourths: u32) -> io::Result<()> {
        self.feature(SET_GAIN, u64::from(sixty_fourths)).map(drop)
    }

    pub(super) fn stream_on(&self) -> io::Result<()> {
        self.feature(SET_STREAMING_RESUME, [0u8; 8]).map(drop)
    }

    pub(super) fn stream_off(&self) -> io::Result<()> {
        self.feature(SET_STREAMING_SUSPEND, [0u8; 8]).map(drop)
    }

    pub(super) fn power_off(&mut self) -> io::Result<()> {
        let power_result =
            if self.is_powered.change(false).is_some() { self.feature(CLOSE, [0u8; 8]).map(drop) } else { Ok(()) };
        let clock_result = if self.is_clocked.is(&true) { self.master_clock(false) } else { Ok(()) };
        power_result.and(clock_result)
    }
}

impl Drop for Sensor {
    fn drop(&mut self) {
        let _ = self.power_off();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_layouts() {
        assert_eq!(size_of::<MasterClock>(), 12);
        assert_eq!(size_of::<FeatureCall>(), 24);
        assert_eq!(size_of::<ControlCall>(), 24);
        assert_eq!(
            (size_of::<Register>(), std::mem::offset_of!(Register, address), std::mem::offset_of!(Register, value)),
            (size_of::<u64>(), 0, 4),
            "a word of 64 bits as a value is one: the register's address in its low half, its value in the high"
        );
        assert_eq!(FEATURE_CONTROL, 0xc018_690f);
        assert_eq!(CONTROL, 0xc018_6914);
        assert_eq!(SET_MCLK, 0xc00c_693c);
    }
}

//! The sensor's modes as its driver's tables have them, and the registers r1metal reads and writes past the driver.

use crate::hw::camera::frame::Frame;
use crate::hw::camera::frame::SENSOR;
use crate::hw::camera::shading;
use crate::hw::camera::shading::Lens;
use crate::hw::camera::shading::SensorArea;
use alloc::format;
use alloc::string::String;

/// The sensor's two identity registers, the high byte's first.
pub const ID_REGISTERS: [u32; 2] = [0x0F17, 0x0F16];
const HI846: u32 = 0x0846;
/// The sensor's register for the lines of a frame, 16 bits.
pub const FRAME_LINES_REGISTER: u32 = 0x0006;
const EXPOSURE_LINES_REGISTER: u32 = 0x0074;

const fn foreign_id(high: u32, low: u32) -> Option<u32> {
    let id = high << 8 | low;
    if id == HI846 { None } else { Some(id) }
}

/// The error text where the two identity registers name another sensor than the Hi846.
pub fn to_sensor_fault(high: u32, low: u32) -> Option<String> {
    foreign_id(high, low).map(|id| format!("the sensor answers with id {id:#06x}"))
}

/// How an exposure's lines reach the sensor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExposureWrite {
    // Past the driver, which keeps a frame length set so: by the sensor's register of that address.
    Register(u32),
    Driver,
}

/// The write of an exposure, by the register where a frame length was set past the driver.
pub const fn to_exposure(is_past_driver: bool) -> ExposureWrite {
    if is_past_driver { ExposureWrite::Register(EXPOSURE_LINES_REGISTER) } else { ExposureWrite::Driver }
}

/// A mode of the sensor, by its member of enum MSDK_SCENARIO_ID_ENUM.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    // 1632x1224, binned, 30 frames a second.
    Preview = 0,
    // 3264x2448, 30 frames a second.
    Capture = 1,
    // 640x480, the middle 2560x1920 a quarter each way, up to 120 frames a second.
    HighSpeed = 3,
    // 1280x720, binned and cropped, up to 90 frames a second.
    SlimVideo = 4,
}

impl Mode {
    /// The frames the sensor sends in this mode.
    pub const fn frame(self) -> Frame {
        match self {
            Mode::Preview => Frame::whole(1632, 1224),
            Mode::Capture => SENSOR,
            Mode::HighSpeed => Frame::whole(640, 480),
            Mode::SlimVideo => Frame::whole(1280, 720),
        }
    }

    pub(super) const fn frame_lines(self) -> u32 {
        match self {
            Mode::Preview | Mode::Capture => 2526,
            Mode::HighSpeed => 631,
            Mode::SlimVideo => 842,
        }
    }

    const fn area(self) -> SensorArea {
        match self {
            Mode::Preview => SensorArea { left: 0, top: 0, scale: 2 },
            Mode::Capture => SensorArea { left: 0, top: 0, scale: 1 },
            Mode::HighSpeed => SensorArea { left: 352, top: 264, scale: 4 },
            Mode::SlimVideo => SensorArea { left: 352, top: 504, scale: 2 },
        }
    }

    /// The lens over the mode's frames: the reference module's shading, no unit having its own.
    pub const fn lens(self) -> Lens {
        Lens { shading: &shading::REFERENCE, area: self.area() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Mode; 4] = [Mode::Preview, Mode::Capture, Mode::HighSpeed, Mode::SlimVideo];

    #[test]
    fn mode_sizes() {
        assert_eq!(Mode::Preview.frame(), Frame::whole(1632, 1224));
        assert_eq!(Mode::Capture.frame().bytes(), 9_987_840);
        assert_eq!(Mode::SlimVideo.frame().line_bytes(), 1600);
        assert_eq!((Mode::HighSpeed.frame().line_bytes(), Mode::HighSpeed.frame().bytes()), (800, 384_000));
        assert_eq!(ALL.map(Mode::frame_lines), [2526, 2526, 631, 842]);
        assert_eq!(ALL.map(|mode| mode as u32), [0, 1, 3, 4]);
        assert_eq!(
            (foreign_id(0x08, 0x46), foreign_id(0x02, 0x46)),
            (None, Some(0x0246)),
            "the Hi846, and another sensor"
        );
    }

    #[test]
    fn frames_centred_on_sensor() {
        let sensor = Mode::Capture.frame();
        for mode in ALL {
            let (frame, area) = (mode.frame(), mode.area());
            assert_eq!(
                (2 * area.left + area.scale * frame.width(), 2 * area.top + area.scale * frame.lines),
                (sensor.width(), sensor.lines),
                "{mode:?}: as much of the sensor left of its frames as right of them, and above as below"
            );
        }
        assert_eq!(Mode::SlimVideo.area(), SensorArea { left: 352, top: 504, scale: 2 });
        assert_eq!(
            Mode::HighSpeed.area(),
            SensorArea { left: 352, top: 264, scale: 4 },
            "the driver's window: pixels 352 to 2912 and lines 264 to 2184, each four sensor pixels a pixel"
        );
    }

    #[test]
    fn exposure_write_and_identity() {
        assert_eq!(to_exposure(false), ExposureWrite::Driver);
        assert_eq!(to_exposure(true), ExposureWrite::Register(0x0074), "by the register of its lines");
        assert_eq!(to_sensor_fault(0x08, 0x46), None);
        assert_eq!(to_sensor_fault(0x02, 0x46).as_deref(), Some("the sensor answers with id 0x0246"));
    }
}

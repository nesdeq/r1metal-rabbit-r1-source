//! The MIPI receiver of the R1's image sensor in Rabbit's programming order: CSI port 2, analog blocks CSI2A and CSI2B, sensor interface 5 and mux 2.

use crate::hw::Block;
use crate::hw::Hardware;
use crate::hw::camera::frame::Frame;
use crate::hw::poll;
use alloc::format;
use alloc::string::String;
use core::time::Duration;

const TOP_MUX: usize = 0x08;
const PORT2_CONTROL: usize = 0x24;
const INTERFACE1: usize = 0x0000;
const INTERFACE5: usize = 0x4000;
const MUX2: usize = 0x2D00;
const INTERFACE5_INDEX: u32 = 4;
const MUX2_SHIFT: u32 = 8;
const MUX_FIELD: u32 = 0xF;

const CONTROL: usize = 0x200;
const EXTENSION: usize = 0x204;
const CLOCK_STEP: usize = 0x614;
const LANE_MUX: usize = 0x824;
const CSI2_CONTROL: usize = 0xA00;
const CSI2_SETTLE: usize = 0xA08;
const CSI2_DPCM: usize = 0xA0C;
const CSI2_INT_ENABLE: usize = 0xA10;
const CSI2_STATUS: usize = 0xA14;
const CSI2_DEBUG_SELECT: usize = 0xA18;
const CSI2_SPARE: usize = 0xA20;
const CSI2_HS_TRAIL: usize = 0xA40;
const CSI2_RESYNC: usize = 0xA74;
const CSI2_MODE: usize = 0xAE8;
const CSI2_INT_ENABLE_EXT: usize = 0xB10;
const CSI2_SYNC: usize = 0xB20;

const MUX_CONTROL: usize = 0x00;
const MUX_STATUS: usize = 0x08;
const MUX_MEASURED: usize = 0x14;
const MUX_EXTENSION: usize = 0x3C;

const CSI0A: usize = 0x0000;
const CSI2A: usize = 0x4000;
const CSI2B: usize = 0x5000;
const ANALOG_CONTROL: usize = 0x00;
const ANALOG_TRIM_LOW: usize = 0x08;
const ANALOG_TRIM_HIGH: usize = 0x0C;
const ANALOG_EQUALISER_LOW: usize = 0x18;
const ANALOG_EQUALISER_HIGH: usize = 0x1C;
const ANALOG_WORD_24: usize = 0x24;
const ANALOG_WORD_40: usize = 0x40;
const ANALOG_CALIBRATION: usize = 0x48;
const ANALOG_RESET_MODE: usize = 0x80;
const ANALOG_BYTE_CLOCK: usize = 0xA8;

const TIME_STAMP_STEP: u32 = 0x67;
const INTERFACE_ENABLE: u32 = 1;
const CSI2_IP: u32 = 0x40;
const OLD_CSI2_IP: u32 = 0x20;
const INTERFACE_SOURCE: u32 = 0xF << 12;
const PAD_SELECT: u32 = 7 << 28;
const FOUR_LANES_ONE_CLOCK: u32 = 1;
const PORT_CLOCK_SELECT: u32 = 7 << 8;
const PORT_CLOCK_LANE_2: u32 = 2 << 8;
const PORT_ENABLE: u32 = 1 << 31;

const BANDGAP_FILTER: u32 = 1 << 2;
const BANDGAP_CORE: u32 = 1 << 3;
const LANE0_CLOCK_MODE: u32 = 1 << 5;
const LANE0_CLOCK_SELECT: u32 = 1 << 6;
const LANE1_CLOCK_MODE: u32 = 1 << 8;
const LANE1_CLOCK_SELECT: u32 = 1 << 9;
const LANE2_CLOCK_MODE: u32 = 1 << 11;
const LANE2_CLOCK_SELECT: u32 = 1 << 12;
const EQUALISER_FIELDS_LOW: [u32; 4] = [4, 6, 20, 22];
const EQUALISER_FIELDS_HIGH: [u32; 2] = [4, 6];
const TWO_BITS: u32 = 3;
const WORD_24_FIELD: u32 = 0xFF << 24;
const WORD_24_VALUE: u32 = 0x40 << 24;
const CSI0A_WORD_40: u32 = 0x90;
const RESET_MODE: u32 = 3 << 16;
const BYTE_CLOCK_INVERTS_A: [u32; 3] = [1, 2, 4];
const BYTE_CLOCK_INVERTS_B: [u32; 3] = [1, 1, 1];
const CALIBRATE_LOW: [u32; 2] = [1, 1 << 16];
const CALIBRATE_HIGH: u32 = 1;
const CALIBRATION_DONE: u32 = 0x38;
const FIVE_BITS: u32 = 0x1F;

const LANE_ORDER: [(u32, u32); 4] = [(24, 1), (26, 2), (28, 0), (30, 3)];
const SETTLE: u32 = 0x15;
const SETTLE_SHIFT: u32 = 8;
const HS_TRAIL: u32 = 0x1B;
const HS_TRAIL_FIELD: u32 = 0xFF;
const LANES_AND_CLOCK: u32 = 0x1F;
const HEADER_ORDER: u32 = 1 << 16;
const HS_TRAIL_ENABLE: u32 = 1 << 25;
const CLOCK_HS_OPTION: u32 = 1 << 27;
const HS_DETECT: u32 = 1 << 7;
const RESYNC_BYPASS: u32 = 1 << 10;
const CPHY_SELECT: u32 = 1 << 11;
const RESYNC_COUNT: u32 = 7;
const RESYNC_COUNT_VALUE: u32 = 3;
const MODE: u32 = 0xFF;
const HEADER_LENGTH: u32 = 7 << 8;
const SYNC_FIELD: u32 = 0xFFFF;
const SYNC_PATTERN_SHIFT: u32 = 16;
const SYNC_MASK: u32 = 0xFF00;
const SYNC_PATTERN: u32 = 0x001D;
const SPARE_BIT: u32 = 1;
const DEBUG_PACKET_COUNT: u32 = 0x8000_001A;
const EXT_INTERRUPTS: u32 = 0x1F;
const ALL: u32 = u32::MAX;

const MUX_ENABLE: u32 = 1 << 31;
const MUX_SOURCE: u32 = 0xF << 12;
const MUX_SOURCE_MIPI: u32 = 8 << 12;
const MUX_NORMAL: u32 = 1;
const MUX_FOUR_PIXELS: u32 = 1 << 4;
const MUX_TWO_PIXELS: u32 = 1 << 8;
const MUX_FULL_WRITE: u32 = 3 << 28;
const MUX_FULL_WRITE_RAW: u32 = 2 << 28;
const MUX_FLUSH: u32 = 0x3F << 22;
const MUX_FLUSH_RAW: u32 = 0x1B << 22;
const MUX_PUSH: u32 = 0x3F << 16;
const MUX_PUSH_RAW: u32 = 0x1F << 16;
const MUX_SYNC_POLARITIES: u32 = 3 << 9;
const MUX_RESETS: u32 = 3;
const MUX_EXTENSION_BIT_8: u32 = 1 << 8;
const MUX_OVERRUN: u32 = 1;
const MEASURED_WIDTH_SHIFT: u32 = 16;
const MEASURED_HEIGHT: u32 = 0xFFFF;

const STATUS_FRAMED: u32 = 0x9044;
const STATUS_ERRORS: u32 = 0xB0;

const MASTER_CLOCK_PAUSE: u32 = 100;
const BANDGAP_PAUSE: u32 = 30;
const SHORT_PAUSE: u32 = 1;
const CALIBRATION_POLL_PAUSE: u32 = 30;
const CALIBRATION_POLLS: u32 = 100;
const LANE_MUX_PAUSE: u32 = 30;
const HEALTH_PAUSE: u32 = 1_000;

const ANALOG_RESET: [(usize, u32); 6] = [
    (ANALOG_EQUALISER_LOW, 0x8850_8850),
    (ANALOG_EQUALISER_HIGH, 0x0000_8850),
    (ANALOG_WORD_24, 0),
    (ANALOG_TRIM_LOW, 0x1010_1010),
    (ANALOG_TRIM_HIGH, 0x0000_1010),
    (ANALOG_CONTROL, 0x0000_0002),
];

const DEVINFO_TRIM: [usize; 2] = [0x1D0, 0x1D4];
const TRIM_FIELDS: [(usize, [u32; 2]); 3] =
    [(ANALOG_TRIM_LOW, [0, 8]), (ANALOG_TRIM_LOW, [16, 24]), (ANALOG_TRIM_HIGH, [0, 8])];
const TRIM_SHIFTS_A: [u32; 3] = [12, 7, 2];
const TRIM_SHIFTS_B: [u32; 3] = [27, 22, 20];
/// The error text where `receiver_on` tells false.
pub const NO_CALIBRATION: &str = "the receiver's offset calibration does not finish";
/// The error text where `Trim::from_devinfo` finds none.
pub const TRIM_TOO_SHORT: &str = "too short";

/// The factory trim of the port's two analog blocks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Trim {
    csi2a: u32,
    csi2b: u32,
}

impl Trim {
    /// No trim: the receiver keeps its reset trim, as on a device whose trim cannot be read.
    pub const NONE: Trim = Trim { csi2a: 0, csi2b: 0 };

    /// Picks the trim out of `/proc/device-tree/chosen/atag,devinfo`; none if that is too short.
    pub fn from_devinfo(devinfo: &[u8]) -> Option<Trim> {
        let word = |at: usize| Some(u32::from_le_bytes(devinfo.get(at..at + 4)?.try_into().ok()?));
        Some(Trim { csi2a: word(DEVINFO_TRIM[0])?, csi2b: word(DEVINFO_TRIM[1])? })
    }
}

/// What the receiver says about the stream it sees.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Health {
    status: u32,
    mux_status: u32,
    width: u32,
    height: u32,
}

impl Health {
    const fn is_framed(&self) -> bool {
        self.status & STATUS_FRAMED == STATUS_FRAMED && self.height != 0
    }

    const fn is_faulty(&self) -> bool {
        self.status & STATUS_ERRORS != 0 || self.mux_status & MUX_OVERRUN != 0
    }

    const fn sees(&self, frame: Frame) -> bool {
        self.is_framed() && !self.is_faulty() && self.width == frame.width() && self.height == frame.lines
    }

    /// The error text of a stream that is no clean stream of whole frames of that size.
    pub fn to_fault(&self, frame: Frame) -> Option<String> {
        let Health { status, mux_status, width, height } = *self;
        (!self.sees(frame)).then(|| {
            format!(
                "the receiver sees no clean stream: status {status:#x}, mux status {mux_status:#x}, frames of {width}x{height}"
            )
        })
    }
}

/// The registers that go with the sensor's master clock; the clock itself is the kernel's.
pub fn master_clock<H: Hardware>(hw: &mut H) {
    hw.write(Block::Seninf, INTERFACE1 + CLOCK_STEP, TIME_STAMP_STEP);
    hw.update(Block::Seninf, INTERFACE1 + CONTROL, 0, INTERFACE_ENABLE);
    hw.update(Block::Seninf, INTERFACE1 + EXTENSION, 0, CSI2_IP);
    hw.pause(MASTER_CLOCK_PAUSE);
}

/// Makes mux 2 take the MIPI stream of sensor interface 5 and hand CAMSV0 two pixels a clock.
pub fn mux_on<H: Hardware>(hw: &mut H) {
    let control = MUX2 + MUX_CONTROL;
    hw.update(Block::Seninf, control, 0, MUX_ENABLE);
    hw.update(Block::Seninf, control, MUX_SOURCE, MUX_SOURCE_MIPI);
    hw.update(Block::Seninf, MUX2 + MUX_EXTENSION, TWO_BITS, MUX_NORMAL);
    hw.update(Block::Seninf, MUX2 + MUX_EXTENSION, MUX_FOUR_PIXELS, 0);
    hw.update(Block::Seninf, control, 0, MUX_TWO_PIXELS);
    hw.update(Block::Seninf, control, MUX_FULL_WRITE, MUX_FULL_WRITE_RAW);
    hw.update(Block::Seninf, control, MUX_FLUSH, MUX_FLUSH_RAW);
    hw.update(Block::Seninf, control, MUX_PUSH, MUX_PUSH_RAW);
    hw.update(Block::Seninf, control, MUX_SYNC_POLARITIES, 0);
    let value = hw.read(Block::Seninf, control);
    hw.write(Block::Seninf, control, value | MUX_RESETS);
    hw.write(Block::Seninf, control, value & !MUX_RESETS);
    hw.update(Block::Seninf, MUX2 + MUX_EXTENSION, 0, MUX_EXTENSION_BIT_8);
    hw.update(Block::Seninf, TOP_MUX, MUX_FIELD << MUX2_SHIFT, INTERFACE5_INDEX << MUX2_SHIFT);
}

fn port_and_interface<H: Hardware>(hw: &mut H, is_on: bool) {
    hw.update(Block::Seninf, PORT2_CONTROL, FOUR_LANES_ONE_CLOCK, 0);
    hw.update(Block::Seninf, PORT2_CONTROL, PORT_CLOCK_SELECT, PORT_CLOCK_LANE_2);
    hw.update(Block::Seninf, PORT2_CONTROL, PORT_ENABLE, if is_on { PORT_ENABLE } else { 0 });
    hw.update(Block::Seninf, INTERFACE5 + CONTROL, INTERFACE_ENABLE, u32::from(is_on));
    hw.update(Block::Seninf, INTERFACE5 + CONTROL, PAD_SELECT, 0);
    hw.update(Block::Seninf, INTERFACE5 + CONTROL, INTERFACE_SOURCE, 0);
    hw.update(Block::Seninf, INTERFACE5 + EXTENSION, CSI2_IP, if is_on { CSI2_IP } else { 0 });
    hw.update(Block::Seninf, INTERFACE5 + EXTENSION, OLD_CSI2_IP, 0);
}

fn trim_blocks<H: Hardware>(hw: &mut H, trim: Trim) {
    for (block, word, shifts) in [(CSI2A, trim.csi2a, TRIM_SHIFTS_A), (CSI2B, trim.csi2b, TRIM_SHIFTS_B)] {
        for ((field, lows), shift) in TRIM_FIELDS.into_iter().zip(shifts) {
            let value = word >> shift & FIVE_BITS;
            for low in lows {
                hw.update(Block::Analog, block + field, FIVE_BITS << low, value << low);
            }
        }
    }
}

fn is_calibrated<H: Hardware>(hw: &mut H, block: usize) -> bool {
    let word = (Block::Analog, block + ANALOG_CALIBRATION);
    poll(hw, word, (CALIBRATION_POLLS, CALIBRATION_POLL_PAUSE), |state| state & CALIBRATION_DONE != 0)
}

/// Brings the receiver up for four data lanes of 10 bit pixels; tells whether both analog blocks finished their offset calibration.
pub fn receiver_on<H: Hardware>(hw: &mut H, trim: Trim) -> bool {
    port_and_interface(hw, true);
    for (block, lane2_is_clock) in [(CSI2A, true), (CSI2B, false)] {
        let word = block + ANALOG_CONTROL;
        hw.update(Block::Analog, word, LANE0_CLOCK_MODE, 0);
        hw.update(Block::Analog, word, 0, LANE0_CLOCK_SELECT);
        hw.update(Block::Analog, word, LANE1_CLOCK_MODE, 0);
        hw.update(Block::Analog, word, 0, LANE1_CLOCK_SELECT);
        hw.update(Block::Analog, word, LANE2_CLOCK_MODE, if lane2_is_clock { LANE2_CLOCK_MODE } else { 0 });
        hw.update(Block::Analog, word, 0, LANE2_CLOCK_SELECT);
    }
    for invert in BYTE_CLOCK_INVERTS_A {
        hw.update(Block::Analog, CSI2A + ANALOG_BYTE_CLOCK, 0, invert);
    }
    for invert in BYTE_CLOCK_INVERTS_B {
        hw.update(Block::Analog, CSI2B + ANALOG_BYTE_CLOCK, 0, invert);
    }
    for block in [CSI2A, CSI2B] {
        for low in EQUALISER_FIELDS_LOW {
            hw.update(Block::Analog, block + ANALOG_EQUALISER_LOW, TWO_BITS << low, 1 << low);
        }
        for low in EQUALISER_FIELDS_HIGH {
            hw.update(Block::Analog, block + ANALOG_EQUALISER_HIGH, TWO_BITS << low, 1 << low);
        }
    }
    hw.write(Block::Analog, CSI0A + ANALOG_WORD_40, CSI0A_WORD_40);
    for block in [CSI2A, CSI2B] {
        hw.update(Block::Analog, block + ANALOG_WORD_24, WORD_24_FIELD, WORD_24_VALUE);
    }
    for block in [CSI2A, CSI2B] {
        hw.update(Block::Analog, block + ANALOG_RESET_MODE, RESET_MODE, 0);
    }
    if trim.csi2a != 0 && trim.csi2b != 0 {
        trim_blocks(hw, trim);
    }
    for block in [CSI2A, CSI2B] {
        hw.update(Block::Analog, block + ANALOG_CONTROL, 0, BANDGAP_CORE);
    }
    hw.pause(BANDGAP_PAUSE);
    for block in [CSI2A, CSI2B] {
        hw.update(Block::Analog, block + ANALOG_CONTROL, 0, BANDGAP_FILTER);
    }
    hw.pause(SHORT_PAUSE);
    for block in [CSI2A, CSI2B] {
        for enable in CALIBRATE_LOW {
            hw.update(Block::Analog, block + ANALOG_EQUALISER_LOW, 0, enable);
        }
        hw.update(Block::Analog, block + ANALOG_EQUALISER_HIGH, 0, CALIBRATE_HIGH);
    }
    hw.pause(SHORT_PAUSE);
    let calibrations = [CSI2A, CSI2B].map(|block| is_calibrated(hw, block));
    for (low, lane) in LANE_ORDER {
        hw.update(Block::Seninf, INTERFACE5 + LANE_MUX, TWO_BITS << low, lane << low);
    }
    hw.pause(LANE_MUX_PAUSE);

    let csi = INTERFACE5;
    hw.write(Block::Seninf, csi + CSI2_DPCM, 0);
    hw.write(Block::Seninf, csi + CSI2_SETTLE, SETTLE << SETTLE_SHIFT);
    hw.update(Block::Seninf, csi + CSI2_CONTROL, CLOCK_HS_OPTION, 0);
    hw.update(Block::Seninf, csi + CSI2_CONTROL, HS_DETECT, 0);
    hw.update(Block::Seninf, csi + CSI2_CONTROL, 0, LANES_AND_CLOCK | HEADER_ORDER);
    hw.update(Block::Seninf, csi + CSI2_CONTROL, 0, HS_TRAIL_ENABLE);
    hw.update(Block::Seninf, csi + CSI2_HS_TRAIL, HS_TRAIL_FIELD, HS_TRAIL);
    hw.update(Block::Seninf, csi + CSI2_RESYNC, RESYNC_BYPASS, 0);
    hw.update(Block::Seninf, csi + CSI2_RESYNC, CPHY_SELECT, 0);
    hw.update(Block::Seninf, csi + CSI2_RESYNC, RESYNC_COUNT, RESYNC_COUNT_VALUE);
    hw.update(Block::Seninf, csi + CSI2_MODE, MODE, 0);
    hw.update(Block::Seninf, csi + CSI2_MODE, HEADER_LENGTH, 0);
    hw.update(Block::Seninf, csi + CSI2_SYNC, SYNC_FIELD, SYNC_MASK);
    hw.update(Block::Seninf, csi + CSI2_SYNC, SYNC_FIELD << SYNC_PATTERN_SHIFT, SYNC_PATTERN << SYNC_PATTERN_SHIFT);
    hw.update(Block::Seninf, csi + CSI2_SPARE, SPARE_BIT, 0);
    hw.write(Block::Seninf, csi + CSI2_DEBUG_SELECT, DEBUG_PACKET_COUNT);
    hw.write(Block::Seninf, csi + CSI2_INT_ENABLE, ALL);
    hw.write(Block::Seninf, csi + CSI2_STATUS, ALL);
    hw.write(Block::Seninf, csi + CSI2_INT_ENABLE_EXT, EXT_INTERRUPTS);
    calibrations == [true, true]
}

/// Switches the receiver off, the bandgaps of both analog blocks last.
pub fn receiver_off<H: Hardware>(hw: &mut H) {
    port_and_interface(hw, false);
    hw.update(Block::Seninf, INTERFACE5 + CSI2_CONTROL, LANES_AND_CLOCK, 0);
    for block in [CSI2A, CSI2B] {
        hw.update(Block::Analog, block + ANALOG_CONTROL, BANDGAP_CORE, 0);
        hw.update(Block::Analog, block + ANALOG_CONTROL, BANDGAP_FILTER, 0);
    }
}

/// Puts the analog words back to their boot values: these blocks keep their state when the camera's power domain drops.
pub fn analog_to_reset<H: Hardware>(hw: &mut H) {
    for block in [CSI2A, CSI2B] {
        for (word, value) in ANALOG_RESET {
            hw.write(Block::Analog, block + word, value);
        }
    }
    hw.write(Block::Analog, CSI0A + ANALOG_WORD_40, 0);
}

fn health<H: Hardware>(hw: &mut H) -> Health {
    let measured = hw.read(Block::Seninf, MUX2 + MUX_MEASURED);
    Health {
        status: hw.read(Block::Seninf, INTERFACE5 + CSI2_STATUS),
        mux_status: hw.read(Block::Seninf, MUX2 + MUX_STATUS),
        width: measured >> MEASURED_WIDTH_SHIFT,
        height: measured & MEASURED_HEIGHT,
    }
}

fn clear_status<H: Hardware>(hw: &mut H) {
    hw.write(Block::Seninf, INTERFACE5 + CSI2_STATUS, ALL);
}

fn await_frames<H: Hardware>(hw: &mut H, pauses: u32) -> Health {
    for _ in 0..pauses {
        let state = health(hw);
        if state.is_framed() {
            return state;
        }
        hw.pause(HEALTH_PAUSE);
    }
    health(hw)
}

/// The receiver's view of a begun stream: whole frames waited for, the latched status cleared, and whole frames waited for again, each wait at least `timeout` where it runs out.
pub fn await_stream<H: Hardware>(hw: &mut H, timeout: Duration) -> Health {
    let pauses = u32::try_from(timeout.as_micros() / u128::from(HEALTH_PAUSE)).unwrap_or(u32::MAX);
    await_frames(hw, pauses);
    clear_status(hw);
    await_frames(hw, pauses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::fake::Fake;
    use alloc::vec;
    use alloc::vec::Vec;

    const TRIM: Trim = Trim { csi2a: 0x5300_C630, csi2b: 0x5B00_0000 };

    fn after_boot() -> Fake {
        let mut values = vec![
            (Block::Seninf, TOP_MUX, 0x0004_3210),
            (Block::Seninf, MUX2 + MUX_CONTROL, 0x06DF_0080),
            (Block::Seninf, INTERFACE5 + LANE_MUX, 0xE400_0000),
            (Block::Seninf, INTERFACE5 + CSI2_CONTROL, 0x0008_6160),
            (Block::Seninf, INTERFACE5 + CSI2_SETTLE, 0x0000_2000),
            (Block::Seninf, INTERFACE5 + CSI2_SPARE, 0xFFFF_FFFF),
            (Block::Seninf, INTERFACE5 + CSI2_RESYNC, 1),
            (Block::Seninf, INTERFACE5 + CSI2_SYNC, 0x001D_FF00),
            (Block::Seninf, INTERFACE5 + CSI2_INT_ENABLE_EXT, 0x0000_0F1E),
        ];
        for block in [CSI2A, CSI2B] {
            values.extend(ANALOG_RESET.map(|(word, value)| (Block::Analog, block + word, value)));
            values.push((Block::Analog, block + ANALOG_RESET_MODE, 2));
            values.push((Block::Analog, block + ANALOG_BYTE_CLOCK, 7));
            values.push((Block::Analog, block + ANALOG_CALIBRATION, 0x38));
        }
        Fake::with(&values)
    }

    #[test]
    fn mux_registers() {
        let mut hw = after_boot();
        mux_on(&mut hw);
        assert_eq!(hw.at(Block::Seninf, MUX2 + MUX_CONTROL), 0xA6DF_8180);
        assert_eq!(hw.at(Block::Seninf, MUX2 + MUX_EXTENSION), 0x101);
        assert_eq!(hw.at(Block::Seninf, TOP_MUX), 0x0004_3410);
        let resets: Vec<u32> =
            hw.writes.iter().filter(|write| write.1 == MUX2 + MUX_CONTROL).map(|write| write.2 & MUX_RESETS).collect();
        assert_eq!(resets[resets.len() - 2..], [3, 0], "the reset pulse comes last on the control word");
    }

    #[test]
    fn receiver_registers() {
        let mut hw = after_boot();
        assert!(receiver_on(&mut hw, TRIM));
        assert_eq!(hw.at(Block::Seninf, PORT2_CONTROL), 0x8000_0200);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CONTROL), 1);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + EXTENSION), 0x40);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + LANE_MUX), 0xC900_0000);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CSI2_CONTROL), 0x0209_617F);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CSI2_SETTLE), 0x1500);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CSI2_HS_TRAIL), 0x1B);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CSI2_RESYNC), 3);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CSI2_SPARE), 0xFFFF_FFFE);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CSI2_DEBUG_SELECT), 0x8000_001A);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CSI2_INT_ENABLE_EXT), 0x1F);
        assert_eq!(hw.at(Block::Analog, CSI2A + ANALOG_CONTROL), 0x0000_1A4E);
        assert_eq!(hw.at(Block::Analog, CSI2B + ANALOG_CONTROL), 0x0000_124E);
        assert_eq!(hw.at(Block::Analog, CSI2A + ANALOG_TRIM_LOW), 0x0C0C_0C0C);
        assert_eq!(hw.at(Block::Analog, CSI2A + ANALOG_TRIM_HIGH), 0x0000_0C0C);
        assert_eq!(hw.at(Block::Analog, CSI2B + ANALOG_TRIM_LOW), 0x0C0C_0B0B);
        assert_eq!(hw.at(Block::Analog, CSI2B + ANALOG_TRIM_HIGH), 0x0000_1010);
        for block in [CSI2A, CSI2B] {
            assert_eq!(hw.at(Block::Analog, block + ANALOG_EQUALISER_LOW), 0x8851_8851);
            assert_eq!(hw.at(Block::Analog, block + ANALOG_EQUALISER_HIGH), 0x0000_8851);
            assert_eq!(hw.at(Block::Analog, block + ANALOG_WORD_24), 0x4000_0000);
            assert_eq!(hw.at(Block::Analog, block + ANALOG_RESET_MODE), 2);
        }
        assert_eq!(hw.at(Block::Analog, CSI0A + ANALOG_WORD_40), 0x90);
    }

    #[test]
    fn bandgap_order() {
        let mut hw = after_boot();
        receiver_on(&mut hw, TRIM);
        let control = CSI2A + ANALOG_CONTROL;
        let on_write = |bit: u32| {
            hw.writes
                .iter()
                .position(|&(block, offset, value)| block == Block::Analog && offset == control && value & bit != 0)
        };
        assert!(on_write(BANDGAP_CORE).unwrap() < on_write(BANDGAP_FILTER).unwrap());
        assert!(hw.pause_micros >= u64::from(BANDGAP_PAUSE + LANE_MUX_PAUSE));
    }

    #[test]
    fn no_trim() {
        for trim in [Trim { csi2a: 0, csi2b: 0x5B00_0000 }, Trim::NONE] {
            let mut hw = after_boot();
            receiver_on(&mut hw, trim);
            assert_eq!(hw.at(Block::Analog, CSI2A + ANALOG_TRIM_LOW), 0x1010_1010);
            assert_eq!(hw.at(Block::Analog, CSI2B + ANALOG_TRIM_LOW), 0x1010_1010);
        }
    }

    #[test]
    fn calibration_timeout() {
        let mut hw = after_boot();
        hw.memory.insert((Block::Analog, CSI2B + ANALOG_CALIBRATION), 0);
        assert!(!receiver_on(&mut hw, TRIM));
        assert!(hw.pause_micros >= u64::from(CALIBRATION_POLLS * CALIBRATION_POLL_PAUSE));
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CSI2_CONTROL), 0x0209_617F, "the rest of the sequence still runs");
    }

    #[test]
    fn switch_off_and_reset() {
        let mut hw = after_boot();
        receiver_on(&mut hw, TRIM);
        receiver_off(&mut hw);
        assert_eq!(hw.at(Block::Seninf, PORT2_CONTROL), 0x0000_0200);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CONTROL), 0);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + EXTENSION), 0);
        assert_eq!(hw.at(Block::Seninf, INTERFACE5 + CSI2_CONTROL), 0x0209_6160);
        assert_eq!(hw.at(Block::Analog, CSI2A + ANALOG_CONTROL), 0x0000_1A42);
        assert_eq!(hw.at(Block::Analog, CSI2B + ANALOG_CONTROL), 0x0000_1242);
        analog_to_reset(&mut hw);
        for block in [CSI2A, CSI2B] {
            for (word, value) in ANALOG_RESET {
                assert_eq!(hw.at(Block::Analog, block + word), value);
            }
        }
        assert_eq!(hw.at(Block::Analog, CSI0A + ANALOG_WORD_40), 0);
    }

    #[test]
    fn master_clock_registers() {
        let mut hw = Fake::default();
        master_clock(&mut hw);
        assert_eq!(hw.writes, [(Block::Seninf, 0x614, 0x67), (Block::Seninf, 0x200, 1), (Block::Seninf, 0x204, 0x40)]);
        assert_eq!(hw.pause_micros, 100);
    }

    #[test]
    fn health_states() {
        let mut hw = Fake::with(&[
            (Block::Seninf, INTERFACE5 + CSI2_STATUS, 0x0040_9044),
            (Block::Seninf, MUX2 + MUX_STATUS, 0x18),
            (Block::Seninf, MUX2 + MUX_MEASURED, 0x0CC0_0990),
        ]);
        let state = health(&mut hw);
        assert_eq!((state.width, state.height), (3264, 2448));
        assert!(state.is_framed() && !state.is_faulty());
        assert!(!Health { status: 0x1044, ..state }.is_framed(), "no frame end yet");
        assert!(!Health { height: 0, ..state }.is_framed());
        assert!(Health { status: 0x9064, ..state }.is_faulty(), "a checksum error");
        assert!(Health { mux_status: 0x19, ..state }.is_faulty(), "a mux overrun");
        let capture = Frame::whole(3264, 2448);
        assert!(state.sees(capture) && !state.sees(Frame::whole(1632, 1224)), "a clean stream of frames of that size");
        assert_eq!(state.to_fault(capture), None);
        assert_eq!(
            state.to_fault(Frame::whole(1632, 1224)).as_deref(),
            Some("the receiver sees no clean stream: status 0x409044, mux status 0x18, frames of 3264x2448")
        );
        assert!(!Health { status: 0x9064, ..state }.sees(capture) && !Health { height: 0, ..state }.sees(capture));
        clear_status(&mut hw);
        assert_eq!(hw.writes, [(Block::Seninf, INTERFACE5 + CSI2_STATUS, u32::MAX)]);
    }

    #[test]
    fn stream_wait() {
        let mut dark = Fake::default();
        let state = await_stream(&mut dark, Duration::from_millis(5));
        assert!(
            !state.is_framed() && dark.pause_micros == 2 * 5 * 1_000,
            "no frames: both waits run out their patience"
        );
        assert_eq!(dark.writes, [(Block::Seninf, INTERFACE5 + CSI2_STATUS, u32::MAX)], "the status cleared between");
        let mut streaming = Fake::with(&[
            (Block::Seninf, INTERFACE5 + CSI2_STATUS, 0x9044),
            (Block::Seninf, MUX2 + MUX_MEASURED, 0x0CC0_0990),
        ]);
        assert!(
            await_stream(&mut streaming, Duration::from_millis(5)).is_framed() && streaming.pause_micros == 0,
            "frames seen at once wait for nothing"
        );
    }

    #[test]
    fn trim_from_devinfo() {
        let mut devinfo = vec![0u8; 804];
        devinfo[0x1D0..0x1D4].copy_from_slice(&0x5300_C630u32.to_le_bytes());
        devinfo[0x1D4..0x1D8].copy_from_slice(&0x5B00_0000u32.to_le_bytes());
        assert_eq!(Trim::from_devinfo(&devinfo), Some(TRIM));
        assert_eq!(Trim::from_devinfo(&devinfo[..0x1D6]), None);
    }
}

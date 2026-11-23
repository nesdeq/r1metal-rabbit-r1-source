//! CAMSV0, the block that writes the sensor's raw frames to memory, in Rabbit's register order.

use crate::hw::Block;
use crate::hw::Hardware;
use crate::hw::camera::frame::Frame;
use crate::hw::poll;
use alloc::collections::VecDeque;

const MODULE_EN: usize = 0x10;
const FMT_SEL: usize = 0x14;
const INT_EN: usize = 0x18;
const SW_CTL: usize = 0x20;
const IMGO_FBC: usize = 0x2C;
const CLK_EN: usize = 0x30;
const PAK: usize = 0x4C;
const FBC_CTL1: usize = 0x110;
const FBC_CTL2: usize = 0x114;
const ENQ_ADDR: usize = 0x118;
const SPECIAL_FUN_EN: usize = 0x218;
const XSIZE: usize = 0x230;
const YSIZE: usize = 0x234;
const STRIDE: usize = 0x238;
const CON: usize = 0x23C;
const CON2: usize = 0x240;
const CON3: usize = 0x244;
const CROP: usize = 0x248;
const WORD_474: usize = 0x474;
const DMA_DEBUG_SEL: usize = 0x488;
const SEN_MODE: usize = 0x500;
const VF_CON: usize = 0x504;
const GRAB_PXL: usize = 0x508;
const GRAB_LIN: usize = 0x50C;
const INTER_ST: usize = 0x53C;
const WORD_574: usize = 0x574;
const FH_EN: usize = 0xE00;
const FH_ADDR: usize = 0xE04;

const TG_EN: u32 = 1;
const PAK_EN: u32 = 1 << 2;
const PAK_SEL: u32 = 1 << 3;
const IMGO_EN: u32 = 1 << 4;
const SUBSAMPLE_EN: u32 = 1 << 5;
const SUBSAMPLE_PERIOD: u32 = 0x3F << 8;
const DOUBLE_BUFFER: u32 = 1 << 30;
const FORMAT_RAW10: u32 = 1;
const INTERRUPTS: u32 = 0x0013_04B1;
const PAK_MODE: u32 = 7;
const PAK_RAW10: u32 = 1;
const PAK_PIXELS: u32 = 3 << 4;
const PAK_TWO_PIXELS: u32 = 1 << 4;
const WORD_474_BIT_31: u32 = 1 << 31;
const SPECIAL_FUNCTIONS: u32 = 0x6100_0000;
const ALL: u32 = u32::MAX;

const CMOS_EN: u32 = 1;
const TWO_PIXEL_BUS: u32 = 1 << 1;
const FOUR_PIXEL_BUS: u32 = 1 << 13;
const SEN_MODE_BIT_16: u32 = 1 << 16;
const SUBSAMPLE_BITS: [u32; 2] = [1 << 18, 1 << 17];
const VIEW_FINDER: u32 = 1;
const SINGLE_FRAME: u32 = 1 << 1;
const TG_STATE: u32 = 0x3F << 8;
const TG_IDLE: u32 = 1 << 8;

const FBC_MODE: u32 = 1 << 16;
const FBC_EN: u32 = 1 << 15;
const FBC_BIT_22: u32 = 1 << 22;
const FBC_TOP_BYTE: u32 = 0xFF << 24;
const FBC_PENDING: u32 = 0x7F;
const FBC_DROPS_SHIFT: u32 = 24;
const ENQUEUE: u32 = 1;
const BUS_TWO_PIXELS: u32 = 0x0103_0000;
const DMA_CON: u32 = 0x8000_0080;
const DMA_THRESHOLDS: u32 = 0x0002_0002;
const FRAME_HEADER: u32 = 1;

const SW_RESET: u32 = 4;
const DMA_RESET: u32 = 1;
const DMA_RESET_DONE: u32 = 3;
const RESET_POLL_PAUSE: u32 = 10;
const RESET_POLLS: u32 = 20;
const CLOCKS: [u32; 3] = [1, 1 << 2, 1 << 15];

/// The interrupt status bit of a vsync.
pub const VSYNC: u32 = 1;
/// The interrupt status bit of a finished frame.
pub const FRAME_DONE: u32 = 1 << 20;
// Vsyncs the block gets to rest after its view finder bit is cleared, as stock gives it.
const REST_VSYNCS: u32 = 10;
/// The log line of a block that came to no rest.
pub const NO_VSYNC_LINE: &str = "camera: no vsync while CAMSV0 comes to rest";
/// The slots of a stream's buffer, all queued as it starts, and as many of a burst's as are queued at once.
pub const SLOTS: usize = 3;

/// Why the block did not do what was asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    Reset,
    Streaming,
    NotArmed,
    NoInput,
}

impl Error {
    pub const fn message(self) -> &'static str {
        match self {
            Error::Reset => "CAMSV0 does not finish its reset",
            Error::Streaming => "CAMSV0 streams already",
            Error::NotArmed => "CAMSV0 takes no buffer",
            Error::NoInput => "CAMSV0 has no sensor input",
        }
    }
}

/// The block's count of buffers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Counts {
    pending: u32,
    /// Frames lost for want of a buffer.
    pub drops: u32,
}

impl Counts {
    /// How many of the `queued` buffers are full.
    pub const fn full(&self, queued: usize) -> usize {
        queued.saturating_sub(self.pending as usize)
    }
}

/// What a capture does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaptureStep {
    // Queue the buffer of that frame.
    Queue(usize),
    // No frame came since the last look.
    Wait,
    Look,
    Done,
}

/// How far a capture of a number of frames, each into a buffer of its own, has got.
pub struct Capture {
    frames: usize,
    window: usize,
    queued: usize,
    done: usize,
}

impl Capture {
    /// A capture of `frames` frames, at most `window` of them queued at once; the first `queued()` are queued as it begins.
    pub const fn new(frames: usize, window: usize) -> Capture {
        let queued = if frames < window { frames } else { window };
        Capture { frames, window, queued, done: 0 }
    }

    /// How many frames are queued so far.
    pub const fn queued(&self) -> usize {
        self.queued
    }

    /// Takes the block's counts: what to do next.
    pub const fn step(&mut self, counts: Counts) -> CaptureStep {
        let full = counts.full(self.queued);
        let is_same = full == self.done;
        self.done = full;
        if self.done >= self.frames {
            return CaptureStep::Done;
        }
        if self.queued < self.frames && self.queued < self.done + self.window {
            self.queued += 1;
            return CaptureStep::Queue(self.queued - 1);
        }
        if is_same { CaptureStep::Wait } else { CaptureStep::Look }
    }
}

/// How far the block has come to rest since its view finder bit was cleared.
#[derive(Default)]
pub struct Rest {
    vsyncs: u32,
}

impl Rest {
    /// Takes a vsync and whether the sensor input is idle after it: true while the next vsync is waited for.
    pub const fn take_vsync(&mut self, is_idle: bool) -> bool {
        self.vsyncs += 1;
        !is_idle && self.vsyncs < REST_VSYNCS
    }
}

/// The slots of a stream's buffers the block has queued, in the order it fills them.
pub struct BufferQueue(VecDeque<usize>);

impl BufferQueue {
    /// Every one of `slots` slots, queued in their order.
    pub fn all(slots: usize) -> BufferQueue {
        BufferQueue((0..slots).collect())
    }

    /// How many are queued.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True while none is.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Takes the `full` oldest buffers: returns the newest's slot, out of the line, and the others', to be queued again; none where `full` is nought or more than are queued.
    pub fn take(&mut self, full: usize) -> Option<(usize, impl Iterator<Item = usize> + '_)> {
        let stale = full.checked_sub(1)?;
        let newest = self.0.remove(stale)?;
        self.0.rotate_left(stale);
        let tail = self.0.len() - stale;
        Some((newest, self.0.range(tail..).copied()))
    }

    /// Puts a slot that is queued again at the end of the line.
    pub fn put(&mut self, slot: usize) {
        self.0.push_back(slot);
    }
}

/// The frames the sensor has sent since a stream began, drops included, by the block's counts.
#[derive(Default)]
pub struct FrameCount {
    frames: u64,
    drops: u32,
}

impl FrameCount {
    /// Takes `full` buffers and the block's count of drops, which only grows or starts anew; returns the frames sent.
    pub fn add(&mut self, full: usize, drops: u32) -> u64 {
        let new_drops = drops.checked_sub(self.drops).unwrap_or(drops);
        (self.frames, self.drops) = (self.frames + full as u64 + u64::from(new_drops), drops);
        self.frames
    }
}

fn reset<H: Hardware>(hw: &mut H) -> bool {
    for value in [SW_RESET, 0, DMA_RESET] {
        hw.write(Block::Camsv, SW_CTL, value);
    }
    let is_done = poll(hw, (Block::Camsv, SW_CTL), (RESET_POLLS, RESET_POLL_PAUSE), |state| state == DMA_RESET_DONE);
    hw.write(Block::Camsv, SW_CTL, 0);
    is_done
}

/// Sets the sensor input window, two pixels a clock; the first step of a start, before the kernel's bookkeeping is cleared.
pub fn set_window<H: Hardware>(hw: &mut H, frame: Frame) {
    hw.update(Block::Camsv, SEN_MODE, CMOS_EN, 0);
    for bit in SUBSAMPLE_BITS {
        hw.update(Block::Camsv, SEN_MODE, bit, 0);
    }
    hw.write(Block::Camsv, WORD_574, 0);
    hw.update(Block::Camsv, SEN_MODE, 0, SEN_MODE_BIT_16);
    hw.update(Block::Camsv, VF_CON, SINGLE_FRAME, 0);
    hw.update(Block::Camsv, SEN_MODE, 0, TWO_PIXEL_BUS);
    hw.update(Block::Camsv, SEN_MODE, FOUR_PIXEL_BUS, 0);
    hw.write(Block::Camsv, GRAB_PXL, frame.left | frame.right << 16);
    hw.write(Block::Camsv, GRAB_LIN, frame.lines << 16);
}

fn dma_config<H: Hardware>(hw: &mut H, frame: Frame) {
    hw.write(Block::Camsv, XSIZE, frame.line_bytes() - 1);
    hw.write(Block::Camsv, YSIZE, frame.lines - 1);
    hw.write(Block::Camsv, STRIDE, frame.line_bytes() | BUS_TWO_PIXELS);
    hw.write(Block::Camsv, CROP, 0);
    hw.write(Block::Camsv, CON, DMA_CON);
    hw.write(Block::Camsv, CON2, DMA_THRESHOLDS);
    hw.write(Block::Camsv, CON3, DMA_THRESHOLDS);
}

/// Resets the block and sets it up to pack RAW10, two pixels a clock, into queued buffers: top, frame buffer control and write DMA, then their enables.
pub fn arm<H: Hardware>(hw: &mut H, frame: Frame) -> Result<(), Error> {
    if !reset(hw) {
        return Err(Error::Reset);
    }
    hw.update(Block::Camsv, MODULE_EN, 0, TG_EN);
    hw.update(Block::Camsv, MODULE_EN, 0, DOUBLE_BUFFER);
    hw.write(Block::Camsv, FMT_SEL, FORMAT_RAW10);
    hw.write(Block::Camsv, INT_EN, INTERRUPTS);
    hw.update(Block::Camsv, MODULE_EN, SUBSAMPLE_EN, 0);
    hw.update(Block::Camsv, MODULE_EN, SUBSAMPLE_PERIOD, 0);
    hw.update(Block::Camsv, MODULE_EN, 0, PAK_EN);
    hw.update(Block::Camsv, MODULE_EN, PAK_SEL, 0);
    hw.update(Block::Camsv, PAK, PAK_MODE, PAK_RAW10);
    hw.update(Block::Camsv, PAK, PAK_PIXELS, PAK_TWO_PIXELS);
    hw.write(Block::Camsv, FH_EN, 0);
    hw.update(Block::Camsv, WORD_474, WORD_474_BIT_31, 0);
    hw.write(Block::Camsv, DMA_DEBUG_SEL, ALL);
    hw.update(Block::Camsv, SPECIAL_FUN_EN, 0, SPECIAL_FUNCTIONS);

    hw.write(Block::Camsv, FBC_CTL1, 0);
    hw.update(Block::Camsv, FBC_CTL1, 0, FBC_MODE);
    dma_config(hw, frame);
    hw.update(Block::Camsv, SEN_MODE, 0, CMOS_EN);
    if is_streaming(hw) {
        return Err(Error::Streaming);
    }
    hw.update(Block::Camsv, FBC_CTL1, FBC_TOP_BYTE, 0);
    hw.update(Block::Camsv, FBC_CTL1, 0, FBC_EN);
    hw.update(Block::Camsv, FBC_CTL1, 0, FBC_BIT_22);
    hw.update(Block::Camsv, MODULE_EN, 0, IMGO_EN);
    hw.update(Block::Camsv, FH_EN, 0, FRAME_HEADER);
    Ok(())
}

/// Hands the block one buffer at device address `buffer`, filled in queue order; `header` is where it writes each frame's header, the same for every buffer.
pub fn queue<H: Hardware>(hw: &mut H, frame: Frame, buffer: u32, header: u32) -> Result<(), Error> {
    dma_config(hw, frame);
    if hw.read(Block::Camsv, FBC_CTL1) & FBC_EN == 0 {
        return Err(Error::NotArmed);
    }
    hw.write(Block::Camsv, ENQ_ADDR, buffer);
    hw.write(Block::Camsv, FH_ADDR, header);
    hw.update(Block::Camsv, IMGO_FBC, 0, ENQUEUE);
    Ok(())
}

/// Switches the block's clocks on; the kernel's view finder call comes after it.
pub fn clocks_on<H: Hardware>(hw: &mut H) -> Result<(), Error> {
    for clock in CLOCKS {
        hw.update(Block::Camsv, CLK_EN, 0, clock);
    }
    if hw.read(Block::Camsv, SEN_MODE) & CMOS_EN == 0 {
        return Err(Error::NoInput);
    }
    Ok(())
}

pub fn counts<H: Hardware>(hw: &mut H) -> Counts {
    let word = hw.read(Block::Camsv, FBC_CTL2);
    Counts { pending: word & FBC_PENDING, drops: word >> FBC_DROPS_SHIFT }
}

/// True while the view finder bit is set.
pub fn is_streaming<H: Hardware>(hw: &mut H) -> bool {
    hw.read(Block::Camsv, VF_CON) & VIEW_FINDER != 0
}

/// True once the sensor input has come to rest after the view finder bit was cleared.
pub fn is_idle<H: Hardware>(hw: &mut H) -> bool {
    hw.read(Block::Camsv, INTER_ST) & TG_STATE == TG_IDLE
}

/// The first half of a stop, once the stream rests: every enable off; the kernel's bookkeeping is cleared after it.
pub fn disarm<H: Hardware>(hw: &mut H) {
    hw.update(Block::Camsv, MODULE_EN, DOUBLE_BUFFER, 0);
    hw.write(Block::Camsv, MODULE_EN, 0);
    hw.write(Block::Camsv, FMT_SEL, 0);
    hw.write(Block::Camsv, INT_EN, 0);
    hw.write(Block::Camsv, FBC_CTL1, 0);
    hw.update(Block::Camsv, MODULE_EN, 0, DOUBLE_BUFFER);
}

/// The second half of a stop: reset, clocks off, and the enables of the parts, whether or not the reset handshake finished.
pub fn shut<H: Hardware>(hw: &mut H) -> Result<(), Error> {
    let is_done = reset(hw);
    for clock in CLOCKS {
        hw.update(Block::Camsv, CLK_EN, clock, 0);
    }
    hw.write(Block::Camsv, FBC_CTL1, 0);
    hw.update(Block::Camsv, MODULE_EN, IMGO_EN, 0);
    hw.update(Block::Camsv, FH_EN, FRAME_HEADER, 0);
    hw.update(Block::Camsv, SEN_MODE, CMOS_EN, 0);
    if is_done { Ok(()) } else { Err(Error::Reset) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::fake::Fake;
    use alloc::vec;
    use alloc::vec::Vec;

    const SLIM: Frame = Frame::whole(1280, 720);

    fn after_power_on() -> Fake {
        Fake::with(&[
            (Block::Camsv, MODULE_EN, 0x4000_0000),
            (Block::Camsv, FMT_SEL, 0x0000_0100),
            (Block::Camsv, SW_CTL, 2),
            (Block::Camsv, CON, 0x8000_0080),
            (Block::Camsv, CON2, 0x0080_0080),
            (Block::Camsv, CON3, 0x0080_0080),
            (Block::Camsv, DMA_DEBUG_SEL, 0xFFFF_FFFF),
            (Block::Camsv, GRAB_PXL, 0x7FFF_0000),
            (Block::Camsv, GRAB_LIN, 0x3FFF_0000),
            (Block::Camsv, INTER_ST, 0x0000_0102),
            (Block::Camsv, FH_EN, 0x0000_007F),
        ])
    }

    fn after_start() -> Fake {
        let mut hw = after_power_on();
        set_window(&mut hw, SLIM);
        assert_eq!(arm(&mut hw, SLIM), Ok(()));
        assert_eq!(queue(&mut hw, SLIM, 0x0080_0000, 0x00B0_0000), Ok(()));
        assert_eq!(clocks_on(&mut hw), Ok(()));
        hw
    }

    #[test]
    fn start_registers() {
        let hw = after_start();
        let expected = [
            (MODULE_EN, 0x4000_0015),
            (FMT_SEL, 1),
            (INT_EN, 0x0013_04B1),
            (CLK_EN, 0x8005),
            (PAK, 0x11),
            (FBC_CTL1, 0x0041_8000),
            (ENQ_ADDR, 0x0080_0000),
            (SPECIAL_FUN_EN, 0x6100_0000),
            (XSIZE, 0x63F),
            (YSIZE, 0x2CF),
            (STRIDE, 0x0103_0640),
            (CON, 0x8000_0080),
            (CON2, 0x0002_0002),
            (CON3, 0x0002_0002),
            (SEN_MODE, 0x0001_0003),
            (GRAB_PXL, 0x0500_0000),
            (GRAB_LIN, 0x02D0_0000),
            (FH_EN, 1),
            (FH_ADDR, 0x00B0_0000),
            (IMGO_FBC, 1),
        ];
        for (offset, value) in expected {
            assert_eq!(hw.at(Block::Camsv, offset), value, "register {offset:#x}");
        }
    }

    #[test]
    fn window_grab() {
        let mut hw = after_power_on();
        set_window(&mut hw, Frame { left: 640, right: 1280, lines: 720 });
        assert_eq!(hw.at(Block::Camsv, GRAB_PXL), 0x0500_0280);
    }

    #[test]
    fn reset_handshake() {
        let mut hw = after_power_on();
        set_window(&mut hw, SLIM);
        let start = hw.writes.len();
        arm(&mut hw, SLIM).unwrap();
        let handshake: Vec<u32> = hw.writes[start..start + 4].iter().map(|write| write.2).collect();
        assert_eq!(handshake, [4, 0, 1, 0]);
        assert!(hw.writes[start..start + 4].iter().all(|write| write.1 == SW_CTL));

        let mut stuck = after_power_on();
        stuck.is_stuck = true;
        assert_eq!(arm(&mut stuck, SLIM), Err(Error::Reset));
        assert_eq!(stuck.pause_micros, u64::from(RESET_POLLS * RESET_POLL_PAUSE));
        assert_eq!(stuck.at(Block::Camsv, MODULE_EN), 0x4000_0000, "nothing is enabled after a failed reset");
    }

    #[test]
    fn refused_arm_and_queue() {
        let mut hw = after_power_on();
        hw.memory.insert((Block::Camsv, VF_CON), 1);
        assert_eq!(arm(&mut hw, SLIM), Err(Error::Streaming));
        assert_eq!(queue(&mut hw, SLIM, 0x0080_0000, 0x00B0_0000), Err(Error::NotArmed));
        assert_eq!(hw.at(Block::Camsv, ENQ_ADDR), 0);
        let mut off = after_power_on();
        assert_eq!(clocks_on(&mut off), Err(Error::NoInput));
    }

    #[test]
    fn queue_header() {
        let mut hw = after_start();
        queue(&mut hw, SLIM, 0x00B0_0000, 0x00B0_0000 - 0x1000).unwrap();
        let queued: Vec<u32> = hw.writes.iter().filter(|write| write.1 == ENQ_ADDR).map(|write| write.2).collect();
        assert_eq!(queued, [0x0080_0000, 0x00B0_0000]);
        assert_eq!(hw.writes.iter().filter(|write| write.1 == IMGO_FBC).count(), 2);
    }

    #[test]
    fn count_fields() {
        let mut hw = Fake::with(&[
            (Block::Camsv, FBC_CTL2, 0x0208_0A02),
            (Block::Camsv, VF_CON, 1),
            (Block::Camsv, INTER_ST, 0x0808_0403),
        ]);
        assert_eq!(counts(&mut hw), Counts { pending: 2, drops: 2 });
        assert_eq!([0, 2, 3].map(|queued| counts(&mut hw).full(queued)), [0, 0, 1], "of three queued, one is full");
        assert!(is_streaming(&mut hw) && !is_idle(&mut hw));
        hw.memory.insert((Block::Camsv, INTER_ST), 0x0808_0102);
        hw.memory.insert((Block::Camsv, VF_CON), 0);
        assert!(!is_streaming(&mut hw) && is_idle(&mut hw));
    }

    #[test]
    fn buffer_queue() {
        let mut queued = BufferQueue::all(3);
        let (newest, stale) = queued.take(1).unwrap();
        assert_eq!((newest, stale.count(), queued.len()), (0, 0, 2), "one full: the oldest is the newest");
        queued.put(0);
        let (newest, stale) = queued.take(2).unwrap();
        assert_eq!((newest, stale.collect::<Vec<_>>()), (2, vec![1]), "of two full, the older is queued again");
        assert_eq!(queued.0, [0, 1], "behind the one that still waits");
        queued.put(2);
        let (newest, stale) = queued.take(3).unwrap();
        assert_eq!((newest, stale.collect::<Vec<_>>(), queued.len()), (2, vec![0, 1], 2), "all full");
        assert!(queued.take(0).is_none() && queued.take(3).is_none(), "none full, or more than are queued");
        assert_eq!(queued.0, [0, 1], "and the line is left as it was");
    }

    #[test]
    fn frame_count() {
        let mut sent = FrameCount::default();
        assert_eq!([sent.add(1, 0), sent.add(2, 0), sent.add(1, 3), sent.add(1, 3)], [1, 3, 7, 8]);
        assert_eq!(sent.add(1, 1), 10, "a count that started anew: what it shows was dropped since");
        assert_eq!(Error::NoInput.message(), "CAMSV0 has no sensor input");
    }

    #[test]
    fn capture_steps() {
        let pending = |pending: u32| Counts { pending, drops: 0 };
        let mut pair = Capture::new(2, 3);
        assert_eq!(pair.queued(), 2, "fewer frames than the window: all queued at once");
        assert_eq!(pair.step(pending(2)), CaptureStep::Wait, "none in");
        assert_eq!(pair.step(pending(1)), CaptureStep::Look, "one came, and the count is read again");
        assert_eq!(pair.step(pending(1)), CaptureStep::Wait);
        assert_eq!(pair.step(pending(0)), CaptureStep::Done);
        let mut burst = Capture::new(5, 3);
        assert_eq!(burst.queued(), 3, "a window's worth queued as it begins");
        assert_eq!(burst.step(pending(3)), CaptureStep::Wait);
        assert_eq!(burst.step(pending(2)), CaptureStep::Queue(3), "a frame came: the next buffer goes in behind");
        assert_eq!(burst.step(pending(3)), CaptureStep::Wait, "three queued again, one full");
        assert_eq!(burst.step(pending(1)), CaptureStep::Queue(4), "two more came");
        assert_eq!(burst.step(pending(2)), CaptureStep::Wait, "every buffer queued, and no frame since");
        assert_eq!(burst.step(pending(1)), CaptureStep::Look);
        assert_eq!(burst.step(pending(0)), CaptureStep::Done, "and every frame in");
    }

    #[test]
    fn rest_steps() {
        assert!(!Rest::default().take_vsync(true), "idle at the first vsync: no more is waited for");
        let mut rest = Rest::default();
        assert!(rest.take_vsync(false) && !rest.take_vsync(true), "idle at the second");
        let mut busy = Rest::default();
        let waits = (0..REST_VSYNCS).filter(|_| busy.take_vsync(false)).count();
        assert_eq!(waits as u32, REST_VSYNCS - 1, "a block that never rests is given stock's ten vsyncs, no more");
    }

    #[test]
    fn stop_registers() {
        let mut hw = after_start();
        disarm(&mut hw);
        assert_eq!(shut(&mut hw), Ok(()));
        let expected = [
            (MODULE_EN, 0x4000_0000),
            (FMT_SEL, 0),
            (INT_EN, 0),
            (CLK_EN, 0),
            (FBC_CTL1, 0),
            (SEN_MODE, 0x0001_0002),
            (FH_EN, 0),
            (SW_CTL, 0),
        ];
        for (offset, value) in expected {
            assert_eq!(hw.at(Block::Camsv, offset), value, "register {offset:#x}");
        }
    }
}

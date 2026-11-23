//! The device's hardware as data: register blocks and what is decided about each part.

pub mod camera;
pub mod power;
pub mod projector;
pub mod rate;
pub mod slots;
pub mod sound;
pub mod uevent;
pub mod usb;
pub mod vdec;
pub mod venc;

use alloc::format;
use alloc::string::String;

/// How long the kernel waits for another holder of the codec block, as stock asks.
pub const LOCK_TIMEOUT_MS: u32 = 1000;
/// The chip's cores in work order: the four small ones, cheapest per job, then the four large ones.
pub const CORES: [usize; 8] = [4, 5, 6, 7, 0, 1, 2, 3];
// How many of them are small.
const SMALL: usize = 4;
/// The small cores, for a call's work beside its picture's coding: the far side's pictures decoded.
pub const SMALL_CORES: &[usize] = CORES.split_at(SMALL).0;
/// The large cores, for work that takes whole cores for as long as it runs: a call's picture coded.
pub const LARGE_CORES: &[usize] = CORES.split_at(SMALL).1;
/// The parts a job is cut into for a crew that works on every core.
pub const PARTS: usize = CORES.len();
/// The parts a picture for the screen is drawn in from a frame of a viewfinder or a played video: one a small core.
pub const FRAME_PARTS: usize = SMALL;
/// The load the scheduler counts a core's whole capacity as (the kernel's `SCHED_CAPACITY_SCALE`): asked of a core, its top clock.
pub const FULL_LOAD: u32 = 1024;
/// No load asked of a core: its clock is the governor's.
pub const GOVERNED: u32 = 0;
// The large cores' top clock in kHz, by the kernel's table for the R1's MT6765X (`CPU_DVFS_FREQ0_L_C65X`).
const LARGE_TOP_KHZ: u64 = 2_200_000;
// schedutil asks a quarter more clock than a load takes, in 1024ths: MediaTek's `capacity_margin` (`map_util_freq_with_margin`).
const MARGIN: u64 = 1280;
// The large cores' fastest step at 0.85 V: the steps above climb to 1.2 V on the rail all eight cores share, a quarter more clock for twice the energy a cycle (`CPU_DVFS_FREQ8_L_C65X`, `CPU_DVFS_VOLT8_VPROC_L_C65X`).
const CODING_KHZ: u64 = 1_750_000;
/// The load a thread coding a call's picture asks of its large core while it codes: the most schedutil clocks at `CODING_KHZ`.
pub const CODING_LOAD: u32 = load_within(CODING_KHZ);

// The clock in kHz schedutil asks of the large cores for a load, in the kernel's integer steps; the cluster runs at the lowest step at or above it.
const fn to_large_khz(load: u32) -> u64 {
    LARGE_TOP_KHZ * load as u64 / FULL_LOAD as u64 / FULL_LOAD as u64 * MARGIN
}

const fn load_within(khz: u64) -> u32 {
    let mut load = FULL_LOAD;
    while to_large_khz(load) > khz {
        load -= 1;
    }
    load
}

/// The register blocks the camera path, the video encoder and the video decoder work on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Block {
    Seninf,
    // The analog MIPI receivers.
    Analog,
    Camsv,
    // What the video codecs share.
    Codec,
    Venc,
    Vdec,
}

/// The 32-bit address a block reaches a buffer of `bytes` at, from the kernel's `address` for `length` bytes; none where that is nought, for another length or past 32 bits.
pub fn device_address(address: u64, length: u64, bytes: u64) -> Option<u32> {
    let end = address.checked_add(bytes)?;
    (address != 0 && length == bytes && end <= u64::from(u32::MAX)).then_some(address as u32)
}

/// The error text of a kernel address of no use.
pub fn to_no_address_text(address: u64, length: u64) -> String {
    format!("device address {address:#x} for {length:#x} bytes is of no use")
}

/// The mapping of a register of `block` and its offset there; each of `blocks` is a block with its mapping, start and bytes.
pub fn locate(blocks: &[(Block, usize, usize, usize)], block: Block, offset: usize) -> (usize, usize) {
    let entry = blocks.iter().find(|&&(entry_block, _, _, bytes)| entry_block == block && offset < bytes);
    let &(_, mapping, start, _) = entry.unwrap_or_else(|| panic!("register {offset:#x} of {block:?}"));
    (mapping, start + offset)
}

/// Mapped registers and the passing of time.
pub trait Hardware {
    fn read(&mut self, block: Block, offset: usize) -> u32;

    fn write(&mut self, block: Block, offset: usize, value: u32);

    // Lets at least `micros` microseconds pass.
    fn pause(&mut self, micros: u32);

    fn update(&mut self, block: Block, offset: usize, clear: u32, set: u32) {
        let value = self.read(block, offset);
        self.write(block, offset, value & !clear | set);
    }
}

fn poll<H: Hardware>(
    hw: &mut H,
    (block, offset): (Block, usize),
    (pauses, pause): (u32, u32),
    is_done: impl Fn(u32) -> bool,
) -> bool {
    for _ in 0..pauses {
        if is_done(hw.read(block, offset)) {
            return true;
        }
        hw.pause(pause);
    }
    is_done(hw.read(block, offset))
}

#[cfg(test)]
fn last_write(writes: &[(usize, u32)], offset: usize) -> u32 {
    writes
        .iter()
        .rev()
        .find(|write| write.0 == offset)
        .unwrap_or_else(|| panic!("register {offset:#x} is not written"))
        .1
}

#[cfg(test)]
mod fake {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;

    const CAMSV_SW_CTL: usize = 0x20;
    const RESET_TRIGGER: u32 = 1;
    const RESET_DONE: u32 = 3;

    #[derive(Default)]
    pub(super) struct Fake {
        pub(super) memory: BTreeMap<(Block, usize), u32>,
        pub(super) writes: Vec<(Block, usize, u32)>,
        pub(super) pause_micros: u64,
        pub(super) is_stuck: bool,
    }

    impl Fake {
        pub(super) fn with(values: &[(Block, usize, u32)]) -> Fake {
            Fake {
                memory: values.iter().map(|&(block, offset, value)| ((block, offset), value)).collect(),
                ..Fake::default()
            }
        }

        pub(super) fn at(&self, block: Block, offset: usize) -> u32 {
            self.memory.get(&(block, offset)).copied().unwrap_or(0)
        }
    }

    impl Hardware for Fake {
        fn read(&mut self, block: Block, offset: usize) -> u32 {
            self.at(block, offset)
        }

        fn write(&mut self, block: Block, offset: usize, value: u32) {
            let is_reset = (block, offset) == (Block::Camsv, CAMSV_SW_CTL) && value == RESET_TRIGGER && !self.is_stuck;
            self.memory.insert((block, offset), if is_reset { RESET_DONE } else { value });
            self.writes.push((block, offset, value));
        }

        fn pause(&mut self, micros: u32) {
            self.pause_micros += u64::from(micros);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;

    #[test]
    fn update_masks_bits() {
        let mut hw = Fake::with(&[(Block::Seninf, 0x24, 0x0000_0701)]);
        hw.update(Block::Seninf, 0x24, 0x700, 0x8000_0200);
        assert_eq!(hw.at(Block::Seninf, 0x24), 0x8000_0201);
        assert_eq!(hw.writes, [(Block::Seninf, 0x24, 0x8000_0201)]);
    }

    #[test]
    fn locate_in_mapping() {
        let blocks = [(Block::Codec, 0, 0, 0x1000), (Block::Vdec, 0, 0x4_0000, 0x6000), (Block::Camsv, 1, 0, 0x1000)];
        assert_eq!(
            [locate(&blocks, Block::Codec, 4), locate(&blocks, Block::Vdec, 0x5FFC), locate(&blocks, Block::Camsv, 0)],
            [(0, 4), (0, 0x4_5FFC), (1, 0)]
        );
    }

    #[test]
    #[should_panic(expected = "register 0x1000 of Codec")]
    fn locate_past_block() {
        locate(&[(Block::Codec, 0, 0, 0x1000), (Block::Vdec, 0, 0x4_0000, 0x6000)], Block::Codec, 0x1000);
    }

    #[test]
    fn buffer_address() {
        assert_eq!(device_address(0x0250_0000, 0x1000, 0x1000), Some(0x0250_0000));
        assert_eq!(device_address(0xFFFF_F000, 0xFFF, 0xFFF), Some(0xFFFF_F000), "up to the last address 32 bits name");
        assert_eq!(
            [device_address(0, 0x1000, 0x1000), device_address(0x1000, 0x800, 0x1000)],
            [None, None],
            "none at nought, nor for another length than the buffer's"
        );
        assert_eq!(
            [device_address(0xFFFF_F000, 0x1000, 0x1000), device_address(u64::MAX, 1, 1)],
            [None, None],
            "nor one that reaches past 32 bits"
        );
        assert_eq!(to_no_address_text(0, 0x1000), "device address 0x0 for 0x1000 bytes is of no use");
    }

    #[test]
    fn coding_load_clock() {
        // The large cores' step below the coding step (`CPU_DVFS_FREQ9_L_C65X`).
        const BELOW_KHZ: u64 = 1_617_000;
        let coding = to_large_khz(CODING_LOAD);
        assert!(
            BELOW_KHZ < coding && coding <= CODING_KHZ,
            "the coding load takes the large cores to the coding step: {coding} kHz asked"
        );
        assert!(to_large_khz(CODING_LOAD + 1) > CODING_KHZ, "and is the most load that stays there");
        assert!(to_large_khz(FULL_LOAD) >= LARGE_TOP_KHZ, "the full load asks the top clock");
        assert_eq!(to_large_khz(GOVERNED), 0, "and none asks nothing");
    }
}

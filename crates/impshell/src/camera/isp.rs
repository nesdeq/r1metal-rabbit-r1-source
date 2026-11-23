use crate::media::ion::Ion;
use crate::sys::{self, Registers};
use r1_funcore::hw::Block;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};

const DEVICE: &str = "/dev/camera-isp";
const WAIT_IRQ: u32 = sys::ioc(sys::IOC_WRITE, b'k', 3, size_of::<Wait>());
const CLEAR_IRQ: u32 = sys::ioc(sys::IOC_WRITE, b'k', 4, size_of::<Clear>());
const BUFFER_CTRL: u32 = sys::ioc(sys::IOC_BOTH, b'k', 6, size_of::<BufferControl>());
const DEBUG_FLAG: u32 = sys::ioc(sys::IOC_WRITE, b'k', 8, size_of::<usize>());
const GET_INT_ERR: u32 = sys::ioc(sys::IOC_READ, b'k', 17, size_of::<usize>());
const VF_LOG: u32 = sys::ioc(sys::IOC_WRITE, b'k', 29, size_of::<usize>());
const LARB_MMU_CTL: u32 = sys::ioc(sys::IOC_WRITE, b'k', 36, size_of::<LarbWord>());
const CAMSV0_IRQ: u32 = 3;
const IRQ_TYPES: usize = 10;
const CAMSV0_DEVICE: u32 = 6;
const CLEAR_THEN_WAIT: u32 = 1;
const BOOKKEEPING_CLEAR: u32 = 1;
const VIEW_FINDER_ON: u32 = 11;
const VIEW_FINDER_OFF: u32 = 10;
const START_CLEAR: u32 = 0x4000_0000;
const CAMERA_LARB: u32 = 3;
const LARB_PORTS: u32 = 21;
const PORT_CONTROL: u32 = 0x380;
const PORT_CONTROL_STEP: u32 = 4;
const TRANSLATE: u32 = 1;
const BLOCKS: [(Block, libc::off_t, usize); 3] =
    [(Block::Seninf, 0x1a04_0000, 0x8000), (Block::Analog, 0x11c1_0000, 0x6000), (Block::Camsv, 0x1a05_0000, 0x1000)];

#[repr(C)]
#[derive(Default)]
struct Wait {
    kind: u32,
    dump_registers: u32,
    clear: u32,
    status_type: u32,
    status: u32,
    user_key: i32,
    timeout_ms: u32,
    time_info: [u32; 7],
}

#[repr(C)]
struct Clear {
    kind: u32,
    user_key: i32,
    status_type: u32,
    status: u32,
}

#[repr(C)]
#[derive(Default)]
struct BufferControl {
    control: u32,
    module: u32,
    buffer_id: u32,
    data: u64,
    ex_data: u64,
    extend: u64,
}

#[repr(C)]
struct LarbWord {
    larb: u32,
    offset: u32,
    value: u32,
}

pub(super) struct Isp {
    file: File,
    pub(super) registers: Registers,
}

impl Isp {
    pub(super) fn open() -> io::Result<Isp> {
        let file = OpenOptions::new().read(true).write(true).open(DEVICE)?;
        let map_block =
            |registers: Registers, (block, at, bytes)| registers.map(file.as_raw_fd(), at, &[(block, 0, bytes)]);
        let isp = Isp { registers: BLOCKS.into_iter().try_fold(Registers::default(), map_block)?, file };
        let mut debug_off = 0u32;
        unsafe { sys::ioctl(isp.file.as_raw_fd(), DEBUG_FLAG, &mut debug_off)? };
        Ok(isp)
    }

    pub(super) fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    pub(super) fn ion(&self) -> io::Result<Ion> {
        Ion::open(self.fd())
    }

    pub(super) fn translate(&self) -> io::Result<()> {
        for port in 0..LARB_PORTS {
            let mut word =
                LarbWord { larb: CAMERA_LARB, offset: PORT_CONTROL + PORT_CONTROL_STEP * port, value: TRANSLATE };
            unsafe { sys::ioctl(self.file.as_raw_fd(), LARB_MMU_CTL, &mut word)? };
        }
        Ok(())
    }

    pub(super) fn clear_bookkeeping(&self) -> io::Result<()> {
        let mut control = BufferControl { control: BOOKKEEPING_CLEAR, module: CAMSV0_IRQ, ..BufferControl::default() };
        unsafe { sys::ioctl(self.file.as_raw_fd(), BUFFER_CTRL, &mut control) }
    }

    pub(super) fn view_finder(&self, is_on: bool) -> io::Result<()> {
        if is_on {
            let mut clear = Clear { kind: CAMSV0_IRQ, user_key: 0, status_type: 0, status: START_CLEAR };
            unsafe { sys::ioctl(self.file.as_raw_fd(), CLEAR_IRQ, &mut clear)? };
        }
        let mut words = [if is_on { VIEW_FINDER_ON } else { VIEW_FINDER_OFF }, CAMSV0_DEVICE, 0];
        unsafe { sys::ioctl(self.file.as_raw_fd(), VF_LOG, words.as_mut_ptr()) }
    }

    pub(super) fn wait(&self, status: u32, timeout_ms: u32) -> io::Result<bool> {
        let mut wait = Wait { kind: CAMSV0_IRQ, clear: CLEAR_THEN_WAIT, status, timeout_ms, ..Wait::default() };
        match unsafe { sys::ioctl(self.file.as_raw_fd(), WAIT_IRQ, &mut wait) } {
            Ok(()) => Ok(true),
            Err(e) if e.raw_os_error() == Some(libc::EFAULT) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub(super) fn errors(&self) -> io::Result<u32> {
        let mut words = [0u32; IRQ_TYPES];
        unsafe { sys::ioctl(self.file.as_raw_fd(), GET_INT_ERR, words.as_mut_ptr())? };
        Ok(words[CAMSV0_IRQ as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_layouts() {
        assert_eq!(size_of::<Wait>(), 56);
        assert_eq!(size_of::<Clear>(), 16);
        assert_eq!(size_of::<BufferControl>(), 40);
        assert_eq!(size_of::<LarbWord>(), 12);
        assert_eq!(WAIT_IRQ, 0x4038_6b03);
        assert_eq!(CLEAR_IRQ, 0x4010_6b04);
        assert_eq!(BUFFER_CTRL, 0xc028_6b06);
        assert_eq!(DEBUG_FLAG, 0x4008_6b08);
        assert_eq!(GET_INT_ERR, 0x8008_6b11);
        assert_eq!(VF_LOG, 0x4008_6b1d);
        assert_eq!(LARB_MMU_CTL, 0x400c_6b24);
    }
}

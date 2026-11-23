use crate::sys::{self, Mapping};
use r1_funcore::base::handoff::Latch;
use r1_funcore::gfx::Surface;
use r1_funcore::gfx::panel::{Geometry, Ring, ScreenInfo};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;

const DEVICE: &str = "/dev/fb0";
const FBIOGET_VSCREENINFO: u32 = 0x4600;
const FBIOGET_FSCREENINFO: u32 = 0x4602;
const FBIOPAN_DISPLAY: u32 = 0x4606;
const FBIOBLANK: u32 = 0x4611;
const FB_BLANK_UNBLANK: libc::c_ulong = 0;
const FB_BLANK_POWERDOWN: libc::c_ulong = 4;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct VarScreenInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct FixScreenInfo {
    id: [u8; 16],
    smem_start: libc::c_ulong,
    smem_len: u32,
    kind: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: libc::c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

fn screen_info(var: &VarScreenInfo, fix: &FixScreenInfo) -> ScreenInfo {
    ScreenInfo {
        width: var.xres,
        height: var.yres,
        lines: var.yres_virtual,
        bits_per_pixel: var.bits_per_pixel,
        channels: [var.red, var.green, var.blue].map(|channel| (channel.offset, channel.length)),
        line_bytes: fix.line_length,
        memory: fix.smem_len,
    }
}

pub struct Framebuffer {
    file: File,
    map: Mapping,
    var: VarScreenInfo,
    geometry: Geometry,
    ring: Ring,
    power: Latch<bool>,
}

impl Framebuffer {
    pub fn open() -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(DEVICE)?;
        let fd = file.as_raw_fd();
        let (mut var, mut fix) = (VarScreenInfo::default(), FixScreenInfo::default());
        unsafe {
            sys::ioctl(fd, FBIOGET_VSCREENINFO, &mut var)?;
            sys::ioctl(fd, FBIOGET_FSCREENINFO, &mut fix)?;
        }
        let info = screen_info(&var, &fix);
        let geometry = Geometry::from_info(&info)
            .map_err(|unfit| io::Error::new(io::ErrorKind::Unsupported, unfit.to_text(&info)))?;
        let map = sys::map(fd, 0, fix.smem_len as usize)?;
        let ring = Ring::new(&geometry, var.yoffset);
        Ok(Framebuffer { file, map, var, geometry, ring, power: Latch::with(true) })
    }

    /// The pages as the driver reported them.
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// The frame number the off-screen page shows, if known.
    pub fn back_frame(&self) -> Option<u64> {
        self.ring.back_frame()
    }

    /// The off-screen page, ready to draw on.
    pub fn frame(&mut self) -> Surface<'_> {
        let geometry = self.geometry;
        let (start, words) = geometry.page_words(self.ring.back());
        let pixels = unsafe { std::slice::from_raw_parts_mut(self.map.as_ptr().cast::<u32>().add(start), words) };
        Surface::new(pixels, geometry.width, geometry.height, geometry.stride, geometry.format)
            .expect("geometry was validated against the mapping")
    }

    /// Pans to the drawn page, frame number `frame`, and moves to the next page; returns as the next refresh begins to show it.
    pub fn present(&mut self, frame: u64) -> io::Result<()> {
        self.var.xoffset = 0;
        self.var.yoffset = self.geometry.page_line(self.ring.back());
        self.ring.set_back_frame(frame);
        unsafe { sys::ioctl(self.file.as_raw_fd(), FBIOPAN_DISPLAY, &mut self.var)? };
        self.ring.advance();
        Ok(())
    }

    /// Notes that the drawn page equals the frame on screen, `frame`, so the next page is drawn into.
    pub fn skip_back(&mut self, frame: u64) {
        self.ring.skip_back(frame);
    }

    /// Powers the panel, the touch panel and the wheel up or down; a change the kernel refused is tried again next time.
    pub fn set_power(&mut self, is_on: bool) -> io::Result<()> {
        if self.power.change(is_on).is_none() {
            return Ok(());
        }
        self.ring.forget();
        let mode = if is_on { FB_BLANK_UNBLANK } else { FB_BLANK_POWERDOWN };
        unsafe { sys::ioctl_value(self.file.as_raw_fd(), FBIOBLANK, mode) }
            .map(drop)
            .inspect_err(|_| self.power.forget())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::base::pixel::PixelFormat;

    fn r1_info() -> (VarScreenInfo, FixScreenInfo) {
        let channel = |offset| Bitfield { offset, length: 8, msb_right: 0 };
        let var = VarScreenInfo {
            xres: 480,
            yres: 640,
            xres_virtual: 480,
            yres_virtual: 1920,
            bits_per_pixel: 32,
            red: channel(0),
            green: channel(8),
            blue: channel(16),
            transp: channel(24),
            ..Default::default()
        };
        let fix = FixScreenInfo { line_length: 1920, smem_len: 1920 * 1920, ..Default::default() };
        (var, fix)
    }

    #[test]
    fn screen_info_sizes() {
        assert_eq!(size_of::<VarScreenInfo>(), 160);
        assert_eq!(size_of::<FixScreenInfo>(), 80);
    }

    #[test]
    fn mtkfb_default_mode() {
        let (var, fix) = r1_info();
        let info = screen_info(&var, &fix);
        assert_eq!((info.width, info.height, info.lines, info.bits_per_pixel), (480, 640, 1920, 32));
        assert_eq!((info.channels, info.line_bytes, info.memory), ([(0, 8), (8, 8), (16, 8)], 1920, 1920 * 1920));
        let geometry = Geometry::from_info(&info).unwrap();
        assert_eq!((geometry.pages, geometry.format), (3, PixelFormat { r_shift: 0, g_shift: 8, b_shift: 16 }));
    }
}

use crate::media::codec::{Driver, Lock, Owner};
use crate::media::ion::{Buffer, Ion};
use crate::sys;
use r1_funcore::codec::jpeg::{
    JOB_TIMEOUT_MS, QUALITY, blocks, check_size, file_bytes, file_end, picture_bytes, restart_interval, to_unended_text,
};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};

const DEVICE: &str = "/proc/mtk_jpeg";
const SOURCE_PORT: i32 = 13;
const TARGET_PORT: i32 = 14;
const JPEG_ENC_IOCTL_INIT: u32 = sys::ioc(0, b'x', 11, 0);
const JPEG_ENC_IOCTL_CONFIG: u32 = sys::ioc(sys::IOC_WRITE, b'x', 12, size_of::<Job>());
const JPEG_ENC_IOCTL_WAIT: u32 = sys::ioc(sys::IOC_BOTH, b'x', 13, size_of::<Wait>());
const JPEG_ENC_IOCTL_DEINIT: u32 = sys::ioc(0, b'x', 14, 0);
const JPEG_ENC_IOCTL_START: u32 = sys::ioc(0, b'x', 15, 0);
const NV12: u32 = 2;
const QUALITY_CODE: u32 = 3;
const _: () = assert!(QUALITY == 95, "the encoder's code is that of quality 95");

/// A buffer the encoder reads the planes of a picture of `pixels` from.
pub fn planes_buffer(ion: &Ion, pixels: usize) -> io::Result<Buffer> {
    ion.buffer(picture_bytes(pixels), SOURCE_PORT)
}

/// A buffer the encoder writes the file of a picture of `pixels` to.
pub fn file_buffer(ion: &Ion, pixels: usize) -> io::Result<Buffer> {
    ion.buffer(file_bytes(pixels), TARGET_PORT)
}

#[repr(C)]
#[derive(Default)]
struct Job {
    dst_buffer_addr: u32,
    dst_buffer_size: u32,
    enc_width: u32,
    enc_height: u32,
    enable_exif: u8,
    alloc_buffer: u8,
    enc_quality: u32,
    enc_format: u32,
    disable_gmc: u32,
    restart_interval: u32,
    src_buffer_addr: u32,
    src_chroma_addr: u32,
    img_stride: u32,
    mem_stride: u32,
    total_enc_du: u32,
    dst_buf_addr_offset: u32,
    dst_buf_addr_offset_mask: u32,
    src_fd: i32,
    src_fd2: i32,
    dst_fd: i32,
    mem_height: u32,
}

#[repr(C)]
struct Wait {
    timeout: i64,
    file_size: *mut u32,
    result: *mut u32,
    cycle_count: *mut u32,
}

/// The encoder, powered and ready for a job.
pub struct Jpeg {
    file: File,
    owner: Owner,
}

impl Jpeg {
    /// Takes the encoder, its descriptor numbered above `floor`.
    pub fn open(floor: RawFd) -> io::Result<Jpeg> {
        let file = sys::open_above(DEVICE, floor)?;
        unsafe { sys::ioctl_value(file.as_raw_fd(), JPEG_ENC_IOCTL_INIT, 0)? };
        Ok(Jpeg { file, owner: Owner::default() })
    }

    /// An ION node numbered above the encoder's, so a dying process releases the encoder before it frees the buffers.
    pub fn ion(&self) -> io::Result<Ion> {
        Ion::open(self.file.as_raw_fd())
    }

    fn run(&self, job: &mut Job) -> io::Result<usize> {
        let fd = self.file.as_raw_fd();
        unsafe { sys::ioctl(fd, JPEG_ENC_IOCTL_CONFIG, job)? };
        unsafe { sys::ioctl_value(fd, JPEG_ENC_IOCTL_START, 0)? };
        let (mut size, mut result, mut cycles) = (0u32, 0u32, 0u32);
        let mut wait = Wait {
            timeout: JOB_TIMEOUT_MS.into(),
            file_size: &mut size,
            result: &mut result,
            cycle_count: &mut cycles,
        };
        unsafe { sys::ioctl(fd, JPEG_ENC_IOCTL_WAIT, &mut wait)? };
        Ok(size as usize)
    }

    /// Encodes the square picture of `side` pixels at the start of `source`, luma then chroma, into `target`; returns the file.
    pub fn encode<'t>(&mut self, source: &Buffer, side: usize, target: &'t Buffer) -> io::Result<&'t [u8]> {
        source.flush(0..picture_bytes(side * side))?;
        let side = side as u32;
        let mut job = Job {
            dst_buffer_size: target.len() as u32,
            enc_width: side,
            enc_height: side,
            enc_quality: QUALITY_CODE,
            enc_format: NV12,
            restart_interval: restart_interval(side),
            img_stride: side,
            mem_stride: side,
            total_enc_du: blocks(side, side) - 1,
            src_fd: source.fd(),
            src_fd2: source.fd(),
            dst_fd: target.fd(),
            mem_height: side,
            ..Job::default()
        };
        let lock = Lock::take(&self.owner, Driver::JpegEncoder, self.file.as_raw_fd())?;
        let result = self.run(&mut job);
        let release_result = lock.release();
        let size = result?;
        release_result?;
        let size = check_size(size, target.len()).map_err(io::Error::other)?;
        let file = target.dma_bytes(0..size)?;
        let length = file_end(file).ok_or_else(|| io::Error::other(to_unended_text(file.len())))?;
        Ok(&file[..length])
    }
}

impl Drop for Jpeg {
    fn drop(&mut self) {
        let _ = unsafe { sys::ioctl_value(self.file.as_raw_fd(), JPEG_ENC_IOCTL_DEINIT, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_layouts() {
        assert_eq!(size_of::<Job>(), 80);
        assert_eq!(std::mem::offset_of!(Job, enc_quality), 20);
        assert_eq!(std::mem::offset_of!(Job, src_fd), 64);
        assert_eq!(size_of::<Wait>(), 32);
        assert_eq!(JPEG_ENC_IOCTL_INIT, 0x780b);
        assert_eq!(JPEG_ENC_IOCTL_CONFIG, 0x4050_780c);
        assert_eq!(JPEG_ENC_IOCTL_WAIT, 0xc020_780d);
        assert_eq!(JPEG_ENC_IOCTL_DEINIT, 0x780e);
        assert_eq!(JPEG_ENC_IOCTL_START, 0x780f);
    }
}

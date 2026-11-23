use crate::media::ion::{Buffer, Ion};
use crate::sys::PAGE;
use r1_funcore::domain::camera::check_burst;
use r1_funcore::hw::camera::frame::Frame;
use std::io;

const CAMSV0_PORT: i32 = 43;

// The frames CAMSV0 writes, a stream's or a burst's: a slot for each, and a page for their headers.
pub(super) struct Frames {
    buffer: Buffer,
    frame: Frame,
    slot_bytes: usize,
    slots: usize,
}

impl Frames {
    fn slot_bytes(frame: Frame) -> usize {
        frame.bytes().next_multiple_of(PAGE)
    }

    // The bytes of a buffer of `slots` frame slots and a header page.
    pub(super) fn buffer_bytes(frame: Frame, slots: usize) -> usize {
        slots * Frames::slot_bytes(frame) + PAGE
    }

    // A buffer of `slots` frame slots and a header page, from the camera's own ION, which its node lies under.
    pub(super) fn new(ion: &Ion, frame: Frame, slots: usize) -> io::Result<Frames> {
        let buffer = ion.buffer(Frames::buffer_bytes(frame, slots), CAMSV0_PORT)?;
        Ok(Frames { buffer, frame, slot_bytes: Frames::slot_bytes(frame), slots })
    }

    pub(super) fn address(&self, slot: usize) -> u32 {
        self.buffer.device_address() + (slot * self.slot_bytes) as u32
    }

    pub(super) fn header(&self) -> u32 {
        self.address(self.slots)
    }

    pub(super) const fn frame(&self) -> Frame {
        self.frame
    }

    pub(super) fn bytes(&self, slot: usize) -> io::Result<&[u8]> {
        let start = slot * self.slot_bytes;
        self.buffer.dma_bytes(start..start + self.frame.bytes())
    }

    // The frames of a burst of `count`, each in its slot.
    pub(super) fn frames(&self, count: usize) -> io::Result<Vec<&[u8]>> {
        check_burst(count, self.slots).map_err(io::Error::other)?;
        (0..count).map(|slot| self.bytes(slot)).collect()
    }
}

use crate::sys;
use r1_funcore::hw::camera::swivel::{self, Step};
use std::fs;
use std::io;
use std::thread;

const ANGLE: &str = "/sys/devices/platform/step_motor_ms35774/orientation";

#[derive(Default)]
pub(super) struct Swivel(());

pub(super) struct Rotation<'s> {
    _swivel: &'s mut Swivel,
    motion: swivel::Motion,
}

impl Swivel {
    pub(super) fn begin(&mut self, target: u32) -> io::Result<Rotation<'_>> {
        let angle = sys::read_number(ANGLE)?;
        if let Some(order) = swivel::to_order(angle, target) {
            fs::write(ANGLE, order)?;
        }
        Ok(Rotation { _swivel: self, motion: swivel::Motion::new(angle, target, sys::monotonic()) })
    }
}

impl Rotation<'_> {
    fn settle(&mut self) -> io::Result<()> {
        while !self.motion.is_reached() {
            match self.motion.next_step(sys::read_number(ANGLE)?, sys::monotonic()) {
                Step::Reached => {}
                Step::Wait(poll) => thread::sleep(poll),
                Step::Late(text) => return Err(io::Error::other(text)),
            }
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> io::Result<()> {
        self.settle()
    }
}

impl Drop for Rotation<'_> {
    fn drop(&mut self) {
        let _ = self.settle();
    }
}

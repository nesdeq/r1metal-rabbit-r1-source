use crate::sync::Board;
use crate::{log, sys};
use r1_funcore::base::handoff::{Latch, Orders, Step};
use r1_funcore::base::text::switch_word;
use r1_funcore::input::hub::{RECORD_BYTES, RECORDS_PER_READ, to_switch_writes};
use r1_funcore::input::pose::{Pose, Posture};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;

const NODE: &str = "/dev/m_acc_misc";

fn switch(board: &Board<Orders<bool>>) {
    while let Step::Serve(is_on) = board.wait(Orders::next_step) {
        if let Err(e) = to_switch_writes(is_on).into_iter().try_for_each(|(file, text)| fs::write(file, text)) {
            log!("accelerometer: switching {}: {e}", switch_word(is_on));
        }
    }
}

/// The accelerometer's queue and its switching thread; dropping it switches the sensor off.
pub struct Tilt {
    node: File,
    board: Arc<Board<Orders<bool>>>,
    power: Latch<bool>,
    /// The newest reading and the smoothed gravity in the screen's plane.
    pub posture: Posture,
}

impl Tilt {
    /// Opens the queue and starts the switching thread; the sensor stays off.
    pub fn open() -> io::Result<Tilt> {
        let node = File::open(NODE)?;
        let board = Arc::new(Board::new(Orders::default())?);
        let worker = Arc::clone(&board);
        sys::spawn_background("accelerometer", move || switch(&worker))?;
        Ok(Tilt { node, board, power: Latch::with(false), posture: Posture::default() })
    }

    fn order(&self, is_on: bool) {
        self.board.lock().add(is_on);
        self.board.notify();
    }

    /// Switches the sensor; true on a change, from which the readings begin anew.
    pub fn want(&mut self, is_on: bool) -> bool {
        if self.power.change(is_on).is_none() {
            return false;
        }
        if is_on {
            // What the queue still holds was measured before the sensor was last switched off.
            if let Err(e) = self.drain() {
                log!("accelerometer: emptying its queue: {e}");
            }
            self.posture = Posture::default();
        }
        self.order(is_on);
        true
    }

    /// Readable while readings wait.
    pub fn fd(&self) -> RawFd {
        self.node.as_raw_fd()
    }

    fn drain(&mut self) -> io::Result<()> {
        let mut records = [0u8; RECORDS_PER_READ * RECORD_BYTES];
        loop {
            match (&self.node).read(&mut records) {
                Ok(0) => return Ok(()),
                Ok(count) => self.posture.take_records(&records[..count]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Takes the waiting readings; returns the pose if it changed, and whether the device was picked up.
    pub fn read(&mut self) -> io::Result<(Option<Pose>, bool)> {
        self.drain()?;
        Ok(self.posture.take_changes())
    }

    fn close(&mut self) {
        if self.power.change(false).is_some() {
            self.order(false);
        }
        self.board.lock().close();
        self.board.notify();
    }
}

impl Drop for Tilt {
    fn drop(&mut self) {
        self.close();
    }
}

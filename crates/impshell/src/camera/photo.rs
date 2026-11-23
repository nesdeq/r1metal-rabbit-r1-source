use super::call::Sender;
use super::frames::Frames;
use super::swivel::Swivel;
use super::{Rig, Shared, Stats, Stop, scan};
use crate::log;
use crate::storage::{InDirectory, stem_now};
use crate::sys;
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::base::sampling::Turn;
use r1_funcore::domain::camera::{
    self, Job, Outcome, Sight, Supply, Taken, UNSUPPLIED_TEXT, View, Viewfinder, to_burst_count,
};
use r1_funcore::hw::camera::develop::MODE;
use r1_funcore::hw::camera::exposure::{Auto, MOST_FRAMES};
use r1_funcore::hw::camera::frame::Frame;
use r1_funcore::hw::camera::raw::Finder;
use r1_funcore::hw::camera::stream::burst_slots;
use r1_funcore::storage::PICTURES;
use std::io;
use std::time::Instant;

fn photo_name(shared: &Shared) -> io::Result<String> {
    let state = shared.board.lock();
    stem_now(state.darkroom.names(InDirectory(&shared.data, PICTURES)))
}

impl Rig<'_> {
    pub(super) fn run_photos(
        &mut self,
        shared: &Shared,
        swivel: &mut Swivel,
        (viewfinder, format): (Viewfinder, PixelFormat),
        auto: &mut Auto,
        slots: &mut Option<Frames>,
    ) -> Result<Stats, Stop> {
        let (slots, finder) = self.bring_up(swivel, (viewfinder, format), auto, slots)?;
        let frame = slots.frame();
        let mut stats = Stats::default();
        loop {
            self.start_sensor(frame)?;
            let view = self.show_until_click(shared, (viewfinder, format), slots, &finder, auto, &mut stats);
            self.halt();
            match view? {
                View::Leave => return Ok(stats),
                View::Shoot(turn, review) => {
                    // The screen is told that the stream stops for the photo, which it has no picture of until the stream begins anew.
                    shared.note_shooting();
                    if let Err(e) = self.take_photo(shared, viewfinder, (turn, review), auto, &mut stats) {
                        log!("camera: photo: {e}");
                        shared.set_outcome(Outcome::NoPhoto);
                    }
                }
                View::Show | View::Pause => {}
            }
            self.rest()?;
            if shared.board.wait(|state| state.after_rest(viewfinder)) == View::Leave {
                return Ok(stats);
            }
            self.configure(viewfinder.mode(), auto.exposure(), auto.pace())?;
            auto.restart(viewfinder.pace());
        }
    }

    fn show_until_click(
        &mut self,
        shared: &Shared,
        (viewfinder, format): (Viewfinder, PixelFormat),
        slots: &Frames,
        finder: &Finder,
        auto: &mut Auto,
        stats: &mut Stats,
    ) -> Result<View, Stop> {
        let mut stream = self.start_stream(slots, finder, auto)?;
        let mut sender = None;
        loop {
            let view = shared.board.lock().next_view(viewfinder, format, stream.frame_count());
            if view != View::Show {
                return Ok(view);
            }
            let Some(full) = self.next_full(&mut stream, stats)? else {
                continue;
            };
            let drawn = self.draw_frame(shared, &mut stream, full.slot, stats)?;
            if viewfinder.is_scanning {
                let (picture, width) = stream.picture();
                scan::offer(&shared.board, picture, width, format);
            }
            stream.show(shared, viewfinder);
            // The owner has this side's picture first; the far side's is made of the same frame while the call wants them and its rate lets.
            let at_ns = sys::boottime_ns();
            let wish = shared.board.lock().take_picture_wish(viewfinder, at_ns);
            if let Some(stamp) = wish {
                let sender = match &mut sender {
                    Some(sender) => sender,
                    slot => slot.insert(Sender::new(viewfinder.mode(), stream.tone())?),
                };
                sender.send(shared, self.crew(), drawn.raw, at_ns, stamp);
            }
            if self.requeue(&mut stream, drawn, auto, stats)?
                && let Some(sender) = &mut sender
            {
                sender.planes.set_tone(stream.tone());
            }
        }
    }

    // A buffer for a burst of a photo of `viewfinder`: one a developed burst left, a new one while the bursts that wait are fewer than the budget, else the first one given back, the sensor resting meanwhile; an error once the viewfinder is left before one is.
    fn frames(&mut self, shared: &Shared, viewfinder: Viewfinder, frame: Frame) -> io::Result<Frames> {
        let budget = to_burst_count(Frames::buffer_bytes(frame, MOST_FRAMES));
        let supply = shared.board.lock().supply(viewfinder, budget);
        let supply = match supply {
            Some(supply) => supply,
            None => {
                self.rest()?;
                shared.board.wait(|state| state.supply(viewfinder, budget))
            }
        };
        match supply {
            Supply::Spare(frames) => Ok(frames),
            Supply::Make => {
                Frames::new(self.ion(), frame, MOST_FRAMES).inspect_err(|_| shared.board.lock().darkroom.note_unmade())
            }
            Supply::Leave => Err(io::Error::other(UNSUPPLIED_TEXT)),
        }
    }

    // Takes a burst turned by `turn` and leaves it to the darkroom, which saves its photo and shows it in a review as `review` tells, or, with none, hands it to the robot alone.
    fn take_photo(
        &mut self,
        shared: &Shared,
        viewfinder: Viewfinder,
        (turn, review): (Turn, Option<Sight>),
        auto: &Auto,
        stats: &mut Stats,
    ) -> io::Result<()> {
        let (start, clicked_ns) = (Instant::now(), sys::monotonic_ns());
        let photo = review.map(|sight| photo_name(shared).map(|name| Taken { name, sight })).transpose()?;
        let (frame, still) = (MODE.frame(), auto.still());
        let frames = self.frames(shared, viewfinder, frame)?;
        let addresses: Vec<u32> = burst_slots(still.frames).map(|slot| frames.address(slot)).collect();
        let captured = self
            .configure(MODE, still.exposure, still.pace)
            .and_then(|()| self.start_sensor(frame))
            .and_then(|()| self.capture(frame, &addresses, frames.header()));
        if let Err(e) = captured {
            shared.board.lock().darkroom.keep(frames);
            return Err(e);
        }
        let name = camera::to_log_name(photo.as_ref());
        if let Some(taken) = &photo {
            shared.set_taken(&taken.name);
        }
        camera::to_taken_lines(name, &still, turn.quarters, start.elapsed(), auto)
            .iter()
            .for_each(|line| log!("{line}"));
        stats.note_photo();
        stats.to_photo_zone_lines(name).for_each(|line| log!("{line}"));
        shared.board.lock().darkroom.put(Job::new(frames, still, turn, photo, clicked_ns));
        shared.board.notify();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Data;

    #[test]
    fn photo_name_needs_partition() {
        let shared = Shared::new(Data::unusable()).unwrap();
        assert!(photo_name(&shared).is_err(), "without a partition there is no telling which names are taken");
    }
}

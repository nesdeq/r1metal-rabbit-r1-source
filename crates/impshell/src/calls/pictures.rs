use super::Shared;
use crate::crew::Crew;
use crate::{log, sys};
use r1_funcore::domain::calls::Sender;
use r1_funcore::domain::calls::state::Coding;
use r1_funcore::domain::camera::CallPicture;
use r1_funcore::hw::{CODING_LOAD, GOVERNED, LARGE_CORES};
use std::time::Instant;

pub(super) const TAG: &str = "call pictures";

fn ask_clock(load: u32) {
    if let Err(e) = sys::set_clock_floor(load) {
        log!("{TAG}: the clock floor: {e}");
    }
}

// The pictures' thread: codes each picture of the camera's the board hands it by the encoder's steps, each step's work on a crew of the large cores and its time noted, hands its frame to the calls' thread, and lets go of the encoder once no picture goes.
pub(super) fn run(shared: &Shared) {
    // The crew's threads keep the load the thread asks as they start, and run for a picture's parts alone.
    ask_clock(CODING_LOAD);
    let crew = Crew::on_cores(TAG, LARGE_CORES);
    ask_clock(GOVERNED);
    let (mut sender, mut picture, mut steps) = (Sender::default(), CallPicture::default(), Vec::new());
    loop {
        match shared.board.wait(|state| state.next_picture(&mut picture)) {
            Coding::Rest => (sender, picture) = (Sender::default(), CallPicture::default()),
            Coding::Code { rate, is_key } => {
                // Every change of the board wakes the thread, so it asks the load for a picture's coding alone.
                ask_clock(CODING_LOAD);
                let begun = Instant::now();
                steps.clear();
                if let Some(encoder) = sender.begin(&picture, rate, is_key) {
                    encoder.code_steps(|step, work| {
                        let stepped = Instant::now();
                        crew.run(work);
                        steps.push((step, stepped.elapsed()));
                    });
                }
                if let Some(frame) = sender.finish() {
                    shared.board.lock().add_frame(frame, begun.elapsed(), &steps);
                    shared.nudge.raise();
                }
                ask_clock(GOVERNED);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Board;
    use crate::sys::{Epoll, Wake};
    use r1_funcore::base::pixel::PixelFormat;
    use r1_funcore::domain::calls::state::State;
    use r1_funcore::domain::camera::CALL_SIZE;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn picture_coded_and_handed_on() {
        let shared = Arc::new(Shared {
            board: Board::new(State::new(PixelFormat::XRGB8888)).unwrap(),
            nudge: Wake::new().unwrap(),
        });
        let worker = Arc::clone(&shared);
        thread::spawn(move || run(&worker));
        let pixels = CALL_SIZE.0 * CALL_SIZE.1;
        let luma = (0..pixels).map(|at| (at % CALL_SIZE.0 / 3 + at / CALL_SIZE.0 / 2) as u8).collect();
        let mut picture =
            CallPicture { planes: [luma, vec![120; pixels / 4], vec![140; pixels / 4]], ..CallPicture::default() };
        assert!(shared.board.lock().set_video(Some(300_000), true), "a picture goes to the far side");
        shared.board.lock().add_picture(&mut picture);
        shared.board.notify();
        let mut epoll = Epoll::new().unwrap();
        epoll.add(shared.nudge.fd(), 7).unwrap();
        assert_eq!(
            epoll.wait(Some(Duration::from_secs(30))).unwrap(),
            [7],
            "the calls' thread is nudged for its frame"
        );
        let state = shared.board.lock();
        assert!(
            !state.is_off() && state.wants_picture(),
            "a frame waits for the calls' thread, the next picture wanted"
        );
    }
}

use super::Shared;
use crate::{log, sys};
use r1_funcore::domain::calls::Pictures;
use r1_funcore::domain::calls::state::{Decoding, State, Told};
use r1_funcore::gfx::Picture;
use r1_funcore::hw::SMALL_CORES;
use std::time::Instant;

pub(super) const TAG: &str = "far pictures";

// The far pictures' thread: decodes each far frame the board hands it, on the small cores beside the encoder's, makes its picture in one the board swaps with the screen's, hands that to the main loop or its flaw to the calls' thread, and lets go of the decoders once no call stands.
pub(super) fn run(shared: &Shared) {
    if let Err(e) = sys::pin_to_cores(SMALL_CORES) {
        log!("calls: the far pictures' thread is not kept on its cores: {e}");
    }
    let (mut pictures, mut picture) = (Pictures::default(), Picture::default());
    loop {
        match shared.board.wait(State::next_arrival) {
            Decoding::Rest => (pictures, picture) = (Pictures::default(), Picture::default()),
            Decoding::Decode { arrival, room, format } => {
                let begun = Instant::now();
                let decoded = pictures.decode(arrival);
                let between = Instant::now();
                let shown = decoded.show(room, format, &mut picture);
                let told = shared.board.lock().add_shown(shown, &mut picture, between - begun, between.elapsed());
                match told {
                    Told::Screen => shared.board.raise(),
                    Told::Calls => shared.nudge.raise(),
                    Told::Nobody => {}
                }
            }
        }
    }
}

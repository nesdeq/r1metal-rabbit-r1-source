use super::Shared;
use crate::crew::Crew;
use r1_funcore::base::planes::{self, part_lines};
use r1_funcore::domain::camera::CallPicture;
use r1_funcore::hw::PARTS;
use r1_funcore::hw::camera::mode::Mode;
use r1_funcore::hw::camera::tone::Tone;
use r1_funcore::hw::camera::video::{Chroma, NO_PLANES, Planes, Scratch};
use std::io;

// A call's pictures for the far side out of a stream's raw frames, each frame's planes at its own size, Y, U and V apart.
pub(super) struct Sender {
    // Its tone is the stream's, taken anew where a frame changes it.
    pub(super) planes: Planes,
    scratch: [Scratch; PARTS],
    picture: CallPicture,
}

impl Sender {
    pub(super) fn new(mode: Mode, tone: &Tone) -> io::Result<Sender> {
        let planes = Planes::tight(mode.frame(), tone).ok_or_else(|| io::Error::other(NO_PLANES))?;
        let planes = planes.with_rec_601(tone).with_lens(mode.lens());
        Ok(Sender { planes, scratch: Default::default(), picture: CallPicture::default() })
    }

    // Makes the picture of a raw frame that came at `at_ns`, in parts on the crew, and hands it over with the turn that stands it upright and the way the camera looks.
    pub(super) fn send(
        &mut self,
        shared: &Shared,
        crew: &Crew,
        raw: &[u8],
        at_ns: u64,
        (quarters, is_rear): (u8, bool),
    ) {
        let size = self.planes.size();
        {
            let planes = &self.planes;
            let cut = planes::parts_apart(&mut self.picture.planes, size, part_lines(size.1, PARTS));
            let mut pieces = cut.zip(&mut self.scratch);
            let mut parts: [_; PARTS] = std::array::from_fn(|_| pieces.next());
            crew.for_each(&mut parts, |_, piece| {
                if let Some(((lines, luma, [blue, red]), scratch)) = piece {
                    planes.convert(raw, lines.clone(), luma, Chroma::Apart([blue, red]), scratch);
                }
            });
        }
        (self.picture.at_ns, self.picture.quarters, self.picture.is_rear) = (at_ns, quarters, is_rear);
        shared.board.lock().put_call_picture(&mut self.picture);
        shared.board.raise();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Data;
    use r1_funcore::domain::camera::CALL_SIZE;
    use r1_funcore::hw::camera::exposure::Auto;

    #[test]
    fn crew_picture_equals_one_thread() {
        let mode = Mode::HighSpeed;
        let raw: Vec<u8> = (0..mode.frame().bytes()).map(|at| (at * 31 + at / 7) as u8).collect();
        let tone = Tone::new(Auto::live().recipe());
        let shared = Shared::new(Data::unusable()).unwrap();
        let made = |threads: usize| {
            let crew = Crew::new("test", threads).unwrap();
            let mut sender = Sender::new(mode, &tone).unwrap();
            sender.send(&shared, &crew, &raw, 7, (3, false));
            let mut picture = CallPicture::default();
            assert!(shared.board.lock().take_call_picture(&mut picture), "handed over on {threads} threads");
            picture
        };
        let (parted, whole) = (made(PARTS - 1), made(0));
        let sizes = whole.planes.each_ref().map(Vec::len);
        assert_eq!(sizes, [CALL_SIZE.0 * CALL_SIZE.1, CALL_SIZE.0 * CALL_SIZE.1 / 4, CALL_SIZE.0 * CALL_SIZE.1 / 4]);
        assert!(whole.planes[0].iter().any(|sample| *sample != whole.planes[0][0]), "a picture with something in it");
        assert!(whole.planes == parted.planes, "its parts on the crew's threads as on the caller's alone");
        assert_eq!((parted.at_ns, parted.quarters, parted.is_rear), (7, 3, false));
    }
}

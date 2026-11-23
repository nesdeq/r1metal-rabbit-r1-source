//! The projectors that draw a video's stored pictures for the screen or a grid place.

use super::Room;
use crate::base::pixel::PixelFormat;
use crate::base::sampling::Turn;
use crate::codec::h264::read::Sequence;
use crate::gfx::{to_cover, to_fit, turned_size};
use crate::hw::projector::Projector;
use crate::input::pose::Pose;
use alloc::format;
use alloc::string::String;

/// The refusal of a stream's pictures.
pub fn to_unsupported_text(sequence: &Sequence) -> String {
    let (width, height) = sequence.size();
    format!("pictures of {width}x{height}")
}

/// A stream's picture size once turned upright as its file says.
pub fn upright_size(sequence: &Sequence, quarters: u8) -> (usize, usize) {
    let (width, height) = sequence.size();
    turned_size((width as usize, height as usize), quarters)
}

fn to_projector(
    sequence: &Sequence,
    quarters: u8,
    (width, height): (usize, usize),
    format: PixelFormat,
) -> Option<Projector> {
    Projector::new(sequence, Turn { quarters: quarters % 4, is_mirrored: false }, width, height, format)
}

/// The projector for the screen in a pose: as large as the pictures fit, upright for the holder.
pub fn screen_projector(
    sequence: &Sequence,
    quarters: u8,
    format: PixelFormat,
    room: Room,
    pose: Pose,
) -> Option<Projector> {
    let turn = pose.quarters();
    let fit = to_fit(upright_size(sequence, quarters), room.turned_size(turn));
    to_projector(sequence, quarters + turn, turned_size(fit, turn), format)
}

/// The projector for a video's thumbnail: upright and as small as it still covers a place.
pub fn poster_projector(
    sequence: &Sequence,
    quarters: u8,
    format: PixelFormat,
    place: (usize, usize),
) -> Option<Projector> {
    to_projector(sequence, quarters, to_cover(upright_size(sequence, quarters), place), format)
}

#[cfg(test)]
mod tests {
    use super::super::samples::ROOM;
    use super::*;
    use crate::base::sampling::turn_words;
    use crate::codec::h264::read::samples::sized;
    use crate::gfx::Picture;
    use crate::hw::projector::Scratch;
    use crate::hw::vdec::Layout;
    use alloc::vec;
    use alloc::vec::Vec;

    const FORMAT: PixelFormat = PixelFormat::XRGB8888;
    const UNDER_BAR: Room = Room { bar: 97, ..ROOM };

    fn stream(columns: u32, rows: u32, crop: u32) -> Sequence {
        Sequence { crop: [0, 0, 0, crop], ..sized(columns, rows) }
    }

    #[test]
    fn projector_sizes() {
        let recording = stream(80, 45, 0);
        let poses = [Pose::Upright, Pose::RightUp, Pose::UpsideDown, Pose::LeftUp];
        let sizes = |room: Room, quarters: u8| {
            poses.map(|pose| screen_projector(&recording, quarters, FORMAT, room, pose).unwrap().size())
        };
        assert_eq!(
            sizes(ROOM, 1),
            [(360, 640), (480, 270), (360, 640), (480, 270)],
            "recorded upright: the whole screen's height upright, 270 by 480 for whoever holds the device sideways"
        );
        assert_eq!(
            sizes(ROOM, 0),
            [(480, 270), (360, 640), (480, 270), (360, 640)],
            "recorded sideways: 640 by 360 for whoever holds the device sideways"
        );
        assert_eq!(sizes(ROOM, 3), sizes(ROOM, 1));
        assert_eq!(screen_projector(&stream(120, 68, 8), 0, FORMAT, ROOM, Pose::Upright).unwrap().size(), (480, 270));
        assert_eq!(screen_projector(&stream(40, 23, 8), 0, FORMAT, ROOM, Pose::LeftUp).unwrap().size(), (360, 640));
        assert_eq!(
            screen_projector(&stream(20, 12, 12), 0, FORMAT, ROOM, Pose::Upright).unwrap().size(),
            (320, 180),
            "a small video stays as it is"
        );
        assert!(screen_projector(&stream(200, 45, 0), 0, FORMAT, ROOM, Pose::Upright).is_none());
        assert_eq!(to_unsupported_text(&stream(200, 45, 0)), "pictures of 3200x720", "which its refusal names");
        assert_eq!(
            poses.map(|pose| UNDER_BAR.turned_size(pose.quarters())),
            [(480, 543), (640, 383), (480, 543), (640, 383)],
            "the bar lies at the top for whoever holds the device, and takes its height off the room"
        );
        assert_eq!(
            sizes(UNDER_BAR, 0),
            [(480, 270), (360, 640), (480, 270), (360, 640)],
            "a video recorded sideways stands as large under the bar as without it"
        );
        assert_eq!(
            sizes(UNDER_BAR, 1),
            [(305, 543), (383, 215), (305, 543), (383, 215)],
            "one recorded upright gives way to the bar"
        );
        assert_eq!(
            (upright_size(&recording, 0), upright_size(&recording, 1), upright_size(&recording, 3)),
            ((1280, 720), (720, 1280), (720, 1280)),
            "the size the file's turn gives the pictures"
        );
        assert_eq!(
            poster_projector(&recording, 1, FORMAT, (228, 173)).unwrap().size(),
            (228, 405),
            "for the grid, as small as it still covers a place"
        );
    }

    #[test]
    fn picture_turns() {
        let sequence = stream(8, 4, 0);
        let stored: Vec<u8> = (0..Layout::from_sequence(&sequence).unwrap().picture_bytes())
            .map(|at| (at % 251 + at / 4096) as u8)
            .collect();
        let drawn = |quarters: u8, pose: Pose| {
            let projector = screen_projector(&sequence, quarters, FORMAT, ROOM, pose).unwrap();
            let (width, height) = projector.size();
            let mut words = vec![0; width * height];
            projector.draw(&stored, 0..height, &mut words, &mut Scratch::default());
            Picture { words, width }
        };
        let upright = drawn(0, Pose::Upright);
        assert_eq!((upright.width, upright.words.len()), (128, 128 * 64));
        for (quarters, pose, turns) in [
            (1, Pose::Upright, 1),
            (0, Pose::RightUp, 1),
            (1, Pose::RightUp, 2),
            (3, Pose::RightUp, 0),
            (2, Pose::LeftUp, 1),
            (3, Pose::UpsideDown, 1),
        ] {
            let mut expected = Vec::new();
            let turn = Turn { quarters: turns, is_mirrored: false };
            let width = turn_words(&upright.words, upright.width, turn, &mut expected);
            let shown = drawn(quarters, pose);
            assert_eq!(
                (shown.width, shown.words == expected),
                (width, true),
                "{quarters} quarter turns in the file, held {pose:?}"
            );
        }
    }
}

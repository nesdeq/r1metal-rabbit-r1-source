//! The pictures a conversation shows of its photos: which kind of file one is, and the size it is drawn at.

use crate::codec::jpeg::{self, Jpeg};
use crate::gfx::palette::Roles;
use crate::gfx::png::Png;
use crate::gfx::{Picture, to_fit};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

// The longest side of a picture Signal sends at its highest quality (Signal-Android's `PushMediaConstraints`, `LEVEL_3`): a PNG longer is not inflated, and nothing is drawn taller.
const LARGEST_SIDE: usize = 4096;

// A photo's file with the picture it was decoded to, none where it could not be.
pub(crate) type Photo = (String, Option<Picture>);

/// What a photo's file is, as it is drawn.
pub enum Drawn {
    // A JPEG, standing as its EXIF orientation says.
    Jpeg(u8),
    // A PNG this reader reads, no larger than Signal sends.
    Png(Png),
    // Neither.
    Not,
}

/// What a photo's file holds.
pub fn drawn(file: &[u8]) -> Drawn {
    if let Ok(jpeg) = Jpeg::read(file) {
        return Drawn::Jpeg(jpeg.orientation());
    }
    Png::read(file, LARGEST_SIDE).map_or(Drawn::Not, Drawn::Png)
}

/// The area a picture is fitted into to stand `wide` pixels across.
pub const fn area(wide: usize) -> (usize, usize) {
    (wide, LARGEST_SIDE)
}

/// The size a JPEG stored at `size` and oriented so is decoded at to stand `wide` pixels across once turned, never larger than it is.
pub fn decoded_size(size: (usize, usize), orientation: u8, wide: usize) -> (usize, usize) {
    let standing = to_fit(jpeg::shown_size(size, orientation), area(wide));
    jpeg::shown_size(standing, orientation)
}

// How tall a photo told to be of `size` stands where it stands `wide` pixels across, never larger than it is: the place it is given before its picture is decoded; none where no size is told.
pub(crate) fn standing_height(size: (usize, usize), wide: usize) -> Option<usize> {
    (size.0 > 0 && size.1 > 0).then(|| to_fit(size, area(wide)).1)
}

// What the pictures' thread and the main loop share: the photos wanted decoded at a width, those of them still to decode, the one the thread decodes, and the colours a picture is drawn over.
#[derive(Default)]
pub(super) struct State {
    wanted: Option<(Vec<String>, usize)>,
    asked: Option<(VecDeque<String>, usize)>,
    decoding: Option<(String, usize)>,
    pub roles: Roles,
}

impl State {
    // Takes the photos the screen wants decoded, by their files, and the width they stand at, in place of the order that stood, but for the photo the thread decodes at that width, which comes without being asked again; true where that is news for the thread.
    pub(super) fn want(&mut self, wanted: Option<(Vec<String>, usize)>) -> bool {
        if self.wanted == wanted {
            return false;
        }
        let decoding = &self.decoding;
        let is_under_way = |file: &String, wide: usize| {
            decoding.as_ref().is_some_and(|(held, held_wide)| held == file && *held_wide == wide)
        };
        self.asked = wanted
            .clone()
            .map(|(files, wide)| (files.into_iter().filter(|file| !is_under_way(file, wide)).collect(), wide));
        self.wanted = wanted;
        true
    }

    // The next photo to decode, the width it stands at and the colours it is drawn over; none while no order holds one.
    pub(super) fn take_next(&mut self) -> Option<(String, usize, Roles)> {
        let (files, wide) = self.asked.as_mut()?;
        let next = files.pop_front().map(|file| (file, *wide, self.roles));
        if files.is_empty() {
            self.asked = None;
        }
        if let Some((file, wide, _)) = &next {
            self.decoding = Some((file.clone(), *wide));
        }
        next
    }

    // Notes that the picture of the photo the thread decoded was given.
    pub(super) fn note_given(&mut self) {
        self.decoding = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::png::samples::grey_png;

    const BASELINE: &[u8] = include_bytes!("../../../assets/jpeg/colours_420.jpg");

    #[test]
    fn each_wish_asked_once() {
        let mut state = State::default();
        let files = |names: &[&str]| names.iter().map(|name| String::from(*name)).collect::<Vec<_>>();
        let wish = Some((files(&["a.jpg", "b.jpg", "c.jpg"]), 480));
        assert!(!state.want(None) && state.take_next().is_none(), "nothing wanted, nothing asked");
        assert!(state.want(wish.clone()) && !state.want(wish));
        let (file, wide, _) = state.take_next().expect("an order");
        assert_eq!((file.as_str(), wide), ("a.jpg", 480), "one photo at a time");
        assert!(state.want(None) && state.take_next().is_none(), "what is wanted no more is decoded no more");
        assert!(state.want(Some((files(&["a.jpg"]), 400))), "another width is another wish");
        assert_eq!(state.take_next().map(|(file, wide, _)| (file, wide)), Some((String::from("a.jpg"), 400)));
        assert!(state.take_next().is_none(), "once");
    }

    #[test]
    fn photo_under_way_asked_once() {
        let mut state = State::default();
        let wish =
            |names: &[&str], wide| Some((names.iter().map(|name| String::from(*name)).collect::<Vec<_>>(), wide));
        let next = |state: &mut State| state.take_next().map(|(file, wide, _)| (file, wide));
        state.want(wish(&["a.jpg", "b.jpg", "c.jpg"], 480));
        assert_eq!(next(&mut state), Some((String::from("a.jpg"), 480)));
        state.note_given();
        assert_eq!(next(&mut state), Some((String::from("b.jpg"), 480)), "the second is under way");
        assert!(state.want(wish(&["b.jpg", "c.jpg"], 480)), "the first came back, and the wish holds it no more");
        assert_eq!(
            next(&mut state),
            Some((String::from("c.jpg"), 480)),
            "the photo under way is not decoded a second time"
        );
        assert!(state.want(wish(&["b.jpg", "c.jpg"], 400)), "at another width");
        assert_eq!(next(&mut state), Some((String::from("b.jpg"), 400)), "every photo is asked anew");
        state.note_given();
        assert!(state.want(wish(&["b.jpg"], 480)));
        assert_eq!(next(&mut state), Some((String::from("b.jpg"), 480)), "one given is asked where it is wanted again");
    }

    #[test]
    fn decoded_size_fits_room() {
        assert_eq!(decoded_size((1600, 1200), 1, 480), (480, 360), "upright, as wide as the room");
        assert_eq!(
            decoded_size((1600, 1200), 6, 480),
            (640, 480),
            "stored lying, decoded so that it stands 480 across once turned"
        );
        assert_eq!(decoded_size((300, 200), 1, 480), (300, 200), "a small one keeps its size");
        assert_eq!(decoded_size((480, 9000), 1, 480), (218, LARGEST_SIDE), "none taller than Signal sends");
    }

    #[test]
    fn place_before_picture() {
        for size in [(1600, 1200), (1200, 1600), (300, 200), (480, 9000)] {
            assert_eq!(
                standing_height(size, 464),
                Some(decoded_size(size, 1, 464).1),
                "a photo told to be {size:?} is given the height its picture stands at"
            );
        }
        assert_eq!(standing_height((1600, 1200), 464), Some(348));
        assert_eq!([standing_height((0, 0), 464), standing_height((1600, 0), 464)], [None, None], "no size told");
    }

    #[test]
    fn kinds_of_files() {
        assert!(matches!(drawn(BASELINE), Drawn::Jpeg(1)), "a baseline JPEG without EXIF stands upright");
        assert!(matches!(drawn(b"GIF89a"), Drawn::Not));
        assert!(matches!(drawn(b"RIFF0000WEBPVP8 "), Drawn::Not), "a WebP is none");
        let mut progressive = BASELINE.to_vec();
        let at =
            progressive.windows(5).position(|head| head == [0xFF, 0xC0, 0x00, 0x11, 0x08]).expect("a frame header");
        progressive[at + 1] = 0xC2;
        assert!(matches!(drawn(&progressive), Drawn::Not), "a progressive JPEG is none");
        assert_eq!(area(480), (480, LARGEST_SIDE));
    }

    #[test]
    fn png_side_limit() {
        let side = LARGEST_SIDE as u32;
        assert!(matches!(drawn(&grey_png(side, 1)), Drawn::Png(_)));
        assert!(matches!(drawn(&grey_png(1, side)), Drawn::Png(_)));
        assert!(matches!(drawn(&grey_png(side + 1, 1)), Drawn::Not), "wider than Signal sends");
        assert!(matches!(drawn(&grey_png(1, side + 1)), Drawn::Not), "taller");
    }
}

//! The rolls: a roll's request and what stands in its grid.

pub mod feed;
pub mod items;
pub mod player;
pub mod projection;
pub mod roll;
pub mod show;
pub mod state;
pub mod stats;
pub mod stills;

use crate::base::time::Moment;
use crate::base::time::stamp::parse_stamp;
use crate::domain::tally::Tally;
use crate::gfx::store::AsPicture;
use crate::gfx::{Picture, turned_size};
use crate::input::pose::Pose;
use crate::signal::attachment::{self, Enclosure};
use crate::storage::{PICTURES, VIDEOS};
use alloc::format;
use alloc::string::String;
use items::Item;

/// The refusal of a redraw of an item that is no photo of the pixel cam's.
pub const NO_PIXEL_PHOTO: &str = "no photo of the pixel cam's";

fn to_size_text(picture: &Picture) -> String {
    format!("{}x{}", picture.width, picture.words.len() / picture.width.max(1))
}

/// The log line of a thumbnail made in `ms`.
pub fn to_thumbnail_line(name: &str, picture: &Picture, ms: u128) -> String {
    format!("gallery: {name}: a small picture of {} in {ms} ms", to_size_text(picture))
}

/// The log line of a deleted item.
pub fn to_deleted_line(name: &str) -> String {
    format!("gallery: {name} deleted")
}

/// What a viewfinder takes and a roll holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Photo,
    Video,
}

pub(crate) fn quarters(kind: Kind, pose: Pose) -> u8 {
    match kind {
        Kind::Photo => 0,
        Kind::Video => pose.quarters(),
    }
}

/// The room of an item shown large.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Room {
    pub screen: (usize, usize),
    /// The height of the top bar.
    pub bar: usize,
}

impl Room {
    pub(crate) fn turned_size(self, quarters: u8) -> (usize, usize) {
        let (width, height) = turned_size(self.screen, quarters);
        (width, height.saturating_sub(self.bar))
    }

    /// The size of an item shown large.
    pub fn large_size(self, kind: Kind, pose: Pose) -> (usize, usize) {
        self.turned_size(quarters(kind, pose))
    }
}

/// The word beside an item in the grid.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Label {
    #[default]
    None,
    // Its pixel art is still to come.
    Waiting,
    Unreadable,
    // A video's length.
    Seconds(u64),
}

/// An item's small picture for the grid.
#[derive(Clone)]
pub struct Thumbnail {
    pub picture: Picture,
    pub label: Label,
    /// The item's upright size.
    pub size: (usize, usize),
    /// The item's file as the camera took it, by which a stamp's errand names the item.
    pub file: String,
}

impl Thumbnail {
    /// The grid's entry of an item that cannot be read.
    pub fn unreadable(file: &str) -> Thumbnail {
        Thumbnail { picture: Picture::default(), label: Label::Unreadable, size: (0, 0), file: file.into() }
    }
}

impl AsPicture for Thumbnail {
    const IS_REUSABLE: bool = false;

    fn picture(&self) -> &Picture {
        &self.picture
    }
}

/// The gallery's message for a roll's screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Note {
    Empty(Kind),
    Unreadable,
    NotDeleted,
    NoPixels,
}

/// The action of an icon stamped on an item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stamp {
    Trash,
    Redraw,
    // The item goes to a conversation of Signal's.
    Send,
}

/// An item a stamp asked to send on Signal, by its file's name: a photo with its size, a video with its file's bytes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Sending {
    Photo(String, (usize, usize)),
    Video(String, u64),
}

impl Sending {
    // The item as a message of Signal's carries it, by its file from the data partition's root; none for a video larger than Signal's apps send.
    pub(crate) fn to_enclosure(&self) -> Option<Enclosure> {
        match self {
            Sending::Photo(name, size) => Some(Enclosure::photo(format!("{PICTURES}/{name}"), *size)),
            Sending::Video(name, bytes) => {
                attachment::fits(*bytes).then(|| Enclosure::video(format!("{VIDEOS}/{name}")))
            }
        }
    }
}

impl Kind {
    // The actions a roll offers of its items: a photo is deleted, drawn anew or sent, a video deleted or sent.
    pub(crate) const fn stamps(self) -> &'static [Stamp] {
        match self {
            Kind::Photo => &[Stamp::Trash, Stamp::Redraw, Stamp::Send],
            Kind::Video => &[Stamp::Trash, Stamp::Send],
        }
    }
}

impl Stamp {
    /// The note where the action failed.
    pub const fn failure(self) -> Note {
        match self {
            Stamp::Trash => Note::NotDeleted,
            Stamp::Redraw => Note::NoPixels,
            Stamp::Send => Note::Unreadable,
        }
    }
}

/// A stamped icon's request of the gallery.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Errand {
    pub(crate) kind: Kind,
    // The item's file as the camera took it: its place in the roll changes with the roll, under a screen that still shows the roll as it was.
    pub(crate) file: String,
    pub stamp: Stamp,
    // True where the roll showed photos as the camera took them when the icon was touched: a photo goes as it showed.
    pub(crate) is_original: bool,
}

/// The gallery's request while a roll shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RollRequest {
    pub kind: Kind,
    /// The first item of the page; items count from the newest.
    pub page: usize,
    /// A new value where the grid dropped its thumbnails.
    pub round: u32,
    /// The item shown large.
    pub open_item: Option<usize>,
    /// A new value where the wheel left an item shown large.
    pub turn: u32,
    /// True for photos as the camera took them.
    pub is_original: bool,
    /// The thumbnails of a page.
    pub places: usize,
    /// The size a thumbnail covers.
    pub place_size: (usize, usize),
    /// The items wanted before and after the page.
    pub margin: usize,
    pub room: Room,
}

impl RollRequest {
    /// The roll and its item shown large.
    pub fn large_item(&self) -> Option<(Kind, usize)> {
        Some((self.kind, self.open_item?))
    }
}

/// The tally of the rolls for home.
pub fn count_rolls(photos: &[Item], videos: &[Item], now_s: u64) -> Tally {
    let today = Moment::from_seconds(now_s);
    let is_today = |moment: &Moment| (moment.year, moment.month, moment.day) == (today.year, today.month, today.day);
    let photos_today = photos.iter().filter_map(|item| parse_stamp(&item.original)).filter(is_today).count();
    Tally { photos: photos.len(), photos_today, videos: videos.len(), ..Tally::default() }
}

#[cfg(test)]
mod samples {
    use super::{Kind, RollRequest, Room};

    pub(super) const SCREEN: (usize, usize) = (480, 640);
    pub(super) const ROOM: Room = Room { screen: SCREEN, bar: 0 };

    // A grid's request for a roll's first page in its first round, nothing shown large.
    pub(super) fn request(kind: Kind) -> RollRequest {
        RollRequest {
            kind,
            page: 0,
            round: 1,
            open_item: None,
            turn: 0,
            is_original: false,
            places: 6,
            place_size: (228, 173),
            margin: 0,
            room: ROOM,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::samples::{ROOM, SCREEN};
    use super::*;
    use crate::gfx::to_fit;
    use alloc::string::String;

    #[test]
    fn fit_to_turned_screen() {
        let view_size = |pose: Pose| ROOM.turned_size(pose.quarters());
        assert_eq!(to_fit((2048, 2048), view_size(Pose::RightUp)), (480, 480), "a square fits the same either way");
        assert_eq!(
            to_fit((1280, 720), view_size(Pose::LeftUp)),
            (640, 360),
            "a wide picture gains from the device held sideways"
        );
        assert_eq!(to_fit((720, 1280), view_size(Pose::RightUp)), (270, 480));
        assert_eq!(
            (to_fit((854, 480), SCREEN), to_fit((480, 854), SCREEN)),
            ((480, 270), (360, 640)),
            "269.8 pixels count as 270, 359.7 as 360"
        );
        assert_eq!(to_fit((959, 1280), SCREEN), (480, 640), "479.5 pixels across are the screen's 480, never more");
        let poses = [Pose::Upright, Pose::RightUp, Pose::UpsideDown, Pose::LeftUp];
        assert_eq!(poses.map(view_size), [(480, 640), (640, 480), (480, 640), (640, 480)]);
        assert_eq!(
            poses.map(|pose| (ROOM.large_size(Kind::Photo, pose), ROOM.large_size(Kind::Video, pose))),
            poses.map(|pose| (view_size(Pose::Upright), view_size(pose))),
            "a photo stands in the upright room however the device is held, a video turns with it"
        );
        let unreadable = Thumbnail::unreadable("a.jpg");
        assert_eq!(
            (unreadable.picture.words.len(), unreadable.label, unreadable.size, unreadable.file.as_str()),
            (0, Label::Unreadable, (0, 0), "a.jpg"),
            "of a thing that cannot be read, a word alone, and its file all the same"
        );
    }

    #[test]
    fn home_tally() {
        let names =
            ["20261009_235959_original.jpg", "20261010_000000_original.jpg", "20261010_120000_pixel.jpg", "clip.jpg"]
                .map(String::from);
        let photos = items::photos(&names, &[]);
        let videos = items::videos(&["20261010_080000.mp4".into()]);
        let noon = Moment { year: 2026, month: 10, day: 10, weekday: 0, hour: 12, minute: 0, second: 0 }.to_seconds();
        assert_eq!(
            count_rolls(&photos, &videos, noon),
            Tally { photos: 4, photos_today: 2, videos: 1, ..Tally::default() },
            "a photo of the day before and a file of no time are none of today's"
        );
    }

    #[test]
    fn stamp_failure_notes() {
        assert_eq!(
            [Stamp::Trash, Stamp::Redraw, Stamp::Send].map(Stamp::failure),
            [Note::NotDeleted, Note::NoPixels, Note::Unreadable]
        );
        assert_eq!(
            (Kind::Photo.stamps(), Kind::Video.stamps()),
            (&[Stamp::Trash, Stamp::Redraw, Stamp::Send][..], &[Stamp::Trash, Stamp::Send][..]),
            "no video is drawn anew"
        );
    }

    #[test]
    fn sent_item_files() {
        assert_eq!(
            Sending::Photo("a.jpg".into(), (4, 3)).to_enclosure(),
            Some(Enclosure::photo("camera/pictures/a.jpg".into(), (4, 3))),
            "a photo by its file in the pictures' roll, with its size"
        );
        assert_eq!(
            Sending::Video("a.mp4".into(), 1 << 20).to_enclosure(),
            Some(Enclosure::video("camera/videos/a.mp4".into()))
        );
        assert_eq!(
            Sending::Video("a.mp4".into(), u64::from(u32::MAX)).to_enclosure(),
            None,
            "a video larger than Signal's apps send goes nowhere"
        );
    }

    #[test]
    fn thumbnail_and_errand_lines() {
        let picture = Picture { words: alloc::vec![0; 6], width: 3 };
        assert_eq!(to_thumbnail_line("a", &picture, 7), "gallery: a: a small picture of 3x2 in 7 ms");
        assert_eq!(to_deleted_line("a"), "gallery: a deleted");
    }
}

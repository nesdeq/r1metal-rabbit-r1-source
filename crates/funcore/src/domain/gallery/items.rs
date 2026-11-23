//! A roll's items, from the names of the camera's files.

use super::{Kind, Label, Stamp};
use crate::base::file_names::stem;
use crate::domain::magic::{self, Order, Variant};
use crate::storage::{PICTURE_EXTENSION, PICTURES, VIDEO_EXTENSION, VIDEOS};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// A step of an item's deletion.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DeleteStep {
    // A file to take away, by its directory and its name.
    Remove(&'static str, String),
    // An order for the pixel cam.
    Tell(Order),
}

/// An item of a roll: its pixel art file, its original file, and the first's label.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Item {
    pixel: String,
    /// The file shown for photos as taken.
    pub original: String,
    label: Label,
}

impl Item {
    /// An item of one file.
    pub fn from_file(name: &str) -> Item {
        Item { pixel: name.into(), original: name.into(), label: Label::None }
    }

    // The file that shows for the item, and its label.
    pub(super) fn view(&self, is_original: bool) -> (&str, Label) {
        if is_original { (&self.original, Label::None) } else { (&self.pixel, self.label) }
    }

    // The file a stamped icon acts on: the one that shows where a photo is sent, else the photo as taken.
    pub(super) fn target(&self, stamp: Stamp, is_original: bool) -> &str {
        match stamp {
            Stamp::Send => self.view(is_original).0,
            Stamp::Trash | Stamp::Redraw => &self.original,
        }
    }

    /// The photo's name for the pixel cam.
    pub fn photo(&self) -> Option<&str> {
        magic::parse_name(&self.original).filter(|parts| parts.1 == Variant::Original).map(|parts| parts.0)
    }

    // The item's files.
    fn files(&self) -> impl Iterator<Item = &str> {
        let pixel = (self.pixel != self.original).then_some(self.pixel.as_str());
        core::iter::once(self.original.as_str()).chain(pixel)
    }

    /// The steps that delete the item of a roll of `kind`, in their order: its own files, then, for a photo of the pixel cam's, the order to let go of it, its pixel picture by its name, whatever the roll listed, and its small picture that waits.
    pub fn delete_steps(&self, kind: Kind) -> Vec<DeleteStep> {
        let directory = location(kind).0;
        let own = self.files().map(|file| DeleteStep::Remove(directory, file.into()));
        let of_photo = self.photo().into_iter().flat_map(|photo| {
            [
                DeleteStep::Tell(Order::Forget(photo.into())),
                DeleteStep::Remove(PICTURES, magic::to_name(photo, Variant::Pixel, PICTURE_EXTENSION)),
                DeleteStep::Remove(magic::DIRECTORY, magic::to_waiting_name(photo)),
            ]
        });
        own.chain(of_photo).collect()
    }
}

/// A roll's directory and file extension.
pub const fn location(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Photo => (PICTURES, PICTURE_EXTENSION),
        Kind::Video => (VIDEOS, VIDEO_EXTENSION),
    }
}

#[derive(Default)]
struct Pair<'a> {
    original: Option<&'a str>,
    pixel: Option<&'a str>,
}

fn to_item(photo: &str, pair: Pair<'_>, waiting: &[String]) -> Option<Item> {
    match (pair.original, pair.pixel) {
        (Some(original), Some(pixel)) => {
            Some(Item { pixel: pixel.into(), original: original.into(), label: Label::None })
        }
        (Some(original), None) => {
            let is_waiting = waiting.iter().any(|name| name == photo);
            let label = if is_waiting { Label::Waiting } else { Label::None };
            Some(Item { label, ..Item::from_file(original) })
        }
        (None, Some(pixel)) => Some(Item::from_file(pixel)),
        (None, None) => None,
    }
}

/// The picture roll's items, newest first: a photo and its pixel art are one item.
pub fn photos(names: &[String], waiting: &[String]) -> Vec<Item> {
    let mut pairs: BTreeMap<(&str, bool), Pair<'_>> = BTreeMap::new();
    for name in names {
        match magic::parse_name(name) {
            Some((photo, Variant::Original)) => pairs.entry((photo, false)).or_default().original = Some(name),
            Some((photo, Variant::Pixel)) => pairs.entry((photo, false)).or_default().pixel = Some(name),
            None => {
                pairs.insert((stem(name), true), Pair { original: Some(name), pixel: Some(name) });
            }
        }
    }
    pairs.into_iter().rev().filter_map(|((photo, _), pair)| to_item(photo, pair, waiting)).collect()
}

/// The video roll's items, newest first.
pub fn videos(names: &[String]) -> Vec<Item> {
    names.iter().rev().map(|name| Item::from_file(name)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photo_pairs_newest_first() {
        let names = [
            "20261008_080000.JPG",
            "20261009_193512.jpg",
            "20261009_193512_1.jpg",
            "20261010_100000_original.jpg",
            "20261010_100000_pixel.jpg",
            "20261010_110000_original.jpg",
            "20261010_120000_1_original.jpg",
            "20261010_120000_original.jpg",
            "20261010_120000_pixel.jpg",
            "20261010_130000_pixel.jpg",
        ]
        .map(String::from);
        let waiting = ["20261010_120000_1".into(), "20261010_100000".into()];
        let items = photos(&names, &waiting);
        // Each item's pixel file and label, then its original file, which has no label.
        let views: Vec<(&str, Label, &str)> =
            items.iter().map(|item| (item.view(false).0, item.view(false).1, item.view(true).0)).collect();
        let single = |name: &'static str| (name, Label::None, name);
        let expected = [
            single("20261010_130000_pixel.jpg"),
            ("20261010_120000_1_original.jpg", Label::Waiting, "20261010_120000_1_original.jpg"),
            ("20261010_120000_pixel.jpg", Label::None, "20261010_120000_original.jpg"),
            single("20261010_110000_original.jpg"),
            ("20261010_100000_pixel.jpg", Label::None, "20261010_100000_original.jpg"),
            single("20261009_193512_1.jpg"),
            single("20261009_193512.jpg"),
            single("20261008_080000.JPG"),
        ];
        assert_eq!(
            views, expected,
            "a photo shows as its pixel art or as it was taken; one without pixel art shows as it was taken either way, with a word while the art is still to come; pixel art whose photo is gone stands alone, and so does a file of before the pixel cam"
        );
        assert!(items.iter().all(|item| item.view(true).1 == Label::None));
        assert!(photos(&[], &waiting).is_empty());
        let names: Vec<Option<&str>> = items.iter().map(Item::photo).collect();
        assert_eq!(
            names,
            [
                None,
                Some("20261010_120000_1"),
                Some("20261010_120000"),
                Some("20261010_110000"),
                Some("20261010_100000"),
                None,
                None,
                None
            ],
            "a photo of the pixel cam is known to it by its name; pixel art alone and an older file are not"
        );
        assert_eq!(
            items[2].files().collect::<Vec<_>>(),
            ["20261010_120000_original.jpg", "20261010_120000_pixel.jpg"],
            "a photo's files: as taken, then its pixel art"
        );
        let (art, taken) = ("20261010_120000_pixel.jpg", "20261010_120000_original.jpg");
        assert_eq!(
            [Stamp::Send, Stamp::Trash, Stamp::Redraw]
                .map(|stamp| (items[2].target(stamp, false), items[2].target(stamp, true))),
            [(art, taken), (taken, taken), (taken, taken)],
            "a photo goes as it shows, its pixel art or as taken, and is deleted and redrawn by the photo as taken"
        );
        assert_eq!(
            items[1].target(Stamp::Send, false),
            "20261010_120000_1_original.jpg",
            "one whose art is still to come goes as taken"
        );
    }

    #[test]
    fn deletion_steps() {
        let remove = |directory: &'static str, name: &str| DeleteStep::Remove(directory, name.into());
        let names = ["20261010_120000_original.jpg", "20261010_120000_pixel.jpg"].map(String::from);
        let forget = DeleteStep::Tell(Order::Forget("20261010_120000".into()));
        assert_eq!(
            photos(&names, &[])[0].delete_steps(Kind::Photo),
            [
                remove(PICTURES, "20261010_120000_original.jpg"),
                remove(PICTURES, "20261010_120000_pixel.jpg"),
                forget.clone(),
                remove(PICTURES, "20261010_120000_pixel.jpg"),
                remove(magic::DIRECTORY, "20261010_120000.jpg"),
            ],
            "a photo's own files first; once they are gone the pixel cam lets go of it, and its pixel picture goes by its name, which a redrawing may have kept since the roll was read, with its small picture that waits"
        );
        assert_eq!(
            photos(&names[..1], &[])[0].delete_steps(Kind::Photo),
            [
                remove(PICTURES, "20261010_120000_original.jpg"),
                forget,
                remove(PICTURES, "20261010_120000_pixel.jpg"),
                remove(magic::DIRECTORY, "20261010_120000.jpg"),
            ],
            "the same for one the roll listed no pixel picture of"
        );
        assert_eq!(
            photos(&names[1..], &[])[0].delete_steps(Kind::Photo),
            [remove(PICTURES, "20261010_120000_pixel.jpg")],
            "pixel art whose photo is gone is a file alone, and no photo of the pixel cam's"
        );
        assert_eq!(
            Item::from_file("clip.mp4").delete_steps(Kind::Video),
            [remove(VIDEOS, "clip.mp4")],
            "a video is its file in the videos' roll"
        );
    }

    #[test]
    fn video_file_and_locations() {
        let clip = Item::from_file("clip.mp4");
        assert_eq!(
            (clip.view(true), clip.view(false), clip.photo()),
            (("clip.mp4", Label::None), ("clip.mp4", Label::None), None)
        );
        assert_eq!(clip.files().collect::<Vec<_>>(), ["clip.mp4"]);
        let names = ["20261009_193512.mp4", "20261010_080000.mp4"].map(String::from);
        assert_eq!(
            videos(&names),
            [Item::from_file("20261010_080000.mp4"), Item::from_file("20261009_193512.mp4")],
            "newest first"
        );
        assert_eq!(
            (location(Kind::Photo), location(Kind::Video)),
            ((PICTURES, PICTURE_EXTENSION), (VIDEOS, VIDEO_EXTENSION))
        );
    }
}

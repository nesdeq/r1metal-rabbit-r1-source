//! The roll's decoded photos.

use super::items::Item;
use super::to_size_text;
use crate::gfx::Picture;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

/// The log line of a file of `bytes` bytes read and decoded to `picture`.
pub fn to_decoded_line(
    name: &str,
    bytes: usize,
    read_time: Duration,
    picture: &Picture,
    decode_time: Duration,
) -> String {
    format!(
        "gallery: {name}: {bytes} bytes read in {} ms, decoded to {} in {} ms",
        read_time.as_millis(),
        to_size_text(picture),
        decode_time.as_millis()
    )
}

/// A decoded photo.
pub struct Still {
    /// The upright picture.
    pub picture: Picture,
    /// The picture's size in its file.
    pub size: (usize, usize),
}

/// The next step for the open photo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next<'a> {
    // The file's picture is ready to show.
    Show(&'a str),
    Decode(&'a str),
    Wait,
}

/// The roll's decoded pictures and the one last shown large.
pub struct Cache {
    ready: Vec<(String, Option<Still>)>,
    last: Option<(usize, u32, String)>,
    is_heading_older: bool,
}

impl Default for Cache {
    fn default() -> Cache {
        Cache { ready: Vec::new(), last: None, is_heading_older: true }
    }
}

impl Cache {
    /// Forgets the last shown picture, so a reopened item shows again.
    pub fn reopen(&mut self) {
        self.last = None;
    }

    fn has(&self, file: &str) -> bool {
        self.ready.iter().any(|entry| entry.0 == file)
    }

    /// The next step for the open item: its picture, else the files beside it.
    pub fn next<'a>(&mut self, items: &'a [Item], index: usize, turn: u32, is_original: bool) -> Next<'a> {
        if let Some(previous) = self.last.as_ref().map(|last| last.0).filter(|previous| *previous != index) {
            self.is_heading_older = index > previous;
        }
        let file = items[index].view(is_original).0;
        let (older_index, newer_index) = (Some(index + 1), index.checked_sub(1));
        let neighbours = if self.is_heading_older { [older_index, newer_index] } else { [newer_index, older_index] };
        let neighbour_files =
            neighbours.into_iter().flatten().filter_map(|at| items.get(at)).map(|item| item.view(is_original).0);
        let files: Vec<&str> = core::iter::once(items[index].view(!is_original).0).chain(neighbour_files).collect();
        self.ready.retain(|entry| entry.0 == file || files.contains(&entry.0.as_str()));
        let is_shown = self.last.as_ref().is_some_and(|last| (last.0, last.1, last.2.as_str()) == (index, turn, file));
        if !is_shown {
            return if self.has(file) { Next::Show(file) } else { Next::Decode(file) };
        }
        files.into_iter().find(|name| !self.has(name)).map_or(Next::Wait, Next::Decode)
    }

    /// Adds a file's decoded picture.
    pub fn add(&mut self, file: &str, still: Option<Still>) {
        self.ready.push((file.into(), still));
    }

    /// A file's decoded picture.
    pub fn picture(&self, file: &str) -> Option<&Still> {
        self.ready.iter().find(|entry| entry.0 == file)?.1.as_ref()
    }

    /// Notes that the item was shown for the wheel's turn from `file`.
    pub fn mark_shown(&mut self, index: usize, turn: u32, file: &str) {
        self.last = Some((index, turn, file.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::gallery::items::photos;
    use alloc::vec;

    fn still(word: u32) -> Option<Still> {
        Some(Still { picture: Picture { words: vec![word], width: 1 }, size: (1, 1) })
    }

    fn roll() -> Vec<Item> {
        let names: Vec<String> = (0..5)
            .flat_map(|at| {
                [alloc::format!("2026101{at}_100000_original.jpg"), alloc::format!("2026101{at}_100000_pixel.jpg")]
            })
            .collect();
        photos(&names, &[])
    }

    #[test]
    fn open_photo_steps() {
        let (items, mut cache) = (roll(), Cache::default());
        let file = items[2].view(false).0;
        assert_eq!(cache.next(&items, 2, 0, false), Next::Decode(file), "a photo just opened is decoded first");
        cache.add(file, still(2));
        assert_eq!(cache.next(&items, 2, 0, false), Next::Show(file), "and then shows");
        cache.mark_shown(2, 0, file);
        assert_eq!(cache.picture(file).map(|still| still.picture.words[0]), Some(2));
        let mut decoded = Vec::new();
        while let Next::Decode(name) = cache.next(&items, 2, 0, false) {
            decoded.push(name);
            cache.add(name, None);
        }
        assert_eq!(
            decoded,
            [items[2].view(true).0, items[3].view(false).0, items[1].view(false).0],
            "its photo as taken, then the older one the wheel goes toward, then the newer one"
        );
        assert_eq!(cache.next(&items, 2, 0, false), Next::Wait, "all that is wanted is ready");
        assert!(cache.has(items[3].view(false).0) && cache.picture(items[3].view(false).0).is_none());
        assert_eq!(cache.next(&items, 2, 1, false), Next::Show(file), "another turn of the wheel wants it anew");
        cache.reopen();
        assert_eq!(cache.next(&items, 2, 0, false), Next::Show(file), "and so does a photo opened anew");
    }

    #[test]
    fn decoded_line() {
        let picture = Picture { words: vec![0; 6], width: 3 };
        assert_eq!(
            to_decoded_line("a.jpg", 1234, Duration::from_millis(12), &picture, Duration::from_millis(34)),
            "gallery: a.jpg: 1234 bytes read in 12 ms, decoded to 3x2 in 34 ms"
        );
    }

    #[test]
    fn newer_direction() {
        let (items, mut cache) = (roll(), Cache::default());
        cache.mark_shown(3, 0, items[3].view(false).0);
        for at in [3, 4, 1, 2] {
            cache.add(items[at].view(false).0, still(at as u32));
        }
        let file = items[2].view(false).0;
        assert_eq!(cache.next(&items, 2, 1, false), Next::Show(file));
        assert!(!cache.has(items[4].view(false).0), "a photo two away is let go");
        cache.mark_shown(2, 1, file);
        assert_eq!(
            cache.next(&items, 2, 1, false),
            Next::Decode(items[2].view(true).0),
            "the photo's other picture first"
        );
        cache.add(items[2].view(true).0, None);
        assert_eq!(cache.next(&items, 2, 1, false), Next::Wait, "the newer and the older one are kept from before");
        assert_eq!(
            cache.next(&items, 4, 1, true),
            Next::Decode(items[4].view(true).0),
            "shown as taken, the photo's own file is the one decoded to show"
        );
    }
}

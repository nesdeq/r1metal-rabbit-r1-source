//! A roll as the gallery's thread serves it.

use super::items::Item;
use super::stills::Cache;
use super::{Errand, Kind, Label, Note, RollRequest, Stamp, Thumbnail};
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

const CACHE_PAGES: usize = 1;

/// The items whose thumbnails are made next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Batch {
    // Photos, each decoded on a thread of its own.
    Photos(Vec<usize>),
    // A video alone: the decoder is one, and drawing its poster takes every thread of the crew.
    Video(usize),
}

/// An item's thumbnail for a round of the grid.
pub struct RoundThumbnail {
    pub round: u32,
    pub item: usize,
    pub thumbnail: Thumbnail,
}

/// The grid the thumbnails are made for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grid {
    pub places: usize,
    pub size: (usize, usize),
}

impl Grid {
    pub const fn from_request(request: RollRequest) -> Grid {
        Grid { places: request.places, size: request.place_size }
    }
}

/// The end of a pass over a roll.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RollStep {
    Wait,
    Show,
    Deal,
}

/// A roll as its thread serves it.
pub struct Roll {
    pub kind: Kind,
    /// The main loop's generation the items were read after.
    pub generation: u32,
    /// Its items, newest first.
    pub items: Vec<Item>,
    thumbnails: Vec<(usize, Thumbnail)>,
    is_original: bool,
    grid: Option<Grid>,
    round: Option<u32>,
    dealt_items: Vec<usize>,
    /// The roll of photos' decoded pictures.
    pub cache: Cache,
}

impl Roll {
    pub fn new(kind: Kind, generation: u32, items: Vec<Item>) -> Roll {
        Roll {
            kind,
            generation,
            items,
            thumbnails: Vec::new(),
            is_original: false,
            grid: None,
            round: None,
            dealt_items: Vec::new(),
            cache: Cache::default(),
        }
    }

    fn clear_thumbnails(&mut self) {
        self.thumbnails.clear();
        self.dealt_items.clear();
    }

    /// Sets the grid; another size drops the thumbnails.
    pub fn set_grid(&mut self, grid: Grid) {
        if self.grid != Some(grid) {
            self.clear_thumbnails();
        }
        self.grid = Some(grid);
    }

    /// Drops the thumbnails of the photos with a new pixel picture, and every decoded large picture of the roll.
    pub fn forget_redraws(&mut self, redraws: &[String]) {
        if redraws.is_empty() {
            return;
        }
        let items = &self.items;
        let is_redraw = |item: usize| {
            items.get(item).and_then(Item::photo).is_some_and(|photo| redraws.iter().any(|redraw| redraw == photo))
        };
        self.thumbnails.retain(|entry| !is_redraw(entry.0));
        self.dealt_items.retain(|item| !is_redraw(*item));
        self.cache = Cache::default();
    }

    /// True where the main loop's generation differs from the one the items were read after.
    pub const fn is_stale(&self, generation: u32) -> bool {
        generation != self.generation
    }

    /// Takes the roll's current items.
    pub fn relist(&mut self, items: Vec<Item>, generation: u32) {
        if items.len() == self.items.len() {
            let previous = &self.items;
            self.thumbnails.retain(|entry| items[entry.0] == previous[entry.0]);
            self.dealt_items.retain(|item| items[*item] == previous[*item]);
        } else {
            self.thumbnails.clear();
            self.round = None;
        }
        self.items = items;
        self.generation = generation;
    }

    /// Sets whether photos show as taken; a change drops the thumbnails.
    pub fn set_original(&mut self, is_original: bool) {
        if self.is_original != is_original {
            self.clear_thumbnails();
        }
        self.is_original = is_original;
    }

    /// Once a round: the item count, and why the roll shows nothing where it holds none.
    pub fn begin_round(&mut self, round: u32) -> Option<(usize, Option<Note>)> {
        if self.round == Some(round) {
            return None;
        }
        self.round = Some(round);
        self.dealt_items.clear();
        let note = self.items.is_empty().then_some(Note::Empty(self.kind));
        Some((self.items.len(), note))
    }

    /// The errands of the roll's own items, each with the item its file names and the file it acts on; one of another roll, or of an item the roll holds no more, is dropped.
    pub fn errands(&self, errands: Vec<Errand>) -> impl Iterator<Item = (&Item, Stamp, &str)> {
        let own = errands.into_iter().filter(|errand| errand.kind == self.kind);
        own.filter_map(|errand| {
            let item = self.items.iter().find(|item| item.original == errand.file)?;
            Some((item, errand.stamp, item.target(errand.stamp, errand.is_original)))
        })
    }

    /// The file that shows for an item, and its label.
    pub fn view(&self, item: usize) -> (&str, Label) {
        self.items[item].view(self.is_original)
    }

    /// The end of a pass for that request.
    pub const fn next_step(&self, request: RollRequest) -> RollStep {
        if self.items.is_empty() {
            RollStep::Wait
        } else if request.open_item.is_some() {
            RollStep::Show
        } else {
            RollStep::Deal
        }
    }

    fn missing(&self, items: Range<usize>) -> Vec<usize> {
        items.filter(|item| self.thumbnails.iter().all(|entry| entry.0 != *item)).collect()
    }

    /// Deals the grid its thumbnails, and returns the batch to make next.
    pub fn deal(&mut self, request: RollRequest, grid: Grid) -> (Vec<RoundThumbnail>, Option<Batch>) {
        let count = self.items.len();
        let visible_start = request.page.saturating_sub(request.margin).min(count);
        let visible_end = request.page.saturating_add(grid.places + request.margin).min(count);
        let span = CACHE_PAGES * (grid.places + 2 * request.margin);
        let cache_start = visible_start.saturating_sub(span);
        let cache_end = visible_end.saturating_add(span).min(count);
        self.thumbnails.retain(|entry| (cache_start..cache_end).contains(&entry.0));
        self.dealt_items.retain(|item| (visible_start..visible_end).contains(item));
        let is_ready = |entry: &&(usize, Thumbnail)| {
            (visible_start..visible_end).contains(&entry.0) && !self.dealt_items.contains(&entry.0)
        };
        let ready: Vec<RoundThumbnail> = self
            .thumbnails
            .iter()
            .filter(is_ready)
            .map(|entry| RoundThumbnail { round: request.round, item: entry.0, thumbnail: entry.1.clone() })
            .collect();
        self.dealt_items.extend(ready.iter().map(|ready| ready.item));
        let spans = [visible_start..visible_end, visible_end..cache_end, cache_start..visible_start];
        let next = spans.into_iter().map(|span| self.missing(span)).find(|items| !items.is_empty());
        let batch = next.map(|items| match self.kind {
            Kind::Photo => Batch::Photos(items),
            Kind::Video => Batch::Video(items[0]),
        });
        (ready, batch)
    }

    /// Adds thumbnails of the roll's items.
    pub fn add_thumbnails(&mut self, thumbnails: Vec<(usize, Thumbnail)>) {
        self.thumbnails.extend(thumbnails);
    }
}

#[cfg(test)]
mod tests {
    use super::super::samples;
    use super::*;
    use crate::gfx::Picture;
    use alloc::vec;

    const GRID: Grid = Grid { places: 2, size: (228, 173) };

    fn request(kind: Kind, page: usize) -> RollRequest {
        RollRequest { page, places: GRID.places, ..samples::request(kind) }
    }

    fn thumbnail(mark: u32) -> Thumbnail {
        let file = alloc::format!("{mark}.jpg");
        Thumbnail { picture: Picture { words: vec![mark], width: 1 }, label: Label::None, size: (1, 1), file }
    }

    fn roll(kind: Kind, count: usize) -> Roll {
        Roll::new(kind, 0, (0..count).map(|at| Item::from_file(&alloc::format!("{at}.jpg"))).collect())
    }

    fn thumbnails(items: &[usize]) -> Vec<(usize, Thumbnail)> {
        items.iter().map(|&item| (item, thumbnail(item as u32))).collect()
    }

    fn deal(roll: &mut Roll, page: usize) -> (Vec<usize>, Option<Batch>) {
        let (ready, next) = roll.deal(request(roll.kind, page), GRID);
        assert!(ready.iter().all(|ready| ready.round == 1 && ready.thumbnail.picture.words == [ready.item as u32]));
        (ready.iter().map(|ready| ready.item).collect(), next)
    }

    fn photos_of(items: &[usize]) -> Option<Batch> {
        Some(Batch::Photos(items.to_vec()))
    }

    #[test]
    fn deal_order() {
        let mut photos = roll(Kind::Photo, 9);
        assert_eq!(deal(&mut photos, 4), (vec![], photos_of(&[4, 5])), "the page that shows lacks all");
        photos.add_thumbnails(thumbnails(&[4, 5]));
        assert_eq!(deal(&mut photos, 4), (vec![4, 5], photos_of(&[6, 7])), "the page after it next");
        photos.add_thumbnails(thumbnails(&[6, 7]));
        assert_eq!(deal(&mut photos, 4), (vec![], photos_of(&[2, 3])), "given once, then the page before");
        photos.add_thumbnails(thumbnails(&[2, 3]));
        assert_eq!(deal(&mut photos, 4).1, None, "nothing is left to make");
        assert_eq!(deal(&mut photos, 6), (vec![6, 7], photos_of(&[8])), "the next page is given what was made of it");
        assert_eq!(deal(&mut photos, 2).0, vec![], "what lies beyond the pages kept was let go");
    }

    #[test]
    fn video_batch_and_margin() {
        let mut videos = roll(Kind::Video, 9);
        assert_eq!(deal(&mut videos, 4).1, Some(Batch::Video(4)), "a video's poster is made alone");
        let drum = RollRequest { margin: 1, ..request(Kind::Video, 4) };
        let mut photos = roll(Kind::Photo, 9);
        assert_eq!(photos.deal(drum, GRID).1, photos_of(&[3, 4, 5, 6]), "those a drum shows beside its band");
    }

    #[test]
    fn thumbnails_remade() {
        let mut photos = roll(Kind::Photo, 4);
        photos.set_grid(GRID);
        photos.add_thumbnails(thumbnails(&[0, 1]));
        assert_eq!(deal(&mut photos, 0).0, vec![0, 1]);
        assert_eq!((photos.begin_round(1), photos.begin_round(1)), (Some((4, None)), None), "a round is begun once");
        assert_eq!(deal(&mut photos, 0).0, vec![0, 1], "and given its pictures anew");
        photos.set_original(false);
        assert_eq!(deal(&mut photos, 0).0, vec![], "the same way of showing keeps them");
        photos.set_original(true);
        assert_eq!(deal(&mut photos, 0).1, photos_of(&[0, 1]), "the other way makes them anew");
        photos.add_thumbnails(thumbnails(&[0, 1]));
        photos.set_grid(GRID);
        assert_eq!(deal(&mut photos, 0).0, vec![0, 1], "the same grid keeps them");
        photos.set_grid(Grid { places: 3, ..GRID });
        assert_eq!(deal(&mut photos, 0).1, photos_of(&[0, 1]), "another grid makes them anew");
        photos.add_thumbnails(thumbnails(&[0, 1]));
        assert!(!photos.is_stale(0) && photos.is_stale(1));
        let mut now = photos.items.clone();
        now[1] = Item::from_file("other.jpg");
        photos.relist(now, 1);
        assert_eq!(
            (photos.generation, deal(&mut photos, 0).1),
            (1, photos_of(&[1])),
            "a changed file's picture alone is made anew"
        );
        photos.relist(photos.items[..3].to_vec(), 2);
        assert_eq!(photos.begin_round(1), Some((3, None)), "a roll of another length begins its round anew");
        assert_eq!(deal(&mut photos, 0).1, photos_of(&[0, 1]), "with every picture made anew");
        assert_eq!(photos.view(2), ("2.jpg", Label::None));
    }

    #[test]
    fn own_errands() {
        let mut photos = roll(Kind::Photo, 3);
        let errand = |kind, file: &str, stamp| Errand { kind, file: file.into(), stamp, is_original: false };
        let errands = vec![
            errand(Kind::Photo, "2.jpg", Stamp::Redraw),
            errand(Kind::Video, "0.jpg", Stamp::Trash),
            errand(Kind::Photo, "3.jpg", Stamp::Trash),
            errand(Kind::Photo, "0.jpg", Stamp::Trash),
            errand(Kind::Photo, "1.jpg", Stamp::Send),
        ];
        let done = |photos: &Roll, errands: Vec<Errand>| -> Vec<(String, Stamp)> {
            photos.errands(errands).map(|(item, stamp, _)| (item.original.clone(), stamp)).collect()
        };
        assert_eq!(
            done(&photos, errands),
            [("2.jpg".into(), Stamp::Redraw), ("0.jpg".into(), Stamp::Trash), ("1.jpg".into(), Stamp::Send)],
            "in the order they were asked, without the other roll's and that of a file the roll does not hold"
        );
        let mut grown = vec![Item::from_file("new.jpg")];
        grown.extend(photos.items.iter().cloned());
        photos.relist(grown, 1);
        assert_eq!(
            done(&photos, vec![errand(Kind::Photo, "1.jpg", Stamp::Trash)]),
            [("1.jpg".into(), Stamp::Trash)],
            "an item is the one its file names, wherever a roll that has grown since holds it"
        );
        let names = ["20261010_100000_original.jpg", "20261010_100000_pixel.jpg"].map(String::from);
        let paired = Roll::new(Kind::Photo, 0, crate::domain::gallery::items::photos(&names, &[]));
        let sent = |is_original| {
            let send = Errand { is_original, ..errand(Kind::Photo, "20261010_100000_original.jpg", Stamp::Send) };
            paired.errands(vec![send]).map(|(_, _, file)| String::from(file)).collect::<Vec<_>>()
        };
        assert_eq!(
            [sent(false), sent(true)],
            [["20261010_100000_pixel.jpg"], ["20261010_100000_original.jpg"]],
            "a photo is sent as the roll showed it when its icon was touched"
        );
    }

    #[test]
    fn empty_roll_round() {
        let mut empty = roll(Kind::Video, 0);
        assert_eq!(
            empty.begin_round(1),
            Some((0, Some(Note::Empty(Kind::Video)))),
            "a roll the partition holds nothing of"
        );
        assert_eq!(
            (empty.begin_round(1), empty.begin_round(2).map(|(count, _)| count)),
            (None, Some(0)),
            "told once a round"
        );
    }

    #[test]
    fn pass_ends() {
        let grid = request(Kind::Photo, 0);
        let open = RollRequest { open_item: Some(0), ..grid };
        assert_eq!(roll(Kind::Photo, 0).next_step(open), RollStep::Wait, "a roll that holds nothing");
        assert_eq!(
            (roll(Kind::Photo, 1).next_step(open), roll(Kind::Photo, 1).next_step(grid)),
            (RollStep::Show, RollStep::Deal)
        );
    }

    #[test]
    fn redraw_drops_thumbnail() {
        let names = ["20261010_100000_original.jpg", "20261011_100000_original.jpg"].map(String::from);
        let mut photos = Roll::new(Kind::Photo, 0, crate::domain::gallery::items::photos(&names, &[]));
        photos.add_thumbnails(thumbnails(&[0, 1]));
        photos.forget_redraws(&[]);
        assert_eq!(deal(&mut photos, 0).0, vec![0, 1]);
        photos.forget_redraws(&["20261010_100000".into()]);
        assert_eq!(deal(&mut photos, 0).1, photos_of(&[1]), "the older one, the second");
    }
}

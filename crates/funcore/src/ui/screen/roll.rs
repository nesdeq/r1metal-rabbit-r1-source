use super::bar::BarWords;
use super::input::Touch;
use super::{Screen, View};
use crate::base::time::write_duration;
use crate::domain::cause::Cause;
use crate::domain::gallery::{Errand, Kind, Label, Note, RollRequest, Room, Stamp, Thumbnail};
use crate::domain::scene::{PICTURES, VIDEOS};
use crate::gfx::key::Ids;
use crate::gfx::store::Store;
use crate::gfx::{Picture, Rect};
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::picture::carousel::Carousel;
use crate::ui::picture::drum::Drum;
use crate::ui::picture::stamps::{Stamps, picture_area};
use crate::ui::robot::Reaction;
use crate::ui::speech::write_place;
use crate::ui::words::{UNREADABLE, WAITING, cause_word};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const MAGIC_ON: &str = "MAGIC ON";
const MAGIC_OFF: &str = "MAGIC OFF";
const NO_PICTURES: &str = "NO PICTURES";
const NO_VIDEOS: &str = "NO VIDEOS";

pub(crate) const fn roll_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Photo => PICTURES,
        Kind::Video => VIDEOS,
    }
}

const fn note_word(note: Note) -> (&'static str, bool) {
    match note {
        Note::Empty(Kind::Photo) => (NO_PICTURES, true),
        Note::Empty(Kind::Video) => (NO_VIDEOS, true),
        Note::Unreadable => (UNREADABLE, true),
        Note::NotDeleted => (cause_word(Cause::NotDeleted), false),
        Note::NoPixels => (cause_word(Cause::NoPixels), false),
    }
}

fn write_label(label: Label, text: &mut String) {
    match label {
        Label::None => {}
        Label::Waiting => text.push_str(WAITING),
        Label::Unreadable => text.push_str(UNREADABLE),
        Label::Seconds(seconds) => write_duration(text, seconds),
    }
}

pub(crate) struct Rolls {
    pub(crate) grid: Grid,
    pub(crate) store: Store<Thumbnail>,
    pub(crate) picture: Picture,
    // The item the gallery's last large picture is of, and its file as the camera took it.
    shown: Option<(usize, String)>,
    pub(crate) id: u64,
    pub(crate) is_current: bool,
    pub(crate) carousel: Carousel,
    turn: u32,
    pub(crate) drum: Drum,
    is_original: bool,
    pub(crate) stamps: Stamps,
    errands: Vec<Errand>,
    is_playing: bool,
}

impl Rolls {
    pub(crate) fn new(carousel: Carousel, stamps: Stamps, ids: &Ids) -> Rolls {
        Rolls {
            grid: Grid::empty(),
            store: Store::default(),
            picture: Picture::default(),
            shown: None,
            id: ids.next(),
            is_current: false,
            carousel,
            turn: 0,
            drum: Drum::new(),
            is_original: false,
            stamps,
            errands: Vec::new(),
            is_playing: false,
        }
    }

    // The way a photo's picture flips: one way to the photo as taken, the other to its pixel art.
    const fn flip_way(&self) -> f32 {
        if self.is_original { 1.0 } else { -1.0 }
    }
}

fn write_speech(ui: &Ui, text: &mut String) {
    let count = ui.rolls.grid.items.unwrap_or(0);
    let is_video = matches!(ui.screen, Some(Screen::Grid(Kind::Video) | Screen::Roll(Kind::Video)));
    let noun = if is_video { "video" } else { "picture" };
    let _ = match count {
        0 => write!(text, "No {noun}s yet."),
        1 => write!(text, "One {noun}."),
        _ => write!(text, "{count} {noun}s."),
    };
}

impl Ui {
    fn roll_room(&self) -> Room {
        let ruler = &self.ruler;
        Room { screen: (ruler.width.max(0) as usize, ruler.height.max(0) as usize), bar: ruler.bar().max(0) as usize }
    }

    pub(crate) fn roll_area(&self) -> Rect {
        let (quarters, ruler) = (self.quarters(), &self.ruler);
        let (width, height) = self.roll_room().turned_size(quarters);
        Rect::new(0, 0, ruler.width, ruler.height).place(quarters, 0, ruler.bar(), width as i32, height as i32)
    }

    pub(super) fn open_roll(&mut self, kind: Kind) {
        (self.note, self.rolls.is_original) = (None, false);
        self.rolls.grid.begin();
        self.rolls.drum.clear();
        self.stand_dial(0);
        self.screen = Some(Screen::Grid(kind));
    }

    /// The request to the gallery while awake.
    pub fn gallery_request(&self) -> Option<RollRequest> {
        self.visible_roll_request().filter(|_| self.is_awake())
    }

    /// The visible roll's request to the gallery, awake or asleep.
    pub fn visible_roll_request(&self) -> Option<RollRequest> {
        let (kind, open_item) = match self.screen {
            Some(Screen::Grid(kind)) => (kind, None),
            Some(Screen::Roll(kind)) => (kind, Some(self.rolls.grid.selection)),
            _ => return None,
        };
        Some(RollRequest {
            kind,
            page: self.rolls.grid.page(),
            round: self.rolls.grid.round,
            open_item,
            turn: self.rolls.turn,
            is_original: self.rolls.is_original,
            places: self.rolls.grid.places(),
            place_size: self.rolls.grid.place_size(),
            margin: self.roll_margin(),
            room: self.roll_room(),
        })
    }

    /// Takes a note of the gallery's; true if the screen changed.
    #[must_use]
    pub fn take_gallery_note(&mut self, note: Note, now_ns: u64) -> bool {
        let (word, is_lasting) = note_word(note);
        self.set_note(word.into(), is_lasting, now_ns)
    }

    pub(super) fn flip(&mut self, now_ns: u64) {
        self.rolls.is_original = !self.rolls.is_original;
        self.rolls.grid.renew();
        if matches!(self.screen, Some(Screen::Roll(_))) {
            if self.rolls.grid.opening() < 1.0 {
                self.rolls.carousel.clear();
                self.rolls.is_current = false;
            } else {
                let selection = self.rolls.grid.selection;
                let current = core::mem::take(&mut self.rolls.is_current)
                    .then(|| (selection, core::mem::take(&mut self.rolls.picture)));
                self.rolls.carousel.flip(&self.ids, current, self.rolls.flip_way());
                self.rolls.id = self.ids.next();
            }
            self.rolls.turn = self.rolls.turn.wrapping_add(1);
        }
        self.react(Reaction::Poke, now_ns);
    }

    /// Takes the gallery's item count of a roll; true if the screen changed.
    #[must_use]
    pub fn set_items(&mut self, kind: Kind, count: usize) -> bool {
        if !matches!(self.screen, Some(Screen::Grid(screen_kind) | Screen::Roll(screen_kind)) if screen_kind == kind) {
            return false;
        }
        if self.rolls.grid.items != Some(count) {
            self.rolls.drum.clear();
        }
        self.rolls.grid.set_items(count);
        self.rolls.drum.trim(self.rolls.grid.selection);
        self.screen == Some(Screen::Grid(kind))
    }

    /// Takes an item's thumbnail for the grid or the drum; true if the screen changed.
    #[must_use]
    pub fn set_thumbnail(&mut self, round: u32, item: usize, thumbnail: Thumbnail) -> bool {
        self.make_opening_room(thumbnail.picture.words.len());
        self.place_thumbnail(round, item, thumbnail) && matches!(self.screen, Some(Screen::Grid(_)))
    }

    // What the rolls hold is let go of once none shows: the large picture, the small ones and the panes.
    pub(crate) fn let_go_rolls(&mut self) {
        if self.visible_roll_request().is_none() {
            (self.rolls.picture, self.rolls.shown, self.rolls.is_current) = (Picture::default(), None, false);
            self.rolls.store.clear();
            self.rolls.drum.clear();
            self.rolls.carousel.clear();
        }
    }

    pub(crate) fn advance_roll(&mut self, now_ns: u64) {
        self.rolls.grid.advance(now_ns);
        let Rolls { grid, store, .. } = &mut self.rolls;
        store.advance(now_ns, grid);
        let room = self.roll_area().view_size(self.quarters()).1 as f32;
        self.rolls.carousel.advance(now_ns, room);
    }

    pub(super) fn step_roll(&mut self, detents: i32) {
        let previous = self.rolls.grid.selection;
        let is_large = matches!(self.screen, Some(Screen::Roll(_)));
        self.rolls.grid.step(if is_large { -detents.signum() } else { detents });
        let selection = self.rolls.grid.selection;
        self.rolls.drum.trim(selection);
        if !is_large {
            self.rolls.is_current &= selection == previous;
        } else if selection == previous {
            if detents != 0 {
                self.rolls.carousel.bump(detents.signum() as f32);
            }
        } else if self.rolls.grid.opening() < 1.0 {
            self.rolls.carousel.clear();
            (self.rolls.is_current, self.rolls.turn) = (false, self.rolls.turn.wrapping_add(1));
        } else if let Some(picture) = self.rolls.carousel.take_back(selection) {
            (self.rolls.picture, self.rolls.id, self.rolls.is_current) = (picture, self.ids.next(), true);
            self.rolls.turn = self.rolls.turn.wrapping_add(1);
        } else {
            let current = core::mem::take(&mut self.rolls.is_current)
                .then(|| (previous, core::mem::take(&mut self.rolls.picture)));
            self.rolls.carousel.pull(&self.ids, selection, current);
            (self.rolls.id, self.rolls.turn) = (self.ids.next(), self.rolls.turn.wrapping_add(1));
        }
    }

    pub(super) fn view_item(&mut self, kind: Kind) {
        if self.rolls.grid.items.is_some_and(|count| count > 0) {
            self.rolls.picture.words.clear();
            (self.rolls.id, self.rolls.is_current, self.note) = (self.ids.next(), false, None);
            self.rolls.carousel.clear();
            self.rolls.grid.open(true);
            self.screen = Some(Screen::Roll(kind));
        }
    }

    pub(crate) fn stamp_touched(&self, kind: Kind, area: Rect, touch: &Touch) -> Option<Stamp> {
        self.rolls.stamps.touched(self.ruler.gap, kind, area, (touch.start, touch.at))
    }

    // Sending loses nothing and costs nothing: the first touch acts, where the others arm. The errand names the file of the picture touched, which the roll may hold at another place by the time it is run.
    fn touch_stamp(
        &mut self,
        kind: Kind,
        (item, file): (usize, String),
        stamp: Stamp,
        armed: Option<(usize, Stamp)>,
        now_ns: u64,
    ) {
        if stamp != Stamp::Send && armed != Some((item, stamp)) {
            self.rolls.stamps.armed = Some((item, stamp));
            return;
        }
        self.rolls.errands.push(Errand { kind, file, stamp, is_original: self.rolls.is_original });
        if stamp == Stamp::Trash {
            if matches!(self.screen, Some(Screen::Roll(_))) {
                self.rolls.carousel.clear();
                (self.note, self.rolls.is_current, self.screen) = (None, false, Some(Screen::Grid(kind)));
            }
            self.rolls.grid.redeal();
            self.rolls.drum.clear();
            self.react(Reaction::Shake, now_ns);
        }
    }

    /// Takes the errands to the gallery the stamped icons asked for.
    pub fn take_errands(&mut self) -> Vec<Errand> {
        core::mem::take(&mut self.rolls.errands)
    }

    pub(super) fn tap(&mut self, kind: Kind, touch: &Touch, armed: Option<(usize, Stamp)>, now_ns: u64) {
        if let Some((touched, stamp)) = self.stamp_tapped(kind, touch) {
            self.touch_stamp(kind, touched, stamp, armed, now_ns);
        }
    }

    fn rest_photo_area(&self) -> Option<Rect> {
        let does_rest = self.rolls.is_current
            && self.has_roll_picture()
            && self.rolls.grid.opening() >= 1.0
            && self.rolls.carousel.is_resting();
        does_rest.then(|| picture_area(self.roll_area(), &self.rolls.picture))
    }

    pub(super) fn tap_large(&mut self, touch: &Touch, armed: Option<(usize, Stamp)>, now_ns: u64) {
        let stamp = self.rest_photo_area().and_then(|area| self.stamp_touched(Kind::Photo, area, touch));
        // A picture the carousel gave back stands without its file until the gallery shows the item anew.
        let shown = self.rolls.shown.clone().filter(|shown| shown.0 == self.rolls.grid.selection);
        if let (Some(stamp), Some(shown)) = (stamp, shown) {
            self.touch_stamp(Kind::Photo, shown, stamp, armed, now_ns);
        }
    }

    /// The roll's large picture, into which a playing video swaps its next one.
    pub const fn roll_picture_mut(&mut self) -> &mut Picture {
        &mut self.rolls.picture
    }

    /// Notes a new large picture in the roll.
    pub fn note_roll_frame(&mut self) {
        self.rolls.id = self.ids.next();
    }

    /// Takes the gallery's picture of an item, with the item's file, and hands back the roll's previous one.
    pub fn take_roll_picture(&mut self, (item, file): (usize, String), picture: &mut Picture) {
        if self.rolls.is_current
            && item == self.rolls.grid.selection
            && self.screen == Some(Screen::Roll(Kind::Photo))
            && self.rolls.grid.opening() >= 1.0
        {
            let current = (item, core::mem::take(&mut self.rolls.picture));
            self.rolls.carousel.flip(&self.ids, Some(current), self.rolls.flip_way());
        }
        core::mem::swap(&mut self.rolls.picture, picture);
        self.rolls.id = self.ids.next();
        self.rolls.is_current = item == self.rolls.grid.selection;
        self.rolls.shown = Some((item, file));
        if self.rolls.is_current {
            let (width, height) = (self.rolls.picture.width, self.rolls.picture.height());
            self.rolls.carousel.show(if self.quarters() % 2 == 1 { width } else { height } as f32);
        }
    }

    /// Sets whether the roll plays a video.
    pub const fn set_playing(&mut self, is_on: bool) {
        self.rolls.is_playing = is_on;
    }

    /// True while a video of the roll shows large and plays.
    pub fn is_playing(&self) -> bool {
        self.is_awake() && self.rolls.is_playing && self.screen == Some(Screen::Roll(Kind::Video))
    }

    pub(crate) fn has_roll_picture(&self) -> bool {
        self.rolls.picture.width > 0 && self.rolls.picture.words.len() >= self.rolls.picture.width
    }

    fn write_selection_label(&self, text: &mut String) {
        if let Some(thumbnail) = self.selection_thumbnail() {
            write_label(thumbnail.0.label, text);
        }
    }

    // Writes the roll's word, a note or the place and label of what is chosen; true for a note.
    fn write_roll_word(&self, word: &mut String) -> bool {
        if let Some((note, _)) = &self.note {
            word.push_str(note);
        } else if let Some(count) = self.rolls.grid.items.filter(|count| *count > 0) {
            write_place(word, self.rolls.grid.selection + 1, count);
            word.push(' ');
            self.write_selection_label(word);
        }
        word.truncate(word.trim_end().len());
        self.note.is_some()
    }

    fn write_roll_caption(&self, caption: &mut String) {
        let _ = write!(caption, "{}/{} ", self.rolls.grid.selection + 1, self.rolls.grid.items.unwrap_or(0));
        self.write_selection_label(caption);
        caption.truncate(caption.trim_end().len());
    }

    pub(super) fn view_gallery<'a>(&self, kind: Kind, text: &'a mut String) -> View<'a> {
        let has_note = self.write_roll_word(text);
        let word_end = text.len();
        self.write_roll_caption(text);
        let (word, caption) = text.split_at(word_end);
        View::Gallery { kind, word, has_note, caption }
    }

    pub(crate) fn roll_words<'a>(&self, kind: Kind, word: &'a str, has_note: bool) -> BarWords<'a> {
        let state = (kind == Kind::Photo).then_some(if self.rolls.is_original { MAGIC_OFF } else { MAGIC_ON });
        BarWords {
            status: has_note.then_some(word),
            subject: Some(word).filter(|word| !has_note && !word.is_empty()),
            speech: Some(write_speech),
            state,
            ..BarWords::default()
        }
    }
}

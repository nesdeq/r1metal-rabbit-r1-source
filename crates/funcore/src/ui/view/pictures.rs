use crate::base::math::round;
use crate::base::time::write_duration;
use crate::codec::qr::Symbol;
use crate::domain::gallery::{Kind, Stamp, Thumbnail};
use crate::gfx::pen::Pen;
use crate::gfx::store::{Placement, SOLE, Store};
use crate::gfx::{Picture, Rect};
use crate::ui::Ui;
use crate::ui::bar::draw_turned_bar;
use crate::ui::field::draw_tickers;
use crate::ui::grid::Grid;
use crate::ui::list::LinePicture;
use crate::ui::picture::RollForm;
use crate::ui::picture::cells::{draw_captioned, draw_cells};
use crate::ui::picture::code::draw_code;
use crate::ui::picture::drum::REACH;
use crate::ui::picture::finder::{draw_finder, draw_note, draw_picture, draw_progress, draw_run_time};
use crate::ui::picture::opening::Opening;
use crate::ui::picture::stamps::picture_area;
use crate::ui::screen::bar::BarWords;
use crate::ui::screen::finder::ROW_BLOCKS;
use crate::ui::screen::input::{Touch, tapped};
use crate::ui::screen::roll::roll_name;
use crate::ui::screen::{Screen, seconds_since};
use alloc::string::String;

// The shape phones send a picture in upright, width to height: a far picture of it keeps the room's whole height beside this side's.
const UPRIGHT: (i32, i32) = (9, 16);

impl Ui {
    pub(in crate::ui) fn has_drums(&self) -> bool {
        self.look().roll.form == RollForm::Drum
    }

    // Photos stand in a store that cuts them in where the look's rolls do.
    pub(in crate::ui) fn photo_store<T>(&self) -> Store<T> {
        if self.look().roll.is_cut { Store::with_cuts() } else { Store::default() }
    }

    pub(in crate::ui) fn does_drum_show(&self) -> bool {
        self.has_drums() && matches!(self.screen, Some(Screen::Grid(_)))
    }

    pub(in crate::ui) fn is_drum_resting(&self) -> bool {
        self.dial.roll.at == self.rolls.grid.selection as f32
    }

    pub(in crate::ui) fn selection_thumbnail(&self) -> Option<(&Thumbnail, u64)> {
        if self.has_drums() {
            return self.rolls.drum.thumbnail(self.rolls.grid.selection);
        }
        self.rolls.store.get(self.rolls.grid.selection_spot()?)
    }

    pub(in crate::ui) fn roll_margin(&self) -> usize {
        if self.has_drums() { REACH } else { 0 }
    }

    pub(in crate::ui) fn place_thumbnail(&mut self, round: u32, item: usize, thumbnail: Thumbnail) -> bool {
        if self.has_drums() {
            return round == self.rolls.grid.round
                && self.rolls.drum.take(&self.ids, self.rolls.grid.selection, item, thumbnail);
        }
        let grid = &self.rolls.grid;
        let spot = grid.spot_of(item).filter(|_| round == grid.round);
        spot.is_some_and(|spot| self.rolls.store.put(&self.ids, grid, (spot, None), thumbnail, false))
    }

    // The stamp a touch tapped, with the item it stands on and the file of the small picture that shows there.
    pub(in crate::ui) fn stamp_tapped(&self, kind: Kind, touch: &Touch) -> Option<((usize, String), Stamp)> {
        if self.has_drums() {
            let band = self.rolls.grid.selection_area().filter(|_| self.is_drum_resting())?;
            let touched = (self.rolls.grid.selection, self.selection_thumbnail()?.0.file.clone());
            return self.stamp_touched(kind, band, touch).map(|stamp| (touched, stamp));
        }
        let item = tapped(&self.rolls.grid, touch)?;
        let thumbnail = self.rolls.grid.spot_of(item).and_then(|spot| self.rolls.store.get(spot))?;
        let area = picture_area(self.rolls.grid.area_of(item)?, &thumbnail.0.picture);
        self.stamp_touched(kind, area, touch).map(|stamp| ((item, thumbnail.0.file.clone()), stamp))
    }

    pub(in crate::ui) fn draw_finder(&self, pen: &mut Pen<'_, '_>, kind: Kind, now_ns: u64) {
        let (kit, quarters) = (self.kit(), self.quarters());
        let (width, height) = self.finder_size(kind);
        let (room, shutter) = (self.finder_room(), self.finder.shutter.at);
        let area = room.centre(width as i32, height as i32);
        match self.finder.review.as_ref().filter(|_| kind == Kind::Photo) {
            Some(review) => {
                pen.fill(pen.bounds(), kit.roles.background);
                review.store.draw(pen, (SOLE, Placement::Shows(0)), (area, area), |_, _, _, _| {});
            }
            None => draw_finder(pen, &kit, (&self.finder.picture, self.finder.id), area, shutter),
        }
        if let Some(start_ns) = self.finder.recording.filter(|_| kind == Kind::Video) {
            let mut time = self.run_time.borrow_mut();
            time.clear();
            write_duration(&mut time, seconds_since(start_ns, now_ns));
            draw_run_time(pen, &kit, &self.disc, &time, (area, quarters));
        }
        if let Some((note, _)) = &self.note {
            draw_note(pen, &kit, note, (Rect::new(area.x, room.y, area.w, room.h), quarters), self.slide.at);
        }
        self.draw_row(pen);
    }

    // The width of this side's picture in a video call: what the room under the bar leaves beside an upright far picture of its whole height, less a gap.
    pub(in crate::ui) fn own_width(&self) -> usize {
        let (room, (across, down)) = (self.ruler.under_bar(), UPRIGHT);
        let upright = (room.h * across + down - 1) / down;
        (room.w - upright - self.ruler.gap).max(0) as usize
    }

    // Where this side's picture stands in a video call: in the foot's right corner of the room under the bar, at its frame's size.
    fn own_area(&self) -> Rect {
        let (room, (width, height)) = (self.ruler.under_bar(), self.call_viewfinder().frame_size());
        let (width, height) = (width as i32, height as i32);
        Rect::new(room.right() - width, room.bottom() - height, width, height)
    }

    // The room the far side's picture is fitted to, clear of this side's: the room's whole width above it, or its whole height left of it, whichever a picture of the shape last shown, or of an upright one before any, stands larger in.
    pub(in crate::ui) fn far_area(&self) -> Rect {
        let (room, own, gap) = (self.ruler.under_bar(), self.own_area(), self.ruler.gap);
        let above = Rect::new(room.x, room.y, room.w, (own.y - gap - room.y).max(0));
        let beside = Rect::new(room.x, room.y, (own.x - gap - room.x).max(0), room.h);
        let (width, height) =
            self.far_picture().map_or(UPRIGHT, |picture| (picture.width as i32, picture.height() as i32));
        // The scale a room gives the shape, times its width and height.
        let reach = |area: Rect| (i64::from(area.w) * i64::from(height)).min(i64::from(area.h) * i64::from(width));
        if reach(beside) > reach(above) { beside } else { above }
    }

    // The far side's picture and where it stands: at the top left of its room, where it comes and fits there.
    pub(super) fn far_place(&self) -> Option<(&Picture, Rect)> {
        let (room, picture) = (self.far_area(), self.far_picture()?);
        let area = Rect::new(room.x, room.y, picture.width as i32, picture.height() as i32);
        (area.w <= room.w && area.h <= room.h).then_some((picture, area))
    }

    pub(in crate::ui) fn draw_call_pictures(&self, pen: &mut Pen<'_, '_>) {
        let kit = self.kit();
        match self.far_place() {
            Some((picture, area)) => draw_finder(pen, &kit, (&picture.words, self.calls.far_id), area, 0.0),
            None => pen.fill(pen.bounds(), kit.roles.background),
        }
        draw_picture(pen, &kit, (self.own_picture(), self.calls.own_id), self.own_area());
    }

    pub(in crate::ui) fn row_box(&self) -> Rect {
        let ruler = &self.ruler;
        let (width, height, gap) = ruler.block();
        let right = round(self.bar_seat().x) - ruler.gap;
        let first = right - ROW_BLOCKS as i32 * (width + gap) + gap;
        let top = ruler.bar_top() + (ruler.large() - height) / 2;
        Rect::new(first, top, right - first, height)
    }

    fn draw_row(&self, pen: &mut Pen<'_, '_>) {
        if self.do_photos_show() && self.painting.row.at > 0.0 {
            draw_progress(pen, &self.kit(), self.row_box(), self.painting.row.at, &self.painting.blocks);
        }
    }

    fn draw_header(&self, pen: &mut Pen<'_, '_>, kind: Kind, (word, has_note): (&str, bool), is_open: bool) {
        let name = roll_name(kind);
        let quarters = self.quarters();
        if !is_open || quarters == 0 {
            let words = self.roll_words(kind, word, has_note);
            return self.draw_bar(pen, name, &words);
        }
        draw_turned_bar(pen, &self.kit(), (name, word), round(self.bar_seat().width), quarters);
    }

    fn does_stamp(&self) -> bool {
        matches!(self.screen, Some(Screen::Grid(Kind::Photo) | Screen::Roll(Kind::Photo)))
    }

    fn draw_opening(&self, pen: &mut Pen<'_, '_>, share: f32) {
        let Some(place) = self.rolls.grid.selection_area() else {
            return;
        };
        let (rolls, kit) = (&self.rolls, self.kit());
        let small = self
            .selection_thumbnail()
            .filter(|thumbnail| thumbnail.0.picture.width > 0 && !thumbnail.0.picture.words.is_empty());
        let is_large = self.has_roll_picture() && (rolls.is_current || small.is_none());
        let large = is_large.then_some((&rolls.picture, rolls.id));
        let opening = Opening { place, view: self.roll_area(), small, large, quarters: self.quarters(), share };
        opening.draw(pen, &kit, |pen, visible, window| {
            if self.does_stamp() {
                rolls.stamps.draw(pen, &kit, Kind::Photo, (visible, window), Some(rolls.grid.selection));
            }
        });
    }

    fn draw_roll(&self, pen: &mut Pen<'_, '_>) {
        let (rolls, kit, room) = (&self.rolls, self.kit(), self.roll_area());
        let lead = rolls.is_current.then_some((&rolls.picture, rolls.id));
        rolls.carousel.draw(pen, (room, self.quarters()), lead, |pen, visible, is_lead| {
            if self.screen == Some(Screen::Roll(Kind::Photo)) {
                let selection = is_lead.then_some(rolls.grid.selection);
                rolls.stamps.draw(pen, &kit, Kind::Photo, (visible, room), selection);
            }
        });
    }

    fn draw_roll_form(&self, pen: &mut Pen<'_, '_>, kind: Kind, caption: &str) {
        let (rolls, kit, has_arrow) = (&self.rolls, self.kit(), self.look().list.has_arrow);
        let cells = (&rolls.grid, &rolls.store);
        match self.look().roll.form {
            RollForm::Cells => draw_cells(pen, &kit, cells, (has_arrow, self.cells_mark()), |_, _, _, _| {}),
            RollForm::Drum => {
                let (at, room) = (self.dial.roll.at, kit.ruler.room());
                rolls.drum.draw(pen, &kit, &rolls.grid, at, |pen, area| {
                    rolls.stamps.draw(pen, &kit, kind, (area, room), Some(rolls.grid.selection));
                });
            }
            RollForm::Captioned => draw_captioned(pen, &kit, cells, has_arrow, caption),
        }
    }

    pub(in crate::ui) fn draw_gallery(&self, pen: &mut Pen<'_, '_>, kind: Kind, word: (&str, bool), caption: &str) {
        let (share, ground) = (self.rolls.grid.opening().clamp(0.0, 1.0), self.roles.background);
        pen.fill(pen.bounds(), ground);
        if share < 1.0 {
            self.draw_roll_form(pen, kind, caption);
            if !self.has_drums() {
                self.rolls.stamps.draw_on_cells(pen, &self.kit(), kind, (&self.rolls.grid, &self.rolls.store));
            }
            pen.fill(Rect::new(0, 0, self.ruler.width, self.ruler.bar()), ground);
            self.draw_header(pen, kind, word, false);
        }
        if share >= 1.0 && self.rolls.carousel.has_panes() {
            self.draw_roll(pen);
        } else if share > 0.0 {
            self.draw_opening(pen, share);
        }
        if share >= 1.0 {
            self.draw_header(pen, kind, word, true);
        }
        self.draw_row(pen);
    }

    pub(in crate::ui) fn draw_cards(
        &self,
        pen: &mut Pen<'_, '_>,
        cells: (&Grid, &Store<LinePicture>),
        name: &str,
        words: &BarWords<'_>,
    ) {
        let kit = self.kit();
        pen.fill(pen.bounds(), self.roles.background);
        draw_cells(pen, &kit, cells, (self.look().list.has_arrow, self.cells_mark()), |pen, card, at, within| {
            draw_tickers(&kit, pen, &card.tickers, at, within);
        });
        pen.fill(Rect::new(0, 0, self.ruler.width, self.ruler.bar()), self.roles.background);
        self.draw_bar(pen, name, words);
    }

    pub(in crate::ui) fn draw_link(&self, pen: &mut Pen<'_, '_>, symbol: Option<&Symbol>, caption: &str) {
        pen.fill(pen.bounds(), self.roles.background);
        draw_code(pen, &self.kit(), symbol, caption);
        pen.fill(Rect::new(0, 0, self.ruler.width, self.ruler.bar()), self.roles.background);
    }
}

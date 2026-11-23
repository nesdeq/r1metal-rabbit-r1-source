mod home;
pub(super) mod lines;
pub(super) mod passage;
mod pictures;
pub(super) mod rail;
pub(super) mod rooms;
mod seat;
pub(super) mod settle;
mod sketch;
pub(super) mod window;

use super::Ui;
use super::bar::{Bar, BarStyle, BarText, Status, Typing, WordPlace, letter_count};
use super::corner::CornerFacts;
use super::edit::{CaretMark, draw_band, draw_eye, draw_secret, eye_box, parts};
use super::field::{cells, cut};
use super::gauge::{BlockRow, ClockPlace, Gauge, Scale, Sweep};
use super::item::{line_blocks, row_sweep, typed_sweep};
use super::keys::{KeyCaps, draw_keys};
use super::list::ListForm;
use super::list::dial::figure_band;
use super::picture::finder::draw_picture;
use super::screen::bar::BarWords;
use super::screen::debug::{PACING_LINE, PACING_NAME, RUNNING};
use super::screen::typing::KeyboardId;
use super::screen::voice::RequestList;
use super::screen::{Screen, View, WindowContent};
use super::speech::write_screen_word;
use crate::base::math::round;
use crate::base::pixel::Color;
use crate::base::time::offset::OFFSETS;
use crate::gfx::Rect;
use crate::gfx::pen::Pen;
use alloc::string::String;
use lines::List;

// The share of the pane's width the name of whom a conversation or a call is with takes at most.
const TITLE_SHARE: (i32, i32) = (2, 3);

impl Ui {
    pub(super) fn visible_list(&self) -> Option<List> {
        match self.screen {
            None => self.menu_list(),
            Some(Screen::Key) => self.keying.is_listed().then_some(List::Key),
            Some(Screen::Brightness | Screen::Volume | Screen::Clock) => {
                (self.look().list.form == ListForm::Dial).then_some(List::Settings)
            }
            Some(Screen::Call) => (!self.is_video_call()).then_some(List::Call),
            Some(screen) => List::of(screen),
        }
    }

    // A value staged on its line steps as a setting's screen does, whichever way the dial rolls.
    pub(super) fn wheel_way(&self, detents: i32) -> i32 {
        let does_roll = self.does_dial_roll() && self.staged_shown().is_none();
        if does_roll || self.does_drum_show() { detents.saturating_neg() } else { detents }
    }

    fn draw_screen(&self, pen: &mut Pen<'_, '_>, now_ns: u64) {
        let mut word = String::new();
        core::mem::swap(&mut word, &mut self.word.borrow_mut());
        word.clear();
        {
            let view = self.view_of(self.screen, &mut word, now_ns);
            self.draw_view(pen, view, now_ns);
        }
        core::mem::swap(&mut word, &mut self.word.borrow_mut());
    }

    // The row of blocks the microphone's level stands on, on the screen that shows, with the colour of a block unlit where the row itself paints it: the first line of a list that records, a note's, a request to the bell or the rundowns or a call's own, whose picture holds its unlit blocks, or the row a room keeps for it.
    fn meter_place(&self) -> Option<(BlockRow, Option<Color>)> {
        let note = self.is_hearing_note().then_some(List::Memos);
        let request = RequestList::ALL.into_iter().find(|list| self.is_hearing_request(*list));
        let call = self.is_hearing_call().then_some(List::Call);
        if let Some(list) = note.or(request.map(RequestList::list)).or(call) {
            let grid = self.grid_of(list);
            let area = grid.selection_area().filter(|_| grid.selection == 0 && !self.is_dial(list))?;
            let (width, height) = grid.place_size();
            let Rect { x: left, y: top, .. } = area.centre(width as i32, height as i32);
            let inset = if self.does_cut() { self.ruler.typed_left() } else { self.ruler.framed_inset() };
            let row = line_blocks(&self.kit(), (width, height), inset);
            let first = Rect { x: left + row.first.x, y: top + row.first.y, ..row.first };
            return Some((BlockRow { first, ..row }, None));
        }
        let ((room, top), height) = (self.meter_row()?, self.ruler.small());
        let (block, low, block_gap) = self.ruler.block();
        let first = Rect::new(room.x, top + (height - low) / 2, block, low);
        let does_show = top + height > room.y && top < self.ruler.room().bottom();
        does_show.then_some((BlockRow { first, step: block + block_gap }, Some(self.roles.rule)))
    }

    fn draw_meter(&self, pen: &mut Pen<'_, '_>) {
        if let Some((row, ground)) = self.meter_place() {
            row.draw_lit(pen, self.microphone.blocks.iter().map(|block| block.at), (ground, 1.0), self.roles.text);
        }
    }

    fn draw_window_content(&self, pen: &mut Pen<'_, '_>, content: WindowContent<'_>) {
        match content {
            WindowContent::Lines(list) => self.draw_rows(pen, list),
            WindowContent::Scan => {
                let picture = (&self.finder.picture[..], self.finder.id);
                draw_picture(pen, &self.kit(), picture, self.scan_square());
            }
            WindowContent::Secret { text, is_typing, is_shown, keys } => {
                if let Some(text) = text {
                    let keyboard = self.keyboard(keys);
                    let caret = is_typing.then(|| (keyboard.shake(), keyboard.caret().1, self.caret_mark()));
                    let colour = if is_typing { self.roles.text } else { self.roles.text_dim };
                    draw_secret(pen, &self.kit(), (self.safe_field(), colour), (text, is_shown), caret);
                    draw_eye(pen, &self.kit(), is_shown);
                }
                if is_typing {
                    draw_keys(pen, &self.kit(), self.keyboard(keys), self.key_caps());
                }
            }
        }
    }

    fn safe_field(&self) -> Rect {
        let (room, ruler) = (self.ruler.under_bar(), &self.ruler);
        Rect::new(room.x, ruler.bar() + ruler.gap, room.w, ruler.large())
    }

    fn caret_mark(&self) -> CaretMark {
        CaretMark { style: self.look().caret, is_lit: self.caret.is_lit }
    }

    fn key_caps(&self) -> KeyCaps<'_> {
        KeyCaps { style: self.look().keys, icons: &self.key_icons }
    }

    fn draw_band(&self, pen: &mut Pen<'_, '_>, id: KeyboardId, drop: f32) {
        let is_up = self.keys_up() == Some(id);
        draw_band(pen, &self.kit(), (self.keyboard(id), self.key_caps()), self.caret_mark(), is_up, drop);
    }

    pub(super) fn is_on_eye(&self, (start, at): ((i32, i32), (i32, i32))) -> bool {
        let eye = eye_box(&self.ruler);
        eye.contains(start.0, start.1) && eye.contains(at.0, at.1)
    }

    fn draw_text_room(
        &self,
        pen: &mut Pen<'_, '_>,
        room: super::textroom::RoomPicture<'_>,
        keys: Option<(KeyboardId, f32)>,
        is_lit: Option<bool>,
    ) {
        room.draw(pen, self.ruler.room());
        self.draw_meter(pen);
        self.draw_scroll_mark(pen);
        if let Some((id, drop)) = keys {
            self.draw_band(pen, id, drop);
        }
        if let Some(is_lit) = is_lit {
            self.draw_disc(pen, is_lit);
        }
        pen.fill(Rect::new(0, 0, self.ruler.width, self.ruler.bar()), self.roles.background);
    }

    fn word_place(&self) -> WordPlace<(Option<Screen>, usize)> {
        WordPlace {
            place: (self.screen, self.tree.front()),
            is_beneath: self.is_beneath,
            may_unfold: !self.does_cut() && self.typing == Typing::Done,
        }
    }

    pub(in crate::ui) fn cut_title<'a>(&self, name: &'a str) -> &'a str {
        let (share, of) = TITLE_SHARE;
        cut(name, cells(self.ruler.pane_width() * share / of, &self.ruler.heading))
    }

    fn beside_robot_width(&self) -> i32 {
        (round(self.robot_seat().x) - self.ruler.under_bar().x - self.ruler.gap).max(0)
    }

    pub(super) fn beside_robot_room(&self) -> usize {
        cells(self.beside_robot_width(), &self.ruler.body)
    }

    fn write_bar_text(&self, style: BarStyle, words: &BarWords<'_>, text: &mut String) {
        match style.text {
            BarText::Trail => self.write_trail(text),
            BarText::Speech => self.write_speech(words, text),
            BarText::Plain => {
                let figure = words.figure.filter(|_| style.has_figure);
                let selection = words.selection.filter(|_| style.has_selection);
                let place = words.place.filter(|_| selection.is_none());
                let status = words.status.or(words.silent_status).or(figure);
                write_screen_word(status, place, selection.or(words.subject), text);
            }
        }
    }

    fn draw_bar(&self, pen: &mut Pen<'_, '_>, name: &str, words: &BarWords<'_>) {
        let style = self.look().bar;
        let mut text = self.bar_text.borrow_mut();
        text.clear();
        self.write_bar_text(style, words, &mut text);
        let typing = match (style.text, self.typing) {
            (BarText::Speech, Typing::Due) => Some(""),
            (BarText::Speech, Typing::Start(start_ns)) => {
                let count = letter_count(start_ns, self.ticking.tick_ns);
                Some(text.char_indices().nth(count).map_or(text.as_str(), |entry| &text[..entry.0]))
            }
            _ => None,
        };
        let bar = Bar { name, text: &text, typing, room: self.beside_robot_width() };
        bar.draw(pen, &self.kit(), style.text, &self.bar_word, self.word_place());
        drop(text);
        if style.text == BarText::Trail && self.does_status_show() {
            self.draw_status(pen, words);
        }
    }

    pub(super) fn note_word_written(&mut self) {
        let place = self.word_place();
        self.bar_word.note_written(&self.bar_text.borrow(), place);
    }

    pub(super) fn does_status_show(&self) -> bool {
        self.look().has_status
            && !matches!(self.screen, Some(Screen::Finder(_) | Screen::Roll(_) | Screen::Keyboard | Screen::Pin))
            && !self.is_typing_text()
    }

    fn draw_status(&self, pen: &mut Pen<'_, '_>, words: &BarWords<'_>) {
        let mut text = self.bar_text.borrow_mut();
        text.clear();
        match words.state {
            Some(state) => text.push_str(state),
            None => self.write_state(&mut text),
        }
        let state_end = text.len();
        // A reversed line marks the place itself.
        let is_marked = self.visible_list().is_some_and(|list| self.is_reversed(list));
        let place = words.place.filter(|_| !is_marked);
        let is_paged = self.look().bar.has_pager;
        let has_words = (is_paged && self.write_more(&mut text))
            || write_screen_word(words.status.or(words.silent_status), place, words.subject, &mut text);
        let status = Status {
            state: &text[..state_end],
            words: has_words.then(|| &text[state_end..]),
            clock: self.clock.as_ref().map(|clock| (clock.0.as_str(), clock.1.as_str())),
        };
        status.draw(pen, &self.kit());
    }

    fn draw_password(
        &self,
        pen: &mut Pen<'_, '_>,
        (name, words): (&str, &BarWords<'_>),
        keys: KeyboardId,
        is_shown: bool,
    ) {
        let (ruler, roles, keyboard) = (&self.ruler, self.roles, self.keyboard(keys));
        pen.fill(pen.bounds(), roles.background);
        let (cell, line) = (ruler.cell(), ruler.large());
        let (left, top) = (ruler.under_bar().x + keyboard.shake(), ruler.bar() + ruler.gap);
        let (row, column, kit) = (keyboard.caret().0, keyboard.caret().1, self.kit());
        let caret = Rect::new(left + round(column * cell as f32), top + row as i32 * line, cell, line);
        self.caret_mark().draw(pen, &kit, caret);
        for (index, part) in parts(keyboard.text(), keyboard.line_length()).enumerate() {
            let area = Rect::new(left, top + index as i32 * line, (ruler.width - left).max(0), line);
            draw_secret(pen, &kit, (area, roles.text), (part, is_shown), None);
        }
        draw_eye(pen, &kit, is_shown);
        self.draw_bar(pen, name, words);
        draw_keys(pen, &kit, keyboard, self.key_caps());
    }

    fn draw_gauge_view(&self, pen: &mut Pen<'_, '_>, name: &str, words: &BarWords<'_>, gauges: [Option<Gauge<'_>>; 2]) {
        if let Some(list) = self.dial_list() {
            return self.draw_lines(pen, list, name, words);
        }
        pen.fill(pen.bounds(), self.roles.background);
        let count = gauges.iter().flatten().count();
        for (at, gauge) in gauges.iter().flatten().enumerate() {
            gauge.draw(pen, &self.kit(), &self.layout().place_gauge(&self.ruler, gauge, at, count));
        }
        self.draw_bar(pen, name, words);
    }

    fn clock_place(&self) -> ClockPlace {
        self.layout().place_clock(&self.ruler)
    }

    pub(super) fn mark_aim(&self) -> f32 {
        let (first, _, step) = self.clock_place().tick_row(&self.ruler, OFFSETS.len() as i32);
        let place = OFFSETS.partition_point(|offset| *offset < self.settings.offset).min(OFFSETS.len() - 1);
        (first + place as i32 * step) as f32
    }

    fn time_top(&self) -> i32 {
        if self.dial_list().is_some() {
            let room = self.ruler.room();
            return room.y + (room.h - self.ruler.large()) / 2;
        }
        self.clock_place().time_top
    }

    // True while the corner's time, battery, network and bell show where the theme puts them.
    pub(in crate::ui) fn does_corner_show(&self) -> bool {
        self.do_stages_show() || self.is_menu_home() || self.does_rail_show()
    }

    pub(super) fn is_on_battery(&self, (start, at): ((i32, i32), (i32, i32))) -> bool {
        let is_on = |area: Rect| area.contains(start.0, start.1) && area.contains(at.0, at.1);
        self.charge.is_plugged && self.does_corner_show() && self.look().corner.area(&self.ruler).is_some_and(is_on)
    }

    fn draw_corner(&self, pen: &mut Pen<'_, '_>) {
        let facts = CornerFacts {
            clock: self.clock.as_ref().map(|clock| (clock.0.as_str(), clock.1.as_str())),
            link: self.link,
            bell: self.bell(),
            bell_time: self.bell_time(),
            is_low: self.charge.is_low,
            is_on_cable: self.charge.is_on_cable,
        };
        self.corner.draw(pen, &self.kit(), self.look().corner, &facts);
    }

    fn draw_clock_view(&self, pen: &mut Pen<'_, '_>, name: &str, words: &BarWords<'_>, scale: &Scale<'_>) {
        if let Some(list) = self.dial_list() {
            return self.draw_lines(pen, list, name, words);
        }
        pen.fill(pen.bounds(), self.roles.background);
        scale.draw(pen, &self.kit(), &self.clock_place());
        self.draw_bar(pen, name, words);
    }

    pub(super) fn draw_robot(&self, pen: &mut Pen<'_, '_>) {
        if self.robot_place.is_shown {
            self.robot.draw(pen, &self.roles, &self.seat());
        }
    }

    fn does_caret_show(&self) -> bool {
        let has_prompt = self.does_chat_show() && self.does_chat_prompt(false);
        self.look().list.form == ListForm::Typed
            && !self.is_beneath
            && (self.visible_list().is_some() || has_prompt || self.is_typing())
    }

    fn draw_view(&self, pen: &mut Pen<'_, '_>, view: View<'_>, now_ns: u64) {
        match view {
            View::Home { sentence } => self.draw_home(pen, sentence),
            View::Lines { list, name, words } => {
                self.draw_lines(pen, list, name, &words);
                self.draw_meter(pen);
            }
            View::Debug { list, name, words } => {
                self.draw_lines(pen, list, name, &words);
                self.draw_sweep(pen, now_ns);
            }
            View::Cards { grid, store, name, words } => self.draw_cards(pen, (grid, store), name, &words),
            View::Palettes { name, words } => {
                if self.has_cards(List::Colours)
                    && let Some(store) = self.store_of(List::Colours)
                {
                    return self.draw_cards(pen, (&self.themes.palettes, store), name, &words);
                }
                self.draw_lines(pen, List::Colours, name, &words);
            }
            View::Gauges { name, words, gauges } => self.draw_gauge_view(pen, name, &words, gauges),
            View::Clock { name, words, scale } => self.draw_clock_view(pen, name, &words, &scale),
            View::Text { name, words, room, keys } => {
                self.draw_text_room(pen, room, keys, None);
                self.draw_bar(pen, name, &words);
            }
            View::Conversation { name, words, room, keys, is_lit } => {
                self.draw_text_room(pen, room, keys, is_lit);
                self.draw_bar(pen, self.cut_title(name), &words);
            }
            View::Call { name, words, is_video: true } => {
                self.draw_call_pictures(pen);
                self.draw_bar(pen, self.cut_title(name), &words);
            }
            View::Call { name, words, is_video: false } => {
                self.draw_lines(pen, List::Call, self.cut_title(name), &words);
                self.draw_meter(pen);
            }
            View::Chat { name, words, caption, hint } => self.draw_chat(pen, (name, &words), (caption, hint)),
            View::Password { name, words, keys, is_shown } => self.draw_password(pen, (name, &words), keys, is_shown),
            View::Window { words, content } => {
                self.draw_window(pen, &words, |ui, pen| ui.draw_window_content(pen, content));
            }
            View::Link { name, words, code, caption } => {
                self.draw_link(pen, code, caption);
                self.draw_bar(pen, name, &words);
            }
            View::Finder(kind) => self.draw_finder(pen, kind, now_ns),
            View::Gallery { kind, word, has_note, caption } => self.draw_gallery(pen, kind, (word, has_note), caption),
        }
    }

    fn draw_sweep(&self, pen: &mut Pen<'_, '_>, now_ns: u64) {
        if !self.debug.pacing.is_on {
            return;
        }
        let (ruler, kit) = (&self.ruler, self.kit());
        let is_typed = self.does_cut();
        let sweep = if self.dial_list().is_some() {
            let room = ruler.room();
            let foot = room.y + room.h / 2 + figure_band(ruler) / 2;
            Sweep { start: ruler.gap, end: ruler.width - ruler.gap, top: foot - ruler.gap - ruler.block().1 }
        } else {
            let grid = &self.debug.grid;
            let is_selected = grid.selection == PACING_LINE;
            let Some(area) = (if is_selected { grid.selection_area() } else { grid.area_of(PACING_LINE) }) else {
                return;
            };
            let size = grid.size_at(PACING_LINE);
            let Rect { x: left, y: top, .. } = area.centre(size.0 as i32, size.1 as i32);
            let line =
                if is_typed { typed_sweep(&kit, (PACING_NAME, RUNNING), size) } else { row_sweep(&kit, RUNNING, size) };
            Sweep { start: left + line.start, end: left + line.end, top: top + line.top }
        };
        let is_reversed = is_typed && self.debug.grid.selection == PACING_LINE;
        let colour = if is_reversed { self.roles.background } else { self.roles.text };
        sweep.draw(pen, &kit, now_ns, colour);
    }
}

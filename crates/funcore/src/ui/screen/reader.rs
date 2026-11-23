use super::bar::BarWords;
use super::input::STEP_LINES;
use super::memos::{Text, body_of, day_of, entry_name};
use super::{BACKWARD, FORWARD, View};
use crate::gfx::motion::Edge;
use crate::ui::Ui;
use crate::ui::screen::Screen;
use crate::ui::textroom::{Plate, TextRoom, TextRows};
use alloc::string::String;
use core::fmt::Write;

#[derive(Default)]
pub(crate) struct Reader {
    pub(super) text: Option<Text>,
    pub(in crate::ui) rows: TextRows,
    pub(in crate::ui) room: TextRoom<(i32, usize, bool)>,
    entry_name: String,
    // The name of the text that is read.
    name: String,
}

fn write_speech(ui: &Ui, text: &mut String) {
    if let Some(day) = ui.current_memo().and_then(|memo| day_of(&memo.stem)) {
        let _ = write!(text, "Written {} {}.", day.day, day.month_name());
    }
}

impl Ui {
    pub(super) fn read_step(&self) -> i32 {
        STEP_LINES * self.ruler.small()
    }

    pub(super) fn open_reader(&mut self, text: Text) {
        self.depart(FORWARD);
        (self.reader.text, self.reader.room.at, self.reader.room.scroll) =
            (Some(text), 0, Edge::still(-self.read_step() as f32));
        self.hush();
        self.lay_script();
        self.screen = Some(Screen::Reader);
    }

    pub(super) fn script_text(&self) -> Option<&str> {
        self.current_memo().zip(self.reader.text).and_then(|(memo, text)| body_of(memo, text))
    }

    pub(crate) fn is_script_done(&self) -> bool {
        self.script_text().is_none_or(|text| self.reader.rows.is_done(text))
    }

    pub(crate) fn lay_more(&mut self, height: i32) -> bool {
        let (cells, heights) = self.ruler.text_cells();
        let mut rows = core::mem::take(&mut self.reader.rows);
        let is_more = self.script_text().is_some_and(|text| rows.add_more(text, cells, heights, height));
        self.reader.rows = rows;
        is_more
    }

    fn lay_script(&mut self) {
        self.reader.rows.clear();
        self.reader.room.plate.key = None;
        self.reader.room.plate.make_room(self.ruler.room_size());
        self.lay_more(self.reader.room.at + self.ruler.under_bar().h);
        self.reader.room.at = self.reader.room.at.min(self.read_max_scroll());
        let shelf = self.memos.shelf;
        self.reader.entry_name =
            self.memos.open_entry.as_deref().map(|stem| entry_name(shelf, stem)).unwrap_or_default();
        let label = self.current_memo().zip(self.reader.text).map(|(memo, text)| self.text_label(memo, text).0);
        self.reader.name = label.unwrap_or_default();
    }

    pub(super) fn reflow_reader(&mut self) {
        let is_gone = self.script_text().is_none();
        if is_gone {
            self.depart(BACKWARD);
            self.screen = Some(Screen::Memo);
        }
        self.lay_script();
        self.reader.room.plate.renew(self.ruler.room_size(), !is_gone);
    }

    pub(crate) fn land_reader(&mut self) {
        self.reader.room.scroll = Edge::still(self.reader.room.at as f32);
        self.reader.room.plate.land();
    }

    pub(super) fn view_reader(&self) -> View<'_> {
        let subject = Some(self.reader.entry_name.as_str());
        let words = BarWords { status: self.aloud_word(), subject, speech: Some(write_speech), ..BarWords::default() };
        View::Text { name: &self.reader.name, words, room: self.reader.room.plate.as_room(), keys: None }
    }
}

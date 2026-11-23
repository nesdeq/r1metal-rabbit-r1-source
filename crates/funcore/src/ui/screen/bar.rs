use super::Screen;
use crate::domain::cost::MONTH;
use crate::domain::scene::shelf_names;
use crate::ui::Ui;
use crate::ui::speech::{push_lowercase, write_place, write_robot_sentence};
use crate::wifi::radio::Way;
use alloc::string::String;
use core::fmt::Write;

const STATUS_CABLE: &str = " CABLE";
const STATUS_WIFI: &str = " WIFI";
const STATUS_MOBILE: &str = " MOBILE";
const STATUS_MORE: &str = "--More--";
const PERCENT: i64 = 100;
const HOME: &str = "home";

#[derive(Clone, Copy, Default)]
pub(crate) struct BarWords<'a> {
    pub(crate) status: Option<&'a str>,
    pub(crate) silent_status: Option<&'a str>,
    pub(crate) figure: Option<&'a str>,
    pub(crate) selection: Option<&'a str>,
    pub(crate) subject: Option<&'a str>,
    pub(crate) place: Option<(usize, usize)>,
    pub(crate) speech: Option<fn(&Ui, &mut String)>,
    pub(crate) state: Option<&'a str>,
}

impl Ui {
    pub(in crate::ui) fn write_speech(&self, words: &BarWords<'_>, text: &mut String) {
        match (words.status, words.speech) {
            (Some(status), _) => write_robot_sentence(status, text),
            (None, Some(speech)) => speech(self, text),
            (None, None) => {}
        }
        if !text.is_empty() {
            return;
        }
        match (words.figure, words.subject, words.place) {
            (Some(figure), ..) => write_robot_sentence(figure, text),
            (None, Some(subject), _) => text.push_str(subject),
            (None, None, Some((at, count))) => write_place(text, at, count),
            (None, None, None) => {}
        }
    }

    pub(in crate::ui) fn write_trail(&self, text: &mut String) {
        let path = &self.tree.path;
        let index = path.len().saturating_sub(1 + usize::from(self.screen.is_none()));
        let mut part = path.get(index).filter(|_| index > 0).and_then(|level| self.tree.name_of(*level));
        let parent = self.screen.and_then(Screen::parent);
        if parent.is_some() {
            part = self.tree.level().node(self.tree.level().selection()).map(|node| node.name).or(part);
        }
        if parent.and_then(Screen::parent).is_some() {
            part = Some(if self.screen == Some(Screen::Share) { MONTH } else { shelf_names(self.memos.shelf).1 });
        }
        push_lowercase(text, part.unwrap_or(HOME));
    }

    pub(in crate::ui) fn write_state(&self, text: &mut String) {
        let reading = self.charge.reading;
        if let Some((level, _)) = reading {
            let _ = write!(text, "{level}%");
        }
        let is_plugged = reading.is_some_and(|reading| reading.1.is_plugged());
        text.push_str(match (is_plugged, self.link) {
            (true, _) => STATUS_CABLE,
            (false, Some(Way::Wifi)) => STATUS_WIFI,
            (false, Some(Way::Cell)) => STATUS_MOBILE,
            (false, None) => "",
        });
        self.write_bell(text);
        if text.starts_with(' ') {
            text.remove(0);
        }
    }

    pub(in crate::ui) fn write_more(&self, text: &mut String) -> bool {
        let share = match (self.visible_grid(), self.text_room()) {
            (Some(grid), _) if grid.has_more() => None,
            (None, Some((room, height, offset))) if offset + room.h < height => {
                Some(i64::from(offset + room.h) * PERCENT / i64::from(height.max(1)))
            }
            _ => return false,
        };
        text.push_str(STATUS_MORE);
        if let Some(share) = share {
            let _ = write!(text, "({share}%)");
        }
        true
    }
}

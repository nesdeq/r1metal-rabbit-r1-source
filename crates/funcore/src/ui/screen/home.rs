use super::View;
use super::bar::BarWords;
use super::tree::MAX_NAMED_CHILDREN;
use crate::base::text::capitalize_at;
use crate::domain::scene::{EVERYTHING, SETTINGS};
use crate::ui::Ui;
use crate::ui::field::{cells, sentence_count};
use crate::ui::gauge::Gauge;
use crate::ui::kit::find_glyph;
use crate::ui::ruler::SENTENCE_LINES;
use crate::ui::speech::push_lowercase;
use crate::ui::view::lines::List;
use alloc::string::String;
use core::fmt::Write;

const MORNING: u8 = 5;
const AFTERNOON: u8 = 12;
const EVENING: u8 = 18;
const NIGHT: u8 = 22;
const OR: &str = " or ";
const OR_MORE: &str = " or more?";

fn add_if_fits(greeting: &mut String, room: usize, sentence: core::fmt::Arguments<'_>) {
    let length = greeting.len();
    let _ = greeting.write_fmt(sentence);
    if sentence_count(greeting, room) > SENTENCE_LINES {
        greeting.truncate(length);
    }
}

fn write_menu_speech(ui: &Ui, text: &mut String) {
    match ui.tree.name_of(ui.tree.front()).unwrap_or_default() {
        SETTINGS => match ui.networks.first_known() {
            "" => text.push_str("All is set. No WiFi known."),
            known => {
                let _ = write!(text, "All is set. WiFi: {known}.");
            }
        },
        EVERYTHING => text.push_str("Everything I can do."),
        _ => {
            let menu = ui.tree.level();
            if menu.visible.len() > MAX_NAMED_CHILDREN {
                return;
            }
            let (start, count, room) = (text.len(), menu.visible.len(), ui.beside_robot_room());
            for name_count in (1..=count).rev() {
                text.truncate(start);
                for (at, place) in menu.visible.iter().take(name_count).enumerate() {
                    text.push_str(match at {
                        0 => "",
                        _ if name_count == count && at + 1 == count => OR,
                        _ => ", ",
                    });
                    push_lowercase(text, menu.nodes[*place].name);
                }
                text.push_str(if name_count == count { "?" } else { OR_MORE });
                if text[start..].chars().count() <= room {
                    break;
                }
            }
            capitalize_at(text, start);
        }
    }
}

impl Ui {
    pub(crate) fn sentence_left(&self) -> i32 {
        self.ruler.gap + self.ruler.framed_inset()
    }

    pub(crate) fn sentence_room(&self) -> usize {
        cells(self.ruler.width - 2 * self.sentence_left(), &self.ruler.body)
    }

    fn write_greeting(&self, greeting: &mut String) {
        let hour = self.local_now().map(|now| now.hour);
        greeting.push_str(match hour {
            Some(hour) if (MORNING..AFTERNOON).contains(&hour) => "Good morning.",
            Some(hour) if (AFTERNOON..EVENING).contains(&hour) => "Good afternoon.",
            Some(hour) if (EVENING..NIGHT).contains(&hour) => "Good evening.",
            Some(_) => "Good night.",
            None => "Hello.",
        });
        let room = self.sentence_room();
        let reading = self.charge.reading;
        let (level, is_plugged) =
            (reading.map(|reading| reading.0), reading.is_some_and(|reading| reading.1.is_plugged()));
        if let Some(level) = level.filter(|_| self.charge.is_low) {
            add_if_fits(greeting, room, format_args!(" I am low, at {level}%. A cable soon?"));
        }
        match self.signal_waiting() {
            (_, 0) => {}
            (name, 1) => add_if_fits(greeting, room, format_args!(" {name} wrote.")),
            (_, people) => add_if_fits(greeting, room, format_args!(" {people} people wrote.")),
        }
        if self.tally.has_edition {
            add_if_fits(greeting, room, format_args!(" Today's news is ready."));
        }
        match self.tally.notes {
            0 => {}
            1 => add_if_fits(greeting, room, format_args!(" I keep a note.")),
            notes => add_if_fits(greeting, room, format_args!(" I keep {notes} notes.")),
        }
        match level {
            Some(level) if is_plugged => add_if_fits(greeting, room, format_args!(" I am on the cable, at {level}%.")),
            Some(level) if !self.charge.is_low => add_if_fits(greeting, room, format_args!(" I am at {level}%.")),
            _ => {}
        }
    }

    // What the rail's line holds, beside the rail: the lines of the level it opens, or the line itself, large.
    fn view_rail_line<'a>(&'a self, word: &'a mut String) -> View<'a> {
        let (level, front) = (self.tree.level(), self.tree.front());
        let node = level.node(level.selection());
        if let Some(node) = node {
            self.write_node_word(node, word);
        }
        let word: &'a str = word;
        let words = BarWords { subject: Some(word).filter(|word| !word.is_empty()), ..BarWords::default() };
        if let Some(shown) = self.pane_level() {
            let name = self.tree.name_of(shown).unwrap_or_default();
            return View::Lines { list: List::Menu(shown), name, words };
        }
        match node.and_then(|node| Some((node, find_glyph(&self.glyphs, node.icon)?))) {
            Some((node, icon)) => {
                let gauge = Gauge { icon, name: node.name, value: word, blocks: &[], is_set: true, sway: 0.0 };
                View::Gauges { name: node.name, words, gauges: [Some(gauge), None] }
            }
            None => {
                let name = self.tree.name_of(front).unwrap_or_default();
                View::Lines { list: List::Menu(front), name, words }
            }
        }
    }

    pub(super) fn view_menus<'a>(&'a self, sentence: &'a mut String) -> View<'a> {
        let (level, front) = (self.tree.level(), self.tree.front());
        if level.is_rail() {
            return self.view_rail_line(sentence);
        }
        if front == 0 || level.stage().is_some() {
            self.write_greeting(sentence);
            let sentence: &'a str = sentence;
            return View::Home { sentence };
        }
        let words = BarWords {
            place: self.place_in(List::Menu(front)),
            selection: level.node(level.selection()).map(|node| node.name),
            speech: Some(write_menu_speech),
            ..BarWords::default()
        };
        let name = self.tree.name_of(front).unwrap_or_default();
        View::Lines { list: List::Menu(front), name, words }
    }
}

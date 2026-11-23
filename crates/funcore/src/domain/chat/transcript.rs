//! A day of the conversation as a readable Markdown file.

use super::InputKind;
use crate::base::text::to_one_line;
use crate::base::time::Moment;
use crate::base::time::stamp::is_compact_day;
use crate::domain::paper::names::TEXT_EXTENSION;
use crate::domain::paper::sheet::HEAD_END;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const DEFAULT_NAME: &str = "Robot";
const MARK_OPEN: &str = "<!-- ";
const MARK_CLOSE: &str = " -->";
const COMMENT_OPEN: &str = "<!--";
const COMMENT_ESCAPE: &str = "<! --";

/// The source of an entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    User(InputKind),
    // A photo the user showed with the message before it; it has no text.
    Shown,
    Robot,
    Searches,
    Sources,
}

impl EntryKind {
    const fn to_word(self) -> &'static str {
        match self {
            EntryKind::User(InputKind::Voice) => "you spoken",
            EntryKind::User(InputKind::Text) => "you typed",
            EntryKind::Shown => "shown",
            EntryKind::Robot => "robot",
            EntryKind::Searches => "searched",
            EntryKind::Sources => "sources",
        }
    }

    fn parse(word: &str) -> Option<EntryKind> {
        match word {
            "you spoken" => Some(EntryKind::User(InputKind::Voice)),
            "you typed" => Some(EntryKind::User(InputKind::Text)),
            "shown" => Some(EntryKind::Shown),
            "robot" => Some(EntryKind::Robot),
            "searched" => Some(EntryKind::Searches),
            "sources" => Some(EntryKind::Sources),
            _ => None,
        }
    }

    const fn is_list(self) -> bool {
        matches!(self, EntryKind::Searches | EntryKind::Sources)
    }
}

/// An entry of a day; the text of searches and sources holds a line each.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub kind: EntryKind,
    /// The time of day as `14:02`.
    pub time: String,
    pub text: String,
}

impl Entry {
    /// An entry at `now`'s time of day.
    pub fn new(kind: EntryKind, now: &Moment, text: &str) -> Entry {
        Entry { kind, time: now.to_time_text(), text: text.trim().into() }
    }
}

pub(super) fn to_source_line(title: &str, url: &str) -> String {
    let title = to_one_line(title).replace(['[', ']'], "");
    let url = url.trim();
    if title.is_empty() { url.into() } else { format!("[{title}]({url})") }
}

pub fn file_name(day: &str) -> String {
    format!("{day}.{TEXT_EXTENSION}")
}

pub fn parse_file_name(name: &str) -> Option<&str> {
    name.strip_suffix(TEXT_EXTENSION)?.strip_suffix('.').filter(|day| is_compact_day(day))
}

fn to_file_head(moment: &Moment) -> String {
    format!(
        "# Conversation of {}\n\nWhat the robot and its owner said and typed to each other that day, and what the robot looked up.\n{HEAD_END}",
        moment.to_date_words(true, true)
    )
}

// The entry under its bold label.
fn to_labelled(entry: &Entry, robot: &str) -> String {
    let time = &entry.time;
    let label = match entry.kind {
        EntryKind::User(InputKind::Voice) => format!("**You**, {time}, spoken"),
        EntryKind::User(InputKind::Text) => format!("**You**, {time}, typed"),
        EntryKind::Robot => format!("**{}**, {time}", robot.trim()),
        EntryKind::Shown => String::from("*Photo shown*"),
        EntryKind::Searches => String::from("*Searched the web:*"),
        EntryKind::Sources => String::from("*Sources:*"),
    };
    let text = entry.text.trim().replace(COMMENT_OPEN, COMMENT_ESCAPE);
    if text.is_empty() {
        return format!("{label}\n");
    }
    let body: String = if entry.kind.is_list() {
        text.lines().map(str::trim).filter(|line| !line.is_empty()).map(|line| format!("- {line}\n")).collect()
    } else {
        format!("{text}\n")
    };
    format!("{label}\n\n{body}")
}

/// The text an entry appends to the day's file, the file's head first where it has none.
pub fn to_file_text(now: &Moment, entry: &Entry, robot: Option<&str>, has_head: bool) -> String {
    let head = if has_head { String::new() } else { to_file_head(now) };
    let mark = format!("\n{MARK_OPEN}{} {}{MARK_CLOSE}\n", entry.kind.to_word(), entry.time);
    head + &mark + &to_labelled(entry, robot.unwrap_or(DEFAULT_NAME))
}

/// A day's entries as its screen reads them, each under its label.
pub fn to_reading(entries: &[Entry], robot: Option<&str>) -> String {
    let robot = robot.unwrap_or(DEFAULT_NAME);
    let parts: Vec<String> = entries.iter().map(|entry| to_labelled(entry, robot)).collect();
    parts.join("\n")
}

fn parse_mark(line: &str) -> Option<(EntryKind, &str)> {
    let mark = line.trim().strip_prefix(MARK_OPEN)?.strip_suffix(MARK_CLOSE)?;
    let (word, time) = mark.rsplit_once(' ')?;
    Some((EntryKind::parse(word)?, time))
}

/// The entries of a day's file.
pub fn parse(file: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut has_passed_label = false;
    for line in file.lines() {
        if let Some((kind, time)) = parse_mark(line) {
            entries.push(Entry { kind, time: time.into(), text: String::new() });
            has_passed_label = false;
            continue;
        }
        let Some(entry) = entries.last_mut() else {
            continue;
        };
        if !has_passed_label {
            has_passed_label = !line.trim().is_empty();
            continue;
        }
        let text = if entry.kind.is_list() { line.trim().strip_prefix("- ") } else { Some(line) };
        if let Some(text) = text {
            entry.text.push_str(text);
            entry.text.push('\n');
        }
    }
    for entry in &mut entries {
        entry.text = entry.text.trim().into();
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_159_320;

    #[test]
    fn file_round_trip() {
        let now = Moment::from_seconds(NOW);
        let entries = [
            Entry::new(EntryKind::User(InputKind::Voice), &now, " Und morgen?\n\nRegen? "),
            Entry::new(EntryKind::Shown, &now, ""),
            Entry::new(EntryKind::Searches, &now, "wetter wien"),
            Entry::new(EntryKind::Robot, &now, "Regen ab Mittag."),
            Entry::new(EntryKind::Sources, &now, "[Wetter](https://a.at)\nhttps://b.at"),
            Entry::new(EntryKind::User(InputKind::Text), &now, "Danke"),
        ];
        let mut file = String::new();
        for (index, entry) in entries.iter().enumerate() {
            file += &to_file_text(&now, entry, Some("Rabbit"), index > 0);
        }
        assert!(file.starts_with(
            "# Conversation of Friday 16 October 2026\n\nWhat the robot and its owner said and typed to each other that day, and what the robot looked up.\n\n---\n\n<!-- you spoken 14:02 -->\n**You**, 14:02, spoken\n\nUnd morgen?\n\nRegen?\n"
        ));
        assert!(file.contains("**Rabbit**, 14:02\n"));
        assert_eq!(parse(&file), entries);
        assert!(file.contains("<!-- shown 14:02 -->\n*Photo shown*\n\n<!-- searched 14:02 -->"), "a label alone");
        assert_eq!(
            to_reading(&entries[2..4], None),
            "*Searched the web:*\n\n- wetter wien\n\n**Robot**, 14:02\n\nRegen ab Mittag.\n"
        );
    }

    #[test]
    fn comment_in_text_escaped() {
        let now = Moment::from_seconds(NOW);
        let entry = Entry::new(EntryKind::Robot, &now, "<!-- robot 09:00 -->");
        let file = to_file_text(&now, &entry, None, true);
        assert!(file.contains("**Robot**, 14:02"));
        assert_eq!(parse(&file).len(), 1);
    }

    #[test]
    fn file_name_tells_day() {
        assert_eq!(file_name("20261016"), "20261016.md");
        assert_eq!(parse_file_name("20261016.md"), Some("20261016"));
        assert_eq!((parse_file_name("notes.md"), parse_file_name("20261016.txt")), (None, None));
    }

    #[test]
    fn source_line_link_or_url() {
        assert_eq!(to_source_line(" A [b] ", " https://a.at "), "[A b](https://a.at)");
        assert_eq!(to_source_line(" ", " https://a.at "), "https://a.at");
    }
}

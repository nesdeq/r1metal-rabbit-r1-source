//! Notes, conversations and news editions as listed entries.

pub mod state;
pub mod work;

use super::news::topics;
use super::paper::names::{directory, edition_folder, folder, rundown_file, rundown_name, summary_file, words_file};
use super::paper::sheet;
use super::shelf::Shelf;
use super::tally::Tally;
use crate::openai::ledger::SummaryKind;
use crate::storage::paths::Listed;
use crate::storage::{Files, NEWS};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The log line of a recording refused for a full partition.
pub const FULL_LINE: &str = "notes: no recording: the data partition is nearly full";

/// The log line of a recording's start.
pub fn to_recording_line(stem: &str) -> String {
    format!("notes: recording {stem}")
}

/// The log line of a recorded sound file.
pub fn to_recorded_line(name: &str, packets: usize, bytes: u64) -> String {
    format!("notes: {name}: {packets} packets of sound, {bytes} bytes")
}

/// The log line of a recording with no sound.
pub fn to_unheard_line(stem: &str) -> String {
    format!("notes: {stem}: nothing was heard, no note is kept")
}

/// The log line of a deleted entry.
pub fn to_deleted_line(stem: &str, kind: SummaryKind) -> String {
    format!("notes: {stem} deleted from {}", directory(kind))
}

/// An entry of a shelf: a note, a conversation, an edition, a day of the robot's conversation or a fact it keeps.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Memo {
    /// The file name without extension; an edition's or a conversation's day; a fact's line as its file keeps it.
    pub stem: String,
    /// A note's length.
    pub seconds: u64,
    /// The transcript as Markdown; a day's words as read.
    pub words: Option<String>,
    /// The summary as Markdown; a day's recollection; a fact's text.
    pub summary: Option<String>,
    /// An edition's rundowns, each by its name with its Markdown.
    pub rundowns: Vec<(String, String)>,
}

/// A request of the notes and conversations screens.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Request {
    // Begins a recording, or ends the running one.
    Record,
    // Plays the named note, or stops it.
    Play(String),
    Delete(SummaryKind, String),
}

impl Request {
    /// The entry a delete removes.
    pub fn delete_target(&self) -> Option<(SummaryKind, &str)> {
        match self {
            Request::Delete(kind, stem) => Some((*kind, stem)),
            Request::Record | Request::Play(_) => None,
        }
    }
}

/// Note lengths by stem, cached between listings.
#[derive(Default)]
pub struct Lengths(Vec<(String, u64)>);

impl Lengths {
    fn seconds_of(&mut self, stem: &str, mut seconds: impl FnMut(&str) -> Option<u64>) -> Option<u64> {
        if let Some(entry) = self.0.iter().find(|entry| entry.0 == stem) {
            return Some(entry.1);
        }
        let seconds = seconds(stem)?;
        self.0.push((stem.into(), seconds));
        Some(seconds)
    }

    fn keep_only(&mut self, stems: &[String]) {
        self.0.retain(|entry| stems.contains(&entry.0));
    }
}

// A sheet's text without its head; none where it is missing or cannot be read.
fn read_body(files: &mut impl Files, place: &str, name: &str) -> Option<String> {
    files.read(place, name).ok().flatten().map(|file| sheet::body_of(&file))
}

// The summary of an entry of that kind, by its stem.
pub(super) fn read_summary(files: &mut impl Files, kind: SummaryKind, stem: &str) -> Option<String> {
    read_body(files, &folder(kind, stem), &summary_file(stem))
}

// The words of an entry of that kind, by its stem: a note's transcript, a conversation's remarks.
pub(super) fn read_words(files: &mut impl Files, kind: SummaryKind, stem: &str) -> Option<String> {
    read_body(files, &folder(kind, stem), &words_file(kind, stem))
}

/// Lists a shelf's entries newest first: notes and conversations from their directory, without the note being recorded, the editions from theirs; `seconds` reads a note's length from its sound. The robot's shelves are the conversation's to list.
pub fn list<F: Files>(
    shelf: Shelf,
    mut files: F,
    recording: Option<&str>,
    lengths: &mut Lengths,
    seconds: impl FnMut(&str) -> Option<u64>,
) -> Result<Vec<Memo>, F::Error> {
    match (shelf.summary_kind(), shelf) {
        (Some(kind), _) => {
            let stems = files.names(directory(kind), Listed::Folders)?;
            Ok(list_entries(kind, stems, recording, lengths, files, seconds))
        }
        (None, Shelf::News) => Ok(list_editions(files.names(NEWS, Listed::Folders)?, files)),
        (None, _) => Ok(Vec::new()),
    }
}

// Lists a kind's entries newest first from stems oldest first, without the note being recorded.
fn list_entries(
    kind: SummaryKind,
    stems: Vec<String>,
    recording: Option<&str>,
    lengths: &mut Lengths,
    mut files: impl Files,
    mut seconds: impl FnMut(&str) -> Option<u64>,
) -> Vec<Memo> {
    if kind == SummaryKind::Note {
        lengths.keep_only(&stems);
    }
    let visible = stems.into_iter().rev().filter(|stem| Some(stem.as_str()) != recording);
    let to_memo = |stem: String| {
        let length = if kind == SummaryKind::Note { lengths.seconds_of(&stem, &mut seconds).unwrap_or(0) } else { 0 };
        let (words, summary) = (read_words(&mut files, kind, &stem), read_summary(&mut files, kind, &stem));
        Memo { stem, seconds: length, words, summary, rundowns: Vec::new() }
    };
    visible.map(to_memo).collect()
}

// The names of the rundowns a day's edition holds, the two a new device has first, then by name.
fn rundown_names<F: Files>(files: &mut F, day: &str) -> Result<Vec<String>, F::Error> {
    let files = files.names(&edition_folder(day), Listed::Files)?;
    let mut names: Vec<String> = files.iter().filter_map(|file| rundown_name(day, file)).map(String::from).collect();
    names.sort_by(|one, other| topics::compare(one, other));
    Ok(names)
}

/// True where the edition of a day holds a rundown.
pub fn has_rundown<F: Files>(mut files: F, day: &str) -> Result<bool, F::Error> {
    Ok(!rundown_names(&mut files, day)?.is_empty())
}

/// The rundowns of a day's edition, each by its name with its text; none where the day's folder cannot be read.
pub fn read_rundowns(files: &mut impl Files, day: &str) -> Vec<(String, String)> {
    let names = rundown_names(files, day).unwrap_or_default();
    let read = |name: String| {
        let text = read_body(files, &edition_folder(day), &rundown_file(day, &name))?;
        Some((name, text))
    };
    names.into_iter().filter_map(read).collect()
}

// Lists the editions newest first from days oldest first.
fn list_editions(days: Vec<String>, mut files: impl Files) -> Vec<Memo> {
    let to_memo = |day: String| {
        let rundowns = read_rundowns(&mut files, &day);
        Memo { stem: day, seconds: 0, words: None, summary: None, rundowns }
    };
    days.into_iter().rev().map(to_memo).collect()
}

/// The tally of the notes and the news for home; `seconds` reads a note's length from its sound.
pub fn count_shelves(
    notes: &[String],
    lengths: &mut Lengths,
    seconds: impl FnMut(&str) -> Option<u64>,
    has_edition: bool,
) -> Tally {
    let newest_note_s = notes.last().and_then(|stem| lengths.seconds_of(stem, seconds));
    Tally { notes: notes.len(), newest_note_s, has_edition, ..Tally::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests::Disk;

    fn disk(files: &[(&str, &str, &str)]) -> Disk {
        let file =
            |(place, name, text): &(&str, &str, &str)| ((*place).into(), (*name).into(), text.as_bytes().to_vec());
        Disk { files: files.iter().map(file).collect(), ..Disk::default() }
    }

    fn stems(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| String::from(*name)).collect()
    }

    #[test]
    fn lines_of_notes() {
        assert_eq!(to_recording_line("n"), "notes: recording n");
        assert_eq!(to_recorded_line("n.m4a", 3, 9), "notes: n.m4a: 3 packets of sound, 9 bytes");
        assert_eq!(to_unheard_line("n"), "notes: n: nothing was heard, no note is kept");
        assert_eq!(to_deleted_line("n", SummaryKind::Note), "notes: n deleted from audio/notes");
    }

    #[test]
    fn entries_newest_first() {
        let mut lengths = Lengths::default();
        let mut lookups = Vec::new();
        let place = "audio/notes/20261011_100000";
        let mut sheets = disk(&[
            (place, "20261011_100000_transcript.md", "# Note\n\n---\n\nWords.\n"),
            (place, "20261011_100000_summary.md", "Summed."),
        ]);
        let entries = list_entries(
            SummaryKind::Note,
            stems(&["20261010_100000", "20261011_100000", "20261012_100000"]),
            Some("20261012_100000"),
            &mut lengths,
            &mut sheets,
            |stem| {
                lookups.push(String::from(stem));
                (stem != "20261010_100000").then_some(42)
            },
        );
        assert_eq!(
            entries.iter().map(|memo| (memo.stem.as_str(), memo.seconds)).collect::<Vec<_>>(),
            [("20261011_100000", 42), ("20261010_100000", 0)]
        );
        assert_eq!(
            (entries[0].words.as_deref(), entries[0].summary.as_deref()),
            (Some("Words."), Some("Summed.")),
            "each sheet without its head"
        );
        assert_eq!((&entries[1].words, &entries[1].summary), (&None, &None), "a sheet that is not there is none");
        assert_eq!(lengths.seconds_of("20261011_100000", |_| panic!("it is kept")), Some(42));
        lengths.keep_only(&stems(&["20261010_100000"]));
        assert_eq!(lengths.seconds_of("20261011_100000", |_| Some(7)), Some(7));
        let no_seconds = |_: &str| panic!("no length");
        let talks = list_entries(SummaryKind::Talk, stems(&["t"]), None, &mut lengths, &mut sheets, no_seconds);
        assert_eq!((talks[0].seconds, &talks[0].words), (0, &None));
        assert_eq!(lengths.seconds_of("20261011_100000", |_| None), Some(7));
        assert_eq!(lookups, ["20261011_100000", "20261010_100000"]);
    }

    #[test]
    fn editions_newest_first() {
        let mut sheets = disk(&[
            ("news/20261012", "20261012_news.md", "20261012 news"),
            ("news/20261013", "20261013_tech.md", "Chips."),
            ("news/20261013", "20261013_finance.md", "Stocks."),
            ("news/20261013", "20261013_news.pdf", "pages"),
            ("news/20261013", "20261013_news.md", "# News\n\n---\n\n20261013 news\n"),
            ("news/20261013", "20261013_art.md", "Paint."),
        ]);
        let editions = list_editions(stems(&["20261012", "20261013"]), &mut sheets);
        let edition = |day: &str, rundowns: &[(&str, &str)]| Memo {
            stem: day.into(),
            seconds: 0,
            summary: None,
            words: None,
            rundowns: rundowns.iter().map(|(name, text)| (String::from(*name), String::from(*text))).collect(),
        };
        assert_eq!(
            editions,
            [
                edition(
                    "20261013",
                    &[("news", "20261013 news"), ("finance", "Stocks."), ("art", "Paint."), ("tech", "Chips.")]
                ),
                edition("20261012", &[("news", "20261012 news")])
            ],
            "every rundown the day's folder holds: the two a new device has first, the others by name"
        );
        assert_eq!(
            [has_rundown(&mut sheets, "20261012"), has_rundown(&mut sheets, "20261014")],
            [Ok(true), Ok(false)],
            "a day's edition is there with its first rundown"
        );
        let mut pages_alone = disk(&[("news/20261013", "20261013_news.pdf", "pages")]);
        assert_eq!(has_rundown(&mut pages_alone, "20261013"), Ok(false));
        let no_seconds = |_: &str| panic!("no length");
        let listed = list(Shelf::News, &mut sheets, None, &mut Lengths::default(), no_seconds);
        assert_eq!(listed, Ok(editions), "the news' shelf lists the editions");
        assert_eq!(
            list(Shelf::Facts, &mut sheets, None, &mut Lengths::default(), no_seconds),
            Ok(Vec::new()),
            "the robot's shelves are none of the notes'"
        );
    }

    #[test]
    fn home_tally() {
        let mut lengths = Lengths::default();
        let tally = count_shelves(&stems(&["a", "b"]), &mut lengths, |stem| (stem == "b").then_some(9), true);
        assert_eq!(tally, Tally { notes: 2, newest_note_s: Some(9), has_edition: true, ..Tally::default() });
        assert_eq!(count_shelves(&[], &mut lengths, |_| panic!("no note"), false), Tally::default());
        assert_eq!(lengths.seconds_of("b", |_| None), Some(9));
    }
}

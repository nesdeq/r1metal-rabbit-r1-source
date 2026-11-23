//! The answers to the model's reads of the device.

use crate::domain::cost::Figure;
use crate::domain::memos::{read_summary, read_words};
use crate::domain::paper::names::directory;
use crate::openai::ledger::SummaryKind;
use crate::storage::Files;
use crate::storage::paths::Listed;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

/// The answer where nothing of what was asked exists.
pub const NOTHING: &str = "Nothing.";
const NO_SUMMARY: &str = "not summed up yet";
const NO_WORDS: &str = "not written down yet";

fn first_line(text: &str) -> &str {
    let lines = text.lines().map(|line| line.trim().trim_start_matches('#').trim());
    lines.into_iter().find(|line| !line.is_empty()).unwrap_or("")
}

fn or_nothing(parts: Vec<String>, separator: &str) -> String {
    if parts.is_empty() { NOTHING.into() } else { parts.join(separator) }
}

/// The answer where the entries cannot be read.
pub fn to_unreadable(reason: &str) -> String {
    format!("They cannot be read: {reason}.")
}

fn is_of_day(stem: &str, day: &str) -> bool {
    stem.starts_with(day)
}

// The entries of a kind, a line each with the first line of its summary.
fn to_listing(entries: &[(String, Option<String>)]) -> String {
    let lines = entries
        .iter()
        .map(|(stem, summary)| format!("- {stem}: {}", summary.as_deref().map_or(NO_SUMMARY, first_line)));
    or_nothing(lines.collect(), "\n")
}

/// The answer to a read of a kind's entries: a line each, or those of `day` each in full.
pub fn read_entries(files: &mut impl Files, kind: SummaryKind, day: Option<&str>) -> String {
    let stems = match files.names(directory(kind), Listed::Folders) {
        Ok(stems) => stems,
        Err(e) => return to_unreadable(&e.to_string()),
    };
    let Some(day) = day else {
        let entries: Vec<_> = stems.iter().map(|stem| (stem.clone(), read_summary(files, kind, stem))).collect();
        return to_listing(&entries);
    };
    let of_day = stems.iter().filter(|stem| is_of_day(stem, day));
    let to_entry = |stem: &String| (stem.clone(), read_summary(files, kind, stem), read_words(files, kind, stem));
    to_full_listing(&of_day.map(to_entry).collect::<Vec<_>>())
}

// A day's entries, each in full.
fn to_full_listing(entries: &[(String, Option<String>, Option<String>)]) -> String {
    let parts = entries.iter().map(|(stem, summary, words)| {
        format!(
            "## {stem}\n\n### Summary\n\n{}\n\n### Words\n\n{}",
            summary.as_deref().unwrap_or(NO_SUMMARY),
            words.as_deref().unwrap_or(NO_WORDS)
        )
    });
    or_nothing(parts.collect(), "\n\n")
}

/// The day of the edition to read: the one requested, or the last of `days`.
pub fn edition_day(request: Option<&str>, days: &[String]) -> Option<String> {
    request.map(String::from).or_else(|| days.last().cloned())
}

/// A day's edition, with each written rundown by its name.
pub fn to_edition(day: Option<&str>, rundowns: &[(String, String)]) -> String {
    let Some(day) = day else {
        return NOTHING.into();
    };
    if rundowns.is_empty() {
        return format!("No edition of {day}.");
    }
    let parts: Vec<String> = rundowns.iter().map(|(name, text)| format!("## {name}\n\n{text}")).collect();
    format!("# {day}\n\n{}", parts.join("\n\n"))
}

fn write_cost_lines(lines: &[Figure], depth: usize, text: &mut String) {
    for line in lines {
        if !text.is_empty() {
            text.push('\n');
        }
        let _ = write!(text, "{:indent$}- {}: {}", "", line.name, line.value, indent = 2 * depth);
        if let Some(page) = &line.page {
            write_cost_lines(&page.lines, depth + 1, text);
        }
    }
}

/// The cost of OpenAI's services, each page's lines under the line that opens it.
pub fn to_cost(lines: &[Figure]) -> String {
    let mut text = String::new();
    write_cost_lines(lines, 0, &mut text);
    if text.is_empty() { NOTHING.into() } else { text }
}

/// The device's facts by label and value.
pub fn to_about(facts: &[(&str, String)]) -> String {
    let lines = facts.iter().map(|(label, value)| format!("- {label}: {value}"));
    or_nothing(lines.collect(), "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::cost::Page;
    use crate::storage::tests::Disk;
    use alloc::vec;

    #[test]
    fn entries_from_folders() {
        let file = |place: &str, name: &str, text: &str| (place.into(), name.into(), text.as_bytes().to_vec());
        let (first, second) = ("audio/notes/20261015_100000", "audio/notes/20261016_100000");
        let mut disk = Disk {
            files: vec![
                file(first, "20261015_100000_summary.md", "# Note\n\n---\n\n## Walk\n\nText"),
                file(first, "20261015_100000_transcript.md", "We walked."),
                file(second, "20261016_100000_transcript.md", "Words."),
                file("audio/translations/20261016_090000", "20261016_090000_conversation.md", "Remarks."),
            ],
            ..Disk::default()
        };
        assert_eq!(
            read_entries(&mut &mut disk, SummaryKind::Note, None),
            "- 20261015_100000: Walk\n- 20261016_100000: not summed up yet",
            "every note by its summary's first line, a sheet without its head"
        );
        assert_eq!(
            read_entries(&mut &mut disk, SummaryKind::Note, Some("20261016")),
            "## 20261016_100000\n\n### Summary\n\nnot summed up yet\n\n### Words\n\nWords.",
            "a day's notes with their words"
        );
        assert_eq!(
            read_entries(&mut &mut disk, SummaryKind::Talk, Some("20261016")),
            "## 20261016_090000\n\n### Summary\n\nnot summed up yet\n\n### Words\n\nRemarks.",
            "a translation's words are its remarks"
        );
        assert_eq!(read_entries(&mut &mut disk, SummaryKind::Talk, Some("20261015")), NOTHING);
        assert_eq!(read_entries(&mut &mut Disk::default(), SummaryKind::Note, None), NOTHING);
    }

    #[test]
    fn entry_listings() {
        let entries = [("a".into(), Some("## Heading\n\nText".into())), ("b".into(), None)];
        assert_eq!(to_listing(&entries), "- a: Heading\n- b: not summed up yet");
        assert_eq!(to_listing(&[]), NOTHING);
        assert_eq!(
            to_full_listing(&[("a".into(), None, Some("Words".into()))]),
            "## a\n\n### Summary\n\nnot summed up yet\n\n### Words\n\nWords"
        );
        assert_eq!(to_full_listing(&[]), NOTHING);
        assert!(is_of_day("20261016_100000", "20261016") && !is_of_day("20261015_100000", "20261016"));
        assert_eq!(to_unreadable("gone"), "They cannot be read: gone.");
    }

    #[test]
    fn edition_by_day() {
        let days = vec![String::from("20261015"), String::from("20261016")];
        assert_eq!(edition_day(None, &days).as_deref(), Some("20261016"));
        assert_eq!(edition_day(Some("x"), &days).as_deref(), Some("x"));
        assert_eq!(edition_day(None, &[]), None);
        assert_eq!(to_edition(None, &[]), NOTHING);
        assert_eq!(to_edition(Some("d"), &[]), "No edition of d.");
        assert_eq!(
            to_edition(Some("d"), &[("news".into(), "Rain.".into()), ("tech".into(), "Chips.".into())]),
            "# d\n\n## news\n\nRain.\n\n## tech\n\nChips."
        );
    }

    #[test]
    fn cost_nests_pages() {
        let line = Figure { name: "May".into(), value: "$1".into(), page: None };
        let page = Page { name: "", subject: String::new(), lines: vec![line] };
        let lines = [Figure { name: "All".into(), value: "$2".into(), page: Some(page) }];
        assert_eq!(to_cost(&lines), "- All: $2\n  - May: $1");
        assert_eq!(to_cost(&[]), NOTHING);
    }

    #[test]
    fn about_lines() {
        assert_eq!(to_about(&[("RAM", "3.8 GIB".into())]), "- RAM: 3.8 GIB");
        assert_eq!(to_about(&[]), NOTHING);
    }
}

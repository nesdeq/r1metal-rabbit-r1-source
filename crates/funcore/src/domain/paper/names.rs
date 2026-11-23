//! The file names of entries.

use crate::openai::ledger::SummaryKind;
use crate::storage::{NEWS, NOTES, TALKS};
use alloc::format;
use alloc::string::String;

pub const SOUND_EXTENSION: &str = "m4a";
pub const TEXT_EXTENSION: &str = "md";
const PAGES_EXTENSION: &str = "pdf";
const WORDS_SUFFIX: &str = "_transcript";
const REMARKS_SUFFIX: &str = "_conversation";
const SUMMARY_SUFFIX: &str = "_summary";

/// The directory of a kind's entries, each in a folder of its own.
pub const fn directory(kind: SummaryKind) -> &'static str {
    match kind {
        SummaryKind::Note => NOTES,
        SummaryKind::Talk => TALKS,
    }
}

/// An entry's folder from the data partition's root: its kind's directory, then its stem.
pub fn folder(kind: SummaryKind, stem: &str) -> String {
    format!("{}/{stem}", directory(kind))
}

pub fn sound_file(stem: &str) -> String {
    format!("{stem}.{SOUND_EXTENSION}")
}

// A note's sound, from the data partition's root.
pub(crate) fn note_sound_path(stem: &str) -> String {
    format!("{}/{}", folder(SummaryKind::Note, stem), sound_file(stem))
}

pub fn words_file(kind: SummaryKind, stem: &str) -> String {
    let suffix = if kind == SummaryKind::Note { WORDS_SUFFIX } else { REMARKS_SUFFIX };
    format!("{stem}{suffix}.{TEXT_EXTENSION}")
}

pub(crate) fn summary_file(stem: &str) -> String {
    format!("{stem}{SUMMARY_SUFFIX}.{TEXT_EXTENSION}")
}

pub(crate) fn pages_file(stem: &str) -> String {
    format!("{stem}{SUMMARY_SUFFIX}.{PAGES_EXTENSION}")
}

/// A day's edition's folder from the data partition's root: the news' directory, then its day.
pub fn edition_folder(day: &str) -> String {
    format!("{NEWS}/{day}")
}

pub fn rundown_file(day: &str, topic: &str) -> String {
    format!("{day}_{topic}.{TEXT_EXTENSION}")
}

pub fn rundown_pages_file(day: &str, topic: &str) -> String {
    format!("{day}_{topic}.{PAGES_EXTENSION}")
}

// True for a word that names a rundown in its files: lowercase letters and digits, a letter first.
pub(crate) fn is_name(word: &str) -> bool {
    word.starts_with(|first: char| first.is_ascii_lowercase())
        && word.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

// The name of the rundown a file of a day's edition holds as Markdown; none for another file.
pub(crate) fn rundown_name<'a>(day: &str, file: &'a str) -> Option<&'a str> {
    let stem = file.strip_suffix(TEXT_EXTENSION)?.strip_suffix('.')?;
    stem.strip_prefix(day)?.strip_prefix('_').filter(|name| is_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEM: &str = "20261012_190328";

    #[test]
    fn note_file_names() {
        assert_eq!(
            [
                sound_file(STEM),
                words_file(SummaryKind::Note, STEM),
                words_file(SummaryKind::Talk, STEM),
                summary_file(STEM),
                pages_file(STEM)
            ],
            [
                "20261012_190328.m4a",
                "20261012_190328_transcript.md",
                "20261012_190328_conversation.md",
                "20261012_190328_summary.md",
                "20261012_190328_summary.pdf"
            ]
        );
    }

    #[test]
    fn rundown_file_names() {
        const DAY: &str = "20261013";
        assert_eq!(
            ["news", "finance"].map(|topic| [rundown_file(DAY, topic), rundown_pages_file(DAY, topic)]),
            [["20261013_news.md", "20261013_news.pdf"], ["20261013_finance.md", "20261013_finance.pdf"]]
                .map(|names| names.map(String::from))
        );
        assert_eq!(edition_folder(DAY), "news/20261013");
        assert!(
            is_name("news") && is_name("f1") && !is_name("F1") && !is_name("1f") && !is_name("") && !is_name("a b"),
            "a rundown's name: lowercase letters and digits, a letter first"
        );
        assert_eq!(
            ["20261013_news.md", "20261013_f1.md", "20261013_news.pdf", "20261014_news.md", "20261013_.md", "news.md"]
                .map(|file| rundown_name(DAY, file)),
            [Some("news"), Some("f1"), None, None, None, None],
            "a day's Markdown of a rundown's name, and no other file"
        );
    }

    #[test]
    fn entry_folders() {
        assert_eq!((directory(SummaryKind::Note), directory(SummaryKind::Talk)), ("audio/notes", "audio/translations"));
        assert_eq!(folder(SummaryKind::Talk, "20261012_190328"), "audio/translations/20261012_190328");
        assert_eq!(note_sound_path(STEM), "audio/notes/20261012_190328/20261012_190328.m4a");
    }
}

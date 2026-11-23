//! The Markdown of an entry's documents.

use super::dates::{to_edition_name, to_entry_name, to_rundown_heading, to_sentence_date};
use super::names::{rundown_file, sound_file, words_file};
use crate::base::text::to_one_line;
use crate::base::time::to_duration_text;
use crate::domain::dialogue::Turn;
use crate::openai::ledger::SummaryKind;
use crate::openai::models::{Model, interpreter, transcriber};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const TRANSLATOR_MARK: &str = "**Translator:**";
pub(crate) const HEAD_END: &str = "\n---\n";
const PARAGRAPH_CHARS: usize = 600;
const SENTENCE_ENDS: [char; 3] = ['.', '?', '!'];
// What a summary is called over its text: on its document's bar, and where the interface names it.
pub(crate) const SUMMARY: &str = "SUMMARY";
const NOTE_TASK: &str = include_str!("../../../assets/notes/summary.txt");
const TALK_TASK: &str = include_str!("../../../assets/notes/talk.txt");

/// The model's instructions for an entry's summary.
pub const fn summary_task(kind: SummaryKind) -> &'static str {
    match kind {
        SummaryKind::Note => NOTE_TASK,
        SummaryKind::Talk => TALK_TASK,
    }
}

// What a rundown is called over its text, by its name: on its document's bar, and where the interface names it.
pub(crate) fn to_rundown_bar(name: &str) -> String {
    name.to_ascii_uppercase()
}

/// What a document is of, by which its title, its bar, its head and the file its footer names are told.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Subject<'a> {
    // A note's summary: its stem and seconds.
    Note(&'a str, u64),
    // A conversation's summary: its stem.
    Talk(&'a str),
    // A rundown: its day, name and sources line.
    Rundown(&'a str, &'a str, &'a str),
}

impl<'a> Subject<'a> {
    pub(crate) const fn from_entry(kind: SummaryKind, stem: &'a str, seconds: u64) -> Subject<'a> {
        match kind {
            SummaryKind::Note => Subject::Note(stem, seconds),
            SummaryKind::Talk => Subject::Talk(stem),
        }
    }

    pub(super) fn to_title(self) -> String {
        match self {
            Subject::Note(stem, _) => format!("Summary of the note of {}", to_sentence_date(stem)),
            Subject::Talk(stem) => format!("Summary of the conversation of {}", to_sentence_date(stem)),
            Subject::Rundown(day, topic, _) => to_rundown_heading(day, topic),
        }
    }

    pub(super) fn to_bar_name(self) -> String {
        match self {
            Subject::Note(..) | Subject::Talk(_) => SUMMARY.into(),
            Subject::Rundown(_, topic, _) => to_rundown_bar(topic),
        }
    }

    pub(super) fn to_source_file(self) -> String {
        match self {
            Subject::Note(stem, _) => sound_file(stem),
            Subject::Talk(stem) => words_file(SummaryKind::Talk, stem),
            Subject::Rundown(day, topic, _) => rundown_file(day, topic),
        }
    }

    pub(super) fn to_bar_line(self) -> String {
        match self {
            Subject::Note(stem, seconds) => format!("{}, {}", to_entry_name(stem), to_duration_text(seconds)),
            Subject::Talk(stem) => to_entry_name(stem),
            Subject::Rundown(day, ..) => to_edition_name(day),
        }
    }
}

fn paragraph_end(text: &str) -> Option<usize> {
    text.char_indices().enumerate().find_map(|(count, (at, character))| {
        let end = at + character.len_utf8();
        let ends_sentence = SENTENCE_ENDS.contains(&character) && text[end..].starts_with(char::is_whitespace);
        (ends_sentence && count + 1 >= PARAGRAPH_CHARS).then_some(end)
    })
}

fn to_paragraphs(words: &str) -> String {
    let lines: Vec<&str> = words.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if lines.len() > 1 {
        return lines.join("\n\n");
    }
    let mut text = String::with_capacity(words.len());
    let mut rest = words.trim();
    while let Some(end) = paragraph_end(rest) {
        let (paragraph, tail) = rest.split_at(end);
        text.push_str(paragraph);
        text.push_str("\n\n");
        rest = tail.trim_start();
    }
    text.push_str(rest);
    text
}

pub(crate) fn to_words_file(stem: &str, seconds: u64, words: &str) -> String {
    format!(
        "# Note of {}\n\nTranscript of `{}` ({}), written down by OpenAI's `{}`.\n{HEAD_END}\n{}\n",
        to_sentence_date(stem),
        sound_file(stem),
        to_duration_text(seconds),
        transcriber::MODEL,
        to_paragraphs(words)
    )
}

fn to_remark_lines(turn: &Turn) -> String {
    let (source, translation) = (to_one_line(&turn.source), to_one_line(&turn.translation));
    match (source.is_empty(), translation.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("- {TRANSLATOR_MARK} {translation}\n"),
        (false, true) => format!("- {source}\n"),
        (false, false) => format!("- {source}\n  - {translation}\n"),
    }
}

/// The Markdown file of a conversation.
pub fn to_remarks_file(stem: &str, seconds: u64, turns: &[Turn]) -> String {
    let head = format!(
        "# Conversation of {}\n\nA conversation of {} as OpenAI's `{}` heard and translated it: each remark as it was spoken, and under it as the translator said it to the other.\n{HEAD_END}\n",
        to_sentence_date(stem),
        to_duration_text(seconds),
        interpreter::MODEL
    );
    turns.iter().map(to_remark_lines).fold(head, |file, lines| file + &lines)
}

/// The Markdown file of a summary or of a rundown that `model` wrote.
pub fn to_summary_file(subject: Subject<'_>, text: &str, model: Model) -> String {
    let model = model.to_word();
    let head = match subject {
        Subject::Rundown(day, topic, sources) => format!(
            "# {}\n\nThe day's rundown, written by OpenAI's `{model}` from what the feeds told that morning. {}.",
            to_rundown_heading(day, topic),
            sources.trim().trim_end_matches('.')
        ),
        Subject::Note(stem, seconds) => format!(
            "# Note of {}\n\nSummary of `{}` ({}), made by OpenAI's `{model}` from its [transcript]({}).",
            to_sentence_date(stem),
            sound_file(stem),
            to_duration_text(seconds),
            words_file(SummaryKind::Note, stem)
        ),
        Subject::Talk(stem) => format!(
            "# Conversation of {}\n\nSummary, in both of its languages, of the [conversation]({}) the translator rendered, made by OpenAI's `{model}`.",
            to_sentence_date(stem),
            words_file(SummaryKind::Talk, stem)
        ),
    };
    format!("{head}\n{HEAD_END}\n{}\n", text.trim())
}

/// The text behind the head of such a file.
pub fn body(file: &str) -> &str {
    file.split_once(HEAD_END).map_or(file, |parts| parts.1).trim()
}

/// The text behind the head of such a file as the partition gives it, what is no UTF-8 replaced.
pub fn body_of(file: &[u8]) -> String {
    body(&String::from_utf8_lossy(file)).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEM: &str = "20261012_190328";
    const NOTE: Subject<'static> = Subject::Note(STEM, 42);
    const TALK: Subject<'static> = Subject::Talk(STEM);

    #[test]
    fn words_and_summary_files() {
        let file = to_words_file(STEM, 42, " Buy milk. And call Anna. ");
        assert_eq!(
            file,
            "# Note of 12 October 2026, 19:03\n\nTranscript of `20261012_190328.m4a` (0:42), written down by OpenAI's `gpt-transcribe`.\n\n---\n\nBuy milk. And call Anna.\n"
        );
        assert_eq!(body(&file), "Buy milk. And call Anna.");
        let summary = to_summary_file(NOTE, "Shopping.\n\n## To do\n\n- **Anna**: call\n\n---\n\nEnd\n", Model::Luna);
        assert!(summary.starts_with(
            "# Note of 12 October 2026, 19:03\n\nSummary of `20261012_190328.m4a` (0:42), made by OpenAI's `gpt-6-luna` from its [transcript](20261012_190328_transcript.md).\n\n---\n\nShopping."
        ));
        assert_eq!(body(&summary), "Shopping.\n\n## To do\n\n- **Anna**: call\n\n---\n\nEnd");
        assert_eq!((body("no head at all\n"), body("")), ("no head at all", ""));
        assert_eq!(body_of(b"# Note\n\n---\n\nBuy m\xFFlk.\n"), "Buy m\u{FFFD}lk.", "as its bytes are read");
    }

    #[test]
    fn conversation_file() {
        let turn = |source: &str, translation: &str| Turn { source: source.into(), translation: translation.into() };
        let turns = [
            turn("", " Hello, I am the translator.\n"),
            turn("Wo ist der  Bahnhof?", "Where is the station?"),
            turn("", ""),
            turn("Over there.", ""),
        ];
        let file = to_remarks_file(STEM, 95, &turns);
        assert_eq!(
            file,
            "# Conversation of 12 October 2026, 19:03\n\nA conversation of 1:35 as OpenAI's `gpt-live-1` heard and translated it: each remark as it was spoken, and under it as the translator said it to the other.\n\n---\n\n- **Translator:** Hello, I am the translator.\n- Wo ist der Bahnhof?\n  - Where is the station?\n- Over there.\n"
        );
        let summary = to_summary_file(
            TALK,
            "## Deutsch\n\nEs ging um den Bahnhof.\n\n## English\n\nIt was about the station.\n",
            Model::Terra,
        );
        assert!(
            summary.starts_with(
                "# Conversation of 12 October 2026, 19:03\n\nSummary, in both of its languages, of the [conversation](20261012_190328_conversation.md) the translator rendered, made by OpenAI's `gpt-6.1-terra`.\n\n---\n\n## Deutsch"
            ),
            "the model that wrote it"
        );
    }

    #[test]
    fn summary_tasks() {
        assert!(summary_task(SummaryKind::Note).contains("concise summary"));
        assert!(summary_task(SummaryKind::Talk).contains("once in each of the two languages"));
    }

    #[test]
    fn paragraph_cuts() {
        let sentence = "This sentence has just fifty characters to its end. ";
        let long: String = core::iter::repeat_n(sentence, 30).collect();
        let text = to_paragraphs(&long);
        let parts: Vec<&str> = text.split("\n\n").collect();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|part| part.ends_with("end.") && part.starts_with("This")));
        assert_eq!(parts[0].len(), 12 * 52 - 1);
        assert_eq!(text.replace("\n\n", " "), long.trim());
        assert_eq!(to_paragraphs("One.\n\nTwo.\r\n  Three.  "), "One.\n\nTwo.\n\nThree.");
        assert_eq!(
            (to_paragraphs("3.14 is no end. Ok"), to_paragraphs("")),
            ("3.14 is no end. Ok".into(), String::new())
        );
    }

    #[test]
    fn rundown_head() {
        const DAY: &str = "20261013";
        const SOURCES: &str = "Sources (2/3 feeds, 9 stories): a.com, b.org";
        let file = to_summary_file(
            Subject::Rundown(DAY, "finance", SOURCES),
            "\n## Markets\n\n**Stocks** fell.\n",
            Model::Luna,
        );
        assert_eq!(
            file,
            "# Finance of 13 October 2026\n\nThe day's rundown, written by OpenAI's `gpt-6-luna` from what the feeds told that morning. Sources (2/3 feeds, 9 stories): a.com, b.org.\n\n---\n\n## Markets\n\n**Stocks** fell.\n"
        );
        let news = Subject::Rundown(DAY, "news", SOURCES);
        assert_eq!(
            (news.to_title(), news.to_bar_name(), news.to_bar_line(), news.to_source_file()),
            (
                "News of 13 October 2026".into(),
                "NEWS".into(),
                "tuesday 13 october 2026".into(),
                "20261013_news.md".into()
            )
        );
    }
}

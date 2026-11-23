//! The robot's search of its memory and of every earlier day, ranked by Okapi BM25.

use super::InputKind;
use super::memory::Memory;
use super::transcript::{Entry, EntryKind};
use crate::base::math::log2;
use crate::base::text::{to_one_line, words};
use crate::base::time::stamp::{parse_compact_day, parse_stamp};
use crate::domain::memos::{read_summary, read_words};
use crate::domain::paper::names::directory;
use crate::openai::ledger::SummaryKind;
use crate::storage::Files;
use crate::storage::paths::Listed;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const HIT_COUNT: usize = 10;
const SATURATION: f32 = 1.2;
const LENGTH_WEIGHT: f32 = 0.75;
const KNOWLEDGE_LABEL: &str = "you learned";

/// A text of a note or a translation the search reads while the notes tool is on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Paper {
    stem: String,
    label: &'static str,
    text: String,
}

/// The kinds of entries the search reads.
pub const KINDS: [SummaryKind; 2] = [SummaryKind::Note, SummaryKind::Talk];

// What the search reads of an entry: a note's words and summary, a translation's summary; `words` reads the entry's words, which a translation's are not asked for.
fn to_papers(
    kind: SummaryKind,
    stem: &str,
    summary: Option<String>,
    words: impl FnOnce() -> Option<String>,
) -> Vec<Paper> {
    let texts = match kind {
        SummaryKind::Note => [(summary, "a note's summary"), (words(), "a note's words")],
        SummaryKind::Talk => [(summary, "a translation's summary"), (None, "")],
    };
    let papers = texts.into_iter().filter_map(|(text, label)| Some((text?, label)));
    papers.map(|(text, label)| Paper { stem: stem.into(), label, text }).collect()
}

/// What the search reads of a kind's entries, or why they cannot be listed.
pub fn read_papers<F: Files>(files: &mut F, kind: SummaryKind) -> Result<Vec<Paper>, F::Error> {
    let mut papers = Vec::new();
    for stem in files.names(directory(kind), Listed::Folders)? {
        let summary = read_summary(files, kind, &stem);
        papers.extend(to_papers(kind, &stem, summary, || read_words(files, kind, &stem)));
    }
    Ok(papers)
}

fn to_stamp_day(stem: &str) -> String {
    parse_stamp(stem).map_or_else(|| stem.to_string(), |moment| moment.to_iso_minute())
}

struct Document<'a> {
    day: String,
    label: &'static str,
    text: &'a str,
    words: Vec<String>,
}

fn to_iso_day(day: &str) -> String {
    parse_compact_day(day).map_or_else(|| day.to_string(), |moment| moment.to_iso_date())
}

fn to_hit_line(day: &str, label: &str, text: &str) -> String {
    let text = to_one_line(text);
    if day.is_empty() { format!("- {label}: {text}\n") } else { format!("- {day}, {label}: {text}\n") }
}

fn to_latest_knowledge(memory: &Memory) -> String {
    let lines: String = memory
        .knowledge
        .iter()
        .rev()
        .take(HIT_COUNT)
        .map(|(day, line)| to_hit_line(day, KNOWLEDGE_LABEL, line))
        .collect();
    if lines.is_empty() { String::from("Nothing learned yet.") } else { format!("What you learned last:\n{lines}") }
}

fn entry_label(kind: EntryKind) -> Option<&'static str> {
    match kind {
        EntryKind::User(InputKind::Voice) => Some("the user said"),
        EntryKind::User(InputKind::Text) => Some("the user typed"),
        EntryKind::Robot => Some("you said"),
        EntryKind::Shown | EntryKind::Searches | EntryKind::Sources => None,
    }
}

fn collect_documents<'a>(
    memory: &'a Memory,
    days: &'a [(String, Vec<Entry>)],
    papers: &'a [Paper],
) -> Vec<Document<'a>> {
    let core = memory.core.iter().map(|line| Document {
        day: String::new(),
        label: "always holds",
        text: line,
        words: words(line).collect(),
    });
    let knowledge = memory.knowledge.iter().map(|(day, line)| Document {
        day: day.clone(),
        label: KNOWLEDGE_LABEL,
        text: line,
        words: words(line).collect(),
    });
    let conversation = days.iter().flat_map(|(day, entries)| {
        entries.iter().filter_map(move |entry| {
            let label = entry_label(entry.kind)?;
            let day = format!("{} {}", to_iso_day(day), entry.time);
            Some(Document { day, label, text: &entry.text, words: words(&entry.text).collect() })
        })
    });
    let notes = papers.iter().map(|paper| Document {
        day: to_stamp_day(&paper.stem),
        label: paper.label,
        text: &paper.text,
        words: words(&paper.text).collect(),
    });
    core.chain(knowledge).chain(conversation).chain(notes).collect()
}

fn score_document(document: &Document<'_>, terms: &[String], weights: &[f32], mean_length: f32) -> f32 {
    let length = 1.0 - LENGTH_WEIGHT + LENGTH_WEIGHT * document.words.len() as f32 / mean_length.max(1.0);
    let term = |(term, weight): (&String, &f32)| {
        let frequency = document.words.iter().filter(|word| *word == term).count() as f32;
        weight * frequency * (SATURATION + 1.0) / (frequency + SATURATION * length)
    };
    terms.iter().zip(weights).map(term).sum()
}

/// A search's answer: the best first, of two equal the later; for no words, the latest knowledge.
pub fn search(query: &str, memory: &Memory, days: &[(String, Vec<Entry>)], papers: &[Paper]) -> String {
    let terms: Vec<String> = words(query).collect();
    if terms.is_empty() {
        return to_latest_knowledge(memory);
    }
    let documents = collect_documents(memory, days, papers);
    let count = documents.len() as f32;
    let mean_length = documents.iter().map(|document| document.words.len()).sum::<usize>() as f32 / count.max(1.0);
    let rarity = |word: &String| {
        let match_count = documents.iter().filter(|document| document.words.contains(word)).count() as f32;
        log2((count - match_count + 0.5) / (match_count + 0.5) + 1.0)
    };
    let weights: Vec<f32> = terms.iter().map(rarity).collect();
    let mut hits: Vec<(f32, usize)> = documents
        .iter()
        .map(|document| score_document(document, &terms, &weights, mean_length))
        .enumerate()
        .map(|(index, score)| (score, index))
        .filter(|hit| hit.0 > 0.0)
        .collect();
    hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    let lines: String = hits
        .iter()
        .take(HIT_COUNT)
        .map(|hit| &documents[hit.1])
        .map(|document| to_hit_line(&document.day, document.label, document.text))
        .collect();
    if lines.is_empty() { String::from("Nothing found.") } else { format!("Found:\n{lines}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests::Disk;
    use alloc::vec;

    fn entry(kind: EntryKind, text: &str) -> Entry {
        Entry { kind, time: "14:02".into(), text: text.into() }
    }

    fn memory() -> Memory {
        let mut memory = Memory::default();
        memory.core = vec!["Lives in Vienna.".into()];
        memory.knowledge = vec![("2026-10-16".into(), "Likes green tea.".into())];
        memory
    }

    #[test]
    fn hits_ranked_with_day_and_label() {
        let days = vec![(
            String::from("20261015"),
            vec![
                entry(EntryKind::User(InputKind::Voice), "Wie wird das Wetter in Vienna?"),
                entry(EntryKind::Robot, "Regen."),
                entry(EntryKind::Searches, "Vienna weather"),
            ],
        )];
        assert_eq!(
            search("vienna", &memory(), &days, &[]),
            "Found:\n- always holds: Lives in Vienna.\n- 2026-10-15 14:02, the user said: Wie wird das Wetter in Vienna?\n"
        );
        assert_eq!(search("tea", &memory(), &days, &[]), "Found:\n- 2026-10-16, you learned: Likes green tea.\n");
        assert_eq!(search("nothing", &memory(), &days, &[]), "Nothing found.");
    }

    #[test]
    fn notes_searched_while_given() {
        let summary = || Some(String::from("A walk in the Prater."));
        let mut papers = to_papers(SummaryKind::Note, "20261012_190328", summary(), || Some("We walked.".into()));
        let unread = || panic!("a translation's remarks are not read");
        papers.extend(to_papers(SummaryKind::Talk, "20261013_080000", summary(), unread));
        assert_eq!(papers.len(), 3, "a translation's remarks are not searched");
        assert_eq!(
            search("prater", &Memory::default(), &[], &papers),
            "Found:\n- 2026-10-13 08:00, a translation's summary: A walk in the Prater.\n- 2026-10-12 19:03, a note's summary: A walk in the Prater.\n"
        );
        assert_eq!(
            search("walked", &Memory::default(), &[], &papers),
            "Found:\n- 2026-10-12 19:03, a note's words: We walked.\n"
        );
    }

    #[test]
    fn papers_from_folders() {
        let file = |place: &str, name: &str, text: &str| (place.into(), name.into(), text.as_bytes().to_vec());
        let (note, talk) = ("audio/notes/20261012_190328", "audio/translations/20261013_080000");
        let mut disk = Disk {
            files: vec![
                file(note, "20261012_190328_summary.md", "# Note\n\n---\n\nA walk.\n"),
                file(note, "20261012_190328_transcript.md", "We walked."),
                file(talk, "20261013_080000_summary.md", "A talk."),
                file(talk, "20261013_080000_conversation.md", "Remarks."),
                file("audio/notes/20261014_070000", "20261014_070000.m4a", "sound"),
            ],
            ..Disk::default()
        };
        let read = |disk: &mut Disk| KINDS.map(|kind| read_papers(&mut &mut *disk, kind).unwrap());
        let paper = |stem: &str, label, text: &str| Paper { stem: stem.into(), label, text: text.into() };
        assert_eq!(
            read(&mut disk),
            [
                vec![
                    paper("20261012_190328", "a note's summary", "A walk."),
                    paper("20261012_190328", "a note's words", "We walked.")
                ],
                vec![paper("20261013_080000", "a translation's summary", "A talk.")]
            ],
            "each sheet without its head; a note not written down yet gives nothing, nor a translation's remarks"
        );
        assert!(read(&mut Disk::default()).iter().all(Vec::is_empty), "no entry, no paper");
    }

    #[test]
    fn empty_query() {
        assert_eq!(
            search("  ", &memory(), &[], &[]),
            "What you learned last:\n- 2026-10-16, you learned: Likes green tea.\n"
        );
        assert_eq!(search("", &Memory::default(), &[], &[]), "Nothing learned yet.");
    }

    #[test]
    fn tie_takes_later() {
        let days = vec![(String::from("x"), vec![entry(EntryKind::Robot, "same"), entry(EntryKind::Robot, "same")])];
        let mut with_time = days;
        with_time[0].1[1].time = "15:00".into();
        assert_eq!(
            search("same", &Memory::default(), &with_time, &[]),
            "Found:\n- x 15:00, you said: same\n- x 14:02, you said: same\n"
        );
    }
}

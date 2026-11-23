//! What the scribe writes of an entry, and when.

pub mod state;

use crate::base::text::has_words;
use crate::codec::aac::FRAME;
use crate::domain::paper::names::{directory, folder, pages_file, sound_file, summary_file, words_file};
use crate::domain::paper::pages::to_pages;
use crate::domain::paper::sheet::{self, Subject};
use crate::domain::shelf::Shelf;
use crate::gfx::palette::Roles;
use crate::openai::fault::Fault;
use crate::openai::ledger::SummaryKind;
use crate::openai::summary::Summary;
use crate::openai::transcript;
use crate::storage::Files;
use crate::storage::paths::Listed;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use state::Model;

const PIECE_S: u64 = 1200;

/// An entry: its kind and its file stem.
pub type Entry = (SummaryKind, String);

/// True while the screens of notes or conversations show.
pub fn is_wanted(shelf: Option<Shelf>) -> bool {
    matches!(shelf, Some(Shelf::Notes | Shelf::Talks))
}

/// The size of a note's sound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Extent {
    pub packets: usize,
    pub rate: u32,
    pub bytes: u64,
    pub seconds: u64,
}

/// The effects of the work on an entry beside its files, one a method.
pub trait Desk {
    type Sound;
    type Stay;

    fn open_sound(&mut self, stem: &str) -> Result<(Self::Sound, Extent), Fault>;
    fn hold(&mut self) -> Result<Self::Stay, Fault>;
    fn transcribe_whole(&mut self, sound: &Self::Sound, stay: &Self::Stay, seconds: u64) -> Result<String, Fault>;
    // Transcribes `count` packets of a sound, from the one numbered `first`.
    fn transcribe_piece(
        &mut self,
        sound: &Self::Sound,
        stay: &Self::Stay,
        first: usize,
        count: usize,
    ) -> Result<String, Fault>;
    // Books a note whose transcript is stored.
    fn book_note(&mut self);
    fn note_written(&mut self);
    fn set_model(&mut self, model: Model);
    fn summarize(&mut self, kind: SummaryKind, words: &str) -> Result<Summary, Fault>;
    // The pages' local time and offset, and their colours.
    fn to_page_marks(&mut self) -> ((u64, i16), Roles);
}

fn source_file(kind: SummaryKind, stem: &str) -> String {
    if kind == SummaryKind::Note { sound_file(stem) } else { words_file(kind, stem) }
}

fn is_lacking<E>(
    kind: SummaryKind,
    stem: &str,
    recording: Option<&str>,
    mut has: impl FnMut(&str) -> Result<bool, E>,
) -> Result<bool, E> {
    if kind == SummaryKind::Note && recording == Some(stem) {
        return Ok(false);
    }
    if !has(&source_file(kind, stem))? {
        return Ok(false);
    }
    Ok(!has(&words_file(kind, stem))? || !has(&summary_file(stem))?)
}

/// The partition's entries that lack words or a summary and have their source, never the note being recorded.
pub fn scan<F: Files>(mut files: F, recording: Option<&str>) -> Result<Vec<Entry>, F::Error> {
    let mut entries = Vec::new();
    for kind in [SummaryKind::Note, SummaryKind::Talk] {
        for stem in files.names(directory(kind), Listed::Folders)? {
            let place = folder(kind, &stem);
            if is_lacking(kind, &stem, recording, |name| files.has(&place, name))? {
                entries.push((kind, stem));
            }
        }
    }
    Ok(entries)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Need {
    Nothing,
    Words,
    Summary,
}

fn needs_summary(has_summary: bool, words: &str) -> bool {
    !has_summary && has_words(words)
}

fn need_of(kind: SummaryKind, words: Option<&str>, has_summary: bool) -> Need {
    match words {
        None if kind == SummaryKind::Note => Need::Words,
        Some(words) if needs_summary(has_summary, words) => Need::Summary,
        _ => Need::Nothing,
    }
}

fn piece_packets(rate: u32) -> usize {
    (PIECE_S * u64::from(rate) / FRAME as u64).max(1) as usize
}

fn fits_one_piece(packets: usize, rate: u32, bytes: u64) -> bool {
    packets <= piece_packets(rate) && bytes <= transcript::MAX_BYTES
}

fn pieces(packets: usize, rate: u32) -> impl Iterator<Item = (usize, usize)> {
    let size = piece_packets(rate);
    (0..packets).step_by(size).map(move |first| (first, size.min(packets - first)))
}

fn transcribe<D: Desk>(desk: &mut D, sound: &D::Sound, stay: &D::Stay, extent: Extent) -> Result<String, Fault> {
    if fits_one_piece(extent.packets, extent.rate, extent.bytes) {
        return desk.transcribe_whole(sound, stay, extent.seconds);
    }
    let mut text = String::new();
    for (first, count) in pieces(extent.packets, extent.rate) {
        let words = desk.transcribe_piece(sound, stay, first, count)?;
        append_words(&mut text, &words);
    }
    Ok(text)
}

/// Writes what an entry lacks; false if it needs nothing or was deleted meanwhile.
pub fn write_entry<F: Files, D: Desk>(mut files: F, desk: &mut D, entry: &Entry) -> Result<bool, Fault> {
    let (kind, stem) = (entry.0, entry.1.as_str());
    let place = folder(kind, stem);
    let (words_name, summary_name) = (words_file(kind, stem), summary_file(stem));
    let stored_words = files.read(&place, &words_name)?.map(|file| sheet::body_of(&file));
    // A note's sound is read by its index; the note being recorded, which has none yet, is left out of every listing, but for one begun while the partition is looked at, which fails to be read and waits until it is handed over whole.
    let sound = match kind {
        SummaryKind::Note => Some(desk.open_sound(stem)?),
        SummaryKind::Talk => None,
    };
    let seconds = sound.as_ref().map_or(0, |sound| sound.1.seconds);
    let subject = Subject::from_entry(kind, stem, seconds);
    let has_summary = files.has(&place, &summary_name)?;
    let need = need_of(kind, stored_words.as_deref(), has_summary);
    if need == Need::Nothing {
        return Ok(false);
    }
    // An entry deleted while a model has it is there no more when the answer comes, and gets nothing written: its folder is never made anew.
    let source = source_file(kind, stem);
    let stay = desk.hold()?;
    let words = match (need, stored_words, &sound) {
        (Need::Words, _, Some((sound, extent))) => {
            let transcript = transcribe(desk, sound, &stay, *extent)?;
            let file = sheet::to_words_file(stem, seconds, &transcript);
            if !files.has(&place, &source)? || !files.put_into(&place, &words_name, file.as_bytes())? {
                return Ok(false);
            }
            // The transcript is whole and kept, so the note counts.
            desk.book_note();
            desk.note_written();
            transcript
        }
        (_, Some(words), _) => words,
        _ => return Ok(false),
    };
    if !needs_summary(has_summary, &words) {
        return Ok(true);
    }
    desk.set_model(Model::Summing);
    let summary = desk.summarize(kind, &words)?;
    let file = sheet::to_summary_file(subject, &summary.text, summary.usage.model);
    if !files.has(&place, &source)? || !files.put_into(&place, &summary_name, file.as_bytes())? {
        return Ok(false);
    }
    let (created, roles) = desk.to_page_marks();
    Ok(files.put_into(&place, &pages_file(stem), &to_pages(subject, &summary.text, Some(created), &roles))?)
}

/// The log line of the end of an entry's work.
pub fn to_log_line(stem: &str, outcome: &Result<bool, Fault>, ms: u128) -> Option<String> {
    match outcome {
        Ok(true) => Some(format!("scribe: {stem}: written in {ms} ms")),
        Ok(false) => None,
        Err(Fault::Temporary(reason)) => Some(format!("scribe: {stem}: given up after {ms} ms: {reason}")),
        Err(fault) => Some(format!("scribe: {stem}: {fault}")),
    }
}

fn append_words(text: &mut String, words: &str) {
    if !text.is_empty() && !words.is_empty() {
        text.push(' ');
    }
    text.push_str(words);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests::Disk;
    use crate::storage::{NOTES, TALKS};
    use alloc::vec;
    use alloc::vec::Vec;

    #[derive(Default)]
    struct Clerk {
        effects: Vec<String>,
        extent: Option<Extent>,
    }

    impl Desk for Clerk {
        type Sound = ();
        type Stay = ();

        fn open_sound(&mut self, stem: &str) -> Result<((), Extent), Fault> {
            self.effects.push(format!("open {stem}"));
            self.extent.map(|extent| ((), extent)).ok_or(Fault::Temporary("no sound".into()))
        }

        fn hold(&mut self) -> Result<(), Fault> {
            self.effects.push("hold".into());
            Ok(())
        }

        fn transcribe_whole(&mut self, (): &(), (): &(), seconds: u64) -> Result<String, Fault> {
            self.effects.push(format!("whole {seconds}"));
            Ok("Words.".into())
        }

        fn transcribe_piece(&mut self, (): &(), (): &(), first: usize, count: usize) -> Result<String, Fault> {
            self.effects.push(format!("piece {first} {count}"));
            Ok("Words.".into())
        }

        fn book_note(&mut self) {
            self.effects.push("book".into());
        }

        fn note_written(&mut self) {
            self.effects.push("written".into());
        }

        fn set_model(&mut self, model: Model) {
            self.effects.push(format!("{model:?}"));
        }

        fn summarize(&mut self, _: SummaryKind, words: &str) -> Result<Summary, Fault> {
            self.effects.push(format!("summarize {words}"));
            Ok(Summary { text: "Summed.".into(), usage: crate::openai::tokens::Usage::default() })
        }

        fn to_page_marks(&mut self) -> ((u64, i16), Roles) {
            self.effects.push("marks".into());
            ((0, 0), Roles::default())
        }
    }

    const NOTE: Extent = Extent { packets: 10, rate: 48_000, bytes: 100, seconds: 7 };

    fn note() -> Entry {
        (SummaryKind::Note, "n".into())
    }

    #[test]
    fn writing_a_note() {
        let place = "audio/notes/n";
        let mut disk = Disk::with_files(&[(place, "n.m4a")]);
        let mut clerk = Clerk { extent: Some(NOTE), ..Clerk::default() };
        assert_eq!(write_entry(&mut disk, &mut clerk, &note()), Ok(true));
        assert_eq!(
            clerk.effects,
            ["open n", "hold", "whole 7", "book", "written", "Summing", "summarize Words.", "marks"]
        );
        assert_eq!(
            disk.effects,
            [
                "read n_transcript.md",
                "has n_summary.md",
                "has n.m4a",
                "put n_transcript.md",
                "has n.m4a",
                "put n_summary.md",
                "put n_summary.pdf"
            ]
        );
    }

    #[test]
    fn writing_a_note_in_pieces() {
        let long = Extent { packets: 120_000, ..NOTE };
        let mut disk = Disk::with_files(&[("audio/notes/n", "n.m4a")]);
        let mut clerk = Clerk { extent: Some(long), ..Clerk::default() };
        write_entry(&mut disk, &mut clerk, &note()).unwrap();
        assert_eq!(clerk.effects[2..5], ["piece 0 56250", "piece 56250 56250", "piece 112500 7500"]);
    }

    #[test]
    fn writing_a_complete_entry() {
        let place = "audio/notes/n";
        let mut disk = Disk::with_files(&[(place, "n.m4a"), (place, "n_transcript.md"), (place, "n_summary.md")]);
        let mut clerk = Clerk { extent: Some(NOTE), ..Clerk::default() };
        assert_eq!(write_entry(&mut disk, &mut clerk, &note()), Ok(false));
        assert_eq!(clerk.effects, ["open n"], "no network is held for it");
    }

    #[test]
    fn writing_a_deleted_entry() {
        let mut disk = Disk::with_files(&[("audio/notes/n", "n_transcript.md")]);
        let mut clerk = Clerk { extent: Some(NOTE), ..Clerk::default() };
        assert_eq!(write_entry(&mut disk, &mut clerk, &note()), Ok(false), "its sound is gone");
        assert_eq!(disk.effects.last().map(String::as_str), Some("has n.m4a"));
        assert_eq!(clerk.effects.last().map(String::as_str), Some("summarize file"));
    }

    #[test]
    fn writing_a_conversation() {
        let place = "audio/translations/t";
        let mut disk = Disk::with_files(&[(place, "t_conversation.md")]);
        disk.files[0].2 = b"# Conversation\n\n---\n\n- Hi\n".to_vec();
        let mut clerk = Clerk::default();
        assert_eq!(write_entry(&mut disk, &mut clerk, &(SummaryKind::Talk, "t".into())), Ok(true));
        assert_eq!(clerk.effects, ["hold", "Summing", "summarize - Hi", "marks"], "its words without their head");
    }

    #[test]
    fn scan_lacking_entries() {
        let mut disk = Disk::with_files(&[
            ("audio/notes/a", "a.m4a"),
            ("audio/notes/b", "b.m4a"),
            ("audio/notes/c", "c_summary.md"),
            ("audio/translations/t", "t_conversation.md"),
        ]);
        assert_eq!(
            scan(&mut disk, Some("b")),
            Ok(vec![(SummaryKind::Note, "a".into()), (SummaryKind::Talk, "t".into())]),
            "of {NOTES} and {TALKS}"
        );
    }

    #[test]
    fn wanted_on_audio_shelves() {
        assert_eq!(
            [Some(Shelf::Notes), Some(Shelf::Talks), Some(Shelf::News), None].map(is_wanted),
            [true, true, false, false]
        );
    }

    #[test]
    fn long_note_pieces() {
        assert_eq!(piece_packets(48_000), 56_250);
        assert_eq!(piece_packets(0), 1);
        assert!(
            fits_one_piece(56_250, 48_000, transcript::MAX_BYTES)
                && !fits_one_piece(56_251, 48_000, 1)
                && !fits_one_piece(1, 48_000, transcript::MAX_BYTES + 1)
        );
        let mut text = String::new();
        for words in ["Eins.", "", "Zwei."] {
            append_words(&mut text, words);
        }
        assert_eq!(text, "Eins. Zwei.");
        assert_eq!(pieces(120_000, 48_000).collect::<Vec<_>>(), [(0, 56_250), (56_250, 56_250), (112_500, 7_500)]);
        assert_eq!(pieces(0, 48_000).count(), 0);
    }

    #[test]
    fn lacking_entries() {
        let files = |names: &'static [&'static str]| move |name: &str| Ok::<bool, ()>(names.contains(&name));
        type Case = (SummaryKind, &'static str, Option<&'static str>, &'static [&'static str]);
        let table: [(Case, bool); 6] = [
            ((SummaryKind::Note, "n", None, &["n.m4a"]), true),
            ((SummaryKind::Note, "n", Some("n"), &["n.m4a"]), false),
            ((SummaryKind::Note, "n", Some("o"), &["n.m4a", "n_transcript.md"]), true),
            ((SummaryKind::Note, "n", None, &["n.m4a", "n_transcript.md", "n_summary.md"]), false),
            ((SummaryKind::Talk, "t", None, &["t_summary.md"]), false),
            ((SummaryKind::Talk, "t", Some("t"), &["t_conversation.md"]), true),
        ];
        for ((kind, stem, recording, names), expected) in table {
            assert_eq!(is_lacking(kind, stem, recording, files(names)), Ok(expected), "{stem} {names:?}");
        }
        assert_eq!(is_lacking(SummaryKind::Note, "n", None, |_| Err(())), Err(()));
    }

    #[test]
    fn words_before_summary() {
        let table = [
            ((SummaryKind::Note, None, false), Need::Words),
            ((SummaryKind::Note, None, true), Need::Words),
            ((SummaryKind::Talk, None, false), Need::Nothing),
            ((SummaryKind::Note, Some("Words."), false), Need::Summary),
            ((SummaryKind::Talk, Some("- Hi"), true), Need::Nothing),
            ((SummaryKind::Talk, Some(""), false), Need::Nothing),
            ((SummaryKind::Talk, Some(" \n"), false), Need::Nothing),
        ];
        for ((kind, words, has_summary), need) in table {
            assert_eq!(need_of(kind, words, has_summary), need);
        }
        assert!(needs_summary(false, "Words.") && !needs_summary(true, "Words.") && !needs_summary(false, " \n"));
        assert_eq!(
            (source_file(SummaryKind::Note, "x"), source_file(SummaryKind::Talk, "x")),
            (sound_file("x"), words_file(SummaryKind::Talk, "x"))
        );
    }

    #[test]
    fn log_lines() {
        let line = |outcome: Result<bool, Fault>| to_log_line("n", &outcome, 12);
        assert_eq!(
            [line(Ok(true)), line(Ok(false)), line(Err(Fault::Temporary("gone".into())))],
            [Some("scribe: n: written in 12 ms".into()), None, Some("scribe: n: given up after 12 ms: gone".into())]
        );
        assert_eq!(
            [line(Err(Fault::Refused("no".into()))), line(Err(Fault::Offline("asleep".into())))],
            [Some("scribe: n: refused: no".into()), Some("scribe: n: not in a network: asleep".into())]
        );
    }
}

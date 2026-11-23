use crate::base::text::capitalize_at;
use alloc::string::String;
use core::fmt::Write;

const ACRONYMS: [&str; 10] = ["UTC", "MB", "MAH", "MA", "MV", "GIB", "CPU", "RAM", "IMEI", "PDF"];
const RESPELLINGS: [(&str, &str); 2] = [("WIFI", "WiFi"), ("OPENAI", "OpenAI")];
const SENTENCE_ENDS: [char; 3] = ['.', '?', '!'];
const ELLIPSIS: char = '\u{2026}';
const PARTICLES: [&str; 6] = ["UP", "OFF", "ON", "OUT", "IN", "DOWN"];

pub(super) fn push_lowercase(text: &mut String, word: &str) {
    word.chars().for_each(|letter| text.extend(letter.to_lowercase()));
}

fn is_ongoing(word: &str) -> bool {
    let ends = |word: &str, end: &str| {
        word.len() > end.len() && word.get(word.len() - end.len()..).is_some_and(|tail| tail.eq_ignore_ascii_case(end))
    };
    let mut parts = word.trim().split(' ');
    let first = parts.next().unwrap_or_default();
    let is_doing = ends(first, "ING") && !ends(first, "THING");
    match (parts.next(), parts.next()) {
        (None, _) => is_doing,
        (Some(particle), None) => is_doing && PARTICLES.iter().any(|known| known.eq_ignore_ascii_case(particle)),
        _ => false,
    }
}

fn write_status(word: &str, text: &mut String) {
    text.push_str(word);
    if is_ongoing(word) {
        text.push(ELLIPSIS);
    }
}

pub(super) fn write_place(text: &mut String, at: usize, count: usize) {
    let _ = write!(text, "{at} of {count}");
}

pub(super) fn write_screen_word(
    status: Option<&str>,
    place: Option<(usize, usize)>,
    subject: Option<&str>,
    text: &mut String,
) -> bool {
    match (status, place, subject) {
        (Some(word), ..) => write_status(word, text),
        (None, Some((at, count)), _) => write_place(text, at, count),
        (None, None, Some(subject)) => text.push_str(subject),
        (None, None, None) => return false,
    }
    true
}

fn write_word(part: &str, text: &mut String) {
    let letters = part.trim_matches(|letter: char| !letter.is_alphanumeric());
    let is_capitals = letters.chars().any(char::is_alphabetic) && !letters.chars().any(char::is_lowercase);
    let is_kept = ACRONYMS.contains(&letters) || letters.chars().any(|letter| letter.is_ascii_digit());
    if let Some(spelling) = RESPELLINGS.iter().find(|entry| entry.0 == letters) {
        let at = part.find(letters).unwrap_or(0);
        text.push_str(&part[..at]);
        text.push_str(spelling.1);
        text.push_str(&part[at + letters.len()..]);
    } else if is_capitals && !is_kept && letters.chars().count() > 1 {
        push_lowercase(text, part);
    } else {
        text.push_str(part);
    }
}

pub(super) fn write_robot_words(word: &str, text: &mut String, is_start: bool) {
    let (start, mut is_start) = (text.len(), is_start);
    for (at, part) in word.split(' ').enumerate() {
        if at > 0 {
            text.push(' ');
        }
        let part_start = text.len();
        write_word(part, text);
        if is_start && text[part_start..].starts_with(char::is_lowercase) {
            capitalize_at(text, part_start);
        }
        is_start = part.ends_with(SENTENCE_ENDS) || is_start && part.is_empty();
    }
    if text[start..].chars().last().is_some_and(char::is_alphabetic) {
        text.push(if is_ongoing(word) { ELLIPSIS } else { '.' });
    }
}

pub(super) fn write_robot_sentence(word: &str, text: &mut String) {
    write_robot_words(word, text, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn robot_sentences() {
        let said = |word: &str| {
            let mut text = String::new();
            write_robot_sentence(word, &mut text);
            text
        };
        for (word, sentence, reason) in [
            ("TRANSCRIBING", format!("Transcribing{ELLIPSIS}"), "what goes on trails off"),
            ("SIGNING IN", format!("Signing in{ELLIPSIS}"), "with its particle too"),
            ("NOTHING", "Nothing.".into(), "a thing goes on nowhere"),
            ("SAVING THE NOTE", "Saving the note.".into(), "nor does what more than a particle follows"),
            ("WIFI OFF", "WiFi off.".into(), "a name keeps its own spelling"),
            ("12 MB", "12 MB.".into(), "a number and a unit stay as they are"),
            ("NO OPENAI KEY", "No OpenAI key.".into(), "a name inside a sentence"),
            ("DONE. NEXT ONE", "Done. Next one.".into(), "each sentence begins with a capital"),
            ("READY?", "Ready?".into(), "what ends in a mark gets no other"),
            ("A", "A.".into(), "a single letter stays a capital"),
            ("Already Mixed", "Already Mixed.".into(), "words not all in capitals stay as written"),
        ] {
            assert_eq!(said(word), sentence, "{word:?}: {reason}");
        }
        let mut text = String::from("Before: ");
        write_robot_words("STILL GOING", &mut text, false);
        assert_eq!(text, "Before: still going.", "words inside a sentence begin small, and are appended");
    }

    #[test]
    fn screen_words() {
        let word = |status, place, subject| {
            let mut text = String::new();
            let is_written = write_screen_word(status, place, subject, &mut text);
            (is_written, text)
        };
        assert_eq!(
            word(Some("SAVING"), Some((2, 5)), Some("NOTES")),
            (true, format!("SAVING{ELLIPSIS}")),
            "a status first, trailing off while it goes on"
        );
        assert_eq!(word(Some("SAVED"), None, None), (true, "SAVED".into()));
        assert_eq!(word(None, Some((2, 5)), Some("NOTES")), (true, "2 of 5".into()), "then the place");
        assert_eq!(word(None, None, Some("NOTES")), (true, "NOTES".into()), "then the subject");
        assert_eq!(word(None, None, None), (false, String::new()), "and nothing where there is none");
    }
}

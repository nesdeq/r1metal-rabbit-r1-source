use crate::domain::paper::markdown::{self, Block};
use alloc::string::String;
use alloc::vec::Vec;

const SENTENCE_ENDS: [char; 4] = ['.', '!', '?', '\u{2026}'];
const MIN_STOP_LETTERS: usize = 2;
const SCHEMES: [&str; 2] = ["https://", "http://"];

fn is_address(word: &str) -> bool {
    let text = word.trim_start_matches(['(', '<']);
    SCHEMES.iter().any(|scheme| text.starts_with(scheme))
}

fn split_link(text: &str) -> Option<(&str, &str)> {
    let (words, tail) = text.strip_prefix('[')?.split_once(']')?;
    let (_, rest) = tail.strip_prefix('(')?.split_once(')')?;
    Some((words, rest))
}

fn skip_citation(text: &str) -> Option<&str> {
    let mut rest = text.trim_start();
    let mut has_link = false;
    loop {
        if let Some(tail) = rest.strip_prefix(')') {
            return has_link.then_some(tail);
        }
        rest = split_link(rest)?.1.trim_start().trim_start_matches([',', ';']).trim_start();
        has_link = true;
    }
}

fn to_unlinked(line: &str) -> String {
    let mut text = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(character) = rest.chars().next() {
        if let Some(tail) = rest.strip_prefix('(').and_then(skip_citation) {
            text.truncate(text.trim_end_matches(' ').len());
            rest = tail;
        } else if let Some((words, tail)) = split_link(rest) {
            text.push_str(words);
            rest = tail;
        } else {
            text.push(character);
            rest = &rest[character.len_utf8()..];
        }
    }
    text
}

fn to_plain(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = to_unlinked(line);
        let words: Vec<&str> = line.split_whitespace().filter(|word| !is_address(word)).collect();
        lines.extend(markdown::parse(&words.join(" ")).first().and_then(Block::to_voice_text));
    }
    lines.join(" ")
}

fn passage_end(text: &str, is_first: bool) -> Option<usize> {
    let mut depth = 0i32;
    let mut letters = 0usize;
    for (at, character) in text.char_indices() {
        let end = at + character.len_utf8();
        match character {
            '\n' => return Some(end),
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            _ => {}
        }
        let is_stop = SENTENCE_ENDS.contains(&character) && text[end..].starts_with(char::is_whitespace);
        if is_first && depth <= 0 && is_stop && letters >= MIN_STOP_LETTERS {
            return Some(end);
        }
        letters = if character.is_alphabetic() { letters + 1 } else { 0 };
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(super) struct Passages {
    offset: usize,
    count: usize,
}

impl Passages {
    /// The next passage as far as `reply` has grown, the rest too where `is_end`.
    pub fn next(&mut self, reply: &str, is_end: bool) -> Option<String> {
        loop {
            let rest = reply.get(self.offset..)?;
            let end = passage_end(rest, self.count == 0);
            let cut = end.or((is_end && !rest.trim().is_empty()).then_some(rest.len()))?;
            self.offset += cut;
            let passage = to_plain(&rest[..cut]);
            if !passage.is_empty() {
                self.count += 1;
                return Some(passage);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn all(reply: &str) -> Vec<String> {
        let mut passages = Passages::default();
        core::iter::from_fn(|| passages.next(reply, true)).collect()
    }

    #[test]
    fn first_sentence_then_lines() {
        let mut passages = Passages::default();
        assert_eq!(passages.next("Hallo", false), None);
        assert_eq!(passages.next("Hallo du. Wie", false).as_deref(), Some("Hallo du."));
        assert_eq!(passages.next("Hallo du. Wie geht es?", false), None);
        assert_eq!(passages.next("Hallo du. Wie geht es?\nGut", false).as_deref(), Some("Wie geht es?"));
        assert_eq!(passages.next("Hallo du. Wie geht es?\nGut", true).as_deref(), Some("Gut"));
        assert_eq!(passages.next("Hallo du. Wie geht es?\nGut", true), None);
    }

    #[test]
    fn short_word_stop() {
        assert_eq!(all("Am 16. Oktober, z. B. morgen."), vec![String::from("Am 16. Oktober, z. B. morgen.")]);
    }

    #[test]
    fn marks_links_citations_dropped() {
        assert_eq!(
            all("Das **ist** [hier](https://a.at) gut ([a](https://a.at), [b](https://b.at)).\n"),
            vec![String::from("Das ist hier gut.")]
        );
        assert_eq!(all("Siehe https://a.at dort.\n"), vec![String::from("Siehe dort.")]);
        assert_eq!(all("---\n\n"), Vec::<String>::new());
    }
}

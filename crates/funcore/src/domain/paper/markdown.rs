use alloc::string::String;
use alloc::vec::Vec;

const HEADING: char = '#';
const LEVELS: usize = 6;
const BULLETS: [char; 3] = ['-', '*', '+'];
const BULLET: &str = "\u{2022}";
const NUMBER_ENDS: [char; 2] = ['.', ')'];
const RULES: [char; 3] = ['-', '*', '_'];
const RULE_SIGNS: usize = 3;
pub(super) const INDENT: usize = 2;
const TAB: usize = 4;
const MARKS: [char; 2] = ['*', '_'];
const CODE: char = '`';
const ESCAPE: char = '\\';

// Strong, stressed, and an item's mark.
pub(super) type Style = (bool, bool, bool);

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Span {
    pub(crate) text: String,
    pub(crate) is_strong: bool,
    pub(crate) is_stressed: bool,
    pub(super) is_mark: bool,
}

impl Span {
    pub(super) const fn style(&self) -> Style {
        (self.is_strong, self.is_stressed, self.is_mark)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Block {
    Heading(u8, Vec<Span>),
    Paragraph(Vec<Span>),
    Item { mark: String, depth: usize, text: Vec<Span> },
    Rule,
}

pub(super) fn add_text(spans: &mut Vec<Span>, text: &str, style: Style) {
    match spans.last_mut() {
        Some(last) if last.style() == style => last.text.push_str(text),
        _ if text.is_empty() => {}
        _ => {
            let (is_strong, is_stressed, is_mark) = style;
            spans.push(Span { text: text.into(), is_strong, is_stressed, is_mark });
        }
    }
}

fn is_closed(rest: &[char], mark: char) -> bool {
    (1..rest.len()).any(|at| {
        rest[at] == mark && rest[at - 1] != mark && rest.get(at + 1) != Some(&mark) && !rest[at - 1].is_whitespace()
    })
}

fn parse_spans(line: &str) -> Vec<Span> {
    let characters: Vec<char> = line.chars().collect();
    // The sign that began the stress that is on, while one is.
    let (mut spans, mut is_strong, mut stress, mut at): (Vec<Span>, bool, Option<char>, usize) =
        (Vec::new(), false, None, 0);
    let mut glyph = [0u8; 4];
    let mut add_plain = |spans: &mut Vec<Span>, character: char, is_strong: bool, is_stressed: bool| {
        add_text(spans, character.encode_utf8(&mut glyph), (is_strong, is_stressed, false));
    };
    while let Some(&character) = characters.get(at) {
        let (previous, next) = (at.checked_sub(1).and_then(|index| characters.get(index)), characters.get(at + 1));
        at += 1;
        match character {
            ESCAPE if next.is_some_and(char::is_ascii_punctuation) => {
                add_plain(&mut spans, characters[at], is_strong, stress.is_some());
                at += 1;
            }
            CODE => {
                // What stands between two of these is written as it is.
                let end =
                    characters[at..].iter().position(|&other| other == CODE).map_or(characters.len(), |end| at + end);
                characters[at..end].iter().for_each(|&code| add_plain(&mut spans, code, is_strong, stress.is_some()));
                at = (end + 1).min(characters.len());
            }
            '[' => {
                // A link is written as its text; a bracket that begins none is a bracket.
                let close = characters[at..].iter().position(|&other| other == ']').map(|close| at + close);
                let target = close.filter(|&close| characters.get(close + 1) == Some(&'('));
                let end = target.and_then(|close| {
                    characters[close..].iter().position(|&other| other == ')').map(|end| (close, close + end))
                });
                match end {
                    Some((close, end)) => {
                        let text: String = characters[at..close].iter().collect();
                        for span in parse_spans(&text) {
                            let style = (is_strong || span.is_strong, stress.is_some() || span.is_stressed, false);
                            add_text(&mut spans, &span.text, style);
                        }
                        at = end + 1;
                    }
                    None => add_plain(&mut spans, character, is_strong, stress.is_some()),
                }
            }
            mark if MARKS.contains(&mark) => {
                let is_word = |other: Option<&char>| other.is_some_and(|other| other.is_alphanumeric());
                let is_blank = |other: Option<&char>| other.is_none_or(|other| other.is_whitespace());
                if next == Some(&mark) {
                    is_strong = !is_strong;
                    at += 1;
                } else if is_word(previous) && is_word(next) || is_blank(previous) && is_blank(next) {
                    // A sign inside a word, as in a file's name, and one that stands alone are themselves.
                    add_plain(&mut spans, mark, is_strong, stress.is_some());
                } else if stress == Some(mark) {
                    stress = None;
                } else if stress.is_none() && is_closed(&characters[at - 1..], mark) {
                    // A single sign stresses what follows only where another ends it: one without its other marks nothing.
                    stress = Some(mark);
                }
            }
            other => add_plain(&mut spans, other, is_strong, stress.is_some()),
        }
    }
    spans
}

fn parse_item(line: &str) -> Option<(usize, String, &str)> {
    let text = line.trim_start();
    let indent: usize = line[..line.len() - text.len()].chars().map(|blank| if blank == '\t' { TAB } else { 1 }).sum();
    let bullet_text = text.strip_prefix(BULLETS).filter(|rest| rest.starts_with([' ', '\t']));
    if let Some(rest) = bullet_text {
        return Some((indent / INDENT, BULLET.into(), rest.trim_start()));
    }
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let rest = text[digits..].strip_prefix(NUMBER_ENDS).filter(|rest| digits > 0 && rest.starts_with([' ', '\t']))?;
    Some((indent / INDENT, text[..digits + 1].into(), rest.trim_start()))
}

enum Line<'a> {
    Blank,
    Heading(u8, &'a str),
    Rule,
    Item(usize, String, &'a str),
    Text(&'a str),
}

fn parse_line(line: &str) -> Line<'_> {
    let trimmed = line.trim();
    let level = trimmed.chars().take_while(|&sign| sign == HEADING).count();
    let is_heading = trimmed[level..].starts_with([' ', '\t']) || trimmed.len() == level;
    let is_rule = RULES.iter().any(|&sign| {
        trimmed.chars().all(|other| other == sign || other == ' ')
            && trimmed.chars().filter(|&other| other == sign).count() >= RULE_SIGNS
    });
    if trimmed.is_empty() {
        Line::Blank
    } else if (1..=LEVELS).contains(&level) && is_heading {
        Line::Heading(level as u8, trimmed[level..].trim().trim_end_matches(HEADING).trim_end())
    } else if is_rule {
        Line::Rule
    } else if let Some((depth, mark, text)) = parse_item(line) {
        Line::Item(depth, mark, text)
    } else {
        Line::Text(trimmed)
    }
}

pub(crate) struct Blocks<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Blocks<'a> {
    pub(crate) fn new(text: &'a str, at: usize) -> Blocks<'a> {
        Blocks { text, at }
    }

    fn line_at(&self, at: usize) -> Option<(&'a str, usize)> {
        let rest = self.text.get(at..).filter(|rest| !rest.is_empty())?;
        let (line, length) = rest.split_once('\n').map_or((rest, rest.len()), |(line, _)| (line, line.len() + 1));
        Some((line.strip_suffix('\r').unwrap_or(line), at + length))
    }
}

impl Iterator for Blocks<'_> {
    type Item = (Block, usize);

    fn next(&mut self) -> Option<(Block, usize)> {
        loop {
            let (line, next_at) = self.line_at(self.at)?;
            self.at = next_at;
            let (mut buffer, item) = match parse_line(line) {
                Line::Blank => continue,
                Line::Heading(level, text) => return Some((Block::Heading(level, parse_spans(text)), next_at)),
                Line::Rule => return Some((Block::Rule, next_at)),
                Line::Item(depth, mark, text) => (String::from(text), Some((mark, depth))),
                Line::Text(first) => (String::from(first), None),
            };
            // A line that begins nothing of its own goes on with the paragraph or the item before it.
            while let Some((text, next_at)) = self.line_at(self.at).and_then(|(line, next_at)| match parse_line(line) {
                Line::Text(text) => Some((text, next_at)),
                _ => None,
            }) {
                buffer.push(' ');
                buffer.push_str(text);
                self.at = next_at;
            }
            let text = parse_spans(&buffer);
            let block = match item {
                Some((mark, depth)) => Block::Item { mark, depth, text },
                None => Block::Paragraph(text),
            };
            return Some((block, self.at));
        }
    }
}

pub(crate) fn parse(text: &str) -> Vec<Block> {
    Blocks::new(text, 0).map(|(block, _)| block).collect()
}

pub(crate) fn to_opening(text: &str, limit: usize) -> String {
    let (mut lines, mut block, mut is_read) = (text.lines(), String::new(), false);
    let mut window = limit.max(1);
    loop {
        // The block's lines are joined as far as `window` characters are sure to lie in them.
        while !is_read && block.len() <= window * char::MAX.len_utf8() {
            match lines.next().map(parse_line) {
                Some(Line::Blank | Line::Rule) if block.is_empty() => {}
                Some(Line::Heading(_, text)) if block.is_empty() => (block, is_read) = (text.into(), !text.is_empty()),
                Some(Line::Item(_, _, text)) if block.is_empty() => block.push_str(text),
                Some(Line::Text(line)) => {
                    if !block.is_empty() {
                        block.push(' ');
                    }
                    block.push_str(line);
                }
                _ => is_read = true,
            }
        }
        // A part of the block reads as the whole does if it ends before a blank and every bracket in it is closed within it: only a link is told by what follows.
        let cut = block.char_indices().skip(window).find(|(_, character)| character.is_whitespace()).map(|(at, _)| at);
        let part = cut.map_or(block.as_str(), |at| &block[..at]);
        let is_link_closed = |rest: &str| {
            rest.find(']').is_some_and(|close| !rest[close + 1..].starts_with('(') || rest[close..].contains(')'))
        };
        let is_complete = is_read && cut.is_none();
        if is_complete || part.match_indices('[').all(|(at, _)| is_link_closed(&part[at..])) {
            let opening: String = parse_spans(part).iter().flat_map(|span| span.text.chars()).take(limit).collect();
            if is_complete || opening.chars().count() >= limit {
                return opening;
            }
        }
        window *= 2;
    }
}

impl Block {
    pub(crate) const fn is_item(&self) -> bool {
        matches!(self, Block::Item { .. })
    }

    pub(crate) fn to_voice_text(&self) -> Option<String> {
        let spans = match self {
            Block::Heading(_, spans) | Block::Paragraph(spans) | Block::Item { text: spans, .. } => spans,
            Block::Rule => return None,
        };
        let words: String = spans.iter().map(|span| span.text.as_str()).collect();
        Some(words.trim().into()).filter(|words: &String| !words.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn span(text: &str, is_strong: bool, is_stressed: bool) -> Span {
        Span { text: text.into(), is_strong, is_stressed, is_mark: false }
    }

    fn plain(text: &str) -> Vec<Span> {
        vec![span(text, false, false)]
    }

    #[test]
    fn block_kinds() {
        let text = "# Note of today\n\nA plan for **Friday**,\nmade by _all_ of us.\n\n---\n\n## Decided ##\n- Go ahead\n  with it\n* `gpt_6` stays\n  - deeper\n1. first\n12) twelfth\n\n#hash and -dash are text\n";
        assert_eq!(
            parse(text),
            [
                Block::Heading(1, plain("Note of today")),
                Block::Paragraph(vec![
                    span("A plan for ", false, false),
                    span("Friday", true, false),
                    span(", made by ", false, false),
                    span("all", false, true),
                    span(" of us.", false, false)
                ]),
                Block::Rule,
                Block::Heading(2, plain("Decided")),
                Block::Item { mark: "\u{2022}".into(), depth: 0, text: plain("Go ahead with it") },
                Block::Item { mark: "\u{2022}".into(), depth: 0, text: plain("gpt_6 stays") },
                Block::Item { mark: "\u{2022}".into(), depth: 1, text: plain("deeper") },
                Block::Item { mark: "1.".into(), depth: 0, text: plain("first") },
                Block::Item { mark: "12)".into(), depth: 0, text: plain("twelfth") },
                Block::Paragraph(plain("#hash and -dash are text")),
            ]
        );
        assert!(parse("").is_empty() && parse("\n \n").is_empty());
    }

    #[test]
    fn blocks_from_offset() {
        let text = "# Head\r\n\r\nOne\ntwo.\n\n- item\n  goes on\n- next\n---\nEnd";
        let all: Vec<(Block, usize)> = Blocks::new(text, 0).collect();
        let blocks = parse(text);
        assert_eq!(all.iter().map(|(block, _)| block.clone()).collect::<Vec<Block>>(), blocks);
        assert_eq!(blocks.len(), 6, "a heading, a paragraph, two items, a line across, a paragraph");
        for (number, (_, next_at)) in all.iter().enumerate() {
            let rest: Vec<Block> = Blocks::new(text, *next_at).map(|(block, _)| block).collect();
            assert_eq!(rest, blocks[number + 1..], "from behind block {number} on, the blocks after it");
        }
        assert_eq!(all.last().map(|(_, next_at)| *next_at), Some(text.len()), "the last block ends the document");
        assert!(Blocks::new(text, text.len()).next().is_none() && Blocks::new(text, text.len() + 9).next().is_none());
        assert!(Blocks::new("K\u{E4}se", 2).next().is_none(), "a place inside a character is no place to read from");
        assert_eq!(Blocks::new("One\n\n\n", 0).map(|(_, next_at)| next_at).collect::<Vec<usize>>(), [4]);
    }

    #[test]
    fn opening_of_first_block() {
        fn full_opening(text: &str, limit: usize) -> String {
            let first = parse(text).into_iter().find_map(|block| match block {
                Block::Heading(_, spans) | Block::Paragraph(spans) | Block::Item { text: spans, .. } => Some(spans),
                Block::Rule => None,
            });
            first.iter().flatten().flat_map(|span| span.text.chars()).take(limit).collect()
        }
        let texts = [
            "# Note of today\n\nA plan for **Friday**.",
            "---\n\n- **strong** item that\n  goes on over lines\n- next",
            "\n\nA plan for **Friday**,\nmade by _all_ of us in the room today, and `co*de_` too.\n\nMore.",
            "See [the transcript of the whole meeting](20261013_090502_transcript.md) and [more] of it, and so on and on.",
            "A [link whose address is cut by any short part](https://example.org/a/very/long/address/that/goes/on/and/on) end",
            "One word after another, and a_b and 5 * 3, \\*stars\\* and an open [bracket that is never closed at all",
            "W\u{F6}rter mit Umlauten: K\u{E4}se, Br\u{F6}tchen und **Stra\u{DF}e**, immer weiter und weiter und weiter.",
            "",
            "---\n\n***\n",
        ];
        for text in texts {
            for limit in [0, 1, 5, 12, 20, 40, 400] {
                assert_eq!(to_opening(text, limit), full_opening(text, limit), "{limit} characters of {text:?}");
            }
        }
        assert_eq!(to_opening("# Note of today\n\nA plan.", 9), "Note of t");
        let long = "Words that were said in a meeting, on one line and on the next.\n".repeat(4000);
        assert_eq!(to_opening(&long, 30), full_opening(&long, 30));
        let (start, _) = (crate::base::alloc_count::allocations(), to_opening(&long, 30));
        let count = crate::base::alloc_count::allocations() - start;
        let (start, _) = (crate::base::alloc_count::allocations(), to_opening(&long[..640], 30));
        assert_eq!(
            crate::base::alloc_count::allocations() - start,
            count,
            "a text of 256 KB takes as little to open as one of ten lines"
        );
    }

    #[test]
    fn mark_signs() {
        let text = |line: &str| parse_spans(line).iter().map(|span| span.text.as_str()).collect::<String>();
        assert_eq!(text("the file 20261012_190328.m4a, 5 * 3 and a_b"), "the file 20261012_190328.m4a, 5 * 3 and a_b");
        assert_eq!(text("*stressed* and _stressed_ and `co*de_`"), "stressed and stressed and co*de_");
        assert_eq!(
            text("a [link](https://example.org/a_b) and [no link] and \\*stars\\*"),
            "a link and [no link] and *stars*"
        );
        assert_eq!(
            parse_spans("**all** of __it__ and **[this](x)**"),
            [
                span("all", true, false),
                span(" of ", false, false),
                span("it", true, false),
                span(" and ", false, false),
                span("this", true, false)
            ]
        );
        assert_eq!(text("an open `code and an open [bracket"), "an open code and an open [bracket");
        assert!(parse_spans("").is_empty() && parse_spans("****").is_empty());
    }

    #[test]
    fn stress_signs() {
        assert_eq!(
            parse_spans("The *Financial Times* and _Nikkei_ say so."),
            [
                span("The ", false, false),
                span("Financial Times", false, true),
                span(" and ", false, false),
                span("Nikkei", false, true),
                span(" say so.", false, false)
            ]
        );
        assert_eq!(
            parse_spans("***both*** and *one **two** three*"),
            [
                span("both", true, true),
                span(" and ", false, false),
                span("one ", false, true),
                span("two", true, true),
                span(" three", false, true)
            ],
            "strong and stressed at once, either inside the other"
        );
        assert_eq!(
            parse_spans("a [*linked* title](x) and `*code*`"),
            [span("a ", false, false), span("linked", false, true), span(" title and *code*", false, false)],
            "a link's text is read as any other, and what is code is left as it is"
        );
        assert_eq!(
            parse_spans("up 5%* this year and *more to come"),
            plain("up 5% this year and more to come"),
            "a sign no other ends stresses nothing, and is left out"
        );
        assert_eq!(
            parse_spans("*one _two* three_"),
            [span("one two", false, true), span(" three", false, false)],
            "the sign that began a stress ends it, and no other"
        );
    }
}

use super::markdown::{Block, INDENT, Span, Style, add_text};
use crate::gfx::wrap::{self, Rows, Step};
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Heading(u8),
    Text,
    Rule,
    Gap,
}

impl Kind {
    pub(crate) const fn height(self, line: i32, large: i32) -> i32 {
        match self {
            Kind::Heading(_) => large,
            Kind::Text => line,
            Kind::Rule | Kind::Gap => line / 2,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Row {
    pub(crate) kind: Kind,
    pub(crate) indent: usize,
    pub(crate) spans: Vec<Span>,
}

impl Row {
    // The row that parts what stands over it from what stands under it.
    pub(crate) const fn gap() -> Row {
        Row { kind: Kind::Gap, indent: 0, spans: Vec::new() }
    }

    // A row of text written one way throughout.
    pub(crate) fn plain(text: String, is_stressed: bool) -> Row {
        let span = Span { text, is_strong: false, is_stressed, is_mark: false };
        Row { kind: Kind::Text, indent: 0, spans: alloc::vec![span] }
    }

    fn to_plain(&self) -> String {
        let blanks = core::iter::repeat_n(' ', self.indent);
        blanks.chain(self.spans.iter().flat_map(|span| span.text.chars())).collect()
    }

    pub(crate) fn is_marked(&self, span: &Span) -> bool {
        matches!(self.kind, Kind::Heading(_)) || span.is_mark || span.is_strong || span.is_stressed
    }
}

fn to_styled(spans: &[Span]) -> impl Iterator<Item = (char, Style)> + '_ {
    spans.iter().flat_map(|span| span.text.chars().map(move |character| (character, span.style())))
}

fn wrap(
    characters: impl Iterator<Item = (char, Style)>,
    kind: Kind,
    width: usize,
    first_indent: usize,
    indent: usize,
    rows: &mut Vec<Row>,
) {
    let styled: Vec<(char, Style)> = characters.collect();
    let mut row = Row { kind, indent: first_indent, spans: Vec::new() };
    let mut glyph = [0u8; 4];
    let words = styled.split(|(character, _)| character.is_whitespace()).filter(|word| !word.is_empty());
    let shape = Rows { width, first_indent, indent, is_empty_word_text: false };
    wrap::walk(words, shape, |step| match step {
        Step::Piece { word, has_blank } => {
            if has_blank {
                // The blank between two words written one way is written that way with them, so that they stay one span; a mark stands by itself.
                let (is_strong, is_stressed) =
                    row.spans.last().map_or((false, false), |span| (span.is_strong, span.is_stressed));
                let (is_next_strong, is_next_stressed, _) = word[0].1;
                add_text(&mut row.spans, " ", (is_strong && is_next_strong, is_stressed && is_next_stressed, false));
            }
            for &(character, style) in word {
                add_text(&mut row.spans, character.encode_utf8(&mut glyph), style);
            }
        }
        Step::RowEnd => rows.push(core::mem::replace(&mut row, Row { kind, indent, spans: Vec::new() })),
    });
    if !row.spans.is_empty() {
        rows.push(row);
    }
}

pub(crate) fn wrap_lines(text: &str, width: usize, indent: usize) -> Vec<String> {
    let (width, mut rows) = (width.max(1), Vec::new());
    let indent = indent.min(width / 2);
    wrap(
        text.chars().map(|character| (character, (false, false, false))),
        Kind::Text,
        width,
        indent,
        indent,
        &mut rows,
    );
    rows.iter().map(Row::to_plain).collect()
}

pub(crate) fn lay_block(
    block: &Block,
    previous: Option<bool>,
    width: usize,
    heading_width: usize,
    rows: &mut Vec<Row>,
) {
    let (width, heading_width) = (width.max(1), heading_width.max(1));
    if previous.is_some_and(|is_item| !(is_item && block.is_item())) {
        rows.push(Row::gap());
    }
    match block {
        Block::Heading(level, spans) => wrap(to_styled(spans), Kind::Heading(*level), heading_width, 0, 0, rows),
        Block::Paragraph(spans) => wrap(to_styled(spans), Kind::Text, width, 0, 0, rows),
        Block::Item { mark, depth, text } => {
            // The mark stands out to the left of the item's text, however many rows that takes, as a span of its own.
            let (indent, mark_width) = ((depth * INDENT).min(width / 2), mark.chars().count() + 1);
            let head =
                mark.chars().map(|character| (character, (false, false, true))).chain([(' ', (false, false, false))]);
            wrap(head.chain(to_styled(text)), Kind::Text, width, indent, (indent + mark_width).min(width / 2), rows);
        }
        Block::Rule => rows.push(Row { kind: Kind::Rule, indent: 0, spans: Vec::new() }),
    }
}

pub(super) fn lay_out(blocks: &[Block], width: usize, heading_width: usize) -> Vec<Row> {
    let (mut rows, mut previous) = (Vec::new(), None);
    for block in blocks {
        lay_block(block, previous, width, heading_width, &mut rows);
        previous = Some(block.is_item());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::super::markdown::parse;
    use super::*;
    use alloc::vec;

    fn span(text: &str, is_strong: bool, is_stressed: bool) -> Span {
        Span { text: text.into(), is_strong, is_stressed, is_mark: false }
    }

    fn to_texts(rows: &[Row]) -> Vec<(Kind, String)> {
        rows.iter().map(|row| (row.kind, row.to_plain())).collect()
    }

    #[test]
    fn stress_over_rows() {
        let rows = lay_out(&parse("say *one two three four* now"), 9, 9);
        assert_eq!(
            to_texts(&rows),
            [(Kind::Text, "say one".into()), (Kind::Text, "two three".into()), (Kind::Text, "four now".into())]
        );
        assert_eq!(
            (&rows[1].spans, &rows[2].spans),
            (&vec![span("two three", false, true)], &vec![span("four", false, true), span(" now", false, false)]),
            "stressed words stay so over the rows they are broken into, the blank between two of them with them"
        );
    }

    #[test]
    fn block_rows() {
        let blocks = parse(
            "## A heading that is long\n\nOne two three four five six.\n\n- an item that goes on\n- next\n\n---\n\nEnd",
        );
        assert_eq!(
            to_texts(&lay_out(&blocks, 12, 9)),
            [
                (Kind::Heading(2), "A heading".into()),
                (Kind::Heading(2), "that is".into()),
                (Kind::Heading(2), "long".into()),
                (Kind::Gap, String::new()),
                (Kind::Text, "One two".into()),
                (Kind::Text, "three four".into()),
                (Kind::Text, "five six.".into()),
                (Kind::Gap, String::new()),
                (Kind::Text, "\u{2022} an item".into()),
                (Kind::Text, "  that goes".into()),
                (Kind::Text, "  on".into()),
                (Kind::Text, "\u{2022} next".into()),
                (Kind::Gap, String::new()),
                (Kind::Rule, String::new()),
                (Kind::Gap, String::new()),
                (Kind::Text, "End".into())
            ],
            "an item's rows stand in from its mark, and no gap parts two items"
        );
        let nested = parse("- a\n  - bb cc\n    1. ddd eee");
        assert_eq!(
            to_texts(&lay_out(&nested, 8, 8)),
            [
                (Kind::Text, "\u{2022} a".into()),
                (Kind::Text, "  \u{2022} bb".into()),
                (Kind::Text, "    cc".into()),
                (Kind::Text, "    1.".into()),
                (Kind::Text, "    ddd".into()),
                (Kind::Text, "    eee".into())
            ],
            "an indent never takes more than half a row"
        );
    }

    #[test]
    fn marked_runs() {
        fn marked(row: &Row) -> Vec<(&str, bool)> {
            row.spans.iter().map(|span| (span.text.as_str(), row.is_marked(span))).collect()
        }
        let rows = lay_out(
            &parse("## A **head**\n\nPlain and **strong** and *stressed*.\n\n- an item\n12. **Ben** calls"),
            40,
            40,
        );
        assert_eq!(
            rows.iter().filter(|row| row.kind != Kind::Gap).map(marked).collect::<Vec<_>>(),
            [
                vec![("A ", true), ("head", true)],
                vec![("Plain and ", false), ("strong", true), (" and ", false), ("stressed", true), (".", false)],
                vec![("\u{2022}", true), (" an item", false)],
                vec![("12.", true), (" ", false), ("Ben", true), (" calls", false)]
            ],
            "all of a heading, strong and stressed words, and the mark of an item, which is a run of its own"
        );
        let mark = &rows.iter().find(|row| row.to_plain().starts_with('\u{2022}')).expect("the item's row").spans[0];
        assert!(
            mark.is_mark && !mark.is_strong && !mark.is_stressed,
            "a mark is neither strong nor stressed: {mark:?}"
        );
        let item_rows = lay_out(&parse("- one two three"), 7, 7);
        assert_eq!(
            item_rows.iter().map(marked).collect::<Vec<_>>(),
            [vec![("\u{2022}", true), (" one", false)], vec![("two", false)], vec![("three", false)]],
            "the rows an item goes on in have no mark"
        );
    }

    #[test]
    fn long_word_cut() {
        let rows = lay_out(&parse("ab **cdefghijkl** m"), 5, 5);
        assert_eq!(
            to_texts(&rows),
            [
                (Kind::Text, "ab".into()),
                (Kind::Text, "cdefg".into()),
                (Kind::Text, "hijkl".into()),
                (Kind::Text, "m".into())
            ]
        );
        assert!(rows[1].spans[0].is_strong && rows[2].spans[0].is_strong && !rows[3].spans[0].is_strong);
        let mixed = lay_out(&parse("one **two** three"), 80, 80);
        assert_eq!(
            mixed[0].spans,
            [span("one ", false, false), span("two", true, false), span(" three", false, false)]
        );
        assert!(mixed.iter().chain(&rows).all(|row| row.to_plain().chars().count() <= 80));
        for width in 1..12 {
            let rows =
                lay_out(&parse("# H\u{E4}ding\n\n- K\u{E4}se und **Br\u{F6}tchen** kaufen\n\n12. x"), width, width);
            assert!(
                rows.iter().all(|row| row.to_plain().chars().count() <= width),
                "rows of {width}: {:?}",
                to_texts(&rows)
            );
        }
    }

    #[test]
    fn plain_wrap() {
        assert_eq!(
            wrap_lines("Wo ist  der *Bahnhof*,\nbitte?", 12, 0),
            ["Wo ist der", "*Bahnhof*,", "bitte?"],
            "its signs are its own, and its blanks count once"
        );
        assert_eq!(wrap_lines("Where is the station?", 12, 2), ["  Where is", "  the", "  station?"]);
        assert_eq!(wrap_lines("ab", 2, 5), [" a", " b"], "an indent never takes more than half a line");
        assert!(wrap_lines("", 12, 2).is_empty() && wrap_lines("  \n ", 12, 0).is_empty());
    }

    #[test]
    fn made_rows() {
        assert_eq!(Row::gap(), Row { kind: Kind::Gap, indent: 0, spans: Vec::new() });
        assert_eq!(
            Row::plain("read, auto mode".into(), true),
            Row { kind: Kind::Text, indent: 0, spans: vec![span("read, auto mode", false, true)] },
            "one run, stressed or not, never strong and no mark"
        );
    }
}

use super::glyph;

pub(crate) trait Word: Copy {
    fn is_empty(self) -> bool;
    fn cells(self) -> usize;
    fn cut_at(self, room: usize) -> usize;
    fn split(self, at: usize) -> (Self, Self);
}

impl Word for &str {
    fn is_empty(self) -> bool {
        str::is_empty(self)
    }

    fn cells(self) -> usize {
        glyph::cells_of(self)
    }

    fn cut_at(self, room: usize) -> usize {
        self.char_indices().nth(glyph::fit(self.chars(), room)).map_or(self.len(), |(at, _)| at)
    }

    fn split(self, at: usize) -> (Self, Self) {
        str::split_at(self, at)
    }
}

impl<S: Copy> Word for &[(char, S)] {
    fn is_empty(self) -> bool {
        <[(char, S)]>::is_empty(self)
    }

    fn cells(self) -> usize {
        glyph::cells_in(self.iter().map(|(character, _)| *character))
    }

    fn cut_at(self, room: usize) -> usize {
        glyph::fit(self.iter().map(|(character, _)| *character), room)
    }

    fn split(self, at: usize) -> (Self, Self) {
        <[(char, S)]>::split_at(self, at)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Rows {
    pub(crate) width: usize,
    pub(crate) first_indent: usize,
    pub(crate) indent: usize,
    // A sentence counts a word of no cells as text, an empty tail too; a page does not.
    pub(crate) is_empty_word_text: bool,
}

pub(crate) enum Step<W> {
    Piece { word: W, has_blank: bool },
    RowEnd,
}

pub(crate) fn walk<W: Word>(words: impl Iterator<Item = W>, rows: Rows, mut step: impl FnMut(Step<W>)) {
    let (mut column, mut indent, mut has_piece) = (rows.first_indent, rows.first_indent, false);
    for word in words {
        let mut rest = word;
        while !rest.is_empty() {
            let has_text = column > indent || rows.is_empty_word_text && has_piece;
            let (blank, cells) = (usize::from(has_text), rest.cells());
            if column + blank + cells <= rows.width {
                step(Step::Piece { word: rest, has_blank: has_text });
                (column, has_piece) = (column + blank + cells, true);
                break;
            }
            // A word that does not fit begins the next row; one longer than a whole row is cut where its row ends.
            let cut = if has_text { 0 } else { rest.cut_at(rows.width.saturating_sub(column)) };
            let (head, tail) = rest.split(cut);
            if cut > 0 {
                step(Step::Piece { word: head, has_blank: false });
            }
            step(Step::RowEnd);
            (rest, column, indent, has_piece) = (tail, rows.indent, rows.indent, false);
            if rows.is_empty_word_text && rest.is_empty() {
                step(Step::Piece { word: rest, has_blank: false });
                has_piece = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn steps<'a>(words: &[&'a str], width: usize, is_empty_word_text: bool) -> Vec<Option<(&'a str, bool)>> {
        let (rows, mut steps) = (Rows { width, first_indent: 0, indent: 0, is_empty_word_text }, Vec::new());
        walk(words.iter().copied(), rows, |step| {
            steps.push(match step {
                Step::Piece { word, has_blank } => Some((word, has_blank)),
                Step::RowEnd => None,
            });
        });
        steps
    }

    #[test]
    fn words_of_no_cells() {
        let unseen = ["\u{200B}", "ab"];
        assert_eq!(
            steps(&unseen, 2, true),
            [Some(("\u{200B}", false)), None, Some(("ab", false))],
            "in a sentence a word of no cells is text: the next stands a blank after it, and so on the next row"
        );
        assert_eq!(
            steps(&unseen, 2, false),
            [Some(("\u{200B}", false)), Some(("ab", false))],
            "on a page it takes no room, and the next begins the row"
        );
        let wide = ["\u{1F600}"];
        assert_eq!(
            steps(&wide, 1, true),
            [Some(("\u{1F600}", false)), None, Some(("", false))],
            "a glyph wider than its row stands whole, and a sentence counts the empty rest as a row's text"
        );
        assert_eq!(steps(&wide, 1, false), [Some(("\u{1F600}", false)), None], "which a page does not");
        assert_eq!(
            steps(&["abcdef", "g"], 4, true),
            [Some(("abcd", false)), None, Some(("ef", false)), Some(("g", true))],
            "a word longer than a row is cut where the row ends, and its rest is text like any"
        );
    }
}

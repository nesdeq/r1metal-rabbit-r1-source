use super::emoji::{self, Emoji, PICTURE, TONES};
use super::unifont::{self, Drawing};
use core::ops::RangeInclusive;

const ASCII_END: char = '\u{80}';
const JOINER: char = '\u{200D}';
const KEYCAP: char = '\u{20E3}';
const FLAG_LETTERS: RangeInclusive<char> = '\u{1F1E6}'..='\u{1F1FF}';
const TAGS: RangeInclusive<char> = '\u{E0020}'..='\u{E007E}';
const TAGS_END: char = '\u{E007F}';
// Unicode 18's Default_Ignorable_Code_Point.
const UNSEEN: [RangeInclusive<char>; 17] = [
    '\u{AD}'..='\u{AD}',
    '\u{34F}'..='\u{34F}',
    '\u{61C}'..='\u{61C}',
    '\u{115F}'..='\u{1160}',
    '\u{17B4}'..='\u{17B5}',
    '\u{180B}'..='\u{180F}',
    '\u{200B}'..='\u{200F}',
    '\u{202A}'..='\u{202E}',
    '\u{2060}'..='\u{206F}',
    '\u{3164}'..='\u{3164}',
    '\u{FE00}'..='\u{FE0F}',
    '\u{FEFF}'..='\u{FEFF}',
    '\u{FFA0}'..='\u{FFA0}',
    '\u{FFF0}'..='\u{FFF8}',
    '\u{1BCA0}'..='\u{1BCA3}',
    '\u{1D173}'..='\u{1D17A}',
    '\u{E0000}'..='\u{E0FFF}',
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Glyph {
    Letter,
    Nothing,
    Drawing(Drawing),
    Emoji(Emoji),
}

impl Glyph {
    pub(super) const fn cells(self) -> usize {
        match self {
            Glyph::Letter => 1,
            Glyph::Nothing => 0,
            Glyph::Drawing(drawing) => drawing.width.div_ceil(u8::BITS) as usize,
            Glyph::Emoji(_) => 2,
        }
    }
}

fn is_unseen(character: char) -> bool {
    UNSEEN.iter().any(|unseen| unseen.contains(&character)) || TONES.contains(&character) || character == KEYCAP
}

// An emoji's sequence as far as it is read: its key, and each place it may end at, by its key's length and its characters.
struct Sequence {
    key: [char; emoji::KEY_CODES],
    codes: usize,
    length: usize,
    ends: [(usize, usize); emoji::KEY_CODES],
    count: usize,
}

impl Sequence {
    fn push(&mut self, character: char) -> bool {
        let has_room = self.codes < self.key.len();
        if has_room {
            self.key[self.codes] = character;
            self.codes += 1;
            self.length += 1;
        }
        has_room
    }

    fn end(&mut self) {
        self.ends[self.count] = (self.codes, self.length);
        self.count += 1;
    }

    // The longest of its ends that is an emoji, with the characters it takes.
    fn longest(&self, is_asked: bool) -> Option<(Emoji, usize)> {
        self.ends[..self.count].iter().rev().find_map(|&(codes, length)| {
            let emoji = emoji::find(&self.key[..codes])?;
            (codes > 1 || is_asked || emoji.is_shown_bare()).then_some((emoji, length))
        })
    }
}

// What `first` and the characters after it draw as, and how many of them that takes.
fn read(first: char, rest: impl Iterator<Item = char> + Clone) -> (Glyph, usize) {
    let mut rest = rest.peekable();
    let next = rest.peek().copied();
    if first < ASCII_END && !matches!(next, Some(PICTURE | KEYCAP)) {
        return (Glyph::Letter, 1);
    }
    if is_unseen(first) {
        return (Glyph::Nothing, 1);
    }
    let mut sequence =
        Sequence { key: [first; emoji::KEY_CODES], codes: 1, length: 1, ends: [(0, 0); emoji::KEY_CODES], count: 0 };
    let is_asked = next == Some(PICTURE);
    if FLAG_LETTERS.contains(&first) {
        sequence.end();
        if rest.next_if(|second| FLAG_LETTERS.contains(second)).is_some_and(|second| sequence.push(second)) {
            sequence.end();
        }
    } else {
        loop {
            while rest.next_if(|after| *after == PICTURE || TONES.contains(after)).is_some() {
                sequence.length += 1;
            }
            sequence.end();
            let is_first = sequence.count == 1;
            if is_first && rest.next_if_eq(&KEYCAP).is_some_and(|keycap| sequence.push(keycap)) {
                sequence.end();
                break;
            }
            if is_first && rest.peek().is_some_and(|after| TAGS.contains(after)) {
                let mut is_whole = true;
                while let Some(tag) = rest.next_if(|after| TAGS.contains(after)) {
                    is_whole &= sequence.push(tag);
                }
                if is_whole && rest.next_if_eq(&TAGS_END).is_some_and(|end| sequence.push(end)) {
                    sequence.end();
                }
                break;
            }
            let mut ahead = rest.clone();
            match (ahead.next(), ahead.next()) {
                (Some(JOINER), Some(joined)) if !is_unseen(joined) && sequence.codes + 2 <= sequence.key.len() => {
                    sequence.push(JOINER);
                    sequence.push(joined);
                    rest = ahead;
                }
                _ => break,
            }
        }
    }
    match sequence.longest(is_asked) {
        Some((emoji, length)) => (Glyph::Emoji(emoji), length),
        None => (unifont::find(first).map_or(Glyph::Letter, Glyph::Drawing), 1),
    }
}

// What each run of `characters` draws as, with the character it begins with and how many it takes.
pub(super) fn glyphs(characters: impl Iterator<Item = char> + Clone) -> impl Iterator<Item = (Glyph, char, usize)> {
    let mut rest = characters;
    core::iter::from_fn(move || {
        let first = rest.next()?;
        let (glyph, length) = read(first, rest.clone());
        if length > 1 {
            rest.nth(length - 2);
        }
        Some((glyph, first, length))
    })
}

pub(super) fn cells_in(characters: impl Iterator<Item = char> + Clone) -> usize {
    glyphs(characters).map(|(glyph, _, _)| glyph.cells()).sum()
}

pub(crate) fn cells_of(text: &str) -> usize {
    cells_in(text.chars())
}

// Each run of `text` as what draws letters alone writes it: its character where it is a letter, none where the screen draws a picture for it, and the cells it takes.
pub(crate) fn letters(text: &str) -> impl Iterator<Item = (Option<char>, usize)> + '_ {
    glyphs(text.chars()).map(|(glyph, first, _)| ((glyph == Glyph::Letter).then_some(first), glyph.cells()))
}

// Whether `text` is one emoji the screen draws, and nothing beside it but what is drawn as nothing.
pub(crate) fn is_emoji(text: &str) -> bool {
    let mut drawn = glyphs(text.chars()).map(|(glyph, _, _)| glyph).filter(|glyph| *glyph != Glyph::Nothing);
    matches!((drawn.next(), drawn.next()), (Some(Glyph::Emoji(_)), None))
}

// How many of `characters` stand in `room` cells as whole glyphs, the first glyph whatever it takes.
pub(super) fn fit(characters: impl Iterator<Item = char> + Clone, room: usize) -> usize {
    let (mut used, mut fitting) = (0, 0);
    for (glyph, _, length) in glyphs(characters) {
        used += glyph.cells();
        if used > room && fitting > 0 {
            break;
        }
        fitting += length;
    }
    fitting
}

// Where in `text` the glyph begins that outgrows `room` cells.
pub(crate) fn overflow_at(text: &str, room: usize) -> Option<usize> {
    let (mut used, mut places) = (0, text.char_indices());
    glyphs(text.chars()).find_map(|(glyph, _, length)| {
        let at = places.next().map(|(at, _)| at);
        if length > 1 {
            places.nth(length - 2);
        }
        used += glyph.cells();
        (used > room).then_some(at).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn kinds(text: &str) -> Vec<(char, usize, usize)> {
        glyphs(text.chars()).map(|(glyph, first, length)| (first, length, glyph.cells())).collect()
    }

    #[test]
    fn letters_drawings_and_unseen() {
        assert_eq!(kinds("a\u{E4}"), [('a', 1, 1), ('\u{E4}', 1, 1)], "Spleen's own letters, a cell each");
        let wide = glyphs("\u{4E2D}".chars()).next().map(|(glyph, _, _)| glyph);
        assert!(
            matches!(wide, Some(Glyph::Drawing(Drawing { width: 16, .. }))),
            "a character Spleen lacks is Unifont's"
        );
        assert_eq!(kinds("\u{4E2D}\u{5D0}"), [('\u{4E2D}', 1, 2), ('\u{5D0}', 1, 1)], "two cells wide, or one");
        for unseen in ['\u{FE0F}', '\u{200D}', '\u{20E3}', '\u{1F3FD}', '\u{E0067}', '\u{200B}', '\u{FEFF}', '\u{AD}'] {
            assert_eq!(kinds(unseen.encode_utf8(&mut [0; 4])), [(unseen, 1, 0)], "{unseen:?} is drawn as nothing");
        }
        for letter in ['\u{2665}', '\u{2192}', '\u{10FFFF}', '\u{E000}'] {
            let glyph = glyphs([letter].into_iter()).next().map(|(glyph, _, _)| glyph);
            assert_eq!(glyph, Some(Glyph::Letter), "{letter:?} is the font's, drawn or not");
        }
    }

    #[test]
    fn emoji_sequences() {
        let emoji = |text: &str| {
            let read: Vec<(bool, usize)> = glyphs(text.chars())
                .map(|(glyph, _, length)| (matches!(glyph, Glyph::Emoji(_)), length))
                .filter(|(_, length)| *length > 0)
                .collect();
            read
        };
        assert_eq!(emoji("\u{1F600}"), [(true, 1)], "a face by itself");
        assert_eq!(emoji("\u{2764}\u{FE0F}"), [(true, 2)], "a heart asked for as a picture");
        assert_eq!(emoji("\u{2764}"), [(false, 1)], "and a heart by itself, which is writing");
        assert_eq!(emoji("\u{1F44D}\u{1F3FD}"), [(true, 2)], "a tone of skin goes with its emoji and changes nothing");
        assert_eq!(emoji("\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7}"), [(true, 2), (true, 2)], "two flags of two letters");
        assert_eq!(emoji("\u{1F1E9}a"), [(true, 1), (false, 1)], "a flag's letter alone is its own picture");
        assert_eq!(emoji("1\u{FE0F}\u{20E3}2"), [(true, 3), (false, 1)], "a key cap, and a digit");
        assert_eq!(emoji("#\u{20E3}"), [(true, 2)]);
        assert_eq!(
            emoji("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}"),
            [(true, 7)],
            "a family is one picture"
        );
        assert_eq!(
            emoji("\u{1F937}\u{1F3FD}\u{200D}\u{2642}\u{FE0F}"),
            [(true, 5)],
            "a man shrugging, his tone and the selector with him"
        );
        assert_eq!(
            emoji("\u{1F600}\u{200D}\u{1F600}"),
            [(true, 1), (false, 1), (true, 1)],
            "what is joined to no emoji stands as its parts, the joiner as nothing"
        );
        assert_eq!(
            emoji("\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}"),
            [(true, 7)],
            "a flag named by tags"
        );
        assert_eq!(
            emoji("\u{1F3F4}\u{E0078}\u{E0078}\u{E007F}"),
            [(true, 1), (false, 1), (false, 1), (false, 1)],
            "tags that name no flag leave the flag they follow, and are drawn as nothing"
        );
    }

    #[test]
    fn cells_and_room() {
        assert_eq!(cells_of("hi \u{1F44D}\u{1F3FD} \u{2764}\u{FE0F} \u{4E2D}"), 3 + 2 + 1 + 2 + 1 + 2);
        let thumbs = "a\u{1F44D}\u{1F3FD}b";
        let places = [0, 1, 2, 3, 9].map(|room| overflow_at(thumbs, room));
        assert_eq!(
            places,
            [Some(0), Some(1), Some(1), Some(thumbs.len() - 1), None],
            "the emoji outgrows two cells with its tone, the letter after them three"
        );
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}x";
        assert_eq!(overflow_at(family, 2), Some(family.len() - 1), "a joined emoji is never cut");
        let fitting = |text: &str, room: usize| fit(text.chars(), room);
        assert_eq!((fitting(thumbs, 0), fitting(thumbs, 1), fitting(thumbs, 3), fitting(thumbs, 4)), (1, 1, 3, 4));
        assert_eq!(fitting("\u{1F1E9}\u{1F1EA}\u{1F1EB}\u{1F1F7}", 3), 2, "a flag whole, or not at all");
        assert_eq!(fitting("\u{1F1E9}\u{1F1EA}", 0), 2, "the first glyph whatever it takes");
        assert_eq!(
            letters("a\u{4E2D}\u{200B}\u{1F44D}\u{1F3FD}").collect::<Vec<_>>(),
            [(Some('a'), 1), (None, 2), (None, 0), (None, 2)],
            "a letter by itself; a drawing, what is unseen and an emoji by their cells alone"
        );
        assert_eq!(
            ["\u{2764}\u{FE0F}", "\u{1F44D}\u{1F3FD}", "\u{1F44D}\u{1F44D}", "\u{2764}", "ok", ""].map(is_emoji),
            [true, true, false, false, false, false],
            "one emoji, with its selector or its tone, and nothing else"
        );
    }
}

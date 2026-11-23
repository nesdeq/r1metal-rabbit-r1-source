use alloc::borrow::Cow;
use alloc::string::String;

const CDATA: (&str, &str) = ("<![CDATA[", "]]>");
const COMMENT: (&str, &str) = ("<!--", "-->");
const INSTRUCTION: (&str, &str) = ("<?", "?>");
const NAME_MAX: usize = 10;
const LATIN: [&str; 96] = [
    "nbsp", "iexcl", "cent", "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf", "laquo", "not", "shy",
    "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro", "para", "middot", "cedil", "sup1", "ordm",
    "raquo", "frac14", "frac12", "frac34", "iquest", "Agrave", "Aacute", "Acirc", "Atilde", "Auml", "Aring", "AElig",
    "Ccedil", "Egrave", "Eacute", "Ecirc", "Euml", "Igrave", "Iacute", "Icirc", "Iuml", "ETH", "Ntilde", "Ograve",
    "Oacute", "Ocirc", "Otilde", "Ouml", "times", "Oslash", "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute", "THORN",
    "szlig", "agrave", "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil", "egrave", "eacute", "ecirc",
    "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde", "ograve", "oacute", "ocirc", "otilde", "ouml",
    "divide", "oslash", "ugrave", "uacute", "ucirc", "uuml", "yacute", "thorn", "yuml",
];
const FIRST_LATIN: u32 = 160;
const NAMED: [(&str, char); 31] = [
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("ndash", '\u{2013}'),
    ("mdash", '\u{2014}'),
    ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'),
    ("sbquo", '\u{201A}'),
    ("ldquo", '\u{201C}'),
    ("rdquo", '\u{201D}'),
    ("bdquo", '\u{201E}'),
    ("dagger", '\u{2020}'),
    ("bull", '\u{2022}'),
    ("hellip", '\u{2026}'),
    ("permil", '\u{2030}'),
    ("prime", '\u{2032}'),
    ("lsaquo", '\u{2039}'),
    ("rsaquo", '\u{203A}'),
    ("euro", '\u{20AC}'),
    ("trade", '\u{2122}'),
    ("larr", '\u{2190}'),
    ("rarr", '\u{2192}'),
    ("minus", '\u{2212}'),
    ("ensp", '\u{2002}'),
    ("emsp", '\u{2003}'),
    ("thinsp", '\u{2009}'),
    ("OElig", '\u{152}'),
    ("oelig", '\u{153}'),
    ("zwnj", '\u{200C}'),
];

fn character_by_number(digits: &str) -> Option<char> {
    let number = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    Some(char::from_u32(number).unwrap_or(char::REPLACEMENT_CHARACTER))
}

fn character_by_name(name: &str) -> Option<char> {
    if let Some(index) = LATIN.iter().position(|known| *known == name) {
        return char::from_u32(FIRST_LATIN + index as u32);
    }
    NAMED.iter().find(|entry| entry.0 == name).map(|entry| entry.1)
}

fn character(reference: &str) -> Option<char> {
    match reference.strip_prefix('#') {
        Some(digits) => character_by_number(digits),
        None => character_by_name(reference),
    }
}

fn split_reference(text: &str) -> Option<(char, &str)> {
    let (end, _) = text.char_indices().take(NAME_MAX + 1).find(|entry| entry.1 == ';')?;
    Some((character(&text[..end])?, &text[end + 1..]))
}

pub(crate) fn unescape(text: &str) -> Cow<'_, str> {
    let Some(first) = text.find('&') else {
        return Cow::Borrowed(text);
    };
    let mut plain = String::with_capacity(text.len());
    plain.push_str(&text[..first]);
    let mut rest = &text[first..];
    while let Some(body) = rest.strip_prefix('&') {
        let (first, tail) = match split_reference(body) {
            Some((character, tail)) => (character, tail),
            None => ('&', body),
        };
        plain.push(first);
        let next = tail.find('&').unwrap_or(tail.len());
        plain.push_str(&tail[..next]);
        rest = &tail[next..];
    }
    Cow::Owned(plain)
}

// Whether a `<` with that behind it begins a tag: before a letter, `/`, `!` or `?`, as HTML's tokenizer has it; any other `<` is text.
pub(crate) fn begins_tag(behind: &str) -> bool {
    behind.starts_with(|next: char| next.is_ascii_alphabetic() || matches!(next, '/' | '!' | '?'))
}

// The length of a tag's inside, from behind its `<` to its `>`, which a quoted value may hold; none for one nothing closes.
fn tag_length(tag: &str) -> Option<usize> {
    let mut open_quote = None;
    for (at, character) in tag.char_indices() {
        match (open_quote, character) {
            (None, '>') => return Some(at),
            (None, '"' | '\'') => open_quote = Some(character),
            (Some(quote), _) if quote == character => open_quote = None,
            _ => {}
        }
    }
    None
}

// The bytes of the comment or tag a text begins with, its `<` and its end included: a comment ends at its own close, whatever it holds; none for one nothing closes.
pub(crate) fn markup_length(text: &str) -> Option<usize> {
    if let Some(body) = text.strip_prefix(COMMENT.0) {
        return body.find(COMMENT.1).map(|at| COMMENT.0.len() + at + COMMENT.1.len());
    }
    tag_length(text.strip_prefix('<')?).map(|length| length + 2)
}

pub(crate) enum Piece<'a> {
    Open { name: &'a str, attributes: &'a str, is_empty: bool },
    Close(&'a str),
    Text(Cow<'a, str>),
}

pub(crate) struct Pieces<'a> {
    rest: &'a str,
}

impl<'a> Pieces<'a> {
    pub(crate) const fn new(document: &'a str) -> Pieces<'a> {
        Pieces { rest: document }
    }

    fn take_between(&mut self, (open, close): (&str, &str)) -> Option<&'a str> {
        let body = self.rest.strip_prefix(open)?;
        let (inside, tail) = body.split_once(close).unwrap_or((body, ""));
        self.rest = tail;
        Some(inside)
    }

    fn take_text(&mut self) -> Piece<'a> {
        let begins = |&(at, _): &(usize, &str)| begins_tag(&self.rest[at + 1..]);
        let end = self.rest.match_indices('<').find(begins).map_or(self.rest.len(), |(at, _)| at);
        let (text, tail) = self.rest.split_at(end);
        self.rest = tail;
        Piece::Text(unescape(text))
    }

    // Leaves out what stands before the closing tag of that name, and the tag, or all the rest where none closes: what HTML's script or style holds is no markup.
    pub(crate) fn skip_element(&mut self, name: &str) {
        let closes = |&(at, _): &(usize, &str)| {
            let tail = self.rest[at + 1..].strip_prefix('/');
            tail.and_then(|tail| tail.get(..name.len())).is_some_and(|start| start.eq_ignore_ascii_case(name))
        };
        let closing = self.rest.match_indices('<').find(closes).map(|(at, _)| &self.rest[at..]);
        self.rest = closing.and_then(|closing| closing.split_once('>')).map_or("", |(_, tail)| tail);
    }

    fn take_tag(&mut self, tag: &'a str) -> Option<&'a str> {
        let Some(length) = tag_length(tag) else {
            self.rest = "";
            return None;
        };
        self.rest = &tag[length + 1..];
        Some(&tag[..length])
    }
}

fn piece_of_tag(tag: &str) -> Option<Piece<'_>> {
    if tag.starts_with('!') {
        return None;
    }
    if let Some(name) = tag.strip_prefix('/') {
        return Some(Piece::Close(name.trim()));
    }
    let body = tag.strip_suffix('/');
    let (name, attributes) = body.unwrap_or(tag).split_once(char::is_whitespace).unwrap_or((body.unwrap_or(tag), ""));
    Some(Piece::Open { name, attributes, is_empty: body.is_some() })
}

impl<'a> Iterator for Pieces<'a> {
    type Item = Piece<'a>;

    fn next(&mut self) -> Option<Piece<'a>> {
        while !self.rest.is_empty() {
            if let Some(text) = self.take_between(CDATA) {
                return Some(Piece::Text(Cow::Borrowed(text)));
            }
            if self.take_between(COMMENT).is_some() || self.take_between(INSTRUCTION).is_some() {
                continue;
            }
            let Some(tag) = self.rest.strip_prefix('<').filter(|tag| begins_tag(tag)) else {
                return Some(self.take_text());
            };
            if let Some(piece) = self.take_tag(tag).and_then(piece_of_tag) {
                return Some(piece);
            }
        }
        None
    }
}

// A tag's attributes one by one: each name, and its value as written where it has one; a value no quote ends is the last.
fn pairs(attributes: &str) -> impl Iterator<Item = (&str, Option<&str>)> {
    let mut rest = attributes.trim_start();
    core::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let key_end = rest.find(|at: char| at == '=' || at.is_whitespace()).unwrap_or(rest.len());
        let (key, tail) = rest.split_at(key_end);
        let tail = tail.trim_start();
        let Some(body) = tail.strip_prefix('=').map(str::trim_start) else {
            rest = tail;
            return Some((key, None));
        };
        let quote = body.chars().next().filter(|first| matches!(first, '"' | '\''));
        let (value, after) = match quote {
            Some(quote) => body[1..].split_once(quote).or_else(|| {
                rest = "";
                None
            })?,
            None => body.split_at(body.find(char::is_whitespace).unwrap_or(body.len())),
        };
        rest = after.trim_start();
        Some((key, Some(value)))
    })
}

pub(crate) fn attribute<'a>(attributes: &'a str, name: &str) -> Option<Cow<'a, str>> {
    pairs(attributes).find_map(|(key, value)| value.filter(|_| key == name)).map(unescape)
}

// True where a tag has an attribute of that name, with a value or without one.
pub(crate) fn has_attribute(attributes: &str, name: &str) -> bool {
    pairs(attributes).any(|(key, _)| key == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_after_valueless_one() {
        let tag = r#"ssr await-children class="evolution-card x" width=420 alt='a &amp; b' hidden"#;
        assert_eq!(
            ["class", "width", "alt", "hidden", "ssr", "src"].map(|name| attribute(tag, name)),
            [Some("evolution-card x".into()), Some("420".into()), Some("a & b".into()), None, None, None]
        );
        assert_eq!(attribute(r#"href = "/x/" "#, "href"), Some("/x/".into()));
        assert_eq!(attribute(r#"src="never ended"#, "src"), None);
        assert_eq!(
            ["hidden", "ssr", "width", "evolution-card", "src"].map(|name| has_attribute(tag, name)),
            [true, true, true, false, false],
            "an attribute is there with a value or without one, and a word of a value is none"
        );
        assert!(has_attribute(r#"hidden src="never ended"#, "hidden") && !has_attribute(r#"src="never ended"#, "src"));
    }

    #[test]
    fn element_that_holds_no_markup() {
        let mut pieces = Pieces::new("<script>if (a<b && c>\"</p>\") x='</scr';</SCRIPT >after<style>p > a {}");
        assert!(matches!(pieces.next(), Some(Piece::Open { name: "script", .. })));
        pieces.skip_element("script");
        assert!(matches!(pieces.next(), Some(Piece::Text(text)) if text == "after"), "up to its own closing tag");
        assert!(matches!(pieces.next(), Some(Piece::Open { name: "style", .. })));
        pieces.skip_element("style");
        assert!(pieces.next().is_none(), "one nothing closes holds all the rest");
    }

    #[test]
    fn unknown_entity_stays() {
        assert_eq!(unescape("a &amp; b &nope; c &#65; &#x42; &"), "a & b &nope; c A B &");
        assert_eq!(unescape("x&eacute;&"), "x\u{E9}&");
        assert_eq!(unescape("&"), "&");
    }
}

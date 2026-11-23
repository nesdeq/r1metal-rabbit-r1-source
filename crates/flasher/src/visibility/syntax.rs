//! Rust source as the workspace's own checks read it, `r1 visibility` and the architecture test, which takes this file in by its path: a source's pieces, what the paths of a `use` tree end in, and the module a file makes.

use std::ops::Range;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    Comment,
    Word,
    // A string of any prefix.
    Text,
    // A character or a byte between quotes.
    Character,
    // A lifetime or a number.
    Other,
    Mark,
}

// A piece of a source: what it is, where it lies, and for a string or a character where its content lies between its quotes.
type Piece = (Kind, Range<usize>, Range<usize>);

fn name_end(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_') {
        at += 1;
    }
    at
}

// Where what is quoted from `at` on ends, and where its closing quote does; a backslash takes the byte behind it.
fn quoted_end(bytes: &[u8], mut at: usize, quote: u8) -> (usize, usize) {
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'\\' => at += 2,
            _ if byte == quote => return (at, at + 1),
            _ => at += 1,
        }
    }
    (bytes.len(), bytes.len())
}

fn hash_count(bytes: &[u8], at: usize) -> usize {
    bytes[at..].iter().take_while(|byte| **byte == b'#').count()
}

fn is_raw(bytes: &[u8], at: usize) -> bool {
    bytes.get(at + hash_count(bytes, at)) == Some(&b'"')
}

// The content of the raw string whose hashes begin at `at`, and where the string ends.
fn raw_end(source: &str, at: usize) -> (Range<usize>, usize) {
    let hashes = hash_count(source.as_bytes(), at);
    let closing = format!("\"{}", "#".repeat(hashes));
    let text = at + hashes + 1;
    match source[text..].find(&closing) {
        Some(found) => (text..text + found, text + found + closing.len()),
        None => (text..source.len(), source.len()),
    }
}

// What the quote at `at` opens: a character with its content, or a lifetime, which has none; and where it ends.
fn quote_end(source: &str, at: usize) -> (Kind, Range<usize>, usize) {
    let rest = &source[at + 1..];
    if rest.starts_with('\\') {
        let (content_end, end) = quoted_end(source.as_bytes(), at + 1, b'\'');
        return (Kind::Character, at + 1..content_end, end);
    }
    match rest.chars().next() {
        Some(first) if rest[first.len_utf8()..].starts_with('\'') => {
            let content_end = at + 1 + first.len_utf8();
            (Kind::Character, at + 1..content_end, content_end + 1)
        }
        _ => {
            let end = name_end(source.as_bytes(), at + 1);
            (Kind::Other, end..end, end)
        }
    }
}

fn number_end(bytes: &[u8], at: usize) -> usize {
    let mut end = name_end(bytes, at);
    while bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end = name_end(bytes, end + 1);
    }
    end
}

fn block_end(bytes: &[u8], mut at: usize) -> usize {
    let mut depth = 0;
    while at + 1 < bytes.len() {
        match &bytes[at..at + 2] {
            b"/*" => (depth, at) = (depth + 1, at + 2),
            b"*/" if depth == 1 => return at + 2,
            b"*/" => (depth, at) = (depth - 1, at + 2),
            _ => at += 1,
        }
    }
    bytes.len()
}

// A source's pieces in their order, its blanks left out.
pub(super) fn pieces(source: &str) -> Vec<Piece> {
    let bytes = source.as_bytes();
    let (mut at, mut pieces) = (0, Vec::new());
    while at < bytes.len() {
        let (byte, next_byte) = (bytes[at], bytes.get(at + 1).copied());
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let quoted = |from: usize| {
            let (content_end, end) = quoted_end(bytes, from, b'"');
            (Kind::Text, from..content_end, end)
        };
        let raw = |hashes_at: usize| {
            let (content, end) = raw_end(source, hashes_at);
            (Kind::Text, content, end)
        };
        let plain = |kind: Kind, end: usize| (kind, end..end, end);
        let (kind, content, end) = match byte {
            b'/' if next_byte == Some(b'/') => {
                plain(Kind::Comment, source[at..].find('\n').map_or(bytes.len(), |found| at + found))
            }
            b'/' if next_byte == Some(b'*') => plain(Kind::Comment, block_end(bytes, at)),
            b'"' => quoted(at + 1),
            b'r' if is_raw(bytes, at + 1) => raw(at + 1),
            b'b' | b'c' if next_byte == Some(b'"') => quoted(at + 2),
            b'b' | b'c' if next_byte == Some(b'r') && is_raw(bytes, at + 2) => raw(at + 2),
            b'b' if next_byte == Some(b'\'') => quote_end(source, at + 1),
            b'\'' => quote_end(source, at),
            _ if byte.is_ascii_alphabetic() || byte == b'_' => plain(Kind::Word, name_end(bytes, at)),
            _ if byte.is_ascii_digit() => plain(Kind::Other, number_end(bytes, at)),
            b':' if next_byte == Some(b':') => plain(Kind::Mark, at + 2),
            _ => plain(Kind::Mark, at + source[at..].chars().next().map_or(1, char::len_utf8)),
        };
        pieces.push((kind, at..end, content));
        at = end;
    }
    pieces
}

// What a path of a `use` tree ends in: a name, the module before it (`self`), or a glob.
pub(super) enum Leaf {
    Name(String),
    Module,
    Glob,
}

// Every path a `use` tree takes: the parts before its end, what it ends in, and the name the `use` gives that where it gives one; `tokens` are the tree's own, from behind `use` to before its semicolon.
pub(super) fn use_leaves(tokens: &[&str]) -> Vec<(Vec<String>, Leaf, Option<String>)> {
    let text = |at: usize| tokens.get(at).copied().unwrap_or("");
    let (mut path, mut groups, mut leaves) = (Vec::<String>::new(), Vec::new(), Vec::new());
    for (at, token) in tokens.iter().enumerate() {
        let given = (text(at + 1) == "as").then(|| text(at + 2).to_string());
        let is_name = token.starts_with(|first: char| first.is_ascii_alphabetic() || first == '_');
        match *token {
            "{" => groups.push(path.len()),
            "}" => path.truncate(groups.pop().unwrap_or(0)),
            "," => path.truncate(groups.last().copied().unwrap_or(0)),
            "*" => leaves.push((path.clone(), Leaf::Glob, None)),
            "self" if text(at + 1) != "::" => leaves.push((path.clone(), Leaf::Module, given)),
            _ if !is_name || *token == "as" || at > 0 && text(at - 1) == "as" => {}
            _ if text(at + 1) == "::" => path.push((*token).to_string()),
            _ => leaves.push((path.clone(), Leaf::Name((*token).to_string()), given)),
        }
    }
    leaves
}

// The module a file of a crate's sources makes, by its path from their root: `lib.rs` and `main.rs` the crate's root, a `mod.rs` its directory's module.
pub(super) fn module_path(from_src: &str) -> Vec<String> {
    let mut parts: Vec<String> = from_src.trim_end_matches(".rs").split('/').map(String::from).collect();
    if matches!(parts.as_slice(), [only] if only == "lib" || only == "main") {
        return Vec::new();
    }
    if parts.last().is_some_and(|last| last == "mod") {
        parts.pop();
    }
    parts
}

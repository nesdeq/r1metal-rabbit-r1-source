//! What the sources of a crate say of visibility: every visibility token with the item it opens and the module it lies in, what is compiled on some platforms alone, the names the traits' methods take, and the paths a `use` takes.

use super::syntax::{self, Leaf, pieces, use_leaves};
use std::ops::Range;

// The words of a `cfg` that name the platform a build is for: what such a `cfg` holds is compiled on some platforms alone.
const PLATFORM: [&str; 8] = [
    "target_os",
    "target_arch",
    "target_env",
    "target_family",
    "target_vendor",
    "target_pointer_width",
    "unix",
    "windows",
];

// How far an item reaches as its token says it; `pub(in …)` and `pub(self)` are left as they are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Vis {
    Private,
    Super,
    Crate,
    Pub,
}

impl Vis {
    // From the narrowest to the widest.
    pub(super) const LADDER: [Vis; 4] = [Vis::Private, Vis::Super, Vis::Crate, Vis::Pub];

    // The token that says it, none for private.
    pub(super) const fn token(self) -> &'static str {
        match self {
            Vis::Private => "",
            Vis::Super => "pub(super)",
            Vis::Crate => "pub(crate)",
            Vis::Pub => "pub",
        }
    }

    // As a report names it.
    pub(super) const fn named(self) -> &'static str {
        match self {
            Vis::Private => "private",
            other => other.token(),
        }
    }
}

// What a visibility token opens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    Fn,
    Struct,
    Enum,
    Union,
    Trait,
    Type,
    Const,
    Static,
    Mod,
    Use,
    Field,
    TupleField,
    Method,
    AssocConst,
    AssocType,
}

impl Kind {
    // True for what a module holds by its name, as a path names it.
    pub(super) const fn in_module(self) -> bool {
        matches!(
            self,
            Kind::Fn
                | Kind::Struct
                | Kind::Enum
                | Kind::Union
                | Kind::Trait
                | Kind::Type
                | Kind::Const
                | Kind::Static
                | Kind::Mod
                | Kind::Use
        )
    }

    // True for what an `impl` holds, which a trait's method of the same name could stand in for where it reached less.
    pub(super) const fn associated(self) -> bool {
        matches!(self, Kind::Method | Kind::AssocConst | Kind::AssocType)
    }
}

// An item with a visibility token: where the token lies in its file and what it says, what it opens, its name, the type it belongs to, and the module it lies in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Item {
    pub(super) offset: usize,
    pub(super) end: usize,
    pub(super) vis: Vis,
    pub(super) kind: Kind,
    // For a tuple's field its place, for a `use` the tree it takes as written without blanks.
    pub(super) name: String,
    pub(super) owner: Option<String>,
    pub(super) module: Vec<String>,
}

// What one file says.
#[derive(Default, Debug)]
pub(super) struct Scanned {
    pub(super) items: Vec<Item>,
    // The bytes compiled on some platforms alone.
    pub(super) gated: Vec<Range<usize>>,
    // The modules declared from here on some platforms alone, by their paths.
    pub(super) gated_modules: Vec<Vec<String>>,
    // The methods the file's traits declare.
    pub(super) trait_methods: Vec<String>,
}

// A source as its code alone: every comment and the content of every literal blanked, each byte where it was and every line end kept.
pub(super) fn code_of(source: &str) -> Vec<u8> {
    let mut code = source.as_bytes().to_vec();
    for (kind, range, content) in pieces(source) {
        let blanked = match kind {
            syntax::Kind::Comment => range,
            syntax::Kind::Text | syntax::Kind::Character => content,
            _ => continue,
        };
        code[blanked].iter_mut().filter(|byte| **byte != b'\n').for_each(|byte| *byte = b' ');
    }
    code
}

// True for a byte a name holds.
const fn in_name(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

// The name at `at` after the blanks before it, and the place behind it; none where no name stands there.
fn name_at(code: &[u8], at: usize) -> Option<(String, usize)> {
    let start = at + code[at.min(code.len())..].iter().take_while(|byte| byte.is_ascii_whitespace()).count();
    let wide = code[start.min(code.len())..].iter().take_while(|byte| in_name(**byte)).count();
    (wide > 0).then(|| (String::from_utf8_lossy(&code[start..start + wide]).into_owned(), start + wide))
}

// The first byte after the blanks from `at` on.
fn next_byte(code: &[u8], at: usize) -> Option<(u8, usize)> {
    code[at.min(code.len())..]
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .map(|skip| (code[at + skip], at + skip))
}

// True where the word `word` stands at `at`, a name of its own.
fn word_at(code: &[u8], at: usize, word: &[u8]) -> bool {
    code[at..].starts_with(word)
        && (at == 0 || !(in_name(code[at - 1]) || code[at - 1] == b'$' || code[at - 1] == b'#'))
        && code.get(at + word.len()).is_none_or(|byte| !in_name(*byte))
}

// Where the delimiter that closes the one at `open` stands; the end of the code where none does.
fn closing(code: &[u8], open: usize) -> usize {
    let (opens, closes) = (
        code[open],
        match code[open] {
            b'(' => b')',
            b'[' => b']',
            _ => b'}',
        },
    );
    let mut depth = 0usize;
    for (at, byte) in code.iter().enumerate().skip(open) {
        if *byte == opens {
            depth += 1;
        } else if *byte == closes {
            depth -= 1;
            if depth == 0 {
                return at;
            }
        }
    }
    code.len()
}

// The text without its attributes, `#[…]` and `#![…]`.
fn without_attributes(text: &[u8]) -> String {
    let mut kept = Vec::new();
    let mut at = 0;
    while at < text.len() {
        if text[at] == b'#' && (text.get(at + 1) == Some(&b'[') || text[at + 1..].starts_with(b"![")) {
            let open = at + text[at..].iter().position(|byte| *byte == b'[').unwrap_or(0);
            at = closing(text, open) + 1;
        } else {
            kept.push(text[at]);
            at += 1;
        }
    }
    String::from_utf8_lossy(&kept).trim().to_string()
}

// The text without the visibility it begins with.
fn without_visibility(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("pub") else {
        return text;
    };
    let rest = rest.trim_start();
    match rest.strip_prefix('(') {
        Some(inside) => inside.split_once(')').map_or(rest, |(_, after)| after.trim_start()),
        None => rest,
    }
}

// The name of a type as a path writes it: its last part, without its parameters.
pub(super) fn type_name(path: &str) -> String {
    let bare = path.split('<').next().unwrap_or_default().trim().trim_start_matches('&').trim();
    bare.rsplit("::").next().unwrap_or_default().trim().to_string()
}

// What a block opened by a brace or a parenthesis is, by the text before it.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Block {
    Impl { owner: String, of_trait: bool },
    Trait,
    Mod(String),
    Struct(String),
    Tuple(String),
    Other,
}

// What a brace opens, by the text since the statement before it.
fn braced(header: &[u8]) -> Block {
    let text = without_attributes(header).split_whitespace().collect::<Vec<&str>>().join(" ");
    let text = without_visibility(&text);
    if let Some(implemented) = text.strip_prefix("unsafe ").unwrap_or(text).strip_prefix("impl") {
        let mut rest = implemented.trim_start();
        if rest.starts_with('<') {
            let mut depth = 0usize;
            let end = rest.char_indices().find_map(|(at, character)| {
                depth = match character {
                    '<' => depth + 1,
                    '>' => depth - 1,
                    _ => depth,
                };
                (depth == 0).then_some(at + 1)
            });
            rest = &rest[end.unwrap_or(rest.len())..];
        }
        let rest = rest.split(" where").next().unwrap_or_default();
        let parted = rest.match_indices(" for ").find(|(at, _)| !rest[at + 5..].trim_start().starts_with('<'));
        return match parted {
            Some((at, _)) => Block::Impl { owner: type_name(&rest[at + 5..]), of_trait: true },
            None => Block::Impl { owner: type_name(rest), of_trait: false },
        };
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        ["trait" | "unsafe", ..] if words.contains(&"trait") => Block::Trait,
        ["mod", name] => Block::Mod((*name).to_string()),
        ["struct" | "union", name, ..] => Block::Struct(name.split('<').next().unwrap_or_default().to_string()),
        _ => Block::Other,
    }
}

// What a parenthesis opens: the fields of a tuple struct, by the text since the statement before it.
fn parenthesised(header: &[u8]) -> Block {
    let text = without_attributes(header);
    let text = without_visibility(&text);
    match text.split_whitespace().collect::<Vec<&str>>().as_slice() {
        ["struct", name, ..] => Block::Tuple(name.split('<').next().unwrap_or_default().to_string()),
        _ => Block::Other,
    }
}

// What a module's or an `impl`'s item behind a visibility token is: its kind and its name, a `use` its tree without blanks.
fn item_at(code: &[u8], from: usize) -> Option<(Kind, String)> {
    let mut at = from;
    loop {
        let (word, behind) = name_at(code, at)?;
        at = behind;
        let kind = match word.as_str() {
            "async" | "unsafe" | "default" => continue,
            "extern" => {
                if let Some((b'"', quote)) = next_byte(code, at) {
                    at = quote + 1 + code[quote + 1..].iter().position(|byte| *byte == b'"').unwrap_or(0) + 1;
                }
                continue;
            }
            "const"
                if name_at(code, at)
                    .is_some_and(|(next, _)| matches!(next.as_str(), "fn" | "unsafe" | "async" | "extern")) =>
            {
                continue;
            }
            "fn" => Kind::Fn,
            "struct" => Kind::Struct,
            "enum" => Kind::Enum,
            "union" => Kind::Union,
            "trait" => Kind::Trait,
            "type" => Kind::Type,
            "const" => Kind::Const,
            "static" => Kind::Static,
            "mod" => Kind::Mod,
            "use" => {
                let end = code[at..].iter().position(|byte| *byte == b';').map_or(code.len(), |found| at + found);
                let tree = String::from_utf8_lossy(&code[at..end]).split_whitespace().collect::<Vec<&str>>().join(" ");
                return Some((Kind::Use, tree));
            }
            _ => return None,
        };
        let (mut name, behind) = name_at(code, at)?;
        if name == "mut" {
            name = name_at(code, behind)?.0;
        }
        return Some((kind, name));
    }
}

// Where the item that begins at or after `from` ends: past its closing brace, or its semicolon.
fn item_end(code: &[u8], from: usize) -> usize {
    let mut at = from;
    while let Some((byte, place)) = next_byte(code, at) {
        if byte == b'#' && code.get(place + 1) == Some(&b'[') {
            at = closing(code, place + 1) + 1;
        } else {
            break;
        }
    }
    let mut depth = 0usize;
    while at < code.len() {
        match code[at] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'{' if depth == 0 => return closing(code, at) + 1,
            b';' if depth == 0 => return at + 1,
            _ => {}
        }
        at += 1;
    }
    code.len()
}

// The module a place lies in: the file's, and the modules written inline around the place.
fn module_here(module: &[String], open: &[(u8, Block, usize)]) -> Vec<String> {
    let inline = open.iter().filter_map(|(_, block, _)| match block {
        Block::Mod(name) => Some(name.clone()),
        _ => None,
    });
    module.iter().cloned().chain(inline).collect()
}

// True for an attribute that compiles what it stands on for some platforms alone.
fn for_platforms(attribute: &str) -> bool {
    attribute.starts_with("#[cfg(")
        && attribute.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|part| PLATFORM.contains(&part))
}

// Everything a file says, its module being `module`.
pub(super) fn scan(source: &str, module: &[String]) -> Scanned {
    let code = code_of(source);
    let mut scanned = Scanned::default();
    // Each delimiter that is open: which it is, what it opens, and for a tuple's fields how many commas came.
    let mut open: Vec<(u8, Block, usize)> = Vec::new();
    // Where the statement or the part being read began, at every depth.
    let mut began = vec![0usize];
    let mut at = 0;
    while at < code.len() {
        let byte = code[at];
        match byte {
            b'#' if code.get(at + 1) == Some(&b'[') => {
                let close = closing(&code, at + 1).min(code.len() - 1);
                let attribute = String::from_utf8_lossy(&code[at..=close]).into_owned();
                if for_platforms(&attribute) {
                    let end = item_end(&code, close + 1);
                    scanned.gated.push(at..end);
                    let item =
                        without_attributes(&code[close + 1..end]).split_whitespace().collect::<Vec<&str>>().join(" ");
                    if let Some(name) =
                        without_visibility(&item).strip_prefix("mod ").and_then(|rest| rest.strip_suffix(';'))
                    {
                        let mut path = module_here(module, &open);
                        path.push(name.trim().to_string());
                        scanned.gated_modules.push(path);
                    }
                }
            }
            b'(' | b'[' | b'{' => {
                let header = &code[*began.last().unwrap_or(&0)..at];
                let block = match byte {
                    b'{' => braced(header),
                    b'(' => parenthesised(header),
                    _ => Block::Other,
                };
                open.push((byte, block, 0));
                began.push(at + 1);
            }
            b')' | b']' | b'}' => {
                if let Some((opened, _, _)) = open.pop() {
                    began.pop();
                    if let (b'{', Some(last)) = (opened, began.last_mut()) {
                        *last = at + 1;
                    }
                }
            }
            b';' => {
                if let Some(last) = began.last_mut() {
                    *last = at + 1;
                }
            }
            b',' if !open.is_empty() => {
                if let Some(last) = began.last_mut() {
                    *last = at + 1;
                }
                if let Some((_, _, commas)) = open.last_mut() {
                    *commas += 1;
                }
            }
            _ if word_at(&code, at, b"fn") && matches!(open.last(), Some((b'{', Block::Trait, _))) => {
                if let Some((name, _)) = name_at(&code, at + 2) {
                    scanned.trait_methods.push(name);
                }
            }
            _ if word_at(&code, at, b"pub") => {
                let (vis, end) = match next_byte(&code, at + 3) {
                    Some((b'(', paren)) => {
                        let close = paren + code[paren..].iter().position(|byte| *byte == b')').unwrap_or(0);
                        let inside: String =
                            String::from_utf8_lossy(&code[paren + 1..close]).split_whitespace().collect();
                        let vis = match inside.as_str() {
                            "crate" => Some(Vis::Crate),
                            "super" => Some(Vis::Super),
                            _ => None,
                        };
                        (vis, close + 1)
                    }
                    _ => (Some(Vis::Pub), at + 3),
                };
                let item = match (vis, open.last()) {
                    (None, _) => None,
                    (_, None | Some((b'{', Block::Mod(_), _))) => item_at(&code, end),
                    (_, Some((b'{', Block::Impl { of_trait: false, .. }, _))) => {
                        item_at(&code, end).and_then(|(kind, name)| {
                            let kind = match kind {
                                Kind::Fn => Kind::Method,
                                Kind::Const => Kind::AssocConst,
                                Kind::Type => Kind::AssocType,
                                _ => return None,
                            };
                            Some((kind, name))
                        })
                    }
                    (_, Some((b'{', Block::Struct(_), _))) => name_at(&code, end)
                        .filter(|(_, behind)| next_byte(&code, *behind).is_some_and(|(byte, _)| byte == b':'))
                        .map(|(name, _)| (Kind::Field, name)),
                    (_, Some((b'(', Block::Tuple(_), commas))) => Some((Kind::TupleField, commas.to_string())),
                    _ => None,
                };
                if let (Some(vis), Some((kind, name))) = (vis, item) {
                    let owner = match open.last() {
                        Some((_, Block::Impl { owner, .. } | Block::Struct(owner) | Block::Tuple(owner), _)) => {
                            Some(owner.clone())
                        }
                        _ => None,
                    };
                    scanned.items.push(Item {
                        offset: at,
                        end,
                        vis,
                        kind,
                        name,
                        owner,
                        module: module_here(module, &open),
                    });
                }
                at = end;
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    scanned
}

// Every path a `use` tree takes, as its parts, the name it takes last, `*` for a glob and `self` for the module before it.
pub(super) fn use_paths(tree: &str) -> Vec<Vec<String>> {
    let tokens: Vec<&str> = pieces(tree).into_iter().map(|(_, range, _)| &tree[range]).collect();
    let ended = |(mut path, leaf, _): (Vec<String>, Leaf, Option<String>)| {
        path.push(match leaf {
            Leaf::Name(name) => name,
            Leaf::Module => "self".to_string(),
            Leaf::Glob => "*".to_string(),
        });
        path
    };
    use_leaves(&tokens).into_iter().map(ended).collect()
}

#[cfg(test)]
mod tests {
    use super::super::syntax::module_path;
    use super::*;

    const SOURCE: &str = r##"//! pub fn in_a_comment() {}
use std::io;
pub(crate) const LIMIT: &str = "pub fn in_a_string() { ";
pub(super) static mut COUNT: u32 = 0;
pub const fn made() -> char { '{' }
pub(crate) struct Pair<'a> {
    pub(super) left: &'a str,
    right: u32,
}
pub struct Word(pub(crate) u32, pub u8);
impl<'a> Pair<'a> {
    pub(crate) fn left(&self) -> &str { self.left }
    pub(super) const MOST: u32 = 7;
}
impl core::fmt::Debug for Pair<'_> {
    fn fmt(&self, told: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { let raw = r#"pub fn no() {"#; Ok(()) }
}
pub(crate) trait Told {
    fn told(&self) -> u32;
}
pub(crate) mod inner {
    pub(in crate::inner) fn hidden() {}
    pub(super) use super::{Pair, inner as nested, io::*};
    fn body() { pub struct Local; }
}
#[cfg(target_os = "macos")]
pub(crate) fn mac_only() {}
#[cfg(not(target_os = "macos"))]
mod serial;
"##;

    #[test]
    fn visibility_tokens() {
        let scanned = scan(SOURCE, &["link".to_string()]);
        let found: Vec<(Vis, Kind, &str, Option<&str>, String)> = scanned
            .items
            .iter()
            .map(|item| (item.vis, item.kind, item.name.as_str(), item.owner.as_deref(), item.module.join("::")))
            .collect();
        let link = || "link".to_string();
        assert_eq!(
            found,
            [
                (Vis::Crate, Kind::Const, "LIMIT", None, link()),
                (Vis::Super, Kind::Static, "COUNT", None, link()),
                (Vis::Pub, Kind::Fn, "made", None, link()),
                (Vis::Crate, Kind::Struct, "Pair", None, link()),
                (Vis::Super, Kind::Field, "left", Some("Pair"), link()),
                (Vis::Pub, Kind::Struct, "Word", None, link()),
                (Vis::Crate, Kind::TupleField, "0", Some("Word"), link()),
                (Vis::Pub, Kind::TupleField, "1", Some("Word"), link()),
                (Vis::Crate, Kind::Method, "left", Some("Pair"), link()),
                (Vis::Super, Kind::AssocConst, "MOST", Some("Pair"), link()),
                (Vis::Crate, Kind::Trait, "Told", None, link()),
                (Vis::Crate, Kind::Mod, "inner", None, link()),
                (Vis::Super, Kind::Use, "super::{Pair, inner as nested, io::*}", None, "link::inner".to_string()),
                (Vis::Crate, Kind::Fn, "mac_only", None, link()),
            ]
        );
        let token =
            |name: &str| scanned.items.iter().find(|item| item.name == name).map(|item| &SOURCE[item.offset..item.end]);
        assert_eq!((token("LIMIT"), token("made"), token("0")), (Some("pub(crate)"), Some("pub"), Some("pub(crate)")));
        assert_eq!(scanned.trait_methods, ["told"]);
        let mac = scanned.items.iter().find(|item| item.name == "mac_only").unwrap();
        assert!(scanned.gated.iter().any(|gated| gated.contains(&mac.offset)), "compiled on some platforms alone");
        assert_eq!(
            scanned.gated_modules,
            [vec!["link".to_string(), "serial".to_string()]],
            "and so is a module declared so"
        );
    }

    #[test]
    fn code_blanks_comments_and_literals() {
        let source = "a /* b /* c */ d */ \"e\\\"f\" 'g' '\\n' 'é' b'h' x<'a> r#\"i\"j\"# // k\nl";
        let code = code_of(source);
        assert_eq!(code.len(), source.len());
        let kept = String::from_utf8(code).unwrap();
        assert_eq!(
            kept.split_whitespace().collect::<Vec<_>>(),
            ["a", "\"", "\"", "'", "'", "'", "'", "'", "'", "b'", "'", "x<'a>", "r#\"", "\"#", "l"]
        );
    }

    #[test]
    fn use_tree_paths() {
        assert_eq!(
            use_paths("super::{Pair, inner as nested, io::*}"),
            [vec!["super", "Pair"], vec!["super", "inner"], vec!["super", "io", "*"]],
            "a path names what it takes, not the name the use gives it"
        );
        assert_eq!(
            use_paths("crate::a::{self, b::{C, D}}"),
            [vec!["crate", "a", "self"], vec!["crate", "a", "b", "C"], vec!["crate", "a", "b", "D"]]
        );
        assert_eq!(
            (module_path("lib.rs"), module_path("ui/mod.rs"), module_path("ui/keys.rs")),
            (vec![], vec!["ui".to_string()], vec!["ui".to_string(), "keys".to_string()])
        );
        assert_eq!(
            (type_name("Pair<'a>"), type_name("crate::gfx::Rect"), type_name("&Word")),
            ("Pair".into(), "Rect".into(), "Word".into())
        );
    }
}

//! The architecture test: the kernel changes by patches, funcore stays pure, impshell holds no decision, format or screen word.

#[path = "../../flasher/src/visibility/syntax.rs"]
mod syntax;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use syntax::{Kind, Leaf};

// The standard library's names that reach the OS, beside the locks and paths of `INCIDENTAL`; `libc` reaches it by its calls (`calls_libc`), a library by its crate's name (`LIBRARIES`).
const OS: [&str; 16] = [
    "fs",
    "File",
    "OpenOptions",
    "TcpListener",
    "TcpStream",
    "UdpSocket",
    "thread",
    "process",
    "env",
    "Condvar",
    "mpsc",
    "thread_local",
    "OwnedFd",
    "RawFd",
    "AsRawFd",
    "FromRawFd",
];
const LOG_MACROS: [&str; 3] = ["log", "println", "eprintln"];
// Beside one of these, as beside an atomic, a branch decides on a value alone.
const INCIDENTAL: [&str; 6] = ["Mutex", "MutexGuard", "RwLock", "OnceLock", "Path", "PathBuf"];
// Methods that ask the OS whether another thread holds a lock and what the filesystem holds at a path.
const QUERIES: [&str; 11] = [
    "try_lock",
    "exists",
    "try_exists",
    "metadata",
    "symlink_metadata",
    "is_file",
    "is_dir",
    "is_symlink",
    "read_dir",
    "read_link",
    "canonicalize",
];
// A time held or handed on is a value; reading a clock reaches the OS.
const CLOCKS: [&str; 2] = ["Instant", "SystemTime"];
const LAYOUT: [&str; 6] = ["size_of", "offset_of", "align_of", "ioc", "as_ptr", "as_mut_ptr"];
// A path under one of these is a name the kernel lays out.
const KERNEL_FILESYSTEMS: [&str; 4] = ["/dev/", "/sys/", "/proc/", "/config/"];
const LIBRARIES: [&str; 5] = ["aws_lc_rs", "aws_lc_sys", "rustls", "webpki_roots", "flate2"];
const LANGUAGE: [&str; 16] = [
    "Display",
    "Debug",
    "Drop",
    "Error",
    "From",
    "Default",
    "Clone",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Iterator",
    "Deref",
    "Read",
    "Write",
];
const STREAM_METHODS: [&str; 10] = [
    "read",
    "read_vectored",
    "read_to_end",
    "read_to_string",
    "read_exact",
    "write",
    "write_vectored",
    "flush",
    "write_all",
    "write_fmt",
];
const BRANCHES: [&str; 5] = ["if", "match", "for", "while", "loop"];
// A body that makes text and branches nowhere is a log line or a call's error.
const TEXT_MACROS: [&str; 3] = ["format", "write", "writeln"];
const CORE: &str = "r1_funcore";
// The modules of funcore's root by layer, the lowest first: one imports only those of lower layers.
const CORE_LAYERS: [&[&str]; 8] = [
    &["base"],
    &["crypto", "gfx", "codec"],
    &["hw", "input", "fs", "net"],
    &["storage"],
    &["logs", "cell", "openai", "signal"],
    &["wifi"],
    &["domain"],
    &["ui"],
];
// Those of impshell's, the features one layer.
const SHELL_LAYERS: [&[&str]; 17] = [
    &["sys"],
    &["sync"],
    &["pool", "crew"],
    &["clock"],
    &["crypto"],
    &["storage"],
    &["usb"],
    &["display", "input", "media", "power"],
    &["sound"],
    &["cell"],
    &["wifi"],
    &["net"],
    &["openai", "timekeeper"],
    &["backlog", "voiced"],
    &[
        "about",
        "aloud",
        "bell",
        "calls",
        "camera",
        "chat",
        "cost",
        "gallery",
        "magic",
        "memos",
        "news",
        "pokedex",
        "router",
        "rundown",
        "signal",
        "translator",
        "web",
    ],
    &["app"],
    &["pid1"],
];

struct Token {
    kind: Kind,
    text: String,
    line: usize,
}

// A source's tokens, its comments left out, each with the line it begins on.
fn tokens(source: &str) -> Vec<Token> {
    let (mut at, mut line, mut tokens) = (0, 1, Vec::new());
    for (kind, range, _) in syntax::pieces(source) {
        line += source[at..range.start].matches('\n').count();
        at = range.start;
        if kind != Kind::Comment {
            tokens.push(Token { kind, text: source[range].into(), line });
        }
    }
    tokens
}

fn find_closing(tokens: &[Token], open: usize) -> usize {
    let close = match tokens[open].text.as_str() {
        "(" => ")",
        "[" => "]",
        "<" => ">",
        _ => "}",
    };
    let mut depth = 0;
    for (at, token) in tokens.iter().enumerate().skip(open) {
        if token.text == tokens[open].text {
            depth += 1;
        } else if token.text == close {
            depth -= 1;
            if depth == 0 {
                return at;
            }
        }
    }
    tokens.len() - 1
}

fn is_word(tokens: &[Token], at: usize) -> bool {
    tokens.get(at).is_some_and(|token| token.kind == Kind::Word)
}

fn text(tokens: &[Token], at: usize) -> &str {
    tokens.get(at).map_or("", |token| token.text.as_str())
}

fn words(tokens: &[Token]) -> impl Iterator<Item = (usize, &str)> {
    tokens.iter().enumerate().filter(|(_, token)| token.kind == Kind::Word).map(|(at, token)| (at, token.text.as_str()))
}

fn read_path(tokens: &[Token], mut at: usize) -> (Vec<String>, usize) {
    while matches!(text(tokens, at), "&" | "dyn" | "mut") {
        at += 1;
    }
    let mut words = Vec::new();
    while is_word(tokens, at) {
        words.push(tokens[at].text.clone());
        at += 1;
        if text(tokens, at) != "::" {
            break;
        }
        at += 1;
    }
    if text(tokens, at) == "<" {
        at = find_closing(tokens, at) + 1;
    }
    (words, at)
}

fn find_semicolon(tokens: &[Token], mut at: usize) -> usize {
    let mut depth = 0i32;
    while at < tokens.len() {
        match text(tokens, at) {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            ";" if depth == 0 => return at,
            _ => {}
        }
        at += 1;
    }
    at
}

fn find_statement(tokens: &[Token], at: usize) -> (usize, usize) {
    let end = find_semicolon(tokens, at);
    (end, (at..end).find(|&place| text(tokens, place) == "=").unwrap_or(end))
}

fn find_opening(tokens: &[Token], mut at: usize) -> usize {
    let mut depth = 0i32;
    while at < tokens.len() {
        match text(tokens, at) {
            "(" | "[" => depth += 1,
            ")" | "]" => depth -= 1,
            "{" | ";" if depth == 0 => return at,
            _ => {}
        }
        at += 1;
    }
    at
}

struct Function {
    name: String,
    line: usize,
    owner: Option<String>,
    trait_name: Option<String>,
    trait_path: Vec<String>,
    is_test: bool,
    is_test_code: bool,
    span: Range<usize>,
    body: Range<usize>,
}

struct Type {
    name: String,
    fields: Range<usize>,
    is_kernel: bool,
}

struct Constant {
    name: String,
    kind: Range<usize>,
    value: Range<usize>,
}

#[derive(Default)]
struct Items {
    functions: Vec<Function>,
    types: Vec<Type>,
    constants: Vec<Constant>,
    traits: Vec<String>,
    aliases: Vec<(String, Range<usize>)>,
    thread_statics: Vec<Range<usize>>,
    modules: Vec<(String, Option<String>, bool)>,
    test_ranges: Vec<Range<usize>>,
    imports: HashMap<String, Vec<String>>,
    // What each name a `use` gives is called where it is taken from.
    originals: HashMap<String, String>,
    module_imports: HashSet<String>,
}

struct Import {
    name: String,
    path: Vec<String>,
    is_module: bool,
    original: String,
}

// What a `use` takes, each by the name it has in the file, and the paths it takes whole by a glob.
fn to_imports(tokens: &[Token]) -> (Vec<Import>, Vec<Vec<String>>) {
    let texts: Vec<&str> = tokens.iter().map(|token| token.text.as_str()).collect();
    let (mut imports, mut globs) = (Vec::new(), Vec::new());
    for (path, leaf, given) in syntax::use_leaves(&texts) {
        let given = given.filter(|given| given != "_");
        match leaf {
            Leaf::Glob => globs.push(path),
            Leaf::Name(original) => {
                let name = given.unwrap_or_else(|| original.clone());
                imports.push(Import { name, path, is_module: false, original });
            }
            Leaf::Module => {
                if let Some((module, rest)) = path.split_last() {
                    let name = given.unwrap_or_else(|| module.clone());
                    imports.push(Import { name, path: rest.to_vec(), is_module: true, original: module.clone() });
                }
            }
        }
    }
    (imports, globs)
}

// The arguments of a list, each whole with what its own brackets hold.
fn arguments(list: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0, 0);
    for (at, letter) in list.char_indices() {
        match letter {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&list[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&list[start..]);
    parts
}

// True for a `cfg` predicate that holds in a build of tests alone: `test`, or an `all` one of whose own arguments is such; under `not` or `any` another build holds the item too.
fn requires_test(predicate: &str) -> bool {
    let all = predicate.strip_prefix("all(").and_then(|rest| rest.strip_suffix(')'));
    predicate == "test" || all.is_some_and(|list| arguments(list).into_iter().any(requires_test))
}

fn is_test_attribute(attribute: &str) -> bool {
    let predicate = attribute.strip_prefix("cfg(").and_then(|rest| rest.strip_suffix(')'));
    attribute == "test" || predicate.is_some_and(requires_test)
}

#[derive(Clone, Default)]
struct Group {
    is_test: bool,
    owner: Option<String>,
    trait_name: Option<String>,
    trait_path: Vec<String>,
}

fn items(tokens: &[Token], is_test_file: bool) -> Items {
    let mut items = Items::default();
    let mut groups: Vec<Group> = Vec::new();
    let mut attributes: Vec<String> = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        let is_in_test = is_test_file || groups.iter().any(|group| group.is_test);
        let is_test_item = is_in_test || attributes.iter().any(|attribute| is_test_attribute(attribute));
        let next = text(tokens, at + 1);
        match tokens[at].text.as_str() {
            "{" => groups.push(Group { is_test: is_test_item, ..Group::default() }),
            "}" => drop(groups.pop()),
            ";" => attributes.clear(),
            _ => {}
        }
        // Where an item ends: the walk goes on behind it, its attributes spent.
        let item_end = match tokens[at].text.as_str() {
            "#" => {
                let open = if next == "!" { at + 2 } else { at + 1 };
                if text(tokens, open) == "[" {
                    let close = find_closing(tokens, open);
                    attributes.push(tokens[open + 1..close].iter().map(|token| token.text.as_str()).collect());
                    at = close + 1;
                    continue;
                }
                None
            }
            "mod" if is_word(tokens, at + 1) => {
                if text(tokens, at + 2) == "{" {
                    if is_test_item {
                        items.test_ranges.push(at..find_closing(tokens, at + 2) + 1);
                    }
                    groups.push(Group { is_test: is_test_item, ..Group::default() });
                } else {
                    let custom = attributes.iter().find_map(|attribute| attribute.strip_prefix("path="));
                    items.modules.push((next.into(), custom.map(|path| path.trim_matches('"').into()), is_test_item));
                }
                Some(at + 3)
            }
            "impl" => {
                let start = if next == "<" { find_closing(tokens, at + 1) + 1 } else { at + 1 };
                let (mut words, end) = read_path(tokens, start);
                let name = words.pop().unwrap_or_default();
                groups.push(if text(tokens, end) == "for" {
                    let kind = read_path(tokens, end + 1).0.pop().unwrap_or_default();
                    Group { is_test: is_test_item, owner: Some(kind), trait_name: Some(name), trait_path: words }
                } else {
                    Group { is_test: is_test_item, owner: Some(name), ..Group::default() }
                });
                let open = find_opening(tokens, end);
                if is_test_item {
                    items.test_ranges.push(at..find_closing(tokens, open) + 1);
                }
                Some(open + 1)
            }
            "trait" if is_word(tokens, at + 1) => {
                items.traits.push(next.into());
                groups.push(Group { is_test: is_test_item, trait_name: Some(next.into()), ..Group::default() });
                let open = find_opening(tokens, at + 2);
                if is_test_item {
                    items.test_ranges.push(at..find_closing(tokens, open) + 1);
                }
                Some(open + 1)
            }
            "const" | "static"
                if is_word(tokens, at + 1)
                    && !matches!(next, "fn" | "unsafe" | "mut")
                    && (at == 0 || text(tokens, at - 1) != "*") =>
            {
                let (end, equals) = find_statement(tokens, at);
                items.constants.push(Constant { name: next.into(), kind: at + 3..equals, value: equals..end });
                Some(end + 1)
            }
            "type"
                if is_word(tokens, at + 1)
                    && !groups.iter().any(|group| group.owner.is_some() || group.trait_name.is_some()) =>
            {
                let (end, equals) = find_statement(tokens, at);
                items.aliases.push((next.into(), equals + 1..end));
                Some(end + 1)
            }
            "thread_local" if next == "!" && text(tokens, at + 2) == "{" => {
                items.thread_statics.push(at..find_closing(tokens, at + 2) + 1);
                None
            }
            "fn" if is_word(tokens, at + 1) => {
                let open = find_opening(tokens, at + 2);
                if text(tokens, open) == "{" {
                    let end = find_closing(tokens, open) + 1;
                    let is_test = attributes.iter().any(|attribute| attribute == "test");
                    // A method's trait is that of the impl that owns it; a function of a trait's own, that trait.
                    let owner_group = groups.iter().rev().find(|group| group.owner.is_some());
                    let group = owner_group.or_else(|| groups.iter().rev().find(|group| group.trait_name.is_some()));
                    let Group { owner, trait_name, trait_path, .. } = group.cloned().unwrap_or_default();
                    if is_test_item || is_test {
                        items.test_ranges.push(at..end);
                    }
                    items.functions.push(Function {
                        name: next.into(),
                        line: tokens[at].line,
                        owner,
                        trait_name,
                        trait_path,
                        is_test,
                        is_test_code: is_test_item || is_test,
                        span: at..end,
                        body: open..end,
                    });
                    Some(end)
                } else {
                    Some(open + 1)
                }
            }
            "struct" | "enum" | "union" if is_word(tokens, at + 1) => {
                let mut open = at + 2;
                while !matches!(text(tokens, open), "{" | "(" | ";" | "") {
                    open += 1;
                }
                let close = if text(tokens, open) == ";" { open } else { find_closing(tokens, open) + 1 };
                let end = if text(tokens, open) == "(" { find_opening(tokens, close) + 1 } else { close.max(open + 1) };
                let is_kernel = attributes.iter().any(|attribute| attribute.starts_with("repr(C"));
                items.types.push(Type { name: next.into(), fields: open..close, is_kernel });
                Some(end)
            }
            "use" => {
                let end = find_semicolon(tokens, at);
                for Import { name, path, is_module, original } in to_imports(&tokens[at + 1..end]).0 {
                    if is_module {
                        items.module_imports.insert(name.clone());
                    } else {
                        items.module_imports.remove(&name);
                    }
                    items.originals.insert(name.clone(), original);
                    items.imports.insert(name, path);
                }
                Some(end + 1)
            }
            "macro_rules" if next == "!" => Some(find_closing(tokens, find_opening(tokens, at + 2)) + 1),
            _ if tokens[at].kind == Kind::Word && next == "!" && matches!(text(tokens, at + 2), "{" | "(" | "[") => {
                Some(find_closing(tokens, at + 2) + 1)
            }
            _ => None,
        };
        match item_end {
            Some(end) => {
                // An item the tests' build alone holds is test code whatever kind it is of: a constant, a `use`, a type.
                if is_test_item {
                    items.test_ranges.push(at..end);
                }
                (at, attributes) = (end, Vec::new());
            }
            None => at += 1,
        }
    }
    items
}

struct Source {
    path: PathBuf,
    tokens: Vec<Token>,
    items: Items,
    is_test: bool,
}

impl Source {
    fn is_test_at(&self, at: usize) -> bool {
        self.is_test || self.items.test_ranges.iter().any(|range| range.contains(&at))
    }
}

fn read_sources(file: &Path, is_test: bool, sources: &mut Vec<Source>) {
    let source = fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    let tokens = tokens(&source);
    let items = items(&tokens, is_test);
    let modules = items.modules.clone();
    sources.push(Source { path: file.into(), tokens, items, is_test });
    // A file's modules of their own lie beside it, or in the directory of its name.
    let parent = file.parent().unwrap_or(Path::new("."));
    let directory = match file.file_stem().and_then(|stem| stem.to_str()) {
        Some("mod" | "main" | "lib") | None => parent.into(),
        Some(stem) => parent.join(stem),
    };
    for (name, custom, is_test_module) in modules {
        let child = match custom {
            Some(path) => parent.join(path),
            None if directory.join(format!("{name}.rs")).exists() => directory.join(format!("{name}.rs")),
            None => directory.join(name).join("mod.rs"),
        };
        read_sources(&child, is_test || is_test_module, sources);
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn to_location(file: &Path, line: usize) -> String {
    let root = workspace().canonicalize().unwrap_or_default();
    let full = file.canonicalize().unwrap_or_else(|_| file.into());
    format!("{}:{line}", full.strip_prefix(&root).unwrap_or(&full).display())
}

// Its constants and its types are numbers and layouts.
fn calls_libc(tokens: &[Token], at: usize) -> bool {
    text(tokens, at) == "libc" && text(tokens, at + 1) == "::" && text(tokens, at + 3) == "("
}

// No call, no constant, no alias of a C type.
fn is_libc_layout(tokens: &[Token], at: usize) -> bool {
    let name = text(tokens, at + 2);
    text(tokens, at) == "libc"
        && text(tokens, at + 1) == "::"
        && text(tokens, at + 3) != "("
        && name.bytes().any(|byte| byte.is_ascii_lowercase())
        && !name.starts_with("c_")
        && !name.ends_with("_t")
}

fn reads_os(tokens: &[Token], at: usize) -> bool {
    let word = text(tokens, at);
    CLOCKS.contains(&word) && text(tokens, at + 1) == "::" && text(tokens, at + 2) == "now"
        || word == "elapsed" && at > 0 && text(tokens, at - 1) == "." && text(tokens, at + 1) == "("
        || word == "last_os_error"
}

fn is_os_word(word: &str) -> bool {
    OS.iter().chain(&INCIDENTAL).chain(&LIBRARIES).any(|name| *name == word)
}

fn counts(word: &str, with_incidental: bool) -> bool {
    with_incidental || !INCIDENTAL.contains(&word)
}

// An `unsafe` block is a call past the language, to the kernel or to memory a device maps.
fn touches_os(tokens: &[Token], with_incidental: bool, is_held: impl Fn(usize, &str) -> bool) -> bool {
    words(tokens).any(|(at, word)| {
        is_os_word(word) && counts(word, with_incidental)
            || calls_libc(tokens, at)
            || reads_os(tokens, at)
            || QUERIES.contains(&word) && at > 0 && text(tokens, at - 1) == "." && text(tokens, at + 1) == "("
            || word == "unsafe" && text(tokens, at + 1) == "{"
            || with_incidental && word.starts_with("Atomic")
            || is_held(at, word)
            || LOG_MACROS.contains(&word) && text(tokens, at + 1) == "!"
    })
}

fn has_raw_pointer(tokens: &[Token]) -> bool {
    tokens.windows(2).any(|pair| pair[0].text == "*" && matches!(pair[1].text.as_str(), "const" | "mut"))
}

fn find_angle_opening(tokens: &[Token], close: usize) -> Option<usize> {
    let mut depth = 0;
    for at in (0..=close).rev() {
        match text(tokens, at) {
            ">" => depth += 1,
            "<" if depth == 1 => return Some(at),
            "<" => depth -= 1,
            _ => {}
        }
    }
    None
}

fn path_before(tokens: &[Token], at: usize) -> Vec<&str> {
    let (mut words, mut back) = (Vec::new(), at);
    while back >= 2 && text(tokens, back - 1) == "::" {
        let generic = (text(tokens, back - 2) == ">")
            .then(|| find_angle_opening(tokens, back - 2))
            .flatten()
            .filter(|&open| open >= 2 && text(tokens, open - 1) == "::");
        let word = generic.map_or(back - 2, |open| open - 2);
        if !is_word(tokens, word) {
            break;
        }
        back = word;
        words.push(text(tokens, back));
    }
    words.reverse();
    words
}

fn let_name(tokens: &[Token], at: usize) -> usize {
    if text(tokens, at + 1) == "mut" { at + 2 } else { at + 1 }
}

fn to_fields(tokens: &[Token], range: Range<usize>, is_named: bool) -> Vec<(String, Range<usize>)> {
    if range.len() < 2 {
        return Vec::new();
    }
    let (mut parts, mut start, mut depth) = (Vec::new(), range.start + 1, 0i32);
    for at in range.start + 1..range.end - 1 {
        match tokens[at].text.as_str() {
            "(" | "[" | "<" | "{" => depth += 1,
            ">" if tokens[at - 1].text == "-" => {}
            ")" | "]" | ">" | "}" => depth -= 1,
            "," if depth == 0 => {
                parts.push(start..at);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(start..range.end - 1);
    parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .enumerate()
        .filter_map(|(place, part)| {
            if !is_named {
                return Some((place.to_string(), part));
            }
            let colon = part.clone().find(|&at| tokens[at].text == ":").filter(|&colon| colon > part.start)?;
            Some((tokens[colon - 1].text.clone(), colon + 1..part.end))
        })
        .collect()
}

fn parameters(tokens: &[Token], start: usize) -> HashMap<String, Range<usize>> {
    let mut open = start + 2;
    if text(tokens, open) == "<" {
        open = find_closing(tokens, open) + 1;
    }
    if text(tokens, open) != "(" {
        return HashMap::new();
    }
    to_fields(tokens, open..find_closing(tokens, open) + 1, true).into_iter().collect()
}

// The place of the file that defines an item among the crate's, and its name.
type Key = (usize, String);

#[derive(Default)]
struct Reach {
    os: Vec<bool>,
    acts: Vec<bool>,
    layout: Vec<bool>,
    library: Vec<bool>,
    handles: HashSet<Key>,
    kernel: HashSet<Key>,
    own_world: Vec<bool>,
}

struct Crate {
    sources: Vec<Source>,
    functions: Vec<(usize, usize)>,
    definitions: HashMap<String, Vec<usize>>,
    // The module each source makes, by its path from the crate's root, and all of them.
    module_paths: Vec<Vec<String>>,
    modules: HashSet<Vec<String>>,
    kinds: HashSet<Key>,
    fields: HashMap<(Key, String), Range<usize>>,
    reach: Reach,
}

impl Crate {
    fn read(root: &Path) -> Crate {
        let mut sources = Vec::new();
        read_sources(root, false, &mut sources);
        let functions: Vec<(usize, usize)> = sources
            .iter()
            .enumerate()
            .flat_map(|(file, source)| (0..source.items.functions.len()).map(move |at| (file, at)))
            .collect();
        let mut definitions: HashMap<String, Vec<usize>> = HashMap::new();
        let (mut kinds, mut field_kinds) = (HashSet::new(), HashMap::new());
        for (file, source) in sources.iter().enumerate() {
            let items = &source.items;
            for kind in &items.types {
                let is_named = text(&source.tokens, kind.fields.start) == "{";
                for (field, range) in to_fields(&source.tokens, kind.fields.clone(), is_named) {
                    field_kinds.insert(((file, kind.name.clone()), field), range);
                }
            }
            let types = items.types.iter().map(|kind| &kind.name).chain(items.aliases.iter().map(|(name, _)| name));
            kinds.extend(types.clone().map(|name| (file, name.clone())));
            let free =
                items.functions.iter().filter(|function| function.owner.is_none() && function.trait_name.is_none());
            let names = types
                .chain(&items.traits)
                .chain(items.constants.iter().map(|constant| &constant.name))
                .chain(free.map(|function| &function.name));
            for name in names {
                let files = definitions.entry(name.clone()).or_default();
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
        let directory = root.parent().unwrap_or(Path::new("."));
        let module_paths: Vec<Vec<String>> =
            sources.iter().map(|source| module_path(directory, &source.path)).collect();
        let modules = module_paths.iter().cloned().collect();
        let fields = field_kinds;
        Crate { sources, functions, definitions, module_paths, modules, kinds, fields, reach: Reach::default() }
    }

    // The crate with what its functions reach, which the rules of the shell's functions ask.
    fn with_reach(mut self) -> Crate {
        self.reach = self.to_reach();
        self
    }

    fn index(&self, name: &str) -> usize {
        (0..self.functions.len()).find(|&index| self.function(index).2.name == name).unwrap()
    }

    fn reaches_os(&self, name: &str) -> bool {
        self.reach.os[self.index(name)]
    }

    fn plumbs(&self, name: &str) -> bool {
        self.is_plumbing(self.index(name), &HashSet::new())
    }

    fn passes(&self, name: &str) -> bool {
        self.reaches_world(self.index(name)) || self.plumbs(name)
    }

    fn function(&self, index: usize) -> (usize, &[Token], &Function) {
        let (file, at) = self.functions[index];
        (file, &self.sources[file].tokens, &self.sources[file].items.functions[at])
    }

    // The path from the crate's root that `path` stands for in `file`: behind `crate`, from the file's own module behind `self`, a module up for each `super`, through a module of the file's own, through what a `use` of the file takes by the path's first name, each such name once, or through a type the crate defines by it; none for a path another crate begins.
    fn to_root_path(&self, file: usize, path: &[&str], taken: &mut Vec<String>) -> Option<Vec<String>> {
        let (own, items) = (&self.module_paths[file], &self.sources[file].items);
        let (&first, rest) = path.split_first()?;
        let child: Vec<String> = own.iter().cloned().chain([first.to_string()]).collect();
        let mut root_path = match first {
            "crate" => Vec::new(),
            "self" => own.clone(),
            "super" => own[..own.len().saturating_sub(1)].to_vec(),
            _ if self.modules.contains(&child) => child,
            _ => match (items.imports.get(first), items.originals.get(first)) {
                (Some(import), Some(original)) if !taken.iter().any(|name| name == first) => {
                    taken.push(first.to_string());
                    let through: Vec<&str> = import.iter().map(String::as_str).chain([original.as_str()]).collect();
                    self.to_root_path(file, &through, taken)?
                }
                (None, _) if first == "Self" || self.definitions.contains_key(first) => child,
                _ => return None,
            },
        };
        for &part in rest {
            match part {
                "super" => drop(root_path.pop()),
                "self" => {}
                _ => root_path.push(part.to_string()),
            }
        }
        Some(root_path)
    }

    fn resolve(&self, file: usize, name: &str, path: &[&str]) -> Vec<Key> {
        self.resolve_files(file, name, path, &mut HashSet::new()).into_iter().map(|at| (at, name.to_string())).collect()
    }

    fn resolve_files(&self, file: usize, name: &str, path: &[&str], visited: &mut HashSet<usize>) -> Vec<usize> {
        let Some(files) = self.definitions.get(name) else {
            return Vec::new();
        };
        let items = &self.sources[file].items;
        let full_path: Vec<&str> = match path {
            [] if items.module_imports.contains(name) => Vec::new(),
            [] => items.imports.get(name).map_or_else(Vec::new, |import| import.iter().map(String::as_str).collect()),
            given => given.to_vec(),
        };
        let Some((&last, rest)) = full_path.split_last() else {
            return files.iter().copied().filter(|&at| at == file).collect();
        };
        let Some(module) = self.to_root_path(file, &full_path, &mut Vec::new()) else {
            return Vec::new();
        };
        if matches!(last, "self" | "super" | "Self") && files.contains(&file) {
            return vec![file];
        }
        let holders: Vec<usize> = (0..self.sources.len()).filter(|&at| self.module_paths[at] == module).collect();
        if holders.is_empty() {
            // The path ends in a type: its item lies where the type is defined, or where an impl of it is.
            let kinds: Vec<usize> =
                self.resolve_files(file, last, rest, visited).into_iter().filter(|at| files.contains(at)).collect();
            return if kinds.is_empty() { files.clone() } else { kinds };
        }
        let defining: Vec<usize> = holders.iter().copied().filter(|at| files.contains(at)).collect();
        if !defining.is_empty() {
            return defining;
        }
        let mut result = Vec::new();
        for at in holders {
            if visited.insert(at) {
                result.extend(self.resolve_files(at, name, &[], visited));
            }
        }
        result
    }

    fn names_item(&self, file: usize, tokens: &[Token], at: usize, items: &HashSet<Key>) -> bool {
        !items.is_empty()
            && is_word(tokens, at)
            && self.resolve(file, &tokens[at].text, &path_before(tokens, at)).iter().any(|key| items.contains(key))
    }

    fn names_layout(&self, file: usize, tokens: &[Token], kernel: &HashSet<Key>) -> bool {
        words(tokens).any(|(at, word)| {
            LAYOUT.contains(&word) || is_libc_layout(tokens, at) || self.names_item(file, tokens, at, kernel)
        })
    }

    fn holds_os(&self, file: usize, kind: Range<usize>, with_incidental: bool) -> bool {
        let tokens = &self.sources[file].tokens[kind];
        has_raw_pointer(tokens)
            || words(tokens).any(|(at, word)| {
                self.is_os_name(file, word) && counts(word, with_incidental)
                    || with_incidental && word.starts_with("Atomic")
                    || self.is_library_word(file, word)
                    || self
                        .alias_target(file, tokens, at)
                        .is_some_and(|(defining, target)| self.holds_os(defining, target, with_incidental))
            })
    }

    fn alias_target(&self, file: usize, tokens: &[Token], at: usize) -> Option<(usize, Range<usize>)> {
        self.resolve(file, &tokens[at].text, &path_before(tokens, at)).into_iter().find_map(|(defining, name)| {
            let aliases = &self.sources[defining].items.aliases;
            aliases.iter().find(|(alias, _)| *alias == name).map(|(_, target)| (defining, target.clone()))
        })
    }

    fn is_core_word(&self, file: usize, word: &str) -> bool {
        self.sources[file].items.imports.get(word).is_some_and(|path| path.first().is_some_and(|first| first == CORE))
    }

    // A whole body that hands its parameters unchanged to a core method on a field of its own: the core's decision.
    fn hands_on(&self, index: usize) -> bool {
        let (file, tokens, function) = self.function(index);
        let Some(owner) = &function.owner else {
            return false;
        };
        let body: Vec<&str> = tokens[function.body.clone()].iter().map(|token| token.text.as_str()).collect();
        let inner = body.get(1..body.len().saturating_sub(1)).unwrap_or_default();
        let inner = inner.strip_suffix(&[";"]).unwrap_or(inner);
        let ["self", ".", field, ".", _, "(", arguments @ .., ")"] = inner else {
            return false;
        };
        let parameters = parameters(tokens, function.span.start);
        let are_parameters = arguments.is_empty()
            || arguments
                .split(|text| *text == ",")
                .all(|argument| matches!(argument, [name] if parameters.contains_key(*name)));
        are_parameters
            && self.resolve(file, owner, &[]).iter().any(|(defining, kind)| {
                let tokens = &self.sources[*defining].tokens;
                let types = &self.sources[*defining].items.types;
                types.iter().filter(|candidate| candidate.name == *kind).any(|candidate| {
                    let is_named = text(tokens, candidate.fields.start) == "{";
                    to_fields(tokens, candidate.fields.clone(), is_named).into_iter().any(|(name, range)| {
                        name == *field
                            && tokens[range]
                                .iter()
                                .any(|token| token.text == CORE || self.is_core_word(*defining, &token.text))
                    })
                })
            })
    }

    fn is_os_name(&self, file: usize, word: &str) -> bool {
        is_os_word(word)
            || self.sources[file].items.imports.get(word).is_some_and(|path| {
                path.first().is_some_and(|first| first == "std") && path.iter().any(|part| is_os_word(part))
            })
    }

    fn holders(&self, items: &[(Key, &[Token])], is_holder: impl Fn(usize, &[Token]) -> bool) -> HashSet<Key> {
        let mut holders: HashSet<Key> =
            items.iter().filter(|(key, tokens)| is_holder(key.0, tokens)).map(|(key, _)| key.clone()).collect();
        loop {
            let more: Vec<Key> = items
                .iter()
                .filter(|(key, tokens)| {
                    !holders.contains(key) && (0..tokens.len()).any(|at| self.names_item(key.0, tokens, at, &holders))
                })
                .map(|(key, _)| key.clone())
                .collect();
            if more.is_empty() {
                return holders;
            }
            holders.extend(more);
        }
    }

    fn owners(&self, index: usize) -> Vec<Key> {
        let (file, _, function) = self.function(index);
        function.owner.as_deref().map_or_else(Vec::new, |owner| {
            self.resolve(file, owner, &[]).into_iter().filter(|key| self.kinds.contains(key)).collect()
        })
    }

    fn kind_of(&self, file: usize, name: &str, path: &[&str], kinds: &HashSet<Key>) -> Option<Key> {
        self.resolve(file, name, path).into_iter().find(|key| kinds.contains(key))
    }

    fn find_kind(&self, file: usize, kind: Range<usize>, kinds: &HashSet<Key>) -> Option<Key> {
        let tokens = &self.sources[file].tokens;
        kind.filter(|&at| is_word(tokens, at))
            .find_map(|at| self.kind_of(file, &tokens[at].text, &path_before(tokens, at), kinds))
    }

    fn locals(&self, file: usize, body: Range<usize>, own: &Option<Key>) -> HashMap<&str, Key> {
        let tokens = &self.sources[file].tokens;
        let mut locals = HashMap::new();
        for at in body.filter(|&at| text(tokens, at) == "let") {
            let name = let_name(tokens, at);
            if !is_word(tokens, name) {
                continue;
            }
            let kind = match text(tokens, name + 1) {
                ":" => {
                    let end = (name + 2..tokens.len())
                        .find(|&place| matches!(text(tokens, place), "=" | ";"))
                        .unwrap_or(tokens.len());
                    self.find_kind(file, name + 2..end, &self.kinds)
                }
                "=" => {
                    let (mut path, mut end) = (Vec::new(), name + 2);
                    while is_word(tokens, end) {
                        path.push(text(tokens, end));
                        end += 1;
                        if text(tokens, end) == "::" && text(tokens, end + 1) == "<" {
                            end = find_closing(tokens, end + 1) + 1;
                        }
                        if text(tokens, end) != "::" {
                            break;
                        }
                        end += 1;
                    }
                    let maker = match text(tokens, end) {
                        "{" => path.split_last(),
                        "(" => path.split_last().and_then(|(_, rest)| rest.split_last()),
                        _ => None,
                    };
                    match maker {
                        Some((&"Self", _)) => own.clone(),
                        Some((&last, rest)) => self.kind_of(file, last, rest, &self.kinds),
                        None => None,
                    }
                }
                _ => None,
            };
            if let Some(kind) = kind {
                locals.insert(tokens[name].text.as_str(), kind);
            }
        }
        locals
    }

    // A library's crate by its name, a name a `use` takes out of one, or the crate itself by the name a `use` gives it.
    fn is_library_word(&self, file: usize, word: &str) -> bool {
        let items = &self.sources[file].items;
        let taken_from = items.imports.get(word).and_then(|path| path.first()).or_else(|| items.originals.get(word));
        LIBRARIES.contains(&word) || taken_from.is_some_and(|first| LIBRARIES.contains(&first.as_str()))
    }

    fn to_reach(&self) -> Reach {
        let (mut shapes, mut constants, mut kernel) = (Vec::new(), Vec::new(), HashSet::new());
        for (file, source) in self.sources.iter().enumerate() {
            let (items, tokens) = (&source.items, &source.tokens);
            let aliases = items.aliases.iter().map(|(name, target)| (name, target));
            for (name, range) in items.types.iter().map(|kind| (&kind.name, &kind.fields)).chain(aliases) {
                shapes.push(((file, name.clone()), &tokens[range.clone()]));
            }
            kernel.extend(items.types.iter().filter(|kind| kind.is_kernel).map(|kind| (file, kind.name.clone())));
            constants.extend(items.constants.iter().map(|constant| {
                let is_thread = items.thread_statics.iter().any(|range| range.contains(&constant.kind.start));
                let (kind, value) = (&tokens[constant.kind.clone()], &tokens[constant.value.clone()]);
                ((file, constant.name.clone()), kind, value, is_thread)
            }));
        }
        let layouts: Vec<Key> = constants
            .iter()
            .filter(|(key, kind, value, _)| {
                self.names_layout(key.0, kind, &kernel) || self.names_layout(key.0, value, &kernel)
            })
            .map(|(key, ..)| key.clone())
            .collect();
        kernel.extend(layouts);
        let function_tokens = |index: usize| {
            let (file, tokens, function) = self.function(index);
            (file, &tokens[function.span.clone()])
        };
        let world = |with_incidental: bool| {
            let field_holders = self.holders(&shapes, |file, fields| {
                has_raw_pointer(fields)
                    || touches_os(fields, with_incidental, |_, word| {
                        self.is_library_word(file, word) || self.is_os_name(file, word) && counts(word, with_incidental)
                    })
            });
            // A static of a thread's lock or flag, of a type that holds one, or of each thread's own reaches the OS wherever it is named.
            let held_statics: HashSet<Key> = constants
                .iter()
                .filter(|(key, kind, _, is_thread)| {
                    *is_thread
                        || touches_os(kind, with_incidental, |at, _| self.names_item(key.0, kind, at, &field_holders))
                })
                .map(|(key, ..)| key.clone())
                .collect();
            let (callees, uses_handle) = self.callees(&field_holders, with_incidental);
            // A function that lets go of a handle it was handed, putting another value in its place or dropping it, reaches the world as the handle's letting go does.
            let lets_go = |index: usize| {
                let (file, tokens, function) = self.function(index);
                parameters(tokens, function.span.start).into_iter().any(|(name, kind)| {
                    self.find_kind(file, kind, &field_holders).is_some()
                        && function.body.clone().any(|at| {
                            text(tokens, at) == name
                                && (text(tokens, at + 1) == "=" && text(tokens, at + 2) != "="
                                    || text(tokens, at.saturating_sub(1)) == "("
                                        && text(tokens, at.saturating_sub(2)) == "drop")
                        })
                })
            };
            // A name a `use` takes from a module of the OS's reaches it as the module's own names do.
            let os = (0..self.functions.len())
                .map(|index| {
                    let (file, tokens) = function_tokens(index);
                    uses_handle[index]
                        || lets_go(index)
                        || touches_os(tokens, with_incidental, |at, word| {
                            self.names_item(file, tokens, at, &held_statics)
                                || self.is_os_name(file, word) && counts(word, with_incidental)
                        })
                })
                .collect();
            let os = spread(os, &callees);
            // A type whose letting go reaches the OS owns what the OS gave it: it is a handle, whatever its fields are.
            let drop_owners: Vec<Key> = (0..self.functions.len())
                .filter(|&index| os[index] && self.function(index).2.trait_name.as_deref() == Some("Drop"))
                .flat_map(|index| self.owners(index))
                .collect();
            (os, field_holders.into_iter().chain(drop_owners).collect::<HashSet<Key>>(), callees)
        };
        let ((os, _, callees), (acts, handles, _)) = (world(true), world(false));
        let (layout, library): (Vec<bool>, Vec<bool>) = (0..self.functions.len())
            .map(|index| {
                let (file, tokens) = function_tokens(index);
                (
                    self.names_layout(file, tokens, &kernel),
                    words(tokens).any(|(_, word)| self.is_library_word(file, word)),
                )
            })
            .unzip();
        let own_world = (0..self.functions.len()).map(|index| self.touches_own_world(index, &handles)).collect();
        let (layout, library, own_world) =
            (spread(layout, &callees), spread(library, &callees), spread(own_world, &callees));
        Reach { os, acts, layout, library, handles, kernel, own_world }
    }

    fn touches_own_world(&self, index: usize, handles: &HashSet<Key>) -> bool {
        let (_, tokens, function) = self.function(index);
        let Some(own) = self.owners(index).into_iter().next() else {
            return false;
        };
        function.body.clone().filter(|&at| text(tokens, at) == "self").any(|at| {
            let mut previous = at.saturating_sub(1);
            while matches!(text(tokens, previous), "&" | "*" | "mut") && previous > 0 {
                previous -= 1;
            }
            let is_taken_apart =
                matches!(text(tokens, previous), "match" | "=") && matches!(text(tokens, at + 1), "{" | "else");
            // The fields from `self` on, each read in the type the one before it is of, up to a method's call.
            let (mut kind, mut place, mut last) = (own.clone(), at + 2, None);
            while text(tokens, place - 1) == "." && text(tokens, place + 1) != "(" {
                let Some(range) = self.fields.get(&(kind.clone(), text(tokens, place).to_string())) else {
                    break;
                };
                last = Some((kind.0, range.clone()));
                let Some(next) = self.find_kind(kind.0, range.clone(), &self.kinds) else {
                    break;
                };
                (kind, place) = (next, place + 2);
            }
            is_taken_apart && handles.contains(&own)
                || last.is_some_and(|(file, range)| {
                    self.holds_os(file, range.clone(), false)
                        || range.into_iter().any(|at| self.names_item(file, &self.sources[file].tokens, at, handles))
                })
        })
    }

    // A method by the type of what it is called on, a trait's default beside it, a function or a type's method by its path, its `use` or its name in its own file, and nothing another crate holds.
    fn callees(&self, handles: &HashSet<Key>, with_incidental: bool) -> (Vec<Vec<usize>>, Vec<bool>) {
        let mut defaults: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut methods: HashMap<(Key, &str), Vec<usize>> = HashMap::new();
        let mut local_functions: HashMap<Key, Vec<usize>> = HashMap::new();
        for index in 0..self.functions.len() {
            let (file, _, function) = self.function(index);
            let name = function.name.as_str();
            if function.owner.is_some() {
                for owner in self.owners(index) {
                    methods.entry((owner, name)).or_default().push(index);
                }
            } else if function.trait_name.is_some() {
                defaults.entry(name).or_default().push(index);
            } else {
                local_functions.entry((file, name.to_string())).or_default().push(index);
            }
        }
        let mut uses_handle = vec![false; self.functions.len()];
        let callees = (0..self.functions.len())
            .map(|index| {
                let (file, tokens, function) = self.function(index);
                let parameters = parameters(tokens, function.span.start);
                let own = self.owners(index).into_iter().next();
                let locals = self.locals(file, function.body.clone(), &own);
                // The names a parameter binds from the body's start on, and a `let` from the end of its statement on, each by the first place it is bound from.
                let mut bindings: HashMap<&str, usize> =
                    parameters.keys().map(|name| (name.as_str(), function.body.start)).collect();
                for at in function.body.clone().filter(|&at| text(tokens, at) == "let") {
                    let name = text(tokens, let_name(tokens, at));
                    let start = bindings.entry(name).or_insert(usize::MAX);
                    *start = (*start).min(find_semicolon(tokens, at));
                }
                let mut callees = Vec::new();
                for at in function.body.clone() {
                    let token = &tokens[at];
                    if token.kind != Kind::Word || text(tokens, at + 1) == "!" {
                        continue;
                    }
                    let name = token.text.as_str();
                    let preceding = if at > 0 { text(tokens, at - 1) } else { "" };
                    let is_call = text(tokens, at + 1) == "(";
                    let path = if preceding == "::" { path_before(tokens, at) } else { Vec::new() };
                    if preceding == "." && is_call {
                        // The fields from `self` to the method, read back, and the type each leads to.
                        let mut chain = Vec::new();
                        let mut dot = at - 1;
                        while dot >= 2 && text(tokens, dot) == "." && tokens[dot - 1].kind != Kind::Mark {
                            chain.push(text(tokens, dot - 1));
                            if text(tokens, dot - 2) != "." {
                                break;
                            }
                            dot -= 2;
                        }
                        // The chain begins at `self`, at a parameter, at a local a `let` gives a type, or at a type that is its own value, whose type is known; at anything else it is not.
                        let root = chain.last().copied();
                        let unit = root
                            .filter(|root| root.starts_with(char::is_uppercase))
                            .and_then(|root| self.kind_of(file, root, &[], &self.kinds));
                        let mut owner = match root.and_then(|root| parameters.get(root)) {
                            Some(kind) if self.holds_os(file, kind.clone(), with_incidental) => {
                                uses_handle[index] = true;
                                None
                            }
                            Some(kind) => self.find_kind(file, kind.clone(), &self.kinds),
                            None if root == Some("self") => own.clone(),
                            None => root.and_then(|root| locals.get(root).cloned()).or(unit),
                        };
                        for field in chain.iter().rev().skip(1) {
                            let Some(kind) = owner.take() else {
                                break;
                            };
                            let Some(range) = self.fields.get(&(kind.clone(), field.to_string())) else {
                                break;
                            };
                            if self.holds_os(kind.0, range.clone(), with_incidental) {
                                uses_handle[index] = true;
                            }
                            owner = self.find_kind(kind.0, range.clone(), &self.kinds);
                        }
                        if let Some(owner) = owner {
                            let owner_methods = methods.get(&(owner.clone(), name));
                            let is_stream_method = |method: &usize| {
                                matches!(self.function(*method).2.trait_name.as_deref(), Some("Read" | "Write"))
                            };
                            if handles.contains(&owner)
                                && STREAM_METHODS.contains(&name)
                                && owner_methods.is_none_or(|methods| methods.iter().any(is_stream_method))
                            {
                                uses_handle[index] = true;
                            }
                            callees.extend(owner_methods.or_else(|| defaults.get(name)).into_iter().flatten());
                        }
                    } else if let Some((&last, rest)) = path.split_last() {
                        let kind =
                            if last == "Self" { own.clone() } else { self.kind_of(file, last, rest, &self.kinds) };
                        match kind {
                            Some(kind) => callees.extend(methods.get(&(kind, name)).into_iter().flatten()),
                            None => callees.extend(
                                self.resolve(file, name, &path)
                                    .iter()
                                    .filter_map(|key| local_functions.get(key))
                                    .flatten(),
                            ),
                        }
                    } else if is_call && !bindings.get(name).is_some_and(|&start| at > start) {
                        // A function of the file's own is called by its name, which a `use` can then take as no function, only as a module or a type.
                        match local_functions.get(&(file, name.to_string())) {
                            Some(own) => callees.extend(own),
                            None => callees.extend(
                                self.resolve(file, name, &[])
                                    .iter()
                                    .filter_map(|key| local_functions.get(key))
                                    .flatten(),
                            ),
                        }
                    }
                }
                callees
            })
            .collect();
        (callees, uses_handle)
    }

    fn decides(&self, index: usize) -> bool {
        let (_, tokens, function) = self.function(index);
        words(&tokens[function.body.clone()]).any(|(_, word)| BRANCHES.contains(&word))
    }

    fn reaches_world(&self, index: usize) -> bool {
        self.reach.acts[index] || self.reach.os[index] && !self.decides(index)
    }

    fn is_grounded(&self, index: usize) -> bool {
        let (_, tokens, function) = self.function(index);
        let is_kernel = tokens[function.span.clone()].iter().any(|token| {
            token.text == "libc"
                || token.kind == Kind::Text
                    && KERNEL_FILESYSTEMS.iter().any(|root| to_literal_text(&token.text).starts_with(root))
        });
        self.reach.acts[index] || self.reach.layout[index] || self.reach.library[index] || is_kernel
    }

    // A method the language, the core or a library asks of a type, a handle's method that decides nothing or touches the world its type holds, a call handed to the core, a kernel structure, a library call, or an effect's text.
    fn is_plumbing(&self, index: usize, core_traits: &HashSet<String>) -> bool {
        let (file, tokens, function) = self.function(index);
        let signature = &tokens[function.span.start..function.body.start];
        let body = &tokens[function.body.clone()];
        let decides = self.decides(index);
        let makes_text = body
            .iter()
            .enumerate()
            .any(|(at, token)| TEXT_MACROS.contains(&token.text.as_str()) && text(body, at + 1) == "!");
        function.trait_name.as_ref().is_some_and(|name| {
            LANGUAGE.contains(&name.as_str())
                || core_traits.contains(name)
                || self.is_library_word(file, name)
                || function.trait_path.first().is_some_and(|first| self.is_library_word(file, first))
        }) || function
            .owner
            .as_ref()
            .is_some_and(|owner| self.resolve(file, owner, &[]).iter().any(|key| self.reach.handles.contains(key)))
            && (!decides || self.reach.own_world[index])
            || self.hands_on(index)
            || self.names_layout(file, &tokens[function.span.clone()], &self.reach.kernel)
            || self.reach.library[index]
            || !decides && (makes_text || signature.iter().any(|token| token.text == "io"))
    }
}

fn spread(mut reaches: Vec<bool>, callees: &[Vec<usize>]) -> Vec<bool> {
    loop {
        let more: Vec<usize> = (0..reaches.len())
            .filter(|&index| !reaches[index] && callees[index].iter().any(|&callee| reaches[callee]))
            .collect();
        if more.is_empty() {
            return reaches;
        }
        more.into_iter().for_each(|index| reaches[index] = true);
    }
}

fn write_crate(test: &str, files: &[(&str, &str)]) -> Crate {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    for (name, source) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    Crate::read(&root.join("main.rs")).with_reach()
}

fn module_crate(test: &str, lines: &[&str]) -> Crate {
    write_crate(test, &[("main.rs", "mod held;\n"), ("held.rs", &lines.join("\n"))])
}

// Each of the two is read once for every rule that asks it, the shell alone with what its functions reach.
static IMPSHELL: LazyLock<Crate> =
    LazyLock::new(|| Crate::read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("main.rs")).with_reach());
static FUNCORE: LazyLock<Crate> =
    LazyLock::new(|| Crate::read(&workspace().join("crates").join("funcore").join("src").join("lib.rs")));

fn impshell() -> &'static Crate {
    &IMPSHELL
}

fn funcore() -> &'static Crate {
    &FUNCORE
}

fn image() -> Crate {
    Crate::read(&workspace().join("crates").join("image").join("src").join("lib.rs"))
}

fn is_dependency_table(table: &str) -> bool {
    matches!(table, "dependencies" | "build-dependencies")
        || table.starts_with("target.") && table.ends_with("dependencies")
}

// The lines of a manifest that declare a dependency `allowed` does not name, as a key of a table of dependencies or as a table of its own, or a build script.
fn find_dependencies(manifest: &str, allowed: &[&str]) -> Vec<String> {
    let (mut table, mut lines) = ("", Vec::new());
    for line in manifest.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        if line.starts_with('[') {
            table = line.trim_matches(['[', ']']);
            let own = table.rsplit_once('.').filter(|(holder, _)| is_dependency_table(holder));
            if own.is_some_and(|(_, name)| !allowed.contains(&name.trim_matches(['"', '\'']))) {
                lines.push(format!("[{table}]"));
            }
            continue;
        }
        let key = line.split(['=', '.', ' ']).next().unwrap_or_default();
        if is_dependency_table(table) && !allowed.contains(&key) || table == "package" && key == "build" {
            lines.push(format!("[{table}] {line}"));
        }
    }
    lines
}

fn to_dependency_lines(crate_dir: &str, allowed: &[&str]) -> Vec<String> {
    let manifest = fs::read_to_string(workspace().join(crate_dir).join("Cargo.toml")).unwrap();
    let mut lines: Vec<String> = find_dependencies(&manifest, allowed)
        .into_iter()
        .map(|line| format!("{crate_dir}/Cargo.toml: {line}"))
        .collect();
    if workspace().join(crate_dir).join("build.rs").exists() {
        lines.push(format!("{crate_dir}/build.rs: a build script"));
    }
    lines
}

// Machine code, foreign functions, names for the linker and memory reached as a device's all need `unsafe`.
const SAFE: [&str; 16] =
    ["#", "!", "[", "cfg_attr", "(", "not", "(", "test", ")", ",", "deny", "(", "unsafe_code", ")", ")", "]"];
// The one module of funcore's that holds `unsafe`: its kernels' NEON bodies, functions of a target feature, which plain code calls only in `unsafe`.
const KERNELS: &str = "base/simd.rs";
// The lint levels that would let `unsafe` in where the crate denies it.
const LIFTS: [&str; 3] = ["allow", "expect", "warn"];
// What a static of one of these holds is state no caller hands in.
const MUTABLE: [&str; 9] =
    ["Cell", "RefCell", "UnsafeCell", "OnceCell", "LazyCell", "Mutex", "RwLock", "OnceLock", "LazyLock"];

fn to_literal_text(token: &str) -> String {
    let text = token.trim_start_matches(['b', 'r', 'c']).trim_matches('#').trim_matches('"');
    let mut result = String::new();
    let mut is_in_braces = false;
    for letter in text.chars() {
        match letter {
            '{' => is_in_braces = true,
            '}' => is_in_braces = false,
            _ if !is_in_braces => result.push(letter),
            _ => {}
        }
    }
    result
}

fn is_screen_word(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.windows(2).any(|pair| pair.iter().all(u8::is_ascii_uppercase)) && !bytes.iter().any(u8::is_ascii_lowercase)
}

fn is_ui_file(source: &Source) -> bool {
    source.path.components().any(|part| part.as_os_str() == "ui")
}

// A single capital word elsewhere in the ui is none, as the kernel's names and the protocols' words are spelled so too.
fn screen_words(funcore: &Crate) -> HashSet<String> {
    let mut words = HashSet::new();
    for source in funcore.sources.iter().filter(|source| is_ui_file(source)) {
        let is_word_file = source.path.ends_with("words.rs");
        for (_, token) in source.tokens.iter().enumerate().filter(|(at, _)| !source.is_test_at(*at)) {
            let word = to_literal_text(&token.text);
            if token.kind == Kind::Text && is_screen_word(&word) && (is_word_file || word.contains(' ')) {
                words.insert(word);
            }
        }
    }
    words
}

fn word_constants(funcore: &Crate) -> HashSet<Key> {
    let mut keys = HashSet::new();
    for (file, source) in funcore.sources.iter().enumerate().filter(|(_, source)| is_ui_file(source)) {
        let constants = source.items.constants.iter().filter(|constant| {
            let literal = source.tokens[constant.value.clone()].iter().find(|token| token.kind == Kind::Text);
            !source.is_test_at(constant.kind.start)
                && literal.is_some_and(|literal| is_screen_word(&to_literal_text(&literal.text)))
        });
        keys.extend(constants.map(|constant| (file, constant.name.clone())));
    }
    keys
}

type Uses = HashMap<String, Vec<(Vec<String>, String)>>;

fn to_uses(source: &Source) -> (Uses, Vec<Vec<String>>) {
    let tokens = &source.tokens;
    let (mut names, mut globs) = (Uses::new(), Vec::new());
    for at in (0..tokens.len()).filter(|&at| is_word(tokens, at) && text(tokens, at) == "use" && !source.is_test_at(at))
    {
        let (imports, import_globs) = to_imports(&tokens[at + 1..find_semicolon(tokens, at)]);
        for Import { name, path, original, .. } in imports {
            names.entry(name).or_default().push((path, original));
        }
        globs.extend(import_globs);
    }
    (names, globs)
}

fn module_path(root: &Path, file: &Path) -> Vec<String> {
    syntax::module_path(&file.strip_prefix(root).unwrap_or(file).to_string_lossy())
}

// By a path from the core's root, through any `use` that takes it or the first word of its path by any name, or through a glob; a name the core's own `use` gives a word is one too.
fn find_named_words(shell: &Crate, core: &Crate, words: &HashSet<Key>) -> Vec<String> {
    let mut modules: HashMap<Vec<String>, Vec<usize>> = HashMap::new();
    for (file, path) in core.module_paths.iter().enumerate() {
        modules.entry(path.clone()).or_default().push(file);
    }
    let core_uses: Vec<Uses> = core.sources.iter().map(|source| to_uses(source).0).collect();
    let names_word = |holder: usize, name: &str, path: &[&str]| {
        core.resolve(holder, name, path).iter().any(|key| words.contains(key))
    };
    let is_word_path = |path: &[String]| {
        let Some((name, [first, module @ ..])) = path.split_last() else {
            return false;
        };
        first == CORE
            && modules.get(module).into_iter().flatten().any(|&holder| {
                names_word(holder, name.as_str(), &[])
                    || core_uses[holder].get(name).into_iter().flatten().any(|(import, original)| {
                        let import: Vec<&str> = import.iter().map(String::as_str).collect();
                        original != name && names_word(holder, original.as_str(), &import)
                    })
            })
    };
    let mut locations = Vec::new();
    for source in &shell.sources {
        let (imports, globs) = to_uses(source);
        let tokens = &source.tokens;
        for at in (0..tokens.len()).filter(|&at| is_word(tokens, at) && !source.is_test_at(at)) {
            let mut path: Vec<String> = path_before(tokens, at).into_iter().map(String::from).collect();
            path.push(tokens[at].text.clone());
            let (head, rest) = (path[0].as_str(), &path[1..]);
            // The paths from the core's root its first word stands for: the core's own, each a `use` takes it on, or else each a glob does.
            let heads: Vec<Vec<String>> = match imports.get(head) {
                _ if head == CORE => vec![vec![head.to_string()]],
                Some(import) => {
                    import.iter().map(|(path, original)| path.iter().chain([original]).cloned().collect()).collect()
                }
                None => globs.iter().map(|glob| glob.iter().cloned().chain([head.to_string()]).collect()).collect(),
            };
            if heads.into_iter().any(|head| is_word_path(&[head, rest.to_vec()].concat())) {
                locations.push(format!("{} {}", to_location(&source.path, tokens[at].line), tokens[at].text));
            }
        }
    }
    locations
}

fn inline_modules(tokens: &[Token]) -> Vec<Range<usize>> {
    (0..tokens.len())
        .filter(|&at| text(tokens, at) == "mod" && is_word(tokens, at + 1) && text(tokens, at + 2) == "{")
        .map(|at| at..find_closing(tokens, at + 2))
        .collect()
}

// Where a path at `at` goes on from the crate's root: behind `crate::`, or behind as many `super::` as its module lies deep.
fn root_path(tokens: &[Token], at: usize, depth: usize) -> Option<usize> {
    match text(tokens, at) {
        "crate" if text(tokens, at + 1) == "::" => Some(at + 2),
        "super" if depth > 0 && (at == 0 || text(tokens, at - 1) != "::") => {
            let is_super =
                |step: &usize| text(tokens, at + 2 * step) == "super" && text(tokens, at + 2 * step + 1) == "::";
            let supers = (0..).take_while(is_super).count();
            (supers == depth).then_some(at + 2 * supers)
        }
        _ => None,
    }
}

// The names a path leads to from `at`: one, or each a group there begins with.
fn first_names(tokens: &[Token], at: usize) -> Vec<usize> {
    if text(tokens, at) != "{" {
        return vec![at];
    }
    let (mut places, mut depth) = (Vec::new(), 0);
    for place in at..find_closing(tokens, at) {
        match text(tokens, place) {
            "{" => depth += 1,
            "}" => depth -= 1,
            _ if depth == 1 && matches!(text(tokens, place - 1), "{" | ",") => places.push(place),
            _ => {}
        }
    }
    places
}

// Each module of the root no layer names, and each path from a module into another of its own layer or a higher one.
fn find_upward_paths(target: &Crate, layers: &[&[&str]]) -> Vec<String> {
    let root_source = &target.sources[0];
    let layer_of = |module: &str| layers.iter().position(|layer| layer.contains(&module));
    let unnamed = root_source.items.modules.iter().filter(|(name, _, is_test)| !is_test && layer_of(name).is_none());
    let mut locations: Vec<String> =
        unnamed.map(|(name, ..)| format!("{} in no layer: {name}", to_location(&root_source.path, 1))).collect();
    for (source, path) in target.sources.iter().zip(&target.module_paths) {
        let tokens = &source.tokens;
        let own = path.first().map(String::as_str).and_then(|module| Some((module, layer_of(module)?)));
        let Some((own, own_layer)) = own else {
            continue;
        };
        let inline = inline_modules(tokens);
        for at in 0..tokens.len() {
            let depth = path.len() + inline.iter().filter(|module| module.contains(&at)).count();
            for place in root_path(tokens, at, depth).map_or_else(Vec::new, |start| first_names(tokens, start)) {
                let name = text(tokens, place);
                if name != own && layer_of(name).is_some_and(|layer| layer >= own_layer) {
                    locations.push(format!("{} {name}", to_location(&source.path, tokens[place].line)));
                }
            }
        }
    }
    locations
}

fn find_state(tokens: &[Token], at: usize) -> Option<String> {
    match (text(tokens, at), text(tokens, at + 1)) {
        ("thread_local", "!") => Some("thread_local".into()),
        ("static", "mut") => Some("static mut".into()),
        ("static", name) => {
            let mut kind = tokens[at + 2..find_semicolon(tokens, at)].iter().take_while(|token| token.text != "=");
            kind.any(|token| token.text.starts_with("Atomic") || MUTABLE.contains(&token.text.as_str()))
                .then(|| format!("static {name}"))
        }
        _ => None,
    }
}

fn find_os(tokens: &[Token], at: usize) -> Option<String> {
    let token = &tokens[at];
    let is_os = token.kind == Kind::Word
        && (token.text == "libc"
            || is_os_word(&token.text)
            || reads_os(tokens, at)
            || token.text.starts_with("Atomic")
            || LOG_MACROS.contains(&token.text.as_str()) && text(tokens, at + 1) == "!");
    is_os.then(|| token.text.clone())
}

fn outside_tests(target: &Crate, find: impl Fn(&[Token], usize) -> Option<String>) -> Vec<String> {
    let mut locations = Vec::new();
    for source in &target.sources {
        for at in (0..source.tokens.len()).filter(|&at| !source.is_test_at(at)) {
            if let Some(reason) = find(&source.tokens, at) {
                locations.push(format!("{} {reason}", to_location(&source.path, source.tokens[at].line)));
            }
        }
    }
    locations
}

// Each `unsafe` and each lint level that lifts `unsafe_code`, outside the tests of every file but the kernels' own.
fn find_unsafe(target: &Crate, kernels: &str) -> Vec<String> {
    let mut locations = Vec::new();
    for source in target.sources.iter().filter(|source| !source.path.ends_with(kernels)) {
        let tokens = &source.tokens;
        for at in (0..tokens.len()).filter(|&at| !source.is_test_at(at)) {
            let is_lift = text(tokens, at) == "unsafe_code" && LIFTS.contains(&text(tokens, at.saturating_sub(2)));
            if text(tokens, at) == "unsafe" || is_lift {
                locations.push(format!("{} {}", to_location(&source.path, tokens[at].line), tokens[at].text));
            }
        }
    }
    locations
}

fn functions_where(target: &Crate, is_wrong: impl Fn(usize, &Function) -> bool) -> Vec<String> {
    (0..target.functions.len())
        .filter(|&index| is_wrong(index, target.function(index).2))
        .map(|index| {
            let (file, _, function) = target.function(index);
            let owner = function.owner.as_ref().map_or_else(String::new, |owner| format!("{owner}::"));
            format!("{} {owner}{}", to_location(&target.sources[file].path, function.line), function.name)
        })
        .collect()
}

// Of the kernel's tracked files, a line each, those that are neither a patch file directly in one of the two folders of patches nor its configuration.
fn find_kernel_strays(names: &str) -> Vec<&str> {
    let is_patch = |name: &str| {
        ["kernel/patches/", "kernel/module-patches/"].iter().any(|directory| {
            name.strip_prefix(directory).is_some_and(|file| file.ends_with(".patch") && !file.contains('/'))
        })
    };
    names.lines().filter(|name| !is_patch(name) && !["kernel/config", "kernel/config.changes"].contains(name)).collect()
}

// Each text outside the shell's tests that is one of the screen's words.
fn find_written_words(shell: &Crate, words: &HashSet<String>) -> Vec<String> {
    outside_tests(shell, |tokens, at| {
        let token = &tokens[at];
        (token.kind == Kind::Text && words.contains(&to_literal_text(&token.text))).then(|| token.text.clone())
    })
}

#[test]
fn kernel_changed_by_patches_alone() {
    let output = std::process::Command::new("git").arg("-C").arg(workspace()).args(["ls-files", "kernel"]).output();
    let names = String::from_utf8(output.expect("git lists what is tracked").stdout).unwrap();
    let others = find_kernel_strays(&names);
    assert!(
        others.is_empty(),
        "what is tracked of the kernel and is no patch file nor its configuration:\n{}",
        others.join("\n")
    );
}

#[test]
fn funcore_depends_on_nothing() {
    let funcore = funcore();
    let mut lines = to_dependency_lines("crates/funcore", &[]);
    let root = &funcore.sources[0].tokens;
    if !root.windows(5).any(|run| run.iter().map(|token| token.text.as_str()).eq(["#", "!", "[", "no_std", "]"])) {
        lines.push("crates/funcore/src/lib.rs: no #![no_std]".into());
    }
    lines.extend(outside_tests(funcore, |tokens, at| {
        (text(tokens, at) == "extern" && text(tokens, at + 1) == "crate" && text(tokens, at + 2) == "std")
            .then(|| "std".into())
    }));
    assert!(lines.is_empty(), "{}", lines.join("\n"));
}

#[test]
fn funcore_confines_unsafe() {
    let funcore = funcore();
    let root = &funcore.sources[0].tokens;
    assert!(
        root.windows(SAFE.len()).any(|run| run.iter().map(|token| token.text.as_str()).eq(SAFE)),
        "crates/funcore/src/lib.rs denies unsafe code outside its tests, the only way past `hw::Hardware` and the other traits to the machine"
    );
    assert!(funcore.sources.iter().any(|source| source.path.ends_with(KERNELS)), "the kernels' module is read");
    let locations = find_unsafe(funcore, KERNELS);
    assert!(locations.is_empty(), "unsafe code outside {KERNELS}:\n{}", locations.join("\n"));
}

#[test]
fn unsafe_found_outside_kernels() {
    let files = [
        ("main.rs", "#![cfg_attr(not(test), deny(unsafe_code))]\nmod base;\nmod codec;\n"),
        ("base/mod.rs", "mod simd;\n"),
        ("base/simd.rs", "#![allow(unsafe_code)]\nfn sum() -> u32 {\n    unsafe { body() }\n}\n"),
        (
            "codec.rs",
            "#[allow(unsafe_code)]\nfn filter() -> u32 {\n    unsafe { body() }\n}\n#[expect(unsafe_code)]\nfn quiet() {}\n#[cfg(test)]\nmod tests {\n    fn checked() -> u32 {\n        unsafe { body() }\n    }\n}\n",
        ),
    ];
    let fixture = write_crate("kernels", &files);
    let locations = find_unsafe(&fixture, KERNELS);
    let found: Vec<&str> = locations.iter().filter_map(|location| location.rsplit(' ').next()).collect();
    assert_eq!(
        found,
        ["unsafe_code", "unsafe", "unsafe_code"],
        "a lint level that lets `unsafe` in and an `unsafe` block outside the kernels' module; none in it, none for the crate's denial, none in a test"
    );
}

#[test]
fn funcore_keeps_no_state() {
    let locations = outside_tests(funcore(), find_state);
    assert!(locations.is_empty(), "state no caller hands in:\n{}", locations.join("\n"));
}

#[test]
fn layers_import_lower_ones() {
    let mut locations = find_upward_paths(funcore(), &CORE_LAYERS);
    locations.extend(find_upward_paths(impshell(), &SHELL_LAYERS));
    assert!(
        locations.is_empty(),
        "a module in no layer, or a path into a module of the same layer or a higher one:\n{}",
        locations.join("\n")
    );
}

#[test]
fn upward_path_found_by_every_form() {
    let files = [
        ("main.rs", "mod low;\nmod mid;\nmod high;\nmod stray;\n#[cfg(test)]\nmod support;\n"),
        ("low.rs", "use crate::mid::Thing;\npub(crate) struct Base;\n"),
        ("mid/mod.rs", "mod high;\nmod part;\nuse crate::low::Base;\nuse crate::{low, high::Top};\n"),
        ("mid/high.rs", "pub struct Kept;\n"),
        (
            "mid/part.rs",
            "use super::super::high;\nuse super::Own;\nfn told() {\n    crate::mid::tell();\n    crate::peer::tell();\n}\n#[cfg(test)]\nmod tests {\n    use super::super::high::Kept;\n    use super::super::super::high::Top;\n}\n",
        ),
        ("high.rs", "use crate::low::Base;\nuse crate::mid;\n"),
        ("stray.rs", "use crate::high;\n"),
        ("support.rs", "use crate::high;\n"),
    ];
    let locations = find_upward_paths(&write_crate("layers", &files), &[&["low"], &["mid", "peer"], &["high"]]);
    let names: Vec<&str> = locations.iter().filter_map(|location| location.rsplit(' ').next()).collect();
    assert_eq!(
        names,
        ["stray", "mid", "high", "high", "peer", "high"],
        "a module of the root no layer names; a higher module by a `use`, in a group, behind `super`s up to the root, in a module of tests; one of its own layer by a path in a body; none for a lower module, its own, a child named as a higher one or a module of tests"
    );
}

#[test]
fn image_readers_reach_no_os() {
    let mut lines = to_dependency_lines("crates/image", &["lz4_flex", "sha1"]);
    lines.extend(outside_tests(&image(), find_os));
    assert!(lines.is_empty(), "{}", lines.join("\n"));
}

#[test]
fn impshell_writes_no_screen_word() {
    let words = screen_words(funcore());
    assert!(words.contains("NO NETWORK") && words.contains("SAVED"), "the screen's words are known");
    let locations = find_written_words(impshell(), &words);
    assert!(locations.is_empty(), "words of the screen, which funcore alone makes:\n{}", locations.join("\n"));
}

#[test]
fn impshell_names_no_screen_word() {
    let funcore = funcore();
    let words = word_constants(funcore);
    let has_word = |file: &str, name: &str| {
        words.iter().any(|(at, word)| word == name && funcore.sources[*at].path.ends_with(file))
    };
    assert!(has_word("ui/words.rs", "SAVED") && has_word("ui/words.rs", "WAITING"), "the screen's words are known");
    let locations = find_named_words(impshell(), funcore, &words);
    assert!(
        locations.is_empty(),
        "words of the screen picked outside funcore, which picks them:\n{}",
        locations.join("\n")
    );
}

#[test]
fn screen_word_found_by_every_path() {
    let core = write_crate(
        "interface_words",
        &[
            ("main.rs", "mod ui;\n"),
            (
                "ui/words.rs",
                "pub const FULL: &str = \"STORAGE FULL\";\npub const NONE: &'static str = \"NO STORAGE\";\npub const COUNT: u8 = 1;\npub const NAME: &str = \"chat.md\";\n#[cfg(test)]\nmod tests {\n    const TESTED: &str = \"TESTED\";\n}\n",
            ),
            (
                "ui/mod.rs",
                "pub mod words;\npub use words::FULL as STORAGE_FULL;\npub const WAITING: &str = \"WAITING\";\n",
            ),
        ],
    );
    let words = word_constants(&core);
    let mut constants: Vec<&str> = words.iter().map(|(_, name)| name.as_str()).collect();
    constants.sort_unstable();
    assert_eq!(
        constants,
        ["FULL", "NONE", "WAITING"],
        "a constant of the interface that holds a text as the screen writes it, outside tests"
    );
    let written = [
        "fn none() -> &'static str {\n    \"NO STORAGE\"\n}",
        "fn lower() -> &'static str {\n    \"No storage\"\n}",
        "fn one() -> &'static str {\n    \"STORAGE\"\n}",
        "fn elsewhere() -> &'static str {\n    \"WAITING\"\n}",
        "#[cfg(test)]\nmod tests {\n    const NONE: &str = \"NO STORAGE\";\n}",
    ];
    let locations = find_written_words(&module_crate("written_words", &written), &screen_words(&core));
    let texts: Vec<&str> =
        locations.iter().filter_map(|location| location.split_once(' ')).map(|(_, text)| text).collect();
    assert_eq!(
        texts,
        ["\"NO STORAGE\""],
        "a text the screen's words hold, outside tests; none in other letters, none the words' file does not hold, none of one word the interface holds elsewhere"
    );
    let lines = [
        "use r1_funcore::ui::words::{self, FULL};",
        "use r1_funcore::ui::words::NONE as NOTHING;",
        "use r1_funcore::ui::words::{self as kept};",
        "use r1_funcore::ui::words as stored;",
        "use r1_funcore::ui::*;",
        "fn full() -> &'static str {\n    FULL\n}",
        "fn none() -> &'static str {\n    words::NONE\n}",
        "fn renamed() -> &'static str {\n    NOTHING\n}",
        "fn through() -> &'static str {\n    kept::FULL\n}",
        "fn kept_none() -> &'static str {\n    stored::NONE\n}",
        "fn globbed() -> &'static str {\n    WAITING\n}",
        "fn again() -> &'static str {\n    r1_funcore::ui::STORAGE_FULL\n}",
        "fn inner() -> &'static str {\n    use r1_funcore::ui::words::FULL as LOCAL;\n    LOCAL\n}",
        "fn kind(word: Option<&str>, count: u8) -> bool {\n    word.is_some() && count == r1_funcore::ui::words::COUNT\n}",
        "#[cfg(test)]\nmod tests {\n    use r1_funcore::ui::words::FULL;\n    fn full() -> bool {\n        FULL.is_empty()\n    }\n}",
    ];
    let locations = find_named_words(&module_crate("words", &lines), &core, &words);
    let names: Vec<&str> = locations.iter().filter_map(|location| location.rsplit(' ').next()).collect();
    assert_eq!(
        names,
        [
            "FULL",
            "NONE",
            "NOTHING",
            "FULL",
            "NONE",
            "NOTHING",
            "FULL",
            "NONE",
            "WAITING",
            "STORAGE_FULL",
            "FULL",
            "LOCAL",
            "LOCAL"
        ],
        "a word where a `use` takes it, by its name or another, where it is named so, through a module a `use` takes by any name or a glob, by a name the core gives it, and through a `use` in a function"
    );
}

#[test]
fn impshell_tests_are_grounded() {
    let impshell = impshell();
    let locations = functions_where(impshell, |index, function| function.is_test && !impshell.is_grounded(index));
    assert!(locations.is_empty(), "tests of what funcore is to hold:\n{}", locations.join("\n"));
}

#[test]
fn impshell_functions_reach_the_world() {
    let (impshell, funcore) = (impshell(), funcore());
    let traits: HashSet<String> =
        funcore.sources.iter().flat_map(|source| source.items.traits.iter().cloned()).collect();
    let locations = functions_where(impshell, |index, function| {
        !function.is_test_code && !impshell.reaches_world(index) && !impshell.is_plumbing(index, &traits)
    });
    assert!(locations.is_empty(), "decisions and formats funcore is to hold:\n{}", locations.join("\n"));
}

#[test]
fn tokens_skip_comments_keep_literals() {
    let source = "// a { comment\nfn one<'a>(x: &'a str) -> char { /* b } /* c */ */ let s = r#\"{\"\"#; 'x' }\nconst N: u8 = b'}';";
    let lexed = tokens(source);
    let texts: Vec<&str> = lexed.iter().map(|token| token.text.as_str()).collect();
    let expected = "fn one < 'a > ( x : & 'a str ) - > char { let s = r#\"{\"\"# ; 'x' } const N : u8 = b'}' ;";
    assert_eq!(texts, expected.split(' ').collect::<Vec<_>>());
    assert_eq!(lexed[0].line, 2, "the line a token begins on");
    let file = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn checks() {}\n}\nimpl From<u8> for Thing {\n    fn from(_: u8) -> Thing { Thing }\n}\n";
    let file_items = items(&tokens(file), false);
    // A function as the walk reads it: its name, its owner and trait, and whether it is a test and test code.
    type Row<'a> = (&'a str, Option<&'a str>, Option<&'a str>, bool, bool);
    let functions: Vec<Row> = file_items
        .functions
        .iter()
        .map(|function| {
            (
                function.name.as_str(),
                function.owner.as_deref(),
                function.trait_name.as_deref(),
                function.is_test,
                function.is_test_code,
            )
        })
        .collect();
    assert_eq!(
        functions,
        [("checks", None, None, true, true), ("from", Some("Thing"), Some("From"), false, false)],
        "a test in a module of tests, and a method of a trait's impl"
    );
    let pointer_items = items(
        &tokens(
            "type Op = fn(*const u8) -> u8;\nimpl Iterator for Holder {\n    type Item = (*const u8, u8);\n    fn next(&mut self) -> Option<Self::Item> {\n        None\n    }\n}\nfn after() -> u8 {\n    1\n}\n",
        ),
        false,
    );
    assert_eq!(
        (
            pointer_items.functions.iter().map(|function| function.name.as_str()).collect::<Vec<_>>(),
            pointer_items.aliases.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
            pointer_items.constants.len()
        ),
        (vec!["next", "after"], vec!["Op"], 0),
        "a raw pointer's mark is no constant, and an alias of a trait's impl is none of the file's own"
    );
    assert!(
        is_screen_word("NO NETWORK")
            && !is_screen_word("No network")
            && !is_screen_word("Y\\n")
            && !is_screen_word("0x0200")
    );
}

#[test]
fn rules_find_their_targets() {
    let apply = |source: &str, rule: fn(&[Token], usize) -> Option<String>| {
        let lexed = tokens(source);
        (0..lexed.len()).filter_map(|at| rule(&lexed, at)).collect::<Vec<String>>()
    };
    assert_eq!(
        apply(
            "static NEXT: AtomicU64 = AtomicU64::new(1); static mut M: u8 = 0; static HELD: Mutex<Vec<u8>> = Mutex::new(Vec::new()); thread_local! { static C: Cell<u8> = Cell::new(0); }",
            find_state
        ),
        ["static NEXT", "static mut", "static HELD", "thread_local", "static C"]
    );
    assert!(
        apply(
            "static TABLE: [u8; 3] = [1, 2, 3]; const ONE: Cell<u8> = Cell::new(1); fn f(text: &'static str) {}",
            find_state
        )
        .is_empty(),
        "a table that never changes, a constant, a lifetime"
    );
    assert_eq!(apply("fs::read(path); log!(\"x\"); println!(\"y\"); let log = 1;", find_os), ["fs", "log", "println"]);
    assert_eq!(
        apply("fn f(at: Instant) -> Duration { Instant::now() - at } fn g(at: Instant) { at.elapsed(); }", find_os),
        ["Instant", "elapsed"],
        "a time handed on is none, the clock read is"
    );
    assert!(
        is_screen_word("NO NETWORK") && is_screen_word("SAVED") && !is_screen_word("chat.md") && !is_screen_word("A"),
        "as the screen writes"
    );
    let manifest = "[package]\nname = \"x\"\nbuild = \"make.rs\"\n\n[dependencies]\nserde = \"1\"\nkept = { version = \"1\" }\n\n[dependencies.toml]\nversion = \"1\"\n\n[dependencies.\"held\"]\nversion = \"1\"\n\n[target.'cfg(unix)'.dependencies]\nlibc = \"0.2\"\n\n[target.'cfg(unix)'.build-dependencies.cc]\nversion = \"1\"\n\n[build-dependencies]\ncc = \"1\"\n\n[dev-dependencies]\npng = \"0.18\"\n\n[dev-dependencies.rand]\nversion = \"1\"\n\n[features]\nextras = []\n";
    assert_eq!(
        find_dependencies(manifest, &["kept", "held"]),
        [
            "[package] build = \"make.rs\"",
            "[dependencies] serde = \"1\"",
            "[dependencies.toml]",
            "[target.'cfg(unix)'.dependencies] libc = \"0.2\"",
            "[target.'cfg(unix)'.build-dependencies.cc]",
            "[build-dependencies] cc = \"1\""
        ],
        "a build script, and a dependency by a key of its table or by a table of its own, for a target too; none for one allowed, for the tests' or for a feature"
    );
    let file = "#[cfg(not(test))]\nfn shipped() {}\n#[cfg(any(test, feature = \"x\"))]\nfn both() {}\n#[cfg(all(unix, test))]\nfn tested() {}\n#[cfg_attr(test, allow(dead_code))]\nfn kept() {}\n";
    let checked: Vec<(String, bool)> = items(&tokens(file), false)
        .functions
        .into_iter()
        .map(|function| (function.name, function.is_test_code))
        .collect();
    assert_eq!(
        checked,
        [("shipped", false), ("both", false), ("tested", true), ("kept", false)]
            .map(|(name, is_test)| (name.to_string(), is_test)),
        "code is the tests' own only where no other build holds it"
    );
    let file = "#[cfg(test)]\nimpl Gauge {\n    fn sample(&self) -> u8 {\n        1\n    }\n}\n#[cfg(test)]\nstatic SEEN: AtomicU8 = AtomicU8::new(0);\n#[cfg(test)]\nuse crate::words::SAVED;\nstatic KEPT: AtomicU8 = AtomicU8::new(0);\nfn shipped() {}\n";
    let lexed = tokens(file);
    let source = Source { path: PathBuf::new(), items: items(&lexed, false), tokens: lexed, is_test: false };
    let is_test_code = |name: &str| {
        let at = source.tokens.iter().position(|token| token.text == name);
        at.is_some_and(|at| source.is_test_at(at))
    };
    assert_eq!(
        ["sample", "SEEN", "SAVED", "KEPT", "shipped"].map(is_test_code),
        [true, true, true, false, false],
        "an impl, a static and a `use` of the tests' build alone are test code, as a function and a module of it are"
    );
    assert_eq!(
        find_kernel_strays(
            "kernel/config\nkernel/config.changes\nkernel/patches/0001-a.patch\nkernel/module-patches/0001-b.patch\nkernel/patches/deep/0002-c.patch\nkernel/patches/notes.txt\nkernel/src/main.c\n"
        ),
        ["kernel/patches/deep/0002-c.patch", "kernel/patches/notes.txt", "kernel/src/main.c"],
        "a patch below a folder of patches, a file there that is no patch, and a source; none for the configuration and the patches"
    );
}

#[test]
fn names_resolve_per_file() {
    let files = [
        ("main.rs", "mod held;\nmod opcodes;\nmod other;\nmod usb;\n"),
        (
            "held.rs",
            "pub struct Handle {\n    file: File,\n}\npub const READ: u32 = ioc(1);\nfn request() -> u32 {\n    READ\n}\n",
        ),
        ("usb/mod.rs", "pub use crate::held::Handle;\n"),
        (
            "opcodes.rs",
            "use crate::usb::Handle;\nconst READ: u32 = 15;\nfn answer(code: u32) -> bool {\n    code == READ\n}\nimpl Handle {\n    fn opened(&self) -> bool {\n        true\n    }\n}\n",
        ),
        (
            "other.rs",
            "struct Handle;\nimpl Handle {\n    fn counted(&self) -> u32 {\n        if true { 1 } else { 2 }\n    }\n}\n",
        ),
    ];
    let fixture = write_crate("boundaries", &files);
    assert!(
        fixture.reach.layout[fixture.index("request")] && !fixture.reach.layout[fixture.index("answer")],
        "each file's READ is its own"
    );
    assert!(fixture.plumbs("opened"), "a type a module takes from another holds a file there");
    assert!(!fixture.plumbs("counted"), "a type of that name elsewhere holds none");
    let held = "use std::fs::File;\npub struct State {\n    file: File,\n}\nimpl State {\n    pub fn ready(&self) -> bool {\n        self.file.metadata().is_ok()\n    }\n}\n";
    let counted = "pub struct State {\n    count: u32,\n}\nimpl State {\n    pub fn ready(&self) -> bool {\n        self.count > 0\n    }\n}\n";
    let files = [
        ("main.rs", "mod a;\nmod b;\nmod c;\n"),
        ("a/mod.rs", "pub mod state;\n"),
        ("a/state.rs", held),
        ("b/mod.rs", "pub mod state;\n"),
        ("b/state.rs", counted),
        (
            "c.rs",
            "use crate::b::state::State;\nfn told(state: &State) -> u8 {\n    if state.ready() { 1 } else { 0 }\n}\nfn asked(state: &crate::a::state::State) -> u8 {\n    if state.ready() { 1 } else { 0 }\n}\n",
        ),
    ];
    let fixture = write_crate("leaves", &files);
    assert!(
        !fixture.reaches_os("told") && fixture.reaches_os("asked"),
        "of two modules of one name, a path leads to the one it names from the crate's root"
    );
}

#[test]
fn handles_and_library_traits_are_plumbing() {
    let lines = [
        "use aws_lc_rs::hkdf;",
        "use aws_lc_sys as lc;",
        "fn hashed(bytes: &[u8]) -> u8 {\n    if bytes.is_empty() { 0 } else { lc::hash(bytes) }\n}",
        "use r1_funcore::fs::exfat::Volume;",
        "use std::sync::Arc;",
        "use std::sync::mpsc::Receiver;",
        "struct Ending {\n    telling: Receiver<u32>,\n}",
        "impl Ending {\n    fn finished(&self) -> bool {\n        if self.telling.try_recv().is_ok() { true } else { false }\n    }\n}",
        "struct Job {\n    part: *const u8,\n}",
        "struct Board {\n    job: Option<Job>,\n}",
        "impl Board {\n    fn take(&mut self) -> bool {\n        if self.job.is_some() { true } else { false }\n    }\n}",
        "struct Bytes(usize);",
        "impl hkdf::KeyType for Bytes {\n    fn len(&self) -> usize {\n        if true { self.0 } else { 0 }\n    }\n}",
        "struct Kept {\n    volume: Volume,\n}",
        "impl Kept {\n    fn counted(&self) -> u32 {\n        if true { 1 } else { 2 }\n    }\n}",
        "struct Owner(Arc<u64>);",
        "impl Owner {\n    fn address(&self) -> u64 {\n        if true { Arc::as_ptr(&self.0) as u64 } else { 0 }\n    }\n}",
    ];
    let fixture = module_crate("handles", &lines);
    assert!(fixture.plumbs("finished"), "a channel's end that a `use` takes from the OS's module");
    assert!(fixture.plumbs("take"), "a type that holds one that holds a raw pointer");
    assert!(fixture.plumbs("len"), "a method of a trait of a library's");
    assert!(fixture.plumbs("hashed"), "a call into a library's crate by the name a `use` gives it");
    assert!(fixture.plumbs("address"), "the place of a value in memory, which only the kernel is told");
    assert!(!fixture.plumbs("counted"), "a module of another crate's that is named as the OS's is none");
}

#[test]
fn handle_streams_reach_the_world() {
    let lines = [
        "use std::io::{self, Read};",
        "use std::net::TcpStream;",
        "struct Line {\n    tcp: TcpStream,\n}",
        "impl Read for Line {\n    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {\n        match self {\n            Line { tcp } => tcp.read(into),\n        }\n    }\n}",
        "impl Line {\n    fn write(&mut self, bytes: &[u8]) -> usize {\n        bytes.len()\n    }\n}",
        "struct Bytes(Vec<u8>);",
        "impl Read for Bytes {\n    fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {\n        Ok(into.len().min(self.0.len()))\n    }\n}",
        "fn heard(line: &mut Line) -> usize {\n    let mut length = [0u8; 2];\n    if line.read_exact(&mut length).is_ok() { 2 } else { 0 }\n}",
        "fn taken(line: &mut Line) -> usize {\n    let mut length = [0u8; 2];\n    if line.read(&mut length).is_ok() { 2 } else { 0 }\n}",
        "fn told(line: &mut Line) -> usize {\n    if true { line.write(&[1]) } else { 0 }\n}",
        "fn copied(bytes: &mut Bytes) -> usize {\n    let mut length = [0u8; 2];\n    if bytes.read_exact(&mut length).is_ok() { 2 } else { 0 }\n}",
    ];
    let fixture = module_crate("streams", &lines);
    assert!(fixture.reaches_os("heard"), "the language's reading asked of a type that holds a socket");
    assert!(fixture.reaches_os("taken"), "and the type's own, which it writes for the language");
    assert!(
        !fixture.reaches_os("told"),
        "a method of the type's own by a name of the language's is followed, and reaches nothing"
    );
    assert!(!fixture.reaches_os("copied"), "reading memory reaches nothing");
}

#[test]
fn generics_aliases_statics_resolve() {
    let lines = [
        "use std::cell::Cell;",
        "use std::fs;",
        "use std::sync::Mutex;",
        "use std::sync::mpsc::Sender;",
        "thread_local! {\n    static ASKING: Cell<bool> = const { Cell::new(false) };\n}",
        "struct Held<T> {\n    inner: Mutex<T>,\n}",
        "impl<T> Held<T> {\n    fn plain() -> Held<T> {\n        unimplemented!()\n    }\n    fn fresh() -> bool {\n        fs::metadata(\"x\").is_ok()\n    }\n    fn count(&self) -> usize {\n        self.inner.lock().map_or(0, |_| 1)\n    }\n}",
        "type Waiting = Vec<Sender<u8>>;",
        "fn asks(asks: bool) {\n    ASKING.set(asks);\n}",
        "fn counted() -> usize {\n    let held = Held::<u8>::plain();\n    held.count()\n}",
        "fn made() -> bool {\n    Held::<u8>::fresh()\n}",
        "fn refuse(waiting: &mut Waiting) {\n    waiting.drain(..).for_each(|answer| drop(answer.send(1)));\n}",
        "fn look() -> bool {\n    fs::metadata(\"y\").is_ok()\n}",
        "fn sought(look: impl Fn() -> bool) -> bool {\n    if look() { true } else { false }\n}",
        "fn looked() -> bool {\n    let look = look();\n    if look { true } else { false }\n}",
        "fn relooked() -> bool {\n    let look = || true;\n    if look() { true } else { false }\n}",
    ];
    let fixture = module_crate("names", &lines);
    assert!(fixture.reaches_os("asks"), "a static of a thread's own is the thread's state");
    assert!(fixture.reaches_os("counted"), "a value a path with generic arguments makes is of the type it names");
    assert!(fixture.reaches_os("made"), "and a call by such a path is followed");
    assert!(fixture.reaches_os("refuse"), "an alias stands for the type it names");
    assert!(!fixture.reaches_os("sought"), "a call of a parameter is no call of a function of its name");
    assert!(fixture.reaches_os("looked"), "a `let` binds its name only once its statement is done");
    assert!(!fixture.reaches_os("relooked"), "and from then on");
}

#[test]
fn forwarding_to_core_is_plumbing() {
    let lines = [
        "use r1_funcore::hw::power::doze::Doze;",
        "struct Sleep {\n    doze: Doze,\n    count: u32,\n}",
        "impl Sleep {\n    fn button(&mut self, down: bool) -> bool {\n        self.doze.button(down)\n    }\n    fn due(&self) -> bool {\n        self.doze.due()\n    }\n    fn later(&mut self, down: bool) -> bool {\n        self.doze.button(!down)\n    }\n    fn counted(&self) -> u32 {\n        self.count.max(1)\n    }\n}",
    ];
    let fixture = module_crate("forwards", &lines);
    assert!(fixture.plumbs("button") && fixture.plumbs("due"), "the core's decision handed on");
    assert!(!fixture.plumbs("later"), "what is handed on is changed on the way");
    assert!(!fixture.plumbs("counted"), "a field that is none of the core's");
}

#[test]
fn self_import_is_no_bare_name() {
    let lines = [
        "use r1_funcore::domain::news::editor::{self, State};",
        "use r1_funcore::domain::news::feed::told;",
        "use std::fs;",
        "fn editor() -> bool {\n    fs::metadata(\"x\").is_ok()\n}",
        "fn edited() -> bool {\n    editor()\n}",
        "fn kept() -> editor::News {\n    editor::News::default()\n}",
        "fn heard() -> bool {\n    told()\n}",
        "use r1_funcore::openai::key;",
        "fn key() -> bool {\n    fs::metadata(\"y\").is_ok()\n}",
        "fn keyed() -> bool {\n    key()\n}",
    ];
    let fixture = module_crate("modules", &lines);
    assert!(fixture.reaches_os("edited"), "a call by the name of a module taken by `self` is of the file's function");
    assert!(!fixture.reaches_os("kept"), "and a path through that module leads to it, outside the crate");
    assert!(!fixture.reaches_os("heard"), "a name another crate's path takes is that crate's");
    assert!(
        fixture.reaches_os("keyed"),
        "a call by the name of a function of the file's own is of it, whatever module a `use` takes by that name"
    );
}

#[test]
fn handle_branch_on_plain_field_decides() {
    let lines = [
        "use std::net::TcpStream;",
        "struct Socket(TcpStream);",
        "struct Wire {\n    socket: Socket,\n    day: u32,\n}",
        "struct Link {\n    wire: Wire,\n    count: u32,\n}",
        "enum Line {\n    Open(TcpStream),\n    Shut,\n}",
        "impl Wire {\n    fn day(&self) -> u32 {\n        self.day\n    }\n    fn late(&self) -> bool {\n        if self.day > 3 { true } else { false }\n    }\n    fn socket(&self) -> Option<&Socket> {\n        if self.day > 3 { Some(&self.socket) } else { None }\n    }\n}",
        "impl Link {\n    fn counted(&self) -> u32 {\n        if self.count > 3 { 1 } else { 2 }\n    }\n    fn shallow(&self) -> bool {\n        if self.wire.day > 3 { true } else { false }\n    }\n    fn deep(&self) -> Option<&Socket> {\n        if self.count > 3 { Some(&self.wire.socket) } else { None }\n    }\n    fn first(&self) -> Option<&Socket> {\n        Some(&self.wire.socket)\n    }\n    fn through(&self) -> Option<&Socket> {\n        if self.count > 3 { self.first() } else { None }\n    }\n}",
        "impl Line {\n    fn open(&self) -> bool {\n        match self {\n            Line::Open(_) => true,\n            Line::Shut => false,\n        }\n    }\n}",
    ];
    let fixture = module_crate("held", &lines);
    assert!(fixture.passes("day"), "a method of a handle that decides nothing");
    assert!(
        !fixture.passes("late") && !fixture.passes("counted"),
        "one that branches beside plain fields of its own alone"
    );
    assert!(!fixture.passes("shallow"), "a plain field of a field that holds the world is plain");
    assert!(
        fixture.passes("socket") && fixture.passes("deep"),
        "one that branches beside a field of its own that holds the world, by the fields that lead to it"
    );
    assert!(fixture.passes("through"), "or beside a method of its own that touches that world");
    assert!(fixture.passes("open"), "or that takes apart a value of its own that holds the world");
}

#[test]
fn branch_beside_lock_decides() {
    let lines = [
        "use std::fs::{File, read_to_string};",
        "use std::path::Path;",
        "use std::sync::Mutex;",
        "use std::sync::atomic::{AtomicBool, Ordering};",
        "struct Gauge {\n    level: Mutex<u32>,\n}",
        "impl Gauge {\n    fn low(&self) -> bool {\n        if *self.level.lock().unwrap() < 10 { true } else { false }\n    }\n}",
        "struct Opened {\n    file: File,\n}",
        "fn flagged(on: &AtomicBool) -> u32 {\n    if on.load(Ordering::Relaxed) { 1 } else { 2 }\n}",
        "fn named(path: &Path) -> bool {\n    if path.ends_with(\"x\") { true } else { false }\n}",
        "fn told(level: &Mutex<u32>) {\n    if *level.lock().unwrap() > 3 {\n        log!(\"high\");\n    }\n}",
        "fn free(level: &Mutex<u32>) -> bool {\n    if level.try_lock().is_ok() { true } else { false }\n}",
        "fn there(path: &Path) -> bool {\n    if path.symlink_metadata().is_ok() { true } else { false }\n}",
        "fn fetched(path: &Path) -> bool {\n    if read_to_string(path).is_ok() { true } else { false }\n}",
        "fn release(level: &Mutex<u32>, held: &mut Option<Opened>) {\n    if *level.lock().unwrap() == 0 {\n        *held = None;\n    }\n}",
        "fn counted(level: &Mutex<u32>) -> u32 {\n    *level.lock().unwrap() + 1\n}",
        "#[test]\nfn locked() {\n    let level = Mutex::new(1);\n    assert_eq!(*level.lock().unwrap(), 1);\n}",
        "#[test]\nfn mounted() {\n    assert_eq!(Path::new(\"/sys/class/udc\").file_name().unwrap(), \"udc\");\n}",
    ];
    let fixture = module_crate("incidental", &lines);
    assert!(!fixture.passes("low"), "a method of a type that holds a lock alone, branching on what the lock holds");
    assert!(!fixture.passes("flagged") && !fixture.passes("named"), "a branch beside an atomic or a path alone");
    assert!(fixture.passes("told"), "a line of the log is the world's");
    assert!(
        fixture.passes("free") && fixture.passes("there"),
        "whether another thread holds a lock, and what the filesystem holds at a path, are the world's to tell"
    );
    assert!(fixture.passes("fetched"), "a name a `use` takes from a module of the OS's");
    assert!(fixture.passes("release"), "a handle it was handed let go of");
    assert!(fixture.passes("counted"), "beside a lock alone, what branches nowhere");
    assert!(
        !fixture.is_grounded(fixture.index("locked")) && fixture.is_grounded(fixture.index("mounted")),
        "a test of a lock alone tests a value, one of a path of the kernel's its layout"
    );
}

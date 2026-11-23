//! `r1 visibility`: every item of the workspace that reaches further than what uses it. In a copy of the workspace every item is written a step narrower than it is, the compiler checks the copy in each of `BUILDS`, and each item it asks for is put back as written, round by round, until it asks for nothing; what then stays narrower is reported, and so is what nothing but tests uses.

mod messages;
mod source;
mod syntax;

use crate::{Result, workspace_root};
use messages::{Message, Span, quoted};
use source::{Item, Kind, Vis, type_name, use_paths};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;
use syntax::module_path;

// Where under the workspace the copy lies, and where the compiler keeps what it makes of it from one run to the next.
const COPY: &str = "target/visibility/workspace";
const BUILT: &str = "target/visibility/target";
// What of the workspace the copy holds: everything a check of its crates reads.
const COPIED: [&str; 9] =
    [".cargo", "crates", "docs", "scripts", "tools", "udev", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml"];
// The builds checked, as `cargo check` takes them: every crate with its tests, then impshell without its debug shell and the image crate without `boot`, each of which compiles what the first leaves out.
const BUILDS: [&[&str]; 3] = [
    &["--workspace", "--all-targets"],
    &["-p", "r1-impshell", "--all-targets", "--no-default-features"],
    &["-p", "r1-image", "--all-targets", "--no-default-features"],
];
// The methods of the standard library's traits and of its collections that a method of a type's own, of the same name, could silently give way to, through a trait or a deref, where it reached less than its caller: such a method keeps how far it reaches.
const STANDARD_METHODS: [&str; 136] = [
    "from",
    "into",
    "try_from",
    "try_into",
    "next",
    "next_back",
    "default",
    "clone",
    "clone_from",
    "eq",
    "ne",
    "cmp",
    "partial_cmp",
    "lt",
    "le",
    "gt",
    "ge",
    "max",
    "min",
    "clamp",
    "fmt",
    "hash",
    "drop",
    "deref",
    "deref_mut",
    "as_ref",
    "as_mut",
    "borrow",
    "borrow_mut",
    "write",
    "write_all",
    "write_str",
    "write_char",
    "write_fmt",
    "flush",
    "read",
    "read_exact",
    "read_to_end",
    "read_to_string",
    "seek",
    "from_str",
    "to_string",
    "to_owned",
    "add",
    "sub",
    "mul",
    "div",
    "rem",
    "neg",
    "not",
    "shl",
    "shr",
    "bitand",
    "bitor",
    "bitxor",
    "add_assign",
    "sub_assign",
    "mul_assign",
    "div_assign",
    "index",
    "index_mut",
    "from_iter",
    "extend",
    "into_iter",
    "sum",
    "product",
    "len",
    "size_hint",
    "count",
    "last",
    "nth",
    "step_by",
    "chain",
    "zip",
    "map",
    "filter",
    "filter_map",
    "enumerate",
    "peekable",
    "skip",
    "take",
    "fold",
    "all",
    "any",
    "find",
    "position",
    "rev",
    "collect",
    "partition",
    "flat_map",
    "flatten",
    "inspect",
    "by_ref",
    "copied",
    "cloned",
    "cycle",
    "unzip",
    "min_by",
    "max_by",
    "min_by_key",
    "max_by_key",
    "for_each",
    "try_fold",
    "scan",
    "skip_while",
    "take_while",
    "map_while",
    "fuse",
    "reduce",
    "find_map",
    "rposition",
    "is_sorted",
    "error",
    "source",
    "description",
    "call",
    "call_mut",
    "call_once",
    "poll",
    "type_id",
    "nth_back",
    "rfold",
    "rfind",
    "try_for_each",
    "is_empty",
    "to_vec",
    "get",
    "get_mut",
    "iter",
    "iter_mut",
    "contains",
    "push",
    "pop",
    "insert",
    "remove",
    "clear",
];
// The codes of errors that a path no longer reaches what it names: through a glob, or by its name in a module.
const NOT_FOUND: [&str; 7] = ["E0425", "E0412", "E0422", "E0423", "E0433", "E0531", "E0532"];

// An item the check works on: the file it is in, what it is, its line, and whether its crate is a program, whose `pub` reaches no further than `pub(crate)`.
struct Subject {
    file: usize,
    item: Item,
    line: usize,
    program: bool,
}

impl Subject {
    // How deep the module is that the item reaches to with `vis`, from the crate's root; less than none beyond the crate.
    fn reach(&self, vis: Vis) -> isize {
        let depth = self.item.module.len() as isize;
        match vis {
            Vis::Pub if !self.program => -1,
            Vis::Pub | Vis::Crate => 0,
            Vis::Super => (depth - 1).max(0),
            Vis::Private => depth,
        }
    }

    // The widest visibility that reaches less far than the item is written to reach; none where none does.
    fn narrower(&self) -> Option<Vis> {
        let written = self.reach(self.item.vis);
        Vis::LADDER.into_iter().rev().find(|step| self.reach(*step) > written)
    }

    // The item as a report names it: its kind, and its name after its type's where it has one.
    fn named(&self) -> String {
        let kind = match self.item.kind {
            Kind::Fn => "fn",
            Kind::Struct => "struct",
            Kind::Enum => "enum",
            Kind::Union => "union",
            Kind::Trait => "trait",
            Kind::Type => "type",
            Kind::Const => "const",
            Kind::Static => "static",
            Kind::Mod => "mod",
            Kind::Use => "use",
            Kind::Field | Kind::TupleField => "field",
            Kind::Method => "method",
            Kind::AssocConst => "associated const",
            Kind::AssocType => "associated type",
        };
        match &self.item.owner {
            Some(owner) => format!("{kind} {owner}::{}", self.item.name),
            None => format!("{kind} {}", self.item.name),
        }
    }
}

// A file that holds subjects: its path from the workspace's root, as the compiler names it, and its text.
struct Source {
    path: String,
    text: String,
}

// What the compiler asked of a round: items to put back as written, items nothing but tests uses, or what no narrower item answers.
enum Flag {
    Restore(Vec<usize>),
    Unused(Vec<usize>),
    Unmapped,
    Nothing,
}

// The crate a file of the workspace belongs to, by its directory under `crates/`.
fn crate_of(file: &str) -> Option<&str> {
    file.strip_prefix("crates/")?.split('/').next()
}

// The module a file of a crate's sources makes.
fn module_at(file: &str) -> Option<Vec<String>> {
    Some(module_path(file.strip_prefix("crates/")?.split_once('/')?.1.strip_prefix("src/")?))
}

// True for a `use` whose tree takes one of `names`, or a glob.
fn takes(subject: &Subject, names: &[&str]) -> bool {
    subject.item.kind == Kind::Use
        && use_paths(&subject.item.name)
            .iter()
            .any(|path| path.last().is_some_and(|last| last == "*" || names.contains(&last.as_str())))
}

// The subjects as a round has written them: which are narrower than written, each file's text, and in it where each subject's token begins.
struct Placed<'a> {
    subjects: &'a [Subject],
    sources: &'a [Source],
    narrowed: &'a [Option<Vis>],
    texts: HashMap<String, String>,
    tokens: HashMap<String, Vec<(usize, usize)>>,
}

impl Placed<'_> {
    // The subject a place of a definition names: the item whose token begins there, or the `use` it lies in.
    fn defined(&self, span: &Span) -> Option<usize> {
        let tokens = self.tokens.get(&span.file)?;
        let (offset, id) = tokens[tokens.partition_point(|(offset, _)| *offset <= span.start).checked_sub(1)?];
        if offset == span.start {
            return Some(id);
        }
        let text = self.texts.get(&span.file)?;
        let inside = text[offset..].find(';').is_some_and(|end| span.start <= offset + end);
        (self.subjects[id].item.kind == Kind::Use && inside).then_some(id)
    }

    // The subject whose token stands on the line a place is on, before it: what a warning of an unused item names.
    fn on_line(&self, span: &Span) -> Option<usize> {
        let tokens = self.tokens.get(&span.file)?;
        let (offset, id) = tokens[tokens.partition_point(|(offset, _)| *offset <= span.start).checked_sub(1)?];
        let text = self.texts.get(&span.file)?;
        (!text[offset..span.start].contains('\n')).then_some(id)
    }

    // The crate a subject belongs to.
    fn crate_of(&self, subject: &Subject) -> Option<&str> {
        crate_of(&self.sources[subject.file].path)
    }

    // The subjects a `use` takes by `name`, by the paths of its tree read from its module.
    fn reexported(&self, use_id: usize, name: &str) -> Vec<usize> {
        let taker = &self.subjects[use_id];
        use_paths(&taker.item.name)
            .into_iter()
            .filter(|path| path.last().is_some_and(|last| last == name || last == "*"))
            .flat_map(|path| {
                let (mut base, mut rest) = (taker.item.module.clone(), path.as_slice());
                match rest.first().map(String::as_str) {
                    Some("crate") => (base, rest) = (Vec::new(), &rest[1..]),
                    Some("self") => rest = &rest[1..],
                    _ => {}
                }
                while rest.first().is_some_and(|part| part == "super") {
                    base.pop();
                    rest = &rest[1..];
                }
                base.extend(rest[..rest.len().saturating_sub(1)].iter().cloned());
                let found = self.subjects.iter().enumerate().filter(|(_, subject)| {
                    subject.item.kind.in_module()
                        && subject.item.name == name
                        && subject.item.module == base
                        && self.crate_of(subject) == self.crate_of(taker)
                });
                found.map(|(id, _)| id).collect::<Vec<usize>>()
            })
            .collect()
    }

    // The `use` items that take `name`, or a glob, in a module whose parent holds the module of `file`: a glob over such a module no longer brings the name where the `use` reaches less.
    fn globbed(&self, name: &str, file: &str) -> Vec<usize> {
        let (Some(crate_name), Some(module)) = (crate_of(file), module_at(file)) else {
            return Vec::new();
        };
        let found = self.subjects.iter().enumerate().filter(|(_, subject)| {
            let parent = &subject.item.module[..subject.item.module.len().saturating_sub(1)];
            module.starts_with(parent) && self.crate_of(subject) == Some(crate_name) && takes(subject, &[name])
        });
        found.map(|(id, _)| id).collect()
    }

    // Every subject of a name a message quotes, and every `use` that takes such a name or a glob: what the message may come of where none it names is narrower than written.
    fn by_name(&self, quoted: &[&str]) -> Vec<usize> {
        let names: Vec<String> = quoted.iter().map(|name| type_name(name)).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let found = self
            .subjects
            .iter()
            .enumerate()
            .filter(|(_, subject)| names.contains(&subject.item.name.as_str()) || takes(subject, &names));
        found.map(|(id, _)| id).collect()
    }

    // What a message asks of the subjects: those it names, or where none of them is narrower than written, those of the names it quotes.
    fn flagged(&self, message: &Message) -> Flag {
        let (level, code) = (message.level.as_str(), message.code.as_deref().unwrap_or(""));
        let names = quoted(&message.text);
        let last_part = |name: &str| name.rsplit("::").next().unwrap_or(name).to_string();
        let definitions = || {
            let secondary = message.spans.iter().filter(|span| !span.primary);
            secondary.chain(message.children.iter().flat_map(|(_, spans)| spans)).filter_map(|span| self.defined(span))
        };
        // A note that what is named exists and cannot be reached names its definition, whatever the error.
        let telling = |words: &String| ["inaccessible", "not visible", "private"].iter().any(|key| words.contains(key));
        let unreachable: Vec<usize> = message
            .children
            .iter()
            .filter(|(words, _)| telling(words))
            .flat_map(|(_, spans)| spans)
            .filter_map(|span| self.defined(span))
            .collect();
        let missing = missing_in(&message.rendered);
        let named: Vec<usize> = match (level, code) {
            ("error", _) if !unreachable.is_empty() => unreachable,
            ("error", "E0616" | "E0451") => match names.split_last() {
                Some((owner, fields)) => {
                    let owner = type_name(owner);
                    let found = self.subjects.iter().enumerate().filter(|(_, subject)| {
                        matches!(subject.item.kind, Kind::Field | Kind::TupleField)
                            && fields.contains(&subject.item.name.as_str())
                            && subject.item.owner.as_deref() == Some(owner.as_str())
                    });
                    found.map(|(id, _)| id).collect()
                }
                None => Vec::new(),
            },
            ("error", "E0364" | "E0365") => {
                let name = names.first().map(|name| last_part(name)).unwrap_or_default();
                let all = message.spans.iter().chain(message.children.iter().flat_map(|(_, spans)| spans));
                let uses: HashSet<usize> = all
                    .filter_map(|span| self.defined(span))
                    .filter(|id| self.subjects[*id].item.kind == Kind::Use)
                    .collect();
                uses.into_iter().flat_map(|use_id| self.reexported(use_id, &name)).collect()
            }
            ("error", "E0603" | "E0624") => {
                let mut ids: Vec<usize> = definitions().collect();
                if message.text.contains("constructor")
                    && let Some(owner) = names.first()
                {
                    let owner = last_part(owner);
                    let fields = self.subjects.iter().enumerate().filter(|(_, subject)| {
                        subject.item.kind == Kind::TupleField && subject.item.owner.as_deref() == Some(owner.as_str())
                    });
                    ids.extend(fields.map(|(id, _)| id));
                }
                ids
            }
            ("error", _) if missing.is_some() => {
                let (name, path) = missing.unwrap_or_default();
                let module: Vec<String> = path
                    .trim_start_matches("crate::")
                    .split("::")
                    .filter(|part| !part.is_empty() && *part != "crate")
                    .map(String::from)
                    .collect();
                let found = self.subjects.iter().enumerate().filter(|(_, subject)| {
                    let item = &subject.item;
                    (item.module == module && takes(subject, &[name]))
                        || (item.module.starts_with(&module) && item.name == name)
                });
                found.map(|(id, _)| id).collect()
            }
            ("error", code) if NOT_FOUND.contains(&code) => {
                let file =
                    message.spans.iter().find(|span| span.primary).map(|span| span.file.clone()).unwrap_or_default();
                let name = names.first().map(|name| last_part(name)).unwrap_or_default();
                self.globbed(&name, &file)
            }
            ("warning", "private_interfaces" | "private_bounds") => {
                let telling = message.children.iter().filter(|(words, _)| words.contains("only usable at"));
                telling.flat_map(|(_, spans)| spans).filter_map(|span| self.defined(span)).collect()
            }
            ("warning", "dead_code" | "unused_imports") => {
                let primary = message.spans.iter().filter(|span| span.primary);
                return Flag::Unused(primary.filter_map(|span| self.on_line(span)).collect());
            }
            ("error", _) => Vec::new(),
            _ => return Flag::Nothing,
        };
        let narrowed = |ids: &[usize]| ids.iter().any(|id| self.narrowed[*id].is_some());
        let asked = if narrowed(&named) { named } else { self.by_name(&names) };
        if narrowed(&asked) { Flag::Restore(asked) } else { Flag::Unmapped }
    }
}

// The name and the module path of a message that a path names nothing: "no `X` in `P`", "could not find `X` in `P`".
fn missing_in(rendered: &str) -> Option<(&str, &str)> {
    ["no `", "could not find `"].into_iter().find_map(|lead| {
        let (_, after) = rendered.split_once(lead)?;
        let (name, rest) = after.split_once('`')?;
        let path = match rest.strip_prefix(" in `") {
            Some(inside) => inside.split_once('`')?.0,
            None if rest.starts_with(" in the root") => "",
            None => return None,
        };
        Some((name, path))
    })
}

// Every source of the crates under `crates/` and the subjects in them: items with a visibility token but what is compiled on some platforms alone, a method of a trait's name, and what such code names.
fn read(root: &Path) -> Result<(Vec<Source>, Vec<Subject>)> {
    let mut crates = Vec::new();
    let mut guarded: HashSet<String> = STANDARD_METHODS.iter().map(|name| name.to_string()).collect();
    let mut directories: Vec<_> = fs::read_dir(root.join("crates"))?.collect::<std::io::Result<Vec<_>>>()?;
    directories.sort_by_key(fs::DirEntry::file_name);
    for directory in directories {
        let src = directory.path().join("src");
        if !src.is_dir() {
            continue;
        }
        let program = !src.join("lib.rs").exists();
        let mut files = Vec::new();
        rust_files(&src, &mut files)?;
        let mut scanned = Vec::new();
        for file in files {
            let text = fs::read_to_string(&file)?;
            let from_src = file.strip_prefix(&src)?.to_string_lossy().replace('\\', "/");
            let module = module_path(&from_src);
            let read = source::scan(&text, &module);
            guarded.extend(read.trait_methods.iter().cloned());
            let path = file.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
            scanned.push((path, text, module, read));
        }
        crates.push((program, scanned));
    }
    let (mut sources, mut subjects) = (Vec::new(), Vec::new());
    for (program, files) in crates {
        let gated_modules: Vec<Vec<String>> =
            files.iter().flat_map(|(_, _, _, read)| read.gated_modules.clone()).collect();
        let gated_file = |module: &[String]| gated_modules.iter().any(|gated| module.starts_with(gated));
        // Every name code compiled on some platforms alone holds: an item of such a name may be used there, which no check here sees.
        let mut gated_names = HashSet::new();
        for (_, text, module, read) in &files {
            let (code, whole) = (source::code_of(text), 0..text.len());
            let ranges = if gated_file(module) { std::slice::from_ref(&whole) } else { read.gated.as_slice() };
            for range in ranges {
                let words = String::from_utf8_lossy(&code[range.clone()]).into_owned();
                gated_names.extend(
                    words
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .filter(|word| !word.is_empty())
                        .map(String::from),
                );
            }
        }
        for (path, text, module, read) in files {
            let file = sources.len();
            for item in read.items {
                let left = gated_file(&module)
                    || read.gated.iter().any(|range| range.contains(&item.offset))
                    || (item.kind.associated() && guarded.contains(&item.name))
                    || gated_names.contains(&item.name);
                if !left {
                    let line = text[..item.offset].matches('\n').count() + 1;
                    subjects.push(Subject { file, item, line, program });
                }
            }
            sources.push(Source { path, text });
        }
    }
    Ok((sources, subjects))
}

// Every Rust file under a directory, in the order of their paths.
fn rust_files(directory: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

// Copies a file or a directory of the workspace into the copy, but for what a compiler made.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    if from.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            if entry.file_name() != "target" {
                copy_tree(&entry.path(), &to.join(entry.file_name()))?;
            }
        }
    } else {
        fs::copy(from, to)?;
    }
    Ok(())
}

// A file's text with each subject `narrowed` holds a visibility for written so and every other as it is, and where each one's token begins in it.
fn rewritten(
    source: &Source,
    subjects: &[(usize, &Subject)],
    narrowed: &[Option<Vis>],
) -> (String, Vec<(usize, usize)>) {
    let (mut text, mut tokens, mut last) = (String::with_capacity(source.text.len()), Vec::new(), 0);
    for (id, subject) in subjects {
        let token = match narrowed[*id] {
            Some(vis) => vis.token(),
            None => &source.text[subject.item.offset..subject.item.end],
        };
        text.push_str(&source.text[last..subject.item.offset]);
        tokens.push((text.len(), *id));
        text.push_str(token);
        last = subject.item.end;
        // A private item keeps no blank where its token stood.
        if token.is_empty() {
            last += source.text[last..].len() - source.text[last..].trim_start_matches([' ', '\t']).len();
        }
    }
    text.push_str(&source.text[last..]);
    (text, tokens)
}

// The compiler's messages on the copy in each of `BUILDS`, each once; a crate that fails stops none that does not depend on it.
fn check(copy: &Path, built: &Path) -> Result<Vec<Message>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let (mut found, mut seen) = (Vec::new(), HashSet::new());
    for build in BUILDS {
        let output = Command::new(&cargo)
            .current_dir(copy)
            .env("CARGO_TARGET_DIR", built)
            .args(["check", "--keep-going", "--message-format=json"])
            .args(build)
            .output()
            .map_err(|e| format!("cargo check: {e}"))?;
        let messages = messages::messages(&String::from_utf8_lossy(&output.stdout), &mut seen);
        let errors = messages.iter().any(|message| message.level == "error");
        if !output.status.success() && !errors && !found.iter().any(|message: &Message| message.level == "error") {
            return Err(format!("cargo check {}: {}", build.join(" "), String::from_utf8_lossy(&output.stderr)).into());
        }
        found.extend(messages);
    }
    Ok(found)
}

/// Finds every item that reaches further than what uses it, and every item nothing but tests uses; an error that names them where there are any.
pub fn check_workspace() -> Result<()> {
    if cfg!(target_os = "macos") {
        return crate::build::lima::r1(&["visibility"]);
    }
    let root = workspace_root();
    let (copy, built) = (root.join(COPY), root.join(BUILT));
    let (sources, subjects) = read(&root)?;
    if copy.exists() {
        fs::remove_dir_all(&copy)?;
    }
    for part in COPIED {
        copy_tree(&root.join(part), &copy.join(part))?;
    }
    let mut by_file: Vec<Vec<(usize, &Subject)>> = sources.iter().map(|_| Vec::new()).collect();
    for (id, subject) in subjects.iter().enumerate() {
        by_file[subject.file].push((id, subject));
    }
    let mut written: Vec<String> = sources.iter().map(|source| source.text.clone()).collect();
    let mut narrowed: Vec<Option<Vis>> = subjects.iter().map(Subject::narrower).collect();
    let (unused, warned) = loop {
        let (mut texts, mut tokens) = (HashMap::new(), HashMap::new());
        for (file, source) in sources.iter().enumerate() {
            let (text, placed) = rewritten(source, &by_file[file], &narrowed);
            if text != written[file] {
                fs::write(copy.join(&source.path), &text)?;
                written[file] = text.clone();
            }
            texts.insert(source.path.clone(), text);
            tokens.insert(source.path.clone(), placed);
        }
        let placed = Placed { subjects: &subjects, sources: &sources, narrowed: &narrowed, texts, tokens };
        let (mut asked, mut unused, mut unmapped, mut warned) = (HashSet::new(), Vec::new(), Vec::new(), Vec::new());
        for message in check(&copy, &built)? {
            match placed.flagged(&message) {
                Flag::Restore(ids) => asked.extend(ids),
                // A warning of what no subject is stands as the compiler tells it.
                Flag::Unused(ids) if ids.is_empty() => warned.push(message.rendered),
                Flag::Unused(ids) => unused.extend(ids),
                Flag::Unmapped => unmapped.push(message.rendered),
                Flag::Nothing => {}
            }
        }
        let restored = asked.into_iter().filter(|id| narrowed[*id].take().is_some()).count();
        eprintln!("visibility: {restored} items put back as written");
        if restored == 0 && !unmapped.is_empty() {
            return Err(format!("what the compiler asks no item answers:\n{}", unmapped.join("\n")).into());
        }
        if restored == 0 {
            break (unused, warned);
        }
    };
    let mut told = Vec::new();
    for (id, subject) in subjects.iter().enumerate() {
        if let Some(narrower) = narrowed[id] {
            told.push(format!(
                "{}:{}: {} is {} where {} reaches every use of it",
                sources[subject.file].path,
                subject.line,
                subject.named(),
                subject.item.vis.named(),
                narrower.named()
            ));
        }
    }
    for id in unused.into_iter().collect::<BTreeSet<usize>>() {
        let subject = &subjects[id];
        told.push(format!(
            "{}:{}: {} is used by nothing but tests, or by nothing",
            sources[subject.file].path,
            subject.line,
            subject.named()
        ));
    }
    told.extend(warned);
    if told.is_empty() {
        println!("every item reaches as far as what uses it and no further");
        return Ok(());
    }
    told.sort();
    told.iter().for_each(|line| println!("{line}"));
    Err(format!("{} items reach further than what uses them, or serve nothing but tests", told.len()).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A subject of a module `depth` deep, written to reach as `vis` says, in a library or a program.
    fn subject(depth: usize, vis: Vis, program: bool) -> Subject {
        let item = Item {
            offset: 0,
            end: vis.token().len(),
            vis,
            kind: Kind::Fn,
            name: "made".into(),
            owner: None,
            module: vec!["m".to_string(); depth],
        };
        Subject { file: 0, item, line: 1, program }
    }

    // The subjects of a file at `crates/c/src/b.rs`, as a round writes them with `narrowed`.
    fn placed<'a>(subjects: &'a [Subject], sources: &'a [Source], narrowed: &'a [Option<Vis>]) -> Placed<'a> {
        let by_file: Vec<(usize, &Subject)> = subjects.iter().enumerate().collect();
        let (text, tokens) = rewritten(&sources[0], &by_file, narrowed);
        let path = sources[0].path.clone();
        Placed {
            subjects,
            sources,
            narrowed,
            texts: HashMap::from([(path.clone(), text)]),
            tokens: HashMap::from([(path, tokens)]),
        }
    }

    // A file's source and its subjects.
    fn scanned(text: &str) -> (Vec<Source>, Vec<Subject>) {
        let scanned = source::scan(text, &["b".to_string()]);
        let subjects =
            scanned.items.into_iter().map(|item| Subject { file: 0, item, line: 1, program: false }).collect();
        (vec![Source { path: "crates/c/src/b.rs".into(), text: text.into() }], subjects)
    }

    #[test]
    fn narrower_step() {
        let narrower = |depth, vis, program| subject(depth, vis, program).narrower();
        assert_eq!(
            [narrower(2, Vis::Pub, false), narrower(2, Vis::Crate, false), narrower(2, Vis::Super, false)],
            [Some(Vis::Crate), Some(Vis::Super), Some(Vis::Private)]
        );
        assert_eq!(
            narrower(1, Vis::Crate, false),
            Some(Vis::Private),
            "pub(super) just below the root reaches the crate"
        );
        assert_eq!(narrower(0, Vis::Crate, false), None, "and a private item of the root does too");
        assert_eq!(narrower(2, Vis::Pub, true), Some(Vis::Super), "in a program pub reaches no further than the crate");
    }

    #[test]
    fn rewritten_tokens_and_definitions() {
        let (sources, subjects) = scanned("pub(crate)  fn made() {}\npub use crate::a::{made, more};\n");
        let narrowed = [Some(Vis::Private), None];
        let placed = placed(&subjects, &sources, &narrowed);
        assert_eq!(
            placed.texts[&sources[0].path], "fn made() {}\npub use crate::a::{made, more};\n",
            "the blank behind a token taken away goes with it, and a token as written stays"
        );
        assert_eq!(placed.tokens[&sources[0].path], [(0, 0), (13, 1)]);
        let at = |start| Span { file: sources[0].path.clone(), start, primary: false };
        assert_eq!((placed.defined(&at(0)), placed.defined(&at(40)), placed.defined(&at(3))), (Some(0), Some(1), None));
        assert_eq!(
            missing_in("error[E0432]: unresolved import\n  no `made` in `crate::a`"),
            Some(("made", "crate::a")),
            "a path that names nothing"
        );
    }

    #[test]
    fn message_flags() {
        let (sources, subjects) = scanned("pub(crate) fn made() {}\npub(crate) fn kept() {}\n");
        let narrowed = [Some(Vis::Private), None];
        let placed = placed(&subjects, &sources, &narrowed);
        let message = |level: &str, code: &str, text: &str, start| Message {
            level: level.into(),
            code: Some(code.into()),
            text: text.into(),
            rendered: text.into(),
            spans: vec![
                Span { file: "crates/c/src/a.rs".into(), start: 0, primary: true },
                Span { file: sources[0].path.clone(), start, primary: false },
            ],
            children: Vec::new(),
        };
        assert!(
            matches!(placed.flagged(&message("error", "E0603", "function `made` is private", 0)), Flag::Restore(ids) if ids == [0]),
            "by its definition"
        );
        assert!(
            matches!(placed.flagged(&message("error", "E0425", "cannot find function `made` in this scope", 99)), Flag::Restore(ids) if ids == [0]),
            "by its name, where no glob takes it"
        );
        assert!(
            matches!(
                placed.flagged(&message("error", "E0425", "cannot find function `kept` in this scope", 99)),
                Flag::Unmapped
            ),
            "what is as written answers nothing"
        );
        let unused = Message {
            spans: vec![Span { file: sources[0].path.clone(), start: 3, primary: true }],
            ..message("warning", "dead_code", "function `made` is never used", 0)
        };
        assert!(
            matches!(placed.flagged(&unused), Flag::Unused(ids) if ids == [0]),
            "a warning of an unused item names the item whose token stands on its line"
        );
    }
}

//! The voice browser: a page read out of its HTML, what was asked resolved to where it leads, and the language model's step for what the device cannot open itself.

pub mod state;
pub mod visit;

use crate::base::markup::{Piece, Pieces, attribute, has_attribute};
use crate::base::text::to_one_line;
use crate::codec::http::{CLOSE, Head, IDENTITY, SCHEME, Target, USER_AGENT};
use crate::net::fetch::{self, ResponseKind};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

const ACCEPT: &str = "text/html, application/xhtml+xml;q=0.9, */*;q=0.5";
const CONTENT_TYPE: &str = "content-type";
// The kinds of document the device reads a page out of.
const READ_KINDS: [&str; 2] = ["text/html", "application/xhtml+xml"];
/// The headers of a page's request.
pub const HEADERS: [(&str, &str); 4] =
    [("User-Agent", USER_AGENT), ("Accept", ACCEPT), ("Accept-Encoding", IDENTITY), ("Connection", CLOSE)];
/// The model's task: to resolve what was asked, and to write the page the device could not open.
pub const TASK: &str = include_str!("../../../assets/web/browse.txt");
const PLAIN_SCHEME: &str = "http://";
const TITLE: &str = "title";
const ANCHOR: &str = "a";
const ITEM: &str = "li";
const MAIN: &str = "main";
const ARTICLE: &str = "article";
const PARAGRAPH: &str = "p";
const SECTION: &str = "section";
const FIRST_HEADING: &str = "h1";
const RAISED: &str = "sup";
// What holds nothing, and is never closed.
const VOID: [&str; 14] =
    ["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];
// What holds code or styles, which are no markup.
const RAW: [&str; 2] = ["script", "style"];
// What holds no words of the page: forms and their controls, drawings, and the site's own menus around the page.
const UNREAD: [&str; 11] =
    ["noscript", "template", "svg", "iframe", "form", "button", "select", "nav", "footer", "aside", "dialog"];
// The roles that say the same of any element: a site's menu, its foot, what stands beside the page, a search and a window over the page.
const UNREAD_ROLES: [&str; 5] = ["navigation", "contentinfo", "complementary", "search", "dialog"];
const UNDISPLAYED: &str = "display:none";
// What begins a line of its own and ends it.
const BLOCKS: [&str; 37] = [
    PARAGRAPH,
    "div",
    SECTION,
    "nav",
    "footer",
    "aside",
    "form",
    "dialog",
    ARTICLE,
    MAIN,
    "ul",
    "ol",
    ITEM,
    "table",
    "tr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "figure",
    "figcaption",
    "dl",
    "dt",
    "dd",
    "br",
    "hr",
    "header",
    "address",
    "details",
    "summary",
    "body",
    "html",
    "caption",
];
const HEADINGS: [(&str, &str); 6] =
    [("h1", "# "), ("h2", "## "), ("h3", "### "), ("h4", "### "), ("h5", "### "), ("h6", "### ")];
const BULLET: &str = "- ";
// The signs Markdown reads as its own, written behind a stroke so that the page's words stay words.
const SIGNS: [char; 5] = ['\\', '*', '_', '`', '['];
const LINE_SIGNS: [char; 3] = ['#', '-', '+'];
// A link's number stands between these behind its words, strong, its bracket written as a bracket.
const MARK_START: &str = " **\\[";
const MARK_END: &str = "]**";
// How the model writes the same mark's beginning.
const WRITTEN_MARK_START: &str = " **[";
const BACK_WORD: &str = "BACK";
const LINK_WORD: &str = "LINK";
const OPEN_WORD: &str = "OPEN";
const LINKS_WORD: &str = "LINKS";
const BACK_WORDS: [&str; 2] = ["back", "go back"];
// The words a link's number is said among, which name no link themselves.
const FILLERS: [&str; 9] = ["the", "link", "number", "open", "go", "to", "one", "please", "result"];
const NUMBERS: [(&str, &str); 20] = [
    ("one", "first"),
    ("two", "second"),
    ("three", "third"),
    ("four", "fourth"),
    ("five", "fifth"),
    ("six", "sixth"),
    ("seven", "seventh"),
    ("eight", "eighth"),
    ("nine", "ninth"),
    ("ten", "tenth"),
    ("eleven", "eleventh"),
    ("twelve", "twelfth"),
    ("thirteen", "thirteenth"),
    ("fourteen", "fourteenth"),
    ("fifteen", "fifteenth"),
    ("sixteen", "sixteenth"),
    ("seventeen", "seventeenth"),
    ("eighteen", "eighteenth"),
    ("nineteen", "nineteenth"),
    ("twenty", "twentieth"),
];

/// A page as the browser shows it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Page {
    /// Where the device fetched it; empty for one the model wrote.
    pub address: String,
    pub title: String,
    /// Its words as Markdown, a link's number behind what leads there.
    pub text: String,
    /// Where each numbered link leads, the first numbered 1.
    pub links: Vec<String>,
}

impl Page {
    // The page's words without the links' numbers, block for block as the text has them: what a reading aloud says.
    pub(crate) fn to_spoken(&self) -> String {
        let mut spoken = String::with_capacity(self.text.len());
        let mut rest = self.text.as_str();
        while let Some((before, behind)) = rest.split_once(MARK_START) {
            let digits = behind.bytes().take_while(u8::is_ascii_digit).count();
            let tail = behind[digits..].strip_prefix(MARK_END).filter(|_| digits > 0);
            spoken.push_str(before);
            if tail.is_none() {
                spoken.push_str(MARK_START);
            }
            rest = tail.unwrap_or(behind);
        }
        spoken.push_str(rest);
        spoken
    }
}

fn to_mark(number: usize) -> String {
    format!("{MARK_START}{number}{MARK_END}")
}

fn to_read_line(page: &Page, bytes: usize, ms: u128) -> String {
    format!(
        "web: a page of {bytes} bytes read as {} characters with {} links in {ms} ms",
        page.text.chars().count(),
        page.links.len()
    )
}

fn to_unread_line(reason: &dyn core::fmt::Display) -> String {
    format!("web: the page is not read by the device: {reason}")
}

fn is_named(name: &str, names: &[&str]) -> bool {
    names.iter().any(|known| name.eq_ignore_ascii_case(known))
}

/// What an answer's head announces for a page fetched at `target`, or why the device reads no page out of it: what no fetched document's answer may be, and a document its head names as another kind than a page's.
pub fn check_head(head: &Head, target: &Target) -> Result<ResponseKind, String> {
    let kind = fetch::check_head(head, target).map_err(|error| error.to_string())?;
    let named = head.header(CONTENT_TYPE).map(|kind| kind.split(';').next().unwrap_or_default().trim());
    match named {
        Some(named) if kind == ResponseKind::Document && !is_named(named, &READ_KINDS) => {
            Err(format!("a document of the kind {named}"))
        }
        _ => Ok(kind),
    }
}

// An element the reading stands in, by its name, with how many of that name are open inside it.
struct Inside {
    name: String,
    depth: usize,
}

impl Inside {
    fn enter(name: &str) -> Option<Inside> {
        Some(Inside { name: name.to_ascii_lowercase(), depth: 1 })
    }

    // Takes the opening of an element that holds something, or a closing; true once the element itself is closed.
    fn take(&mut self, name: &str, is_open: bool) -> bool {
        if name.eq_ignore_ascii_case(&self.name) {
            self.depth = if is_open { self.depth + 1 } else { self.depth.saturating_sub(1) };
        }
        self.depth == 0
    }
}

// True for an element that holds no words of the page: by its name, its role, or as one the page does not show.
fn is_unread(name: &str, attributes: &str) -> bool {
    let is_undisplayed = |style: &str| {
        let style: String = style.chars().filter(|sign| !sign.is_whitespace()).collect();
        style.to_ascii_lowercase().contains(UNDISPLAYED)
    };
    is_named(name, &UNREAD)
        || has_attribute(attributes, "hidden")
        || attribute(attributes, "aria-hidden").is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
        || attribute(attributes, "role").is_some_and(|role| is_named(role.trim(), &UNREAD_ROLES))
        || attribute(attributes, "style").is_some_and(|style| is_undisplayed(&style))
}

// The pieces of a document that may hold words of the page, each with its place among them: an opening is empty where its element holds nothing, and so is the opening of an element that is not read, which alone is handed on of it.
struct Readable<'a> {
    pieces: Pieces<'a>,
    count: usize,
    unread: Option<Inside>,
}

impl<'a> Readable<'a> {
    const fn new(document: &'a str) -> Readable<'a> {
        Readable { pieces: Pieces::new(document), count: 0, unread: None }
    }
}

impl<'a> Iterator for Readable<'a> {
    type Item = (usize, Piece<'a>);

    fn next(&mut self) -> Option<(usize, Piece<'a>)> {
        loop {
            let (at, piece) = (self.count, self.pieces.next()?);
            self.count += 1;
            let (name, attributes, holds) = match &piece {
                Piece::Open { name, attributes, is_empty } => (*name, *attributes, !is_empty && !is_named(name, &VOID)),
                Piece::Close(name) => (*name, "", false),
                Piece::Text(_) => {
                    if self.unread.is_none() {
                        return Some((at, piece));
                    }
                    continue;
                }
            };
            let is_raw = holds && is_named(name, &RAW);
            if is_raw {
                self.pieces.skip_element(name);
            }
            if let Some(inside) = &mut self.unread {
                if !is_raw && (holds || matches!(piece, Piece::Close(_))) && inside.take(name, holds) {
                    self.unread = None;
                }
                continue;
            }
            let is_left_out = is_raw || holds && is_unread(name, attributes);
            if is_left_out && !is_raw {
                self.unread = Inside::enter(name);
            }
            return Some((
                at,
                match piece {
                    Piece::Open { .. } => Piece::Open { name, attributes, is_empty: !holds || is_left_out },
                    other => other,
                },
            ));
        }
    }
}

// An open element: its name, the place of its opening, and the bytes of the words in it.
struct Element<'a> {
    name: &'a str,
    at: usize,
    words: usize,
}

// What a block holds: the bytes of the words of the paragraphs that are its own, and of all the words in it.
#[derive(Default)]
struct Held {
    paragraphs: usize,
    words: usize,
}

fn close(open: &mut Vec<Element<'_>>, holders: &mut BTreeMap<usize, Held>) {
    let Some(element) = open.pop() else { return };
    if let Some(held) = holders.get_mut(&element.at) {
        held.words = element.words;
    }
    if let Some(parent) = open.last_mut() {
        parent.words += element.words;
    }
}

// The element whose words are the page's, by the place of its opening: the block that holds more than half of the document's paragraph words in paragraphs of its own, or of its sections, and more than half of all its words; else its first `main`; else its one `article`; none where the whole body is.
fn scope_of(document: &str) -> Option<usize> {
    let (mut open, mut holders): (Vec<Element<'_>>, BTreeMap<usize, Held>) = (Vec::new(), BTreeMap::new());
    let (mut paragraphs, mut words, mut main, mut articles) = (0, 0, None, Vec::new());
    let is_open = |open: &[Element<'_>], name: &str| open.iter().any(|element| is_named(element.name, &[name]));
    for (at, piece) in Readable::new(document) {
        match piece {
            Piece::Open { name, is_empty: false, .. } => {
                // A paragraph holds no paragraph: one that opens ends the one before it.
                if is_named(name, &[PARAGRAPH]) && open.last().is_some_and(|top| is_named(top.name, &[PARAGRAPH])) {
                    close(&mut open, &mut holders);
                }
                if is_named(name, &[MAIN]) && main.is_none() {
                    main = Some(at);
                }
                if is_named(name, &[ARTICLE]) && !is_open(&open, ARTICLE) {
                    articles.push(at);
                }
                open.push(Element { name, at, words: 0 });
            }
            Piece::Open { .. } => {}
            Piece::Close(name) => {
                let depth = open.iter().rposition(|element| element.name.eq_ignore_ascii_case(name));
                while depth.is_some_and(|depth| open.len() > depth) {
                    close(&mut open, &mut holders);
                }
            }
            Piece::Text(text) => {
                let bytes: usize = text.split_whitespace().map(str::len).sum();
                words += bytes;
                if let Some(top) = open.last_mut() {
                    top.words += bytes;
                }
                let paragraph = open.iter().rposition(|element| is_named(element.name, &[PARAGRAPH]));
                let holder = paragraph
                    .and_then(|at| open[..at].iter().rfind(|element| !is_named(element.name, &[SECTION])))
                    .map(|element| element.at);
                if let Some(holder) = holder {
                    paragraphs += bytes;
                    holders.entry(holder).or_default().paragraphs += bytes;
                }
            }
        }
    }
    while !open.is_empty() {
        close(&mut open, &mut holders);
    }
    let most = holders.iter().max_by_key(|(_, held)| held.paragraphs);
    let text = most.filter(|(_, held)| held.paragraphs * 2 > paragraphs && held.words * 2 > words).map(|(at, _)| *at);
    text.or(main).or(match articles[..] {
        [one] => Some(one),
        _ => None,
    })
}

// An open link: where it leads, whether words stand in it, and the words its label says of it.
struct Link {
    address: String,
    has_words: bool,
    label: String,
}

// Words as a line of the page holds them: one space between two, the signs Markdown reads as its own behind a stroke.
fn to_words(text: &str) -> String {
    let mut reader = Reader::default();
    reader.add_words(text);
    reader.line.truncate(reader.line.trim_end().len());
    reader.line
}

#[derive(Default)]
struct Reader {
    lines: Vec<String>,
    line: String,
    prefix: &'static str,
    links: Vec<String>,
    link: Option<Link>,
    // The last link, where it held no words, by its label's words and its address: it is the link of the line that ends in those words, as a card's headline does under the link laid over the card.
    labelled: Option<(String, String)>,
    // Where in the line what is raised above it began, and whether a link in it leads to a place on the page: a footnote's mark, which is no word of the page.
    raised: Option<(usize, bool)>,
    title: String,
    // The words of the document's first heading, wherever it stands.
    heading: String,
}

impl Reader {
    // The number of the link to that address: the one it has where another link led there before.
    fn number(&mut self, address: String) -> usize {
        match self.links.iter().position(|known| *known == address) {
            Some(at) => at + 1,
            None => {
                self.links.push(address);
                self.links.len()
            }
        }
    }

    fn end_line(&mut self) {
        let words = core::mem::take(&mut self.line);
        let words = words.trim();
        if !words.is_empty() {
            let labelled = self.labelled.take_if(|(label, _)| words.ends_with(label.as_str()));
            let mark = labelled.map(|(_, address)| to_mark(self.number(address))).unwrap_or_default();
            let guard = if self.prefix.is_empty() && words.starts_with(LINE_SIGNS) { "\\" } else { "" };
            self.lines.push(format!("{}{guard}{words}{mark}", self.prefix));
        }
        (self.prefix, self.raised) = ("", None);
    }

    fn take_raised(&mut self, opens: bool) {
        if opens {
            self.raised = self.raised.or(Some((self.line.len(), false)));
        } else if let Some((start, true)) = self.raised.take() {
            self.line.truncate(start);
        }
    }

    fn add_words(&mut self, text: &str) {
        let mut is_spaced = self.line.is_empty() || self.line.ends_with(' ');
        for character in text.chars() {
            if character.is_whitespace() {
                if !is_spaced {
                    self.line.push(' ');
                }
                is_spaced = true;
                continue;
            }
            // A word that follows a link's number without a space, as a menu's next entry does, stands apart from it.
            if character.is_alphanumeric() && self.line.ends_with(MARK_END) {
                self.line.push(' ');
            }
            if SIGNS.contains(&character) {
                self.line.push('\\');
            }
            self.line.push(character);
            is_spaced = false;
            if let Some(link) = &mut self.link {
                link.has_words = true;
            }
        }
    }

    // Numbers a link that held words; one that held none and is labelled waits for the line that says its label.
    fn end_link(&mut self) {
        let Some(link) = self.link.take() else { return };
        if !link.has_words {
            self.labelled = Some((link.label, link.address)).filter(|(label, _)| !label.is_empty());
            return;
        }
        self.labelled = None;
        let mark = to_mark(self.number(link.address));
        let words_end = self.line.trim_end().len();
        match self.lines.last_mut().filter(|_| words_end == 0) {
            Some(last) => last.push_str(&mark),
            None => {
                self.line.truncate(words_end);
                self.line.push_str(&mark);
            }
        }
    }
}

// Reads a page out of the HTML fetched at `target`: the words of the block that holds its text, of its `main`, of its one `article`, or of its body, without the site's menus, what it does not show and its footnotes' marks, a line a block, with its links numbered, under its first heading or its title; none where it holds no words, as a page a script builds.
fn read_document(document: &str, target: &Target) -> Option<Page> {
    let scope = scope_of(document);
    let (mut reader, mut scoped): (Reader, Option<Inside>) = (Reader::default(), None);
    let (mut is_titled, mut is_headed) = (false, false);
    for (at, piece) in Readable::new(document) {
        let (name, attributes, holds) = match &piece {
            Piece::Open { name, attributes, is_empty } => (*name, *attributes, !is_empty),
            Piece::Close(name) => (*name, "", false),
            Piece::Text(text) => {
                if is_titled {
                    reader.title.push_str(text);
                    continue;
                }
                if is_headed {
                    reader.heading.push_str(text);
                }
                if scope.is_none() || scoped.is_some() {
                    reader.add_words(text);
                }
                continue;
            }
        };
        if is_named(name, &[TITLE]) {
            is_titled = holds && reader.title.is_empty();
            continue;
        }
        if is_named(name, &[FIRST_HEADING]) {
            is_headed = holds && reader.heading.is_empty();
        }
        match &mut scoped {
            Some(inside) => {
                if (holds || matches!(piece, Piece::Close(_))) && inside.take(name, holds) {
                    reader.end_link();
                    reader.end_line();
                    scoped = None;
                    continue;
                }
            }
            None if scope == Some(at) => {
                scoped = Inside::enter(name);
                continue;
            }
            None if scope.is_some() => continue,
            None => {}
        }
        if is_named(name, &[RAISED]) && (holds || matches!(piece, Piece::Close(_))) {
            reader.take_raised(holds);
        } else if is_named(name, &[ANCHOR]) {
            reader.end_link();
            let href = attribute(attributes, "href").filter(|_| holds);
            let is_place = href.as_deref().is_some_and(|href| href.trim_start().starts_with('#'));
            if let (Some(raised), true) = (&mut reader.raised, is_place) {
                raised.1 = true;
            }
            let leads = href.filter(|_| !is_place).and_then(|href| target.resolve(&href));
            let label = attribute(attributes, "aria-label").map(|label| to_words(&label)).unwrap_or_default();
            reader.link = leads.map(|target| Link { address: target.to_address(), has_words: false, label });
        } else if is_named(name, &BLOCKS) && reader.link.is_some() {
            // A block in a link is of the link's words: it parts them, and ends no line.
            reader.add_words(" ");
        } else if is_named(name, &BLOCKS) {
            reader.end_line();
            let heading = HEADINGS.iter().find(|(tag, _)| name.eq_ignore_ascii_case(tag)).map(|(_, prefix)| *prefix);
            let bullet = is_named(name, &[ITEM]).then_some(BULLET);
            reader.prefix = heading.or(bullet).filter(|_| holds).unwrap_or_default();
        }
    }
    reader.end_link();
    reader.end_line();
    if reader.lines.is_empty() {
        return None;
    }
    let headed = reader.lines.iter().find_map(|line| line.strip_prefix("# "));
    let title = match headed {
        Some(heading) => to_plain(heading),
        None => {
            let named = [&reader.heading, &reader.title].map(|words| to_one_line(words));
            let title = named.into_iter().find(|words| !words.is_empty()).unwrap_or_else(|| target.host.clone());
            reader.lines.insert(0, format!("# {}", to_words(&title)));
            title
        }
    };
    Some(Page { address: target.to_address(), title, text: reader.lines.join("\n\n"), links: reader.links })
}

// A line of the page's Markdown as plain words: without its strokes and its links' numbers.
fn to_plain(line: &str) -> String {
    let page = Page { text: line.into(), ..Page::default() };
    let (mut plain, mut is_escaped) = (String::new(), false);
    for character in page.to_spoken().chars() {
        if character == '\\' && !is_escaped {
            is_escaped = true;
            continue;
        }
        plain.push(character);
        is_escaped = false;
    }
    plain
}

// What was asked of the browser, as far as the device tells it without the model.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Request {
    Back,
    // The link of that number on the page that shows.
    Link(usize),
    Address(String),
    // Anything else, in the asker's words.
    Ask(String),
}

fn number_of(word: &str) -> Option<usize> {
    if let Ok(number) = word.parse::<usize>() {
        return Some(number);
    }
    NUMBERS.iter().position(|(cardinal, ordinal)| word == *cardinal || word == *ordinal).map(|at| at + 1)
}

// An address written or said as one: one word under a scheme, or one that holds a dot between letters or digits and ends in letters, as `example.org/news` does.
fn address_of(text: &str) -> Option<String> {
    if text.contains(char::is_whitespace) {
        return None;
    }
    let bare = text.strip_prefix(SCHEME).or_else(|| text.strip_prefix(PLAIN_SCHEME)).unwrap_or(text);
    let host = bare.split(['/', '?', '#']).next().unwrap_or_default();
    let (name, ending) = host.rsplit_once('.')?;
    let is_host = !name.is_empty()
        && ending.len() > 1
        && ending.chars().all(|letter| letter.is_ascii_alphabetic())
        && host.chars().all(|sign| sign.is_alphanumeric() || matches!(sign, '.' | '-'));
    let address = format!("{SCHEME}{bare}");
    (is_host && Target::parse(&address).is_some()).then_some(address)
}

// What a request said or typed asks, on a page of `link_count` links: back, a link by its number alone or among the words it is said with, an address, or what only the model resolves.
fn parse(said: &str, link_count: usize) -> Request {
    let text = said.trim().trim_end_matches(['.', '!', '?']).trim();
    let lower = text.to_lowercase();
    if BACK_WORDS.contains(&lower.as_str()) {
        return Request::Back;
    }
    if let Some(address) = address_of(text) {
        return Request::Address(address);
    }
    let words: Vec<&str> = lower.split_whitespace().collect();
    let mut numbers =
        words.iter().filter(|word| !FILLERS.contains(word) || words.len() == 1).map(|word| number_of(word));
    match (numbers.next(), numbers.next()) {
        (Some(Some(number)), None) if (1..=link_count).contains(&number) => Request::Link(number),
        _ => Request::Ask(text.into()),
    }
}

// What the model answered a request with.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Reply {
    Back,
    Link(usize),
    Open(String),
    Page(Page),
}

// Reads the model's answer: `BACK`, `LINK` and a number, `OPEN` and an address, or a page in Markdown whose last lines, under `LINKS`, give each numbered link's address; a page numbers its links from 1, so a number past its marks' count names no link.
fn read_reply(reply: &str) -> Reply {
    let text = reply.trim();
    let (word, rest) = text.split_once(char::is_whitespace).unwrap_or((text, ""));
    let is_one_line = !text.contains('\n');
    if is_one_line && text == BACK_WORD {
        return Reply::Back;
    }
    if is_one_line
        && word == LINK_WORD
        && let Ok(number) = rest.trim().parse()
    {
        return Reply::Link(number);
    }
    if is_one_line
        && word == OPEN_WORD
        && let Some(address) = address_of(rest.trim())
    {
        return Reply::Open(address);
    }
    let lines: Vec<&str> = text.lines().collect();
    let list_at = lines.iter().rposition(|line| line.trim() == LINKS_WORD);
    let (body, list) = list_at.map_or((&lines[..], &[][..]), |at| (&lines[..at], &lines[at + 1..]));
    let text = body.join("\n").trim().replace(WRITTEN_MARK_START, MARK_START);
    let marks = text.matches(MARK_START).count();
    let mut links: Vec<String> = Vec::new();
    for line in list {
        let Some((number, address)) = line.trim().split_once(char::is_whitespace) else { continue };
        let number = number.trim_end_matches(['.', ')', ':']).parse::<usize>().ok();
        let number = number.filter(|number| (1..=marks).contains(number));
        let address = Some(address.trim()).filter(|address| Target::parse(address).is_some());
        if let (Some(number), Some(address)) = (number, address) {
            if links.len() < number {
                links.resize(number, String::new());
            }
            links[number - 1] = address.into();
        }
    }
    let title = text.lines().find_map(|line| line.strip_prefix("# ")).map(to_plain).unwrap_or_default();
    Reply::Page(Page { address: String::new(), title, text, links })
}

// What the model is told of a request: the words, and the page that shows as the screen has it, each link's number behind its words, so that it can tell which is meant.
fn to_input(said: &str, page: Option<&Page>) -> String {
    let mut input = format!("Request: {}\n", said.trim());
    let Some(page) = page else {
        input.push_str("No page shows.\n");
        return input;
    };
    if !page.address.is_empty() {
        let _ = writeln!(input, "The address of the page that shows: {}", page.address);
    }
    let _ = writeln!(input, "The page that shows:\n{}", page.text);
    input
}

// What the model is told to write a page of an address the device could not read.
fn to_page_input(address: &str) -> String {
    format!("Request: write the page at {address} as a page, never as BACK, LINK or OPEN.\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENT: &str = r##"<!doctype html><html><head><title>The  Daily
        Example</title><style>p { color: red }</style><script>var a = "<p>no</p>";</script></head>
        <body><nav><a href="/">Home</a> <a href="/world">World</a></nav>
        <main><h1>Budget passes &amp; markets rise</h1>
        <p>The vote came <a href="/politics/vote?id=7">late on Friday</a>, by 5 * 3 votes.</p>
        <ul><li><a href="https://other.example/a_b">A first reaction</a></li><li>No link here</li></ul>
        <h2>What follows</h2><p># not a heading<br>- not an item</p>
        <p><a href="/politics/vote?id=7">The same vote</a> and <a href="#top">the top</a>, <a href="mailto:a@b.c">mail</a>
        <a href="/empty"><img src="x.png"></a></p><form><input value="q"><button>Search</button></form>
        </main><footer>Imprint <a href="/imprint">here</a></footer></body></html>"##;

    fn target() -> Target {
        Target::parse("https://news.example/today/index.html").unwrap()
    }

    #[test]
    fn page_out_of_html() {
        let page = read_document(DOCUMENT, &target()).expect("a page of words");
        assert_eq!(
            page.text,
            "# Budget passes & markets rise\n\nThe vote came late on Friday **\\[1]**, by 5 \\* 3 votes.\n\n- A first reaction **\\[2]**\n\n- No link here\n\n## What follows\n\n\\# not a heading\n\n\\- not an item\n\nThe same vote **\\[1]** and the top, mail",
            "the words of its main alone, a line a block, a link's number behind its words, the page's own signs left as words"
        );
        assert_eq!(
            page.links,
            ["https://news.example/politics/vote?id=7", "https://other.example/a_b"],
            "each address once, by the number it first had; a place on the page, another scheme and a link without words are none"
        );
        assert_eq!(
            (page.title.as_str(), page.address.as_str()),
            ("Budget passes & markets rise", "https://news.example/today/index.html")
        );
        assert_eq!(
            page.to_spoken(),
            "# Budget passes & markets rise\n\nThe vote came late on Friday, by 5 \\* 3 votes.\n\n- A first reaction\n\n- No link here\n\n## What follows\n\n\\# not a heading\n\n\\- not an item\n\nThe same vote and the top, mail",
            "what is read aloud says no numbers"
        );
    }

    #[test]
    fn scope_and_title() {
        let whole = "<html><head><title>Plain &amp; simple</title></head><body><header>Site</header><nav>Menu</nav><div>One<p>Two</div><aside>Ads</aside></body></html>";
        let page = read_document(whole, &target()).unwrap();
        assert_eq!(
            page.text, "# Plain & simple\n\nSite\n\nOne\n\nTwo",
            "without a main or one article, the body but its menus, under the title its head gives"
        );
        assert_eq!(page.title, "Plain & simple");
        let two = "<article><p>One</p></article><article><p>Two</p></article>";
        assert_eq!(
            read_document(two, &target()).map(|page| page.text).as_deref(),
            Some("# news.example\n\nOne\n\nTwo")
        );
        let one =
            "<p>Teaser</p><article><h1>Head_line</h1><p>Body</p><article>Nested</article> tail</article><p>After</p>";
        assert_eq!(
            read_document(one, &target()).map(|page| (page.text, page.title)),
            Some(("# Head\\_line\n\nBody\n\nNested\n\ntail".into(), "Head_line".into())),
            "the one article whole, one nested in it too"
        );
        for empty in ["", "<html><body><script>render()</script><noscript>Enable JavaScript</noscript></body></html>"] {
            assert_eq!(read_document(empty, &target()), None, "a page a script builds holds no words");
        }
    }

    #[test]
    fn text_block_without_clutter() {
        let document = r##"<html><head><title>Site</title><script>if (a<b) { x = "</p><p>"; }</script></head><body>
            <h1>The <span>heading</span></h1>
            <div><ul><li><a href="/de">Deutsch</a></li><li><a href="/fr">Fran&ccedil;ais</a></li></ul>
            <div class="text"><div style="display: none">Short description</div>
                <section><p>The first paragraph of the text<sup><a href="#note-1">[1]</a></sup> covers x<sup>2</sup>.</p></section>
                <section><h2 hidden>Unseen</h2><p>The second paragraph <span aria-hidden="true">icon</span>goes on.</p>
                <div role="navigation"><a href="/more">More of the site</a></div><br/><div/></section>
            </div></div>
            <p>Imprint</p></body></html>"##;
        let page = read_document(document, &target()).expect("a page of words");
        assert_eq!(
            (page.text.as_str(), page.title.as_str(), page.links.len()),
            (
                "# The heading\n\nThe first paragraph of the text covers x2.\n\nThe second paragraph goes on.",
                "The heading",
                0
            ),
            "the block that holds the paragraphs, its sections with it, under the document's first heading: no menu beside it, nothing the page does not show, no footnote's mark, and no word of a script"
        );
        let thin = "<main><h1>Front</h1><ul><li><a href=\"/a\">One story</a></li><li><a href=\"/b\">Another story</a></li></ul></main><div><p>We use cookies.</p></div>";
        assert_eq!(
            read_document(thin, &target()).map(|page| page.text).as_deref(),
            Some("# Front\n\n- One story **\\[1]**\n\n- Another story **\\[2]**"),
            "a block of paragraphs that holds the lesser part of the words is not the text: the main is"
        );
    }

    #[test]
    fn cards_and_rows() {
        let document = r##"<main>
            <div><a href="/a" aria-label="Fishermen sue"></a><div>US military</div><h3>Fishermen sue</h3></div>
            <div><a href="/share" aria-label="Share"></a><p>Nothing says it.</p></div>
            <div><a href="/b"><div>Kyiv</div><h3>Hospital struck</h3><p>At night.</p></a></div>
            <p><a href="/x">one</a><a href="/y">two</a>| three</p>
            <table><tr><td>1.</td><td><a href="/vote"><div></div></a></td><td><a href="/c">Story</a> (c.example)</td></tr></table>
            </main>"##;
        let page = read_document(document, &target()).expect("a page of words");
        assert_eq!(
            page.text,
            "# news.example\n\nUS military\n\n### Fishermen sue **\\[1]**\n\nNothing says it.\n\nKyiv Hospital struck At night. **\\[2]**\n\none **\\[3]** two **\\[4]**| three\n\n1. Story **\\[5]** (c.example)",
            "a link laid over a card is the link of the line that says its label, and of none where no line does; the blocks in a link are its words on one line; a word stands apart from the number before it; a row is a line"
        );
        assert_eq!(page.links.len(), 5);
    }

    #[test]
    fn requests() {
        assert_eq!([parse("Back.", 3), parse(" go back ", 0)], [Request::Back, Request::Back]);
        assert_eq!(
            [
                parse("3", 5),
                parse("the third link", 5),
                parse("Link number two.", 5),
                parse("open twenty", 20),
                parse("one", 2)
            ],
            [Request::Link(3), Request::Link(3), Request::Link(2), Request::Link(20), Request::Link(1)]
        );
        assert_eq!(
            [parse("6", 5), parse("three or four", 5), parse("the one about batteries", 5), parse("0", 5)],
            [
                Request::Ask("6".into()),
                Request::Ask("three or four".into()),
                Request::Ask("the one about batteries".into()),
                Request::Ask("0".into())
            ],
            "a number the page has no link of, two numbers, and words that name none are the model's"
        );
        assert_eq!(
            [
                parse("example.org", 0),
                parse("https://Example.org/a?b=1", 0),
                parse("http://example.org/x", 0),
                parse("en.wikipedia.org/wiki/Rust", 9)
            ],
            [
                Request::Address("https://example.org".into()),
                Request::Address("https://Example.org/a?b=1".into()),
                Request::Address("https://example.org/x".into()),
                Request::Address("https://en.wikipedia.org/wiki/Rust".into())
            ],
            "an address goes under TLS, whatever scheme it was written under"
        );
        assert_eq!(
            [
                parse("the guardian", 0),
                parse("3.5", 9),
                parse("example.", 0),
                parse("a.b c.d", 0),
                parse("user@example.org", 0)
            ],
            [
                Request::Ask("the guardian".into()),
                Request::Ask("3.5".into()),
                Request::Ask("example".into()),
                Request::Ask("a.b c.d".into()),
                Request::Ask("user@example.org".into())
            ]
        );
    }

    #[test]
    fn model_replies() {
        assert_eq!(
            [read_reply(" BACK\n"), read_reply("LINK 4"), read_reply("OPEN https://www.example.org/news")],
            [Reply::Back, Reply::Link(4), Reply::Open("https://www.example.org/news".into())]
        );
        let written = "# Battery news\n\nA new cell **[1]** holds more.\n\n- Second story **[2]**\n\nLINKS\n1 https://a.example/cell\n2. https://b.example/story\n3 not an address\n";
        assert_eq!(
            read_reply(written),
            Reply::Page(Page {
                address: String::new(),
                title: "Battery news".into(),
                text: "# Battery news\n\nA new cell **\\[1]** holds more.\n\n- Second story **\\[2]**".into(),
                links: alloc::vec!["https://a.example/cell".into(), "https://b.example/story".into()],
            }),
            "a page with its links' addresses, a number marked as the device marks one"
        );
        let unmarked = "# Two\n\nOne **[1]** and two **[2]**.\n\nLINKS\n1 https://a.example/\n3 https://c.example/\n100000000 https://far.example/\n";
        let Reply::Page(page) = read_reply(unmarked) else { panic!("a page") };
        assert_eq!(page.links, ["https://a.example/"], "a number past the count of the page's marks names no link");
        let Reply::Page(page) = read_reply("LINK four\nand more") else { panic!("a page") };
        assert_eq!((page.title.as_str(), page.links.len()), ("", 0), "what is none of the words is a page as written");
        assert_eq!(read_reply("OPEN nowhere"), Reply::Page(Page { text: "OPEN nowhere".into(), ..Page::default() }));
    }

    #[test]
    fn model_input_and_lines() {
        let page = read_document(DOCUMENT, &target()).unwrap();
        assert_eq!(
            to_input(" the second one ", Some(&page)),
            format!(
                "Request: the second one\nThe address of the page that shows: https://news.example/today/index.html\nThe page that shows:\n{}\n",
                page.text
            )
        );
        let written = Page { text: "# Written\n\nOne **\\[1]**".into(), ..Page::default() };
        assert_eq!(
            to_input("one", Some(&written)),
            "Request: one\nThe page that shows:\n# Written\n\nOne **\\[1]**\n",
            "a page the model wrote has no address"
        );
        assert_eq!(to_input("the guardian", None), "Request: the guardian\nNo page shows.\n");
        assert!(to_page_input("https://a.example/x").contains("https://a.example/x"));
        assert_eq!(
            to_read_line(&page, 1_234, 56),
            format!("web: a page of 1234 bytes read as {} characters with 2 links in 56 ms", page.text.chars().count())
        );
        assert_eq!(to_unread_line(&"status 403"), "web: the page is not read by the device: status 403");
        assert_eq!(HEADERS.map(|(name, _)| name), ["User-Agent", "Accept", "Accept-Encoding", "Connection"]);
        let head = |text: &str| Head::parse(format!("HTTP/1.1 {text}\r\n\r\n").as_bytes()).unwrap().unwrap().0;
        assert_eq!(
            [
                check_head(&head("200 OK\r\nContent-Type: Text/HTML; charset=utf-8"), &target()),
                check_head(&head("200 OK"), &target()),
                check_head(&head("200 OK\r\nContent-Type: application/pdf"), &target()),
                check_head(&head("403 Forbidden\r\nContent-Type: text/html"), &target()),
                check_head(&head("302 Found\r\nLocation: /b\r\nContent-Type: text/plain"), &target())
            ],
            [
                Ok(ResponseKind::Document),
                Ok(ResponseKind::Document),
                Err("a document of the kind application/pdf".into()),
                Err("status 403".into()),
                Ok(ResponseKind::Redirect(Target::parse("https://news.example/b").unwrap()))
            ],
            "a page by its kind or without one named, no other document, and an asker sent on whatever kind the sending is"
        );
        assert!(TASK.contains("BACK") && TASK.contains("LINKS"));
    }
}

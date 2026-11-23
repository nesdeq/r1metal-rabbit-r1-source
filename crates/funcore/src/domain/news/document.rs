//! The stories of RSS, RDF and Atom documents, as plain lines, and the feeds a page of HTML names as its own.

use super::feed::Feed;
use super::fetch::{FeedError, FetchOutcome};
use super::stories::{NO_DATE, NO_SUMMARY, NO_TITLE, Story};
use crate::base::markup::Piece;
use crate::base::markup::Pieces;
use crate::base::markup::attribute;
use crate::base::markup::begins_tag;
use crate::base::markup::markup_length;
use crate::base::markup::unescape;
use crate::base::text::to_one_line;
use crate::codec::http::Target;
use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

const BYTE_ORDER_MARK: &[u8] = &[0xEF, 0xBB, 0xBF];
const LITERAL_TYPES: [&str; 2] = ["text", "xhtml"];
const LATIN_9: &str = "iso-8859-15";
const WESTERN: [&str; 4] = ["iso-8859-1", "latin1", "windows-1252", "cp1252"];
const WINDOWS_1252_START: u8 = 0x80;
const WINDOWS_1252_TABLE: [u16; 32] = [
    0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D,
    0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D,
    0x017E, 0x0178,
];
const LATIN_9_TABLE: [(u8, char); 8] = [
    (0xA4, '\u{20AC}'),
    (0xA6, '\u{160}'),
    (0xA8, '\u{161}'),
    (0xB4, '\u{17D}'),
    (0xB8, '\u{17E}'),
    (0xBC, '\u{152}'),
    (0xBD, '\u{153}'),
    (0xBE, '\u{178}'),
];
const PROLOG_BYTES: usize = 200;
const LINK: &str = "link";
const ALTERNATE: &str = "alternate";
const FEED_KINDS: [&str; 2] = ["application/rss+xml", "application/atom+xml"];

fn to_plain(text: &str) -> String {
    let text = unescape(text);
    let mut plain = String::with_capacity(text.len());
    let mut rest = &*text;
    while let Some(at) = rest.find('<') {
        plain.push_str(&rest[..at]);
        rest = &rest[at..];
        if !begins_tag(&rest[1..]) {
            plain.push('<');
            rest = &rest[1..];
            continue;
        }
        // A tag that nothing closes leaves the rest as text.
        let Some(length) = markup_length(rest) else {
            break;
        };
        plain.push(' ');
        rest = &rest[length..];
    }
    plain.push_str(rest);
    to_one_line(&plain)
}

fn to_text(text: &str, attributes: &str) -> String {
    match attribute(attributes, "type") {
        Some(kind) if LITERAL_TYPES.contains(&kind.as_ref()) => to_one_line(text),
        _ => to_plain(text),
    }
}

fn local_name(name: &str) -> Option<&str> {
    match name.split_once(':') {
        None => Some(name),
        Some(("atom" | "a10" | "rss", local)) => Some(local),
        Some(_) => None,
    }
}

#[derive(Default)]
struct Entry {
    title: Option<String>,
    link: Option<String>,
    alternate: Option<String>,
    guid: Option<String>,
    published: Option<String>,
    updated: Option<String>,
    summary: Option<String>,
    content: Option<String>,
}

impl Entry {
    fn add_element(&mut self, name: &str, attributes: &str, text: String) {
        let set_once = |slot: &mut Option<String>, value: String| {
            slot.get_or_insert(value);
        };
        match (local_name(name), name) {
            (Some("title"), _) => set_once(&mut self.title, to_text(&text, attributes)),
            (Some("link"), _) => match attribute(attributes, "href") {
                Some(href) => {
                    let rel = attribute(attributes, "rel");
                    let kind = attribute(attributes, "type");
                    let is_page =
                        rel.is_none_or(|rel| rel == "alternate") && kind.is_none_or(|kind| kind.contains("html"));
                    if is_page {
                        set_once(&mut self.alternate, href.trim().into());
                    }
                }
                None => set_once(&mut self.link, text.trim().into()),
            },
            (Some("guid"), _) if attribute(attributes, "isPermaLink").is_none_or(|value| value != "false") => {
                set_once(&mut self.guid, text.trim().into());
            }
            (Some("pubDate" | "published" | "issued"), _) => set_once(&mut self.published, text.trim().into()),
            (Some("updated" | "modified"), _) | (_, "dc:date") => set_once(&mut self.updated, text.trim().into()),
            (Some("description" | "summary"), _) => set_once(&mut self.summary, to_text(&text, attributes)),
            (Some("content"), _) | (_, "content:encoded") => {
                // What stands elsewhere, or is no text, is no summary.
                let is_text = attribute(attributes, "src").is_none()
                    && attribute(attributes, "type").is_none_or(|kind| kind.contains("text") || kind.contains("html"));
                if is_text {
                    set_once(&mut self.content, to_text(&text, attributes));
                }
            }
            _ => {}
        }
    }

    fn into_story(self, source: &str, root: &str) -> Story {
        let permanent = self.guid.filter(|guid| guid.starts_with("http"));
        let summary = self.summary.or(self.content);
        let link = self.link.filter(|link| !link.is_empty()).or(self.alternate).or(permanent).unwrap_or_default();
        Story {
            title: self.title.unwrap_or_else(|| NO_TITLE.into()),
            link: if link.starts_with('/') && !link.starts_with("//") { [root, &link].concat() } else { link },
            published: [self.published, self.updated]
                .into_iter()
                .flatten()
                .find(|date| !date.is_empty())
                .unwrap_or_else(|| NO_DATE.into()),
            summary: summary.unwrap_or_else(|| NO_SUMMARY.into()),
            source: source.into(),
        }
    }
}

fn read_entry(pieces: &mut Pieces<'_>, source: &str, root: &str) -> Story {
    let mut entry = Entry::default();
    // The element that stands directly in the entry and is being read, with the text in it so far.
    let mut element = None::<(&str, &str, String)>;
    for piece in pieces {
        match piece {
            Piece::Close(name) if matches!(local_name(name), Some("item" | "entry")) => break,
            Piece::Close(name) => match element.take() {
                Some((tag, attributes, text)) if tag == name => entry.add_element(tag, attributes, text),
                // Markup inside an element parts its words as a tag would.
                Some((tag, attributes, mut text)) => {
                    text.push(' ');
                    element = Some((tag, attributes, text));
                }
                None => {}
            },
            Piece::Open { name, attributes, is_empty } => match &mut element {
                Some((_, _, text)) => text.push(' '),
                None if is_empty => entry.add_element(name, attributes, String::new()),
                None => element = Some((name, attributes, String::new())),
            },
            Piece::Text(content) => element.iter_mut().for_each(|(_, _, text)| text.push_str(&content)),
        }
    }
    entry.into_story(source, root)
}

fn western_char(byte: u8) -> char {
    match byte.checked_sub(WINDOWS_1252_START).and_then(|at| WINDOWS_1252_TABLE.get(usize::from(at))) {
        Some(&code) => char::from_u32(u32::from(code)).unwrap_or(char::REPLACEMENT_CHARACTER),
        None => char::from(byte),
    }
}

fn latin_9_char(byte: u8) -> char {
    LATIN_9_TABLE.iter().find(|(code, _)| *code == byte).map_or(char::from(byte), |(_, character)| *character)
}

fn decode(body: &[u8]) -> Cow<'_, str> {
    let body = body.strip_prefix(BYTE_ORDER_MARK).unwrap_or(body);
    let prolog = String::from_utf8_lossy(&body[..body.len().min(PROLOG_BYTES)]).to_ascii_lowercase();
    let encoding = prolog.split_once("encoding=").map(|(_, rest)| rest.trim_start_matches(['"', '\'']));
    let to_char: fn(u8) -> char = match encoding {
        Some(encoding) if encoding.starts_with(LATIN_9) => latin_9_char,
        Some(encoding) if WESTERN.iter().any(|name| encoding.starts_with(name)) => western_char,
        _ => return String::from_utf8_lossy(body),
    };
    Cow::Owned(body.iter().map(|&byte| to_char(byte)).collect())
}

// The stories of the document read at `target` for `feed`: a link that begins at the root is of the server the document was read at.
fn parse_stories(body: &[u8], feed: Feed<'_>, target: &Target) -> Vec<Story> {
    let (source, root) = (feed.source(), target.to_origin());
    let document = decode(body);
    let mut pieces = Pieces::new(&document);
    let mut stories = Vec::new();
    while let Some(piece) = pieces.next() {
        if let Piece::Open { name, is_empty: false, .. } = piece
            && matches!(local_name(name), Some("item" | "entry"))
        {
            stories.push(read_entry(&mut pieces, &source, &root));
        }
    }
    stories
}

// The stories of a whole document; one without any is no feed's, whatever its server calls it.
fn parse_document(document: &[u8], feed: Feed<'_>, target: &Target) -> Result<Vec<Story>, FeedError> {
    let stories = parse_stories(document, feed, target);
    if stories.is_empty() { Err(FeedError::NoStories(document.len())) } else { Ok(stories) }
}

/// What the document read at `target` for `feed` comes to: its stories, or, where it tells none and `follows_pages`, the first feed it names as its own at another address, to be read in its place.
pub fn to_outcome(
    document: &[u8],
    feed: Feed<'_>,
    target: &Target,
    follows_pages: bool,
) -> Result<FetchOutcome, FeedError> {
    let stories = parse_document(document, feed, target);
    if follows_pages
        && stories.is_err()
        && let Some(address) = find_feeds(document, target).into_iter().next()
        && let Some(next) = Target::parse(&address).filter(|next| next != target)
    {
        return Ok(FetchOutcome::Named(address, next));
    }
    Ok(FetchOutcome::Stories(stories?, document.len()))
}

// The addresses of the feeds a page fetched at `target` names as its own, in its order, each once: every `link` that is an alternate of the page as RSS or Atom.
fn find_feeds(page: &[u8], target: &Target) -> Vec<String> {
    let document = decode(page);
    let mut feeds: Vec<String> = Vec::new();
    for piece in Pieces::new(&document) {
        let Piece::Open { name, attributes, .. } = piece else { continue };
        let is_of = |key: &str, values: &[&str]| {
            attribute(attributes, key)
                .is_some_and(|value| values.iter().any(|known| value.trim().eq_ignore_ascii_case(known)))
        };
        if !name.eq_ignore_ascii_case(LINK) || !is_of("rel", &[ALTERNATE]) || !is_of("type", &FEED_KINDS) {
            continue;
        }
        let address =
            attribute(attributes, "href").and_then(|href| target.resolve(&href)).map(|feed| feed.to_address());
        feeds.extend(address.filter(|address| !feeds.contains(address)));
    }
    feeds
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/" xmlns:media="http://search.yahoo.com/mrss/" xmlns:dc="http://purl.org/dc/elements/1.1/">
<channel>
<title>The Feed</title>
<link>https://example.com/</link>
<description>All of it</description>
<item>
  <title>Chips &amp; <![CDATA[<em>ships</em>]]></title>
  <link> https://example.com/a?x=1&amp;y=2 </link>
  <guid isPermaLink="false">a-1</guid>
  <pubDate>Tue, 13 Oct 2026 06:00:00 GMT</pubDate>
  <dc:date>2026-10-13T06:30:00Z</dc:date>
  <media:title>A picture's title</media:title>
  <media:description>A picture's words</media:description>
  <description>&lt;p&gt;Fabs are full &amp;amp; prices rise&amp;nbsp;by &lt;b&gt;12&amp;#37;&lt;/b&gt;.&lt;/p&gt;
     &lt;img src="x.png"/&gt;</description>
  <content:encoded><![CDATA[<p>The whole text.</p>]]></content:encoded>
</item>
<item>
  <guid>https://example.com/b</guid>
  <content:encoded><![CDATA[<p>Only the&nbsp;text &mdash; no <a href="x">summary</a>.</p>]]></content:encoded>
</item>
<item><title/></item>
</channel></rss>"#;

    fn at(address: &str) -> Target {
        Target::parse(address).unwrap()
    }

    // The stories of a document read for the feed of that line, at an address of its own.
    fn read(body: &[u8], line: &str) -> Vec<Story> {
        parse_stories(body, Feed::parse(line), &at("https://read.example/feed"))
    }

    #[test]
    fn rss_items() {
        let stories = read(RSS.as_bytes(), "example.com");
        assert_eq!(stories.len(), 3, "the channel's own title and link are no story");
        assert_eq!(
            stories[0],
            Story {
                title: "Chips & ships".into(),
                link: "https://example.com/a?x=1&y=2".into(),
                published: "Tue, 13 Oct 2026 06:00:00 GMT".into(),
                summary: "Fabs are full & prices rise by 12% .".into(),
                source: "example.com".into(),
            },
            "the date of publication before the other, the description before the whole text, nothing of the pictures' family"
        );
        assert_eq!(
            stories[1],
            Story {
                title: "No title".into(),
                link: "https://example.com/b".into(),
                published: "No date".into(),
                summary: "Only the text \u{2014} no summary .".into(),
                source: "example.com".into(),
            },
            "a permanent guid is a link, the whole text a summary where there is no other"
        );
        assert_eq!(
            (stories[2].title.as_str(), stories[2].link.as_str(), stories[2].summary.as_str()),
            ("", "", "No summary available"),
            "an empty title is one; what an entry does not tell is said so"
        );
    }

    #[test]
    fn atom_and_rdf_entries() {
        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Feed</title>
<entry>
  <title type="html">One &lt;i&gt;two&lt;/i&gt;</title>
  <link rel="self" href="https://example.org/self"/>
  <link rel="alternate" type="text/html" href="https://example.org/one?a=1&amp;b=2"/>
  <link rel="enclosure" type="audio/mpeg" href="https://example.org/one.mp3"/>
  <id>tag:example.org,2026:1</id>
  <updated>2026-10-13T07:00:00Z</updated>
  <published>2026-10-13T06:00:00Z</published>
  <author><name>Somebody</name><title>Dr.</title></author>
  <content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml"><p>First.</p><p>Second &amp; last.</p></div></content>
</entry>
<entry><title>Two</title><link href='https://example.org/two'/><updated>2026-10-12T00:00:00Z</updated><summary>Short.</summary><content type="html">&lt;p&gt;Long.&lt;/p&gt;</content></entry>
<entry><title>Three</title><content src="https://example.org/three.pdf" type="application/pdf"/></entry>
<entry><title type="text">Why &amp;lt;b&amp;gt; and 1 &lt; 2</title><summary type="text">AT&amp;amp;T &lt;and&gt;  R&amp;D</summary></entry>
</feed>"#;
        let stories = read(atom.as_bytes(), "example.org");
        assert_eq!(
            stories.iter().map(|story| story.title.as_str()).collect::<Vec<_>>(),
            ["One two", "Two", "Three", "Why &lt;b&gt; and 1 < 2"],
            "an author's title is not the entry's"
        );
        assert_eq!(
            stories[3].summary, "AT&amp;T <and> R&D",
            "a text that says it is text names its characters once and holds no tags, whatever it looks like"
        );
        assert_eq!(
            (stories[0].link.as_str(), stories[0].published.as_str(), stories[0].summary.as_str()),
            ("https://example.org/one?a=1&b=2", "2026-10-13T06:00:00Z", "First. Second & last."),
            "the link a reader follows, and the words of the content without its markup"
        );
        assert_eq!(
            (stories[1].link.as_str(), stories[1].published.as_str(), stories[1].summary.as_str()),
            ("https://example.org/two", "2026-10-12T00:00:00Z", "Short.")
        );
        assert_eq!(stories[2].summary, "No summary available", "content that lies elsewhere is no summary");
        let rdf = r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns="http://purl.org/rss/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/">
<channel rdf:about="https://example.net/"><title>Feed</title><items><rdf:Seq><rdf:li rdf:resource="https://example.net/1"/></rdf:Seq></items></channel>
<item rdf:about="https://example.net/1"><title>Only one</title><link>https://example.net/1</link><description>It &gt; that.</description><dc:date>2026-10-13T05:00:00+00:00</dc:date></item>
</rdf:RDF>"#;
        assert_eq!(
            read(rdf.as_bytes(), "example.net"),
            [Story {
                title: "Only one".into(),
                link: "https://example.net/1".into(),
                published: "2026-10-13T05:00:00+00:00".into(),
                summary: "It > that.".into(),
                source: "example.net".into(),
            }]
        );
    }

    #[test]
    fn plain_line() {
        assert_eq!(to_plain("  a\n\tb \u{A0} c  "), "a b c");
        assert_eq!(to_plain("<p>a</p><p>b</p>"), "a b");
        assert_eq!(
            to_plain("1 < 2 and 3 > 2, <> too, <b>bold</b><!-- x --></p><?y?>"),
            "1 < 2 and 3 > 2, <> too, bold",
            "a tag begins with a letter, a slash, a bang or a question behind its bracket, and a bracket before anything else is text"
        );
        assert_eq!(to_plain("a < b"), "a < b", "a bracket that nothing closes stays");
        assert_eq!(
            to_plain("x <b y <c z <d"),
            "x <b y <c z <d",
            "and a tag that nothing closes leaves the rest as it is written"
        );
        assert_eq!(to_plain("S&amp;P 500 &lt; 4000 and VIX &gt; 30"), "S&P 500 < 4000 and VIX > 30");
        assert_eq!(
            to_plain("&lt;b&gt;bold&lt;/b&gt; &amp;amp; &#8217;&#x2019;&euro;&Uuml;&yuml;"),
            "bold &amp; \u{2019}\u{2019}\u{20AC}\u{DC}\u{FF}"
        );
        assert_eq!(
            to_plain("AT&T; R&D &unknown; &#xZZ; &"),
            "AT&T; R&D &unknown; &#xZZ; &",
            "a name that is none stays as written"
        );
        assert_eq!(
            to_plain("<a title=\"a > b\">x</a> y <!-- c > d -->z"),
            "x y z",
            "a tag ends behind a quoted value that holds a bracket, and a comment at its own close"
        );
        assert_eq!(to_plain("&#1114112;"), "\u{FFFD}", "a number that is no character");
        assert_eq!(to_plain(""), "");
    }

    #[test]
    fn partial_and_foreign_documents() {
        assert!(read(b"", "x").is_empty() && read(b"<html><body>404</body></html>", "x").is_empty());
        assert!(read(&[0xFF, 0xFE, 0x00, b'<'], "x").is_empty());
        let partial = &RSS.as_bytes()[..RSS.find("<pubDate>").unwrap() + 20];
        let stories = read(partial, "x");
        assert_eq!(
            (stories.len(), stories[0].title.as_str()),
            (1, "Chips & ships"),
            "what was read of an entry is told"
        );
        assert_eq!(
            read(b"<item><title>a</title><link>b", "x")[0].link,
            "",
            "an element that never closes tells nothing"
        );
        assert_eq!(
            read(b"<item><title a=\"1>2\">t</title></item>", "x")[0].title,
            "t",
            "a quoted value may hold a bracket"
        );
        let bare = read(b"<item><title>Stocks < 5% today</title><link>https://x/1</link></item>", "x");
        assert_eq!(
            (bare[0].title.as_str(), bare[0].link.as_str()),
            ("Stocks < 5% today", "https://x/1"),
            "a bracket before no name is text in the document too, and costs no element"
        );
        let latin = b"<?xml version='1.0' encoding='ISO-8859-1'?><rss><item><title>K\xE4se</title></item></rss>";
        assert_eq!(read(latin, "x")[0].title, "K\u{E4}se", "an encoding of one byte a character is read as such");
        let with_mark = [BYTE_ORDER_MARK, "<rss><item><title>K\u{E4}se</title></item></rss>".as_bytes()].concat();
        assert_eq!(read(&with_mark, "x")[0].title, "K\u{E4}se");
        let to_title = |encoding: &str, title: &[u8]| {
            let head = alloc::format!("<?xml version='1.0' encoding='{encoding}'?><rss><item><title>");
            read(&[head.as_bytes(), title, b"</title></item></rss>"].concat(), "x")[0].title.clone()
        };
        let windows_text = b"\x93Quoted\x94 \x96 \x80 5 \x85 \x8A";
        assert_eq!(
            [to_title("windows-1252", windows_text), to_title("ISO-8859-1", windows_text)],
            ["\u{201C}Quoted\u{201D} \u{2013} \u{20AC} 5 \u{2026} \u{160}"; 2],
            "Windows' quotes, dashes and signs, which browsers read in what calls itself Latin-1 too"
        );
        assert_eq!(to_title("iso-8859-15", b"\xA4 \xBD \xE4"), "\u{20AC} \u{153} \u{E4}", "Latin-9's own characters");
        assert_eq!(
            to_title("cp1252", b"\x81"),
            "\u{81}",
            "a byte Windows gives no character is the control of its number"
        );
    }

    #[test]
    fn unclosed_markup() {
        let sloppy = b"<rss><channel><item><title>One</title><description>a<br>b</p> c</description><link>https://x/1</link></item><item><title>Two</title></item><item><title>Three<br></item><item><title>Four</title></item></channel></rss>";
        let stories = read(sloppy, "x");
        assert_eq!(
            stories.iter().map(|story| story.title.as_str()).collect::<Vec<_>>(),
            ["One", "Two", "No title", "Four"],
            "every entry is one, and an element the entry's end cuts short tells nothing"
        );
        assert_eq!(
            (stories[0].summary.as_str(), stories[0].link.as_str()),
            ("a b c", "https://x/1"),
            "an element ends at its own close, whatever was left open in it or closed without being open"
        );
    }

    #[test]
    fn source_and_relative_link() {
        let document = b"<rss><item><link>/2026/a/</link></item><item><link>//cdn.example/b</link></item></rss>";
        let feed = Feed::parse("https://harpers.org/feed/");
        let stories = parse_stories(document, feed, &at("https://harpers.org/feed/"));
        assert_eq!(
            (stories[0].link.as_str(), stories[0].source.as_str(), stories[1].link.as_str()),
            ("https://harpers.org/2026/a/", "harpers.org", "//cdn.example/b")
        );
        let moved = parse_stories(document, feed, &at("https://feeds.example.net:8443/harpers"));
        assert_eq!(
            (moved[0].link.as_str(), moved[0].source.as_str(), moved[1].link.as_str()),
            ("https://feeds.example.net:8443/2026/a/", "harpers.org", "//cdn.example/b"),
            "read where a redirect or a page sent the asker, a link from the root is of that server, the source the feed's still"
        );
    }

    #[test]
    fn feeds_a_page_names() {
        let page = br#"<!doctype html><html><head><title>Motor</title>
            <link rel="stylesheet" href="/a.css"><link rel="alternate" hreflang="de" href="/de/">
            <link rel="alternate" type="application/rss+xml" title="All" href="/rss/all.xml">
            <LINK rel="Alternate" type="Application/Atom+XML" href="https://feeds.example.net/f1?x=1&amp;y=2"/>
            <link rel="alternate" type="application/rss+xml" href="/rss/all.xml">
            <link rel="alternate" type="application/rss+xml" href="http://plain.example/rss">
            <link rel="alternate" type="application/json" href="/feed.json"></head><body><p>Words</p></body></html>"#;
        let target = Target::parse("https://motor.example/f1/news").unwrap();
        assert_eq!(
            find_feeds(page, &target),
            ["https://motor.example/rss/all.xml", "https://feeds.example.net/f1?x=1&y=2"],
            "its RSS and Atom alternates, each once, at their whole addresses: no stylesheet, no other language, nothing in the clear, no other kind"
        );
        assert!(find_feeds(RSS.as_bytes(), &target).is_empty() && find_feeds(b"", &target).is_empty());
    }

    #[test]
    fn page_sends_on_to_its_feed() {
        let page = br#"<html><head><link rel="alternate" type="application/rss+xml" href="/feed.xml"></head></html>"#;
        let bare = b"<html><body>Words</body></html>";
        let (feed, at_page) = (Feed::parse("x"), Target::parse("https://motor.example/f1").unwrap());
        let at_feed = Target::parse("https://motor.example/feed.xml").unwrap();
        assert_eq!(
            to_outcome(page, feed, &at_page, true),
            Ok(FetchOutcome::Named("https://motor.example/feed.xml".into(), at_feed.clone())),
            "a page that is no feed and names one sends the asker on to it"
        );
        assert_eq!(
            [to_outcome(page, feed, &at_page, false), to_outcome(page, feed, &at_feed, true)],
            [Err(FeedError::NoStories(page.len())), Err(FeedError::NoStories(page.len()))],
            "where pages are not followed, and where the page names the address it was read at, it is no feed"
        );
        assert_eq!(
            to_outcome(bare, feed, &at_page, true),
            Err(FeedError::NoStories(bare.len())),
            "nor one that names none"
        );
        let stories = to_outcome(RSS.as_bytes(), feed, &at_page, true);
        assert!(
            matches!(stories, Ok(FetchOutcome::Stories(stories, bytes)) if stories.len() == 3 && bytes == RSS.len()),
            "a feed gives its stories and its length"
        );
    }

    #[test]
    fn document_without_stories() {
        let (feed, target) = (Feed::parse("x"), at("https://read.example/feed"));
        assert_eq!(parse_document(RSS.as_bytes(), feed, &target).map(|stories| stories.len()), Ok(3));
        let page = b"<html><body>Just a moment...</body></html>";
        assert_eq!(
            [parse_document(b"", feed, &target), parse_document(page, feed, &target)],
            [Err(FeedError::NoStories(0)), Err(FeedError::NoStories(page.len()))]
        );
    }
}

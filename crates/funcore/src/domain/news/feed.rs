//! A rundown's feeds: their addresses and source names.

use alloc::format;
use alloc::string::String;

const FEEDBURNER: &str = "feedburner.com";
const WWW: &str = "www.";

/// A feed's address, and its source name where the address does not tell it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Feed<'a> {
    pub(super) address: &'a str,
    name: Option<&'a str>,
}

impl<'a> Feed<'a> {
    /// Parses a list line: an address, after an optional name and blank.
    pub fn parse(line: &'a str) -> Feed<'a> {
        let line = line.trim();
        match line.split_once(char::is_whitespace) {
            Some((name, address)) => Feed { address: address.trim_start(), name: Some(name) },
            None => Feed { address: line, name: None },
        }
    }

    /// The source name of the feed's stories.
    pub fn source(&self) -> String {
        self.name.map_or_else(|| source_of(self.address), String::from)
    }

    // The list line that parses to the feed.
    pub(super) fn to_line(self) -> String {
        match self.name {
            Some(name) => format!("{name} {}", self.address),
            None => self.address.into(),
        }
    }
}

// The lines of a feeds list.
pub(super) fn lines(list: &str) -> impl Iterator<Item = &str> {
    list.lines().map(str::trim).filter(|line| !line.is_empty())
}

fn split_address(address: &str) -> (&str, &str) {
    let authority_at = address.find("://").map_or(0, |at| at + 3);
    let path_at = address[authority_at..].find('/').map_or(address.len(), |at| authority_at + at);
    (&address[authority_at..path_at], address.get(path_at + 1..).unwrap_or_default())
}

fn source_of(address: &str) -> String {
    let (authority, path) = split_address(address);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = host_port.split(':').next().unwrap_or(host_port).to_ascii_lowercase();
    if host.contains(FEEDBURNER) {
        return path.trim_matches('/').split('/').next().unwrap_or_default().into();
    }
    let name = host.strip_prefix(WWW).unwrap_or(&host);
    match name.rmatch_indices('.').nth(1) {
        Some((dot, _)) => name[dot + 1..].into(),
        None => name.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn list_line() {
        let plain = Feed::parse("  https://www.heise.de/rss/heise-atom.xml \n");
        assert_eq!((plain.address, plain.source().as_str()), ("https://www.heise.de/rss/heise-atom.xml", "heise.de"));
        let with_name = Feed::parse("example.org   https://news.example.com/search?q=site:example.org+when:2d");
        assert_eq!(
            (with_name.address, with_name.source().as_str()),
            ("https://news.example.com/search?q=site:example.org+when:2d", "example.org")
        );
        assert_eq!(
            (plain.to_line(), with_name.to_line()),
            (
                "https://www.heise.de/rss/heise-atom.xml".into(),
                "example.org https://news.example.com/search?q=site:example.org+when:2d".into()
            ),
            "a line as a list holds it, its blanks as one"
        );
        assert_eq!(
            lines(" https://a.example/rss \n\n  \nb https://b.example/feed\n").collect::<Vec<_>>(),
            ["https://a.example/rss", "b https://b.example/feed"]
        );
    }

    #[test]
    fn source_names() {
        for (address, name) in [
            ("https://feeds.arstechnica.com/arstechnica/index", "arstechnica.com"),
            ("https://www.heise.de/rss/heise-atom.xml", "heise.de"),
            ("https://venturebeat.com/feed/", "venturebeat.com"),
            ("http://rss.slashdot.org/Slashdot/slashdotMain", "slashdot.org"),
            ("https://feeds.a.dj.com/rss/RSSMarketsMain.xml", "dj.com"),
            ("https://www.ecb.europa.eu/rss/press.html", "europa.eu"),
            ("https://feeds.feedburner.com/zerohedge/feed", "zerohedge"),
            ("https://feeds.feedburner.com/RockPaperShotgun", "RockPaperShotgun"),
            ("https://User:secret@WWW.Example.COM:8443/feed", "example.com"),
            ("localhost", "localhost"),
            ("", ""),
        ] {
            assert_eq!(source_of(address), name, "{address}");
        }
    }
}

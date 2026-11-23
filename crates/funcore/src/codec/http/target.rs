use alloc::format;
use alloc::string::{String, ToString};

const PORT: u16 = 443;
pub(crate) const SCHEME: &str = "https://";

fn is_plain(text: &str) -> bool {
    !text.chars().any(|character| character.is_control() || character == ' ')
}

// The host and port an authority names (RFC 3986, 3.2): an IPv6 address in brackets, which it leaves off, and the port `default` where none is written; none for what names no server.
pub(crate) fn parse_authority(authority: &str, default: u16) -> Option<(&str, u16)> {
    let (host, port) = match authority.strip_prefix('[') {
        Some(inner) => {
            let (host, tail) = inner.split_once(']')?;
            (host, if tail.is_empty() { None } else { Some(tail.strip_prefix(':')?) })
        }
        None => authority.rsplit_once(':').map_or((authority, None), |(host, port)| (host, Some(port))),
    };
    let is_bracketed = authority.starts_with('[');
    if host.is_empty() || host.contains('@') || !is_bracketed && host.contains(':') || !is_plain(host) {
        return None;
    }
    let port = match port {
        Some(port) if port.bytes().all(|digit| digit.is_ascii_digit()) => {
            port.parse().ok().filter(|port| *port != 0)?
        }
        Some(_) => return None,
        None => default,
    };
    Some((host, port))
}

/// A URL under TLS, taken apart.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    /// A name, or an IPv6 address without its brackets.
    pub host: String,
    pub port: u16,
    /// The path and query, as the request line takes them.
    pub path: String,
}

impl Target {
    /// Parses an `https` or `wss` URL; none for any other scheme or a URL with a user.
    pub fn parse(url: &str) -> Option<Target> {
        let (scheme, rest) = url.split_once("://")?;
        if !["https", "wss"].iter().any(|secure| scheme.eq_ignore_ascii_case(secure)) {
            return None;
        }
        let rest = rest.split_once('#').map_or(rest, |(address, _)| address);
        let (authority, path) = rest.find(['/', '?']).map_or((rest, ""), |at| rest.split_at(at));
        let (host, port) = parse_authority(authority, PORT)?;
        if !is_plain(path) {
            return None;
        }
        let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
        Some(Target { host: host.to_string(), port, path })
    }

    pub(super) fn is_same_server(&self, other: &Target) -> bool {
        (self.host.as_str(), self.port) == (other.host.as_str(), other.port)
    }

    pub(super) fn authority(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == PORT { host } else { format!("{host}:{}", self.port) }
    }

    // The scheme and authority of its URL, its origin (RFC 6454).
    pub(crate) fn to_origin(&self) -> String {
        format!("{SCHEME}{}", self.authority())
    }

    // The URL that parses to the target.
    pub(crate) fn to_address(&self) -> String {
        format!("{}{}", self.to_origin(), self.path)
    }

    pub(crate) fn resolve(&self, location: &str) -> Option<Target> {
        let location = location.trim();
        if location.is_empty() {
            return None;
        }
        if let Some(absolute) = Target::parse(location) {
            return Some(absolute);
        }
        // A colon before the first separator names a scheme this code does not speak.
        if location[..location.find(['/', '?', '#']).unwrap_or(location.len())].contains(':') {
            return None;
        }
        if let Some(rest) = location.strip_prefix("//") {
            return Target::parse(&format!("{SCHEME}{rest}"));
        }
        let base = self.path.split_once('?').map_or(self.path.as_str(), |(path, _)| path);
        let path = if location.starts_with('/') {
            location.to_string()
        } else if location.starts_with('?') {
            format!("{base}{location}")
        } else {
            format!("{}{location}", &base[..base.rfind('/').map_or(0, |at| at + 1)])
        };
        Target::parse(&format!("{SCHEME}{}{path}", self.authority()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: &str) -> Option<(String, u16, String)> {
        Target::parse(url).map(|target| (target.host, target.port, target.path))
    }

    #[test]
    fn parse_url() {
        assert_eq!(target("https://example.org/a/b?c=d#e"), Some(("example.org".into(), 443, "/a/b?c=d".into())));
        assert_eq!(
            target("HTTPS://Example.org:8080?x=1"),
            Some(("Example.org".into(), 8080, "/?x=1".into())),
            "a question without a path is asked of the root, however the scheme is written"
        );
        assert_eq!(target("wss://example.org/socket"), Some(("example.org".into(), 443, "/socket".into())));
        assert_eq!(target("https://[2001:db8::1]:8443/x"), Some(("2001:db8::1".into(), 8443, "/x".into())));
        assert_eq!(target("https://[2001:db8::1]"), Some(("2001:db8::1".into(), 443, "/".into())));
        assert_eq!(
            [target("http://example.org/"), target("HTTP://example.org:443/"), target("ws://192.0.2.7:9000/")],
            [None, None, None],
            "nothing in the clear"
        );
        for none in [
            "example.org",
            "ftp://example.org/",
            "https:///x",
            "https://user@example.org/",
            "https://example.org:0/",
            "https://example.org:port/",
            "https://example.org:+80/",
            "https://example.org:70000/",
            "https://[2001:db8::1/",
            "https://[2001:db8::1]x/",
            "https://exa mple.org/",
            "https://example.org/a b",
        ] {
            assert_eq!(target(none), None, "{none}");
        }
        let authority = |url: &str| Target::parse(url).map(|target| target.authority());
        assert_eq!(authority("https://example.org:443/"), Some("example.org".into()));
        assert_eq!(authority("https://example.org:80/"), Some("example.org:80".into()));
        assert_eq!(authority("wss://[2001:db8::1]:8443/"), Some("[2001:db8::1]:8443".into()));
        assert_eq!(
            Target::parse("https://[2001:db8::1]:8443/x?y").map(|target| target.to_address()).as_deref(),
            Some("https://[2001:db8::1]:8443/x?y"),
            "the URL a target was parsed from"
        );
        assert_eq!(
            Target::parse("WSS://example.org:443/x?y").map(|target| target.to_origin()).as_deref(),
            Some("https://example.org"),
            "its origin, without the path and HTTPS's own port"
        );
    }

    #[test]
    fn same_server() {
        let parsed = |url: &str| Target::parse(url).unwrap();
        let server = parsed("https://example.org/v1/responses");
        assert!(
            parsed("https://example.org:443/v1/audio?x=1").is_same_server(&server)
                && parsed("wss://example.org/v1/realtime").is_same_server(&server),
            "another path, HTTPS's own port written out, a WebSocket"
        );
        assert!(
            !parsed("https://example.org:8443/").is_same_server(&server)
                && !parsed("https://example.net/").is_same_server(&server),
            "another port, another host"
        );
    }

    #[test]
    fn resolve_location() {
        let base = Target::parse("https://example.org:8080/news/rss.xml?edition=1").unwrap();
        let bracketed = Target::parse("https://[2001:db8::1]/a/b").unwrap();
        let url_of = |base: &Target, location: &str| base.resolve(location).map(|target| target.to_address());
        for (location, url, reason) in [
            ("https://feeds.example.net/all?x=1#top", "https://feeds.example.net/all?x=1", "a URL whole"),
            ("//cdn.example.net/all", "https://cdn.example.net/all", "an address without its scheme, under TLS"),
            ("/feed/", "https://example.org:8080/feed/", "the server it was asked of"),
            ("atom.xml", "https://example.org:8080/news/atom.xml", "beside what was asked"),
            ("?edition=2", "https://example.org:8080/news/rss.xml?edition=2", "another question"),
            (
                " /go?to=https://example.net/x ",
                "https://example.org:8080/go?to=https://example.net/x",
                "an address inside a question is no address to go to",
            ),
        ] {
            assert_eq!(url_of(&base, location).as_deref(), Some(url), "{reason}");
        }
        assert_eq!(url_of(&bracketed, "c").as_deref(), Some("https://[2001:db8::1]/a/c"));
        assert_eq!(
            [url_of(&bracketed, "http://example.org/feed"), url_of(&bracketed, "ws://example.org/")],
            [None, None],
            "an answer sends nobody into the clear"
        );
        assert_eq!(url_of(&bracketed, "wss://example.org/").as_deref(), Some("https://example.org/"));
        assert_eq!(
            ["", "ftp://example.org/x", "mailto:someone@example.org", "/a b", "//"]
                .map(|location| url_of(&base, location)),
            [None, None, None, None, None],
            "nowhere, a URL of another kind, and what is no address lead nowhere"
        );
    }
}

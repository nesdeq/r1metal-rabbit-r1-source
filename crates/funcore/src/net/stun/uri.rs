use super::Transport;
use crate::codec::http::parse_authority;
use alloc::string::{String, ToString};

// The ports of STUN and TURN, in the clear and under TLS (RFC 8489, 18.6; RFC 8656, 5).
const PORT: u16 = 3478;
const TLS_PORT: u16 = 5349;
const TRANSPORT_KEY: &str = "transport=";

// A STUN or TURN server named by its URI (RFC 7064, 7065).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Uri {
    // A name, or an IP address without its brackets.
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) transport: Transport,
    // True for a relay, false for a STUN server.
    pub(crate) is_relay: bool,
}

impl Uri {
    // Reads a `stun:`, `stuns:`, `turn:` or `turns:` URI; none for another, or a transport these do not take.
    pub(crate) fn parse(text: &str) -> Option<Uri> {
        let (scheme, rest) = text.trim().split_once(':')?;
        let scheme = scheme.to_ascii_lowercase();
        let (is_relay, is_secure) = match scheme.as_str() {
            "stun" => (false, false),
            "stuns" => (false, true),
            "turn" => (true, false),
            "turns" => (true, true),
            _ => return None,
        };
        let (authority, query) = rest.split_once('?').unwrap_or((rest, ""));
        let asked = match query {
            "" => None,
            _ if is_relay => {
                let (key, value) = query.split_at_checked(TRANSPORT_KEY.len())?;
                key.eq_ignore_ascii_case(TRANSPORT_KEY).then_some(())?;
                Some(value.to_ascii_lowercase())
            }
            _ => return None,
        };
        let transport = match (is_secure, asked.as_deref()) {
            (false, None | Some("udp")) => Transport::Udp,
            (false, Some("tcp")) => Transport::Tcp,
            (true, None | Some("tcp")) => Transport::Tls,
            _ => return None,
        };
        let (host, port) = parse_authority(authority, if is_secure { TLS_PORT } else { PORT })?;
        Some(Uri { host: host.to_string(), port, transport, is_relay })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Option<(String, u16, Transport, bool)> {
        Uri::parse(text).map(|uri| (uri.host, uri.port, uri.transport, uri.is_relay))
    }

    #[test]
    fn server_uris() {
        assert_eq!(
            read("turn:turn.example.org:80?transport=udp"),
            Some(("turn.example.org".into(), 80, Transport::Udp, true))
        );
        assert_eq!(
            read("TURN:192.0.2.9?Transport=TCP"),
            Some(("192.0.2.9".into(), 3478, Transport::Tcp, true)),
            "case aside"
        );
        assert_eq!(
            read("turns:turn.example.org:443?transport=tcp"),
            Some(("turn.example.org".into(), 443, Transport::Tls, true))
        );
        assert_eq!(
            read("turns:[2001:db8::9]"),
            Some(("2001:db8::9".into(), 5349, Transport::Tls, true)),
            "an IPv6 address"
        );
        assert_eq!(read("turn:[2001:db8::9]:3479"), Some(("2001:db8::9".into(), 3479, Transport::Udp, true)));
        assert_eq!(read("stun:stun.example.org"), Some(("stun.example.org".into(), 3478, Transport::Udp, false)));
        assert_eq!(read("stuns:stun.example.org"), Some(("stun.example.org".into(), 5349, Transport::Tls, false)));
        for none in [
            "http://example.org",
            "turn:",
            "turn:example.org:0",
            "turn:example.org?transport=sctp",
            "turns:example.org?transport=udp",
            "stun:example.org?transport=tcp",
            "turn:example.org?proto=udp",
            "turn:user@example.org",
            "turn:2001:db8::9",
        ] {
            assert_eq!(read(none), None, "{none}");
        }
    }
}

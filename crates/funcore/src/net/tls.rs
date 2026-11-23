//! TLS as the device speaks it: what a connection is for, what each purpose offers and trusts, and how a connection is told in the log.

use crate::codec::http;
use alloc::format;
use alloc::string::String;

const HTTP: [&[u8]; 1] = [http::ALPN];

/// What a TLS connection is for; each has one configuration for the process, so a server met again resumes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    Names,
    Http,
    Signal,
    // A call's relay or calling server reached over a stream.
    Relay,
}

/// The root certificates a purpose trusts: Mozilla's, or Signal's two alone (`signal::service::ROOTS`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Roots {
    Public,
    Signal,
}

/// The versions of TLS a purpose speaks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Versions {
    // 1.3, and 1.2 where a server speaks no later one.
    Both,
    Tls13,
}

impl Purpose {
    pub const ALL: [Purpose; 4] = [Purpose::Names, Purpose::Http, Purpose::Signal, Purpose::Relay];

    /// The protocols offered by ALPN: none to the resolver or a call's relay, HTTP/1.1 to every other server.
    pub const fn protocols(self) -> &'static [&'static [u8]] {
        match self {
            Purpose::Names | Purpose::Relay => &[],
            Purpose::Http | Purpose::Signal => &HTTP,
        }
    }

    pub const fn roots(self) -> Roots {
        match self {
            Purpose::Names | Purpose::Http | Purpose::Relay => Roots::Public,
            Purpose::Signal => Roots::Signal,
        }
    }

    pub const fn versions(self) -> Versions {
        match self {
            Purpose::Names | Purpose::Http | Purpose::Relay => Versions::Both,
            Purpose::Signal => Versions::Tls13,
        }
    }
}

/// The log text of a TLS connection: the library's name of its version, and how the server took it.
pub fn to_text(version: Option<String>, is_resumed: bool) -> String {
    let handshake = if is_resumed { "known to the server from before" } else { "greeted in full" };
    format!("{}, {handshake}", version.as_deref().unwrap_or("TLS"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purpose_terms() {
        let terms = Purpose::ALL.map(|purpose| (purpose.protocols(), purpose.roots(), purpose.versions()));
        assert_eq!(
            terms,
            [
                (&[][..], Roots::Public, Versions::Both),
                (&[&b"http/1.1"[..]][..], Roots::Public, Versions::Both),
                (&[&b"http/1.1"[..]][..], Roots::Signal, Versions::Tls13),
                (&[][..], Roots::Public, Versions::Both)
            ],
            "the resolver and a call's relay offer no protocol; Signal's hosts are trusted under its roots alone, over TLS 1.3"
        );
        assert!(Purpose::ALL.iter().enumerate().all(|(at, purpose)| *purpose as usize == at), "each by its place");
        assert_eq!(
            [to_text(Some("TLSv1_3".into()), true), to_text(None, false)],
            ["TLSv1_3, known to the server from before", "TLS, greeted in full"]
        );
    }
}

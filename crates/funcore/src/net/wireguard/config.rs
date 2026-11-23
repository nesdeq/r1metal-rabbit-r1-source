//! A tunnel's config: read from the text `wg-quick` takes, and kept in lines of the firmware's own.

use super::OVERHEAD_BYTES;
use super::noise::Keys;
use crate::base::code::{base64, unbase64};
use crate::base::lines::value;
use crate::crypto::{KEY_BYTES, X25519};
use alloc::format;
use alloc::string::String;
use core::net::Ipv4Addr;
use core::time::Duration;

/// The file a kept config lies in.
pub const FILE: &str = "tunnel";
const INTERFACE: &str = "[interface]";
const PEER: &str = "[peer]";
const COMMENT: char = '#';
const EVERYTHING: &str = "0.0.0.0/0";
// The owner's bound on what a tunnel carries in one packet.
const MOST_MTU: usize = 1384;
// What every host of IPv4 takes whole (RFC 791).
const LEAST_MTU: usize = 576;
const PRIVATE_KEY: &str = "private";
const ADDRESS_KEY: &str = "address";
const DNS_KEY: &str = "dns";
const MTU_KEY: &str = "mtu";
const PEER_KEY: &str = "peer";
const PRESHARED_KEY: &str = "preshared";
const ENDPOINT_KEY: &str = "endpoint";
const KEEPALIVE_KEY: &str = "keepalive";

/// Why a text is no config the firmware takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flaw {
    // No text of an interface and one peer with their keys and the peer's address.
    NotAConfig,
    // No IPv4 address for this end of the tunnel.
    NoAddress,
    // No IPv4 resolver, which the lookups of the tunnel's side go to.
    NoResolver,
    // The peer does not take every address, so part of the traffic would have no way.
    NotWhole,
    // A tunnel that carries less than every host of IPv4 takes whole.
    TooNarrow,
    // An address of the tunnel's that the tunnel's owner has in use on another link.
    Clash,
}

impl Flaw {
    // The words of a failed answer to a config handed over.
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Flaw::NotAConfig => "no config of one interface and one peer",
            Flaw::NoAddress => "the config has no IPv4 address",
            Flaw::NoResolver => "the config has no IPv4 DNS",
            Flaw::NotWhole => "the config's AllowedIPs lack 0.0.0.0/0",
            Flaw::TooNarrow => "the config's MTU is less than 576",
            Flaw::Clash => "the config's address or DNS is one of the link to the host",
        }
    }
}

// Where the peer is reached: a name or an address, and a port.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Endpoint {
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl Endpoint {
    fn parse(text: &str) -> Option<Endpoint> {
        let (host, port) = text.rsplit_once(':')?;
        let host = host.strip_prefix('[').and_then(|host| host.strip_suffix(']')).unwrap_or(host);
        let is_plain = !host.is_empty() && !host.contains(char::is_whitespace);
        is_plain.then(|| Some(Endpoint { host: host.into(), port: port.parse().ok()? })).flatten()
    }

    fn to_text(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// A tunnel's config; its `Debug` tells no key.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    private: [u8; KEY_BYTES],
    pub(crate) address: Ipv4Addr,
    pub(crate) resolver: Ipv4Addr,
    mtu: Option<u16>,
    peer: [u8; KEY_BYTES],
    preshared: Option<[u8; KEY_BYTES]>,
    pub(crate) endpoint: Endpoint,
    keepalive: Option<u16>,
}

impl core::fmt::Debug for Config {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Config").field("endpoint", &self.endpoint).finish_non_exhaustive()
    }
}

fn parse_key(text: &str) -> Option<[u8; KEY_BYTES]> {
    unbase64(text.trim())?.try_into().ok()
}

fn entries(list: &str) -> impl Iterator<Item = &str> {
    list.split(',').map(str::trim).filter(|entry| !entry.is_empty())
}

// The first IPv4 address of a list, each entry with the length of its prefix or without.
fn first_ipv4(list: &str) -> Option<Ipv4Addr> {
    entries(list).find_map(|entry| entry.split('/').next()?.parse().ok())
}

// The value of a key of one section, whatever its case and the spaces about its `=`.
fn find<'a>(section: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    section.iter().find(|(name, _)| name.eq_ignore_ascii_case(key)).map(|(_, value)| *value)
}

impl Config {
    /// Reads the text `wg-quick` takes: one interface, one peer.
    pub fn parse(text: &str) -> Result<Config, Flaw> {
        let (mut interface, mut peers, mut section) = (alloc::vec::Vec::new(), alloc::vec::Vec::new(), None);
        for line in text.lines().map(|line| line.split(COMMENT).next().unwrap_or_default().trim()) {
            if line.eq_ignore_ascii_case(INTERFACE) {
                section = Some(false);
            } else if line.eq_ignore_ascii_case(PEER) {
                section = Some(true);
                peers.push(alloc::vec::Vec::new());
            } else if let (Some(is_peer), Some((key, value))) = (section, line.split_once('=')) {
                let entry = (key.trim(), value.trim());
                match peers.last_mut().filter(|_| is_peer) {
                    Some(peer) => peer.push(entry),
                    None => interface.push(entry),
                }
            } else if !line.is_empty() {
                return Err(Flaw::NotAConfig);
            }
        }
        let [peer] = &peers[..] else {
            return Err(Flaw::NotAConfig);
        };
        let key = |section: &[(&str, &str)], name: &str| find(section, name).and_then(parse_key);
        let endpoint = find(peer, "Endpoint").and_then(Endpoint::parse);
        let (Some(private), Some(public), Some(endpoint)) =
            (key(&interface, "PrivateKey"), key(peer, "PublicKey"), endpoint)
        else {
            return Err(Flaw::NotAConfig);
        };
        let preshared = match find(peer, "PresharedKey") {
            Some(text) => Some(parse_key(text).ok_or(Flaw::NotAConfig)?),
            None => None,
        };
        let number = |section: &[(&str, &str)], name: &str| match find(section, name) {
            Some(text) => text
                .parse::<u16>()
                .map(|number| Some(number).filter(|number| *number > 0))
                .map_err(|_| Flaw::NotAConfig),
            None => Ok(None),
        };
        let config = Config {
            private,
            address: find(&interface, "Address").and_then(first_ipv4).ok_or(Flaw::NoAddress)?,
            resolver: find(&interface, "DNS").and_then(first_ipv4).ok_or(Flaw::NoResolver)?,
            mtu: number(&interface, "MTU")?,
            peer: public,
            preshared,
            endpoint,
            keepalive: number(peer, "PersistentKeepalive")?,
        };
        if !find(peer, "AllowedIPs").is_some_and(|list| entries(list).any(|entry| entry == EVERYTHING)) {
            return Err(Flaw::NotWhole);
        }
        if config.mtu.is_some_and(|mtu| usize::from(mtu) < LEAST_MTU) {
            return Err(Flaw::TooNarrow);
        }
        Ok(config)
    }

    // Reads a kept config; none for what `to_text` does not write.
    pub(crate) fn parse_kept(text: &str) -> Option<Config> {
        let key = |name: &str| value(text, name).and_then(parse_key);
        let number = |name: &str| value(text, name).map(|number| number.parse::<u16>().ok());
        Some(Config {
            private: key(PRIVATE_KEY)?,
            address: value(text, ADDRESS_KEY)?.parse().ok()?,
            resolver: value(text, DNS_KEY)?.parse().ok()?,
            mtu: number(MTU_KEY).map_or(Some(None), |mtu| mtu.map(Some))?,
            peer: key(PEER_KEY)?,
            preshared: value(text, PRESHARED_KEY).map_or(Some(None), |text| parse_key(text).map(Some))?,
            endpoint: Endpoint::parse(value(text, ENDPOINT_KEY)?)?,
            keepalive: number(KEEPALIVE_KEY).map_or(Some(None), |seconds| seconds.map(Some))?,
        })
    }

    /// The kept file's content.
    pub fn to_text(&self) -> String {
        let mut text = format!(
            "{PRIVATE_KEY} {}\n{ADDRESS_KEY} {}\n{DNS_KEY} {}\n",
            base64(&self.private),
            self.address,
            self.resolver
        );
        if let Some(mtu) = self.mtu {
            text.push_str(&format!("{MTU_KEY} {mtu}\n"));
        }
        text.push_str(&format!("{PEER_KEY} {}\n", base64(&self.peer)));
        if let Some(preshared) = &self.preshared {
            text.push_str(&format!("{PRESHARED_KEY} {}\n", base64(preshared)));
        }
        text.push_str(&format!("{ENDPOINT_KEY} {}\n", self.endpoint.to_text()));
        if let Some(seconds) = self.keepalive {
            text.push_str(&format!("{KEEPALIVE_KEY} {seconds}\n"));
        }
        text
    }

    /// The keys of the tunnel's two ends; none where the library gives this end's public key none.
    pub fn to_keys(&self, crypto: &impl X25519) -> Option<Keys> {
        Some(Keys {
            private: self.private,
            public: crypto.public_of(&self.private)?,
            peer: self.peer,
            preshared: self.preshared.unwrap_or_default(),
        })
    }

    /// How often the tunnel is kept alive where nothing else is sent, where the config asks for it.
    pub fn keepalive(&self) -> Option<Duration> {
        self.keepalive.map(|seconds| Duration::from_secs(u64::from(seconds)))
    }

    // The largest packet the tunnel carries where the network under it carries packets of `outer` bytes: the owner's bound, the config's own and what the outer network leaves, the least of them; none where that is less than every host takes whole.
    pub(crate) fn mtu_within(&self, outer: usize) -> Option<usize> {
        let mtu = MOST_MTU.min(self.mtu.map_or(MOST_MTU, usize::from)).min(outer.saturating_sub(OVERHEAD_BYTES));
        (mtu >= LEAST_MTU).then_some(mtu)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::net::wireguard::tests::Toy;

    pub(crate) const TEXT: &str = "[Interface]\nPrivateKey = ERERERERERERERERERERERERERERERERERERERERERE=\nAddress = fd00::2/128, 10.2.0.2/32\nDNS = 10.2.0.1, example.org\nMTU = 1300\n\n[Peer]\n# the provider's\nPublicKey = IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI=\npresharedkey=MzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM=\nAllowedIPs = 0.0.0.0/0, ::/0\nEndpoint = vpn.example.org:51820\nPersistentKeepalive = 25\n";

    pub(crate) fn config() -> Config {
        Config::parse(TEXT).unwrap()
    }

    #[test]
    fn wg_quick_text() {
        let config = config();
        assert_eq!(
            (config.private, config.peer, config.preshared),
            ([0x11; 32], [0x22; 32], Some([0x33; 32])),
            "the keys, whatever the case of their names and the spaces about them"
        );
        assert_eq!(
            (config.address, config.resolver, config.mtu, config.keepalive()),
            (Ipv4Addr::new(10, 2, 0, 2), Ipv4Addr::new(10, 2, 0, 1), Some(1300), Some(Duration::from_secs(25))),
            "the first IPv4 address of each list, what is no address passed over"
        );
        assert_eq!(config.endpoint, Endpoint { host: "vpn.example.org".into(), port: 51820 });
        let plain =
            TEXT.replace("presharedkey=MzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM=\n", "").replace("MTU = 1300\n", "");
        let plain = Config::parse(&plain.replace("PersistentKeepalive = 25", "PersistentKeepalive = 0")).unwrap();
        assert_eq!((plain.preshared, plain.mtu, plain.keepalive()), (None, None, None));
        let by_address = Config::parse(&TEXT.replace("vpn.example.org:51820", "[2001:db8::9]:443")).unwrap();
        assert_eq!(by_address.endpoint, Endpoint { host: "2001:db8::9".into(), port: 443 });
        assert_eq!(by_address.endpoint.to_text(), "[2001:db8::9]:443");
    }

    #[test]
    fn refused_texts() {
        let without =
            |line: &str| TEXT.lines().filter(|kept| !kept.starts_with(line)).collect::<alloc::vec::Vec<_>>().join("\n");
        for (text, flaw, reason) in [
            (String::from("sk-proj-abc"), Flaw::NotAConfig, "another text"),
            (String::new(), Flaw::NotAConfig, "nothing"),
            (without("PrivateKey"), Flaw::NotAConfig, "no key of this end"),
            (without("PublicKey"), Flaw::NotAConfig, "no key of the peer"),
            (without("Endpoint"), Flaw::NotAConfig, "nowhere to reach the peer"),
            (
                TEXT.replace("ERERERERERERERERERERERERERERERERERERERERERE=", "ERER"),
                Flaw::NotAConfig,
                "a key of another length",
            ),
            (
                format!("{TEXT}[Peer]\nPublicKey = IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI=\n"),
                Flaw::NotAConfig,
                "a second peer",
            ),
            (TEXT.replace("MTU = 1300", "MTU = wide"), Flaw::NotAConfig, "a number that is none"),
            (TEXT.replace("fd00::2/128, 10.2.0.2/32", "fd00::2/128"), Flaw::NoAddress, "an address of IPv6 alone"),
            (without("DNS"), Flaw::NoResolver, "no resolver"),
            (
                TEXT.replace("DNS = 10.2.0.1, example.org", "DNS = fd00::1"),
                Flaw::NoResolver,
                "a resolver of IPv6 alone",
            ),
            (TEXT.replace("0.0.0.0/0, ::/0", "10.0.0.0/8"), Flaw::NotWhole, "a tunnel for part of the traffic"),
            (without("AllowedIPs"), Flaw::NotWhole, "a peer that takes no address"),
            (TEXT.replace("MTU = 1300", "MTU = 500"), Flaw::TooNarrow, "less than every host takes"),
        ] {
            assert_eq!(Config::parse(&text), Err(flaw), "{reason}");
        }
        assert_eq!(
            [Flaw::NotAConfig, Flaw::NoAddress, Flaw::NoResolver, Flaw::NotWhole, Flaw::TooNarrow, Flaw::Clash]
                .map(Flaw::message),
            [
                "no config of one interface and one peer",
                "the config has no IPv4 address",
                "the config has no IPv4 DNS",
                "the config's AllowedIPs lack 0.0.0.0/0",
                "the config's MTU is less than 576",
                "the config's address or DNS is one of the link to the host",
            ],
            "what the console answers"
        );
    }

    #[test]
    fn kept_round_trip() {
        let config = config();
        let text = config.to_text();
        assert_eq!(
            text,
            "private ERERERERERERERERERERERERERERERERERERERERERE=\naddress 10.2.0.2\ndns 10.2.0.1\nmtu 1300\npeer IiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiI=\npreshared MzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzMzM=\nendpoint vpn.example.org:51820\nkeepalive 25\n"
        );
        assert_eq!(Config::parse_kept(&text), Some(config.clone()));
        let plain = Config { mtu: None, preshared: None, keepalive: None, ..config };
        assert_eq!(plain.to_text().lines().count(), 5);
        assert_eq!(Config::parse_kept(&plain.to_text()), Some(plain.clone()));
        for line in ["private", "address", "dns", "peer", "endpoint"] {
            let cut: String = plain
                .to_text()
                .lines()
                .filter(|kept| !kept.starts_with(line))
                .map(|kept| format!("{kept}\n"))
                .collect();
            assert_eq!(Config::parse_kept(&cut), None, "without its {line} line");
        }
        assert_eq!(Config::parse_kept(&format!("{}mtu wide\n", plain.to_text())), None, "a line that does not read");
    }

    #[test]
    fn keys_and_mtu() {
        let config = config();
        let keys = config.to_keys(&Toy::default()).unwrap();
        assert!(keys.private == [0x11; 32] && keys.peer == [0x22; 32] && keys.preshared == [0x33; 32]);
        assert_eq!(keys.public, Toy::default().public_of(&[0x11; 32]).unwrap());
        let text = format!("{config:?}");
        assert!(
            text.starts_with("Config { endpoint: ") && !text.contains("17, 17") && !text.contains("ERER"),
            "{text}"
        );
        assert_eq!(config.mtu_within(1500), Some(1300), "the config's own, under the bound");
        let unset = Config { mtu: None, ..config.clone() };
        assert_eq!(
            [unset.mtu_within(1500), unset.mtu_within(1464), unset.mtu_within(1428), unset.mtu_within(1280)],
            [Some(1384), Some(1384), Some(1348), Some(1200)],
            "the owner's bound, or what the outer network leaves after 80 bytes"
        );
        assert_eq!((unset.mtu_within(655), unset.mtu_within(656)), (None, Some(576)));
    }
}

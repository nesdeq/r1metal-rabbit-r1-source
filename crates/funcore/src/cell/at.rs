//! The modem's AT channel: lines, final results, the answers and unsolicited lines the driving reads, and the commands it sends.

use crate::cell::link::Interface;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const END: &str = "\r";
const OK: &str = "OK";
const ERROR: &str = "ERROR";
const DEVICE_ERROR: &str = "+CME ERROR:";
const MESSAGE_ERROR: &str = "+CMS ERROR:";
const FAILURES: [&str; 3] = ["NO CARRIER", "NO ANSWER", "BUSY"];
const IPV4: u8 = 1;
const IPV6: u8 = 2;
const IPV4V6: u8 = 3;
const IPV6_BYTES: usize = 16;
// The `+EIND` event that tells the AT channel takes commands, as MediaTek's `gsm0710muxd` waits for it.
const AT_READY_EVENT: &str = "128";
// The last argument of `AT+EPDN`'s interface question, as the stock RIL sends it (nixos-agm-m7, `docs/MODEM-RE.md` 9).
const IFST_ARGUMENT: u32 = 20;

// Echo off, result codes and their words on (ITU-T V.250).
pub(super) const ECHO_OFF: &str = "ATE0Q0V1";
// Numbered errors, packet domain events, registration lines, and an attach whenever the radio is on (`+EGTYPE`, as nixos-agm-m7's radio HAL sends it).
pub(super) const SETUP: [&str; 4] = ["AT+CMEE=1", "AT+CGEREP=1,0", "AT+CEREG=1", "AT+EGTYPE=1"];
// The radio on, and off with the SIM kept powered (`AT+CFUN=0` powers it off too).
pub(super) const RADIO_ON: &str = "AT+CFUN=1";
pub(super) const RADIO_OFF: &str = "AT+CFUN=4";
pub(super) const SIM_STATE: &str = "AT+CPIN?";
pub(super) const PIN_TRIES: &str = "AT+EPINC";
pub(super) const REGISTRATION: &str = "AT+CEREG?";
// The setup of data connections once the SIM is ready, as MediaTek's RIL sends it.
pub(super) const DATA_SETUP: [&str; 3] = ["AT+EIFCONFIG=3", "AT+EIAREG=1", "AT+EDALLOW=1"];
// The detach and the SIM off before a stop.
pub(super) const POWER_OFF: &str = "AT+EPOF";

/// Lines out of the channel's bytes, which may end in either of CR and LF or both.
#[derive(Default)]
pub struct Lines {
    pending: Vec<u8>,
}

impl Lines {
    /// The whole lines `bytes` completes, without empty ones.
    pub fn take(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for &byte in bytes {
            if byte == b'\r' || byte == b'\n' {
                let line = String::from(String::from_utf8_lossy(&self.pending).trim());
                self.pending.clear();
                if !line.is_empty() {
                    lines.push(line);
                }
            } else {
                self.pending.push(byte);
            }
        }
        lines
    }
}

// A command's text without its arguments, which may hold a PIN.
pub(crate) fn name_of(text: &str) -> &str {
    text.split('=').next().unwrap_or_default()
}

/// An AT command; its `Debug` tells the command without its arguments, which may hold a PIN.
#[derive(Clone, PartialEq, Eq)]
pub struct Command(String);

impl Command {
    pub(super) fn new(text: impl Into<String>) -> Command {
        Command(text.into())
    }

    pub(super) fn name(&self) -> &str {
        name_of(&self.0)
    }

    /// The command as it is written to the channel.
    pub fn to_wire(&self) -> String {
        format!("{}{END}", self.0)
    }

    #[cfg(test)]
    pub(super) fn text(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Command {
    fn from(text: &str) -> Command {
        Command::new(text)
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

// How a command ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Final {
    Ok,
    // With the device's error number where it gave one.
    Error(Option<u32>),
}

// The final result a line is, if it is one.
pub(super) fn to_final(line: &str) -> Option<Final> {
    if line == OK {
        return Some(Final::Ok);
    }
    if line == ERROR || FAILURES.contains(&line) {
        return Some(Final::Error(None));
    }
    let code = line.strip_prefix(DEVICE_ERROR).or_else(|| line.strip_prefix(MESSAGE_ERROR))?;
    Some(Final::Error(code.trim().parse().ok()))
}

// The comma-separated fields after a line's prefix, each without its spaces and quotes.
fn fields<'a>(line: &'a str, prefix: &str) -> Option<Vec<&'a str>> {
    let rest = line.strip_prefix(prefix)?.strip_prefix(':')?;
    Some(rest.split(',').map(|field| field.trim().trim_matches('"')).collect())
}

// A line the modem sends on its own.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Urc {
    // The AT channel takes commands (`+EIND: 128`); one sent before may go unanswered.
    AtReady,
    // The EPS registration's state (`+CEREG: <stat>`).
    Registration(u8),
    // A PDN connection came up or went (`+CGEV`), by its id.
    PdnUp(u32),
    PdnDown(u32),
    Detached,
    // The modem's network interface is up, with its id, the address types and the addresses (`+EIF: ... ifup`).
    InterfaceUp(Interface),
    InterfaceDown(u32),
    // The interface's MTU (`+EIF: ... mtu`).
    Mtu(u32, u32),
    // The network attached the device with this APN (`+EIAREG: ME ATTACH`).
    Attached(String),
    // A PDN connection's interface and addresses (`+EPDN: ... "new"`).
    Pdn(u32, Interface),
}

// The kind the modem's `+EIF` confirmations name: 1 IPv4, 2 IPv6, 3 both.
const fn to_kind(interface: &Interface) -> u8 {
    match (interface.ipv4.is_some(), interface.ipv6.is_some()) {
        (true, true) => IPV4V6,
        (false, true) => IPV6,
        _ => IPV4,
    }
}

// An address as the modem writes one: dotted IPv4, IPv6 in its usual text, as 16 dotted bytes, or as 16 bytes in hex parted by colons.
fn parse_address(text: &str) -> Option<IpAddr> {
    let text = text.trim().trim_matches('"');
    if let Ok(ipv4) = text.parse::<Ipv4Addr>() {
        return Some(IpAddr::V4(ipv4));
    }
    let to_ipv6 =
        |parts: Vec<u8>| <[u8; IPV6_BYTES]>::try_from(parts).ok().map(|bytes| IpAddr::V6(Ipv6Addr::from(bytes)));
    let dotted: Option<Vec<u8>> = text.split('.').map(|part| part.parse().ok()).collect();
    if let Some(address) = dotted.and_then(to_ipv6) {
        return Some(address);
    }
    let colons: Option<Vec<u8>> = text.split(':').map(|part| u8::from_str_radix(part, 16).ok()).collect();
    if let Some(address) = colons.and_then(to_ipv6) {
        return Some(address);
    }
    text.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
}

fn to_interface(id: u32, addresses: &[&str], mtu: Option<u32>) -> Interface {
    let (mut ipv4, mut ipv6) = (None, None);
    for address in addresses.iter().filter_map(|field| parse_address(field)) {
        match address {
            IpAddr::V4(address) => ipv4 = ipv4.or(Some(address)),
            IpAddr::V6(address) => ipv6 = ipv6.or(Some(address)),
        }
    }
    Interface { id, ipv4, ipv6, mtu }
}

// The unsolicited line a line is, if it is one this firmware reads.
pub(super) fn parse_urc(line: &str) -> Option<Urc> {
    if let Some(fields) = fields(line, "+EIND") {
        return (fields.first() == Some(&AT_READY_EVENT)).then_some(Urc::AtReady);
    }
    if let Some(fields) = fields(line, "+CEREG") {
        return (fields.len() == 1).then(|| fields[0].parse().ok().map(Urc::Registration)).flatten();
    }
    if let Some(event) = line.strip_prefix("+CGEV:").map(str::trim) {
        let id = |rest: &str| rest.split(',').next()?.trim().parse().ok();
        return if let Some(rest) = event.strip_prefix("ME PDN ACT") {
            id(rest).map(Urc::PdnUp)
        } else if let Some(rest) = event.strip_prefix("NW PDN DEACT").or_else(|| event.strip_prefix("ME PDN DEACT")) {
            id(rest).map(Urc::PdnDown)
        } else if event.starts_with("NW DETACH") || event.starts_with("ME DETACH") {
            Some(Urc::Detached)
        } else {
            None
        };
    }
    if let Some(fields) = fields(line, "+EIF") {
        let id = fields.first()?.parse().ok()?;
        return match *fields.get(1)? {
            "ifup" => {
                let kind = u8::from_str_radix(fields.get(2)?, 16).ok()?;
                let count = if kind == IPV4V6 { 2 } else { 1 };
                Some(Urc::InterfaceUp(to_interface(id, fields.get(3..3 + count)?, None)))
            }
            "ifdown" => Some(Urc::InterfaceDown(id)),
            "mtu" => Some(Urc::Mtu(id, fields.get(2)?.parse().ok()?)),
            _ => None,
        };
    }
    if let Some(rest) = line.strip_prefix("+EIAREG:").map(str::trim) {
        let fields: Vec<&str> =
            rest.strip_prefix("ME ATTACH")?.split(',').map(|field| field.trim().trim_matches('"')).collect();
        return fields
            .first()
            .filter(|apn| !apn.is_empty())
            .map(|apn| Urc::Attached(String::from(without_operator(apn))));
    }
    if let Some(fields) = fields(line, "+EPDN") {
        if *fields.get(1)? != "new" {
            return None;
        }
        let (aid, id, mtu) = (fields[0].parse().ok()?, fields.get(3)?.parse().ok()?, fields.get(4)?.parse().ok());
        return Some(Urc::Pdn(aid, to_interface(id, fields.get(6..)?, mtu)));
    }
    None
}

// The SIM's state as `+CPIN?` answers it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Card {
    Ready,
    Pin,
    Puk,
    // Locked another way: to a network, a PIN2, a phone.
    Locked,
}

// The SIM's state in a line answering `+CPIN?`.
pub(super) fn parse_card(line: &str) -> Option<Card> {
    let state = line.strip_prefix("+CPIN:")?.trim();
    Some(match state {
        "READY" => Card::Ready,
        "SIM PIN" => Card::Pin,
        "SIM PUK" => Card::Puk,
        _ => Card::Locked,
    })
}

// The PIN tries left in a line answering `+EPINC` (`<pin1>,<pin2>,<puk1>,<puk2>`).
pub(super) fn parse_pin_tries(line: &str) -> Option<u8> {
    fields(line, "+EPINC")?.first()?.parse().ok()
}

// The registration's state in a line answering `+CEREG?` (`<n>,<stat>,...`).
pub(super) fn parse_registration(line: &str) -> Option<u8> {
    let fields = fields(line, "+CEREG")?;
    if fields.len() < 2 { None } else { fields[1].parse().ok() }
}

// A PDN connection as a line of `+CGCONTRDP` describes it: its id, its APN without the operator's part, the network's DNS servers and its IPv4 MTU.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Context {
    pub(super) id: u32,
    pub(super) apn: String,
    pub(super) servers: Vec<IpAddr>,
    pub(super) mtu: Option<u32>,
}

// The fields of `+CGCONTRDP` past the APN (3GPP TS 27.007): the primary and the secondary DNS server, and the IPv4 MTU.
const SERVER_FIELDS: [usize; 2] = [5, 6];
const MTU_FIELD: usize = 11;
const OPERATOR_MARK: &str = ".mnc";

// An APN without the operator's part the network may add (`.mnc<MNC>.mcc<MCC>.gprs`, 3GPP TS 23.003).
fn without_operator(apn: &str) -> &str {
    apn.find(OPERATOR_MARK).map_or(apn, |at| &apn[..at])
}

// The connection in a line answering `+CGCONTRDP`; a server given as nothing or as zeros is none.
pub(super) fn parse_context(line: &str) -> Option<Context> {
    let fields = fields(line, "+CGCONTRDP")?;
    let apn = without_operator(fields.get(2)?);
    let mtu = fields.get(MTU_FIELD).and_then(|mtu| mtu.parse().ok()).filter(|&mtu| mtu > 0);
    let servers = SERVER_FIELDS
        .iter()
        .filter_map(|&at| parse_address(fields.get(at)?))
        .filter(|server| !server.is_unspecified())
        .collect();
    Some(Context { id: fields.first()?.parse().ok()?, apn: String::from(apn), servers, mtu })
}

// A text in a command's quotes, with quotes and backslashes as the modem's escapes, as MediaTek's RIL writes them.
fn quoted(text: &str) -> String {
    let escaped = text.replace('\\', "\\5C").replace('"', "\\22");
    format!("\"{escaped}\"")
}

// `AT+CPIN` with the PIN.
pub(super) fn enter_pin(pin: &str) -> Command {
    Command(format!("AT+CPIN={}", quoted(pin)))
}

// `AT+EAPNACT` that brings the default data connection of `apn` up.
pub(super) fn activate(apn: &str) -> Command {
    Command(format!("AT+EAPNACT=1,{},\"default\",0", quoted(apn)))
}

// `AT+EIF` that tells the modem the AP's side of an interface is up or down.
pub(super) fn confirm(interface: &Interface, is_up: bool) -> Command {
    let state = if is_up { "up" } else { "down" };
    Command(format!("AT+EIF={},\"ifst\",\"{state}\",{}", interface.id, to_kind(interface)))
}

// `AT+EPDN` that asks for a connection's interface and addresses.
pub(super) fn describe(aid: u32) -> Command {
    Command(format!("AT+EPDN={aid},\"ifst\",{IFST_ARGUMENT}"))
}

// `AT+CGCONTRDP` for one connection, or for all.
pub(super) fn context(aid: Option<u32>) -> Command {
    Command(aid.map_or_else(|| String::from("AT+CGCONTRDP"), |aid| format!("AT+CGCONTRDP={aid}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn lines_across_reads() {
        let mut lines = Lines::default();
        assert_eq!(lines.take(b"\r\nOK\r\n\r\n+CEREG: 1"), ["OK"]);
        assert_eq!(lines.take(b"\r\n"), ["+CEREG: 1"], "a line ends in a later read");
        assert!(lines.take(b"\r\r\n\n").is_empty());
    }

    #[test]
    fn finals() {
        assert_eq!(to_final("OK"), Some(Final::Ok));
        assert_eq!(to_final("ERROR"), Some(Final::Error(None)));
        assert_eq!(to_final("+CME ERROR: 16"), Some(Final::Error(Some(16))));
        assert_eq!(to_final("+CME ERROR: 5841"), Some(Final::Error(Some(5841))));
        assert_eq!(to_final("NO CARRIER"), Some(Final::Error(None)));
        assert_eq!((to_final("+CPIN: READY"), to_final("OKAY")), (None, None));
    }

    #[test]
    fn unsolicited_lines() {
        assert_eq!((parse_urc("+EIND: 128"), parse_urc("+EIND: 64")), (Some(Urc::AtReady), None));
        assert_eq!(parse_urc("+CEREG: 1"), Some(Urc::Registration(1)));
        assert_eq!(parse_urc("+CEREG: 1,5"), None, "the answer to a question");
        assert_eq!(parse_urc("+CGEV: ME PDN ACT 0"), Some(Urc::PdnUp(0)));
        assert_eq!(parse_urc("+CGEV: ME PDN ACT 1, 2, 4"), Some(Urc::PdnUp(1)));
        assert_eq!(
            (parse_urc("+CGEV: NW PDN DEACT 3"), parse_urc("+CGEV: ME PDN DEACT 3")),
            (Some(Urc::PdnDown(3)), Some(Urc::PdnDown(3)))
        );
        assert_eq!(parse_urc("+CGEV: NW DETACH"), Some(Urc::Detached));
        let up = Interface { id: 100, ipv4: Some(Ipv4Addr::new(10, 1, 2, 3)), ipv6: None, mtu: None };
        assert_eq!(parse_urc("+EIF: 100, ifup, 1, 10.1.2.3, 0"), Some(Urc::InterfaceUp(up.clone())));
        assert_eq!(parse_urc("+EIF: 100, \"ifup\", 1, \"10.1.2.3\", 0"), Some(Urc::InterfaceUp(up)), "quoted or not");
        let both = parse_urc("+EIF: 201, ifup, 3, 10.1.2.3, FE:80:00:00:00:00:00:00:00:00:00:00:00:00:00:01, 0");
        let Some(Urc::InterfaceUp(both)) = both else { panic!("{both:?}") };
        assert_eq!((both.index(), to_kind(&both), both.ipv6), (1, 3, Some("fe80::1".parse().unwrap())));
        assert_eq!(parse_urc("+EIF: 100, mtu, 1500"), Some(Urc::Mtu(100, 1500)));
        assert_eq!(parse_urc("+EIF: 100, ifdown, 0"), Some(Urc::InterfaceDown(100)));
        assert_eq!(parse_urc("+EIAREG: ME ATTACH \"internet\",1,0"), Some(Urc::Attached("internet".into())));
        assert_eq!(
            parse_urc("+EIAREG: ME ATTACH \"web.mnc001.mcc001.gprs\",1,0"),
            Some(Urc::Attached("web".into())),
            "without the operator's part"
        );
        let pdn = parse_urc("+EPDN: 0, \"new\", 1, 100, 1500, 1, \"10.1.2.3\"");
        assert_eq!(
            pdn,
            Some(Urc::Pdn(
                0,
                Interface { id: 100, ipv4: Some(Ipv4Addr::new(10, 1, 2, 3)), ipv6: None, mtu: Some(1500) }
            ))
        );
        assert_eq!(parse_urc("+EPDN: 0, \"update\", 100, 1, \"10.1.2.3\""), None);
        assert_eq!((parse_urc("+ECSQ: 1"), parse_urc("RING")), (None, None));
    }

    #[test]
    fn addresses() {
        assert_eq!(
            parse_address("32.1.13.184.0.0.0.3.61.48.97.182.50.254.113.251"),
            Some("2001:db8:0:3:3d30:61b6:32fe:71fb".parse().unwrap())
        );
        assert_eq!(parse_address("\"2001:db8::1\""), Some("2001:db8::1".parse().unwrap()));
        assert_eq!(parse_address("10.0.0.1"), Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert_eq!((parse_address("0"), parse_address("")), (None, None));
    }

    #[test]
    fn answers() {
        assert_eq!(
            ["+CPIN: READY", "+CPIN: SIM PIN", "+CPIN: SIM PUK", "+CPIN: PH-NET PIN"].map(parse_card),
            [Some(Card::Ready), Some(Card::Pin), Some(Card::Puk), Some(Card::Locked)]
        );
        assert_eq!(parse_pin_tries("+EPINC: 3,3,10,10"), Some(3));
        assert_eq!((parse_registration("+CEREG: 1,5"), parse_registration("+CEREG: 2")), (Some(5), None));
        let context = parse_context(
            "+CGCONTRDP: 1,5,\"internet.mnc001.mcc001.gprs\",\"10.1.2.3.255.255.255.255\",\"\",\"192.0.2.53\",\"0.0.0.0\",\"\",\"\",0,0,1440",
        );
        let servers = vec![IpAddr::from([192, 0, 2, 53])];
        assert_eq!(
            context,
            Some(Context { id: 1, apn: "internet".into(), servers, mtu: Some(1440) }),
            "a server of zeros is none"
        );
        let v6 = parse_context(
            "+CGCONTRDP: 1,5,\"internet.v6\",\"32.1.13.184.0.0.0.3.0.0.0.0.0.0.0.1.255.255.255.255.255.255.255.255.0.0.0.0.0.0.0.0\",\"\",\"32.1.13.184.0.0.0.0.0.0.0.0.0.0.0.83\",\"2001:db8::35\"",
        );
        assert_eq!(
            v6.map(|context| context.servers),
            Some(vec!["2001:db8::53".parse().unwrap(), "2001:db8::35".parse().unwrap()]),
            "IPv6 servers as dotted bytes or in their usual text"
        );
        let ims = Context { id: 2, apn: "ims".into(), servers: vec![], mtu: None };
        assert_eq!(parse_context("+CGCONTRDP: 2,6,\"ims\""), Some(ims));
    }

    #[test]
    fn commands() {
        assert_eq!(Command::new(SIM_STATE).to_wire(), "AT+CPIN?\r", "ended as the modem's parser takes a line");
        assert_eq!(enter_pin("1234").text(), "AT+CPIN=\"1234\"");
        assert_eq!(format!("{:?}", enter_pin("1234")), "AT+CPIN", "no digit of the PIN where it is formatted");
        assert_eq!(activate("internet").text(), "AT+EAPNACT=1,\"internet\",\"default\",0");
        assert_eq!(
            activate("a\"b\\c").text(),
            "AT+EAPNACT=1,\"a\\22b\\5Cc\",\"default\",0",
            "quotes and backslashes escaped"
        );
        let interface =
            Interface { id: 100, ipv4: Some(Ipv4Addr::new(10, 0, 0, 1)), ipv6: Some(Ipv6Addr::LOCALHOST), mtu: None };
        assert_eq!(
            (confirm(&interface, true).text(), confirm(&interface, false).text()),
            ("AT+EIF=100,\"ifst\",\"up\",3", "AT+EIF=100,\"ifst\",\"down\",3")
        );
        assert_eq!(
            (describe(0).text(), context(Some(0)).text(), context(None).text()),
            ("AT+EPDN=0,\"ifst\",20", "AT+CGCONTRDP=0", "AT+CGCONTRDP")
        );
    }
}

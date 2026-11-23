use super::dns::nat64::Prefix;
use super::{
    Address, DESTINATION, DONT_FRAGMENT, FRAGMENT_AT, FRAGMENT_BITS, ICMP, ID_AT, IP_HEAD, LENGTH_AT, MORE_FRAGMENTS,
    SOURCE, TCP, TTL_AT, UDP, icmp, ipv4_head, parse_ipv4_head, set_ipv4_checksum, sum, to_udp_sum, to_updated_sum,
    to_whole_ipv4,
};
use crate::base::bytes::{be16, be32};
use alloc::vec::Vec;
use core::net::{Ipv4Addr, Ipv6Addr};

// The IPv4 address this end's packets carry before they are translated (RFC 7335).
pub(crate) const ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 0, 0, 1);
// What a packet grows by as it is translated: the difference of the two heads.
pub(crate) const GROWTH: usize = IPV6_HEAD - IP_HEAD;
// The bytes of an address that number its interface within its network.
pub(crate) const NUMBER_BYTES: usize = 8;
const IPV6: u8 = 6;
const IPV6_HEAD: usize = 40;
const VERSION_SHIFT: u8 = 4;
const TOS_AT: usize = 1;
const PAYLOAD_AT: usize = 4;
const NEXT_AT: usize = 6;
const HOPS_AT: usize = 7;
const FROM: core::ops::Range<usize> = 8..24;
const TO: core::ops::Range<usize> = 24..40;
const FRAGMENT: u8 = 44;
const FRAGMENT_HEAD: usize = 8;
const ICMPV6: u8 = 58;
const TCP_CHECKSUM_AT: usize = 16;
const UDP_CHECKSUM_AT: usize = 6;
const ECHO_REQUEST: u8 = 128;
const ECHO_REPLY: u8 = 129;
const UNREACHABLE: u8 = 1;
const TOO_BIG: u8 = 2;
const TIME_EXCEEDED: u8 = 3;
const PARAMETER_PROBLEM: u8 = 4;
// A packet of this size or less passes every link of IPv6 whole, its head counted off (RFC 7915, 5.1).
const WHOLE_EVERYWHERE: usize = 1280 - GROWTH;

// The translation of one stay in a network of IPv6 alone, IPv4 packets as IPv6 ones and back (RFC 7915): this end's two addresses (RFC 6877), and the NAT64's prefix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Translator {
    own: Ipv6Addr,
    prefix: Prefix,
}

fn pseudo_sum(from: &[u8], to: &[u8], next: u8, payload: &[u8]) -> u16 {
    sum(&[from, to, &(payload.len() as u32).to_be_bytes(), &[0, 0, 0, next], payload])
}

// The kind and code an error of IPv4's has as one of IPv6's, with the four bytes behind its checksum.
fn error_to_ipv6(kind: u8, code: u8, rest: [u8; 4]) -> Option<(u8, u8, [u8; 4])> {
    let mtu = u32::from(u16::from_be_bytes([rest[2], rest[3]]));
    let pointer = |at: u32| at.to_be_bytes();
    match (kind, code) {
        (icmp::UNREACHABLE, 0 | 1 | 5 | 6 | 7 | 8 | 11 | 12) => Some((UNREACHABLE, 0, [0; 4])),
        (icmp::UNREACHABLE, 9 | 10 | 13 | 15) => Some((UNREACHABLE, 1, [0; 4])),
        (icmp::UNREACHABLE, 2) => Some((PARAMETER_PROBLEM, 1, pointer(NEXT_AT as u32))),
        (icmp::UNREACHABLE, 3) => Some((UNREACHABLE, 4, [0; 4])),
        (icmp::UNREACHABLE, icmp::TOO_BIG) => Some((TOO_BIG, 0, (mtu + GROWTH as u32).to_be_bytes())),
        (icmp::TIME_EXCEEDED, code) => Some((TIME_EXCEEDED, code, [0; 4])),
        (icmp::PARAMETER_PROBLEM, 0) => {
            let at = match rest[0] {
                0 => 0,
                1 => 1,
                2 | 3 => 4,
                8 => 7,
                9 => 6,
                12..=15 => 8,
                16..=19 => 24,
                _ => return None,
            };
            Some((PARAMETER_PROBLEM, 0, pointer(at)))
        }
        _ => None,
    }
}

fn error_to_ipv4(kind: u8, code: u8, rest: [u8; 4]) -> Option<(u8, u8, [u8; 4])> {
    let mtu = u32::from_be_bytes(rest).saturating_sub(GROWTH as u32).min(u32::from(u16::MAX)) as u16;
    match (kind, code) {
        (UNREACHABLE, 0 | 2 | 3) => Some((icmp::UNREACHABLE, 1, [0; 4])),
        (UNREACHABLE, 1) => Some((icmp::UNREACHABLE, 10, [0; 4])),
        (UNREACHABLE, 4) => Some((icmp::UNREACHABLE, 3, [0; 4])),
        (TOO_BIG, _) => Some((icmp::UNREACHABLE, icmp::TOO_BIG, [0, 0, (mtu >> 8) as u8, mtu as u8])),
        (TIME_EXCEEDED, code) => Some((icmp::TIME_EXCEEDED, code, [0; 4])),
        (PARAMETER_PROBLEM, 1) => Some((icmp::UNREACHABLE, 2, [0; 4])),
        (PARAMETER_PROBLEM, 0) => {
            let at = match u32::from_be_bytes(rest) {
                0 => 0,
                1 => 1,
                4 | 5 => 2,
                6 => 9,
                7 => 8,
                8..=23 => 12,
                24..=39 => 16,
                _ => return None,
            };
            Some((icmp::PARAMETER_PROBLEM, 0, [at, 0, 0, 0]))
        }
        _ => None,
    }
}

impl Translator {
    // A translation toward the NAT64 of `prefix`, whose packets leave from an address of the network of `kernel`, the one the kernel holds there, under the interface number `number`, drawn at random (RFC 6877, 6.3), or the next one where the kernel has that.
    pub(crate) fn new(kernel: Ipv6Addr, mut number: [u8; NUMBER_BYTES], prefix: Prefix) -> Translator {
        let mut own = kernel.octets();
        if own[NUMBER_BYTES..] == number {
            number[NUMBER_BYTES - 1] ^= 1;
        }
        own[NUMBER_BYTES..].copy_from_slice(&number);
        Translator { own: Ipv6Addr::from(own), prefix }
    }

    fn map_out(&self, address: Address) -> [u8; 16] {
        let address = Ipv4Addr::from(address);
        if address == ADDRESS { self.own.octets() } else { self.prefix.synthesize(address).octets() }
    }

    fn map_in(&self, address: &[u8]) -> Option<Address> {
        let address = Ipv6Addr::from(<[u8; 16]>::try_from(address).ok()?);
        if address == self.own {
            Some(ADDRESS.octets())
        } else {
            self.prefix.extract(address).map(|ipv4| ipv4.octets())
        }
    }

    // The head of IPv6 an IPv4 head becomes, for a payload of `length` bytes behind it.
    fn head_to_ipv6(&self, packet: &[u8], next: u8, length: usize) -> [u8; IPV6_HEAD] {
        let mut head = [0u8; IPV6_HEAD];
        (head[0], head[1]) = (IPV6 << VERSION_SHIFT | packet[TOS_AT] >> 4, packet[TOS_AT] << 4);
        head[PAYLOAD_AT..PAYLOAD_AT + 2].copy_from_slice(&(length as u16).to_be_bytes());
        (head[NEXT_AT], head[HOPS_AT]) = (next, packet[TTL_AT]);
        head[FROM].copy_from_slice(&self.map_out(packet[SOURCE].try_into().expect("four bytes of a head")));
        head[TO].copy_from_slice(&self.map_out(packet[DESTINATION].try_into().expect("four bytes of a head")));
        head
    }

    // A packet of IPv4 an error quotes, as far as it is quoted, as one of IPv6.
    fn quote_to_ipv6(&self, quoted: &[u8]) -> Option<Vec<u8>> {
        let (head, protocol) = parse_ipv4_head(quoted)?;
        let length = usize::from(be16(quoted, LENGTH_AT)?).checked_sub(head)?;
        let next = if protocol == ICMP { ICMPV6 } else { protocol };
        let mut packet = self.head_to_ipv6(quoted, next, length).to_vec();
        packet.extend_from_slice(&quoted[head..]);
        if let Some(kind) = packet.get_mut(IPV6_HEAD).filter(|_| protocol == ICMP) {
            *kind = if *kind == icmp::REQUEST { ECHO_REQUEST } else { ECHO_REPLY };
        }
        Some(packet)
    }

    fn quote_to_ipv4(&self, quoted: &[u8]) -> Option<Vec<u8>> {
        let head = quoted.get(..IPV6_HEAD)?;
        let protocol = if head[NEXT_AT] == ICMPV6 { ICMP } else { head[NEXT_AT] };
        let (source, target) = (self.map_in(&head[FROM])?, self.map_in(&head[TO])?);
        let mut packet = ipv4_head(protocol, source, target, usize::from(be16(head, PAYLOAD_AT)?)).to_vec();
        packet[TTL_AT] = head[HOPS_AT];
        set_ipv4_checksum(&mut packet);
        packet.extend_from_slice(&quoted[IPV6_HEAD..]);
        if let Some(kind) = packet.get_mut(IP_HEAD).filter(|_| protocol == ICMP) {
            *kind = if *kind == ECHO_REQUEST { icmp::REQUEST } else { icmp::REPLY };
        }
        Some(packet)
    }

    // An ICMP message of IPv4 as one of IPv6, without its checksum.
    fn message_to_ipv6(&self, message: &[u8]) -> Option<Vec<u8>> {
        let (kind, code) = (*message.first()?, *message.get(1)?);
        Some(match kind {
            icmp::REQUEST => [&[ECHO_REQUEST, 0, 0, 0][..], message.get(4..)?].concat(),
            icmp::REPLY => [&[ECHO_REPLY, 0, 0, 0][..], message.get(4..)?].concat(),
            _ => {
                let rest = *message.get(4..)?.first_chunk()?;
                let (kind, code, rest) = error_to_ipv6(kind, code, rest)?;
                [&[kind, code, 0, 0][..], &rest, &self.quote_to_ipv6(message.get(icmp::HEADER..)?)?].concat()
            }
        })
    }

    fn message_to_ipv4(&self, message: &[u8]) -> Option<Vec<u8>> {
        let (kind, code) = (*message.first()?, *message.get(1)?);
        let mut translated = match kind {
            ECHO_REQUEST => [&[icmp::REQUEST, 0, 0, 0][..], message.get(4..)?].concat(),
            ECHO_REPLY => [&[icmp::REPLY, 0, 0, 0][..], message.get(4..)?].concat(),
            _ => {
                let rest = *message.get(4..)?.first_chunk()?;
                let (kind, code, rest) = error_to_ipv4(kind, code, rest)?;
                [&[kind, code, 0, 0][..], &rest, &self.quote_to_ipv4(message.get(icmp::HEADER..)?)?].concat()
            }
        };
        icmp::set_checksum(&mut translated);
        Some(translated)
    }

    // An IPv4 packet from this end as the IPv6 one the NAT64 takes; none for a packet from another address, of a protocol the translation knows none of, or in pieces that cannot be told apart behind it.
    pub(crate) fn to_ipv6(self, packet: &[u8]) -> Option<Vec<u8>> {
        let packet = to_whole_ipv4(packet)?;
        let (head, protocol) = parse_ipv4_head(packet)?;
        if packet[SOURCE] != ADDRESS.octets() {
            return None;
        }
        let fragment = be16(packet, FRAGMENT_AT)?;
        let (place, follows) = (fragment & FRAGMENT_BITS & !MORE_FRAGMENTS, fragment & MORE_FRAGMENTS != 0);
        let is_piece = place != 0 || follows;
        let next = match protocol {
            TCP | UDP => protocol,
            ICMP if !is_piece => ICMPV6,
            _ => return None,
        };
        let message = if protocol == ICMP { Some(self.message_to_ipv6(&packet[head..])?) } else { None };
        let payload = message.as_deref().unwrap_or(&packet[head..]);
        let extra = if is_piece { FRAGMENT_HEAD } else { 0 };
        let mut translated = Vec::with_capacity(IPV6_HEAD + extra + payload.len());
        translated.extend(self.head_to_ipv6(packet, if is_piece { FRAGMENT } else { next }, extra + payload.len()));
        if is_piece {
            translated.extend([next, 0]);
            translated.extend((place << 3 | u16::from(follows)).to_be_bytes());
            translated.extend(u32::from(be16(packet, ID_AT)?).to_be_bytes());
        }
        translated.extend_from_slice(payload);
        if place == 0 {
            let (head, rest) = translated.split_at_mut(IPV6_HEAD);
            let (addresses, payload) = (&head[FROM.start..TO.end], &mut rest[extra..]);
            match protocol {
                TCP | UDP => {
                    let at = if protocol == TCP { TCP_CHECKSUM_AT } else { UDP_CHECKSUM_AT };
                    let old = be16(payload, at)?;
                    // A datagram sent without a checksum gets one, which IPv6 asks of every one.
                    let new = if protocol == UDP && old == 0 {
                        (!is_piece).then(|| !pseudo_sum(&addresses[..16], &addresses[16..], UDP, payload))?
                    } else {
                        to_updated_sum(old, &packet[SOURCE.start..DESTINATION.end], addresses)
                    };
                    let new = if protocol == UDP { to_udp_sum(new) } else { new };
                    payload[at..at + 2].copy_from_slice(&new.to_be_bytes());
                }
                _ => {
                    let checksum = !pseudo_sum(&addresses[..16], &addresses[16..], ICMPV6, payload);
                    payload[2..4].copy_from_slice(&checksum.to_be_bytes());
                }
            }
        }
        Some(translated)
    }

    // An IPv6 packet the NAT64 sent this end as the IPv4 one it stands for; none for a packet for another address, from outside the NAT64's prefix, or that the translation knows none of.
    pub(crate) fn to_ipv4(self, packet: &[u8]) -> Option<Vec<u8>> {
        let head = packet.get(..IPV6_HEAD).filter(|head| head[0] >> VERSION_SHIFT == IPV6)?;
        let mut payload = packet.get(IPV6_HEAD..IPV6_HEAD + usize::from(be16(head, PAYLOAD_AT)?))?;
        if head[TO] != self.own.octets() {
            return None;
        }
        let source = self.prefix.extract(Ipv6Addr::from(<[u8; 16]>::try_from(&head[FROM]).ok()?))?.octets();
        let (mut next, mut place, mut follows, mut number) = (head[NEXT_AT], 0, false, 0);
        let is_piece = next == FRAGMENT;
        if is_piece {
            let field = be16(payload, 2)?;
            (next, place, follows, number) = (payload[0], field >> 3, field & 1 != 0, be32(payload, 4)? as u16);
            payload = payload.get(FRAGMENT_HEAD..)?;
        }
        let protocol = match next {
            TCP | UDP => next,
            ICMPV6 if !is_piece => ICMP,
            _ => return None,
        };
        let message = if protocol == ICMP { Some(self.message_to_ipv4(payload)?) } else { None };
        let payload = message.as_deref().unwrap_or(payload);
        let mut translated = Vec::with_capacity(IP_HEAD + payload.len());
        translated.extend(ipv4_head(protocol, source, ADDRESS.octets(), payload.len()));
        (translated[TOS_AT], translated[TTL_AT]) = (head[0] << 4 | head[1] >> 4, head[HOPS_AT]);
        let field = match is_piece {
            true => place | if follows { MORE_FRAGMENTS } else { 0 },
            false if IP_HEAD + payload.len() > WHOLE_EVERYWHERE => DONT_FRAGMENT,
            false => 0,
        };
        translated[ID_AT..ID_AT + 2].copy_from_slice(&number.to_be_bytes());
        translated[FRAGMENT_AT..FRAGMENT_AT + 2].copy_from_slice(&field.to_be_bytes());
        set_ipv4_checksum(&mut translated);
        translated.extend_from_slice(payload);
        if place == 0 && protocol != ICMP {
            let at = IP_HEAD + if protocol == TCP { TCP_CHECKSUM_AT } else { UDP_CHECKSUM_AT };
            let old = be16(&translated, at)?;
            let new = to_updated_sum(old, &head[FROM.start..TO.end], &translated[SOURCE.start..DESTINATION.end]);
            let new = if protocol == UDP { to_udp_sum(new) } else { new };
            translated[at..at + 2].copy_from_slice(&new.to_be_bytes());
        }
        Some(translated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::PROTOCOL_AT;
    use crate::net::dns::nat64::tests::prefix;
    use alloc::vec;

    const FAR: Address = [93, 184, 216, 34];

    const NUMBER: [u8; NUMBER_BYTES] = [0xAA, 0xAA, 0xBB, 0xBB, 0xCC, 0xCC, 0xDD, 0xDD];

    fn translator() -> Translator {
        Translator::new("2001:db8:1:2::1".parse().unwrap(), NUMBER, prefix("64:ff9b::", 96))
    }

    #[test]
    fn own_address() {
        let own: Ipv6Addr = "2001:db8:1:2:aaaa:bbbb:cccc:dddd".parse().unwrap();
        assert_eq!(translator().own, own, "of the kernel's network, under the number drawn");
        assert_eq!(
            Translator::new(own, NUMBER, prefix("64:ff9b::", 96)).own,
            "2001:db8:1:2:aaaa:bbbb:cccc:dddc".parse::<Ipv6Addr>().unwrap(),
            "and never the kernel's own"
        );
    }

    fn own() -> [u8; 16] {
        translator().own.octets()
    }

    fn far() -> [u8; 16] {
        "64:ff9b::5db8:d822".parse::<Ipv6Addr>().unwrap().octets()
    }

    fn with_sum(mut packet: Vec<u8>, protocol: u8, at: usize) -> Vec<u8> {
        let length = (packet.len() - IP_HEAD) as u16;
        let checksum =
            !sum(&[&packet[SOURCE.start..DESTINATION.end], &[0, protocol], &length.to_be_bytes(), &packet[IP_HEAD..]]);
        packet[IP_HEAD + at..IP_HEAD + at + 2].copy_from_slice(&checksum.to_be_bytes());
        packet
    }

    fn udp(source: Address, target: Address, data: &[u8]) -> Vec<u8> {
        let length = (8 + data.len()) as u16;
        let mut packet = ipv4_head(UDP, source, target, usize::from(length)).to_vec();
        packet.extend([[0x13, 0x88], [0x01, 0xBB], length.to_be_bytes(), [0, 0]].concat());
        packet.extend_from_slice(data);
        with_sum(packet, UDP, UDP_CHECKSUM_AT)
    }

    fn echo(source: Address, target: Address, kind: u8) -> Vec<u8> {
        let mut message = vec![kind, 0, 0, 0, 0, 7, 0, 1, b'h', b'i'];
        icmp::set_checksum(&mut message);
        [&ipv4_head(ICMP, source, target, message.len())[..], &message].concat()
    }

    fn is_sound_ipv6(packet: &[u8]) -> bool {
        let payload = &packet[IPV6_HEAD..];
        usize::from(be16(packet, PAYLOAD_AT).unwrap()) == payload.len()
            && pseudo_sum(&packet[FROM], &packet[TO], packet[NEXT_AT], payload) == u16::MAX
    }

    fn is_sound_ipv4(packet: &[u8]) -> bool {
        let length = (packet.len() - IP_HEAD) as u16;
        let pseudo = sum(&[
            &packet[SOURCE.start..DESTINATION.end],
            &[0, packet[PROTOCOL_AT]],
            &length.to_be_bytes(),
            &packet[IP_HEAD..],
        ]);
        let transport = if packet[PROTOCOL_AT] == ICMP { sum(&[&packet[IP_HEAD..]]) } else { pseudo };
        sum(&[&packet[..IP_HEAD]]) == u16::MAX && transport == u16::MAX
    }

    #[test]
    fn datagram_out_and_back() {
        let translator = translator();
        let out = translator.to_ipv6(&udp(ADDRESS.octets(), FAR, b"question")).expect("a packet of this end");
        assert_eq!(
            (&out[..8], &out[FROM], &out[TO]),
            (&[0x60, 0, 0, 0, 0, 16, 17, 64][..], &own()[..], &far()[..]),
            "IPv6's head: the payload's length, UDP, the hops it had; from this end's address to the far end's behind the NAT64"
        );
        assert!(
            is_sound_ipv6(&out) && out.ends_with(b"question"),
            "the datagram's checksum holds under the new addresses"
        );
        let mut unsummed = udp(ADDRESS.octets(), FAR, b"question");
        unsummed[IP_HEAD + 6..IP_HEAD + 8].fill(0);
        assert!(is_sound_ipv6(&translator.to_ipv6(&unsummed).unwrap()), "one sent without a checksum is given one");
        let mut answer = out.clone();
        answer[FROM].copy_from_slice(&far());
        answer[TO].copy_from_slice(&own());
        let back = translator.to_ipv4(&answer).expect("the far end's answer");
        assert_eq!((&back[SOURCE], &back[DESTINATION], back[TTL_AT]), (&FAR[..], &ADDRESS.octets()[..], 64));
        assert!(is_sound_ipv4(&back) && back.ends_with(b"question"));
        assert_eq!(translator.to_ipv6(&udp([192, 0, 0, 2], FAR, b"x")), None, "a packet from another address");
        let mut elsewhere = answer.clone();
        elsewhere[TO.start] ^= 1;
        assert_eq!(translator.to_ipv4(&elsewhere), None, "one for another address");
        let mut outside = answer.clone();
        outside[FROM.start + 3] ^= 1;
        assert_eq!(translator.to_ipv4(&outside), None, "one from outside the NAT64's prefix");
        assert_eq!(
            (translator.to_ipv4(&answer[..47]), translator.to_ipv4(&back)),
            (None, None),
            "cut short, or of IPv4"
        );
        let gre = [&ipv4_head(47, ADDRESS.octets(), FAR, 8)[..], &[0; 8]].concat();
        assert_eq!(translator.to_ipv6(&gre), None, "a protocol the translation knows none of");
    }

    // The two bytes that, behind `data` of whole words, bring the checksum of a datagram between these addresses to nought.
    fn filling(from: &[u8], to: &[u8], data: &[u8]) -> [u8; 2] {
        let length = (8 + data.len() + 2) as u16;
        let ports = [[0x13, 0x88], [0x01, 0xBB], length.to_be_bytes()].concat();
        (!sum(&[from, to, &[0, UDP], &length.to_be_bytes(), &ports, data])).to_be_bytes()
    }

    #[test]
    fn datagram_sum_of_nought() {
        let translator = translator();
        let data = [&b"question"[..], &filling(&own(), &far(), b"question")].concat();
        let out = translator.to_ipv6(&udp(ADDRESS.octets(), FAR, &data)).unwrap();
        assert_eq!(
            (be16(&out, IPV6_HEAD + UDP_CHECKSUM_AT), is_sound_ipv6(&out)),
            (Some(u16::MAX), true),
            "a checksum that comes to nought goes as all ones, nought telling that there is none (RFC 768)"
        );
        let data = [&b"question"[..], &filling(&ADDRESS.octets(), &FAR, b"question")].concat();
        let mut answer = translator.to_ipv6(&udp(ADDRESS.octets(), FAR, &data)).unwrap();
        answer[FROM].copy_from_slice(&far());
        answer[TO].copy_from_slice(&own());
        let back = translator.to_ipv4(&answer).unwrap();
        assert_eq!(
            (be16(&back, IP_HEAD + UDP_CHECKSUM_AT), is_sound_ipv4(&back)),
            (Some(u16::MAX), true),
            "and so on the way in"
        );
    }

    #[test]
    fn echoes_and_their_checksums() {
        let translator = translator();
        let out = translator.to_ipv6(&echo(ADDRESS.octets(), FAR, icmp::REQUEST)).unwrap();
        assert_eq!(
            (out[NEXT_AT], out[IPV6_HEAD], &out[IPV6_HEAD + 4..]),
            (ICMPV6, ECHO_REQUEST, &[0, 7, 0, 1, b'h', b'i'][..])
        );
        assert!(is_sound_ipv6(&out), "ICMPv6 counts the addresses in, as ICMP of IPv4 does not");
        let mut answer = out.clone();
        answer[FROM].copy_from_slice(&far());
        answer[TO].copy_from_slice(&own());
        answer[IPV6_HEAD] = ECHO_REPLY;
        let back = translator.to_ipv4(&answer).unwrap();
        assert_eq!((back[PROTOCOL_AT], back[IP_HEAD]), (ICMP, icmp::REPLY));
        assert!(is_sound_ipv4(&back));
    }

    #[test]
    fn errors_with_what_they_quote() {
        let translator = translator();
        let sent = translator.to_ipv6(&udp(ADDRESS.octets(), FAR, b"question")).unwrap();
        let mut too_big = vec![TOO_BIG, 0, 0, 0, 0, 0, 0x05, 0x00];
        too_big.extend_from_slice(&sent);
        let router: [u8; 16] = "64:ff9b::a00:1".parse::<Ipv6Addr>().unwrap().octets();
        let mut error = vec![0x60, 0, 0, 0, 0, 0, ICMPV6, 60];
        error[PAYLOAD_AT..PAYLOAD_AT + 2].copy_from_slice(&(too_big.len() as u16).to_be_bytes());
        error.extend(router);
        error.extend(own());
        error.extend(&too_big);
        let told = translator.to_ipv4(&error).expect("an error about a packet this end sent");
        assert_eq!(
            (&told[SOURCE], &told[IP_HEAD..IP_HEAD + 2], be16(&told, IP_HEAD + 6)),
            (&[10, 0, 0, 1][..], &[3, 4][..], Some(1280 - 20)),
            "too large to pass whole: IPv4's word for it, with twenty bytes less"
        );
        let quoted = &told[IP_HEAD + 8..];
        assert_eq!(
            (&quoted[SOURCE], &quoted[DESTINATION], be16(quoted, LENGTH_AT), &quoted[IP_HEAD..IP_HEAD + 4]),
            (&ADDRESS.octets()[..], &FAR[..], Some(36), &[0x13, 0x88, 0x01, 0xBB][..]),
            "and the packet it quotes as this end sent it"
        );
        assert!(is_sound_ipv4(&told) && sum(&[&quoted[..IP_HEAD]]) == u16::MAX);
        let came = udp(FAR, ADDRESS.octets(), b"answer");
        let mut refusal = vec![3, 3, 0, 0, 0, 0, 0, 0];
        refusal.extend_from_slice(&came[..IP_HEAD + 8]);
        icmp::set_checksum(&mut refusal);
        let out = translator
            .to_ipv6(&[&ipv4_head(ICMP, ADDRESS.octets(), FAR, refusal.len())[..], &refusal].concat())
            .unwrap();
        assert_eq!(
            (&out[IPV6_HEAD..IPV6_HEAD + 2], &out[IPV6_HEAD + 8 + 8..IPV6_HEAD + 8 + 24]),
            (&[1, 4][..], &far()[..])
        );
        assert!(is_sound_ipv6(&out), "this end's own word on a packet that came: no such port");
    }

    #[test]
    fn error_kinds() {
        assert_eq!(error_to_ipv6(3, 1, [0; 4]), Some((1, 0, [0; 4])), "no route to it");
        assert_eq!(error_to_ipv6(3, 13, [0; 4]), Some((1, 1, [0; 4])), "forbidden");
        assert_eq!(error_to_ipv6(3, 2, [0; 4]), Some((4, 1, [0, 0, 0, 6])), "no such protocol: the head's next");
        assert_eq!(error_to_ipv6(3, 4, [0, 0, 0x05, 0xDC]), Some((2, 0, [0, 0, 0x05, 0xF0])), "twenty bytes more pass");
        assert_eq!(error_to_ipv6(11, 1, [0; 4]), Some((3, 1, [0; 4])));
        assert_eq!(
            error_to_ipv6(12, 0, [9, 0, 0, 0]),
            Some((4, 0, [0, 0, 0, 6])),
            "the place of the flaw in the other head"
        );
        assert_eq!((error_to_ipv6(12, 0, [4, 0, 0, 0]), error_to_ipv6(5, 0, [0; 4])), (None, None));
        assert_eq!(error_to_ipv4(1, 3, [0; 4]), Some((3, 1, [0; 4])));
        assert_eq!(error_to_ipv4(1, 1, [0; 4]), Some((3, 10, [0; 4])));
        assert_eq!(error_to_ipv4(1, 4, [0; 4]), Some((3, 3, [0; 4])));
        assert_eq!(error_to_ipv4(3, 0, [0; 4]), Some((11, 0, [0; 4])));
        assert_eq!(error_to_ipv4(4, 1, [0; 4]), Some((3, 2, [0; 4])));
        assert_eq!(error_to_ipv4(4, 0, [0, 0, 0, 24]), Some((12, 0, [16, 0, 0, 0])));
        assert_eq!(
            (error_to_ipv4(4, 0, [0, 0, 0, 40]), error_to_ipv4(1, 9, [0; 4]), error_to_ipv4(137, 0, [0; 4])),
            (None, None, None)
        );
    }

    #[test]
    fn pieces_keep_their_places() {
        let translator = translator();
        let whole = udp(ADDRESS.octets(), FAR, &[7u8; 56]);
        let piece = |range: core::ops::Range<usize>, field: u16| {
            let mut piece = whole[..IP_HEAD].to_vec();
            piece.extend_from_slice(&whole[IP_HEAD + range.start..IP_HEAD + range.end]);
            piece[LENGTH_AT..LENGTH_AT + 2].copy_from_slice(&((IP_HEAD + range.len()) as u16).to_be_bytes());
            piece[ID_AT..ID_AT + 2].copy_from_slice(&[0x12, 0x34]);
            piece[FRAGMENT_AT..FRAGMENT_AT + 2].copy_from_slice(&field.to_be_bytes());
            set_ipv4_checksum(&mut piece);
            piece
        };
        let first = translator.to_ipv6(&piece(0..32, MORE_FRAGMENTS)).unwrap();
        let later = translator.to_ipv6(&piece(32..64, 4)).unwrap();
        assert_eq!(
            (first[NEXT_AT], &first[IPV6_HEAD..IPV6_HEAD + 8], &later[IPV6_HEAD..IPV6_HEAD + 8]),
            (FRAGMENT, &[17, 0, 0, 1, 0, 0, 0x12, 0x34][..], &[17, 0, 0, 32, 0, 0, 0x12, 0x34][..]),
            "a head of pieces: the protocol, the place in eights with whether more follow, the number"
        );
        let joined = [&first[IPV6_HEAD + 8..], &later[IPV6_HEAD + 8..]].concat();
        assert_eq!(pseudo_sum(&own(), &far(), UDP, &joined), u16::MAX, "the first piece's checksum holds for all");
        let mut answer = later.clone();
        answer[FROM].copy_from_slice(&far());
        answer[TO].copy_from_slice(&own());
        let back = translator.to_ipv4(&answer).unwrap();
        assert_eq!((be16(&back, FRAGMENT_AT), be16(&back, ID_AT), back.len()), (Some(4), Some(0x1234), IP_HEAD + 32));
        let mut unsummed = piece(0..32, MORE_FRAGMENTS);
        unsummed[IP_HEAD + 6..IP_HEAD + 8].fill(0);
        assert_eq!(translator.to_ipv6(&unsummed), None, "a piece without a checksum cannot be given one");
        let mut echo_piece = echo(ADDRESS.octets(), FAR, icmp::REQUEST);
        echo_piece[FRAGMENT_AT..FRAGMENT_AT + 2].copy_from_slice(&MORE_FRAGMENTS.to_be_bytes());
        set_ipv4_checksum(&mut echo_piece[..IP_HEAD]);
        assert_eq!(translator.to_ipv6(&echo_piece), None, "nor an echo in pieces be summed anew");
        let large = translator.to_ipv6(&udp(ADDRESS.octets(), FAR, &[7u8; 1300])).unwrap();
        let mut answer = large.clone();
        answer[FROM].copy_from_slice(&far());
        answer[TO].copy_from_slice(&own());
        assert_eq!(
            be16(&translator.to_ipv4(&answer).unwrap(), FRAGMENT_AT),
            Some(DONT_FRAGMENT),
            "a large packet that came whole may not be cut"
        );
        assert_eq!((GROWTH, WHOLE_EVERYWHERE), (20, 1260));
    }
}

//! SNTP (RFC 4330, 5905): the service, the request and the reading of its reply; no right clock is needed.

use crate::base::bytes::be32;
use crate::base::time::MILLIS_PER_SECOND;
use crate::base::time::setting::is_settable;
use alloc::format;
use alloc::string::String;
use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Cloudflare's time service.
pub const SERVICE: &str = "time.cloudflare.com";
/// Its addresses, a family in turn; it is never asked by name, since names are asked under TLS, which needs the clock this sets.
pub const SERVERS: [IpAddr; 4] = [
    IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0xf1, 0, 0, 0, 0, 1)),
    IpAddr::V4(Ipv4Addr::new(162, 159, 200, 1)),
    IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0xf1, 0, 0, 0, 0, 0x123)),
    IpAddr::V4(Ipv4Addr::new(162, 159, 200, 123)),
];
pub const PORT: u16 = 123;
const BYTES: usize = 48;
/// The random bytes of a request's mark, in the place of the client's timestamp.
pub const MARK_BYTES: usize = 8;
const REQUEST_HEAD: u8 = 0x23;
const LEAP_SHIFT: u8 = 6;
const VERSION_SHIFT: u8 = 3;
const THREE_BITS: u8 = 0x07;
const LEAP_ALARM: u8 = 3;
const MODE_SERVER: u8 = 4;
const VERSIONS: core::ops::RangeInclusive<u8> = 3..=4;
const STRATA: core::ops::RangeInclusive<u8> = 1..=15;
const STRATUM: usize = 1;
const ORIGIN: core::ops::Range<usize> = 24..32;
const TRANSMIT: core::ops::Range<usize> = 40..48;
const HALF_BITS: u32 = 32;
const TO_1970: u64 = 2_208_988_800;
const ERA: u64 = 1 << HALF_BITS;
const SECOND_HALF: u32 = 1 << (HALF_BITS - 1);

/// A request; `mark`, a number nobody could guess, takes the place of the client's timestamp and comes back in the reply.
pub fn request(mark: [u8; MARK_BYTES]) -> [u8; BYTES] {
    let mut packet = [0u8; BYTES];
    packet[0] = REQUEST_HEAD;
    packet[TRANSMIT].copy_from_slice(&mark);
    packet
}

/// A reply's transmit time, in milliseconds of UTC since 1970; none for another request's, a refusal, no time, or one a clock is not set to.
pub fn parse_reply(packet: &[u8], mark: [u8; MARK_BYTES]) -> Option<u64> {
    let first = *packet.first().filter(|_| packet.len() >= BYTES)?;
    let (leap, version, mode) = (first >> LEAP_SHIFT, first >> VERSION_SHIFT & THREE_BITS, first & THREE_BITS);
    if leap == LEAP_ALARM
        || !VERSIONS.contains(&version)
        || mode != MODE_SERVER
        || !STRATA.contains(&packet[STRATUM])
        || packet[ORIGIN] != mark
    {
        return None;
    }
    let (seconds, fraction) = (be32(packet, TRANSMIT.start)?, be32(packet, TRANSMIT.start + 4)?);
    if seconds == 0 && fraction == 0 {
        return None;
    }
    let since_1900 = u64::from(seconds) + if seconds < SECOND_HALF { ERA } else { 0 };
    let since_1970 = since_1900.checked_sub(TO_1970).filter(|since| is_settable(*since))?;
    Some(since_1970 * MILLIS_PER_SECOND + ((u64::from(fraction) * MILLIS_PER_SECOND) >> HALF_BITS))
}

/// The seconds of UTC at a reply's arrival: half the round trip after `transmit_ms`.
pub const fn arrival_seconds(transmit_ms: u64, round_trip_ms: u64) -> u64 {
    (transmit_ms + round_trip_ms / 2 + MILLIS_PER_SECOND / 2) / MILLIS_PER_SECOND
}

/// The log line of a server that answered.
pub fn to_answered_line(server: IpAddr, ms: u128) -> String {
    format!("clock: {SERVICE} at {server} answered after {ms} ms")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const MARK: [u8; 8] = [0x9e, 0x37, 0x79, 0xb9, 0x7f, 0x4a, 0x7c, 0x15];

    fn reply(seconds: u32, fraction: u32) -> Vec<u8> {
        let mut packet = [0u8; BYTES].to_vec();
        (packet[0], packet[STRATUM]) = (0x24, 2);
        packet[ORIGIN].copy_from_slice(&MARK);
        packet[TRANSMIT.start..TRANSMIT.start + 4].copy_from_slice(&seconds.to_be_bytes());
        packet[TRANSMIT.start + 4..TRANSMIT.end].copy_from_slice(&fraction.to_be_bytes());
        packet
    }

    #[test]
    fn request_bytes() {
        let packet = request(MARK);
        assert_eq!((packet.len(), packet[0], &packet[40..48]), (48, 0x23, &MARK[..]));
        assert!(packet[1..40].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn reply_times() {
        assert_eq!(parse_reply(&reply(4_000_686_559, 0), MARK), Some(1_791_697_759_000), "2026-10-11 05:49:19 UTC");
        assert_eq!(parse_reply(&reply(4_000_686_559, 1 << 31), MARK), Some(1_791_697_759_500), "half a second on");
        assert_eq!(parse_reply(&reply(4_000_686_559, u32::MAX), MARK), Some(1_791_697_759_999));
        assert_eq!(parse_reply(&reply(3_976_214_400, 0), MARK), Some(1_767_225_600_000), "the first second of 2026");
        assert_eq!(
            parse_reply(&reply(u32::MAX, 0), MARK),
            Some(2_085_978_495_000),
            "the last second of the count, 2036-02-07 06:28:15 UTC"
        );
        assert_eq!(parse_reply(&reply(0, 1), MARK), Some(2_085_978_496_000), "and the first of the count that follows");
        assert_eq!(parse_reply(&reply(100, 0), MARK), Some(2_085_978_596_000));
        let mut longer = reply(4_000_686_559, 0);
        longer.extend_from_slice(&[0; 20]);
        assert_eq!(
            parse_reply(&longer, MARK),
            Some(1_791_697_759_000),
            "what follows the forty eight bytes is not looked at"
        );
        let mut version_3 = reply(4_000_686_559, 0);
        version_3[0] = 0x1c;
        assert_eq!(parse_reply(&version_3, MARK), Some(1_791_697_759_000), "a server that answers in version 3");
    }

    #[test]
    fn refused_replies() {
        let valid = reply(4_000_686_559, 0);
        let with = |change: &dyn Fn(&mut Vec<u8>)| {
            let mut packet = valid.clone();
            change(&mut packet);
            parse_reply(&packet, MARK)
        };
        assert_eq!(
            with(&|packet| packet[24] ^= 1),
            None,
            "an answer to another question, or made up by someone who did not see this one"
        );
        assert_eq!(with(&|packet| packet[STRATUM] = 0), None, "a refusal");
        assert_eq!(with(&|packet| packet[STRATUM] = 16), None, "a server with no time");
        assert_eq!(with(&|packet| packet[0] = 0xe4), None, "a server whose clock is not set");
        assert_eq!(with(&|packet| packet[0] = 0x23), None, "a question sent back");
        assert_eq!(with(&|packet| packet[0] = 0x25), None, "a broadcast");
        assert_eq!(with(&|packet| packet[0] = 0x14), None, "a version of old");
        assert_eq!(with(&|packet| packet[40..48].fill(0)), None, "no stamp");
        assert_eq!(parse_reply(&reply(2_208_988_799, 0), MARK), None, "a stamp of before 1970");
        assert_eq!(
            (parse_reply(&reply(2_208_988_800, 1), MARK), parse_reply(&reply(3_976_214_399, 0), MARK)),
            (None, None),
            "nor one of a year before this firmware's first, which no certificate would be checked by"
        );
        assert_eq!(with(&|packet| packet.truncate(47)), None);
        assert_eq!((parse_reply(&[], MARK), parse_reply(&valid, [0; 8])), (None, None));
    }

    #[test]
    fn server_families() {
        assert_eq!(SERVERS.map(|server| server.is_ipv6()), [true, false, true, false]);
    }

    #[test]
    fn arrival_rounding() {
        assert_eq!(arrival_seconds(1_791_697_759_000, 40), 1_791_697_759, "20 ms on its way");
        assert_eq!(arrival_seconds(1_791_697_759_400, 200), 1_791_697_760, "half a second after it left, rounded up");
        assert_eq!(arrival_seconds(1_791_697_759_499, 0), 1_791_697_759);
        assert_eq!(to_answered_line(SERVERS[1], 7), "clock: time.cloudflare.com at 162.159.200.1 answered after 7 ms");
    }
}

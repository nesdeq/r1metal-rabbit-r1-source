//! WiFi: nl80211, scans, RSN, SAE, the WPA handshakes, joins, a join's proof of reach, known networks, the radio thread's requests, the WMT driver.

pub mod credential;
pub mod join;
pub mod known;
pub mod netlink;
pub mod places;
pub mod radio;
pub mod reach;
pub mod rsn;
pub mod sae;
pub mod scan;
pub mod search;
pub mod station;
pub mod wmt;
pub mod wpa;

use alloc::format;
use alloc::string::String;
use core::time::Duration;

/// The limit of a scan, a join and an address request: one that never ends would hold the radio on for good.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The part of `limit` left `elapsed` after the start; none once it is over.
pub fn time_left(elapsed: Duration, limit: Duration) -> Option<Duration> {
    limit.checked_sub(elapsed).filter(|remaining| !remaining.is_zero())
}

/// Directory of the known networks, the device's secret and the join places, under the data partition's private directory.
pub const DIRECTORY: &str = ".r1metal/wifi";

const VENDOR: u8 = 221;

/// A core line with the link's log tag.
pub fn to_wifi_line(line: &str) -> String {
    format!("wifi: {line}")
}

fn elements(mut bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    core::iter::from_fn(move || {
        let element = bytes.get(..2 + usize::from(*bytes.get(1)?))?;
        bytes = &bytes[element.len()..];
        Some(element)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_limit() {
        let (second, milli) = (Duration::from_secs(1), Duration::from_millis(1));
        let left = |elapsed: Duration| time_left(elapsed, TIMEOUT);
        assert_eq!(
            [left(Duration::ZERO), left(9 * second), left(TIMEOUT - milli)],
            [Some(TIMEOUT), Some(second), Some(milli)],
            "the end is waited for as long as the patience left"
        );
        assert_eq!([left(TIMEOUT), left(TIMEOUT + second)], [None, None], "at the limit it has not ended in time");
        assert_eq!(
            [time_left(milli, second), time_left(second, second)],
            [Some(second - milli), None],
            "a shorter limit ends sooner"
        );
    }

    #[test]
    fn tagged_line() {
        assert_eq!(to_wifi_line("not joined: Late"), "wifi: not joined: Late");
    }
}

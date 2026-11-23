// What the services are paid in and by: millionths of a dollar and their text in dollars and cents, and the seconds a service paid by time counts.

use crate::base::json::number_at;
use crate::base::time::SECONDS_PER_MINUTE;
use alloc::format;
use alloc::string::String;

pub(super) const MICROS_PER_DOLLAR: u64 = 1_000_000;
const CENTS_PER_DOLLAR: u64 = 100;
const MICROS_PER_CENT: u64 = MICROS_PER_DOLLAR / CENTS_PER_DOLLAR;
const UNDER_A_CENT: &str = "<$0.01";

pub(crate) fn dollars(micros: u64) -> String {
    let cents = micros.saturating_add(MICROS_PER_CENT / 2) / MICROS_PER_CENT;
    if cents == 0 && micros > 0 {
        return UNDER_A_CENT.into();
    }
    format!("${}.{:02}", cents / CENTS_PER_DOLLAR, cents % CENTS_PER_DOLLAR)
}

pub(super) const fn cost_by_minute(seconds: u64, micros_per_minute: u64) -> u64 {
    seconds.saturating_mul(micros_per_minute) / SECONDS_PER_MINUTE
}

// The seconds an answer counts at `path`, a second begun counted whole.
pub(crate) fn parse_whole_seconds(document: &str, path: &[&str]) -> Option<u64> {
    let seconds = number_at::<f64>(document, path).filter(|seconds| *seconds >= 0.0)?;
    Some((seconds as u64).saturating_add(u64::from(seconds > (seconds as u64) as f64)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_round_up() {
        let parse = |seconds: &str| {
            parse_whole_seconds(&alloc::format!("{{\"usage\": {{\"seconds\": {seconds}}}}}"), &["usage", "seconds"])
        };
        assert_eq!(
            ["0", "27", "27.0", "27.25", "0.01", "-3", "\"27\""].map(parse),
            [Some(0), Some(27), Some(27), Some(28), Some(1), None, None]
        );
        assert_eq!(parse("1e30"), Some(u64::MAX), "more than a count holds is the most it holds");
        assert_eq!(parse_whole_seconds("{}", &["usage", "seconds"]), None);
    }

    #[test]
    fn dollars_text() {
        assert_eq!(
            [0, 1, 1_200, 4_999, 5_000, 14_999, 15_000, 1_234_567, 100_000_000].map(dollars),
            ["$0.00", "<$0.01", "<$0.01", "<$0.01", "$0.01", "$0.01", "$0.02", "$1.23", "$100.00"],
            "what is booked and is less than half a cent is not nothing"
        );
    }
}

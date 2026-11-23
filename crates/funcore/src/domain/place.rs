//! Where the device is, as a place service tells it.

use crate::base::json;
use crate::base::time::SECONDS_PER_MINUTE;
use crate::base::time::offset;
use crate::codec::http::{CLOSE, USER_AGENT};
use crate::openai::chat::request::Location;
use alloc::format;
use alloc::string::{String, ToString};

/// The place service, which tells over HTTPS the offset from UTC and the place a question comes from.
pub const SERVICE: &str = "https://ipwho.is/?fields=success,country_code,region,city,timezone";
/// The headers it is asked with.
pub const HEADERS: [(&str, &str); 2] = [("User-Agent", USER_AGENT), ("Connection", CLOSE)];
pub const DIRECTORY: &str = ".r1metal/clock";
pub const OFFSET_FILE: &str = "place";
pub const LOCATION_FILE: &str = "where";
/// An upper bound on an answer of the place service.
pub const ANSWER_BYTES: u64 = 512;
/// The error text of a service address that makes no URL.
pub const NO_URL: &str = "the place service's address makes no URL";
const ZONE: &str = "timezone";

/// The error text of an answer of that status that gives no offset.
pub fn to_unplaced_text(status: u16) -> String {
    format!("an answer of status {status} that tells no place")
}

/// The log line of an unchanged offset, after that many milliseconds.
pub fn to_same_line(minutes: i16, ms: u128) -> String {
    format!("net: place: {} as before, in {ms} ms", offset::to_text(minutes))
}

/// The log line of a changed offset.
pub fn to_moved_line(minutes: i16, ms: u128) -> String {
    format!("net: place: the clocks here are at {}, in {ms} ms", offset::to_text(minutes))
}

/// True where a place is not the stored one.
pub fn is_new(stored: Option<&Location>, place: &Location) -> bool {
    stored != Some(place)
}

fn is_success(answer: &str) -> bool {
    json::value_at(answer, &["success"]) == Some("true")
}

/// Minutes a place's clocks are ahead of UTC; none unless the service knows and a zone has that offset.
pub fn parse_offset(answer: &str) -> Option<i16> {
    let seconds: i64 = json::number_at(answer, &[ZONE, "offset"])?;
    let per_minute = SECONDS_PER_MINUTE as i64;
    if !is_success(answer) || seconds % per_minute != 0 {
        return None;
    }
    offset::checked(seconds / per_minute)
}

/// An offset as its file holds it: its minutes.
pub fn to_offset_file(offset: i16) -> String {
    offset.to_string()
}

/// True where the service tells another offset than its last: only a change of its word moves an offset set by hand.
pub fn has_offset_changed(stored: Option<&str>, offset: i16) -> bool {
    stored.and_then(|text| text.trim().parse().ok()) != Some(offset)
}

/// The place a service's answer names; none unless it knows the place and names a city.
pub fn parse_answer(answer: &str) -> Option<Location> {
    let field = |path: &[&str]| json::text_at(answer, path).map(json::unescape).unwrap_or_default();
    let city = field(&["city"]);
    if !is_success(answer) || city.trim().is_empty() {
        return None;
    }
    Some(Location { country: field(&["country_code"]), region: field(&["region"]), city, zone: field(&[ZONE, "id"]) })
}

/// The place as its file holds it: a line for each of its parts.
pub fn to_text(place: &Location) -> String {
    format!("{}\n{}\n{}\n{}\n", place.country, place.region, place.city, place.zone)
}

/// The place a stored text holds; none for a text without a city.
pub fn parse(text: &str) -> Option<Location> {
    let mut lines = text.lines().map(str::trim);
    let mut line = || lines.next().unwrap_or_default().to_string();
    let (country, region, city, zone) = (line(), line(), line(), line());
    (!city.is_empty()).then_some(Location { country, region, city, zone })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer_with(offset: &str) -> String {
        format!("{{\"success\":true,\"timezone\":{{\"id\":\"Europe/Berlin\",\"offset\":{offset},\"utc\":\"+02:00\"}}}}")
    }

    #[test]
    fn offset_from_answer() {
        assert_eq!(parse_offset(&answer_with("7200")), Some(120));
        assert_eq!(parse_offset("{\"timezone\":{\"offset\":-12600},\"success\":true}"), Some(-210));
        assert_eq!(
            parse_offset(
                "{\n    \"success\": true,\n    \"country_code\": \"NP\",\n    \"timezone\": {\n        \"id\": \"Asia/Kathmandu\",\n        \"offset\": 20700\n    }\n}"
            ),
            Some(345)
        );
        assert_eq!(
            (
                parse_offset(&answer_with("0")),
                parse_offset(&answer_with("50400")),
                parse_offset(&answer_with("-39600"))
            ),
            (Some(0), Some(840), Some(-660))
        );
        for invalid in [
            String::from("{\"success\":false,\"message\":\"Reserved range\",\"timezone\":{\"offset\":7200}}"),
            String::from("{\"success\":\"true\",\"timezone\":{\"offset\":7200}}"),
            String::from("{\"success\":true}"),
            String::from("{\"success\":true,\"offset\":7200}"),
            answer_with("7230"),
            answer_with("50460"),
            answer_with("-39660"),
            answer_with("\"7200\""),
            answer_with("99999999999999999999"),
            String::from("{\"success\":true,\"timezone\":{\"offset\":7200"),
            String::from("<html>too many requests</html>"),
            String::new(),
        ] {
            assert_eq!(parse_offset(&invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn offset_changes_by_service() {
        assert!(!has_offset_changed(Some(&to_offset_file(120)), 120) && !has_offset_changed(Some("-210\n"), -210));
        assert!(
            has_offset_changed(Some("120"), 60) && has_offset_changed(None, 0) && has_offset_changed(Some("noon"), 0)
        );
    }

    #[test]
    fn place_lines() {
        assert_eq!(to_same_line(120, 9), "net: place: UTC+02:00 as before, in 9 ms");
        assert_eq!(to_moved_line(120, 9), "net: place: the clocks here are at UTC+02:00, in 9 ms");
        assert_eq!(to_unplaced_text(502), "an answer of status 502 that tells no place");
        let place = parse("AT\nVienna\nVienna\nEurope/Vienna\n").unwrap();
        assert!(is_new(None, &place) && !is_new(Some(&place), &place));
    }

    #[test]
    fn place_round_trip() {
        let answer = "{\"success\":true,\"country_code\":\"AT\",\"region\":\"Vienna\",\"city\":\"Vienna\",\"timezone\":{\"id\":\"Europe/Vienna\",\"abbr\":\"CEST\",\"is_dst\":true,\"offset\":7200,\"utc\":\"+02:00\"}}";
        let place = parse_answer(answer).expect("a place");
        assert_eq!(
            place,
            Location {
                country: "AT".into(),
                region: "Vienna".into(),
                city: "Vienna".into(),
                zone: "Europe/Vienna".into()
            }
        );
        assert_eq!(parse(&to_text(&place)), Some(place));
        assert_eq!(
            parse_answer("{\"success\":true,\"city\":\"S\\u00e3o Paulo\"}").map(|place| place.city),
            Some("S\u{E3}o Paulo".into())
        );
        for invalid in [
            "{\"success\":false,\"city\":\"Vienna\"}",
            "{\"success\":true,\"city\":\"\"}",
            "{\"success\":true}",
            "<html>too many requests</html>",
        ] {
            assert_eq!(parse_answer(invalid), None, "{invalid:?}");
        }
        assert_eq!((parse(""), parse("AT\nVienna\n\nEurope/Vienna\n")), (None, None));
    }
}

use crate::base::text::to_capitalized;
use crate::base::time::stamp::{parse_compact_day, parse_stamp};
use alloc::format;
use alloc::string::String;

pub(crate) fn to_edition_name(day: &str) -> String {
    parse_compact_day(day).map_or_else(|| day.into(), |day| day.to_date_words(true, false))
}

pub(super) fn to_rundown_heading(day: &str, topic: &str) -> String {
    let date = parse_compact_day(day).map_or_else(|| day.into(), |day| day.to_date_words(false, true));
    format!("{} of {date}", to_capitalized(topic))
}

pub(crate) fn to_entry_name(stem: &str) -> String {
    match parse_stamp(stem) {
        Some(moment) => format!("{} {}", moment.to_date_words(false, false), moment.to_time_text()),
        None => stem.into(),
    }
}

pub(super) fn to_sentence_date(stem: &str) -> String {
    match parse_stamp(stem) {
        Some(moment) => format!("{}, {}", moment.to_date_words(false, true), moment.to_time_text()),
        None => stem.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEM: &str = "20261012_190328";

    #[test]
    fn entry_names() {
        assert_eq!(
            (to_entry_name(STEM), to_sentence_date(STEM)),
            ("12 october 2026 19:03".into(), "12 October 2026, 19:03".into())
        );
        assert_eq!(
            (to_entry_name("20261012_190328_1"), to_entry_name("memo")),
            ("12 october 2026 19:03".into(), "memo".into())
        );
    }

    #[test]
    fn edition_names() {
        assert_eq!(
            (to_edition_name("20261013"), to_edition_name("20260930")),
            ("tuesday 13 october 2026".into(), "wednesday 30 september 2026".into())
        );
        assert_eq!(
            (to_edition_name("2026101"), to_edition_name("20261313"), to_edition_name("today")),
            ("2026101".into(), "20261313".into(), "today".into())
        );
        assert_eq!(to_rundown_heading("today", "news"), "News of today");
        assert_eq!(
            to_rundown_heading("20261013", "motorsport"),
            "Motorsport of 13 October 2026",
            "the rundown's name and the month, each with a capital"
        );
    }
}

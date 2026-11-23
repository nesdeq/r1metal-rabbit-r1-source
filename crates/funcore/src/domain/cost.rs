//! What OpenAI's services were paid, as lines: each app in all and a line a month, with their pages.

use super::device::HAS_EXTRAS;
use super::scene::{AUDIO_NOTE, BELL, CAMERA, CHAT, NEWS, POKEDEX, SIGNAL, TRANSLATOR, WEB};
use crate::base::time::SECONDS_PER_MINUTE;
use crate::openai::ledger::{KINDS, Kind, Ledger, Month, Paid};
use crate::openai::money::dollars;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::iter;

const OPENAI: &str = "OPENAI";
const UNKNOWN: &str = "UNKNOWN";
pub(crate) const MONTH: &str = "MONTH";
const SUMMARIES: &str = "SUMMARIES";
const READ_ALOUD: &str = "READ ALOUD";

struct App {
    name: &'static str,
    is_permanent: bool,
    kinds: &'static [(Kind, &'static str)],
}

const APPS: [App; 9] = [
    App { name: CHAT, is_permanent: true, kinds: &[(Kind::Chat, "TURNS"), (Kind::Search, "WEB SEARCHES")] },
    App {
        name: SIGNAL,
        is_permanent: true,
        kinds: &[(Kind::Auto, "AUTO REPLIES"), (Kind::Dictation, "MESSAGES SAID"), (Kind::SignalReading, READ_ALOUD)],
    },
    App { name: CAMERA, is_permanent: true, kinds: &[(Kind::Picture, "PICTURES")] },
    App {
        name: AUDIO_NOTE,
        is_permanent: true,
        kinds: &[(Kind::Note, "NOTES"), (Kind::NoteSummary, SUMMARIES), (Kind::NoteReading, READ_ALOUD)],
    },
    App {
        name: TRANSLATOR,
        is_permanent: true,
        kinds: &[(Kind::Session, "SESSIONS"), (Kind::TalkSummary, SUMMARIES), (Kind::TalkReading, READ_ALOUD)],
    },
    App { name: NEWS, is_permanent: true, kinds: &[(Kind::Rundown, "RUNDOWNS"), (Kind::NewsReading, READ_ALOUD)] },
    App { name: WEB, is_permanent: true, kinds: &[(Kind::Web, "PAGES"), (Kind::WebReading, READ_ALOUD)] },
    App { name: BELL, is_permanent: true, kinds: &[(Kind::Bell, "REQUESTS")] },
    App { name: POKEDEX, is_permanent: HAS_EXTRAS, kinds: &[(Kind::Pokedex, "NAMES SAID")] },
];

/// A line of the cost or of a page of it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Figure {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) page: Option<Page>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Page {
    pub(crate) name: &'static str,
    pub(crate) subject: String,
    pub(crate) lines: Vec<Figure>,
}

impl Figure {
    fn new(name: impl Into<String>, value: impl Into<String>) -> Figure {
        Figure { name: name.into(), value: value.into(), page: None }
    }

    /// The first line alone, for a ledger that cannot be read.
    pub fn unknown() -> Figure {
        Figure::new(OPENAI, UNKNOWN)
    }

    // The total's value in two: the amount it begins with, and the words after it.
    pub(crate) fn split_amount(&self) -> (&str, &str) {
        self.value.split_once(' ').unwrap_or((&self.value, ""))
    }

    // The total's value as far as it tells of the month.
    pub(crate) fn month_part(&self) -> &str {
        self.value.split_once(", ").map_or(&self.value, |(month, _)| month)
    }
}

fn to_minutes(seconds: u64) -> u64 {
    seconds.saturating_add(SECONDS_PER_MINUTE / 2) / SECONDS_PER_MINUTE
}

fn is_listed(app: &App, paid: &[Paid; KINDS]) -> bool {
    app.is_permanent || app.kinds.iter().any(|kind| paid[kind.0 as usize] != Paid::default())
}

fn cost_of(app: &App, paid: &[Paid; KINDS]) -> u64 {
    app.kinds.iter().fold(0, |total: u64, kind| total.saturating_add(paid[kind.0 as usize].micros))
}

fn kinds_of(app: &App, paid: &[Paid; KINDS], seconds: u64) -> Vec<Figure> {
    let to_figure = |&(kind, name): &(Kind, &'static str)| {
        let Paid { count, micros } = paid[kind as usize];
        let duration = if kind == Kind::Note { format!(", {} MIN", to_minutes(seconds)) } else { String::new() };
        Figure::new(name, format!("{} FOR {count}{duration}", dollars(micros)))
    };
    app.kinds.iter().map(to_figure).collect()
}

fn app_figure(app: &App, (paid, seconds): (&[Paid; KINDS], u64), subject: impl FnOnce(&str) -> String) -> Figure {
    let cost = dollars(cost_of(app, paid));
    let page = Page { name: app.name, subject: subject(&cost), lines: kinds_of(app, paid, seconds) };
    Figure { page: Some(page), ..Figure::new(app.name, cost) }
}

fn month_figure(month: &Month, apps: &[&App]) -> Figure {
    let (name, cost) = (month.name(), dollars(month.micros));
    let usage = (&month.paid, month.seconds);
    let lines = apps.iter().map(|app| app_figure(app, usage, |cost| format!("{name}, {cost}"))).collect();
    let page = Page { name: MONTH, subject: format!("{name}, {cost}"), lines };
    Figure { page: Some(page), ..Figure::new(name, cost) }
}

fn to_summary(ledger: &Ledger, year: u32, month: u8) -> String {
    let total = ledger.months.iter().fold(0, |total: u64, entry| total.saturating_add(entry.micros));
    format!("{} THIS MONTH, {} IN ALL", dollars(ledger.spent(year, month)), dollars(total))
}

fn add_up(ledger: &Ledger) -> ([Paid; KINDS], u64) {
    let mut paid = [Paid::default(); KINDS];
    let mut seconds = 0;
    for month in &ledger.months {
        for (total, entry) in paid.iter_mut().zip(month.paid) {
            *total += entry;
        }
        seconds = month.seconds.saturating_add(seconds);
    }
    (paid, seconds)
}

/// Every line of the cost out of a ledger.
pub fn to_figures(ledger: &Ledger, year: u32, month: u8) -> Vec<Figure> {
    let total = Figure::new(OPENAI, to_summary(ledger, year, month));
    let (paid, seconds) = add_up(ledger);
    let apps: Vec<&App> = APPS.iter().filter(|app| is_listed(app, &paid)).collect();
    let app_figures = apps.iter().map(|app| app_figure(app, (&paid, seconds), |cost| format!("{cost} IN ALL")));
    let months = ledger.months.iter().map(|month| month_figure(month, &apps));
    iter::once(total).chain(app_figures).chain(months).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::ledger::Entry;
    use alloc::vec;

    fn figure<'a>(figures: &'a [Figure], name: &str) -> &'a Figure {
        figures.iter().find(|figure| figure.name == name).expect(name)
    }

    fn page(figure: &Figure) -> (&'static str, &str, Vec<(&str, &str)>) {
        let page = figure.page.as_ref().expect("a page");
        (page.name, &page.subject, page.lines.iter().map(|line| (line.name.as_str(), line.value.as_str())).collect())
    }

    // October: a note of 90 s for $0.009 and a rundown for $0.03; September: an auto reply; August: a name said to the Pokedex.
    fn ledger() -> Ledger {
        let pokedex = format!("2026-08 0 0 0 0 450 0 0 0 0{} 1 450\n", " 0 0".repeat(6));
        let mut ledger = Ledger::parse(&format!("2026-10 0 0 0 0 9000 1 90 0 9000\n{pokedex}"));
        ledger.add(2026, 10, Entry::one(Kind::Rundown), 30_000);
        ledger.add(2026, 9, Entry::one(Kind::Auto), 2_000);
        ledger
    }

    #[test]
    fn lines_in_order() {
        let figures = to_figures(&ledger(), 2026, 10);
        assert_eq!(
            figures.iter().map(|figure| figure.name.as_str()).collect::<Vec<_>>(),
            [
                OPENAI, CHAT, SIGNAL, CAMERA, AUDIO_NOTE, TRANSLATOR, NEWS, WEB, BELL, POKEDEX, "2026-10", "2026-09",
                "2026-08"
            ],
            "the total, every app in the tree's order, the Pokedex once something was paid for it, then the months, the newest first"
        );
        assert_eq!((figures[0].value.as_str(), &figures[0].page), ("$0.04 THIS MONTH, $0.04 IN ALL", &None));
        assert_eq!(
            (figures[0].split_amount(), figures[0].month_part()),
            (("$0.04", "THIS MONTH, $0.04 IN ALL"), "$0.04 THIS MONTH"),
            "the total's parts as its lines show them"
        );
        assert_eq!(
            (Figure::unknown().split_amount(), Figure::unknown().month_part()),
            ((UNKNOWN, ""), UNKNOWN),
            "a total that tells no amount is one part"
        );
        assert_eq!(to_figures(&ledger(), 2026, 11)[0].value, "$0.00 THIS MONTH, $0.04 IN ALL");
        // The Pokedex as firmware without the extras has it, which the tests' build never does.
        let extra = &APPS[APPS.len() - 1];
        let pokedex = App { name: extra.name, is_permanent: false, kinds: extra.kinds };
        assert!(
            !is_listed(&pokedex, &[Paid::default(); KINDS]) && is_listed(&pokedex, &add_up(&ledger()).0),
            "without the extras the Pokedex is listed once something was paid for it, and not before"
        );
        assert_eq!(Figure::unknown(), Figure { name: OPENAI.into(), value: UNKNOWN.into(), page: None });
    }

    #[test]
    fn app_and_month_pages() {
        let figures = to_figures(&ledger(), 2026, 10);
        let notes = figure(&figures, AUDIO_NOTE);
        assert_eq!(
            (notes.value.as_str(), page(notes)),
            (
                "$0.01",
                (
                    AUDIO_NOTE,
                    "$0.01 IN ALL",
                    vec![("NOTES", "$0.01 FOR 1, 2 MIN"), ("SUMMARIES", "$0.00 FOR 0"), ("READ ALOUD", "$0.00 FOR 0")]
                )
            ),
            "an app's page tells each kind's cost and count over all months, a note's minutes with it"
        );
        assert_eq!(page(figure(&figures, SIGNAL)).2[0], ("AUTO REPLIES", "<$0.01 FOR 1"));
        let october = figure(&figures, "2026-10");
        let (name, subject, lines) = page(october);
        assert_eq!((october.value.as_str(), name, subject), ("$0.04", MONTH, "2026-10, $0.04"));
        assert_eq!(
            lines[..7],
            [
                (CHAT, "$0.00"),
                (SIGNAL, "$0.00"),
                (CAMERA, "$0.00"),
                (AUDIO_NOTE, "$0.01"),
                (TRANSLATOR, "$0.00"),
                (NEWS, "$0.03"),
                (WEB, "$0.00")
            ],
            "a month's page tells what each app cost that month"
        );
        let share = &october.page.as_ref().unwrap().lines[5];
        assert_eq!(
            page(share),
            (NEWS, "2026-10, $0.03", vec![("RUNDOWNS", "$0.03 FOR 1"), ("READ ALOUD", "$0.00 FOR 0")]),
            "and an app's share of it each kind's cost and count that month"
        );
        let september = figure(&figures, "2026-09").page.as_ref().unwrap();
        assert_eq!(page(&september.lines[3]).2[0], ("NOTES", "$0.00 FOR 0, 0 MIN"), "a month's minutes are its own");
        assert_eq!([29, 30, 89, 90].map(to_minutes), [0, 1, 1, 2], "minutes to the nearest, half a minute up");
    }
}

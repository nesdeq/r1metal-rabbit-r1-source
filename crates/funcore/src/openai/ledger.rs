//! The ledger of what OpenAI's services were paid, and its file: a line a month, the newest first, each kind apart, and a month's total where its kinds are forgotten.

use super::image::Usage;
use super::tokens;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// The kind of a summary: of a note, or of a conversation of two languages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SummaryKind {
    Note,
    Talk,
}

/// The ledger's file, in `key::DIRECTORY`.
pub const FILE: &str = "ledger";
const MONTHS: core::ops::RangeInclusive<u8> = 1..=12;
pub(crate) const KINDS: usize = 18;
const FIELDS: usize = 2 * KINDS + 5;
const REQUIRED_FIELDS: usize = 5;
const OLD_FORM_WORDS: [usize; 4] = [6, 8, 17, 22];

/// The kinds of thing the services are paid for, in the ledger's order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Picture,
    // A note whose transcript is complete and stored; its sound, in whatever pieces, is its seconds and its cost.
    Note,
    NoteSummary,
    // A rundown of a day's edition; a request for the rundowns, read by the language model or written down, adds its cost and counts none.
    Rundown,
    // A translator session.
    Session,
    // A reading of a day's rundown, from a touch on the robot to the end of the text, its leaving or a failure, in which at least one passage was paid for.
    NewsReading,
    // A turn of the conversation: the words written down, the model's answer and its voice; and the day's recollection.
    Chat,
    // A web search of a turn of the conversation.
    Search,
    // A name said to the Pokédex and written down.
    Pokedex,
    // A summary of a translated conversation.
    TalkSummary,
    // Counted as a news reading.
    NoteReading,
    // Counted as a news reading.
    TalkReading,
    // A message said for Signal and written down.
    Dictation,
    // A message of Signal's auto mode.
    Auto,
    // Counted as a news reading.
    SignalReading,
    // A page the model resolved a request to or wrote for the browser; a request said and written down adds its cost and counts none.
    Web,
    // Counted as a news reading.
    WebReading,
    // A request to the bell the language model read; one said adds its writing down and counts none.
    Bell,
}

/// The kinds Signal is paid for, which the ledger forgets once the phone unlinks the R1.
pub const SIGNAL: [Kind; 3] = [Kind::Dictation, Kind::Auto, Kind::SignalReading];

/// What a cost is entered as: its kind, and how many of the kind it counts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    kind: Kind,
    count: u64,
}

impl Entry {
    /// One of `kind`.
    pub const fn one(kind: Kind) -> Entry {
        Entry::of(kind, 1)
    }

    // Work beside what `kind` counts: it adds its cost and counts none.
    pub(super) const fn work(kind: Kind) -> Entry {
        Entry::of(kind, 0)
    }

    pub(super) const fn of(kind: Kind, count: u64) -> Entry {
        Entry { kind, count }
    }

    // A summary of that kind.
    pub(super) const fn summary(kind: SummaryKind) -> Entry {
        match kind {
            SummaryKind::Note => Entry::one(Kind::NoteSummary),
            SummaryKind::Talk => Entry::one(Kind::TalkSummary),
        }
    }
}

#[cfg(test)]
impl Kind {
    const ALL: [Kind; KINDS] = [
        Kind::Picture,
        Kind::Note,
        Kind::NoteSummary,
        Kind::Rundown,
        Kind::Session,
        Kind::NewsReading,
        Kind::Chat,
        Kind::Search,
        Kind::Pokedex,
        Kind::TalkSummary,
        Kind::NoteReading,
        Kind::TalkReading,
        Kind::Dictation,
        Kind::Auto,
        Kind::SignalReading,
        Kind::Web,
        Kind::WebReading,
        Kind::Bell,
    ];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Paid {
    pub(crate) count: u64,
    pub(crate) micros: u64,
}

impl core::ops::AddAssign for Paid {
    fn add_assign(&mut self, other: Paid) {
        self.count = self.count.saturating_add(other.count);
        self.micros = self.micros.saturating_add(other.micros);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Month {
    year: u32,
    month: u8,
    pub(crate) paid: [Paid; KINDS],
    usage: Usage,
    pub(crate) seconds: u64,
    pub(crate) micros: u64,
}

impl Month {
    pub(crate) fn name(&self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
}

/// The ledger: a month to a line, the newest first.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Ledger {
    pub(crate) months: Vec<Month>,
}

impl Ledger {
    /// Parses the file's text. A line is skipped that does not begin with a month's name and the `REQUIRED_FIELDS` numbers, or that is as long as a line of an older form; after those a field that is no number or is missing counts for nothing, and words after the last field are ignored.
    pub fn parse(text: &str) -> Ledger {
        let mut ledger = Ledger::default();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            let is_old = OLD_FORM_WORDS.contains(&words.clone().count());
            let month = words.next().and_then(|name| name.split_once('-')).and_then(|(year, month)| {
                Some((year.parse::<u32>().ok()?, month.parse::<u8>().ok().filter(|month| MONTHS.contains(month))?))
            });
            let fields: Vec<Option<u64>> = words.take(FIELDS).map(|word| word.parse().ok()).collect();
            let has_required = fields.get(..REQUIRED_FIELDS).is_some_and(|first| !first.contains(&None));
            let Some((year, month)) = month.filter(|_| has_required && !is_old) else {
                continue;
            };
            let numbers: [u64; FIELDS] = core::array::from_fn(|at| fields.get(at).copied().flatten().unwrap_or(0));
            let [pictures, text, image, out, micros, notes, seconds, picture_micros, note_micros, kinds @ ..] = numbers;
            let entry = ledger.month_mut(year, month);
            let [picture, note, others @ ..] = &mut entry.paid;
            *picture += Paid { count: pictures, micros: picture_micros };
            *note += Paid { count: notes, micros: note_micros };
            for (paid, &[count, micros]) in others.iter_mut().zip(kinds.as_chunks().0) {
                *paid += Paid { count, micros };
            }
            (entry.micros, entry.seconds) =
                (entry.micros.saturating_add(micros), entry.seconds.saturating_add(seconds));
            entry.usage += Usage { text, image, out };
        }
        ledger
    }

    /// The file's text, a line a month: name, pictures, their tokens of text in, picture in and picture out, cost in millionths of a dollar, notes, the seconds of their sound, the cost of the pictures and of the sound, then each other kind's count and cost in `Kind`'s order.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for entry in &self.months {
            let Month { paid: [pictures, notes, others @ ..], usage, seconds, micros, .. } = *entry;
            let head = [pictures.count, usage.text, usage.image, usage.out, micros, notes.count, seconds];
            let kinds = others.into_iter().flat_map(|paid| [paid.count, paid.micros]);
            text.push_str(&entry.name());
            for field in head.into_iter().chain([pictures.micros, notes.micros]).chain(kinds) {
                let _ = write!(text, " {field}");
            }
            text.push('\n');
        }
        text
    }

    fn month_mut(&mut self, year: u32, month: u8) -> &mut Month {
        let at = self
            .months
            .iter()
            .position(|entry| (entry.year, entry.month) <= (year, month))
            .unwrap_or(self.months.len());
        if self.months.get(at).is_none_or(|entry| (entry.year, entry.month) != (year, month)) {
            self.months.insert(at, Month { year, month, ..Month::default() });
        }
        &mut self.months[at]
    }

    /// Adds a cost of that month as `entry` tells.
    pub fn add(&mut self, year: u32, month: u8, entry: Entry, micros: u64) {
        let line = self.month_mut(year, month);
        line.paid[entry.kind as usize] += Paid { count: entry.count, micros };
        line.micros = line.micros.saturating_add(micros);
    }

    /// Adds a picture drawn in that month.
    pub fn add_picture(&mut self, year: u32, month: u8, usage: Usage) {
        self.add(year, month, Entry::one(Kind::Picture), usage.micros());
        self.month_mut(year, month).usage += usage;
    }

    /// Adds a picture paid for and not sent, as far as the service counted it; true if it was added.
    pub fn add_failed_picture(&mut self, year: u32, month: u8, usage: Usage) -> bool {
        let is_counted = usage.is_counted();
        if is_counted {
            self.add_picture(year, month, usage);
        }
        is_counted
    }

    // Adds failed work of the language model as `entry` tells, as far as the service counted it, with what its web searches cost; true if it was added.
    pub(super) fn add_failed_work(
        &mut self,
        year: u32,
        month: u8,
        entry: Entry,
        (usage, search_micros): (tokens::Usage, u64),
    ) -> bool {
        let is_counted = usage.is_counted() || search_micros > 0;
        if is_counted {
            self.add(year, month, entry, usage.micros().saturating_add(search_micros));
        }
        is_counted
    }

    // Adds no note, which `add_note` does.
    pub(super) fn add_sound(&mut self, year: u32, month: u8, seconds: u64, micros: u64) {
        self.add(year, month, Entry::work(Kind::Note), micros);
        let line = self.month_mut(year, month);
        line.seconds = line.seconds.saturating_add(seconds);
    }

    /// Adds a note whose transcript was completed and stored in that month; its cost comes with its pieces.
    pub fn add_note(&mut self, year: u32, month: u8) {
        self.add(year, month, Entry::one(Kind::Note), 0);
    }

    /// Adds a translator session held in that month, one that cost nothing being no entry; true if it was added.
    pub fn add_session(&mut self, year: u32, month: u8, micros: u64) -> bool {
        let has_cost = micros > 0;
        if has_cost {
            self.add(year, month, Entry::one(Kind::Session), micros);
        }
        has_cost
    }

    /// Adds a text read aloud, all its passages together: a reply's voice adds to its turns' cost and counts no turn, any other reading counts one whatever it cost.
    pub fn add_reading(&mut self, year: u32, month: u8, kind: Kind, micros: u64) {
        self.add(year, month, Entry::of(kind, u64::from(kind != Kind::Chat)), micros);
    }

    // What that month cost in millionths of a dollar, the kinds it forgot included.
    pub(crate) fn spent(&self, year: u32, month: u8) -> u64 {
        let entry = self.months.iter().find(|entry| (entry.year, entry.month) == (year, month));
        entry.map_or(0, |entry| entry.micros)
    }

    pub(super) fn is_within(&self, year: u32, month: u8, limit: u64) -> bool {
        self.spent(year, month) < limit
    }

    /// Takes those kinds out of every month, each month keeping its total; true if any of them held anything.
    pub fn forget(&mut self, kinds: &[Kind]) -> bool {
        let mut is_changed = false;
        for month in &mut self.months {
            for kind in kinds {
                is_changed |= core::mem::take(&mut month.paid[*kind as usize]) != Paid::default();
            }
        }
        is_changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openai::image::MEDIUM;

    fn untyped_micros(month: &Month) -> u64 {
        month.micros.saturating_sub(month.paid.iter().fold(0, |all: u64, paid| all.saturating_add(paid.micros)))
    }

    #[test]
    fn months_and_kinds() {
        let mut ledger = Ledger::default();
        assert_eq!(ledger.to_text(), String::new());
        ledger.add_picture(2026, 10, MEDIUM);
        ledger.add_picture(2026, 9, MEDIUM);
        ledger.add_picture(2026, 10, MEDIUM);
        ledger.add_picture(2027, 1, Usage { text: 400, image: 1024, out: 1756 });
        ledger.add_sound(2026, 10, 120, 9_000);
        ledger.add_sound(2026, 10, 67, 5_025);
        ledger.add_note(2026, 10);
        ledger.add(2026, 10, Entry::summary(SummaryKind::Note), 1_200);
        ledger.add(2026, 10, Entry::one(Kind::Rundown), 15_583);
        ledger.add(2026, 10, Entry::one(Kind::Rundown), 16_009);
        ledger.add(2026, 10, Entry::work(Kind::Rundown), 400);
        ledger.add_session(2026, 10, 85_000);
        ledger.add_reading(2026, 10, Kind::NewsReading, 6_300);
        ledger.add(2026, 10, Entry::of(Kind::Chat, 3), 2_400);
        ledger.add(2026, 10, Entry::work(Kind::Chat), 600);
        ledger.add(2026, 10, Entry::of(Kind::Search, 5), 50_000);
        ledger.add(2026, 10, Entry::one(Kind::Pokedex), 450);
        ledger.add(2026, 10, Entry::summary(SummaryKind::Talk), 900);
        ledger.add_reading(2026, 10, Kind::NoteReading, 700);
        ledger.add_reading(2026, 10, Kind::TalkReading, 500);
        ledger.add(2026, 10, Entry::one(Kind::Dictation), 300);
        ledger.add(2026, 10, Entry::one(Kind::Auto), 2_000);
        ledger.add_reading(2026, 10, Kind::SignalReading, 400);
        ledger.add(2026, 10, Entry::one(Kind::Web), 700);
        ledger.add(2026, 10, Entry::work(Kind::Web), 50);
        ledger.add_reading(2026, 10, Kind::WebReading, 900);
        ledger.add(2026, 10, Entry::one(Kind::Bell), 300);
        ledger.add(2026, 10, Entry::work(Kind::Bell), 50);
        ledger.add_sound(2026, 11, 60, 4_500);
        let zeros = " 0".repeat(18);
        assert_eq!(
            ledger.to_text(),
            format!(
                "2027-01 1 400 1024 1756 62872 0 0 62872 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0{zeros}\n2026-11 0 0 0 0 4500 0 60 0 4500 0 0 0 0 0 0 0 0 0 0 0 0 0 0{zeros}\n2026-10 2 852 2048 878 245751 1 187 46984 14025 1 1200 2 31992 1 85000 1 6300 3 3000 5 50000 1 450 1 900 1 700 1 500 1 300 1 2000 1 400 1 750 1 900 1 350\n2026-09 1 426 1024 439 23492 0 0 23492 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0{zeros}\n"
            ),
            "the pictures' tokens and seconds, and behind them what each kind cost and how many there were of those that are counted by the line's end: a note of two pieces is one note with the seconds and the cost of both, sound written down for no note is seconds and cost alone, and a part of a turn, a request for the rundowns or one said to the browser or to the bell adds its cost and counts nothing"
        );
        assert_eq!(Ledger::parse(&ledger.to_text()), ledger, "the text gives the book back");
        let october = ledger.months[2];
        assert_eq!(
            october.paid,
            [
                Paid { count: 2, micros: 46_984 },
                Paid { count: 1, micros: 14_025 },
                Paid { count: 1, micros: 1_200 },
                Paid { count: 2, micros: 31_992 },
                Paid { count: 1, micros: 85_000 },
                Paid { count: 1, micros: 6_300 },
                Paid { count: 3, micros: 3_000 },
                Paid { count: 5, micros: 50_000 },
                Paid { count: 1, micros: 450 },
                Paid { count: 1, micros: 900 },
                Paid { count: 1, micros: 700 },
                Paid { count: 1, micros: 500 },
                Paid { count: 1, micros: 300 },
                Paid { count: 1, micros: 2_000 },
                Paid { count: 1, micros: 400 },
                Paid { count: 1, micros: 750 },
                Paid { count: 1, micros: 900 },
                Paid { count: 1, micros: 350 }
            ],
            "each of the eighteen kinds has its own count and its own cost"
        );
        assert_eq!(
            (october.seconds, untyped_micros(&october), october.micros),
            (187, 0, 245_751),
            "and the kinds add up to the month's cost"
        );
        assert_eq!(
            ledger.months.iter().map(Month::name).collect::<Vec<_>>(),
            ["2027-01", "2026-11", "2026-10", "2026-09"],
            "the months by their names, the newest first"
        );
    }

    #[test]
    fn failed_work() {
        let mut ledger = Ledger::default();
        let entries: [(Entry, Kind); 5] = [
            (Entry::summary(SummaryKind::Note), Kind::NoteSummary),
            (Entry::summary(SummaryKind::Talk), Kind::TalkSummary),
            (Entry::one(Kind::Rundown), Kind::Rundown),
            (Entry::work(Kind::Chat), Kind::Chat),
            (Entry::one(Kind::Auto), Kind::Auto),
        ];
        for (entry, _) in entries {
            assert!(!ledger.add_failed_work(2026, 10, entry, (tokens::Usage::default(), 0)));
        }
        assert!(!ledger.add_failed_picture(2026, 10, Usage::default()) && !ledger.add_session(2026, 10, 0));
        assert_eq!(ledger, Ledger::default(), "nothing counted, nothing booked");
        let usage = tokens::Usage { input: 1000, output: 100, ..tokens::Usage::default() };
        for (entry, _) in entries {
            assert!(ledger.add_failed_work(2026, 10, entry, (usage, 0)));
        }
        assert!(ledger.add_failed_picture(2026, 10, MEDIUM) && ledger.add_session(2026, 10, 85_000));
        let october = ledger.months[0];
        let paid = Paid { count: 1, micros: 150 };
        assert_eq!(
            entries.map(|(_, kind)| october.paid[kind as usize]),
            [paid, paid, paid, Paid { count: 0, micros: 150 }, paid],
            "work that came to nothing is booked as its kind is, at what the service counted"
        );
        assert_eq!(
            (october.paid[Kind::Picture as usize], october.paid[Kind::Session as usize]),
            (Paid { count: 1, micros: MEDIUM.micros() }, Paid { count: 1, micros: 85_000 })
        );
        assert!(ledger.add_failed_work(2026, 10, Entry::one(Kind::Web), (usage, 20_000)));
        assert!(ledger.add_failed_work(2026, 10, Entry::one(Kind::Web), (tokens::Usage::default(), 10_000)));
        assert_eq!(
            ledger.months[0].paid[Kind::Web as usize],
            Paid { count: 2, micros: 30_150 },
            "a step of the browser's that came to nothing is booked with its searches, and for them alone"
        );
    }

    #[test]
    fn sound_and_notes() {
        let mut ledger = Ledger::default();
        ledger.add_sound(2026, 10, 1200, 54_000);
        ledger.add_note(2026, 10);
        ledger.add_sound(2026, 10, 1200, 54_000);
        let october = ledger.months[0];
        assert_eq!(
            (october.paid[Kind::Note as usize], october.seconds, october.micros, untyped_micros(&october)),
            (Paid { count: 1, micros: 108_000 }, 2400, 108_000, 0),
            "a piece of a note whose transcript failed part way is paid for and counts no note"
        );
        ledger.add_note(2026, 10);
        assert_eq!(
            ledger.months[0].paid[Kind::Note as usize],
            Paid { count: 2, micros: 108_000 },
            "a transcript that is made and kept again is another note, and costs what its pieces cost when they are sent"
        );
        ledger.add(2026, 10, Entry::one(Kind::Dictation), 450);
        assert_eq!(
            (ledger.months[0].paid[Kind::Dictation as usize], ledger.months[0].seconds),
            (Paid { count: 1, micros: 450 }, 2400),
            "a message said for Signal is one of its own, and adds no seconds to the notes'"
        );
    }

    #[test]
    fn readings() {
        let mut ledger = Ledger::default();
        ledger.add_reading(2026, 10, Kind::NewsReading, 0);
        ledger.add_reading(2026, 10, Kind::NewsReading, 1_000);
        ledger.add_reading(2026, 10, Kind::Chat, 500);
        ledger.add_reading(2026, 10, Kind::SignalReading, 200);
        let paid = |kind: Kind| ledger.months[0].paid[kind as usize];
        assert_eq!(
            [Kind::NewsReading, Kind::Chat, Kind::SignalReading].map(paid),
            [Paid { count: 2, micros: 1_000 }, Paid { count: 0, micros: 500 }, Paid { count: 1, micros: 200 }],
            "a reply's voice counts no reading, nor a turn"
        );
    }

    #[test]
    fn forget_signal() {
        let mut ledger = Ledger::default();
        ledger.add(2026, 10, Entry::one(Kind::Dictation), 300);
        ledger.add(2026, 10, Entry::one(Kind::Auto), 2_000);
        ledger.add_reading(2026, 9, Kind::SignalReading, 400);
        ledger.add(2026, 9, Entry::one(Kind::Rundown), 15_583);
        let totals = ledger.months.iter().map(|month| month.micros).collect::<Vec<_>>();
        assert!(ledger.forget(&SIGNAL));
        assert!(
            ledger.months.iter().all(|month| SIGNAL.iter().all(|kind| month.paid[*kind as usize] == Paid::default())),
            "neither count nor cost of a kind forgotten stays"
        );
        assert_eq!(ledger.months.iter().map(|month| month.micros).collect::<Vec<_>>(), totals, "the money does");
        assert_eq!(
            (untyped_micros(&ledger.months[0]), ledger.months[1].paid[Kind::Rundown as usize]),
            (2_300, Paid { count: 1, micros: 15_583 }),
            "told by no kind, and the other kinds left alone"
        );
        assert_eq!(Ledger::parse(&ledger.to_text()), ledger, "and so it is kept");
        assert!(!ledger.forget(&SIGNAL), "forgotten once");
        assert!(
            !ledger.is_within(2026, 10, 2_300) && ledger.is_within(2026, 10, 2_301),
            "a month's spending counts what its forgotten kinds cost"
        );
    }

    #[test]
    fn month_within_limit() {
        let mut ledger = Ledger::default();
        ledger.add(2026, 10, Entry::one(Kind::Rundown), 24_999_999);
        assert!(ledger.is_within(2026, 10, 25_000_000), "below the limit");
        ledger.add(2026, 10, Entry::one(Kind::Auto), 1);
        assert!(!ledger.is_within(2026, 10, 25_000_000), "at it");
        assert!(ledger.is_within(2026, 11, 25_000_000) && ledger.is_within(2026, 9, 1), "another month spent nothing");
        assert_eq!((ledger.spent(2026, 10), ledger.spent(2026, 11)), (25_000_000, 0));
    }

    #[test]
    fn kind_order() {
        assert_eq!(
            Kind::ALL.map(|kind| kind as usize),
            core::array::from_fn(|at| at),
            "the order of the array that holds them"
        );
    }

    #[test]
    fn partial_and_old_lines() {
        let ones = |count| " 1".repeat(count);
        let invalid = [
            String::from("nonsense"),
            format!("2026-13{}", ones(FIELDS)),
            String::from("2026-10 2 852 2048"),
            format!("2026-10 x{}", ones(FIELDS - 1)),
        ];
        let old = OLD_FORM_WORDS.map(|words| format!("2026-10{}", ones(words - 1)));
        assert_eq!(
            Ledger::parse(&[invalid.join("\n"), old.join("\n")].join("\n")),
            Ledger::default(),
            "no other month, none without the pictures and the cost, and no line of an older form"
        );
        let paid = |line: &str| Ledger::parse(line).months[0].paid;
        let [_, _, summaries, rundowns, sessions, ..] =
            paid("2026-10 2 852 2048 878 185101 1 187 46984 14025 1 1200 x 31592 1");
        assert_eq!(
            (summaries, rundowns, sessions),
            (Paid { count: 1, micros: 1_200 }, Paid { count: 0, micros: 31_592 }, Paid { count: 1, micros: 0 }),
            "a field that is no number, or that is not there, counts for nothing, and the fields behind it are read all the same"
        );
        assert_eq!(
            Ledger::parse(&format!("2026-10{} 9 and more", ones(FIELDS))),
            Ledger::parse(&format!("2026-10{}", ones(FIELDS))),
            "words behind the last field are left alone"
        );
    }

    #[test]
    fn saturating_sums() {
        let maximum = format!("2026-10{}\n", " 18446744073709551615".repeat(FIELDS));
        let mut full = Ledger::parse(&[maximum, format!("2026-10{}\n", " 1".repeat(FIELDS))].concat());
        full.add_picture(2026, 10, Usage { text: 1, image: 1, out: 1 });
        full.add_sound(2026, 10, 1, 1);
        let october = full.months[0];
        let [pictures, ..] = october.paid;
        assert_eq!(
            (
                full.months.len(),
                october.micros,
                october.seconds,
                october.usage.text,
                pictures,
                untyped_micros(&october)
            ),
            (1, u64::MAX, u64::MAX, u64::MAX, Paid { count: u64::MAX, micros: u64::MAX }, 0),
            "a book whose counts are as large as a count holds adds up to the most there is, and never wraps round to little"
        );
    }
}

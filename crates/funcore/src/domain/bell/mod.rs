//! What the bell rings for: every alarm, reminder and timer that is set, and how a model sets and cancels one.

pub mod state;

use super::settings::Settings;
use crate::base::json::{number_at, text_at, unescape, value_at};
use crate::base::lines::{last_value, values};
use crate::base::text::to_one_line;
use crate::base::time::offset::{to_local, to_utc};
use crate::base::time::stamp::parse_iso_day;
use crate::base::time::{Moment, SECONDS_PER_MINUTE, next_of_day, parse_minute_of_day, to_duration_text};
use crate::hw::sound::playback::{MemoryFile, Playback};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

pub const FILE: &str = "bell";
pub(crate) const RING_S: u64 = 5 * SECONDS_PER_MINUTE;
const RING: &[u8] = include_bytes!("../../../assets/bell/ring.m4a");
const SET_NAME: &str = "set_reminder";
const CANCEL_NAME: &str = "cancel_reminder";
const SET_REMINDER: &str = r#"{"type":"function","name":"set_reminder","description":"Have the device ring: an alarm, a reminder or a timer, as many of each as the user wants. For a time give at as HH:MM of the device's clock, and on as YYYY-MM-DD for a day, or on null for the next time the clock shows it; for a timer give in_seconds, its length, with at and on null. text is what the ring is for, a few words the screen shows as it rings, or null. Every message of the user begins with the device's date and time: work a day like tomorrow or next Friday out from it. The answer names the moment that is set: tell it to the user, and where it is not what they meant, cancel it and set it anew.","strict":true,"parameters":{"type":"object","properties":{"on":{"type":["string","null"]},"at":{"type":["string","null"]},"in_seconds":{"type":["integer","null"]},"text":{"type":["string","null"]}},"required":["on","at","in_seconds","text"],"additionalProperties":false}}"#;
const CANCEL_REMINDER: &str = r#"{"type":"function","name":"cancel_reminder","description":"Cancel an alarm, a reminder or a timer that is set. which is text only its own text holds, its time as HH:MM or as YYYY-MM-DD HH:MM, the word alarm, reminder or timer where one alone is of that kind, or all for every one. The answer says what was cancelled, or lists what is set where which names none of them or more than one.","strict":true,"parameters":{"type":"object","properties":{"which":{"type":"string"}},"required":["which"],"additionalProperties":false}}"#;
// The declarations of the two functions a model changes the bell by.
pub(super) const FUNCTIONS: [&str; 2] = [SET_REMINDER, CANCEL_REMINDER];
const TASK: &str = include_str!("../../../assets/bell/request.txt");
const FUNCTIONS_MARK: &str = "{FUNCTIONS}";
const NOW_LABEL: &str = "The device's clock shows";
const REQUEST_LABEL: &str = "Request";
const KEY_ENTRY: &str = "entry";
const CLOCK_WORD: &str = "clock";
const SPAN_WORD: &str = "span";
// The settings' file of an earlier firmware kept one alarm and one timer, each in seconds of UTC.
const KEY_ALARM: &str = "alarm";
const KEY_TIMER: &str = "timer";
const ALL: &str = "all";
const NO_TIME: &str = "The device does not know the time yet.";
const NOTHING_SET: &str = "Nothing is set.";
const NO_MATCH: &str = "Nothing that is set matches that.";
/// The log line of a ring that wakes the device.
pub const RINGS_LINE: &str = "wake: the bell rings";

/// The log line of the ring's sound.
pub const fn to_ring_line(does_ring: bool) -> &'static str {
    if does_ring { "bell: rings" } else { "bell: silent" }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum When {
    // Seconds of the device's clock, rung when it shows them, whatever its offset from UTC is by then.
    Clock(u64),
    // Seconds of UTC, a span after the entry was set.
    Span(u64),
}

impl When {
    const fn at_s(self, offset: i16) -> u64 {
        match self {
            When::Clock(local) => to_utc(local, offset),
            When::Span(utc) => utc,
        }
    }

    const fn local_s(self, offset: i16) -> u64 {
        match self {
            When::Clock(local) => local,
            When::Span(utc) => to_local(utc, offset),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Alarm,
    Reminder,
    Timer,
}

impl Kind {
    const fn to_word(self) -> &'static str {
        match self {
            Kind::Alarm => "alarm",
            Kind::Reminder => "reminder",
            Kind::Timer => "timer",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    when: When,
    pub(crate) text: String,
}

// Its text is the owner's, which no log tells.
impl core::fmt::Debug for Entry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Entry").field("when", &self.when).finish_non_exhaustive()
    }
}

impl Entry {
    pub(crate) const fn kind(&self) -> Kind {
        match self.when {
            When::Span(_) => Kind::Timer,
            When::Clock(_) if self.text.is_empty() => Kind::Alarm,
            When::Clock(_) => Kind::Reminder,
        }
    }

    // The day and time it rings at on a clock `offset` minutes ahead of UTC, as `friday 20 november 2026, 09:00`.
    pub(crate) fn to_when_text(&self, offset: i16) -> String {
        to_when_text(&Moment::from_seconds(self.when.local_s(offset)))
    }

    fn is_named(&self, which: &str, offset: i16) -> bool {
        let (local, which) = (Moment::from_seconds(self.when.local_s(offset)), which.to_lowercase());
        which == self.kind().to_word()
            || which == local.to_time_text()
            || which == local.to_iso_minute()
            || !self.text.is_empty() && self.text.to_lowercase().contains(&which)
    }

    fn to_line(&self, utc: u64, offset: i16) -> String {
        let local = Moment::from_seconds(self.when.local_s(offset));
        let mut line = format!("- {}, {}", self.kind().to_word(), local.to_iso_minute());
        if let When::Span(at) = self.when {
            let _ = write!(line, ", {} left", to_duration_text(at.saturating_sub(utc)));
        }
        if !self.text.is_empty() {
            let _ = write!(line, ": {}", self.text);
        }
        line
    }
}

fn to_when_text(local: &Moment) -> String {
    format!("{}, {}", local.to_date_words(true, false), local.to_time_text())
}

/// When a ring is asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Due {
    // A time of day in minutes, on the day whose midnight on the device's clock is given, else the next time it comes.
    Clock { day: Option<u64>, minute: u16 },
    // Seconds from now.
    Span(u64),
}

/// A change a model asks of the bell.
#[derive(Clone, PartialEq, Eq)]
pub enum Change {
    Set { due: Due, text: String },
    // Text one entry alone holds, its time, its kind's word, or all.
    Cancel(String),
}

// A ring by a time of day, on a day or the next it comes, or by a length, never by both.
fn parse_ring(arguments: &str) -> Option<Change> {
    let field = |name: &str| text_at(arguments, &[name]).map(unescape);
    let nonblank = |name: &str| field(name).filter(|text| !text.trim().is_empty());
    let (on, at) = (nonblank("on"), nonblank("at"));
    let seconds: Option<u64> = number_at(arguments, &["in_seconds"]);
    let due = match (at, seconds) {
        (Some(at), None) => {
            let day = match on {
                Some(day) => Some(parse_iso_day(day.trim())?.to_seconds()),
                None => None,
            };
            Due::Clock { day, minute: parse_minute_of_day(&at)? }
        }
        (None, Some(seconds)) if on.is_none() && seconds > 0 => Due::Span(seconds),
        _ => return None,
    };
    Some(Change::Set { due, text: field("text").unwrap_or_default() })
}

// The change a call of one of the bell's functions asks; none for another function or arguments that are not its own.
pub(super) fn parse_call(name: &str, arguments: &str) -> Option<Change> {
    match name {
        SET_NAME => parse_ring(arguments),
        CANCEL_NAME => {
            let which = text_at(arguments, &["which"]).map(unescape)?;
            (!which.trim().is_empty()).then(|| Change::Cancel(which.trim().into()))
        }
        _ => None,
    }
}

/// The task of the model that turns a request said or typed to the bell into calls of its functions, a line each.
pub fn task() -> String {
    TASK.replace(FUNCTIONS_MARK, &FUNCTIONS.join("\n"))
}

// The changes a reply to that task asks, in its lines' order; a line that is no call of the bell's is passed over.
fn parse_reply(reply: &str) -> Vec<Change> {
    let to_change = |line: &str| {
        let (name, arguments) = line.trim().split_once(' ')?;
        value_at(arguments, &[]).filter(|value| value.starts_with('{'))?;
        parse_call(name, arguments)
    };
    reply.lines().filter_map(to_change).collect()
}

// What a ring is for and what names one are the owner's words, which no log tells.
impl core::fmt::Debug for Change {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Change::Set { due, .. } => f.debug_tuple("Set").field(due).finish(),
            Change::Cancel(_) => f.write_str("Cancel"),
        }
    }
}

/// Every ring that is set.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Bell {
    entries: Vec<Entry>,
}

fn parse_entry(line: &str) -> Option<Entry> {
    let (kind, rest) = line.split_once(' ')?;
    let (seconds, text) = rest.split_once(' ').unwrap_or((rest, ""));
    let seconds = seconds.parse().ok()?;
    let when = match kind {
        CLOCK_WORD => When::Clock(seconds),
        SPAN_WORD => When::Span(seconds),
        _ => return None,
    };
    Some(Entry { when, text: text.into() })
}

impl Bell {
    /// The rings a kept text holds; a line that does not read is passed over.
    pub fn parse(text: &str) -> Bell {
        Bell { entries: values(text, KEY_ENTRY).filter_map(parse_entry).collect() }
    }

    /// The text of the bell's file.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for entry in &self.entries {
            let (word, seconds) = match entry.when {
                When::Clock(local) => (CLOCK_WORD, local),
                When::Span(utc) => (SPAN_WORD, utc),
            };
            let _ = write!(text, "{KEY_ENTRY} {word} {seconds}");
            if !entry.text.is_empty() {
                let _ = write!(text, " {}", entry.text);
            }
            text.push('\n');
        }
        text
    }

    /// The bell a start reads: what its file keeps, else the alarm and the timer the settings' file of an earlier firmware keeps, which are then true to keep in the bell's own.
    pub fn read(kept: Option<&str>, settings: &str) -> (Bell, bool) {
        if let Some(text) = kept {
            return (Bell::parse(text), false);
        }
        let offset = Settings::parse(settings).offset;
        let seconds = |word: &str| word.trim().parse::<u64>().ok();
        let alarm = last_value(settings, KEY_ALARM, seconds).map(|utc| When::Clock(to_local(utc, offset)));
        let timer = last_value(settings, KEY_TIMER, seconds).map(When::Span);
        let entries: Vec<Entry> =
            [alarm, timer].into_iter().flatten().map(|when| Entry { when, text: String::new() }).collect();
        let is_moved = !entries.is_empty();
        (Bell { entries }, is_moved)
    }

    /// The log line of the bell read at a start.
    pub fn to_read_line(&self) -> String {
        format!("bell: {} set, as kept", self.entries.len())
    }

    /// The log line of the bell written.
    pub fn to_kept_line(&self) -> String {
        format!("bell: kept {} set", self.entries.len())
    }

    pub(crate) const fn len(&self) -> usize {
        self.entries.len()
    }

    // Takes the rings of another bell too, but one it holds already.
    pub(crate) fn merge(&mut self, other: Bell) {
        for entry in other.entries {
            if !self.holds(&entry) {
                self.entries.push(entry);
            }
        }
    }

    pub(crate) fn sorted(&self, offset: i16) -> Vec<Entry> {
        let mut entries = self.entries.clone();
        entries.sort_by_key(|entry| entry.when.at_s(offset));
        entries
    }

    pub(crate) fn next_s(&self, offset: i16) -> Option<u64> {
        self.entries.iter().map(|entry| entry.when.at_s(offset)).min()
    }

    // The rings due at `utc`, the earliest first, which are set no longer.
    pub(crate) fn take_due(&mut self, utc: u64, offset: i16) -> Vec<Entry> {
        let is_due = |entry: &Entry| entry.when.at_s(offset) <= utc;
        let (mut due, kept): (Vec<Entry>, Vec<Entry>) =
            core::mem::take(&mut self.entries).into_iter().partition(is_due);
        self.entries = kept;
        due.sort_by_key(|entry| entry.when.at_s(offset));
        due
    }

    // What the timer that runs out first has left.
    pub(crate) fn time_left_s(&self, utc: u64) -> Option<u64> {
        let spans = self.entries.iter().filter_map(|entry| match entry.when {
            When::Span(at) => Some(at.saturating_sub(utc)),
            When::Clock(_) => None,
        });
        spans.min()
    }

    pub(crate) fn has_clock(&self) -> bool {
        self.entries.iter().any(|entry| matches!(entry.when, When::Clock(_)))
    }

    pub(crate) fn holds(&self, entry: &Entry) -> bool {
        self.entries.contains(entry)
    }

    pub(crate) fn remove(&mut self, entry: &Entry) -> bool {
        let place = self.entries.iter().position(|kept| kept == entry);
        place.map(|at| self.entries.remove(at)).is_some()
    }

    fn to_listing(&self, utc: u64, offset: i16) -> String {
        if self.entries.is_empty() {
            return String::from(NOTHING_SET);
        }
        let lines: Vec<String> = self.sorted(offset).iter().map(|entry| entry.to_line(utc, offset)).collect();
        format!("Set, by the device's clock:\n{}", lines.join("\n"))
    }

    pub(crate) fn to_model_text(&self, utc: Option<u64>, offset: i16) -> String {
        utc.map_or_else(|| String::from(NO_TIME), |utc| self.to_listing(utc, offset))
    }

    // The input of the model that takes a request said or typed to the bell at `utc`: what the device's clock shows, what is set, and the request.
    fn to_input(&self, request: &str, utc: u64, offset: i16) -> String {
        let clock = to_when_text(&Moment::from_seconds(to_local(utc, offset)));
        format!("{NOW_LABEL} {clock}.\n{}\n{REQUEST_LABEL}: {}", self.to_listing(utc, offset), to_one_line(request))
    }

    fn set(&mut self, due: Due, text: &str, utc: u64, offset: i16) -> (String, bool) {
        let now = to_local(utc, offset);
        let second_of_day = |minute: u16| u64::from(minute) * SECONDS_PER_MINUTE;
        let when = match due {
            Due::Clock { day: None, minute } => When::Clock(next_of_day(now, second_of_day(minute))),
            Due::Clock { day: Some(midnight), minute } => When::Clock(midnight + second_of_day(minute)),
            Due::Span(seconds) => When::Span(utc.saturating_add(seconds)),
        };
        let entry = Entry { when, text: to_one_line(text) };
        let at = entry.to_when_text(offset);
        if when.at_s(offset) <= utc {
            let clock = to_when_text(&Moment::from_seconds(now));
            return (format!("Not set: {at} is past. The device's clock shows {clock}."), false);
        }
        if self.holds(&entry) {
            return (format!("Already set for {at}."), false);
        }
        let told = match when {
            When::Clock(_) => format!("Set for {at}, by the device's clock."),
            When::Span(_) => format!("Set: it rings in {}, at {at}.", to_duration_text(when.at_s(offset) - utc)),
        };
        self.entries.push(entry);
        (told, true)
    }

    fn cancel(&mut self, which: &str, utc: u64, offset: i16) -> (String, bool) {
        let which = which.trim();
        if which.eq_ignore_ascii_case(ALL) {
            let count = self.entries.len();
            self.entries.clear();
            return (format!("Cancelled all {count}."), count > 0);
        }
        let named: Vec<usize> =
            (0..self.entries.len()).filter(|at| self.entries[*at].is_named(which, offset)).collect();
        match named[..] {
            [at] => {
                let entry = self.entries.remove(at);
                (format!("Cancelled:\n{}", entry.to_line(utc, offset)), true)
            }
            [] => (format!("{NO_MATCH} {}", self.to_listing(utc, offset)), false),
            _ => {
                let listing = self.to_listing(utc, offset);
                (format!("{} match that: name one by its date and time. {listing}", named.len()), false)
            }
        }
    }

    // Makes a change a model asked at `utc`: the answer to the model, and whether anything changed.
    pub(crate) fn apply(&mut self, change: Change, utc: Option<u64>, offset: i16) -> (String, bool) {
        let Some(utc) = utc else {
            return (String::from(NO_TIME), false);
        };
        match change {
            Change::Set { due, text } => self.set(due, &text, utc, offset),
            Change::Cancel(which) => self.cancel(&which, utc, offset),
        }
    }
}

/// The sound a ring plays, from its first sample.
pub fn ring_sound() -> Option<Playback<MemoryFile<'static>>> {
    Playback::new(MemoryFile::parse(RING).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::time::stamp::parse_iso_day;
    use alloc::vec;

    // Friday 2026-10-16, 22:15 UTC.
    const FRIDAY: u64 = 1_792_188_900;

    fn clock(day: &str, minute: u64, text: &str) -> Entry {
        let midnight = parse_iso_day(day).expect("a day").to_seconds();
        Entry { when: When::Clock(midnight + minute * SECONDS_PER_MINUTE), text: text.into() }
    }

    fn span(utc: u64, text: &str) -> Entry {
        Entry { when: When::Span(utc), text: text.into() }
    }

    fn set(due: Due, text: &str) -> Change {
        Change::Set { due, text: text.into() }
    }

    #[test]
    fn file_round_trip() {
        let bell = Bell {
            entries: vec![
                clock("2026-11-20", 540, "dentist at ten"),
                span(FRIDAY + 300, ""),
                clock("2026-10-17", 450, ""),
                span(FRIDAY + 60, "tea"),
            ],
        };
        let text = bell.to_text();
        assert_eq!(
            text,
            "entry clock 1795165200 dentist at ten\nentry span 1792189200\nentry clock 1792222200\nentry span 1792188960 tea\n"
        );
        assert_eq!(Bell::parse(&text), bell);
        assert_eq!(Bell::default().to_text(), "");
        assert_eq!(
            Bell::parse("entry clock soon\nentry noon 5\nentry\nring 5\nentry span 7 \u{E4}h\r\nentry clock 9\n"),
            Bell { entries: vec![span(7, "\u{E4}h"), Entry { when: When::Clock(9), text: String::new() }] },
            "a line that does not read is passed over"
        );
        assert_eq!(
            (bell.to_read_line(), bell.to_kept_line()),
            ("bell: 4 set, as kept".into(), "bell: kept 4 set".into())
        );
    }

    #[test]
    fn earlier_settings_move() {
        let settings = "offset 120\nalarm 1792222200\ntimer 1792189200\n";
        let (bell, is_moved) = Bell::read(None, settings);
        assert!(is_moved, "what the settings' file kept is kept in the bell's own from now on");
        assert_eq!(
            bell.entries,
            [clock("2026-10-17", 570, ""), span(1_792_189_200, "")],
            "the alarm at the time the clock showed for it, 09:30 two hours ahead of UTC, the timer as it was"
        );
        assert_eq!(bell.next_s(120), Some(1_792_189_200));
        assert_eq!(
            Bell::read(None, "offset 120\ntimer soon\n"),
            (Bell::default(), false),
            "nothing kept, nothing moved"
        );
        assert_eq!(
            Bell::read(Some(""), settings),
            (Bell::default(), false),
            "the bell's own file counts once it is there, empty too"
        );
        assert_eq!(Bell::read(Some("entry span 5 tea\n"), settings).0.entries, [span(5, "tea")]);
    }

    #[test]
    fn soonest_rings_first() {
        let (alarm, timer, later) = (clock("2026-10-17", 450, ""), span(FRIDAY + 50, "tea"), span(FRIDAY + 90, ""));
        let mut bell = Bell { entries: vec![alarm.clone(), later.clone(), timer.clone()] };
        assert_eq!((bell.next_s(0), bell.time_left_s(FRIDAY)), (Some(FRIDAY + 50), Some(50)));
        assert!(bell.has_clock() && bell.take_due(FRIDAY + 49, 0).is_empty());
        assert_eq!(bell.take_due(FRIDAY + 90, 0), [timer, later], "what is due, the earliest first");
        assert_eq!((bell.len(), bell.time_left_s(FRIDAY)), (1, None), "and set no longer");
        let at = parse_iso_day("2026-10-17").unwrap().to_seconds() + 450 * 60;
        assert_eq!(
            [bell.next_s(0), bell.next_s(120), bell.next_s(-60)],
            [Some(at), Some(at - 7200), Some(at + 3600)],
            "an alarm rings when the device's clock shows its time, wherever that clock stands by then"
        );
        assert_eq!(bell.take_due(at - 7200, 120), [alarm]);
        assert!(bell.next_s(0).is_none() && !bell.has_clock() && bell.take_due(u64::MAX, 0).is_empty());
    }

    #[test]
    fn set_by_the_conversation() {
        let mut bell = Bell::default();
        let apply = |bell: &mut Bell, change| bell.apply(change, Some(FRIDAY), 120);
        assert_eq!(
            apply(&mut bell, set(Due::Clock { day: None, minute: 450 }, "")),
            ("Set for saturday 17 october 2026, 07:30, by the device's clock.".into(), true),
            "the next time the clock, two hours ahead and past midnight, shows it"
        );
        let friday = parse_iso_day("2026-11-20").unwrap().to_seconds();
        assert_eq!(
            apply(&mut bell, set(Due::Clock { day: Some(friday), minute: 540 }, " dentist\nat ten ")),
            ("Set for friday 20 november 2026, 09:00, by the device's clock.".into(), true)
        );
        assert_eq!(
            apply(&mut bell, set(Due::Span(300), "tea")),
            ("Set: it rings in 5:00, at saturday 17 october 2026, 00:20.".into(), true)
        );
        assert_eq!(
            bell.entries,
            [clock("2026-10-17", 450, ""), clock("2026-11-20", 540, "dentist at ten"), span(FRIDAY + 300, "tea")]
        );
        assert_eq!(
            [bell.entries[0].kind(), bell.entries[1].kind(), bell.entries[2].kind()],
            [Kind::Alarm, Kind::Reminder, Kind::Timer]
        );
        let today = parse_iso_day("2026-10-17").unwrap().to_seconds();
        assert_eq!(
            apply(&mut bell, set(Due::Clock { day: Some(today), minute: 10 }, "x")),
            (
                "Not set: saturday 17 october 2026, 00:10 is past. The device's clock shows saturday 17 october 2026, 00:15.".into(),
                false
            )
        );
        assert_eq!(
            apply(&mut bell, set(Due::Span(300), "tea")),
            ("Already set for saturday 17 october 2026, 00:20.".into(), false)
        );
        assert_eq!(bell.apply(set(Due::Span(5), ""), None, 0), (NO_TIME.into(), false));
        assert_eq!(bell.len(), 3, "what is refused sets nothing");
    }

    #[test]
    fn cancelled_by_name() {
        let entries = vec![
            clock("2026-10-17", 450, ""),
            clock("2026-11-20", 540, "Dentist at ten"),
            clock("2026-11-21", 540, "call Anna"),
            span(FRIDAY + 300, "tea"),
        ];
        let listing = "Set, by the device's clock:\n- timer, 2026-10-16 22:20, 5:00 left: tea\n- alarm, 2026-10-17 07:30\n- reminder, 2026-11-20 09:00: Dentist at ten\n- reminder, 2026-11-21 09:00: call Anna";
        let cancel = |which: &str| {
            let mut bell = Bell { entries: entries.clone() };
            let (told, is_changed) = bell.apply(Change::Cancel(which.into()), Some(FRIDAY), 0);
            (told, is_changed, bell.len())
        };
        assert_eq!(cancel("dentist"), ("Cancelled:\n- reminder, 2026-11-20 09:00: Dentist at ten".into(), true, 3));
        assert_eq!(cancel(" 07:30 "), ("Cancelled:\n- alarm, 2026-10-17 07:30".into(), true, 3));
        assert_eq!(cancel("2026-11-21 09:00").2, 3, "by its day and time");
        assert_eq!(cancel("Timer"), ("Cancelled:\n- timer, 2026-10-16 22:20, 5:00 left: tea".into(), true, 3));
        assert_eq!(cancel("ALARM").2, 3, "the one of its kind");
        assert_eq!(
            cancel("09:00"),
            (format!("2 match that: name one by its date and time. {listing}"), false, 4),
            "two ring at that time of day"
        );
        assert!(!cancel("reminder").1, "nor is one alone a reminder");
        assert_eq!(cancel("coffee"), (format!("{NO_MATCH} {listing}"), false, 4));
        assert_eq!(cancel("all"), ("Cancelled all 4.".into(), true, 0));
        let mut none = Bell::default();
        assert_eq!(none.apply(Change::Cancel("ALL".into()), Some(FRIDAY), 0), ("Cancelled all 0.".into(), false));
        assert_eq!(
            none.apply(Change::Cancel("tea".into()), Some(FRIDAY), 0),
            (format!("{NO_MATCH} {NOTHING_SET}"), false)
        );
        let bell = Bell { entries };
        assert_eq!(bell.to_model_text(Some(FRIDAY), 0), listing);
        assert_eq!(
            (bell.to_model_text(None, 0), none.to_model_text(Some(FRIDAY), 0)),
            (NO_TIME.into(), NOTHING_SET.into())
        );
    }

    #[test]
    fn calls_and_replies() {
        let set_call = |arguments| parse_call("set_reminder", arguments);
        assert_eq!(
            set_call(r#"{"on":null,"at":"7:30","in_seconds":null,"text":null}"#),
            Some(set(Due::Clock { day: None, minute: 450 }, "")),
            "an alarm at the next time the clock shows it"
        );
        let day = parse_iso_day("2026-11-20").map(|day| day.to_seconds());
        assert_eq!(
            set_call(r#"{"on":" 2026-11-20 ","at":"09:00","in_seconds":null,"text":"Dentist"}"#),
            Some(set(Due::Clock { day, minute: 540 }, "Dentist"))
        );
        assert_eq!(
            set_call(r#"{"on":null,"at":null,"in_seconds":300,"text":"tea"}"#),
            Some(set(Due::Span(300), "tea"))
        );
        for arguments in [
            r#"{"on":null,"at":null,"in_seconds":null,"text":"x"}"#,
            r#"{"on":null,"at":"07:30","in_seconds":300,"text":null}"#,
            r#"{"on":"2026-11-20","at":null,"in_seconds":300,"text":null}"#,
            r#"{"on":"2026-02-30","at":"07:30","in_seconds":null,"text":null}"#,
            r#"{"on":"tomorrow","at":"07:30","in_seconds":null,"text":null}"#,
            r#"{"on":null,"at":"25:00","in_seconds":null,"text":null}"#,
            r#"{"on":null,"at":null,"in_seconds":0,"text":null}"#,
        ] {
            assert_eq!(set_call(arguments), None, "{arguments}");
        }
        assert_eq!(parse_call("cancel_reminder", r#"{"which":" dentist "}"#), Some(Change::Cancel("dentist".into())));
        assert_eq!(parse_call("cancel_reminder", r#"{"which":" "}"#), None);
        assert_eq!(parse_call("use_device", r#"{"which":"all"}"#), None, "no function of the bell's");
        let task = task();
        assert!(
            task.contains("\"name\":\"set_reminder\"")
                && task.contains("\"name\":\"cancel_reminder\"")
                && !task.contains(FUNCTIONS_MARK),
            "the task holds the declarations the robot is given too"
        );
        let reply = "set_reminder {\"on\":null,\"at\":\"07:30\",\"in_seconds\":null,\"text\":\"gym\"}\n\n  cancel_reminder {\"which\":\"all\"}\nset_reminder at seven\nNOTHING\n";
        assert_eq!(
            parse_reply(reply),
            [set(Due::Clock { day: None, minute: 450 }, "gym"), Change::Cancel("all".into())],
            "a line a call, in their order; what is no call of the bell's is passed over"
        );
        assert!(parse_reply("NOTHING").is_empty());
    }

    #[test]
    fn kept_and_removed() {
        let (tea, alarm) = (span(FRIDAY + 300, "tea"), clock("2026-10-17", 450, ""));
        let mut bell = Bell { entries: vec![tea.clone()] };
        bell.merge(Bell { entries: vec![alarm.clone(), tea.clone()] });
        assert_eq!(bell.entries, [tea.clone(), alarm.clone()], "what another bell holds too is taken once");
        assert_eq!(bell.sorted(0), [tea.clone(), alarm.clone()]);
        assert!(bell.holds(&tea) && bell.remove(&tea) && !bell.remove(&tea) && bell.entries == [alarm.clone()]);
        assert_eq!(alarm.to_when_text(120), "saturday 17 october 2026, 07:30", "by the clock, wherever it stands");
        assert_eq!(tea.to_when_text(120), "saturday 17 october 2026, 00:20", "a timer where the clock stands now");
        assert_eq!(
            format!("{:?} {:?} {:?}", tea, set(Due::Span(5), "tea"), Change::Cancel("tea".into())),
            "Entry { when: Span(1792189200), .. } Set(Span(5)) Cancel",
            "no text of a ring is told where one is formatted"
        );
    }

    #[test]
    fn ring_decodes() {
        assert!(ring_sound().is_some());
        assert_eq!((to_ring_line(true), to_ring_line(false)), ("bell: rings", "bell: silent"));
    }
}

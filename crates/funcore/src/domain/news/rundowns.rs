//! The rundowns pending at the model.

use super::topics::{Topic, Topics};
use super::{Edition, STATE_DIR};
use crate::base::time::Moment;
use crate::base::time::stamp::is_compact_day;
use crate::domain::paper::names::{edition_folder, is_name, rundown_file};
use crate::openai::fault::Fault;
use crate::openai::summary::{PATIENCE_SECONDS, Poll, SLOW_POLL_SECONDS, Standing, Summary, poll_address};
use crate::storage::Files;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::time::Duration;

pub const FILE: &str = "pending";
/// Past most measured rundowns (63 to 107.6 s), short of the slowest (162.8 s); a late look costs the reader nothing.
pub const FIRST_LOOK_S: u64 = 120;
const FIELD_SEPARATOR: char = '\t';
const FIELD_COUNT: usize = 5;

/// The log line of a rundown the model works on.
pub fn to_asked_line(topic: &str) -> String {
    format!("news: the day's {topic} is on its way, first looked at {FIRST_LOOK_S} s from now")
}

/// The log line of a rundown that is not written, and why.
pub fn to_unwritten_line(topic: &str, fault: &Fault) -> String {
    format!("news: the day's {topic} is not written: {fault}")
}

/// The log line of a rundown that did not come in its time and is asked for anew.
pub fn to_anew_line(topic: &str, fault: &Fault) -> String {
    format!("news: the day's {topic} is asked for anew: {fault}")
}

// The log line of a rundown a look at `now_s` of UTC found on its way still, as the service says it stands.
fn to_standing_line(rundown: &Rundown, standing: Standing, now_s: u64) -> String {
    format!(
        "news: the day's {} is {} at the service {} s after it was asked for",
        rundown.topic,
        standing.to_word(),
        rundown.elapsed(now_s).as_secs()
    )
}

/// A rundown asked of the model and not answered yet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rundown {
    /// The rundown's name.
    pub topic: String,
    /// The service's name of the work.
    pub name: String,
    /// The request's time, in seconds of UTC.
    pub at_s: u64,
    /// Its day, `yyyymmdd`.
    pub day: String,
    /// The first line of the report of its stories, of their sources.
    pub sources: String,
    look_count: u32,
}

impl Rundown {
    pub fn new(topic: &str, name: String, at_s: u64, day: String, sources: &str) -> Rundown {
        let sources = sources.replace(['\t', '\n', '\r'], " ");
        Rundown { topic: topic.into(), name, at_s, day, sources, look_count: 0 }
    }

    /// The model's time so far, at `now_s` of UTC.
    pub fn elapsed(&self, now_s: u64) -> Duration {
        Duration::from_secs(now_s.saturating_sub(self.at_s))
    }

    fn first_look_s(&self) -> u64 {
        self.at_s.saturating_add(FIRST_LOOK_S)
    }

    fn next_look_s(&self) -> u64 {
        self.first_look_s().saturating_add(u64::from(self.look_count).saturating_mul(SLOW_POLL_SECONDS))
    }

    fn note_look(&mut self, now_s: u64) -> bool {
        let due_count = now_s.saturating_sub(self.first_look_s()) / SLOW_POLL_SECONDS + 1;
        self.look_count = (self.look_count.saturating_add(1)).max(u32::try_from(due_count).unwrap_or(u32::MAX));
        self.next_look_s() < self.at_s.saturating_add(PATIENCE_SECONDS + 1)
    }

    fn parse(line: &str) -> Option<Rundown> {
        let mut fields = line.splitn(FIELD_COUNT, FIELD_SEPARATOR);
        let topic = fields.next().filter(|word| is_name(word))?;
        let name = fields.next().filter(|name| poll_address(name).is_some())?;
        let at_s = fields.next()?.parse().ok()?;
        let day = fields.next().filter(|day| is_compact_day(day))?;
        Some(Rundown::new(topic, name.to_string(), at_s, day.to_string(), fields.next()?))
    }

    fn to_line(&self) -> String {
        let Rundown { topic, name, at_s, day, sources, .. } = self;
        let separator = FIELD_SEPARATOR;
        format!("{topic}{separator}{name}{separator}{at_s}{separator}{day}{separator}{sources}\n")
    }
}

/// The pending rundowns, as their file holds them.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Rundowns {
    /// In request order.
    pub entries: Vec<Rundown>,
}

impl Rundowns {
    /// Parses a file; a damaged line is left out.
    pub fn parse(text: &str) -> Rundowns {
        Rundowns { entries: text.lines().filter_map(Rundown::parse).collect() }
    }

    pub fn to_text(&self) -> String {
        self.entries.iter().map(Rundown::to_line).collect()
    }

    fn to_file(&self) -> Option<String> {
        (!self.entries.is_empty()).then(|| self.to_text())
    }

    /// Writes the file, or removes it where nothing is pending.
    pub fn save<F: Files>(&self, mut files: F) -> Result<(), F::Error> {
        match self.to_file() {
            Some(text) => files.put(STATE_DIR, FILE, text.as_bytes()),
            None => files.remove(STATE_DIR, FILE),
        }
    }

    /// The log line of a file's rundowns, none where it held none.
    pub fn to_resumed_line(&self) -> Option<String> {
        let count = self.entries.len();
        (count > 0).then(|| format!("news: {count} rundowns the model was asked for are on their way"))
    }

    /// Adds a rundown, replacing one of the same day and name.
    pub fn add(&mut self, rundown: Rundown) {
        self.entries.retain(|entry| entry.day != rundown.day || entry.topic != rundown.topic);
        self.entries.push(rundown);
    }

    fn has(&self, day: &str, topic: &str) -> bool {
        self.entries.iter().any(|entry| entry.day == day && entry.topic == topic)
    }

    /// The rundowns of `topics` that are made and that the edition of `local`'s day lacks and the model is not working on.
    pub fn to_missing<F: Files>(
        &self,
        mut files: F,
        local: u64,
        (edition, is_forced): (Edition, bool),
        topics: &Topics,
    ) -> Result<Vec<Topic>, F::Error> {
        self.missing(local, (edition, is_forced), topics, |day, topic| {
            files.has(&edition_folder(day), &rundown_file(day, topic))
        })
    }

    fn missing<E>(
        &self,
        local: u64,
        (edition, is_forced): (Edition, bool),
        topics: &Topics,
        mut has_file: impl FnMut(&str, &str) -> Result<bool, E>,
    ) -> Result<Vec<Topic>, E> {
        let moment = Moment::from_seconds(local);
        if !is_forced && !edition.hour.is_some_and(|hour| moment.hour >= hour) {
            return Ok(Vec::new());
        }
        let day = moment.to_compact_date();
        let mut missing = Vec::new();
        for topic in topics.made() {
            if !self.has(&day, &topic.name) && (is_forced || !has_file(&day, &topic.name)?) {
                missing.push(topic.clone());
            }
        }
        Ok(missing)
    }

    /// Takes what a look at `now_s` of UTC found of a pending rundown: one on its way still is kept among these.
    pub fn take_look(&mut self, mut entry: Rundown, poll: Poll, now_s: u64) -> Seen {
        match settle_look(&mut entry, poll, now_s) {
            (Poll::Done(summary), _) => Seen::Written(summary),
            (Poll::Failed(fault), true) => Seen::Overdue(fault),
            (Poll::Failed(fault), false) => Seen::Ended(fault),
            (Poll::Retry(fault), _) => {
                let line = format!("news: the day's {} could not be asked after: {fault}", entry.topic);
                self.add(entry);
                Seen::Waiting(line)
            }
            (Poll::Pending(standing), _) => {
                let line = to_standing_line(&entry, standing, now_s);
                self.add(entry);
                Seen::Waiting(line)
            }
        }
    }

    /// The day of the first pending rundown.
    pub fn first_day(&self) -> Option<&str> {
        self.entries.first().map(|entry| entry.day.as_str())
    }

    /// The time of the soonest look.
    pub fn next_look_s(&self) -> Option<u64> {
        self.entries.iter().map(Rundown::next_look_s).min()
    }

    /// True once a look is due: then all are looked at, since the network is held for it.
    pub fn is_look_due(&self, now_s: u64) -> bool {
        self.next_look_s().is_some_and(|look| look <= now_s)
    }
}

/// What a look found of a pending rundown.
#[derive(PartialEq, Debug)]
pub enum Seen {
    // The model wrote it: the device stores its text as the day's rundown.
    Written(Summary),
    // It is on its way still, as the log line tells.
    Waiting(String),
    // It ended with nothing, which counts in the look's result.
    Ended(Fault),
    // It did not come in its time: its work is cancelled, and it is asked for anew where it may be (`to_anew`).
    Overdue(Fault),
}

/// The rundown an overdue one is asked for anew as: the one of its name that the owner keeps and has made still; none where the service cannot be asked.
pub fn to_anew<'a>(topics: &'a Topics, overdue: &Rundown, is_reachable: bool) -> Option<&'a Topic> {
    topics.get_made(&overdue.topic).filter(|_| is_reachable)
}

// The result of a look that found `poll`, and whether the rundown is overdue: one still open at the last look within `PATIENCE_SECONDS` of its asking fails as overdue, to be cancelled.
fn settle_look(rundown: &mut Rundown, poll: Poll, now_s: u64) -> (Poll, bool) {
    let is_open = matches!(poll, Poll::Pending(_) | Poll::Retry(_));
    if is_open && !rundown.note_look(now_s) {
        return (Poll::Failed(Fault::Temporary(format!("it did not come in {PATIENCE_SECONDS} s"))), true);
    }
    (poll, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::time::SECONDS_PER_DAY;
    use crate::storage::tests::Disk;
    use alloc::vec;

    fn to_rundown(topic: &str, name: &str, at_s: u64) -> Rundown {
        Rundown::new(topic, name.into(), at_s, "20261014".into(), "Sources (21/21 feeds, 681 stories): a.com, b.org")
    }

    #[test]
    fn file_round_trip() {
        let mut rundowns = Rundowns::default();
        assert!(rundowns.entries.is_empty() && rundowns.to_text().is_empty() && rundowns.next_look_s().is_none());
        assert_eq!(rundowns.to_file(), None);
        rundowns.add(to_rundown("finance", "resp_0a1B-2c_3", 1_792_000_000));
        rundowns.add(to_rundown("news", "resp_9", 1_792_000_001));
        assert_eq!(
            rundowns.to_text(),
            "finance\tresp_0a1B-2c_3\t1792000000\t20261014\tSources (21/21 feeds, 681 stories): a.com, b.org\nnews\tresp_9\t1792000001\t20261014\tSources (21/21 feeds, 681 stories): a.com, b.org\n"
        );
        assert_eq!(Rundowns::parse(&rundowns.to_text()), rundowns);
        assert_eq!(rundowns.first_day(), Some("20261014"));
        let mut copy = rundowns.clone();
        copy.add(to_rundown("news", "resp_10", 1_792_000_050));
        assert_eq!(
            copy.entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
            ["resp_0a1B-2c_3", "resp_10"]
        );
        let plain = Rundown::new("news", "resp_1".into(), 5, "20261014".into(), "a\tb\nc\r");
        assert_eq!(plain.sources, "a b c ");
    }

    #[test]
    fn save_and_lines() {
        let mut rundowns = Rundowns::default();
        let mut disk = Disk::with_files(&[(STATE_DIR, FILE)]);
        assert_eq!(rundowns.save(&mut disk), Ok(()));
        assert!(disk.names().is_empty(), "nothing on its way, no file");
        rundowns.add(to_rundown("news", "resp_1", 1000));
        assert_eq!(rundowns.save(&mut disk), Ok(()));
        assert_eq!(disk.files[0].2, rundowns.to_text().as_bytes());
        assert_eq!(Rundowns::default().to_resumed_line(), None);
        assert_eq!(
            rundowns.to_resumed_line().as_deref(),
            Some("news: 1 rundowns the model was asked for are on their way")
        );
        assert_eq!(to_asked_line("finance"), "news: the day's finance is on its way, first looked at 120 s from now");
        assert_eq!(
            [Standing::Queued, Standing::InProgress].map(|standing| to_standing_line(
                &rundowns.entries[0],
                standing,
                1150
            )),
            [
                "news: the day's news is queued at the service 150 s after it was asked for",
                "news: the day's news is in_progress at the service 150 s after it was asked for"
            ]
        );
    }

    #[test]
    fn missing_from_folder() {
        let at = Moment { year: 2026, month: 10, day: 14, weekday: 0, hour: 9, minute: 0, second: 0 }.to_seconds();
        let mut disk = Disk::with_files(&[("news/20261014", "20261014_finance.md")]);
        let missing = Rundowns::default().to_missing(&mut disk, at, (Edition::default(), false), &Topics::default());
        assert_eq!(missing, Ok(vec![Topic::named("news", true)]));
    }

    #[test]
    fn damaged_lines() {
        let text = [
            "news\tresp_1\t100\t20261014\tSources",
            "Weather\tresp_2\t100\t20261014\tSources",
            "news\tresp/../x\t100\t20261014\tSources",
            "news\tresp_3\tsoon\t20261014\tSources",
            "news\tresp_4\t100\t../../x\tSources",
            "news\tresp_5\t100\t2026101\tSources",
            "news\tresp_6\t100",
            "",
            "finance\tresp_7\t200\t20261014\t",
            "weather\tresp_8\t300\t20261014\tSources\twith\ttabs",
        ]
        .join("\n");
        let parsed = Rundowns::parse(&text);
        assert_eq!(
            parsed.entries.iter().map(|entry| entry.name.clone()).collect::<Vec<_>>(),
            ["resp_1", "resp_7", "resp_8"]
        );
        assert_eq!(
            (parsed.entries[2].topic.as_str(), parsed.entries[2].sources.as_str()),
            ("weather", "Sources with tabs"),
            "a rundown of any name that names one"
        );
        let mut last = Rundowns::parse("news\tresp_9\t18446744073709551615\t20261014\tSources").entries.remove(0);
        assert_eq!(last.next_look_s(), u64::MAX);
        assert!(!last.note_look(1_792_000_000));
    }

    #[test]
    fn look_schedule() {
        let mut rundown = to_rundown("news", "resp_1", 1000);
        let mut looks = vec![rundown.next_look_s()];
        while rundown.note_look(rundown.next_look_s()) {
            looks.push(rundown.next_look_s());
        }
        assert_eq!(looks, [1120, 1150, 1180, 1210, 1240, 1270, 1300], "the last at five minutes");
        assert_eq!(rundown.next_look_s(), 1330);
        let mut late = to_rundown("news", "resp_1", 1000);
        assert!(!late.note_look(1000 + 4000) && late.next_look_s() > 1000 + 4000);
        let mut early = to_rundown("news", "resp_2", 1001);
        assert!(early.note_look(1120) && early.next_look_s() == 1151);
        let mut slow = to_rundown("news", "resp_3", 1000);
        assert!(slow.note_look(1125) && slow.next_look_s() == 1150);
    }

    #[test]
    fn soonest_look() {
        let mut rundowns = Rundowns::default();
        assert!(!rundowns.is_look_due(u64::MAX));
        rundowns.add(to_rundown("finance", "resp_1", 1000));
        rundowns.add(to_rundown("news", "resp_2", 1001));
        assert_eq!(rundowns.next_look_s(), Some(1120));
        assert!(!rundowns.is_look_due(1119) && rundowns.is_look_due(1120) && rundowns.is_look_due(5000));
        let mut copy = rundowns.clone();
        copy.entries[0].note_look(1120);
        assert_eq!(copy.next_look_s(), Some(1121));
        assert_eq!(Rundowns::parse(&copy.to_text()).next_look_s(), Some(1120));
        assert_eq!(
            (rundowns.entries[0].elapsed(1163), rundowns.entries[0].elapsed(5)),
            (Duration::from_secs(163), Duration::ZERO)
        );
    }

    #[test]
    fn missing_rundowns() {
        let at = |hour: u8, minute: u8| {
            Moment { year: 2026, month: 10, day: 14, weekday: 0, hour, minute, second: 0 }.to_seconds()
        };
        let (mut rundowns, topics) = (Rundowns::default(), Topics::default());
        let (news, finance) = (Topic::named("news", true), Topic::named("finance", true));
        let mut reads = Vec::new();
        let mut has_file = |day: &str, topic: &str| {
            reads.push((String::from(day), String::from(topic)));
            Ok::<bool, ()>(topic == "finance")
        };
        let (unasked, forced) = ((Edition::default(), false), (Edition::default(), true));
        assert_eq!(rundowns.missing(at(7, 59), unasked, &topics, &mut has_file), Ok(Vec::new()));
        assert_eq!(rundowns.missing(at(8, 0), unasked, &topics, &mut has_file), Ok(vec![news.clone()]));
        assert_eq!(rundowns.missing(at(7, 59), forced, &topics, &mut has_file), Ok(vec![news, finance.clone()]));
        rundowns.add(to_rundown("news", "resp_1", 1000));
        assert_eq!(
            [
                rundowns.missing(at(23, 59), unasked, &topics, &mut has_file),
                rundowns.missing(at(23, 59), forced, &topics, &mut has_file)
            ],
            [Ok(Vec::new()), Ok(vec![finance.clone()])]
        );
        let read = |topic: &str| (String::from("20261014"), String::from(topic));
        assert_eq!(reads, [read("news"), read("finance"), read("finance")]);
        assert_eq!(
            rundowns.missing(at(23, 59) - SECONDS_PER_DAY, forced, &topics, |_, _| Ok::<bool, ()>(true)),
            Ok(vec![Topic::named("news", true), finance.clone()]),
            "a rundown on its way for another day is another day's"
        );
        assert_eq!(
            rundowns.missing(at(23, 59) - SECONDS_PER_DAY, unasked, &topics, |_, _| Err("unreadable")),
            Err("unreadable")
        );
        assert_eq!(rundowns.missing(at(23, 59), forced, &topics, |_, _| Err("unreadable")), Ok(vec![finance.clone()]));
        let (evening, kept) = (Edition { hour: Some(20) }, Topics::parse("rundown news off\nrundown finance on\n"));
        let none_kept = |_: &str, _: &str| Ok::<bool, ()>(false);
        assert_eq!(
            [
                Rundowns::default().missing(at(19, 59), (evening, false), &kept, none_kept),
                Rundowns::default().missing(at(20, 0), (evening, false), &kept, none_kept),
                Rundowns::default().missing(at(7, 0), (evening, true), &kept, none_kept),
                Rundowns::default().missing(at(23, 0), (Edition { hour: None }, false), &kept, none_kept),
            ],
            [Ok(Vec::new()), Ok(vec![finance.clone()]), Ok(vec![finance]), Ok(Vec::new())],
            "at the hour set, the rundowns switched on; forced, those whatever the hour; none with no daily edition"
        );
        let own = Topics::parse("rundown tech on\nfeed tech https://a.example/rss\n");
        assert_eq!(
            Rundowns::default().missing(at(8, 0), unasked, &own, none_kept).map(|missing| missing[0].feeds.len()),
            Ok(1),
            "a rundown the owner keeps, with its feeds"
        );
    }

    #[test]
    fn look_outcomes() {
        let summary = Summary { text: "A rundown.".into(), usage: Default::default() };
        let (mut remaining, entry) = (Rundowns::default(), to_rundown("news", "resp_1", 1000));
        assert_eq!(remaining.take_look(entry.clone(), Poll::Done(summary.clone()), 1120), Seen::Written(summary));
        let refused = || Fault::Refused("gone".into());
        assert_eq!(remaining.take_look(entry.clone(), Poll::Failed(refused()), 1120), Seen::Ended(refused()));
        assert!(remaining.entries.is_empty(), "what is written or ended is on its way no more");
        assert_eq!(
            remaining.take_look(entry.clone(), Poll::Pending(Standing::Queued), 1120),
            Seen::Waiting("news: the day's news is queued at the service 120 s after it was asked for".into())
        );
        assert_eq!(remaining.next_look_s(), Some(1150), "kept, with its next look");
        let busy = to_rundown("finance", "resp_2", 1000);
        assert_eq!(
            remaining.take_look(busy, Poll::Retry(Fault::Temporary("status 429".into())), 1120),
            Seen::Waiting("news: the day's finance could not be asked after: status 429".into())
        );
        assert_eq!(remaining.entries.len(), 2);
        let late = Fault::Temporary("it did not come in 300 s".into());
        assert_eq!(
            remaining.take_look(entry.clone(), Poll::Pending(Standing::InProgress), 5000),
            Seen::Overdue(late.clone())
        );
        assert_eq!(remaining.entries.len(), 2, "an overdue one is kept no longer");
        let kept = Topics::parse("rundown news on\nrundown finance off\n");
        assert_eq!(to_anew(&kept, &entry, true).map(|topic| topic.name.as_str()), Some("news"));
        assert_eq!(to_anew(&kept, &entry, false), None, "nothing is asked anew where the service cannot be asked");
        let (off, gone) = (to_rundown("finance", "resp_3", 1000), to_rundown("tech", "resp_4", 1000));
        assert_eq!((to_anew(&kept, &off, true), to_anew(&kept, &gone, true)), (None, None), "nor one made no more");
        assert_eq!(
            [to_anew_line("news", &late), to_unwritten_line("news", &late)],
            [
                "news: the day's news is asked for anew: it did not come in 300 s",
                "news: the day's news is not written: it did not come in 300 s"
            ]
        );
    }

    #[test]
    fn settle_outcomes() {
        let summary = Summary { text: "A rundown.".into(), usage: Default::default() };
        let mut rundown = to_rundown("news", "resp_1", 1000);
        assert_eq!(settle_look(&mut rundown, Poll::Done(summary.clone()), 1120), (Poll::Done(summary), false));
        let failed = || Poll::Failed(Fault::Refused("gone".into()));
        assert_eq!(settle_look(&mut rundown, failed(), 1120), (failed(), false));
        assert_eq!(rundown.next_look_s(), 1120);
        let pending = || Poll::Pending(Standing::InProgress);
        assert!(settle_look(&mut rundown, pending(), 1120) == (pending(), false) && rundown.next_look_s() == 1150);
        let retry = || Poll::Retry(Fault::Temporary("status 429".into()));
        assert_eq!(settle_look(&mut rundown, retry(), 1150), (retry(), false));
        assert_eq!(rundown.next_look_s(), 1180);
        assert_eq!(settle_look(&mut rundown, pending(), 1270), (pending(), false));
        let overdue = || (Poll::Failed(Fault::Temporary("it did not come in 300 s".into())), true);
        assert_eq!(settle_look(&mut rundown, pending(), 1300), overdue(), "on its way still at the last look");
        let mut late = to_rundown("news", "resp_2", 1000);
        assert_eq!(settle_look(&mut late, retry(), 5000), overdue());
        assert_eq!(settle_look(&mut late, failed(), 5000), (failed(), false), "what a late look finds ended is ended");
    }
}

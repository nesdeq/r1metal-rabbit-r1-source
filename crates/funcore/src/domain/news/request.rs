//! A request said or typed for the rundowns: the model's task and input, what its reply asks, what of that stands once its feeds are read, and what the job's threads and the main loop share.

use super::silent::Silent;
use super::topics::{ICONS, Topic, Topics};
use crate::base::lines::values;
use crate::base::text::to_one_line;
use crate::domain::cause::Cause;
use crate::domain::paper::names::is_name;
use crate::domain::voice::{Changes, Request, Voice};
use crate::openai::fault::Fault;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The transcriber's prompt for a request said.
pub const PROMPT: &str = "A request about the rundowns of a daily news edition: a subject to add with its sources, its language and its length, or a rundown to change, switch off or delete.";
const TASK: &str = include_str!("../../../assets/news/request.txt");
const ICONS_MARK: &str = "{ICONS}";
const KEPT_LABEL: &str = "The rundowns the device keeps:";
const NONE_KEPT: &str = "The device keeps no rundown.";
const SILENT_LABEL: &str = "Feeds that gave no story when the device last read them:";
const REQUEST_LABEL: &str = "Request";
const KEY_DELETE: &str = "delete";

/// The task of the model that turns a request into the lines that change the rundowns, with the names of the icons a rundown may go by.
pub fn task() -> String {
    let names: Vec<&str> = ICONS.iter().map(|icon| icon.0).collect();
    TASK.replace(ICONS_MARK, &names.join(", "))
}

/// The log line of a request read: the count of the changes it asks, and of those that stand once the device has read the feeds they add.
pub fn to_read_line(asked_count: usize, standing_count: usize) -> String {
    format!(
        "news: a request for the rundowns asks {asked_count} changes, {standing_count} of them stand with their feeds read"
    )
}

/// The rundowns as the interface holds them, with the feeds that were silent: what a request is read against.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Standing(pub(crate) Topics, pub(crate) Silent);

/// A change a request asks of the rundowns.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Change {
    // A rundown as it is kept from then on, a new one or in the place of the one of its name.
    Put(Topic),
    Delete(String),
}

impl Standing {
    /// The model's input for a request: the rundowns with every feed each reads, those of them that were silent, and the request on one line.
    pub fn to_input(&self, request: &str) -> String {
        let Standing(topics, silent) = self;
        let kept = topics.to_told();
        let kept = if kept.is_empty() { String::from(NONE_KEPT) } else { format!("{KEPT_LABEL}\n{}", kept.trim_end()) };
        let silent = silent.to_told(topics);
        let silent = if silent.is_empty() { silent } else { format!("\n{SILENT_LABEL}\n{}", silent.trim_end()) };
        format!("{kept}{silent}\n{REQUEST_LABEL}: {}", to_one_line(request))
    }

    // The changes a reply asks of the rundowns: each rundown it names as it is kept from then on, what the reply leaves out as it is, and each one it deletes; what changes nothing is passed over.
    fn parse_reply(&self, reply: &str) -> Vec<Change> {
        let kept = &self.0;
        let deleted: Vec<&str> =
            values(reply, KEY_DELETE).map(str::trim).filter(|name| is_name(name) && kept.get(name).is_some()).collect();
        let mut changes = Vec::new();
        for mut topic in Topics::parse(reply).into_all() {
            if deleted.contains(&topic.name.as_str()) {
                continue;
            }
            if let Some(kept) = kept.get(&topic.name) {
                if topic.about.is_empty() {
                    topic.about.clone_from(&kept.about);
                }
                if topic.icon.is_empty() {
                    topic.icon = kept.icon;
                }
                if topic.feeds.is_empty() || topic.feed_lines() == kept.feed_lines() {
                    topic.feeds.clone_from(&kept.feeds);
                }
                if topic == *kept {
                    continue;
                }
            }
            changes.push(Change::Put(topic));
        }
        changes.extend(deleted.into_iter().map(|name| Change::Delete(name.into())));
        changes
    }

    // The feeds a rundown as a change keeps it adds to what the one of its name reads: what the device reads before the change stands.
    fn added_feeds(&self, topic: &Topic) -> Vec<String> {
        let known = self.0.get(&topic.name).map(Topic::feed_lines).unwrap_or_default();
        topic.feeds.iter().filter(|line| !known.contains(&line.as_str())).cloned().collect()
    }

    // The change that keeps a rundown, once the device has read the feeds it adds: `found` holds for each of `added` the line that gave stories, its own or a feed its page names, none where nothing did, and such a feed is left out; no feed is kept twice. None where nothing is left to keep.
    fn settle(&self, mut topic: Topic, added: &[String], found: &[Option<String>]) -> Option<Change> {
        let mut feeds = Vec::new();
        for line in core::mem::take(&mut topic.feeds) {
            let read = added.iter().position(|added| *added == line).map(|at| found.get(at).cloned().flatten());
            let line = read.unwrap_or(Some(line));
            if let Some(line) = line.filter(|line| !feeds.contains(line)) {
                feeds.push(line);
            }
        }
        topic.feeds = feeds;
        let kept = self.0.get(&topic.name);
        if let Some(kept) = kept.filter(|kept| kept.feed_lines() == topic.feed_lines()) {
            topic.feeds.clone_from(&kept.feeds);
        }
        let is_new = kept != Some(&topic) && !topic.feed_lines().is_empty();
        is_new.then_some(Change::Put(topic))
    }
}

/// What the device does next for a request the model has answered.
#[derive(PartialEq, Eq, Debug)]
pub enum Step<'a> {
    // Reads the feeds a rundown adds; `Reading::take_found` takes what each gave.
    Read(&'a [String]),
    // The count of the changes the reply asked, and those of them that stand.
    Done(usize, Vec<Change>),
}

/// A request from the model's reply on: each rundown the reply keeps has the feeds it adds read before it stands.
pub struct Reading {
    standing: Standing,
    asked_count: usize,
    asked: VecDeque<Change>,
    // The rundown whose added feeds are read, with them.
    read: Option<(Topic, Vec<String>)>,
    changes: Vec<Change>,
}

impl Reading {
    /// The reading of what a reply asks of the rundowns as they stand.
    pub fn new(standing: Standing, reply: &str) -> Reading {
        let asked: VecDeque<Change> = standing.parse_reply(reply).into();
        Reading { standing, asked_count: asked.len(), asked, read: None, changes: Vec::new() }
    }

    /// The next step: the feeds to read for the next rundown that adds any, in the order the changes were asked, or what the request came to.
    pub fn step(&mut self) -> Step<'_> {
        while self.read.is_none() {
            match self.asked.pop_front() {
                Some(Change::Put(topic)) => {
                    let added = self.standing.added_feeds(&topic);
                    if added.is_empty() {
                        self.changes.extend(self.standing.settle(topic, &added, &[]));
                    } else {
                        self.read = Some((topic, added));
                    }
                }
                Some(change @ Change::Delete(_)) => self.changes.push(change),
                None => return Step::Done(self.asked_count, core::mem::take(&mut self.changes)),
            }
        }
        Step::Read(self.read.as_ref().map_or(&[], |(_, added)| added.as_slice()))
    }

    /// Takes what the feeds of the last `Step::Read` gave: for each the line that gave stories, its own or a feed its page names, none where nothing did.
    pub fn take_found(&mut self, found: &[Option<String>]) {
        if let Some((topic, added)) = self.read.take() {
            self.changes.extend(self.standing.settle(topic, &added, found));
        }
    }
}

/// The job of a request said or typed for the rundowns: what it came to, of which the interface keeps each rundown and shows a deletion for the owner's click.
pub type Asker = Request<Standing, Change>;
/// The job's report for the main loop.
pub type Report = Changes<Change>;

type State = Voice<Asker>;

impl State {
    /// Takes what a request came to: the count of the changes its reply asked, or why none came, and those of them that stand with their feeds read. True if the screen has news.
    pub fn take_changes(&mut self, changes: Result<(usize, Vec<Change>), Fault>) -> bool {
        match changes {
            Ok((0, changes)) => self.keep_changes(changes, Cause::NoAnswer),
            Ok((_, changes)) => self.keep_changes(changes, Cause::NoFeed),
            Err(fault) => self.fail(&fault),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::voice::Phase;
    use alloc::vec;

    const KEPT: &str = "rundown news on\nrundown tech on\nabout tech chips and compilers, short\nfeed tech https://a.example/rss\nfeed tech https://b.example/feed\n";

    fn standing() -> Standing {
        Standing(Topics::parse(KEPT), Silent::parse("feed tech https://b.example/feed\n"))
    }

    fn tech() -> Topic {
        standing().0.get("tech").unwrap().clone()
    }

    #[test]
    fn input_tells_every_feed() {
        let input = standing().to_input(" add\nformula one ");
        assert!(
            input.starts_with(
                "The rundowns the device keeps:\nrundown news on\nfeed news venturebeat.com https://news.google.com/rss/search?q=site:"
            ),
            "the firmware's feeds of a rundown that keeps none of its own: {input}"
        );
        assert!(
            input.ends_with(
                "feed tech https://b.example/feed\nFeeds that gave no story when the device last read them:\nfeed tech https://b.example/feed\nRequest: add formula one"
            ),
            "{input}"
        );
        assert_eq!(input.matches("\nfeed news ").count(), 21);
        let stale = Silent::parse("feed tech https://gone.example/feed\nfeed f1 https://f1.example/rss\n");
        assert!(
            !Standing(Topics::parse(KEPT), stale).to_input("x").contains(SILENT_LABEL),
            "a feed its rundown reads no more, and one of a rundown kept no more, are told to no model"
        );
        assert_eq!(
            Standing(Topics::parse(""), Silent::default()).to_input("news"),
            "The device keeps no rundown.\nRequest: news"
        );
        let task = task();
        assert!(task.contains("\nfeed NAME ADDRESS\n") && task.contains("reply NOTHING."));
        assert!(
            task.contains("best: newspaper, finance, public, ")
                && task.contains(", celebration, podcasts.")
                && !task.contains(ICONS_MARK),
            "the icons a rundown may go by, by their names: {task}"
        );
        assert_eq!(
            to_read_line(2, 1),
            "news: a request for the rundowns asks 2 changes, 1 of them stand with their feeds read"
        );
    }

    #[test]
    fn reply_becomes_changes() {
        let standing = standing();
        let sport = Topic {
            about: "Formula 1, in German".into(),
            icon: "sports_motorsports",
            feeds: vec!["https://f1.example/rss".into(), "https://motor.example/f1".into()],
            ..Topic::named("f1", true)
        };
        let reply = "rundown f1 on\nabout f1 Formula 1, in German\nicon f1 sports_motorsports\nfeed f1 https://f1.example/rss\nfeed f1 https://motor.example/f1\n";
        assert_eq!(standing.parse_reply(reply), [Change::Put(sport.clone())], "a new rundown as it is said");
        assert_eq!(
            Standing(Topics::parse(reply), Silent::default()).parse_reply("rundown f1 off\n"),
            [Change::Put(Topic { is_on: false, ..sport })],
            "the icon a reply leaves out stays too"
        );
        assert_eq!(
            standing.parse_reply("rundown tech on\nicon tech memory\n"),
            [Change::Put(Topic { icon: "memory", ..tech() })],
            "an icon alone is a change"
        );
        assert_eq!(
            standing.parse_reply("rundown tech off\n"),
            [Change::Put(Topic { is_on: false, ..tech() })],
            "what a reply leaves out of a rundown the device keeps stays as it is"
        );
        assert_eq!(
            standing.parse_reply("rundown tech on\nabout tech only chips\n"),
            [Change::Put(Topic { about: "only chips".into(), ..tech() })]
        );
        let fewer = Topic { feeds: vec!["https://a.example/rss".into()], ..tech() };
        assert_eq!(
            standing.parse_reply("rundown tech on\nfeed tech https://a.example/rss\n"),
            [Change::Put(fewer)],
            "its feed lines are every feed it reads from then on"
        );
        assert_eq!(
            standing.parse_reply("delete tech\nrundown tech off\ndelete weather\ndelete Tech\n"),
            [Change::Delete("tech".into())],
            "a rundown the device keeps, and no change to what is deleted"
        );
        for reply in ["NOTHING", "rundown tech on\n", "about nobody x\n", "rundown news on\n", ""] {
            assert!(standing.parse_reply(reply).is_empty(), "{reply:?} changes nothing");
        }
        let news_lines: String = standing
            .0
            .get("news")
            .unwrap()
            .feed_lines()
            .iter()
            .fold(String::from("rundown news on\n"), |text, line| text + "feed news " + line + "\n");
        assert!(
            standing.parse_reply(&news_lines).is_empty(),
            "the firmware's feeds told back are the firmware's still"
        );
        let more = standing.parse_reply(&(news_lines + "feed news https://c.example/rss\n"));
        assert!(matches!(&more[..], [Change::Put(news)] if news.feeds.len() == 22), "and with one more its own");
    }

    #[test]
    fn changes_hold_what_was_read() {
        let standing = standing();
        let more = Topic {
            feeds: vec![
                "https://a.example/rss".into(),
                "https://c.example/".into(),
                "https://d.example/rss".into(),
                "https://e.example/rss".into(),
            ],
            ..tech()
        };
        let added = standing.added_feeds(&more);
        assert_eq!(added, ["https://c.example/", "https://d.example/rss", "https://e.example/rss"]);
        let found = [Some("https://c.example/feed.xml".into()), None, Some("https://a.example/rss".into())];
        let kept = Topic { feeds: vec!["https://a.example/rss".into(), "https://c.example/feed.xml".into()], ..tech() };
        assert_eq!(
            standing.settle(more.clone(), &added, &found),
            Some(Change::Put(kept)),
            "the feed a page named in the page's place, what gave no story left out, and no feed twice"
        );
        let same =
            Topic { feeds: tech().feeds.into_iter().chain(["https://d.example/rss".into()]).collect(), ..tech() };
        assert_eq!(standing.settle(same.clone(), &standing.added_feeds(&same), &[None]), None, "nothing left to keep");
        let new = Topic { feeds: vec!["https://f.example/rss".into()], ..Topic::named("f1", true) };
        assert_eq!(standing.settle(new.clone(), &standing.added_feeds(&new), &[None]), None, "a rundown with no feed");
        let named = Topic {
            feeds: vec!["https://c.example/".into(), "https://a.example/rss".into(), "https://b.example/feed".into()],
            ..tech()
        };
        assert_eq!(
            standing.settle(named.clone(), &standing.added_feeds(&named), &[Some("https://a.example/rss".into())]),
            None,
            "a page listed before the feed it names, which the rundown reads, adds nothing"
        );
        let twice =
            Topic { feeds: tech().feeds.into_iter().chain(["https://a.example/rss".into()]).collect(), ..tech() };
        assert_eq!(standing.settle(twice, &[], &[]), None, "nor does a feed a reply lists twice");
        let restored = Topic::named("finance", true);
        assert!(standing.added_feeds(&restored).is_empty());
        assert_eq!(
            standing.settle(restored.clone(), &[], &[]),
            Some(Change::Put(restored)),
            "one of the two a new device has reads the firmware's feeds"
        );
    }

    #[test]
    fn reading_steps() {
        let reply = "rundown f1 on\nfeed f1 https://f1.example/rss\nrundown tech off\ndelete news\n";
        let added = [String::from("https://f1.example/rss")];
        let (kept_off, deleted) = (Change::Put(Topic { is_on: false, ..tech() }), Change::Delete("news".into()));
        let mut reading = Reading::new(standing(), reply);
        assert_eq!(reading.step(), Step::Read(&added), "the feeds the first rundown adds are read first");
        assert_eq!(reading.step(), Step::Read(&added), "and asked for until what they gave is handed over");
        reading.take_found(&[None]);
        assert_eq!(
            reading.step(),
            Step::Done(3, vec![kept_off.clone(), deleted.clone()]),
            "a rundown left with no feed falls away; one that adds none and a deletion stand unread, in their order"
        );
        let mut reading = Reading::new(standing(), reply);
        assert_eq!(reading.step(), Step::Read(&added));
        reading.take_found(&[Some("https://f1.example/feed.xml".into())]);
        let found = Topic { feeds: vec!["https://f1.example/feed.xml".into()], ..Topic::named("f1", true) };
        assert_eq!(reading.step(), Step::Done(3, vec![Change::Put(found), kept_off, deleted]));
        assert_eq!(Reading::new(standing(), "NOTHING").step(), Step::Done(0, Vec::new()), "a reply that asks nothing");
    }

    #[test]
    fn outcome_reported_once() {
        let mut state = State::new(true);
        assert!(state.is_keyed(), "a request typed needs the key as one said does");
        state.set_standing(standing());
        assert_eq!(*state.standing(), standing());
        let change = Change::Delete("tech".into());
        assert!(state.take_changes(Ok((1, vec![change.clone()]))));
        let report = state.take_report();
        assert_eq!((report.phase, report.changes), (Phase::Idle, vec![change]));
        assert!(state.take_report().changes.is_empty(), "told once");
        assert!(state.take_changes(Ok((0, Vec::new()))));
        assert_eq!(state.take_report().phase, Phase::Failed(Cause::NoAnswer), "a request that asks nothing");
        assert!(state.take_changes(Ok((2, Vec::new()))));
        assert_eq!(state.take_report().phase, Phase::Failed(Cause::NoFeed), "one whose feeds all gave no story");
        assert!(state.take_changes(Err(Fault::Offline("asleep".into()))));
        assert_eq!(state.take_report().phase, Phase::Failed(Cause::NoNetwork));
    }
}

//! Fetching feeds: the request, why a feed gave no story, the rounds of redirects, and what the feeds a request adds came to.

use super::feed::Feed;
use super::stories::{Story, TopicStories};
use super::topics::{Topic, Topics};
use crate::codec::http::CLOSE;
use crate::codec::http::IDENTITY;
use crate::codec::http::Target;
use crate::codec::http::USER_AGENT;
use crate::net::fetch::MAX_HOPS;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Display};

const ACCEPT: &str =
    "application/rss+xml, application/atom+xml, application/rdf+xml, application/xml;q=0.9, text/xml;q=0.9, */*;q=0.8";
/// The headers of a feed request.
pub const HEADERS: [(&str, &str); 4] =
    [("User-Agent", USER_AGENT), ("Accept", ACCEPT), ("Accept-Encoding", IDENTITY), ("Connection", CLOSE)];

/// The reason a feed gave no story where every fetch gave an answer; why a fetch at one address gave no document is a `net::fetch::Fault`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FeedError {
    BadAddress,
    TooManyRedirects,
    // The document's bytes.
    NoStories(usize),
}

impl fmt::Display for FeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeedError::BadAddress => f.write_str("the feed's address is no URL"),
            FeedError::TooManyRedirects => f.write_str("the feed sends the asker on and on"),
            FeedError::NoStories(bytes) => write!(f, "a document of {bytes} bytes that tells no story"),
        }
    }
}

impl core::error::Error for FeedError {}

/// The result of a fetch at one address.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FetchOutcome {
    // The document's stories and length.
    Stories(Vec<Story>, usize),
    Redirect(Target),
    // The address of the feed a page names as its own, and where that is.
    Named(String, Target),
}

/// A feed's stories or error, its document's length, its redirects, and the address of the feed a page at its own address named, where that was read in its place.
#[derive(Debug)]
pub struct FeedResult<E> {
    pub stories: Result<Vec<Story>, E>,
    pub bytes: usize,
    pub hops: usize,
    named: Option<String>,
}

/// Feeds fetched in rounds: a feed that is sent elsewhere, by a redirect or by a page that names it, is fetched there in the next round.
pub struct Rounds<E> {
    results: Vec<Option<FeedResult<E>>>,
    named: Vec<Option<String>>,
    next: Vec<(usize, Target)>,
    round_count: usize,
}

impl<E> Rounds<E> {
    /// The rounds of those feeds; the first holds each feed whose address is a URL.
    pub fn new(feeds: &[Feed<'_>]) -> Rounds<E> {
        let next =
            feeds.iter().enumerate().filter_map(|(index, feed)| Some((index, Target::parse(feed.address)?))).collect();
        let (results, named) = feeds.iter().map(|_| (None, None)).unzip();
        Rounds { results, named, next, round_count: 0 }
    }

    /// The next round's feed indices and targets; none once all are fetched or `MAX_HOPS` are followed.
    pub fn next_round(&mut self) -> Option<Vec<(usize, Target)>> {
        if self.next.is_empty() || self.round_count > MAX_HOPS {
            return None;
        }
        self.round_count += 1;
        Some(core::mem::take(&mut self.next))
    }

    /// Adds the outcome of this round's fetch of the feed at `index`.
    pub fn add_outcome(&mut self, index: usize, outcome: Result<FetchOutcome, E>) {
        let hops = self.round_count.saturating_sub(1);
        let (stories, bytes) = match outcome {
            Ok(FetchOutcome::Redirect(target)) => {
                self.next.push((index, target));
                return;
            }
            Ok(FetchOutcome::Named(address, target)) => {
                self.named[index] = Some(address);
                self.next.push((index, target));
                return;
            }
            Ok(FetchOutcome::Stories(stories, bytes)) => (Ok(stories), bytes),
            Err(error) => (Err(error), 0),
        };
        self.results[index] = Some(FeedResult { stories, bytes, hops, named: self.named[index].take() });
    }

    /// Each feed's result, in order; `to_error` turns a `FeedError` into an `E`.
    pub fn finish(self, to_error: impl Fn(FeedError) -> E) -> Vec<FeedResult<E>> {
        let Rounds { mut results, next, .. } = self;
        for (index, _) in next {
            let stories = Err(to_error(FeedError::TooManyRedirects));
            results[index] = Some(FeedResult { stories, bytes: 0, hops: MAX_HOPS + 1, named: None });
        }
        let no_address =
            || FeedResult { stories: Err(to_error(FeedError::BadAddress)), bytes: 0, hops: 0, named: None };
        results.into_iter().map(|result| result.unwrap_or_else(no_address)).collect()
    }
}

/// What the reading of the feed lines a request adds came to, with the log's lines: for each line itself where it gave stories, the address of the feed a page at its address named where that did, none where neither did.
pub fn to_found<E: Display>(lines: &[String], results: Vec<FeedResult<E>>) -> (Vec<Option<String>>, Vec<String>) {
    let mut left_out = Vec::new();
    let to_found = |(line, result): (&String, FeedResult<E>)| {
        let to_line = |e: E| format!("news: a feed asked for is left out: {}: {e}", Feed::parse(line).source());
        let stories = result.stories.map_err(|e| left_out.push(to_line(e)));
        stories.ok().map(|_| result.named.unwrap_or_else(|| line.clone()))
    };
    let found = lines.iter().zip(results).map(to_found).collect();
    (found, left_out)
}

/// The most feeds read at once, each on a thread and a connection of its own: as many as the two rundowns of a new device read together; more are read in turns of as many.
pub fn most_at_once() -> usize {
    Topics::default().all().iter().map(|topic| topic.feed_lines().len()).sum()
}

/// The rundowns' feeds, each with its rundown's index.
pub fn to_feeds(topics: &[Topic]) -> Vec<(usize, Feed<'_>)> {
    let mut feeds = Vec::new();
    for (index, topic) in topics.iter().enumerate() {
        feeds.extend(topic.feeds().into_iter().map(|feed| (index, feed)));
    }
    feeds
}

/// Folds the results of `to_feeds` into each rundown's stories, in the rundowns' order, and the log lines, `ms` after the reading began.
pub fn to_topic_stories<E: Display>(
    topics: &[Topic],
    feeds: &[(usize, Feed<'_>)],
    results: Vec<FeedResult<E>>,
    ms: u128,
) -> (Vec<TopicStories>, Vec<String>) {
    let mut totals: Vec<TopicStories> = topics.iter().map(|_| TopicStories::default()).collect();
    let (mut max_bytes, mut max_hops, mut lines) = (0, 0, Vec::new());
    for ((index, feed), result) in feeds.iter().zip(results) {
        (max_bytes, max_hops) = (max_bytes.max(result.bytes), max_hops.max(result.hops));
        let stories = result.stories.map_err(|e| lines.push(format!("news: {}: {e}", feed.source())));
        totals[*index].add(feed, stories.ok());
    }
    let to_line = |(topic, total): (&Topic, &TopicStories)| {
        let (answered, feeds, name) = (total.answered_count, total.feed_count, &topic.name);
        format!("news: {answered} of {feeds} feeds of {name} answered, with {} stories", total.stories.len())
    };
    lines.extend(topics.iter().zip(&totals).map(to_line));
    lines.push(format!(
        "news: the feeds read in {ms} ms, the largest document {max_bytes} bytes, the asker sent elsewhere {max_hops} times at the most"
    ));
    (totals, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_reasons() {
        assert_eq!(
            HEADERS.map(|(name, _)| name),
            ["User-Agent", "Accept", "Accept-Encoding", "Connection"],
            "under the firmware's own name, for a feed, not packed, and closed behind"
        );
        assert_eq!((HEADERS[0].1, HEADERS[2].1, HEADERS[3].1), (USER_AGENT, "identity", "close"));
        assert!(HEADERS[1].1.starts_with("application/rss+xml, application/atom+xml, "));
        assert_eq!(
            [FeedError::BadAddress, FeedError::TooManyRedirects, FeedError::NoStories(7)]
                .map(|error| alloc::string::ToString::to_string(&error)),
            [
                "the feed's address is no URL",
                "the feed sends the asker on and on",
                "a document of 7 bytes that tells no story"
            ]
        );
    }

    #[test]
    fn stories_by_rundown() {
        let own = Topic { feeds: alloc::vec!["https://c.org/rss".into()], ..Topic::named("tech", true) };
        let topics = [Topic::named("finance", true), Topic::named("news", true), own];
        let listing = to_feeds(&topics);
        assert_eq!(
            [0, 1, 2].map(|place| listing.iter().filter(|(index, _)| *index == place).count()),
            [26, 21, 1],
            "each feed with its rundown's place: the firmware's lists and a rundown's own"
        );
        assert_eq!(most_at_once(), 47, "what a new device reads at once every day bounds every reading");
        let pair = [(0, Feed::parse("a.org https://a.org/")), (1, Feed::parse("b.org https://b.org/"))];
        let results = alloc::vec![
            FeedResult { stories: Ok(alloc::vec![to_story("One")]), bytes: 120, hops: 1, named: None },
            FeedResult { stories: Err("gone"), bytes: 0, hops: 3, named: None },
        ];
        let (totals, lines) = to_topic_stories(&topics[..2], &pair, results, 7);
        assert_eq!(totals.iter().map(|total| total.answered_count).collect::<Vec<_>>(), [1, 0]);
        assert!(totals[0].silent.is_empty());
        assert_eq!(totals[1].silent, ["b.org https://b.org/"], "the feed that gave none is told with its rundown");
        assert_eq!(
            lines,
            [
                "news: b.org: gone",
                "news: 1 of 1 feeds of finance answered, with 1 stories",
                "news: 0 of 1 feeds of news answered, with 0 stories",
                "news: the feeds read in 7 ms, the largest document 120 bytes, the asker sent elsewhere 3 times at the most"
            ]
        );
    }

    fn to_story(title: &str) -> Story {
        Story {
            title: title.into(),
            link: String::new(),
            published: String::new(),
            summary: String::new(),
            source: String::new(),
        }
    }

    type Row<'a> = (Result<Vec<&'a str>, &'a FeedError>, usize, usize);

    fn to_rows(results: &[FeedResult<FeedError>]) -> Vec<Row<'_>> {
        fn titles(stories: &[Story]) -> Vec<&str> {
            stories.iter().map(|story| story.title.as_str()).collect()
        }
        results.iter().map(|result| (result.stories.as_deref().map(titles), result.bytes, result.hops)).collect()
    }

    #[test]
    fn redirect_rounds() {
        let feeds =
            ["https://a.org/rss", "no address at all", "https://b.org/feed", "https://c.org/x"].map(Feed::parse);
        let mut rounds = Rounds::new(&feeds);
        let first = rounds.next_round().unwrap();
        assert_eq!(
            first.iter().map(|(index, target)| (*index, target.host.as_str())).collect::<Vec<_>>(),
            [(0, "a.org"), (2, "b.org"), (3, "c.org")],
            "every feed whose address is a URL, at once"
        );
        rounds.add_outcome(0, Ok(FetchOutcome::Stories(alloc::vec![to_story("One")], 120)));
        rounds.add_outcome(2, Ok(FetchOutcome::Redirect(Target::parse("https://b.org/feed/").unwrap())));
        rounds.add_outcome(3, Err(FeedError::NoStories(9)));
        let second = rounds.next_round().unwrap();
        assert_eq!(
            second.iter().map(|(index, target)| (*index, target.path.as_str())).collect::<Vec<_>>(),
            [(2, "/feed/")],
            "only the one sent elsewhere, and there"
        );
        rounds.add_outcome(2, Ok(FetchOutcome::Stories(alloc::vec![to_story("Two"), to_story("Three")], 300)));
        assert!(rounds.next_round().is_none(), "every feed is read");
        assert_eq!(
            to_rows(&rounds.finish(|error| error)),
            [
                (Ok(alloc::vec!["One"]), 120, 0),
                (Err(&FeedError::BadAddress), 0, 0),
                (Ok(alloc::vec!["Two", "Three"]), 300, 1),
                (Err(&FeedError::NoStories(9)), 0, 0)
            ],
            "each in its place, whatever became of the others"
        );
    }

    #[test]
    fn feed_a_page_named() {
        let lines =
            ["https://a.org/f1", "motor https://b.org/rss", "https://c.org/", "https://d.org/"].map(String::from);
        let feeds: Vec<Feed<'_>> = lines.iter().map(|line| Feed::parse(line)).collect();
        let mut rounds = Rounds::new(&feeds);
        assert_eq!(rounds.next_round().unwrap().len(), 4);
        let named = |address: &str| FetchOutcome::Named(address.into(), Target::parse(address).unwrap());
        rounds.add_outcome(0, Ok(named("https://a.org/feed.xml")));
        rounds.add_outcome(1, Ok(FetchOutcome::Stories(alloc::vec![to_story("One")], 120)));
        rounds.add_outcome(2, Err(FeedError::NoStories(9)));
        rounds.add_outcome(3, Ok(named("https://d.org/feed.xml")));
        let second = rounds.next_round().unwrap();
        assert_eq!(
            second.iter().map(|(index, target)| (*index, target.path.as_str())).collect::<Vec<_>>(),
            [(0, "/feed.xml"), (3, "/feed.xml")],
            "a feed a page named is read in the round after, as one a redirect sends on"
        );
        rounds.add_outcome(0, Ok(FetchOutcome::Stories(alloc::vec![to_story("Two")], 300)));
        rounds.add_outcome(3, Err(FeedError::NoStories(5)));
        let results = rounds.finish(|error| error);
        assert_eq!(
            results.iter().map(|result| (result.named.as_deref(), result.hops)).collect::<Vec<_>>(),
            [(Some("https://a.org/feed.xml"), 1), (None, 0), (None, 0), (Some("https://d.org/feed.xml"), 1)]
        );
        let (found, left_out) = to_found(&lines, results);
        assert_eq!(
            found,
            [Some("https://a.org/feed.xml".into()), Some(lines[1].clone()), None, None],
            "the feed a page named in the page's place, a feed that answers as its line has it, none for what gave no story"
        );
        assert_eq!(
            left_out,
            [
                "news: a feed asked for is left out: c.org: a document of 9 bytes that tells no story",
                "news: a feed asked for is left out: d.org: a document of 5 bytes that tells no story"
            ]
        );
    }

    #[test]
    fn redirect_limit() {
        let mut rounds = Rounds::new(&[Feed::parse("https://a.org/rss")]);
        let mut count = 0;
        while let Some(round) = rounds.next_round() {
            count += 1;
            for (index, target) in round {
                rounds.add_outcome(index, Ok(FetchOutcome::Redirect(target)));
            }
        }
        assert_eq!(count, MAX_HOPS + 1, "asked first and then where it was sent, MAX_HOPS times");
        assert_eq!(to_rows(&rounds.finish(|error| error)), [(Err(&FeedError::TooManyRedirects), 0, MAX_HOPS + 1)]);
        let mut empty = Rounds::<FeedError>::new(&[]);
        assert!(empty.next_round().is_none() && empty.finish(|error| error).is_empty(), "no feed, no round");
    }
}

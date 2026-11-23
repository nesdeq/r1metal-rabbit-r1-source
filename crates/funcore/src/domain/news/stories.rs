//! A rundown's stories and their report for the model.

use super::feed::Feed;
use crate::openai::fault::Fault;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

pub(super) const NO_TITLE: &str = "No title";
pub(super) const NO_DATE: &str = "No date";
pub(super) const NO_SUMMARY: &str = "No summary available";

/// A story of a feed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Story {
    pub title: String,
    pub(super) link: String,
    pub(super) published: String,
    pub(super) summary: String,
    pub(super) source: String,
}

/// A rundown's stories and feed counts.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TopicStories {
    pub stories: Vec<Story>,
    pub answered_count: usize,
    pub feed_count: usize,
    /// The list lines of the feeds that gave no story.
    pub silent: Vec<String>,
}

impl TopicStories {
    /// Adds a feed's stories, none where it gave none.
    pub fn add(&mut self, feed: &Feed<'_>, stories: Option<Vec<Story>>) {
        self.feed_count += 1;
        match stories {
            Some(stories) => {
                self.answered_count += 1;
                self.stories.extend(stories);
            }
            None => self.silent.push(feed.to_line()),
        }
    }

    /// The report of its stories, which the model's input carries behind the day; with no story, why no rundown is written.
    pub fn to_report(&self) -> Result<String, Fault> {
        if self.stories.is_empty() {
            return Err(Fault::Temporary(format!(
                "no story to write it from, {} of {} feeds answered",
                self.answered_count, self.feed_count
            )));
        }
        let mut names: Vec<&str> = self.stories.iter().map(|story| story.source.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let mut report = format!(
            "Sources ({}/{} feeds, {} stories): {}",
            self.answered_count,
            self.feed_count,
            self.stories.len(),
            names.join(", ")
        );
        for story in &self.stories {
            let _ = write!(
                report,
                "\n\nTitle: {}\nSource: {}\nLink: {}\nPublished: {}\nSummary: {}",
                story.title, story.source, story.link, story.published, story.summary
            );
        }
        Ok(report)
    }
}

/// A report's first line, of sources.
pub fn sources_line(report: &str) -> &str {
    report.lines().next().unwrap_or_default()
}

/// The model's input for a rundown: the day, written `yyyy-mm-dd`, and the report of its stories.
pub fn to_input(day: &str, report: &str) -> String {
    format!("Date: {day}\n{report}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn story(title: &str, source: &str) -> Story {
        Story {
            title: title.into(),
            link: format!("https://{source}/{title}"),
            published: "Tue, 13 Oct 2026 06:00:00 GMT".into(),
            summary: "What happened.".into(),
            source: source.into(),
        }
    }

    #[test]
    fn feed_counts() {
        let mut totals = TopicStories::default();
        let feed = Feed::parse("gone.example https://gone.example/rss");
        totals.add(&feed, Some(vec![story("a", "wired.com"), story("b", "wired.com")]));
        totals.add(&feed, None);
        totals.add(&feed, Some(vec![story("c", "aeon.co")]));
        assert_eq!(
            (totals.stories.iter().map(|story| story.title.as_str()).collect::<Vec<_>>(), totals.answered_count),
            (vec!["a", "b", "c"], 2)
        );
        assert_eq!(totals.feed_count, 3);
        assert_eq!(totals.silent, ["gone.example https://gone.example/rss"], "the feed that gave none, by its line");
    }

    #[test]
    fn report_text() {
        let stories = vec![story("b", "wired.com"), story("a", "RockPaperShotgun"), story("c", "wired.com")];
        let report =
            TopicStories { stories, answered_count: 2, feed_count: 3, silent: Vec::new() }.to_report().unwrap();
        assert_eq!(sources_line(&report), "Sources (2/3 feeds, 3 stories): RockPaperShotgun, wired.com");
        assert_eq!(
            report.split("\n\n").nth(2),
            Some(
                "Title: a\nSource: RockPaperShotgun\nLink: https://RockPaperShotgun/a\nPublished: Tue, 13 Oct 2026 06:00:00 GMT\nSummary: What happened."
            )
        );
        assert_eq!(report.split("\n\n").count(), 4);
        assert_eq!(
            TopicStories { feed_count: 21, ..TopicStories::default() }.to_report(),
            Err(Fault::Temporary("no story to write it from, 0 of 21 feeds answered".into())),
            "which counts as this time's failure, so the next edition tries again"
        );
        assert_eq!(sources_line(""), "");
        assert_eq!(
            to_input("2026-10-13", "Title: \"X\""),
            "Date: 2026-10-13\nTitle: \"X\"",
            "a rundown gives the model the day before the stories"
        );
    }
}

//! The feeds that gave no story when their rundown's stories were last gathered.

use super::topics::{KEY_FEED, Topic, Topics, split_named};
use crate::base::lines::values;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

pub const FILE: &str = "silent";

/// The feeds that were silent at the last gathering, each by its rundown's name and its list line.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Silent(Vec<(String, String)>);

impl Silent {
    /// The feeds a kept text holds; a line that does not read is passed over.
    pub fn parse(text: &str) -> Silent {
        let to_entry = |(name, line): (&str, &str)| (!line.is_empty()).then(|| (name.into(), line.into()));
        Silent(values(text, KEY_FEED).filter_map(split_named).filter_map(to_entry).collect())
    }

    /// The text of their file, a line each as the rundowns' file holds a feed.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for (name, line) in &self.0 {
            let _ = writeln!(text, "{KEY_FEED} {name} {line}");
        }
        text
    }

    // Takes the feeds of a rundown that a gathering found silent, in the place of what was known of it; true where that is news.
    pub(super) fn note(&mut self, name: &str, lines: &[String]) -> bool {
        if self.0.iter().filter(|entry| entry.0 == name).map(|entry| &entry.1).eq(lines) {
            return false;
        }
        self.0.retain(|entry| entry.0 != name);
        self.0.extend(lines.iter().map(|line| (name.into(), line.clone())));
        true
    }

    pub(crate) fn holds(&self, name: &str, line: &str) -> bool {
        self.0.iter().any(|entry| entry.0 == name && entry.1 == line)
    }

    // How many of the feeds a rundown reads were silent: what was kept of a feed it reads no more counts for nothing.
    pub(crate) fn count_in(&self, topic: &Topic) -> usize {
        topic.feed_lines().into_iter().filter(|line| self.holds(&topic.name, line)).count()
    }

    // The lines a model is told them by: those of the feeds the rundowns kept still read, in the rundowns' order.
    pub(super) fn to_told(&self, topics: &Topics) -> String {
        let mut text = String::new();
        for topic in topics.all() {
            for line in topic.feed_lines().into_iter().filter(|line| self.holds(&topic.name, line)) {
                let _ = writeln!(text, "{KEY_FEED} {} {line}", topic.name);
            }
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip_and_notes() {
        let text = "feed news https://a.example/rss\nfeed tech b.example https://news.example/search?q=b\n";
        let mut silent = Silent::parse(text);
        assert_eq!(silent.to_text(), text);
        assert!(silent.holds("news", "https://a.example/rss") && !silent.holds("tech", "https://a.example/rss"));
        assert_eq!(
            Silent::parse("feed News x\nfeed news\nrundown news on\nfeed news  https://a.example/rss \n"),
            Silent(alloc::vec![("news".into(), "https://a.example/rss".into())]),
            "a line that names a rundown and a feed"
        );
        assert!(!silent.note("news", &["https://a.example/rss".into()]), "what was known is no news");
        assert!(silent.note("news", &["https://c.example/rss".into(), "https://d.example/rss".into()]));
        let kept = Topics::parse(
            "rundown news on\nfeed news https://c.example/rss\nfeed news https://e.example/rss\nrundown tech off\nfeed tech https://t.example/rss\n",
        );
        assert_eq!(
            kept.all().iter().map(|topic| silent.count_in(topic)).collect::<Vec<_>>(),
            [1, 0],
            "of the feeds a rundown reads now: one replaced or removed since it was silent counts no more"
        );
        assert_eq!(
            silent.to_told(&kept),
            "feed news https://c.example/rss\n",
            "and a model is told of those alone, none of a rundown kept no more"
        );
        assert!(silent.note("tech", &[]) && !silent.note("tech", &[]), "a rundown whose feeds all answered");
        assert_eq!(silent.to_text(), "feed news https://c.example/rss\nfeed news https://d.example/rss\n");
        assert_eq!(Silent::default().to_text(), "");
    }
}

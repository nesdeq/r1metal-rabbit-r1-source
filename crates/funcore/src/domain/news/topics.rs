//! The rundowns the daily edition makes, as the owner keeps them: each with its name, whether it is made, what it is about, its icon and its feeds.

use super::feed::{self, Feed};
use crate::base::lines::values;
use crate::base::text::{parse_switch, switch_word, to_one_line};
use crate::domain::paper::names::is_name;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt::Write;

pub const FILE: &str = "rundowns";
// The key the settings' file of an earlier firmware keeps each of the two rundowns' switch by, too.
const KEY_RUNDOWN: &str = "rundown";
const KEY_ABOUT: &str = "about";
const KEY_ICON: &str = "icon";
pub(super) const KEY_FEED: &str = "feed";
const NEWS: &str = "news";
const FINANCE: &str = "finance";
// The rundowns a new device has, whose feeds and instructions the firmware holds.
const STANDARD: [&str; 2] = [NEWS, FINANCE];
const NEWS_FEEDS: &str = include_str!("../../../assets/news/news.feeds");
const FINANCE_FEEDS: &str = include_str!("../../../assets/news/finance.feeds");
const NEWS_TASK: &str = include_str!("../../../assets/news/news.txt");
const FINANCE_TASK: &str = include_str!("../../../assets/news/finance.txt");
const TOPIC_TASK: &str = include_str!("../../../assets/news/topic.txt");
const NAME_MARK: &str = "{NAME}";
const ABOUT_MARK: &str = "{ABOUT}";

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        [$(($name, include_str!(concat!("../../../assets/material/", $name, ".svg")))),*]
    };
}

// The icons a rundown may go by, each under its name among Material Symbols with its drawing: the news' and finance's first, which a rundown that keeps none goes by.
pub(crate) const ICONS: [(&str, &str); 82] = icons![
    "newspaper",
    "finance",
    "public",
    "flag",
    "account_balance",
    "gavel",
    "how_to_vote",
    "military_tech",
    "shield",
    "local_police",
    "trending_up",
    "currency_bitcoin",
    "work",
    "storefront",
    "real_estate_agent",
    "factory",
    "agriculture",
    "oil_barrel",
    "bolt",
    "solar_power",
    "eco",
    "computer",
    "memory",
    "smart_toy",
    "code",
    "sports_esports",
    "science",
    "biotech",
    "rocket_launch",
    "psychology",
    "medical_services",
    "school",
    "sports",
    "sports_soccer",
    "sports_basketball",
    "sports_tennis",
    "sports_football",
    "sports_baseball",
    "sports_hockey",
    "sports_golf",
    "sports_cricket",
    "sports_rugby",
    "sports_volleyball",
    "sports_handball",
    "sports_motorsports",
    "motorcycle",
    "sports_mma",
    "directions_bike",
    "directions_run",
    "pool",
    "downhill_skiing",
    "sailing",
    "hiking",
    "fitness_center",
    "chess",
    "movie",
    "tv",
    "music_note",
    "theater_comedy",
    "palette",
    "menu_book",
    "photo_camera",
    "architecture",
    "history_edu",
    "checkroom",
    "restaurant",
    "wine_bar",
    "flight",
    "directions_car",
    "ev_station",
    "train",
    "directions_boat",
    "forest",
    "pets",
    "wb_sunny",
    "volcano",
    "church",
    "child_care",
    "potted_plant",
    "construction",
    "celebration",
    "podcasts",
];

fn find_icon(word: &str) -> Option<&'static (&'static str, &'static str)> {
    ICONS.iter().find(|icon| icon.0 == word)
}

// The drawing a rundown of that name goes by where it keeps `icon`: that icon's, else finance's or the news' by its name.
fn to_drawing(name: &str, icon: &str) -> &'static str {
    find_icon(icon).unwrap_or(&ICONS[usize::from(name == FINANCE)]).1
}

// The order a day's rundowns are listed in: the two a new device has first, then by name.
pub(crate) fn compare(one: &str, other: &str) -> Ordering {
    let place = |name: &str| STANDARD.iter().position(|standard| *standard == name).unwrap_or(STANDARD.len());
    place(one).cmp(&place(other)).then_with(|| one.cmp(other))
}

// A line's name and what follows it.
pub(super) fn split_named(line: &str) -> Option<(&str, &str)> {
    let (name, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
    is_name(name).then_some((name, rest.trim()))
}

fn standard_feeds(name: &str) -> &'static str {
    match name {
        NEWS => NEWS_FEEDS,
        FINANCE => FINANCE_FEEDS,
        _ => "",
    }
}

/// A rundown of the daily edition.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Topic {
    /// Its word in file names and lines of its files.
    pub name: String,
    pub(crate) is_on: bool,
    // What it is about, in the owner's words; none for the two a new device has, whose instructions say it.
    pub(crate) about: String,
    // The name of its icon among `ICONS`; none where it goes by the icon of its name.
    pub(super) icon: &'static str,
    // Its feeds, a line each as a feeds list holds one; none where it reads the firmware's list of its name.
    pub(super) feeds: Vec<String>,
}

impl Topic {
    pub(crate) fn named(name: &str, is_on: bool) -> Topic {
        Topic { name: name.into(), is_on, about: String::new(), icon: "", feeds: Vec::new() }
    }

    // The drawing of the icon it goes by.
    pub(crate) fn drawing(&self) -> &'static str {
        to_drawing(&self.name, self.icon)
    }

    // The lines of its feeds: its own, or the firmware's of its name where it keeps none.
    pub(crate) fn feed_lines(&self) -> Vec<&str> {
        if self.feeds.is_empty() {
            return feed::lines(standard_feeds(&self.name)).collect();
        }
        self.feeds.iter().map(String::as_str).collect()
    }

    pub(super) fn feeds(&self) -> Vec<Feed<'_>> {
        self.feed_lines().into_iter().map(Feed::parse).collect()
    }

    /// The model's instructions for its stories: the firmware's for the two a new device has, while the owner says nothing of them, else the instructions of a rundown the owner tells about.
    pub fn task(&self) -> String {
        match self.name.as_str() {
            NEWS if self.about.is_empty() => NEWS_TASK.into(),
            FINANCE if self.about.is_empty() => FINANCE_TASK.into(),
            name => TOPIC_TASK.replace(NAME_MARK, name).replace(ABOUT_MARK, &self.about),
        }
    }
}

/// The rundowns of the daily edition, in the order they are listed and made.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Topics(Vec<Topic>);

impl Default for Topics {
    fn default() -> Topics {
        Topics(STANDARD.map(|name| Topic::named(name, true)).into())
    }
}

impl Topics {
    /// The rundowns a kept text holds, in the order of their `rundown` lines; a line that does not read, or that names none of them, is passed over.
    pub fn parse(text: &str) -> Topics {
        let mut topics: Vec<Topic> = Vec::new();
        for (name, switch) in values(text, KEY_RUNDOWN).filter_map(split_named) {
            let Some(is_on) = parse_switch(switch) else { continue };
            match topics.iter_mut().find(|topic| topic.name == name) {
                Some(topic) => topic.is_on = is_on,
                None => topics.push(Topic::named(name, is_on)),
            }
        }
        for (name, about) in values(text, KEY_ABOUT).filter_map(split_named) {
            if let Some(topic) = topics.iter_mut().find(|topic| topic.name == name) {
                topic.about = to_one_line(about);
            }
        }
        for (name, word) in values(text, KEY_ICON).filter_map(split_named) {
            if let (Some(topic), Some(icon)) = (topics.iter_mut().find(|topic| topic.name == name), find_icon(word)) {
                topic.icon = icon.0;
            }
        }
        for (name, feed) in values(text, KEY_FEED).filter_map(split_named).filter(|(_, feed)| !feed.is_empty()) {
            if let Some(topic) = topics.iter_mut().find(|topic| topic.name == name) {
                topic.feeds.push(to_one_line(feed));
            }
        }
        Topics(topics)
    }

    fn write_lines(&self, has_every_feed: bool) -> String {
        let mut text = String::new();
        for topic in &self.0 {
            let name = &topic.name;
            let _ = writeln!(text, "{KEY_RUNDOWN} {name} {}", switch_word(topic.is_on));
            if !topic.about.is_empty() {
                let _ = writeln!(text, "{KEY_ABOUT} {name} {}", topic.about);
            }
            if !topic.icon.is_empty() {
                let _ = writeln!(text, "{KEY_ICON} {name} {}", topic.icon);
            }
            let own = topic.feeds.iter().map(String::as_str).collect();
            for feed in if has_every_feed { topic.feed_lines() } else { own } {
                let _ = writeln!(text, "{KEY_FEED} {name} {feed}");
            }
        }
        text
    }

    /// The text of the rundowns' file.
    pub fn to_text(&self) -> String {
        self.write_lines(false)
    }

    // The lines a model is told them by: their file's, with every feed a rundown reads, the firmware's too.
    pub(super) fn to_told(&self) -> String {
        self.write_lines(true)
    }

    pub(super) fn into_all(self) -> Vec<Topic> {
        self.0
    }

    /// The rundowns a start reads: what their file keeps, else the two a new device has, each switched as the settings' file of an earlier firmware keeps it, which are then true to keep in the rundowns' own.
    pub fn read(kept: Option<&str>, settings: &str) -> (Topics, bool) {
        if let Some(text) = kept {
            return (Topics::parse(text), false);
        }
        let earlier = Topics::parse(settings);
        let mut topics = Topics::default();
        for topic in &mut topics.0 {
            topic.is_on = earlier.get(&topic.name).map_or(topic.is_on, |kept| kept.is_on);
        }
        (topics, !earlier.0.is_empty())
    }

    /// The log line of the rundowns read at a start.
    pub fn to_read_line(&self) -> String {
        alloc::format!("news: {} rundowns kept, {} of them made, as kept", self.0.len(), self.made().count())
    }

    /// The log line of the rundowns written.
    pub fn to_kept_line(&self) -> String {
        alloc::format!("news: kept {} rundowns, {} of them made", self.0.len(), self.made().count())
    }

    pub fn all(&self) -> &[Topic] {
        &self.0
    }

    pub fn get(&self, name: &str) -> Option<&Topic> {
        self.0.iter().find(|topic| topic.name == name)
    }

    // The drawing of the icon a rundown of that name goes by, kept or, as in an edition of an earlier day, kept no longer.
    pub(crate) fn drawing_of(&self, name: &str) -> &'static str {
        self.get(name).map_or_else(|| to_drawing(name, ""), Topic::drawing)
    }

    // The rundowns that are switched on.
    pub(crate) fn made(&self) -> impl Iterator<Item = &Topic> {
        self.0.iter().filter(|topic| topic.is_on)
    }

    // The rundown of that name, where it is kept and switched on.
    pub(super) fn get_made(&self, name: &str) -> Option<&Topic> {
        self.get(name).filter(|topic| topic.is_on)
    }

    // The two a new device has that are kept no longer.
    pub(crate) fn lacking(&self) -> impl Iterator<Item = &'static str> {
        STANDARD.into_iter().filter(|name| self.get(name).is_none())
    }

    // Switches a rundown; true where that changed it.
    pub(crate) fn switch(&mut self, name: &str, is_on: bool) -> bool {
        let topic = self.0.iter_mut().find(|topic| topic.name == name);
        topic.is_some_and(|topic| core::mem::replace(&mut topic.is_on, is_on) != is_on)
    }

    // Keeps a rundown in the place of the one of its name, or behind the others; true where that changed them.
    pub(crate) fn put(&mut self, topic: Topic) -> bool {
        match self.0.iter_mut().find(|kept| kept.name == topic.name) {
            Some(kept) => core::mem::replace(kept, topic.clone()) != topic,
            None => {
                self.0.push(topic);
                true
            }
        }
    }

    // Takes a rundown away; true where there was one of that name.
    pub(crate) fn remove(&mut self, name: &str) -> bool {
        let count = self.0.len();
        self.0.retain(|topic| topic.name != name);
        self.0.len() != count
    }

    // Takes a feed of a rundown away by its place among its lines, but its last; one that read the firmware's list keeps the rest of it as its own. True where it went.
    pub(crate) fn remove_feed(&mut self, name: &str, at: usize) -> bool {
        let Some(topic) = self.0.iter_mut().find(|topic| topic.name == name) else {
            return false;
        };
        let mut lines: Vec<String> = topic.feed_lines().into_iter().map(String::from).collect();
        if at >= lines.len() || lines.len() == 1 {
            return false;
        }
        lines.remove(at);
        topic.feeds = lines;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_round_trip() {
        let text = "rundown news on\nrundown finance off\nrundown motorsport on\nabout motorsport Formula 1 and MotoGP: races and results\nicon motorsport sports_motorsports\nfeed motorsport https://www.autosport.example/rss/f1\nfeed motorsport motogp.example https://news.example/search?q=motogp\n";
        let topics = Topics::parse(text);
        assert_eq!(topics.to_text(), text);
        assert_eq!(
            topics.all().iter().map(|topic| (topic.name.as_str(), topic.is_on)).collect::<Vec<_>>(),
            [("news", true), ("finance", false), ("motorsport", true)]
        );
        assert_eq!(topics.made().count(), 2);
        assert_eq!(
            ["news", "finance", "weather"].map(|name| topics.get_made(name).map(|topic| topic.name.as_str())),
            [Some("news"), None, None],
            "one that is switched off or not kept is not made"
        );
        let motorsport = topics.get("motorsport").unwrap();
        assert_eq!(
            motorsport.feeds().iter().map(Feed::source).collect::<Vec<_>>(),
            ["autosport.example", "motogp.example"],
            "its own feeds, a line each as a feeds list holds one"
        );
        assert_eq!(Topics::default().to_text(), "rundown news on\nrundown finance on\n");
        assert_eq!(
            Topics::parse(
                "rundown Sport on\nrundown 1x on\nrundown tech maybe\nrundown\nabout nobody x\nfeed news\nrundown tech off\nrundown tech on\nabout tech  chips\n and more\n"
            ),
            Topics(alloc::vec![Topic { about: "chips".into(), ..Topic::named("tech", true) }]),
            "a name of lowercase letters and digits, its last switch that reads, and nothing for a rundown that is not there"
        );
        assert_eq!(Topics::parse("").all().len(), 0, "a file that keeps none makes none");
        assert_eq!(
            (topics.to_read_line(), topics.to_kept_line()),
            ("news: 3 rundowns kept, 2 of them made, as kept".into(), "news: kept 3 rundowns, 2 of them made".into())
        );
    }

    #[test]
    fn earlier_settings_move() {
        let settings = "brightness 40\nrundown news on\nrundown finance off\nrundown weather on\n";
        let (topics, is_moved) = Topics::read(None, settings);
        assert!(is_moved, "what the settings' file kept is kept in the rundowns' own from now on");
        assert_eq!(
            topics.all().iter().map(|topic| (topic.name.as_str(), topic.is_on)).collect::<Vec<_>>(),
            [("news", true), ("finance", false)],
            "the two a new device has, each switched as it was"
        );
        assert_eq!(Topics::read(None, "brightness 40\n"), (Topics::default(), false), "nothing kept, nothing moved");
        assert_eq!(
            Topics::read(Some(""), settings),
            (Topics::parse(""), false),
            "the rundowns' own file counts once it is there, empty too"
        );
    }

    #[test]
    fn standard_feeds_and_tasks() {
        let topics = Topics::default();
        let (news, finance) = (topics.all()[0].feeds(), topics.all()[1].feeds());
        assert_eq!((news.len(), finance.len()), (21, 26), "the two a new device has read the firmware's lists");
        assert_eq!(
            (news[1].address, news[1].source().as_str(), news[20].address),
            (
                "https://feeds.arstechnica.com/arstechnica/index",
                "arstechnica.com",
                "https://www.thenewatlantis.com/feed"
            )
        );
        assert!(news.iter().chain(&finance).all(|feed| feed.address.starts_with("https://")));
        assert_eq!(
            (news[0].source().as_str(), news[0].address.starts_with("https://news.google.com/rss/search?q=site:")),
            ("venturebeat.com", true)
        );
        let tasks: Vec<String> = topics.all().iter().map(Topic::task).collect();
        assert!(tasks[0].starts_with("You are given the stories of today's news feeds"));
        assert!(tasks[1].starts_with("You are given the stories of today's finance feeds"));
        let own = Topic {
            about: "Formula 1, short, in German".into(),
            feeds: alloc::vec!["https://a.example/rss".into()],
            ..Topic::named("motorsport", true)
        };
        let task = own.task();
        assert!(
            task.starts_with("You are given the stories of today's motorsport feeds")
                && task.contains("and says of it: Formula 1, short, in German\n")
                && !task.contains(NAME_MARK)
                && !task.contains(ABOUT_MARK),
            "{task}"
        );
        for task in tasks.iter().chain([&task]) {
            assert!(task.contains("No title, no tables, no links, no code, no emoji"));
        }
        let told = Topic { about: "only chips".into(), ..Topic::named("news", true) };
        assert!(told.task().contains("says of it: only chips"), "one of the two the owner tells about is told so");
        assert_eq!(told.feeds().len(), 21, "and reads the firmware's list while it keeps no feed of its own");
        assert!(Topic::named("weather", true).feeds().is_empty(), "the firmware has no list of another name");
    }

    #[test]
    fn switched_put_and_removed() {
        let mut topics = Topics::default();
        assert!(topics.switch("finance", false) && !topics.switch("finance", false) && !topics.switch("sport", true));
        assert!(topics.remove("news") && !topics.remove("news"));
        assert_eq!(topics.to_text(), "rundown finance off\n");
        assert_eq!(topics.lacking().collect::<Vec<_>>(), ["news"], "what a new device has and this one keeps no more");
        let sport = Topic { feeds: alloc::vec!["https://a.example/rss".into()], ..Topic::named("sport", true) };
        assert!(topics.put(sport.clone()) && !topics.put(sport) && topics.put(Topic::named("news", true)));
        assert!(topics.put(Topic::named("finance", true)), "one of its name is kept in its place");
        assert_eq!(
            topics.all().iter().map(|topic| topic.name.as_str()).collect::<Vec<_>>(),
            ["finance", "sport", "news"]
        );
        assert!(topics.remove_feed("finance", 0) && !topics.remove_feed("finance", 25) && !topics.remove_feed("x", 0));
        let finance = topics.get("finance").unwrap();
        assert_eq!(
            (finance.feeds.len(), finance.feeds().len()),
            (25, 25),
            "what is left of the firmware's list is its own from then on"
        );
        assert!(
            !topics.remove_feed("sport", 0) && topics.get("sport").unwrap().feeds().len() == 1,
            "a rundown keeps its last feed"
        );
    }

    #[test]
    fn names_and_order() {
        let mut names = ["tech", "finance", "art", "news"];
        names.sort_by(|one, other| compare(one, other));
        assert_eq!(names, ["news", "finance", "art", "tech"]);
        assert_eq!(split_named(" tech  on "), Some(("tech", "on")));
        assert_eq!((split_named("tech"), split_named("Tech on")), (Some(("tech", "")), None));
    }

    #[test]
    fn icons_by_name() {
        assert_eq!((ICONS[0].0, ICONS[1].0), ("newspaper", "finance"), "what a rundown that keeps none goes by");
        for (at, (name, drawing)) in ICONS.iter().enumerate() {
            assert!(drawing.starts_with("<svg"), "{name}");
            assert!(ICONS[..at].iter().all(|earlier| earlier.0 != *name), "{name} stands once");
        }
        let topics = Topics::parse(
            "rundown news on\nrundown finance on\nrundown f1 on\nicon f1 flag\nicon f1 sports_motorsports\nicon f1 no_such_icon\nicon nobody flag\nrundown chess on\nicon finance chess\n",
        );
        let drawing = |word: &str| find_icon(word).unwrap().1;
        assert_eq!(
            ["news", "finance", "f1", "chess", "weather"].map(|name| topics.drawing_of(name)),
            ["newspaper", "chess", "sports_motorsports", "newspaper", "newspaper"].map(drawing),
            "its own icon, the last that names one; else the news', for a rundown kept no longer too"
        );
        assert_eq!(Topics::default().drawing_of("finance"), drawing("finance"), "finance by its name");
        assert_eq!(
            topics.to_text(),
            "rundown news on\nrundown finance on\nicon finance chess\nrundown f1 on\nicon f1 sports_motorsports\nrundown chess on\n"
        );
    }
}

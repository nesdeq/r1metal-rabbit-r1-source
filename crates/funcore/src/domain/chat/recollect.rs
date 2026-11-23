//! The day's recollection that settles the robot's memory.

use super::memory::{ALWAYS, Memory, join_day, list_items, split_day, to_bullets};
use crate::base::time::Moment;
use crate::domain::paper::sheet::HEAD_END;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The instructions of the model that settles a day.
pub const PROMPT: &str = include_str!("../../../assets/chat/recollect.txt");
const DAY: &str = "## Day";
const KNOWLEDGE: &str = "## Knowledge";

/// The parsed answer of a day's recollection.
#[derive(Clone, PartialEq, Eq)]
pub struct Recollection {
    pub day: String,
    pub core: Vec<String>,
    pub knowledge: Vec<(String, String)>,
}

// As `Memory` tells it: the day's words and the facts are left out.
impl core::fmt::Debug for Recollection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Recollection")
            .field("core", &self.core.len())
            .field("knowledge", &self.knowledge.len())
            .finish_non_exhaustive()
    }
}

/// The input of the model that settles a day.
pub fn to_request(memory: &Memory, moment: &Moment, words: &str) -> String {
    let knowledge = memory.knowledge.iter().map(|(day, line)| join_day(day, line));
    format!(
        "{ALWAYS}\n\n{}\n{KNOWLEDGE}\n\n{}\n## The words of {}\n\n{}\n",
        to_bullets(&memory.core),
        to_bullets(knowledge),
        moment.to_date_words(true, true),
        words.trim()
    )
}

impl Recollection {
    /// Parses an answer; none without its three sections or with an empty day.
    pub fn parse(answer: &str) -> Option<Recollection> {
        let heads = [DAY, ALWAYS, KNOWLEDGE];
        let mut sections: [Option<Vec<&str>>; 3] = [None, None, None];
        let mut current = None;
        for line in answer.lines() {
            match heads.iter().position(|head| line.trim() == *head) {
                Some(index) => {
                    sections[index] = Some(Vec::new());
                    current = Some(index);
                }
                None => {
                    if let Some(section) = current.and_then(|index: usize| sections[index].as_mut()) {
                        section.push(line);
                    }
                }
            }
        }
        let [Some(day), Some(core), Some(knowledge)] = sections else {
            return None;
        };
        let day = day.join("\n");
        let knowledge = list_items(knowledge).into_iter().map(split_day).collect();
        Some(Recollection { day: day.trim().into(), core: list_items(core), knowledge })
            .filter(|recollection| !recollection.day.is_empty())
    }
}

/// The file of a day's recollection.
pub fn to_day_file(moment: &Moment, text: &str) -> String {
    format!(
        "# The day of {}\n\nWhat the robot kept of that day's conversation, settled overnight, as every later conversation is told it.\n{HEAD_END}\n{}\n",
        moment.to_date_words(true, true),
        text.trim()
    )
}

/// The log line of a day whose words are missing.
pub fn to_unread_line(day: &str) -> String {
    format!("chat: the words of {day} cannot be read, and the day is not settled")
}

/// The log line of an answer without its sections.
pub fn to_unsectioned_line(day: &str) -> String {
    format!("chat: the recollection of {day} lacks its sections and is kept as it came, the memory as it was")
}

/// The log line of a day forgotten while it settled.
pub fn to_forgotten_line(day: &str) -> String {
    format!("chat: {day} was forgotten while it settled, and keeps nothing of it")
}

/// The log line of a day settled in `text`.
pub fn to_settled_line(day: &str, text: &str) -> String {
    format!("chat: {day} settled in {} characters", text.chars().count())
}

/// The `days` before `today` that `done_days` lacks, each with whether the network is kept past it for the next.
pub fn unsettled_days(days: Vec<String>, done_days: &[String], today: &str) -> Vec<(String, bool)> {
    let mut unsettled: Vec<(String, bool)> = days
        .into_iter()
        .filter(|day| day.as_str() < today && !done_days.contains(day))
        .map(|day| (day, true))
        .collect();
    if let Some(last) = unsettled.last_mut() {
        last.1 = false;
    }
    unsettled
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const NOW: u64 = 1_792_159_320;

    #[test]
    fn request_text() {
        let mut memory = Memory::default();
        memory.core = vec!["Lives in Vienna.".into()];
        memory.knowledge = vec![("2026-10-16".into(), "Likes tea.".into())];
        assert_eq!(
            to_request(&memory, &Moment::from_seconds(NOW), " Words. "),
            "## Always\n\n- Lives in Vienna.\n\n## Knowledge\n\n- 2026-10-16: Likes tea.\n\n## The words of Friday 16 October 2026\n\nWords.\n"
        );
    }

    #[test]
    fn parse_sections() {
        let answer = "## Day\n\nRain all day.\n\n## Always\n\n- Lives in Graz.\n\n## Knowledge\n\n- 2026-10-16: Likes tea.\n- No day.\n";
        assert_eq!(
            Recollection::parse(answer),
            Some(Recollection {
                day: "Rain all day.".into(),
                core: vec!["Lives in Graz.".into()],
                knowledge: vec![("2026-10-16".into(), "Likes tea.".into()), (String::new(), "No day.".into())]
            })
        );
        assert_eq!(
            format!("{:?}", Recollection::parse(answer)),
            "Some(Recollection { core: 1, knowledge: 2, .. })",
            "formatted, it tells how many facts it holds and none of them"
        );
        assert_eq!(Recollection::parse("## Day\n\nx\n"), None);
        assert_eq!(Recollection::parse("## Day\n\n## Always\n\n## Knowledge\n"), None);
    }

    #[test]
    fn day_file_text() {
        assert_eq!(
            to_day_file(&Moment::from_seconds(NOW), " Rain. "),
            "# The day of Friday 16 October 2026\n\nWhat the robot kept of that day's conversation, settled overnight, as every later conversation is told it.\n\n---\n\nRain.\n"
        );
    }

    #[test]
    fn unsettled_past_days() {
        let days = || ["1", "2", "3", "4"].map(String::from).to_vec();
        assert_eq!(unsettled_days(days(), &[String::from("1")], "3"), [(String::from("2"), false)]);
        assert_eq!(
            unsettled_days(days(), &[String::from("1")], "5"),
            [(String::from("2"), true), (String::from("3"), true), (String::from("4"), false)],
            "the network is kept from one day to the next, and let go of after the last"
        );
        assert!(unsettled_days(days(), &[], "1").is_empty());
    }

    #[test]
    fn settle_lines() {
        assert_eq!(to_unread_line("1"), "chat: the words of 1 cannot be read, and the day is not settled");
        assert_eq!(
            to_unsectioned_line("1"),
            "chat: the recollection of 1 lacks its sections and is kept as it came, the memory as it was"
        );
        assert_eq!(to_settled_line("1", "Grün"), "chat: 1 settled in 4 characters");
        assert_eq!(to_forgotten_line("1"), "chat: 1 was forgotten while it settled, and keeps nothing of it");
    }
}

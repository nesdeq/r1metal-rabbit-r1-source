//! The robot's memory of the user and of itself, as two Markdown files.

use crate::base::text::to_one_line;
use crate::base::time::stamp::parse_iso_day;
use crate::domain::paper::sheet::{HEAD_END, body};
use crate::gfx::font::{spleen_12x24, spleen_16x32};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{Display, Write};

// The bar's 342 px beside the robot, at 16 px a character.
pub(super) const NAME_MAX_CHARS: usize = 21;
const NAME_SIGNS: [char; 3] = [' ', '-', '\''];
const CORE_HEAD: &str = "# What the robot keeps\n\nWhat the robot of this R1 knows of its owner and of itself, read into every conversation it holds.";
const KNOWLEDGE_HEAD: &str =
    "# What the robot has learned\n\nWhat the robot of this R1 found worth knowing later, a line each, with its day.";
const YOU: &str = "## You";
const ROBOT: &str = "## The robot";
pub(super) const ALWAYS: &str = "## Always";
const LEARNED: &str = "## Learned";
const NAME: &str = "Name";
const TONE: &str = "Tone";
const AUTHOR_PREFIX: &str = ", given by ";
const DATE_PREFIX: &str = ", on ";
const NO_NAME: &str = "none, as the user wished";
const NO_TONE: &str = "none in particular";
const DECLINED: &str = "rather not say";
const PICK_SEPARATOR: &str = "; ";
const TONE_SEPARATOR: &str = ", ";

/// An area of the profile the first conversation asks about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Area {
    Name,
    Age,
    Place,
    Languages,
    Work,
    Interests,
    Movies,
    Games,
    Music,
    Media,
}

impl Area {
    pub(super) const REST: [Area; 9] = [
        Area::Age,
        Area::Place,
        Area::Languages,
        Area::Work,
        Area::Interests,
        Area::Movies,
        Area::Games,
        Area::Music,
        Area::Media,
    ];

    pub(super) fn parse(word: &str) -> Option<Area> {
        Some(match word {
            "name" => Area::Name,
            "age" => Area::Age,
            "place" => Area::Place,
            "languages" => Area::Languages,
            "work" => Area::Work,
            "interests" => Area::Interests,
            "movies" => Area::Movies,
            "games" => Area::Games,
            "music" => Area::Music,
            "media" => Area::Media,
            _ => return None,
        })
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Area::Name => "Name",
            Area::Age => "Age",
            Area::Place => "Place",
            Area::Languages => "Languages",
            Area::Work => "Work",
            Area::Interests => "Interests",
            Area::Movies => "Movies",
            Area::Games => "Games",
            Area::Music => "Music",
            Area::Media => "Other media",
        }
    }

    pub(super) const fn is_favourites(self) -> bool {
        matches!(self, Area::Movies | Area::Games | Area::Music | Area::Media)
    }
}

fn label_value<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let text = line.strip_prefix("**")?.strip_prefix(label)?.strip_prefix(":**")?;
    Some(text.trim())
}

pub(super) fn split_day(line: String) -> (String, String) {
    match line.split_once(": ").filter(|parts| parse_iso_day(parts.0).is_some()) {
        Some((day, text)) => (day.into(), text.into()),
        None => (String::new(), line),
    }
}

pub(super) fn join_day(day: &str, text: &str) -> String {
    if day.is_empty() { text.into() } else { format!("{day}: {text}") }
}

fn to_folded(text: &str) -> String {
    to_one_line(text).to_lowercase()
}

fn is_heading(line: &str) -> bool {
    line.trim_start().starts_with("## ")
}

pub(super) fn list_items<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let bullets = lines.into_iter().filter_map(|line| line.trim().strip_prefix("- "));
    bullets.map(to_one_line).filter(|item| !item.is_empty()).collect()
}

pub(super) fn to_bullets(items: impl IntoIterator<Item = impl Display>) -> String {
    items.into_iter().fold(String::new(), |mut text, item| {
        let _ = writeln!(text, "- {item}");
        text
    })
}

fn section_items(file: &str, head: &str) -> Vec<String> {
    let mut lines = file.lines().skip_while(|line| line.trim() != head);
    lines.next();
    list_items(lines.take_while(|line| !is_heading(line)))
}

fn to_section(head: &str, items: &[String]) -> String {
    if items.is_empty() { String::new() } else { format!("{head}\n\n{}", to_bullets(items)) }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct ProfileEntry {
    pub(super) is_declined: bool,
    pub(super) pick_count: usize,
}

/// The contents of the two memory files.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Memory {
    you: Vec<String>,
    robot: Vec<String>,
    pub(crate) core: Vec<String>,
    // Facts with their day as `2026-10-16`, oldest first.
    pub(crate) knowledge: Vec<(String, String)>,
}

// The facts are the owner's, which no log tells: how many it holds of each kind.
impl core::fmt::Debug for Memory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Memory")
            .field("you", &self.you.len())
            .field("robot", &self.robot.len())
            .field("core", &self.core.len())
            .field("knowledge", &self.knowledge.len())
            .finish()
    }
}

fn find_label(lines: &[String], label: &str) -> Option<usize> {
    lines.iter().position(|line| label_value(line, label).is_some())
}

fn put_label(lines: &mut Vec<String>, label: &str, value: &str, is_first: bool) {
    let line = format!("**{label}:** {value}");
    match find_label(lines, label) {
        Some(index) => lines[index] = line,
        None if is_first => lines.insert(0, line),
        None => lines.push(line),
    }
}

fn value_of<'a>(lines: &'a [String], label: &str) -> Option<&'a str> {
    lines.iter().find_map(|line| label_value(line, label))
}

fn is_writable(character: char) -> bool {
    let (large_font, small_font) = (spleen_16x32(), spleen_12x24());
    let is_allowed = character.is_alphanumeric() || NAME_SIGNS.contains(&character);
    let capital = character.to_uppercase().next().unwrap_or(character);
    is_allowed && large_font.has(character) && small_font.has(character) && large_font.has(capital)
}

impl Memory {
    pub fn parse(core: &str, knowledge: &str) -> Memory {
        let items = list_items(body(knowledge).lines().take_while(|line| !is_heading(line)));
        Memory {
            you: section_items(core, YOU),
            robot: section_items(core, ROBOT),
            core: section_items(core, ALWAYS),
            knowledge: items.into_iter().map(split_day).collect(),
        }
    }

    fn to_sections(&self, knowledge: &[String]) -> String {
        let sections = [
            to_section(YOU, &self.you),
            to_section(ROBOT, &self.robot),
            to_section(ALWAYS, &self.core),
            to_section(LEARNED, knowledge),
        ];
        let texts: Vec<String> = sections.into_iter().filter(|section| !section.is_empty()).collect();
        texts.join("\n")
    }

    fn knowledge_lines(&self) -> impl Iterator<Item = String> + '_ {
        self.knowledge.iter().map(|(day, line)| join_day(day, line))
    }

    /// The text of the core file.
    pub fn to_core_text(&self) -> String {
        format!("{CORE_HEAD}\n{HEAD_END}\n{}", self.to_sections(&[]))
    }

    /// The text of the knowledge file.
    pub fn to_knowledge_text(&self) -> String {
        format!("{KNOWLEDGE_HEAD}\n{HEAD_END}\n{}", to_bullets(self.knowledge_lines()))
    }

    pub(super) fn to_digest(&self) -> String {
        let knowledge: Vec<String> = self.knowledge_lines().collect();
        let sections = self.to_sections(&knowledge);
        if sections.is_empty() { String::from("Nothing yet.\n") } else { sections }
    }

    pub(super) fn set_profile(&mut self, area: Area, entries: &[String], is_declined: bool) {
        let picks: Vec<String> =
            entries.iter().map(|entry| to_one_line(entry)).filter(|entry| !entry.is_empty()).collect();
        let value = if is_declined || picks.is_empty() { String::from(DECLINED) } else { picks.join(PICK_SEPARATOR) };
        put_label(&mut self.you, area.label(), &value, false);
    }

    pub(super) fn profile_of(&self, area: Area) -> Option<ProfileEntry> {
        let value = value_of(&self.you, area.label())?;
        let is_declined = value == DECLINED;
        let pick_count =
            if is_declined { 0 } else { value.split(PICK_SEPARATOR).filter(|pick| !pick.trim().is_empty()).count() };
        Some(ProfileEntry { is_declined, pick_count })
    }

    pub(crate) fn user_name(&self) -> Option<&str> {
        value_of(&self.you, Area::Name.label()).filter(|name| *name != DECLINED)
    }

    pub(super) fn set_robot_name(&mut self, name: Option<&str>, author: Option<&str>, day: &str) -> Result<(), String> {
        let value = match name.map(str::trim).filter(|name| !name.is_empty()) {
            None => String::from(NO_NAME),
            Some(name) if name.chars().count() > NAME_MAX_CHARS => {
                return Err(format!("longer than the {NAME_MAX_CHARS} characters the screen has room for"));
            }
            Some(name) if !name.chars().all(is_writable) => {
                return Err(String::from("has a character the screen cannot write"));
            }
            Some(name) => {
                let credit =
                    author.map_or_else(String::new, |author| format!("{AUTHOR_PREFIX}{}", to_one_line(author)));
                format!("{}{credit}{DATE_PREFIX}{day}", to_one_line(name))
            }
        };
        put_label(&mut self.robot, NAME, &value, true);
        Ok(())
    }

    pub(super) fn has_been_named(&self) -> bool {
        find_label(&self.robot, NAME).is_some()
    }

    pub fn robot_name(&self) -> Option<&str> {
        let value = value_of(&self.robot, NAME).filter(|value| *value != NO_NAME)?;
        Some(value.split(',').next().unwrap_or(value).trim()).filter(|name| !name.is_empty())
    }

    pub(super) fn set_tones(&mut self, tones: &[String]) {
        let lines: Vec<String> = tones.iter().map(|tone| to_one_line(tone)).filter(|tone| !tone.is_empty()).collect();
        let value = if lines.is_empty() { String::from(NO_TONE) } else { lines.join(TONE_SEPARATOR) };
        put_label(&mut self.robot, TONE, &value, false);
    }

    pub(super) fn has_tone(&self) -> bool {
        find_label(&self.robot, TONE).is_some()
    }

    pub(crate) fn tones(&self) -> Vec<&str> {
        let Some(value) = value_of(&self.robot, TONE).filter(|value| *value != NO_TONE) else {
            return Vec::new();
        };
        value.split(TONE_SEPARATOR).map(str::trim).filter(|tone| !tone.is_empty()).collect()
    }

    pub(super) fn save(&mut self, content: &str, is_core: bool, replaces: Option<&str>, day: &str) -> String {
        let fact = to_one_line(content);
        let needle = replaces.map(to_folded).filter(|needle| !needle.is_empty());
        let is_match = |line: &String| needle.as_ref().is_some_and(|needle| to_folded(line).contains(needle));
        let match_count = self.core.iter().filter(|line| is_match(line)).count()
            + self.knowledge.iter().filter(|fact| is_match(&fact.1)).count();
        if match_count > 1 {
            return format!(
                "Not kept: {match_count} entries hold the text it replaces; give text only the old entry holds."
            );
        }
        self.core.retain(|line| !is_match(line));
        self.knowledge.retain(|fact| !is_match(&fact.1));
        if is_core {
            self.core.push(fact);
        } else {
            self.knowledge.push((day.into(), fact));
        }
        let answer = if is_core { "Kept among what always holds" } else { "Kept among what you learned" };
        match (replaces.is_some(), match_count) {
            (false, _) => format!("{answer}."),
            (true, 1) => format!("{answer}, replacing 1 entry."),
            (true, _) => format!("{answer}, replacing nothing: no entry holds that text."),
        }
    }

    /// Replaces core and knowledge by a recollection of `previous`, with lines added since it behind and lines gone since it left out.
    pub fn apply_recollection(
        &mut self,
        previous: &Memory,
        mut core: Vec<String>,
        mut knowledge: Vec<(String, String)>,
    ) {
        core.retain(|line| !previous.core.contains(line) || self.core.contains(line));
        knowledge.retain(|fact| !previous.knowledge.contains(fact) || self.knowledge.contains(fact));
        core.extend(self.core.iter().filter(|line| !previous.core.contains(line)).cloned());
        knowledge.extend(self.knowledge.iter().filter(|fact| !previous.knowledge.contains(fact)).cloned());
        self.core = core;
        self.knowledge = knowledge;
    }

    // Every fact as its file keeps it: the profile's and those that always hold, then the learned ones, the newest first.
    pub(super) fn facts(&self) -> Vec<String> {
        let learned = self.knowledge.iter().rev().map(|(day, line)| join_day(day, line));
        self.you.iter().chain(&self.core).cloned().chain(learned).collect()
    }

    // The facts that hold `text`, whatever its letters' case and blanks.
    pub(super) fn holding(&self, text: &str) -> Vec<String> {
        let needle = to_folded(text);
        self.facts().into_iter().filter(|fact| !needle.is_empty() && to_folded(fact).contains(&needle)).collect()
    }

    /// Forgets the fact its file keeps as `line`; true where it held one.
    pub fn forget(&mut self, line: &str) -> bool {
        let count = self.you.len() + self.core.len() + self.knowledge.len();
        self.you.retain(|kept| kept != line);
        self.core.retain(|kept| kept != line);
        self.knowledge.retain(|(day, kept)| join_day(day, kept) != line);
        count != self.you.len() + self.core.len() + self.knowledge.len()
    }
}

// The `yyyymmdd` a fact was learned on; none for one that always holds.
pub(crate) fn fact_day(line: &str) -> Option<String> {
    let (day, _) = line.split_once(": ")?;
    parse_iso_day(day).map(|moment| moment.to_compact_date())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn files_round_trip() {
        let mut memory = Memory::default();
        memory.set_profile(Area::Name, &["Anna".into()], false);
        memory.set_profile(Area::Movies, &["Alien".into(), " Heat ".into()], false);
        memory.set_profile(Area::Age, &[], true);
        memory.set_robot_name(Some("Rabbit"), Some("Anna"), "Friday 16 October 2026").unwrap();
        memory.set_tones(&["witty".into(), "warm".into()]);
        memory.save("Likes tea.", false, None, "2026-10-16");
        memory.save("Lives in Vienna.", true, None, "2026-10-16");
        assert_eq!(Memory::parse(&memory.to_core_text(), &memory.to_knowledge_text()), memory);
        assert_eq!(
            format!("{memory:?}"),
            "Memory { you: 3, robot: 2, core: 1, knowledge: 1 }",
            "formatted, it tells how many facts it holds and none of them"
        );
        assert_eq!(memory.robot_name(), Some("Rabbit"));
        assert_eq!(memory.user_name(), Some("Anna"));
        assert_eq!(memory.tones(), ["witty", "warm"]);
        assert_eq!(memory.profile_of(Area::Movies), Some(ProfileEntry { is_declined: false, pick_count: 2 }));
        assert_eq!(memory.profile_of(Area::Age), Some(ProfileEntry { is_declined: true, pick_count: 0 }));
        assert_eq!(memory.profile_of(Area::Games), None);
    }

    #[test]
    fn empty_digest() {
        assert_eq!(Memory::default().to_digest(), "Nothing yet.\n");
        assert_eq!(Memory::parse("", ""), Memory::default());
    }

    #[test]
    fn unwritable_names() {
        let mut memory = Memory::default();
        assert_eq!(
            memory.set_robot_name(Some("A name far longer than the bar"), None, "d"),
            Err("longer than the 21 characters the screen has room for".into())
        );
        assert_eq!(
            memory.set_robot_name(Some("R2\u{1F916}"), None, "d"),
            Err("has a character the screen cannot write".into())
        );
        assert!(!memory.has_been_named());
        memory.set_robot_name(None, None, "d").unwrap();
        assert!(memory.has_been_named() && memory.robot_name().is_none());
    }

    #[test]
    fn save_replaces_one_match() {
        let mut memory = Memory::default();
        memory.save("Lives in Vienna.", true, None, "2026-10-16");
        memory.save("Likes tea.", false, None, "2026-10-16");
        assert_eq!(
            memory.save("Lives in Graz.", true, Some("in vienna"), "2026-10-17"),
            "Kept among what always holds, replacing 1 entry."
        );
        assert_eq!(memory.core, ["Lives in Graz."]);
        assert_eq!(
            memory.save("Likes coffee.", false, Some("nothing like this"), "2026-10-17"),
            "Kept among what you learned, replacing nothing: no entry holds that text."
        );
        assert_eq!(
            memory.save("x", false, Some("likes"), "d"),
            "Not kept: 2 entries hold the text it replaces; give text only the old entry holds."
        );
    }

    #[test]
    fn recollection_keeps_newer_lines() {
        let previous =
            Memory { core: vec!["a".into()], knowledge: vec![("d".into(), "b".into())], ..Memory::default() };
        let mut now = previous.clone();
        now.core.push("new".into());
        now.apply_recollection(&previous, vec!["settled".into()], Vec::new());
        assert_eq!(now.core, ["settled", "new"]);
        assert!(now.knowledge.is_empty());
        let mut forgetting = previous.clone();
        assert!(forgetting.forget("d: b") && !forgetting.forget("d: b"));
        forgetting.apply_recollection(&previous, vec!["a".into()], vec![("d".into(), "b".into())]);
        assert_eq!(
            (forgetting.core, forgetting.knowledge),
            (vec![String::from("a")], Vec::new()),
            "a fact forgotten while the day settles stays forgotten"
        );
    }

    #[test]
    fn facts_listed_and_forgotten() {
        let mut memory = Memory::default();
        memory.set_profile(Area::Name, &["Anna".into()], false);
        memory.set_robot_name(Some("Pixel"), None, "d").unwrap();
        memory.save("Lives in Vienna.", true, None, "2026-10-16");
        memory.save("Likes tea.", false, None, "2026-10-16");
        memory.save("Likes rain.", false, None, "2026-10-17");
        assert_eq!(
            memory.facts(),
            ["**Name:** Anna", "Lives in Vienna.", "2026-10-17: Likes rain.", "2026-10-16: Likes tea."],
            "the robot's own name is no fact of the user's"
        );
        assert_eq!(memory.holding(" LIKES "), ["2026-10-17: Likes rain.", "2026-10-16: Likes tea."]);
        assert!(memory.holding(" ").is_empty());
        assert_eq!(
            [fact_day("2026-10-16: Likes tea."), fact_day("Lives in Vienna."), fact_day("**Name:** Anna")],
            [Some("20261016".into()), None, None]
        );
        assert!(memory.forget("**Name:** Anna") && memory.forget("Lives in Vienna."));
        assert!(memory.forget("2026-10-16: Likes tea.") && !memory.forget("Likes rain."));
        assert_eq!(memory.facts(), ["2026-10-17: Likes rain."]);
        assert_eq!(memory.user_name(), None);
    }

    #[test]
    fn line_splits_at_real_day() {
        assert_eq!(split_day("2026-10-16: Likes tea.".into()), ("2026-10-16".into(), "Likes tea.".into()));
        assert_eq!(split_day("Anna-Maria: friend".into()), (String::new(), "Anna-Maria: friend".into()));
        assert_eq!((join_day("", "x"), join_day("2026-10-16", "x")), ("x".into(), "2026-10-16: x".into()));
    }
}

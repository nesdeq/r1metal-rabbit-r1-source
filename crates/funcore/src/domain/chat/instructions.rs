//! The instructions of the model and the voice.

use super::memory::Memory;
use super::onboarding::Plan;
use super::tools::to_told;
use crate::base::text::{AND, to_listing};
use crate::base::time::stamp::parse_compact_day;
use crate::domain::settings::{Tool, Tools};
use crate::openai::models::chat::CACHE_DURATION;
use crate::openai::request::with_modules;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::time::Duration;

const PERSONA: &str = include_str!("../../../assets/chat/robot.txt");
const TOLD: &str = include_str!("../../../assets/chat/tools.txt");
const VOICE: &str = include_str!("../../../assets/chat/voice.txt");
const SECTION_MARK: &str = "# ";
const OFF_SECTION: &str = "off";
const PLANNED_SECTION: &str = "planned";

/// The model's last instructions, rebuilt once the service's cache drops them or they change.
pub struct Instructions {
    pub text: String,
    tools: Tools,
    last_use: Duration,
    is_stale: bool,
}

impl Instructions {
    /// Instructions told with `tools`, first used at `now`.
    pub fn new(text: String, tools: Tools, now: Duration) -> Instructions {
        Instructions { text, tools, last_use: now, is_stale: false }
    }

    fn is_warm(&self, now: Duration) -> bool {
        !self.is_stale && now.saturating_sub(self.last_use) < CACHE_DURATION
    }

    /// Notes that the model was asked with them at `now`.
    pub fn mark_used(&mut self, now: Duration) {
        self.last_use = now;
    }
}

/// The instructions' text while it still serves at `now` with `tools`.
pub fn warm_text(instructions: &Option<Instructions>, now: Duration, tools: Tools) -> Option<&str> {
    instructions
        .as_ref()
        .filter(|instructions| instructions.is_warm(now) && instructions.tools == tools)
        .map(|instructions| instructions.text.as_str())
}

fn told_section(name: &str) -> &'static str {
    let mut sections = TOLD.split(SECTION_MARK).filter_map(|section| section.split_once('\n'));
    sections.find(|section| section.0.trim() == name).map_or("", |section| section.1.trim())
}

// What the model is told of the tools: each that is on, those that are off, and what the device lacks.
fn to_tools_text(tools: Tools) -> String {
    let mut parts: Vec<String> =
        Tool::ALL.iter().filter(|tool| tools.is_on(**tool)).map(|tool| told_section(tool.to_word()).into()).collect();
    let off: Vec<&str> = Tool::ALL.iter().filter(|tool| !tools.is_on(**tool)).map(|tool| to_told(*tool)).collect();
    if !off.is_empty() {
        parts.push(told_section(OFF_SECTION).replace("{OFF}", &to_listing(&off, AND)));
    }
    parts.push(told_section(PLANNED_SECTION).into());
    parts.join("\n\n")
}

/// Marks the instructions for a rebuild.
pub fn mark_stale(instructions: &mut Option<Instructions>) {
    if let Some(instructions) = instructions {
        instructions.is_stale = true;
    }
}

fn to_name_line(memory: &Memory) -> String {
    match memory.robot_name() {
        Some(name) => format!("Your name is {name}: the user gave it to you, or let you choose it."),
        None if memory.has_been_named() => String::from("You have no name, as the user wished."),
        None => String::from("You have no name yet."),
    }
}

fn to_tone_line(memory: &Memory) -> String {
    let tones = memory.tones();
    if tones.is_empty() {
        return String::new();
    }
    format!("\n\nTone: the user wants you to sound {}.", to_listing(&tones, AND))
}

fn to_recollection(day: &str, text: &str) -> String {
    let date = parse_compact_day(day).map_or_else(|| day.into(), |moment| moment.to_date_words(true, true));
    format!("## {date}\n\n{}", text.trim())
}

/// The instructions of every turn, with the tools that are on.
pub fn to_instructions(memory: &Memory, onboarding: Option<&Plan>, days: &[(String, String)], tools: Tools) -> String {
    let persona = PERSONA
        .trim()
        .replace("{NAME}", &to_name_line(memory))
        .replace("{TOOLS}", &to_tools_text(tools))
        .replace("{TONE}", &to_tone_line(memory));
    let mut text = with_modules(&persona);
    if let Some(plan) = onboarding {
        let _ = write!(text, "\n\n{}", plan.to_instructions());
    }
    let _ = write!(text, "\n\n# What you remember\n\n{}", memory.to_digest().trim());
    let recollections: Vec<String> = days
        .iter()
        .filter(|day| !day.1.trim().is_empty())
        .map(|(day, recollection)| to_recollection(day, recollection))
        .collect();
    if !recollections.is_empty() {
        let _ = write!(text, "\n\n# The last days\n\n{}", recollections.join("\n\n"));
    }
    text
}

/// The voice's instructions: each passage as written, the robot's tone in its delivery alone.
pub fn to_voice_instructions(memory: &Memory) -> String {
    let tones = memory.tones();
    if tones.is_empty() {
        return VOICE.trim().into();
    }
    format!(
        "{}\n\nThe user wants the robot to sound {}: let that colour how you speak, never what you say.",
        VOICE.trim(),
        to_listing(&tones, AND)
    )
}

/// The speech model's prompt: the two names, for their spelling.
pub fn to_speech_prompt(memory: &Memory) -> String {
    let names: Vec<&str> = memory.user_name().into_iter().chain(memory.robot_name()).collect();
    names.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::chat::memory::Area;
    use crate::domain::chat::onboarding::SEED_BYTES;

    #[test]
    fn instruction_sections() {
        let plan = Plan::draw(&[3; SEED_BYTES]);
        let mut memory = Memory::default();
        let tools = Tools::default();
        let first = to_instructions(&memory, Some(&plan), &[], tools);
        assert!(first.starts_with("You are the robot of an R1, a small pocket device"));
        assert!(first.contains("You have no name yet.") && first.contains("Nothing yet."));
        memory.set_robot_name(Some("Pixel"), None, "d").unwrap();
        memory.set_tones(&["witty".into(), "dry".into()]);
        let days = [("20261016".into(), " Rain. ".into()), ("x".into(), " ".into())];
        let later = to_instructions(&memory, None, &days, tools);
        assert!(later.contains("Your name is Pixel"));
        assert!(later.contains("Tone: the user wants you to sound witty and dry."));
        assert!(later.contains("# The last days\n\n## Friday 16 October 2026\n\nRain."));
        memory.set_robot_name(None, None, "d").unwrap();
        assert!(to_instructions(&memory, None, &[], tools).contains("You have no name, as the user wished."));
    }

    #[test]
    fn tools_told() {
        let text = to_tools_text(Tools::default());
        assert!(text.starts_with("Web search: search for anything current"), "{text}");
        assert!(
            text.contains("\n\nThe device: use_device")
                && !text.contains("read_signal")
                && text.contains("you do not have them now: Signal. When")
                && text.ends_with("updates over WiFi and Bluetooth."),
            "{text}"
        );
        let few = Tools::default().with(Tool::Web, false).with(Tool::Device, false);
        assert!(to_tools_text(few).contains("you do not have them now: the web search, the device and Signal."));
        let all = to_tools_text(Tools::default().with(Tool::Signal, true));
        assert!(all.contains("Signal: read_signal") && !all.contains("Switched off"));
        assert!(!to_instructions(&Memory::default(), None, &[], Tools::default()).contains("{TOOLS}"));
    }

    #[test]
    fn voice_tone() {
        let mut memory = Memory::default();
        assert_eq!(to_voice_instructions(&memory), VOICE.trim());
        memory.set_tones(&["calm".into()]);
        assert!(
            to_voice_instructions(&memory).ends_with(
                "The user wants the robot to sound calm: let that colour how you speak, never what you say."
            )
        );
    }

    #[test]
    fn speech_prompt_names() {
        let mut memory = Memory::default();
        memory.set_profile(Area::Name, &["Anna".into()], false);
        assert_eq!(to_speech_prompt(&memory), "Anna");
        memory.set_robot_name(Some("Pixel"), None, "d").unwrap();
        assert_eq!(to_speech_prompt(&memory), "Anna, Pixel");
        assert_eq!(to_speech_prompt(&Memory::default()), "");
    }

    #[test]
    fn warm_until_stale_or_expired() {
        let mut instructions = Instructions::new("x".into(), Tools::default(), Duration::ZERO);
        assert!(instructions.is_warm(CACHE_DURATION - Duration::from_secs(1)) && !instructions.is_warm(CACHE_DURATION));
        instructions.mark_used(CACHE_DURATION);
        assert!(instructions.is_warm(CACHE_DURATION));
        instructions.is_stale = true;
        assert!(!instructions.is_warm(CACHE_DURATION));
    }

    #[test]
    fn warm_text_until_stale() {
        let tools = Tools::default();
        let mut instructions = Some(Instructions::new("x".into(), tools, Duration::ZERO));
        assert_eq!(warm_text(&instructions, Duration::ZERO, tools), Some("x"));
        assert_eq!(warm_text(&instructions, Duration::ZERO, tools.with(Tool::Web, false)), None, "a tool switched");
        mark_stale(&mut instructions);
        assert_eq!(warm_text(&instructions, Duration::ZERO, tools), None);
        mark_stale(&mut None);
    }
}

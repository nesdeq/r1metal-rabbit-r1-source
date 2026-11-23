//! The answers to function calls the memory takes, and their effects on a turn.

use super::memory::Memory;
use super::onboarding::Progress;
use super::state::Lapse;
use super::tools::{Act, Asked, DONE, MemoryCall, ReadTarget, ToolCall, Words};
use super::transcript::{EntryKind, to_source_line};
use crate::base::time::Moment;
use crate::base::time::stamp::is_compact_day;
use crate::domain::settings::{Setting, Settings, Tool, Value};
use crate::domain::signal::robot::{Question, Reply};
use crate::openai::chat::answer::{Answer, Call};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const THREAD: &str = "thread";
const FORGET_OPENED: &str =
    "It opens on its screen once your reply is over, and only the user's click there forgets it: say so.";
const NO_FACT: &str = "No fact you keep holds that text.";
const STAGED: &str =
    "It waits on its line for the user's click once your reply is over: say so, and never that it is set.";

/// What a call has its turn do before the model is answered.
#[derive(Clone, PartialEq, Eq)]
pub enum Step<'a> {
    // What is kept is searched, the notes and translations with it where the robot may read them.
    Search { query: &'a str, has_papers: bool },
    Read { target: ReadTarget, day: Option<&'a str> },
    // The main loop is asked, and `to_asked_answer` makes the answer of its reply.
    Ask(Asked),
    EndOnboarding,
    // The facts and the days are read, and `to_forget_answer` makes the answer of them.
    Forget(&'a str),
    // The memory takes it (`apply_to_memory`).
    Remember(&'a MemoryCall),
    // Answered as it stands, the device acting once the reply is over.
    Answer(&'static str, Act),
}

// As its call: what is searched for and what is forgotten are told by no log.
impl core::fmt::Debug for Step<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Step::Search { .. } => "Search",
            Step::Read { .. } => "Read",
            Step::Ask(_) => "Ask",
            Step::EndOnboarding => "EndOnboarding",
            Step::Forget(_) => "Forget",
            Step::Remember(_) => "Remember",
            Step::Answer(..) => "Answer",
        })
    }
}

/// The step of a call, under what the owner has set.
pub fn to_step<'a>(call: &'a ToolCall, settings: &Settings) -> Step<'a> {
    match call {
        ToolCall::Search(query) => Step::Search { query, has_papers: settings.tools.is_on(Tool::Notes) },
        ToolCall::Read { target, day } => Step::Read { target: *target, day: day.as_deref() },
        ToolCall::ReadSignal(name) => Step::Ask(Asked::Signal(Question::Read(name.clone()))),
        ToolCall::DraftSignal { conversation, .. } => Step::Ask(Asked::Signal(Question::Draft(conversation.clone()))),
        ToolCall::Bell(change) => Step::Ask(Asked::Bell(change.clone())),
        ToolCall::EndOnboarding => Step::EndOnboarding,
        ToolCall::Forget(which) => Step::Forget(which),
        ToolCall::Device(act) => Step::Answer(DONE, act.clone()),
        ToolCall::Change(setting, value) => {
            Step::Answer(to_change_answer(settings, *setting, *value), Act::Change(*setting, *value))
        }
        ToolCall::Memory(call) => Step::Remember(call),
    }
}

/// The answer to a call the main loop was asked for, with the draft it opens where Signal found its conversation.
pub fn to_asked_answer(call: &ToolCall, reply: Reply) -> (String, Option<Act>) {
    let draft = match call {
        ToolCall::DraftSignal { text, .. } => reply.draft.map(|peer| Act::Draft(peer, Words(text.clone()))),
        _ => None,
    };
    (reply.told, draft)
}

/// The answer to a forget of `which`, with the act that opens the line where a click forgets it.
pub fn to_forget_answer(which: &str, memory: &Memory, days: &[String]) -> (String, Option<Act>) {
    match to_lapse(which, memory, days) {
        Ok(lapse) => (FORGET_OPENED.into(), Some(Act::Forget(lapse))),
        Err(answer) => (answer, None),
    }
}

// What a forget's `which` names: the thread, a day of the conversation's `days`, or the one fact that holds it; else why none.
fn to_lapse(which: &str, memory: &Memory, days: &[String]) -> Result<Lapse, String> {
    if which.eq_ignore_ascii_case(THREAD) {
        return Ok(Lapse::Thread);
    }
    if is_compact_day(which) {
        return days
            .iter()
            .any(|day| day == which)
            .then(|| Lapse::Day(which.into()))
            .ok_or_else(|| format!("No words of {which} are kept."));
    }
    let mut facts = memory.holding(which);
    match facts.len() {
        1 => Ok(Lapse::Fact(facts.remove(0))),
        0 => Err(NO_FACT.into()),
        count => Err(format!("{count} facts hold that text; give text only one of them holds.")),
    }
}

// The answer to a change of a setting: made once the reply is over, but staged where it leaves the device less safe or less private.
fn to_change_answer(settings: &Settings, setting: Setting, value: Value) -> &'static str {
    if settings.is_eased(setting, &settings.with_value(setting, value)) { DONE } else { STAGED }
}

/// Applies a call to the memory and returns the model's answer.
pub fn apply_to_memory(call: &MemoryCall, memory: &mut Memory, today: &Moment) -> String {
    match call {
        MemoryCall::Save { content, is_core, replaces } => {
            memory.save(content, *is_core, replaces.as_deref(), &today.to_iso_date())
        }
        MemoryCall::Profile { area, entries, is_declined } => {
            memory.set_profile(*area, entries, *is_declined);
            Progress::from_memory(memory).to_answer()
        }
        MemoryCall::Name(name) => {
            let author = memory.user_name().map(str::to_string);
            let day = today.to_date_words(true, true);
            let result = memory.set_robot_name(name.as_deref(), author.as_deref(), &day);
            match (result, memory.robot_name()) {
                (Ok(()), Some(name)) => format!("Taken: your name is {name}."),
                (Ok(()), None) => String::from("Taken: you have no name."),
                (Err(reason), _) => format!("Not taken: the name {reason}. Ask for another, or offer one that fits."),
            }
        }
        MemoryCall::Tone(tones) => {
            memory.set_tones(tones);
            String::from("Taken.")
        }
    }
}

/// True once the first conversation knows enough.
pub fn knows_enough(memory: &Memory) -> bool {
    Progress::from_memory(memory).is_enough
}

/// The effects of a call the memory answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Effects {
    /// The instructions are rebuilt.
    pub is_stale: bool,
    /// The robot's name changed.
    pub is_renamed: bool,
    /// The robot got a name.
    pub is_named: bool,
    /// The first conversation ended.
    pub is_onboarded: bool,
}

/// The effects of that call, given the robot's name before and after it.
pub fn effects_of(
    call: &MemoryCall,
    old_name: Option<&str>,
    new_name: Option<&str>,
    has_enough: bool,
    is_meeting: bool,
) -> Effects {
    let is_onboarded = has_enough && is_meeting;
    let is_renamed = old_name != new_name;
    Effects {
        is_stale: is_onboarded || matches!(call, MemoryCall::Name(_) | MemoryCall::Tone(_)),
        is_renamed,
        is_named: is_renamed && new_name.is_some(),
        is_onboarded,
    }
}

/// The entries an answer writes into the day's file, in order.
pub fn to_entries(answer: &Answer) -> Vec<(EntryKind, String)> {
    let (terms, text) = (answer.search_terms(), answer.text());
    let sources: Vec<String> =
        answer.sources().iter().map(|source| to_source_line(&source.title, &source.url)).collect();
    let mut entries = Vec::new();
    if !terms.is_empty() {
        entries.push((EntryKind::Searches, terms.join("\n")));
    }
    if !text.is_empty() {
        entries.push((EntryKind::Robot, text));
    }
    if !sources.is_empty() {
        entries.push((EntryKind::Sources, sources.join("\n")));
    }
    entries
}

pub(super) fn continues_turn(answer: &Answer, calls: &[Call], needs_answer: bool) -> bool {
    needs_answer || (!calls.is_empty() && answer.text().is_empty())
}

/// The log line of an answer the model did not finish.
pub fn to_cut_line(answer: &Answer) -> Option<String> {
    answer.cut_reason().map(|reason| format!("chat: the answer was cut short: {reason}"))
}

/// The log line of a function call.
pub fn to_called_line(name: &str) -> String {
    format!("chat: the model called {name}")
}

/// The log line of a call of an unknown function or with foreign arguments.
pub fn to_unknown_call_line(name: &str) -> String {
    format!("chat: the model called {name} with arguments that are not its own")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::chat::memory::Area;
    use crate::domain::chat::tools::Readout;
    use crate::domain::settings::Tools;
    use crate::signal::address::Peer;
    use alloc::vec;

    const NOW: u64 = 1_792_159_320;

    #[test]
    fn call_steps() {
        let settings = Settings::default();
        let without_notes = Settings { tools: Tools::default().with(Tool::Notes, false), ..settings };
        let search = ToolCall::Search("tea".into());
        assert_eq!(to_step(&search, &settings), Step::Search { query: "tea", has_papers: true });
        assert_eq!(
            to_step(&search, &without_notes),
            Step::Search { query: "tea", has_papers: false },
            "the notes are searched only while the robot may read them"
        );
        let read = ToolCall::Read { target: ReadTarget::Readout(Readout::Bell), day: Some("20261016".into()) };
        assert_eq!(
            to_step(&read, &settings),
            Step::Read { target: ReadTarget::Readout(Readout::Bell), day: Some("20261016") }
        );
        let draft = ToolCall::DraftSignal { conversation: "Anna".into(), text: "Lunch?".into() };
        assert_eq!(to_step(&draft, &settings), Step::Ask(Asked::Signal(Question::Draft("Anna".into()))));
        assert_eq!(to_step(&ToolCall::ReadSignal(None), &settings), Step::Ask(Asked::Signal(Question::Read(None))));
        let cancel = crate::domain::bell::Change::Cancel("all".into());
        assert_eq!(
            to_step(&ToolCall::Bell(cancel.clone()), &settings),
            Step::Ask(Asked::Bell(cancel)),
            "the interface holds the bell, and answers what it set or cancelled"
        );
        assert_eq!(to_step(&ToolCall::EndOnboarding, &settings), Step::EndOnboarding);
        assert_eq!(to_step(&ToolCall::Forget("thread".into()), &settings), Step::Forget("thread"));
        assert_eq!(
            [format!("{:?}", to_step(&search, &settings)), format!("{:?}", to_step(&draft, &settings))],
            ["Search", "Ask"],
            "formatted, a step tells its kind alone"
        );
        let tone = MemoryCall::Tone(Vec::new());
        assert_eq!(to_step(&ToolCall::Memory(tone.clone()), &settings), Step::Remember(&tone));
        assert_eq!(to_step(&ToolCall::Device(Act::Home), &settings), Step::Answer(DONE, Act::Home));
        let (limit, shown) = ((Setting::Limit, Value::Step(None)), (Setting::Drive, Value::Switch(true)));
        let changes = [limit, shown].map(|(setting, value)| ToolCall::Change(setting, value));
        assert_eq!(
            [to_step(&changes[0], &settings), to_step(&changes[1], &settings)],
            [Step::Answer(DONE, Act::Change(limit.0, limit.1)), Step::Answer(STAGED, Act::Change(shown.0, shown.1))],
            "a change acts either way, and its answer says whether it waits for a click"
        );
    }

    #[test]
    fn asked_and_forget_answers() {
        let peer = Peer::Group([1; 32]);
        let found = || Reply { told: "Drafted.".into(), draft: Some(peer) };
        let draft = ToolCall::DraftSignal { conversation: "Anna".into(), text: "Lunch?".into() };
        assert_eq!(
            to_asked_answer(&draft, found()),
            ("Drafted.".into(), Some(Act::Draft(peer, Words("Lunch?".into())))),
            "the draft opens in the conversation Signal found"
        );
        assert_eq!(
            to_asked_answer(&draft, Reply { told: "Nobody of that name.".into(), draft: None }),
            ("Nobody of that name.".into(), None)
        );
        assert_eq!(
            to_asked_answer(&ToolCall::ReadSignal(None), found()),
            ("Drafted.".into(), None),
            "a reading drafts nothing"
        );
        let memory = Memory::default();
        assert_eq!(to_forget_answer("thread", &memory, &[]), (FORGET_OPENED.into(), Some(Act::Forget(Lapse::Thread))));
        assert_eq!(to_forget_answer("coffee", &memory, &[]), (NO_FACT.into(), None), "nothing named, nothing opens");
    }

    #[test]
    fn memory_answers() {
        let today = Moment::from_seconds(NOW);
        let mut memory = Memory::default();
        let profile = MemoryCall::Profile { area: Area::Name, entries: vec!["Anna".into()], is_declined: false };
        assert!(apply_to_memory(&profile, &mut memory, &today).starts_with("Noted. Known: Name."));
        let name = MemoryCall::Name(Some("Pixel".into()));
        assert_eq!(apply_to_memory(&name, &mut memory, &today), "Taken: your name is Pixel.");
        assert_eq!(memory.robot_name(), Some("Pixel"));
        assert!(memory.to_core_text().contains("**Name:** Pixel, given by Anna, on Friday 16 October 2026"));
        assert_eq!(apply_to_memory(&MemoryCall::Name(None), &mut memory, &today), "Taken: you have no name.");
        let long_name = MemoryCall::Name(Some("A name far longer than the bar".into()));
        assert_eq!(
            apply_to_memory(&long_name, &mut memory, &today),
            "Not taken: the name longer than the 21 characters the screen has room for. Ask for another, or offer one that fits."
        );
        let save = MemoryCall::Save { content: "Tea.".into(), is_core: true, replaces: None };
        assert_eq!(apply_to_memory(&save, &mut memory, &today), "Kept among what always holds.");
        assert_eq!(apply_to_memory(&MemoryCall::Tone(vec!["calm".into()]), &mut memory, &today), "Taken.");
        assert_eq!(memory.tones(), ["calm"]);
    }

    #[test]
    fn call_effects() {
        let save = MemoryCall::Save { content: "x".into(), is_core: false, replaces: None };
        let table = [
            ((&save, None, None, false, true), Effects::default()),
            (
                (&MemoryCall::Name(Some("P".into())), None, Some("P"), false, false),
                Effects { is_stale: true, is_renamed: true, is_named: true, is_onboarded: false },
            ),
            (
                (&MemoryCall::Name(None), Some("P"), None, false, false),
                Effects { is_stale: true, is_renamed: true, is_named: false, is_onboarded: false },
            ),
            ((&save, None, None, true, true), Effects { is_stale: true, is_onboarded: true, ..Effects::default() }),
            (
                (&MemoryCall::Tone(Vec::new()), None, None, true, false),
                Effects { is_stale: true, ..Effects::default() },
            ),
        ];
        for ((call, old_name, new_name, has_enough, is_meeting), expected) in table {
            assert_eq!(effects_of(call, old_name, new_name, has_enough, is_meeting), expected, "{call:?}");
        }
    }

    #[test]
    fn forget_names_one() {
        let mut memory = Memory::default();
        memory.save("Likes tea.", false, None, "2026-10-16");
        memory.save("Likes rain.", false, None, "2026-10-17");
        let days = [String::from("20261016")];
        assert_eq!(to_lapse("Thread", &memory, &days), Ok(Lapse::Thread));
        assert_eq!(to_lapse("20261016", &memory, &days), Ok(Lapse::Day("20261016".into())));
        assert_eq!(to_lapse("20261017", &memory, &days), Err("No words of 20261017 are kept.".into()));
        assert_eq!(to_lapse("tea", &memory, &days), Ok(Lapse::Fact("2026-10-16: Likes tea.".into())));
        assert_eq!(
            to_lapse("likes", &memory, &days),
            Err("2 facts hold that text; give text only one of them holds.".into())
        );
        assert_eq!(to_lapse("coffee", &memory, &days), Err(NO_FACT.into()));
    }

    #[test]
    fn change_made_or_staged() {
        let settings = Settings::default();
        assert_eq!(to_change_answer(&settings, Setting::Limit, Value::Step(None)), DONE);
        assert_eq!(to_change_answer(&settings, Setting::Tool(Tool::Signal), Value::Switch(true)), STAGED);
    }

    #[test]
    fn answer_entries() {
        let search = r#"{"type":"web_search_call","status":"completed","action":{"type":"search","query":"Wetter Wien","sources":[{"type":"url","url":"https://wetter.at/wien"}]}}"#;
        let message = r#"{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Regen.","annotations":[]}]}"#;
        let answer = |output: &[&str]| Answer {
            output: output.iter().map(|item| (*item).into()).collect(),
            ..Answer::default()
        };
        assert_eq!(
            to_entries(&answer(&[search, message])),
            [
                (EntryKind::Searches, "Wetter Wien".into()),
                (EntryKind::Robot, "Regen.".into()),
                (EntryKind::Sources, to_source_line("", "https://wetter.at/wien")),
            ],
            "what was searched for, the words, then the pages they rest on"
        );
        assert_eq!(to_entries(&answer(&[message])), [(EntryKind::Robot, "Regen.".into())], "words alone");
        assert!(to_entries(&Answer::default()).is_empty(), "an answer of calls alone writes nothing");
    }

    #[test]
    fn call_lines() {
        assert_eq!(to_called_line("x"), "chat: the model called x");
        assert_eq!(to_unknown_call_line("x"), "chat: the model called x with arguments that are not its own");
        let cut = Answer { status: "incomplete".into(), reason: Some("max_output_tokens".into()), ..Answer::default() };
        assert_eq!(to_cut_line(&cut).as_deref(), Some("chat: the answer was cut short: max_output_tokens"));
    }
}

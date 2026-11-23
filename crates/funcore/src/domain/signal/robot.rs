//! What the robot asks of Signal and is told, of the device as it is and without the network.

use super::auto::CONTEXT_MESSAGES;
use super::list::{entry_words, last_of, to_summaries};
use crate::base::time::offset::to_local;
use crate::base::time::{MILLIS_PER_SECOND, Moment};
use crate::signal::address::{Peer, ServiceId};
use crate::signal::content::Placeholder;
use crate::signal::conversation::Entry;
use crate::signal::device::{Device, UNNAMED};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const NOT_LINKED: &str = "Signal is not linked on this device: the user links it on Signal's screen.";
const ENDED: &str = "Signal does not run on this device until it starts again.";
const NO_CONVERSATIONS: &str = "The user's Signal holds no conversation yet.";
const NONE_NAMED: &str = "No conversation or contact of the user's Signal is named so.";
const DRAFTED: &str =
    "The conversation opens with the draft in it once your reply is over; it goes only when the user clicks it.";
const THE_USER: &str = "the user";
const IN_AUTO_MODE: &str = "you in auto mode";

/// What the robot asks of Signal: the conversations, or the last messages of one by its name; or the conversation a draft goes to, by its name.
#[derive(Clone, PartialEq, Eq)]
pub enum Question {
    Read(Option<String>),
    Draft(String),
}

impl core::fmt::Debug for Question {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Question::Read(None) => "Read",
            Question::Read(Some(_)) => "ReadConversation",
            Question::Draft(_) => "Draft",
        })
    }
}

/// What the model is told of a readout, and the conversation a draft goes to where one was found for it.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Reply {
    pub told: String,
    pub draft: Option<Peer>,
}

impl core::fmt::Debug for Reply {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Reply").field("draft", &self.draft.is_some()).finish_non_exhaustive()
    }
}

const fn carried(placeholder: Placeholder) -> &'static str {
    match placeholder {
        Placeholder::Photo => "[a photo]",
        Placeholder::Video => "[a video]",
        Placeholder::VoiceNote => "[a voice note]",
        Placeholder::File => "[a file]",
        Placeholder::Sticker => "[a sticker]",
        Placeholder::Contact => "[a shared contact]",
        Placeholder::Poll => "[a poll]",
        Placeholder::Unsupported => "[a message this device cannot show]",
    }
}

fn author<'a>(entry: &Entry, name: &'a str, writers: &'a BTreeMap<ServiceId, String>) -> &'a str {
    match (entry.is_owner, entry.is_robot, entry.sender) {
        (true, false, _) => THE_USER,
        (true, true, _) => IN_AUTO_MODE,
        (false, _, Some(sender)) => writers.get(&sender).map_or(UNNAMED, String::as_str),
        (false, _, None) => name,
    }
}

// A message with when the R1 took it in by the clock on screen, `offset` minutes from UTC, and who wrote it.
fn told_entry(entry: &Entry, author: &str, offset: i16) -> String {
    let taken = Moment::from_seconds(to_local(entry.at / MILLIS_PER_SECOND, offset));
    let stamp = format!("[{}, {}]", taken.to_date_words(true, true), taken.to_time_text());
    let mut text = String::from(entry_words(entry).trim());
    for placeholder in entry.placeholders.iter().filter(|_| entry.deleter.is_none()) {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(carried(*placeholder));
    }
    format!("{stamp} {author}: {text}")
}

// The conversation a name names among the conversations and the contacts a message may go to: the one of that name whatever its letters' case, else the one alone whose name holds it; why none, where none or several do.
fn named(device: &Device, name: &str, now_ms: u64) -> Result<(Peer, String), String> {
    let wanted = name.trim().to_lowercase();
    let summaries = to_summaries(device, now_ms);
    let mut listed: Vec<(Peer, String)> =
        summaries.into_iter().filter_map(|summary| Some((summary.peer, summary.name?))).collect();
    for (peer, name) in device.contactable(now_ms) {
        if !listed.iter().any(|(held, _)| *held == peer) {
            listed.push((peer, name));
        }
    }
    let same: Vec<&(Peer, String)> = listed.iter().filter(|(_, held)| held.to_lowercase() == wanted).collect();
    let holding: Vec<&(Peer, String)> =
        listed.iter().filter(|(_, held)| !wanted.is_empty() && held.to_lowercase().contains(&wanted)).collect();
    match (same.as_slice(), holding.as_slice()) {
        ([found], _) | ([], [found]) => Ok((*found).clone()),
        ([], []) => Err(String::from(NONE_NAMED)),
        ([], several) | (several, _) => {
            let names: Vec<&str> = several.iter().map(|(_, name)| name.as_str()).collect();
            Err(format!("{} are named so: {}. Ask the user which one is meant.", names.len(), names.join("; ")))
        }
    }
}

fn told_conversations(device: &Device, now_ms: u64, offset: i16) -> String {
    let summaries = to_summaries(device, now_ms);
    if summaries.is_empty() {
        return String::from(NO_CONVERSATIONS);
    }
    let mut told = String::from("The user's Signal conversations, the pinned first, then the newest first:\n");
    for summary in summaries {
        let name = summary.name.as_deref().unwrap_or(UNNAMED);
        let _ = write!(told, "- {name}: {} unread", summary.unread);
        if let Some(last) = &summary.last {
            let writers = device.writers_of(core::slice::from_ref(last));
            let _ = write!(told, "; the newest, {}", told_entry(last, author(last, name, &writers), offset));
        }
        told.push('\n');
    }
    told
}

fn told_conversation(device: &Device, (peer, name): (Peer, String), offset: i16) -> String {
    let last = last_of(device.entries(&peer), CONTEXT_MESSAGES);
    if last.is_empty() {
        return format!("The conversation with {name} holds no message yet.");
    }
    let writers = device.writers_of(last);
    let mut told = format!("The last {} messages of the conversation with {name}, the oldest first:\n", last.len());
    for entry in device.resolved_entries(last) {
        let _ = writeln!(told, "{}", told_entry(&entry, author(&entry, &name, &writers), offset));
    }
    told
}

// What Signal answers the robot where no device is held: that it is not linked.
pub(super) fn unlinked() -> Reply {
    Reply { told: String::from(NOT_LINKED), draft: None }
}

// What Signal answers the robot once its thread has ended: that it does not run.
pub(super) fn ended() -> Reply {
    Reply { told: String::from(ENDED), draft: None }
}

/// What Signal answers the robot of the device as it is at `now_ms`, the clock on screen `offset` minutes from UTC: what it reads, marking nothing read; or the conversation a draft goes to, found by its name; that it is not linked, where no device is held.
pub fn answer(device: Option<&Device>, question: &Question, now_ms: u64, offset: i16) -> Reply {
    let Some(device) = device else {
        return unlinked();
    };
    let told = |told: String| Reply { told, draft: None };
    match question {
        Question::Read(None) => told(told_conversations(device, now_ms, offset)),
        Question::Read(Some(name)) => match named(device, name, now_ms) {
            Ok(found) => told(told_conversation(device, found, offset)),
            Err(why) => told(why),
        },
        Question::Draft(name) => match named(device, name, now_ms) {
            Ok((peer, _)) => Reply { told: String::from(DRAFTED), draft: Some(peer) },
            Err(why) => told(why),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::tests::Standin;

    const ALICE: ServiceId = ServiceId::Aci([1; 16]);
    const BORIS: ServiceId = ServiceId::Aci([2; 16]);
    // Friday 16 October 2026, 14:02 UTC, and a minute later.
    const ASKED: u64 = 1_792_159_320_000;
    const ANSWERED: u64 = 1_792_159_380_000;

    fn with_lunch() -> Device {
        let mut device = Device::unkeyed();
        for (service, name) in [(ALICE, "Alice"), (BORIS, "Alina"), (ServiceId::Aci([5; 16]), "Alfred")] {
            device.add_named_contact(service, name, name == "Alfred");
        }
        device.compose(&Standin::default(), Peer::Contact(ALICE), ("Lunch?", &[]), ASKED, false);
        device.add_incoming(ALICE, ("Yes", &[Placeholder::Photo]), 9, ANSWERED);
        device
    }

    #[test]
    fn not_linked_and_none_named() {
        let question = Question::Read(None);
        assert_eq!(answer(None, &question, ANSWERED, 0), Reply { told: NOT_LINKED.into(), draft: None });
        let empty = Device::unkeyed();
        assert_eq!(answer(Some(&empty), &question, ANSWERED, 0).told, NO_CONVERSATIONS);
        let device = with_lunch();
        let read = |name: &str| answer(Some(&device), &Question::Read(Some(name.into())), ANSWERED, 0).told;
        assert_eq!(read("Zoe"), NONE_NAMED);
        assert_eq!(read("note to self"), "The conversation with Note to Self holds no message yet.");
        assert_eq!(read(" "), NONE_NAMED, "no words name nobody");
    }

    #[test]
    fn told_by_screen_clock() {
        let device = with_lunch();
        let listed = answer(Some(&device), &Question::Read(None), ANSWERED, 120);
        assert_eq!(
            listed.told,
            "The user's Signal conversations, the pinned first, then the newest first:\n- Alice: 1 unread; the newest, [Friday 16 October 2026, 16:03] Alice: Yes [a photo]\n",
            "by the clock on screen"
        );
        let read = answer(Some(&device), &Question::Read(Some(" ALICE ".into())), ANSWERED, 0);
        assert_eq!(
            read.told,
            "The last 2 messages of the conversation with Alice, the oldest first:\n[Friday 16 October 2026, 14:02] the user: Lunch?\n[Friday 16 October 2026, 14:03] Alice: Yes [a photo]\n"
        );
        assert_eq!(
            device.conversation(&Peer::Contact(ALICE)).map(|conversation| conversation.unread()),
            Some(1),
            "reading marks nothing read"
        );
    }

    #[test]
    fn last_messages_unmarked() {
        let (crypto, alice) = (Standin::default(), Peer::Contact(ALICE));
        let mut device = with_lunch();
        device.compose(&crypto, alice, (&format!("{}On my way", super::super::auto::MARK), &[]), ANSWERED, true);
        let read = |device: &Device| answer(Some(device), &Question::Read(Some("alice".into())), ANSWERED, 0).told;
        assert!(read(&device).ends_with("you in auto mode: On my way\n"), "auto mode's mark is not read");
        for at in 0..CONTEXT_MESSAGES as u64 {
            device.compose(&crypto, alice, ("filler", &[]), ANSWERED + at, false);
        }
        let told = read(&device);
        assert!(told.starts_with("The last 50 messages of the conversation with Alice"), "{told}");
        assert!(!told.contains("Lunch?") && !told.contains("On my way"), "the last 50 alone");
        assert_eq!(told.matches("the user: filler").count(), CONTEXT_MESSAGES);
    }

    #[test]
    fn draft_by_name() {
        let device = with_lunch();
        let draft = |name: &str| answer(Some(&device), &Question::Draft(name.into()), ANSWERED, 0);
        let several = draft("ali");
        assert_eq!(
            (several.told.as_str(), several.draft),
            ("2 are named so: Alice; Alina. Ask the user which one is meant.", None),
            "a hidden contact is none a message goes to"
        );
        assert_eq!(
            draft("alina"),
            Reply { told: DRAFTED.into(), draft: Some(Peer::Contact(BORIS)) },
            "no conversation yet"
        );
        assert_eq!(draft("ALICE").draft, Some(Peer::Contact(ALICE)), "the name whole wins over names that hold it");
        assert_eq!(draft("alfred").draft, None, "hidden");
        assert_eq!(
            [format!("{:?}", draft("alina")), format!("{:?}", Question::Draft("x".into()))],
            ["Reply { draft: true, .. }", "Draft"]
        );
    }
}

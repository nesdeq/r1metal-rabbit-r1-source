extern crate std;

use crate::base::time::Moment;
use crate::domain::bell::{self, Bell};
use crate::domain::chat::InputKind;
use crate::domain::chat::memory::Memory;
use crate::domain::chat::recollect::to_day_file;
use crate::domain::chat::thread::{RobotState, Thread};
use crate::domain::chat::transcript::{self, Entry, EntryKind};
use crate::domain::dialogue::Turn;
use crate::domain::flash::Flash;
use crate::domain::news::rundowns::Rundowns;
use crate::domain::news::silent::Silent;
use crate::domain::news::topics::Topics;
use crate::domain::paper::pages;
use crate::domain::paper::sheet::{self, Subject};
use crate::domain::place;
use crate::domain::settings::Settings;
use crate::domain::uses;
use crate::gfx::palette::{Roles, STANDARD};
use crate::gfx::theme::Theme;
use crate::hw::power::battery;
use crate::hw::power::usage::Usage;
use crate::logs::{self, Crashes, Logging, lines};
use crate::openai::chat::request::Location;
use crate::openai::key;
use crate::openai::ledger::{self, Ledger};
use crate::openai::models::Model;
use crate::signal::device::{self, Device};
use crate::storage::safe;
use crate::wifi::known::Known;
use crate::wifi::places::Places;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use std::fs;
use std::path::{Path, PathBuf};

const NOW: u64 = 1_792_159_320;
const STEM: &str = "20261016_140200";
const DAY: &str = "20261016";
const SOURCES: &str = "Sources (21/21 feeds, 681 stories): a.com, b.org";
const GIST: &str = "# Gist\n\nThe note says **two** things:\n\n- one\n- two\n";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn bytes(name: &str) -> Vec<u8> {
    fs::read(root().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn text(name: &str) -> String {
    String::from_utf8(bytes(name)).unwrap()
}

#[test]
fn settings_round_trip() {
    let file = text("settings");
    let settings = Settings::parse(&file);
    let colours = [("stage", 0), ("dial", 2), ("deck", 3), ("terminal", 15), ("companion", 14), ("rail", 2)];
    assert_eq!(
        (settings.brightness, settings.volume, settings.sounds, settings.has_robot, settings.offset),
        (70, 40, 30, false, 120)
    );
    assert_eq!((settings.logging, settings.theme), (Logging::Debug, Theme::Terminal));
    assert_eq!(settings.colours, colours.map(|colour| colour.1));
    assert_eq!((settings.signal_wake, settings.is_signal_aloud), (15, false));
    assert_eq!((settings.auto_replies, settings.auto_gap, settings.auto_hourly), (50, 120, 10));
    let tail = crate::domain::settings::tests::DEFAULT_TAIL;
    let (rings, kept) = ("alarm 1792222200\ntimer 1792189200\n", "entry clock 1792229400\nentry span 1792189200\n");
    assert_eq!(
        settings.to_text(),
        std::format!("{}{tail}", file.replace(rings, "")),
        "what the earlier firmware kept, but its alarm and timer, which are the bell's now, and behind it the settings it did not have, as a new device has them"
    );
    let (bell, is_moved) = Bell::read(None, &file);
    assert!(is_moved && bell.to_text() == kept, "the alarm by the clock it was set by, two hours ahead, and the timer");
    assert_eq!(Settings::default().to_text(), std::format!("{}{tail}", text("settings_fresh")));
    assert_eq!(
        Topics::read(None, &file),
        (Topics::default(), false),
        "a file that kept no rundown's switch leaves the two a new device has as they are"
    );
}

#[test]
fn rundowns_round_trip() {
    let file = text("rundowns");
    let kept = Topics::parse(&file);
    assert_eq!(kept.to_text(), file);
    assert_eq!(Topics::read(Some(&file), "rundown finance on\n"), (kept, false), "its own file counts");
    let (moved, is_moved) = Topics::read(None, "brightness 40\nrundown news off\nrundown finance on\n");
    assert!(is_moved, "the switches an earlier firmware kept among the settings are the rundowns' own file's now");
    assert_eq!(moved.to_text(), "rundown news off\nrundown finance on\n");
    let file = text("news/silent");
    assert_eq!(Silent::parse(&file).to_text(), file);
}

#[test]
fn bell_round_trip() {
    let file = text("bell");
    assert_eq!(Bell::parse(&file).to_text(), file);
    assert_eq!(Bell::read(Some(&file), &text("settings")), (Bell::parse(&file), false), "its own file counts");
}

#[test]
fn place_round_trip() {
    let file = text("where");
    let location =
        Location { country: "AT".into(), region: "Wien".into(), city: "Wien".into(), zone: "Europe/Vienna".into() };
    assert_eq!(place::parse(&file).as_ref(), Some(&location));
    assert_eq!(place::to_text(&location), file);
    let offset = text("place");
    assert!(!place::has_offset_changed(Some(&offset), 120));
    assert_eq!(place::to_offset_file(120), offset);
}

#[test]
fn memory_round_trip() {
    let (core, knowledge) = (text("chat/core.md"), text("chat/knowledge.md"));
    let memory = Memory::parse(&core, &knowledge);
    assert_eq!((memory.user_name(), memory.robot_name()), (Some("Anna"), Some("Pixel")));
    assert_eq!(memory.tones(), ["dry", "warm"]);
    assert_eq!(memory.core, ["Tea, no sugar."]);
    let knowledge_lines = [("2026-10-16", "Plays the cello."), ("", "Keeps bees.")];
    assert_eq!(memory.knowledge, knowledge_lines.map(|(day, line)| (String::from(day), String::from(line))));
    assert_eq!((memory.to_core_text(), memory.to_knowledge_text()), (core, knowledge));
}

#[test]
fn robot_state_and_thread_round_trip() {
    let file = text("chat/state");
    let state = RobotState::parse(&file).expect("a seed");
    assert!(state.is_onboarded && state.is_muted);
    assert_eq!(state.to_text(), file);
    let file = text("chat/thread");
    let thread = Thread::parse(&file);
    assert_eq!(thread.items.len(), 2);
    assert!(thread.items[1].contains("Hi \\\"there\\\""));
    assert!(!thread.needs_rewrite(&file));
    assert_eq!(thread.to_text(), file);
}

#[test]
fn transcript_round_trip() {
    let (file, now) = (text("chat/talk.md"), Moment::from_seconds(NOW));
    let entries = transcript::parse(&file);
    assert_eq!(
        entries,
        [
            Entry::new(EntryKind::User(InputKind::Voice), &now, "Und morgen?\n\nRegen?"),
            Entry::new(EntryKind::Searches, &now, "wetter wien"),
            Entry::new(EntryKind::Robot, &now, "Regen ab Mittag."),
            Entry::new(EntryKind::Sources, &now, "[Wetter](https://a.at)\nhttps://b.at"),
            Entry::new(EntryKind::User(InputKind::Text), &now, "Danke"),
        ]
    );
    let output: String = entries
        .iter()
        .enumerate()
        .map(|(at, entry)| transcript::to_file_text(&now, entry, Some("Pixel"), at > 0))
        .collect();
    assert_eq!(output, file);
    assert_eq!(transcript::to_file_text(&now, &entries[2], None, false), text("chat/nameless.md"));
}

#[test]
fn journal_round_trip() {
    let file = text("chat/journal.md");
    assert_eq!(to_day_file(&Moment::from_seconds(NOW), "Talked about the rain.\n"), file);
    assert_eq!(sheet::body(&file), "Talked about the rain.");
}

#[test]
fn notes_round_trip() {
    let speech = "One sentence said aloud. Another one said after it.";
    let words = text("notes/words.md");
    assert_eq!(sheet::to_words_file(STEM, 75, speech), words);
    assert_eq!(sheet::body(&words), speech);
    let turns = [
        Turn { source: "Wo ist der Bahnhof?".into(), translation: "Where is the station?".into() },
        Turn { source: "Straight ahead.".into(), translation: String::new() },
        Turn { source: String::new(), translation: "Geradeaus.".into() },
    ];
    assert_eq!(sheet::to_remarks_file(STEM, 130, &turns), text("notes/remarks.md"));
    let summary = text("notes/note_summary.md");
    assert_eq!(sheet::to_summary_file(Subject::Note(STEM, 75), GIST, Model::Luna), summary);
    assert_eq!(sheet::body(&summary), GIST.trim());
    assert_eq!(sheet::to_summary_file(Subject::Talk(STEM), GIST, Model::Luna), text("notes/talk_summary.md"));
    let file = pages::to_pages(Subject::Note(STEM, 75), GIST, Some((NOW, 120)), &Roles::at(STANDARD));
    assert!(file == bytes("notes/note_summary.pdf"), "the pages are the old pages");
}

#[test]
fn news_round_trip() {
    let news = Subject::Rundown(DAY, "news", SOURCES);
    assert_eq!(sheet::to_summary_file(news, GIST, Model::Luna), text("news/rundown.md"));
    let finance = Subject::Rundown(DAY, "finance", SOURCES);
    assert_eq!(sheet::to_summary_file(finance, GIST, Model::Luna), text("news/finance.md"));
    assert!(pages::to_pages(news, GIST, None, &Roles::at(STANDARD)) == bytes("news/rundown.pdf"));
    let file = text("news/pending");
    let rundowns = Rundowns::parse(&file);
    let entries: Vec<(&str, &str, u64, &str)> = rundowns
        .entries
        .iter()
        .map(|entry| (entry.topic.as_str(), entry.name.as_str(), entry.at_s, entry.day.as_str()))
        .collect();
    assert_eq!(
        entries,
        [("finance", "resp_0a1B-2c_3", 1_792_000_000, "20261014"), ("news", "resp_9", 1_792_000_001, "20261014")]
    );
    assert_eq!(rundowns.entries[0].sources, SOURCES);
    assert_eq!(rundowns.to_text(), file);
}

#[test]
fn ledger_and_key_round_trip() {
    let file = text("openai/book");
    let with_later: String = file.lines().map(|line| std::format!("{line} 0 0 0 0 0 0\n")).collect();
    assert_eq!(
        Ledger::parse(&file).to_text(),
        with_later,
        "what the earlier firmware kept, and behind each month the browser's two kinds and the bell's, which it did not have"
    );
    assert_eq!(Ledger::parse(&with_later).to_text(), with_later);
    let file = text("openai/key");
    assert_eq!(key::parse_file(&file), Some("sk-proj-Fake0Key_-9"));
    assert_eq!(key::to_file_text("sk-proj-Fake0Key_-9"), file);
}

#[test]
fn gauge_and_usage_round_trip() {
    for name in ["power/gauge", "power/gauge_unknown"] {
        let file = text(name);
        assert_eq!(battery::Memory::parse(&file).expect("a gauge").text(), file);
    }
    for name in ["power/usage", "power/usage_bare"] {
        let file = text(name);
        let (usage, saved) = Usage::parse(&file).expect("a usage");
        assert_eq!(usage.text(saved), file);
    }
}

#[test]
fn networks_round_trip() {
    let file = text("wifi/known");
    assert_eq!(Known::parse(&file).to_text(), file);
    let file = text("wifi/places");
    assert_eq!(Places::parse(&file).to_text(), file);
}

#[test]
fn flash_round_trip() {
    let file = text("flashed");
    let flash = Flash::parse(&file).expect("a flash");
    assert_eq!((flash.stamp, flash.logging), (9, Logging::Off));
    assert_eq!(flash.key.as_deref(), Some("sk-proj-Fake0Key_-9"));
    assert_eq!(flash.to_text(), file);
}

#[test]
fn logs_round_trip() {
    let mut log = lines::boot_note(Some("20261016-a"));
    lines::add_line(&mut log, (NOW, 123), "data: mounted");
    lines::add_line(&mut log, (NOW + 1, 7), "wifi: joined");
    assert_eq!(log, text("logs/log"));
    let file = text("logs/crashes");
    assert_eq!(Crashes::parse(&file).into_text(), file);
}

#[test]
fn use_line_format() {
    assert_eq!(uses::to_line(1_767_225_600, uses::SIGNAL_USE), "1767225600 signal\n");
}

#[test]
fn safe_salt_stable() {
    assert!(safe::salt(&[7; 16])[..] == bytes("safe_salt")[..]);
}

#[test]
fn file_paths_stable() {
    use crate::cell;
    use crate::domain::{chat, flash, magic, news, router, settings};
    use crate::hw::power::usage;
    use crate::net::wireguard;
    use crate::storage::{CHAT, CHAT_DAYS, CHAT_PRIVATE, CHAT_TALK, INTERFACE};
    use crate::wifi::{self, known, places, station};
    let files = [
        (cell::DIRECTORY, cell::sim::FILE, ".r1metal/cell/sim"),
        (cell::DIRECTORY, cell::attaches::FILE, ".r1metal/cell/attaches"),
        (cell::DIRECTORY, cell::drives::SEEDED_FILE, ".r1metal/cell/seeded"),
        (cell::drives::NVRAM_DIRECTORY, "", ".r1metal/cell/nvram/"),
        (cell::drives::FACTORY_DIRECTORY, "", ".r1metal/cell/factory/"),
        (INTERFACE, settings::FILE, ".r1metal/interface/settings"),
        (INTERFACE, flash::STAMP_FILE, ".r1metal/interface/flash"),
        (INTERFACE, bell::FILE, ".r1metal/interface/bell"),
        (INTERFACE, news::topics::FILE, ".r1metal/interface/rundowns"),
        (battery::DIRECTORY, battery::FILE, ".r1metal/battery/gauge"),
        (battery::DIRECTORY, usage::FILE, ".r1metal/battery/usage"),
        (wifi::DIRECTORY, known::NETWORK_FILE, ".r1metal/wifi/networks"),
        (wifi::DIRECTORY, station::SECRET_FILE, ".r1metal/wifi/secret"),
        (wifi::DIRECTORY, places::PLACE_FILE, ".r1metal/wifi/place"),
        (place::DIRECTORY, place::OFFSET_FILE, ".r1metal/clock/place"),
        (place::DIRECTORY, place::LOCATION_FILE, ".r1metal/clock/where"),
        (key::DIRECTORY, key::FILE, ".r1metal/openai/key"),
        (key::DIRECTORY, ledger::FILE, ".r1metal/openai/ledger"),
        (CHAT_PRIVATE, chat::STATE_FILE, ".r1metal/chat/state"),
        (CHAT_PRIVATE, chat::THREAD_FILE, ".r1metal/chat/thread"),
        (magic::DIRECTORY, &magic::to_waiting_name("20261009_193512"), ".r1metal/magic/20261009_193512.jpg"),
        (news::STATE_DIR, news::editor::TRIED_FILE, ".r1metal/news/tried"),
        (news::STATE_DIR, news::rundowns::FILE, ".r1metal/news/pending"),
        (news::STATE_DIR, news::silent::FILE, ".r1metal/news/silent"),
        (logs::DIRECTORY, logs::BOOT, ".r1metal/logs/boot"),
        (logs::DIRECTORY, logs::CRASH_LIST, ".r1metal/logs/crashes"),
        (logs::DIRECTORY, logs::SCRUBBED, ".r1metal/logs/scrubbed"),
        (device::DIRECTORY, &device::File::Account.to_path().1, ".r1metal/signal/account"),
        (device::DIRECTORY, &device::File::Calls.to_path().1, ".r1metal/signal/calls"),
        (device::SESSIONS, "", ".r1metal/signal/sessions/"),
        (device::SENDER_KEYS, "", ".r1metal/signal/sender-keys/"),
        (device::CONVERSATIONS, "", ".r1metal/signal/conversations/"),
        (uses::DIRECTORY, uses::FILE, ".r1metal/analytics/uses"),
        (router::DIRECTORY, wireguard::config::FILE, ".r1metal/router/tunnel"),
        (CHAT, chat::CORE_FILE, "chat/core.md"),
        (CHAT, chat::KNOWLEDGE_FILE, "chat/knowledge.md"),
        (CHAT_TALK, &transcript::file_name("20261016"), "chat/talk/20261016.md"),
        (CHAT_DAYS, &transcript::file_name("20261016"), "chat/days/20261016.md"),
    ];
    for (directory, file, path) in files {
        assert_eq!(std::format!("{directory}/{file}"), path, "as docs/firmware/storage.md lists it");
    }
    assert_eq!(logs::ON_DEVICE, "/data/.r1metal/logs", "where the host and the log look");
}

fn walk(directory: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap().flatten() {
        if entry.path().is_dir() {
            walk(&entry.path(), paths);
        } else {
            paths.push(entry.path());
        }
    }
}

#[test]
fn signal_store_round_trip() {
    let base = root().join("signal");
    let mut paths = Vec::new();
    walk(&base, &mut paths);
    let mut files = BTreeMap::new();
    for path in &paths {
        let (directory, name) = (path.parent().unwrap().strip_prefix(&base).unwrap(), path.file_name().unwrap());
        let file =
            device::File::from_path(directory.to_str().unwrap(), name.to_str().unwrap()).expect("a file of the store");
        files.insert(file, fs::read(path).unwrap());
    }
    assert_eq!(files.len(), 10);
    let device = Device::read(&files).expect("a device");
    for (file, content) in &files {
        assert!(device.file_bytes(file).as_ref() == Some(content), "{:?}", file.to_path().1);
    }
}

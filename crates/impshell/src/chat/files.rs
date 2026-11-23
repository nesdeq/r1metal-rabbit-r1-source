use crate::log;
use crate::storage::Data;
use r1_funcore::base::time::Moment;
use r1_funcore::domain::chat::memory::Memory;
use r1_funcore::domain::chat::recollect;
use r1_funcore::domain::chat::state::Store;
use r1_funcore::domain::chat::thread::{FileChange, RobotState, Thread};
use r1_funcore::domain::chat::transcript::{self, Entry};
use r1_funcore::domain::chat::{CORE_FILE, KNOWLEDGE_FILE, STATE_FILE, THREAD_FILE, THREAD_MENDED_LINE, recall_days};
use r1_funcore::domain::paper::names::TEXT_EXTENSION;
use r1_funcore::domain::paper::sheet;
use r1_funcore::storage::paths::Listed;
use r1_funcore::storage::{CHAT, CHAT_DAYS, CHAT_PRIVATE, CHAT_TALK};
use std::io;

pub(super) fn read(data: &Data) -> io::Result<Store> {
    let text =
        |directory: &str, name: &str| -> io::Result<String> { Ok(data.text(directory, name)?.unwrap_or_default()) };
    let file = text(CHAT_PRIVATE, THREAD_FILE)?;
    let thread = Thread::parse(&file);
    if thread.needs_rewrite(&file) {
        match data.put(CHAT_PRIVATE, THREAD_FILE, thread.to_text().as_bytes()) {
            Ok(()) => log!("{THREAD_MENDED_LINE}"),
            Err(e) => log!("chat: the thread's file holds what is no item, and is not written anew: {e}"),
        }
    }
    Ok(Store {
        memory: Memory::parse(&text(CHAT, CORE_FILE)?, &text(CHAT, KNOWLEDGE_FILE)?),
        robot: RobotState::parse(&text(CHAT_PRIVATE, STATE_FILE)?),
        thread,
    })
}

// Writes the memory's two files; true where both are written.
pub(super) fn write_memory(data: &Data, memory: &Memory) -> bool {
    let core = data.put(CHAT, CORE_FILE, memory.to_core_text().as_bytes());
    let both = core.and_then(|()| data.put(CHAT, KNOWLEDGE_FILE, memory.to_knowledge_text().as_bytes()));
    both.inspect_err(|e| log!("chat: the memory is not kept: {e}")).is_ok()
}

pub(super) fn write_state(data: &Data, state: &RobotState) {
    if let Err(e) = data.put(CHAT_PRIVATE, STATE_FILE, state.to_text().as_bytes()) {
        log!("chat: the conversation's state is not kept: {e}");
    }
}

// Writes a change of the thread into its file; true where it is written.
pub(super) fn write_item(data: &Data, change: &FileChange) -> bool {
    let result = match change {
        FileChange::Rewrite(file) => data.put(CHAT_PRIVATE, THREAD_FILE, file.as_bytes()),
        FileChange::Line(line) => data.append(CHAT_PRIVATE, THREAD_FILE, line.as_bytes()),
    };
    result.inspect_err(|e| log!("chat: an item of the thread is not kept: {e}")).is_ok()
}

fn days(data: &Data, directory: &str) -> Vec<String> {
    match data.names(directory, Listed::Extension(TEXT_EXTENSION)) {
        Ok(names) => names.iter().filter_map(|name| transcript::parse_file_name(name)).map(String::from).collect(),
        Err(e) => {
            log!("chat: the days of {directory} cannot be listed: {e}");
            Vec::new()
        }
    }
}

pub(super) fn talk_days(data: &Data) -> Vec<String> {
    days(data, CHAT_TALK)
}

pub(super) fn entries(data: &Data, day: &str) -> Vec<Entry> {
    match data.text(CHAT_TALK, &transcript::file_name(day)) {
        Ok(file) => file.map(|file| transcript::parse(&file)).unwrap_or_default(),
        Err(e) => {
            log!("chat: the words of {day} cannot be read: {e}");
            Vec::new()
        }
    }
}

pub(super) fn every_day(data: &Data) -> Vec<(String, Vec<Entry>)> {
    talk_days(data).into_iter().map(|day| (day.clone(), entries(data, &day))).collect()
}

pub(super) fn write_entry(data: &Data, now: &Moment, entry: &Entry, robot: Option<&str>) -> io::Result<String> {
    let day = now.to_compact_date();
    let name = transcript::file_name(&day);
    let has_file = data.has(CHAT_TALK, &name)?;
    data.append(CHAT_TALK, &name, transcript::to_file_text(now, entry, robot, has_file).as_bytes())?;
    Ok(day)
}

pub(super) fn recollection(data: &Data, day: &str) -> Option<String> {
    let file = data.read(CHAT_DAYS, &transcript::file_name(day));
    let file = file.inspect_err(|e| log!("chat: what {day} settled cannot be read: {e}")).ok().flatten();
    file.map(|file| sheet::body_of(&file))
}

pub(super) fn read_recall_days(data: &Data, today: &str) -> Vec<(String, String)> {
    let recall = recall_days(&days(data, CHAT_DAYS), today);
    recall.iter().filter_map(|day| recollection(data, day).map(|text| (day.clone(), text))).collect()
}

// Removes a day's words and recollection.
pub(super) fn forget_day(data: &Data, day: &str) -> io::Result<()> {
    let name = transcript::file_name(day);
    data.remove(CHAT_TALK, &name).and(data.remove(CHAT_DAYS, &name))
}

// True while the day's words are kept.
pub(super) fn is_day_kept(data: &Data, day: &str) -> io::Result<bool> {
    data.has(CHAT_TALK, &transcript::file_name(day))
}

pub(super) fn unsettled_days(data: &Data, today: &str) -> Vec<(String, bool)> {
    recollect::unsettled_days(talk_days(data), &days(data, CHAT_DAYS), today)
}

pub(super) fn write_day(data: &Data, day: &str, moment: &Moment, text: &str) -> io::Result<()> {
    data.put(CHAT_DAYS, &transcript::file_name(day), recollect::to_day_file(moment, text).as_bytes())
}

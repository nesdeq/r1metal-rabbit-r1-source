//! Reading a text aloud.

mod pace;
pub mod reading;
pub mod state;

use super::paper::markdown::Blocks;
use super::shelf::Shelf;
use crate::openai::aloud::TASK;
use crate::openai::ledger::Kind;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// What a reading says: its passages so far, whether more come, and the voice's task.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Script {
    pub passages: Vec<String>,
    pub is_growing: bool,
    pub task: String,
}

impl Script {
    // A whole text.
    fn from_passages(passages: Vec<String>) -> Script {
        Script { passages, is_growing: false, task: TASK.into() }
    }

    /// A reply that grows passage by passage, said in the way `task` tells.
    pub const fn for_reply(task: String) -> Script {
        Script { passages: Vec::new(), is_growing: true, task }
    }
}

/// What a reading says, as its screen holds it.
pub enum Words<'a> {
    Arrived(&'a [String]),
    // A text, from that block on.
    Text(&'a str, usize),
}

impl Words<'_> {
    fn to_passages(&self) -> Vec<String> {
        match self {
            Words::Arrived(passages) => passages.to_vec(),
            Words::Text(text, first_block) => {
                Blocks::new(text, 0).skip(*first_block).filter_map(|block| block.0.to_voice_text()).collect()
            }
        }
    }
}

/// Whose reading the reader holds; each kind numbers its readings apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadingSource {
    Text(Shelf),
    Reply,
    Signal,
    // The page the browser shows.
    Web,
}

impl ReadingSource {
    /// The log line of a reading that begins, `passages` long where it is no reply.
    pub fn to_begun_line(self, round: u32, passages: usize) -> String {
        if self == ReadingSource::Reply {
            format!("aloud: reading {round}: a reply as it is written")
        } else {
            format!("aloud: reading {round}: {passages} passages")
        }
    }

    pub const fn ledger_kind(self) -> Kind {
        match self {
            ReadingSource::Text(Shelf::Notes) => Kind::NoteReading,
            ReadingSource::Text(Shelf::Talks) => Kind::TalkReading,
            ReadingSource::Text(Shelf::News) => Kind::NewsReading,
            // Never read aloud; the conversation's own, as its replies are.
            ReadingSource::Text(Shelf::Facts | Shelf::Days) | ReadingSource::Reply => Kind::Chat,
            ReadingSource::Signal => Kind::SignalReading,
            ReadingSource::Web => Kind::WebReading,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_kinds() {
        let table = [
            (ReadingSource::Text(Shelf::Notes), Kind::NoteReading),
            (ReadingSource::Text(Shelf::Talks), Kind::TalkReading),
            (ReadingSource::Text(Shelf::News), Kind::NewsReading),
            (ReadingSource::Reply, Kind::Chat),
            (ReadingSource::Signal, Kind::SignalReading),
            (ReadingSource::Web, Kind::WebReading),
        ];
        for (source, kind) in table {
            assert_eq!(source.ledger_kind(), kind);
        }
    }

    #[test]
    fn passages_of_words() {
        let arrived = [String::from("One."), String::from("Two.")];
        assert_eq!(Words::Arrived(&arrived).to_passages(), arrived, "what arrived, as it came");
        let text = "# Head\n\nOne\ntwo.\n\n---\n\n- item\n\nEnd.";
        assert_eq!(
            Words::Text(text, 0).to_passages(),
            ["Head", "One two.", "item", "End."],
            "a text's blocks, the line across saying nothing"
        );
        assert_eq!(Words::Text(text, 2).to_passages(), ["item", "End."], "from that block on");
        assert!(Words::Text(text, 5).to_passages().is_empty());
    }

    #[test]
    fn scripts() {
        let text = Script::from_passages(alloc::vec![String::from("One.")]);
        assert!(
            !text.is_growing && text.task == TASK,
            "a whole text is read by the reader's own task, and grows no more"
        );
        let reply = Script::for_reply("Say it.".into());
        assert!(reply.is_growing && reply.passages.is_empty() && reply.task == "Say it.", "a reply grows from nothing");
    }

    #[test]
    fn begun_lines() {
        assert_eq!(ReadingSource::Reply.to_begun_line(3, 0), "aloud: reading 3: a reply as it is written");
        assert_eq!(ReadingSource::Signal.to_begun_line(3, 2), "aloud: reading 3: 2 passages");
    }
}

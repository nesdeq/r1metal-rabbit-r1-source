//! The file of what the device was used for.

use super::chat::tools::Destination;
use alloc::format;
use alloc::string::String;

/// Kept whatever the logging, for statistics such as the most and the last used.
pub const DIRECTORY: &str = ".r1metal/analytics";
pub const FILE: &str = "uses";
/// The word a use of Signal is kept under, whose lines go once the phone unlinks the R1, those of its calls with them.
pub const SIGNAL_USE: &str = "signal";
// The word a use of Signal's calls is kept under.
const CALLS_USE: &str = "calls";

// True for a word whose lines go with those of `event`: Signal's calls go with Signal.
fn is_of(word: &str, event: &str) -> bool {
    word == event || event == SIGNAL_USE && word == CALLS_USE
}

/// What a hand or the model had the device do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Use {
    Open(Destination),
    Robot,
    Chat,
    // The conversation's thread forgotten.
    Forget,
}

impl Use {
    /// The word it is kept under.
    pub const fn to_word(self) -> &'static str {
        match self {
            Use::Robot => "robot",
            Use::Chat => "chat",
            Use::Forget => "forget",
            Use::Open(destination) => match destination {
                Destination::PhotoCamera => "photo",
                Destination::VideoCamera => "video",
                Destination::Pictures => "pictures",
                Destination::Videos => "videos",
                Destination::Notes => "notes",
                Destination::Translations => "translator",
                Destination::News => "news",
                Destination::Rundowns => "rundowns",
                Destination::Web => "web",
                Destination::Signal => SIGNAL_USE,
                Destination::Calls => CALLS_USE,
                Destination::Pokedex => "pokedex",
                Destination::Wifi => "wifi",
                Destination::Brightness => "brightness",
                Destination::Volume => "volume",
                Destination::Themes => "themes",
                Destination::Colours => "colours",
                Destination::Clock => "clock",
                Destination::Bell => "bell",
                Destination::Passphrase => "passphrase",
                Destination::SimPin => "pin",
                Destination::Tunnel => "tunnel",
                Destination::Cost => "cost",
                Destination::Power => "power",
                Destination::About => "about",
                Destination::Debug => "debug",
                Destination::Memory => "memory",
                Destination::Days => "days",
            },
        }
    }
}

/// The line that keeps an event done at `at_s`, UTC.
pub fn to_line(at_s: u64, event: &str) -> String {
    format!("{at_s} {event}\n")
}

/// The uses a file keeps without the lines of `event`, a use of Signal's calls going with Signal's; none where it holds none.
pub fn without_event(uses: &str, event: &str) -> Option<String> {
    let is_of_event = |line: &str| line.trim_end().split_once(' ').is_some_and(|parts| is_of(parts.1, event));
    let rest: String = uses.split_inclusive('\n').filter(|line| !is_of_event(line)).collect();
    (rest.len() != uses.len()).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_lines() {
        let uses =
            [to_line(1_767_225_600, SIGNAL_USE), to_line(1_767_225_700, "photo"), to_line(1_767_225_800, SIGNAL_USE)];
        assert_eq!(uses[0], "1767225600 signal\n");
        assert_eq!(
            [Destination::Passphrase, Destination::SimPin, Destination::Debug, Destination::Signal]
                .map(|destination| Use::Open(destination).to_word()),
            ["passphrase", "pin", "debug", SIGNAL_USE],
            "the words an earlier firmware kept them under"
        );
        assert_eq!(without_event(&uses.concat(), SIGNAL_USE), Some("1767225700 photo\n".into()));
        assert_eq!(without_event("1767225700 photo\n", SIGNAL_USE), None);
        assert_eq!(
            without_event("1767225600 signal\r\n1767225700 signalling\n", SIGNAL_USE),
            Some("1767225700 signalling\n".into())
        );
        let calls = Use::Open(Destination::Calls).to_word();
        assert_eq!(
            without_event(&[to_line(1, calls), to_line(2, "photo"), to_line(3, SIGNAL_USE)].concat(), SIGNAL_USE),
            Some("2 photo\n".into()),
            "the uses of Signal's calls go with Signal's"
        );
        assert_eq!(without_event(&to_line(1, SIGNAL_USE), calls), None, "and not the other way round");
    }
}

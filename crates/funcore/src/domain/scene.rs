//! The scene on the screen as data, with the words the log and the screens share.

use super::gallery::Kind;
use super::shelf::Shelf;
use alloc::format;
use alloc::string::String;

pub(crate) const CHAT: &str = "CHAT";
pub(crate) const SIGNAL: &str = "SIGNAL";
pub(crate) const CALLS: &str = "CALLS";
pub(crate) const CAMERA: &str = "CAMERA";
pub(crate) const PHOTO: &str = "PHOTO";
pub(crate) const VIDEO: &str = "VIDEO";
pub(crate) const PICTURES: &str = "PICTURES";
pub(crate) const VIDEOS: &str = "VIDEOS";
pub(super) const MIC: &str = "MIC";
pub(crate) const AUDIO_NOTE: &str = "AUDIO NOTE";
pub(crate) const TRANSLATOR: &str = "TRANSLATOR";
pub(crate) const NEWS: &str = "NEWS";
pub(crate) const WEB: &str = "WEB";
pub(crate) const POKEDEX: &str = "POKEDEX";
pub(crate) const SETTINGS: &str = "SETTINGS";
pub(crate) const WIFI: &str = "WIFI";
pub(super) const MOBILE_DATA: &str = "MOBILE DATA";
pub(crate) const MODEM: &str = "MODEM";
pub(crate) const SIM_PIN: &str = "SIM PIN";
pub(crate) const KEEP_PIN: &str = "KEEP PIN";
pub(crate) const USB_MODEM: &str = "USB MODEM";
pub(crate) const VPN: &str = "VPN";
pub(crate) const TUNNEL: &str = "TUNNEL";
pub(crate) const BRIGHTNESS: &str = "BRIGHTNESS";
pub(crate) const VOLUME: &str = "VOLUME";
pub(crate) const THEMES: &str = "THEMES";
pub(crate) const COLORS: &str = "COLORS";
pub(crate) const CLOCK: &str = "CLOCK";
pub(crate) const BELL: &str = "BELL";
pub(crate) const PASSPHRASE: &str = "PASSPHRASE";
pub(super) const USB_DRIVE: &str = "USB DRIVE";
pub(crate) const DRIVE: &str = "DRIVE";
pub(crate) const COST: &str = "COST";
pub(crate) const POWER: &str = "POWER";
pub(crate) const ABOUT: &str = "ABOUT";
pub(crate) const DEBUG: &str = "DEBUG";
pub(crate) const ROBOT: &str = "ROBOT";
pub(crate) const EVERYTHING: &str = "EVERYTHING";
pub(super) const FEATURES: &str = "FEATURES";
pub(super) const TOOLS: &str = "TOOLS";
pub(crate) const WEB_SEARCH: &str = "WEB SEARCH";
pub(crate) const DEVICE: &str = "DEVICE";
pub(crate) const NOTES: &str = "NOTES";
pub(crate) const CHAT_MODEL: &str = "CHAT MODEL";
pub(crate) const CHAT_REASONING: &str = "CHAT REASONING";
pub(crate) const RECOLLECTION_MODEL: &str = "RECOLLECTION MODEL";
pub(crate) const RECOLLECTION_REASONING: &str = "RECOLLECTION REASONING";
pub(crate) const MEMORY: &str = "MEMORY";
pub(crate) const DAYS: &str = "DAYS";
pub(crate) const FORGET: &str = "FORGET";
pub(crate) const FETCH: &str = "FETCH BY ITSELF";
pub(crate) const READ_ALOUD: &str = "READ ALOUD";
pub(crate) const SAID: &str = "SAID MESSAGES";
pub(crate) const TEXT_ONLY: &str = "TEXT ONLY";
pub(crate) const AUTO_STOPS: &str = "AUTO MODE STOPS";
pub(crate) const AUTO_WAITS: &str = "AUTO MODE WAITS";
pub(crate) const AUTO_HOURLY: &str = "AUTO MODE AN HOUR";
pub(crate) const AUTO_MODEL: &str = "AUTO MODEL";
pub(crate) const AUTO_REASONING: &str = "AUTO REASONING";
pub(crate) const UNLINK: &str = "UNLINK";
pub(crate) const SUMMARIZE_MODEL: &str = "SUMMARIZE MODEL";
pub(crate) const SUMMARIZE_REASONING: &str = "SUMMARIZE REASONING";
pub(crate) const EDITION: &str = "EDITION";
pub(crate) const RUNDOWNS: &str = "RUNDOWNS";
pub(crate) const REQUEST_MODEL: &str = "REQUEST MODEL";
pub(crate) const REQUEST_REASONING: &str = "REQUEST REASONING";
pub(crate) const MODEL: &str = "MODEL";
pub(crate) const REASONING: &str = "REASONING";
pub(crate) const LIMIT: &str = "LIMIT";
pub(crate) const LANGUAGE: &str = "LANGUAGE";
pub(crate) const MUTED: &str = "MUTED";
pub(crate) const KEYS: &str = "KEYS";
pub(crate) const SCANNING: &str = "SCANNING";
pub(crate) const CHECKING: &str = "CHECKING";
pub(crate) const KEEPING: &str = "KEEPING";
pub(crate) const OPEN: &str = "OPEN";

/// What the window that scans a code takes by it: the key to OpenAI's services, or a tunnel's config.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scan {
    Key,
    Tunnel,
}

impl Scan {
    /// The log line of a code of that many bytes the window read.
    pub fn to_log_line(self, bytes: usize) -> String {
        let tag = match self {
            Scan::Key => "key",
            Scan::Tunnel => "router",
        };
        format!("{tag}: the window's viewfinder read a code of {bytes} bytes")
    }
}

/// The step of the window that scans a code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyStep {
    Asking,
    Scanning,
    Checking,
    Keeping,
    Done,
}

impl KeyStep {
    const fn to_word(self) -> &'static str {
        match self {
            KeyStep::Asking => "ASKING",
            KeyStep::Scanning => SCANNING,
            KeyStep::Checking => CHECKING,
            KeyStep::Keeping => KEEPING,
            KeyStep::Done => "DONE",
        }
    }
}

/// The step of the safe's window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SafeStep {
    Waiting,
    Typing,
    Choosing,
    Repeating,
    Trying,
    Open,
}

impl SafeStep {
    const fn to_word(self) -> &'static str {
        match self {
            SafeStep::Waiting => "WAITING",
            SafeStep::Typing => "TYPING",
            SafeStep::Choosing => "CHOOSING",
            SafeStep::Repeating => "REPEATING",
            SafeStep::Trying => "TRYING",
            SafeStep::Open => OPEN,
        }
    }
}

/// What the screen shows, with the selected node's name on the menus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scene {
    Stage(&'static str),
    Home(&'static str),
    Menu(&'static str),
    Debug,
    Keyboard,
    Pin,
    Networks,
    Known,
    Cost,
    Spending(&'static str),
    Share(&'static str),
    Power,
    About,
    Entries(Shelf),
    Entry(Shelf),
    Reader,
    Talk,
    Chat { is_muted: bool },
    Pokedex { is_typing: bool },
    Web { is_typing: bool },
    Link,
    Signal,
    Conversation { is_typing: bool },
    Calls,
    CallChoice,
    Call { is_video: bool },
    Incoming,
    Finder(Kind),
    Grid(Kind),
    Roll(Kind),
    Brightness,
    Volume,
    Themes,
    Colours,
    Clock,
    Bell,
    BellEntry,
    Ring,
    Rundowns,
    Rundown,
    Key(KeyStep),
    Tunnel(KeyStep),
    Safe(SafeStep),
}

const fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Photo => PHOTO,
        Kind::Video => VIDEO,
    }
}

// The plural and singular names of a shelf's entries.
pub(crate) const fn shelf_names(shelf: Shelf) -> (&'static str, &'static str) {
    match shelf {
        Shelf::Notes => (NOTES, "NOTE"),
        Shelf::Talks => ("TRANSLATIONS", "TRANSLATION"),
        Shelf::News => (NEWS, EDITION),
        Shelf::Facts => (MEMORY, "FACT"),
        Shelf::Days => (DAYS, "DAY"),
    }
}

const fn keys_word(is_typing: bool) -> &'static str {
    if is_typing { KEYS } else { "" }
}

/// The log line of a scene's two words.
pub fn to_line((name, detail): (&str, &str)) -> String {
    if detail.is_empty() { format!("shows: {name}") } else { format!("shows: {name} {detail}") }
}

impl Scene {
    /// The scene's two words for the log.
    pub const fn to_words(self) -> (&'static str, &'static str) {
        match self {
            Scene::Stage(name) => ("STAGE", name),
            Scene::Home(name) => ("HOME", name),
            Scene::Menu(name) => ("MENU", name),
            Scene::Debug => (DEBUG, ""),
            Scene::Keyboard => ("KEYBOARD", "PASSWORD"),
            Scene::Pin => ("KEYBOARD", SIM_PIN),
            Scene::Networks => ("NETWORKS", ""),
            Scene::Known => ("KNOWN NETWORK", ""),
            Scene::Cost => (COST, ""),
            Scene::Spending(name) => ("SPENDING", name),
            Scene::Share(name) => ("SHARE", name),
            Scene::Power => (POWER, ""),
            Scene::About => (ABOUT, ""),
            Scene::Entries(shelf) => (shelf_names(shelf).0, ""),
            Scene::Entry(shelf) => (shelf_names(shelf).1, ""),
            Scene::Reader => ("READER", ""),
            Scene::Talk => (TRANSLATOR, ""),
            Scene::Chat { is_muted } => (CHAT, if is_muted { MUTED } else { "" }),
            Scene::Pokedex { is_typing } => (POKEDEX, keys_word(is_typing)),
            Scene::Web { is_typing } => (WEB, keys_word(is_typing)),
            Scene::Link => (SIGNAL, "LINK"),
            Scene::Signal => (SIGNAL, ""),
            Scene::Conversation { is_typing } => ("CONVERSATION", keys_word(is_typing)),
            Scene::Calls => (CALLS, ""),
            Scene::CallChoice => (CALLS, "CHOICE"),
            Scene::Call { is_video } => ("CALL", if is_video { VIDEO } else { "" }),
            Scene::Incoming => ("CALL", "INCOMING"),
            Scene::Finder(kind) => ("VIEWFINDER", kind_word(kind)),
            Scene::Grid(kind) => ("GRID", kind_word(kind)),
            Scene::Roll(kind) => ("ROLL", kind_word(kind)),
            Scene::Brightness => (BRIGHTNESS, ""),
            Scene::Volume => (VOLUME, ""),
            Scene::Themes => (THEMES, ""),
            Scene::Colours => (COLORS, ""),
            Scene::Clock => (CLOCK, ""),
            Scene::Bell => (BELL, ""),
            Scene::BellEntry => (BELL, "ENTRY"),
            Scene::Ring => (BELL, "RINGS"),
            Scene::Rundowns => (RUNDOWNS, ""),
            Scene::Rundown => (RUNDOWNS, "ENTRY"),
            Scene::Key(step) => ("KEY", step.to_word()),
            Scene::Tunnel(step) => (TUNNEL, step.to_word()),
            Scene::Safe(step) => ("SAFE", step.to_word()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines() {
        assert_eq!(to_line(Scene::Menu(WIFI).to_words()), "shows: MENU WIFI");
        assert_eq!(to_line(Scene::Cost.to_words()), "shows: COST", "a scene of one word has no blank behind it");
    }

    #[test]
    fn scene_words() {
        let table = [
            (Scene::Stage(CAMERA), ("STAGE", "CAMERA")),
            (Scene::Home(CHAT), ("HOME", "CHAT")),
            (Scene::Menu(SETTINGS), ("MENU", "SETTINGS")),
            (Scene::Debug, ("DEBUG", "")),
            (Scene::Keyboard, ("KEYBOARD", "PASSWORD")),
            (Scene::Pin, ("KEYBOARD", "SIM PIN")),
            (Scene::Networks, ("NETWORKS", "")),
            (Scene::Known, ("KNOWN NETWORK", "")),
            (Scene::Cost, ("COST", "")),
            (Scene::Spending(NEWS), ("SPENDING", "NEWS")),
            (Scene::Share(NEWS), ("SHARE", "NEWS")),
            (Scene::Power, ("POWER", "")),
            (Scene::About, ("ABOUT", "")),
            (Scene::Entries(Shelf::Notes), ("NOTES", "")),
            (Scene::Entries(Shelf::Talks), ("TRANSLATIONS", "")),
            (Scene::Entries(Shelf::News), ("NEWS", "")),
            (Scene::Entries(Shelf::Facts), ("MEMORY", "")),
            (Scene::Entries(Shelf::Days), ("DAYS", "")),
            (Scene::Entry(Shelf::Notes), ("NOTE", "")),
            (Scene::Entry(Shelf::Talks), ("TRANSLATION", "")),
            (Scene::Entry(Shelf::News), ("EDITION", "")),
            (Scene::Entry(Shelf::Facts), ("FACT", "")),
            (Scene::Entry(Shelf::Days), ("DAY", "")),
            (Scene::Reader, ("READER", "")),
            (Scene::Talk, ("TRANSLATOR", "")),
            (Scene::Chat { is_muted: false }, ("CHAT", "")),
            (Scene::Chat { is_muted: true }, ("CHAT", "MUTED")),
            (Scene::Pokedex { is_typing: true }, ("POKEDEX", "KEYS")),
            (Scene::Web { is_typing: false }, ("WEB", "")),
            (Scene::Link, ("SIGNAL", "LINK")),
            (Scene::Signal, ("SIGNAL", "")),
            (Scene::Conversation { is_typing: true }, ("CONVERSATION", "KEYS")),
            (Scene::Calls, ("CALLS", "")),
            (Scene::CallChoice, ("CALLS", "CHOICE")),
            (Scene::Call { is_video: false }, ("CALL", "")),
            (Scene::Call { is_video: true }, ("CALL", "VIDEO")),
            (Scene::Incoming, ("CALL", "INCOMING")),
            (Scene::Finder(Kind::Photo), ("VIEWFINDER", "PHOTO")),
            (Scene::Grid(Kind::Video), ("GRID", "VIDEO")),
            (Scene::Roll(Kind::Photo), ("ROLL", "PHOTO")),
            (Scene::Brightness, ("BRIGHTNESS", "")),
            (Scene::Volume, ("VOLUME", "")),
            (Scene::Themes, ("THEMES", "")),
            (Scene::Colours, ("COLORS", "")),
            (Scene::Clock, ("CLOCK", "")),
            (Scene::Bell, ("BELL", "")),
            (Scene::BellEntry, ("BELL", "ENTRY")),
            (Scene::Ring, ("BELL", "RINGS")),
            (Scene::Rundowns, ("RUNDOWNS", "")),
            (Scene::Rundown, ("RUNDOWNS", "ENTRY")),
        ];
        for (scene, words) in table {
            assert_eq!(scene.to_words(), words, "{scene:?}");
        }
        assert_eq!(
            [KeyStep::Asking, KeyStep::Scanning, KeyStep::Checking, KeyStep::Done]
                .map(|step| Scene::Key(step).to_words()),
            [("KEY", "ASKING"), ("KEY", "SCANNING"), ("KEY", "CHECKING"), ("KEY", "DONE")]
        );
        assert_eq!(
            [KeyStep::Asking, KeyStep::Scanning, KeyStep::Keeping, KeyStep::Done]
                .map(|step| Scene::Tunnel(step).to_words()),
            [("TUNNEL", "ASKING"), ("TUNNEL", "SCANNING"), ("TUNNEL", "KEEPING"), ("TUNNEL", "DONE")]
        );
        assert_eq!(
            [Scan::Key.to_log_line(51), Scan::Tunnel.to_log_line(310)],
            [
                "key: the window's viewfinder read a code of 51 bytes",
                "router: the window's viewfinder read a code of 310 bytes"
            ]
        );
        let steps = [
            SafeStep::Waiting,
            SafeStep::Typing,
            SafeStep::Choosing,
            SafeStep::Repeating,
            SafeStep::Trying,
            SafeStep::Open,
        ];
        assert_eq!(
            steps.map(|step| Scene::Safe(step).to_words().1),
            ["WAITING", "TYPING", "CHOOSING", "REPEATING", "TRYING", "OPEN"]
        );
    }
}

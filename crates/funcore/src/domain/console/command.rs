//! The host's commands: their verbs, their lines and their refusals.

use crate::base::text::{parse_switch, switch_word};
use crate::base::time::offset;
use crate::cell::at;
use crate::cell::sim::Pin;
use crate::hw::power::Power;
use crate::input::Event;
use crate::input::pose::Pose;
use crate::logs::Logging;
use crate::storage::safe::Passphrase;
use alloc::format;
use alloc::string::{String, ToString};
use core::str::FromStr;

/// The verbs of every firmware, as `r1 sh --help` names them.
pub const VERBS: [&str; 19] = [
    "ping",
    "reboot",
    "bootloader",
    "poweroff",
    "input",
    "anim",
    "clock",
    "time",
    "radio",
    "link",
    "camera",
    "news",
    "chat",
    "record",
    "key",
    "log",
    "pin",
    "attach",
    "at",
];
/// The verbs of the debug shell.
pub const SHELL_VERBS: [&str; 16] = [
    "ls", "cat", "dmesg", "write", "rm", "mkdir", "rmdir", "read", "append", "sum", "unlock", "lock", "stage",
    "commit", "tunnel", "bench",
];

/// The most bytes one `read` asks for: as base64 they make one line of 64 KiB.
pub const READ_MOST: u32 = 3 * 16384;

/// Why a device refuses a parsed command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    NothingTurns,
    NoSuchZone,
    NoRecording,
    NoKnownNetwork,
    NoModem,
    NoShellThread,
    NoShell,
}

impl Refusal {
    /// The words of the failed answer.
    pub const fn message(self) -> &'static str {
        match self {
            Refusal::NothingTurns => "nothing that turns with the device shows",
            Refusal::NoSuchZone => "no zone is that far from UTC",
            Refusal::NoRecording => "the screen cannot be recorded: no frames function or no screen",
            Refusal::NoKnownNetwork => crate::wifi::known::UNKNOWN,
            Refusal::NoModem => crate::cell::modem::NOT_READY,
            Refusal::NoShellThread => "the shell has no thread",
            Refusal::NoShell => "built without the debug shell",
        }
    }
}

/// What the device can do that a command may need.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
    pub can_turn: bool,
    pub can_record: bool,
    pub can_link: bool,
    pub can_cell: bool,
}

/// The network the console asks the device to be in: none, the default network or else the modem's, or the modem's alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Link {
    Off,
    On,
    Cell,
}

impl Link {
    const fn to_word(self) -> &'static str {
        match self {
            Link::Off => "off",
            Link::On => "on",
            Link::Cell => "cell",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Command {
    // A session's own word tells its answers from what an earlier session left unread.
    Ping(Option<String>),
    Reboot,
    Bootloader,
    PowerOff,
    Anim(bool),
    Dmesg,
    Ls(String),
    Cat(String),
    Write { path: String, text: String },
    Remove(String),
    MakeDirectory(String),
    RemoveDirectory(String),
    // Answered as one line of base64; a short answer means the file ends there.
    Read { path: String, offset: u64, length: u32 },
    // The data is base64; a missing file is made.
    Append { path: String, data: String },
    // Answered with the SHA-256 of a file's bytes, in hexadecimal.
    Sum(String),
    // The safe's passphrase, which opens the shell's gate to Signal's files and a restore's.
    Unlock(Passphrase),
    Lock,
    // Makes the directory a restore's files are staged in, in place of what was staged or confirmed before.
    Stage,
    // Confirms the staged files, which take the partition's place at the next start.
    Commit,
    // A tunnel's config as `wg-quick` takes it, in base64, which replaces any kept before; the router's thread keeps it.
    Tunnel(String),
    // Times a call's pictures coded and decoded, and each vector kernel against its plain body.
    Bench,
    Input(Event),
    // Holds until the accelerometer tells of another pose.
    Pose(Pose),
    // Seconds of UTC since 1970, and the minutes the screen's clock is ahead of UTC.
    Clock { utc: u64, offset: i16 },
    Time,
    Radio(bool),
    Link(Link),
    Camera(bool),
    News,
    Chat(String),
    Record(bool),
    // None asks whether a key is stored; a key given replaces any before it.
    Key(Option<String>),
    Log(Logging),
    // The SIM's PIN, handed to it and kept once it takes it where KEEP PIN is on.
    Pin(Pin),
    // The modem's attach outlasts the stays, or not any more.
    Attach(bool),
    // An AT command for the modem, its answer told in the log.
    At(String),
}

impl core::fmt::Debug for Command {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Command::Key(Some(_)) => f.write_str("key (given)"),
            Command::Pin(_) => f.write_str("pin (given)"),
            Command::Unlock(_) => f.write_str("unlock (given)"),
            Command::Tunnel(_) => f.write_str("tunnel (given)"),
            Command::At(text) => write!(f, "at {}", at::name_of(text)),
            command => f.write_str(&command.encode()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseError {
    Empty,
    UnknownVerb,
    MissingArgument,
    UnexpectedArgument,
    NotANumber,
    // A `read` of more than `READ_MOST` bytes.
    TooLong,
}

impl ParseError {
    pub const fn message(self) -> &'static str {
        match self {
            ParseError::Empty => "empty command",
            ParseError::UnknownVerb => "unknown verb",
            ParseError::MissingArgument => "missing argument",
            ParseError::UnexpectedArgument => "unexpected argument",
            ParseError::NotANumber => "not a number",
            ParseError::TooLong => "a read takes at most 49152 bytes",
        }
    }
}

fn parse_decimal<T: FromStr>(word: &str) -> Result<T, ParseError> {
    word.parse().map_err(|_| ParseError::NotANumber)
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(' ').filter(|word| !word.is_empty())
}

fn next_number<'a, T: FromStr>(words: &mut impl Iterator<Item = &'a str>) -> Result<T, ParseError> {
    parse_decimal(words.next().ok_or(ParseError::MissingArgument)?)
}

fn expect_end<'a>(mut words: impl Iterator<Item = &'a str>, command: Command) -> Result<Command, ParseError> {
    if words.next().is_some() { Err(ParseError::UnexpectedArgument) } else { Ok(command) }
}

fn next_switch(rest: &str) -> Result<bool, ParseError> {
    parse_switch(rest).ok_or(if rest.is_empty() { ParseError::MissingArgument } else { ParseError::UnexpectedArgument })
}

/// The command of a host's line, or the words of the failed answer.
pub fn read_line(line: &str, capabilities: Capabilities) -> Result<Command, &'static str> {
    let command = Command::parse(line).map_err(ParseError::message)?;
    command.refusal(capabilities).map_or(Ok(command), |refusal| Err(refusal.message()))
}

impl Command {
    pub fn parse(line: &str) -> Result<Command, ParseError> {
        let line = line.trim_matches(['\r', '\n']);
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        let no_argument = |command| if rest.is_empty() { Ok(command) } else { Err(ParseError::UnexpectedArgument) };
        let path = || if rest.is_empty() { Err(ParseError::MissingArgument) } else { Ok(rest.to_string()) };
        match verb {
            "" => Err(ParseError::Empty),
            "ping" => Ok(Command::Ping((!rest.is_empty()).then(|| rest.to_string()))),
            "reboot" => no_argument(Command::Reboot),
            "bootloader" => no_argument(Command::Bootloader),
            "poweroff" => no_argument(Command::PowerOff),
            "anim" => next_switch(rest).map(Command::Anim),
            "dmesg" => no_argument(Command::Dmesg),
            "ls" => path().map(Command::Ls),
            "cat" => path().map(Command::Cat),
            "write" => {
                let (path, text) = rest.split_once(' ').ok_or(ParseError::MissingArgument)?;
                Ok(Command::Write { path: path.to_string(), text: text.to_string() })
            }
            "rm" => path().map(Command::Remove),
            "mkdir" => path().map(Command::MakeDirectory),
            "rmdir" => path().map(Command::RemoveDirectory),
            "read" => {
                let mut parts = rest.rsplitn(3, ' ');
                let (length, offset, path) = (parts.next(), parts.next(), parts.next());
                let (Some(length), Some(offset), Some(path)) = (length, offset, path.filter(|path| !path.is_empty()))
                else {
                    return Err(ParseError::MissingArgument);
                };
                let (offset, length) = (parse_decimal(offset)?, parse_decimal(length)?);
                if length > READ_MOST {
                    return Err(ParseError::TooLong);
                }
                Ok(Command::Read { path: path.to_string(), offset, length })
            }
            "append" => {
                let (path, data) =
                    rest.rsplit_once(' ').filter(|(path, _)| !path.is_empty()).ok_or(ParseError::MissingArgument)?;
                Ok(Command::Append { path: path.to_string(), data: data.to_string() })
            }
            "sum" => path().map(Command::Sum),
            "unlock" if rest.is_empty() => Err(ParseError::MissingArgument),
            "unlock" => Ok(Command::Unlock(rest.into())),
            "lock" => no_argument(Command::Lock),
            "stage" => no_argument(Command::Stage),
            "commit" => no_argument(Command::Commit),
            "tunnel" => path().map(Command::Tunnel),
            "bench" => no_argument(Command::Bench),
            "input" => {
                let mut fields = words(rest);
                let mut word = || fields.next().ok_or(ParseError::MissingArgument);
                let event = match (word()?, word()) {
                    ("ptt", Ok("down")) => Event::PttDown,
                    ("ptt", Ok("up")) => Event::PttUp,
                    ("wheel", Ok(detents)) => Event::Wheel(parse_decimal(detents)?),
                    ("touch", Ok(x)) => Event::TouchAt { x: parse_decimal(x)?, y: parse_decimal(word()?)? },
                    ("lift", Err(_)) => Event::TouchUp,
                    ("pose", Ok(name)) => {
                        return expect_end(
                            fields,
                            Command::Pose(Pose::from_name(name).ok_or(ParseError::UnexpectedArgument)?),
                        );
                    }
                    ("ptt" | "wheel" | "touch" | "pose", Err(missing)) => return Err(missing),
                    _ => return Err(ParseError::UnexpectedArgument),
                };
                expect_end(fields, Command::Input(event))
            }
            "clock" => {
                let mut fields = words(rest);
                let clock = Command::Clock { utc: next_number(&mut fields)?, offset: next_number(&mut fields)? };
                expect_end(fields, clock)
            }
            "time" => no_argument(Command::Time),
            "radio" => next_switch(rest).map(Command::Radio),
            "link" if rest == Link::Cell.to_word() => Ok(Command::Link(Link::Cell)),
            "link" => next_switch(rest).map(|is_on| Command::Link(if is_on { Link::On } else { Link::Off })),
            "camera" => next_switch(rest).map(Command::Camera),
            "news" => no_argument(Command::News),
            "chat" => path().map(Command::Chat),
            "record" => next_switch(rest).map(Command::Record),
            "key" if rest.is_empty() => Ok(Command::Key(None)),
            "key" => crate::openai::key::parse_key(rest)
                .map(|key| Command::Key(Some(key.to_string())))
                .ok_or(ParseError::UnexpectedArgument),
            "log" if rest.is_empty() => Err(ParseError::MissingArgument),
            "log" => Logging::parse(rest).map(Command::Log).ok_or(ParseError::UnexpectedArgument),
            "pin" if rest.is_empty() => Err(ParseError::MissingArgument),
            "pin" => Pin::parse(rest).map(Command::Pin).ok_or(ParseError::UnexpectedArgument),
            "attach" => next_switch(rest).map(Command::Attach),
            "at" => path().map(Command::At),
            _ => Err(ParseError::UnknownVerb),
        }
    }

    fn refusal(&self, capabilities: Capabilities) -> Option<Refusal> {
        match self {
            Command::Pose(_) if !capabilities.can_turn => Some(Refusal::NothingTurns),
            Command::Clock { offset, .. } if offset::checked(i64::from(*offset)).is_none() => Some(Refusal::NoSuchZone),
            Command::Record(_) if !capabilities.can_record => Some(Refusal::NoRecording),
            Command::Link(Link::On) if !capabilities.can_link => Some(Refusal::NoKnownNetwork),
            Command::Link(Link::Cell) if !capabilities.can_cell => Some(Refusal::NoModem),
            _ => None,
        }
    }

    /// The path of a verb on files, which `shell::is_denied` judges before it runs.
    pub fn path(&self) -> Option<&str> {
        match self {
            Command::Ls(path)
            | Command::Cat(path)
            | Command::Read { path, .. }
            | Command::Write { path, .. }
            | Command::Remove(path)
            | Command::MakeDirectory(path)
            | Command::RemoveDirectory(path)
            | Command::Append { path, .. }
            | Command::Sum(path) => Some(path),
            Command::Ping(_)
            | Command::Reboot
            | Command::Bootloader
            | Command::PowerOff
            | Command::Anim(_)
            | Command::Dmesg
            | Command::Unlock(_)
            | Command::Lock
            | Command::Stage
            | Command::Commit
            | Command::Tunnel(_)
            | Command::Bench
            | Command::Input(_)
            | Command::Pose(_)
            | Command::Clock { .. }
            | Command::Time
            | Command::Radio(_)
            | Command::Link(_)
            | Command::Camera(_)
            | Command::News
            | Command::Chat(_)
            | Command::Record(_)
            | Command::Key(_)
            | Command::Log(_)
            | Command::Pin(_)
            | Command::Attach(_)
            | Command::At(_) => None,
        }
    }

    /// True for a command the device goes down at, which ends the link before an answer.
    pub const fn ends_link(&self) -> bool {
        matches!(self, Command::Reboot | Command::Bootloader | Command::PowerOff)
    }

    /// The command's way down.
    pub const fn to_power(&self) -> Option<Power> {
        match self {
            Command::Reboot => Some(Power::Reboot),
            Command::Bootloader => Some(Power::Bootloader),
            Command::PowerOff => Some(Power::PowerOff),
            _ => None,
        }
    }

    // True for a verb run only once the safe's passphrase is given: those that stage a restore, and the one that hands over a tunnel's config, whose file the gate keeps.
    pub(super) const fn needs_gate(&self) -> bool {
        matches!(self, Command::Stage | Command::Commit | Command::Tunnel(_))
    }

    /// True for a command another thread does and answers, the lines behind it waiting: a key to store, a tunnel's config to keep and every other debug shell verb.
    pub const fn needs_thread(&self) -> bool {
        match self {
            Command::Key(Some(_))
            | Command::Dmesg
            | Command::Ls(_)
            | Command::Cat(_)
            | Command::Write { .. }
            | Command::Remove(_)
            | Command::MakeDirectory(_)
            | Command::RemoveDirectory(_)
            | Command::Read { .. }
            | Command::Append { .. }
            | Command::Sum(_)
            | Command::Unlock(_)
            | Command::Lock
            | Command::Stage
            | Command::Commit
            | Command::Tunnel(_)
            | Command::Bench => true,
            Command::Ping(_)
            | Command::Reboot
            | Command::Bootloader
            | Command::PowerOff
            | Command::Anim(_)
            | Command::Input(_)
            | Command::Pose(_)
            | Command::Clock { .. }
            | Command::Time
            | Command::Radio(_)
            | Command::Link(_)
            | Command::Camera(_)
            | Command::News
            | Command::Chat(_)
            | Command::Record(_)
            | Command::Key(None)
            | Command::Log(_)
            | Command::Pin(_)
            | Command::Attach(_)
            | Command::At(_) => false,
        }
    }

    /// The host's line, ended as `take_lines` takes a line.
    pub fn to_line(&self) -> String {
        format!("{}\n", self.encode())
    }

    pub fn encode(&self) -> String {
        match self {
            Command::Ping(None) => "ping".into(),
            Command::Ping(Some(word)) => format!("ping {word}"),
            Command::Reboot => "reboot".into(),
            Command::Bootloader => "bootloader".into(),
            Command::PowerOff => "poweroff".into(),
            Command::Anim(on) => format!("anim {}", switch_word(*on)),
            Command::Dmesg => "dmesg".into(),
            Command::Ls(path) => format!("ls {path}"),
            Command::Cat(path) => format!("cat {path}"),
            Command::Write { path, text } => format!("write {path} {text}"),
            Command::Remove(path) => format!("rm {path}"),
            Command::MakeDirectory(path) => format!("mkdir {path}"),
            Command::RemoveDirectory(path) => format!("rmdir {path}"),
            Command::Read { path, offset, length } => format!("read {path} {offset} {length}"),
            Command::Append { path, data } => format!("append {path} {data}"),
            Command::Sum(path) => format!("sum {path}"),
            Command::Unlock(passphrase) => format!("unlock {}", passphrase.as_str()),
            Command::Lock => "lock".into(),
            Command::Stage => "stage".into(),
            Command::Commit => "commit".into(),
            Command::Tunnel(code) => format!("tunnel {code}"),
            Command::Bench => "bench".into(),
            Command::Input(Event::PttDown) => "input ptt down".into(),
            Command::Input(Event::PttUp) => "input ptt up".into(),
            Command::Input(Event::Wheel(detents)) => format!("input wheel {detents}"),
            Command::Input(Event::TouchAt { x, y }) => format!("input touch {x} {y}"),
            Command::Input(Event::TouchUp) => "input lift".into(),
            Command::Pose(pose) => format!("input pose {}", pose.name()),
            Command::Clock { utc, offset } => format!("clock {utc} {offset}"),
            Command::Time => "time".into(),
            Command::Radio(on) => format!("radio {}", switch_word(*on)),
            Command::Link(link) => format!("link {}", link.to_word()),
            Command::Camera(on) => format!("camera {}", switch_word(*on)),
            Command::News => "news".into(),
            Command::Chat(text) => format!("chat {text}"),
            Command::Record(on) => format!("record {}", switch_word(*on)),
            Command::Key(None) => "key".into(),
            Command::Key(Some(key)) => format!("key {key}"),
            Command::Log(logging) => format!("log {}", logging.to_word()),
            Command::Pin(pin) => format!("pin {}", pin.digits()),
            Command::Attach(on) => format!("attach {}", switch_word(*on)),
            Command::At(text) => format!("at {text}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::wire::{Line, take_lines};
    use super::*;

    #[test]
    fn encode_parse_round_trip() {
        let commands = [
            Command::Ping(None),
            Command::Ping(Some("4711-1792000000123456789".into())),
            Command::Reboot,
            Command::Bootloader,
            Command::PowerOff,
            Command::Anim(true),
            Command::Anim(false),
            Command::Dmesg,
            Command::Ls("/sys/class/leds".into()),
            Command::Cat("/proc/cmdline".into()),
            Command::Write { path: "/sys/power/state".into(), text: "mem".into() },
            Command::Write { path: "/a".into(), text: "two words".into() },
            Command::Remove("/data/camera/a note".into()),
            Command::MakeDirectory("/data/audio/notes/a folder".into()),
            Command::RemoveDirectory("/data/audio/notes/a folder".into()),
            Command::Read { path: "/data/camera/a note".into(), offset: 49152, length: 49152 },
            Command::Append { path: "/data/camera/a note".into(), data: "Zm9vYg==".into() },
            Command::Sum("/data/camera/a note".into()),
            Command::Unlock("opening".into()),
            Command::Unlock(" two words ".into()),
            Command::Lock,
            Command::Stage,
            Command::Commit,
            Command::Tunnel("W0ludGVyZmFjZV0K".into()),
            Command::Bench,
            Command::Input(Event::PttDown),
            Command::Input(Event::PttUp),
            Command::Input(Event::Wheel(-3)),
            Command::Input(Event::TouchAt { x: 240, y: 5 }),
            Command::Input(Event::TouchUp),
            Command::Pose(Pose::RightUp),
            Command::Pose(Pose::UpsideDown),
            Command::Clock { utc: 1_747_392_631, offset: 120 },
            Command::Clock { utc: 0, offset: -210 },
            Command::Time,
            Command::Radio(true),
            Command::Radio(false),
            Command::Link(Link::On),
            Command::Link(Link::Off),
            Command::Link(Link::Cell),
            Command::Camera(true),
            Command::Camera(false),
            Command::News,
            Command::Chat("Wie wird das Wetter morgen?".into()),
            Command::Record(true),
            Command::Record(false),
            Command::Key(None),
            Command::Key(Some("sk-proj-Fake0Key_-9".into())),
            Command::Log(Logging::Off),
            Command::Log(Logging::On),
            Command::Log(Logging::Debug),
            Command::Pin(Pin::parse("1234").unwrap()),
            Command::Attach(true),
            Command::Attach(false),
            Command::At("AT+CEREG?".into()),
            Command::At("AT+CPSMS=1,,,\"00100001\",\"00000000\"".into()),
        ];
        for command in commands {
            let (mut inbox, mut is_dropping) = (command.to_line().into_bytes(), false);
            assert_eq!(take_lines(&mut inbox, &mut is_dropping), [Line::Command(command.encode())], "{command:?}");
            assert_eq!(Command::parse(&command.encode()), Ok(command));
        }
        for verb in VERBS.iter().chain(&SHELL_VERBS) {
            assert_ne!(Command::parse(verb), Err(ParseError::UnknownVerb), "{verb}");
        }
        assert_eq!(
            (format!("{:?}", Command::Key(Some("sk-proj-Fake0Key_-9".into()))), format!("{:?}", Command::Key(None))),
            ("key (given)".into(), "key".into()),
            "a command is told as its line has it, and a key it hands over never"
        );
        assert_eq!(format!("{:?}", Command::Pin(Pin::parse("1234").unwrap())), "pin (given)", "nor a PIN");
        assert_eq!(format!("{:?}", Command::Unlock("opening".into())), "unlock (given)", "nor a passphrase");
        assert_eq!(
            format!("{:?}", Command::Tunnel("W0ludGVyZmFjZV0K".into())),
            "tunnel (given)",
            "nor a tunnel's config, which holds a key"
        );
        assert_eq!(
            format!("{:?}", Command::At("AT+CPIN=\"1234\"".into())),
            "at AT+CPIN",
            "nor the arguments of an AT command, which may hold one"
        );
    }

    #[test]
    fn refusals() {
        let (can, cannot) = (
            Capabilities { can_turn: true, can_record: true, can_link: true, can_cell: true },
            Capabilities { can_turn: false, can_record: false, can_link: false, can_cell: false },
        );
        let cases = [
            (Command::Pose(Pose::RightUp), Refusal::NothingTurns),
            (Command::Record(false), Refusal::NoRecording),
            (Command::Link(Link::On), Refusal::NoKnownNetwork),
            (Command::Link(Link::Cell), Refusal::NoModem),
        ];
        for (command, refusal) in cases {
            assert_eq!((command.refusal(can), command.refusal(cannot)), (None, Some(refusal)), "{command:?}");
        }
        assert_eq!(
            Command::Link(Link::Off).refusal(cannot),
            None,
            "a link is let go of whether a network is known or not"
        );
        assert_eq!(Refusal::NoKnownNetwork.message(), "no network is known");
        assert_eq!(Refusal::NoModem.message(), "the modem is not ready");
        assert_eq!(Command::Clock { utc: 0, offset: 840 }.refusal(cannot), None, "the zone farthest east");
        assert_eq!(Command::Clock { utc: 0, offset: 841 }.refusal(can), Some(Refusal::NoSuchZone), "and past it");
        assert_eq!(
            Command::Clock { utc: 0, offset: -661 }.refusal(can),
            Some(Refusal::NoSuchZone),
            "past the farthest west"
        );
    }

    const EVERY_VERB: [&str; 36] = [
        "tunnel W0ludGVyZmFjZV0K",
        "bench",
        "sum /a",
        "unlock opening",
        "lock",
        "stage",
        "commit",
        "pin 1234",
        "attach on",
        "at AT",
        "ping",
        "reboot",
        "bootloader",
        "poweroff",
        "input lift",
        "anim on",
        "clock 0 0",
        "time",
        "radio on",
        "link on",
        "camera on",
        "news",
        "chat hi",
        "record on",
        "key",
        "key sk-proj-Fake0Key_-9",
        "log on",
        "ls /a",
        "cat /a",
        "dmesg",
        "write /a x",
        "rm /a",
        "mkdir /a",
        "rmdir /a",
        "read /a 0 1",
        "append /a eA==",
    ];

    fn every_verb() -> impl Iterator<Item = (&'static str, &'static str, Command)> {
        for verb in VERBS.iter().chain(&SHELL_VERBS) {
            assert!(EVERY_VERB.iter().any(|line| line.split(' ').next() == Some(verb)), "{verb} has a line");
        }
        EVERY_VERB.into_iter().map(|line| (line, line.split(' ').next().unwrap(), Command::parse(line).unwrap()))
    }

    #[test]
    fn file_verb_paths() {
        let pathless = ["dmesg", "unlock", "lock", "stage", "commit", "tunnel", "bench"];
        for (line, verb, command) in every_verb() {
            let is_file_verb = SHELL_VERBS.contains(&verb) && !pathless.contains(&verb);
            assert_eq!(command.path(), is_file_verb.then_some("/a"), "{line}: a verb on files alone has a path");
        }
    }

    #[test]
    fn gated_verbs() {
        for (line, verb, command) in every_verb() {
            assert_eq!(command.needs_gate(), ["stage", "commit", "tunnel"].contains(&verb), "{line}");
        }
    }

    #[test]
    fn thread_commands() {
        for (line, verb, command) in every_verb() {
            let needs_thread = SHELL_VERBS.contains(&verb) || matches!(command, Command::Key(Some(_)));
            assert_eq!(command.needs_thread(), needs_thread, "{line}");
        }
    }

    #[test]
    fn line_ends_ignored() {
        assert_eq!(Command::parse("ping\r\n"), Ok(Command::Ping(None)));
    }

    #[test]
    fn line_to_command_or_failure() {
        let can = Capabilities { can_turn: true, can_record: true, can_link: true, can_cell: true };
        let cannot = Capabilities { can_turn: false, can_record: false, can_link: false, can_cell: false };
        assert_eq!(read_line("record on", can), Ok(Command::Record(true)));
        assert_eq!(read_line("record on", cannot), Err(Refusal::NoRecording.message()));
        assert_eq!(read_line("frobnicate", can), Err(ParseError::UnknownVerb.message()));
    }

    #[test]
    fn power_commands() {
        assert_eq!(
            [Command::Reboot, Command::Bootloader, Command::PowerOff, Command::Time].map(|command| command.to_power()),
            [Some(Power::Reboot), Some(Power::Bootloader), Some(Power::PowerOff), None]
        );
    }

    #[test]
    fn malformed_commands() {
        assert_eq!(Command::parse(""), Err(ParseError::Empty));
        assert_eq!(Command::parse("frobnicate"), Err(ParseError::UnknownVerb));
        assert_eq!(Command::parse("ls"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("write /x"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("rm"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("mkdir"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("rmdir"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("read /x 0"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("read /x start 16"), Err(ParseError::NotANumber));
        assert_eq!(
            Command::parse(&format!("read /dev/zero 0 {}", READ_MOST + 1)),
            Err(ParseError::TooLong),
            "a part longer than the host ever asks for"
        );
        assert_eq!(
            (READ_MOST, ParseError::TooLong.message()),
            (49152, "a read takes at most 49152 bytes"),
            "the answer names the bound"
        );
        assert_eq!(Command::parse("append /x"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("sum"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("unlock"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("lock now"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("stage now"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("commit now"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("tunnel"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("input"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("input ptt"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("input ptt sideways"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("input wheel"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("input wheel up"), Err(ParseError::NotANumber));
        assert_eq!(Command::parse("input touch 240"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("input touch 240 5 9"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("input lift now"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("input nose"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("input pose"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("input pose sideways"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("input pose upright now"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("clock"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("clock 1747392631"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("clock noon 120"), Err(ParseError::NotANumber));
        assert_eq!(Command::parse("clock 1747392631 east"), Err(ParseError::NotANumber));
        assert_eq!(Command::parse("clock 1747392631 120 9"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("time now"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("reboot now"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("anim"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("anim sideways"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("radio"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("radio loud"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("link"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("link up"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("camera"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("camera rear"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("news now"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("chat"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("record"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("record video"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("key not-a-key"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("key sk-two words"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("log"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("log loud"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("pin"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("pin 12"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("pin 12345 6"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("link wifi"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("attach"), Err(ParseError::MissingArgument));
        assert_eq!(Command::parse("attach always"), Err(ParseError::UnexpectedArgument));
        assert_eq!(Command::parse("at"), Err(ParseError::MissingArgument));
    }

    #[test]
    fn extra_blanks_ignored() {
        assert_eq!(Command::parse("input wheel  3 "), Ok(Command::Input(Event::Wheel(3))));
        assert_eq!(Command::parse("clock 1747392631  120 "), Ok(Command::Clock { utc: 1_747_392_631, offset: 120 }));
    }
}

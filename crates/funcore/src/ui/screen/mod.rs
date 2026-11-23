pub(super) mod about;
pub(super) mod action;
pub(super) mod aloud;
pub(super) mod bar;
pub(super) mod bell;
pub(super) mod calls;
pub(super) mod chat;
pub(super) mod clock;
pub(super) mod cost;
pub(super) mod debug;
pub(super) mod finder;
mod home;
pub(super) mod input;
pub(super) mod keying;
pub(super) mod levels;
pub(super) mod memos;
pub(super) mod networks;
pub(super) mod pin;
mod pokedex;
pub(super) mod power;
pub(super) mod reader;
pub(super) mod roll;
pub(super) mod rundowns;
pub(super) mod safe;
pub(super) mod seat;
mod settings;
mod shows;
pub(super) mod signal;
pub(super) mod sleep;
pub(super) mod talk;
mod tally;
pub(super) mod themes;
pub(super) mod tree;
pub(super) mod typing;
pub(super) mod voice;
mod web;

use super::corner::BELL_ICONS;
use super::gauge::{Gauge, Scale};
use super::grid::Grid;
use super::list::LinePicture;
use super::textroom::{self, RoomPicture};
use super::view::lines::List;
use super::{BACKWARD, FORWARD};
use crate::base::time::NANOS_PER_SECOND;
use crate::codec::qr::Symbol;
use crate::domain::gallery::Kind;
use crate::domain::voice::Phase;
use crate::gfx::store::Store;
use bar::BarWords;
use typing::KeyboardId;

pub(super) const ICONS: [&[&str]; 12] = [
    &cost::ICONS,
    &power::ICONS,
    &memos::ICONS,
    &networks::ICONS,
    &keying::ICONS,
    &signal::ICONS,
    &calls::ICONS,
    &BELL_ICONS,
    &bell::ICONS,
    &rundowns::ICONS,
    &levels::ICONS,
    &textroom::ICONS,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Screen {
    Debug,
    Keyboard,
    Pin,
    Networks,
    Known,
    Cost,
    Spending,
    Share,
    Power,
    About,
    Memos,
    Memo,
    Reader,
    Talk,
    Chat,
    Pokedex,
    Web,
    Link,
    Signal,
    Conversation,
    Calls,
    CallChoice,
    Call,
    // The window of a call that rings here.
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
    Key,
    Safe,
}

impl Screen {
    pub(in crate::ui) const fn parent(self) -> Option<Screen> {
        Some(match self {
            Screen::Roll(kind) => Screen::Grid(kind),
            Screen::BellEntry => Screen::Bell,
            Screen::Rundown => Screen::Rundowns,
            Screen::Spending => Screen::Cost,
            Screen::Share => Screen::Spending,
            Screen::Reader => Screen::Memo,
            Screen::Keyboard | Screen::Known => Screen::Networks,
            Screen::Memo | Screen::Talk => Screen::Memos,
            Screen::Conversation => Screen::Signal,
            Screen::CallChoice => Screen::Calls,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
pub(super) enum WindowContent<'a> {
    Lines(List),
    Scan,
    Secret { text: Option<&'a str>, is_typing: bool, is_shown: bool, keys: KeyboardId },
}

pub(super) enum View<'a> {
    Home {
        sentence: &'a str,
    },
    Lines {
        list: List,
        name: &'a str,
        words: BarWords<'a>,
    },
    Debug {
        list: List,
        name: &'a str,
        words: BarWords<'a>,
    },
    Cards {
        grid: &'a Grid,
        store: &'a Store<LinePicture>,
        name: &'a str,
        words: BarWords<'a>,
    },
    Palettes {
        name: &'a str,
        words: BarWords<'a>,
    },
    Gauges {
        name: &'a str,
        words: BarWords<'a>,
        gauges: [Option<Gauge<'a>>; 2],
    },
    Clock {
        name: &'a str,
        words: BarWords<'a>,
        scale: Scale<'a>,
    },
    Text {
        name: &'a str,
        words: BarWords<'a>,
        room: RoomPicture<'a>,
        keys: Option<(KeyboardId, f32)>,
    },
    Conversation {
        name: &'a str,
        words: BarWords<'a>,
        room: RoomPicture<'a>,
        keys: Option<(KeyboardId, f32)>,
        is_lit: Option<bool>,
    },
    Chat {
        name: &'a str,
        words: BarWords<'a>,
        caption: &'a str,
        hint: &'a str,
    },
    Password {
        name: &'a str,
        words: BarWords<'a>,
        keys: KeyboardId,
        is_shown: bool,
    },
    Window {
        words: BarWords<'a>,
        content: WindowContent<'a>,
    },
    Link {
        name: &'a str,
        words: BarWords<'a>,
        code: Option<&'a Symbol>,
        caption: &'a str,
    },
    // A call: its lines, or where it is a video call the far side's picture and this side's.
    Call {
        name: &'a str,
        words: BarWords<'a>,
        is_video: bool,
    },
    Finder(Kind),
    Gallery {
        kind: Kind,
        word: &'a str,
        has_note: bool,
        caption: &'a str,
    },
}

pub(super) const fn seconds_since(start_ns: u64, now_ns: u64) -> u64 {
    now_ns.saturating_sub(start_ns) / NANOS_PER_SECOND
}

#[derive(Default)]
pub(crate) struct Stopwatch {
    pub(in crate::ui) start_ns: Option<u64>,
    seconds: Option<u64>,
}

impl Stopwatch {
    pub(super) fn due(&self, does_show: bool) -> Option<u64> {
        let seconds = self.seconds.map_or(0, |seconds| seconds + 1);
        self.start_ns.filter(|_| does_show).map(|start_ns| start_ns + seconds * NANOS_PER_SECOND)
    }

    pub(super) fn update(&mut self, does_show: bool, now_ns: u64) -> bool {
        let seconds = self.start_ns.filter(|_| does_show).map(|start_ns| seconds_since(start_ns, now_ns));
        core::mem::replace(&mut self.seconds, seconds) != seconds && does_show
    }
}

// Numbered so an answer to an earlier round is known.
#[derive(Default)]
struct Round {
    number: u32,
    is_on: bool,
}

impl Round {
    const fn begin(&mut self) {
        (self.number, self.is_on) = (self.number.wrapping_add(1), true);
    }

    fn current(&self) -> Option<u32> {
        self.is_on.then_some(self.number)
    }

    fn note_phase(&mut self, number: u32, previous: Phase, phase: Phase) -> bool {
        if number != self.number || previous == phase {
            return false;
        }
        self.is_on &= !matches!(phase, Phase::Idle | Phase::Failed(_));
        true
    }
}

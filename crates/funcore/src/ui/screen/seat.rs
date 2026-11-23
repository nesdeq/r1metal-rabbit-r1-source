use super::Screen;
use super::voice::PageId;
use crate::base::math::round;
use crate::domain::gallery::Kind;
use crate::domain::settings::Tool;
use crate::domain::uses::Use;
use crate::domain::voice::Phase;
use crate::gfx::motion::{Edge, REST_DISTANCE};
use crate::input::pose;
use crate::ui::robot::{Activity, News, Reaction, Seat, Sight};
use crate::ui::ruler::diagonal;
use crate::ui::{Action, Ui};
use core::f32::consts::TAU;

const UPRIGHT_TOLERANCE: f32 = 5.0 / 360.0;
const UPRIGHT_RETURN: f32 = 4.0 / 360.0;
const HOLD_NS: u64 = 3_000_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) enum Beckon {
    Talk,
    Flip,
    Look,
    Read,
    Mute,
    Keys,
}

#[derive(Default)]
pub(in crate::ui) struct RobotPlace {
    pub(in crate::ui) is_shown: bool,
    pub(in crate::ui) beckon: Option<Beckon>,
    pub(in crate::ui) at: [Edge; 4],
    pub(in crate::ui) quarters: u8,
    pub(in crate::ui) upright: Edge,
    pub(in crate::ui) aim: f32,
    pub(in crate::ui) is_following: bool,
    pub(in crate::ui) is_planted: bool,
}

impl Ui {
    pub(in crate::ui) const fn does_robot_live(&self) -> bool {
        self.settings.has_robot && (!self.is_awaited || self.safe.window.is_open)
    }

    pub(in crate::ui) fn hush_robot(&mut self, is_hushed: bool, now_ns: u64) {
        self.react(if is_hushed { Reaction::Hush } else { Reaction::Unhush }, now_ns);
    }

    pub(in crate::ui) fn react(&mut self, reaction: Reaction, now_ns: u64) {
        if self.does_robot_live() {
            self.robot.react(reaction, now_ns);
        }
    }

    pub(in crate::ui) fn activity(&self) -> Activity {
        let session = if self.is_talking() { self.translator.session } else { Phase::Idle };
        let reading = if self.is_voiced() { self.aloud.phase } else { Phase::Idle };
        let phases = || [session, reading].into_iter().chain(self.voices().map(|voice| voice.0.phase));
        let is_at = |target: Phase| phases().any(|phase| phase == target);
        let is_filming = self.finder.recording.is_some() && self.screen == Some(Screen::Finder(Kind::Video));
        let is_radio = self.radio().is_some() || self.joining().is_some() || self.face.is_asking;
        let is_hearing =
            self.memos.recording.start_ns.is_some() || self.memos.playing.is_some() || is_at(Phase::Listening);
        let is_writing = self.memos.writing.is_some()
            || self.memos.summarizing.is_some()
            || phases().any(|phase| matches!(phase, Phase::Waking | Phase::Writing | Phase::Thinking));
        let is_connecting = is_at(Phase::Connecting) || self.key_check().is_some();
        if is_filming {
            Activity::Filming
        } else if is_at(Phase::Speaking) {
            Activity::Speaking
        } else if is_radio || is_hearing {
            Activity::Listening
        } else if is_at(Phase::Searching) {
            Activity::Searching
        } else if self.painting.drawing.is_some() {
            Activity::Painting
        } else if is_writing || is_connecting {
            Activity::Thinking
        } else {
            Activity::Idle
        }
    }

    pub(in crate::ui) fn is_on_robot(&self, point: (i32, i32)) -> bool {
        Seat { turn: self.robot_place.upright.at, ..self.robot_seat() }.contains(point)
    }

    pub(in crate::ui) fn robot_sees(&mut self, sight: Sight, now_ns: u64) {
        if self.robot_place.is_shown && self.do_stages_show() && !self.is_at_home() {
            self.robot.glance(sight, now_ns);
        }
    }

    pub(in crate::ui) fn robot_feels(&mut self, (x, y): (i32, i32), now_ns: u64) {
        if !self.robot_place.is_shown || !self.is_at_home() {
            return;
        }
        if self.is_on_robot((x, y)) {
            return self.react(Reaction::Boop, now_ns);
        }
        let (offset_x, offset_y) = self.seat().offset((x as f32, y as f32));
        self.robot.notice(offset_x, offset_y, now_ns);
    }

    /// True while the robot stands at home, out of the bar, on a lit screen.
    pub fn tilts(&self) -> bool {
        self.does_robot_live() && self.is_at_home() && self.has_home_seat() && !self.is_dark
    }

    /// Takes the device's tilt; true if the robot at home turns for it.
    #[must_use]
    pub fn set_tilt(&mut self, x: i32, y: i32) -> bool {
        let Some(turn) = pose::upright_turn(x, y).filter(|_| self.tilts()) else {
            return false;
        };
        let tolerance = if self.robot_place.is_following { UPRIGHT_RETURN } else { UPRIGHT_TOLERANCE };
        self.robot_place.is_following = turn.abs() > tolerance;
        let turn = if self.robot_place.is_following { turn } else { 0.0 };
        let turn = turn + round(self.robot_place.upright.at - turn) as f32;
        if (turn - self.robot_place.aim).abs() < self.upright_grain() {
            return false;
        }
        self.robot_place.aim = turn;
        true
    }

    /// Takes whether the device was picked up; true if the robot at home notices.
    #[must_use]
    pub fn take_lift(&mut self, is_lifted: bool) -> bool {
        let is_noticed = is_lifted && self.tilts() && self.robot_place.is_shown;
        if is_noticed {
            self.robot.news = Some(News::Lifted);
        }
        is_noticed
    }

    pub(in crate::ui) fn upright_grain(&self) -> f32 {
        let [_, _, width, height] = self.robot_place.at.map(|edge| edge.at);
        REST_DISTANCE / (TAU * (diagonal(width, height) / 2.0).max(1.0))
    }

    pub(in crate::ui) fn robot_beckon(&self) -> Option<Beckon> {
        match self.screen {
            None if self.is_at_home() => Some(Beckon::Talk),
            Some(Screen::Grid(Kind::Photo) | Screen::Roll(Kind::Photo)) => Some(Beckon::Flip),
            Some(Screen::Finder(Kind::Photo)) if self.settings.tools.is_on(Tool::Camera) && !self.finder.is_looking => {
                Some(Beckon::Look)
            }
            Some(Screen::Reader | Screen::Web) if self.does_read_aloud() => Some(Beckon::Read),
            Some(Screen::Chat) => Some(Beckon::Mute),
            Some(Screen::Call) if self.is_call_talking() => Some(Beckon::Mute),
            Some(Screen::Pokedex | Screen::Conversation) => Some(Beckon::Keys),
            _ => None,
        }
    }

    pub(in crate::ui) fn act_on_robot(&mut self, beckon: Beckon, now_ns: u64) -> Option<Action> {
        match beckon {
            Beckon::Talk => return self.run_action(Use::Chat, now_ns),
            Beckon::Flip => self.flip(now_ns),
            Beckon::Look => return Some(self.shoot_for_robot(now_ns)),
            Beckon::Read => self.touch_aloud(),
            Beckon::Mute if self.screen == Some(Screen::Call) => self.mute_call(now_ns),
            Beckon::Mute => self.toggle_mute(now_ns),
            Beckon::Keys if self.does_conversation_show() => self.toggle_conversation_keys(now_ns),
            Beckon::Keys => self.toggle_paged_keys(PageId::Pokedex, now_ns),
        }
        None
    }

    pub(in crate::ui) fn hold_due(&self) -> Option<u64> {
        let touch = self.touch.as_ref().filter(|touch| !touch.is_spent && self.does_conversation_show())?;
        (self.is_on_robot(touch.start) && self.is_on_robot(touch.at)).then_some(touch.start_ns + HOLD_NS)
    }

    pub(in crate::ui) fn hold_on_robot(&mut self, now_ns: u64) {
        if self.hold_due().is_none_or(|due| now_ns < due) {
            return;
        }
        if let Some(touch) = self.touch.as_mut() {
            touch.is_spent = true;
        }
        self.switch_auto(now_ns);
    }
}

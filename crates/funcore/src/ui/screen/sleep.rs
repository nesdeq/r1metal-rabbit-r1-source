use super::action::Action;
use super::typing::KeyboardId;
use crate::domain::console::command::Link;
use crate::domain::rundown;
use crate::hw::power::doze::Doze;
use crate::input::Event;
use crate::ui::Ui;
use alloc::string::String;

pub(crate) const IDLE_TO_SLEEP_NS: u64 = 60_000_000_000;
pub(crate) const DARK_UNDER_TASK_NS: u64 = 15_000_000_000;

#[derive(Default)]
pub(crate) struct ConsoleRadio {
    // The console's asks for the core and for a network, by number: the radio spends a wish it could not meet, and an ask anew goes out again.
    power: Option<u32>,
    link: Option<u32>,
    is_cell_only: bool,
    asks: u32,
}

impl ConsoleRadio {
    const fn next_ask(&mut self) -> u32 {
        self.asks = self.asks.wrapping_add(1);
        self.asks
    }
}

impl Ui {
    pub(crate) fn is_kept_awake(&self) -> bool {
        self.charge.is_plugged || self.is_in_use()
    }

    pub(crate) fn is_in_use(&self) -> bool {
        let does_sound = self.is_playing()
            || self.memos.playing.is_some()
            || self.is_signal_sounding()
            || self.is_voiced()
            || self.signal_reading().is_some()
            || self.does_ring()
            || self.does_call_stand()
            || self.voices().iter().any(|voice| voice.0.keeps_awake());
        self.finder.recording.is_some() || self.memos.recording.start_ns.is_some() || does_sound || self.is_rundown_on()
    }

    pub const fn is_awake(&self) -> bool {
        self.doze.is_none()
    }

    /// The device's doze, for the kernel's part of it.
    pub const fn doze_mut(&mut self) -> Option<&mut Doze> {
        self.doze.as_mut()
    }

    /// Notes the device's wake; returns the wake's log line.
    pub fn note_wake(&mut self, now_ns: u64, is_for_cable: bool) -> Option<String> {
        let wake_line = self.doze.take().map(|doze| doze.to_wake_line(now_ns));
        (self.used_at, self.input_at) = (Some(now_ns), Some(now_ns));
        (self.is_dark, self.is_touch_swallowed) = (false, false);
        if is_for_cable {
            self.robot.rouse(now_ns);
        } else {
            self.robot.wake(now_ns);
            self.press_waking(true, now_ns);
        }
        wake_line
    }

    pub(crate) fn fall_asleep(&mut self, now_ns: u64) -> Action {
        self.doze = Some(Doze::new(self.is_button_down, self.charge.is_plugged, now_ns));
        self.let_go(now_ns);
        self.let_go_staged();
        (self.used_at, self.touch, self.passage, self.finder.is_looking) = (Some(now_ns), None, None, false);
        self.end_debug();
        self.debug.rundown = rundown::Status::Off;
        KeyboardId::ALL.into_iter().for_each(|id| self.keyboard_mut(id).release());
        let radio = &mut self.console_radio;
        (radio.power, radio.link, radio.is_cell_only) = (None, None, false);
        self.is_camera_held = false;
        Action::Sleep
    }

    /// Takes the console's request for the radio.
    pub const fn set_console_radio(&mut self, is_on: bool) {
        let ask = self.console_radio.next_ask();
        self.console_radio.power = if is_on && self.is_awake() { Some(ask) } else { None };
    }

    /// Takes the console's request for a network.
    pub const fn set_console_link(&mut self, link: Link) {
        let (ask, is_awake) = (self.console_radio.next_ask(), self.is_awake());
        self.console_radio.link = if !matches!(link, Link::Off) && is_awake { Some(ask) } else { None };
        self.console_radio.is_cell_only = matches!(link, Link::Cell) && is_awake;
    }

    /// Takes the console's request for a call's camera with no call; true if the screen changed.
    #[must_use]
    pub const fn set_console_camera(&mut self, is_on: bool) -> bool {
        self.is_camera_held = is_on && self.is_awake();
        // Its pictures show, where they do, as they come (`note_own_frame`).
        false
    }

    /// The console's asks for the radio and for a network, by number, and whether it wants the modem's network alone.
    pub const fn console_radio(&self) -> (Option<u32>, Option<u32>, bool) {
        (self.console_radio.power, self.console_radio.link, self.console_radio.is_cell_only)
    }

    pub const fn is_dark(&self) -> bool {
        self.is_dark
    }

    pub(crate) fn is_unwatched(&self) -> bool {
        self.is_voiced()
            || self.memos.recording.start_ns.is_some()
            || self.memos.playing.is_some()
            || self.is_talking()
            || self.is_chat_unwatched()
            || self.is_call_unwatched()
    }

    pub(crate) fn darken(&mut self, now_ns: u64) {
        let input_ns = *self.input_at.get_or_insert(now_ns);
        let is_due = self.is_unwatched() && now_ns.saturating_sub(input_ns) >= DARK_UNDER_TASK_NS;
        self.is_dark = !self.charge.is_plugged && (self.is_dark || is_due);
    }

    pub(super) fn swallow(&mut self, event: Event) -> Option<bool> {
        let was_dark = core::mem::replace(&mut self.is_dark, false);
        let (is_down, is_up) = (matches!(event, Event::TouchAt { .. }), event == Event::TouchUp);
        self.is_touch_swallowed |= was_dark && is_down;
        let is_swallowed = self.is_touch_swallowed && (is_down || is_up);
        self.is_touch_swallowed &= !is_up;
        (was_dark || is_swallowed).then_some(was_dark)
    }
}

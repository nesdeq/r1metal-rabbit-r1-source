use super::bar::BarWords;
use super::typing::KeyboardId;
use super::{BACKWARD, Screen, View};
use crate::cell::modem::Status;
use crate::cell::sim::{PIN_DIGITS, Pin};
use crate::domain::scene::SIM_PIN;
use crate::ui::Ui;
use crate::ui::keys::Keyboard;
use crate::ui::robot::Reaction;
use alloc::string::String;
use core::fmt::Write;

const SPEECH: &str = "The SIM's PIN?";

pub(crate) struct SimPin {
    pub(in crate::ui) keys: Keyboard,
    pub(in crate::ui) is_shown: bool,
    // What was typed, until the main loop takes it.
    typed: Option<Pin>,
}

impl SimPin {
    pub(crate) const fn new(keys: Keyboard) -> SimPin {
        SimPin { keys, is_shown: false, typed: None }
    }
}

fn write_speech(_: &Ui, text: &mut String) {
    text.push_str(SPEECH);
}

// The SIM's wait for its PIN, with the tries it has left.
pub(super) fn write_tries(text: &mut String, tries: u8) -> core::fmt::Result {
    write!(text, "NEEDED, {tries} TRIES LEFT")
}

impl Ui {
    /// The PIN typed for the SIM since this was last asked.
    pub fn take_pin(&mut self) -> Option<Pin> {
        self.pin.typed.take()
    }

    pub(super) fn open_pin(&mut self) {
        self.pin.keys.begin_digits(*PIN_DIGITS.start(), *PIN_DIGITS.end());
        (self.screen, self.pin.is_shown) = (Some(Screen::Pin), false);
    }

    pub(super) fn enter_pin(&mut self, now_ns: u64) {
        self.pin.typed = Pin::parse(&self.pin.keys.take_text());
        self.react(Reaction::Nod, now_ns);
        self.depart(BACKWARD);
        self.return_to_menu(now_ns);
    }

    // What the SIM PIN line tells: the tries left while the SIM waits for its PIN.
    pub(super) fn write_pin_word(&self, text: &mut String) {
        if let Some(Status::Pin(tries)) = self.modem.filter(|_| self.settings.is_cellular) {
            let _ = write_tries(text, tries);
        }
    }

    pub(super) fn view_pin(&self) -> View<'_> {
        let words = BarWords { speech: Some(write_speech), ..BarWords::default() };
        View::Password { name: SIM_PIN, words, keys: KeyboardId::Pin, is_shown: self.pin.is_shown }
    }
}

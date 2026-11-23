use super::Screen;
use crate::ui::Ui;
use crate::ui::keys::Keyboard;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyboardId {
    Password,
    Pin,
    Passphrase,
    Chat,
    Pokedex,
    Signal,
    Web,
}

impl KeyboardId {
    pub(super) const ALL: [KeyboardId; 7] = [
        KeyboardId::Password,
        KeyboardId::Pin,
        KeyboardId::Passphrase,
        KeyboardId::Chat,
        KeyboardId::Pokedex,
        KeyboardId::Signal,
        KeyboardId::Web,
    ];
}

impl Ui {
    pub(in crate::ui) fn keyboard(&self, id: KeyboardId) -> &Keyboard {
        match id {
            KeyboardId::Password => &self.networks.keyboard,
            KeyboardId::Pin => &self.pin.keys,
            KeyboardId::Passphrase => &self.safe.keys,
            KeyboardId::Chat => &self.chat.keys,
            KeyboardId::Pokedex => &self.pokedex.keys,
            KeyboardId::Signal => &self.signal.keys,
            KeyboardId::Web => &self.web.keys,
        }
    }

    pub(in crate::ui) fn keyboard_mut(&mut self, id: KeyboardId) -> &mut Keyboard {
        match id {
            KeyboardId::Password => &mut self.networks.keyboard,
            KeyboardId::Pin => &mut self.pin.keys,
            KeyboardId::Passphrase => &mut self.safe.keys,
            KeyboardId::Chat => &mut self.chat.keys,
            KeyboardId::Pokedex => &mut self.pokedex.keys,
            KeyboardId::Signal => &mut self.signal.keys,
            KeyboardId::Web => &mut self.web.keys,
        }
    }

    pub(in crate::ui) fn keys_up(&self) -> Option<KeyboardId> {
        match self.entered_screen() {
            Some(Screen::Keyboard) => Some(KeyboardId::Password),
            Some(Screen::Pin) => Some(KeyboardId::Pin),
            Some(Screen::Safe) => Some(KeyboardId::Passphrase),
            Some(Screen::Chat) if self.chat.is_muted => Some(KeyboardId::Chat),
            Some(Screen::Pokedex) if self.pokedex.is_typing => Some(KeyboardId::Pokedex),
            Some(Screen::Conversation) if self.signal.is_typing => Some(KeyboardId::Signal),
            Some(Screen::Web) if self.web.is_typing => Some(KeyboardId::Web),
            _ => None,
        }
    }

    pub(in crate::ui) fn is_typing(&self) -> bool {
        self.keys_up().is_some_and(|id| id != KeyboardId::Passphrase || self.is_safe_typing())
    }

    pub(in crate::ui) fn is_typing_text(&self) -> bool {
        matches!(self.keys_up(), Some(KeyboardId::Chat | KeyboardId::Pokedex | KeyboardId::Signal | KeyboardId::Web))
    }

    pub(in crate::ui) fn on_keys(&self, y: i32) -> Option<KeyboardId> {
        self.keys_up().filter(|id| y >= self.keyboard(*id).top())
    }

    pub(in crate::ui) fn end_typing(&mut self, id: KeyboardId, now_ns: u64) {
        match id {
            KeyboardId::Password => self.enter_password(),
            KeyboardId::Pin => self.enter_pin(now_ns),
            KeyboardId::Passphrase => self.enter_passphrase(),
            KeyboardId::Chat => self.send_chat(),
            KeyboardId::Pokedex => self.send_pokedex(now_ns),
            KeyboardId::Signal => self.send_conversation(),
            KeyboardId::Web => self.send_web(now_ns),
        }
    }
}

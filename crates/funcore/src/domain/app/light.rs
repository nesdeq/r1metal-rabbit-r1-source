//! The screen's light as the main loop keeps it.

use alloc::format;
use alloc::string::String;

/// The log line of a backlight whose device has not appeared.
pub const NO_DEVICE_LINE: &str = "backlight: waiting for the device";

/// The log line of the screen going dark under a task, or lit again.
pub const fn to_dark_line(is_dark: bool) -> &'static str {
    if is_dark { "screen: dark under a task nobody watches" } else { "screen: lit" }
}

/// The log line of a brightness set.
pub fn to_level_line(brightness: u8, level: u32) -> String {
    format!("backlight: {brightness} %, level {level}")
}

/// The screen's light.
#[derive(Default)]
pub struct Light {
    is_waiting_for_picture: bool,
    brightness: Option<u8>,
    has_no_device: bool,
    is_unlit: bool,
}

impl Light {
    /// The screen comes back: it is lit once a picture is up.
    pub const fn relight(&mut self) {
        self.is_waiting_for_picture = true;
        self.brightness = None;
    }

    pub const fn note_device(&mut self) {
        self.has_no_device = false;
    }

    /// Takes whether the interface wants the screen dark under a task nobody watches; the change where there is one.
    pub const fn set_dark(&mut self, is_awake: bool, is_dark: bool) -> Option<bool> {
        if !is_awake || is_dark == self.is_unlit {
            return None;
        }
        self.is_unlit = is_dark;
        Some(is_dark)
    }

    /// True while the screen shows nothing.
    pub const fn is_blank(&self, is_awake: bool) -> bool {
        !is_awake || self.is_unlit
    }

    /// True while the panel is to be powered up: the screen comes back and no picture is up yet.
    pub const fn is_coming_back(&self, is_awake: bool) -> bool {
        is_awake && !self.is_unlit && self.is_waiting_for_picture
    }

    /// A frame is up: the light may be set.
    pub const fn note_frame(&mut self) {
        self.is_waiting_for_picture = false;
    }

    /// The brightness to set now.
    pub fn level(&self, is_awake: bool, target: u8) -> Option<u8> {
        let can_set = is_awake && !self.is_waiting_for_picture && !self.is_unlit && !self.has_no_device;
        (can_set && self.brightness != Some(target)).then_some(target)
    }

    pub const fn note_level(&mut self, brightness: u8, has_device: bool) {
        if has_device {
            self.brightness = Some(brightness);
        } else {
            self.has_no_device = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_stages() {
        let mut light = Light::default();
        let brightness = crate::domain::settings::Settings::default().brightness;
        assert_eq!(
            light.level(true, brightness),
            Some(brightness),
            "the default's brightness is set with the first picture"
        );
        assert_eq!(light.level(true, 60), Some(60));
        light.note_level(60, true);
        assert_eq!(light.level(true, 60), None);
        assert_eq!((light.set_dark(false, true), light.set_dark(true, false)), (None, None), "asleep, or no change");
        assert_eq!(light.set_dark(true, true), Some(true));
        assert!(light.is_unlit && light.level(true, 70).is_none());
        assert_eq!(light.set_dark(true, false), Some(false), "it comes back");
        light.relight();
        assert!(light.level(true, 60).is_none(), "lit once a picture is up");
        light.note_frame();
        assert_eq!(light.level(true, 60), Some(60), "the light is set anew");
        light.note_level(60, false);
        assert_eq!(light.level(true, 60), None, "a backlight whose device has not appeared is waited for");
        light.note_device();
        assert_eq!((light.level(true, 60), light.level(false, 60)), (Some(60), None));
        light.note_level(60, true);
        assert_eq!(
            (light.level(true, 60), light.level(true, 80)),
            (None, Some(80)),
            "a brightness set stands, and another one, as the kept settings bring, is set"
        );
    }

    #[test]
    fn panel_comes_back() {
        let mut light = Light::default();
        assert!(!light.is_coming_back(true), "a screen that never went dark");
        light.relight();
        assert!(light.is_coming_back(true) && !light.is_coming_back(false), "awake, before the picture");
        light.set_dark(true, true);
        assert!(!light.is_coming_back(true), "not while dark under a task");
        light.set_dark(true, false);
        light.note_frame();
        assert!(!light.is_coming_back(true), "on once a picture is up");
    }

    #[test]
    fn light_lines() {
        assert_eq!(
            (to_dark_line(true), to_dark_line(false)),
            ("screen: dark under a task nobody watches", "screen: lit")
        );
        assert_eq!(to_level_line(60, 128), "backlight: 60 %, level 128");
    }
}

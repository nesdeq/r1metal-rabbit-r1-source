//! Events of the R1's controls, device independent, and their log line.

pub mod evdev;
pub mod hub;
mod lift;
pub mod pose;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const EVENT_LINE: &str = "event ";
const KEYBOARD_TOUCH: &str = "TouchAt on a keyboard";

/// The text of the console's line of an event, as the input file takes it.
pub fn to_event_text(line: &str) -> &str {
    &line[EVENT_LINE.len()..]
}

/// The log line of an input device added.
pub fn to_added_line(name: &str) -> String {
    format!("input added: {name}")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    PttDown,
    PttUp,
    // Detents turned; positive is the direction the kernel reports as KEY_DOWN.
    Wheel(i32),
    TouchAt { x: i32, y: i32 },
    TouchUp,
}

impl Event {
    pub(crate) const fn button_down(self) -> Option<bool> {
        match self {
            Event::PttDown => Some(true),
            Event::PttUp => Some(false),
            _ => None,
        }
    }
}

struct Device<F> {
    path: String,
    name: String,
    file: Option<F>,
}

/// The event devices, each in a place it keeps for as long as the app runs, `F` an open device.
pub struct Devices<F>(Vec<Device<F>>);

impl<F> Default for Devices<F> {
    fn default() -> Devices<F> {
        Devices(Vec::new())
    }
}

impl<F> Devices<F> {
    /// The place a device opens at; none for a path that is no event device's, or one that is open.
    pub fn open_place(&self, path: &str) -> Option<usize> {
        let node = path.rsplit_once('/').map_or(path, |(_, node)| node);
        let place = self.0.iter().position(|device| device.path == path);
        let is_open = place.is_some_and(|place| self.0[place].file.is_some());
        (evdev::is_event_node(node) && !is_open).then(|| place.unwrap_or(self.0.len()))
    }

    /// Takes a device opened at its place.
    pub fn open(&mut self, place: usize, path: &str, name: String, file: F) {
        let device = Device { path: path.into(), name, file: Some(file) };
        match self.0.get_mut(place) {
            Some(slot) => *slot = device,
            None => self.0.push(device),
        }
    }

    /// A device that is open.
    pub fn file(&self, place: usize) -> Option<&F> {
        self.0.get(place)?.file.as_ref()
    }

    /// Takes away a device that failed.
    pub fn close(&mut self, place: usize) {
        if let Some(device) = self.0.get_mut(place) {
            device.file = None;
        }
    }

    /// The name a device told.
    pub fn name(&self, place: usize) -> &str {
        &self.0[place].name
    }

    /// The names of the devices that are open.
    pub fn names(&self) -> Vec<&str> {
        self.0.iter().filter(|device| device.file.is_some()).map(|device| device.name.as_str()).collect()
    }

    /// The devices that are open, each with its place.
    pub fn open_files(&self) -> impl Iterator<Item = (usize, &F)> {
        self.0.iter().enumerate().filter_map(|(place, device)| Some((place, device.file.as_ref()?)))
    }
}

/// The paths of a directory of devices in the order they are opened at the start.
pub fn to_open_order(mut paths: Vec<String>) -> Vec<String> {
    paths.sort();
    paths
}

// The names of the input devices in words, as the debug screen and the log have them.
pub(crate) fn to_names_text(names: &[&str]) -> String {
    names.join(" ")
}

pub(crate) fn write_event_line(line: &mut String, event: &Event, is_typing: bool) {
    line.clear();
    line.push_str(EVENT_LINE);
    if is_typing && matches!(event, Event::TouchAt { .. }) {
        line.push_str(KEYBOARD_TOUCH);
    } else {
        let _ = write!(line, "{event:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_touch_line() {
        let mut line = String::from("left over");
        write_event_line(&mut line, &Event::TouchAt { x: 37, y: 512 }, true);
        assert_eq!(line, "event TouchAt on a keyboard");
        write_event_line(&mut line, &Event::TouchAt { x: 37, y: 512 }, false);
        assert_eq!(line, "event TouchAt { x: 37, y: 512 }", "elsewhere a finger is told where it is");
        write_event_line(&mut line, &Event::Wheel(-2), true);
        assert_eq!(
            to_event_text(&line),
            "Wheel(-2)",
            "the wheel is told on a keyboard too, and the file gets the line without its beginning"
        );
    }

    #[test]
    fn devices_keep_places() {
        let mut devices = Devices::default();
        assert_eq!(
            (devices.open_place("/dev/input/mice"), devices.open_place("/dev/event/js0"), devices.open_place("event")),
            (None, None, Some(0)),
            "an event device by its node's name, not by the directory it lies in"
        );
        assert_eq!(devices.open_place("/dev/input/event3"), Some(0));
        devices.open(0, "/dev/input/event3", String::from("keys"), 7);
        assert_eq!((devices.open_place("/dev/input/event3"), devices.open_place("/dev/input/event4")), (None, Some(1)));
        devices.open(1, "/dev/input/event4", String::from("touch"), 8);
        assert_eq!((devices.names(), devices.file(1)), (alloc::vec!["keys", "touch"], Some(&8)));
        devices.close(0);
        assert_eq!((devices.open_place("/dev/input/event3"), devices.names()), (Some(0), alloc::vec!["touch"]));
        devices.open(0, "/dev/input/event3", String::from("back"), 9);
        assert_eq!(
            (devices.name(0), devices.open_files().collect::<Vec<_>>()),
            ("back", alloc::vec![(0, &9), (1, &8)])
        );
    }

    #[test]
    fn button_events() {
        assert_eq!(
            [Event::PttDown, Event::PttUp, Event::Wheel(1), Event::TouchAt { x: 0, y: 0 }, Event::TouchUp]
                .map(Event::button_down),
            [Some(true), Some(false), None, None, None]
        );
    }
}

//! Evdev records decoded into events: the side button, the wheel, and the touch panel's frames.

use super::Event;
use alloc::vec::Vec;

const NODE_PREFIX: &str = "event";

/// struct input_event on a 64 bit kernel: a 16 byte timeval, then type, code and value.
pub const RECORD_SIZE: usize = 24;
pub const TYPE_OFFSET: usize = 16;
pub const CODE_OFFSET: usize = 18;
pub const VALUE_OFFSET: usize = 20;

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const SYN_MT_REPORT: u16 = 2;
// The kernel's word that a device's queue overflowed and records are lost (Documentation/input/event-codes.rst).
const SYN_DROPPED: u16 = 3;
/// The bytes of a device's key states as `EVIOCGKEY` gives them: a bit a key code, to `KEY_MAX`, 0x2ff.
pub const KEY_STATE_BYTES: usize = 0x300 / 8;
const KEY_UP: u16 = 103;
const KEY_DOWN: u16 = 108;
const KEY_POWER: u16 = 116;
const BTN_TOUCH: u16 = 0x14a;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const KEY_PRESS: i32 = 1;
const KEY_RELEASE: i32 = 0;
const NO_TRACKING_ID: i32 = -1;

#[derive(Clone, Copy, Default)]
struct Contact {
    id: Option<i32>,
    x: Option<i32>,
    y: Option<i32>,
}

impl Contact {
    const fn is_reported(&self) -> bool {
        self.id.is_some() || self.x.is_some() || self.y.is_some()
    }
}

/// What the devices hold down, as each tells its keys, for a decoder whose queue dropped records.
#[derive(Default)]
pub struct Held {
    is_button: bool,
    is_finger: bool,
}

impl Held {
    /// Takes a device's key states.
    pub fn add(&mut self, keys: &[u8; KEY_STATE_BYTES]) {
        let is_down = |key: u16| keys[usize::from(key / 8)] & 1 << (key % 8) != 0;
        self.is_button |= is_down(KEY_POWER);
        self.is_finger |= is_down(BTN_TOUCH);
    }
}

/// Turns the raw event stream of all devices into events, merging duplicate reporters.
#[derive(Default)]
pub struct Decoder {
    // A queue dropped records: what follows up to the frame's end is of a frame cut short; then the devices are asked what they hold.
    is_dropping: bool,
    has_dropped: bool,
    is_ptt_down: bool,
    is_touching: bool,
    position: Option<(i32, i32)>,
    has_rested: bool,
    current: Contact,
    contacts: Vec<Contact>,
    tracked_id: Option<i32>,
    frame_ids: Vec<i32>,
    other_ids: Vec<i32>,
    x: Option<i32>,
    y: Option<i32>,
    frame_has_position: bool,
    frame_has_contact_end: bool,
    is_frame_lifted: bool,
}

impl Decoder {
    pub fn feed(&mut self, kind: u16, code: u16, value: i32) -> Option<Event> {
        if self.is_dropping {
            if (kind, code) == (EV_SYN, SYN_REPORT) {
                (self.is_dropping, self.has_dropped) = (false, true);
                (self.current, self.frame_has_position) = (Contact::default(), false);
                (self.frame_has_contact_end, self.is_frame_lifted) = (false, false);
                self.contacts.clear();
            }
            return None;
        }
        match (kind, code) {
            (EV_SYN, SYN_DROPPED) => self.is_dropping = true,
            (EV_KEY, KEY_POWER) => {
                let is_pressed = match value {
                    KEY_PRESS => true,
                    KEY_RELEASE => false,
                    _ => return None,
                };
                if core::mem::replace(&mut self.is_ptt_down, is_pressed) != is_pressed {
                    return Some(if is_pressed { Event::PttDown } else { Event::PttUp });
                }
            }
            (EV_KEY, KEY_UP) if value == KEY_PRESS => return Some(Event::Wheel(-1)),
            (EV_KEY, KEY_DOWN) if value == KEY_PRESS => return Some(Event::Wheel(1)),
            (EV_KEY, BTN_TOUCH) if value == KEY_RELEASE => self.is_frame_lifted = true,
            (EV_ABS, ABS_MT_TRACKING_ID) if value == NO_TRACKING_ID => self.is_frame_lifted = true,
            (EV_ABS, ABS_MT_TRACKING_ID) if value > NO_TRACKING_ID => self.current.id = Some(value),
            (EV_ABS, ABS_MT_POSITION_X) => {
                self.current.x = Some(value);
                if !self.frame_has_contact_end {
                    self.x = Some(value);
                    self.frame_has_position = true;
                }
            }
            (EV_ABS, ABS_MT_POSITION_Y) => {
                self.current.y = Some(value);
                if !self.frame_has_contact_end {
                    self.y = Some(value);
                    self.frame_has_position = true;
                }
            }
            (EV_SYN, SYN_MT_REPORT) => {
                self.frame_has_contact_end = true;
                let contact = core::mem::take(&mut self.current);
                if contact.is_reported() {
                    self.contacts.push(contact);
                }
            }
            (EV_SYN, SYN_REPORT) => return self.end_frame(),
            _ => {}
        }
        None
    }

    fn end_frame(&mut self) -> Option<Event> {
        let has_position = core::mem::take(&mut self.frame_has_position);
        let is_list_empty = core::mem::take(&mut self.frame_has_contact_end) && !has_position;
        let is_lifted = core::mem::take(&mut self.is_frame_lifted) || is_list_empty;
        let mut contacts = core::mem::take(&mut self.contacts);
        // A panel that reports no lists ends its one contact with the frame.
        let last = core::mem::take(&mut self.current);
        if contacts.is_empty() && last.is_reported() {
            contacts.push(last);
        }
        let event = self.decode_frame(&contacts, has_position, is_lifted);
        contacts.clear();
        self.contacts = contacts;
        event
    }

    fn decode_frame(&mut self, contacts: &[Contact], has_position: bool, is_lifted: bool) -> Option<Event> {
        if is_lifted {
            (self.position, self.tracked_id) = (None, None);
            self.frame_ids.clear();
            self.other_ids.clear();
            return core::mem::take(&mut self.is_touching).then_some(Event::TouchUp);
        }
        if contacts.iter().any(|contact| contact.id.is_some()) {
            return self.follow(contacts);
        }
        match (has_position, self.x, self.y) {
            (true, Some(x), Some(y)) => self.report(x, y),
            _ => None,
        }
    }

    fn follow(&mut self, contacts: &[Contact]) -> Option<Event> {
        let event = match self.tracked_id {
            Some(id) => match contacts.iter().find(|contact| contact.id == Some(id)) {
                Some(contact) => self.report_contact(contact),
                None => {
                    (self.tracked_id, self.position) = (None, None);
                    self.other_ids.clear();
                    self.other_ids.extend(
                        contacts.iter().filter_map(|contact| contact.id).filter(|id| self.frame_ids.contains(id)),
                    );
                    core::mem::take(&mut self.is_touching).then_some(Event::TouchUp)
                }
            },
            None => {
                self.other_ids.retain(|id| contacts.iter().any(|contact| contact.id == Some(*id)));
                let new = contacts.iter().find(|contact| contact.id.is_some_and(|id| !self.other_ids.contains(&id)));
                new.and_then(|contact| {
                    (self.tracked_id, self.position) = (contact.id, None);
                    self.report_contact(contact)
                })
            }
        };
        self.frame_ids.clear();
        self.frame_ids.extend(contacts.iter().filter_map(|contact| contact.id));
        // A finger on the screen that makes no event, the followed one resting or another one that is none of the touch, is still there.
        self.has_rested |= event.is_none() && !contacts.is_empty();
        event
    }

    fn report_contact(&mut self, contact: &Contact) -> Option<Event> {
        let (Some(x), Some(y)) = (contact.x, contact.y) else {
            return None;
        };
        self.report(x, y)
    }

    fn report(&mut self, x: i32, y: i32) -> Option<Event> {
        self.is_touching = true;
        if self.position.replace((x, y)) == Some((x, y)) {
            self.has_rested = true;
            return None;
        }
        Some(Event::TouchAt { x, y })
    }

    /// True, once, if a finger rested on the screen since the last call.
    pub fn take_rest(&mut self) -> bool {
        core::mem::take(&mut self.has_rested)
    }

    /// True, once, where a queue dropped records and the frame it cut short has ended: the devices are to be asked what they hold.
    pub fn take_drop(&mut self) -> bool {
        core::mem::take(&mut self.has_dropped)
    }

    /// What the dropped records left untold, by what the devices hold now: a press that began or ended unseen, and the end of a touch whose finger no device holds; a finger still down shows in its next frame.
    pub fn resync(&mut self, held: &Held) -> [Option<Event>; 2] {
        let is_changed = core::mem::replace(&mut self.is_ptt_down, held.is_button) != held.is_button;
        let press = is_changed.then_some(if held.is_button { Event::PttDown } else { Event::PttUp });
        [press, if held.is_finger { None } else { self.decode_frame(&[], false, true) }]
    }
}

pub(super) fn is_event_node(name: &str) -> bool {
    name.starts_with(NODE_PREFIX)
}

fn bytes_at<const N: usize>(record: &[u8; RECORD_SIZE], at: usize) -> [u8; N] {
    core::array::from_fn(|index| record[at + index])
}

/// A record's type, code and value.
pub fn parse_record(record: &[u8; RECORD_SIZE]) -> (u16, u16, i32) {
    (
        u16::from_ne_bytes(bytes_at(record, TYPE_OFFSET)),
        u16::from_ne_bytes(bytes_at(record, CODE_OFFSET)),
        i32::from_ne_bytes(bytes_at(record, VALUE_OFFSET)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: [Event; 0] = [];

    fn at(x: i32, y: i32) -> Event {
        Event::TouchAt { x, y }
    }

    fn run(decoder: &mut Decoder, raw: &[(u16, u16, i32)]) -> Vec<Event> {
        raw.iter().filter_map(|&(kind, code, value)| decoder.feed(kind, code, value)).collect()
    }

    #[test]
    fn event_nodes() {
        assert!(is_event_node("event3"));
        assert!(!is_event_node("mice") && !is_event_node("js0"));
    }

    #[test]
    fn button_and_wheel() {
        let mut decoder = Decoder::default();
        let presses = [(EV_KEY, KEY_POWER, 1), (EV_SYN, SYN_REPORT, 0), (EV_KEY, KEY_POWER, 1), (EV_KEY, KEY_POWER, 2)];
        assert_eq!(run(&mut decoder, &presses), [Event::PttDown], "two drivers report the press, and it repeats");
        assert_eq!(run(&mut decoder, &[(EV_KEY, KEY_POWER, 0), (EV_KEY, KEY_POWER, 0)]), [Event::PttUp]);
        let turns = [(EV_KEY, KEY_DOWN, 1), (EV_KEY, KEY_DOWN, 0), (EV_KEY, KEY_UP, 1), (EV_KEY, KEY_UP, 0)];
        assert_eq!(run(&mut decoder, &turns), [Event::Wheel(1), Event::Wheel(-1)], "a detent on a press only");
    }

    #[test]
    fn dropped_records() {
        let held = |keys: &[u16]| {
            let (mut held, mut states) = (Held::default(), [0u8; KEY_STATE_BYTES]);
            keys.iter().for_each(|key| states[usize::from(key / 8)] |= 1 << (key % 8));
            held.add(&states);
            held
        };
        let mut decoder = Decoder::default();
        let down = [(EV_KEY, KEY_POWER, 1), (EV_ABS, ABS_MT_TRACKING_ID, 0)];
        let place = [(EV_ABS, ABS_MT_POSITION_X, 10), (EV_ABS, ABS_MT_POSITION_Y, 20), (EV_SYN, SYN_REPORT, 0)];
        assert_eq!(run(&mut decoder, &[&down[..], &place[..]].concat()), [Event::PttDown, at(10, 20)]);
        // The queue overflowed as the finger lifted and the button was let go: of the lift's frame its end alone is left.
        let cut = [(EV_SYN, SYN_DROPPED, 0), (EV_ABS, ABS_MT_POSITION_X, 99), (EV_KEY, KEY_POWER, 0)];
        assert_eq!(run(&mut decoder, &cut), NONE, "what follows a drop is of a frame cut short, and tells nothing");
        assert!(!decoder.take_drop(), "nor is anything asked before that frame has ended");
        assert_eq!(run(&mut decoder, &[(EV_SYN, SYN_REPORT, 0)]), NONE);
        assert!(decoder.take_drop() && !decoder.take_drop(), "then the devices are asked what they hold, once");
        assert_eq!(
            decoder.resync(&held(&[])),
            [Some(Event::PttUp), Some(Event::TouchUp)],
            "they hold nothing: the press and the touch ended unseen"
        );
        assert_eq!(decoder.resync(&held(&[])), [None, None], "which is told once");
        assert_eq!(run(&mut decoder, &place), [at(10, 20)], "and a finger that comes down there is a new touch");
        assert_eq!(
            decoder.resync(&held(&[KEY_POWER, BTN_TOUCH])),
            [Some(Event::PttDown), None],
            "a press that began unseen is told, and a finger a device still holds keeps its touch"
        );
        let moved = [(EV_ABS, ABS_MT_POSITION_X, 11), (EV_ABS, ABS_MT_POSITION_Y, 20), (EV_SYN, SYN_REPORT, 0)];
        assert_eq!(run(&mut decoder, &moved), [at(11, 20)], "which goes on as it moves");
        assert_eq!(
            decoder.resync(&held(&[KEY_POWER, BTN_TOUCH, KEY_UP])),
            [None, None],
            "nothing changed, nothing told"
        );
    }

    #[test]
    fn rest_and_move() {
        let mut decoder = Decoder::default();
        let frame = |decoder: &mut Decoder, x: i32, y: i32| {
            run(decoder, &[(EV_ABS, ABS_MT_POSITION_X, x), (EV_ABS, ABS_MT_POSITION_Y, y), (EV_SYN, SYN_REPORT, 0)])
        };
        assert_eq!(frame(&mut decoder, 100, 200), [at(100, 200)]);
        assert!(!decoder.take_rest(), "a finger that has reported once has not rested");
        assert_eq!(frame(&mut decoder, 100, 200), NONE, "the place it was at again is no event");
        assert_eq!(frame(&mut decoder, 100, 200), NONE, "however often it reports it");
        assert!(decoder.take_rest() && !decoder.take_rest(), "but the decoder tells once that the finger is there");
        assert_eq!(run(&mut decoder, &[(EV_SYN, SYN_REPORT, 0)]), NONE);
        assert!(!decoder.take_rest(), "a frame that says nothing of a place tells nothing of a finger");
        assert_eq!(frame(&mut decoder, 101, 200), [at(101, 200)], "a pixel along is a move");
        assert_eq!(frame(&mut decoder, 100, 200), [at(100, 200)], "and so is the way back to where it was");
        let lift = run(&mut decoder, &[(EV_ABS, ABS_MT_TRACKING_ID, -1), (EV_SYN, SYN_REPORT, 0)]);
        assert_eq!(lift, [Event::TouchUp]);
        assert_eq!(
            frame(&mut decoder, 100, 200),
            [at(100, 200)],
            "a finger that comes down where the last one lifted from is a new touch, and an event"
        );
    }

    fn send_list(decoder: &mut Decoder, contacts: &[(i32, i32, i32)], is_number_first: bool) -> Vec<Event> {
        let mut raw = Vec::new();
        for &(id, x, y) in contacts {
            let (place, number) =
                ([(EV_ABS, ABS_MT_POSITION_X, x), (EV_ABS, ABS_MT_POSITION_Y, y)], (EV_ABS, ABS_MT_TRACKING_ID, id));
            if is_number_first {
                raw.push(number);
                raw.extend(place);
            } else {
                raw.extend(place);
                raw.push(number);
            }
            raw.push((EV_SYN, SYN_MT_REPORT, 0));
        }
        raw.push((EV_SYN, SYN_REPORT, 0));
        run(decoder, &raw)
    }

    fn send_empty(decoder: &mut Decoder) -> Vec<Event> {
        run(decoder, &[(EV_KEY, BTN_TOUCH, 0), (EV_SYN, SYN_MT_REPORT, 0), (EV_SYN, SYN_REPORT, 0)])
    }

    #[test]
    fn first_finger_keeps_touch() {
        for is_number_first in [false, true] {
            let mut decoder = Decoder::default();
            let mut frame = |contacts: &[(i32, i32, i32)]| send_list(&mut decoder, contacts, is_number_first);
            assert_eq!(frame(&[(0, 100, 200)]), [at(100, 200)], "the first finger");
            assert_eq!(frame(&[(0, 100, 200), (1, 400, 500)]), NONE, "a second comes down: no event");
            assert_eq!(frame(&[(1, 400, 500), (0, 100, 200)]), NONE, "the panel lists them the other way round");
            assert_eq!(
                frame(&[(1, 401, 502), (0, 110, 205)]),
                [at(110, 205)],
                "both move, and the first finger's place is the touch's, though the second is listed first"
            );
            assert_eq!(frame(&[(0, 111, 205), (1, 402, 503)]), [at(111, 205)]);
            assert_eq!(frame(&[(1, 403, 504), (0, 111, 205)]), NONE, "the first finger rests, the second moves");
            assert!(decoder.take_rest(), "and there are fingers on the screen");
        }
    }

    #[test]
    fn touch_ends_with_first_finger() {
        let mut decoder = Decoder::default();
        assert_eq!(send_list(&mut decoder, &[(0, 100, 200)], false), [at(100, 200)]);
        assert_eq!(send_list(&mut decoder, &[(1, 300, 400), (0, 100, 200)], false), NONE);
        assert_eq!(
            send_list(&mut decoder, &[(1, 300, 400)], false),
            [Event::TouchUp],
            "its number is gone from the frame while another finger stays: the touch is over"
        );
        decoder.take_rest();
        for step in 0..5 {
            assert_eq!(
                send_list(&mut decoder, &[(1, 300 + step, 400 + 2 * step)], false),
                NONE,
                "the finger that stayed moves {step}: it is no touch and takes the gesture over from nobody"
            );
        }
        assert!(decoder.take_rest(), "though it is on the screen");
        assert_eq!(send_empty(&mut decoder), NONE, "and it lifts: the touch ended before, and ends no more");
        assert_eq!(
            send_list(&mut decoder, &[(2, 50, 60)], false),
            [at(50, 60)],
            "a finger that comes down after that is a touch"
        );
        assert_eq!(send_empty(&mut decoder), [Event::TouchUp]);
    }

    #[test]
    fn new_finger_beside_other() {
        let mut decoder = Decoder::default();
        send_list(&mut decoder, &[(0, 100, 200), (1, 300, 400)], true);
        assert_eq!(send_list(&mut decoder, &[(1, 300, 400)], true), [Event::TouchUp]);
        assert_eq!(
            send_list(&mut decoder, &[(1, 300, 400), (0, 50, 60)], true),
            [at(50, 60)],
            "the first finger's number given to a new finger while the other stays: a new touch"
        );
        assert_eq!(send_list(&mut decoder, &[(0, 52, 60), (1, 310, 410)], true), [at(52, 60)]);
        assert_eq!(send_list(&mut decoder, &[(0, 52, 60)], true), NONE, "the other finger lifts: nothing");
        assert_eq!(send_empty(&mut decoder), [Event::TouchUp]);
    }

    #[test]
    fn finger_in_lift_frame() {
        let mut decoder = Decoder::default();
        send_list(&mut decoder, &[(0, 100, 200), (1, 300, 400)], false);
        assert_eq!(send_list(&mut decoder, &[(1, 300, 400), (2, 20, 30)], false), [Event::TouchUp]);
        assert_eq!(send_list(&mut decoder, &[(1, 300, 400), (2, 20, 30)], false), [at(20, 30)]);
    }

    fn send_unnumbered(decoder: &mut Decoder, contacts: &[(i32, i32)]) -> Vec<Event> {
        let entry = |&(x, y): &(i32, i32)| {
            [(EV_ABS, ABS_MT_POSITION_X, x), (EV_ABS, ABS_MT_POSITION_Y, y), (EV_SYN, SYN_MT_REPORT, 0)]
        };
        let mut raw: Vec<(u16, u16, i32)> = contacts.iter().flat_map(entry).collect();
        raw.push((EV_SYN, SYN_REPORT, 0));
        run(decoder, &raw)
    }

    #[test]
    fn unnumbered_contacts() {
        let mut decoder = Decoder::default();
        let empty = [(EV_SYN, SYN_MT_REPORT, 0), (EV_SYN, SYN_REPORT, 0)];
        assert_eq!(send_unnumbered(&mut decoder, &[(10, 20), (300, 400)]), [at(10, 20)]);
        assert_eq!(send_unnumbered(&mut decoder, &[(10, 20), (300, 400)]), NONE, "the same again is a rest");
        assert!(decoder.take_rest());
        assert_eq!(send_unnumbered(&mut decoder, &[(12, 20), (300, 400)]), [at(12, 20)]);
        assert_eq!(
            send_unnumbered(&mut decoder, &[(300, 400), (12, 20)]),
            [at(300, 400)],
            "without numbers nothing tells the fingers apart: the first listed is the touch"
        );
        assert_eq!(run(&mut decoder, &empty), [Event::TouchUp], "an empty list lifts it");
        assert!(!decoder.take_rest(), "a lift is no rest");
        assert_eq!(run(&mut decoder, &empty), NONE, "once");
        send_unnumbered(&mut decoder, &[(10, 20)]);
        assert_eq!(send_empty(&mut decoder), [Event::TouchUp]);
    }

    #[test]
    fn numbered_once_panel() {
        let mut decoder = Decoder::default();
        let events = run(
            &mut decoder,
            &[
                (EV_ABS, ABS_MT_TRACKING_ID, 3),
                (EV_ABS, ABS_MT_POSITION_X, 70),
                (EV_ABS, ABS_MT_POSITION_Y, 80),
                (EV_KEY, BTN_TOUCH, 1),
                (EV_SYN, SYN_REPORT, 0),
                (EV_ABS, ABS_MT_POSITION_Y, 90),
                (EV_SYN, SYN_REPORT, 0),
                (EV_ABS, ABS_MT_POSITION_X, 75),
                (EV_SYN, SYN_REPORT, 0),
                (EV_SYN, SYN_REPORT, 0),
                (EV_ABS, ABS_MT_POSITION_Y, 90),
                (EV_SYN, SYN_REPORT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_KEY, BTN_TOUCH, 0),
                (EV_SYN, SYN_REPORT, 0),
            ],
        );
        assert_eq!(events, [at(70, 80), at(70, 90), at(75, 90), Event::TouchUp]);
        assert!(decoder.take_rest(), "the same place again is a finger at rest");
    }
}

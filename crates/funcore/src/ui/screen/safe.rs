use super::bar::BarWords;
use super::input::Touch;
use super::typing::KeyboardId;
use super::{View, WindowContent};
use crate::domain::device::Part;
use crate::domain::scene::{CHECKING, OPEN, PASSPHRASE, SafeStep};
use crate::hw::power::Power;
use crate::input::Event;
use crate::storage::progress::Progress;
use crate::storage::safe::{ATTEMPTS, MAX_LENGTH, MIN_LENGTH, Passphrase, Refusal, Request};
use crate::ui::Ui;
use crate::ui::keys::Keyboard;
use crate::ui::robot::Reaction;
use crate::ui::screen::Screen;
use crate::ui::view::window::Window;
use crate::ui::words::STARTING;

const NOT_SET: &str = "PASSPHRASE NOT SET";
const TYPE: &str = "TYPE PASSPHRASE";
const NEW: &str = "NEW PASSPHRASE";
const FORMATTING: &str = "FORMATTING";
const AGAIN: &str = "TYPE IT AGAIN";
const OPENING: &str = "OPENING";
const ENCRYPTING: &str = "ENCRYPTING";
const CHANGED: &str = "PASSPHRASE CHANGED";
const POWERING_OFF: &str = "POWERING OFF";
const SETTINGS_WAIT_NS: u64 = 1_000_000_000;
const WRONG: &str = "WRONG PASSPHRASE";
const DIFFERENT: &str = "NOT THE SAME";
const NOT_OPENED: &str = "NOT OPENED";
const NOT_MADE: &str = "NOT ENCRYPTED";
const NOT_CHANGED: &str = "NOT CHANGED";
const LENGTH: &str = "6 TO 12 CHARACTERS";
// `LENGTH` names both.
const _: () = assert!(MIN_LENGTH == 6 && MAX_LENGTH == 12);

#[derive(PartialEq, Eq)]
enum Step {
    Waiting(&'static str),
    Typing,
    Choosing,
    Repeating(Passphrase),
    Trying(Passphrase),
    Open,
    // The open safe's key is sealed under a new passphrase, and the window falls.
    Changed,
}

pub(crate) struct Safe {
    pub(crate) window: Window,
    step: Step,
    is_made: bool,
    pub(super) keys: Keyboard,
    number: u32,
    failure: Option<&'static str>,
    wrong_count: u32,
    pub(super) is_shut: bool,
    is_changing: bool,
    current: Option<Passphrase>,
    is_shown: bool,
    open_ns: Option<u64>,
}

impl Safe {
    pub(crate) const fn new(keys: Keyboard) -> Safe {
        Safe {
            window: Window::new(PASSPHRASE),
            step: Step::Open,
            is_made: false,
            keys,
            number: 0,
            failure: None,
            wrong_count: 0,
            is_shut: false,
            is_changing: false,
            current: None,
            is_shown: false,
            open_ns: None,
        }
    }

    const fn is_typing(&self) -> bool {
        matches!(self.step, Step::Typing | Step::Choosing | Step::Repeating(_))
    }

    pub(super) const fn scene_step(&self) -> SafeStep {
        match self.step {
            Step::Waiting(_) => SafeStep::Waiting,
            Step::Typing => SafeStep::Typing,
            Step::Choosing => SafeStep::Choosing,
            Step::Repeating(_) => SafeStep::Repeating,
            Step::Trying(_) => SafeStep::Trying,
            Step::Open | Step::Changed => SafeStep::Open,
        }
    }

    fn ask(&mut self, step: Step) {
        self.step = step;
        self.keys.begin(MIN_LENGTH, MAX_LENGTH);
    }

    fn let_keys_go(&mut self) {
        self.keys.release();
        drop(Passphrase::from(self.keys.take_text()));
    }
}

impl Ui {
    pub(super) const fn is_safe_typing(&self) -> bool {
        self.safe.is_typing()
    }

    /// Notes that the data partition's private directory waits in a shut safe.
    pub fn await_safe(&mut self) {
        self.safe.is_shut = true;
        self.safe.step = Step::Waiting(STARTING);
        self.safe.window.rise_over(None, self.ruler.height);
        (self.screen, self.passage) = (Some(Screen::Safe), None);
    }

    /// Takes the data partition's progress; true if the screen changed.
    #[must_use]
    pub fn set_progress(&mut self, progress: &Progress, now_ns: u64) -> bool {
        let has_fact_changed = self.set_fact(Part::Data, Some(progress.to_word()));
        self.take_progress(progress, now_ns) || has_fact_changed
    }

    fn take_progress(&mut self, progress: &Progress, now_ns: u64) -> bool {
        if !self.safe.is_shut || self.does_safe_give_up() {
            return false;
        }
        match progress {
            Progress::Looking | Progress::Waiting => self.set_safe_wait(STARTING),
            Progress::Formatting => self.set_safe_wait(FORMATTING),
            // The passphrase opened the safe: what is typed is let go of while the safe takes in what was kept plain.
            Progress::Encrypting => self.safe.step = Step::Waiting(ENCRYPTING),
            Progress::Shut { is_made, refused } => return self.note_safe_shut(*is_made, refused, now_ns),
            Progress::Mounted(_) | Progress::Failed(_) => {
                (self.safe.is_shut, self.safe.failure) = (false, None);
                self.safe.step = Step::Open;
                self.safe.let_keys_go();
                let is_mounted = matches!(progress, Progress::Mounted(_));
                if is_mounted {
                    self.react(Reaction::Nod, now_ns);
                }
                self.safe.open_ns = Some(now_ns);
                if !self.is_awaited || !is_mounted {
                    self.drop_safe();
                }
            }
        }
        true
    }

    fn note_safe_shut(&mut self, is_made: bool, outcome: &Option<(u32, Refusal)>, now_ns: u64) -> bool {
        self.safe.is_made = is_made;
        self.safe.window.name = if is_made { TYPE } else { NOT_SET };
        let refusal = match outcome {
            Some((number, refusal)) if *number == self.safe.number => refusal,
            _ if matches!(self.safe.step, Step::Waiting(_)) => {
                self.ask_safe();
                return true;
            }
            _ => return false,
        };
        if !matches!(self.safe.step, Step::Trying(_)) {
            return false;
        }
        self.safe.wrong_count += u32::from(*refusal == Refusal::Wrong);
        if self.does_safe_give_up() {
            self.safe.failure = None;
            self.safe.step = Step::Waiting(POWERING_OFF);
            return true;
        }
        self.safe.failure = Some(match (refusal, is_made) {
            (Refusal::Wrong, _) => WRONG,
            (Refusal::Failed, true) => NOT_OPENED,
            (Refusal::Failed, false) => NOT_MADE,
        });
        self.ask_safe();
        self.react(Reaction::Slump, now_ns);
        true
    }

    fn does_safe_wait_for_settings(&self) -> bool {
        self.screen == Some(Screen::Safe) && self.safe.step == Step::Open && !self.safe.window.is_closing
    }

    fn drop_safe(&mut self) {
        let screen = core::mem::replace(&mut self.screen, self.safe.window.beneath);
        self.tree_home(None);
        self.screen = screen;
        (self.safe.window.is_closing, self.safe.open_ns) = (true, None);
    }

    pub(crate) fn settle_safe(&mut self) {
        if self.does_safe_wait_for_settings() {
            self.drop_safe();
        }
    }

    pub(crate) fn safe_due(&self) -> Option<u64> {
        self.safe.open_ns.filter(|_| self.does_safe_wait_for_settings()).map(|open_ns| open_ns + SETTINGS_WAIT_NS)
    }

    pub(crate) fn tick_safe(&mut self, now_ns: u64) {
        if self.safe_due().is_some_and(|due| now_ns >= due) {
            self.drop_safe();
        }
    }

    const fn does_safe_give_up(&self) -> bool {
        self.safe.wrong_count >= ATTEMPTS
    }

    /// The way down the safe asks of the device.
    pub const fn safe_way_down(&self) -> Option<Power> {
        if self.does_safe_give_up() { Some(Power::PowerOff) } else { None }
    }

    fn set_safe_wait(&mut self, word: &'static str) {
        if matches!(self.safe.step, Step::Waiting(_)) {
            self.safe.step = Step::Waiting(word);
        }
    }

    fn ask_safe(&mut self) {
        self.safe.ask(if self.safe.is_made { Step::Typing } else { Step::Choosing });
    }

    /// The number of the safe's pending request, with the request.
    pub fn safe_request(&self) -> Option<(u32, Request<'_>)> {
        let Step::Trying(passphrase) = &self.safe.step else {
            return None;
        };
        let request = match &self.safe.current {
            Some(current) => Request::Change { current: current.as_str(), new: passphrase.as_str() },
            None => Request::Open(passphrase.as_str()),
        };
        Some((self.safe.number, request))
    }

    pub(super) fn change_passphrase(&mut self) {
        self.safe.current = None;
        (self.safe.is_changing, self.safe.is_made, self.safe.failure, self.safe.is_shown) = (true, true, None, false);
        self.safe.window.name = TYPE;
        self.safe.window.rise_over(self.screen, self.ruler.height);
        self.screen = Some(Screen::Safe);
        self.ask_safe();
    }

    /// Takes the outcome of a change of passphrase; true if the screen changed.
    #[must_use]
    pub fn take_safe_change(&mut self, number: u32, outcome: Result<(), Refusal>, now_ns: u64) -> bool {
        let is_trying = matches!(self.safe.step, Step::Trying(_));
        if !self.safe.is_changing || !is_trying || number != self.safe.number {
            return false;
        }
        self.safe.current = None;
        match outcome {
            Ok(()) => {
                (self.safe.step, self.safe.window.is_closing) = (Step::Changed, true);
                self.safe.let_keys_go();
                self.react(Reaction::Nod, now_ns);
            }
            Err(refusal) => {
                self.safe.failure = Some(if refusal == Refusal::Wrong { WRONG } else { NOT_CHANGED });
                self.safe.window.name = TYPE;
                self.ask_safe();
                self.react(Reaction::Slump, now_ns);
            }
        }
        true
    }

    fn leave_change(&mut self) -> bool {
        if !self.safe.is_changing || !self.safe.is_typing() {
            return false;
        }
        self.safe.current = None;
        self.safe.step = Step::Open;
        (self.safe.failure, self.safe.window.is_closing) = (None, true);
        self.safe.let_keys_go();
        true
    }

    pub(super) fn enter_passphrase(&mut self) {
        let entry = Passphrase::from(self.safe.keys.take_text());
        self.safe.failure = None;
        let try_passphrase = |safe: &mut Safe, passphrase: Passphrase| {
            safe.number = safe.number.wrapping_add(1);
            safe.step = Step::Trying(passphrase);
        };
        match core::mem::replace(&mut self.safe.step, Step::Open) {
            Step::Typing if self.safe.is_changing => {
                self.safe.current = Some(entry);
                self.safe.window.name = NEW;
                self.safe.ask(Step::Choosing);
            }
            Step::Typing => try_passphrase(&mut self.safe, entry),
            Step::Choosing => self.safe.ask(Step::Repeating(entry)),
            Step::Repeating(first) if first == entry => try_passphrase(&mut self.safe, entry),
            Step::Repeating(_) => {
                self.safe.failure = Some(DIFFERENT);
                self.safe.ask(Step::Choosing);
            }
            step => self.safe.step = step,
        }
    }

    pub(super) fn handle_safe(&mut self, event: Event, now_ns: u64) {
        let (is_typing, size) = (self.safe.is_typing(), (self.ruler.width, self.ruler.height));
        match event {
            Event::TouchAt { x, y } => {
                let touch = self.touch.get_or_insert(Touch::down((x, y), now_ns, size));
                touch.at = (x, y);
                let (start_y, is_swiped) = (touch.start.1, !touch.is_spent && touch.is_swiped(size));
                if let Some(id) = self.on_keys(start_y).filter(|_| is_typing) {
                    self.keyboard_mut(id).touch(x, y, now_ns);
                } else if is_swiped && self.leave_change() {
                    self.touch.iter_mut().for_each(|touch| touch.is_spent = true);
                }
            }
            Event::TouchUp => match self.touch.take() {
                Some(touch) if let Some(id) = self.on_keys(touch.start.1).filter(|_| is_typing) => {
                    if self.keyboard_mut(id).lift() {
                        self.end_typing(id, now_ns);
                    }
                }
                Some(touch) if !touch.is_spent && self.is_on_eye((touch.start, touch.at)) => {
                    self.safe.is_shown = !self.safe.is_shown;
                }
                _ => {}
            },
            Event::Wheel(detents) if is_typing => self.safe.keys.wheel(detents),
            Event::PttUp if is_typing && self.safe.keys.ask_done() => self.enter_passphrase(),
            _ => {}
        }
    }

    pub(crate) fn safe_gone(&mut self) {
        (self.safe.window.is_closing, self.safe.is_changing) = (false, false);
        self.deal_menu();
        self.keep_asking();
    }

    pub(super) fn view_safe(&self) -> View<'_> {
        let word = self.safe.failure.unwrap_or(match self.safe.step {
            Step::Waiting(word) => word,
            Step::Typing => "",
            Step::Choosing => LENGTH,
            Step::Repeating(_) => AGAIN,
            Step::Trying(_) if self.safe.is_changing => CHECKING,
            Step::Trying(_) if self.safe.is_made => OPENING,
            Step::Trying(_) => ENCRYPTING,
            Step::Open => OPEN,
            Step::Changed => CHANGED,
        });
        let is_typing = self.safe.is_typing();
        let text = match &self.safe.step {
            Step::Trying(passphrase) => Some(passphrase.as_str()),
            _ if is_typing => Some(self.safe.keys.text()),
            _ => None,
        };
        let words = BarWords { status: Some(word), ..BarWords::default() };
        let keys = KeyboardId::Passphrase;
        let content = WindowContent::Secret { text, is_typing, is_shown: self.safe.is_shown, keys };
        View::Window { words, content }
    }
}

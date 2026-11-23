use super::action::Action;
use super::bar::BarWords;
use super::{BACKWARD, View, WindowContent};
use crate::domain::camera::Viewfinder;
use crate::domain::cause::Cause;
use crate::domain::chat::tools::Destination;
use crate::domain::gallery::Kind;
use crate::domain::router::{self, Asker, Order};
use crate::domain::scene::{self, CHECKING, KEEPING, KeyStep, SCANNING, Scan, Scene};
use crate::domain::uses::Use;
use crate::gfx::Rect;
use crate::net::wireguard::config::Flaw;
use crate::openai::key::{self, CheckResult, Report, SCAN_COMMAND};
use crate::ui::Ui;
use crate::ui::corner::WIFI_ICON;
use crate::ui::grid::Grid;
use crate::ui::item::Item;
use crate::ui::robot::Reaction;
use crate::ui::screen::Screen;
use crate::ui::view::lines::List;
use crate::ui::view::window::Window;
use crate::ui::words::cause_word;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const SCAN_ICON: &str = icon!("qr_code_scanner");
const DONE_ICON: &str = icon!("check_circle");
const KEY_ICON: &str = icon!("key");
const FORGET_ICON: &str = icon!("delete");
pub(super) const ICONS: [&str; 4] = [SCAN_ICON, DONE_ICON, KEY_ICON, FORGET_ICON];
const NAME: &str = "OPENAI KEY";
const SET_KEY: &str = "SET OPENAI API KEY";
const NONE_YET: &str = "NONE YET";
const KEPT: &str = "KEPT";
const KEY_SET: &str = "KEY SET";
const CONFIG_SET: &str = "CONFIG SET";
const NOT_A_KEY: &str = "NOT A KEY";
const NOT_KEPT: &str = "NOT KEPT";
const NONE_KNOWN: &str = "NONE KNOWN";
const SCAN_CODE: &str = "SCAN QR CODE";
// The window's lines while it asks: the scan's, then the one that opens the networks or forgets the config.
const SCAN_LINE: usize = 0;
const OTHER_LINE: usize = 1;

const fn check_word(result: CheckResult) -> &'static str {
    match result {
        CheckResult::Stored => KEY_SET,
        CheckResult::Refused => "KEY REFUSED",
        CheckResult::Offline => cause_word(Cause::NoNetwork),
        CheckResult::Unanswered => cause_word(Cause::NoAnswer),
        CheckResult::NotStored => NOT_KEPT,
    }
}

const fn flaw_word(flaw: Flaw) -> &'static str {
    match flaw {
        Flaw::NotAConfig => "NOT A CONFIG",
        Flaw::NoAddress => "NO IPV4 ADDRESS",
        Flaw::NoResolver => "NO IPV4 DNS",
        Flaw::NotWhole => "NOT ALL TRAFFIC",
        Flaw::TooNarrow => "MTU TOO SMALL",
        Flaw::Clash => "ADDRESS IN USE",
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Step {
    Asking,
    Scanning,
    Checking(String),
    // A config read off a code is with the router's thread, which keeps it.
    Keeping,
    Done,
}

// The window that takes by a scanned code what the device keeps: the key to OpenAI's services, or a tunnel's config.
pub(crate) struct Keying {
    subject: Scan,
    step: Step,
    pub(crate) window: Window,
    pub(crate) grid: Grid,
    check: u32,
    failure: Option<&'static str>,
    last_scan: Option<Vec<u8>>,
    // What the window asks of the router, until the main loop takes it.
    order: Option<Order>,
}

impl Keying {
    pub(crate) fn new() -> Keying {
        Keying {
            subject: Scan::Key,
            step: Step::Asking,
            window: Window::new(NAME),
            grid: Grid::empty(),
            check: 0,
            failure: None,
            last_scan: None,
            order: None,
        }
    }

    pub(crate) const fn is_listed(&self) -> bool {
        matches!(self.step, Step::Asking | Step::Done)
    }

    pub(super) const fn scene(&self) -> Scene {
        let step = match self.step {
            Step::Asking => KeyStep::Asking,
            Step::Scanning => KeyStep::Scanning,
            Step::Checking(_) => KeyStep::Checking,
            Step::Keeping => KeyStep::Keeping,
            Step::Done => KeyStep::Done,
        };
        match self.subject {
            Scan::Key => Scene::Key(step),
            Scan::Tunnel => Scene::Tunnel(step),
        }
    }
}

fn write_speech(ui: &Ui, text: &mut String) {
    text.push_str(match (ui.keying.subject, &ui.keying.step) {
        (Scan::Key, Step::Asking) if ui.is_keyed == Some(true) => "My OpenAI key works.",
        (Scan::Key, Step::Asking) => "I need an OpenAI key.",
        (Scan::Tunnel, Step::Asking) if ui.has_tunnel == Some(true) => "I keep a tunnel's config.",
        (Scan::Tunnel, Step::Asking) => "I keep no tunnel's config.",
        (_, Step::Scanning) => "Show me the code.",
        (_, Step::Checking(_)) => "I ask OpenAI.",
        (_, Step::Keeping) => "I keep it.",
        (Scan::Key, Step::Done) => "The key works.",
        (Scan::Tunnel, Step::Done) => "The config is kept.",
    });
}

impl Ui {
    /// Whether an OpenAI key is stored.
    pub const fn is_keyed(&self) -> Option<bool> {
        self.is_keyed
    }

    /// Takes the key keeper's report; true if the screen changed.
    #[must_use]
    pub fn take_key_report(&mut self, report: &Report, now_ns: u64) -> bool {
        let is_keyed = report.has_key.is_some_and(|is_keyed| self.set_keyed(is_keyed));
        let is_refused = report.is_refused && self.note_key_refused(now_ns);
        let is_checked = report.check.is_some_and(|(check, result)| self.note_key_checked(check, result, now_ns));
        is_keyed || is_refused || is_checked
    }

    // True while the window shows for that subject and is not on its way down.
    fn does_window_ask(&self, subject: Scan) -> bool {
        self.screen == Some(Screen::Key) && self.keying.subject == subject && !self.keying.window.is_closing
    }

    fn set_keyed(&mut self, is_keyed: bool) -> bool {
        let has_changed = self.is_keyed.replace(is_keyed) != Some(is_keyed);
        if !is_keyed {
            return self.keep_asking();
        }
        if self.does_window_ask(Scan::Key) && !matches!(self.keying.step, Step::Done | Step::Checking(_)) {
            self.finish_key();
            return true;
        }
        if has_changed && self.screen == Some(Screen::Cost) && !self.cost.is_counting {
            self.deal_cost();
            return true;
        }
        false
    }

    fn note_key_refused(&mut self, now_ns: u64) -> bool {
        if !self.does_window_ask(Scan::Key) || self.keying.step != Step::Asking {
            return false;
        }
        self.keying.failure = Some(check_word(CheckResult::Refused));
        self.react(Reaction::Slump, now_ns);
        true
    }

    pub(crate) fn keep_asking(&mut self) -> bool {
        if self.may_key_be_left() || self.is_awaited || self.keying.window.is_open || self.safe.window.is_open {
            return false;
        }
        self.lower_ring();
        (self.passage, self.rolls.stamps.armed) = (None, None);
        self.close_screen();
        self.tree_home(None);
        self.open_key(None);
        true
    }

    fn may_key_be_left(&self) -> bool {
        self.is_keyed != Some(false)
    }

    // True while the key's window is open and may not be left; the tunnel's may always.
    pub(super) fn is_key_held(&self) -> bool {
        self.keying.window.is_open && self.keying.subject == Scan::Key && !self.may_key_be_left()
    }

    fn open_keying(&mut self, subject: Scan, name: &'static str, beneath: Option<Screen>) {
        (self.keying.subject, self.keying.window.name) = (subject, name);
        self.keying.window.rise_over(beneath, self.ruler.height);
        (self.screen, self.keying.last_scan, self.keying.failure) = (Some(Screen::Key), None, None);
        self.ask_key();
    }

    pub(super) fn open_key(&mut self, beneath: Option<Screen>) {
        self.open_keying(Scan::Key, NAME, beneath);
    }

    pub(super) fn open_tunnel(&mut self) {
        self.open_keying(Scan::Tunnel, scene::TUNNEL, None);
    }

    fn ask_key(&mut self) {
        self.keying.step = Step::Asking;
        self.lay_key();
    }

    fn finish_key(&mut self) {
        (self.keying.step, self.keying.failure, self.keying.last_scan) = (Step::Done, None, None);
        self.lay_key();
    }

    fn lay_key(&mut self) {
        let count = (0..).map_while(|index| self.key_item(index)).count();
        let grid = &mut self.keying.grid;
        grid.begin();
        grid.set_items_at(count, 0);
        grid.land();
        self.deal_lines(List::Key, |_| true);
    }

    pub(crate) fn key_item(&self, index: usize) -> Option<Item> {
        match (self.keying.subject, &self.keying.step, index) {
            (Scan::Key, Step::Done, 0) => Some(Item::new("DONE", "THE KEY WORKS").with_icon(DONE_ICON)),
            (Scan::Tunnel, Step::Done, 0) => Some(Item::new("DONE", "THE CONFIG IS KEPT").with_icon(DONE_ICON)),
            (_, Step::Done, _) => None,
            (Scan::Key, _, SCAN_LINE) => {
                Some(Item::lead(SCAN_CODE, format!("MADE BY {SCAN_COMMAND}")).with_icon(SCAN_ICON))
            }
            (Scan::Key, _, OTHER_LINE) => {
                let known = self.networks.first_known();
                Some(Item::lead("WIFI", if known.is_empty() { NONE_KNOWN } else { known }).with_icon(WIFI_ICON))
            }
            (Scan::Tunnel, _, SCAN_LINE) => Some(Item::lead(SCAN_CODE, "OF A WIREGUARD CONFIG").with_icon(SCAN_ICON)),
            (Scan::Tunnel, _, OTHER_LINE) if self.has_tunnel == Some(true) => {
                Some(Item::lead(scene::FORGET, "THE CONFIG THAT IS KEPT").with_icon(FORGET_ICON))
            }
            _ => None,
        }
    }

    pub(super) fn set_key_item(&self) -> Item {
        Item::lead(SET_KEY, if self.is_keyed == Some(true) { KEPT } else { NONE_YET }).with_icon(KEY_ICON)
    }

    // What the tunnel's line tells: whether a config is kept.
    pub(super) fn tunnel_word(&self) -> &'static str {
        if self.has_tunnel == Some(true) { KEPT } else { NONE_YET }
    }

    // Takes whether a config of a tunnel is kept; true where that is news.
    pub(super) fn set_tunnel_kept(&mut self, has_config: bool) -> bool {
        let has_changed = self.has_tunnel.replace(has_config) != Some(has_config);
        if has_changed && self.does_window_ask(Scan::Tunnel) && self.keying.step == Step::Asking {
            self.lay_key();
        }
        has_changed
    }

    // Takes what keeping the config the window read came to; true if the screen changed.
    pub(super) fn note_config_kept(&mut self, asker: Asker, result: &Result<(), String>, now_ns: u64) -> bool {
        if asker != Asker::Window || !self.does_window_ask(Scan::Tunnel) || self.keying.step != Step::Keeping {
            return false;
        }
        if result.is_ok() {
            self.finish_key();
            self.react(Reaction::Nod, now_ns);
        } else {
            (self.keying.failure, self.keying.last_scan) = (Some(NOT_KEPT), None);
            self.ask_key();
            self.react(Reaction::Slump, now_ns);
        }
        true
    }

    /// What the tunnel's window asks of the router since this was last asked: a config to keep, or the kept one forgotten.
    pub fn take_router_order(&mut self) -> Option<Order> {
        self.keying.order.take()
    }

    fn close_key(&mut self) {
        self.keying.window.is_closing = true;
        self.keying.grid.press(false);
    }

    pub(crate) fn key_gone(&mut self) {
        self.keying.window.is_closing = false;
        match self.screen {
            Some(Screen::Cost) => self.deal_cost(),
            None => self.deal_menu(),
            _ => {}
        }
        // A key that went while the tunnel's window showed is asked for now.
        self.keep_asking();
    }

    pub(super) fn click_key(&mut self, now_ns: u64) -> Option<Action> {
        if self.keying.window.is_closing {
            return None;
        }
        self.keying.failure = None;
        match (&self.keying.step, self.keying.subject) {
            (Step::Asking, _) if self.keying.grid.selection == SCAN_LINE => self.scan_key(),
            (Step::Asking, Scan::Key) => return self.run_action(Use::Open(Destination::Wifi), now_ns),
            (Step::Asking, Scan::Tunnel) => self.keying.order = Some(Order::Forget),
            (Step::Scanning, _) => self.ask_key(),
            (Step::Checking(_) | Step::Keeping, _) => {}
            (Step::Done, _) => self.close_key(),
        }
        None
    }

    pub(super) fn wheel_key(&mut self, detents: i32) {
        if self.keying.window.is_closing {
            return;
        }
        match self.keying.step {
            Step::Asking | Step::Done => {
                self.keying.failure = None;
                self.wheel_lines(List::Key, detents);
            }
            Step::Scanning => self.turn_camera(detents),
            Step::Checking(_) | Step::Keeping => {}
        }
    }

    pub(super) fn key_back(&mut self) {
        if self.keying.window.is_closing {
            return;
        }
        match self.keying.step {
            Step::Scanning => {
                self.keying.failure = None;
                self.ask_key();
            }
            Step::Checking(_) | Step::Keeping => {}
            Step::Asking if self.is_key_held() => {}
            Step::Asking | Step::Done => self.close_key(),
        }
    }

    pub(super) fn key_home(&mut self) -> bool {
        if !self.is_key_held() {
            if self.keying.window.is_open {
                (self.keying.window.is_open, self.keying.window.is_closing) = (false, false);
            }
            return false;
        }
        if self.screen != Some(Screen::Key) {
            self.depart(BACKWARD);
            self.screen = Some(Screen::Key);
            self.deal_lines(List::Key, |_| true);
        }
        true
    }

    pub(super) fn return_to_key(&mut self) -> bool {
        if !self.keying.window.is_open || self.screen != Some(Screen::Networks) {
            return false;
        }
        self.screen = Some(Screen::Key);
        self.deal_lines(List::Key, |_| true);
        true
    }

    fn take_key_scan(&mut self, code: &[u8], now_ns: u64) {
        match core::str::from_utf8(code).ok().and_then(key::parse_key) {
            Some(key) => {
                self.keying.check = self.keying.check.wrapping_add(1);
                (self.keying.step, self.keying.failure) = (Step::Checking(key.into()), None);
            }
            None => {
                self.keying.failure = Some(NOT_A_KEY);
                self.react(Reaction::Shake, now_ns);
            }
        }
    }

    fn take_tunnel_scan(&mut self, code: &[u8], now_ns: u64) {
        let read = core::str::from_utf8(code).map_err(|_| Flaw::NotAConfig).and_then(router::read_config);
        match read {
            Ok(config) => {
                self.keying.order = Some(Order::Keep(config, Asker::Window));
                (self.keying.step, self.keying.failure) = (Step::Keeping, None);
            }
            Err(flaw) => {
                self.keying.failure = Some(flaw_word(flaw));
                self.react(Reaction::Shake, now_ns);
            }
        }
    }

    /// Takes a scanned code's bytes; what the window that scans took it for, where the screen changed.
    #[must_use]
    pub fn take_scan(&mut self, code: &[u8], now_ns: u64) -> Option<Scan> {
        if !self.is_key_scanning() || self.keying.last_scan.as_deref() == Some(code) {
            return None;
        }
        self.keying.last_scan = Some(code.into());
        match self.keying.subject {
            Scan::Key => self.take_key_scan(code, now_ns),
            Scan::Tunnel => self.take_tunnel_scan(code, now_ns),
        }
        Some(self.keying.subject)
    }

    /// The number of the window's pending check, with the scanned key.
    pub fn key_check(&self) -> Option<(u32, &str)> {
        match (&self.keying.step, self.screen) {
            (Step::Checking(key), Some(Screen::Key)) => Some((self.keying.check, key.as_str())),
            _ => None,
        }
    }

    fn note_key_checked(&mut self, check: u32, result: CheckResult, now_ns: u64) -> bool {
        if self.key_check().map(|pending| pending.0) != Some(check) {
            return false;
        }
        if result == CheckResult::Stored {
            self.finish_key();
            self.react(Reaction::Nod, now_ns);
            return true;
        }
        self.keying.failure = Some(check_word(result));
        if result == CheckResult::Refused {
            self.scan_key();
        } else {
            self.keying.last_scan = None;
            self.ask_key();
        }
        self.react(Reaction::Slump, now_ns);
        true
    }

    fn scan_key(&mut self) {
        self.keying.step = Step::Scanning;
        self.finder.picture.clear();
        self.finder.id = self.ids.next();
    }

    fn is_key_scanning(&self) -> bool {
        self.screen == Some(Screen::Key) && self.keying.step == Step::Scanning && !self.keying.window.is_closing
    }

    // True while the window shows the scan's picture, as it scans and has what it read checked or kept.
    pub(super) fn does_scan_show(&self) -> bool {
        self.screen == Some(Screen::Key) && !self.keying.is_listed()
    }

    pub(super) fn key_camera(&self) -> Option<Viewfinder> {
        let side = self.scan_side();
        let viewfinder = Viewfinder {
            kind: Kind::Photo,
            facing: self.finder.facing,
            is_scanning: true,
            is_calling: false,
            size: (side, side),
        };
        self.is_key_scanning().then_some(viewfinder)
    }

    pub(crate) fn scan_square(&self) -> Rect {
        let room = self.ruler.under_bar();
        let side = room.w.min(room.h).max(0);
        room.centre(side, side)
    }

    fn scan_side(&self) -> usize {
        self.scan_square().w.max(0) as usize
    }

    pub(super) fn view_key(&self) -> View<'_> {
        let is_kept = match self.keying.subject {
            Scan::Key => self.is_keyed == Some(true),
            Scan::Tunnel => self.has_tunnel == Some(true),
        };
        let silent_status = match (&self.keying.step, self.keying.subject) {
            (Step::Asking, _) if is_kept => KEPT,
            (Step::Asking, _) => NONE_YET,
            (Step::Scanning, _) => SCANNING,
            (Step::Checking(_), _) => CHECKING,
            (Step::Keeping, _) => KEEPING,
            (Step::Done, Scan::Key) => KEY_SET,
            (Step::Done, Scan::Tunnel) => CONFIG_SET,
        };
        let words = BarWords {
            status: self.keying.failure,
            silent_status: Some(silent_status),
            speech: Some(write_speech),
            ..BarWords::default()
        };
        let content = if self.keying.is_listed() { WindowContent::Lines(List::Key) } else { WindowContent::Scan };
        View::Window { words, content }
    }
}

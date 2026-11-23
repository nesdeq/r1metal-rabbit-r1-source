use super::bar::BarWords;
use super::typing::KeyboardId;
use super::{BACKWARD, FORWARD, View};
use crate::domain::chat::tools::Destination;
use crate::domain::uses::Use;
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::item::{Aside, Item};
use crate::ui::keys::Keyboard;
use crate::ui::robot::Reaction;
use crate::ui::screen::Screen;
use crate::ui::view::lines::List;
use crate::ui::words::{JOINING, LISTENING};
use crate::wifi::known::{Access, Change, Entry, Network, Row, TrialError, find_missing, rank, to_rows};
use crate::wifi::places::Place;
use crate::wifi::scan;
use crate::wifi::wpa::PASSWORD_LENGTHS;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

const NAME: &str = "WIFI";
const NETWORK_NAME: &str = "NETWORK";
const PASSWORD_NAME: &str = "PASSWORD";
const RADIO_FAILED: &str = "RADIO FAILED";
const LIST_ACTIONS: usize = 1;
const PAGE_LINES: [PageLine; 3] = [PageLine::Prefer, PageLine::Forget, PageLine::ForgetAll];
pub(super) const ICONS: [&str; 3] = [icon!("check_circle"), icon!("delete"), icon!("delete_sweep")];

const fn failure_word(error: TrialError) -> &'static str {
    match error {
        TrialError::NotHeard => "NOT HEARD",
        TrialError::WrongPassword => "WRONG PASSWORD",
        TrialError::NotLetIn => "NOT LET IN",
        TrialError::NoAddress => "NO ADDRESS",
        TrialError::NoRadio => RADIO_FAILED,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Line {
    Row(usize),
    Absent(usize),
    Listen,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PageLine {
    Prefer,
    Forget,
    ForgetAll,
}

pub(crate) struct Networks {
    pub(crate) keyboard: Keyboard,
    pub(super) is_shown: bool,
    name: String,
    change: Option<(Change, Option<Place>)>,
    trial_change: Option<Change>,
    trial: u32,
    failure: Option<TrialError>,
    pub(crate) list: Grid,
    rows: Vec<Row>,
    pass: u32,
    pub(in crate::ui) is_listening: bool,
    is_radio_failed: bool,
    known: Vec<Entry>,
    absent: Vec<Entry>,
    pub(crate) page: Grid,
    page_entry: Option<(Entry, bool)>,
}

impl Networks {
    pub(crate) fn new(keyboard: Keyboard) -> Networks {
        Networks {
            keyboard,
            is_shown: false,
            name: String::new(),
            change: None,
            trial_change: None,
            trial: 0,
            failure: None,
            list: Grid::empty(),
            rows: Vec::new(),
            pass: 0,
            is_listening: false,
            is_radio_failed: false,
            known: Vec::new(),
            absent: Vec::new(),
            page: Grid::empty(),
            page_entry: None,
        }
    }

    pub(super) fn first_known(&self) -> &str {
        self.known.first().map_or("", |entry| entry.name.as_str())
    }
}

fn write_list_speech(ui: &Ui, text: &mut String) {
    let _ = match ui.networks.rows.len() {
        0 => text.write_str("I hear no network."),
        1 => text.write_str("I hear one network."),
        count => write!(text, "I hear {count} networks."),
    };
}

fn write_known_speech(ui: &Ui, text: &mut String) {
    if let Some((entry, is_heard)) = &ui.networks.page_entry {
        let _ = write!(text, "I know {}{}", entry.name, if *is_heard { ", and hear it." } else { "." });
    }
}

fn write_password_speech(ui: &Ui, text: &mut String) {
    let _ = write!(text, "The password of {}?", ui.networks.name);
}

impl Ui {
    /// The change to the known networks made since this was last asked.
    pub fn take_network_change(&mut self) -> Option<(Change, Option<Place>)> {
        self.networks.change.take()
    }

    /// The number of the pending join, with the change it proves.
    pub fn joining(&self) -> Option<(u32, &Change)> {
        self.pending_trial().filter(|_| self.is_awake())
    }

    fn pending_trial(&self) -> Option<(u32, &Change)> {
        let is_visible = self.screen == Some(Screen::Networks);
        self.networks.trial_change.as_ref().filter(|_| is_visible).map(|change| (self.networks.trial, change))
    }

    // Back on the list from a network's page or the password's keys.
    fn back_to_list(&mut self) {
        if self.screen != Some(Screen::Networks) {
            self.depart(BACKWARD);
        }
        (self.screen, self.networks.page_entry) = (Some(Screen::Networks), None);
    }

    fn attempt(&mut self, change: Change) {
        self.back_to_list();
        (self.networks.trial_change, self.networks.trial, self.networks.failure) =
            (Some(change), self.networks.trial.wrapping_add(1), None);
    }

    /// Takes a join's outcome; true if the screen changed.
    #[must_use]
    pub fn take_join_outcome(&mut self, trial: u32, outcome: Result<Place, TrialError>, now_ns: u64) -> bool {
        if self.pending_trial().map(|pending| pending.0) != Some(trial) {
            return false;
        }
        match (self.networks.trial_change.take(), outcome) {
            (Some(change), Ok(place)) => self.apply_change(change, Some(place), now_ns),
            (_, outcome) => {
                self.networks.failure = outcome.err();
                self.networks.list.swing(1.0);
                self.react(Reaction::Slump, now_ns);
            }
        }
        true
    }

    /// Takes the networks an earlier run stored; true if the screen changed.
    #[must_use]
    pub fn set_known(&mut self, known: Vec<Entry>) -> bool {
        let is_first_new = self.networks.known.first() != known.first();
        self.networks.known = known;
        self.relist();
        let is_line = is_first_new && self.redeal_lines(&[Use::Open(Destination::Wifi)]);
        self.screen == Some(Screen::Networks) || is_line
    }

    fn relist(&mut self) {
        rank(&mut self.networks.rows, &self.networks.known);
        self.networks.absent = find_missing(&self.networks.known, &self.networks.rows);
        if self.networks.list.items.is_some() {
            self.networks.list.redeal();
            self.count_lines();
        }
        self.deal_networks();
    }

    fn count_lines(&mut self) {
        let networks = self.networks.rows.len() + self.networks.absent.len();
        self.networks.list.set_items_at(networks + LIST_ACTIONS, if networks > 0 { LIST_ACTIONS } else { 0 });
    }

    fn line_at(&self, index: usize) -> Line {
        let (rows, absent) = (self.networks.rows.len(), self.networks.absent.len());
        match index.checked_sub(LIST_ACTIONS) {
            Some(at) if at < rows => Line::Row(at),
            Some(at) if at < rows + absent => Line::Absent(at - rows),
            _ => Line::Listen,
        }
    }

    /// True while a network can be had: one is known, or the modem can serve a link.
    pub fn can_link(&self, is_cell_usable: bool) -> bool {
        !self.networks.known.is_empty() || is_cell_usable
    }

    /// The number of the pending pass of listening for networks.
    pub fn radio(&self) -> Option<u32> {
        self.pending_pass().filter(|_| self.is_awake())
    }

    fn pending_pass(&self) -> Option<u32> {
        (self.screen == Some(Screen::Networks) && self.networks.is_listening).then_some(self.networks.pass)
    }

    pub(super) fn open_networks(&mut self) {
        self.networks.list.begin();
        self.stand_dial(0);
        self.listen();
    }

    fn listen(&mut self) {
        self.networks.rows.clear();
        (self.networks.pass, self.networks.is_listening, self.networks.is_radio_failed) =
            (self.networks.pass.wrapping_add(1), true, false);
        (self.networks.trial_change, self.networks.failure) = (None, None);
        self.screen = Some(Screen::Networks);
    }

    /// Takes the networks a pass of listening found; true if the screen changed.
    #[must_use]
    pub fn set_scan(&mut self, pass: u32, networks: Option<&[scan::Network]>) -> bool {
        if self.pending_pass() != Some(pass) {
            return false;
        }
        (self.networks.is_listening, self.networks.is_radio_failed) = (false, networks.is_none());
        self.networks.rows = to_rows(networks.unwrap_or(&[]), &self.networks.known);
        self.networks.absent = find_missing(&self.networks.known, &self.networks.rows);
        self.networks.list.begin();
        self.count_lines();
        self.stand_dial(self.networks.list.selection);
        self.deal_networks();
        true
    }

    pub(crate) fn network_item(&self, index: usize) -> Option<Item> {
        if index >= self.networks.list.items.unwrap_or(0) {
            return None;
        }
        Some(match self.line_at(index) {
            Line::Row(at) => {
                let network = &self.networks.rows[at];
                let is_known = self.networks.known.iter().any(|entry| network.matches(entry));
                let word = match network.access {
                    _ if is_known && at == 0 => "DEFAULT HERE",
                    _ if is_known => "KNOWN",
                    Access::Open => "OPEN",
                    Access::Password => "PASSWORD",
                    Access::Barred => "CANNOT JOIN",
                };
                let has_lead = is_known || network.access == Access::Password;
                let name = network.name.clone();
                let item = if has_lead { Item::lead(name, word) } else { Item::new(name, word) };
                item.with_faint(network.access == Access::Barred).with_aside(Aside::Signal(network.signal))
            }
            Line::Absent(at) => Item::lead(self.networks.absent[at].name.clone(), "KNOWN, NOT HEARD").with_faint(true),
            Line::Listen => Item::new("LISTEN AGAIN", format!("{} HEARD", self.networks.rows.len())),
        })
    }

    fn deal_networks(&mut self) {
        self.deal_lines(List::Networks, |_| true);
    }

    pub(super) fn wheel_networks(&mut self, detents: i32) {
        self.networks.failure = None;
        self.wheel_lines(List::Networks, detents);
    }

    pub(super) fn pick_network(&mut self) {
        let list = &self.networks.list;
        let Some(selection) = list.items.filter(|count| *count > 0).map(|_| list.selection) else {
            return;
        };
        if self.networks.trial_change.is_some() {
            return self.networks.list.swing(1.0);
        }
        self.networks.failure = None;
        match self.line_at(selection) {
            Line::Row(at) => {
                let (access, name) = (self.networks.rows[at].access, self.networks.rows[at].name.clone());
                let known = self.networks.known.iter().find(|entry| self.networks.rows[at].matches(entry)).cloned();
                match (known, access) {
                    (Some(entry), _) => self.open_known(entry, true),
                    (None, Access::Open) => self.attempt(Change::Remember(Network { name, password: String::new() })),
                    (None, Access::Password) => {
                        self.networks.name = name;
                        self.ask_password();
                    }
                    (None, Access::Barred) => self.networks.list.swing(1.0),
                }
            }
            Line::Absent(at) => {
                let entry = self.networks.absent[at].clone();
                self.open_known(entry, false);
            }
            Line::Listen => {
                self.networks.list.leave();
                self.listen();
            }
        }
    }

    fn open_known(&mut self, entry: Entry, is_heard: bool) {
        self.depart(FORWARD);
        self.networks.page_entry = Some((entry, is_heard));
        self.show_lines(List::Known, PAGE_LINES.len(), None);
        self.screen = Some(Screen::Known);
    }

    pub(crate) fn known_item(&self, index: usize) -> Option<Item> {
        let (entry, is_heard) = self.networks.page_entry.as_ref()?;
        let [prefer, forget, forget_all] = ICONS;
        Some(match PAGE_LINES.get(index)? {
            PageLine::Prefer if *is_heard => Item::action("MAKE DEFAULT", "THE NETWORK TO JOIN HERE", prefer),
            PageLine::Prefer => Item::action("MAKE DEFAULT", "NOT HEARD HERE", prefer).with_faint(true),
            PageLine::Forget => {
                let word = if entry.is_locked { "THIS NETWORK AND ITS PASSWORD" } else { "THIS NETWORK" };
                Item::action("FORGET", word, forget)
            }
            PageLine::ForgetAll => {
                Item::action("FORGET ALL", format!("{} KNOWN", self.networks.known.len()), forget_all)
            }
        })
    }

    pub(super) fn act_on_known(&mut self, now_ns: u64) {
        let (Some((entry, is_heard)), Some(line)) =
            (self.networks.page_entry.clone(), PAGE_LINES.get(self.networks.page.selection))
        else {
            return;
        };
        match line {
            PageLine::Prefer if is_heard => self.attempt(Change::Prefer(entry)),
            PageLine::Prefer => self.networks.page.swing(1.0),
            PageLine::Forget => self.apply_change(Change::Forget(entry), None, now_ns),
            PageLine::ForgetAll => self.apply_change(Change::ForgetAll, None, now_ns),
        }
    }

    fn apply_change(&mut self, change: Change, place: Option<Place>, now_ns: u64) {
        self.back_to_list();
        change.apply_to(&mut self.networks.known);
        self.relist();
        let reaction = match change {
            Change::Remember(_) | Change::Prefer(_) => Reaction::Nod,
            Change::Forget(_) | Change::ForgetAll => Reaction::Shake,
        };
        self.react(reaction, now_ns);
        self.networks.change = Some((change, place));
    }

    fn ask_password(&mut self) {
        self.depart(FORWARD);
        self.networks.keyboard.begin(PASSWORD_LENGTHS.0, PASSWORD_LENGTHS.1);
        (self.screen, self.networks.is_shown) = (Some(Screen::Keyboard), false);
    }

    pub(super) fn enter_password(&mut self) {
        let network =
            Network { name: core::mem::take(&mut self.networks.name), password: self.networks.keyboard.take_text() };
        self.attempt(Change::Remember(network));
    }

    pub(super) fn view_networks<'a>(&'a self, word: &'a mut String) -> View<'a> {
        let status = if self.networks.is_listening {
            Some(LISTENING)
        } else if self.networks.trial_change.is_some() {
            Some(JOINING)
        } else if self.networks.is_radio_failed {
            Some(RADIO_FAILED)
        } else {
            self.networks.failure.map(failure_word)
        };
        let networks = self.networks.list.items.map(|count| count.saturating_sub(LIST_ACTIONS));
        let selection = self.networks.list.selection.checked_sub(LIST_ACTIONS);
        let _ = match (networks, selection) {
            (Some(0), _) => word.write_str("NO NETWORKS"),
            (Some(1), None) => word.write_str("1 NETWORK"),
            (Some(networks), None) => write!(word, "{networks} NETWORKS"),
            _ => Ok(()),
        };
        let count: &'a str = word;
        let place = networks.zip(selection).filter(|entry| entry.0 > 0).map(|(networks, at)| (at + 1, networks));
        let subject = Some(count).filter(|count| !count.is_empty());
        let words = BarWords { status, place, subject, speech: Some(write_list_speech), ..BarWords::default() };
        View::Lines { list: List::Networks, name: NAME, words }
    }

    pub(super) fn view_known(&self) -> View<'_> {
        let subject = self.networks.page_entry.as_ref().map_or("", |page| page.0.name.as_str());
        let words = BarWords { subject: Some(subject), speech: Some(write_known_speech), ..BarWords::default() };
        View::Lines { list: List::Known, name: NETWORK_NAME, words }
    }

    pub(super) fn view_keyboard(&self) -> View<'_> {
        let subject = Some(self.networks.name.as_str());
        let words = BarWords { subject, speech: Some(write_password_speech), ..BarWords::default() };
        View::Password { name: PASSWORD_NAME, words, keys: KeyboardId::Password, is_shown: self.networks.is_shown }
    }
}

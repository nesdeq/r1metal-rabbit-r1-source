//! What the settings thread is handed and what it shares.

use super::bell::Bell;
use super::flash::Flash;
use super::news::topics::Topics;
use super::settings::Settings;
use crate::base::handoff::{Orders, Step};
use crate::wifi::known::{Change, Entry, Known};
use crate::wifi::places::{Place, Places};
use crate::wifi::station::SECRET_BYTES;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Display;

const GONE: &str = "the keeper of the settings is gone";
/// The log line of no partition to keep the settings on.
pub const NO_PARTITION_LINE: &str = "settings: no data partition to keep them on";
/// The log line of a flash's stamp stored at last.
pub const STAMP_KEPT_LINE: &str = "settings: the flash's key is kept, and its stamp now too";
/// The log line of a way down that came before all was stored.
pub const UNSETTLED_LINE: &str = "settings: not all was kept in the time a way down waits";

/// The log line of settings read back.
pub fn to_read_line(settings: &Settings) -> String {
    format!("settings: {}, as kept", settings.to_log_line())
}

/// The log line of no settings stored.
pub fn to_unkept_line() -> String {
    format!("settings: none kept, {}", Settings::default().to_log_line())
}

/// The log line of settings written.
pub fn to_written_line(settings: &Settings) -> String {
    format!("settings: kept {}", settings.to_log_line())
}

/// The log line of settings that were not written, and why.
pub fn to_unwritten_line(settings: &Settings, error: &dyn Display) -> String {
    format!("settings: keeping {}: {error}", settings.to_log_line())
}

/// The log line of a flash's choice that was not stored, and why.
pub fn to_flash_unkept_line(flash: &Flash, error: &dyn Display) -> String {
    format!("settings: keeping what flash {} chose: {error}", flash.stamp)
}

/// The log line of a flash's choice stored, `has_key` where it gave a key.
pub fn to_flash_line(flash: &Flash, settings: &Settings, has_key: bool) -> String {
    let key_text = if has_key { ", and gave an OpenAI key, whose keeping its stamp waits for" } else { "" };
    format!("settings: flash {} chose {}{key_text}, kept", flash.stamp, settings.to_log_line())
}

/// An order to the settings thread, done in the order it came.
pub enum Order {
    Keep(Settings),
    Bell(Bell),
    Rundowns(Topics),
    Networks(Change, Option<Place>),
    Place(Place),
    Use(&'static str, u64),
    ForgetUses(&'static str, u64),
    FlashKept,
}

/// What an earlier run stored, and the OpenAI key a flash gave.
pub struct StoredSettings {
    pub settings: Settings,
    pub known: Vec<Entry>,
    pub key: Option<String>,
    pub bell: Bell,
    pub topics: Topics,
}

/// What the settings thread, the readers of what it stores and the main loop share.
#[derive(Default)]
pub struct State {
    pub report: Option<StoredSettings>,
    pub known: Known,
    pub secret: Option<[u8; SECRET_BYTES]>,
    pub places: Places,
    orders: Orders<Order>,
    forget_outcomes: BTreeMap<u64, Result<(), String>>,
    order_count: u64,
    done_count: u64,
    is_serving: bool,
}

impl State {
    /// The stored settings are read: from now on orders are served, and waited for.
    pub fn begin_serving(&mut self, places: Places, stored: StoredSettings) {
        self.places = places;
        self.report = Some(stored);
        self.is_serving = true;
    }

    /// Ends the thread once the orders are done.
    pub const fn close(&mut self) {
        self.orders.close();
    }

    pub fn add_order(&mut self, order: Order) {
        self.order_count += 1;
        self.orders.add(order);
    }

    /// Queues the forgetting of an event's uses, and gives its number.
    pub fn ask_forget(&mut self, event: &'static str) -> u64 {
        self.add_order(Order::ForgetUses(event, self.order_count + 1));
        self.order_count
    }

    /// The thread's next step, none while it has only to wait.
    pub fn next_step(&mut self) -> Option<Step<Order>> {
        self.orders.next_step()
    }

    /// The order taken last is done.
    pub const fn end_order(&mut self) {
        self.done_count += 1;
    }

    /// Takes the outcome of the forgetting of that number.
    pub fn end_forget(&mut self, number: u64, outcome: Result<(), String>) {
        self.forget_outcomes.insert(number, outcome);
    }

    /// The outcome of the forgetting of that number, once there is one.
    pub fn take_forget_outcome(&mut self, number: u64) -> Option<Result<(), String>> {
        self.forget_outcomes.remove(&number).or(self.orders.is_closed().then(|| Err(GONE.into())))
    }

    /// The number of the last order, for whoever waits for all that was handed over: none while orders are not served.
    pub const fn settle_mark(&self) -> Option<u64> {
        if self.is_serving && !self.orders.is_closed() { Some(self.order_count) } else { None }
    }

    /// True once the orders up to that number are done.
    pub const fn is_settled(&self, mark: u64) -> bool {
        self.done_count >= mark
    }

    /// Makes a change to the known networks, and gives them as they are now.
    pub fn change_known(&mut self, change: Change) -> Known {
        self.known.apply(change);
        self.known.clone()
    }

    /// Drops the places of forgotten networks and adds the place of a join a network proved; the places as they are now, where that changed them.
    pub fn change_places(&mut self, place: Option<Place>) -> Option<Places> {
        let has_dropped = self.places.retain_known(&self.known);
        let has_added = place.is_some_and(|place| self.places.add_if_known(place, &self.known));
        (has_dropped || has_added).then(|| self.places.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logs::Logging;
    use crate::wifi::known::Network;

    fn stored() -> StoredSettings {
        StoredSettings {
            settings: Settings::default(),
            known: Vec::new(),
            key: None,
            bell: Bell::default(),
            topics: Topics::default(),
        }
    }

    #[test]
    fn orders_settle() {
        let mut state = State::default();
        state.add_order(Order::FlashKept);
        assert_eq!(state.settle_mark(), None, "nothing is waited for before the stored settings are read");
        state.begin_serving(Places::default(), stored());
        assert!(state.report.is_some(), "what was stored is the report");
        assert_eq!(state.settle_mark(), Some(1), "an order handed over before counts");
        state.add_order(Order::Keep(Settings::default()));
        assert_eq!(state.settle_mark(), Some(2));
        assert!(matches!(state.next_step(), Some(Step::Serve(Order::FlashKept))), "in the order they came");
        state.end_order();
        assert!(state.is_settled(1) && !state.is_settled(2), "settled up to the order done");
        assert!(matches!(state.next_step(), Some(Step::Serve(Order::Keep(_)))) && state.next_step().is_none());
        state.end_order();
        assert!(state.is_settled(2));
        state.close();
        assert_eq!(state.settle_mark(), None, "a keeper that ends is not waited for");
        assert!(matches!(state.next_step(), Some(Step::End)));
    }

    #[test]
    fn forgetting_by_number() {
        let mut state = State::default();
        state.add_order(Order::Use("chat", 5));
        let (first, second) = (state.ask_forget("chat"), state.ask_forget("signal"));
        assert_eq!((first, second), (2, 3), "a forgetting's number is its order's place among all of them");
        assert!(matches!(state.next_step(), Some(Step::Serve(Order::Use("chat", 5)))));
        assert!(matches!(state.next_step(), Some(Step::Serve(Order::ForgetUses("chat", 2)))), "and its order names it");
        assert_eq!(state.take_forget_outcome(first), None, "none before it ended");
        state.end_forget(first, Ok(()));
        assert_eq!((state.take_forget_outcome(first), state.take_forget_outcome(first)), (Some(Ok(())), None), "once");
        state.end_forget(second, Err("no room".into()));
        state.close();
        assert_eq!(state.take_forget_outcome(second), Some(Err("no room".into())), "an outcome there is taken first");
        assert_eq!(
            state.take_forget_outcome(second),
            Some(Err(GONE.into())),
            "a keeper that ended answers whoever still waits"
        );
    }

    #[test]
    fn places_follow_known() {
        let mut state = State::default();
        let home = Network { name: "home".into(), password: "secret12".into() };
        let place = Place { network: home.to_entry(), frequency: 2437, other_band: None };
        assert_eq!(state.change_places(Some(place.clone())), None, "no place of a network that is not known");
        let known = state.change_known(Change::Remember(home.clone()));
        assert_eq!((known.len(), known == state.known), (1, true), "the known networks as they are now");
        let places = state.change_places(Some(place.clone())).expect("the place of a join a known network proved");
        assert_eq!((places.last(), places == state.places), (Some(&place), true));
        assert_eq!(state.change_places(Some(place)), None, "the last join's place as it stands changes nothing");
        assert_eq!(state.change_places(None), None);
        state.change_known(Change::Forget(home.to_entry()));
        assert_eq!(state.change_places(None), Some(Places::default()), "a forgotten network's place goes with it");
    }

    #[test]
    fn log_lines() {
        let settings = Settings { brightness: 80, ..Settings::default() };
        let (told, fresh) = (settings.to_log_line(), Settings::default().to_log_line());
        assert_eq!(to_read_line(&settings), format!("settings: {told}, as kept"));
        assert_eq!(to_unkept_line(), format!("settings: none kept, {fresh}"), "a new device's");
        assert_eq!(to_written_line(&settings), format!("settings: kept {told}"));
        assert_eq!(to_unwritten_line(&settings, &"no room"), format!("settings: keeping {told}: no room"));
        let flash = Flash { stamp: 7, logging: Logging::Off, key: None };
        assert_eq!(to_flash_unkept_line(&flash, &"no room"), "settings: keeping what flash 7 chose: no room");
        assert_eq!(to_flash_line(&flash, &settings, false), format!("settings: flash 7 chose {told}, kept"));
        assert_eq!(
            to_flash_line(&flash, &settings, true),
            format!("settings: flash 7 chose {told}, and gave an OpenAI key, whose keeping its stamp waits for, kept")
        );
    }
}

mod networks;

use crate::log;
use crate::storage::Data;
use crate::sync::Board;
use r1_funcore::base::handoff::Step;
use r1_funcore::domain::bell::{self, Bell};
use r1_funcore::domain::flash::{Flash, STAMP_FILE};
use r1_funcore::domain::keeper::{
    NO_PARTITION_LINE, Order, STAMP_KEPT_LINE, State, StoredSettings, UNSETTLED_LINE, to_flash_line,
    to_flash_unkept_line, to_read_line, to_unkept_line, to_unwritten_line, to_written_line,
};
use r1_funcore::domain::news::topics::{self, Topics};
use r1_funcore::domain::settings::{self, Settings};
use r1_funcore::domain::uses;
use r1_funcore::storage::INTERFACE;
use r1_funcore::wifi::known::{Change, Known};
use r1_funcore::wifi::places::{Place, Places};
use r1_funcore::wifi::station::SECRET_BYTES;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

// The settings an earlier run kept, with their file's text where it was read.
fn read_settings(partition: Option<&Data>) -> (Settings, Option<String>) {
    let Some(data) = partition else {
        log!("{NO_PARTITION_LINE}");
        return (Settings::default(), None);
    };
    match data.text(INTERFACE, settings::FILE) {
        Ok(Some(text)) => {
            let settings = Settings::parse(&text);
            log!("{}", to_read_line(&settings));
            (settings, Some(text))
        }
        Ok(None) => {
            log!("{}", to_unkept_line());
            (Settings::default(), None)
        }
        Err(e) => {
            log!("settings: reading what was kept: {e}");
            (Settings::default(), None)
        }
    }
}

fn write_bell(data: &Data, bell: &Bell) {
    match data.put(INTERFACE, bell::FILE, bell.to_text().as_bytes()) {
        Ok(()) => log!("{}", bell.to_kept_line()),
        Err(e) => log!("bell: keeping what is set: {e}"),
    }
}

// The bell an earlier run kept: what an earlier firmware kept of it among the settings is kept in the bell's own file from here on.
fn read_bell(partition: Option<&Data>, settings: Option<&str>) -> Bell {
    let Some(data) = partition else {
        return Bell::default();
    };
    let kept = match data.text(INTERFACE, bell::FILE) {
        Ok(kept) => kept,
        Err(e) => {
            log!("bell: reading what was kept: {e}");
            return Bell::default();
        }
    };
    let (bell, is_moved) = Bell::read(kept.as_deref(), settings.unwrap_or_default());
    log!("{}", bell.to_read_line());
    if is_moved {
        write_bell(data, &bell);
    }
    bell
}

fn write_topics(data: &Data, kept: &Topics) {
    match data.put(INTERFACE, topics::FILE, kept.to_text().as_bytes()) {
        Ok(()) => log!("{}", kept.to_kept_line()),
        Err(e) => log!("news: keeping the rundowns: {e}"),
    }
}

// The rundowns an earlier run kept: the two switches an earlier firmware kept among the settings are kept in the rundowns' own file from here on.
fn read_topics(partition: Option<&Data>, settings: Option<&str>) -> Topics {
    let Some(data) = partition else {
        return Topics::default();
    };
    let kept = match data.text(INTERFACE, topics::FILE) {
        Ok(kept) => kept,
        Err(e) => {
            log!("news: reading the rundowns that were kept: {e}");
            return Topics::default();
        }
    };
    let (kept, is_moved) = Topics::read(kept.as_deref(), settings.unwrap_or_default());
    log!("{}", kept.to_read_line());
    if is_moved {
        write_topics(data, &kept);
    }
    kept
}

fn apply_flash(
    partition: Option<&Data>,
    settings: Settings,
    flash: Option<Flash>,
) -> (Settings, Option<String>, Option<String>) {
    let Some(mut flash) = flash else {
        return (settings, None, None);
    };
    let Some(data) = partition else {
        return (flash.apply_to(settings), None, None);
    };
    let stored = data.text(INTERFACE, STAMP_FILE).inspect_err(|e| log!("settings: reading the flash: {e}"));
    if !flash.is_new(stored.ok().flatten().as_deref()) {
        return (settings, None, None);
    }
    let (current_stamp, pending_stamp) = flash.stamps_to_keep();
    let (settings, key) = (flash.apply_to(settings), flash.key.take());
    let result =
        data.put(INTERFACE, settings::FILE, settings.to_text().as_bytes()).and_then(|()| match &current_stamp {
            Some(stamp) => data.put(INTERFACE, STAMP_FILE, stamp.as_bytes()),
            None => Ok(()),
        });
    let is_stored = match result {
        Ok(()) => {
            log!("{}", to_flash_line(&flash, &settings, key.is_some()));
            true
        }
        Err(e) => {
            log!("{}", to_flash_unkept_line(&flash, &e));
            false
        }
    };
    (settings, key, pending_stamp.filter(|_| is_stored))
}

fn forget_uses(data: &Data, event: &str) -> io::Result<()> {
    match data.text(uses::DIRECTORY, uses::FILE)?.and_then(|text| uses::without_event(&text, event)) {
        Some(remaining) => data.put(uses::DIRECTORY, uses::FILE, remaining.as_bytes()),
        None => Ok(()),
    }
}

fn work(data: &Data, board: &Board<State>, flash: Option<Flash>) {
    // The partition is waited for here alone; what an earlier run kept is read off it where there is one.
    let partition = data.wait_decided().then_some(data);
    let (kept, text) = read_settings(partition);
    let (settings, key, mut pending_stamp) = apply_flash(partition, kept, flash);
    let bell = read_bell(partition, text.as_deref());
    let topics = read_topics(partition, text.as_deref());
    let known = networks::read_known(partition);
    board.lock().known = known.clone();
    let secret = networks::read_secret(partition);
    board.lock().secret = secret;
    let places = networks::read_places(partition, &known);
    board.lock().begin_serving(places, StoredSettings { settings, known: known.entries(), key, bell, topics });
    board.raise();
    while let Step::Serve(order) = board.wait(State::next_step) {
        match order {
            Order::Keep(stored) => match data.put(INTERFACE, settings::FILE, stored.to_text().as_bytes()) {
                Ok(()) => log!("{}", to_written_line(&stored)),
                Err(e) => log!("{}", to_unwritten_line(&stored, &e)),
            },
            Order::Bell(bell) => write_bell(data, &bell),
            Order::Rundowns(kept) => write_topics(data, &kept),
            Order::Networks(change, place) => {
                let line = change.to_log_line();
                let known = board.lock().change_known(change);
                networks::write_known(data, &known, &line);
                let places = board.lock().change_places(place);
                networks::write_places(data, places.as_ref());
            }
            Order::Place(place) => {
                let places = board.lock().change_places(Some(place));
                networks::write_places(data, places.as_ref());
            }
            Order::Use(event, at_s) => {
                if let Err(e) = data.append(uses::DIRECTORY, uses::FILE, uses::to_line(at_s, event).as_bytes()) {
                    log!("settings: keeping a use of {event}: {e}");
                }
            }
            Order::ForgetUses(event, number) => {
                let result = forget_uses(data, event).inspect_err(|e| log!("settings: forgetting uses: {e}"));
                board.lock().end_forget(number, result.map_err(|e| e.to_string()));
            }
            Order::FlashKept => {
                if let Some(stamp) = pending_stamp.take() {
                    match data.put(INTERFACE, STAMP_FILE, stamp.as_bytes()) {
                        Ok(()) => log!("{STAMP_KEPT_LINE}"),
                        Err(e) => log!("settings: keeping the flash's stamp: {e}"),
                    }
                }
            }
        }
        board.lock().end_order();
        board.notify();
    }
}

fn add_order(board: &Board<State>, order: Order) {
    board.lock().add_order(order);
    board.notify();
}

fn settle(board: &Board<State>, deadline: Instant) {
    let (state, limit) = (board.lock(), deadline.saturating_duration_since(Instant::now()));
    if let Some(mark) = state.settle_mark()
        && !board.wait_while_within(state, limit, |state| !state.is_settled(mark))
    {
        log!("{UNSETTLED_LINE}");
    }
}

/// Read access to the known networks, for whoever joins one.
#[derive(Clone)]
pub struct Networks(Arc<Board<State>>);

impl Networks {
    /// The known networks, the last chosen first.
    pub fn known(&self) -> Known {
        self.0.lock().known.clone()
    }

    /// The name of the last chosen network.
    pub fn latest_name(&self) -> Option<String> {
        self.0.lock().known.first().map(|network| network.name.clone())
    }

    pub fn secret(&self) -> Option<[u8; SECRET_BYTES]> {
        self.0.lock().secret
    }

    /// Where the device joined each known network, the last join first.
    pub fn places(&self) -> Places {
        self.0.lock().places.clone()
    }

    /// Queues a join's place for storing, if its network is still known then.
    pub fn keep_place(&self, place: Place) {
        add_order(&self.0, Order::Place(place));
    }
}

/// The main loop's handle of the thread that stores the settings.
pub struct Keeper {
    board: Arc<Board<State>>,
}

impl Keeper {
    /// Starts the thread, which reads the stored settings from `data` and applies `flash`.
    pub fn start(data: Data, flash: Option<Flash>) -> io::Result<Keeper> {
        let board = Arc::new(Board::new(State::default())?);
        let worker = Arc::clone(&board);
        thread::Builder::new().name("settings".into()).spawn(move || work(&data, &worker, flash))?;
        Ok(Keeper { board })
    }

    /// Readable once the stored settings are read.
    pub fn fd(&self) -> RawFd {
        self.board.fd()
    }

    /// Takes the stored settings, once.
    pub fn take_stored(&self) -> Option<StoredSettings> {
        self.board.take(|state| state.report.take())
    }

    /// Queues `settings` for storing.
    pub fn keep(&self, settings: Settings) {
        add_order(&self.board, Order::Keep(settings));
    }

    /// Queues the bell for storing.
    pub fn keep_bell(&self, bell: Bell) {
        add_order(&self.board, Order::Bell(bell));
    }

    /// Queues the rundowns for storing.
    pub fn keep_rundowns(&self, kept: Topics) {
        add_order(&self.board, Order::Rundowns(kept));
    }

    /// Queues a use of `event` at `at_s`, in UTC seconds, for storing.
    pub fn add_use(&self, event: &'static str, at_s: u64) {
        add_order(&self.board, Order::Use(event, at_s));
    }

    /// Queues the flash's stamp for storing, once the flash's key is stored.
    pub fn keep_flash_stamp(&self) {
        add_order(&self.board, Order::FlashKept);
    }

    /// Queues a change of the known networks, then `place`, the join that proved the network.
    pub fn change(&self, change: Change, place: Option<Place>) {
        add_order(&self.board, Order::Networks(change, place));
    }

    /// The handle for whoever joins a network.
    pub fn networks(&self) -> Networks {
        Networks(Arc::clone(&self.board))
    }

    /// The handle for whoever forgets uses.
    pub fn uses(&self) -> Uses {
        Uses(Arc::clone(&self.board))
    }

    /// Waits until every order is done or `deadline` passes; a thread that waits for the partition is not waited for.
    pub fn settle(&self, deadline: Instant) {
        settle(&self.board, deadline);
    }

    /// The handle for the battery's cutoff.
    pub fn settler(&self) -> Settler {
        Settler(Arc::clone(&self.board))
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.board.lock().close();
        self.board.notify();
    }
}

/// The stored uses, for Signal's unlink.
pub struct Uses(Arc<Board<State>>);

impl Uses {
    /// Removes every use of `event` after the earlier orders; an error if the keeper is gone.
    pub fn forget(&self, event: &'static str) -> io::Result<()> {
        let number = self.0.lock().ask_forget(event);
        self.0.notify();
        self.0.wait(|state| state.take_forget_outcome(number)).map_err(io::Error::other)
    }

    /// Uses of a closed keeper, for tests.
    #[cfg(test)]
    pub fn closed() -> Uses {
        Keeper::start(Data::unusable(), None).unwrap().uses()
    }
}

/// The keeper's settle, for a way down outside the main loop.
pub struct Settler(Arc<Board<State>>);

impl Settler {
    /// Waits as `Keeper::settle` does.
    pub fn settle(&self, deadline: Instant) {
        settle(&self.0, deadline);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Epoll;
    use r1_funcore::logs::Logging;
    use r1_funcore::wifi::known::{Entry, Network};
    use std::fs;

    #[test]
    fn no_partition_gives_defaults() {
        let flash = Flash { stamp: 7, logging: Logging::Off, key: Some("sk-proj-Fake0Key_-9".into()) };
        let keeper = Keeper::start(Data::unusable(), Some(flash)).unwrap();
        let mut epoll = Epoll::with_fd(keeper.fd());
        assert_eq!(epoll.wait(None).unwrap(), vec![7], "the thread says by itself what it found");
        let stored = keeper.take_stored().unwrap();
        assert_eq!(stored.topics, Topics::default(), "the two rundowns a new device has");
        assert_eq!(
            (stored.settings, stored.known, stored.key, keeper.take_stored().is_none()),
            (Settings { logging: Logging::Off, ..Settings::default() }, Vec::new(), None, true),
            "once, with the flash's choice, which nothing kept outweighs, and no key where none can be kept"
        );
        keeper.keep_rundowns(Topics::parse("rundown finance off\n"));
        keeper.keep(Settings {
            brightness: 80,
            volume: 60,
            has_robot: false,
            offset: 120,
            logging: Logging::Off,
            ..Settings::default()
        });
        let place =
            Place { network: Entry { name: "home".into(), is_locked: true }, frequency: 2437, other_band: None };
        keeper.change(
            Change::Remember(Network { name: "home".into(), password: "secret12".into() }),
            Some(place.clone()),
        );
        keeper.networks().keep_place(place);
        keeper.change(Change::ForgetAll, None);
        keeper.keep_flash_stamp();
        assert!(keeper.uses().forget("signal").is_err(), "no uses forgotten where none can be read");
        keeper.settle(Instant::now() + r1_funcore::hw::power::SHUTDOWN_TIMEOUT);
        keeper.settler().settle(Instant::now() + r1_funcore::hw::power::SHUTDOWN_TIMEOUT);
    }

    #[test]
    fn forget_uses_in_order() {
        let root = crate::storage::scratch("uses");
        let keeper = Keeper::start(Data::with_root(&root), None).unwrap();
        keeper.add_use("signal", 1_767_225_600);
        keeper.add_use("photo", 1_767_225_700);
        keeper.add_use("signal", 1_767_225_800);
        keeper.uses().forget("signal").unwrap();
        assert_eq!(fs::read_to_string(root.join(uses::DIRECTORY).join(uses::FILE)).unwrap(), "1767225700 photo\n");
        keeper.uses().forget("signal").unwrap();
        assert!(Uses::closed().forget("signal").is_err(), "a keeper that is gone forgets nothing");
        fs::remove_dir_all(root).unwrap();
    }
}

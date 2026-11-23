//! The interface.

macro_rules! icon {
    ($name:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/material/", $name, ".svg"))
    };
    (older $name:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/material-icons/", $name, ".svg"))
    };
}

mod bar;
mod corner;
mod edit;
mod field;
mod gauge;
mod grid;
mod item;
mod keys;
mod kit;
mod layout;
mod list;
mod picture;
mod robot;
mod ruler;
mod screen;
mod speech;
mod textroom;
mod view;
mod words;

pub use robot::Robot;
pub use screen::action::Action;
pub use screen::chat::Reading;
pub use screen::input::PoseScreen;

use bar::{BarWord, Typing};
use corner::Corner;
use edit::Caret;
use field::TickerClock;
use grid::Grid;
use keys::{KEY_ICONS, Keyboard};
use kit::{Canvas, read_glyphs, read_icon};
use layout::{Layout, Look, Motion, layout_of};
use list::dial::Dial;
use list::{BOARD_COLUMNS, BOARD_ROWS, CARD_ROWS};
use picture::carousel::Carousel;
use picture::stamps::Stamps;
use robot::Sight;
use ruler::{LIVE_SHARE, Ruler};
use screen::Screen;
use screen::input::Touch;
use screen::seat::RobotPlace;
use screen::tree::{SUGGESTED, TREE, Tree, max_points, node_icons};
use screen::voice::RequestList;
use view::lines::{List, Lists};
use view::passage::{Offscreen, Passage};
use view::rail::Rail;

use crate::base::math::round;
use crate::base::pixel::PixelFormat;
use crate::base::time::{NANOS_PER_SECOND, seconds_to_next_minute};
use crate::domain::battery;
use crate::domain::device::Part;
use crate::domain::news::Edition;
use crate::domain::news::topics;
use crate::domain::settings::{BRIGHTNESS_SCALE, Setting, Settings, Value};
use crate::domain::tally::Tally;
use crate::domain::uses::Use;
use crate::gfx::icon::Icon;
use crate::gfx::key::{Ids, Key};
use crate::gfx::mask::Mask;
use crate::gfx::measures::PHI;
use crate::gfx::motion::{Edge, Pace};
use crate::gfx::palette::Roles;
use crate::gfx::pen::{DisplayList, FrameKind, Pen};
use crate::gfx::stamp::Stamp;
use crate::gfx::{Rect, Surface};
use crate::hw::power::doze::Doze;
use crate::hw::sound::volume;
use crate::input::pose::Pose;
use crate::logs::Logging;
use crate::wifi::radio::Way;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

const HOLD_TO_TALK_NS: u64 = 381_966_011;
const NOTE_NS: u64 = NANOS_PER_SECOND;
const CARET_NS: u64 = 618_033_989;
const FORWARD: f32 = -1.0;
const BACKWARD: f32 = 1.0;

pub struct Ui {
    tree: Tree,
    dial: Dial<List>,
    lists: Lists,
    rail: Rail,
    glyphs: Vec<(&'static str, Icon)>,
    display_list: DisplayList,
    mask: RefCell<Mask>,
    frame: u64,
    frame_ns: u64,
    passage: Option<Passage>,
    offscreen: Offscreen,
    word: RefCell<String>,
    bar_text: RefCell<String>,
    typing: Typing,
    bar_word: BarWord<(Option<Screen>, usize)>,
    ruler: Ruler,
    corner: Corner,
    charge: battery::State,
    link: Option<Way>,
    modem: Option<crate::cell::modem::Status>,
    router: Option<crate::domain::router::Standing>,
    has_tunnel: Option<bool>,
    bells: screen::bell::Bells,
    rundowns: screen::rundowns::Rundowns,
    notice_count: u32,
    tally: Tally,
    utc: Option<u64>,
    clock: Option<(String, String)>,
    screen: Option<Screen>,
    hold_start: Option<u64>,
    talk_start: Option<(u64, Screen)>,
    pressed_on: Option<Option<Screen>>,
    used_at: Option<u64>,
    input_at: Option<u64>,
    is_dark: bool,
    is_touch_swallowed: bool,
    does_redraw: bool,
    touch: Option<Touch>,
    finder: screen::finder::Finder,
    note: Option<(String, Option<u64>)>,
    rolls: screen::roll::Rolls,
    painting: screen::finder::Painting,
    cost: screen::cost::Cost,
    power: screen::power::Battery,
    about: screen::about::About,
    is_unlocked: bool,
    uses: Vec<Use>,
    debug: screen::debug::Debugging,
    themes: screen::themes::Themes,
    roles: Roles,
    is_keyed: Option<bool>,
    keying: screen::keying::Keying,
    safe: screen::safe::Safe,
    is_beneath: bool,
    key_icons: [Icon; 4],
    networks: screen::networks::Networks,
    pin: screen::pin::SimPin,
    memos: screen::memos::Memos,
    reader: screen::reader::Reader,
    aloud: screen::aloud::Aloud,
    translator: screen::talk::Translator,
    chat: screen::chat::Chat,
    pokedex: screen::voice::PageScreen,
    web: screen::voice::PageScreen,
    signal: screen::signal::Signal,
    calls: screen::calls::Calls,
    microphone: screen::voice::Meter,
    format: PixelFormat,
    run_time: RefCell<String>,
    disc: Icon,
    pose: Pose,
    settings: Settings,
    staged: Option<(Setting, Value)>,
    is_awaited: bool,
    doze: Option<Doze>,
    is_button_down: bool,
    console_radio: screen::sleep::ConsoleRadio,
    is_camera_held: bool,
    has_store: bool,
    stored_settings: Option<Settings>,
    robot: Robot,
    robot_place: RobotPlace,
    gauges: screen::levels::Gauges,
    face: screen::clock::Face,
    slide: Edge,
    pace: Pace,
    is_calm: bool,
    caret: Caret,
    ticking: TickerClock,
    ids: Ids,
}

fn tree_glyphs() -> Vec<&'static str> {
    let mut files = Vec::from([item::CHEVRON, item::DISC, robot::DRAWING, list::MORE]);
    files.extend(screen::ICONS.iter().flat_map(|icons| icons.iter().copied()));
    files.extend(topics::ICONS.iter().map(|icon| icon.1));
    files.extend(edit::EYE_ICONS);
    node_icons(&TREE, &mut files);
    node_icons(&SUGGESTED, &mut files);
    files
}

impl Ui {
    /// An interface for a screen of that size.
    pub fn new(width: i32, height: i32, format: PixelFormat, facts: Vec<(Part, Option<String>)>) -> Self {
        let (robot, ids) = (Robot::new(), Ids::default());
        let ruler = Ruler::new(width, height, robot.shape());
        let (gap, band, small_cell) = (ruler.gap, ruler.band, ruler.small_cell());
        let line_bytes = (width / small_cell.max(1)).max(1) as usize * char::MAX.len_utf8();
        let keys_height = keys::ROWS * round((width / keys::COLUMNS) as f32 * PHI);
        let keys = Rect::new(0, height - keys_height, width, keys_height);
        let (line_length, chat_line_length) = (ruler.secret_cells(), ruler.text_cells().0.0);
        let typed_keys = || {
            let (heading, body) = (ruler.heading.clone(), ruler.body.clone());
            Keyboard::new(keys, chat_line_length).with_forms(move |form| heading.has(form) && body.has(form))
        };
        let (chat_keys, pokedex_keys, signal_keys, web_keys) = (typed_keys(), typed_keys(), typed_keys(), typed_keys());
        let under_bar = Rect::new(gap, ruler.bar(), width - 2 * gap, height - ruler.bar() - gap);
        let cards = Grid::new(&ids, under_bar, height, gap, screen::themes::COLUMNS, screen::themes::ROWS);
        let rim = ruler.lines().0 as usize;
        let stamp = |svg: &str| Stamp::new(&read_icon(svg), band / PHI / LIVE_SHARE, rim);
        let (columns, rows) = (width.max(0) as usize, height.max(0) as usize);
        let glyphs = read_glyphs(tree_glyphs());
        let points = max_points(&TREE, &glyphs) + robot.points();
        let display_list = DisplayList::new(columns, rows, points);
        let offscreen = Offscreen::new(columns, rows, DisplayList::offscreen(columns, rows), &ids);
        let disc = read_icon(item::DISC);
        let mut mask = Mask::new(columns, rows);
        mask.reserve(glyphs.iter().map(|entry| entry.1.points()).max().unwrap_or(0).max(disc.points()));
        let mut ui = Ui {
            tree: Tree::default(),
            dial: Dial::default(),
            lists: Lists::default(),
            rail: Rail::default(),
            glyphs,
            display_list,
            mask: RefCell::new(mask),
            frame: 0,
            frame_ns: 0,
            passage: None,
            offscreen,
            word: RefCell::new(String::with_capacity(line_bytes)),
            bar_text: RefCell::new(String::with_capacity(line_bytes)),
            typing: Typing::Done,
            bar_word: BarWord {
                current: String::with_capacity(line_bytes),
                previous: String::with_capacity(line_bytes),
                ..BarWord::default()
            },
            ruler,
            corner: Corner::new(),
            charge: battery::State::default(),
            link: None,
            modem: None,
            router: None,
            has_tunnel: None,
            bells: screen::bell::Bells::new(),
            rundowns: screen::rundowns::Rundowns::new(),
            notice_count: 0,
            tally: Tally::default(),
            utc: None,
            clock: None,
            screen: None,
            hold_start: None,
            talk_start: None,
            pressed_on: None,
            used_at: None,
            input_at: None,
            is_dark: false,
            is_touch_swallowed: false,
            does_redraw: true,
            touch: None,
            finder: screen::finder::Finder::new(&ids),
            note: None,
            rolls: screen::roll::Rolls::new(Carousel::new(band * PHI), Stamps::new(stamp), &ids),
            painting: screen::finder::Painting::new(),
            cost: screen::cost::Cost::new(),
            power: screen::power::Battery::new(),
            about: screen::about::About::new(),
            is_unlocked: false,
            uses: Vec::new(),
            debug: screen::debug::Debugging::new(facts),
            themes: screen::themes::Themes::new(cards),
            roles: Roles::at(Settings::default().palette()),
            is_keyed: None,
            keying: screen::keying::Keying::new(),
            safe: screen::safe::Safe::new(Keyboard::new(keys, crate::storage::safe::MAX_LENGTH)),
            is_beneath: false,
            key_icons: KEY_ICONS.map(read_icon),
            networks: screen::networks::Networks::new(Keyboard::new(keys, line_length)),
            pin: screen::pin::SimPin::new(Keyboard::new(keys, line_length)),
            memos: screen::memos::Memos::new(),
            reader: screen::reader::Reader::default(),
            aloud: screen::aloud::Aloud::new(),
            translator: screen::talk::Translator::new(),
            chat: screen::chat::Chat::new(chat_keys),
            pokedex: screen::voice::PageScreen::new(pokedex_keys),
            web: screen::voice::PageScreen::new(web_keys),
            signal: screen::signal::Signal::new(signal_keys),
            calls: screen::calls::Calls::new(&ids),
            microphone: screen::voice::Meter::default(),
            format,
            run_time: RefCell::new(String::new()),
            disc,
            pose: Pose::Upright,
            settings: Settings::default(),
            staged: None,
            is_awaited: false,
            doze: None,
            is_button_down: false,
            console_radio: screen::sleep::ConsoleRadio::default(),
            is_camera_held: false,
            has_store: false,
            stored_settings: None,
            robot,
            robot_place: RobotPlace::default(),
            gauges: screen::levels::Gauges::new(),
            face: screen::clock::Face::new(),
            slide: Edge::still(0.0),
            pace: Pace::default(),
            is_calm: true,
            caret: Caret::default(),
            ticking: TickerClock::default(),
            ids,
        };
        ui.make_canvas_room();
        ui.relayout();
        ui
    }

    fn make_canvas_room(&mut self) {
        let ruler = &self.ruler;
        let (long, bar) = (ruler.width.max(ruler.height).max(0) as usize, ruler.bar().max(0) as usize);
        let (cell, line, inset) = (ruler.cell() as usize, ruler.large() as usize, ruler.framed_inset() as usize);
        let large = (keys::LARGE as usize * cell + 2 * inset) * (keys::LARGE as usize * line + 2 * inset);
        let rooms = [
            (Canvas::Note, long * ruler.small() as usize, true),
            (Canvas::RunTime, long * line, true),
            (Canvas::Header, long * bar, true),
            (Canvas::Large, large, false),
        ];
        for (canvas, words, is_turned) in rooms {
            self.display_list.reserve_canvas(canvas as usize, words, is_turned);
            self.offscreen.display_list.reserve_canvas(canvas as usize, words, is_turned);
        }
    }

    fn make_opening_room(&mut self, words: usize) {
        self.display_list.reserve_canvas(Canvas::Opening as usize, words, false);
        self.offscreen.display_list.reserve_canvas(Canvas::Opening as usize, words, false);
    }

    fn layout(&self) -> &'static dyn Layout {
        layout_of(self.settings.theme)
    }

    fn look(&self) -> Look {
        self.layout().look()
    }

    fn set_ruler(&mut self) {
        let look = self.look();
        self.ruler.set_look(look.is_one_size, look.has_status, look.has_rail);
    }

    fn set_line_lengths(&mut self) {
        let (secret, text) = (self.ruler.secret_cells(), self.ruler.text_cells().0.0);
        self.networks.keyboard.set_line_length(secret);
        self.pin.keys.set_line_length(secret);
        for keys in [&mut self.chat.keys, &mut self.pokedex.keys, &mut self.signal.keys, &mut self.web.keys] {
            keys.set_line_length(text);
        }
    }

    fn relayout(&mut self) {
        self.let_go_staged();
        self.set_ruler();
        self.set_line_lengths();
        for list in List::SCREENS {
            let grid = if self.has_cards(list) {
                self.board_grid(BOARD_COLUMNS, BOARD_ROWS / CARD_ROWS).with_fill()
            } else {
                self.list_grid()
            };
            *self.grid_of_mut(list) = grid;
        }
        (self.rolls.grid, self.rolls.store) = (self.roll_grid(), self.photo_store());
        self.rolls.drum.clear();
        (self.memos.entries, self.memos.open_entry, self.bar_word.place) = (None, None, None);
        self.dial.clear_band();
        (self.lists, self.rail) = (Lists::default(), Rail::default());
        self.stand_dial(0);
        self.tree = self.grow_tree();
        self.show_robot_place();
        self.deal_menu();
        self.reflow_rooms();
        if self.bells.window.is_open {
            self.lay_ring();
        }
        if self.screen == Some(Screen::Incoming) {
            self.lay_answers();
        }
    }

    /// What the user has set.
    pub const fn settings(&self) -> Settings {
        self.settings
    }

    /// The device's uses since the last call, oldest first.
    pub fn take_uses(&mut self) -> Vec<Use> {
        core::mem::take(&mut self.uses)
    }

    fn is_adjusting(&self) -> bool {
        matches!(
            self.screen,
            Some(Screen::Brightness | Screen::Volume | Screen::Themes | Screen::Colours | Screen::Clock)
        )
    }

    /// Notes that a store holds the settings.
    pub const fn await_settings(&mut self) {
        (self.is_awaited, self.has_store) = (true, true);
    }

    /// Takes the stored settings; true if the screen changed.
    #[must_use]
    pub fn recall(&mut self, settings: Settings) -> bool {
        self.stored_settings = Some(settings);
        self.set_settings(self.settings.merge_stored(settings))
    }

    const fn are_settled(&self) -> bool {
        !self.has_store || self.stored_settings.is_some()
    }

    /// How much is logged, none before the settings count.
    pub const fn logging_setting(&self) -> Option<Logging> {
        if self.are_settled() { Some(self.settings.logging) } else { None }
    }

    /// The screen clock's offset from UTC and the daily edition, none before the settings count.
    pub const fn edition_setting(&self) -> Option<(i16, Edition)> {
        if self.are_settled() { Some((self.settings.offset, self.settings.edition)) } else { None }
    }

    /// The settings to store, where they changed since they were last stored.
    pub fn take_changed_settings(&mut self, is_going_down: bool) -> Option<Settings> {
        let settings = self.settings;
        let has_changed = self.stored_settings.is_some_and(|stored| stored != settings);
        (has_changed && (!self.is_adjusting() || !self.is_awake() || is_going_down)).then(|| {
            self.stored_settings = Some(settings);
            settings
        })
    }

    /// Takes the settings that count from now on; true if the screen changed.
    #[must_use]
    pub fn set_settings(&mut self, settings: Settings) -> bool {
        let settings = Settings {
            brightness: BRIGHTNESS_SCALE.nearest(u32::from(settings.brightness)),
            volume: volume::SCALE.nearest(u32::from(settings.volume)),
            sounds: volume::SCALE.nearest(u32::from(settings.sounds)),
            ..settings
        };
        let was_alive = self.does_robot_live();
        self.is_awaited = false;
        let is_redesigned = self.settings.theme != settings.theme;
        let is_offset_new = self.settings.offset != settings.offset;
        let has_changed =
            core::mem::replace(&mut self.settings, settings) != settings || self.does_robot_live() != was_alive;
        self.show_robot_place();
        self.recolour();
        self.update_clock();
        if is_redesigned {
            self.relayout();
            self.theme_home();
        }
        self.deal_menu();
        self.deal_settings();
        let is_debugging = has_changed && self.does_debug_show();
        if is_debugging {
            self.deal_debug();
        }
        self.settle_safe();
        let is_asking = self.keep_asking();
        // The bell's lines tell each ring by the clock, which stands elsewhere now.
        let is_bell_new = is_offset_new && self.note_bell_change();
        is_asking || is_bell_new || has_changed && (self.screen.is_none() || self.is_adjusting() || is_debugging)
    }

    fn theme_home(&mut self) {
        if self.screen == Some(Screen::Ring) {
            self.bells.window.beneath = None;
        } else if self.screen == Some(Screen::Incoming) {
            self.calls.window.beneath = self.calls.window.beneath.filter(|beneath| *beneath == Screen::Call);
        } else if !self.is_adjusting() && self.screen != Some(Screen::Safe) {
            (self.screen, self.passage, self.keying.window.is_open) = (None, None, false);
        }
        // The layout made their grids anew: what stays of the themes and the colours is laid as its opening lays it.
        match self.screen {
            Some(Screen::Themes) => self.open_themes(),
            Some(Screen::Colours) => self.open_colours(),
            _ => {}
        }
        self.show_settings();
    }

    fn do_notes_show(&self) -> bool {
        matches!(
            self.screen,
            Some(Screen::Finder(_) | Screen::Grid(_) | Screen::Roll(_) | Screen::Clock | Screen::Call)
        )
    }

    fn set_note(&mut self, text: String, is_lasting: bool, now_ns: u64) -> bool {
        self.note = Some((text, (!is_lasting).then_some(now_ns + NOTE_NS)));
        self.slide = Edge::still(1.0);
        self.do_notes_show()
    }

    /// Ends a note whose time is over; true if the screen changed.
    #[must_use]
    pub fn fade(&mut self, now_ns: u64) -> bool {
        if !self.is_awake() {
            return false;
        }
        let is_done = matches!(self.note, Some((_, Some(until))) if now_ns >= until);
        if is_done {
            self.note = None;
        }
        is_done && self.do_notes_show()
    }

    /// True while the screen wants a new frame every refresh.
    pub fn is_animating(&self) -> bool {
        if self.is_dark || !self.is_awake() {
            return false;
        }
        let does_robot_move = self.does_robot_live() && self.robot.is_moving();
        let does_move = |list| {
            !self.grid_of(list).is_resting()
                || self.store_of(list).is_some_and(|store| !store.is_resting())
                || self.is_dial(list) && !self.dial.is_resting()
        };
        let is_listed = (self.visible_list().into_iter().chain(self.list_under()))
            .chain(self.pane_list())
            .chain(self.rail_list())
            .any(does_move);
        let does_screen_move = match self.screen {
            None => {
                is_listed || self.tree.levels.iter().any(|level| level.stage().is_some_and(|stage| !stage.is_resting))
            }
            Some(Screen::Debug) => is_listed || self.is_pacing(),
            Some(Screen::Grid(_) | Screen::Roll(_)) => {
                let (grid, store) = (&self.rolls.grid, &self.rolls.store);
                !grid.is_resting() || !store.is_resting() || self.is_playing() || !self.rolls.carousel.is_resting()
            }
            Some(
                Screen::Keyboard
                | Screen::Pin
                | Screen::Safe
                | Screen::Chat
                | Screen::Pokedex
                | Screen::Web
                | Screen::Conversation,
            ) => self.keys_up().is_some_and(|id| !self.keyboard(id).is_resting()),
            Some(Screen::Themes) => {
                !self.themes.cards_grid.is_resting() || !self.themes.cards.is_resting() || self.themes.cards_due != 0
            }
            Some(Screen::Finder(_)) => !self.is_review_resting(),
            Some(_) => is_listed,
        };
        let does_unfold = self.does_room_unfold() || !self.bar_word.unfold.is_resting();
        does_robot_move
            || does_screen_move
            || does_unfold
            || !self.is_calm
            || self.passage.is_some()
            || self.ticking.is_moving()
    }

    /// True while frames are heavy to draw.
    pub fn is_straining(&self) -> bool {
        let is_rolling = self.does_drum_show() && !self.is_drum_resting();
        let does_roll_move = matches!(self.screen, Some(Screen::Grid(_) | Screen::Roll(_)))
            && (self.rolls.grid.is_growing() || !self.rolls.carousel.is_resting() || is_rolling);
        let is_passing = self.passage.is_some_and(|passage| passage.motion != Motion::Print) || self.does_window_move();
        let is_dealing = self.screen == Some(Screen::Themes) && self.themes.cards_due != 0;
        does_roll_move || !self.is_dark && (is_passing || is_dealing || self.does_chat_repaint())
    }

    /// When `tick` or `fade` has to run even if no event comes.
    pub fn deadline(&self) -> Option<u64> {
        let minute = self.utc.map(|utc| self.ticking.tick_ns + seconds_to_next_minute(utc) * NANOS_PER_SECOND);
        let talk = self.talk_start.map(|(start_ns, _)| start_ns + HOLD_TO_TALK_NS);
        let is_billed = !self.charge.is_plugged || self.is_billed();
        let idle =
            self.used_at.filter(|_| !self.is_in_use() && is_billed).map(|at| at + screen::sleep::IDLE_TO_SLEEP_NS);
        let darkening = self
            .input_at
            .filter(|_| self.is_unwatched() && !self.is_dark && !self.charge.is_plugged)
            .map(|at| at + screen::sleep::DARK_UNDER_TASK_NS);
        let note = self.note.as_ref().and_then(|note| note.1);
        let robot = self.robot.deadline().filter(|_| self.does_robot_live() && !self.is_dark);
        let keys = self.keys_up().and_then(|id| self.keyboard(id).deadline());
        let (safe, hold) = (self.safe_due(), self.hold_due());
        let row = self.painting.row_due.filter(|_| self.do_photos_show());
        let seconds = [
            self.memos.recording.due(self.is_hearing_note()),
            self.translator.running.due(self.screen == Some(Screen::Talk)),
        ];
        let voices = self.voices().map(|voice| voice.0.stopwatch.due(voice.1));
        let caret = self.caret.due_ns.filter(|_| !self.is_dark);
        let ticks = self.ticking.due().filter(|_| !self.is_dark);
        let due = [
            minute,
            talk,
            idle,
            darkening,
            note,
            robot,
            keys,
            safe,
            hold,
            row,
            caret,
            ticks,
            self.bell_due(),
            self.call_due(),
        ];
        due.into_iter().chain(seconds).chain(voices).flatten().min()
    }

    /// Moves time on to `now_ns`.
    pub fn tick(&mut self, now_ns: u64) -> Option<Action> {
        if !self.is_awake() {
            return None;
        }
        self.ticking.tick_ns = now_ns;
        self.let_go_finder();
        self.let_go_rolls();
        let was_home = self.is_at_home();
        self.cost_tick();
        if self.entered_screen().is_none() {
            self.tree.advance(now_ns);
        }
        if was_home && !self.is_at_home() {
            self.robot_sees(Sight::Middle, now_ns);
        }
        if self.passage.is_some() {
            self.land_screen();
        }
        if matches!(self.screen, Some(Screen::Grid(_) | Screen::Roll(_))) {
            self.advance_roll(now_ns);
        }
        if matches!(self.screen, Some(Screen::Finder(_))) {
            self.advance_review(now_ns);
        }
        if let Some(id) = self.keys_up() {
            self.keyboard_mut(id).advance(now_ns);
        }
        if self.screen == Some(Screen::Safe) {
            self.tick_safe(now_ns);
        }
        self.show_ring(now_ns);
        self.tick_call(now_ns);
        self.hold_on_robot(now_ns);
        if self.screen == Some(Screen::Themes) {
            self.advance_themes(now_ns);
        }
        let (screen_list, beside) =
            (self.visible_list().filter(|list| !matches!(list, List::Menu(_))), self.rail_list());
        for list in screen_list.into_iter().chain(self.list_under()).chain(self.pane_list()).chain(beside) {
            self.grid_of_mut(list).advance(now_ns);
        }
        for list in self.visible_list().into_iter().chain(self.list_under()).chain(self.pane_list()).chain(beside) {
            self.advance_lines(list, now_ns);
        }
        let followed = self.follow_rail(now_ns);
        self.advance_rooms(now_ns);
        self.tick_home(now_ns);
        self.settle(now_ns);
        self.deal_roll();
        self.dial.advance(now_ns);
        self.bar_word.unfold.advance(now_ns);
        if self.robot_place.is_shown {
            let seat = self.robot_seat();
            self.brief_robot(&seat, now_ns);
            self.robot.advance(now_ns);
        }
        self.darken(now_ns);
        if self.is_in_use() {
            self.used_at = Some(now_ns);
        }
        if now_ns.saturating_sub(*self.used_at.get_or_insert(now_ns)) < screen::sleep::IDLE_TO_SLEEP_NS {
            return followed;
        }
        if self.is_kept_awake() {
            self.end_billed();
            return followed;
        }
        Some(self.fall_asleep(now_ns))
    }

    fn let_go(&mut self, now_ns: u64) {
        self.let_go_home();
        (self.hold_start, self.pressed_on) = (None, None);
        for list in List::SCREENS {
            self.grid_of_mut(list).press(false);
        }
        self.rolls.grid.press(false);
        self.themes.cards_grid.press(false);
        self.let_go_chat();
        self.let_go_pokedex();
        self.let_go_web();
        for list in RequestList::ALL {
            self.let_go_request(list);
        }
        self.let_go_conversation();
        self.tree.levels[0].press(false, now_ns);
        if self.entered_screen().is_none() {
            self.tree.level_mut().press(false, now_ns);
        }
    }

    fn leave_screen(&mut self) {
        self.leave_chat();
        self.leave_pokedex();
        self.leave_web();
        for list in RequestList::ALL {
            self.leave_request_list(list);
        }
        self.leave_conversation();
    }

    /// Takes how the device is in a network now, if it is; true if the screen changed.
    #[must_use]
    pub fn set_link(&mut self, link: Option<Way>) -> bool {
        let has_changed = core::mem::replace(&mut self.link, link) != link;
        has_changed && (self.does_status_show() || self.does_corner_show())
    }

    fn notice(&mut self, is_heard_asleep: bool) {
        if (self.is_awake() || is_heard_asleep) && self.settings.sounds > volume::QUIETEST {
            self.notice_count = self.notice_count.wrapping_add(1);
        }
    }

    /// The count of notices since the start.
    pub const fn notices(&self) -> u32 {
        self.notice_count
    }

    /// Draws a frame onto a page that shows the frame of number `back_frame`, only where the two differ.
    pub fn render_over(&mut self, surface: &mut Surface<'_>, now_ns: u64, back_frame: Option<u64>) -> FrameKind {
        let view = self.scene_key();
        if core::mem::replace(&mut self.ticking.view, view) != view {
            self.ticking.start_ns = now_ns;
        }
        self.ticking.is_seen.set(false);
        self.ready(now_ns);
        self.bar_word.is_drawn.set(false);
        let mut list = core::mem::take(&mut self.display_list);
        let kind = list.frame(surface, back_frame, |pen| self.draw(pen, now_ns));
        self.display_list = list;
        (self.frame, self.frame_ns, self.ticking.has_ticked) = (self.frame + 1, now_ns, self.ticking.is_seen.get());
        self.note_word_written();
        kind
    }

    fn ready(&mut self, now_ns: u64) {
        self.ticking.draw_ns = now_ns;
        self.ready_room();
        self.ready_beneath(now_ns);
    }

    fn scene_key(&self) -> u64 {
        let (name, detail) = self.scene().to_words();
        let selection = match self.screen {
            Some(Screen::Themes) => Some(self.themes.cards_grid.selection),
            Some(Screen::Grid(_) | Screen::Roll(_)) => Some(self.rolls.grid.selection),
            _ => self.visible_list().map(|list| self.grid_of(list).selection),
        };
        Key::default()
            .bytes(name.as_bytes())
            .bytes(detail.as_bytes())
            .word(self.tree.path.len() as u64)
            .word(selection.map_or(u64::MAX, |selection| selection as u64))
            .value()
    }

    fn draw(&self, pen: &mut Pen<'_, '_>, now_ns: u64) {
        self.draw_scene(pen, now_ns);
        self.draw_rail(pen);
        self.draw_robot(pen);
    }
}

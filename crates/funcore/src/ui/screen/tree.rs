use super::Screen;
use super::bell::NONE_SET;
use super::calls::CALL_ICON;
use super::levels::{SOUNDS, SPEAKER_ICON, SUN_ICON};
use crate::base::time::offset;
use crate::base::time::write_duration;
use crate::domain::chat::tools::{Destination, Menu};
use crate::domain::device::{FIRMWARE, HAS_EXTRAS};
use crate::domain::gallery::Kind;
use crate::domain::scene;
use crate::domain::settings::{Setting, Tool};
use crate::domain::shelf::Shelf;
use crate::domain::uses::Use;
use crate::gfx::icon::Icon;
use crate::openai::models::Task;
use crate::ui::corner::{CELL_ICON, WIFI_ICON};
use crate::ui::grid::Grid;
use crate::ui::item::Item;
use crate::ui::kit::{find_glyph, read_icon};
use crate::ui::list::stage::Stage;
use crate::ui::robot::{DRAWING, Sight};
use crate::ui::view::lines::List;
use crate::ui::view::rail::Rail;
use crate::ui::words::{HOLD_TO_TALK, theme_name, write_palette_name};
use crate::ui::{Action, BACKWARD, FORWARD, Ui};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

pub(in crate::ui) const HOME: usize = 0;
pub(super) const MAX_NAMED_CHILDREN: usize = 4;
pub(in crate::ui) const NOTHING: &str = "<svg viewBox=\"0 0 1 1\"><path d=\"M0 0\"/></svg>";
const TODAYS_READY: &str = "TODAY'S READY";
const MEMORY_WORD: &str = "WHAT IT KEEPS OF YOU";
const DAYS_WORD: &str = "WHAT WAS SAID, DAY BY DAY";
const FORGET_WORD: &str = "THE THREAD IT IS GIVEN EVERY TURN";

#[derive(Clone, Copy)]
pub(in crate::ui) enum Content {
    Level(&'static [Node]),
    Action(Use),
    // Set where its line stands.
    Setting(Setting),
    // A line that only tells its word.
    Note(&'static str),
}

impl Content {
    pub(in crate::ui) fn runs(self, action: Use) -> bool {
        matches!(self, Content::Action(found) if found == action)
    }

    pub(in crate::ui) fn sets(self, setting: Setting) -> bool {
        self.setting() == Some(setting)
    }

    pub(in crate::ui) const fn setting(self) -> Option<Setting> {
        if let Content::Setting(setting) = self { Some(setting) } else { None }
    }

    // True for a line whose word tells the modem's state: its level's, its setting's and the SIM PIN's.
    pub(in crate::ui) fn tells_modem(self) -> bool {
        match self {
            Content::Level(nodes) => holds(nodes, Setting::Cellular),
            Content::Setting(setting) => setting == Setting::Cellular,
            Content::Action(action) => action == Use::Open(Destination::SimPin),
            Content::Note(_) => false,
        }
    }

    // True for a line whose word tells of the router: the modem lent to a computer, the tunnel and its config.
    pub(in crate::ui) fn tells_router(self) -> bool {
        match self {
            Content::Setting(setting) => matches!(setting, Setting::Modem | Setting::Tunnel),
            Content::Action(action) => action == Use::Open(Destination::Tunnel),
            Content::Level(_) | Content::Note(_) => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(in crate::ui) enum Presence {
    Always,
    Unlocked,
    // While the R1 is a linked device of Signal's.
    Linked,
    Never,
}

// What decides whether a node shows: the debug screen unlocked, and Signal linked.
#[derive(Clone, Copy)]
pub(in crate::ui) struct Gate {
    is_unlocked: bool,
    is_linked: bool,
}

impl Presence {
    const fn is_visible(self, gate: Gate) -> bool {
        match self {
            Presence::Always => true,
            Presence::Unlocked => gate.is_unlocked,
            Presence::Linked => gate.is_linked,
            Presence::Never => false,
        }
    }
}

pub(in crate::ui) struct Node {
    pub name: &'static str,
    pub(super) icon: &'static str,
    pub(in crate::ui) content: Content,
    presence: Presence,
}

const fn leaf(name: &'static str, icon: &'static str, action: Use) -> Node {
    Node { name, icon, content: Content::Action(action), presence: Presence::Always }
}

const fn screen(name: &'static str, icon: &'static str, destination: Destination) -> Node {
    leaf(name, icon, Use::Open(destination))
}

const fn branch(name: &'static str, icon: &'static str, nodes: &'static [Node]) -> Node {
    Node { name, icon, content: Content::Level(nodes), presence: Presence::Always }
}

// A level the conversation opens by the name its line goes by, which no other level's does.
const fn menu(menu: Menu, icon: &'static str, nodes: &'static [Node]) -> Node {
    branch(menu.to_name(), icon, nodes)
}

pub(super) const fn setting(name: &'static str, icon: &'static str, setting: Setting) -> Node {
    Node { name, icon, content: Content::Setting(setting), presence: Presence::Always }
}

const fn note(name: &'static str, icon: &'static str, word: &'static str) -> Node {
    Node { name, icon, content: Content::Note(word), presence: Presence::Always }
}

const fn extra(node: Node) -> Node {
    if HAS_EXTRAS { node } else { Node { presence: Presence::Never, ..node } }
}

pub(super) const COST_ICON: &str = icon!("paid");
pub(super) const POWER_ICON: &str = icon!("bolt");
const CHAT: Node = leaf(scene::CHAT, icon!("chat"), Use::Chat);
const SIGNAL: Node = screen(scene::SIGNAL, SIGNAL_ICON, Destination::Signal);
const CALLS: Node = Node { presence: Presence::Linked, ..screen(scene::CALLS, CALL_ICON, Destination::Calls) };
const PHOTO: Node = screen(scene::PHOTO, icon!("camera"), Destination::PhotoCamera);
const VIDEO: Node = screen(scene::VIDEO, icon!("videocam"), Destination::VideoCamera);
const PICTURES: Node = screen(scene::PICTURES, icon!("image"), Destination::Pictures);
const VIDEOS: Node = screen(scene::VIDEOS, icon!("movie"), Destination::Videos);
const AUDIO_NOTE: Node = screen(scene::AUDIO_NOTE, NOTES_ICON, Destination::Notes);
const TRANSLATOR: Node = screen(scene::TRANSLATOR, TRANSLATE_ICON, Destination::Translations);
const NEWS: Node = screen(scene::NEWS, NEWS_ICON, Destination::News);
const WEB: Node = screen(scene::WEB, WEB_ICON, Destination::Web);
const POKEDEX_ICON: &str = icon!(older "catching_pokemon");
const POKEDEX: Node = extra(screen(scene::POKEDEX, POKEDEX_ICON, Destination::Pokedex));
const WIFI: Node = screen(scene::WIFI, WIFI_ICON, Destination::Wifi);
const BRIGHTNESS: Node = screen(scene::BRIGHTNESS, SUN_ICON, Destination::Brightness);
const VOLUME: Node = screen(scene::VOLUME, SPEAKER_ICON, Destination::Volume);
const THEMES: Node = screen(scene::THEMES, icon!("space_dashboard"), Destination::Themes);
const COLORS: Node = screen(scene::COLORS, icon!("palette"), Destination::Colours);
const CLOCK: Node = screen(scene::CLOCK, icon!("schedule"), Destination::Clock);
const BELL_ICON: &str = icon!("notifications");
const BELL: Node = screen(scene::BELL, BELL_ICON, Destination::Bell);
const PASSPHRASE: Node = screen(scene::PASSPHRASE, icon!("lock"), Destination::Passphrase);
const COST: Node = screen(scene::COST, COST_ICON, Destination::Cost);
const POWER: Node = screen(scene::POWER, POWER_ICON, Destination::Power);
const ABOUT: Node = screen(scene::ABOUT, icon!("info"), Destination::About);
const MODEL_ICON: &str = icon!("neurology");
const EFFORT_ICON: &str = icon!("psychology");
pub(super) const NOTES_ICON: &str = icon!("speech_to_text");
pub(super) const TRANSLATE_ICON: &str = icon!("translate");
pub(super) const NEWS_ICON: &str = icon!("newspaper");
const SIGNAL_ICON: &str = icon!("forum");
const WEB_ICON: &str = icon!("public");
const WEB_SETTINGS: [Node; 2] = [
    setting(scene::MODEL, MODEL_ICON, Setting::Model(Task::Web)),
    setting(scene::REASONING, EFFORT_ICON, Setting::Effort(Task::Web)),
];
const TOOLS: [Node; 6] = [
    setting(scene::WEB_SEARCH, icon!("travel_explore"), Setting::Tool(Tool::Web)),
    setting(scene::DEVICE, icon!("devices"), Setting::Tool(Tool::Device)),
    setting(scene::NOTES, NOTES_ICON, Setting::Tool(Tool::Notes)),
    setting(scene::NEWS, NEWS_ICON, Setting::Tool(Tool::News)),
    setting(scene::CAMERA, icon!("photo_camera"), Setting::Tool(Tool::Camera)),
    setting(scene::SIGNAL, SIGNAL_ICON, Setting::Tool(Tool::Signal)),
];
pub(super) const MEMORY_ICON: &str = icon!("person_book");
pub(super) const DAYS_ICON: &str = icon!("calendar_month");
const ROBOT_SETTINGS: [Node; 8] = [
    menu(Menu::Tools, icon!("handyman"), &TOOLS),
    setting(scene::CHAT_MODEL, MODEL_ICON, Setting::Model(Task::Chat)),
    setting(scene::CHAT_REASONING, EFFORT_ICON, Setting::Effort(Task::Chat)),
    setting(scene::RECOLLECTION_MODEL, MODEL_ICON, Setting::Model(Task::Recollect)),
    setting(scene::RECOLLECTION_REASONING, EFFORT_ICON, Setting::Effort(Task::Recollect)),
    screen(scene::MEMORY, MEMORY_ICON, Destination::Memory),
    screen(scene::DAYS, DAYS_ICON, Destination::Days),
    leaf(scene::FORGET, icon!("delete_history"), Use::Forget),
];
const SIGNAL_SETTINGS: [Node; 10] = [
    setting(scene::FETCH, icon!("schedule"), Setting::SignalWake),
    setting(scene::READ_ALOUD, icon!("record_voice_over"), Setting::SignalAloud),
    setting(scene::SAID, icon!("mic"), Setting::SaidVoice),
    setting(scene::TEXT_ONLY, icon!("text_fields"), Setting::TextOnly),
    setting(scene::AUTO_STOPS, icon!("stop_circle"), Setting::AutoReplies),
    setting(scene::AUTO_WAITS, icon!("hourglass_top"), Setting::AutoGap),
    setting(scene::AUTO_HOURLY, icon!("timer"), Setting::AutoHourly),
    setting(scene::AUTO_MODEL, MODEL_ICON, Setting::Model(Task::Auto)),
    setting(scene::AUTO_REASONING, EFFORT_ICON, Setting::Effort(Task::Auto)),
    note(scene::UNLINK, icon!("link_off"), "ON THE PHONE: LINKED DEVICES"),
];
const NOTES_SETTINGS: [Node; 2] = [
    setting(scene::SUMMARIZE_MODEL, MODEL_ICON, Setting::Model(Task::Summary)),
    setting(scene::SUMMARIZE_REASONING, EFFORT_ICON, Setting::Effort(Task::Summary)),
];
const NEWS_SETTINGS: [Node; 6] = [
    setting(scene::EDITION, icon!("schedule"), Setting::Edition),
    screen(scene::RUNDOWNS, icon!("list_alt"), Destination::Rundowns),
    setting(scene::MODEL, MODEL_ICON, Setting::Model(Task::News)),
    setting(scene::REASONING, EFFORT_ICON, Setting::Effort(Task::News)),
    setting(scene::REQUEST_MODEL, MODEL_ICON, Setting::Model(Task::Rundowns)),
    setting(scene::REQUEST_REASONING, EFFORT_ICON, Setting::Effort(Task::Rundowns)),
];
const POKEDEX_SETTINGS: [Node; 1] = [setting(scene::LANGUAGE, icon!("language"), Setting::PokedexLanguage)];
const MOBILE_DATA_SETTINGS: [Node; 6] = [
    setting(scene::MODEM, icon!("cell_tower"), Setting::Cellular),
    screen(scene::SIM_PIN, icon!("sim_card"), Destination::SimPin),
    setting(scene::KEEP_PIN, icon!("password"), Setting::KeepPin),
    setting(scene::USB_MODEM, icon!("router"), Setting::Modem),
    setting(scene::VPN, icon!("vpn_lock"), Setting::Tunnel),
    screen(scene::TUNNEL, icon!("vpn_key"), Destination::Tunnel),
];
const BELL_SETTINGS: [Node; 2] = [
    setting(scene::MODEL, MODEL_ICON, Setting::Model(Task::Bell)),
    setting(scene::REASONING, EFFORT_ICON, Setting::Effort(Task::Bell)),
];
const DRIVE_ICON: &str = icon!("usb");
const DRIVE_SETTINGS: [Node; 1] = [setting(scene::DRIVE, DRIVE_ICON, Setting::Drive)];
const FEATURES: [Node; 7] = [
    menu(Menu::Robot, DRAWING, &ROBOT_SETTINGS),
    menu(Menu::Signal, SIGNAL_ICON, &SIGNAL_SETTINGS),
    menu(Menu::Notes, NOTES_ICON, &NOTES_SETTINGS),
    menu(Menu::News, NEWS_ICON, &NEWS_SETTINGS),
    menu(Menu::Web, WEB_ICON, &WEB_SETTINGS),
    menu(Menu::Bell, BELL_ICON, &BELL_SETTINGS),
    extra(menu(Menu::Pokedex, POKEDEX_ICON, &POKEDEX_SETTINGS)),
];
const SETTINGS: [Node; 15] = [
    menu(Menu::Features, icon!("widgets"), &FEATURES),
    WIFI,
    menu(Menu::MobileData, CELL_ICON, &MOBILE_DATA_SETTINGS),
    BRIGHTNESS,
    VOLUME,
    THEMES,
    COLORS,
    CLOCK,
    PASSPHRASE,
    menu(Menu::UsbDrive, DRIVE_ICON, &DRIVE_SETTINGS),
    COST,
    POWER,
    ABOUT,
    Node { presence: Presence::Unlocked, ..screen(scene::DEBUG, icon!("bug_report"), Destination::Debug) },
    Node { presence: Presence::Never, ..leaf(scene::ROBOT, DRAWING, Use::Robot) },
];
const CAMERA: Node = menu(Menu::Camera, icon!("photo_camera"), &[PHOTO, VIDEO, PICTURES, VIDEOS]);
const MIC: [Node; 2] = [AUDIO_NOTE, TRANSLATOR];

pub(in crate::ui) const TREE: [Node; 10] = [
    CHAT,
    SIGNAL,
    CALLS,
    CAMERA,
    menu(Menu::Microphone, icon!("mic"), &MIC),
    NEWS,
    WEB,
    BELL,
    POKEDEX,
    menu(Menu::Settings, icon!("settings"), &SETTINGS),
];
pub(in crate::ui) const SUGGESTED: [Node; 4] = [PHOTO, PICTURES, NEWS, branch(scene::EVERYTHING, icon!("apps"), &TREE)];

// The tools' own level, whose lines switch them.
fn holds_tools(nodes: &[Node]) -> bool {
    nodes.iter().any(|node| matches!(node.content, Content::Setting(Setting::Tool(_))))
}

fn leads_to_tools(content: Content) -> bool {
    matches!(content, Content::Level(nodes) if holds_tools(nodes))
}

pub(super) fn holds(nodes: &[Node], setting: Setting) -> bool {
    nodes.iter().any(|node| node.content.sets(setting))
}

// The switch a level is for, which its line in its parent tells: the modem's and the drive's.
fn switch_of(nodes: &[Node]) -> Option<Setting> {
    [Setting::Cellular, Setting::Drive].into_iter().find(|setting| holds(nodes, *setting))
}

fn route(nodes: &'static [Node], is_target: &dyn Fn(&Node) -> bool, gate: Gate) -> Option<Vec<usize>> {
    for (place, node) in nodes.iter().enumerate().filter(|(_, node)| node.presence.is_visible(gate)) {
        if is_target(node) {
            return Some(alloc::vec![place]);
        }
        if let Content::Level(nodes) = node.content
            && let Some(mut rest) = route(nodes, is_target, gate)
        {
            rest.insert(0, place);
            return Some(rest);
        }
    }
    None
}

pub(in crate::ui) enum Shape {
    Stage(Stage),
    Menu(Box<Grid>),
    Rail(Box<Grid>),
}

pub(in crate::ui) struct Level {
    pub(in crate::ui) nodes: &'static [Node],
    pub(in crate::ui) visible: Vec<usize>,
    pub(in crate::ui) children: Vec<Option<usize>>,
    pub(in crate::ui) shape: Shape,
}

impl Level {
    pub(in crate::ui) fn node(&self, item: usize) -> Option<&'static Node> {
        let nodes = self.nodes;
        self.visible.get(item).map(|at| &nodes[*at])
    }

    pub(in crate::ui) fn selection(&self) -> usize {
        match &self.shape {
            Shape::Stage(stage) => stage.selection,
            Shape::Menu(grid) | Shape::Rail(grid) => grid.selection,
        }
    }

    pub(in crate::ui) const fn stage(&self) -> Option<&Stage> {
        if let Shape::Stage(stage) = &self.shape { Some(stage) } else { None }
    }

    pub(in crate::ui) fn menu(&self) -> Option<&Grid> {
        if let Shape::Menu(grid) | Shape::Rail(grid) = &self.shape { Some(grid) } else { None }
    }

    pub(in crate::ui) fn menu_mut(&mut self) -> Option<&mut Grid> {
        if let Shape::Menu(grid) | Shape::Rail(grid) = &mut self.shape { Some(grid) } else { None }
    }

    pub(in crate::ui) const fn is_rail(&self) -> bool {
        matches!(self.shape, Shape::Rail(_))
    }

    pub(in crate::ui) fn press(&mut self, is_down: bool, now_ns: u64) {
        match &mut self.shape {
            Shape::Stage(stage) => stage.press(is_down, now_ns),
            Shape::Menu(grid) | Shape::Rail(grid) => grid.press(is_down),
        }
    }

    pub(in crate::ui) fn stand(&mut self, item: usize, is_docked: bool) {
        let count = self.visible.len();
        match &mut self.shape {
            Shape::Stage(stage) => stage.stand(item, is_docked),
            Shape::Menu(grid) | Shape::Rail(grid) => {
                grid.press(false);
                grid.set_items_at(count, item);
                grid.land();
            }
        }
    }

    pub(in crate::ui) fn advance(&mut self, now_ns: u64, is_front: bool) {
        match &mut self.shape {
            Shape::Stage(stage) => stage.advance(now_ns),
            Shape::Menu(grid) | Shape::Rail(grid) if is_front => grid.advance(now_ns),
            Shape::Menu(_) | Shape::Rail(_) => {}
        }
    }
}

#[derive(Default)]
pub(in crate::ui) struct Tree {
    pub(in crate::ui) levels: Vec<Level>,
    pub(in crate::ui) path: Vec<usize>,
}

impl Tree {
    pub(in crate::ui) fn front(&self) -> usize {
        self.path.last().copied().unwrap_or(0)
    }

    pub(in crate::ui) fn level(&self) -> &Level {
        &self.levels[self.front()]
    }

    pub(in crate::ui) fn level_mut(&mut self) -> &mut Level {
        let front = self.front();
        &mut self.levels[front]
    }

    pub(in crate::ui) fn name_of(&self, level: usize) -> Option<&'static str> {
        self.levels.iter().find_map(|parent| {
            let at = parent.children.iter().position(|child| *child == Some(level))?;
            Some(parent.nodes[at].name)
        })
    }

    pub(in crate::ui) fn advance(&mut self, now_ns: u64) {
        let front = self.front();
        for (at, level) in self.levels.iter_mut().enumerate() {
            level.advance(now_ns, at == front);
        }
    }
}

pub(in crate::ui) fn visible_of(nodes: &[Node], gate: Gate) -> Vec<usize> {
    (0..nodes.len()).filter(|at| nodes[*at].presence.is_visible(gate)).collect()
}

fn visible(nodes: &[Node], gate: Gate) -> impl Iterator<Item = &Node> {
    nodes.iter().filter(move |node| node.presence.is_visible(gate))
}

pub(in crate::ui) fn icon_of(glyphs: &[(&'static str, Icon)], node: &Node) -> Icon {
    find_glyph(glyphs, node.icon).cloned().unwrap_or_else(|| read_icon(node.icon))
}

pub(in crate::ui) fn node_icons(nodes: &'static [Node], icons: &mut Vec<&'static str>) {
    for node in nodes {
        icons.push(node.icon);
        if let Content::Level(nodes) = node.content {
            node_icons(nodes, icons);
        }
    }
}

pub(in crate::ui) fn max_points(nodes: &'static [Node], glyphs: &[(&'static str, Icon)]) -> usize {
    let shown = nodes.iter().filter(|node| node.presence != Presence::Never);
    let own = shown.filter_map(|node| find_glyph(glyphs, node.icon)).map(Icon::points).sum();
    nodes
        .iter()
        .filter_map(|node| if let Content::Level(nodes) = node.content { Some(nodes) } else { None })
        .fold(own, |max, nodes| max.max(max_points(nodes, glyphs)))
}

impl Ui {
    pub(in crate::ui) fn open(&mut self, now_ns: u64) -> Option<Action> {
        let level = self.tree.level();
        let place = *level.visible.get(level.selection())?;
        let (content, child) = (level.nodes[place].content, level.children[place]);
        match (content, child) {
            (Content::Level(_), Some(next)) => {
                self.enter_level(next, now_ns);
                None
            }
            (Content::Level(_), None) => None,
            (Content::Setting(_), _) => {
                self.click_setting();
                None
            }
            (Content::Note(_), _) => {
                self.swing_line();
                None
            }
            (Content::Action(action), _) => {
                if action == Use::Robot || action == Use::Chat {
                    self.tree.level_mut().press(false, now_ns);
                }
                self.run_action(action, now_ns)
            }
        }
    }

    pub(in crate::ui) fn selected_content(&self) -> Option<Content> {
        let level = self.tree.level();
        level.node(level.selection()).map(|node| node.content)
    }

    pub(in crate::ui) fn enter_level(&mut self, next: usize, now_ns: u64) {
        let is_shown = self.pane_level() == Some(next);
        let is_in_place = is_shown || self.tree.levels[next].is_rail();
        match &mut self.tree.level_mut().shape {
            Shape::Stage(stage) => stage.tuck(true, now_ns),
            Shape::Menu(_) | Shape::Rail(_) if is_in_place => {}
            Shape::Menu(_) | Shape::Rail(_) => self.depart(FORWARD),
        }
        self.tree.path.push(next);
        let count = self.tree.levels[next].visible.len();
        match &mut self.tree.levels[next].shape {
            Shape::Stage(stage) => {
                stage.enter(now_ns);
                self.stack(now_ns);
                self.robot_sees(Sight::Dock, now_ns);
            }
            Shape::Menu(_) if is_shown => {
                self.deal_lines(List::Menu(next), |_| true);
            }
            Shape::Menu(_) => self.show_lines(List::Menu(next), count, Some(0)),
            Shape::Rail(_) => {
                self.rail = Rail::default();
                self.seat_level(next, true);
                self.show_lines(List::Menu(next), count, Some(0));
            }
        }
    }

    pub(in crate::ui) fn unlock(&mut self) {
        if !core::mem::replace(&mut self.is_unlocked, true) {
            self.reveal();
        }
    }

    pub(in crate::ui) fn gate(&self) -> Gate {
        Gate { is_unlocked: self.is_unlocked, is_linked: self.is_linked() }
    }

    // What each level shows, reckoned anew from what decides it; the unlock and the link's change come here. A line that stays stands on its node, one that goes leaves its place to the next.
    pub(in crate::ui) fn reveal(&mut self) {
        let gate = self.gate();
        for at in 0..self.tree.levels.len() {
            let (nodes, shown) = (self.tree.levels[at].nodes, &self.tree.levels[at].visible);
            let visible = visible_of(nodes, gate);
            if visible == *shown {
                continue;
            }
            let old = core::mem::replace(&mut self.tree.levels[at].visible, visible.clone());
            let to_new = |item: usize| old.get(item).and_then(|place| visible.iter().position(|shown| shown == place));
            match &mut self.tree.levels[at].shape {
                Shape::Stage(stage) => {
                    stage.reindex(visible.iter().map(|place| icon_of(&self.glyphs, &nodes[*place])).collect(), &to_new);
                }
                Shape::Menu(grid) | Shape::Rail(grid) => {
                    let selection = to_new(grid.selection).unwrap_or(grid.selection);
                    grid.set_items_at(visible.len(), selection);
                }
            }
            self.keep_rail_line(at, &to_new);
        }
        if self.screen.is_none() {
            self.stand_dial(self.tree.level().selection());
        }
        self.deal_menu();
    }

    pub(in crate::ui) fn show_on_level(&mut self, place: usize, now_ns: u64) -> Option<()> {
        let level = self.tree.level_mut();
        let (item, count) = (level.visible.iter().position(|at| *at == place)?, level.visible.len());
        match &mut level.shape {
            Shape::Stage(stage) => stage.show(item, now_ns),
            Shape::Menu(grid) | Shape::Rail(grid) => grid.set_items_at(count, item),
        }
        Some(())
    }

    // The place each level on the way in stands on, from home, as `stand_at` takes them.
    pub(in crate::ui) fn path_places(&self) -> Vec<usize> {
        let levels = &self.tree.levels;
        self.tree
            .path
            .iter()
            .filter_map(|level| levels[*level].visible.get(levels[*level].selection()).copied())
            .collect()
    }

    // Stands the tree on the places a route gives, from home.
    pub(in crate::ui) fn stand_at(&mut self, places: Vec<usize>, now_ns: u64) {
        for level in self.tree.path.split_off(1) {
            if let Shape::Stage(stage) = &mut self.tree.levels[level].shape {
                stage.clear();
            }
        }
        let last = places.len().saturating_sub(1);
        for (step, place) in places.into_iter().enumerate() {
            let level = self.tree.level_mut();
            let Some(item) = level.visible.iter().position(|at| *at == place) else {
                break;
            };
            level.stand(item, step < last);
            if let Some(next) = level.children[place].filter(|_| step < last) {
                self.tree.path.push(next);
            }
        }
        if self.tree.level().stage().is_some() {
            return self.stack(now_ns);
        }
        self.stand_dial(self.tree.level().selection());
        self.deal_menu();
    }

    fn stack(&mut self, now_ns: u64) {
        let docked = self.tree.path.len().saturating_sub(1);
        for (place, &level) in self.tree.path[..docked].iter().enumerate() {
            if let Shape::Stage(stage) = &mut self.tree.levels[level].shape {
                stage.lift(docked - 1 - place, now_ns);
            }
        }
    }

    pub(in crate::ui) fn tree_home(&mut self, leave_ns: Option<u64>) {
        for level in self.tree.path.split_off(1) {
            if let Shape::Stage(stage) = &mut self.tree.levels[level].shape {
                if let Some(now_ns) = leave_ns { stage.leave(now_ns) } else { stage.clear() }
            }
        }
        let home = &mut self.tree.levels[0];
        match (&mut home.shape, leave_ns) {
            (Shape::Stage(main), Some(now_ns)) => main.rewind(now_ns),
            (Shape::Stage(_), None) => home.stand(0, false),
            (Shape::Menu(_) | Shape::Rail(_), _) => {
                home.stand(0, false);
                self.rail = Rail::default();
                self.stand_dial(0);
                self.deal_menu();
            }
        }
    }

    pub(in crate::ui) fn level_back(&mut self, now_ns: u64) {
        let Some(&front) = self.tree.path.last().filter(|_| self.tree.path.len() > 1) else {
            return;
        };
        if let Shape::Stage(stage) = &mut self.tree.levels[front].shape {
            stage.leave(now_ns);
            self.tree.path.pop();
            if let Shape::Stage(stage) = &mut self.tree.level_mut().shape {
                stage.tuck(false, now_ns);
            }
            self.stack(now_ns);
            return self.robot_sees(Sight::Middle, now_ns);
        }
        let is_rail = self.tree.levels[front].is_rail();
        if self.pane_level() != Some(front) && !is_rail {
            self.depart(BACKWARD);
        }
        self.tree.path.pop();
        self.stand_dial(self.tree.level().selection());
        self.deal_menu();
        if is_rail {
            self.follow_rail_back(now_ns);
        }
    }

    pub(in crate::ui) fn turn_stage(&mut self, detents: i32, now_ns: u64) {
        if let Shape::Stage(stage) = &mut self.tree.level_mut().shape {
            stage.turn(detents, now_ns);
        }
    }

    pub(in crate::ui) fn run_action(&mut self, action: Use, now_ns: u64) -> Option<Action> {
        if !matches!(action, Use::Robot | Use::Forget) {
            self.depart(FORWARD);
        }
        self.uses.push(action);
        self.perform(action, now_ns)
    }

    // Does what a leaf's use does, as a click on its line does but for the passage and the count of uses.
    pub(in crate::ui) fn perform(&mut self, action: Use, now_ns: u64) -> Option<Action> {
        match action {
            Use::Robot => {
                self.settings.has_robot = !self.settings.has_robot;
                self.show_robot_place();
                if self.does_robot_live() {
                    self.robot.greet(now_ns);
                }
            }
            Use::Chat => self.open_chat(),
            Use::Forget => self.forget_thread(now_ns),
            Use::Open(destination) => return self.open_destination(destination),
        }
        None
    }

    pub(in crate::ui) fn open_destination(&mut self, destination: Destination) -> Option<Action> {
        match destination {
            Destination::PhotoCamera => self.open_finder(Kind::Photo),
            Destination::VideoCamera => self.open_finder(Kind::Video),
            Destination::Pictures => self.open_roll(Kind::Photo),
            Destination::Videos => self.open_roll(Kind::Video),
            Destination::Notes => self.open_memos(Shelf::Notes),
            Destination::Translations => self.open_memos(Shelf::Talks),
            Destination::News => self.open_memos(Shelf::News),
            Destination::Rundowns => self.open_rundowns(),
            Destination::Web => self.open_web(),
            Destination::Signal => self.open_signal(),
            Destination::Calls => self.open_calls(),
            Destination::Pokedex => self.open_pokedex(),
            Destination::Wifi => self.open_networks(),
            Destination::Brightness => self.open_brightness(),
            Destination::Volume => self.open_volume(),
            Destination::Themes => self.open_themes(),
            Destination::Colours => self.open_colours(),
            Destination::Clock => return self.open_clock(),
            Destination::Bell => self.open_bell(),
            Destination::Passphrase => self.change_passphrase(),
            Destination::SimPin => self.open_pin(),
            Destination::Tunnel => self.open_tunnel(),
            Destination::Cost => self.open_cost(),
            Destination::Power => self.open_power(),
            Destination::About => self.open_about(),
            Destination::Debug => self.open_debug(),
            Destination::Memory => self.open_memos(Shelf::Facts),
            Destination::Days => self.open_memos(Shelf::Days),
        }
        None
    }

    pub(in crate::ui) fn route_to(&self, is_target: &dyn Fn(Content) -> bool) -> Option<Vec<usize>> {
        route(self.home_nodes(), &|node| is_target(node.content), self.gate())
    }

    // Walks from home to the node a hand reaches it by, opening it where `is_opened`, else standing on it.
    pub(super) fn reach(
        &mut self,
        is_target: &dyn Fn(Content) -> bool,
        is_opened: bool,
        now_ns: u64,
    ) -> Option<Action> {
        let places = self.route_to(is_target)?;
        self.walk(places, is_opened, now_ns)
    }

    // Walks from home into the level of that name, as a hand enters it.
    pub(super) fn enter_menu(&mut self, menu: Menu, now_ns: u64) -> Option<Action> {
        let is_menu = |node: &Node| node.name == menu.to_name() && matches!(node.content, Content::Level(_));
        let places = route(self.home_nodes(), &is_menu, self.gate())?;
        self.walk(places, true, now_ns)
    }

    // Walks from home along the places a route gives, opening the last where `is_opened`, else standing on it.
    fn walk(&mut self, places: Vec<usize>, is_opened: bool, now_ns: u64) -> Option<Action> {
        self.depart(FORWARD);
        self.go_home(now_ns);
        let (mut action, last) = (None, places.len().saturating_sub(1));
        for (step, place) in places.into_iter().enumerate() {
            self.show_on_level(place, now_ns)?;
            if step < last || is_opened {
                action = self.open(now_ns);
            }
        }
        self.deal_menu();
        action
    }

    pub(in crate::ui) fn go_home(&mut self, now_ns: u64) {
        if self.key_home() {
            return;
        }
        self.drop_ring();
        self.let_go_staged();
        let has_screen = self.screen.is_some() || self.is_menu_under_home();
        if has_screen {
            self.depart(BACKWARD);
        }
        self.rolls.stamps.armed = None;
        self.rolls.carousel.clear();
        self.close_screen();
        self.tree_home((!has_screen).then_some(now_ns));
    }

    pub(in crate::ui) fn close_screen(&mut self) {
        self.screen = None;
        self.end_debug();
        self.recolour();
    }

    // Closes a screen the menu opened, whose line is let go of and shows anew.
    pub(in crate::ui) fn return_to_menu(&mut self, now_ns: u64) {
        self.close_screen();
        self.tree.level_mut().press(false, now_ns);
        self.deal_menu();
    }

    pub(in crate::ui) fn go_back(&mut self, now_ns: u64) {
        if self.screen == Some(Screen::Key) {
            return self.key_back();
        }
        if self.screen == Some(Screen::Ring) {
            return self.close_ring();
        }
        if self.screen == Some(Screen::Incoming) {
            return self.decline_call();
        }
        if self.screen == Some(Screen::Call) {
            return self.leave_call();
        }
        self.let_go_staged();
        if self.return_from_contacts() || self.return_from_chat(now_ns) || self.rail_back(now_ns) {
            return;
        }
        self.rolls.stamps.armed = None;
        let parent = self.screen.and_then(Screen::parent);
        if let Some(Screen::Roll(_)) = self.screen {
            (self.note, self.screen) = (None, parent);
            self.rolls.carousel.clear();
            return self.rolls.grid.open(false);
        }
        if self.screen.is_some() {
            self.depart(BACKWARD);
        }
        match parent {
            Some(Screen::Memos) => (self.screen, self.memos.open_entry, self.memos.fault) = (parent, None, None),
            Some(Screen::Signal) => self.go_back_to_signal(),
            Some(Screen::Bell) => {
                self.screen = parent;
                self.lay_bell_list();
            }
            Some(Screen::Rundowns) => {
                self.screen = parent;
                self.lay_rundown_list();
            }
            Some(Screen::Calls) => {
                self.screen = parent;
                self.lay_calls();
            }
            Some(_) => self.screen = parent,
            None if self.return_to_key() => {}
            None if self.screen.is_some() => self.return_to_menu(now_ns),
            None => self.level_back(now_ns),
        }
    }

    pub(in crate::ui) fn menu_item(&self, level: usize, index: usize) -> Option<Item> {
        self.tree.levels[level].node(index).map(|node| self.node_item(node))
    }

    // The line of a node, in a menu or in a screen's list.
    pub(in crate::ui) fn node_item(&self, node: &Node) -> Item {
        let mut word = String::new();
        self.write_node_word(node, &mut word);
        let item = match node.content {
            Content::Setting(_) | Content::Note(_) | Content::Action(Use::Forget) => Item::new(node.name, word),
            Content::Level(_) | Content::Action(_) => Item::lead(node.name, word),
        };
        item.with_icon(node.icon)
    }

    pub(in crate::ui) fn write_node_word(&self, node: &Node, text: &mut String) {
        let switch = if let Content::Level(nodes) = node.content { switch_of(nodes) } else { None };
        if let Some(setting) = switch {
            return self.write_setting(setting, text);
        }
        let action = match node.content {
            Content::Level(nodes) if visible(nodes, self.gate()).count() <= MAX_NAMED_CHILDREN => {
                for (at, child) in visible(nodes, self.gate()).enumerate() {
                    text.push_str(if at == 0 { "" } else { ", " });
                    text.push_str(child.name);
                }
                return;
            }
            Content::Level(nodes) if holds_tools(nodes) || nodes.iter().any(|child| leads_to_tools(child.content)) => {
                let _ = write!(text, "{} OF {} TOOLS ON", self.settings.tools.count_on(), Tool::ALL.len());
                return;
            }
            Content::Level(_) => return,
            Content::Setting(setting) => return self.write_setting(setting, text),
            Content::Note(word) => return text.push_str(word),
            Content::Action(action) => action,
        };
        let tally = self.tally;
        let Use::Open(destination) = action else {
            match action {
                Use::Chat => text.push_str(HOLD_TO_TALK),
                Use::Forget => text.push_str(FORGET_WORD),
                Use::Robot | Use::Open(_) => {}
            }
            return;
        };
        match destination {
            Destination::Wifi => text.push_str(self.networks.first_known()),
            Destination::Brightness => drop(write!(text, "{}%", self.settings.brightness)),
            Destination::Volume => drop(write!(text, "{}%, {SOUNDS} {}%", self.settings.volume, self.settings.sounds)),
            Destination::Themes => text.push_str(theme_name(self.settings.theme)),
            Destination::Colours => write_palette_name(self.settings.palette(), text),
            Destination::Clock => offset::write_text(text, self.settings.offset),
            Destination::Bell => match self.bell_count() {
                0 => text.push_str(NONE_SET),
                count => drop(write!(text, "{count} SET")),
            },
            Destination::SimPin => self.write_pin_word(text),
            Destination::Tunnel => text.push_str(self.tunnel_word()),
            Destination::Power => {
                if let Some((level, _)) = self.charge.reading {
                    let _ = write!(text, "{level}%");
                }
            }
            Destination::About => drop(write!(text, "{FIRMWARE} {}", crate::VERSION)),
            Destination::Cost => text.push_str(self.cost.total.as_deref().unwrap_or_default()),
            Destination::Pictures if tally.photos_today > 0 => {
                let _ = write!(text, "{}, {} TODAY", tally.photos, tally.photos_today);
            }
            Destination::Pictures if tally.photos > 0 => drop(write!(text, "{}", tally.photos)),
            Destination::Videos if tally.videos > 0 => drop(write!(text, "{}", tally.videos)),
            Destination::Notes if tally.notes > 0 => {
                let _ = write!(text, "{}", tally.notes);
                if let Some(seconds) = tally.newest_note_s {
                    text.push_str(", LAST ");
                    write_duration(text, seconds);
                }
            }
            Destination::News if tally.has_edition => text.push_str(TODAYS_READY),
            Destination::Rundowns => self.write_rundowns_word(text),
            Destination::Pokedex => text.push_str(self.pokedex_word()),
            Destination::Web => text.push_str(self.web_word()),
            Destination::Signal => self.write_signal_word(text),
            Destination::Calls => self.write_calls_word(text),
            Destination::Memory => text.push_str(MEMORY_WORD),
            Destination::Days => text.push_str(DAYS_WORD),
            _ => {}
        }
    }

    pub(in crate::ui) fn redeal_lines(&mut self, actions: &[Use]) -> bool {
        self.deal_menu();
        self.deal_settings();
        actions.iter().any(|action| self.does_line_show(|content| content.runs(*action)))
    }
}

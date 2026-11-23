use super::Screen;
use super::action::Action;
use super::voice::RequestList;
use crate::domain::gallery::{Kind, Stamp};
use crate::input::{self, Event};
use crate::ui::grid::Grid;
use crate::ui::robot::{Reaction, Sight};
use crate::ui::view::rooms::TextRoomId;
use crate::ui::view::settle::SCROLLING;
use crate::ui::{CARET_NS, Ui};
use alloc::string::String;

const SWIPE_EDGE_DIVISOR: i32 = 16;
const SWIPE_TRAVEL_DIVISOR: i32 = 8;
pub(super) const STEP_LINES: i32 = 4;

/// The screen that follows the device's pose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PoseScreen {
    Viewfinder,
    Videos,
}

pub(crate) struct Touch {
    top_start: Option<i32>,
    left_start: Option<i32>,
    pub(in crate::ui) is_spent: bool,
    pub(in crate::ui) start: (i32, i32),
    pub(in crate::ui) at: (i32, i32),
    pub(in crate::ui) start_ns: u64,
}

impl Touch {
    pub(in crate::ui) const fn down((x, y): (i32, i32), now_ns: u64, (width, height): (i32, i32)) -> Touch {
        let top_start = if y < height / SWIPE_EDGE_DIVISOR { Some(y) } else { None };
        let left_start = if x < width / SWIPE_EDGE_DIVISOR { Some(x) } else { None };
        Touch { top_start, left_start, is_spent: false, start: (x, y), at: (x, y), start_ns: now_ns }
    }

    pub(in crate::ui) fn is_swiped(&self, size: (i32, i32)) -> bool {
        self.swipe(size).is_some()
    }

    fn swipe(&self, (width, height): (i32, i32)) -> Option<Swipe> {
        let (x, y) = self.at;
        if self.top_start.is_some_and(|start| y - start >= height / SWIPE_TRAVEL_DIVISOR) {
            Some(Swipe::Home)
        } else if self.left_start.is_some_and(|start| x - start >= width / SWIPE_TRAVEL_DIVISOR) {
            Some(Swipe::Back)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Swipe {
    Home,
    Back,
}

pub(in crate::ui) fn tapped(grid: &Grid, touch: &Touch) -> Option<usize> {
    let item_at = |(x, y): (i32, i32)| grid.item_at(x, y);
    item_at(touch.start).filter(|item| item_at(touch.at) == Some(*item))
}

impl Ui {
    // The grid the button squeezes: the rail's while it turns beside the screen it opened, else the screen's.
    pub(in crate::ui) fn visible_grid(&self) -> Option<&Grid> {
        match (self.screen, self.visible_list()) {
            _ if self.does_rail_lead() => self.tree.level().menu(),
            (_, Some(list)) => Some(self.grid_of(list)),
            (Some(Screen::Grid(_)), None) => Some(&self.rolls.grid),
            (Some(Screen::Themes), None) => Some(&self.themes.cards_grid),
            _ => None,
        }
    }

    pub(in crate::ui) fn visible_grid_mut(&mut self) -> Option<&mut Grid> {
        if self.does_rail_lead() {
            return self.tree.level_mut().menu_mut();
        }
        self.screen_grid_mut()
    }

    // The grid of what the screen shows.
    pub(in crate::ui) fn screen_grid_mut(&mut self) -> Option<&mut Grid> {
        match (self.screen, self.visible_list()) {
            (_, Some(list)) => Some(self.grid_of_mut(list)),
            (Some(Screen::Grid(_)), None) => Some(&mut self.rolls.grid),
            (Some(Screen::Themes), None) => Some(&mut self.themes.cards_grid),
            _ => None,
        }
    }

    // Squeezes what the button rests on, a grid's place or a stage's icon, or lets it go.
    pub(super) fn squeeze(&mut self, is_down: bool, now_ns: u64) {
        let is_at_level = self.entered_screen().is_none();
        match self.visible_grid_mut() {
            Some(grid) => grid.press(is_down),
            None if is_at_level => self.tree.level_mut().press(is_down, now_ns),
            None => {}
        }
    }

    /// Writes a control's event as the console and the input file show it.
    pub fn write_event_line(&self, line: &mut String, event: &Event) {
        input::write_event_line(line, event, self.is_typing());
    }

    fn touch_at(&mut self, x: i32, y: i32, now_ns: u64) -> bool {
        let size = (self.ruler.width, self.ruler.height);
        let touch = self.touch.get_or_insert(Touch::down((x, y), now_ns, size));
        touch.at = (x, y);
        let start_y = touch.start.1;
        if let Some(id) = self.on_keys(start_y) {
            self.keyboard_mut(id).touch(x, y, now_ns);
            return true;
        }
        let Some(touch) = self.touch.as_mut().filter(|touch| !touch.is_spent) else {
            return false;
        };
        let Some(swipe) = touch.swipe(size) else {
            return false;
        };
        touch.is_spent = true;
        let is_dropped = swipe == Swipe::Back && self.drop_draft();
        if !is_dropped {
            self.leave_screen();
        }
        if self.pressed_on.is_some() {
            self.let_go(now_ns);
        }
        match swipe {
            _ if is_dropped => {}
            Swipe::Home => self.go_home(now_ns),
            Swipe::Back if self.is_reviewing() => self.finder.review = None,
            Swipe::Back => self.go_back(now_ns),
        }
        true
    }

    fn wheel_text(&mut self, detents: i32) {
        let Some(room) = self.shown_room() else {
            return;
        };
        let line_step = self.read_step();
        let swing = detents.signum() as f32 * SCROLLING.launch_speed(self.ruler.gap as f32);
        let (max_scroll, step, way) = match room {
            TextRoomId::Chat => (self.chat_max_scroll(), line_step, detents.signum()),
            TextRoomId::Conversation => (self.conversation_max_scroll(), line_step, detents.signum()),
            TextRoomId::Talk => (self.talk_excess(), STEP_LINES, detents.signum()),
            TextRoomId::Reader => (self.read_max_scroll(), line_step, -detents.signum()),
            TextRoomId::Page(paged) => (self.paged_max_scroll(paged), line_step, -detents.signum()),
        };
        let (at, scroll) = self.scroll_parts(room);
        let target = (*at + way * step).clamp(0, max_scroll);
        if target == *at {
            scroll.speed -= swing;
        }
        *at = target;
        if room == TextRoomId::Chat {
            self.note_chat_wheeled(detents);
        }
    }

    /// True if the last event changed the screen.
    pub const fn does_redraw(&self) -> bool {
        self.does_redraw
    }

    /// Notes a finger resting on the screen as use of the device.
    pub fn note_finger_rests(&mut self, now_ns: u64) {
        if !self.is_awake() {
            return;
        }
        (self.used_at, self.input_at) = (Some(now_ns), Some(now_ns));
        if self.robot_place.is_shown {
            self.robot.stir(now_ns);
        }
    }

    fn lift_finger(&mut self, screen: Option<Screen>, armed: Option<(usize, Stamp)>, now_ns: u64) -> Option<Action> {
        let touch = self.touch.take().filter(|touch| !touch.is_spent)?;
        let span = (touch.start, touch.at);
        if self.is_on_robot(touch.start) && self.is_on_robot(touch.at) && self.robot_beckon().is_some() {
            self.enter_rail_screen();
            return self.robot_beckon().and_then(|beckon| self.act_on_robot(beckon, now_ns));
        }
        match screen {
            _ if self.is_on_battery(span) => return Some(Action::Charge),
            Some(Screen::Grid(kind)) => self.tap(kind, &touch, armed, now_ns),
            Some(Screen::Roll(Kind::Photo)) => self.tap_large(&touch, armed, now_ns),
            _ if self.on_keys(touch.start.1).is_some() => {
                if let Some(id) = self.on_keys(touch.start.1)
                    && self.keyboard_mut(id).lift()
                {
                    self.end_typing(id, now_ns);
                }
            }
            Some(Screen::Keyboard) if self.is_on_eye(span) => self.networks.is_shown = !self.networks.is_shown,
            Some(Screen::Pin) if self.is_on_eye(span) => self.pin.is_shown = !self.pin.is_shown,
            _ => {}
        }
        None
    }

    fn press(&mut self, now_ns: u64) {
        self.hold_start = Some(now_ns);
        if let Some(voice) = self.talk_screen() {
            self.press_home(voice, now_ns);
        }
        self.squeeze(true, now_ns);
    }

    fn click(&mut self, screen: Option<Screen>, now_ns: u64) -> Option<Action> {
        if self.hold_start.take().is_none() {
            // A hold on a list that takes a request ends as the button is let go, and what was said goes.
            for list in RequestList::ALL {
                self.release_request(list);
            }
            return None;
        }
        self.squeeze(false, now_ns);
        if self.does_rail_lead() {
            self.enter_rail_screen();
            return None;
        }
        if self.click_setting() {
            return None;
        }
        match screen {
            _ if self.keys_up().is_some() => {
                if let Some(id) = self.keys_up()
                    && self.keyboard_mut(id).ask_done()
                {
                    self.end_typing(id, now_ns);
                }
            }
            Some(Screen::Debug) => return self.pick_debug(),
            Some(Screen::Talk) => self.click_talk(),
            Some(
                Screen::Chat
                | Screen::Pokedex
                | Screen::Web
                | Screen::Conversation
                | Screen::Safe
                | Screen::Link
                | Screen::Keyboard
                | Screen::Pin,
            ) => {}
            Some(Screen::Signal) => self.pick_signal(),
            Some(Screen::Calls) => self.pick_call(),
            Some(Screen::CallChoice) => self.pick_call_choice(),
            Some(Screen::Call) => self.click_call(),
            Some(Screen::Incoming) => self.pick_answer(),
            Some(Screen::Finder(kind)) => {
                // A click on the review of the photo just taken goes back to the stream, ready for the next.
                if self.finder.review.take().is_some() {
                    return None;
                }
                if kind == Kind::Photo {
                    self.react(Reaction::Flash, now_ns);
                    self.begin_review();
                }
                return Some(Action::Shoot);
            }
            Some(Screen::Grid(kind)) => self.view_item(kind),
            Some(Screen::Networks) => self.pick_network(),
            Some(Screen::Known) => self.act_on_known(now_ns),
            Some(Screen::Cost) => self.pick_cost(),
            Some(Screen::Spending) => self.pick_spending(),
            Some(Screen::Share) => self.pick_share(),
            Some(Screen::Power) => return self.pick_power(now_ns),
            Some(Screen::About) => self.pick_about(now_ns),
            Some(Screen::Themes) => self.pick_theme(now_ns),
            Some(Screen::Colours) => self.pick_colours(now_ns),
            Some(Screen::Memos) => self.pick_memo(),
            Some(Screen::Memo) => self.act_on_memo(now_ns),
            Some(Screen::Roll(Kind::Video)) => return Some(Action::Play),
            Some(Screen::Clock) => self.choose_next_part(),
            Some(Screen::Bell) => self.pick_bell(),
            Some(Screen::BellEntry) => self.act_on_bell_entry(now_ns),
            Some(Screen::Ring) => self.close_ring(),
            Some(Screen::Rundowns) => self.pick_rundown(now_ns),
            Some(Screen::Rundown) => self.act_on_rundown(now_ns),
            Some(Screen::Volume) => self.choose_next_channel(),
            Some(Screen::Roll(Kind::Photo) | Screen::Brightness) => self.go_back(now_ns),
            Some(Screen::Reader) => self.reader.room.scroll.speed -= SCROLLING.launch_speed(self.ruler.gap as f32),
            Some(Screen::Key) => return self.click_key(now_ns),
            None => return self.open(now_ns),
        }
        None
    }

    fn wheel(&mut self, detents: i32, screen: Option<Screen>, now_ns: u64) -> Option<Action> {
        match screen {
            _ if self.does_rail_turn() => self.turn_rail(detents),
            _ if self.staged_shown().is_some() => self.step_staged(detents),
            Some(screen) if TextRoomId::of(screen).is_some() => self.wheel_text(detents),
            _ if self.keys_up().is_some() => {
                if let Some(id) = self.keys_up() {
                    self.keyboard_mut(id).wheel(detents);
                }
            }
            Some(Screen::Networks) => self.wheel_networks(detents),
            Some(Screen::Power) => self.wheel_power(detents),
            Some(Screen::Themes) => self.wheel_themes(detents),
            Some(Screen::Colours) => self.wheel_colours(detents),
            Some(Screen::Memos) => self.wheel_memos(detents),
            Some(Screen::Memo) => self.wheel_memo(detents),
            Some(Screen::Rundown) => self.wheel_rundown(detents),
            Some(Screen::Key) => self.wheel_key(detents),
            Some(Screen::Link | Screen::Safe) => {}
            Some(Screen::Brightness | Screen::Volume) => self.adjust(-detents.signum()),
            Some(Screen::Clock) => return self.wheel_clock(-detents.signum()),
            Some(Screen::Finder(Kind::Video)) if self.finder.recording.is_some() => {}
            Some(Screen::Finder(_)) => self.turn_camera(detents),
            Some(Screen::Call) if self.is_video_call() => self.turn_call_camera(detents),
            Some(Screen::Grid(_) | Screen::Roll(_)) => self.step_roll(detents),
            _ => match self.visible_list() {
                Some(list) => self.wheel_lines(list, detents),
                None if screen.is_none() => self.turn_stage(detents, now_ns),
                None => {}
            },
        }
        None
    }

    /// Takes a control's event at `now_ns` and returns the action it asks of the device.
    pub fn handle(&mut self, event: Event, now_ns: u64) -> Option<Action> {
        self.is_button_down = event.button_down().unwrap_or(self.is_button_down);
        if let Some(doze) = &mut self.doze {
            self.does_redraw = false;
            return event.button_down().is_some_and(|is_down| doze.take_button(is_down)).then_some(Action::Wake);
        }
        let event = match event {
            Event::Wheel(detents) => Event::Wheel(self.wheel_way(detents)),
            event => event,
        };
        self.used_at = Some(now_ns);
        self.input_at = Some(now_ns);
        self.caret.start_ns = now_ns;
        self.caret.due_ns = self.caret.due_ns.map(|_| now_ns + CARET_NS);
        self.does_redraw = true;
        let pressed = if matches!(event, Event::PttUp) { self.pressed_on.take() } else { None };
        if let Some(does_show) = self.swallow(event) {
            if event == Event::PttDown {
                self.press_waking(false, now_ns);
            }
            self.does_redraw = does_show;
            return None;
        }
        if self.take_silence(event) {
            return None;
        }
        if self.safe.is_shut || self.screen == Some(Screen::Safe) {
            self.handle_safe(event, now_ns);
            return None;
        }
        let armed = if matches!(event, Event::TouchAt { .. }) { None } else { self.rolls.stamps.armed.take() };
        let was_asleep = self.robot.is_asleep();
        if self.robot_place.is_shown {
            self.robot.stir(now_ns);
            if matches!(event, Event::Wheel(_) | Event::PttDown) {
                self.robot_sees(Sight::Middle, now_ns);
            }
        }
        match event {
            Event::PttDown => self.pressed_on = Some(self.screen),
            Event::PttUp if pressed.is_some_and(|screen| screen != self.screen) => {
                self.let_go(now_ns);
                return None;
            }
            Event::PttUp => self.let_go_home(),
            _ => {}
        }
        let screen = self.entered_screen();
        match (event, screen) {
            (Event::TouchAt { x, y }, _) => {
                if self.touch.is_none() && !was_asleep {
                    self.robot_feels((x, y), now_ns);
                }
                self.does_redraw = self.touch_at(x, y, now_ns);
                None
            }
            (Event::TouchUp, _) => self.lift_finger(screen, armed, now_ns),
            (Event::PttDown, Some(Screen::Chat)) => {
                self.press_chat(now_ns);
                None
            }
            (Event::PttUp, Some(Screen::Chat)) => {
                self.release_chat();
                None
            }
            (Event::PttDown, Some(Screen::Pokedex)) => {
                self.press_pokedex(now_ns);
                None
            }
            (Event::PttUp, Some(Screen::Pokedex)) => {
                self.release_pokedex(now_ns);
                None
            }
            (Event::PttDown, Some(Screen::Web)) => {
                self.press_web(now_ns);
                None
            }
            (Event::PttUp, Some(Screen::Web)) => {
                self.release_web(now_ns);
                None
            }
            (Event::PttDown, Some(Screen::Conversation)) => {
                self.press_conversation(now_ns);
                None
            }
            (Event::PttUp, Some(Screen::Conversation)) => {
                self.release_conversation(now_ns);
                None
            }
            (Event::PttDown, _) => {
                self.press(now_ns);
                None
            }
            (Event::PttUp, _) => self.click(screen, now_ns),
            (Event::Wheel(detents), _) => self.wheel(detents, screen, now_ns),
        }
    }
}

use crate::base::math::round;
use crate::domain::settings;
use crate::gfx::Rect;
use crate::gfx::icon::Icon;
use crate::ui::Ui;
use crate::ui::grid::Grid;
use crate::ui::kit::read_icon;
use crate::ui::robot::{Activity, Scene, Seat};
use crate::ui::ruler::HOME_SCALE;
use crate::ui::screen::Screen;
use crate::ui::screen::tree::{HOME, NOTHING, Shape};
use crate::ui::screen::voice::PageId;

fn sight_of(area: Rect) -> (f32, f32) {
    let (x, y) = area.middle();
    (x as f32, y as f32)
}

impl Ui {
    pub(in crate::ui) fn show_robot_place(&mut self) {
        let home = if self.settings.has_robot { read_icon(NOTHING) } else { self.sleeper(HOME_SCALE) };
        if let Some(Shape::Stage(main)) = self.tree.levels.first_mut().map(|main| &mut main.shape) {
            main.replace(HOME, home);
        }
    }

    fn sleeper(&self, boxes: f32) -> Icon {
        self.robot.asleep_icon().fit(self.robot.extent(), boxes * self.robot.shape().0)
    }

    pub(in crate::ui) fn bar_seat(&self) -> Seat {
        let (margin, width) = (self.ruler.gap as f32, self.ruler.bar_robot_width());
        let x = self.ruler.width as f32 - margin - width;
        Seat { x, y: margin, width, height: self.ruler.visible(), quarters: 0, turn: 0.0 }
    }

    pub(in crate::ui) fn seat_in_square(&self, left: i32, top: i32) -> Seat {
        let ((width, height), side) = (self.ruler.home_box(), self.ruler.home_side() as f32);
        let (x, y) = (left as f32 + (side - width) / 2.0, top as f32 + (side - height) / 2.0);
        Seat { x, y, width, height, quarters: 0, turn: 0.0 }
    }

    pub(in crate::ui) fn robot_seat(&self) -> Seat {
        let quarters = self.quarters();
        match self.screen {
            None if self.is_at_home() => self.home_seat(),
            Some(Screen::Chat) if self.is_chat_robot_large() => self.chat_seat(),
            Some(Screen::Key | Screen::Safe | Screen::Ring | Screen::Incoming) => self.window_seat(),
            Some(Screen::Roll(_)) => {
                let (margin, width, height) =
                    (self.ruler.gap as f32, self.ruler.bar_robot_width(), self.ruler.visible());
                let screen = Rect::new(0, 0, self.ruler.width, self.ruler.height);
                let view_width = screen.view_size(quarters).0 as f32;
                let area = screen.place(
                    quarters,
                    round(view_width - margin - width),
                    round(margin),
                    round(width),
                    round(height),
                );
                Seat {
                    x: area.x as f32,
                    y: area.y as f32,
                    width: area.w as f32,
                    height: area.h as f32,
                    quarters,
                    turn: 0.0,
                }
            }
            Some(Screen::Finder(_)) if quarters % 2 == 1 => {
                let (shape_width, shape_height) = self.robot.shape();
                let width = self.ruler.visible() * shape_height / shape_width;
                Seat { x: self.ruler.width as f32 - self.ruler.gap as f32 - width, width, quarters, ..self.bar_seat() }
            }
            Some(Screen::Finder(_)) => Seat { quarters, ..self.bar_seat() },
            _ => self.bar_seat(),
        }
    }

    pub(in crate::ui) fn seat(&self) -> Seat {
        let [x, y, width, height] = self.robot_place.at.map(|edge| edge.at);
        Seat { x, y, width, height, quarters: self.robot_place.quarters, turn: self.robot_place.upright.at }
    }

    fn chat_sight(&self) -> (f32, f32) {
        let room = self.chat_rest_room();
        let middle = self.ruler.centre_left(0) as f32;
        if self.chat.is_below {
            return (middle, room.bottom() as f32);
        }
        let visible = self.chat.height - self.chat_offset();
        let newest = room.y + visible.clamp(0, room.h) - self.ruler.small() / 2;
        (middle, newest.max(room.y) as f32)
    }

    fn conversation_sight(&self) -> (f32, f32) {
        let room = self.conversation_visible_room();
        let offset = round(self.signal.room.scroll.at);
        let newest = room.y + (self.signal.height - offset).clamp(0, room.h) - self.ruler.small() / 2;
        (self.ruler.centre_left(0) as f32, newest.max(room.y) as f32)
    }

    fn talk_sight(&self) -> (f32, f32) {
        let (room, line) = (self.ruler.under_bar(), self.ruler.small());
        let lines = (self.translator.remarks.len() as i32).min(room.h / line.max(1));
        (self.ruler.centre_left(0) as f32, (room.y + self.talk_lift() + (lines * line - line / 2).max(0)) as f32)
    }

    fn scene_and_sight(&self, middle: (f32, f32)) -> (Scene, Option<(f32, f32)>) {
        let centre = (self.ruler.width as f32 / 2.0, self.ruler.height as f32 / 2.0);
        let pane_centre = (self.ruler.centre_left(0) as f32, centre.1);
        let selection = |grid: &Grid| grid.selection_place().map(sight_of);
        let rapt = |is_rapt: bool| if is_rapt { Scene::Rapt } else { Scene::Calm };
        match self.screen {
            None if self.do_menus_show() && !self.is_at_home() => {
                (Scene::Calm, self.tree.level().menu().and_then(selection))
            }
            None | Some(Screen::Volume | Screen::Brightness) => (Scene::Free, None),
            Some(Screen::Chat) => (Scene::Calm, Some(self.chat_sight())),
            Some(Screen::Pokedex) => (Scene::Calm, Some(sight_of(self.paged_visible_room(PageId::Pokedex)))),
            Some(Screen::Web) => (rapt(self.is_voiced()), Some(sight_of(self.paged_visible_room(PageId::Web)))),
            Some(Screen::Conversation) => (Scene::Calm, Some(self.conversation_sight())),
            Some(Screen::Talk) => (Scene::Calm, Some(self.talk_sight())),
            Some(Screen::Grid(_)) => (Scene::Calm, selection(&self.rolls.grid)),
            Some(Screen::Keyboard | Screen::Pin) => {
                let top = self.keys_up().map_or(self.ruler.height, |id| self.keyboard(id).top());
                (Scene::Calm, Some((pane_centre.0, (self.ruler.bar() + top) as f32 / 2.0)))
            }
            Some(Screen::Networks) => {
                let listening = self.networks.is_listening.then_some((middle.0, middle.1 - 1.0));
                (Scene::Calm, selection(&self.networks.list).or(listening))
            }
            Some(Screen::Themes) => (Scene::Calm, selection(&self.themes.cards_grid)),
            Some(Screen::Memos) => (rapt(self.memos.recording.start_ns.is_some()), selection(&self.memos.grid)),
            Some(Screen::Memo) => (rapt(self.memos.playing.is_some()), selection(&self.memos.sheet)),
            Some(Screen::Reader) => (rapt(self.is_voiced()), Some(pane_centre)),
            Some(Screen::Clock) => {
                (Scene::Calm, Some((pane_centre.0, (self.time_top() + self.ruler.large() / 2) as f32)))
            }
            Some(Screen::Roll(_)) => (rapt(self.is_playing()), Some(centre)),
            Some(Screen::Finder(_)) => (rapt(self.finder.recording.is_some()), Some(centre)),
            Some(Screen::Key) if self.keying.is_listed() => (Scene::Calm, selection(&self.keying.grid)),
            Some(Screen::Key) => (Scene::Calm, Some(sight_of(self.scan_square()))),
            Some(Screen::Safe) => (Scene::Calm, Some(sight_of(self.safe_field()))),
            Some(Screen::Call) if self.is_video_call() => {
                (Scene::Calm, Some(sight_of(self.far_place().map_or(self.far_area(), |(_, area)| area))))
            }
            Some(_) => (Scene::Calm, self.visible_list().and_then(|list| selection(self.grid_of(list)))),
        }
    }

    pub(in crate::ui) fn brief_robot(&mut self, seat: &Seat, now_ns: u64) {
        let middle = (seat.x + seat.width / 2.0, seat.y + seat.height / 2.0);
        let (scene, sight) = self.scene_and_sight(middle);
        let (dimmest, brightest) = (f32::from(settings::DIMMEST), f32::from(settings::BRIGHTEST));
        let glare = match self.screen {
            Some(Screen::Brightness) => {
                (2.0 * f32::from(self.settings.brightness) - dimmest - brightest) / (brightest - dimmest)
            }
            _ => 0.0,
        };
        let focus = self.keys_up().and_then(|id| self.keyboard(id).focus());
        let sight = focus.map(|(x, y)| (x as f32, y as f32)).or(sight);
        let activity = self.activity();
        let sight = if activity == Activity::Painting && self.do_photos_show() {
            Some(sight_of(self.row_box()))
        } else {
            sight
        };
        let (x, y) = sight.map_or((0.0, 0.0), |(x, y)| (x - middle.0, y - middle.1));
        let (x, y) = match seat.quarters % 4 {
            0 => (x, y),
            1 => (y, -x),
            2 => (-x, -y),
            _ => (-y, x),
        };
        let (width, height) = (self.ruler.width as f32, self.ruler.height as f32);
        let headroom = match seat.quarters % 4 {
            0 => seat.y / seat.height.max(1.0),
            1 => (width - seat.x - seat.width) / seat.width.max(1.0),
            2 => (height - seat.y - seat.height) / seat.height.max(1.0),
            _ => seat.x / seat.width.max(1.0),
        };
        self.robot.scene = scene;
        self.robot.watch(x, y);
        self.robot.glare = glare;
        self.robot.set_activity(activity);
        self.robot.set_headroom(headroom);
        self.robot.is_hushed = self.is_typing_text();
        let beckon = self.robot_beckon();
        let previous = core::mem::replace(&mut self.robot_place.beckon, beckon);
        let is_new = beckon.is_some() && previous != beckon;
        self.robot.beckon(beckon.is_some(), is_new, self.input_at.unwrap_or(now_ns), now_ns);
        if scene == Scene::Rapt || activity != Activity::Idle {
            self.robot.stir(now_ns);
        }
    }
}

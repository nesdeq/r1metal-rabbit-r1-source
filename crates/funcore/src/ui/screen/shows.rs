use super::input::PoseScreen;
use super::{Screen, View};
use crate::domain::gallery::{self, Kind};
use crate::domain::scene::Scene;
use crate::input::pose::Pose;
use crate::ui::Ui;
use alloc::string::String;

impl Ui {
    pub(in crate::ui) fn quarters(&self) -> u8 {
        match self.screen {
            Some(Screen::Grid(kind) | Screen::Roll(kind)) => gallery::quarters(kind, self.pose),
            Some(Screen::Key | Screen::Safe | Screen::Ring | Screen::Incoming) => 0,
            _ => self.pose.quarters(),
        }
    }

    /// The scene on the screen: the menu's while the rail turns beside the screen it opened.
    pub fn scene(&self) -> Scene {
        let Some(screen) = self.entered_screen() else {
            let level = self.tree.level();
            let name = level.node(level.selection()).map_or("", |node| node.name);
            return match (level.stage().is_some(), self.tree.front()) {
                (true, _) => Scene::Stage(name),
                (false, 0) => Scene::Home(name),
                (false, _) => Scene::Menu(name),
            };
        };
        match screen {
            Screen::Debug => Scene::Debug,
            Screen::Keyboard => Scene::Keyboard,
            Screen::Pin => Scene::Pin,
            Screen::Networks => Scene::Networks,
            Screen::Known => Scene::Known,
            Screen::Cost => Scene::Cost,
            Screen::Spending => Scene::Spending(self.spending().map_or("", |page| page.name)),
            Screen::Share => Scene::Share(self.share().map_or("", |page| page.name)),
            Screen::Power => Scene::Power,
            Screen::About => Scene::About,
            Screen::Memos => Scene::Entries(self.memos.shelf),
            Screen::Memo => Scene::Entry(self.memos.shelf),
            Screen::Reader => Scene::Reader,
            Screen::Talk => Scene::Talk,
            Screen::Chat => Scene::Chat { is_muted: self.chat.is_muted },
            Screen::Pokedex => Scene::Pokedex { is_typing: self.pokedex.is_typing },
            Screen::Web => Scene::Web { is_typing: self.web.is_typing },
            Screen::Link => Scene::Link,
            Screen::Signal => Scene::Signal,
            Screen::Conversation => Scene::Conversation { is_typing: self.signal.is_typing },
            Screen::Calls => Scene::Calls,
            Screen::CallChoice => Scene::CallChoice,
            Screen::Call => Scene::Call { is_video: self.is_video_call() },
            Screen::Incoming => Scene::Incoming,
            Screen::Finder(kind) => Scene::Finder(kind),
            Screen::Grid(kind) => Scene::Grid(kind),
            Screen::Roll(kind) => Scene::Roll(kind),
            Screen::Brightness => Scene::Brightness,
            Screen::Volume => Scene::Volume,
            Screen::Themes => Scene::Themes,
            Screen::Colours => Scene::Colours,
            Screen::Clock => Scene::Clock,
            Screen::Bell => Scene::Bell,
            Screen::BellEntry => Scene::BellEntry,
            Screen::Ring => Scene::Ring,
            Screen::Rundowns => Scene::Rundowns,
            Screen::Rundown => Scene::Rundown,
            Screen::Key => self.keying.scene(),
            Screen::Safe => Scene::Safe(self.safe.scene_step()),
        }
    }

    /// True while a screen shows that turns with the device.
    pub fn does_turn_show(&self) -> bool {
        matches!(self.screen, Some(Screen::Finder(_) | Screen::Roll(Kind::Video)))
    }

    pub(in crate::ui) fn pose_screen(&self) -> Option<PoseScreen> {
        if !self.is_awake() {
            return None;
        }
        match self.screen {
            Some(Screen::Finder(_)) => Some(PoseScreen::Viewfinder),
            Some(Screen::Grid(Kind::Video) | Screen::Roll(Kind::Video)) => Some(PoseScreen::Videos),
            _ => None,
        }
    }

    /// True while the screen follows the pose and the part that serves it is there.
    pub fn does_heed(&self, has_camera: bool, has_gallery: bool) -> bool {
        match self.pose_screen() {
            Some(PoseScreen::Viewfinder) => has_camera,
            Some(PoseScreen::Videos) => has_gallery,
            None => false,
        }
    }

    /// Takes the device's pose; true if the screen turns with it.
    #[must_use]
    pub fn set_pose(&mut self, pose: Pose) -> bool {
        let does_turn = core::mem::replace(&mut self.pose, pose) != pose && self.does_turn_show();
        if does_turn {
            self.rolls.carousel.rest();
        }
        does_turn
    }

    pub(in crate::ui) fn view_of<'a>(&'a self, screen: Option<Screen>, word: &'a mut String, now_ns: u64) -> View<'a> {
        let Some(screen) = screen else {
            return self.view_menus(word);
        };
        match screen {
            Screen::Debug => self.view_debug(),
            Screen::Finder(kind) => View::Finder(kind),
            Screen::Keyboard => self.view_keyboard(),
            Screen::Pin => self.view_pin(),
            Screen::Networks => self.view_networks(word),
            Screen::Known => self.view_known(),
            Screen::Cost => self.view_cost(),
            Screen::Spending => self.view_spending(),
            Screen::Share => self.view_share(),
            Screen::Power => self.view_power(word),
            Screen::About => self.view_about(),
            Screen::Memos => self.view_memos(word, now_ns),
            Screen::Memo => self.view_memo(),
            Screen::Reader => self.view_reader(),
            Screen::Talk => self.view_talk(word, now_ns),
            Screen::Chat => self.view_chat(word, now_ns),
            Screen::Pokedex => self.view_pokedex(word, now_ns),
            Screen::Web => self.view_web(word, now_ns),
            Screen::Link => self.view_link(),
            Screen::Signal => self.view_signal(),
            Screen::Conversation => self.view_conversation(word, now_ns),
            Screen::Calls => self.view_calls(word),
            Screen::CallChoice => self.view_call_choice(),
            Screen::Call => self.view_call(word, now_ns),
            Screen::Incoming => self.view_incoming(),
            Screen::Grid(kind) | Screen::Roll(kind) => self.view_gallery(kind, word),
            Screen::Brightness => self.view_brightness(word),
            Screen::Volume => self.view_volume(word),
            Screen::Themes => self.view_themes(word),
            Screen::Colours => self.view_colours(word),
            Screen::Clock => self.view_clock(word),
            Screen::Bell => self.view_bell(word, now_ns),
            Screen::BellEntry => self.view_bell_entry(),
            Screen::Ring => self.view_ring(),
            Screen::Rundowns => self.view_rundowns(word, now_ns),
            Screen::Rundown => self.view_rundown(),
            Screen::Key => self.view_key(),
            Screen::Safe => self.view_safe(),
        }
    }
}

use super::bar::BarWords;
use super::typing::KeyboardId;
use super::voice::{PageId, ScreenPage};
use super::{Screen, View};
use crate::codec::http::Target;
use crate::domain::chat::tools::Destination;
use crate::domain::scene::{KEYS, WEB};
use crate::domain::uses::Use;
use crate::domain::voice::{Bid, Phase};
use crate::domain::web::Page;
use crate::ui::keys::take_typed;
use crate::ui::words::{HOLD_TO_TALK, VOICE};
use crate::ui::{HOLD_TO_TALK_NS, Ui};
use alloc::string::String;
use alloc::vec::Vec;

const TYPE_A_REQUEST: &str = "TYPE WHERE TO GO";
const SAY_OR_TYPE: &str = "SAY OR TYPE WHERE TO GO";
// What the room tells while no page shows.
pub(super) const START: &str = "# Web\n\nHold the button and say where to go:\n\n- a site, as **the Guardian**\n- a topic or a question\n- on a page, a link's **number** or what it is about\n- **back**\n\nClick the button to type instead. Touch the robot to hear the page.";

impl Ui {
    /// True while the browser shows and the device is awake.
    pub fn is_web_wanted(&self) -> bool {
        self.is_awake() && self.does_web_show()
    }

    pub(crate) fn does_web_show(&self) -> bool {
        self.screen == Some(Screen::Web)
    }

    /// The bids the browser's screen made since this was last taken.
    pub fn take_web_bids(&mut self) -> Vec<Bid> {
        core::mem::take(&mut self.web.bids)
    }

    pub(super) fn open_web(&mut self) {
        self.screen = Some(Screen::Web);
        self.hush();
        if self.web.page.is_none() {
            self.lay_paged(PageId::Web);
        }
        self.open_paged(PageId::Web);
    }

    /// Takes a visit's phase and the page to show; true if the screen changed.
    #[must_use]
    pub fn set_web(&mut self, phase: Phase, page: Option<Page>, now_ns: u64) -> bool {
        let has_new_phase = self.take_paged_phase(PageId::Web, phase, now_ns);
        let has_page = page.is_some();
        if let Some(page) = page {
            // A reading of the page before says nothing of this one.
            self.hush();
            let spoken = page.to_spoken();
            // The bar names the site the page came from, or the page's title where the model wrote it.
            let title = Target::parse(&page.address).map_or(page.title, |target| target.host).to_uppercase();
            self.show_page(PageId::Web, ScreenPage { title, text: page.text, picture: None, spoken }, now_ns);
        }
        let is_line = has_page && self.redeal_lines(&[Use::Open(Destination::Web)]);
        (has_new_phase || has_page) && self.does_web_show() || is_line
    }

    // The page's words as a reading aloud says them, and the block that stands at the top of its room.
    pub(super) fn web_script(&self) -> Option<(&str, usize)> {
        let page = self.web.page.as_ref().filter(|_| self.does_web_show())?;
        Some((page.spoken.as_str(), self.web.rows.top_block(self.web.room.at)))
    }

    pub(super) fn press_web(&mut self, now_ns: u64) {
        self.hold_start = Some(now_ns);
        if !self.web.is_typing {
            // The microphone would hear a reading of the page.
            self.hush();
            self.web.voice.is_talking = true;
            self.web.bids.push(Bid::Listen);
        }
    }

    // A hold says where to go; a click raises the keys, and with the keys up sends what is typed or, with nothing typed, lowers them.
    pub(super) fn release_web(&mut self, now_ns: u64) {
        let hold_ns = self.hold_start.take().map(|start_ns| now_ns.saturating_sub(start_ns));
        if self.web.is_typing {
            if hold_ns.is_some() {
                self.send_web(now_ns);
            }
            return;
        }
        if !core::mem::take(&mut self.web.voice.is_talking) {
            return;
        }
        if hold_ns.is_some_and(|hold_ns| hold_ns < HOLD_TO_TALK_NS) {
            self.web.bids.push(Bid::Drop);
            self.toggle_paged_keys(PageId::Web, now_ns);
        } else {
            self.web.bids.push(Bid::Send);
        }
    }

    pub(super) fn send_web(&mut self, now_ns: u64) {
        if let Some(text) = take_typed(&mut self.web.keys) {
            self.hush();
            self.web.bids.push(Bid::Text(text));
        }
        self.toggle_paged_keys(PageId::Web, now_ns);
    }

    pub(in crate::ui) fn leave_web(&mut self) {
        if self.does_web_show() {
            self.web.keys.release();
            self.let_go_web();
            self.hush();
        }
    }

    pub(crate) fn let_go_web(&mut self) {
        if core::mem::take(&mut self.web.voice.is_talking) {
            self.web.bids.push(Bid::Drop);
        }
    }

    pub(super) fn web_word(&self) -> &str {
        self.web.page.as_ref().map_or(SAY_OR_TYPE, |page| page.title.as_str())
    }

    pub(super) fn view_web<'a>(&'a self, word: &'a mut String, now_ns: u64) -> View<'a> {
        if !self.web.voice.write_word(now_ns, word) {
            let idle = if self.web.is_typing { TYPE_A_REQUEST } else { HOLD_TO_TALK };
            word.push_str(self.aloud_word().unwrap_or(if self.web.page.is_none() { idle } else { "" }));
        }
        let status: &'a str = word;
        let words = BarWords {
            status: Some(status).filter(|status| !status.is_empty()),
            subject: self.web.page.as_ref().map(|page| page.title.as_str()),
            state: Some(if self.web.is_typing { KEYS } else { VOICE }),
            ..BarWords::default()
        };
        let keys = Some((KeyboardId::Web, self.web.band.at));
        View::Text { name: WEB, words, room: self.web.room.plate.as_room(), keys }
    }
}

use super::{Screen, Stopwatch, seconds_since, web};
use crate::base::time::write_duration;
use crate::domain::voice::{Bid, Phase};
use crate::gfx::Picture;
use crate::gfx::motion::Edge;
use crate::hw::sound::loudness;
use crate::ui::Ui;
use crate::ui::gauge::lit_blocks;
use crate::ui::item::SIGNAL_BLOCKS;
use crate::ui::keys::{Keyboard, raise};
use crate::ui::robot::Reaction;
use crate::ui::textroom::{Plate, TextRoom, TextRows};
use crate::ui::view::lines::List;
use crate::ui::words::{CONNECTING, LISTENING, READING, TRANSCRIBING, cause_word};
use alloc::string::String;
use alloc::vec::Vec;

const SEARCHING: &str = "SEARCHING";
const THINKING: &str = "THINKING";
const WAKING: &str = "WAKING UP";

// The microphone's level: the largest sample of the block it recorded last, and the blocks that show it.
#[derive(Default)]
pub(crate) struct Meter {
    peak: u32,
    pub(in crate::ui) blocks: [Edge; SIGNAL_BLOCKS],
}

// A screen of one page under the bar that a request said or typed brings: the Pokédex's or the browser's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) enum PageId {
    Pokedex,
    Web,
}

impl PageId {
    const fn screen(self) -> Screen {
        match self {
            PageId::Pokedex => Screen::Pokedex,
            PageId::Web => Screen::Web,
        }
    }
}

// The page such a screen shows: its name on the bar, its words as its room lays them out, the picture over them where it has one, and its words as a reading aloud says them.
pub(super) struct ScreenPage {
    pub(super) title: String,
    pub(super) text: String,
    pub(super) picture: Option<Picture>,
    pub(super) spoken: String,
}

// What such a screen keeps: its voice, whether its keys are up, the keys and how far their band stands down, the bids it made, its page, and its rows in their room.
pub(crate) struct PageScreen {
    pub(crate) voice: Voice,
    pub(super) is_typing: bool,
    pub(in crate::ui) keys: Keyboard,
    pub(crate) band: Edge,
    pub(super) bids: Vec<Bid>,
    pub(super) page: Option<ScreenPage>,
    pub(in crate::ui) rows: TextRows,
    pub(in crate::ui) room: TextRoom<i32>,
}

impl PageScreen {
    pub(crate) fn new(keys: Keyboard) -> PageScreen {
        PageScreen {
            voice: Voice::new(),
            is_typing: false,
            keys,
            band: Edge::still(0.0),
            bids: Vec::new(),
            page: None,
            rows: TextRows::default(),
            room: TextRoom::default(),
        }
    }

    // The picture that stands over the page's rows.
    pub(in crate::ui) fn picture(&self) -> Option<&Picture> {
        self.page.as_ref().and_then(|page| page.picture.as_ref()).filter(|picture| picture.width > 0)
    }
}

pub(crate) struct Voice {
    pub(super) phase: Phase,
    pub(crate) stopwatch: Stopwatch,
    pub(crate) is_talking: bool,
}

impl Voice {
    pub(super) fn new() -> Voice {
        Voice { phase: Phase::Idle, stopwatch: Stopwatch::default(), is_talking: false }
    }

    pub(in crate::ui) fn is_listening(&self) -> bool {
        self.phase == Phase::Listening
    }

    pub(super) fn keeps_awake(&self) -> bool {
        self.is_talking || self.phase == Phase::Listening || self.phase.is_working()
    }

    pub(super) fn write_word(&self, now_ns: u64, text: &mut String) -> bool {
        let word = match self.phase {
            Phase::Idle => return false,
            Phase::Failed(cause) => cause_word(cause),
            Phase::Listening => {
                text.push_str(LISTENING);
                text.push(' ');
                write_duration(text, self.stopwatch.start_ns.map_or(0, |start_ns| seconds_since(start_ns, now_ns)));
                return true;
            }
            Phase::Writing => TRANSCRIBING,
            Phase::Thinking => THINKING,
            Phase::Searching => SEARCHING,
            Phase::Speaking => READING,
            Phase::Waking => WAKING,
            Phase::Connecting => CONNECTING,
        };
        text.push_str(word);
        true
    }
}

// The voice of a list whose first line takes a request said, and the bids the list made: `C` is what a request is read against, as the list stands when it begins.
pub(super) struct RequestLine<C> {
    pub(super) voice: Voice,
    bids: Vec<Bid<C>>,
}

impl<C> RequestLine<C> {
    pub(super) fn new() -> RequestLine<C> {
        RequestLine { voice: Voice::new(), bids: Vec::new() }
    }

    pub(super) fn take_bids(&mut self) -> Vec<Bid<C>> {
        core::mem::take(&mut self.bids)
    }

    // The press that may become a hold records from its beginning, unless a click on the request's line records already.
    fn overhear(&mut self, standing: C) {
        if !self.voice.is_talking {
            self.bids.extend([Bid::Job(standing), Bid::Overhear]);
        }
    }

    // The request's line takes the recording a hold or a click on it begins.
    fn press(&mut self, standing: C) {
        self.voice.is_talking = true;
        self.bids.extend([Bid::Job(standing), Bid::Listen]);
    }

    // Sends what was said as the button or the request's line is let go; true where something was said.
    fn release(&mut self) -> bool {
        let was_talking = core::mem::take(&mut self.voice.is_talking);
        if was_talking {
            self.bids.push(Bid::Send);
        }
        was_talking
    }

    // A press that was no hold recorded for nothing, unless a click on the request's line records.
    fn drop_press(&mut self) {
        if !self.voice.is_talking {
            self.bids.push(Bid::Drop);
        }
    }

    // Lets go of a recording that runs, as the list is left.
    fn let_go(&mut self) {
        if core::mem::take(&mut self.voice.is_talking) {
            self.bids.push(Bid::Drop);
        }
    }

    // Takes a request in words.
    pub(super) fn ask(&mut self, standing: C, request: String) {
        self.bids.extend([Bid::Job(standing), Bid::Text(request)]);
    }
}

// A list whose first line takes a request said: the bell's or the rundowns'.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) enum RequestList {
    Bell,
    Rundowns,
}

impl RequestList {
    pub(in crate::ui) const ALL: [RequestList; 2] = [RequestList::Bell, RequestList::Rundowns];

    pub(super) const fn of(screen: Screen) -> Option<RequestList> {
        match screen {
            Screen::Bell => Some(RequestList::Bell),
            Screen::Rundowns => Some(RequestList::Rundowns),
            _ => None,
        }
    }

    pub(in crate::ui) const fn list(self) -> List {
        match self {
            RequestList::Bell => List::Bell,
            RequestList::Rundowns => List::Rundowns,
        }
    }

    const fn voice(self) -> VoiceId {
        match self {
            RequestList::Bell => VoiceId::Bell,
            RequestList::Rundowns => VoiceId::Rundowns,
        }
    }
}

// A voice the interface keeps, by the job said that it serves.
#[derive(Clone, Copy)]
pub(super) enum VoiceId {
    Chat,
    Pokedex,
    Signal,
    Web,
    Bell,
    Rundowns,
}

impl VoiceId {
    const ALL: [VoiceId; 6] =
        [VoiceId::Chat, VoiceId::Pokedex, VoiceId::Signal, VoiceId::Web, VoiceId::Bell, VoiceId::Rundowns];
}

impl Ui {
    const fn voice(&self, id: VoiceId) -> &Voice {
        match id {
            VoiceId::Chat => &self.chat.voice,
            VoiceId::Pokedex => &self.pokedex.voice,
            VoiceId::Signal => &self.signal.voice,
            VoiceId::Web => &self.web.voice,
            VoiceId::Bell => &self.bells.request.voice,
            VoiceId::Rundowns => &self.rundowns.request.voice,
        }
    }

    const fn voice_mut(&mut self, id: VoiceId) -> &mut Voice {
        match id {
            VoiceId::Chat => &mut self.chat.voice,
            VoiceId::Pokedex => &mut self.pokedex.voice,
            VoiceId::Signal => &mut self.signal.voice,
            VoiceId::Web => &mut self.web.voice,
            VoiceId::Bell => &mut self.bells.request.voice,
            VoiceId::Rundowns => &mut self.rundowns.request.voice,
        }
    }

    // True while the screen a voice serves shows.
    fn does_voice_show(&self, id: VoiceId) -> bool {
        match id {
            VoiceId::Chat => self.does_chat_show(),
            VoiceId::Pokedex => self.does_pokedex_show(),
            VoiceId::Signal => self.does_conversation_show(),
            VoiceId::Web => self.does_web_show(),
            VoiceId::Bell => self.does_bell_list_show(),
            VoiceId::Rundowns => self.does_rundown_list_show(),
        }
    }

    pub(crate) fn voices(&self) -> [(&Voice, bool); 6] {
        VoiceId::ALL.map(|id| (self.voice(id), self.does_voice_show(id)))
    }

    // Counts the seconds of each voice that listens on the screen that shows; true where one of them has a new second.
    pub(in crate::ui) fn count_voice_seconds(&mut self, now_ns: u64) -> bool {
        VoiceId::ALL.into_iter().fold(false, |has_new, id| {
            let does_show = self.does_voice_show(id);
            self.voice_mut(id).stopwatch.update(does_show, now_ns) | has_new
        })
    }

    // True while a list that takes a request shows and its first line hears what is said.
    pub(in crate::ui) fn is_hearing_request(&self, list: RequestList) -> bool {
        self.does_voice_show(list.voice()) && self.voice(list.voice()).is_listening()
    }

    pub(super) fn overhear_request(&mut self, list: RequestList) {
        match list {
            RequestList::Bell => {
                let standing = self.bell_standing();
                self.bells.request.overhear(standing);
            }
            RequestList::Rundowns => {
                let standing = self.rundowns_standing();
                self.rundowns.request.overhear(standing);
            }
        }
    }

    // The list stands on its first line, which takes what is said from now on.
    pub(super) fn press_request(&mut self, list: RequestList) {
        self.grid_of_mut(list.list()).choose(0);
        match list {
            RequestList::Bell => {
                let standing = self.bell_standing();
                self.bells.request.press(standing);
            }
            RequestList::Rundowns => {
                let standing = self.rundowns_standing();
                self.rundowns.request.press(standing);
            }
        }
        self.deal_lines(list.list(), |_| true);
    }

    // Sends what was said to a list's first line; true where something was said.
    pub(super) fn release_request(&mut self, list: RequestList) -> bool {
        match list {
            RequestList::Bell => self.bells.request.release(),
            RequestList::Rundowns => self.rundowns.request.release(),
        }
    }

    pub(super) fn drop_request_press(&mut self, list: RequestList) {
        match list {
            RequestList::Bell => self.bells.request.drop_press(),
            RequestList::Rundowns => self.rundowns.request.drop_press(),
        }
    }

    pub(in crate::ui) fn leave_request_list(&mut self, list: RequestList) {
        if self.does_voice_show(list.voice()) {
            self.let_go_request(list);
        }
    }

    pub(in crate::ui) fn let_go_request(&mut self, list: RequestList) {
        match list {
            RequestList::Bell => self.bells.request.let_go(),
            RequestList::Rundowns => self.rundowns.request.let_go(),
        }
    }

    pub(in crate::ui) const fn paging(&self, paged: PageId) -> &PageScreen {
        match paged {
            PageId::Pokedex => &self.pokedex,
            PageId::Web => &self.web,
        }
    }

    pub(in crate::ui) const fn paging_mut(&mut self, paged: PageId) -> &mut PageScreen {
        match paged {
            PageId::Pokedex => &mut self.pokedex,
            PageId::Web => &mut self.web,
        }
    }

    // Stands a page's screen as it opens: where it was scrolled to, its keys up where it types.
    pub(super) fn open_paged(&mut self, paged: PageId) {
        let (max_scroll, band_aim) = (self.paged_max_scroll(paged), self.paged_band_aim(paged));
        let size = self.ruler.room_size();
        let paging = self.paging_mut(paged);
        paging.room.at = paging.room.at.min(max_scroll);
        (paging.room.scroll, paging.band) = (Edge::still(paging.room.at as f32), Edge::still(band_aim));
        paging.room.plate.make_room(size);
        if paging.is_typing {
            self.raise_paged_keys(paged, true);
        }
    }

    fn raise_paged_keys(&mut self, paged: PageId, is_up: bool) {
        let line = self.ruler.text_cells().0.0;
        raise(&mut self.paging_mut(paged).keys, is_up, line);
    }

    pub(in crate::ui) fn toggle_paged_keys(&mut self, paged: PageId, now_ns: u64) {
        let is_typing = !self.paging(paged).is_typing;
        self.paging_mut(paged).is_typing = is_typing;
        self.raise_paged_keys(paged, is_typing);
        let max_scroll = self.paged_max_scroll(paged);
        let room = self.paged_room_mut(paged);
        room.at = room.at.min(max_scroll);
        self.hush_robot(is_typing, now_ns);
    }

    pub(in crate::ui) fn land_paged(&mut self, paged: PageId) {
        self.paging_mut(paged).keys.land();
        let band_aim = self.paged_band_aim(paged);
        let paging = self.paging_mut(paged);
        (paging.room.scroll, paging.band) = (Edge::still(paging.room.at as f32), Edge::still(band_aim));
        paging.room.plate.land();
    }

    // Lays a page's rows anew: its page's words, or what the browser's room tells while no page shows.
    pub(in crate::ui) fn lay_paged(&mut self, paged: PageId) {
        let (cells, heights) = self.ruler.text_cells();
        let start = match paged {
            PageId::Pokedex => None,
            PageId::Web => Some(web::START),
        };
        let paging = self.paging_mut(paged);
        paging.rows.clear();
        if let Some(text) = paging.page.as_ref().map(|page| page.text.as_str()).or(start) {
            paging.rows.add_more(text, cells, heights, i32::MAX);
        }
        paging.room.plate.key = None;
        let max_scroll = self.paged_max_scroll(paged);
        let room = self.paged_room_mut(paged);
        room.at = room.at.min(max_scroll);
    }

    // Shows a page a job brought, from its top, its room's picture unfolding over the one before.
    pub(super) fn show_page(&mut self, paged: PageId, page: ScreenPage, now_ns: u64) {
        self.paging_mut(paged).page = Some(page);
        self.lay_paged(paged);
        let (size, does_show) = (self.ruler.room_size(), self.screen == Some(paged.screen()));
        let top = -self.read_step() as f32;
        let room = self.paged_room_mut(paged);
        room.plate.renew(size, does_show);
        (room.at, room.scroll) = (0, Edge::still(top));
        self.react(Reaction::Nod, now_ns);
    }

    // Takes the phase of a page's voice: the page stays where it showed as the microphone's row comes or goes over it, and the scroll then brings the row in from the top or takes it out. True if the phase is new.
    pub(super) fn take_paged_phase(&mut self, paged: PageId, phase: Phase, now_ns: u64) -> bool {
        let (meter_height, page_top) = (self.paged_meter_height(paged), self.paged_top(paged));
        let id = match paged {
            PageId::Pokedex => VoiceId::Pokedex,
            PageId::Web => VoiceId::Web,
        };
        let has_new_phase = self.take_phase(id, phase, now_ns);
        if self.paged_meter_height(paged) != meter_height {
            let shift = (self.paged_top(paged) - page_top) as f32;
            let room = self.paged_room_mut(paged);
            if meter_height == 0 {
                room.at = 0;
            }
            room.scroll.at += shift;
            room.plate.key = None;
        }
        has_new_phase
    }

    /// Takes the largest sample of the block the microphone recorded last; true if the screen changed.
    #[must_use]
    pub fn set_microphone(&mut self, peak: u32) -> bool {
        let has_new_level = loudness(core::mem::replace(&mut self.microphone.peak, peak)) != loudness(peak);
        has_new_level && self.is_microphone_heard()
    }

    // True while the screen that shows records what the microphone hears: a note, a job said, the translator's session as it listens, or a call.
    fn is_microphone_heard(&self) -> bool {
        self.is_hearing_note()
            || self.voices().iter().any(|(voice, does_show)| *does_show && voice.is_listening())
            || self.screen == Some(Screen::Talk) && self.translator.session == Phase::Listening
            || self.is_hearing_call()
    }

    pub(in crate::ui) fn microphone_aims(&self) -> [f32; SIGNAL_BLOCKS] {
        lit_blocks(if self.is_microphone_heard() { loudness(self.microphone.peak) } else { 0 })
    }

    pub(super) fn take_phase(&mut self, id: VoiceId, phase: Phase, now_ns: u64) -> bool {
        let voice = self.voice_mut(id);
        if voice.phase == phase {
            return false;
        }
        (voice.phase, voice.stopwatch) =
            (phase, Stopwatch { start_ns: (phase == Phase::Listening).then_some(now_ns), seconds: None });
        if matches!(phase, Phase::Failed(_)) {
            self.react(Reaction::Slump, now_ns);
        }
        self.used_at = Some(now_ns);
        true
    }
}

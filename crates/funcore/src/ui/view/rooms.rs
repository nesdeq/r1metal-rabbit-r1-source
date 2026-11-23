use crate::base::math::round;
use crate::domain::paper::rows::Kind;
use crate::gfx::motion::Edge;
use crate::gfx::pen::Pen;
use crate::gfx::{Picture, Rect};
use crate::ui::Ui;
use crate::ui::edit::{band_aim, band_foot, band_height, band_top};
use crate::ui::field::draw_field;
use crate::ui::keys::Keyboard;
use crate::ui::robot::Seat;
use crate::ui::screen::Screen;
use crate::ui::screen::bar::BarWords;
use crate::ui::screen::typing::KeyboardId;
use crate::ui::screen::voice::{PageId, Voice};
use crate::ui::textroom::talk::{Line, LineKind, TalkForm, TalkStyle};
use crate::ui::textroom::{
    Plate, TextRoom, TextRows, draw_disc, draw_scroll_mark, max_scroll, paint_artwork_over, paint_rows,
};

// A room of text, by the screen it fills.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::ui) enum TextRoomId {
    Reader,
    Chat,
    Page(PageId),
    Conversation,
    Talk,
}

impl TextRoomId {
    const ALL: [TextRoomId; 6] = [
        TextRoomId::Reader,
        TextRoomId::Chat,
        TextRoomId::Page(PageId::Pokedex),
        TextRoomId::Page(PageId::Web),
        TextRoomId::Conversation,
        TextRoomId::Talk,
    ];

    pub(in crate::ui) const fn of(screen: Screen) -> Option<TextRoomId> {
        Some(match screen {
            Screen::Reader => TextRoomId::Reader,
            Screen::Chat => TextRoomId::Chat,
            Screen::Pokedex => TextRoomId::Page(PageId::Pokedex),
            Screen::Web => TextRoomId::Page(PageId::Web),
            Screen::Conversation => TextRoomId::Conversation,
            Screen::Talk => TextRoomId::Talk,
            _ => return None,
        })
    }
}

// A page as its room's layout reads it: its voice, its keys by their name, how far their band stands down, its rows, its scroll, and the picture over its rows.
struct Page<'a> {
    voice: &'a Voice,
    keys: (KeyboardId, &'a Keyboard),
    band: f32,
    rows: &'a TextRows,
    scroll: f32,
    picture: Option<&'a Picture>,
}

impl Ui {
    pub(in crate::ui) fn talk_form(&self) -> TalkForm<'_> {
        TalkForm { kit: self.kit(), style: self.look().room.talk }
    }

    fn does_name_speakers(&self) -> bool {
        self.look().room.talk == TalkStyle::Named
    }

    fn has_robot_head(&self) -> bool {
        self.look().room.has_head
    }

    pub(in crate::ui) fn reflow_speakers(&mut self) {
        if self.does_name_speakers() {
            self.lay_chat();
        }
    }

    pub(in crate::ui) fn does_chat_prompt(&self, is_muted: bool) -> bool {
        self.does_name_speakers() && self.may_chat_prompt(is_muted)
    }

    fn chat_height_as(&self, is_muted: bool) -> i32 {
        let (body, heading) = (self.ruler.small(), self.ruler.large());
        let prompt = |has_prompt: bool| {
            if has_prompt { Kind::Gap.height(body, heading) + Kind::Text.height(body, heading) } else { 0 }
        };
        self.chat.height - prompt(self.does_chat_prompt(self.chat.is_muted)) + prompt(self.does_chat_prompt(is_muted))
    }

    pub(in crate::ui) fn chat_rest_room(&self) -> Rect {
        self.chat_room_as(self.keys_up() == Some(KeyboardId::Chat))
    }

    fn chat_room_as(&self, is_muted: bool) -> Rect {
        let room = self.ruler.under_bar();
        let top = if is_muted { room.y } else { self.chat_top() };
        let foot = match (is_muted, self.talk_disc()) {
            (true, _) => self.chat_band_top(),
            (false, Some(disc)) => disc.y - self.ruler.gap,
            (false, None) if self.is_chat_robot_large() => self.chat_foot_top() - self.ruler.gap,
            (false, None) => room.bottom(),
        };
        self.room_down_to(top, foot)
    }

    pub(in crate::ui) fn is_chat_robot_large(&self) -> bool {
        self.has_robot_head() && !self.chat.is_muted
    }

    pub(in crate::ui) fn chat_seat(&self) -> Seat {
        let seat = self.seat_in_square((self.ruler.width - self.ruler.home_side()) / 2, 0);
        Seat { x: (self.ruler.width as f32 - seat.width) / 2.0, y: 2.0 * self.ruler.gap as f32, ..seat }
    }

    fn chat_top(&self) -> i32 {
        if !self.has_robot_head() {
            return self.ruler.bar();
        }
        let seat = self.chat_seat();
        round(seat.y + seat.height) + self.ruler.small() + 2 * self.ruler.gap
    }

    pub(in crate::ui) fn chat_foot_top(&self) -> i32 {
        self.ruler.height - self.ruler.gap - self.ruler.small()
    }

    fn chat_band_top(&self) -> i32 {
        band_top(&self.ruler, &self.chat.keys)
    }

    pub(in crate::ui) fn chat_band_height(&self) -> i32 {
        band_height(&self.ruler, &self.chat.keys)
    }

    pub(in crate::ui) fn chat_band_aim(&self) -> f32 {
        band_aim(&self.ruler, &self.chat.keys, self.keys_up() == Some(KeyboardId::Chat))
    }

    fn chat_aim_as(&self, is_muted: bool) -> f32 {
        self.room_aim(self.chat_height_as(is_muted), self.chat_room_as(is_muted), self.chat.room.at)
    }

    pub(in crate::ui) fn chat_offset(&self) -> i32 {
        let height = self.chat_band_height() as f32;
        let shift = self.chat_aim_as(true) - self.chat_aim_as(false);
        let lag = if height > 0.0 { shift * (self.chat.band.at - self.chat_band_aim()) / height } else { 0.0 };
        (self.chat.room.scroll.at - lag) as i32 - self.chat_drop()
    }

    fn chat_drop(&self) -> i32 {
        let height = self.chat_band_height() as f32;
        let share = if height > 0.0 { (self.chat.band.at / height).clamp(0.0, 1.0) } else { 0.0 };
        round((self.chat_room_as(false).y - self.ruler.bar()) as f32 * share)
    }

    pub(in crate::ui) fn chat_visible_room(&self) -> Rect {
        let top = self.ruler.under_bar().y + self.chat_drop();
        let foot = band_foot(&self.ruler, &self.chat.keys, self.chat.band.at);
        self.room_down_to(top, foot.min(self.chat_room_as(false).bottom()))
    }

    fn chat_room(&self) -> (Rect, i32, i32) {
        (self.chat_visible_room(), self.chat.height, self.chat_offset())
    }

    pub(in crate::ui) fn chat_max_scroll(&self) -> i32 {
        max_scroll(self.chat.height, self.chat_rest_room())
    }

    pub(in crate::ui) fn chat_aim(&self) -> f32 {
        self.chat_aim_as(self.keys_up() == Some(KeyboardId::Chat))
    }

    pub(in crate::ui) fn conversation_band_aim(&self) -> f32 {
        band_aim(&self.ruler, &self.signal.keys, self.keys_up() == Some(KeyboardId::Signal))
    }

    pub(in crate::ui) fn conversation_rest_room(&self) -> Rect {
        let room = self.ruler.under_bar();
        let is_up = self.keys_up() == Some(KeyboardId::Signal);
        let foot = if is_up { band_top(&self.ruler, &self.signal.keys) } else { self.voice_foot() };
        self.room_down_to(room.y, foot.min(room.bottom()))
    }

    fn voice_foot(&self) -> i32 {
        self.talk_disc().map_or(self.ruler.under_bar().bottom(), |disc| disc.y - self.ruler.gap)
    }

    pub(in crate::ui) fn conversation_visible_room(&self) -> Rect {
        let foot = band_foot(&self.ruler, &self.signal.keys, self.signal.band.at);
        self.room_down_to(self.ruler.under_bar().y, foot.min(self.voice_foot()))
    }

    fn conversation_room(&self) -> (Rect, i32, i32) {
        (self.conversation_visible_room(), self.signal.height, round(self.signal.room.scroll.at))
    }

    pub(in crate::ui) fn conversation_max_scroll(&self) -> i32 {
        max_scroll(self.signal.height, self.conversation_rest_room())
    }

    pub(in crate::ui) fn conversation_aim(&self) -> f32 {
        self.room_aim(self.signal.height, self.conversation_rest_room(), self.signal.room.at)
    }

    fn page(&self, paged: PageId) -> Page<'_> {
        let paging = self.paging(paged);
        let id = match paged {
            PageId::Pokedex => KeyboardId::Pokedex,
            PageId::Web => KeyboardId::Web,
        };
        Page {
            voice: &paging.voice,
            keys: (id, &paging.keys),
            band: paging.band.at,
            rows: &paging.rows,
            scroll: paging.room.scroll.at,
            picture: paging.picture(),
        }
    }

    pub(in crate::ui) const fn paged_room_mut(&mut self, paged: PageId) -> &mut TextRoom<i32> {
        &mut self.paging_mut(paged).room
    }

    fn picture_height(&self, page: &Page<'_>) -> i32 {
        page.picture.map_or(0, |picture| picture.height() as i32 + self.ruler.gap)
    }

    // The room the microphone's level takes over a page while what is said is heard: its row and a gap under it.
    pub(in crate::ui) fn paged_meter_height(&self, paged: PageId) -> i32 {
        let (body, heading) = (self.ruler.small(), self.ruler.large());
        if self.page(paged).voice.is_listening() {
            Kind::Text.height(body, heading) + Kind::Gap.height(body, heading)
        } else {
            0
        }
    }

    // How far under the top of its room a page begins.
    pub(in crate::ui) fn paged_top(&self, paged: PageId) -> i32 {
        self.paged_meter_height(paged) - self.paged_offset(paged)
    }

    fn paged_height(&self, paged: PageId) -> i32 {
        let page = self.page(paged);
        self.paged_meter_height(paged) + self.picture_height(&page) + page.rows.height
    }

    pub(in crate::ui) fn paged_band_aim(&self, paged: PageId) -> f32 {
        let (id, keys) = self.page(paged).keys;
        band_aim(&self.ruler, keys, self.keys_up() == Some(id))
    }

    fn paged_rest_room(&self, paged: PageId) -> Rect {
        let (room, (id, keys)) = (self.ruler.under_bar(), self.page(paged).keys);
        let foot = if self.keys_up() == Some(id) { band_top(&self.ruler, keys) } else { room.bottom() };
        self.room_down_to(room.y, foot.min(room.bottom()))
    }

    pub(super) fn paged_visible_room(&self, paged: PageId) -> Rect {
        let (room, page) = (self.ruler.under_bar(), self.page(paged));
        let foot = band_foot(&self.ruler, page.keys.1, page.band);
        self.room_down_to(room.y, foot.min(room.bottom()))
    }

    pub(in crate::ui) fn paged_max_scroll(&self, paged: PageId) -> i32 {
        max_scroll(self.paged_height(paged), self.paged_rest_room(paged))
    }

    fn paged_offset(&self, paged: PageId) -> i32 {
        round(self.page(paged).scroll) - self.room_lift(self.paged_rest_room(paged), self.paged_height(paged))
    }

    fn paged_room(&self, paged: PageId) -> (Rect, i32, i32) {
        (self.paged_visible_room(paged), self.paged_height(paged), self.paged_offset(paged))
    }

    pub(in crate::ui) fn read_max_scroll(&self) -> i32 {
        max_scroll(self.reader.rows.height, self.ruler.under_bar())
    }

    fn reader_offset(&self) -> i32 {
        let lift =
            if self.is_script_done() { self.room_lift(self.ruler.under_bar(), self.reader.rows.height) } else { 0 };
        round(self.reader.room.scroll.at) - lift
    }

    fn read_room(&self) -> (Rect, i32, i32) {
        (self.ruler.under_bar(), self.reader.rows.height, self.reader_offset())
    }

    fn talk_rows(&self) -> i32 {
        self.ruler.under_bar().h / self.ruler.small().max(1)
    }

    fn talk_row_count(&self) -> i32 {
        (self.translator.remarks.len() + self.translator.meter_rows()) as i32
    }

    pub(in crate::ui) fn talk_excess(&self) -> i32 {
        (self.talk_row_count() - self.talk_rows()).max(0)
    }

    pub(in crate::ui) fn push_aim(&self) -> f32 {
        ((self.talk_excess() - self.translator.room.at).max(0) * self.ruler.small()) as f32
    }

    pub(super) fn talk_lift(&self) -> i32 {
        let free_rows = (self.talk_rows() - self.talk_row_count()).max(0);
        self.foot_lift(free_rows * self.ruler.small())
    }

    fn talk_offset(&self) -> i32 {
        round(self.translator.room.scroll.at) - self.talk_lift()
    }

    fn talk_room(&self) -> (Rect, i32, i32) {
        (self.ruler.under_bar(), self.talk_row_count() * self.ruler.small(), self.talk_offset())
    }

    pub(in crate::ui) fn foot_lift(&self, free: i32) -> i32 {
        if self.look().room.is_at_foot { free } else { 0 }
    }

    pub(in crate::ui) fn room_down_to(&self, top: i32, foot: i32) -> Rect {
        let room = self.ruler.under_bar();
        Rect::new(room.x, top, room.w, (foot - top).max(0))
    }

    pub(in crate::ui) fn room_aim(&self, height: i32, room: Rect, at: i32) -> f32 {
        let excess = height - room.h;
        ((excess.max(0) - at).max(0) + self.foot_lift(excess.min(0))) as f32
    }

    pub(in crate::ui) fn room_lift(&self, room: Rect, height: i32) -> i32 {
        self.foot_lift((room.h - height).max(0))
    }

    // The room of text the screen that shows fills.
    pub(in crate::ui) fn shown_room(&self) -> Option<TextRoomId> {
        self.screen.and_then(TextRoomId::of)
    }

    fn plate(&self, id: TextRoomId) -> &dyn Plate {
        match id {
            TextRoomId::Reader => &self.reader.room.plate,
            TextRoomId::Chat => &self.chat.room.plate,
            TextRoomId::Page(paged) => &self.paging(paged).room.plate,
            TextRoomId::Conversation => &self.signal.room.plate,
            TextRoomId::Talk => &self.translator.room.plate,
        }
    }

    fn plate_mut(&mut self, id: TextRoomId) -> &mut dyn Plate {
        match id {
            TextRoomId::Reader => &mut self.reader.room.plate,
            TextRoomId::Chat => &mut self.chat.room.plate,
            TextRoomId::Page(paged) => &mut self.paged_room_mut(paged).plate,
            TextRoomId::Conversation => &mut self.signal.room.plate,
            TextRoomId::Talk => &mut self.translator.room.plate,
        }
    }

    pub(in crate::ui) fn scroll_parts(&mut self, id: TextRoomId) -> (&mut i32, &mut Edge) {
        match id {
            TextRoomId::Reader => self.reader.room.scroll_parts(),
            TextRoomId::Chat => self.chat.room.scroll_parts(),
            TextRoomId::Page(paged) => self.paged_room_mut(paged).scroll_parts(),
            TextRoomId::Conversation => self.signal.room.scroll_parts(),
            TextRoomId::Talk => self.translator.room.scroll_parts(),
        }
    }

    pub(in crate::ui) fn ready_room(&mut self) {
        match self.shown_room() {
            Some(TextRoomId::Reader) => self.ready_reader(),
            Some(TextRoomId::Chat) => self.ready_chat(),
            Some(TextRoomId::Page(paged)) => self.ready_paged(paged),
            Some(TextRoomId::Conversation) => self.ready_conversation(),
            Some(TextRoomId::Talk) => self.ready_talk(),
            None => {}
        }
    }

    fn ready_reader(&mut self) {
        let (room, offset) = (self.ruler.room(), self.reader_offset());
        let row_count = self.reader.rows.tops.partition_point(|top| *top < offset + room.h);
        let is_full = self.is_script_done() || self.reader.rows.height >= offset + room.h;
        let mut plate = core::mem::take(&mut self.reader.room.plate);
        let distance = plate.key.filter(|key| key.2 && key.0 != offset).map(|key| offset - key.0);
        let (kit, rows) = (self.kit(), &self.reader.rows);
        plate.ready(&kit, ((offset, row_count, is_full), distance, None), |surface, shift| {
            paint_rows(surface, &kit, rows, kit.ruler.under_bar().y, (shift, offset));
        });
        self.reader.room.plate = plate;
    }

    fn chat_change(&self, offset: i32) -> (Option<i32>, Option<usize>) {
        let (room, size) = (self.ruler.room(), self.ruler.room_size());
        let height = size.1;
        let plate = &self.chat.room.plate;
        let distance = plate.key.filter(|_| plate.is_sized(size)).map(|key| offset - key);
        let bar = self.ruler.bar();
        let change_top = self.chat.stale_row.map(|row| (bar + row - offset - room.y).clamp(0, height as i32) as usize);
        (distance.filter(|distance| (distance.unsigned_abs() as usize) < height), change_top)
    }

    pub(in crate::ui) fn does_chat_repaint(&self) -> bool {
        if !self.does_chat_show() {
            return false;
        }
        let (distance, change_top) = self.chat_change(self.chat_offset());
        distance.is_none() || change_top == Some(0)
    }

    fn ready_chat(&mut self) {
        let offset = self.chat_offset();
        let (distance, change_top) = self.chat_change(offset);
        let mut plate = core::mem::take(&mut self.chat.room.plate);
        let (form, rows) = (self.talk_form(), (&self.chat.lines[..], self.chat.speaker.as_str(), &[][..]));
        let (room, origin) = (self.chat_rest_room(), self.ruler.bar());
        plate.ready(&form.kit, (offset, distance, change_top), |surface, shift| {
            form.paint(surface, rows, room, origin - shift, offset);
        });
        (self.chat.room.plate, self.chat.stale_row) = (plate, None);
    }

    fn ready_paged(&mut self, paged: PageId) {
        let (offset, page_top) = (self.paged_offset(paged), self.paged_top(paged));
        let mut plate = core::mem::take(&mut self.paged_room_mut(paged).plate);
        let distance = plate.key.map(|key| offset - key);
        let (kit, page) = (self.kit(), self.page(paged));
        let artwork = (page.picture, self.picture_height(&page));
        plate.ready(&kit, (offset, distance, None), |surface, shift| {
            paint_artwork_over(surface, &kit, artwork, page.rows, (shift, -page_top));
        });
        self.paged_room_mut(paged).plate = plate;
    }

    fn ready_conversation(&mut self) {
        let offset = round(self.signal.room.scroll.at);
        let mut plate = core::mem::take(&mut self.signal.room.plate);
        let distance = plate.key.map(|key| offset - key);
        let (form, rows) = (self.talk_form(), self.conversation_rows());
        let (room, origin) = (self.conversation_rest_room(), self.ruler.bar());
        plate.ready(&form.kit, (offset, distance, None), |surface, shift| {
            form.paint(surface, rows, room, origin - shift, offset);
        });
        self.signal.room.plate = plate;
    }

    fn ready_talk(&mut self) {
        let offset = self.talk_offset();
        let mut plate = core::mem::take(&mut self.translator.room.plate);
        let distance = plate.key.map(|key| offset - key);
        let form = self.talk_form();
        plate.ready(&form.kit, (offset, distance, None), |surface, shift| {
            form.paint_remarks(surface, &self.translator.remarks, (shift, offset));
        });
        self.translator.room.plate = plate;
    }

    pub(in crate::ui) fn reflow_rooms(&mut self) {
        let does_cut = self.does_cut();
        for id in TextRoomId::ALL {
            self.plate_mut(id).set_cutting(does_cut);
        }
        self.reflow_chat();
        self.chat.room.plate.renew(self.ruler.room_size(), self.does_chat_show());
        self.lay_paged(PageId::Pokedex);
        self.lay_paged(PageId::Web);
    }

    pub(in crate::ui) fn renew_rooms(&mut self) {
        let (size, shown) = (self.ruler.room_size(), self.shown_room());
        for id in TextRoomId::ALL {
            self.plate_mut(id).renew(size, shown == Some(id));
        }
    }

    pub(in crate::ui) fn advance_rooms(&mut self, now_ns: u64) {
        for id in TextRoomId::ALL {
            self.plate_mut(id).advance(now_ns);
        }
    }

    pub(in crate::ui) fn does_room_unfold(&self) -> bool {
        self.shown_room().is_some_and(|id| !self.plate(id).is_resting())
    }

    pub(in crate::ui) fn text_room(&self) -> Option<(Rect, i32, i32)> {
        Some(match self.shown_room()? {
            TextRoomId::Reader => self.read_room(),
            TextRoomId::Chat => self.chat_room(),
            TextRoomId::Page(paged) => self.paged_room(paged),
            TextRoomId::Conversation => self.conversation_room(),
            TextRoomId::Talk => self.talk_room(),
        })
    }

    pub(in crate::ui) fn draw_scroll_mark(&self, pen: &mut Pen<'_, '_>) {
        if let Some(room) = self.text_room().filter(|_| self.look().list.has_arrow) {
            draw_scroll_mark(pen, &self.kit(), room);
        }
    }

    pub(in crate::ui) fn talk_disc(&self) -> Option<Rect> {
        self.look().room.has_disc.then(|| {
            let side = 2 * self.ruler.large();
            Rect::new((self.ruler.width - side) / 2, self.ruler.height - side - side / 2, side, side)
        })
    }

    pub(in crate::ui) fn draw_disc(&self, pen: &mut Pen<'_, '_>, is_lit: bool) {
        if let Some(disc) = self.talk_disc() {
            draw_disc(pen, &self.kit(), (&self.disc, disc), is_lit);
        }
    }

    // Where the microphone's level stands in the room that shows: that room at rest, and the top of the level's row.
    pub(super) fn meter_row(&self) -> Option<(Rect, i32)> {
        let line_top = |lines: &[Line]| {
            let line = lines.last().filter(|line| line.kind == LineKind::Meter)?;
            Some(self.ruler.bar() + line.top)
        };
        Some(match self.shown_room()? {
            TextRoomId::Chat => {
                let top = line_top(&self.chat.lines[self.chat.live_start..])? - self.chat_offset();
                (self.chat_rest_room(), top)
            }
            TextRoomId::Conversation => {
                let top = line_top(self.conversation_rows().0)? - round(self.signal.room.scroll.at);
                (self.conversation_rest_room(), top)
            }
            TextRoomId::Page(paged) => {
                let room = (self.paged_meter_height(paged) > 0).then(|| self.paged_rest_room(paged))?;
                (room, room.y - self.paged_offset(paged))
            }
            TextRoomId::Talk if self.translator.meter_rows() > 0 => {
                let room = self.ruler.under_bar();
                (room, room.y + self.translator.remarks.len() as i32 * self.ruler.small() - self.talk_offset())
            }
            TextRoomId::Talk | TextRoomId::Reader => return None,
        })
    }

    fn draw_chat_cursor(&self, pen: &mut Pen<'_, '_>) {
        let ruler = &self.ruler;
        let (room, offset, full_room) = (self.chat_rest_room(), self.chat_offset(), ruler.room());
        let (cell, height) = (ruler.small_cell(), ruler.small());
        let lines = self.chat.lines[self.chat.live_start..].iter();
        for line in lines.filter(|line| line.kind == LineKind::Cursor) {
            let y = ruler.bar() + line.top - offset;
            if y + height <= room.y || y >= full_room.bottom() {
                continue;
            }
            let left = room.x + self.talk_form().speaker_width(LineKind::Cursor, &self.chat.speaker);
            self.caret_mark().draw(pen, &self.kit(), Rect::new(left, y, cell, height));
        }
    }

    fn draw_chat_large(&self, pen: &mut Pen<'_, '_>, caption: &str, hint: &str) {
        let (ruler, roles, seat) = (&self.ruler, self.roles, self.chat_seat());
        let room = ruler.body.text_width(caption).min(ruler.width - 2 * ruler.gap);
        let at = ((ruler.width - room) / 2, round(seat.y + seat.height) + ruler.gap);
        draw_field(&self.kit(), pen, &ruler.body, at, caption, room, roles.text_dim);
        if round(self.chat.band.at) >= self.chat_band_height() {
            let width = ruler.body.text_width(hint);
            let at = ((ruler.width - width) / 2, self.chat_foot_top());
            pen.fill(Rect::new(0, self.chat_foot_top() - ruler.gap, ruler.width, ruler.height), roles.background);
            draw_field(&self.kit(), pen, &ruler.body, at, hint, width, roles.text_dim);
        }
    }

    pub(in crate::ui) fn draw_chat(
        &self,
        pen: &mut Pen<'_, '_>,
        (name, words): (&str, &BarWords<'_>),
        (caption, hint): (&str, &str),
    ) {
        let (ruler, roles) = (&self.ruler, self.roles);
        self.chat.room.plate.as_room().draw(pen, ruler.room());
        self.draw_meter(pen);
        self.draw_chat_cursor(pen);
        self.draw_scroll_mark(pen);
        self.draw_band(pen, KeyboardId::Chat, self.chat.band.at);
        let visible = self.chat_visible_room();
        pen.fill(Rect::new(0, 0, ruler.width, visible.y.max(ruler.bar())), roles.background);
        if !self.chat.is_muted {
            self.draw_disc(pen, self.chat.voice.is_talking);
        }
        if self.is_chat_robot_large() && visible.y > ruler.bar() {
            return self.draw_chat_large(pen, caption, hint);
        }
        self.draw_bar(pen, name, words);
    }
}

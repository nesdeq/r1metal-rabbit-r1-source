use crate::base::math::round;
use crate::gfx::motion::{Edge, GRAIN, REST_DISTANCE, SWING, Spring, spring};
use crate::ui::bar::{Typing, letter_count};
use crate::ui::gauge::lit_blocks;
use crate::ui::item::SIGNAL_BLOCKS;
use crate::ui::layout::Motion;
use crate::ui::robot::News;
use crate::ui::screen::Screen;
use crate::ui::screen::finder::ROW_BLOCKS;
use crate::ui::screen::levels::{BRIGHTNESS_BLOCKS, VOLUME_BLOCKS};
use crate::ui::screen::voice::PageId;
use crate::ui::{CARET_NS, Ui};

const SEATING: Spring = spring(26.0, 0.65);
const BLOCK: Spring = spring(30.0, 0.5);
const SLIDING: Spring = spring(30.0, 1.0);
const TURNING: Spring = SLIDING;
pub(in crate::ui) const SCROLLING: Spring = spring(12.0, 1.0);

struct Aims {
    seat: [f32; 4],
    does_robot_live: bool,
    does_turn: bool,
    upright: f32,
    upright_grain: f32,
    is_brightness: bool,
    has_note: bool,
    is_finder: bool,
    is_volume: bool,
    is_clock: bool,
    is_talk: bool,
    is_chat: bool,
    is_pokedex: bool,
    is_web: bool,
    is_signal: bool,
    is_reader: bool,
    has_dial: bool,
    is_key: bool,
    is_safe: bool,
    is_ring: bool,
    is_incoming: bool,
    has_row: bool,
    is_metered: bool,
    level: [f32; BRIGHTNESS_BLOCKS],
    volume: [f32; VOLUME_BLOCKS],
    sounds: [f32; VOLUME_BLOCKS],
    clock_mark: f32,
    row: f32,
    row_blocks: [f32; ROW_BLOCKS],
    microphone: [f32; SIGNAL_BLOCKS],
    talk_scroll: f32,
    reader_scroll: f32,
    chat_scroll: f32,
    chat_band: f32,
    pokedex_scroll: f32,
    pokedex_band: f32,
    web_scroll: f32,
    web_band: f32,
    signal_scroll: f32,
    signal_band: f32,
    dial: f32,
    key_rise: f32,
    safe_rise: f32,
    ring_rise: f32,
    incoming_rise: f32,
    shutter: f32,
    block_grain: f32,
    note_grain: f32,
    passage: Option<f32>,
}

impl Ui {
    fn run_motions(&mut self, aims: &Aims, is_step: bool) -> bool {
        let mut is_moving = false;
        let mut run = |edge: &mut Edge, aim: f32, spring: Spring, rest: f32, is_active: bool, is_stood: bool| {
            if is_stood && !is_active {
                *edge = Edge::still(aim);
            }
            is_moving |= is_active && if is_step { edge.settle(aim, spring, rest) } else { *edge != Edge::still(aim) };
        };
        for (edge, place) in self.robot_place.at.iter_mut().zip(aims.seat) {
            run(edge, place, SEATING, REST_DISTANCE, aims.does_robot_live, false);
        }
        run(&mut self.robot_place.upright, aims.upright, TURNING, aims.upright_grain, aims.does_turn, true);
        for (edge, share) in self.gauges.level.iter_mut().zip(aims.level) {
            run(edge, share, BLOCK, aims.block_grain, aims.is_brightness, false);
        }
        for (edge, share) in self.gauges.volume.iter_mut().zip(aims.volume) {
            run(edge, share, BLOCK, aims.block_grain, aims.is_volume, false);
        }
        for (edge, share) in self.gauges.sounds.iter_mut().zip(aims.sounds) {
            run(edge, share, BLOCK, aims.block_grain, aims.is_volume, false);
        }
        let is_swaying = aims.is_brightness || aims.is_volume;
        run(&mut self.gauges.sway, 0.0, SWING, REST_DISTANCE, is_swaying, true);
        run(&mut self.face.mark, aims.clock_mark, SEATING, REST_DISTANCE, aims.is_clock, false);
        run(&mut self.painting.row, aims.row, BLOCK, aims.block_grain, aims.has_row, true);
        for (edge, share) in self.painting.blocks.iter_mut().zip(aims.row_blocks) {
            run(edge, share, BLOCK, aims.block_grain, aims.has_row, true);
        }
        for (edge, share) in self.microphone.blocks.iter_mut().zip(aims.microphone) {
            run(edge, share, BLOCK, aims.block_grain, aims.is_metered, true);
        }
        run(&mut self.translator.room.scroll, aims.talk_scroll, SCROLLING, REST_DISTANCE, aims.is_talk, true);
        run(&mut self.reader.room.scroll, aims.reader_scroll, SCROLLING, REST_DISTANCE, aims.is_reader, true);
        run(&mut self.chat.room.scroll, aims.chat_scroll, SCROLLING, REST_DISTANCE, aims.is_chat, true);
        run(&mut self.chat.band, aims.chat_band, SLIDING, REST_DISTANCE, aims.is_chat, true);
        run(&mut self.pokedex.room.scroll, aims.pokedex_scroll, SCROLLING, REST_DISTANCE, aims.is_pokedex, true);
        run(&mut self.pokedex.band, aims.pokedex_band, SLIDING, REST_DISTANCE, aims.is_pokedex, true);
        run(&mut self.web.room.scroll, aims.web_scroll, SCROLLING, REST_DISTANCE, aims.is_web, true);
        run(&mut self.web.band, aims.web_band, SLIDING, REST_DISTANCE, aims.is_web, true);
        run(&mut self.signal.room.scroll, aims.signal_scroll, SCROLLING, REST_DISTANCE, aims.is_signal, true);
        run(&mut self.signal.band, aims.signal_band, SLIDING, REST_DISTANCE, aims.is_signal, true);
        run(&mut self.dial.roll, aims.dial, SLIDING, GRAIN, aims.has_dial, true);
        run(&mut self.finder.shutter, aims.shutter, SLIDING, aims.block_grain, aims.is_finder, false);
        run(&mut self.slide, 0.0, SLIDING, aims.note_grain, aims.has_note, false);
        run(&mut self.keying.window.rise, aims.key_rise, SLIDING, REST_DISTANCE, aims.is_key, false);
        run(&mut self.safe.window.rise, aims.safe_rise, SLIDING, REST_DISTANCE, aims.is_safe, false);
        run(&mut self.bells.window.rise, aims.ring_rise, SLIDING, REST_DISTANCE, aims.is_ring, false);
        run(&mut self.calls.window.rise, aims.incoming_rise, SLIDING, REST_DISTANCE, aims.is_incoming, false);
        if let (Some(passage), Some(aim)) = (&mut self.passage, aims.passage) {
            run(&mut passage.at, aim, SLIDING, REST_DISTANCE, true, false);
        }
        is_moving
    }

    pub(in crate::ui) fn settle(&mut self, now_ns: u64) {
        let (does_robot_live, is_home) = (self.does_robot_live(), self.is_at_home());
        let robot_seat = self.robot_seat();
        let seat = [robot_seat.x, robot_seat.y, robot_seat.width, robot_seat.height];
        self.robot_place.is_planted &= does_robot_live && is_home;
        if does_robot_live
            && (!self.robot_place.is_shown
                || self.robot_place.is_planted
                || robot_seat.quarters != self.robot_place.quarters)
        {
            (self.robot_place.at, self.robot_place.quarters) = (seat.map(Edge::still), robot_seat.quarters);
        }
        if !self.tilts() {
            (self.robot_place.aim, self.robot_place.is_following) = (round(self.robot_place.upright.at) as f32, false);
        }
        let turns = round(self.robot_place.aim) as f32;
        (self.robot_place.aim, self.robot_place.upright.at) =
            (self.robot_place.aim - turns, self.robot_place.upright.at - turns);
        let (upright, upright_grain) = (self.robot_place.aim, self.upright_grain());
        let does_turn = core::mem::replace(&mut self.robot_place.is_shown, does_robot_live) && does_robot_live;
        let is_finder = matches!(self.screen, Some(Screen::Finder(_)));
        let has_row = self.do_photos_show();
        let (is_full, lit_count) = self.note_painting(has_row, does_robot_live, now_ns);
        let is_painting = is_full || self.painting.drawing.is_some() || self.painting.development.is_some();
        let is_recording = self.is_hearing_note();
        let is_talk = self.screen == Some(Screen::Talk);
        let (is_chat, is_pokedex, is_signal, is_web) =
            (self.does_chat_show(), self.does_pokedex_show(), self.does_conversation_show(), self.does_web_show());
        let has_new_second = self.memos.recording.update(is_recording, now_ns);
        let has_new_second = has_new_second | self.translator.running.update(is_talk, now_ns);
        let has_new_second = has_new_second | self.count_voice_seconds(now_ns);
        let has_new_second = has_new_second | self.calls.talking.update(self.screen == Some(Screen::Call), now_ns);
        let is_reader = self.screen == Some(Screen::Reader);
        let has_new_rows =
            is_reader && !self.is_script_done() && self.lay_more(self.reader.rows.height + self.ruler.under_bar().h);
        let dial_aim = self.dial_aim();
        let passage = self
            .passage
            .filter(|passage| passage.motion != Motion::Print)
            .map(|passage| passage.way * self.ruler.height as f32);
        let aims = Aims {
            seat,
            does_robot_live,
            does_turn,
            upright,
            upright_grain,
            is_brightness: self.screen == Some(Screen::Brightness),
            has_note: is_finder && self.note.is_some(),
            is_finder,
            is_volume: self.screen == Some(Screen::Volume),
            is_clock: self.screen == Some(Screen::Clock),
            is_talk,
            is_chat,
            is_pokedex,
            is_web,
            is_signal,
            is_reader,
            has_dial: dial_aim.is_some(),
            is_key: self.screen == Some(Screen::Key),
            is_safe: self.screen == Some(Screen::Safe),
            is_ring: self.screen == Some(Screen::Ring),
            is_incoming: self.screen == Some(Screen::Incoming),
            has_row,
            is_metered: self.meter_place().is_some(),
            level: self.block_aims(),
            volume: self.step_aims(),
            sounds: self.sound_aims(),
            clock_mark: self.mark_aim(),
            row: if is_painting { 1.0 } else { 0.0 },
            row_blocks: lit_blocks(lit_count),
            microphone: self.microphone_aims(),
            talk_scroll: self.push_aim(),
            reader_scroll: self.reader.room.at as f32,
            chat_scroll: self.chat_aim(),
            chat_band: self.chat_band_aim(),
            pokedex_scroll: self.pokedex.room.at as f32,
            pokedex_band: self.paged_band_aim(PageId::Pokedex),
            web_scroll: self.web.room.at as f32,
            web_band: self.paged_band_aim(PageId::Web),
            signal_scroll: self.conversation_aim(),
            signal_band: self.conversation_band_aim(),
            dial: dial_aim.unwrap_or(self.dial.roll.at),
            key_rise: self.keying.window.aim(self.ruler.height),
            safe_rise: self.safe.window.aim(self.ruler.height),
            ring_rise: self.bells.window.aim(self.ruler.height),
            incoming_rise: self.calls.window.aim(self.ruler.height),
            shutter: if self.hold_start.is_some() { 1.0 } else { 0.0 },
            block_grain: REST_DISTANCE / self.ruler.rim().max(1.0),
            note_grain: REST_DISTANCE / self.ruler.note_drop().max(1.0),
            passage,
        };
        let is_at_rest = !self.run_motions(&aims, false);
        let due = self.pace.take_ticks(now_ns, self.is_calm);
        self.is_calm &= is_at_rest;
        for _ in 0..due {
            if self.is_calm {
                break;
            }
            self.is_calm = !self.run_motions(&aims, true);
        }
        self.end_window();
        self.end_passage(passage, now_ns);
        self.type_on(now_ns);
        let has_blinked = self.blink(now_ns);
        self.is_calm &= !has_new_second && !has_new_rows && self.typing == Typing::Done && !has_blinked;
        let is_seated = self.robot_place.at.iter().zip(seat).all(|(edge, place)| *edge == Edge::still(place));
        self.robot_place.is_planted = does_robot_live && is_home && is_seated;
    }

    fn note_painting(&mut self, has_row: bool, does_robot_live: bool, now_ns: u64) -> (bool, usize) {
        let painting = &mut self.painting;
        painting.full_until = painting.full_until.filter(|until| now_ns < *until);
        let (is_full, drawing) = (painting.full_until.is_some(), painting.drawing);
        let lit_count = if is_full { ROW_BLOCKS } else { painting.lit_blocks(now_ns) };
        if has_row && does_robot_live && !is_full && drawing.is_some() && lit_count > painting.stroke_count {
            self.robot.news = Some(News::Stroke);
        }
        let painting = &mut self.painting;
        painting.stroke_count = if is_full || drawing.is_some() { lit_count } else { 0 };
        painting.row_due =
            if is_full { painting.full_until } else { drawing.and_then(|drawing| drawing.next_block_ns(now_ns)) };
        (is_full, lit_count)
    }

    fn end_passage(&mut self, aim: Option<f32>, now_ns: u64) {
        if let (Some(passage), Some(aim)) = (self.passage, aim)
            && passage.at == Edge::still(aim)
        {
            self.passage = None;
        }
        let printing = self.passage.filter(|passage| passage.motion == Motion::Print);
        if let Some(start_ns) = printing.map(|passage| passage.start_ns.unwrap_or(now_ns)) {
            if self.printed_rows(start_ns, now_ns) >= self.ruler.height {
                self.passage = None;
            } else if let Some(passage) = &mut self.passage {
                passage.start_ns = Some(start_ns);
            }
        }
    }

    fn type_on(&mut self, now_ns: u64) {
        self.typing = match self.typing {
            Typing::Due => Typing::Start(now_ns),
            Typing::Start(start_ns) if letter_count(start_ns, now_ns) >= self.bar_text.borrow().chars().count() => {
                Typing::Done
            }
            typing => typing,
        };
    }

    fn blink(&mut self, now_ns: u64) -> bool {
        let phases = now_ns.saturating_sub(self.caret.start_ns) / CARET_NS;
        let does_show = self.does_caret_show();
        let is_lit = does_show && phases.is_multiple_of(2);
        let has_blinked = core::mem::replace(&mut self.caret.is_lit, is_lit) != is_lit;
        self.caret.due_ns = does_show.then(|| self.caret.start_ns + (phases + 1) * CARET_NS);
        has_blinked
    }
}

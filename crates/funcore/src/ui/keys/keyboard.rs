use crate::base::math::round;
use crate::gfx::Rect;
use crate::gfx::motion::{
    Clock, Edge, GRAIN, NUDGE_SPEED, REST_DISTANCE, STAGGER_MS, SWING, Spring, settle_row, spring,
};
use alloc::string::String;
use alloc::vec::Vec;

pub(in crate::ui) const COLUMNS: i32 = 10;
pub(in crate::ui) const ROWS: i32 = 4;
const ROW_COUNT: usize = ROWS as usize;
const LAST_ROW_KEYS: i32 = 7;
const SPACE_KEYS: i32 = COLUMNS / 2;
const DROP_SHARE: f32 = 0.25;
const RISING: Spring = spring(16.0, 0.8);
const FOLDING: Spring = spring(40.0, 1.0);
const TRAVELLING: Spring = spring(60.0, 1.0);
const CARET: Spring = spring(40.0, 0.8);
const HOLD_NS: u64 = 400_000_000;
const REPEAT_NS: u64 = 100_000_000;
const ACCENTS: [(char, &str); 24] = [
    ('a', "àáâãäåæāăą"),
    ('c', "çćĉċč"),
    ('d', "ďđ"),
    ('e', "èéêëēĕėęě"),
    ('g', "ĝğġģ"),
    ('h', "ĥħ"),
    ('i', "ìíîïĩīĭįı"),
    ('j', "ĵ"),
    ('k', "ķ"),
    ('l', "ĺļľŀł"),
    ('n', "ñńņň"),
    ('o', "òóôõöøōŏőœ"),
    ('r', "ŕŗř"),
    ('s', "śŝşšß"),
    ('t', "ţťŧ"),
    ('u', "ùúûüũūŭůűų"),
    ('w', "ŵ"),
    ('y', "ýÿŷ"),
    ('z', "źżž"),
    ('$', "€£¥¢"),
    ('?', "¿"),
    ('!', "¡"),
    ('"', "«»„“”"),
    ('\'', "‘’"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Key {
    Char(char),
    Shift,
    Signs,
    Space,
    Back,
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layer {
    Lower,
    Upper,
    Digits,
    Signs,
}

impl Layer {
    const ALL: [Layer; 4] = [Layer::Lower, Layer::Upper, Layer::Digits, Layer::Signs];

    const fn rows(self) -> [&'static str; 3] {
        match self {
            Layer::Lower => ["qwertyuiop", "asdfghjkl", "zxcvbnm"],
            Layer::Upper => ["QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM"],
            Layer::Digits => ["1234567890", "-/:;()$&@\"", ".,?!'_#"],
            Layer::Signs => ["[]{}<>^*+=", "%\\|~`", ".,?!'_#"],
        }
    }

    const fn is_letters(self) -> bool {
        matches!(self, Layer::Lower | Layer::Upper)
    }

    const fn after_shift(self) -> Layer {
        match self {
            Layer::Lower => Layer::Upper,
            Layer::Upper => Layer::Lower,
            Layer::Digits => Layer::Signs,
            Layer::Signs => Layer::Digits,
        }
    }

    const fn after_signs(self) -> Layer {
        if self.is_letters() { Layer::Digits } else { Layer::Lower }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Cap {
    pub(super) key: Key,
    pub(super) area: Rect,
    pub(super) is_pressed: bool,
}

#[derive(Clone, Copy)]
struct Offer {
    character: char,
    row: Rect,
    selection: Option<usize>,
}

pub(in crate::ui) struct Keyboard {
    area: Rect,
    key_width: i32,
    key_height: i32,
    line_length: usize,
    layer: Layer,
    text: String,
    min_length: usize,
    max_length: usize,
    is_numeric: bool,
    is_finger_down: bool,
    pressed: Option<(Key, Rect)>,
    has_begun_here: bool,
    repeat_ns: Option<u64>,
    forms: Vec<(char, Vec<char>)>,
    offer_ns: Option<u64>,
    offer: Option<Offer>,
    large: Option<(char, Rect)>,
    growth: Edge,
    large_x: Edge,
    drop: [Edge; ROW_COUNT],
    wait: [u32; ROW_COUNT],
    is_opening: bool,
    caret: Edge,
    shake: Edge,
    clock: Clock,
}

fn to_single_capital(form: &char) -> Option<char> {
    let mut capitals = form.to_uppercase();
    match (capitals.next(), capitals.next()) {
        (Some(capital), None) => Some(capital),
        _ => None,
    }
}

impl Keyboard {
    pub(in crate::ui) fn new(area: Rect, line_length: usize) -> Keyboard {
        Keyboard {
            area,
            key_width: area.w / COLUMNS,
            key_height: area.h / ROWS,
            line_length: line_length.max(1),
            layer: Layer::Lower,
            text: String::new(),
            min_length: 0,
            max_length: 0,
            is_numeric: false,
            is_finger_down: false,
            pressed: None,
            has_begun_here: false,
            repeat_ns: None,
            forms: Vec::new(),
            offer_ns: None,
            offer: None,
            large: None,
            growth: Edge::still(0.0),
            large_x: Edge::still(0.0),
            drop: [Edge::still(0.0); ROW_COUNT],
            wait: [0; ROW_COUNT],
            is_opening: false,
            caret: Edge::still(0.0),
            shake: Edge::still(0.0),
            clock: Clock::new(),
        }
    }

    pub(in crate::ui) fn with_forms(mut self, can_draw: impl Fn(char) -> bool) -> Keyboard {
        for (base, forms) in ACCENTS {
            let small: Vec<char> = forms.chars().filter(|form| can_draw(*form)).collect();
            let upper = base.to_ascii_uppercase();
            if upper != base {
                let is_large = |form: &char| *form != upper && can_draw(*form);
                let large: Vec<char> = small.iter().filter_map(to_single_capital).filter(is_large).collect();
                if !large.is_empty() {
                    self.forms.push((upper, large));
                }
            }
            if !small.is_empty() {
                self.forms.push((base, small));
            }
        }
        self
    }

    fn forms_of(&self, character: char) -> Option<&[char]> {
        self.forms.iter().find(|entry| entry.0 == character).map(|entry| entry.1.as_slice())
    }

    fn offer_row(&self, key: Rect, count: usize) -> (Rect, usize) {
        let width = self.key_width * count as i32;
        let middle = key.x + key.w / 2;
        let left = (middle - width / 2).clamp(self.area.x, (self.area.right() - width).max(self.area.x));
        let above = ((middle - left) / self.key_width.max(1)).clamp(0, count as i32 - 1) as usize;
        (Rect::new(left, key.y - self.key_height, width, self.key_height), above)
    }

    pub(in crate::ui) fn begin(&mut self, min_length: usize, max_length: usize) {
        self.text.clear();
        self.text.reserve(max_length.saturating_mul(char::MAX.len_utf8()));
        self.min_length = min_length;
        self.max_length = max_length;
        (self.layer, self.is_numeric) = (Layer::Lower, false);
        self.release();
        self.large = None;
        self.caret = Edge::still(0.0);
        self.shake = Edge::still(0.0);
        self.stagger(self.area.h as f32);
        self.is_opening = true;
        self.clock.stir();
    }

    pub(in crate::ui) fn begin_digits(&mut self, min_length: usize, max_length: usize) {
        self.begin(min_length, max_length);
        (self.layer, self.is_numeric) = (Layer::Digits, true);
    }

    pub(in crate::ui) fn land(&mut self) {
        let is_still =
            self.drop.iter().all(|drop| *drop == Edge::still(0.0)) && self.wait.iter().all(|wait| *wait == 0);
        if !is_still {
            self.drop = [Edge::still(0.0); ROW_COUNT];
            self.wait = [0; ROW_COUNT];
            self.clock.stir();
        }
    }

    pub(in crate::ui) const fn top(&self) -> i32 {
        self.area.y
    }

    pub(in crate::ui) const fn line_length(&self) -> usize {
        self.line_length
    }

    pub(in crate::ui) fn set_line_length(&mut self, line_length: usize) {
        self.line_length = line_length.max(1);
        self.caret = Edge::still(self.end().1 as f32);
    }

    pub(in crate::ui) fn text(&self) -> &str {
        &self.text
    }

    pub(in crate::ui) fn take_text(&mut self) -> String {
        core::mem::take(&mut self.text)
    }

    pub(super) fn is_valid(&self) -> bool {
        (self.min_length..=self.max_length).contains(&self.text.chars().count())
    }

    pub(in crate::ui) const fn is_resting(&self) -> bool {
        self.clock.is_resting()
    }

    pub(in crate::ui) fn deadline(&self) -> Option<u64> {
        self.repeat_ns.into_iter().chain(self.offer_ns).min()
    }

    pub(super) const fn signs_label(&self) -> &'static str {
        if self.layer.is_letters() { "123" } else { "abc" }
    }

    pub(super) const fn shift_label(&self) -> Option<&'static str> {
        match self.layer {
            Layer::Lower | Layer::Upper => None,
            Layer::Digits => Some("#+="),
            Layer::Signs => Some("123"),
        }
    }

    fn places(&self) -> impl Iterator<Item = (Key, Rect)> + '_ {
        let (width, height, area) = (self.key_width, self.key_height, self.area);
        let top = move |line: i32| area.y + line * height;
        let characters = self.layer.rows().into_iter().enumerate().flat_map(move |(line, row)| {
            let first = area.x + (area.w - row.chars().count() as i32 * width) / 2;
            row.chars().enumerate().map(move |(place, character)| {
                (Key::Char(character), Rect::new(first + place as i32 * width, top(line as i32), width, height))
            })
        });
        let (side, last_row) = ((area.w - LAST_ROW_KEYS * width) / 2, top(ROWS - 2));
        let bar = SPACE_KEYS * width;
        let bar_left = (area.w - bar) / 2;
        let foot_row = top(ROWS - 1);
        characters.chain([
            (Key::Shift, Rect::new(area.x, last_row, side, height)),
            (Key::Back, Rect::new(area.x + area.w - side, last_row, side, height)),
            (Key::Signs, Rect::new(area.x, foot_row, bar_left, height)),
            (Key::Space, Rect::new(area.x + bar_left, foot_row, bar, height)),
            (Key::Done, Rect::new(area.x + bar_left + bar, foot_row, area.w - bar_left - bar, height)),
        ])
    }

    fn key_at(&self, x: i32, y: i32) -> Option<(Key, Rect)> {
        if !self.area.contains(x, y) || self.key_height <= 0 {
            return None;
        }
        let line = self.area.y + (y - self.area.y) / self.key_height * self.key_height;
        let distance = |area: &Rect| if area.contains(x, y) { 0 } else { (x - area.x - area.w / 2).abs().max(1) };
        self.places().filter(|place| place.1.y == line).min_by_key(|place| distance(&place.1))
    }

    fn end(&self) -> (usize, usize) {
        let count = self.text.chars().count();
        (count / self.line_length, count % self.line_length)
    }

    fn follow_text(&mut self, previous_line: usize) {
        let (line, column) = self.end();
        if line != previous_line {
            self.caret = Edge::still(column as f32);
        }
        self.clock.stir();
    }

    fn refuse(&mut self) {
        self.shake.speed += NUDGE_SPEED;
        self.clock.stir();
    }

    fn put(&mut self, character: char) {
        if self.text.chars().count() >= self.max_length || self.is_numeric && !character.is_ascii_digit() {
            return self.refuse();
        }
        let line = self.end().0;
        self.text.push(character);
        self.follow_text(line);
    }

    fn erase(&mut self) -> bool {
        let line = self.end().0;
        let has_erased = self.text.pop().is_some();
        if has_erased {
            self.follow_text(line);
        } else {
            self.refuse();
        }
        has_erased
    }

    fn stagger(&mut self, start: f32) {
        for line in 0..ROW_COUNT {
            self.drop[line] = Edge::still(start);
            self.wait[line] = line as u32 * STAGGER_MS;
        }
    }

    fn set_layer(&mut self, layer: Layer, way: f32) {
        self.layer = layer;
        self.stagger(-way * self.key_height as f32 * DROP_SHARE);
        self.is_opening = false;
        self.clock.stir();
    }

    fn slide_over_offer(&mut self, x: i32, y: i32) -> bool {
        let (Some(offer), Some((_, key))) = (self.offer, self.pressed) else {
            return false;
        };
        let count = self.forms_of(offer.character).map_or(0, <[char]>::len);
        let at = ((x - offer.row.x).div_euclid(self.key_width.max(1))).clamp(0, count as i32 - 1) as usize;
        let selection = (y < key.bottom()).then_some(at);
        if offer.selection != selection {
            self.offer = Some(Offer { selection, ..offer });
            self.clock.stir();
        }
        true
    }

    pub(in crate::ui) fn touch(&mut self, x: i32, y: i32, now_ns: u64) {
        if self.slide_over_offer(x, y) {
            return;
        }
        let key = self.key_at(x, y);
        let has_come_down = !core::mem::replace(&mut self.is_finger_down, true);
        if !has_come_down && key.map(|place| place.0) == self.pressed.map(|place| place.0) {
            return;
        }
        self.pressed = key;
        self.has_begun_here = has_come_down;
        self.repeat_ns = None;
        self.offer_ns = None;
        self.clock.stir();
        match key {
            Some((Key::Char(character), area)) => {
                if self.large.is_none() {
                    self.growth = Edge::still(0.0);
                    self.large_x = Edge::still((area.x + area.w / 2) as f32);
                }
                self.large = Some((character, area));
                self.offer_ns = self.forms_of(character).map(|_| now_ns + HOLD_NS);
            }
            Some((Key::Back, _)) if has_come_down => {
                self.repeat_ns = Some(now_ns + HOLD_NS);
                self.erase();
            }
            _ => {}
        }
    }

    pub(in crate::ui) fn lift(&mut self) -> bool {
        if let Some(offer) = self.offer.take() {
            self.release();
            if let Some(form) = offer.selection.and_then(|at| self.forms_of(offer.character)?.get(at).copied()) {
                self.put(form);
            }
            return false;
        }
        let (pressed, has_begun_here) = (self.pressed.take(), self.has_begun_here);
        self.release();
        match pressed.map(|place| place.0) {
            Some(Key::Char(character)) => self.put(character),
            Some(Key::Space) => self.put(' '),
            Some(Key::Back) if !has_begun_here => drop(self.erase()),
            Some(Key::Shift) => self.set_layer(self.layer.after_shift(), 1.0),
            Some(Key::Signs) => self.set_layer(self.layer.after_signs(), 1.0),
            Some(Key::Done) => return self.ask_done(),
            Some(Key::Back) | None => {}
        }
        false
    }

    pub(in crate::ui) fn release(&mut self) {
        self.is_finger_down = false;
        self.pressed = None;
        self.has_begun_here = false;
        self.repeat_ns = None;
        self.offer_ns = None;
        self.offer = None;
        self.clock.stir();
    }

    pub(in crate::ui) fn ask_done(&mut self) -> bool {
        let is_valid = self.is_valid();
        if !is_valid {
            self.refuse();
        }
        is_valid
    }

    pub(in crate::ui) fn wheel(&mut self, detents: i32) {
        let way = detents.signum();
        if way != 0 {
            let at = Layer::ALL.iter().position(|layer| *layer == self.layer).unwrap_or(0) as i32;
            self.set_layer(Layer::ALL[(at - way).rem_euclid(Layer::ALL.len() as i32) as usize], way as f32);
        }
    }

    fn open_offer(&mut self) {
        if let Some((Key::Char(character), key)) = self.pressed
            && let Some(count) = self.forms_of(character).map(<[char]>::len)
        {
            let (row, above) = self.offer_row(key, count);
            self.offer = Some(Offer { character, row, selection: Some(above) });
            self.clock.stir();
        }
    }

    pub(in crate::ui) fn advance(&mut self, now_ns: u64) {
        if self.offer_ns.is_some_and(|at| now_ns >= at) {
            self.offer_ns = None;
            self.open_offer();
        }
        while let Some(at) = self.repeat_ns.filter(|at| now_ns >= *at) {
            self.repeat_ns = self.erase().then_some(at + REPEAT_NS);
        }
        let mut clock = core::mem::take(&mut self.clock);
        clock.run(now_ns, || self.step());
        self.clock = clock;
    }

    fn step(&mut self) -> bool {
        let mut is_moving = false;
        let spring = if self.is_opening { RISING } else { SWING };
        for line in 0..ROW_COUNT {
            is_moving |= settle_row(&mut self.drop[line], &mut self.wait[line], 0.0, spring);
        }
        if let Some((_, area)) = self.large {
            let is_large = matches!(self.pressed, Some((Key::Char(_), _)));
            let is_changing =
                if is_large { self.growth.settle(1.0, SWING, GRAIN) } else { self.growth.settle(0.0, FOLDING, GRAIN) };
            let target = (area.x + area.w / 2) as f32;
            is_moving |= is_changing | self.large_x.settle(target, TRAVELLING, REST_DISTANCE);
            if !is_large && !is_changing {
                self.large = None;
            }
        }
        is_moving |= self.caret.settle(self.end().1 as f32, CARET, GRAIN);
        is_moving |= self.shake.settle(0.0, SWING, REST_DISTANCE);
        is_moving
    }

    pub(super) fn caps(&self) -> impl Iterator<Item = Cap> + '_ {
        self.places().map(|(key, area)| {
            let line = ((area.y - self.area.y) / self.key_height.max(1)).clamp(0, ROWS - 1) as usize;
            Cap {
                key,
                area: Rect::new(area.x, area.y + round(self.drop[line].at), area.w, area.h),
                is_pressed: self.pressed.is_some_and(|pressed| pressed.0 == key),
            }
        })
    }

    pub(super) fn large(&self) -> Option<(char, Rect, f32)> {
        let (character, area) = self.large?;
        let key = Rect::new(round(self.large_x.at) - area.w / 2, area.y, area.w, area.h);
        Some((character, key, self.growth.at.max(0.0)))
    }

    pub(in crate::ui) fn focus(&self) -> Option<(i32, i32)> {
        if let Some(Offer { row, selection: Some(at), .. }) = self.offer {
            return Some((row.x + at as i32 * self.key_width + self.key_width / 2, row.y + row.h / 2));
        }
        self.pressed.map(|place| place.1.middle())
    }

    pub(super) fn offer_cells(&self) -> Option<(impl Iterator<Item = (char, Rect)> + '_, Option<usize>)> {
        let Offer { character, row, selection } = self.offer?;
        let width = self.key_width;
        let forms = self.forms_of(character)?.iter().enumerate();
        let cells = forms.map(move |(at, form)| (*form, Rect::new(row.x + at as i32 * width, row.y, width, row.h)));
        Some((cells, selection))
    }

    pub(in crate::ui) fn caret(&self) -> (usize, f32) {
        (self.end().0, self.caret.at)
    }

    pub(in crate::ui) fn shake(&self) -> i32 {
        round(self.shake.at)
    }
}

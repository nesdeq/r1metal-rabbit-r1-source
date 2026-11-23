use crate::base::math::round;
use crate::gfx::Rect;
use crate::gfx::key::Ids;
use crate::gfx::motion::{
    Clock, Edge, GRAIN, NUDGE_SPEED, REST_DISTANCE, STAGGER_MS, SWING, Spring, settle_row, spring,
};
use crate::gfx::store::{Placement, Placements, Spot};
use alloc::vec::Vec;

pub(super) const ROLL_COLUMNS: usize = 2;
pub(super) const ROLL_ROWS: usize = 3;
const PAGING: Spring = spring(36.0, 0.8);
const TRAILING: Spring = spring(28.0, 1.0);
const OPENING: Spring = spring(30.0, 1.0);
const STACK_LIMIT: usize = 2;

type Edges = [f32; 4];

pub(super) struct Cell {
    pub(super) area: Rect,
    pub(super) spot: Spot,
    pub(super) placement: Placement,
    pub(super) has_item: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Band {
    pub(super) items: usize,
    pub(super) columns: usize,
    pub(super) height: f32,
    pub(super) caption: f32,
}

impl Band {
    pub(super) fn even(items: usize, columns: usize, room: f32, gap: f32, rows: usize) -> Band {
        Band { items, columns: columns.max(1), height: share(room, rows.max(1), gap), caption: 0.0 }
    }
}

fn share(length: f32, parts: usize, gap: f32) -> f32 {
    (length - (parts - 1) as f32 * gap).max(0.0) / parts as f32
}

trait Sink {
    fn place(&mut self, item: usize, edges: Edges, row: usize);
    fn caption(&mut self, item: usize, edges: Edges, row: usize);
}

struct Probe;

impl Sink for Probe {
    fn place(&mut self, _: usize, _: Edges, _: usize) {}
    fn caption(&mut self, _: usize, _: Edges, _: usize) {}
}

struct Finder {
    item: usize,
    edges: Option<Edges>,
}

impl Sink for Finder {
    fn place(&mut self, item: usize, edges: Edges, _: usize) {
        if item == self.item {
            self.edges = Some(edges);
        }
    }

    fn caption(&mut self, _: usize, _: Edges, _: usize) {}
}

#[derive(Default)]
struct Page {
    first: usize,
    edges: Vec<Edges>,
    rows: Vec<usize>,
    row_count: usize,
    captions: Vec<(usize, Edges, usize)>,
}

impl Sink for Page {
    fn place(&mut self, _: usize, edges: Edges, row: usize) {
        self.edges.push(edges);
        self.rows.push(row);
    }

    fn caption(&mut self, item: usize, edges: Edges, row: usize) {
        self.captions.push((item, edges, row));
    }
}

struct Departure {
    sheet: u64,
    pinned_sheet: u64,
    places: usize,
    page: Page,
    item_count: usize,
    pinned_count: usize,
    offsets: Vec<Edge>,
    wait: Vec<u32>,
    travel: f32,
}

impl Departure {
    const fn spot(&self, item: usize, pinned: usize) -> Spot {
        Spot { sheet: if item < pinned { self.pinned_sheet } else { self.sheet }, item }
    }
}

fn to_rect([left, top, right, bottom]: Edges, offset: f32, inset: f32) -> Rect {
    let (x, y) = (round(left + inset), round(top + offset + inset));
    Rect::new(x, y, (round(right - inset) - x).max(0), (round(bottom + offset - inset) - y).max(0))
}

pub(super) struct Grid {
    area: Edges,
    gap: f32,
    bands: Vec<Band>,
    page: Page,
    height: f32,
    pub(super) items: Option<usize>,
    pub(super) selection: usize,
    pub(super) round: u32,
    sheets: Ids,
    sheet: u64,
    pinned_sheet: u64,
    landings: u32,
    frame: [Edge; 4],
    leading: [bool; 4],
    rise: Vec<Edge>,
    wait: Vec<u32>,
    is_held_back: bool,
    departure: Option<Departure>,
    squeeze: Edge,
    is_pressed: bool,
    nudge: Edge,
    open: Edge,
    is_opened: bool,
    clock: Clock,
    is_at_foot: bool,
    is_filling: bool,
    is_pinning: bool,
    is_rolling: bool,
}

impl Grid {
    pub(super) fn empty() -> Grid {
        Grid::new(&Ids::default(), Rect::default(), 0, 0, 1, 1)
    }

    pub(super) fn new(ids: &Ids, area: Rect, height: i32, gap: i32, columns: usize, rows: usize) -> Grid {
        let band = Band::even(usize::MAX, columns, area.h.max(0) as f32, gap.max(0) as f32, rows);
        Grid::with_bands(ids, area, height, gap, &[band])
    }

    pub(super) fn with_bands(ids: &Ids, area: Rect, height: i32, gap: i32, bands: &[Band]) -> Grid {
        let (left, top, sheets) = (area.x as f32, area.y as f32, ids.part());
        let mut grid = Grid {
            area: [left, top, left + area.w.max(0) as f32, top + area.h.max(0) as f32],
            gap: gap.max(0) as f32,
            bands: bands.to_vec(),
            page: Page::default(),
            height: height.max(0) as f32,
            items: None,
            selection: 0,
            round: 0,
            sheet: sheets.next(),
            pinned_sheet: sheets.next(),
            sheets,
            landings: 0,
            frame: [Edge::still(0.0); 4],
            leading: [false; 4],
            rise: Vec::new(),
            wait: Vec::new(),
            is_held_back: false,
            departure: None,
            squeeze: Edge::still(0.0),
            is_pressed: false,
            nudge: Edge::still(0.0),
            open: Edge::still(0.0),
            is_opened: false,
            clock: Clock::new(),
            is_at_foot: false,
            is_filling: false,
            is_pinning: false,
            is_rolling: false,
        };
        grid.lay_still();
        grid
    }

    pub(super) fn at_foot(mut self) -> Grid {
        self.is_at_foot = true;
        self.lay_still();
        self
    }

    pub(super) const fn with_roll(mut self) -> Grid {
        self.is_rolling = true;
        self
    }

    pub(super) fn with_fill(mut self) -> Grid {
        self.is_filling = true;
        self.lay_still();
        self
    }

    pub(super) fn with_pins(mut self) -> Grid {
        self.is_pinning = true;
        self.lay_still();
        self
    }

    const fn width(&self) -> f32 {
        self.area[2] - self.area[0]
    }

    fn pinned_items(&self) -> usize {
        match self.bands.first() {
            Some(first) if self.is_pinning && self.bands.len() > 1 => first.items,
            _ => 0,
        }
    }

    fn place_on(&self, first: usize, places: usize, item: usize) -> Option<usize> {
        let pinned = self.pinned_items();
        let place = if item < pinned { item } else { item.checked_sub(first)? + pinned };
        (place < places).then_some(place)
    }

    fn item_on(&self, first: usize, place: usize) -> usize {
        place.checked_sub(self.pinned_items()).map_or(place, |unpinned| first + unpinned)
    }

    pub(super) fn item_of(&self, place: usize) -> usize {
        self.item_on(self.page.first, place)
    }

    pub(super) fn visible(&self) -> impl Iterator<Item = usize> + '_ {
        let count = self.items.unwrap_or(0);
        (0..self.places()).map(|place| self.item_of(place)).filter(move |item| *item < count)
    }

    pub(super) fn set_bands(&mut self, bands: &[Band]) {
        let is_same = match (bands.split_last(), self.bands.split_last()) {
            (Some((last, rest)), Some((current_last, current_rest))) => {
                rest == current_rest && Band { items: current_last.items, ..*last } == *current_last
            }
            _ => false,
        };
        if is_same || bands.is_empty() {
            return;
        }
        self.bands = bands.to_vec();
        self.lay_still();
        self.clock.stir();
    }

    fn band_of(&self, item: usize) -> (usize, usize) {
        let last = self.bands.len().saturating_sub(1);
        let mut start = 0usize;
        for (index, band) in self.bands.iter().enumerate() {
            let end = start.saturating_add(band.items);
            if index == last || item < end {
                return (index, start);
            }
            start = end;
        }
        (0, 0)
    }

    fn lay(&self, first: usize, sink: &mut impl Sink) -> usize {
        if self.is_filling {
            return self.fill(first, sink);
        }
        let offset = if self.is_at_foot {
            self.walk(first, 0.0, &mut Probe).1.map_or(0.0, |foot| (self.area[3] - foot).max(0.0))
        } else {
            0.0
        };
        self.walk(first, offset, sink).0
    }

    fn column_left(&self, column: usize, width: f32) -> f32 {
        self.area[0] + column as f32 * (width + self.gap)
    }

    fn walk(&self, first: usize, offset: f32, sink: &mut impl Sink) -> (usize, Option<f32>) {
        let Some(&last_band) = self.bands.last() else {
            return (first, None);
        };
        let (bottom, count, pinned) = (round(self.area[3]), self.items.unwrap_or(0), self.pinned_items());
        let mut item = if pinned > 0 { 0 } else { first };
        let (mut band, mut start) = self.band_of(item);
        let (mut y, mut row, mut foot) = (self.area[1], 0, None);
        loop {
            if item == pinned && first > pinned {
                item = first;
                (band, start) = self.band_of(first);
            }
            let shape = self.bands.get(band).copied().unwrap_or(last_band);
            let heading = if item == start { shape.caption } else { 0.0 };
            if row > 0 && (round(y + heading + shape.height) > bottom || heading + shape.height + self.gap < 1.0) {
                return (item, foot);
            }
            if heading > 0.0 {
                sink.caption(item, [self.area[0], y + offset, self.area[2], y + offset + heading], row);
                y += heading;
            }
            let width = share(self.width(), shape.columns.max(1), self.gap);
            let end = if band + 1 >= self.bands.len() { usize::MAX } else { start.saturating_add(shape.items) };
            if item < count {
                foot = Some(y + shape.height);
            }
            for column in 0..shape.columns.max(1) {
                if item >= end {
                    break;
                }
                let x = self.column_left(column, width);
                sink.place(item, [x, y + offset, x + width, y + offset + shape.height], row);
                item += 1;
            }
            y += shape.height + self.gap;
            row += 1;
            if item >= end {
                band += 1;
                start = end;
            }
        }
    }

    fn runs(&self, start: usize, end: usize, visit: &mut dyn FnMut(Band, usize, usize, bool)) {
        let mut at = start;
        while at < end {
            let (index, band_start) = self.band_of(at);
            let band = self.bands[index];
            let stop = if index + 1 >= self.bands.len() { end } else { band_start.saturating_add(band.items).min(end) };
            visit(band, at, stop - at, at == band_start && band.caption > 0.0);
            at = stop;
        }
    }

    fn fill(&self, first: usize, sink: &mut impl Sink) -> usize {
        let next = self.walk(first, 0.0, &mut Probe).0;
        let (pinned, count) = (self.pinned_items(), self.items.unwrap_or(0));
        let (mut y, mut row) = (self.area[1], 0);
        if let Some(head) = self.bands.first().filter(|_| pinned > 0) {
            let columns = head.columns.max(1);
            let width = share(self.width(), columns, self.gap);
            for item in 0..pinned {
                let x = self.column_left(item % columns, width);
                sink.place(item, [x, y, x + width, y + head.height], row);
                if item % columns == columns - 1 || item + 1 == pinned {
                    y += head.height + self.gap;
                    row += 1;
                }
            }
        }
        let start = first.max(pinned);
        let end = next.min(count).max(start);
        let columns_of = |band: Band, items: usize, is_stacked: bool| {
            let columns = band.columns.max(1);
            if is_stacked && items < STACK_LIMIT * columns { 1 } else { columns }
        };
        let measure = |is_stacked: bool| {
            let (mut height, mut rows, mut captions) = (0.0, 0, 0.0);
            self.runs(start, end, &mut |band, _, items, is_headed| {
                let band_rows = items.div_ceil(columns_of(band, items, is_stacked));
                height += band_rows as f32 * band.height;
                rows += band_rows;
                captions += if is_headed { band.caption } else { 0.0 };
            });
            (height, rows, captions)
        };
        let room = (self.area[3] - y).max(0.0);
        let gaps = |rows: usize| rows.saturating_sub(1) as f32 * self.gap;
        let is_stacked = {
            let (height, rows, captions) = measure(true);
            height + captions + gaps(rows) <= room
        };
        let (height, rows, captions) = measure(is_stacked);
        let spare = room - captions - gaps(rows);
        let grow = if height > 0.0 && spare > 0.0 { spare / height } else { 1.0 };
        self.runs(start, end, &mut |band, run_start, items, is_headed| {
            if is_headed {
                sink.caption(run_start, [self.area[0], y, self.area[2], y + band.caption], row);
                y += band.caption;
            }
            let (columns, row_height) = (columns_of(band, items, is_stacked), band.height * grow);
            let mut item = run_start;
            while item < run_start + items {
                let row_columns = columns.min(run_start + items - item);
                let width = share(self.width(), row_columns, self.gap);
                for column in 0..row_columns {
                    let x = self.column_left(column, width);
                    sink.place(item, [x, y, x + width, y + row_height], row);
                    item += 1;
                }
                y += row_height + self.gap;
                row += 1;
            }
        });
        next
    }

    fn page_of(&self, item: usize) -> usize {
        let mut first = self.pinned_items();
        loop {
            let next = self.lay(first, &mut Probe);
            if item < next || next <= first {
                return first;
            }
            let (band, start) = self.band_of(first);
            if band + 1 == self.bands.len() && (first > start || self.bands[band].caption == 0.0) {
                let size = next - first;
                return first + (item - first) / size * size;
            }
            first = next;
        }
    }

    fn lay_page(&mut self) {
        let mut page = core::mem::take(&mut self.page);
        page.first = self.page_of(self.selection);
        page.edges.clear();
        page.rows.clear();
        page.captions.clear();
        self.lay(page.first, &mut page);
        page.row_count = page.rows.iter().max().map_or(0, |row| row + 1);
        self.rise.resize(page.row_count, Edge::still(0.0));
        self.wait.resize(page.row_count, 0);
        self.page = page;
    }

    fn lay_still(&mut self) {
        self.lay_page();
        self.stand_frame();
    }

    fn stand_frame(&mut self) {
        self.frame = self.edges(self.slot()).map(Edge::still);
    }

    fn row_of(&self, place: usize) -> usize {
        self.page.rows.get(place).copied().unwrap_or(0)
    }

    fn edges_of(&self, item: usize) -> Edges {
        if let Some(place) = self.place_of(item) {
            return self.edges(place);
        }
        let mut finder = Finder { item, edges: None };
        self.lay(self.page_of(item), &mut finder);
        finder.edges.unwrap_or([self.area[0], self.area[1], self.area[0], self.area[1]])
    }

    pub(super) fn rest_area(&self, item: usize) -> Rect {
        to_rect(self.edges_of(item), 0.0, 0.0)
    }

    pub(super) fn size_at(&self, item: usize) -> (usize, usize) {
        let [left, top, right, bottom] = self.edges_of(item);
        (round(right - left).max(0) as usize, round(bottom - top).max(0) as usize)
    }

    fn edges(&self, place: usize) -> Edges {
        self.page.edges.get(place).copied().unwrap_or([self.area[0], self.area[1], self.area[0], self.area[1]])
    }

    pub(super) fn place_size(&self) -> (usize, usize) {
        let earlier = &self.bands[..self.bands.len().saturating_sub(1)];
        self.size_at(earlier.iter().fold(0, |first, band| first.saturating_add(band.items)))
    }

    pub(super) fn places(&self) -> usize {
        self.page.edges.len()
    }

    pub(super) fn captions(&self) -> impl Iterator<Item = (usize, Rect)> + '_ {
        let at = |edges: &[Edge], row: usize| edges.get(row).map_or(0.0, |edge| edge.at);
        let departing = self.departure.iter().flat_map(move |departure| {
            let to_room = move |caption: &(usize, Edges, usize)| {
                (caption.0, to_rect(caption.1, at(&departure.offsets, caption.2), 0.0))
            };
            departure.page.captions.iter().map(to_room)
        });
        let current = self
            .page
            .captions
            .iter()
            .map(move |caption| (caption.0, to_rect(caption.1, at(&self.rise, caption.2), 0.0)));
        departing.chain(current)
    }

    pub(super) const fn page(&self) -> usize {
        self.page.first
    }

    fn place_of(&self, item: usize) -> Option<usize> {
        self.place_on(self.page.first, self.places(), item)
    }

    fn slot(&self) -> usize {
        self.place_of(self.selection).unwrap_or(0)
    }

    fn choice(&self) -> Option<usize> {
        self.items.filter(|count| *count > 0).map(|_| self.slot())
    }

    fn sheet_of(&self, item: usize) -> u64 {
        if item < self.pinned_items() { self.pinned_sheet } else { self.sheet }
    }

    pub(super) fn spot_of(&self, item: usize) -> Option<Spot> {
        self.place_of(item).map(|_| Spot { sheet: self.sheet_of(item), item })
    }

    pub(super) fn selection_spot(&self) -> Option<Spot> {
        self.choice().map(|_| Spot { sheet: self.sheet_of(self.selection), item: self.selection })
    }

    fn placement_at(&self, place: usize) -> Placement {
        if self.wait[self.row_of(place)] > 0 { Placement::Waits } else { Placement::Shows(self.landings) }
    }

    fn arrive(&mut self, start: f32, delay: usize, is_all: bool) {
        let (rows, pinned) = (self.page.row_count, if is_all { 0 } else { self.pinned_rows() });
        for row in pinned..rows {
            let order = if start >= 0.0 { row - pinned } else { rows - 1 - row };
            self.rise[row] = Edge::still(start);
            self.wait[row] = (delay + order) as u32 * STAGGER_MS;
        }
        self.stand_frame();
    }

    fn pinned_rows(&self) -> usize {
        self.page.rows.iter().take(self.pinned_items()).max().map_or(0, |row| row + 1)
    }

    fn depart(&mut self, travel: f32, is_all: bool) {
        let rows = self.page.row_count;
        let to_wait = |row: usize| (if travel < 0.0 { row } else { rows - 1 - row }) as u32 * STAGGER_MS;
        self.departure = Some(Departure {
            sheet: self.sheet,
            pinned_sheet: self.pinned_sheet,
            places: self.places(),
            pinned_count: if is_all { 0 } else { self.pinned_items() },
            item_count: self.items.unwrap_or(0),
            page: core::mem::take(&mut self.page),
            offsets: self.rise.clone(),
            wait: (0..rows).map(to_wait).collect(),
            travel,
        });
        self.sheet = self.sheets.next();
        if is_all {
            self.pinned_sheet = self.sheets.next();
        }
        self.round = self.round.wrapping_add(1);
    }

    fn departure_rows(&self) -> usize {
        self.departure.as_ref().map_or(0, |departure| departure.page.row_count)
    }

    pub(super) fn begin(&mut self) {
        self.items = None;
        self.selection = 0;
        self.departure = None;
        self.is_held_back = false;
        self.round = self.round.wrapping_add(1);
        self.sheet = self.sheets.next();
        self.pinned_sheet = self.sheets.next();
        self.lay_page();
        self.arrive(self.height, 0, true);
        self.squeeze = Edge::still(0.0);
        self.nudge = Edge::still(0.0);
        self.open = Edge::still(0.0);
        self.is_pressed = false;
        self.is_opened = false;
        self.clock.stir();
    }

    pub(super) fn land(&mut self) {
        if self.items.is_none() {
            self.is_held_back = true;
            return;
        }
        self.landings = self.landings.wrapping_add(1);
        let is_still =
            self.rise.iter().all(|rise| *rise == Edge::still(0.0)) && self.wait.iter().all(|wait| *wait == 0);
        if self.departure.is_some() || !is_still {
            self.departure = None;
            self.rise.fill(Edge::still(0.0));
            self.wait.fill(0);
            self.clock.stir();
        }
    }

    fn forget_roll(&mut self) {
        self.items = None;
        self.selection = 0;
        self.lay_page();
        self.is_pressed = false;
        self.is_opened = false;
    }

    pub(super) fn leave(&mut self) {
        self.depart(self.height, true);
        self.forget_roll();
        self.clock.stir();
    }

    pub(super) fn redeal(&mut self) {
        self.depart(-self.height, false);
        self.forget_roll();
        self.arrive(self.height, self.departure_rows(), false);
        self.clock.stir();
    }

    pub(super) fn set_items(&mut self, count: usize) {
        self.items = Some(count);
        self.is_held_back = false;
        if self.is_at_foot || self.is_filling {
            self.lay_still();
        }
        self.choose(self.selection.min(count.saturating_sub(1)));
        self.clock.stir();
    }

    pub(super) fn set_items_at(&mut self, count: usize, first: usize) {
        self.items = Some(count);
        self.is_held_back = false;
        self.selection = first.min(count.saturating_sub(1));
        if self.is_at_foot || self.is_filling || self.page_of(self.selection) != self.page.first {
            self.lay_page();
        }
        self.stand_frame();
        self.clock.stir();
    }

    pub(super) fn choose(&mut self, item: usize) {
        let (previous, start) = (self.page(), self.edges(self.slot()));
        if item == self.selection {
            return;
        }
        self.selection = item;
        self.clock.stir();
        let page = self.page_of(item);
        if page == previous {
            let end = self.edges(self.slot());
            self.leading = [end[0] < start[0], end[1] < start[1], end[2] > start[2], end[3] > start[3]];
        } else if self.is_rolling {
            self.lay_page();
            self.sheet = self.sheets.next();
            self.stand_frame();
        } else {
            let travel = if page > previous { -self.height } else { self.height };
            self.depart(travel, false);
            self.lay_page();
            self.arrive(-travel, self.departure_rows(), false);
        }
    }

    pub(super) fn step(&mut self, detents: i32) {
        let Some(last) = self.items.and_then(|count| count.checked_sub(1)) else {
            return;
        };
        let target = (self.selection as i64 + i64::from(detents.signum())).clamp(0, last as i64) as usize;
        if target == self.selection && detents != 0 {
            self.swing(detents.signum() as f32);
        }
        self.choose(target);
    }

    pub(super) fn swing(&mut self, way: f32) {
        self.nudge.speed += way * NUDGE_SPEED;
        self.clock.stir();
    }

    pub(super) const fn renew(&mut self) {
        self.round = self.round.wrapping_add(1);
    }

    pub(super) fn press(&mut self, is_down: bool) {
        if core::mem::replace(&mut self.is_pressed, is_down) != is_down {
            self.clock.stir();
        }
    }

    pub(super) fn open(&mut self, is_open: bool) {
        if core::mem::replace(&mut self.is_opened, is_open) != is_open {
            self.clock.stir();
        }
    }

    pub(super) const fn opening(&self) -> f32 {
        self.open.at
    }

    pub(super) fn is_growing(&self) -> bool {
        self.open != Edge::still(if self.is_opened { 1.0 } else { 0.0 })
    }

    pub(super) const fn is_resting(&self) -> bool {
        self.clock.is_resting()
    }

    pub(super) fn advance(&mut self, now_ns: u64) {
        let mut clock = core::mem::take(&mut self.clock);
        clock.run(now_ns, || self.step_motion());
        self.clock = clock;
    }

    fn step_motion(&mut self) -> bool {
        let targets = self.edges(self.slot());
        let mut is_moving = false;
        for ((edge, target), is_leading) in self.frame.iter_mut().zip(targets).zip(self.leading) {
            let spring = if is_leading { PAGING } else { TRAILING };
            is_moving |= edge.settle(target, spring, REST_DISTANCE);
        }
        if !self.is_held_back {
            for row in 0..self.page.row_count {
                is_moving |= settle_row(&mut self.rise[row], &mut self.wait[row], 0.0, PAGING);
            }
        }
        if let Some(departure) = &mut self.departure {
            let mut is_gone = true;
            for row in 0..departure.page.row_count {
                is_gone &= !settle_row(&mut departure.offsets[row], &mut departure.wait[row], departure.travel, PAGING);
            }
            is_moving = true;
            if is_gone {
                self.departure = None;
            }
        }
        is_moving |= self.squeeze.settle(if self.is_pressed { 1.0 } else { 0.0 }, SWING, GRAIN);
        is_moving |= self.nudge.settle(0.0, SWING, REST_DISTANCE);
        is_moving |= self.open.settle(if self.is_opened { 1.0 } else { 0.0 }, OPENING, GRAIN);
        is_moving
    }

    fn sway(&self, place: usize) -> (f32, f32) {
        (self.rise[self.row_of(place)].at + self.nudge.at, self.squeeze.at.clamp(0.0, 1.0) * self.gap)
    }

    pub(super) fn selection_area(&self) -> Option<Rect> {
        let place = self.choice()?;
        let (offset, inset) = self.sway(place);
        Some(to_rect(self.edges(place), offset, inset))
    }

    pub(super) fn selection_place(&self) -> Option<Rect> {
        Some(to_rect(self.edges(self.choice()?), 0.0, 0.0))
    }

    pub(super) fn frame_area(&self) -> Option<Rect> {
        let (offset, inset) = self.sway(self.choice()?);
        Some(to_rect(self.frame.map(|edge| edge.at), offset, inset))
    }

    pub(super) fn area_of(&self, item: usize) -> Option<Rect> {
        let place = self.place_of(item)?;
        self.items.filter(|count| item < *count)?;
        Some(to_rect(self.edges(place), self.rise[self.row_of(place)].at, 0.0))
    }

    pub(super) fn drawn_area(&self, item: usize) -> Option<Rect> {
        self.area_of(item).or_else(|| {
            let departure = self.departure.as_ref()?;
            let place = self.place_on(departure.page.first, departure.places, item)?;
            Some(to_rect(departure.page.edges[place], departure.offsets[departure.page.rows[place]].at, 0.0))
        })
    }

    pub(super) fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        let is_at = |place: &usize| to_rect(self.edges(*place), self.rise[self.row_of(*place)].at, 0.0).contains(x, y);
        let place = (0..self.places()).find(is_at)?;
        Some(self.item_of(place)).filter(|item| self.items.is_some_and(|count| *item < count))
    }

    pub(super) fn cells(&self) -> impl Iterator<Item = Cell> + '_ {
        let (count, choice, pinned) = (self.items.unwrap_or(0), self.choice(), self.pinned_items());
        let departing = self.departure.iter().flat_map(move |departure| {
            (0..departure.places).map(move |place| {
                let item = self.item_on(departure.page.first, place);
                let spot = departure.spot(item, pinned);
                let row = departure.page.rows[place];
                Cell {
                    area: to_rect(departure.page.edges[place], departure.offsets[row].at, 0.0),
                    spot,
                    placement: self.placement(spot),
                    has_item: place >= departure.pinned_count && item < departure.item_count,
                }
            })
        });
        let current = (0..self.places()).map(move |place| {
            let is_selected = Some(place) == choice;
            let (offset, inset) = if is_selected { self.sway(place) } else { (self.rise[self.row_of(place)].at, 0.0) };
            let item = self.item_of(place);
            Cell {
                area: to_rect(self.edges(place), offset, inset),
                spot: Spot { sheet: self.sheet_of(item), item },
                placement: self.placement_at(place),
                has_item: item < count,
            }
        });
        departing.chain(current)
    }

    pub(super) fn departure_places(&self) -> usize {
        self.departure.as_ref().map_or(0, |departure| departure.places)
    }

    pub(super) fn pinned_places(&self) -> usize {
        self.pinned_items().min(self.places())
    }

    pub(super) fn spare_rooms(&self) -> impl Iterator<Item = Rect> + '_ {
        let departing = self.departure.iter().filter_map(|departure| {
            self.spare_room(&departure.page, departure.places, departure.item_count, &departure.offsets)
        });
        departing.chain(self.spare_room(&self.page, self.places(), self.items.unwrap_or(0), &self.rise))
    }

    pub(super) fn has_more(&self) -> bool {
        self.has_more_on(&self.page, self.places(), self.items.unwrap_or(0))
    }

    fn has_more_on(&self, page: &Page, places: usize, count: usize) -> bool {
        places.checked_sub(1).is_some_and(|last| self.item_on(page.first, last) + 1 < count)
    }

    fn spare_room(&self, page: &Page, places: usize, count: usize, offsets: &[Edge]) -> Option<Rect> {
        if self.is_filling || self.is_rolling || !self.has_more_on(page, places, count) {
            return None;
        }
        let edges = page.edges.iter().take(places);
        let [left, top, right, foot] = self.area;
        let (room, row) = if self.is_at_foot {
            let captions = page.captions.iter().map(|caption| &caption.1);
            let first = edges.chain(captions).map(|edges| edges[1]).fold(foot, f32::min);
            ([left, top, right, first - self.gap], 0)
        } else {
            let bottom = edges.map(|edges| edges[3]).fold(top, f32::max);
            ([left, bottom + self.gap, right, foot], page.row_count.saturating_sub(1))
        };
        let room = to_rect(room, offsets.get(row).map_or(0.0, |edge| edge.at), 0.0);
        (room.h > 0).then_some(room)
    }
}

impl Placements for Grid {
    fn placement(&self, spot: Spot) -> Placement {
        if let Some(place) = self.place_of(spot.item).filter(|_| spot.sheet == self.sheet_of(spot.item)) {
            return self.placement_at(place);
        }
        let Some(departure) = &self.departure else {
            return Placement::Gone;
        };
        let place = self.place_on(departure.page.first, departure.places, spot.item);
        if spot == departure.spot(spot.item, self.pinned_items())
            && place.is_some_and(|place| place >= departure.pinned_count)
        {
            return Placement::Leaves;
        }
        Placement::Gone
    }

    fn landings(&self) -> u32 {
        self.landings
    }
}

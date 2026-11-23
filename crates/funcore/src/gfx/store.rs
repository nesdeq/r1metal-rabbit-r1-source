use super::key::Ids;
use super::motion::{Clock, Edge, GRAIN, SWING};
use super::pen::Pen;
use super::{Picture, Rect};
use crate::base::math::round;
use alloc::vec::Vec;

pub(crate) trait AsPicture {
    const IS_REUSABLE: bool = true;

    fn picture(&self) -> &Picture;
}

impl AsPicture for Picture {
    fn picture(&self) -> &Picture {
        self
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Spot {
    pub(crate) sheet: u64,
    pub(crate) item: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Placement {
    Shows(u32),
    Waits,
    Leaves,
    Gone,
}

pub(crate) trait Placements {
    fn placement(&self, spot: Spot) -> Placement;
    fn landings(&self) -> u32;
}

pub(crate) const SOLE: Spot = Spot { sheet: 0, item: 0 };

pub(crate) struct SoleSpot(pub(crate) u32);

impl Placements for SoleSpot {
    fn placement(&self, spot: Spot) -> Placement {
        if spot == SOLE { Placement::Shows(self.0) } else { Placement::Gone }
    }

    fn landings(&self) -> u32 {
        self.0
    }
}

fn aim(placement: Placement, does_cut: bool) -> f32 {
    if placement == Placement::Waits && !does_cut { 0.0 } else { 1.0 }
}

fn step_unfold(share: &mut Edge, aim: f32) -> bool {
    share.settle(aim, SWING, GRAIN)
}

fn open_area(area: Rect, share: f32) -> Rect {
    let hidden = 1.0 - share.clamp(0.0, 1.0);
    let (across, down) = (round(area.w as f32 * hidden / 2.0), round(area.h as f32 * hidden / 2.0));
    Rect::new(area.x + across, area.y + down, area.w - 2 * across, area.h - 2 * down)
}

struct Entry<T> {
    spot: Spot,
    key: Option<u64>,
    picture: T,
    id: u64,
    landings: u32,
    under: Option<(T, u64)>,
    unfold: Edge,
    spare: Option<T>,
}

impl<T> Entry<T> {
    fn is_whole(&self, placement: Placement) -> bool {
        match placement {
            Placement::Leaves => true,
            Placement::Shows(landings) => landings != self.landings || self.unfold == Edge::still(1.0),
            Placement::Waits | Placement::Gone => false,
        }
    }
}

impl<T: AsPicture> Entry<T> {
    fn let_go(&mut self, picture: T) {
        if T::IS_REUSABLE {
            self.spare = Some(picture);
        }
    }

    fn drop_under(&mut self) {
        if let Some((under, _)) = self.under.take() {
            self.let_go(under);
        }
    }
}

pub(crate) struct Store<T> {
    entries: Vec<Entry<T>>,
    clock: Clock,
    does_cut: bool,
}

impl<T> Default for Store<T> {
    fn default() -> Self {
        Store { entries: Vec::new(), clock: Clock::new(), does_cut: false }
    }
}

impl<T> Store<T> {
    pub(crate) fn with_cuts() -> Store<T> {
        Store { does_cut: true, ..Store::default() }
    }

    pub(crate) fn prune(&mut self, placements: &impl Placements) {
        self.entries.retain(|entry| placements.placement(entry.spot) != Placement::Gone);
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(crate) fn reserve(&mut self, spots: usize) {
        self.entries.reserve(spots);
    }

    pub(crate) fn spare(&mut self, spot: Spot) -> Option<T> {
        self.entries.iter_mut().find(|entry| entry.spot == spot)?.spare.take()
    }

    pub(crate) fn repaint(&mut self, ids: &Ids, spot: Spot) -> Option<&mut T> {
        let entry = self.entries.iter_mut().find(|entry| entry.spot == spot)?;
        entry.id = ids.next();
        Some(&mut entry.picture)
    }

    pub(crate) const fn is_resting(&self) -> bool {
        self.clock.is_resting()
    }

    pub(crate) const fn does_cut(&self) -> bool {
        self.does_cut
    }
}

impl<T: AsPicture> Store<T> {
    fn at(&self, spot: Spot) -> Option<&Entry<T>> {
        self.entries.iter().find(|entry| entry.spot == spot)
    }

    pub(crate) fn holds(&self, spot: Spot, key: u64) -> bool {
        self.at(spot).is_some_and(|entry| entry.key == Some(key))
    }

    pub(crate) fn put(
        &mut self,
        ids: &Ids,
        placements: &impl Placements,
        (spot, key): (Spot, Option<u64>),
        picture: T,
        is_whole: bool,
    ) -> bool {
        let is_cut = is_whole || self.does_cut;
        self.prune(placements);
        if placements.placement(spot) == Placement::Gone {
            return false;
        }
        let landings = placements.landings();
        let unfold = Edge::still(if is_cut { 1.0 } else { 0.0 });
        match self.entries.iter_mut().find(|entry| entry.spot == spot) {
            None => {
                self.entries.push(Entry {
                    spot,
                    key,
                    picture,
                    id: ids.next(),
                    landings,
                    under: None,
                    unfold,
                    spare: None,
                });
            }
            Some(entry) => {
                entry.key = key;
                let before = core::mem::replace(&mut entry.picture, picture);
                let (last, new) = (before.picture(), entry.picture.picture());
                if last.width == new.width && last.words == new.words {
                    entry.let_go(before);
                    return true;
                }
                entry.drop_under();
                if is_cut {
                    entry.let_go(before);
                } else {
                    entry.under = Some((before, entry.id));
                }
                (entry.id, entry.landings, entry.unfold) = (ids.next(), landings, unfold);
            }
        }
        // A picture that unfolds sets the clock going; one cut in stands as it is.
        if !is_cut {
            self.clock.stir();
        }
        true
    }

    pub(crate) fn get(&self, spot: Spot) -> Option<(&T, u64)> {
        self.at(spot).map(|entry| (&entry.picture, entry.id))
    }

    pub(crate) fn visible_area(&self, spot: Spot, placement: Placement, area: Rect) -> Option<Rect> {
        let entry = self.at(spot)?;
        Some(open_area(area, if entry.under.is_some() || entry.is_whole(placement) { 1.0 } else { entry.unfold.at }))
    }

    pub(crate) fn advance(&mut self, now_ns: u64, placements: &impl Placements) {
        self.prune(placements);
        let Store { entries, clock, does_cut } = self;
        for entry in entries.iter_mut() {
            let placement = placements.placement(entry.spot);
            let is_whole = match placement {
                Placement::Leaves => true,
                Placement::Shows(landings) => core::mem::replace(&mut entry.landings, landings) != landings,
                Placement::Waits | Placement::Gone => false,
            };
            if is_whole {
                entry.unfold = Edge::still(1.0);
                entry.drop_under();
            }
            // A row that has set off sets its pictures unfolding.
            if entry.unfold != Edge::still(aim(placement, *does_cut)) {
                clock.stir();
            }
        }
        clock.run(now_ns, || {
            let mut is_moving = false;
            for entry in entries.iter_mut() {
                is_moving |= step_unfold(&mut entry.unfold, aim(placements.placement(entry.spot), *does_cut));
                if entry.unfold == Edge::still(1.0) {
                    entry.drop_under();
                }
            }
            is_moving
        });
    }

    pub(crate) fn draw(
        &self,
        pen: &mut Pen<'_, '_>,
        (spot, placement): (Spot, Placement),
        (area, within): (Rect, Rect),
        mut extra: impl FnMut(&mut Pen<'_, '_>, &T, (i32, i32), Rect),
    ) {
        let Some(entry) = self.at(spot) else {
            return;
        };
        let mut lay = |pen: &mut Pen<'_, '_>, item: &T, id: u64, open: Rect| {
            let (picture, Some(visible)) = (item.picture(), open.clip(within)) else {
                return;
            };
            if picture.width == 0 {
                return;
            }
            let Rect { x, y, .. } = area.centre(picture.width as i32, picture.height() as i32);
            pen.blit(visible, x, y, picture.width, &picture.words, id);
            extra(pen, item, (x, y), visible);
        };
        if entry.is_whole(placement) {
            return lay(pen, &entry.picture, entry.id, area);
        }
        if let Some((under, id)) = &entry.under {
            lay(pen, under, *id, area);
        }
        lay(pen, &entry.picture, entry.id, open_area(area, entry.unfold.at));
    }
}

pub(crate) struct Unfold {
    share: Edge,
    clock: Clock,
}

impl Default for Unfold {
    fn default() -> Self {
        Unfold { share: Edge::still(1.0), clock: Clock::new() }
    }
}

impl Unfold {
    pub(crate) const fn begin(&mut self) {
        self.share = Edge::still(0.0);
        self.clock.stir();
    }

    pub(crate) const fn land(&mut self) {
        self.share = Edge::still(1.0);
        self.clock.halt();
    }

    pub(crate) fn advance(&mut self, now_ns: u64) {
        let Unfold { share, clock } = self;
        clock.run(now_ns, || step_unfold(share, 1.0));
    }

    pub(crate) const fn is_resting(&self) -> bool {
        self.clock.is_resting()
    }

    pub(crate) fn is_whole(&self) -> bool {
        self.share == Edge::still(1.0)
    }

    pub(crate) fn open_area(&self, area: Rect) -> Rect {
        open_area(area, self.share.at)
    }
}

#[cfg(test)]
impl<T> Store<T> {
    fn under(&self, spot: Spot) -> Option<&T> {
        self.entries.iter().find(|entry| entry.spot == spot)?.under.as_ref().map(|(under, _)| under)
    }

    fn forget_keys(&mut self) {
        self.entries.iter_mut().for_each(|entry| entry.key = None);
    }

    fn count(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::pixel::{Color, PixelFormat};
    use crate::gfx::Surface;
    use crate::gfx::motion::support::{FRAME_MS, MS, frames};
    use crate::gfx::pen::DisplayList;
    use alloc::vec;

    const SETTLE_MS: u64 = 2_000;
    const ONE: Spot = Spot { sheet: 1, item: 0 };
    const TWO: Spot = Spot { sheet: 1, item: 1 };
    const OTHER: Spot = Spot { sheet: 2, item: 0 };
    const PLACE: Rect = Rect::new(10, 5, 20, 10);
    const WIDTH: usize = 40;
    const HEIGHT: usize = 20;

    struct Grid<F>(F, u32);

    impl<F: Fn(Spot) -> Placement> Placements for Grid<F> {
        fn placement(&self, spot: Spot) -> Placement {
            (self.0)(spot)
        }

        fn landings(&self) -> u32 {
            self.1
        }
    }

    fn filled(colour: u32) -> Picture {
        Picture { words: vec![colour; (PLACE.w * PLACE.h) as usize], width: PLACE.w as usize }
    }

    fn run(store: &mut Store<Picture>, from_ms: u64, span_ms: u64, placements: &impl Placements) -> u64 {
        frames(from_ms, span_ms, |now_ns| store.advance(now_ns, placements))
    }

    fn drawn(store: &Store<Picture>, placement: Placement) -> Vec<u32> {
        let mut words = vec![0u32; WIDTH * HEIGHT];
        let mut surface = Surface::new(&mut words, WIDTH as u32, HEIGHT as u32, WIDTH, PixelFormat::XRGB8888).unwrap();
        DisplayList::new(WIDTH, HEIGHT, 0).frame(&mut surface, None, |pen| {
            pen.fill(pen.bounds(), Color(9));
            store.draw(pen, (ONE, placement), (PLACE, PLACE), |_, _, _, _| {});
        });
        words
    }

    fn visible_width(store: &Store<Picture>, placement: Placement) -> Option<i32> {
        store.visible_area(ONE, placement, PLACE).map(|area| area.w)
    }

    #[test]
    fn waits_then_unfolds_from_middle() {
        let (mut store, waits, shows) =
            (Store::default(), Grid(|_| Placement::Waits, 0), Grid(|_| Placement::Shows(0), 0));
        assert!(store.put(&Ids::default(), &waits, (ONE, None), filled(1), false));
        let now = run(&mut store, 0, SETTLE_MS, &waits);
        assert_eq!(
            (visible_width(&store, Placement::Waits), store.is_resting()),
            (Some(0), true),
            "while its row has still to set off it stands folded, and nothing moves"
        );
        store.advance((now + FRAME_MS) * MS, &shows);
        store.advance((now + 5 * FRAME_MS) * MS, &shows);
        let half = visible_width(&store, Placement::Shows(0)).unwrap_or(0);
        assert!(
            (1..PLACE.w).contains(&half) && !store.is_resting(),
            "once it has set off it opens from the middle: {half}"
        );
        let screen = drawn(&store, Placement::Shows(0));
        assert!(
            screen[10 * WIDTH + 20] == 1 && screen[5 * WIDTH + 10] == 9,
            "drawn as far as it has opened, about the place's middle"
        );
        run(&mut store, now + 5 * FRAME_MS, SETTLE_MS, &shows);
        assert_eq!((store.visible_area(ONE, Placement::Shows(0), PLACE), store.is_resting()), (Some(PLACE), true));
        assert!(drawn(&store, Placement::Shows(0))[5 * WIDTH + 10] == 1, "and whole once it has unfolded");
    }

    #[test]
    fn new_picture_unfolds_over_old() {
        let (mut store, shows, ids) = (Store::default(), Grid(|_| Placement::Shows(0), 0), Ids::default());
        store.put(&ids, &shows, (ONE, None), filled(1), false);
        let now = run(&mut store, 0, SETTLE_MS, &shows);
        let first = store.get(ONE).map(|(_, id)| id);
        store.put(&ids, &shows, (ONE, None), filled(1), false);
        assert_eq!(
            (store.get(ONE).map(|(_, id)| id), store.is_resting(), store.under(ONE).is_none()),
            (first, true, true),
            "the same pixels again: the same name, and nothing moves"
        );
        store.put(&ids, &shows, (ONE, None), filled(2), false);
        assert!(
            store.get(ONE).is_some_and(|(picture, id)| picture.words[0] == 2 && Some(id) != first)
                && store.under(ONE).is_some_and(|under| under.words[0] == 1),
            "another picture takes a name of its own, the one before under it"
        );
        store.advance((now + FRAME_MS) * MS, &shows);
        store.advance((now + 5 * FRAME_MS) * MS, &shows);
        let screen = drawn(&store, Placement::Shows(0));
        assert!(
            screen[10 * WIDTH + 20] == 2 && screen[5 * WIDTH + 10] == 1,
            "the new one opens over the old one, which stands whole about it"
        );
        assert_eq!(
            store.visible_area(ONE, Placement::Shows(0), PLACE),
            Some(PLACE),
            "the place counts as filled meanwhile"
        );
        run(&mut store, now + 5 * FRAME_MS, SETTLE_MS, &shows);
        assert!(store.under(ONE).is_none(), "once it has unfolded, what was under it is let go");
    }

    #[test]
    fn spot_holds_key() {
        let (mut store, shows, ids) = (Store::default(), Grid(|_| Placement::Shows(0), 0), Ids::default());
        store.put(&ids, &shows, (ONE, Some(7)), filled(1), false);
        assert!(
            store.holds(ONE, 7) && !store.holds(ONE, 8) && !store.holds(TWO, 7),
            "the key it was put by, in its spot alone"
        );
        store.put(&ids, &shows, (ONE, None), filled(1), false);
        assert!(!store.holds(ONE, 7), "a picture put by no key holds none");
        store.put(&ids, &shows, (ONE, Some(8)), filled(2), false);
        store.forget_keys();
        assert!(!store.holds(ONE, 8), "and one whose key is forgotten holds none");
    }

    #[test]
    fn landed_or_leaving_page_stands_whole() {
        let (mut store, shows, ids) = (Store::default(), Grid(|_| Placement::Shows(0), 0), Ids::default());
        store.put(&ids, &shows, (ONE, None), filled(1), false);
        store.put(&ids, &shows, (TWO, None), filled(2), false);
        store.put(&ids, &shows, (OTHER, None), filled(3), false);
        assert_eq!(
            (
                store.visible_area(ONE, Placement::Shows(1), PLACE),
                store.visible_area(TWO, Placement::Leaves, PLACE),
                store.visible_area(OTHER, Placement::Shows(0), PLACE).map(|area| area.w)
            ),
            (Some(PLACE), Some(PLACE), Some(0)),
            "a page put at rest at once since its picture came, and one on its way out, stand whole at once"
        );
        let grid = Grid(
            |spot: Spot| match spot {
                ONE => Placement::Shows(1),
                TWO => Placement::Leaves,
                _ => Placement::Gone,
            },
            1,
        );
        store.advance(0, &grid);
        assert_eq!(
            (
                store.count(),
                store.get(OTHER).is_none(),
                store.visible_area(ONE, Placement::Shows(1), PLACE),
                store.is_resting()
            ),
            (2, true, Some(PLACE), false),
            "a picture of no page is let go, and the others stand"
        );
        run(&mut store, 0, FRAME_MS, &grid);
        assert!(store.is_resting(), "with nothing left to unfold");
        store.put(&ids, &grid, (ONE, None), filled(4), false);
        assert_eq!(
            store.visible_area(ONE, Placement::Shows(1), PLACE),
            Some(PLACE),
            "a picture that comes after the page was put at rest unfolds over the one before"
        );
        store.advance(FRAME_MS * MS, &grid);
        store.advance(3 * FRAME_MS * MS, &grid);
        assert!(store.under(ONE).is_some() && !store.is_resting(), "as any does");
    }

    #[test]
    fn put_prunes_gone_spots() {
        let (mut store, ids) = (Store::default(), Ids::default());
        let first = Grid(|spot: Spot| if spot.sheet == ONE.sheet { Placement::Shows(0) } else { Placement::Gone }, 0);
        store.put(&ids, &first, (ONE, None), filled(1), false);
        store.put(&ids, &first, (TWO, None), filled(2), false);
        let next = Grid(|spot: Spot| if spot.sheet == OTHER.sheet { Placement::Shows(0) } else { Placement::Gone }, 0);
        assert!(!store.put(&ids, &next, (ONE, None), filled(3), false), "a spot on no page takes nothing");
        assert_eq!(
            (store.count(), store.get(ONE).is_none()),
            (0, true),
            "and what stood on the page before is let go, though the store never stepped"
        );
        assert!(
            store.put(&ids, &next, (OTHER, None), filled(3), false) && store.count() == 1,
            "a spot on the page takes its picture"
        );
    }

    #[test]
    fn cutting_store_stands_whole() {
        let (mut store, waits, ids) = (Store::with_cuts(), Grid(|_| Placement::Waits, 0), Ids::default());
        store.put(&ids, &waits, (ONE, None), filled(1), false);
        run(&mut store, 0, SETTLE_MS, &waits);
        assert_eq!(
            (
                store.visible_area(ONE, Placement::Waits, PLACE),
                store.is_resting(),
                drawn(&store, Placement::Waits)[5 * WIDTH + 10]
            ),
            (Some(PLACE), true, 1),
            "whole at once, its row waiting or not, and nothing moves"
        );
        let first = store.get(ONE).map(|(_, id)| id);
        store.put(&ids, &waits, (ONE, None), filled(2), false);
        run(&mut store, SETTLE_MS, FRAME_MS, &waits);
        assert!(
            store.under(ONE).is_none()
                && store.is_resting()
                && drawn(&store, Placement::Waits)[5 * WIDTH + 10] == 2
                && store.get(ONE).is_some_and(|(_, id)| Some(id) != first),
            "another is cut in over it under a name of its own"
        );
    }

    struct Foreign(Picture);

    impl AsPicture for Foreign {
        const IS_REUSABLE: bool = false;

        fn picture(&self) -> &Picture {
            &self.0
        }
    }

    #[test]
    fn spot_keeps_spare_picture() {
        let (mut store, shows, ids) = (Store::default(), Grid(|_| Placement::Shows(0), 0), Ids::default());
        store.put(&ids, &shows, (ONE, None), filled(1), false);
        assert!(store.spare(ONE).is_none(), "a spot that has let go of nothing keeps nothing");
        store.put(&ids, &shows, (ONE, None), filled(1), false);
        assert!(
            store.spare(ONE).is_some_and(|spare| spare.words[0] == 1),
            "one of the same pixels lets the one before go at once"
        );
        let now = run(&mut store, 0, SETTLE_MS, &shows);
        store.put(&ids, &shows, (ONE, None), filled(2), false);
        assert!(store.spare(ONE).is_none(), "another unfolds over the one before, which shows until it has");
        run(&mut store, now, SETTLE_MS, &shows);
        assert!(
            store.spare(ONE).is_some_and(|spare| spare.words[0] == 1) && store.spare(ONE).is_none(),
            "and then is let go of, taken once"
        );
        let mut cut = Store::with_cuts();
        cut.put(&ids, &shows, (ONE, None), filled(1), false);
        cut.put(&ids, &shows, (ONE, None), filled(2), false);
        assert!(cut.spare(ONE).is_some_and(|spare| spare.words[0] == 1), "a store that cuts lets it go at once");
        let mut foreign = Store::default();
        foreign.put(&ids, &shows, (ONE, None), Foreign(filled(1)), false);
        foreign.put(&ids, &shows, (ONE, None), Foreign(filled(1)), false);
        assert!(foreign.spare(ONE).is_none(), "what is made elsewhere is kept by nobody");
        store.put(&ids, &shows, (TWO, None), filled(1), false);
        store.put(&ids, &shows, (TWO, None), filled(1), false);
        let elsewhere = Grid(|spot: Spot| if spot == OTHER { Placement::Shows(0) } else { Placement::Gone }, 0);
        store.put(&ids, &elsewhere, (OTHER, None), filled(1), false);
        assert!(store.spare(TWO).is_none(), "and a spot on no page keeps nothing");
    }

    #[test]
    fn sole_spot_and_repaint() {
        let (mut store, sole, ids) = (Store::default(), SoleSpot(0), Ids::default());
        assert!(
            store.put(&ids, &sole, (SOLE, None), filled(1), false)
                && !store.put(&ids, &sole, (ONE, None), filled(3), false)
        );
        let first = store.get(SOLE).map(|(_, id)| id);
        store.repaint(&ids, SOLE).expect("the picture of the one spot").words[0] = 2;
        assert!(
            store.get(SOLE).is_some_and(|(picture, id)| picture.words[0] == 2 && Some(id) != first)
                && store.repaint(&ids, ONE).is_none(),
            "drawn into where it stands, it is named anew"
        );
        let mut words = vec![0u32; WIDTH * HEIGHT];
        let mut surface = Surface::new(&mut words, WIDTH as u32, HEIGHT as u32, WIDTH, PixelFormat::XRGB8888).unwrap();
        DisplayList::new(WIDTH, HEIGHT, 0).frame(&mut surface, None, |pen| {
            pen.fill(pen.bounds(), Color(9));
            store.draw(pen, (SOLE, Placement::Shows(1)), (PLACE, Rect::new(10, 5, 5, 10)), |_, _, _, _| {});
        });
        assert!(
            (words[5 * WIDTH + 10], words[5 * WIDTH + 11], words[5 * WIDTH + 15]) == (2, 1, 9),
            "drawn as far as it lies within what it is drawn within too"
        );
        let mut whole = Store::default();
        whole.put(&ids, &sole, (SOLE, None), filled(1), true);
        whole.put(&ids, &sole, (SOLE, None), filled(4), true);
        assert!(
            whole.is_resting()
                && whole.under(SOLE).is_none()
                && whole.visible_area(SOLE, Placement::Shows(0), PLACE) == Some(PLACE)
                && whole.spare(SOLE).is_some_and(|spare| spare.words[0] == 1),
            "put whole, a picture stands whole at once in a store that unfolds others, the one before let go"
        );
    }

    #[test]
    fn unfold_from_middle_to_whole() {
        let mut unfold = Unfold::default();
        assert!(
            unfold.is_whole() && unfold.is_resting() && unfold.open_area(PLACE) == PLACE,
            "at first it stands whole"
        );
        unfold.begin();
        assert_eq!((unfold.open_area(PLACE).w, unfold.is_resting()), (0, false), "begun, it fills nothing of its area");
        let now = frames(0, 5 * FRAME_MS, |now_ns| unfold.advance(now_ns));
        let part = unfold.open_area(PLACE);
        assert!(
            (1..PLACE.w).contains(&part.w) && part.middle() == PLACE.middle(),
            "on its way it fills the middle of its area: {part:?}"
        );
        frames(now, SETTLE_MS, |now_ns| unfold.advance(now_ns));
        assert!(unfold.is_whole() && unfold.is_resting(), "and all of it at rest once it has unfolded");
        unfold.begin();
        unfold.land();
        assert!(unfold.is_whole() && unfold.is_resting(), "landed, it stands whole at once");
    }
}

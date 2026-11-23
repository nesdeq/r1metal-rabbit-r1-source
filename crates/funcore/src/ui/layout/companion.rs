use super::stage::LOOK as STAGE;
use super::{HomeSeat, HomeStyle, HomeWords, Layout, Look, Motion};
use crate::base::math::round;
use crate::gfx::Rect;
use crate::gfx::key::Ids;
use crate::ui::bar::{BarStyle, BarText};
use crate::ui::gauge::{BlockRow, ClockPlace, Gauge, GaugePlace, NamePlace, ValuePlace};
use crate::ui::grid::Grid;
use crate::ui::item::{Form, LARGE_SCALE, RowStyle};
use crate::ui::kit::IconPlace;
use crate::ui::list::{Forms, ListStyle, Page, Primary};
use crate::ui::ruler::{Ruler, SENTENCE_LINES};
use crate::ui::textroom::RoomStyle;
use crate::ui::textroom::talk::TalkStyle;

const LINES: usize = 7;
const ROW: Form = Form::Row(RowStyle { has_icons: true, is_carded: false, is_led: true, has_leads: true });

pub(super) struct CompanionLayout;

fn gauge_height(ruler: &Ruler) -> i32 {
    LARGE_SCALE * ruler.large() + ruler.gap + round(ruler.rim())
}

fn top_in_middle(ruler: &Ruler, height: i32) -> i32 {
    let room = ruler.under_bar();
    room.y + (room.h - height) / 2
}

impl Layout for CompanionLayout {
    fn look(&self) -> Look {
        Look {
            list: ListStyle {
                page: Page::Lines(LINES),
                has_rules: false,
                primary: Primary::Large,
                forms: Forms { figure: Form::Columns, action: ROW, entry: Form::EntryMarked, plain: ROW },
                ..STAGE.list
            },
            bar: BarStyle { text: BarText::Speech, has_figure: false, has_selection: false, has_pager: false },
            room: RoomStyle { talk: TalkStyle::Sides, has_head: true, ..STAGE.room },
            motion: Motion::Typed,
            home: HomeStyle {
                is_suggested: true,
                has_stages: false,
                words: HomeWords::Sentence,
                seat: HomeSeat::Square,
                ..STAGE.home
            },
            ..STAGE
        }
    }

    fn home_grid(&self, ids: &Ids, ruler: &Ruler, _count: usize) -> Grid {
        let height = ruler.large() + ruler.small();
        let gap = ruler.gap;
        let top = ruler.square_foot() + SENTENCE_LINES as i32 * ruler.small() + ruler.gap_phi2();
        let foot = ruler.foot_top() - gap;
        let rows = ((foot - top + gap) / (height + gap)).max(1);
        let area = Rect::new(gap, top, ruler.width - 2 * gap, rows * (height + gap) - gap);
        Grid::new(ids, area, ruler.height, gap, 1, rows as usize)
    }

    fn place_gauge(&self, ruler: &Ruler, gauge: &Gauge<'_>, at: usize, count: usize) -> GaugePlace {
        let (count, height, spacing) = (count as i32, gauge_height(ruler), ruler.gap_phi2());
        let y = top_in_middle(ruler, count * height + (count - 1) * spacing) + at as i32 * (height + spacing);
        let (room, gap, inset) = (ruler.under_bar(), ruler.gap, ruler.framed_inset());
        let (value_height, x) = (LARGE_SCALE * ruler.large(), room.x + inset);
        let icon_left = x + ruler.heading.text_width(gauge.value) * LARGE_SCALE + 2 * gap;
        let side = value_height - ruler.small();
        let (block_gap, blocks) = (ruler.lines().0, gauge.blocks.len() as i32);
        let block = (room.w - 2 * inset - (blocks - 1) * block_gap) / blocks.max(1);
        let first = Rect::new(x, y + value_height + gap, block, round(ruler.rim()));
        GaugePlace {
            icon: Some(IconPlace::at((icon_left, y), side)),
            name: Some(NamePlace { at: (icon_left, y + side), room: room.right() - inset - icon_left }),
            value: Some(ValuePlace { at: (x, y), scale: LARGE_SCALE }),
            blocks: Some(BlockRow { first, step: block + block_gap }),
            ..GaugePlace::default()
        }
    }

    fn place_clock(&self, ruler: &Ruler) -> ClockPlace {
        let time_top = top_in_middle(ruler, ruler.time_height(LARGE_SCALE) + ruler.tick_height());
        ClockPlace {
            scale_top: time_top + ruler.time_height(LARGE_SCALE),
            tick_gap: ruler.lines().1,
            time_top,
            time_scale: LARGE_SCALE,
            time_left: Some(ruler.under_bar().x + ruler.framed_inset()),
            ..ClockPlace::default()
        }
    }
}

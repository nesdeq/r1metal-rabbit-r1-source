use super::stage::LOOK as STAGE;
use super::{HomeSeat, HomeStyle, HomeWords, Layout, Look, Motion};
use crate::gfx::Rect;
use crate::gfx::key::Ids;
use crate::ui::bar::{BarStyle, BarText};
use crate::ui::corner::CornerStyle;
use crate::ui::edit::CaretStyle;
use crate::ui::gauge::{BlockRow, Brackets, ClockPlace, Gauge, GaugePlace, NamePlace, ValuePlace};
use crate::ui::grid::Grid;
use crate::ui::item::Form;
use crate::ui::list::{Forms, ListForm, ListStyle, Page, Primary};
use crate::ui::picture::{RollForm, RollStyle};
use crate::ui::ruler::Ruler;
use crate::ui::textroom::RoomStyle;
use crate::ui::textroom::talk::TalkStyle;

pub(super) struct TerminalLayout;

const fn gauge_height(ruler: &Ruler) -> i32 {
    2 * ruler.small() + ruler.large() + ruler.gap
}

fn top_at_foot(ruler: &Ruler, height: i32) -> i32 {
    ruler.under_bar().bottom() - height
}

impl Layout for TerminalLayout {
    fn look(&self) -> Look {
        Look {
            list: ListStyle {
                form: ListForm::Typed,
                page: Page::Fill,
                has_rules: false,
                is_at_foot: true,
                has_arrow: false,
                primary: Primary::Line,
                forms: Forms {
                    figure: Form::FigureTyped,
                    action: Form::Button,
                    entry: Form::EntryTable,
                    plain: Form::Typed,
                },
                ..STAGE.list
            },
            bar: BarStyle { text: BarText::Trail, has_figure: false, has_selection: false, has_pager: true },
            is_one_size: true,
            has_status: true,
            room: RoomStyle { talk: TalkStyle::Named, is_at_foot: true, ..STAGE.room },
            caret: CaretStyle::Block,
            roll: RollStyle { form: RollForm::Captioned, is_cut: true },
            motion: Motion::Print,
            corner: CornerStyle::Hidden,
            home: HomeStyle { has_stages: false, words: HomeWords::Prompt, seat: HomeSeat::Square, ..STAGE.home },
            ..STAGE
        }
    }

    fn home_grid(&self, ids: &Ids, ruler: &Ruler, _count: usize) -> Grid {
        let line = ruler.line_icon_side();
        let top = ruler.square_foot() + ruler.small() + 2 * ruler.gap_phi2();
        let foot = ruler.height - ruler.status_height() - ruler.gap;
        let rows = ((foot - top) / line.max(1)).max(1);
        let area = Rect::new(0, foot - rows * line, ruler.width, rows * line);
        Grid::new(ids, area, ruler.height, 0, 1, rows as usize).at_foot()
    }

    fn place_gauge(&self, ruler: &Ruler, gauge: &Gauge<'_>, at: usize, count: usize) -> GaugePlace {
        let (count, gap, height) = (count as i32, ruler.gap, gauge_height(ruler));
        let first_top = top_at_foot(ruler, gap / 2 + count * height + (count - 1) * gap) + gap / 2;
        let top = first_top + at as i32 * (height + gap);
        let (small, small_cell, left) = (ruler.small(), ruler.small_cell(), ruler.typed_left());
        let y = top + small + ruler.large() + gap;
        let (blocks_left, block_height, block_inset) = (left + small_cell, small / 2, ruler.lines().0);
        let first = Rect::new(
            blocks_left + block_inset,
            y + (small - block_height) / 2,
            small_cell - 2 * block_inset,
            block_height,
        );
        GaugePlace {
            name: Some(NamePlace { at: (left, top), room: ruler.width - 2 * left }),
            value: Some(ValuePlace { at: (left, top + small), scale: 1 }),
            blocks: Some(BlockRow { first, step: small_cell }),
            brackets: Some(Brackets { left, right: blocks_left + gauge.blocks.len() as i32 * small_cell, y }),
            ..GaugePlace::default()
        }
    }

    fn place_clock(&self, ruler: &Ruler) -> ClockPlace {
        let time_top = top_at_foot(ruler, ruler.time_height(1) + ruler.tick_height());
        ClockPlace {
            scale_top: time_top + ruler.time_height(1),
            tick_gap: ruler.lines().1,
            time_top,
            time_scale: 1,
            time_left: Some(ruler.typed_left()),
            ..ClockPlace::default()
        }
    }
}

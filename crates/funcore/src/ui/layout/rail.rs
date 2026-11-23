use super::stage::{LOOK as STAGE, StageLayout};
use super::{HomeSeat, HomeStyle, Layout, Look};
use crate::gfx::Rect;
use crate::gfx::key::Ids;
use crate::ui::corner::CornerStyle;
use crate::ui::gauge::{ClockPlace, Gauge, GaugePlace};
use crate::ui::grid::Grid;
use crate::ui::item::{Form, RowStyle};
use crate::ui::list::{Forms, ListStyle};
use crate::ui::ruler::Ruler;

pub(super) struct RailLayout;

const ROW: Form = Form::Row(RowStyle { has_icons: false, is_carded: false, is_led: false, has_leads: false });

impl Layout for RailLayout {
    fn look(&self) -> Look {
        Look {
            list: ListStyle {
                has_rules: false,
                forms: Forms { figure: ROW, action: ROW, entry: ROW, plain: ROW },
                ..STAGE.list
            },
            corner: CornerStyle::Rail,
            home: HomeStyle { has_stages: false, seat: HomeSeat::Bar, ..STAGE.home },
            has_rail: true,
            ..STAGE
        }
    }

    fn home_grid(&self, ids: &Ids, ruler: &Ruler, _count: usize) -> Grid {
        let (gap, side) = (ruler.gap, ruler.rail_side());
        let area = Rect::new(gap, gap, side, ruler.height - 2 * gap - ruler.rail_foot());
        let rows = ((area.h + gap) / (side + gap)).max(1);
        Grid::new(ids, area, ruler.height, gap, 1, rows as usize)
    }

    fn place_gauge(&self, ruler: &Ruler, gauge: &Gauge<'_>, at: usize, count: usize) -> GaugePlace {
        StageLayout.place_gauge(ruler, gauge, at, count)
    }

    // The scale's ticks stand a thin line apart, so that all of them fit the pane.
    fn place_clock(&self, ruler: &Ruler) -> ClockPlace {
        ClockPlace { tick_gap: ruler.lines().0, ..StageLayout.place_clock(ruler) }
    }
}

use super::{HomeSeat, HomeStyle, HomeWords, Layout, Look, Motion};
use crate::base::math::round;
use crate::gfx::Rect;
use crate::gfx::measures::PHI;
use crate::ui::bar::{BarStyle, BarText};
use crate::ui::corner::CornerStyle;
use crate::ui::edit::CaretStyle;
use crate::ui::gauge::{BlockRow, ClockPlace, Gauge, GaugePlace};
use crate::ui::item::{Form, RowStyle};
use crate::ui::keys::KeysStyle;
use crate::ui::kit::IconPlace;
use crate::ui::list::{Forms, ListForm, ListStyle, Page, Primary};
use crate::ui::picture::{RollForm, RollStyle};
use crate::ui::ruler::Ruler;
use crate::ui::textroom::RoomStyle;
use crate::ui::textroom::talk::TalkStyle;

const LINES: usize = 6;
const ROW: Form = Form::Row(RowStyle { has_icons: false, is_carded: false, is_led: false, has_leads: true });

pub(super) const LOOK: Look = Look {
    list: ListStyle {
        form: ListForm::Rows,
        page: Page::Lines(LINES),
        has_rules: true,
        is_carded: false,
        is_at_foot: false,
        has_arrow: true,
        primary: Primary::Hidden,
        forms: Forms { figure: ROW, action: ROW, entry: ROW, plain: ROW },
    },
    bar: BarStyle { text: BarText::Plain, has_figure: true, has_selection: false, has_pager: false },
    is_one_size: false,
    has_status: false,
    room: RoomStyle { talk: TalkStyle::Log, is_at_foot: false, has_disc: false, has_head: false },
    keys: KeysStyle { is_carded: false },
    caret: CaretStyle::Underline,
    roll: RollStyle { form: RollForm::Cells, is_cut: false },
    motion: Motion::Slide,
    corner: CornerStyle::Foot,
    home: HomeStyle {
        is_suggested: false,
        has_stages: true,
        has_hero: false,
        words: HomeWords::Nothing,
        seat: HomeSeat::Flight,
    },
    has_rail: false,
};

pub(super) struct StageLayout;

fn centred_icon(ruler: &Ruler) -> IconPlace {
    let side = ruler.stage_side();
    let left = ruler.left as f32 + (ruler.pane_width() as f32 - side) / 2.0;
    IconPlace { left, top: (ruler.height as f32 - side) / 2.0, side }
}

fn icon_foot(ruler: &Ruler) -> i32 {
    let side = ruler.stage_side();
    round((ruler.height as f32 - side) / 2.0 + side + ruler.band / PHI)
}

impl Layout for StageLayout {
    fn look(&self) -> Look {
        LOOK
    }

    fn place_gauge(&self, ruler: &Ruler, gauge: &Gauge<'_>, at: usize, _count: usize) -> GaugePlace {
        let (width, height, gap) = ruler.block();
        let row = gauge.blocks.len() as i32 * (width + gap) - gap;
        let top = icon_foot(ruler) + at as i32 * (height + ruler.gap_phi());
        let first = Rect::new(ruler.centre_left(row), top, width, height);
        GaugePlace {
            icon: (at == 0).then(|| centred_icon(ruler)),
            is_icon_lit: true,
            blocks: Some(BlockRow { first, step: width + gap }),
            ..GaugePlace::default()
        }
    }

    fn place_clock(&self, ruler: &Ruler) -> ClockPlace {
        let scale_top = icon_foot(ruler);
        ClockPlace {
            icon: Some(centred_icon(ruler)),
            scale_top,
            tick_gap: ruler.lines().1,
            time_top: scale_top + ruler.tick_height() + 2 * ruler.gap,
            time_scale: 1,
            ..ClockPlace::default()
        }
    }
}

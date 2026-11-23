use super::stage::LOOK as STAGE;
use super::{HomeSeat, HomeStyle, Layout, Look, Motion};
use crate::ui::bar::{BarStyle, BarText};
use crate::ui::gauge::{ClockPlace, Gauge, GaugePlace};
use crate::ui::item::{Form, RowStyle};
use crate::ui::list::{Forms, ListForm, ListStyle, Primary};
use crate::ui::picture::{RollForm, RollStyle};
use crate::ui::ruler::Ruler;
use crate::ui::textroom::RoomStyle;
use crate::ui::textroom::talk::TalkStyle;

pub(super) struct DialLayout;

const ROW: Form = Form::Row(RowStyle { has_icons: true, is_carded: false, is_led: false, has_leads: true });

impl Layout for DialLayout {
    fn look(&self) -> Look {
        Look {
            list: ListStyle {
                form: ListForm::Dial,
                has_rules: false,
                primary: Primary::Line,
                forms: Forms { figure: ROW, action: ROW, entry: ROW, plain: ROW },
                ..STAGE.list
            },
            room: RoomStyle { talk: TalkStyle::Sides, has_disc: true, ..STAGE.room },
            roll: RollStyle { form: RollForm::Drum, ..STAGE.roll },
            motion: Motion::Roll,
            bar: BarStyle { text: BarText::Plain, has_figure: false, has_selection: false, has_pager: false },
            home: HomeStyle { has_stages: false, seat: HomeSeat::Square, ..STAGE.home },
            ..STAGE
        }
    }

    fn place_gauge(&self, _ruler: &Ruler, _gauge: &Gauge<'_>, _at: usize, _count: usize) -> GaugePlace {
        GaugePlace::default()
    }

    fn place_clock(&self, _ruler: &Ruler) -> ClockPlace {
        ClockPlace::default()
    }
}

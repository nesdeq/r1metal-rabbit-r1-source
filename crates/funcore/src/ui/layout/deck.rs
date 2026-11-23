use super::stage::LOOK as STAGE;
use super::{HomeSeat, HomeStyle, Layout, Look, Motion};
use crate::gfx::Rect;
use crate::gfx::key::Ids;
use crate::ui::bar::{BarStyle, BarText};
use crate::ui::corner::CornerStyle;
use crate::ui::gauge::{BlockRow, ClockPlace, Gauge, GaugePlace, NamePlace, ValuePlace};
use crate::ui::grid::{Band, Grid};
use crate::ui::item::{FIGURE_SCALE, Form, RowStyle};
use crate::ui::keys::KeysStyle;
use crate::ui::kit::IconPlace;
use crate::ui::list::{BOARD_COLUMNS, Forms, ListStyle, Page, Primary};
use crate::ui::ruler::Ruler;
use crate::ui::textroom::RoomStyle;
use crate::ui::textroom::talk::TalkStyle;

pub(super) struct DeckLayout;

const ROW: Form = Form::Row(RowStyle { has_icons: true, is_carded: true, is_led: false, has_leads: true });

impl Layout for DeckLayout {
    fn look(&self) -> Look {
        Look {
            list: ListStyle {
                page: Page::Board,
                has_rules: false,
                is_carded: true,
                primary: Primary::Card,
                forms: Forms { figure: Form::FigureTile, action: Form::ActionTile, entry: Form::EntryCard, plain: ROW },
                ..STAGE.list
            },
            room: RoomStyle { talk: TalkStyle::Cards, ..STAGE.room },
            keys: KeysStyle { is_carded: true },
            motion: Motion::Deal,
            bar: BarStyle { text: BarText::Plain, has_figure: true, has_selection: true, has_pager: false },
            corner: CornerStyle::Top,
            home: HomeStyle { has_stages: false, has_hero: true, seat: HomeSeat::Hero, ..STAGE.home },
            ..STAGE
        }
    }

    fn home_grid(&self, ids: &Ids, ruler: &Ruler, _count: usize) -> Grid {
        let (gap, top) = (ruler.gap, ruler.top_line_height());
        let area = Rect::new(gap, top, ruler.width - 2 * gap, ruler.height - top - gap);
        let hero = ruler.home_side() + 2 * ruler.card_inset();
        let hero = Band { items: 1, columns: 1, height: hero as f32, caption: 0.0 };
        let (room, tile) = (area.h - hero.height as i32 - gap, ruler.tile_height());
        let rows = ((room + gap) / (tile + gap)).max(1) as usize;
        let tiles = Band::even(usize::MAX, BOARD_COLUMNS, room as f32, gap as f32, rows);
        Grid::with_bands(ids, area, ruler.height, gap, &[hero, tiles]).with_pins().with_fill()
    }

    fn place_gauge(&self, ruler: &Ruler, gauge: &Gauge<'_>, at: usize, count: usize) -> GaugePlace {
        let (room, gap, count) = (ruler.under_bar(), ruler.gap, count as i32);
        let height = (room.h - (count - 1) * gap) / count.max(1);
        let area = Rect::new(room.x, room.y + at as i32 * (height + gap), room.w, height);
        let (inset, value_height, x) = (ruler.card_inset(), FIGURE_SCALE * ruler.large(), area.x + ruler.card_inset());
        let value_top = area.bottom() - inset - value_height;
        let name_top = value_top - ruler.small();
        let side = ruler.card_icon(name_top - area.y - inset, value_height);
        let (block, block_gap) = ruler.card_gauge_block();
        let left = area.right() - inset - gauge.blocks.len() as i32 * (block + block_gap) + block_gap;
        GaugePlace {
            card: Some(area),
            icon: Some(IconPlace::at((x, name_top - side), side)),
            name: Some(NamePlace { at: (x, name_top), room: area.w - 2 * inset }),
            value: Some(ValuePlace { at: (x, value_top), scale: FIGURE_SCALE }),
            blocks: Some(BlockRow { first: Rect::new(left, value_top, block, value_height), step: block + block_gap }),
            ..GaugePlace::default()
        }
    }

    fn place_clock(&self, ruler: &Ruler) -> ClockPlace {
        let (card, inset, large) = (ruler.under_bar(), ruler.card_inset(), ruler.large());
        let scale_top = card.bottom() - inset - ruler.tick_height();
        let time_top = scale_top - ruler.time_height(FIGURE_SCALE);
        let (left, side) = (card.x + inset, ruler.card_icon(time_top - card.y - inset, FIGURE_SCALE * large));
        ClockPlace {
            card: Some(card),
            icon: Some(IconPlace::at((left, time_top - side), side)),
            scale_top,
            scale_left: Some(left),
            tick_gap: ruler.lines().0,
            time_top,
            time_scale: FIGURE_SCALE,
            time_left: Some(left),
        }
    }
}

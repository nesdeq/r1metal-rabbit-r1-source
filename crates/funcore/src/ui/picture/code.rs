use crate::codec::qr::{QUIET_ZONE, Symbol};
use crate::gfx::Rect;
use crate::gfx::palette::CODE_LIGHT;
use crate::gfx::pen::Pen;
use crate::ui::field::draw_field;
use crate::ui::kit::Kit;

pub(in crate::ui) fn draw_code(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, symbol: Option<&Symbol>, caption: &str) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (room, line) = (ruler.under_bar(), ruler.small());
    let caption_room = line + ruler.gap;
    if let Some(symbol) = symbol {
        let modules = (symbol.side() + 2 * QUIET_ZONE) as i32;
        let module = (room.w.min(room.h - caption_room) / modules.max(1)).max(1);
        let side = modules * module;
        let (left, top) = (room.x + (room.w - side) / 2, room.y + (room.h - caption_room - side) / 2);
        pen.fill(Rect::new(left, top, side, side), CODE_LIGHT);
        let (left, top) = (left + QUIET_ZONE as i32 * module, top + QUIET_ZONE as i32 * module);
        for y in 0..symbol.side() {
            for x in 0..symbol.side() {
                if symbol.is_dark(x, y) {
                    let at = Rect::new(left + x as i32 * module, top + y as i32 * module, module, module);
                    pen.fill(at, roles.background);
                }
            }
        }
    }
    let width = ruler.body.text_width(caption).min(room.w);
    let at = (room.x + (room.w - width) / 2, room.bottom() - line);
    draw_field(kit, pen, &ruler.body, at, caption, width, roles.text_dim);
}

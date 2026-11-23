use crate::base::pixel::Color;
use crate::gfx::{Lettering, Rect, Surface};
use crate::ui::field::{cells, sentence_count, sentence_lines};
use crate::ui::kit::{Kit, draw_frame, paint_card};
use crate::ui::ruler::RULE;

pub(in crate::ui) struct Sketch<'a> {
    pub(in crate::ui) screen: &'a [u32],
    pub(in crate::ui) frame: Color,
    pub(in crate::ui) name: &'a str,
    // The longest sets the room every sketch leaves them.
    pub(in crate::ui) names: &'a [&'a str],
    pub(in crate::ui) note: Option<&'a str>,
}

impl Sketch<'_> {
    pub(in crate::ui) fn paint(&self, plate: &mut Surface<'_>, kit: &Kit<'_>) {
        let (ruler, roles) = (kit.ruler, kit.roles);
        paint_card(plate, &roles);
        let (gap, small, Rect { w: width, h: height, .. }) = (ruler.gap, ruler.small(), plate.bounds());
        let room = cells(width - 2 * gap, &ruler.body);
        let max_lines = self.names.iter().map(|name| sentence_count(name, room)).max().unwrap_or(1) as i32;
        let max_height = (height - 3 * gap - (max_lines + 1) * small).max(0);
        let sketch_width = (max_height * ruler.width / ruler.height.max(1)).min(width - 2 * gap).max(0);
        let sketch_height = sketch_width * ruler.height / ruler.width.max(1);
        let sketch = Rect::new((width - sketch_width) / 2, gap, sketch_width, sketch_height);
        plate.shrink(sketch, ruler.width.max(0) as usize, self.screen);
        draw_frame(plate, sketch, RULE, self.frame);
        let mut y = 2 * gap + sketch_height;
        let (whole, plain) = (plate.bounds(), Lettering::default());
        sentence_lines(self.name, room, |_, line| {
            plate.text(whole, &ruler.body, (gap, y), line, roles.text, 1, plain);
            y += small;
        });
        if let Some(note) = self.note {
            plate.text(whole, &ruler.body, (gap, y), note, roles.text_dim, 1, plain);
        }
    }
}

use crate::base::math::round;
use crate::base::sampling::{Turn, turn_words};
use crate::domain::gallery::Thumbnail;
use crate::gfx::key::Key;
use crate::gfx::pen::Pen;
use crate::gfx::{self, Picture, Rect};
use crate::ui::kit::{Canvas, Kit, draw_frame, mix_rect};

pub(in crate::ui) struct Opening<'a> {
    pub(in crate::ui) place: Rect,
    pub(in crate::ui) view: Rect,
    pub(in crate::ui) small: Option<(&'a Thumbnail, u64)>,
    pub(in crate::ui) large: Option<(&'a Picture, u64)>,
    pub(in crate::ui) quarters: u8,
    pub(in crate::ui) share: f32,
}

impl Opening<'_> {
    pub(in crate::ui) fn draw(
        &self,
        pen: &mut Pen<'_, '_>,
        kit: &Kit<'_>,
        draw_over: impl FnOnce(&mut Pen<'_, '_>, Rect, Rect),
    ) {
        let Opening { place, view, small, large, quarters, share } = *self;
        let (small, small_id) = (small.map(|thumbnail| thumbnail.0), small.map_or(0, |thumbnail| thumbnail.1));
        let (width, words, id, turned, start_size) = match (large, small) {
            (Some((large, large_id)), small) => {
                let size =
                    small.map_or((large.width, large.height()), |small| (small.picture.width, small.picture.height()));
                let size = if small.is_some() && quarters % 2 == 1 { (size.1, size.0) } else { size };
                (large.width, &large.words[..], large_id, None, size)
            }
            (None, Some(small)) if quarters % 4 == 0 => {
                let size = (small.picture.width, small.picture.height());
                (small.picture.width, &small.picture.words[..], small_id, None, size)
            }
            (None, Some(small)) => {
                let size = gfx::turned_size((small.picture.width, small.picture.height()), quarters);
                let id = Key::default().word(small_id).word(u64::from(quarters)).value();
                (size.0, &[][..], id, Some(small), size)
            }
            (None, None) => (0, &[][..], 0, None, (place.w.max(0) as usize, place.h.max(0) as usize)),
        };
        let end_size = match (large, small) {
            (Some((large, _)), _) => (large.width as i32, large.height() as i32),
            (None, Some(small)) => {
                let (view_width, view_height) = view.view_size(quarters);
                let fit = gfx::to_fit(small.size, (view_width.max(0) as usize, view_height.max(0) as usize));
                if quarters % 2 == 1 { (fit.1 as i32, fit.0 as i32) } else { (fit.0 as i32, fit.1 as i32) }
            }
            (None, None) => (place.w, place.h),
        };
        let end_area = view.centre(end_size.0, end_size.1);
        let start_area = place.centre(start_size.0 as i32, start_size.1 as i32);
        let (area, window) = (mix_rect(start_area, end_area, share), mix_rect(place, view, share));
        if area.clip(window) != Some(window) {
            pen.fill(window, kit.roles.background);
        }
        if width > 0 && area.w > 0 && area.h > 0 {
            match turned {
                Some(small) => {
                    pen.stretch_canvas(Canvas::Opening as usize, (area, window), start_size, id, |words, _| {
                        let turn = Turn { quarters, is_mirrored: false };
                        turn_words(&small.picture.words, small.picture.width, turn, words);
                    });
                }
                None => pen.stretch(area, window, width, words, id),
            }
        }
        if let Some(visible) = area.clip(window).filter(|_| width > 0) {
            draw_over(pen, visible, window);
        }
        draw_frame(pen, window, round(kit.ruler.lines().1 as f32 * (1.0 - share)), kit.roles.glow);
    }
}

mod keyboard;

pub(super) use keyboard::{COLUMNS, Keyboard, ROWS};

use super::kit::{Canvas, Kit, draw_card, draw_frame, mix_rect, trace};
use crate::base::pixel::Color;
use crate::base::text::has_words;
use crate::gfx::icon::Icon;
use crate::gfx::key::Key as Name;
use crate::gfx::pen::Pen;
use crate::gfx::{Lettering, Rect, Surface};
use alloc::string::String;
use keyboard::{Cap, Key};

pub(super) const KEY_ICONS: [&str; 4] = [icon!("shift"), icon!("backspace"), icon!("space_bar"), icon!("check")];
const SHIFT_ICON: usize = 0;
const BACK_ICON: usize = 1;
const SPACE_ICON: usize = 2;
const DONE_ICON: usize = 3;
pub(super) const LARGE: i32 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct KeysStyle {
    pub(super) is_carded: bool,
}

pub(super) fn ready(keys: &mut Keyboard, limit: usize) {
    keys.begin(1, limit);
    keys.land();
}

pub(super) fn raise(keys: &mut Keyboard, is_up: bool, limit: usize) {
    if is_up { ready(keys, limit) } else { keys.release() }
}

pub(super) fn take_typed(keys: &mut Keyboard) -> Option<String> {
    let text = keys.take_text();
    keys.release();
    has_words(&text).then_some(text)
}

fn cap_colour(kit: &Kit<'_>, is_valid: bool, cap: &Cap) -> Color {
    if cap.is_pressed {
        kit.roles.glow
    } else if cap.key == Key::Done && !is_valid {
        kit.roles.text_dim
    } else {
        kit.roles.text
    }
}

fn draw_caps(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, keyboard: &Keyboard, icons: &[Icon; 4], ground: Color) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (cell, line, small_cell, small) = (ruler.cell(), ruler.large(), ruler.small_cell(), ruler.small());
    let side = ruler.width / COLUMNS;
    let mut glyph = [0u8; 4];
    let is_valid = keyboard.is_valid();
    let colours = [roles.text, roles.text_dim, roles.glow];
    for (at, colour) in colours.into_iter().enumerate() {
        // A colour two roles share is drawn once.
        if colours[..at].contains(&colour) {
            continue;
        }
        for cap in keyboard.caps().filter(|cap| cap_colour(kit, is_valid, cap) == colour) {
            let (x, y) = cap.area.middle();
            let label = match cap.key {
                Key::Signs => Some(keyboard.signs_label()),
                Key::Shift => keyboard.shift_label(),
                _ => None,
            };
            if let Some(label) = label {
                let at = (x - label.len() as i32 * small_cell / 2, y - small / 2);
                pen.text(None, &ruler.body, at, label, colour, 1);
            } else if let Key::Char(character) = cap.key {
                let text = character.encode_utf8(&mut glyph);
                pen.text(None, &ruler.heading, (x - cell / 2, y - line / 2), text, colour, 1);
            }
        }
        pen.cover(colour, ground, |mask| {
            for cap in keyboard.caps().filter(|cap| cap_colour(kit, is_valid, cap) == colour) {
                let icon = match cap.key {
                    Key::Shift if keyboard.shift_label().is_none() => SHIFT_ICON,
                    Key::Back => BACK_ICON,
                    Key::Space => SPACE_ICON,
                    Key::Done => DONE_ICON,
                    Key::Char(_) | Key::Shift | Key::Signs => continue,
                };
                let (x, y) = cap.area.middle();
                let (left, top) = ((x - side / 2) as f32, (y - side / 2) as f32);
                trace(mask, &icons[icon], left, top, side as f32);
            }
        });
    }
}

fn draw_large(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, (character, key, growth): (char, Rect, f32)) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (cell, line, bold, inset) = (ruler.cell(), ruler.large(), ruler.lines().1, ruler.framed_inset());
    let (width, height) = (LARGE * cell + 2 * inset, LARGE * line + 2 * inset);
    let left = (key.x + key.w / 2 - width / 2).clamp(0, (ruler.width - width).max(0));
    let full_area = Rect::new(left, key.y - ruler.gap - height, width, height);
    let paint = |surface: &mut Surface<'_>| {
        let bounds = surface.bounds();
        surface.fill(bounds, roles.background);
        draw_frame(surface, bounds, bold, roles.glow);
        let (at, plain) = ((inset, inset), Lettering::default());
        surface.text(bounds, &ruler.heading, at, character.encode_utf8(&mut [0u8; 4]), roles.glow, LARGE, plain);
    };
    let id = Name::default().word(u64::from(character)).word(ruler.heading.id).number(inset);
    let id = id.colour(roles.glow).colour(roles.background).value();
    let (area, size, bounds) = (mix_rect(key, full_area, growth), (width as usize, height as usize), pen.bounds());
    if area == full_area {
        pen.plate(Canvas::Large as usize, bounds, (area.x, area.y), size, 0, id, paint);
    } else if area.w > 0 && area.h > 0 {
        pen.stretch_canvas(Canvas::Large as usize, (area, bounds), size, id, |words, format| {
            if let Some(mut surface) = Surface::new(words, width as u32, height as u32, width as usize, format) {
                paint(&mut surface);
            }
        });
    }
}

#[derive(Clone, Copy)]
pub(super) struct KeyCaps<'a> {
    pub(super) style: KeysStyle,
    pub(super) icons: &'a [Icon; 4],
}

pub(super) fn draw_keys(pen: &mut Pen<'_, '_>, kit: &Kit<'_>, keyboard: &Keyboard, caps: KeyCaps<'_>) {
    let (ruler, roles) = (kit.ruler, kit.roles);
    let (is_carded, gap, bold) = (caps.style.is_carded, ruler.lines().0, ruler.lines().1);
    let ground = if is_carded { roles.surface() } else { roles.background };
    if is_carded {
        for cap in keyboard.caps() {
            let area = cap.area;
            draw_card(pen, Rect::new(area.x + gap, area.y + gap, area.w - 2 * gap, area.h - 2 * gap), ground);
        }
    }
    draw_caps(pen, kit, keyboard, caps.icons, ground);
    if let Some((cells, selection)) = keyboard.offer_cells() {
        let (cell, line) = (ruler.cell(), ruler.large());
        for (at, (form, area)) in cells.enumerate() {
            pen.fill(area, roles.background);
            let colour = if Some(at) == selection { roles.glow } else { roles.text };
            let (x, y) = area.middle();
            let mut glyph = [0u8; 4];
            let form = form.encode_utf8(&mut glyph);
            pen.text(None, &ruler.heading, (x - cell / 2, y - line / 2), form, colour, 1);
            if Some(at) == selection {
                draw_frame(pen, area, bold, roles.glow);
            }
        }
        return;
    }
    if let Some(large) = keyboard.large() {
        draw_large(pen, kit, large);
    }
}

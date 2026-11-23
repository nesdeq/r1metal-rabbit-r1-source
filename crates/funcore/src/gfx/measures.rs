use super::font::Font;
use crate::base::math::round;

// Sizes and strengths that belong together differ by a power of this.
pub(crate) const PHI: f32 = core::f32::consts::GOLDEN_RATIO;
pub(crate) const PHI2: f32 = PHI * PHI;
pub(crate) const PHI3: f32 = PHI2 * PHI;

// Their tops set the top of the clock's ink.
const CLOCK_DIGITS: &str = "0123456789:";

pub(crate) fn measure(heading: &Font, body: &Font) -> (f32, i32) {
    let band = (heading.height - heading.ink_top(CLOCK_DIGITS) + body.height) as f32;
    (band, heading.width as i32 / 2)
}

pub(crate) fn visible(band: f32) -> f32 {
    band * PHI
}

pub(crate) fn bar(band: f32, gap: i32) -> i32 {
    round(visible(band)) + 2 * gap
}

pub(crate) fn thin_line(band: f32) -> i32 {
    round(band / (PHI3 * PHI3)).max(1)
}

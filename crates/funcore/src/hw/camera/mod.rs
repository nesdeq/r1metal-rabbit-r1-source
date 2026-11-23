//! The camera's registers, modes and image pipeline.

mod align;
mod balance;
pub mod burst;
pub mod camsv;
mod colour;
pub mod develop;
pub mod exposure;
pub mod frame;
mod fusion;
mod genlock;
pub mod measure;
mod merge;
pub mod mode;
mod mosaic;
mod noise;
pub mod raw;
pub mod recording;
pub mod seninf;
mod shading;
pub mod sharpen;
pub mod stream;
pub mod swivel;
pub mod tone;
pub mod video;

const RED: usize = 0;
const GREEN: usize = 1;
const BLUE: usize = 2;
const UNITY: i32 = 1 << FRACTION;
const FRACTION: u32 = 10;
// Linear light in 14 bits: a merge's sixteenths of a code, lifted by the tone map, keep their steps under the curve's.
const LIGHT_STEPS: usize = 1 << 14;
const WARM: &[u8] = b"1";
pub const WARM_BYTES: usize = WARM.len();
/// The error text of a stream ended by a board too warm.
pub const TOO_WARM_TEXT: &str = "the board is too warm for the camera";

/// True where the kernel's camera warmth file says too warm, the kernel leaving the stopping to its reader; a file that could not be read, none, is no reason to stop.
pub fn is_too_warm(word: Option<&[u8]>) -> bool {
    word == Some(WARM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmth_word() {
        assert_eq!(
            [Some(&b"1"[..]), Some(b"0"), Some(b""), None].map(is_too_warm),
            [true, false, false, false],
            "a warmth that cannot be read stops no stream"
        );
    }
}

use super::{BLOCK, END_OF_IMAGE, MARKER};
use alloc::format;
use alloc::string::String;

/// The side of an MCU of a 4:2:0 picture in pixels.
pub const UNIT_PIXELS: u32 = 2 * BLOCK as u32;
const UNIT_BLOCKS: u32 = 6;
/// The quality a photo is coded at, of 100, which the encoder is handed as a code of its own.
pub const QUALITY: u32 = 95;
const FILE_BYTES_PER_PIXEL: usize = 2;
/// Milliseconds before a job counts as stuck; the encoder codes 232 megapixels a second.
pub const JOB_TIMEOUT_MS: u32 = 1000;

/// Bytes of a 4:2:0 picture as the encoder takes it: luma, then chroma of half as many.
pub const fn picture_bytes(pixels: usize) -> usize {
    pixels + pixels / 2
}

/// Bytes of room for the file of a picture.
pub const fn file_bytes(pixels: usize) -> usize {
    FILE_BYTES_PER_PIXEL * pixels
}

/// Blocks a 4:2:0 picture is coded in.
pub const fn blocks(width: u32, height: u32) -> u32 {
    width.div_ceil(UNIT_PIXELS) * height.div_ceil(UNIT_PIXELS) * UNIT_BLOCKS
}

/// MCUs between restart markers: one row, so a reader decodes the rows on all cores at once.
pub const fn restart_interval(width: u32) -> u32 {
    width.div_ceil(UNIT_PIXELS)
}

/// Bytes of the file in `bytes`, up to its last end of image.
pub fn file_end(bytes: &[u8]) -> Option<usize> {
    let end = [MARKER, END_OF_IMAGE];
    bytes.windows(end.len()).rposition(|pair| pair == end).map(|at| at + end.len())
}

/// The error text of bytes without an end of image.
pub fn to_unended_text(bytes: usize) -> String {
    format!("the encoder's {bytes} bytes do not end a picture")
}

/// The bytes the encoder tells it wrote, where the room it was given holds them; else the error text.
pub fn check_size(bytes: usize, room: usize) -> Result<usize, String> {
    if bytes <= room { Ok(bytes) } else { Err(format!("the encoder tells of {bytes} bytes in a room of {room}")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START_OF_IMAGE: u8 = 0xD8;

    #[test]
    fn job_values_and_file_end() {
        assert_eq!(UNIT_PIXELS, 16);
        assert_eq!([picture_bytes(2048 * 2048), picture_bytes(16 * 16)], [6_291_456, 384]);
        assert_eq!(
            [blocks(1440, 1440), blocks(72, 52), blocks(16, 16)],
            [90 * 90 * 6, 5 * 4 * 6, 6],
            "units of 16 pixels, the last ones part outside the picture"
        );
        assert_eq!([restart_interval(1440), restart_interval(72)], [90, 5]);
        assert_eq!(file_end(&[MARKER, END_OF_IMAGE, 7, MARKER, END_OF_IMAGE, 0, 0]), Some(5), "the last end counts");
        assert_eq!([file_end(&[MARKER, START_OF_IMAGE, 1, 2]), file_end(&[])], [None, None]);
        assert_eq!(to_unended_text(7), "the encoder's 7 bytes do not end a picture");
        assert_eq!(
            [check_size(8, 8), check_size(9, 8)],
            [Ok(8), Err("the encoder tells of 9 bytes in a room of 8".into())],
            "a size past the room is no file's, whatever an earlier job left there"
        );
    }
}

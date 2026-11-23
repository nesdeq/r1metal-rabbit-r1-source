//! JPEG pictures: baseline files decoded straight to the size a screen shows them at, how a file's EXIF has a picture stand, and the hardware encoder's jobs.

mod decode;
mod exif;
mod job;

pub use decode::{Columns, Error, Jpeg, Plan};
pub use exif::oriented;
pub(crate) use exif::{MIRRORED, UPRIGHT, shown_size};
pub use job::{
    JOB_TIMEOUT_MS, QUALITY, UNIT_PIXELS, blocks, check_size, file_bytes, file_end, picture_bytes, restart_interval,
    to_unended_text,
};

const MARKER: u8 = 0xFF;
const END_OF_IMAGE: u8 = 0xD9;
const BLOCK: usize = 8;

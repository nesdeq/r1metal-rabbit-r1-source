//! H.264 streams: parameter sets, slice heads and the decoded picture buffer.

pub mod dpb;
pub mod read;
pub mod write;

const SEQUENCE_PARAMETERS: u8 = 7;
const PICTURE_PARAMETERS: u8 = 8;
const IMPORTANCE_SHIFT: u32 = 5;
pub(super) const CHROMA_420: u32 = 1;
pub(crate) const REC_709: u32 = 1;
pub(crate) const MACROBLOCK: u32 = 16;
const CROP_UNIT: u32 = 2;
pub(crate) const SMALLEST_COUNT_BITS: u32 = 4;

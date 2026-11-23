//! Layer 1: formats and helpers that depend on nothing else in the crate.

#[cfg(test)]
pub(crate) mod alloc_count;
pub(crate) mod bits;
pub mod bytes;
pub(crate) mod checksum;
pub mod code;
pub mod crew;
pub(crate) mod fft;
pub mod file_names;
pub mod handoff;
pub mod json;
pub(crate) mod lines;
pub(crate) mod markup;
pub(crate) mod math;
pub mod pixel;
pub mod planes;
pub(crate) mod proto;
pub(crate) mod resample;
pub mod sampling;
pub(crate) mod simd;
pub mod text;
pub mod time;
pub(crate) mod ycbcr;

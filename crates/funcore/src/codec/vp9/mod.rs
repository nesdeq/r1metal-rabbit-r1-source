// VP9 video of profile 0, 8 bits and 4:2:0: frames and superframes decoded into Y, U and V planes.

mod decode;
mod filter;
mod header;
mod inter;
mod intra;
mod modes;
mod probs;
mod tables;
#[cfg(test)]
mod tests;
mod tokens;
mod transform;

pub(crate) use decode::Decoder;

// A group call's member's picture as libvpx 1.17.0 encodes it, three spatial layers of 80x44, 160x88 and 320x176 in each frame: the bench decodes it, the tests hold it to libvpx's pictures.
pub(crate) const SVC: &[u8] = include_bytes!("../../../assets/video/vp9/svc.ivf");

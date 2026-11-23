//! Screen recording over USB: records of the 16 by 16 blocks a frame changed.

mod decoder;
mod encoder;
mod pages;
mod records;
mod timeline;
mod totals;

pub use decoder::{Decoder, Malformed};
pub use encoder::Encoder;
pub use pages::{Job, Pages, Shot, ShotOutcome};
pub use records::Records;
pub use timeline::Timeline;
pub use totals::Totals;

use crate::base::bytes::{le16, le32};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

/// The recording's USB interface: vendor-specific, known by subclass, protocol and name.
pub const USB_CLASS: u8 = 0xff;
pub const USB_SUBCLASS: u8 = b'R';
pub const USB_PROTOCOL: u8 = 1;
pub const USB_INTERFACE_NAME: &str = "r1metal frames";
/// Bytes of a write and a transfer: the kernel's largest cheap allocation, under the controller's 65536.
pub const TRANSFER_BYTES: usize = 8 * 4096;
// A reading host takes a part in under a millisecond; a lagging one has frames passed over meanwhile.
const HOST_TIMEOUT: Duration = Duration::from_secs(5);
const BLOCK: usize = 16;
const CHROMA: usize = BLOCK / 2;
const BLOCK_BYTES: usize = BLOCK * BLOCK + 2 * CHROMA * CHROMA;
const MAGIC: [u8; 4] = *b"R1RC";
const HEAD_BYTES: usize = 24;
const KIND_AT: usize = 4;
const TIME_AT: usize = 8;
const LENGTH_AT: usize = 16;
const SKIPS_AT: usize = 20;
const FRAME_HEAD_BYTES: usize = 8;
const COUNT_AT: usize = 4;
const START_BYTES: usize = 8;
const PLACE_BYTES: usize = 2;
const ENTRY_BYTES: usize = PLACE_BYTES + BLOCK_BYTES;

/// A record's kind, by its head's code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Start = 1,
    Frame = 2,
    End = 3,
}

impl Kind {
    const fn from_code(code: u8) -> Option<Kind> {
        match code {
            1 => Some(Kind::Start),
            2 => Some(Kind::Frame),
            3 => Some(Kind::End),
            _ => None,
        }
    }

    const fn fits(self, length: usize) -> bool {
        match self {
            Kind::Start => length == START_BYTES,
            Kind::Frame => {
                length >= FRAME_HEAD_BYTES
                    && length <= FRAME_HEAD_BYTES + u16::MAX as usize * ENTRY_BYTES
                    && (length - FRAME_HEAD_BYTES).is_multiple_of(ENTRY_BYTES)
            }
            Kind::End => length == 0,
        }
    }
}

/// A start record's body: the frame size and the screen's refresh rate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StartBody {
    pub width: usize,
    pub height: usize,
    pub rate: u16,
}

impl StartBody {
    /// Parses a start's body; none if short, unrecordable or of rate 0.
    pub fn read(body: &[u8]) -> Option<StartBody> {
        let (width, height, rate) = (usize::from(le16(body, 0)?), usize::from(le16(body, 2)?), le16(body, 4)?);
        (body.len() == START_BYTES && is_recordable(width, height) && rate > 0).then_some(StartBody {
            width,
            height,
            rate,
        })
    }
}

/// A record's head; `time_us` runs on the monotonic clock, `skips` counts frames since the last record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Head {
    pub kind: Kind,
    pub time_us: u64,
    length: u32,
    pub skips: u32,
}

impl Head {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&[self.kind as u8, 0, 0, 0]);
        out.extend_from_slice(&self.time_us.to_le_bytes());
        out.extend_from_slice(&self.length.to_le_bytes());
        out.extend_from_slice(&self.skips.to_le_bytes());
    }

    fn read(bytes: &[u8]) -> Option<Head> {
        let head = bytes.get(..HEAD_BYTES).filter(|head| head[..MAGIC.len()] == MAGIC)?;
        let word = |at: usize| le32(head, at);
        let (kind, length) = (Kind::from_code(head[KIND_AT])?, word(LENGTH_AT)?);
        let time_us = u64::from(word(TIME_AT)?) | u64::from(word(TIME_AT + size_of::<u32>())?) << 32;
        let skips = word(SKIPS_AT)?;
        kind.fits(length as usize).then_some(Head { kind, time_us, length, skips })
    }
}

/// Log line of a host's request to record or to end.
pub const fn to_asked_line(is_on: bool) -> &'static str {
    if is_on { "recording: begun" } else { "recording: asked to end" }
}

/// Error text for a screen size that cannot be recorded.
pub fn to_unrecordable_text(width: usize, height: usize) -> String {
    format!("a screen of {width}x{height} is not recordable")
}

/// Error text for a host that stopped reading.
pub fn to_gone_text() -> String {
    format!("the host has taken no part of the recording for {} s", HOST_TIMEOUT.as_secs())
}

/// Wait limit of a reap for at least `least` finished parts; none looks and goes on.
pub const fn to_reap_limit(least: usize) -> Duration {
    if least == 0 { Duration::ZERO } else { HOST_TIMEOUT }
}

/// True for a frame size of whole blocks whose sides and block count fit two bytes.
pub const fn is_recordable(width: usize, height: usize) -> bool {
    width > 0
        && height > 0
        && width <= u16::MAX as usize
        && height <= u16::MAX as usize
        && width.is_multiple_of(BLOCK)
        && height.is_multiple_of(BLOCK)
        && (width / BLOCK) * (height / BLOCK) <= u16::MAX as usize
}

const fn largest_record(width: usize, height: usize) -> usize {
    HEAD_BYTES + FRAME_HEAD_BYTES + (width / BLOCK) * (height / BLOCK) * ENTRY_BYTES
}

/// Transfers kept in flight: a whole frame's record, so the link stays busy while the sender waits for a core.
pub const fn depth(width: usize, height: usize) -> usize {
    largest_record(width, height).div_ceil(TRANSFER_BYTES)
}

fn frame_size(body: &[u8]) -> Option<(usize, usize)> {
    Some((usize::from(le16(body, 0)?), usize::from(le16(body, 2)?)))
}

#[cfg(test)]
mod support {
    use super::{BLOCK, Head, Records};
    use crate::base::pixel::{Color, PixelFormat};
    use alloc::vec::Vec;

    pub(super) const SIDE: usize = 2 * BLOCK;

    pub(super) fn frame(format: PixelFormat, paint: impl Fn(usize, usize) -> (u8, u8, u8)) -> Vec<u32> {
        (0..SIDE * SIDE)
            .map(|at| {
                let (r, g, b) = paint(at % SIDE, at / SIDE);
                format.pack(Color(u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)))
            })
            .collect()
    }

    pub(super) fn records(bytes: &[u8]) -> Vec<(Head, Vec<u8>)> {
        let mut records = Records::default();
        records.push(bytes);
        core::iter::from_fn(|| records.take()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_frame_depth() {
        assert_eq!(largest_record(480, 640), 32 + 1200 * ENTRY_BYTES);
        assert_eq!(depth(480, 640), 15, "463232 bytes in transfers of eight pages");
        assert_eq!(depth(16, 16), 1);
    }

    #[test]
    fn start_body_validation() {
        let body = |width: u16, height: u16, rate: u16| {
            [width, height, rate, 0].iter().flat_map(|value| value.to_le_bytes()).collect::<Vec<u8>>()
        };
        assert_eq!(StartBody::read(&body(480, 640, 60)), Some(StartBody { width: 480, height: 640, rate: 60 }));
        assert_eq!(StartBody::read(&body(480, 640, 60)[..7]), None, "cut short");
        assert_eq!(StartBody::read(&body(481, 640, 60)), None, "no whole blocks");
        assert_eq!(StartBody::read(&body(480, 640, 0)), None, "a screen that never refreshes");
    }

    #[test]
    fn host_wait_and_texts() {
        assert_eq!(
            [to_reap_limit(0), to_reap_limit(1), to_reap_limit(15)],
            [Duration::ZERO, HOST_TIMEOUT, HOST_TIMEOUT],
            "a look for finished parts waits for none, a reap of some as long as a host may lag"
        );
        assert_eq!([to_asked_line(true), to_asked_line(false)], ["recording: begun", "recording: asked to end"]);
        assert_eq!(to_unrecordable_text(481, 640), "a screen of 481x640 is not recordable");
        assert_eq!(to_gone_text(), "the host has taken no part of the recording for 5 s");
    }

    #[test]
    fn recordable_sizes() {
        assert!(is_recordable(480, 640) && is_recordable(16, 65_520));
        assert!(
            !is_recordable(1 << 16, 16) && !is_recordable(16, 1 << 16),
            "a side of 65536 is whole blocks, few of them, and no two bytes tell it"
        );
        assert!(!is_recordable(4096, 4096), "nor do they count its 65536 blocks");
    }
}

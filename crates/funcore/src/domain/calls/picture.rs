use crate::base::pixel::PixelFormat;
use crate::base::planes::Cover;
use crate::base::sampling::{Place, Turn, blend, pick, turn_words};
use crate::base::ycbcr::{Chroma, Colours, WEIGHTS_601};
use crate::codec::vp8::{self, Encoder, Laps, Request, Step, ac_step, quantizer_of};
use crate::codec::{Flaw, picture, vp9};
use crate::domain::camera::{CALL_SIZE, CallPicture};
use crate::gfx::{Picture, to_fill, turned_size};
use crate::hw::camera::exposure::CALL_FRAMES_PER_SECOND;
use crate::net::rtp::extension::{Orientation, Turn as Asked};
use crate::net::rtp::frames::{self, Codec};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

/// A frame of this side's picture as its encoder wrote it, for the far side.
pub struct Frame {
    pub(super) bytes: Vec<u8>,
    pub(super) is_key: bool,
    pub(super) size: (u16, u16),
    pub(super) frame_rate: u8,
    pub(super) sampled: Duration,
    pub(super) orientation: Orientation,
    // The bits a second it was coded at, and the quantizer that rate chose, which the log's figures tell.
    pub(super) rate: u64,
    pub(super) quantizer: u8,
}

// libvpx's one-pass rate control in outline (vp8/encoder/ratectrl.c, `vp8_update_rate_correction_factors`): a frame's bits are taken to fall as its quantizer's step grows, by a factor corrected after every frame by a quarter of what the frame showed (its adjustment limit for a frame coded once, onyx_if.c, `encode_frame_to_data_rate`), a key frame's by a factor of its own.
const CORRECTION: (u64, u64) = (1, 4);
// The quantizer of a frame nothing is known for yet: the middle of the scale of 128.
const FIRST_QUANTIZER: u8 = 64;
// The most a key frame may take: what the call's rate carries in this long, libwebrtc's bound for libvpx (libvpx_vp8_encoder.cc, `MaxIntraTarget`: half of its buffer of 600 ms).
const KEY_SPAN: Duration = Duration::from_millis(300);
// The frames a second of a call's stream, which no rate told of a frame exceeds.
const STREAM_RATE: u64 = CALL_FRAMES_PER_SECOND as u64;
const MICROS: u64 = 1_000_000;

// What the frames before showed of the picture: an inter frame's bits a second times its quantizer's step, and a key frame's bits times its.
#[derive(Default)]
struct Rate {
    inter: Option<u64>,
    key: Option<u64>,
}

impl Rate {
    // The quantizer of the next frame at `rate` bits a second.
    fn quantizer(&self, is_key: bool, rate: u64) -> u8 {
        let (factor, budget) =
            if is_key { (self.key, rate * KEY_SPAN.as_micros() as u64 / MICROS) } else { (self.inter, rate) };
        let step = |factor: u64| u32::try_from(factor.div_ceil(budget.max(1))).unwrap_or(u32::MAX);
        factor.map_or(FIRST_QUANTIZER, |factor| quantizer_of(step(factor)))
    }

    // Takes a coded frame: its kind, its quantizer, its bytes, and the time since the frame before it.
    fn take(&mut self, is_key: bool, quantizer: u8, bytes: usize, span: Duration) {
        let bits = 8 * bytes as u64 * u64::from(ac_step(quantizer));
        let (factor, shown) = if is_key {
            (&mut self.key, bits)
        } else {
            (&mut self.inter, bits * MICROS / (span.as_micros() as u64).max(1))
        };
        let (share, whole) = CORRECTION;
        *factor = Some(factor.map_or(shown, |kept| (kept * (whole - share) + shown * share) / whole));
    }
}

/// This side's pictures coded for the far side: the encoder, and the quantizer that holds its frames to the call's rate.
#[derive(Default)]
pub struct Sender {
    encoder: Encoder,
    rate: Rate,
    // The frame begun: its rate, its quantizer, and its picture's time and orientation.
    begun: Option<(u64, u8, Duration, Orientation)>,
    last: Option<Duration>,
}

impl Sender {
    /// Begins a frame of a picture at `rate` bits a second, a key frame where one is wanted or nothing came before: the encoder, whose steps the caller runs before `finish`; none for what is no picture of a call's.
    pub fn begin(&mut self, picture: &CallPicture, rate: u64, wants_key: bool) -> Option<&mut Encoder> {
        let (width, height) = CALL_SIZE;
        let [luma, blue, red] = &picture.planes;
        let planes =
            picture::Picture { width, height, planes: [luma, blue, red], strides: [width, width / 2, width / 2] };
        let is_key = wants_key || self.last.is_none();
        let quantizer = self.rate.quantizer(is_key, rate);
        self.encoder.begin(&planes, Request { is_key, quantizer }).ok()?;
        let turn = [Asked::None, Asked::Quarter, Asked::Half, Asked::ThreeQuarters][usize::from(picture.quarters % 4)];
        let orientation = Orientation { turn, is_flipped: false, is_back: picture.is_rear };
        self.begun = Some((rate, quantizer, Duration::from_nanos(picture.at_ns), orientation));
        Some(&mut self.encoder)
    }

    /// Ends the frame begun: its bytes, with what the far side is told of it; none where none was begun.
    pub fn finish(&mut self) -> Option<Frame> {
        let (rate, quantizer, sampled, orientation) = self.begun.take()?;
        let coded = self.encoder.finish();
        let span = self.last.map_or(Duration::ZERO, |last| sampled.saturating_sub(last));
        self.rate.take(coded.is_key, quantizer, coded.frame.len(), span);
        self.last = Some(sampled);
        let frame_rate = (MICROS / (span.as_micros() as u64).max(1)).clamp(1, STREAM_RATE) as u8;
        let (width, height) = CALL_SIZE;
        Some(Frame {
            bytes: coded.frame,
            is_key: coded.is_key,
            size: (width as u16, height as u16),
            frame_rate,
            sampled,
            orientation,
            rate,
            quantizer,
        })
    }
}

// VP8's colours where a stream names none: Rec. 601 in the video range (RFC 6386, 9.2, colour space 0), which a VP9 picture is shown in too, whatever its header names.
const COLOURS: Colours = Colours::new(WEIGHTS_601, false);
// A 4:2:0 picture's colour samples: one for two pixels each way.
const CHROMA_SHARE: usize = 2;
// A decoded picture's planes: its light and its two colours.
const PLANES: usize = 3;
// Milliseconds a second, as the log tells a time, and bits a kilobit, as it tells a rate.
const MILLIS: f64 = 1000.0;
const KBIT: u64 = 1000;

/// A frame of the far side's picture as it came, put together and, in a group's call, opened, for its decoder.
pub struct Arrival {
    codec: Codec,
    bytes: Vec<u8>,
    is_key: bool,
    orientation: Option<Orientation>,
    // Whom a key frame is asked of where it does not decode: its side at the agent, and its sender's number on that way.
    sender: (u32, u32),
}

impl Arrival {
    // A frame its stream handed over, of a sender on a side.
    pub(super) fn new(frame: &frames::Frame, sender: (u32, u32)) -> Arrival {
        Arrival {
            codec: frame.codec,
            bytes: frame.join(),
            is_key: frame.is_key,
            orientation: frame.orientation,
            sender,
        }
    }

    pub(super) const fn is_key(&self) -> bool {
        self.is_key
    }

    pub(super) const fn sender(&self) -> (u32, u32) {
        self.sender
    }
}

// How a decoded plane of one size is brought to the size its picture shows at before the turn: each sample the mean of what it covers where the plane is the larger, the mix of the four about its middle where it is the smaller.
enum Fit {
    Shrink(Cover),
    Grow(Vec<Place>, Vec<Place>),
}

impl Fit {
    fn new((width, height): (usize, usize), (across, down): (usize, usize)) -> Fit {
        if across <= width {
            return Fit::Shrink(Cover::new((width, height), (across, down)));
        }
        let places = |pixels: usize, samples: usize| {
            (0..pixels).map(|pixel| Place::new(pixel, pixels, (samples, 1), samples)).collect()
        };
        Fit::Grow(places(across, width), places(down, height))
    }

    // Brings a plane `width` samples a line, a line `stride` bytes from the next, to the size shown, `across` samples a line, in `out`, with the lines a growth mixes in `blends` and the sums down a shrinking takes across in `sums`.
    fn bring(
        &self,
        (plane, stride, width): (&[u8], usize, usize),
        across: usize,
        (blends, sums): (&mut Vec<u16>, &mut Vec<u32>),
        out: &mut [u8],
    ) {
        match self {
            Fit::Shrink(cover) => cover.shrink::<1>(plane, (stride, 0), 0..out.len() / across, (out, sums)),
            Fit::Grow(columns, rows) => {
                blends.resize(width, 0);
                let line = |at: u32| &plane[at as usize * stride..][..width];
                for (row, out) in rows.iter().zip(out.chunks_exact_mut(across)) {
                    blend(blends, line(row.low), line(row.high), row.fraction);
                    pick::<1>(blends, columns, out);
                }
            }
        }
    }
}

// What the far side's pictures are made in: how a decoded picture's light and its colours are brought to the size shown, kept with the two sizes for the next picture of them, its planes at that size before the turn, the lines a growth mixes, the sums down a shrinking takes across, and its words before the turn.
#[derive(Default)]
struct Scratch {
    fits: Option<[Fit; 2]>,
    sizes: ((usize, usize), (usize, usize)),
    planes: [Vec<u8>; PLANES],
    blends: Vec<u16>,
    sums: Vec<u32>,
    unturned: Vec<u32>,
}

impl Scratch {
    // Makes into `picture` the picture shown of a decoded one, in `format`, as large as its shape has within `room` once turned by `turn`: its planes brought to that size, each pixel's word made of them, then the words turned; false for a room of nothing. Every buffer is written whole, so none is cleared first.
    fn make(
        &mut self,
        decoded: &picture::Picture<'_>,
        turn: Turn,
        room: (usize, usize),
        format: PixelFormat,
        picture: &mut Picture,
    ) -> bool {
        let size = (decoded.width, decoded.height);
        let (width, height) = to_fill(turned_size(size, turn.quarters), room);
        if width == 0 || height == 0 {
            return false;
        }
        let shown = turned_size((width, height), turn.quarters);
        let chroma = (size.0.div_ceil(CHROMA_SHARE), size.1.div_ceil(CHROMA_SHARE));
        let Scratch { fits, sizes, planes, blends, sums, unturned } = self;
        let fits = match fits {
            Some(fits) if *sizes == (size, shown) => fits,
            _ => {
                *sizes = (size, shown);
                fits.insert([Fit::new(size, shown), Fit::new(chroma, shown)])
            }
        };
        let widths = [size.0, chroma.0, chroma.0];
        for (kind, out) in planes.iter_mut().enumerate() {
            out.resize(shown.0 * shown.1, 0);
            let plane = (decoded.planes[kind], decoded.strides[kind], widths[kind]);
            fits[usize::from(kind > 0)].bring(plane, shown.0, (blends, sums), out);
        }
        let [luma, blue, red] = planes.each_ref().map(Vec::as_slice);
        let words = &mut picture.words;
        if turn.quarters.is_multiple_of(4) && !turn.is_mirrored {
            words.resize(width * height, 0);
            COLOURS.row(luma, Chroma::Apart(blue, red), format, words);
        } else {
            unturned.resize(width * height, 0);
            COLOURS.row(luma, Chroma::Apart(blue, red), format, unturned);
            turn_words(unturned, shown.0, turn, words);
        }
        picture.width = width;
        true
    }
}

/// The far side's pictures: each frame decoded by its codec's decoder, and the picture shown made of it.
#[derive(Default)]
pub struct Pictures {
    vp8: vp8::Decoder,
    vp9: vp9::Decoder,
    scratch: Scratch,
}

impl Pictures {
    /// Decodes a frame by its codec's decoder, for the picture `Decoded::show` makes of it.
    pub fn decode(&mut self, arrival: Arrival) -> Decoded<'_> {
        let Pictures { vp8, vp9, scratch } = self;
        let picture = match arrival.codec {
            Codec::Vp8 => vp8.decode(&arrival.bytes),
            Codec::Vp9 => vp9.decode(&arrival.bytes),
        };
        Decoded { picture, orientation: arrival.orientation, sender: arrival.sender, scratch }
    }

    // Makes into `picture` the picture shown of a picture decoded already, as `Decoded::show` does: the bench's far picture.
    pub(crate) fn make(
        &mut self,
        decoded: &picture::Picture<'_>,
        turn: Turn,
        (room, format): ((usize, usize), PixelFormat),
        picture: &mut Picture,
    ) -> bool {
        self.scratch.make(decoded, turn, room, format, picture)
    }
}

/// A far frame decoded: its picture, lent until the next frame is decoded, or its flaw.
pub struct Decoded<'a> {
    picture: Result<Option<picture::Picture<'a>>, Flaw>,
    orientation: Option<Orientation>,
    sender: (u32, u32),
    scratch: &'a mut Scratch,
}

impl Decoded<'_> {
    /// Makes into `picture` the picture shown, in `format`, turned by the quarter turns its sender asks (the flip is passed over, as libwebrtc passes it over) and as large as its shape has within `room`: what came of the frame.
    pub fn show(self, room: (usize, usize), format: PixelFormat, picture: &mut Picture) -> Shown {
        let turn =
            Turn { quarters: self.orientation.map_or(0, |orientation| orientation.turn as u8), is_mirrored: false };
        match self.picture {
            Ok(Some(decoded)) if self.scratch.make(&decoded, turn, room, format, picture) => Shown::Picture,
            Ok(_) => Shown::Nothing,
            Err(_) => Shown::Flaw(self.sender),
        }
    }
}

/// What came of a far frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shown {
    // The picture shown, made in the picture handed over.
    Picture,
    // A frame that shows nothing, or a room of nothing.
    Nothing,
    // It did not decode, which leaves the frames after it nothing to stand on: the sender to ask a key frame of.
    Flaw((u32, u32)),
}

// How long work of one kind took: how often, in all, and at the longest.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Spans {
    count: u64,
    sum: Duration,
    longest: Duration,
}

impl Spans {
    fn add(&mut self, span: Duration) {
        (self.count, self.sum, self.longest) = (self.count + 1, self.sum + span, self.longest.max(span));
    }

    // Its mean and its longest, as the log tells them.
    fn to_text(self) -> String {
        let mean = to_millis(self.sum) / self.count.max(1) as f64;
        format!("{mean:.1} ms on the mean and {:.1} ms at the longest", to_millis(self.longest))
    }
}

// A span in milliseconds, as the log tells it.
fn to_millis(span: Duration) -> f64 {
    span.as_secs_f64() * MILLIS
}

/// What a call's pictures cost the device, for the log as they stop: each coding, a key frame's apart from the others', with the rate asked of it, its quantizer, its bytes and each of its steps; the camera's pictures a newer one took the place of before they were coded; each decoding, each picture made of a frame decoded, and the most frames that waited to be decoded at once.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Figures {
    keys: Spans,
    others: Spans,
    // The rates asked and the quantizers of the codings in all, the bytes of every frame after the first, and the first frame's picture time and the last's, which those bytes came in.
    rates: u64,
    quantizers: u64,
    bytes: u64,
    sampled: Option<(Duration, Duration)>,
    steps: Laps,
    replaced: u64,
    decoded: Spans,
    made: Spans,
    most_waiting: usize,
}

impl Figures {
    // A frame coded: how long its coding took, and each of its steps.
    pub(super) fn note_coded(&mut self, frame: &Frame, span: Duration, steps: &[(Step, Duration)]) {
        let kind = if frame.is_key { &mut self.keys } else { &mut self.others };
        kind.add(span);
        (self.rates, self.quantizers) = (self.rates + frame.rate, self.quantizers + u64::from(frame.quantizer));
        match &mut self.sampled {
            Some((_, last)) => (*last, self.bytes) = (frame.sampled, self.bytes + frame.bytes.len() as u64),
            None => self.sampled = Some((frame.sampled, frame.sampled)),
        }
        steps.iter().for_each(|&(step, time)| self.steps.add(step, time));
    }

    pub(super) const fn note_replaced(&mut self) {
        self.replaced += 1;
    }

    // A frame decoded, and the picture made of it where one was.
    pub(super) fn note_decoded(&mut self, decoding: Duration, making: Option<Duration>) {
        self.decoded.add(decoding);
        making.into_iter().for_each(|span| self.made.add(span));
    }

    pub(super) fn note_waiting(&mut self, count: usize) {
        self.most_waiting = self.most_waiting.max(count);
    }

    /// The log's lines of them: the pictures coded and their steps, then the far frames decoded, each where there were any.
    pub fn to_lines(self) -> Vec<String> {
        let (keys, others, decoded, made) = (self.keys, self.others, self.decoded, self.made);
        let coded = keys.count + others.count;
        let span = self.sampled.map_or(Duration::ZERO, |(first, last)| last.saturating_sub(first));
        let bytes = if span.is_zero() { 0.0 } else { self.bytes as f64 / span.as_secs_f64() };
        let lines = [
            (
                coded,
                format!(
                    "calls: {coded} pictures coded in {:.1} ms on the mean at {} kbit/s asked and quantizer {:.1} on the mean, {bytes:.0} bytes a second, {} key frames of them in {:.1} ms at the longest and the others in {:.1} ms at the longest, {} more made by the camera and replaced by a newer one before they were coded",
                    to_millis(keys.sum + others.sum) / coded.max(1) as f64,
                    self.rates / coded.max(1) / KBIT,
                    self.quantizers as f64 / coded.max(1) as f64,
                    keys.count,
                    to_millis(keys.longest),
                    to_millis(others.longest),
                    self.replaced
                ),
            ),
            (coded, format!("calls: the codings' steps on the mean: {}", self.steps.to_text(coded))),
            (
                decoded.count,
                format!(
                    "calls: {} far frames decoded in {}, {} pictures made of them in {}, at most {} frames waiting to be decoded",
                    decoded.count,
                    decoded.to_text(),
                    made.count,
                    made.to_text(),
                    self.most_waiting
                ),
            ),
        ];
        lines.into_iter().filter(|(count, _)| *count > 0).map(|(_, line)| line).collect()
    }
}

#[cfg(test)]
pub(super) mod samples {
    use super::*;
    use alloc::vec;

    // A VP8 frame of the one byte `number` from a sender, a key frame or not.
    pub(in crate::domain::calls) fn arrival(number: u8, is_key: bool, sender: (u32, u32)) -> Arrival {
        Arrival { codec: Codec::Vp8, bytes: vec![number], is_key, orientation: None, sender }
    }

    // The byte such a frame is of.
    pub(in crate::domain::calls) fn number(arrival: &Arrival) -> u8 {
        arrival.bytes[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::sampling::turn_words;
    use crate::codec::ivf;
    use crate::codec::vp8::tests::CALL;
    use crate::codec::vp9::SVC;
    use crate::domain::console::bench;
    use crate::gfx::{Rect, Stretching, Surface};
    use alloc::vec;

    // How far, in levels of a channel's 255, a picture made at its own size may lie on the mean from one made of every decoded pixel's word, turned, then scaled (of_words): a shrunk plane's means are taken over each pixel's own share of the decoded picture where of_words takes whole pixels, up to one more or less at either edge, which moves the textured disc's sharp edges within a pixel; a grown plane is mixed in 64ths where of_words mixes words in quarters, and the colours between their own samples where of_words mixes copies of them a pixel wide, which at nine times the size moves a colour's edge by up to half a decoded pixel.
    const SHRUNK_APART: f64 = 2.0;
    const GROWN_APART: f64 = 8.0;
    // And at a pixel, where those edges cross it: never as far as half the range.
    const MOST_APART: u32 = 128;

    // The bench's picture of `index`, taken at fifteen a second and turned three quarters.
    fn scene(index: u64) -> CallPicture {
        CallPicture {
            planes: bench::scene(index as usize),
            at_ns: index * 1_000_000_000 / 15,
            quarters: 3,
            is_rear: false,
        }
    }

    // Codes the picture of `index` at `rate`, as a key frame where asked.
    fn coded(sender: &mut Sender, index: u64, rate: u64, is_key: bool) -> Frame {
        sender.begin(&scene(index), rate, is_key).expect("a call's picture").code_steps(|_, work| work.in_order());
        sender.finish().expect("the frame begun")
    }

    // Codes `count` pictures from `first` on at `rate`, a key frame at `key`: each frame's kind and bytes.
    fn code(sender: &mut Sender, first: u64, count: u64, rate: u64, key: Option<u64>) -> Vec<(bool, usize)> {
        let frames = (first..first + count).map(|index| {
            let frame = coded(sender, index, rate, key == Some(index));
            assert_eq!(frame.orientation.turn, Asked::ThreeQuarters, "turned as its picture tells");
            (frame.is_key, frame.bytes.len())
        });
        frames.collect()
    }

    #[test]
    fn frames_held_to_rate() {
        let mut sender = Sender::default();
        assert!(sender.finish().is_none() && sender.begin(&CallPicture::default(), 300_000, false).is_none());
        // Fifteen pictures a second at three rates one after another, a key frame asked in the middle of the later two.
        for (first, rate) in [(0, 300_000u64), (30, 1_000_000), (60, 150_000)] {
            let frames = code(&mut sender, first, 30, rate, (first > 0).then_some(first + 15));
            let bits = |frames: &[(bool, usize)], is_key: bool| -> Vec<u64> {
                frames.iter().filter(|frame| frame.0 == is_key).map(|frame| 8 * frame.1 as u64).collect()
            };
            assert_eq!(bits(&frames, true).len(), 1, "one key frame: the first of all, then the one asked for");
            let settled = bits(&frames[20..], false);
            let mean = settled.iter().sum::<u64>() * 15 / settled.len() as u64;
            assert!(mean.abs_diff(rate) < rate / 8, "{mean} bits a second held to {rate} within an eighth");
            assert!(bits(&frames, true)[0] < rate, "a key frame takes less than a second of the rate");
        }
    }

    const RGB: PixelFormat = PixelFormat::XRGB8888;
    const SENDER: (u32, u32) = (2, 2000);

    fn arrival(codec: Codec, bytes: &[u8], orientation: Option<Orientation>) -> Arrival {
        Arrival { codec, bytes: bytes.to_vec(), is_key: false, orientation, sender: SENDER }
    }

    // Decodes a frame and makes its picture in `room`: the picture shown or none, or the sender to ask a key frame of.
    fn take(
        far: &mut Pictures,
        codec: Codec,
        bytes: &[u8],
        orientation: Option<Orientation>,
        room: (usize, usize),
    ) -> Result<Option<Picture>, (u32, u32)> {
        let mut picture = Picture::default();
        match far.decode(arrival(codec, bytes, orientation)).show(room, RGB, &mut picture) {
            Shown::Picture => Ok(Some(picture)),
            Shown::Nothing => Ok(None),
            Shown::Flaw(sender) => Err(sender),
        }
    }

    #[test]
    fn frames_to_fitted_pictures() {
        let frames = ivf::frames(CALL);
        let mut far = Pictures::default();
        let (width, height) = far.vp8.decode(frames[0]).unwrap().map(|own| (own.width, own.height)).unwrap();
        let whole =
            take(&mut far, Codec::Vp8, frames[0], None, (width, height)).unwrap().expect("a key frame shows a picture");
        assert_eq!((whole.width, whole.height()), (width, height), "in a room of its own size, as it is");
        assert!(width > 0 && height > 0 && whole.words.iter().any(|word| *word != whole.words[0]), "a picture");
        let asked = |turn: Asked| Some(Orientation { turn, is_flipped: true, is_back: false });
        let turned = take(&mut far, Codec::Vp8, frames[1], asked(Asked::Quarter), (height, width)).unwrap().unwrap();
        assert_eq!(
            (turned.width, turned.height()),
            (height, width),
            "a quarter turn stands a picture on its side, whatever its flip"
        );
        let grown = take(&mut far, Codec::Vp8, frames[1], None, (2 * width, 3 * height)).unwrap().unwrap();
        assert_eq!(
            (grown.width, grown.height()),
            (2 * width, 2 * height),
            "grown until a side meets its room's, its shape kept"
        );
        assert!(grown.words.iter().any(|word| *word != grown.words[0]), "and a picture still");
        let small = take(&mut far, Codec::Vp8, frames[2], None, (width / 2, height)).unwrap().unwrap();
        assert_eq!(
            (small.width, small.height()),
            (width / 2, height / 2),
            "fitted within its room, its shape kept: {width} by {height}"
        );
        assert!(
            matches!(take(&mut far, Codec::Vp8, frames[3], None, (0, 50)), Ok(None)),
            "a room of nothing shows nothing"
        );
        let broken = take(&mut far, Codec::Vp8, &[0x30, 0x01, 0x00, 0x9D], None, (100, 100));
        assert!(broken == Err(SENDER), "what is no frame is a flaw, a key frame asked of its sender");
        let again = take(&mut far, Codec::Vp8, frames[0], None, (width, height)).unwrap().unwrap();
        assert!(again == whole, "a key frame after a flaw shows its picture as before");
    }

    // A group call's VP9: each picture's three spatial layers, of 80x44, 160x88 and 320x176, in one frame.
    #[test]
    fn vp9_frames_to_fitted_pictures() {
        let (pictures, calls) = (ivf::frames(SVC), ivf::frames(CALL));
        let mut far = Pictures::default();
        let whole = take(&mut far, Codec::Vp9, pictures[0], None, (320, 1000)).unwrap().expect("a key picture shows");
        assert_eq!((whole.width, whole.height()), (320, 176), "the upper layer's picture, whole");
        assert!(whole.words.iter().any(|word| *word != whole.words[0]), "a picture");
        let between = take(&mut far, Codec::Vp8, calls[0], None, (1000, 1000)).unwrap();
        assert!(between.is_some(), "a VP8 frame between, by the decoder of its own");
        let asked = Some(Orientation { turn: Asked::Quarter, is_flipped: false, is_back: false });
        let small = take(&mut far, Codec::Vp9, pictures[1], asked, (100, 160)).unwrap().unwrap();
        assert_eq!(
            (small.width, small.height()),
            (88, 160),
            "the next picture, which leans on the first, turned and fitted within its room"
        );
        let broken = far.decode(arrival(Codec::Vp9, &[0x42, 0, 0, 0], None)).picture.err();
        assert_eq!(broken, Some(Flaw::Invalid("frame marker")), "what is no VP9 frame is a flaw");
    }

    // The reference a picture made at its own size is held near: every decoded pixel a word, the whole picture turned, then shrunk by the mean over whole pixels or grown by mixes in quarters.
    fn of_words(decoded: &picture::Picture<'_>, turn: Turn, room: (usize, usize)) -> Picture {
        let ([luma, blue, red], [luma_stride, blue_stride, red_stride]) = (decoded.planes, decoded.strides);
        let words: Vec<u32> = (0..decoded.width * decoded.height)
            .map(|at| {
                let (x, line) = (at % decoded.width, at / decoded.width);
                let colour = |plane: &[u8], stride: usize| i32::from(plane[line / 2 * stride + x / 2]);
                COLOURS.word(
                    i32::from(luma[line * luma_stride + x]),
                    colour(blue, blue_stride),
                    colour(red, red_stride),
                    RGB,
                )
            })
            .collect();
        let mut turned = Vec::new();
        let across = turn_words(&words, decoded.width, turn, &mut turned);
        let (width, height) = to_fill((across, turned.len() / across), room);
        let mut shown = vec![0; width * height];
        let mut surface = Surface::new(&mut shown, width as u32, height as u32, width, RGB).unwrap();
        let area = Rect::new(0, 0, width as i32, height as i32);
        if width > across {
            surface.stretch(&mut Stretching::default(), area, area, across, &turned);
        } else {
            surface.shrink(area, across, &turned);
        }
        Picture { words: shown, width }
    }

    // The mean and the largest difference of two pictures' channels, the largest over a pixel's mean of its three.
    fn apart(made: &Picture, reference: &Picture) -> (f64, u32) {
        let channels = |word: u32| [word >> 16 & 0xFF, word >> 8 & 0xFF, word & 0xFF];
        let (mut sum, mut largest) = (0u64, 0u32);
        for (&made, &reference) in made.words.iter().zip(&reference.words) {
            let pixel: u32 =
                channels(made).into_iter().zip(channels(reference)).map(|(one, other)| one.abs_diff(other)).sum();
            (sum, largest) = (sum + u64::from(pixel), largest.max(pixel / 3));
        }
        (sum as f64 / (3 * made.words.len()) as f64, largest)
    }

    // A call's picture of 640x480 shown in the far picture's rooms, upright and turned, shrunk; and the clip's, smaller, grown; each made into what held the picture before, as the far pictures' thread makes them.
    #[test]
    fn made_as_words_were() {
        let mut sender = Sender::default();
        let (key, clip) = (coded(&mut sender, 0, 1_000_000, true).bytes, ivf::frames(CALL));
        let (mut decoder, mut scratch, mut made) = (vp8::Decoder::default(), Scratch::default(), Picture::default());
        let cases = [
            (&key[..], (301, 535), SHRUNK_APART),
            (&key[..], (464, 261), SHRUNK_APART),
            (clip[0], (900, 900), GROWN_APART),
        ];
        for ((bytes, room, most_mean), quarters) in
            cases.into_iter().flat_map(|case| [0, 1, 2, 3].map(|quarters| (case, quarters)))
        {
            let decoded = decoder.decode(bytes).unwrap().unwrap();
            let turn = Turn { quarters, is_mirrored: false };
            let mut fresh = Picture::default();
            assert!(
                scratch.make(&decoded, turn, room, RGB, &mut made)
                    && Scratch::default().make(&decoded, turn, room, RGB, &mut fresh)
                    && made == fresh,
                "made into the picture before as into a new one"
            );
            let reference = of_words(&decoded, turn, room);
            assert_eq!(
                (made.width, made.height()),
                (reference.width, reference.height()),
                "the size of the reference's"
            );
            let (mean, largest) = apart(&made, &reference);
            assert!(
                mean <= most_mean && largest <= MOST_APART,
                "{} wide turned {quarters} into {room:?}: {mean:.2} apart on the mean, {largest} at the most",
                decoded.width
            );
        }
    }

    #[test]
    fn figures_to_lines() {
        let mut figures = Figures::default();
        assert!(figures.to_lines().is_empty(), "no picture, no line");
        let millis = Duration::from_millis;
        figures.note_decoded(millis(8), Some(millis(2)));
        figures.note_decoded(millis(12), None);
        figures.note_waiting(3);
        figures.note_waiting(1);
        assert_eq!(
            figures.to_lines(),
            [
                "calls: 2 far frames decoded in 10.0 ms on the mean and 12.0 ms at the longest, 1 pictures made of them in 2.0 ms on the mean and 2.0 ms at the longest, at most 3 frames waiting to be decoded"
            ],
            "frames decoded alone: a picture made of one, the other not shown"
        );
        let coded = |bytes: usize, is_key: bool, at: u64, (rate, quantizer): (u64, u8)| Frame {
            bytes: vec![0; bytes],
            is_key,
            size: (640, 480),
            frame_rate: 30,
            sampled: millis(at),
            orientation: Orientation { turn: Asked::None, is_flipped: false, is_back: false },
            rate,
            quantizer,
        };
        let steps = |code: u64| {
            let micros = [
                (Step::Interpolations, 100),
                (Step::Bands, 600),
                (Step::Code, code),
                (Step::Counts, 50),
                (Step::Probabilities, 100),
                (Step::Ends, 2700),
            ];
            micros.map(|(step, time)| (step, Duration::from_micros(time)))
        };
        figures.note_coded(&coded(3000, true, 0, (300_000, 40)), millis(40), &steps(3000));
        figures.note_coded(&coded(1000, false, 100, (310_000, 30)), millis(20), &steps(2100));
        figures.note_coded(&coded(1500, false, 200, (320_000, 29)), Duration::from_micros(25_500), &steps(2100));
        figures.note_replaced();
        assert_eq!(
            figures.to_lines()[..2],
            [
                "calls: 3 pictures coded in 28.5 ms on the mean at 310 kbit/s asked and quantizer 33.0 on the mean, 12500 bytes a second, 1 key frames of them in 40.0 ms at the longest and the others in 25.5 ms at the longest, 1 more made by the camera and replaced by a newer one before they were coded",
                "calls: the codings' steps on the mean: interpolations 100 us, bands 600 us, code 2400 us, counts 50 us, probabilities 100 us, ends 2700 us"
            ],
            "the codings first: the rate asked, the quantizer, the bytes after the first frame over its picture time to the last's, each kind's longest, the camera's pictures never coded, and each step"
        );
    }
}

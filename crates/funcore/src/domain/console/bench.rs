//! The `bench` verb's work, which the shell times case by case: a call's pictures coded and decoded, the camera's pictures of its raw frames, and each vector kernel against its plain body.

use crate::base::crew::{Part, Work, in_order};
use crate::base::math::xorshift32;
use crate::base::pixel::PixelFormat;
use crate::base::planes::{self, part_lines};
use crate::base::sampling::Turn;
use crate::base::simd::{self, BLOCK, PAIR, Sample, plain};
use crate::base::ycbcr::{Chroma, Colours, WEIGHTS_601};
use crate::codec::picture::Picture;
use crate::codec::vp8::frame::{MACROBLOCK, SUBBLOCK};
use crate::codec::vp8::inter::{AFTER, BEFORE, HALF_EIGHTHS};
use crate::codec::vp8::tables::SIXTAP_FILTERS;
use crate::codec::vp8::{self, Encoder, Laps, Request, Step};
use crate::codec::{ivf, vp9};
use crate::domain::calls;
use crate::domain::camera::{CALL_SIZE, Viewfinder};
use crate::domain::gallery::Kind;
use crate::gfx::{self, bands, to_fill, turned_size};
use crate::hw::camera::exposure::Auto;
use crate::hw::camera::measure::Measure;
use crate::hw::camera::mode::Mode;
use crate::hw::camera::raw::Finder;
use crate::hw::camera::recording;
use crate::hw::camera::swivel::{Facing, viewfinder_turn};
use crate::hw::camera::tone::Tone;
use crate::hw::camera::video::{self, Planes, Scratch};
use crate::hw::{FRAME_PARTS, LARGE_CORES, PARTS};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::hint::black_box;
use core::ops::Range;
use core::time::Duration;

// Two seconds of a call's pictures at the 30 a second it is to reach.
const FRAMES: usize = 60;
/// The turns each of two threads takes at parking until the other wakes it, as many as the pictures each coding times.
pub const WAKES: usize = FRAMES;
// The quantizer at which the bench's scene codes to a call's rate, 300 kbit/s at 30 pictures a second: the bytes the bench tells for its codings show it.
const QUANTIZER: u8 = 28;
// The quarter of a macroblock a split's search takes.
const QUARTER: usize = MACROBLOCK / 2;
// VP8's six taps of half a pixel, which reach BEFORE pixels before a block's first and AFTER after its last.
const TAPS: [i32; 6] = SIXTAP_FILTERS[HALF_EIGHTHS];
// The hundredths a speed-up is told in.
const PERCENT: u64 = 100;
// The kernel tells a clock in kHz, a line in MHz.
const KHZ_PER_MHZ: u32 = 1000;
// The camera's mode in a call, whose raw frames a call's pictures are made of, and a raw frame's bytes by a formula: a step for each byte and another every few, which reaches every code the conversions' tables hold.
const CALL_MODE: Mode = Mode::HighSpeed;
const RAW_STEPS: (usize, usize) = (31, 7);
// The photo viewfinder as the camera draws it on the R1's screen, 480 pixels wide: the rear camera's preview frame, its middle square a square as wide.
const VIEWFINDER: Viewfinder =
    Viewfinder { kind: Kind::Photo, facing: Facing::Rear, is_scanning: false, is_calling: false, size: (480, 480) };
// The colours of the far side's picture, VP8's where a stream names none (RFC 6386, 9.2), and the turn of a call's picture shown on its side.
const COLOURS: Colours = Colours::new(WEIGHTS_601, false);
const SIDEWAYS: Turn = Turn { quarters: 1, is_mirrored: false };
// Rooms the interface lays out for the far side's picture (`Ui::far_picture_room`, in `ui`, which `domain` stands below), each with the picture upright and turned a quarter.
const FAR_CASES: [((usize, usize), Turn, &str); 4] = [
    ((301, 535), Turn { quarters: 0, is_mirrored: false }, "upright"),
    ((301, 535), SIDEWAYS, "turned a quarter"),
    ((464, 321), Turn { quarters: 0, is_mirrored: false }, "upright"),
    ((464, 321), SIDEWAYS, "turned a quarter"),
];

// A call's pictures as the bench makes them: each picture's Y, U and V planes.
type Pictures = [[Vec<u8>; 3]];

/// Which body of a kernel a case runs: the plain one, or the one the build takes, NEON on aarch64.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Body {
    Plain,
    Vector,
}

/// What the shell times, one case after another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Case {
    Encode(Scene),
    DecodeVp8,
    DecodeVp9,
    // A picture of a raw frame made on one thread.
    Picture(Conversion),
    // The far side's picture made of the bench's decoded frame on one thread, as `FAR_CASES` has it, by its place there.
    FarPicture(usize),
    // A kernel of `KERNELS`, by its place there.
    Kernel(usize, Body),
}

/// What the bench's pictures show: its scene, or the scene with a modelled sensor's noise in every sample.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scene {
    Clean,
    Noisy,
}

/// The scenes, in the order the lines tell their codings; the noisy one is coded first, so that the clean one's frames are those decoded.
pub const SCENES: [Scene; 2] = [Scene::Clean, Scene::Noisy];

impl Scene {
    // The planes of its picture `index`.
    fn picture(self, index: usize) -> [Vec<u8>; 3] {
        match self {
            Scene::Clean => scene(index),
            Scene::Noisy => noisy(index),
        }
    }

    // What its lines call its pictures.
    fn name(self) -> String {
        let (width, height) = CALL_SIZE;
        let noise = match self {
            Scene::Clean => "",
            Scene::Noisy => " with modelled sensor noise",
        };
        format!("{width}x{height}{noise}")
    }
}

/// A picture the camera makes of its raw frames, which the bench makes as the camera does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Conversion {
    Call,
    Viewfinder,
    Recording,
}

/// The conversions, in the order the cases make them and the lines tell them.
pub const CONVERSIONS: [Conversion; 3] = [Conversion::Call, Conversion::Viewfinder, Conversion::Recording];

impl Conversion {
    // The mode whose raw frames its pictures are made of.
    fn mode(self) -> Mode {
        match self {
            Conversion::Call => CALL_MODE,
            Conversion::Viewfinder => VIEWFINDER.mode(),
            Conversion::Recording => recording::MODE,
        }
    }

    // The parts the camera makes a picture in: one for each of the chip's cores, or a viewfinder's one for each small core.
    const fn parts(self) -> usize {
        match self {
            Conversion::Call | Conversion::Recording => PARTS,
            Conversion::Viewfinder => FRAME_PARTS,
        }
    }

    // What its line calls its pictures: the words before their size and after it.
    const fn name(self) -> [&'static str; 2] {
        match self {
            Conversion::Call => ["call picture", "of a raw frame"],
            Conversion::Viewfinder => ["photo viewfinder", "of a preview frame"],
            Conversion::Recording => ["recording", "of a slim video frame"],
        }
    }
}

/// Where a case's thread was as the case ended: its core, and the clocks of the large cores and of the small ones in kHz, as the kernel tells them.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Place {
    pub core: Option<usize>,
    pub clocks: [Option<u32>; 2],
}

impl Place {
    // Where the thread was, as a line tells it: its core, at its cluster's clock.
    fn to_text(self) -> String {
        let Some(core) = self.core else {
            return String::from("on a core not told");
        };
        let clock = self.clocks[usize::from(!LARGE_CORES.contains(&core))];
        clock.map_or_else(|| format!("on core {core}"), |khz| format!("on core {core} at {} MHz", khz / KHZ_PER_MHZ))
    }
}

/// How long a case took, and where its thread was.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Timing {
    pub time: Duration,
    pub place: Place,
}

// A kernel the bench times: the words of its name before and after the size of the blocks or the picture it works over, that size's sides, and its work over every block of the pictures by either body, which gives a sum of its results so that the work is done.
struct Kernel {
    name: [&'static str; 2],
    size: &'static [usize],
    run: fn(&Pictures, Body) -> u64,
}

impl Kernel {
    // What its line calls it.
    fn to_name(&self) -> String {
        let ([before, after], sides) = (self.name, self.size.iter().map(|side| format!("{side}")));
        format!("{before}{}{after}", sides.collect::<Vec<String>>().join("x"))
    }
}

// The kernels the bench times; another is one entry here.
const KERNELS: [Kernel; 12] = [
    Kernel { name: ["sad ", ""], size: &[MACROBLOCK, MACROBLOCK], run: sads },
    Kernel { name: ["sad ", " against a mean"], size: &[QUARTER, QUARTER], run: mean_sads },
    Kernel { name: ["satd ", ""], size: &[MACROBLOCK, MACROBLOCK], run: satds },
    Kernel { name: ["satd ", ""], size: &[SUBBLOCK, SUBBLOCK], run: subblock_satds },
    Kernel { name: ["six taps ", " across"], size: &[MACROBLOCK, MACROBLOCK], run: filters_across },
    Kernel { name: ["six taps ", " down"], size: &[MACROBLOCK, MACROBLOCK], run: filters_down },
    Kernel { name: ["", " bytes to lanes of columns and back"], size: &[BLOCK, BLOCK], run: block_columns },
    Kernel { name: ["words of ", ""], size: &[CALL_SIZE.0, CALL_SIZE.1], run: words },
    Kernel { name: ["", " words turned a quarter"], size: &[CALL_SIZE.0, CALL_SIZE.1], run: turns },
    Kernel { name: ["", " bytes turned a quarter"], size: &[CALL_SIZE.0, CALL_SIZE.1], run: byte_turns },
    Kernel { name: ["", " pairs turned a quarter"], size: &[CALL_SIZE.0 / PAIR, CALL_SIZE.1], run: pair_turns },
    Kernel { name: ["10-bit codes unpacked, ", " a line"], size: &[LINE_CODES], run: unpacks },
];
// A line of the recording's frames, which the kernel that unpacks is timed over: its codes, and the bytes they are packed in.
const LINE_CODES: usize = recording::MODE.frame().width() as usize;
const LINE_BYTES: usize = recording::MODE.frame().line_bytes() as usize;

/// The cases in the order they run: the frames coded are those decoded, and each kernel's plain body goes just before its other.
pub fn cases() -> impl Iterator<Item = Case> {
    let kernels =
        (0..KERNELS.len()).flat_map(|index| [Case::Kernel(index, Body::Plain), Case::Kernel(index, Body::Vector)]);
    let pictures = CONVERSIONS.map(Case::Picture);
    let far = (0..FAR_CASES.len()).map(Case::FarPicture);
    let codings = [Case::Encode(Scene::Noisy), Case::Encode(Scene::Clean), Case::DecodeVp8, Case::DecodeVp9];
    codings.into_iter().chain(pictures).chain(far).chain(kernels)
}

// The places of the blocks of `side` of a call's picture whose reach of `before` and `after` lies inside it.
fn places(side: usize, (before, after): (usize, usize)) -> impl Iterator<Item = (usize, usize)> {
    let (width, height) = CALL_SIZE;
    let across = (before..width - after - side).step_by(side);
    (before..height - after - side).step_by(side).flat_map(move |y| across.clone().map(move |x| (x, y)))
}

// Each picture's luma with the next one's, as a search compares a picture with the one before.
fn pairs(pictures: &Pictures) -> impl Iterator<Item = (&[u8], &[u8])> {
    pictures.iter().zip(pictures.iter().cycle().skip(1)).map(|(first, second)| (&first[0][..], &second[0][..]))
}

fn sads(pictures: &Pictures, body: Body) -> u64 {
    let stride = CALL_SIZE.0;
    let kernel = if body == Body::Plain { plain::sad } else { simd::sad };
    let mut sum = 0;
    for (first, second) in pairs(pictures) {
        for (x, y) in places(MACROBLOCK, (0, 1)) {
            let (a, b) = (&first[y * stride + x..], &second[(y + 1) * stride + x + 1..]);
            sum += u64::from(kernel((a, stride), (b, stride), (MACROBLOCK, MACROBLOCK), u32::MAX));
        }
    }
    sum
}

fn mean_sads(pictures: &Pictures, body: Body) -> u64 {
    let stride = CALL_SIZE.0;
    let kernel = if body == Body::Plain { plain::sad_mean } else { simd::sad_mean };
    let mut sum = 0;
    for (first, second) in pairs(pictures) {
        for (x, y) in places(QUARTER, (0, 1)) {
            let a = (&first[y * stride + x..], stride);
            let (b, c) = ((&second[y * stride + x + 1..], stride), (&second[(y + 1) * stride + x..], stride));
            sum += u64::from(kernel(a, b, c, (QUARTER, QUARTER), u32::MAX));
        }
    }
    sum
}

// The transformed differences of each block of `side` of each picture's luma against the next one's.
fn transforms(pictures: &Pictures, body: Body, side: usize) -> u64 {
    let stride = CALL_SIZE.0;
    let kernel = if body == Body::Plain { plain::satd } else { simd::satd };
    let mut sum = 0;
    for (first, second) in pairs(pictures) {
        for (x, y) in places(side, (0, 0)) {
            let (a, b) = (&first[y * stride + x..], &second[y * stride + x..]);
            sum += u64::from(kernel((a, stride), (b, stride), (side, side)));
        }
    }
    sum
}

fn satds(pictures: &Pictures, body: Body) -> u64 {
    transforms(pictures, body, MACROBLOCK)
}

fn subblock_satds(pictures: &Pictures, body: Body) -> u64 {
    transforms(pictures, body, SUBBLOCK)
}

// The six taps over each block of each picture, `step` apart: along the lines, or down the columns.
fn filters(pictures: &Pictures, body: Body, step: usize) -> u64 {
    let stride = CALL_SIZE.0;
    let kernel = if body == Body::Plain { plain::filter::<{ TAPS.len() }> } else { simd::filter::<{ TAPS.len() }> };
    let mut out = [0u8; MACROBLOCK * MACROBLOCK];
    let mut sum = 0;
    for picture in pictures {
        for (x, y) in places(MACROBLOCK, (BEFORE, AFTER)) {
            let first = if step == 1 { y * stride + x - BEFORE } else { (y - BEFORE) * stride + x };
            kernel((&picture[0][first..], stride), step, &TAPS, (&mut out, MACROBLOCK), (MACROBLOCK, MACROBLOCK));
            sum += u64::from(out[MACROBLOCK * MACROBLOCK - 1]);
        }
    }
    sum
}

fn filters_across(pictures: &Pictures, body: Body) -> u64 {
    filters(pictures, body, 1)
}

fn filters_down(pictures: &Pictures, body: Body) -> u64 {
    filters(pictures, body, CALL_SIZE.0)
}

// A block's columns as lanes, and the lanes put back as its columns.
type Columns =
    (fn(&[u8], usize) -> [[i16; BLOCK]; BLOCK], fn(&[[i16; BLOCK]; BLOCK], (&mut [u8], usize), Range<usize>));

// Each block of BLOCK lines of BLOCK of each picture's luma taken as lanes of its columns and put back, as a loop filter takes and puts back its segments along lines.
fn block_columns(pictures: &Pictures, body: Body) -> u64 {
    let stride = CALL_SIZE.0;
    let (columns, put): Columns =
        if body == Body::Plain { (plain::columns, plain::put_columns) } else { (simd::columns, simd::put_columns) };
    let mut plane = vec![0u8; CALL_SIZE.0 * CALL_SIZE.1];
    let mut sum = 0;
    for picture in pictures {
        plane.copy_from_slice(&picture[0]);
        for (x, y) in places(BLOCK, (0, 0)) {
            let lanes = columns(&plane[y * stride + x..], stride);
            put(&lanes, (&mut plane[y * stride + x..], stride), 0..BLOCK);
            sum += lanes[BLOCK - 1][0] as u64;
        }
    }
    sum
}

// Each picture's words, as the far side's picture's are made: its own light, and the light of it and of the next standing for its blue and red.
fn words(pictures: &Pictures, body: Body) -> u64 {
    let kernel = if body == Body::Plain { plain::words } else { simd::words };
    let mut out = vec![0u32; CALL_SIZE.0 * CALL_SIZE.1];
    let mut sum = 0;
    for (first, second) in pairs(pictures) {
        kernel(&COLOURS, first, Chroma::Apart(first, second), PixelFormat::XRGB8888, &mut out);
        sum += u64::from(out[out.len() - 1]);
    }
    sum
}

// The first picture's light as words, turned once for each picture as a call's picture shown on its side is.
fn turns(pictures: &Pictures, body: Body) -> u64 {
    let kernel = if body == Body::Plain { plain::turn::<u32> } else { <u32 as Sample>::turn };
    let words: Vec<u32> =
        pictures.first().map_or(Vec::new(), |picture| picture[0].iter().map(|&light| light.into()).collect());
    let mut out = vec![0u32; words.len()];
    let mut sum = 0;
    for _ in pictures {
        kernel(&words, CALL_SIZE.0, SIDEWAYS, &mut out);
        sum += u64::from(out.last().copied().unwrap_or(0));
    }
    sum
}

// Each picture's light turned a quarter, as a recording's light shown on its side is.
fn byte_turns(pictures: &Pictures, body: Body) -> u64 {
    let kernel = if body == Body::Plain { plain::turn::<u8> } else { <u8 as Sample>::turn };
    let mut out = vec![0u8; CALL_SIZE.0 * CALL_SIZE.1];
    let mut sum = 0;
    for picture in pictures {
        kernel(&picture[0], CALL_SIZE.0, SIDEWAYS, &mut out);
        sum += u64::from(out.last().copied().unwrap_or(0));
    }
    sum
}

// Each picture's light taken for packed codes, as a recording's frames hold them, and unpacked a line at a time, as a recording's planes are made.
fn unpacks(pictures: &Pictures, body: Body) -> u64 {
    let kernel = if body == Body::Plain { plain::unpack } else { simd::unpack };
    let mut codes = [0u16; LINE_CODES];
    let mut sum = 0;
    for picture in pictures {
        for line in picture[0].as_chunks::<LINE_BYTES>().0 {
            kernel(line, &mut codes);
            sum += u64::from(codes[LINE_CODES - 1]);
        }
    }
    sum
}

// Each picture's light taken a pair of samples at a time and turned a quarter, as a recording's colours shown on its side are.
fn pair_turns(pictures: &Pictures, body: Body) -> u64 {
    let kernel = if body == Body::Plain { plain::turn::<[u8; PAIR]> } else { <[u8; PAIR] as Sample>::turn };
    let mut out = vec![[0u8; PAIR]; CALL_SIZE.0 * CALL_SIZE.1 / PAIR];
    let mut sum = 0;
    for picture in pictures {
        kernel(picture[0].as_chunks().0, CALL_SIZE.0 / PAIR, SIDEWAYS, &mut out);
        sum += u64::from(out.last().map_or(0, |pair| pair[0]));
    }
    sum
}

// What the camera's pictures are made of and into, as the camera makes them: a raw frame of each conversion's mode, by `CONVERSIONS`' order; the planes of a call's picture and of a recording, the viewfinder's finder and the recipe all go by; a call's picture apart, sized as it is first cut, the viewfinder's words, a recording's luma and pairs; and a scratch and a measure for each part.
struct Camera {
    raws: [Vec<u8>; CONVERSIONS.len()],
    call: Option<Planes>,
    finder: Option<Finder>,
    recording: Option<Planes>,
    tone: Tone,
    call_picture: [Vec<u8>; 3],
    words: Vec<u32>,
    recorded: [Vec<u8>; 2],
    scratches: [Scratch; PARTS],
    measures: [Measure; FRAME_PARTS],
}

impl Default for Camera {
    fn default() -> Camera {
        let (step, group) = RAW_STEPS;
        let raws = CONVERSIONS.map(|conversion| {
            (0..conversion.mode().frame().bytes()).map(|at| (at * step + at / group) as u8).collect()
        });
        let tone = Tone::new(Auto::live().recipe());
        let call = Planes::tight(CALL_MODE.frame(), &tone)
            .map(|planes| planes.with_rec_601(&tone).with_lens(CALL_MODE.lens()));
        let (mode, (width, height)) = (VIEWFINDER.mode(), VIEWFINDER.frame_size());
        let turn = viewfinder_turn(VIEWFINDER.facing);
        let finder = Finder::new(mode.frame(), VIEWFINDER.view(), width, height, turn, PixelFormat::XRGB8888)
            .map(|finder| finder.with_lens(mode.lens()));
        let frame = recording::MODE.frame();
        let stride = recording::settings(frame).map_or(0, |settings| settings.shape.stride());
        let recording = Planes::new(frame, stride, &tone).map(|planes| planes.with_lens(recording::MODE.lens()));
        let (stride, lines) = recording.as_ref().map_or((0, 0), Planes::size);
        Camera {
            raws,
            call,
            finder,
            recording,
            tone,
            call_picture: Default::default(),
            words: vec![0; width * height],
            recorded: [vec![0; stride * lines], vec![0; stride * lines / 2]],
            scratches: Default::default(),
            measures: [Measure::default(); FRAME_PARTS],
        }
    }
}

impl Camera {
    // The size of a conversion's pictures: the samples of a line, and the lines.
    fn size(&self, conversion: Conversion) -> (usize, usize) {
        let size = match conversion {
            Conversion::Call => self.call.as_ref().map(Planes::size),
            Conversion::Viewfinder => self.finder.as_ref().map(Finder::size),
            Conversion::Recording => self.recording.as_ref().map(Planes::size),
        };
        size.unwrap_or_default()
    }

    // Makes a call's picture of the raw frame as many times as the pictures coded, each in `count` parts of whole line pairs handed to `run`: a sample of each picture, summed.
    fn make_call(&mut self, count: usize, run: &mut impl FnMut(Vec<Part<'_>>)) -> u64 {
        let Some(planes) = self.call.as_ref() else {
            return 0;
        };
        let (size, raw) = (planes.size(), &self.raws[Conversion::Call as usize][..]);
        let part = part_lines(size.1, count);
        let mut sum = 0;
        for _ in 0..FRAMES {
            let parts = planes::parts_apart(&mut self.call_picture, size, part).zip(self.scratches.iter_mut());
            run(parts
                .map(|((lines, luma, colours), scratch)| {
                    Box::new(move || planes.convert(raw, lines, luma, video::Chroma::Apart(colours), scratch))
                        as Part<'_>
                })
                .collect());
            sum += u64::from(self.call_picture[0][size.0 * size.1 / 2]);
        }
        sum
    }

    // Draws the viewfinder's picture of the raw frame as many times, each in `count` bands of whole lines handed to `run`, each band measured: a word of each picture and the quads measured, summed.
    fn draw(&mut self, count: usize, run: &mut impl FnMut(Vec<Part<'_>>)) -> u64 {
        let Some(finder) = self.finder.as_ref() else {
            return 0;
        };
        let (raw, tone, width) = (&self.raws[Conversion::Viewfinder as usize][..], &self.tone, finder.size().0);
        let mut sum = 0;
        for _ in 0..FRAMES {
            let parts = bands(&mut self.words, width, count).zip(self.measures.iter_mut());
            run(parts
                .map(|((lines, band), measure)| {
                    Box::new(move || *measure = finder.draw_lines(raw, tone, lines, band)) as Part<'_>
                })
                .collect());
            let quads: u32 = self.measures.iter().map(Measure::quads).sum();
            sum += u64::from(self.words[self.words.len() / 2]) + u64::from(quads);
        }
        sum
    }

    // Makes a recording's planes of the raw frame as many times, each in `count` parts of whole line pairs handed to `run`: a sample of each picture, summed.
    fn record(&mut self, count: usize, run: &mut impl FnMut(Vec<Part<'_>>)) -> u64 {
        let Some(planes) = self.recording.as_ref() else {
            return 0;
        };
        let ((stride, lines), raw) = (planes.size(), &self.raws[Conversion::Recording as usize][..]);
        let mut sum = 0;
        for _ in 0..FRAMES {
            let [luma, chroma] = self.recorded.each_mut();
            let parts = planes::parts(luma, chroma, stride, part_lines(lines, count)).zip(self.scratches.iter_mut());
            run(parts
                .map(|((lines, luma, chroma), scratch)| {
                    Box::new(move || planes.convert(raw, lines, luma, video::Chroma::Pairs(chroma), scratch))
                        as Part<'_>
                })
                .collect());
            sum += u64::from(self.recorded[0][stride * lines / 2]);
        }
        sum
    }
}

// The far side's picture as the far pictures' thread makes it: the bench's first picture coded as a key frame and decoded, its planes and their strides; what it is made in; and the picture made.
struct Far {
    decoded: Option<([Vec<u8>; 3], [usize; 3])>,
    pictures: calls::Pictures,
    picture: gfx::Picture,
}

impl Default for Far {
    fn default() -> Far {
        let (width, height) = CALL_SIZE;
        let [luma, blue, red] = scene(0);
        let picture = Picture { width, height, planes: [&luma, &blue, &red], strides: [width, width / 2, width / 2] };
        let mut encoder = Encoder::default();
        let frame = encoder.begin(&picture, Request { is_key: true, quantizer: QUANTIZER }).map(|()| {
            encoder.code_steps(|_, work| work.in_order());
            encoder.finish().frame
        });
        let mut decoder = vp8::Decoder::default();
        let decoded = frame.ok().and_then(|frame| decoder.decode(&frame).ok().flatten());
        Far {
            decoded: decoded.map(|decoded| (decoded.planes.map(<[u8]>::to_vec), decoded.strides)),
            pictures: calls::Pictures::default(),
            picture: gfx::Picture::default(),
        }
    }
}

impl Far {
    // Makes the far side's picture of the decoded frame as many times as the pictures coded, in the room and turned as case `index` of `FAR_CASES` has it: a word of each picture, summed.
    fn make(&mut self, index: usize) -> u64 {
        let Far { decoded, pictures, picture } = self;
        let (Some((planes, strides)), Some(&(room, turn, _))) = (decoded.as_ref(), FAR_CASES.get(index)) else {
            return 0;
        };
        let (width, height) = CALL_SIZE;
        let [luma, blue, red] = planes.each_ref().map(Vec::as_slice);
        let decoded = Picture { width, height, planes: [luma, blue, red], strides: *strides };
        let mut sum = 0;
        for _ in 0..FRAMES {
            if pictures.make(&decoded, turn, (room, PixelFormat::XRGB8888), picture) {
                sum += u64::from(picture.words[picture.words.len() / 2]);
            }
        }
        sum
    }
}

/// The pictures and streams the cases work on, made before any is timed.
pub struct Bench {
    // Each scene's pictures, by `SCENES`' order, and the frames the coding last coded.
    pictures: [Vec<[Vec<u8>; 3]>; SCENES.len()],
    frames: Vec<Vec<u8>>,
    camera: Camera,
    far: Far,
    // The bytes of each coding of each scene's pictures, in the order they ran.
    coded: [Vec<usize>; SCENES.len()],
    // The size of the group call's VP9 pictures, its upper layer's, as their decoding gave it.
    vp9_size: (usize, usize),
    // What the cases gave, kept so that their work is done.
    sums: Vec<u64>,
}

// The scene the call's tests code: a gradient from its floor up across and down; a disc of a radius, lifted, with a texture of steps; grain of values about nought from a seed; colour tilted along each line; the disc's centre from a share of the picture on, by a share each picture.
const GRADIENT: (i64, i64, i64) = (40, 120, 60);
const DISC: (i64, i64, i64, i64) = (120, 50, 15, 6);
const GRAIN: (u32, u32) = (0x2545_F491, 9);
const TILT: ([i64; 2], usize, i64) = ([3, -2], 320, 64);
const CENTRE: ((usize, usize), (usize, usize)) = ((8, 64), (24, 96));
const NEUTRAL: i64 = 128;

// The planes of picture `index` of a call's size, as a camera's picture of a call has them.
pub(crate) fn scene(index: usize) -> [Vec<u8>; 3] {
    let (width, height) = CALL_SIZE;
    let ((floor, across, down), (radius, lift, texture, step), (seed, grain)) = (GRADIENT, DISC, GRAIN);
    let mut state = seed ^ index as u32;
    let ((first_x, share_x), (first_y, share_y)) = CENTRE;
    let (centre_x, centre_y) =
        ((width * (first_x + index) / share_x) as i64, (height * (first_y + index) / share_y) as i64);
    let luma = (0..width * height).map(|at| {
        state = xorshift32(state);
        let (x, y) = ((at % width) as i64, (at / width) as i64);
        let (dx, dy) = (x - centre_x, y - centre_y);
        let disc = if dx * dx + dy * dy < radius * radius { lift + ((dx ^ dy) & texture) * step } else { 0 };
        let noise = i64::from(state % grain) - i64::from(grain / 2);
        (floor + x * across / width as i64 + y * down / height as i64 + disc + noise).clamp(0, 255) as u8
    });
    let ([blue, red], period, scale) = TILT;
    let colour =
        |tilt: i64| (0..width * height / 4).map(|at| (NEUTRAL + tilt * (at % period) as i64 / scale) as u8).collect();
    [luma.collect(), colour(blue), colour(red)]
}

// The noise the second scene adds to the first, taken and not measured, modelled on a phone camera's at a high gain with no dark frame of the device behind it: a deviation of luma from its most in black to its least in white, as a tone curve lifts the dark parts' noise most, and one of colour, in levels, independent in every sample; each a normal deviate made of the sum of four random bytes, whose deviation is NORMAL's.
const NOISE: (i64, i64, i64) = (6, 2, 4);
const NORMAL: (usize, i64, i64) = (4, 510, 148);
const NOISE_SEED: u32 = 0x9E37_79B9;

// The planes of picture `index` of the scene with the modelled noise.
fn noisy(index: usize) -> [Vec<u8>; 3] {
    let [mut luma, mut blue, mut red] = scene(index);
    let ((dark, light, colour), (bytes, middle, deviation)) = (NOISE, NORMAL);
    let mut state = NOISE_SEED ^ index as u32;
    let mut byte = || {
        state = xorshift32(state);
        i64::from(state as u8)
    };
    let mut add = |sample: &mut u8, spread: i64| {
        let normal = (0..bytes).map(|_| byte()).sum::<i64>() - middle;
        let noise = (2 * normal * spread + deviation).div_euclid(2 * deviation);
        *sample = (i64::from(*sample) + noise).clamp(0, 255) as u8;
    };
    for sample in &mut luma {
        let spread = dark + (light - dark) * i64::from(*sample) / i64::from(u8::MAX);
        add(sample, spread);
    }
    blue.iter_mut().chain(&mut red).for_each(|sample| add(sample, colour));
    [luma, blue, red]
}

impl Default for Bench {
    fn default() -> Bench {
        Bench {
            pictures: SCENES.map(|scene| (0..FRAMES).map(|index| scene.picture(index)).collect()),
            frames: Vec::new(),
            camera: Camera::default(),
            far: Far::default(),
            coded: Default::default(),
            vp9_size: (0, 0),
            sums: Vec::new(),
        }
    }
}

impl Bench {
    // The clean scene's pictures, which the kernels work over.
    fn clean(&self) -> &Pictures {
        &self.pictures[Scene::Clean as usize]
    }

    // Codes a scene's pictures as a call's, each frame's steps handed to `run` with their work: the bytes coded.
    fn code(&mut self, scene: Scene, mut run: impl FnMut(Step, Work<'_>)) -> u64 {
        let (width, height) = CALL_SIZE;
        let mut encoder = Encoder::default();
        self.frames = (self.pictures[scene as usize].iter().enumerate())
            .map(|(index, [luma, blue, red])| {
                let strides = [width, width / 2, width / 2];
                let picture = Picture { width, height, planes: [luma, blue, red], strides };
                if encoder.begin(&picture, Request { is_key: index == 0, quantizer: QUANTIZER }).is_err() {
                    return Vec::new();
                }
                encoder.code_steps(&mut run);
                encoder.finish().frame
            })
            .collect();
        let bytes = self.frames.iter().map(Vec::len).sum();
        self.coded[scene as usize].push(bytes);
        bytes as u64
    }

    /// Codes a scene's pictures as `Case::Encode` does, each frame's steps handed to `run` with their work: the shell runs it on its crew of the large cores and times each step.
    pub fn code_on(&mut self, scene: Scene, run: impl FnMut(Step, Work<'_>)) {
        let sum = self.code(scene, run);
        self.sums.push(black_box(sum));
    }

    // Makes the pictures of a conversion as many times as the pictures coded, each in `count` parts handed to `run`: a sample of each, summed.
    fn make(&mut self, conversion: Conversion, count: usize, mut run: impl FnMut(Vec<Part<'_>>)) -> u64 {
        match conversion {
            Conversion::Call => self.camera.make_call(count, &mut run),
            Conversion::Viewfinder => self.camera.draw(count, &mut run),
            Conversion::Recording => self.camera.record(count, &mut run),
        }
    }

    /// Makes the pictures of a conversion as `Case::Picture` does, in as many parts as the camera makes them in, handed to `run`: the shell runs them on a crew of every core, as the camera's crew runs them.
    pub fn make_on(&mut self, conversion: Conversion, run: impl FnMut(Vec<Part<'_>>)) {
        let sum = self.make(conversion, conversion.parts(), run);
        self.sums.push(black_box(sum));
    }

    /// Does a case's work.
    pub fn run(&mut self, case: Case) {
        let sum = match case {
            Case::Encode(scene) => self.code(scene, |_, work| work.in_order()),
            Case::DecodeVp8 => {
                let mut decoder = vp8::Decoder::default();
                self.frames.iter().filter(|frame| black_box(decoder.decode(frame)).is_ok()).count() as u64
            }
            Case::DecodeVp9 => {
                let frames = ivf::frames(vp9::SVC);
                let mut decoder = vp9::Decoder::default();
                let passes = FRAMES.div_ceil(frames.len());
                let mut decoded = 0;
                for frame in (0..passes).flat_map(|_| frames.iter()) {
                    if let Ok(picture) = black_box(decoder.decode(frame)) {
                        decoded += 1;
                        if let Some(picture) = picture {
                            self.vp9_size = (picture.width, picture.height);
                        }
                    }
                }
                decoded
            }
            Case::Picture(conversion) => self.make(conversion, 1, in_order),
            Case::FarPicture(index) => self.far.make(index),
            Case::Kernel(index, body) => KERNELS.get(index).map_or(0, |kernel| (kernel.run)(self.clean(), body)),
        };
        self.sums.push(black_box(sum));
    }

    /// The answer's lines: what the cases did in `timings`, each case's as `cases` orders them, then each scene's coding on the crew and its steps' times in `crewed`, by `SCENES`' order, each conversion's pictures made in parts in `parted`, by `CONVERSIONS`' order, and two threads' `WAKES` turns each at parking until the other wakes it in `woken`.
    pub fn to_lines(
        &self,
        timings: &[Timing],
        crewed: &[(Timing, Laps)],
        parted: &[Timing],
        woken: Timing,
    ) -> Vec<String> {
        let each = |time: &Duration| time.as_micros() as u64 / FRAMES as u64;
        let [noisy, clean, vp8, vp9, rest @ ..] = timings else {
            return Vec::new();
        };
        let (pictures, rest) = rest.split_at(CONVERSIONS.len().min(rest.len()));
        let (fars, kernels) = rest.split_at(FAR_CASES.len().min(rest.len()));
        let mut lines = Vec::new();
        for (((scene, encode), (crewed, laps)), coded) in SCENES.iter().zip([clean, noisy]).zip(crewed).zip(&self.coded)
        {
            let [alone, on_crew] = [coded.first(), coded.last()].map(|bytes| bytes.copied().unwrap_or(0));
            let name = scene.name();
            lines.push(format!(
                "encode {name}: {FRAMES} frames of {alone} bytes, {} us each, {}",
                each(&encode.time),
                encode.place.to_text()
            ));
            lines.push(format!(
                "encode {name} on the large cores' crew: {FRAMES} frames of {on_crew} bytes, {} us each ({}), the longest {} us, {}",
                each(&crewed.time),
                laps.to_text(FRAMES as u64),
                laps.longest.as_micros(),
                crewed.place.to_text()
            ));
        }
        let wakes = 2 * WAKES as u64;
        lines.push(format!(
            "a parked thread woken: {} us, {}",
            woken.time.as_micros() as u64 / wakes,
            woken.place.to_text()
        ));
        lines.push(format!(
            "decode VP8 {}x{}: {FRAMES} frames, {} us each, {}",
            CALL_SIZE.0,
            CALL_SIZE.1,
            each(&vp8.time),
            vp8.place.to_text()
        ));
        let (width, height) = self.vp9_size;
        lines.push(format!(
            "decode VP9 {width}x{height} in three layers: {FRAMES} frames, {} us each, {}",
            each(&vp9.time),
            vp9.place.to_text()
        ));
        for ((&conversion, alone), parted) in CONVERSIONS.iter().zip(pictures).zip(parted) {
            let ([before, after], (width, height)) = (conversion.name(), self.camera.size(conversion));
            lines.push(format!(
                "{before} {width}x{height} {after}: {FRAMES} pictures, {} us each on one thread {}, {} us each in {} parts on a crew of every core {}",
                each(&alone.time),
                alone.place.to_text(),
                each(&parted.time),
                conversion.parts(),
                parted.place.to_text()
            ));
        }
        for (&((across, down), turn, name), far) in FAR_CASES.iter().zip(fars) {
            let (width, height) = to_fill(turned_size(CALL_SIZE, turn.quarters), (across, down));
            lines.push(format!(
                "far picture {}x{} in a room of {across}x{down} {name}: {FRAMES} pictures of {width}x{height}, {} us each, {}",
                CALL_SIZE.0,
                CALL_SIZE.1,
                each(&far.time),
                far.place.to_text()
            ));
        }
        for (kernel, [plain, vector]) in KERNELS.iter().zip(kernels.as_chunks::<2>().0) {
            let (plain, vector) = (each(&plain.time), each(&vector.time));
            let speed = plain * PERCENT / vector.max(1);
            lines.push(format!(
                "{}: plain {plain} us, NEON {vector} us a picture, {}.{:02} times",
                kernel.to_name(),
                speed / PERCENT,
                speed % PERCENT
            ));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::crew::on_threads;

    #[test]
    #[ignore = "times the cases on the host: run in an optimized build"]
    fn bench_on_host() {
        extern crate std;
        let mut bench = Bench::default();
        let timings: Vec<Timing> = cases()
            .map(|case| {
                let start = std::time::Instant::now();
                bench.run(case);
                Timing { time: start.elapsed(), place: Place::default() }
            })
            .collect();
        // Each step's parts and rows on threads of their own, where the device's crew runs them on four cores.
        let crewed: Vec<(Timing, Laps)> = SCENES
            .iter()
            .map(|&scene| {
                let mut laps = Laps::default();
                let start = std::time::Instant::now();
                bench.code_on(scene, |step, work| {
                    let begun = std::time::Instant::now();
                    on_threads(work);
                    laps.add(step, begun.elapsed());
                });
                (Timing { time: start.elapsed(), place: Place::default() }, laps)
            })
            .collect();
        let parted: Vec<Timing> = CONVERSIONS
            .iter()
            .map(|&conversion| {
                let start = std::time::Instant::now();
                bench.make_on(conversion, |parts| {
                    std::thread::scope(|scope| {
                        parts.into_iter().for_each(|part| {
                            scope.spawn(part);
                        });
                    });
                });
                Timing { time: start.elapsed(), place: Place::default() }
            })
            .collect();
        // Two threads taking turns at parking until the other wakes it, as the shell's two of the crew do.
        let (turn, threads) =
            (core::sync::atomic::AtomicUsize::new(0), [std::sync::OnceLock::new(), std::sync::OnceLock::new()]);
        let take_turns = |side: usize| {
            let _ = threads[side].set(std::thread::current());
            for _ in 0..WAKES {
                while turn.load(core::sync::atomic::Ordering::Acquire) % threads.len() != side {
                    std::thread::park();
                }
                turn.fetch_add(1, core::sync::atomic::Ordering::Release);
                if let Some(other) = threads[1 - side].get() {
                    other.unpark();
                }
            }
        };
        let start = std::time::Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| take_turns(1));
            take_turns(0);
        });
        let woken = Timing { time: start.elapsed(), place: Place::default() };
        for line in bench.to_lines(&timings, &crewed, &parted, woken) {
            std::println!("{line}");
        }
    }

    // The bench's pictures of a conversion as they stand: a call's planes, the viewfinder's words and what they measured, a recording's planes.
    fn made(camera: &Camera, conversion: Conversion) -> Vec<u64> {
        let bytes = |planes: &[Vec<u8>]| planes.iter().flatten().map(|&sample| u64::from(sample)).collect();
        match conversion {
            Conversion::Call => bytes(&camera.call_picture),
            Conversion::Viewfinder => camera.words.iter().map(|&word| u64::from(word)).collect(),
            Conversion::Recording => bytes(&camera.recorded),
        }
    }

    #[test]
    fn picture_in_parts_as_whole() {
        let mut bench = Bench::default();
        for conversion in CONVERSIONS {
            bench.make(conversion, 1, in_order);
            let (whole, measure) = (made(&bench.camera, conversion), bench.camera.measures[0]);
            assert!(whole.iter().any(|&sample| sample != whole[0]), "{conversion:?}: a picture with something in it");
            let camera = &mut bench.camera;
            camera.call_picture.iter_mut().chain(&mut camera.recorded).for_each(|plane| plane.fill(0));
            camera.words.fill(0);
            let mut counts = Vec::new();
            bench.make(conversion, conversion.parts(), |parts| {
                counts.push(parts.len());
                parts.into_iter().rev().for_each(|part| part());
            });
            assert_eq!(
                counts,
                [conversion.parts(); FRAMES],
                "{conversion:?}: a part for each core it takes, each picture"
            );
            assert!(
                made(&bench.camera, conversion) == whole,
                "{conversion:?}: the picture in parts, run in any order, as in one"
            );
            if conversion == Conversion::Viewfinder {
                let parts = bench.camera.measures.iter().fold(Measure::default(), |sum, part| sum.merge(*part));
                assert!(parts == measure, "and its parts measure what it measured whole");
            }
        }
    }

    #[test]
    fn places_told() {
        let clocks = [Some(2_001_000), Some(1_500_000)];
        let places = [Some(LARGE_CORES[0]), Some(7), None].map(|core| Place { core, clocks }.to_text());
        assert_eq!(
            places,
            [
                format!("on core {} at 2001 MHz", LARGE_CORES[0]),
                "on core 7 at 1500 MHz".into(),
                "on a core not told".into()
            ],
            "a large core at the large cores' clock, a small one at the small ones'"
        );
        assert_eq!(Place { core: Some(7), clocks: [None; 2] }.to_text(), "on core 7", "a clock not read is not told");
    }

    #[test]
    fn vp9_size_of_its_stream() {
        let mut bench = Bench::default();
        bench.run(Case::DecodeVp9);
        assert_eq!(bench.vp9_size, (320, 176), "the stream's upper layer, which its line tells");
    }

    #[test]
    fn noise_of_its_deviation() {
        let ([luma, blue, _], [noisy_luma, noisy_blue, _]) = (scene(1), noisy(1));
        let variance = |clean: &[u8], noisy: &[u8]| {
            let squares: i64 = clean.iter().zip(noisy).map(|(&a, &b)| (i64::from(b) - i64::from(a)).pow(2)).sum();
            squares / clean.len() as i64
        };
        let (dark, light, colour) = NOISE;
        let chroma = variance(&blue, &noisy_blue);
        assert!(
            (chroma - colour * colour).abs() * 10 < colour * colour,
            "colour's of a deviation of {colour}: {chroma}"
        );
        let luma_variance = variance(&luma, &noisy_luma);
        assert!(light * light < luma_variance && luma_variance < dark * dark, "luma's between white's and black's");
    }

    #[test]
    fn kernels_agree() {
        let pictures: Vec<[Vec<u8>; 3]> = (0..2).map(scene).collect();
        for kernel in &KERNELS {
            let name = kernel.to_name();
            assert_eq!((kernel.run)(&pictures, Body::Plain), (kernel.run)(&pictures, Body::Vector), "{name}");
        }
    }

    #[test]
    fn far_pictures_as_told() {
        let mut far = Far::default();
        for (index, &(room, turn, name)) in FAR_CASES.iter().enumerate() {
            assert!(far.make(index) > 0, "{room:?} {name}: a picture of the decoded frame");
            let picture = &far.picture;
            assert_eq!(
                (picture.width, picture.height()),
                to_fill(turned_size(CALL_SIZE, turn.quarters), room),
                "{room:?} {name}: of the size its line tells"
            );
        }
    }
}

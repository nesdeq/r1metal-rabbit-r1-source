//! What the main loop and the camera's threads share.

use super::gallery::{Kind, Note, Stamp};
use super::magic::{self, Order, Variant};
use crate::base::file_names::Names;
use crate::base::handoff::{Handover, Request};
use crate::base::pixel::PixelFormat;
use crate::base::sampling::Turn;
use crate::base::time::NANOS_PER_SECOND;
use crate::codec::jpeg;
use crate::hw::camera::exposure::{Auto, CALL_FRAMES_PER_SECOND, Exposure, Pace, Still};
use crate::hw::camera::measure::Measure;
use crate::hw::camera::mode::Mode;
use crate::hw::camera::raw::{Square, Window};
use crate::hw::camera::stream::{is_look_frame, is_settled};
use crate::hw::camera::swivel::{Facing, capture_turn, viewfinder_turn};
use crate::hw::camera::{develop, recording};
use crate::input::pose::Pose;
use crate::storage::PICTURE_EXTENSION;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Debug, Display};
use core::time::Duration;

/// A viewfinder that shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Viewfinder {
    pub kind: Kind,
    pub facing: Facing,
    /// True in the key's window, which scans for a code.
    pub is_scanning: bool,
    /// True in a call, whose far side is sent pictures of the stream.
    pub is_calling: bool,
    /// Width and height of its picture in pixels; a photo's and a call's are drawn by the width alone (`frame_size`).
    pub size: (usize, usize),
}

/// The width and height of a picture for a call's far side: a frame of a call's stream, as the sensor has it.
pub const CALL_SIZE: (usize, usize) = (640, 480);

// The sensor's mode for a call's stream, whose frames are as large as the far side's pictures.
const CALL_MODE: Mode = Mode::HighSpeed;

// The most pictures a second a call's far side is sent, as many as a call's stream has frames (`Pace::CALL`).
const CALL_RATE: u64 = CALL_FRAMES_PER_SECOND as u64;
const CALL_INTERVAL_NS: u64 = NANOS_PER_SECOND / CALL_RATE;
// How early a picture may come for its turn: half a turn, so that a frame of a call's stream made late or early by its drawing still makes one, and a stream of twice the rate every other.
const CALL_EARLY_NS: u64 = CALL_INTERVAL_NS / 2;

/// A picture of the stream for a call's far side: its Y, U and V planes of `CALL_SIZE`, when its frame came on the paced clock, the quarter turns that stand it upright, and whether the camera looks away from the owner.
#[derive(Default)]
pub struct CallPicture {
    pub planes: [Vec<u8>; 3],
    pub at_ns: u64,
    pub quarters: u8,
    pub is_rear: bool,
}

impl Viewfinder {
    /// The pace its stream runs at: a call's at the rate its far side is sent pictures, every other at a viewfinder's 60 frames a second.
    pub const fn pace(&self) -> Pace {
        match self.kind {
            Kind::Photo if self.is_calling => Pace::CALL,
            Kind::Photo | Kind::Video => Pace::LIVE,
        }
    }

    /// The sensor mode its stream runs in.
    pub fn mode(&self) -> Mode {
        match self.kind {
            Kind::Photo if self.is_calling => CALL_MODE,
            Kind::Photo => develop::VIEW_MODE,
            Kind::Video => recording::MODE,
        }
    }

    /// The part of its mode's frame it draws: the middle square for photos, the whole for videos and a call's.
    pub fn view(&self) -> Window {
        let frame = self.mode().frame();
        match self.kind {
            Kind::Photo if !self.is_calling => Square::middle_of(frame).window(),
            Kind::Photo | Kind::Video => Window::whole(frame),
        }
    }

    /// The width and height its stream draws its picture at: a call's whole frame upright and a photo's square, by its width.
    pub const fn frame_size(&self) -> (usize, usize) {
        let (long, short) = CALL_SIZE;
        match self.kind {
            Kind::Photo if self.is_calling => (self.size.0, self.size.0 * long / short),
            Kind::Photo => (self.size.0, self.size.0),
            Kind::Video => self.size,
        }
    }
}

/// How a photo, a recording or a run of the camera went.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    // The file's name.
    Saved(String),
    NoPhoto,
    NoVideo,
    Full,
    TooWarm,
    LowBattery,
    Failed,
}

/// The error text of a power up the battery's reserve refused.
pub const RESERVE_TEXT: &str = "not powered up: the battery is under the camera's reserve";

/// The error text of a photo whose viewfinder was left while its burst waited for a buffer.
pub const UNSUPPLIED_TEXT: &str = "the viewfinder was left while the burst waited for a buffer";

/// What the camera's thread does with the viewfinder wanted.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Start {
    Run(Viewfinder),
    Refuse(Outcome),
}

impl Outcome {
    /// The outcome of a recording: `file` is none where the file was lost and holds none where no frame was kept; `interruption` is what cut it short.
    pub fn from_recording(file: Option<Option<&str>>, interruption: Option<Outcome>) -> Outcome {
        match (file, interruption) {
            (None, _) | (Some(None), None) => Outcome::NoVideo,
            (Some(_), Some(interruption)) => interruption,
            (Some(Some(name)), None) => Outcome::Saved(name.into()),
        }
    }
}

/// A run of the camera in numbers, for the log.
#[derive(Default)]
pub struct Stats {
    frames: u64,
    skipped: u64,
    drawing: Duration,
    slowest: Duration,
    refreshing: Duration,
    photos: u32,
    measure: Measure,
}

impl Stats {
    /// Counts a frame passed over.
    pub const fn note_skipped(&mut self) {
        self.skipped += 1;
    }

    /// Counts a frame's refresh of the cache.
    pub fn note_refreshed(&mut self, duration: Duration) {
        self.refreshing += duration;
    }

    /// Counts a drawn frame; true at a frame the camera's warmth is checked.
    pub fn note_frame(&mut self, measure: Measure, duration: Duration) -> bool {
        self.measure = measure;
        self.frames += 1;
        self.drawing += duration;
        self.slowest = self.slowest.max(duration);
        is_look_frame(self.frames)
    }

    pub const fn note_photo(&mut self) {
        self.photos += 1;
    }

    /// True at a frame the room of a running recording is checked.
    pub const fn is_room_look_due(&self, is_recording: bool) -> bool {
        is_recording && is_look_frame(self.frames)
    }

    /// The log line of a run of `viewfinder`, with the kernel's error bits where they could be read.
    pub fn to_run_line(&self, viewfinder: Viewfinder, duration: Duration, auto: &Auto, errors: Option<u32>) -> String {
        let seconds = duration.as_secs_f32();
        let mean_us = |total: Duration| total.as_micros() as u64 / self.frames.max(1);
        let Exposure { lines, gain } = auto.exposure();
        format!(
            "camera: {:?} {:?} for {seconds:.1} s, {} frames drawn in {} us each ({} us of it cache refresh, {} us at most), {} skipped, {} photos, exposure {lines} lines at gain {gain}/64 lifted {}/1024, {}, error bits {:#x}",
            viewfinder.kind,
            viewfinder.facing,
            self.frames,
            mean_us(self.drawing),
            mean_us(self.refreshing),
            self.slowest.as_micros(),
            self.skipped,
            self.photos,
            auto.lift(),
            auto.to_log_line(),
            errors.unwrap_or(u32::MAX)
        )
    }

    /// The log lines of the zones of a run's last frame.
    pub fn to_zone_lines(&self) -> impl Iterator<Item = String> + '_ {
        let lines = self.measure.to_log_lines().enumerate();
        lines.map(|(row, line)| format!("camera: zones of the last frame, row {row}: {line}"))
    }

    /// The log lines of the zones of the frame before the photo `name`.
    pub fn to_photo_zone_lines<'a>(&'a self, name: &'a str) -> impl Iterator<Item = String> + 'a {
        let lines = self.measure.to_log_lines().enumerate();
        lines.map(move |(row, line)| format!("camera: photo {name}, zones of the frame before it, row {row}: {line}"))
    }
}

/// What the log names a photo for the robot by, which has no file.
pub const LOOK: &str = "for the robot";

/// What the log names a burst's photo by: its name, or `LOOK` where it is the robot's alone.
pub fn to_log_name(photo: Option<&Taken>) -> &str {
    photo.map_or(LOOK, |taken| taken.name.as_str())
}

/// The log text of what became of a photo for the robot.
pub const LOOKED_TEXT: &str = "handed to the robot";

/// The log lines of a burst taken for the photo `name`, its last frame in after `captured`.
pub fn to_taken_lines(name: &str, still: &Still, quarters: u8, captured: Duration, auto: &Auto) -> [String; 2] {
    let Still { exposure: Exposure { lines, gain }, recipe, frames, under, .. } = still;
    [
        format!(
            "camera: photo {name}, {frames} frames of {lines} lines at gain {gain}/64 with gains {:?}, {under}/1024 below the light, {quarters} quarter turns, taken in {} ms",
            recipe.gains,
            captured.as_millis()
        ),
        format!("camera: photo {name}, {}, matrix {:?}", auto.to_log_line(), recipe.matrix),
    ]
}

/// A photo developed, as the log reports it.
pub struct Developed<'a> {
    pub name: &'a str,
    pub length: usize,
    /// Which frame the others were laid on.
    pub reference: usize,
    /// A raw sample's variance per code of light, and with none.
    pub noise: (f32, f32),
    /// From the click: when its development began and its burst was merged, developed, sharpened and encoded.
    pub times: [Duration; 5],
}

impl Developed<'_> {
    /// Its log line; `fate` tells what became of it.
    pub fn to_log_line(&self, fate: &str) -> String {
        let Developed { name, length, reference, noise: (per_code, floor), times } = self;
        let [begun, merged, developed, sharpened, encoded] = *times;
        format!(
            "camera: photo {name}, {length} bytes: begun after {} ms, merged on frame {reference} in {} ms (noise {per_code:.3} per code and {floor:.1}), developed in {} ms, sharpened in {} ms, encoded in {} ms, {fate}",
            begun.as_millis(),
            (merged - begun).as_millis(),
            (developed - merged).as_millis(),
            (sharpened - developed).as_millis(),
            (encoded - sharpened).as_millis()
        )
    }
}

// The kernel's reserve for media buffers on the R1 (`CmaTotal` of /proc/meminfo), which the bursts that wait are held to.
const MEDIA_BYTES: usize = 344_064 * 1024;

/// How many bursts of `bytes` each may wait at once.
pub const fn to_burst_count(bytes: usize) -> usize {
    let count = MEDIA_BYTES / if bytes == 0 { 1 } else { bytes };
    if count == 0 { 1 } else { count }
}

/// Checks that a burst's frames lie in the slots of its buffer; the error text of one that has more.
pub fn check_burst(frames: usize, slots: usize) -> Result<(), String> {
    if frames <= slots { Ok(()) } else { Err(format!("a burst of {frames} frames in a buffer of {slots}")) }
}

/// A burst taken and waiting to be developed: its frames' buffer, how it was taken, and what it becomes.
pub struct Job<B> {
    pub burst: B,
    pub still: Still,
    pub turn: Turn,
    /// The photo it becomes; none for a photo for the robot.
    pub photo: Option<Taken>,
    /// When it was clicked, on the monotonic clock.
    pub clicked_ns: u64,
    session: u32,
}

/// A photo taken for the roll: its name, and how its review shows its pictures.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Taken {
    pub name: String,
    pub sight: Sight,
}

/// How a review shows a picture of its photo: its side, its pixel format and its EXIF orientation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sight {
    pub side: usize,
    pub format: PixelFormat,
    pub orientation: u8,
}

impl Sight {
    // The sight of a review `side` pixels square in `format` of a photo taken looking to `facing`, mirrored where its viewfinder mirrors.
    pub(crate) const fn new(side: usize, facing: Facing, format: PixelFormat) -> Sight {
        let orientation = if viewfinder_turn(facing).is_mirrored { jpeg::MIRRORED } else { jpeg::UPRIGHT };
        Sight { side, format, orientation }
    }
}

impl<B> Job<B> {
    pub const fn new(burst: B, still: Still, turn: Turn, photo: Option<Taken>, clicked_ns: u64) -> Job<B> {
        Job { burst, still, turn, photo, clicked_ns, session: 0 }
    }

    /// The camera session it was taken in.
    pub const fn session(&self) -> u32 {
        self.session
    }
}

/// Where the next burst's buffer comes from.
pub enum Supply<B> {
    Spare(B),
    Make,
    // None: the viewfinder was left while the burst waited for one.
    Leave,
}

/// What the darkroom does next.
pub enum Work<B> {
    Develop(Job<B>),
    // Shows the pixel art of the photo the review shows, as the review shows it.
    Art(String, Sight),
    // Makes a saved photo's copy for the pixel cam anew, for its redraw.
    Copy(String),
    // Nothing waits: the encoder and a photo's buffers are let go of.
    Release,
}

/// The darkroom's work: the bursts that wait, the buffers developed ones leave, the art a review asks for, the copies redraws ask for, and whether the encoder is claimed.
pub struct Darkroom<B> {
    waiting: VecDeque<Job<B>>,
    spares: Vec<B>,
    developing: Option<Option<String>>,
    art: Option<(String, Sight)>,
    copies: VecDeque<String>,
    session: u32,
    alive: usize,
    // A burst or a copy claimed the encoder and a photo's buffers, which they keep until the release.
    is_claimed: bool,
}

impl<B> Default for Darkroom<B> {
    fn default() -> Darkroom<B> {
        Darkroom {
            waiting: VecDeque::new(),
            spares: Vec::new(),
            developing: None,
            art: None,
            copies: VecDeque::new(),
            session: 0,
            alive: 0,
            is_claimed: false,
        }
    }
}

impl<B> Darkroom<B> {
    // Ends a camera session, whose bursts went into buffers of its own: returns its spares, to let go of; one of its bursts developed later lets go of its buffer too.
    fn end_session(&mut self) -> Vec<B> {
        self.session += 1;
        self.alive -= self.spares.len();
        core::mem::take(&mut self.spares)
    }

    // Where the next burst's buffer comes from, at most `budget` of them alive at once; none until a burst is developed.
    fn supply(&mut self, budget: usize) -> Option<Supply<B>> {
        if let Some(spare) = self.spares.pop() {
            return Some(Supply::Spare(spare));
        }
        (self.alive < budget).then(|| {
            self.alive += 1;
            Supply::Make
        })
    }

    /// Notes a buffer that could not be made.
    pub const fn note_unmade(&mut self) {
        self.alive -= 1;
    }

    /// Keeps a buffer no burst was taken into, for the next.
    pub fn keep(&mut self, burst: B) {
        self.spares.push(burst);
    }

    /// Queues a burst for the darkroom.
    pub fn put(&mut self, mut job: Job<B>) {
        job.session = self.session;
        self.waiting.push_back(job);
    }

    /// Asks for the pixel art of the photo a review shows, as the review shows it.
    pub fn want_art(&mut self, photo: String, sight: Sight) {
        self.art = Some((photo, sight));
    }

    /// Asks for a saved photo's copy for the pixel cam to be made anew, for its redraw.
    pub fn want_copy(&mut self, photo: String) {
        self.copies.push_back(photo);
    }

    /// Takes the next work: art a review waits for first, as it is quick, then a copy a redraw waits for, then the next burst to develop; once nothing waits, the release of what a copy or a burst claimed.
    pub fn take_work(&mut self) -> Option<Work<B>> {
        if let Some((photo, sight)) = self.art.take() {
            return Some(Work::Art(photo, sight));
        }
        if let Some(photo) = self.copies.pop_front() {
            self.is_claimed = true;
            return Some(Work::Copy(photo));
        }
        if let Some(job) = self.waiting.pop_front() {
            self.developing = Some(job.photo.as_ref().map(|photo| photo.name.clone()));
            self.is_claimed = true;
            return Some(Work::Develop(job));
        }
        core::mem::take(&mut self.is_claimed).then_some(Work::Release)
    }

    /// Takes back the buffer of a burst developed in `session`; one of an earlier session comes back, to let go of.
    pub fn give_back(&mut self, burst: B, session: u32) -> Option<B> {
        self.developing = None;
        if session == self.session {
            self.spares.push(burst);
            return None;
        }
        self.alive -= 1;
        Some(burst)
    }

    // True where a photo of that name waits or is being developed.
    fn holds(&self, photo: &str) -> bool {
        let is_waiting = self.waiting.iter().any(|job| job.photo.as_ref().is_some_and(|taken| taken.name == photo));
        is_waiting || self.developing.as_ref().is_some_and(|name| name.as_deref() == Some(photo))
    }

    /// The names photos have taken, over the names of the pictures' files.
    pub const fn names<N: Names>(&self, files: N) -> PhotoNames<'_, B, N> {
        PhotoNames { darkroom: self, files }
    }

    // True while a burst taken for the robot alone waits or is developed.
    fn holds_look(&self) -> bool {
        self.waiting.iter().any(|job| job.photo.is_none()) || self.developing == Some(None)
    }

    // True while no burst waits or is developed, no art or copy is asked for, and nothing is claimed.
    fn is_idle(&self) -> bool {
        let is_asked = self.art.is_some() || !self.copies.is_empty();
        self.waiting.is_empty() && self.developing.is_none() && !is_asked && !self.is_claimed
    }
}

/// The names photos have taken: a photo's is taken while its burst waits or is developed, or while its file holds it.
pub struct PhotoNames<'a, B, N> {
    darkroom: &'a Darkroom<B>,
    files: N,
}

impl<B, N: Names> Names for PhotoNames<'_, B, N> {
    type Error = N::Error;

    fn has(&mut self, photo: &str) -> Result<bool, N::Error> {
        let file = magic::to_name(photo, Variant::Original, PICTURE_EXTENSION);
        Ok(self.darkroom.holds(photo) || self.files.has(&file)?)
    }
}

/// The log line of a photo's copy made anew for a redraw, and how it went to the pixel cam.
pub fn to_redraw_line(photo: &str, handover: &str) -> String {
    format!("camera: photo {photo}, for a redraw: {handover}")
}

/// The log text of a photo saved in `duration`, and how it went to the pixel cam.
pub fn to_saved_text(duration: Duration, handover: &str) -> String {
    format!("saved in {} ms, {handover}", duration.as_millis())
}

/// The log text of a photo handed to the pixel cam.
pub fn to_handed_text(bytes: usize, duration: Duration) -> String {
    format!("handed to the pixel cam as {bytes} bytes in {} ms", duration.as_millis())
}

/// The log text of a photo not handed to the pixel cam.
pub fn to_unhanded_text(reason: impl Display) -> String {
    format!("not handed to the pixel cam: {reason}")
}

/// The next step of a viewfinder's stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Leave,
    // A photo is taken, its burst turned so; its review shows as the sight tells, and with none it is the robot's alone.
    Shoot(Turn, Option<Sight>),
    Show,
    // The photo just taken shows in the stream's place: the stream rests.
    Pause,
}

/// The next step of a video viewfinder's stream, once a frame of it shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Recording {
    Leave,
    Show,
    // A recording begins, which a player turns by that many quarter turns.
    Begin(u8),
    End,
}

/// A picture handed to the screen: its viewfinder, its arrival on the monotonic clock, and whether its stream keeps step with the screen's flips.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PictureInfo {
    pub viewfinder: Viewfinder,
    pub at_ns: u64,
    pub is_in_step: bool,
}

/// A photo taken for the robot alone: the JPEG the model is given.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shot {
    pub jpeg: Vec<u8>,
}

/// How far the darkroom has got with the burst it develops.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Development {
    Merging,
    Finishing,
    Done,
}

/// A picture of the photo just taken for its review, developed or redrawn as pixel art.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Review {
    pub photo: String,
    pub picture: Vec<u32>,
    pub width: usize,
}

/// What a code the scanner read holds; its `Debug` tells its size alone, as it may hold a key.
#[derive(Clone, PartialEq, Eq)]
pub struct Code(pub Vec<u8>);

impl Debug for Code {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Code({} bytes)", self.0.len())
    }
}

/// The camera's report to the main loop.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Report {
    pub outcome: Option<Outcome>,
    /// The stream stopped for a photo, and begins anew behind it.
    pub is_shooting: bool,
    pub is_recording: bool,
    /// How a new picture was handed over.
    pub picture: Option<PictureInfo>,
    /// The orders to the pixel cam.
    pub orders: Vec<Order>,
    /// A photo for the robot.
    pub shot: Option<Shot>,
    /// The name of the photo whose burst was just taken.
    pub taken: Option<String>,
    /// How far the darkroom has got since the last report.
    pub development: Option<Development>,
    /// The newest picture of the photo just taken.
    pub review: Option<Review>,
    /// Why a redraw a roll asked for came to nothing.
    pub note: Option<Note>,
    /// The last code the scanner read.
    pub code: Option<Code>,
}

/// What the main loop and the camera's, the darkroom's and the scanner's threads share, `B` a burst's buffer.
pub struct State<B> {
    pub request: Request<Viewfinder>,
    /// The battery keeps the camera from powering up, as the main loop last told.
    pub is_reserved: bool,
    // The next photo is the robot's alone.
    is_look_due: bool,
    has_failed: bool,
    is_recording: bool,
    is_paused: bool,
    pub pose: Pose,
    /// When the panel last began to show a new page, 0 until it has shown one.
    pub flipped_ns: u64,
    pub report: Report,
    view: Handover<Vec<u32>, PictureInfo>,
    // Pictures for a call's far side are made of the stream's frames, the next one's turn by `CALL_RATE` on the paced clock, and the newest one made.
    is_picture_wanted: bool,
    picture_turn_ns: u64,
    sent: Handover<CallPicture, ()>,
    /// What the darkroom is given to do.
    pub darkroom: Darkroom<B>,
    /// The picture the scanner reads.
    pub scan: Scan,
}

impl<B> Default for State<B> {
    fn default() -> State<B> {
        State {
            request: Request::default(),
            is_reserved: false,
            is_look_due: false,
            has_failed: false,
            is_recording: false,
            is_paused: false,
            pose: Pose::default(),
            flipped_ns: 0,
            report: Report::default(),
            view: Handover::default(),
            is_picture_wanted: false,
            picture_turn_ns: 0,
            sent: Handover::default(),
            darkroom: Darkroom::default(),
            scan: Scan::default(),
        }
    }
}

impl<B> State<B> {
    /// Takes the viewfinder that shows, or none; true where it changed. A viewfinder that opens drops the picture and the look left over.
    pub fn set_wanted(&mut self, viewfinder: Option<Viewfinder>) -> bool {
        if self.request.is_opened_by(viewfinder) {
            self.view.mark_stale();
            self.is_look_due = false;
        }
        self.request.set(viewfinder)
    }

    pub const fn set_recording(&mut self, is_recording: bool) {
        self.is_recording = is_recording;
    }

    /// Sets whether the photo just taken shows in the stream's place; true where that changed.
    pub fn set_paused(&mut self, is_paused: bool) -> bool {
        core::mem::replace(&mut self.is_paused, is_paused) != is_paused
    }

    /// A click whose photo is the robot's alone.
    pub const fn look(&mut self) {
        self.is_look_due = true;
        self.request.click();
    }

    // True once for the photo a look asked for.
    const fn take_look(&mut self) -> bool {
        core::mem::replace(&mut self.is_look_due, false)
    }

    /// Swaps in a picture drawn for a viewfinder; `picture` holds the old one to draw over.
    pub fn put_picture(&mut self, picture: &mut Vec<u32>, viewfinder: Viewfinder, at_ns: u64, is_in_step: bool) {
        self.view.put(picture, PictureInfo { viewfinder, at_ns, is_in_step });
    }

    /// Sets whether a call's stream makes pictures of its frames for the far side.
    pub const fn want_call_pictures(&mut self, is_wanted: bool) {
        self.is_picture_wanted = is_wanted;
    }

    /// Where pictures for the far side are wanted of the stream of `viewfinder`, a call's, and its frame, which came at `at_ns` on the paced clock, keeps the pictures to `CALL_RATE` a second (a virtual scheduling, ITU-T I.371's): the quarter turns that stand it upright as the device is held, and whether the camera looks away from the owner.
    pub fn take_picture_wish(&mut self, viewfinder: Viewfinder, at_ns: u64) -> Option<(u8, bool)> {
        let is_turn = at_ns + CALL_EARLY_NS > self.picture_turn_ns;
        if !(viewfinder.is_calling && self.is_picture_wanted && is_turn) {
            return None;
        }
        self.picture_turn_ns = self.picture_turn_ns.max(at_ns) + CALL_INTERVAL_NS;
        let quarters = capture_turn(viewfinder.facing, self.pose.quarters()).quarters;
        Some((quarters, viewfinder.facing == Facing::Rear))
    }

    /// Swaps in a picture made for a call's far side; `picture` holds the old one to write over.
    pub fn put_call_picture(&mut self, picture: &mut CallPicture) {
        self.sent.put(picture, ());
    }

    /// Swaps the newest picture for a call's far side into `picture`; true where one came since the last.
    pub fn take_call_picture(&mut self, picture: &mut CallPicture) -> bool {
        self.sent.take(picture, |()| true).is_some()
    }

    /// Swaps the newest picture drawn for `viewfinder` into `picture`, and takes the report.
    pub fn take_report(&mut self, viewfinder: Option<Viewfinder>, picture: &mut Vec<u32>) -> Report {
        let picture = self.view.take(picture, |info| Some(info.viewfinder) == viewfinder);
        Report { picture, is_recording: self.is_recording, ..core::mem::take(&mut self.report) }
    }

    /// What the thread does with the viewfinder wanted; after a run that failed or was refused nothing until none is wanted.
    pub fn serve(&mut self) -> Option<Start> {
        self.has_failed &= self.request.target.is_some();
        if self.has_failed {
            return None;
        }
        let viewfinder = self.request.serve()?;
        Some(if self.is_reserved { Start::Refuse(Outcome::LowBattery) } else { Start::Run(viewfinder) })
    }

    /// True where the camera turns into its case at the end of a run.
    pub const fn should_park(&self, has_failed: bool) -> bool {
        has_failed || self.request.target.is_none()
    }

    /// Ends a run: the thread holds nothing from then on, and the picture kept for the screen is let go of; returns the spare buffers of its bursts, to let go of.
    pub fn end_run(&mut self, has_failed: bool) -> Vec<B> {
        self.has_failed = has_failed;
        self.request.is_busy = false;
        (self.view, self.sent) = (Handover::default(), Handover::default());
        self.darkroom.end_session()
    }

    /// True while no part of the camera has power, nothing of it moves, and the darkroom has nothing to do and holds nothing.
    pub fn is_off(&self) -> bool {
        !self.request.is_busy && self.darkroom.is_idle()
    }

    /// Where the next burst's buffer for a photo of `viewfinder` comes from, at most `budget` of them alive at once; none until a burst is developed, and none needed once the viewfinder is left meanwhile.
    pub fn supply(&mut self, viewfinder: Viewfinder, budget: usize) -> Option<Supply<B>> {
        let supply = self.darkroom.supply(budget);
        supply.or_else(|| (!self.is_wanted(viewfinder)).then_some(Supply::Leave))
    }

    /// Notes that a redraw's copy could not be made: the roll is told so.
    pub fn note_uncopied(&mut self) {
        self.report.note = Some(Stamp::Redraw.failure());
    }

    /// The scanner's next step, the picture handed over swapped into `picture`; none while it waits for one.
    pub fn next_scan(&mut self, picture: &mut Vec<u32>) -> Option<ScanStep> {
        let is_scanning = self.request.target.is_some_and(|viewfinder| viewfinder.is_scanning);
        self.scan.next_step(is_scanning, picture)
    }

    fn take_click(&mut self, frames: u64) -> bool {
        is_settled(frames) && self.request.take_clicks() > 0
    }

    fn is_wanted(&self, viewfinder: Viewfinder) -> bool {
        self.request.target == Some(viewfinder)
    }

    // No click waits, and the photo just taken shows in the stream's place, which the screen asks for with the click that took it, or the robot's burst is not developed yet: the cell carries no stream on top of that development.
    fn is_resting(&self) -> bool {
        self.request.clicks == 0 && (self.is_paused || self.darkroom.holds_look())
    }

    /// The next step of the photo stream of `viewfinder`, which draws in `format`, `frames` in.
    pub fn next_view(&mut self, viewfinder: Viewfinder, format: PixelFormat, frames: u64) -> View {
        if !self.is_wanted(viewfinder) {
            return View::Leave;
        }
        if self.take_click(frames) {
            let turn = capture_turn(viewfinder.facing, self.pose.quarters());
            let review = (!self.take_look()).then(|| Sight::new(viewfinder.size.0, viewfinder.facing, format));
            return View::Shoot(turn, review);
        }
        if self.is_resting() {
            return View::Pause;
        }
        View::Show
    }

    /// What the photo stream of `viewfinder` does after a rest, none while it rests; a click that waits is left for the stream.
    pub fn after_rest(&self, viewfinder: Viewfinder) -> Option<View> {
        if !self.is_wanted(viewfinder) {
            return Some(View::Leave);
        }
        (!self.is_resting()).then_some(View::Show)
    }

    /// The next step of the video stream of `viewfinder`, `frames` in, once a frame of it shows; a recording runs where `is_running`.
    pub fn next_recording(&mut self, viewfinder: Viewfinder, frames: u64, is_running: bool) -> Recording {
        if !self.is_wanted(viewfinder) {
            return Recording::Leave;
        }
        match (self.take_click(frames), is_running) {
            (false, _) => Recording::Show,
            (true, true) => Recording::End,
            (true, false) => Recording::Begin(capture_turn(viewfinder.facing, self.pose.quarters()).quarters),
        }
    }
}

/// What the scanner's thread does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScanStep {
    // Reads the picture swapped in, of that side and format.
    Read(usize, PixelFormat),
    // No viewfinder scans: the thread lets go of its pictures and buffers.
    Rest,
}

/// What the camera's thread hands the scanner's: a picture to read.
#[derive(Default)]
pub struct Scan {
    picture: Vec<u32>,
    shape: Option<(usize, PixelFormat)>,
    is_waiting: bool,
    // A picture was handed over since the last rest, so the scanner holds pictures and buffers.
    is_holding: bool,
}

impl Scan {
    /// Copies a picture in where the scanner waits for one; true where it does.
    pub fn offer(&mut self, picture: &[u32], side: usize, format: PixelFormat) -> bool {
        if self.is_waiting {
            self.picture.clear();
            self.picture.extend_from_slice(picture);
            (self.shape, self.is_waiting, self.is_holding) = (Some((side, format)), false, true);
            return true;
        }
        false
    }

    /// True while the scanner is ready for a picture.
    pub const fn is_waiting(&self) -> bool {
        self.is_waiting
    }

    /// True where a picture was handed over and not taken.
    pub const fn has_picture(&self) -> bool {
        self.shape.is_some()
    }

    // The scanner's next step while a viewfinder scans or none does, the picture handed over swapped into `picture`; none while it waits, ready for a picture where a viewfinder scans.
    fn next_step(&mut self, is_scanning: bool, picture: &mut Vec<u32>) -> Option<ScanStep> {
        if !is_scanning {
            self.is_waiting = false;
            if !core::mem::take(&mut self.is_holding) {
                return None;
            }
            (self.picture, self.shape) = (Vec::new(), None);
            return Some(ScanStep::Rest);
        }
        let Some((side, format)) = self.shape.take() else {
            self.is_waiting = true;
            return None;
        };
        core::mem::swap(&mut self.picture, picture);
        Some(ScanStep::Read(side, format))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::camera::tone::Tone;
    use crate::hw::camera::video::Planes;

    const REAR: Viewfinder =
        Viewfinder { kind: Kind::Photo, facing: Facing::Rear, is_scanning: false, is_calling: false, size: (480, 480) };
    const SETTLED: u64 = 100;

    #[test]
    fn call_pictures_while_wanted() {
        const CALL: Viewfinder = Viewfinder { facing: Facing::Front, is_calling: true, size: (91, 91), ..REAR };
        let mut state = State::<u32>::default();
        state.request.set(Some(CALL));
        assert_eq!(state.take_picture_wish(CALL, 0), None, "none wanted, none made");
        state.want_call_pictures(true);
        assert_eq!(state.take_picture_wish(REAR, 0), None, "a viewfinder of no call makes none");
        let made = Some((capture_turn(Facing::Front, state.pose.quarters()).quarters, false));
        assert_eq!(
            [0, CALL_INTERVAL_NS / 4, CALL_INTERVAL_NS].map(|at_ns| state.take_picture_wish(CALL, at_ns)),
            [made, None, made],
            "one of each frame whose turn has come while they are wanted, turned as the device is held, by the camera that looks at the owner"
        );
        // Frames of a stream `interval_ns` apart, late or early by `jitter_ns` in turn, while pictures are wanted.
        let pictures = |interval_ns: u64, jitter_ns: [i64; 3]| {
            let mut state = State::<u32>::default();
            state.want_call_pictures(true);
            let frames = (1..=600u64).filter(|&frame| {
                let at_ns = (frame * interval_ns).checked_add_signed(jitter_ns[frame as usize % 3]).unwrap();
                state.take_picture_wish(CALL, at_ns).is_some()
            });
            frames.count()
        };
        assert_eq!(pictures(NANOS_PER_SECOND / 60, [0; 3]), 300, "a stream of 60 frames a second, every other frame");
        assert_eq!(
            pictures(CALL_INTERVAL_NS, [-5_000_000, 0, 5_000_000]),
            600,
            "one of 30, every frame, 5 ms late or early"
        );
        assert!(pictures(NANOS_PER_SECOND / 90, [0; 3]) <= 201, "and of 90 no more than 30 a second");
        let (mut made, mut taken) = (CallPicture { at_ns: 7, ..CallPicture::default() }, CallPicture::default());
        assert!(!state.take_call_picture(&mut taken), "nothing made yet");
        state.put_call_picture(&mut made);
        assert!(state.take_call_picture(&mut taken) && taken.at_ns == 7 && !state.take_call_picture(&mut taken));
        state.put_call_picture(&mut made);
        state.end_run(false);
        assert!(!state.take_call_picture(&mut taken), "a run's end lets go of its picture");
        state.want_call_pictures(false);
        assert!(state.take_picture_wish(CALL, NANOS_PER_SECOND).is_none(), "and none is made once none is wanted");
    }

    #[test]
    fn clicks_once_settled() {
        let mut state = State::<u32>::default();
        state.request.set(Some(REAR));
        state.request.clicks += 1;
        assert!(!state.take_click(0));
        assert!(state.take_click(SETTLED) && !state.take_click(SETTLED));
        assert!(!state.should_park(false) && state.should_park(true));
        state.request.set(None);
        assert!(state.should_park(false));
    }

    #[test]
    fn recording_outcomes() {
        let saved = Outcome::Saved("V1.mp4".into());
        let table = [
            (Some(Some("V1.mp4")), None, saved),
            (Some(Some("V1.mp4")), Some(Outcome::Full), Outcome::Full),
            (Some(None), None, Outcome::NoVideo),
            (Some(None), Some(Outcome::Full), Outcome::Full),
            (None, None, Outcome::NoVideo),
            (None, Some(Outcome::Full), Outcome::NoVideo),
        ];
        for (file, interruption, outcome) in table {
            assert_eq!(Outcome::from_recording(file, interruption), outcome);
        }
    }

    #[test]
    fn failed_run_blocks_until_unwanted() {
        const FRONT: Viewfinder = Viewfinder { facing: Facing::Front, ..REAR };
        let mut state = State::<u32>::default();
        state.request.set(Some(REAR));
        assert_eq!((state.serve(), state.is_off()), (Some(Start::Run(REAR)), false));
        state.end_run(true);
        assert_eq!((state.serve(), state.is_off()), (None, true));
        state.request.set(Some(FRONT));
        assert_eq!(state.serve(), None);
        state.request.set(None);
        assert_eq!((state.serve(), state.is_off()), (None, true));
        state.request.set(Some(REAR));
        assert_eq!(state.serve(), Some(Start::Run(REAR)));
        state.end_run(false);
        assert_eq!(state.serve(), Some(Start::Run(REAR)));
    }

    #[test]
    fn reserve_refuses_power_up() {
        let mut state = State::<u32> { is_reserved: true, ..State::default() };
        state.request.set(Some(REAR));
        assert_eq!(state.serve(), Some(Start::Refuse(Outcome::LowBattery)), "under the reserve nothing powers up");
        state.end_run(true);
        assert_eq!((state.serve(), state.is_off()), (None, true), "nor is it tried again while the viewfinder shows");
        state.is_reserved = false;
        assert_eq!(state.serve(), None, "a cable put in waits for the viewfinder to open anew");
        state.request.set(None);
        state.serve();
        state.request.set(Some(REAR));
        assert_eq!(state.serve(), Some(Start::Run(REAR)));
    }

    #[test]
    fn report_taken_once() {
        let mut state = State::<u32>::default();
        state.report.outcome = Some(Outcome::NoPhoto);
        state.report.is_shooting = true;
        assert_eq!(
            state.take_report(None, &mut Vec::new()),
            Report { outcome: Some(Outcome::NoPhoto), is_shooting: true, ..Report::default() }
        );
        assert_eq!(state.take_report(None, &mut Vec::new()), Report::default());
    }

    const FORMAT: PixelFormat = PixelFormat::XRGB8888;

    // A photo of the rear viewfinder as a click takes it: turned as the device is held, and with its review unless it is the robot's alone.
    fn shot(state: &State<u32>, has_review: bool) -> View {
        let review = has_review.then(|| Sight::new(480, Facing::Rear, FORMAT));
        View::Shoot(capture_turn(Facing::Rear, state.pose.quarters()), review)
    }

    #[test]
    fn next_view_steps() {
        let mut state = State::<u32>::default();
        assert_eq!(state.next_view(REAR, FORMAT, SETTLED), View::Leave);
        state.request.set(Some(REAR));
        assert_eq!(state.next_view(REAR, FORMAT, SETTLED), View::Show);
        state.request.click();
        assert_eq!(state.next_view(REAR, FORMAT, 0), View::Show, "a click waits for a stream that has settled");
        assert_eq!(
            state.next_view(REAR, FORMAT, SETTLED),
            shot(&state, true),
            "a photo turned as the device is held, with its review at the viewfinder's side"
        );
    }

    #[test]
    fn look_is_its_photo_once() {
        let mut state = State::<u32>::default();
        state.request.set(Some(REAR));
        state.look();
        assert_eq!(
            state.next_view(REAR, FORMAT, SETTLED),
            shot(&state, false),
            "a look is a click whose photo is the robot's, with no review"
        );
        state.request.click();
        assert_eq!(state.next_view(REAR, FORMAT, SETTLED), shot(&state, true), "once");
        state.look();
        state.set_wanted(None);
        state.set_wanted(Some(REAR));
        state.request.click();
        assert_eq!(
            state.next_view(REAR, FORMAT, SETTLED),
            shot(&state, true),
            "a viewfinder opened anew takes no look left over"
        );
    }

    #[test]
    fn recording_steps() {
        let mut state = State::<u32>::default();
        assert_eq!(state.next_recording(REAR, SETTLED, true), Recording::Leave, "no viewfinder shows");
        state.request.set(Some(REAR));
        assert_eq!(state.next_recording(REAR, SETTLED, false), Recording::Show);
        state.request.click();
        assert_eq!(state.next_recording(REAR, 0, false), Recording::Show, "a click waits for a settled stream");
        assert_eq!(state.next_recording(REAR, SETTLED, false), Recording::Begin(1));
        state.request.click();
        assert_eq!(state.next_recording(REAR, SETTLED, true), Recording::End);
        state.request.click();
        state.request.set(Some(Viewfinder { facing: Facing::Front, ..REAR }));
        assert_eq!(
            state.next_recording(REAR, SETTLED, true),
            Recording::Leave,
            "another viewfinder ends the stream before a click counts"
        );
    }

    #[test]
    fn stream_by_kind() {
        const VIDEO: Viewfinder = Viewfinder { kind: Kind::Video, size: (305, 543), ..REAR };
        assert!(
            REAR.mode() == develop::VIEW_MODE && VIDEO.mode() == recording::MODE,
            "photos stream in preview mode, videos in the recording's"
        );
        assert_eq!(
            (REAR.view(), VIDEO.view()),
            (Square::middle_of(develop::VIEW_MODE.frame()).window(), Window::whole(recording::MODE.frame())),
            "a photo's viewfinder draws the frame's middle square, a video's all of it"
        );
        assert_eq!(
            (Viewfinder { size: (480, 500), ..REAR }.frame_size(), VIDEO.frame_size()),
            ((480, 480), (305, 543)),
            "a photo's picture is square by its width, a video's as laid out"
        );
        let call = Viewfinder { is_calling: true, size: (150, 150), ..REAR };
        assert_eq!(
            (call.view(), call.frame_size(), call.mode()),
            (Window::whole(Mode::HighSpeed.frame()), (150, 200), Mode::HighSpeed),
            "a call's viewfinder streams the high speed mode and draws all of its frame, upright and as wide as laid out"
        );
        assert_eq!(
            (call.pace(), REAR.pace(), VIDEO.pace()),
            (Pace::CALL, Pace::LIVE, Pace::LIVE),
            "at the rate the far side is sent pictures, every other at a viewfinder's"
        );
        let planes = Planes::tight(call.mode().frame(), &Tone::new(Auto::live().recipe())).map(|planes| planes.size());
        assert_eq!(planes, Some(CALL_SIZE), "a call's frame is the far side's picture, pixel for pixel");
    }

    #[test]
    fn photo_names_taken() {
        struct Files(Vec<&'static str>, Vec<String>);
        impl Names for &mut Files {
            type Error = ();
            fn has(&mut self, name: &str) -> Result<bool, ()> {
                self.1.push(name.into());
                Ok(self.0.contains(&name))
            }
        }
        let mut darkroom: Darkroom<u32> = Darkroom::default();
        let sight = Sight::new(480, Facing::Rear, FORMAT);
        let turn = Turn { quarters: 1, is_mirrored: false };
        darkroom.put(Job::new(7, Auto::live().still(), turn, Some(Taken { name: "waits".into(), sight }), 0));
        let mut files = Files(alloc::vec!["kept_original.jpg"], Vec::new());
        let mut names = darkroom.names(&mut files);
        assert_eq!(
            [names.has("waits"), names.has("kept"), names.has("free")],
            [Ok(true), Ok(true), Ok(false)],
            "taken by a burst that waits, by a file, by neither"
        );
        assert_eq!(
            files.1,
            ["kept_original.jpg", "free_original.jpg"],
            "the files are asked for a photo's original, and not at all for a burst that waits"
        );
        assert_eq!(
            [check_burst(8, 8), check_burst(9, 8)],
            [Ok(()), Err("a burst of 9 frames in a buffer of 8".into())],
            "a burst lies in the slots of its buffer"
        );
    }

    #[test]
    fn run_counts_and_lines() {
        let mut stats = Stats::default();
        stats.note_skipped();
        stats.note_photo();
        stats.note_refreshed(Duration::from_micros(30));
        let looks = (0..30).filter(|_| stats.note_frame(Measure::default(), Duration::from_micros(90))).count();
        assert_eq!((looks, stats.is_room_look_due(true), stats.is_room_look_due(false)), (1, true, false));
        let line = stats.to_run_line(REAR, Duration::from_millis(2500), &Auto::live(), None);
        assert!(line.starts_with(
            "camera: Photo Rear for 2.5 s, 30 frames drawn in 90 us each (1 us of it cache refresh, 90 us at most), 1 skipped, 1 photos, exposure 758 lines at gain 64/64 lifted 1024/1024, "
        ), "{line}");
        assert!(line.ends_with(", error bits 0xffffffff"), "{line}");
        let first = stats.to_zone_lines().next().unwrap();
        assert!(first.starts_with("camera: zones of the last frame, row 0: 0/0/0"), "{first}");
        let first = stats.to_photo_zone_lines("p").next().unwrap();
        assert!(first.starts_with("camera: photo p, zones of the frame before it, row 0: 0/0/0"), "{first}");
    }

    #[test]
    fn photo_lines() {
        let ms = Duration::from_millis;
        let [taken, light] = to_taken_lines("p", &Auto::live().still(), 3, ms(250), &Auto::live());
        assert!(
            taken.starts_with("camera: photo p, 2 frames of 758 lines at gain 64/64 with gains ")
                && taken.ends_with(", 1024/1024 below the light, 3 quarter turns, taken in 250 ms"),
            "{taken}"
        );
        assert!(light.starts_with("camera: photo p, ") && light.contains(", matrix "), "{light}");
        let developed = Developed {
            name: "p",
            length: 9,
            reference: 1,
            noise: (0.5, 2.25),
            times: [ms(300), ms(900), ms(1300), ms(1350), ms(1360)],
        };
        assert_eq!(
            developed.to_log_line(&to_saved_text(ms(4), &to_handed_text(5, ms(2)))),
            "camera: photo p, 9 bytes: begun after 300 ms, merged on frame 1 in 600 ms (noise 0.500 per code and 2.2), developed in 400 ms, sharpened in 50 ms, encoded in 10 ms, saved in 4 ms, handed to the pixel cam as 5 bytes in 2 ms"
        );
        assert_eq!(to_unhanded_text("full"), "not handed to the pixel cam: full");
        assert_eq!(
            to_redraw_line("p", &to_handed_text(5, ms(2))),
            "camera: photo p, for a redraw: handed to the pixel cam as 5 bytes in 2 ms"
        );
        let look = Developed { name: LOOK, ..developed };
        assert!(
            look.to_log_line(LOOKED_TEXT).starts_with("camera: photo for the robot, 9 bytes: ")
                && look.to_log_line(LOOKED_TEXT).ends_with(", encoded in 10 ms, handed to the robot"),
            "a photo for the robot is saved nowhere"
        );
        let taken = Taken { name: "p".into(), sight: Sight::new(480, Facing::Rear, PixelFormat::XRGB8888) };
        assert_eq!((to_log_name(Some(&taken)), to_log_name(None)), ("p", LOOK));
    }

    #[test]
    fn picture_for_its_viewfinder_alone() {
        const VIDEO: Viewfinder = Viewfinder { kind: Kind::Video, size: (305, 543), ..REAR };
        const SCANS: Viewfinder = Viewfinder { is_scanning: true, size: (464, 464), ..REAR };
        let mut state = State::<u32>::default();
        let mut picture = alloc::vec![9u32; 4];
        state.put_picture(&mut alloc::vec![1; 4], REAR, 0, true);
        assert!(
            state.take_report(Some(VIDEO), &mut picture).picture.is_none() && picture == [9; 4],
            "a photo viewfinder's picture has another shape"
        );
        state.put_picture(&mut alloc::vec![2; 4], REAR, 0, true);
        assert!(state.take_report(None, &mut picture).picture.is_none(), "no viewfinder, no picture");
        state.put_picture(&mut alloc::vec![3; 4], VIDEO, 5, false);
        assert_eq!(
            (state.take_report(Some(VIDEO), &mut picture).picture, &picture[..]),
            (Some(PictureInfo { viewfinder: VIDEO, at_ns: 5, is_in_step: false }), &[3u32; 4][..]),
            "the one drawn for the viewfinder that shows is swapped in, with how it was handed over"
        );
        state.put_picture(&mut alloc::vec![4; 4], REAR, 0, true);
        assert!(
            state.take_report(Some(SCANS), &mut picture).picture.is_none(),
            "nor is the photo viewfinder's in the key's window, whose square is another"
        );
        state.put_picture(&mut alloc::vec![5; 4], VIDEO, 0, true);
        assert!(
            state.take_report(Some(Viewfinder { size: (287, 511), ..VIDEO }), &mut picture).picture.is_none()
                && picture == [3; 4],
            "nor one drawn at another size"
        );
    }

    #[test]
    fn reopened_viewfinder_drops_picture() {
        const FRONT: Viewfinder = Viewfinder { facing: Facing::Front, ..REAR };
        let mut state = State::<u32>::default();
        state.set_wanted(Some(REAR));
        state.put_picture(&mut alloc::vec![7; 4], REAR, 0, true);
        state.set_wanted(None);
        state.set_wanted(Some(REAR));
        assert!(
            state.take_report(Some(REAR), &mut Vec::new()).picture.is_none(),
            "a viewfinder opened anew shows nothing of the last"
        );
        state.put_picture(&mut alloc::vec![8; 4], REAR, 0, true);
        state.set_wanted(Some(FRONT));
        state.set_wanted(Some(REAR));
        assert!(
            state.take_report(Some(REAR), &mut Vec::new()).picture.is_some(),
            "turning the camera opens nothing anew"
        );
    }

    #[test]
    fn darkroom_queue_and_buffers() {
        let mut darkroom: Darkroom<u32> = Darkroom::default();
        assert!(darkroom.take_work().is_none() && darkroom.is_idle());
        assert!(matches!(darkroom.supply(2), Some(Supply::Make)) && matches!(darkroom.supply(2), Some(Supply::Make)));
        assert!(darkroom.supply(2).is_none(), "two buffers alive, as many as the budget: the next waits");
        let still = Auto::live().still();
        let turn = Turn { quarters: 1, is_mirrored: false };
        let sight = Sight::new(480, Facing::Rear, PixelFormat::XRGB8888);
        darkroom.put(Job::new(7, still, turn, Some(Taken { name: "a".into(), sight }), 0));
        darkroom.put(Job::new(8, still, turn, None, 0));
        assert!(darkroom.holds("a") && !darkroom.holds("b") && !darkroom.is_idle());
        darkroom.want_art("z".into(), sight);
        darkroom.want_copy("c".into());
        assert!(
            matches!(darkroom.take_work(), Some(Work::Art(photo, Sight { side: 480, .. })) if photo == "z"),
            "art a review waits for goes first"
        );
        assert!(
            matches!(darkroom.take_work(), Some(Work::Copy(photo)) if photo == "c"),
            "then a redraw's copy, which is quick too"
        );
        let Some(Work::Develop(job)) = darkroom.take_work() else { panic!("a burst to develop") };
        assert!((job.burst, job.session()) == (7, 0) && darkroom.holds("a"), "taken, and its name is still held");
        assert_eq!(darkroom.give_back(job.burst, job.session()), None, "the same session's buffer is kept");
        assert!(
            !darkroom.holds("a") && matches!(darkroom.supply(2), Some(Supply::Spare(7))),
            "and serves the next burst"
        );
        darkroom.keep(7);
        assert!(
            matches!(darkroom.supply(2), Some(Supply::Spare(7))),
            "a buffer no burst went into is kept for the next"
        );
        let Some(Work::Develop(late)) = darkroom.take_work() else { panic!("the second burst") };
        assert_eq!(darkroom.end_session(), Vec::<u32>::new(), "no spare left over");
        assert_eq!(darkroom.give_back(late.burst, late.session()), Some(8), "an earlier session's buffer is let go of");
        assert!(matches!(darkroom.supply(2), Some(Supply::Make)), "which makes room for a new one");
        assert!(!darkroom.is_idle(), "the encoder is held while nothing has let it go");
        assert!(
            matches!(darkroom.take_work(), Some(Work::Release)) && darkroom.take_work().is_none() && darkroom.is_idle(),
            "nothing waits: it is let go of, once"
        );
        darkroom.want_art("z".into(), sight);
        assert!(
            matches!(darkroom.take_work(), Some(Work::Art(..))) && darkroom.take_work().is_none(),
            "art claims nothing, so leaves nothing to let go of"
        );
        assert_eq!((to_burst_count(9_990_144 * 8), to_burst_count(0), to_burst_count(usize::MAX)), (4, 352_321_536, 1));
    }

    #[test]
    fn run_end_lets_go() {
        let mut state = State::<u32>::default();
        state.request.set(Some(REAR));
        assert_eq!(state.serve(), Some(Start::Run(REAR)));
        state.put_picture(&mut alloc::vec![1; 4], REAR, 0, true);
        assert!(matches!(state.supply(REAR, 1), Some(Supply::Make)));
        state.darkroom.keep(7);
        assert_eq!(state.end_run(false), [7], "the session's spare is let go of with it");
        assert!(state.is_off());
        assert!(
            state.take_report(Some(REAR), &mut Vec::new()).picture.is_none(),
            "and so is the picture kept for the screen"
        );
        assert!(matches!(state.supply(REAR, 1), Some(Supply::Make)), "the next run makes its buffers anew");
    }

    #[test]
    fn supply_wait_ends_with_viewfinder() {
        let mut state = State::<u32>::default();
        state.request.set(Some(REAR));
        assert!(matches!(state.supply(REAR, 1), Some(Supply::Make)));
        assert!(state.supply(REAR, 1).is_none(), "the budget is spent: the burst waits while the viewfinder shows");
        state.set_wanted(Some(Viewfinder { facing: Facing::Front, ..REAR }));
        assert!(matches!(state.supply(REAR, 1), Some(Supply::Leave)), "and waits no more once it is left");
        state.darkroom.keep(7);
        assert!(matches!(state.supply(REAR, 1), Some(Supply::Spare(7))), "a free buffer serves whatever shows");
    }

    #[test]
    fn scanner_steps_and_rest() {
        const SCANS: Viewfinder = Viewfinder { is_scanning: true, size: (464, 464), ..REAR };
        let mut state = State::<u32>::default();
        let mut picture = Vec::new();
        assert!(state.next_scan(&mut picture).is_none() && !state.scan.is_waiting(), "no viewfinder scans");
        assert!(!state.scan.offer(&[1; 4], 2, PixelFormat::XRGB8888), "nothing is handed over before it waits");
        state.set_wanted(Some(SCANS));
        assert!(state.next_scan(&mut picture).is_none() && state.scan.is_waiting(), "it waits for a picture");
        assert!(state.scan.offer(&[1; 4], 2, PixelFormat::XRGB8888) && !state.scan.is_waiting());
        assert!(!state.scan.offer(&[2; 4], 2, PixelFormat::XRGB8888), "one at a time");
        assert_eq!(state.next_scan(&mut picture), Some(ScanStep::Read(2, PixelFormat::XRGB8888)));
        assert_eq!(picture, [1; 4]);
        state.set_wanted(None);
        assert_eq!(state.next_scan(&mut picture), Some(ScanStep::Rest), "with no viewfinder scanning it lets go");
        assert!(state.next_scan(&mut picture).is_none(), "once");
        state.set_wanted(Some(SCANS));
        state.next_scan(&mut picture);
        state.scan.offer(&[3; 4], 2, PixelFormat::XRGB8888);
        state.set_wanted(None);
        assert!(
            state.next_scan(&mut picture) == Some(ScanStep::Rest) && !state.scan.has_picture(),
            "a picture handed over and not read goes with the rest"
        );
    }

    #[test]
    fn uncopied_and_code_reported() {
        let mut state = State::<u32>::default();
        state.note_uncopied();
        state.report.code = Some(Code(alloc::vec![b'k'; 3]));
        let report = state.take_report(None, &mut Vec::new());
        assert_eq!((report.note, report.code), (Some(Note::NoPixels), Some(Code(alloc::vec![b'k'; 3]))));
        assert_eq!(alloc::format!("{:?}", Code(alloc::vec![b'k'; 3])), "Code(3 bytes)", "a code tells its size alone");
    }

    #[test]
    fn review_mirrors_front() {
        let sight = |facing| Sight::new(480, facing, PixelFormat::XRGB8888).orientation;
        assert_eq!(
            (sight(Facing::Front), sight(Facing::Rear)),
            (jpeg::MIRRORED, jpeg::UPRIGHT),
            "a review shows the front camera's photo as its viewfinder showed the scene"
        );
        assert_eq!(jpeg::oriented(alloc::vec![1, 2, 3, 4], 2, sight(Facing::Front)), (alloc::vec![2, 1, 4, 3], 2));
    }

    #[test]
    fn pause_rests_stream() {
        let mut state = State::<u32>::default();
        state.request.set(Some(REAR));
        state.request.click();
        assert!(state.set_paused(true) && !state.set_paused(true), "told once");
        assert_eq!(
            (state.after_rest(REAR), state.next_view(REAR, FORMAT, 0)),
            (Some(View::Show), View::Show),
            "the click that asked for the rest waits for a stream that has settled"
        );
        assert_eq!(state.next_view(REAR, FORMAT, SETTLED), shot(&state, true), "and takes its photo before the rest");
        assert_eq!(
            (state.next_view(REAR, FORMAT, SETTLED), state.after_rest(REAR)),
            (View::Pause, None),
            "then the photo just taken shows, and the stream rests"
        );
        state.look();
        assert_eq!(state.after_rest(REAR), Some(View::Show), "a photo for the robot ends the rest");
        assert_eq!(state.next_view(REAR, FORMAT, SETTLED), shot(&state, false), "its click kept for the stream");
        state.set_paused(false);
        assert_eq!(
            (state.next_view(REAR, FORMAT, SETTLED), state.after_rest(REAR)),
            (View::Show, Some(View::Show)),
            "back to the stream"
        );
        state.request.set(None);
        assert_eq!(state.after_rest(REAR), Some(View::Leave));
    }

    #[test]
    fn look_rests_until_developed() {
        let mut state = State::<u32>::default();
        state.request.set(Some(REAR));
        let (still, turn) = (Auto::live().still(), Turn { quarters: 1, is_mirrored: false });
        let taken = Taken { name: "saved".into(), sight: Sight::new(480, Facing::Rear, FORMAT) };
        state.darkroom.put(Job::new(6, still, turn, Some(taken), 0));
        assert_eq!(state.after_rest(REAR), Some(View::Show), "a photo for the roll rests nothing by itself");
        state.darkroom.put(Job::new(7, still, turn, None, 0));
        assert_eq!(
            (state.after_rest(REAR), state.next_view(REAR, FORMAT, SETTLED)),
            (None, View::Pause),
            "the stream rests while the robot's burst waits"
        );
        let Some(Work::Develop(saved)) = state.darkroom.take_work() else { panic!("the roll's burst") };
        assert_eq!(state.darkroom.give_back(saved.burst, saved.session()), None);
        let Some(Work::Develop(look)) = state.darkroom.take_work() else { panic!("the robot's burst") };
        assert_eq!(state.after_rest(REAR), None, "and while it is developed");
        state.request.click();
        assert_eq!(state.after_rest(REAR), Some(View::Show), "a click that waits ends the rest");
        assert_eq!(state.next_view(REAR, FORMAT, SETTLED), shot(&state, true), "and takes its photo");
        assert_eq!(state.after_rest(REAR), None, "behind which the rest goes on");
        assert_eq!(state.darkroom.give_back(look.burst, look.session()), None);
        assert_eq!(state.after_rest(REAR), Some(View::Show), "developed, the stream shows again");
    }
}

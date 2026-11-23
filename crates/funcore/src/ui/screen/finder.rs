use super::Screen;
use crate::domain::camera::{self, Development, Outcome, Sight, Viewfinder};
use crate::domain::cause::Cause;
use crate::domain::gallery::Kind;
use crate::domain::magic::{self, Progress, Report};
use crate::gfx::key::Ids;
use crate::gfx::motion::Edge;
use crate::gfx::store::{SOLE, SoleSpot, Store};
use crate::gfx::{Picture, Rect};
use crate::hw::camera::swivel::Facing;
use crate::hw::power::battery;
use crate::ui::robot::Reaction;
use crate::ui::words::{cause_word, outcome_word};
use crate::ui::{Action, NOTE_NS, Ui};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const VIDEO_WIDTH: usize = 9;
const VIDEO_HEIGHT: usize = 16;
// Blocks of the row for a burst's development: merged, then saved, before the pixel cam's.
const DEVELOP_BLOCKS: usize = 2;
// Blocks of the whole row.
pub(crate) const ROW_BLOCKS: usize = DEVELOP_BLOCKS + magic::BLOCKS;

pub(crate) struct Painting {
    pub(crate) drawing: Option<Progress>,
    pub(crate) development: Option<Development>,
    pub(crate) full_until: Option<u64>,
    pub(crate) row_due: Option<u64>,
    pub(crate) stroke_count: usize,
    pub(crate) blocks: [Edge; ROW_BLOCKS],
    pub(crate) row: Edge,
    pixel_changes: u32,
}

impl Painting {
    pub(crate) const fn new() -> Painting {
        Painting {
            drawing: None,
            development: None,
            full_until: None,
            row_due: None,
            stroke_count: 0,
            blocks: [Edge::still(0.0); ROW_BLOCKS],
            row: Edge::still(0.0),
            pixel_changes: 0,
        }
    }

    // The row's lit blocks: the pixel cam's past the development's, or the development's alone while no photo is redrawn.
    pub(crate) fn lit_blocks(&self, now_ns: u64) -> usize {
        match (self.drawing, self.development) {
            (Some(drawing), _) => DEVELOP_BLOCKS + drawing.lit_blocks(now_ns),
            (None, Some(Development::Finishing)) => 1,
            (None, _) => 0,
        }
    }
}

// The way the camera looks after the wheel's turn: down at whoever holds the device, up away from them.
pub(super) const fn turned(facing: Facing, detents: i32) -> Facing {
    match detents.signum() {
        1 => Facing::Front,
        -1 => Facing::Rear,
        _ => facing,
    }
}

// The photo just taken, in the stream's place: the frame it was taken at, then its developed picture, then its pixel art, each unfolding over the last.
pub(crate) struct Review {
    // The photo's name, once the camera has taken its burst: only its pictures show.
    photo: Option<String>,
    sight: Sight,
    pub(crate) store: Store<Picture>,
}

pub(crate) struct Finder {
    pub(super) facing: Facing,
    pub(crate) picture: Vec<u32>,
    pub(crate) id: u64,
    pub(crate) shutter: Edge,
    pub(crate) recording: Option<u64>,
    // A photo for the robot is on its way, which opens the conversation as it comes.
    pub(super) is_looking: bool,
    pub(crate) review: Option<Review>,
}

impl Finder {
    pub(crate) fn new(ids: &Ids) -> Finder {
        let shutter = Edge::still(0.0);
        Finder {
            facing: Facing::Rear,
            picture: Vec::new(),
            id: ids.next(),
            shutter,
            recording: None,
            is_looking: false,
            review: None,
        }
    }
}

impl Ui {
    pub(super) fn open_finder(&mut self, kind: Kind) {
        self.finder.picture.clear();
        (self.note, self.finder.shutter, self.finder.is_looking) = (None, Edge::still(0.0), false);
        self.finder.review = None;
        self.screen = Some(Screen::Finder(kind));
    }

    // The photo viewfinder's frame at the click stays as the review of the photo it took.
    pub(super) fn begin_review(&mut self) {
        let (width, _) = self.finder_size(Kind::Photo);
        let mut store = self.photo_store();
        let frame = Picture { words: self.finder.picture.clone(), width };
        store.put(&self.ids, &SoleSpot(0), (SOLE, None), frame, true);
        let sight = Sight::new(width, self.finder.facing, self.format);
        self.finder.review = Some(Review { photo: None, sight, store });
    }

    /// True while the photo just taken shows in the photo viewfinder's place.
    pub fn is_reviewing(&self) -> bool {
        self.finder.review.is_some() && self.screen == Some(Screen::Finder(Kind::Photo))
    }

    /// Of the photos whose pixel art was kept, the one whose review shows, with how its art shows.
    pub fn review_art(&self, kept: &[String]) -> Option<(String, Sight)> {
        let review = self.finder.review.as_ref().filter(|_| self.is_reviewing())?;
        let photo = review.photo.as_ref().filter(|photo| kept.contains(photo))?;
        Some((photo.clone(), review.sight))
    }

    // What the viewfinders hold is let go of once they show no more: the picture where neither a viewfinder nor the key's window's scan shows, the review where the photo viewfinder does not.
    pub(crate) fn let_go_finder(&mut self) {
        if !matches!(self.screen, Some(Screen::Finder(_))) && !self.does_scan_show() {
            self.finder.picture = Vec::new();
        }
        if self.screen != Some(Screen::Finder(Kind::Photo)) {
            self.finder.review = None;
        }
    }

    /// Notes the name of the photo whose burst the camera took.
    pub fn note_taken(&mut self, photo: String) {
        if let Some(review) = self.finder.review.as_mut().filter(|review| review.photo.is_none()) {
            review.photo = Some(photo);
        }
    }

    /// Takes a picture of the photo just taken, which unfolds over the last where the review shows that photo; true if the screen changed.
    #[must_use]
    pub fn take_review(&mut self, review: camera::Review) -> bool {
        let is_shown = self.is_reviewing();
        let Some(own) = self.finder.review.as_mut().filter(|own| own.photo.as_deref() == Some(review.photo.as_str()))
        else {
            return false;
        };
        let picture = Picture { words: review.picture, width: review.width };
        own.store.put(&self.ids, &SoleSpot(0), (SOLE, None), picture, false);
        is_shown
    }

    pub(crate) fn advance_review(&mut self, now_ns: u64) {
        if let Some(review) = &mut self.finder.review {
            review.store.advance(now_ns, &SoleSpot(0));
        }
    }

    pub(crate) fn is_review_resting(&self) -> bool {
        self.finder.review.as_ref().is_none_or(|review| review.store.is_resting())
    }

    // A touch on the robot in the photo viewfinder: a photo for the robot alone.
    pub(super) fn shoot_for_robot(&mut self, now_ns: u64) -> Action {
        self.finder.is_looking = true;
        self.react(Reaction::Flash, now_ns);
        Action::Look
    }

    /// True while the battery keeps the camera from powering up.
    pub fn is_camera_reserved(&self) -> bool {
        self.charge.reading.is_some_and(|(level, supply)| battery::is_camera_reserve(level, supply))
    }

    /// The viewfinder to run while awake: the one on the screen, else a call's.
    pub fn camera_request(&self) -> Option<Viewfinder> {
        self.visible_camera().filter(|_| self.is_awake()).or_else(|| self.call_camera())
    }

    /// The viewfinder whose pictures the screen takes and the picture they are swapped into: a call's own while its camera runs, else the finder's.
    pub fn camera_picture_mut(&mut self) -> (Option<Viewfinder>, &mut Vec<u32>) {
        match self.call_camera() {
            Some(call) => (Some(call), self.own_picture_mut()),
            None => (self.visible_camera(), self.finder_picture_mut()),
        }
    }

    /// Notes a new picture of the camera's stream: whether the screen changed, and whether its frames keep the stream's pace, as a viewfinder's do.
    #[must_use]
    pub fn note_camera_frame(&mut self) -> (bool, bool) {
        if self.call_camera().is_some() {
            return (self.note_own_frame(), false);
        }
        self.note_finder_frame();
        (true, true)
    }

    /// The viewfinder on the screen, awake or asleep.
    pub fn visible_camera(&self) -> Option<Viewfinder> {
        match self.screen {
            Some(Screen::Finder(kind)) => Some(Viewfinder {
                kind,
                facing: self.finder.facing,
                is_scanning: false,
                is_calling: false,
                size: self.finder_size(kind),
            }),
            Some(Screen::Key) => self.key_camera(),
            _ => None,
        }
    }

    pub(crate) fn finder_room(&self) -> Rect {
        let (bar, ruler) = (self.ruler.bar(), &self.ruler);
        Rect::new(0, bar, ruler.width, (ruler.height - bar).max(0))
    }

    pub(crate) fn finder_size(&self, kind: Kind) -> (usize, usize) {
        let (width, height) = (self.ruler.width.max(0) as usize, self.finder_room().h as usize);
        match kind {
            Kind::Photo => (width.min(height), width.min(height)),
            Kind::Video => {
                let video_height = height.min(width * VIDEO_HEIGHT / VIDEO_WIDTH);
                (video_height * VIDEO_WIDTH / VIDEO_HEIGHT, video_height)
            }
        }
    }

    /// The viewfinder's picture, into which the camera swaps its newest one.
    pub const fn finder_picture_mut(&mut self) -> &mut Vec<u32> {
        &mut self.finder.picture
    }

    /// Notes a new viewfinder picture.
    pub fn note_finder_frame(&mut self) {
        self.finder.id = self.ids.next();
    }

    /// Notes that the camera's stream stopped for a photo; true if the screen changed.
    #[must_use]
    pub fn shut_finder(&mut self) -> bool {
        // A photo for the robot leaves the frame it stopped on: no stream follows until it comes.
        if self.finder.review.is_some() || self.finder.is_looking {
            return false;
        }
        let was_visible = !self.finder.picture.is_empty() && self.visible_camera().is_some();
        self.finder.picture.clear();
        was_visible
    }

    /// Takes the outcome of a photo, a recording or a camera run; true if the screen changed.
    #[must_use]
    pub fn take_outcome(&mut self, outcome: &Outcome, now_ns: u64) -> bool {
        // A photo for the robot that failed ends the wait for it; one saved is another photo.
        self.finder.is_looking &= matches!(outcome, Outcome::Saved(_));
        if *outcome == Outcome::NoPhoto {
            self.finder.review = None;
        }
        let word = outcome_word(outcome);
        let text = match outcome {
            Outcome::Saved(name) => format!("{word} {}", name.to_uppercase()),
            _ => word.into(),
        };
        // A failure stays while the viewfinder it struck shows; a call's camera, which runs with none, is told for a note's time.
        let is_failure = matches!(outcome, Outcome::TooWarm | Outcome::LowBattery | Outcome::Failed);
        self.set_note(text, is_failure && matches!(self.screen, Some(Screen::Finder(_))), now_ns)
    }

    /// True while the pixel cam should send what waits.
    pub fn are_photos_wanted(&self) -> bool {
        self.is_awake() && self.do_photos_show()
    }

    pub(crate) fn do_photos_show(&self) -> bool {
        matches!(self.screen, Some(Screen::Finder(Kind::Photo) | Screen::Grid(Kind::Photo) | Screen::Roll(Kind::Photo)))
    }

    /// Takes the pixel cam's progress; true if the screen changed.
    #[must_use]
    pub fn set_drawing(&mut self, drawing: Option<Progress>) -> bool {
        core::mem::replace(&mut self.painting.drawing, drawing) != drawing && self.do_photos_show()
    }

    /// Takes how far the darkroom has got with a burst; true if the screen changed.
    #[must_use]
    pub fn take_development(&mut self, development: Development) -> bool {
        let development = (development != Development::Done).then_some(development);
        core::mem::replace(&mut self.painting.development, development) != development && self.do_photos_show()
    }

    /// Notes a stored pixel picture.
    pub fn note_pixels_kept(&mut self, now_ns: u64) {
        self.painting.full_until = Some(now_ns + NOTE_NS);
        self.react(Reaction::Proud, now_ns);
        self.notice(false);
    }

    /// The count of the pixel cam's photo changes the screens heard of.
    pub const fn pixel_changes(&self) -> u32 {
        self.painting.pixel_changes
    }

    /// Takes the pixel cam's report; true if the screen changed.
    #[must_use]
    pub fn take_magic_report(&mut self, report: &Report, now_ns: u64) -> bool {
        let has_changed = report.changes_rolls();
        self.painting.pixel_changes = self.painting.pixel_changes.wrapping_add(has_changed.into());
        let has_progress = self.set_drawing(report.progress);
        let is_kept = !report.kept.is_empty();
        if is_kept {
            self.note_pixels_kept(now_ns);
        }
        let has_failed = report.cause.is_some_and(|cause| self.note_pixels_failed(cause, now_ns));
        has_progress || is_kept || has_failed
    }

    /// Shows why a photo was not redrawn; true if the screen changed.
    #[must_use]
    pub fn note_pixels_failed(&mut self, cause: Cause, now_ns: u64) -> bool {
        self.react(Reaction::Slump, now_ns);
        self.set_note(cause_word(cause).into(), false, now_ns)
    }

    pub(super) fn turn_camera(&mut self, detents: i32) {
        self.finder.facing = turned(self.finder.facing, detents);
    }

    /// Sets whether the camera records; true if the screen changed.
    #[must_use]
    pub fn set_recording(&mut self, is_on: bool, now_ns: u64) -> bool {
        if is_on == self.finder.recording.is_some() {
            return false;
        }
        self.finder.recording = is_on.then_some(now_ns);
        self.screen == Some(Screen::Finder(Kind::Video))
    }
}

//! The state the main loop and the gallery's thread share.

use super::player::{Ask, Player, VideoStep};
use super::roll::RoundThumbnail;
use super::show::Show;
use super::{Errand, Kind, Note, RollRequest, Sending};
use crate::base::handoff::{Handover, Request};
use crate::domain::magic::Order;
use crate::domain::tally::{HomeView, Tally};
use crate::gfx::Picture;
use crate::input::pose::Pose;
use alloc::string::String;
use alloc::vec::Vec;

/// The thread's job.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Job {
    Roll(Kind),
    Count,
}

/// The result of a pass over an open photo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Open {
    Closed,
    // The rolls changed: the roll reads its files again.
    Stale,
    Item(RollRequest, usize),
}

type CountKey = Option<(u32, u32, u64)>;

/// The gallery's report for the screen.
#[derive(Default)]
pub struct Report {
    /// The item of a new picture swapped in, and its file as the camera took it.
    pub picture: Option<(usize, String)>,
    pub note: Option<Note>,
    pub is_playing: bool,
    /// The item count of a roll.
    pub item_count: Option<(Kind, usize)>,
    /// Thumbnails the screen has not taken.
    pub thumbnails: Vec<RoundThumbnail>,
    /// The tally of the rolls for home.
    pub tally: Option<Tally>,
    /// The pixel cam's orders.
    pub orders: Vec<Order>,
    /// The item a stamp asked to send on Signal.
    pub send: Option<Sending>,
    /// The photos a stamp asked to redraw anew, whose copies for the pixel cam the darkroom makes.
    pub copies: Vec<String>,
}

/// The state of the main loop and the thread.
#[derive(Default)]
pub struct State {
    pub request: Request<RollRequest>,
    pub pose: Pose,
    /// The playing video.
    pub show: Show,
    generation: u32,
    pixel_changes: u32,
    errands: Vec<Errand>,
    /// The photos with a new pixel picture.
    pub redraws: Vec<String>,
    pub home: HomeView,
    pub report: Report,
    view: Handover<Picture, (Kind, usize)>,
    // The file, as the camera took it, of the item whose picture `view` holds.
    view_file: String,
    last_count: CountKey,
}

impl State {
    /// A new value whenever the rolls change.
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// The request for an item of that kind shown large, and the item.
    pub fn open_item(&self, kind: Kind, count: usize) -> Option<(RollRequest, usize)> {
        let request = self.request.target.filter(|request| request.kind == kind)?;
        Some((request, request.open_item.filter(|index| *index < count)?))
    }

    /// The result of a pass over the open photo of a roll of `count` items, read at `generation`.
    pub fn next_open(&self, count: usize, generation: u32) -> Open {
        match self.open_item(Kind::Photo, count) {
            None => Open::Closed,
            Some(_) if self.generation != generation => Open::Stale,
            Some((request, item)) => Open::Item(request, item),
        }
    }

    /// What a pass over a roll of that kind takes: its request, the generation and the errands.
    pub fn take_pass(&mut self, kind: Kind) -> (Option<RollRequest>, u32, Vec<Errand>) {
        let request = self.request.target.filter(|request| request.kind == kind);
        (request, self.generation, core::mem::take(&mut self.errands))
    }

    /// The screen's request of the video `request` shows.
    pub fn take_ask(&mut self, request: RollRequest) -> Ask {
        Ask { is_wanted: self.request.target == Some(request), clicks: self.request.take_clicks(), pose: self.pose }
    }

    fn next_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Takes the screen's request and the pixel cam's change count; true where either differs. A newly opened roll drops the last roll's picture.
    pub fn set_wanted(&mut self, request: Option<RollRequest>, pixel_changes: u32) -> bool {
        if self.request.is_opened_by(request) {
            self.view.mark_stale();
        }
        let has_new_pixels = self.pixel_changes != pixel_changes;
        self.pixel_changes = pixel_changes;
        if has_new_pixels {
            self.next_generation();
        }
        self.request.set(request) || has_new_pixels
    }

    /// Adds an icon's errand; the roll reads its files again after it.
    pub fn add_errand(&mut self, errand: Errand) {
        self.errands.push(errand);
        self.next_generation();
    }

    /// Reports a roll's item count and why it shows nothing.
    pub fn set_count(&mut self, kind: Kind, count: usize, note: Option<Note>) {
        self.report.item_count = Some((kind, count));
        self.report.note = note.or(self.report.note);
    }

    /// Swaps in a picture of an item, with the item's file as the camera took it; `picture` then holds the old one to draw over.
    pub fn put_picture(&mut self, picture: &mut Picture, kind: Kind, (item, file): (usize, &str)) {
        self.view.put(picture, (kind, item));
        self.view_file.clear();
        self.view_file.push_str(file);
    }

    /// Swaps the newest picture of `target` into `picture` and takes the report; a picture of another item is dropped.
    pub fn take_report(&mut self, target: Option<(Kind, usize)>, picture: &mut Picture) -> Report {
        let item = self.view.take(picture, |view| target == Some(*view)).map(|(_, item)| item);
        let shown = item.map(|item| (item, self.view_file.clone()));
        Report { picture: shown, is_playing: self.show.is_playing, ..core::mem::take(&mut self.report) }
    }

    /// True once the screen wants another than `request`, or the rolls changed since `generation`.
    pub fn has_moved(&self, request: RollRequest, generation: u32) -> bool {
        self.request.target != Some(request) || self.generation != generation
    }

    /// Drops the clicks on a video that cannot show; true once the screen wants another.
    pub fn drop_clicks(&mut self, request: RollRequest) -> bool {
        self.request.take_clicks();
        self.request.target != Some(request)
    }

    /// What a video on show acts on next; none while it waits.
    pub fn next_step(&mut self, request: RollRequest, player: &Player, is_done: bool) -> Option<VideoStep> {
        let has_shown_all = player.is_playing && is_done && self.show.mark_last();
        if self.request.target != Some(request) || self.request.clicks > 0 || self.pose != player.pose {
            return Some(VideoStep::Request);
        }
        match (player.is_playing, is_done) {
            (false, _) => None,
            (true, true) => has_shown_all.then_some(VideoStep::End),
            (true, false) => Some(self.show.missing_count()).filter(|count| *count > 0).map(VideoStep::Draw),
        }
    }

    /// The thread's next job: the requested roll, else a count for home.
    pub fn take_job(&mut self) -> Option<Job> {
        if let Some(request) = self.request.serve() {
            return Some(Job::Roll(request.kind));
        }
        let key = self.home.count_key().map(|(view, day)| (view, self.generation, day));
        let is_due = key.is_some() && self.last_count != key;
        self.request.is_busy |= is_due;
        is_due.then(|| {
            self.last_count = key;
            Job::Count
        })
    }

    /// Notes that the thread holds nothing; an unreported count and the picture kept for the screen are dropped.
    pub fn rest(&mut self) {
        self.request.is_busy = false;
        self.report.item_count = None;
        self.view = Handover::default();
    }

    /// Takes the tally of the rolls for home.
    pub fn take_count(&mut self, tally: Tally) {
        self.request.is_busy = false;
        self.report.tally = Some(tally);
    }

    /// True while the thread holds no file, picture or decoder part.
    pub const fn is_off(&self) -> bool {
        !self.request.is_busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::gallery::samples::request;
    use crate::domain::gallery::show::QUEUE_LENGTH;
    use crate::domain::gallery::{Stamp, Thumbnail};
    use alloc::vec;

    fn is_renewed(state: &State, generation: u32) -> bool {
        state.generation != generation
    }

    fn with_request(request: RollRequest) -> State {
        let mut state = State::default();
        state.request.set(Some(request));
        state
    }

    type Fields = (Option<usize>, Option<Note>, bool, Option<(Kind, usize)>);

    fn fields(report: &Report) -> Fields {
        (report.picture.as_ref().map(|shown| shown.0), report.note, report.is_playing, report.item_count)
    }

    #[test]
    fn job_order() {
        let mut state = State::default();
        assert_eq!((state.take_job(), state.request.is_busy), (None, false), "nothing is asked");
        state.request.set(Some(request(Kind::Video)));
        assert_eq!((state.take_job(), state.request.is_busy), (Some(Job::Roll(Kind::Video)), true));
        state.request.set(None);
        state.home.set(true, 3);
        assert_eq!((state.take_job(), state.request.is_busy), (Some(Job::Count), true), "home shows");
        assert_eq!((state.take_job(), state.request.is_busy), (None, false), "counted once");
        state.generation += 1;
        assert_eq!(state.take_job(), Some(Job::Count), "and again once the rolls have changed");
        state.home.set(true, 4);
        assert_eq!(state.take_job(), Some(Job::Count), "and on a new day");
        state.home.set(false, 4);
        assert_eq!(state.take_job(), None, "but not while another screen shows");
    }

    #[test]
    fn video_and_roll_moves() {
        let open = RollRequest { open_item: Some(2), ..request(Kind::Video) };
        let mut state = with_request(open);
        let has_request =
            |state: &mut State, pose| state.next_step(open, &Player::new(pose), false) == Some(VideoStep::Request);
        assert_eq!((state.open_item(Kind::Video, 3), state.open_item(Kind::Photo, 3)), (Some((open, 2)), None));
        assert_eq!(state.open_item(Kind::Video, 2), None, "a thing beyond the roll's end has nothing to show");
        assert!(!has_request(&mut state, Pose::Upright) && !state.has_moved(open, 0));
        assert!(has_request(&mut state, Pose::RightUp), "the device is held another way");
        state.request.clicks += 1;
        assert!(has_request(&mut state, Pose::Upright), "a click");
        state.request.take_clicks();
        state.generation = 1;
        assert!(
            state.has_moved(open, 0) && !has_request(&mut state, Pose::Upright),
            "new files move a roll, not a video on show"
        );
        assert!(is_renewed(&state, 0) && !is_renewed(&state, 1));
        state.request.set(Some(request(Kind::Video)));
        assert!(has_request(&mut state, Pose::Upright) && state.has_moved(open, 1), "the grid is wanted again");
        assert_eq!(state.open_item(Kind::Video, 3), None, "the grid has no thing open");
    }

    #[test]
    fn generation_changes() {
        let mut state = State { generation: u32::MAX, ..State::default() };
        assert!(!state.set_wanted(None, 0) && state.generation == u32::MAX, "nothing has changed");
        assert!(
            state.set_wanted(None, 1) && is_renewed(&state, u32::MAX) && !is_renewed(&state, 0),
            "the count goes round"
        );
        assert!(!state.set_wanted(None, 1) && !is_renewed(&state, 0), "a count is heard of once");
        let errand = Errand { kind: Kind::Photo, file: "2.jpg".into(), stamp: Stamp::Trash, is_original: false };
        state.add_errand(errand.clone());
        assert_eq!(state.generation, 1);
        assert_eq!(core::mem::take(&mut state.errands), [errand]);
        assert!(core::mem::take(&mut state.errands).is_empty(), "taken once");
    }

    #[test]
    fn dropped_clicks() {
        let open = RollRequest { open_item: Some(0), ..request(Kind::Video) };
        let mut state = with_request(open);
        state.request.clicks += 1;
        assert!(!state.drop_clicks(open) && state.request.clicks == 0, "a video that cannot be shown drops them");
        state.request.set(None);
        assert!(state.drop_clicks(open), "and once the screen wants another, it is left");
    }

    #[test]
    fn video_steps() {
        let open = RollRequest { open_item: Some(0), ..request(Kind::Video) };
        let mut state = with_request(open);
        let mut player = Player::new(Pose::Upright);
        assert_eq!(state.next_step(open, &player, false), None, "a video that stands waits for the screen");
        state.request.clicks += 1;
        assert_eq!(state.next_step(open, &player, false), Some(VideoStep::Request));
        state.request.take_clicks();
        player.take_clicks(1, false);
        state.show.play();
        assert_eq!(
            state.next_step(open, &player, false),
            Some(VideoStep::Draw(QUEUE_LENGTH)),
            "one that plays draws ahead"
        );
        (0..QUEUE_LENGTH).for_each(|_| state.show.add(Picture::default(), 0));
        assert_eq!(state.next_step(open, &player, false), None, "until enough are drawn");
        assert_eq!(
            state.next_step(open, &player, true),
            None,
            "an ended one waits for the screen to show what is drawn"
        );
        state.show.clear();
        assert_eq!(state.next_step(open, &player, true), Some(VideoStep::End), "and acts once it has");
        state.pose = Pose::LeftUp;
        assert_eq!(state.next_step(open, &player, true), Some(VideoStep::Request), "what the screen asks comes first");
    }

    #[test]
    fn pass_of_a_roll() {
        let open = RollRequest { open_item: Some(1), ..request(Kind::Photo) };
        let mut state = with_request(open);
        assert_eq!(state.next_open(2, 0), Open::Item(open, 1));
        assert_eq!(state.next_open(1, 0), Open::Closed, "a thing beyond the roll's end");
        assert_eq!(state.next_open(2, 7), Open::Stale);
        let errand = Errand { kind: Kind::Photo, file: "2.jpg".into(), stamp: Stamp::Trash, is_original: false };
        state.add_errand(errand.clone());
        assert_eq!(state.take_pass(Kind::Photo), (Some(open), 1, vec![errand]));
        assert_eq!(state.take_pass(Kind::Video), (None, 1, Vec::new()), "another roll, and taken once");
        state.request.click();
        assert_eq!(state.take_ask(open), Ask { is_wanted: true, clicks: 1, pose: state.pose });
        assert!(!state.take_ask(request(Kind::Video)).is_wanted);
    }

    #[test]
    fn picture_for_open_item_alone() {
        let mut state = State::default();
        let mut picture = Picture::default();
        state.put_picture(&mut Picture { words: vec![7; 6], width: 3 }, Kind::Photo, (0, "0.jpg"));
        assert_eq!(
            (state.take_report(Some((Kind::Video, 0)), &mut picture).picture, picture.words.len()),
            (None, 0),
            "made for the other roll"
        );
        state.put_picture(&mut Picture { words: vec![7; 6], width: 3 }, Kind::Photo, (2, "2.jpg"));
        assert_eq!(
            (state.take_report(Some((Kind::Photo, 3)), &mut picture).picture, picture.words.len()),
            (None, 0),
            "of an item the wheel has left behind"
        );
        state.put_picture(&mut Picture { words: vec![8; 6], width: 2 }, Kind::Photo, (3, "3.jpg"));
        let shown = state.take_report(Some((Kind::Photo, 3)), &mut picture).picture;
        assert_eq!(
            (shown, &picture.words[..], picture.width),
            (Some((3, "3.jpg".into())), &[8u32; 6][..], 2),
            "the picture with the file it is of"
        );
        state.put_picture(&mut Picture { words: vec![9; 6], width: 2 }, Kind::Photo, (3, "3.jpg"));
        assert!(
            state.take_report(None, &mut picture).picture.is_none()
                && state.take_report(Some((Kind::Photo, 3)), &mut picture).picture.is_none(),
            "one that comes while the grid shows is dropped"
        );
    }

    #[test]
    fn new_roll_drops_clicks_and_picture() {
        let mut state = State::default();
        state.set_wanted(Some(RollRequest { open_item: Some(0), ..request(Kind::Video) }), 0);
        state.put_picture(&mut Picture { words: vec![7; 4], width: 2 }, Kind::Video, (0, "0.mp4"));
        state.request.click();
        state.pose = Pose::RightUp;
        state.set_wanted(None, 0);
        state.set_wanted(Some(request(Kind::Video)), 0);
        assert_eq!(
            (state.request.take_clicks(), state.pose),
            (0, Pose::RightUp),
            "the way the device is held stays what it is"
        );
        assert_eq!(state.take_report(Some((Kind::Video, 0)), &mut Picture::default()).picture, None);
    }

    #[test]
    fn rest_and_report() {
        let mut state = State::default();
        state.request.set(Some(request(Kind::Photo)));
        assert_eq!((state.take_job(), state.request.is_busy), (Some(Job::Roll(Kind::Photo)), true));
        state.set_count(Kind::Photo, 3, None);
        let thumbnail = Thumbnail::unreadable("2.jpg");
        state.report.thumbnails.extend(vec![RoundThumbnail { round: 1, item: 2, thumbnail }]);
        state.report.note = Some(Note::Unreadable);
        state.show.play();
        state.put_picture(&mut Picture::default(), Kind::Photo, (2, "2.jpg"));
        let taken = state.take_report(Some((Kind::Photo, 2)), &mut Picture::default());
        assert_eq!(fields(&taken), (Some(2), Some(Note::Unreadable), true, Some((Kind::Photo, 3))));
        state.show.clear();
        let again = state.take_report(None, &mut Picture::default());
        assert_eq!(fields(&again), (None, None, false, None), "taken once");
        state.set_count(Kind::Photo, 0, Some(Note::Empty(Kind::Photo)));
        let items: Vec<usize> = taken.thumbnails.iter().map(|ready| ready.item).collect();
        assert!(items == [2] && again.thumbnails.is_empty(), "the small pictures are taken once");
        state.put_picture(&mut Picture { words: vec![7; 4], width: 2 }, Kind::Photo, (2, "2.jpg"));
        state.rest();
        assert_eq!(
            (state.request.is_busy, fields(&state.take_report(Some((Kind::Photo, 2)), &mut Picture::default()))),
            (false, (None, Some(Note::Empty(Kind::Photo)), false, None)),
            "a thread at rest holds nothing, a picture for the screen and a count it did not tell are dropped; why the roll shows nothing is told"
        );
        state.request.set(None);
        state.home.set(true, 3);
        assert_eq!((state.take_job(), state.request.is_busy), (Some(Job::Count), true));
        state.take_count(Tally { photos: 4, ..Tally::default() });
        assert!(!state.request.is_busy, "a look rests once it has counted");
        let tally = state.take_report(None, &mut Picture::default()).tally;
        assert_eq!(
            (tally.map(|tally| tally.photos), state.take_report(None, &mut Picture::default()).tally),
            (Some(4), None)
        );
    }
}

//! The pixel cam: a photo's redrawing.

use super::backlog::{Backlog, Holder};
use super::cause::Cause;
use crate::base::time::{NANOS_PER_SECOND, SECONDS_PER_MINUTE};
use crate::hw::camera::develop;
use crate::openai::fault::Fault;
use crate::openai::models::image;
use crate::openai::request::Step;
use crate::storage::{Files, PICTURE_EXTENSION, PICTURES};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Reverse;
use core::fmt::Display;
use core::time::Duration;

const ORIGINAL_MARK: &str = "_original";
const PIXEL_MARK: &str = "_pixel";
const DRAW_TIME_NS: u64 = 11_000_000_000;
const PARTS: u64 = 4;
const STEPS_BEFORE: usize = 3;
const STEPS_AFTER: usize = 2;
pub(crate) const BLOCKS: usize = STEPS_BEFORE + PARTS as usize - 1 + STEPS_AFTER;
const PER_MINUTE: u64 = 20;
/// How many redrawings run at once, which keeps the model's pace under its limit.
pub const MAX_PARALLEL: usize = (PER_MINUTE * DRAW_TIME_NS / (SECONDS_PER_MINUTE * NANOS_PER_SECOND)) as usize;
pub const DIRECTORY: &str = ".r1metal/magic";
const NO_ROOM: &str = "the data partition has no room for the pixel picture";
// A photo shrunk is the picture the image model takes and gives back.
const _: () = assert!(develop::SIDE >= image::SIDE);

/// Which of a photo's two pictures a file is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    Original,
    Pixel,
}

/// The name of one of a photo's two files without its extension.
pub fn to_stem(photo: &str, variant: Variant) -> String {
    let mark = if variant == Variant::Original { ORIGINAL_MARK } else { PIXEL_MARK };
    format!("{photo}{mark}")
}

/// The name of one of a photo's two files.
pub fn to_name(photo: &str, variant: Variant, extension: &str) -> String {
    format!("{}.{extension}", to_stem(photo, variant))
}

pub(super) fn parse_name(name: &str) -> Option<(&str, Variant)> {
    let (stem, _) = name.rsplit_once('.')?;
    let with_mark = |mark: &str, variant| Some((stem.strip_suffix(mark).filter(|photo| !photo.is_empty())?, variant));
    with_mark(ORIGINAL_MARK, Variant::Original).or_else(|| with_mark(PIXEL_MARK, Variant::Pixel))
}

/// The name a photo's small picture waits under to be sent.
pub fn to_waiting_name(photo: &str) -> String {
    format!("{photo}.{PICTURE_EXTENSION}")
}

fn parse_waiting_name(name: &str) -> Option<&str> {
    let (photo, extension) = name.rsplit_once('.')?;
    (extension == PICTURE_EXTENSION).then_some(photo)
}

/// The photos whose small pictures wait, of the names in their directory.
pub fn to_waiting_photos(names: &[String]) -> Vec<String> {
    names.iter().filter_map(|name| parse_waiting_name(name).map(String::from)).collect()
}

const fn is_accepted((width, height): (usize, usize)) -> bool {
    width == height
}

/// The error text of a photo of a size the pixel cam does not take.
pub fn to_refusal(size: (usize, usize)) -> Option<String> {
    (!is_accepted(size)).then(|| format!("a photo of {}x{} is none of the pixel cam's", size.0, size.1))
}

fn is_gone<E>(has: impl FnOnce(Variant) -> Result<bool, E>) -> Result<bool, E> {
    Ok(!has(Variant::Original)?)
}

fn is_needless<E>(is_redraw: bool, mut has: impl FnMut(Variant) -> Result<bool, E>) -> Result<bool, E> {
    Ok(is_gone(&mut has)? || (!is_redraw && has(Variant::Pixel)?))
}

// What comes back is about as large as the small picture.
const fn has_room(free_bytes: u64, small_bytes: usize) -> bool {
    free_bytes >= small_bytes as u64
}

/// The small picture a photo is redrawn from; none where it needs no redrawing, which removes the picture.
pub fn take_small<F: Files>(mut files: F, photo: &str, is_redraw: bool) -> Result<Option<Vec<u8>>, F::Error> {
    let small = to_waiting_name(photo);
    if is_needless(is_redraw, |variant| files.has(PICTURES, &to_name(photo, variant, PICTURE_EXTENSION)))? {
        files.remove(DIRECTORY, &small)?;
        return Ok(None);
    }
    let Some(picture) = files.read(DIRECTORY, &small)? else {
        return Ok(None);
    };
    if !has_room(files.free()?, picture.len()) {
        return Err(files.to_error(NO_ROOM));
    }
    Ok(Some(picture))
}

/// What became of a picture that was drawn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum KeepResult {
    // The name it is stored under.
    Kept(String),
    // The photo was deleted meanwhile.
    Withdrawn,
}

impl KeepResult {
    /// How the redrawing ended.
    pub const fn outcome(&self) -> Outcome {
        match self {
            KeepResult::Kept(_) => Outcome::Kept,
            KeepResult::Withdrawn => Outcome::Withdrawn,
        }
    }

    pub fn to_log_line(&self, photo: &str, bytes: usize, cost: &str, times: &str, ms: u128) -> String {
        match self {
            KeepResult::Kept(name) => format!("magic: {name}, {bytes} bytes, for {cost}: {times}, kept after {ms} ms"),
            KeepResult::Withdrawn => format!(
                "magic: {photo}: drawn for {cost}, and the photo was deleted meanwhile: nothing is kept; {times}"
            ),
        }
    }
}

/// Stores a picture that was drawn and removes the small one.
pub fn keep<F: Files>(mut files: F, photo: &str, is_redraw: bool, drawn: &[u8]) -> Result<KeepResult, F::Error> {
    let name = if is_redraw {
        let name = to_name(photo, Variant::Pixel, PICTURE_EXTENSION);
        files.put(PICTURES, &name, drawn).map(|()| name)
    } else {
        files.save(PICTURES, &to_stem(photo, Variant::Pixel), PICTURE_EXTENSION, drawn)
    }?;
    // The photo is looked for once its picture is kept: one deleted meanwhile has the picture taken away here, or its deletion comes after and takes the picture by its name.
    let is_withdrawn = is_gone(|variant| files.has(PICTURES, &to_name(photo, variant, PICTURE_EXTENSION)))?;
    if is_withdrawn {
        files.remove(PICTURES, &name)?;
    }
    files.remove(DIRECTORY, &to_waiting_name(photo))?;
    Ok(if is_withdrawn { KeepResult::Withdrawn } else { KeepResult::Kept(name) })
}

/// The log line of a photo that needs no redrawing.
pub fn to_needless_line(photo: &str) -> String {
    format!("magic: {photo}: nothing to redraw, the photo is gone or has its pixel picture")
}

/// The log line of a picture drawn and not stored.
pub fn to_unkept_line(photo: &str, cost: &str, reason: &impl Display, times: &str) -> String {
    format!("magic: {photo}: drawn for {cost} and not kept: {reason}; {times}")
}

/// How long the steps of a redrawing took, for the log.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Timings {
    pub joined: Duration,
    pub reached: Duration,
    pub sent: Duration,
    pub answered: Duration,
    pub total: Duration,
    pub bytes: usize,
}

impl Timings {
    /// Takes that the request reached `step` after `elapsed`.
    pub const fn note(&mut self, step: Step, elapsed: Duration) {
        match step {
            Step::Reached => self.reached = elapsed,
            Step::Sent => self.sent = elapsed,
            Step::Answered => self.answered = elapsed,
        }
    }

    pub fn to_log_text(&self) -> String {
        format!(
            "in the network after {} ms, the service reached after {}, the photo's {} bytes sent after {}, the answer's head after {}, the answer read after {}",
            self.joined.as_millis(),
            self.reached.as_millis(),
            self.bytes,
            self.sent.as_millis(),
            self.answered.as_millis(),
            self.total.as_millis()
        )
    }
}

/// How a redrawing that came to something ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Kept,
    Needless,
    // Its picture was stored and removed again, the photo deleted meanwhile.
    Withdrawn,
}

/// The log line of a redrawing the service did not do.
pub fn to_failure_line(photo: &str, fault: &Fault, timings: &Timings) -> String {
    let times = timings.to_log_text();
    match fault {
        Fault::Refused(reason) => format!("magic: {photo}: not redrawn and not asked for again, {reason}; {times}"),
        Fault::NoKey | Fault::Limit => format!("magic: {photo}: {fault}"),
        Fault::Offline(_) | Fault::Temporary(_) => format!("magic: {photo}: not redrawn this time: {fault}; {times}"),
    }
}

/// The log line of a redrawing that found no way into the network or to the service's key.
pub fn to_unheld_line(photo: &str, fault: &Fault) -> String {
    format!("magic: {photo}: {fault}")
}

// True for a fault that is final: the photo is asked for no more.
const fn is_final(fault: &Fault) -> bool {
    matches!(fault, Fault::Refused(_))
}

// True for a redrawing that is done: its photo waits no more.
const fn is_done(outcome: &Result<Outcome, Fault>) -> bool {
    match outcome {
        Ok(_) => true,
        Err(fault) => is_final(fault),
    }
}

/// Ends a redrawing the service did not do: the small picture of a photo that waits no more is taken away.
pub fn end_failed<F: Files>(mut files: F, photo: &str, fault: &Fault) -> Result<(), F::Error> {
    if is_final(fault) {
        files.remove(DIRECTORY, &to_waiting_name(photo))?;
    }
    Ok(())
}

/// How far the redrawing of a photo has got.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Stage {
    Waiting,
    Joined,
    Reached,
    Sent,
    Answered,
}

impl From<Step> for Stage {
    fn from(step: Step) -> Stage {
        match step {
            Step::Reached => Stage::Reached,
            Step::Sent => Stage::Sent,
            Step::Answered => Stage::Answered,
        }
    }
}

/// How far a redrawing has got, and since when.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Progress {
    pub stage: Stage,
    pub since_ns: u64,
}

impl Progress {
    fn elapsed_parts(&self, now_ns: u64) -> u64 {
        (now_ns.saturating_sub(self.since_ns) / (DRAW_TIME_NS / PARTS)).min(PARTS - 1)
    }

    pub(crate) fn lit_blocks(&self, now_ns: u64) -> usize {
        match self.stage {
            Stage::Waiting => 0,
            Stage::Joined => 1,
            Stage::Reached => 2,
            Stage::Sent => STEPS_BEFORE + self.elapsed_parts(now_ns) as usize,
            Stage::Answered => BLOCKS - 1,
        }
    }

    pub(crate) fn next_block_ns(&self, now_ns: u64) -> Option<u64> {
        let parts = self.elapsed_parts(now_ns);
        (self.stage == Stage::Sent && parts < PARTS - 1).then(|| self.since_ns + (parts + 1) * (DRAW_TIME_NS / PARTS))
    }
}

/// An order of the camera or the gallery to the pixel cam, through the main loop.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Order {
    Add(String),
    // The result takes the place of the pixel picture, which stays until then.
    Redraw(String),
    // A redrawing under way looks for the photo once it has stored its picture.
    Forget(String),
}

/// The pixel cam's report to the main loop.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Report {
    pub(crate) progress: Option<Progress>,
    /// The photos whose pixel picture was kept.
    pub kept: Vec<String>,
    withdrawn_count: u32,
    pub cause: Option<Cause>,
    /// The photos whose pixel picture was redrawn: what shows the old one is stale.
    pub redraws: Vec<String>,
}

impl Report {
    pub(crate) const fn changes_rolls(&self) -> bool {
        !self.kept.is_empty() || self.withdrawn_count > 0 || self.cause.is_some()
    }
}

/// What the pixel cam's threads and the main loop share.
#[derive(Default)]
pub struct State {
    pub backlog: Backlog<String, Progress>,
    redraws: Vec<String>,
    done_redraws: Vec<String>,
    pub report: Report,
}

impl Holder for State {
    type Key = String;
    type Busy = Progress;

    fn backlog(&mut self) -> &mut Backlog<String, Progress> {
        &mut self.backlog
    }
}

impl State {
    /// Takes how the redrawing of a photo ended.
    pub fn take_outcome(&mut self, photo: &str, outcome: &Result<Outcome, Fault>) {
        let done = is_done(outcome);
        self.backlog.end(photo, done, outcome.as_ref().is_err_and(Fault::is_stalling));
        if done {
            let was_redraw = self.is_redraw(photo);
            self.redraws.retain(|name| name != photo);
            if was_redraw && *outcome == Ok(Outcome::Kept) {
                self.done_redraws.push(photo.into());
            }
        }
        match outcome {
            Ok(Outcome::Kept) => self.report.kept.push(photo.into()),
            Ok(Outcome::Withdrawn) => self.report.withdrawn_count += 1,
            Ok(Outcome::Needless) => {}
            Err(Fault::Refused(_)) => self.report.cause = Some(Cause::PixelsRefused),
            Err(fault) => self.report.cause = Some(Cause::from_fault(fault, Cause::NoPixels)),
        }
    }

    /// Takes the orders of the camera and the gallery; true where there are any.
    pub fn take_orders(&mut self, orders: Vec<Order>) -> bool {
        let has_any = !orders.is_empty();
        for order in orders {
            match order {
                Order::Add(photo) => self.backlog.add_new(photo),
                Order::Redraw(photo) => self.redraw(&photo),
                Order::Forget(photo) => self.forget(&photo),
            }
        }
        has_any
    }

    // A screen of the photos is open, so what stalled is tried again with it.
    fn redraw(&mut self, photo: &str) {
        if !self.is_redraw(photo) {
            self.redraws.push(photo.into());
        }
        self.backlog.is_stalled = false;
        self.backlog.add_new(photo.into());
    }

    fn forget(&mut self, photo: &str) {
        self.backlog.forget(photo);
        self.redraws.retain(|name| name != photo);
    }

    /// True for a photo whose pixel picture is asked for again.
    pub fn is_redraw(&self, photo: &str) -> bool {
        self.redraws.iter().any(|name| name == photo)
    }

    fn farthest_progress(&self) -> Option<Progress> {
        let busy = self.backlog.busy().map(|busy| busy.1);
        busy.max_by_key(|progress| (progress.stage, Reverse(progress.since_ns)))
    }

    /// The report, with how far the farthest redrawing is and what was redrawn.
    pub fn take_report(&mut self) -> Report {
        let (progress, redraws) = (self.farthest_progress(), core::mem::take(&mut self.done_redraws));
        Report { progress, redraws, ..core::mem::take(&mut self.report) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::backlog::Status;
    use crate::openai::models::TIMEOUT;
    use crate::openai::models::image::SIDE;
    use crate::storage::tests::{Disk, Refused};
    use alloc::vec;

    const SECOND: u64 = NANOS_PER_SECOND;

    fn jobs(state: &State) -> Vec<(&str, Status<Progress>)> {
        state.backlog.pieces.iter().map(|(photo, status)| (photo.as_str(), *status)).collect()
    }

    fn progress(stage: Stage, since_ns: u64) -> Progress {
        Progress { stage, since_ns }
    }

    fn take_next(state: &mut State, now_ns: u64) -> Option<String> {
        state.backlog.take_next(progress(Stage::Waiting, now_ns)).map(|(photo, _)| photo)
    }

    #[test]
    fn parallel_limit_and_waiting_names() {
        assert_eq!(MAX_PARALLEL, 3, "20 a minute at 11 s each");
        assert!(MAX_PARALLEL as u64 * SECONDS_PER_MINUTE * SECOND / DRAW_TIME_NS <= PER_MINUTE);
        assert!(TIMEOUT.as_nanos() as u64 > 10 * DRAW_TIME_NS, "the service gets far longer than it usually takes");
        assert_eq!(to_waiting_name("20261010_231500"), "20261010_231500.jpg");
        assert_eq!(parse_waiting_name("20261010_231500.jpg"), Some("20261010_231500"));
        assert_eq!((parse_waiting_name("20261010_231500.png"), parse_waiting_name("jpg")), (None, None));
    }

    #[test]
    fn outcome_decides_retry() {
        let mut state = State::default();
        state.backlog.set_wanted(true);
        state.backlog.add_waiting(["a", "b", "c", "d", "e", "f"].map(String::from).to_vec());
        for _ in 0..4 {
            take_next(&mut state, SECOND);
        }
        state.take_outcome("a", &Ok(Outcome::Kept));
        state.take_outcome("b", &Ok(Outcome::Needless));
        state.take_outcome("c", &Err(Fault::Refused("no".into())));
        assert_eq!(
            (jobs(&state), state.report.kept.len(), state.report.cause, state.backlog.is_stalled),
            (
                vec![
                    ("d", Status::Busy(progress(Stage::Waiting, SECOND))),
                    ("e", Status::Waiting),
                    ("f", Status::Waiting)
                ],
                1,
                Some(Cause::PixelsRefused),
                false
            ),
            "kept, not needed or refused for good, a photo waits no more"
        );
        state.take_outcome("d", &Err(Fault::Temporary("gone".into())));
        assert_eq!(
            (take_next(&mut state, SECOND).as_deref(), state.report.cause),
            (Some("e"), Some(Cause::NoPixels)),
            "one that failed is left, and the next is tried"
        );
        state.take_outcome("e", &Err(Fault::Offline("asleep".into())));
        assert_eq!(
            (take_next(&mut state, SECOND), state.report.cause, state.backlog.is_stalled),
            (None, Some(Cause::NoNetwork), true),
            "without the network nothing more is tried"
        );
        state.backlog.is_stalled = false;
        assert_eq!(take_next(&mut state, SECOND).as_deref(), Some("f"));
        state.take_outcome("f", &Err(Fault::NoKey));
        assert_eq!(
            (state.report.cause, state.backlog.is_stalled),
            (Some(Cause::NoKey), true),
            "nor is anything tried without a key"
        );
    }

    #[test]
    fn redraw_reported_once() {
        let mut state = State::default();
        state.backlog.end("before", false, true);
        assert!(state.backlog.is_stalled, "a photo before it found no network");
        state.redraw("again");
        state.redraw("again");
        assert!(
            state.is_redraw("again") && !state.is_redraw("once") && !state.backlog.is_stalled,
            "asked for anew once, and tried"
        );
        state.backlog.add_new("once".into());
        state.take_outcome("once", &Ok(Outcome::Kept));
        state.take_outcome("again", &Ok(Outcome::Kept));
        assert_eq!(
            state.take_report(),
            Report { kept: vec!["once".into(), "again".into()], redraws: vec!["again".into()], ..Report::default() },
            "what shows the old picture is made anew, and each photo whose picture was kept is named"
        );
        assert!(!state.is_redraw("again") && state.take_report().redraws.is_empty(), "told once");
        let kept = Report { kept: vec!["once".into()], ..Report::default() };
        assert!(!Report::default().changes_rolls() && kept.changes_rolls());
        assert!(
            Report { withdrawn_count: 1, ..Report::default() }.changes_rolls(),
            "and so does a picture that stood for a moment"
        );
        assert!(
            Report { cause: Some(Cause::NoPixels), ..Report::default() }.changes_rolls(),
            "a photo not redrawn changes what a roll tells too"
        );
    }

    #[test]
    fn forget_keeps_busy() {
        let mut state = State::default();
        state.redraw("sent");
        state.redraw("waits");
        assert_eq!(take_next(&mut state, SECOND).as_deref(), Some("sent"));
        state.forget("sent");
        state.forget("waits");
        assert_eq!(
            jobs(&state),
            [("sent", Status::Busy(progress(Stage::Waiting, SECOND)))],
            "the one being sent finds its photo gone itself"
        );
        assert!(!state.is_redraw("sent") && !state.is_redraw("waits"));
        state.take_outcome("sent", &Ok(Outcome::Withdrawn));
        assert!(jobs(&state).is_empty(), "and waits no more once its picture is taken away again");
        assert_eq!(
            state.take_report(),
            Report { withdrawn_count: 1, ..Report::default() },
            "which is told, so that a roll that listed the picture for the moment it stood lists anew"
        );
    }

    #[test]
    fn orders_by_kind() {
        let mut state = State::default();
        assert!(!state.take_orders(Vec::new()), "none: the threads are not woken");
        let orders = ["new", "gone"].map(|photo| Order::Add(photo.into()));
        assert!(state.take_orders(orders.to_vec()));
        assert!(state.take_orders(vec![Order::Redraw("again".into()), Order::Forget("gone".into())]));
        assert_eq!(
            (jobs(&state), state.is_redraw("again"), state.is_redraw("new")),
            (vec![("new", Status::New), ("again", Status::New)], true, false),
            "a photo added and one asked for anew are due, the one forgotten waits no more"
        );
    }

    #[test]
    fn farthest_redrawing() {
        let mut state = State::default();
        assert_eq!(state.farthest_progress(), None);
        for (photo, reached) in
            [("a", progress(Stage::Joined, 5)), ("b", progress(Stage::Sent, 9)), ("c", progress(Stage::Sent, 7))]
        {
            state.backlog.add_new(photo.into());
            state.backlog.set_busy(photo, reached);
        }
        state.backlog.add_new("d".into());
        state.backlog.add_new("e".into());
        state.backlog.end("e", false, false);
        assert_eq!(
            state.farthest_progress(),
            Some(Progress { stage: Stage::Sent, since_ns: 7 }),
            "of two that have got as far, the one that got there first"
        );
        state.take_outcome("c", &Ok(Outcome::Kept));
        assert_eq!(state.farthest_progress(), Some(Progress { stage: Stage::Sent, since_ns: 9 }));
    }

    #[test]
    fn file_names() {
        assert_eq!(to_name("20261010_231500", Variant::Original, "jpg"), "20261010_231500_original.jpg");
        assert_eq!(to_name("20261010_231500", Variant::Pixel, "jpg"), "20261010_231500_pixel.jpg");
        assert_eq!(to_stem("20261010_231500_1", Variant::Pixel), "20261010_231500_1_pixel");
        assert_eq!(parse_name("20261010_231500_original.jpg"), Some(("20261010_231500", Variant::Original)));
        assert_eq!(parse_name("20261010_231500_pixel.jpg"), Some(("20261010_231500", Variant::Pixel)));
        assert_eq!(
            parse_name("a_pixel_original.jpeg"),
            Some(("a_pixel", Variant::Original)),
            "the last mark counts, whatever the extension"
        );
        for invalid in ["20261010_231500.jpg", "_pixel.jpg", "20261010_231500_pixel", "pixel.jpg", "x_Pixel.jpg", ""] {
            assert_eq!(parse_name(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn square_photos_only() {
        assert!(is_accepted((develop::SIDE, develop::SIDE)) && is_accepted((SIDE, SIDE)), "as the camera takes them");
        assert!(!is_accepted((2048, 1536)) && !is_accepted((1536, 2048)));
        assert_eq!(to_refusal((develop::SIDE, develop::SIDE)), None);
        assert_eq!(to_refusal((3, 2)).as_deref(), Some("a photo of 3x2 is none of the pixel cam's"));
    }

    #[test]
    fn need_room_and_failures() {
        let files = |variants: &'static [Variant]| move |variant: Variant| Ok::<bool, ()>(variants.contains(&variant));
        assert_eq!(is_needless(false, files(&[Variant::Original])), Ok(false), "a photo without its pixel picture");
        assert_eq!(is_needless(false, files(&[Variant::Original, Variant::Pixel])), Ok(true), "one that has it");
        assert_eq!(is_needless(true, files(&[Variant::Original, Variant::Pixel])), Ok(false), "unless asked anew");
        assert_eq!(is_needless(true, files(&[Variant::Pixel])), Ok(true), "a photo that is gone");
        assert_eq!(is_needless(false, |_| Err(())), Err(()));
        assert_eq!(
            [is_gone(files(&[Variant::Pixel])), is_gone(files(&[Variant::Original])), is_gone(|_| Err(()))],
            [Ok(true), Ok(false), Err(())],
            "a photo is gone where its original is not, whatever its pixel picture; a partition that cannot be read tells nothing"
        );
        assert!(has_room(1000, 1000) && !has_room(999, 1000));
        let reasons = || {
            [Fault::NoKey, Fault::Offline("asleep".into()), Fault::Refused("no".into()), Fault::Temporary("x".into())]
        };
        let timings = Timings { bytes: 9, ..Timings::default() };
        let times = timings.to_log_text();
        assert_eq!(
            reasons().map(|reason| to_failure_line("p", &reason, &timings)),
            [
                String::from("magic: p: no key to the services in .r1metal/openai/key"),
                format!("magic: p: not redrawn this time: not in a network: asleep; {times}"),
                format!("magic: p: not redrawn and not asked for again, no; {times}"),
                format!("magic: p: not redrawn this time: x; {times}")
            ]
        );
        assert_eq!(
            reasons().map(Err).map(|outcome| is_done(&outcome)),
            [false, false, true, false],
            "only one refused for good waits no more, a network gone in the middle of it having it tried again"
        );
        assert!(
            [Outcome::Kept, Outcome::Needless, Outcome::Withdrawn].map(Ok).iter().all(is_done),
            "nor does one kept, needless or withdrawn"
        );
    }

    #[test]
    fn small_picture_taken() {
        let original = (PICTURES, "p_original.jpg");
        let (pixel, small) = ((PICTURES, "p_pixel.jpg"), (DIRECTORY, "p.jpg"));
        let mut disk = Disk::with_files(&[original, small]);
        assert_eq!(take_small(&mut disk, "p", false), Ok(Some(Vec::from(b"file".as_slice()))));
        assert_eq!(disk.effects, ["has p_original.jpg", "has p_pixel.jpg", "read p.jpg", "free"]);
        let mut disk = Disk::with_files(&[original, pixel, small]);
        assert_eq!(take_small(&mut disk, "p", false), Ok(None), "it has its pixel picture");
        assert_eq!(disk.names(), ["p_original.jpg", "p_pixel.jpg"], "and waits no more");
        let mut disk = Disk::with_files(&[original, pixel]);
        assert_eq!(take_small(&mut disk, "p", true), Ok(None), "nothing waits to be sent");
        let mut disk = Disk { free: 3, ..Disk::with_files(&[original, small]) };
        assert_eq!(take_small(&mut disk, "p", true), Err(Refused(NO_ROOM.into())));
    }

    #[test]
    fn failed_redrawing_ended() {
        let small = (DIRECTORY, "p.jpg");
        let mut disk = Disk::with_files(&[small]);
        for fault in [Fault::NoKey, Fault::Offline("asleep".into()), Fault::Temporary("x".into())] {
            assert_eq!(end_failed(&mut disk, "p", &fault), Ok(()));
        }
        assert!(disk.effects.is_empty() && disk.names() == ["p.jpg"], "a photo tried again keeps its small picture");
        assert_eq!(end_failed(&mut disk, "p", &Fault::Refused("no".into())), Ok(()));
        assert_eq!(
            (disk.effects.as_slice(), disk.names().len()),
            (&[String::from("remove p.jpg")][..], 0),
            "one refused for good waits no more"
        );
        assert_eq!(
            to_unheld_line("p", &Fault::Offline("asleep".into())),
            "magic: p: not in a network: asleep",
            "a redrawing that got no stay tells why, with no times to tell"
        );
    }

    #[test]
    fn picture_stored() {
        let (original, pixel, small) = ((PICTURES, "p_original.jpg"), (PICTURES, "p_pixel.jpg"), (DIRECTORY, "p.jpg"));
        let mut disk = Disk::with_files(&[original, small]);
        assert_eq!(keep(&mut disk, "p", false, b"new"), Ok(KeepResult::Kept("p_pixel.jpg".into())));
        assert_eq!(disk.effects, ["save p_pixel.jpg", "has p_original.jpg", "remove p.jpg"]);
        let mut disk = Disk::with_files(&[original, pixel, small]);
        assert_eq!(keep(&mut disk, "p", true, b"new"), Ok(KeepResult::Kept("p_pixel.jpg".into())));
        assert_eq!(disk.effects[0], "put p_pixel.jpg", "at one stroke");
        let mut disk = Disk::with_files(&[small]);
        assert_eq!(keep(&mut disk, "p", false, b"new"), Ok(KeepResult::Withdrawn));
        assert_eq!(
            disk.effects,
            ["save p_pixel.jpg", "has p_original.jpg", "remove p_pixel.jpg", "remove p.jpg"],
            "the photo was deleted meanwhile"
        );
        assert!(disk.names().is_empty());
    }

    #[test]
    fn redraw_lines() {
        let kept = KeepResult::Kept("p_pixel.jpg".into());
        assert_eq!(kept.outcome(), Outcome::Kept);
        assert_eq!(
            kept.to_log_line("p", 9, "$0.02", "times", 7),
            "magic: p_pixel.jpg, 9 bytes, for $0.02: times, kept after 7 ms"
        );
        assert_eq!(KeepResult::Withdrawn.outcome(), Outcome::Withdrawn);
        assert_eq!(
            KeepResult::Withdrawn.to_log_line("p", 9, "$0.02", "times", 7),
            "magic: p: drawn for $0.02, and the photo was deleted meanwhile: nothing is kept; times"
        );
        assert_eq!(to_needless_line("p"), "magic: p: nothing to redraw, the photo is gone or has its pixel picture");
        assert_eq!(
            to_unkept_line("p", "$0.02", &"full", "times"),
            "magic: p: drawn for $0.02 and not kept: full; times"
        );
    }

    #[test]
    fn step_timings() {
        let ms = Duration::from_millis;
        let mut timings = Timings { joined: ms(40), bytes: 51_234, ..Timings::default() };
        for (step, elapsed) in [(Step::Reached, 90), (Step::Reached, 120), (Step::Sent, 300), (Step::Answered, 11_500)]
        {
            timings.note(step, ms(elapsed));
        }
        timings.total = ms(12_007);
        assert_eq!(
            timings.to_log_text(),
            "in the network after 40 ms, the service reached after 120, the photo's 51234 bytes sent after 300, the answer's head after 11500, the answer read after 12007"
        );
        assert_eq!(
            [Step::Reached, Step::Sent, Step::Answered].map(Stage::from),
            [Stage::Reached, Stage::Sent, Stage::Answered],
            "a request's steps are the redrawing's stages"
        );
    }

    #[test]
    fn lit_blocks_by_stage() {
        let at = |stage: Stage| Progress { stage, since_ns: 5 * SECOND };
        assert_eq!(BLOCKS, 8);
        assert_eq!(
            [Stage::Waiting, Stage::Joined, Stage::Reached, Stage::Answered]
                .map(|stage| at(stage).lit_blocks(60 * SECOND)),
            [0, 1, 2, 7],
            "a block each for the network, the service and the answer, however long any of them takes; the last is the kept picture's"
        );
        assert!(
            [Stage::Waiting, Stage::Joined, Stage::Reached, Stage::Answered]
                .iter()
                .all(|stage| at(*stage).next_block_ns(6 * SECOND).is_none()),
            "none of which the clock brings on"
        );
        let drawing = at(Stage::Sent);
        let quarter = DRAW_TIME_NS / 4;
        assert_eq!(
            (drawing.lit_blocks(5 * SECOND), drawing.next_block_ns(5 * SECOND)),
            (3, Some(5 * SECOND + quarter)),
            "sent, the third block is lit and the next is due after a quarter of the model's time"
        );
        assert_eq!(
            (
                drawing.lit_blocks(4 * SECOND),
                drawing.lit_blocks(5 * SECOND + quarter - 1),
                drawing.lit_blocks(5 * SECOND + quarter)
            ),
            (3, 3, 4)
        );
        assert_eq!(
            (drawing.lit_blocks(5 * SECOND + 2 * quarter), drawing.next_block_ns(5 * SECOND + 2 * quarter)),
            (5, Some(5 * SECOND + 3 * quarter))
        );
        assert_eq!(
            (drawing.lit_blocks(5 * SECOND + 3 * quarter), drawing.next_block_ns(5 * SECOND + 3 * quarter)),
            (6, None),
            "the last quarter lights nothing by itself"
        );
        assert_eq!(
            (drawing.lit_blocks(500 * SECOND), drawing.next_block_ns(500 * SECOND)),
            (6, None),
            "and a model that takes longer leaves the row where it is until its answer comes"
        );
        assert!(Stage::Waiting < Stage::Joined && Stage::Sent < Stage::Answered, "the stages count in their order");
    }
}

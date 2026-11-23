mod call;
mod darkroom;
mod frames;
mod isp;
mod photo;
mod rig;
mod scan;
mod sensor;
mod swivel;
mod video;

use crate::crew::Crew;
use crate::log;
use crate::storage::Data;
use crate::sync::Board;
use crate::sys;
use frames::Frames;
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::domain::camera::{CallPicture, Development, Outcome, Sight, Viewfinder};
use r1_funcore::domain::camera::{RESERVE_TEXT, Report, Review, Shot, Start, State, Stats};
use r1_funcore::domain::gallery::Kind;
use r1_funcore::domain::magic::Order;
use r1_funcore::gfx::bands;
use r1_funcore::hw::camera::camsv;
use r1_funcore::hw::camera::exposure::Auto;
use r1_funcore::hw::camera::measure::Measure;
use r1_funcore::hw::camera::raw::Finder;
use r1_funcore::hw::camera::seninf::{TRIM_TOO_SHORT, Trim};
use r1_funcore::hw::camera::swivel::PARKED;
use r1_funcore::hw::camera::tone::Tone;
use r1_funcore::hw::{CORES, FRAME_PARTS};
use r1_funcore::input::pose::Pose;
use rig::Rig;
use std::fs;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use swivel::{Rotation, Swivel};

const DEVINFO: &str = "/proc/device-tree/chosen/atag,devinfo";
const MIN_LOAD: u32 = 262;

struct Stop {
    error: io::Error,
    outcome: Option<Outcome>,
}

impl From<io::Error> for Stop {
    fn from(error: io::Error) -> Stop {
        Stop { error, outcome: None }
    }
}

fn to_error(error: camsv::Error) -> io::Error {
    io::Error::other(error.message())
}

struct Shared {
    board: Board<State<Frames>>,
    data: Data,
}

impl Shared {
    fn new(data: Data) -> io::Result<Shared> {
        Ok(Shared { board: Board::new(State::default())?, data })
    }

    fn set_outcome(&self, outcome: Outcome) {
        self.board.lock().report.outcome = Some(outcome);
        self.board.raise();
    }

    fn order(&self, order: Order) {
        self.board.lock().report.orders.push(order);
        self.board.raise();
    }

    fn set_recording(&self, is_recording: bool) {
        self.board.lock().set_recording(is_recording);
        self.board.raise();
    }

    fn note_shooting(&self) {
        self.board.lock().report.is_shooting = true;
        self.board.raise();
    }

    fn set_development(&self, development: Development) {
        self.board.lock().report.development = Some(development);
        self.board.raise();
    }

    fn set_taken(&self, photo: &str) {
        self.board.lock().report.taken = Some(photo.into());
        self.board.raise();
    }

    fn set_review(&self, review: Review) {
        let previous = self.board.lock().report.review.replace(review);
        drop(previous);
        self.board.raise();
    }

    fn hand_shot(&self, shot: Shot) {
        let previous = self.board.lock().report.shot.replace(shot);
        drop(previous);
        self.board.raise();
    }

    fn note_uncopied(&self) {
        self.board.lock().note_uncopied();
        self.board.raise();
    }

    fn show(&self, picture: &mut Vec<u32>, viewfinder: Viewfinder, is_in_step: bool) {
        self.board.lock().put_picture(picture, viewfinder, sys::monotonic_ns(), is_in_step);
        self.board.raise();
    }
}

fn draw(crew: &Crew, finder: &Finder, raw: &[u8], tone: &Tone, picture: &mut [u32]) -> Measure {
    let mut pieces = bands(picture, finder.size().0, FRAME_PARTS);
    let mut parts: [_; FRAME_PARTS] = std::array::from_fn(|_| (pieces.next(), Measure::default()));
    crew.for_each(&mut parts, |_, (band, measure)| {
        if let Some((lines, part)) = band {
            *measure = finder.draw_lines(raw, tone, lines.clone(), part);
        }
    });
    parts.into_iter().map(|(_, measure)| measure).reduce(Measure::merge).unwrap_or_default()
}

fn read_trim() -> Trim {
    let result = fs::read(DEVINFO)
        .map_err(|e| e.to_string())
        .and_then(|devinfo| Trim::from_devinfo(&devinfo).ok_or_else(|| TRIM_TOO_SHORT.to_string()));
    result.unwrap_or_else(|reason| {
        log!("camera: no receiver trim from {DEVINFO}: {reason}");
        Trim::NONE
    })
}

fn park(swivel: &mut Swivel) {
    if let Err(e) = swivel.begin(PARKED).and_then(Rotation::finish) {
        log!("camera: parking: {e}");
    }
}

#[derive(Clone, Copy)]
struct Screen {
    format: PixelFormat,
    refresh_ns: u64,
}

fn run_session(
    shared: &Shared,
    (crew, swivel): (&Crew, &mut Swivel),
    viewfinder: Viewfinder,
    trim: Trim,
    screen: Screen,
    auto: &mut Auto,
) -> Result<(), Stop> {
    let start = Instant::now();
    // The stream's slots are declared before the rig and so freed after it: CAMSV0 has stopped before any of them goes.
    let mut slots = None;
    let mut rig = Rig::open(trim, crew, screen.refresh_ns)?;
    let outcome = match viewfinder.kind {
        Kind::Photo => rig.run_photos(shared, swivel, (viewfinder, screen.format), auto, &mut slots),
        Kind::Video => rig.run_videos(shared, swivel, (viewfinder, screen.format), auto, &mut slots),
    };
    let errors = rig.errors();
    rig.power_down();
    let stats = outcome?;
    log!("{}", stats.to_run_line(viewfinder, start.elapsed(), auto, errors.ok()));
    stats.to_zone_lines().for_each(|line| log!("{line}"));
    Ok(())
}

fn work(shared: &Shared, screen: Screen) -> ! {
    // A part that loses its core to an ordinary thread, the kernel's gauge thread or our own for the logs, is late by all that thread has to do, and the picture with it.
    if let Err(e) = sys::set_worker_priority(MIN_LOAD) {
        log!("camera: its threads stay ordinary ones: {e}");
    }
    // The crew is started here, once: its threads take over this thread's place in line and its least load, and none is started while a viewfinder shows.
    let crew = Crew::on_cores("camera", &CORES);
    let (trim, mut swivel) = (read_trim(), Swivel::default());
    let mut auto = Auto::live();
    loop {
        let outcome = match shared.board.wait(State::serve) {
            Start::Run(viewfinder) => {
                auto.restart(viewfinder.pace());
                run_session(shared, (&crew, &mut swivel), viewfinder, trim, screen, &mut auto)
            }
            Start::Refuse(outcome) => Err(Stop { error: io::Error::other(RESERVE_TEXT), outcome: Some(outcome) }),
        };
        let is_failed = outcome.is_err();
        if let Err(stop) = outcome {
            log!("camera: {}", stop.error);
            shared.set_outcome(stop.outcome.unwrap_or(Outcome::Failed));
        }
        if shared.board.lock().should_park(is_failed) {
            park(&mut swivel);
        }
        let spares = shared.board.lock().end_run(is_failed);
        shared.board.raise();
        drop(spares);
    }
}

/// The main loop's handle of the camera's thread.
pub struct Camera {
    shared: Arc<Shared>,
}

impl Camera {
    /// Starts the thread, idle until a viewfinder is wanted, which draws in `format` and files to `data`, and the scanner's and the darkroom's threads beside it.
    pub fn start(format: PixelFormat, refresh_ns: u64, data: Data) -> io::Result<Camera> {
        let shared = Arc::new(Shared::new(data)?);
        scan::start(Arc::clone(&shared))?;
        // Below every frame the viewfinder draws: its real-time threads take a core from the darkroom's at once.
        let developer = Arc::clone(&shared);
        sys::spawn_background("darkroom", move || darkroom::run(&developer))?;
        let (worker, screen) = (Arc::clone(&shared), Screen { format, refresh_ns });
        thread::Builder::new().name("camera".into()).spawn(move || work(&worker, screen))?;
        Ok(Camera { shared })
    }

    /// Readable when a picture, a word, a code read or a run's end waits.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Sets which viewfinder shows, or that no camera is wanted.
    pub fn want(&self, viewfinder: Option<Viewfinder>) {
        if self.shared.board.lock().set_wanted(viewfinder) {
            self.shared.board.notify();
        }
    }

    /// Sets whether the battery keeps the camera from powering up.
    pub fn set_reserve(&self, is_reserved: bool) {
        self.shared.board.lock().is_reserved = is_reserved;
    }

    /// Hands on a click in the viewfinder: a photo, or a recording's start or end.
    pub fn shoot(&self) {
        self.shared.board.lock().request.click();
    }

    /// Hands on a touch on the robot in the photo viewfinder: a photo for the robot alone, kept nowhere.
    pub fn look(&self) {
        self.shared.board.lock().look();
    }

    /// Sets how the device is held, which turns photos and recordings that begin from now on.
    pub fn set_pose(&self, pose: Pose) {
        self.shared.board.lock().pose = pose;
    }

    /// Sets when the panel began to show a new page: a point on the refresh grid a stream keeps in step with.
    pub fn set_flipped(&self, at_ns: u64) {
        self.shared.board.lock().flipped_ns = at_ns;
    }

    /// True while no part of the camera has power, nothing of it moves, and the darkroom has nothing to do and holds nothing.
    pub fn is_off(&self) -> bool {
        self.shared.board.lock().is_off()
    }

    /// Asks for the pixel art of the photo a review shows, as the review shows it.
    pub fn show_art(&self, photo: String, sight: Sight) {
        self.shared.board.lock().darkroom.want_art(photo, sight);
        self.shared.board.notify();
    }

    /// Asks for a saved photo's copy for the pixel cam to be made anew, for its redraw.
    pub fn copy(&self, photo: String) {
        self.shared.board.lock().darkroom.want_copy(photo);
        self.shared.board.notify();
    }

    /// Sets whether the photo just taken shows in the stream's place, which rests meanwhile.
    pub fn pause(&self, is_paused: bool) {
        if self.shared.board.lock().set_paused(is_paused) {
            self.shared.board.notify();
        }
    }

    /// Sets whether a call's stream makes a picture of each frame for the far side, as many a second as a call takes.
    pub fn want_call_pictures(&self, is_wanted: bool) {
        self.shared.board.lock().want_call_pictures(is_wanted);
    }

    /// Swaps the newest picture for a call's far side into `picture`; true where one came since the last.
    pub fn take_call_picture(&self, picture: &mut CallPicture) -> bool {
        self.shared.board.lock().take_call_picture(picture)
    }

    /// Swaps the newest picture into `picture` if one was drawn for `viewfinder`, and takes the report.
    pub fn latest(&self, viewfinder: Option<Viewfinder>, picture: &mut Vec<u32>) -> Report {
        self.shared.board.take(|state| state.take_report(viewfinder, picture))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::hw::PARTS;
    use r1_funcore::hw::camera::swivel::Facing;
    use r1_funcore::hw::camera::swivel::viewfinder_turn;

    const REAR: Viewfinder =
        Viewfinder { kind: Kind::Photo, facing: Facing::Rear, is_scanning: false, is_calling: false, size: (480, 480) };
    const FRONT: Viewfinder = Viewfinder { facing: Facing::Front, ..REAR };

    fn camera() -> Camera {
        Camera { shared: Arc::new(Shared::new(Data::unusable()).unwrap()) }
    }

    #[test]
    fn pictures_swap_without_copy() {
        let camera = camera();
        let (mut thread_picture, mut screen_picture) = (vec![1u32; 4], vec![9u32; 4]);
        assert!(camera.latest(Some(REAR), &mut screen_picture).picture.is_none());
        assert_eq!(screen_picture, [9; 4], "nothing new, nothing swapped");
        camera.shared.show(&mut thread_picture, REAR, true);
        assert!(thread_picture.is_empty(), "the first swap gives the thread the empty picture the screen began with");
        thread_picture = vec![2; 4];
        camera.shared.show(&mut thread_picture, REAR, false);
        assert_eq!(thread_picture, [1; 4], "a picture the screen never took comes back to be drawn over");
        let info = camera.latest(Some(REAR), &mut screen_picture).picture;
        assert_eq!(
            info.map(|info| (info.viewfinder, info.is_in_step)),
            Some((REAR, false)),
            "with what it was drawn for and how its stream stood"
        );
        assert_eq!(screen_picture, [2; 4]);
        assert!(camera.latest(Some(REAR), &mut screen_picture).picture.is_none());
        camera.shared.set_outcome(Outcome::NoPhoto);
        assert_eq!(camera.latest(None, &mut screen_picture).outcome, Some(Outcome::NoPhoto));
    }

    #[test]
    fn want_wakes_thread() {
        let camera = camera();
        let shared = Arc::clone(&camera.shared);
        let thread = thread::spawn(move || shared.board.wait(State::serve));
        assert!(camera.is_off());
        camera.want(Some(FRONT));
        assert_eq!(thread.join().unwrap(), Start::Run(FRONT));
        assert!(!camera.is_off(), "busy from the moment the thread takes the wish");
    }

    #[test]
    fn crew_draw_equals_one_thread() {
        let frame = REAR.mode().frame();
        let turn = viewfinder_turn(Facing::Rear);
        let finder = Finder::new(frame, REAR.view(), 480, 480, turn, PixelFormat::XRGB8888).unwrap();
        let raw: Vec<u8> = (0..frame.bytes()).map(|at| (at * 31 + at / 7) as u8).collect();
        let tone = Tone::new(Auto::live().recipe());
        let (width, height) = finder.size();
        let (mut expected, mut crew_picture, mut single_picture) =
            (vec![0u32; width * height], vec![0u32; width * height], vec![0u32; width * height]);
        let measure = finder.draw_lines(&raw, &tone, 0..height, &mut expected);
        assert!(
            measure.quads() > 0 && expected.iter().any(|pixel| *pixel != expected[0]),
            "a frame with something in it"
        );
        let crew = Crew::new("test", PARTS - 1).unwrap();
        for _ in 0..3 {
            let count = crate::alloc_count::allocations();
            let result = draw(&crew, &finder, &raw, &tone, &mut crew_picture);
            assert_eq!(crate::alloc_count::allocations(), count, "a frame's picture takes no memory");
            assert!(result == measure && crew_picture == expected);
        }
        let empty_crew = Crew::new("test", 0).unwrap();
        assert!(
            draw(&empty_crew, &finder, &raw, &tone, &mut single_picture) == measure && single_picture == expected,
            "without threads too"
        );
    }
}

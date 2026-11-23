use crate::sync::Board;
use crate::usb::frames::Stream;
use crate::{log, sys};
use r1_funcore::base::pixel::PixelFormat;
use r1_funcore::domain::recording::{self, Encoder, Job, Pages, Shot, ShotOutcome, Totals};
use std::io;
use std::sync::Arc;
use std::thread;

// Ends a recording the host stopped reading; true where one begun anew since keeps the encoder.
fn stop(board: &Board<Pages>) -> bool {
    let (pages, is_kept) = board.lock().stop();
    drop(pages);
    is_kept
}

fn send(board: &Board<Pages>, (width, height): (usize, usize), format: PixelFormat) -> ! {
    if let Err(e) = sys::reset_scheduling() {
        log!("recording: the sender stays where it started: {e}");
    }
    let depth = recording::depth(width, height);
    let (mut stream, mut encoder, mut totals): (Option<Stream>, Option<Encoder>, Totals) =
        (None, None, Totals::default());
    loop {
        let job = board.wait(Pages::job);
        let coder = encoder.get_or_insert_with(|| Encoder::new(width, height, format).expect("a recordable size"));
        let mut is_kept = true;
        let (record, end) = match job {
            Job::Begin(at_us) => {
                // The endpoint's file is closed before it is opened anew: the kernel lets it be open once.
                drop(stream.take());
                stream = Stream::open(depth)
                    .inspect_err(|e| {
                        log!("recording: the frames endpoint: {e}");
                        is_kept = stop(board);
                    })
                    .ok();
                totals = Totals::from_start(at_us);
                (coder.start(at_us), None)
            }
            Job::Send(Shot { pixels, time_us, skips }) => {
                let record = coder.frame(&pixels, width, time_us, skips).unwrap_or_default();
                // The page goes back before the host is waited for, so that the next frame can be drawn into it.
                let pages = board.lock().give_back(pixels);
                drop(pages);
                totals.frame(skips);
                (record, None)
            }
            Job::End(at_us) => {
                // An end is sent whatever ran before it, so that a host that asked for one is never left waiting.
                if stream.is_none() {
                    stream = Stream::open(depth).inspect_err(|e| log!("recording: the frames endpoint: {e}")).ok();
                }
                (coder.end(at_us, 0), Some(at_us))
            }
        };
        totals.record(record.len());
        // An ending recording waits until all of it has gone, since its stream's closing takes back what is in flight.
        let result = stream
            .as_mut()
            .map(|stream| stream.send(record).and_then(|()| if end.is_some() { stream.drain() } else { Ok(()) }));
        if let Some(Err(e)) = result {
            log!("recording: the host has stopped reading: {e}");
            stream = None;
            is_kept = stop(board);
        }
        if let Some(end_us) = end {
            log!("{}", totals.to_log_line(end_us));
            (stream, totals) = (None, Totals::default());
            let (pages, is_begun) = board.lock().take_ended();
            is_kept = is_begun;
            drop(pages);
        }
        // A recording that ended or was stopped lets its encoder go, unless one begun anew since goes on with it.
        encoder = encoder.filter(|_| is_kept);
    }
}

/// The main loop's handle of the screen recording.
pub struct Recorder {
    board: Arc<Board<Pages>>,
    pixels: usize,
}

impl Recorder {
    /// Starts the sender thread for a screen of that size and format; memory is taken only while recording.
    pub fn start(width: usize, height: usize, format: PixelFormat) -> io::Result<Recorder> {
        if !recording::is_recordable(width, height) {
            return Err(io::Error::other(recording::to_unrecordable_text(width, height)));
        }
        let board = Arc::new(Board::new(Pages::default())?);
        let sender = Arc::clone(&board);
        thread::Builder::new().name("recording".into()).spawn(move || send(&sender, (width, height), format))?;
        Ok(Recorder { board, pixels: width * height })
    }

    /// Begins or restarts a recording at `now_ns`; the next frame drawn is its first.
    pub fn begin(&mut self, now_ns: u64) {
        self.board.lock().begin(now_ns, self.pixels);
        self.board.notify();
    }

    /// Ends the recording at `now_ns`, once the waiting frame is sent.
    pub fn end(&mut self, now_ns: u64) {
        self.board.lock().end(now_ns);
        self.board.notify();
    }

    /// A page to draw the next frame into, while a recording runs.
    pub fn page(&mut self) -> Option<Vec<u32>> {
        self.board.lock().page()
    }

    /// Takes a drawn page the screen began to show at `shown_ns`; it replaces the waiting frame.
    pub fn add_frame(&mut self, pixels: Vec<u32>, shown_ns: u64) {
        let outcome = self.board.lock().take_shown(pixels, shown_ns);
        match outcome {
            ShotOutcome::Waits => self.board.notify(),
            ShotOutcome::Shed(pages) => drop(pages),
        }
    }

    /// Takes back a page whose frame never showed.
    pub fn give_back(&mut self, pixels: Vec<u32>) {
        let pages = self.board.lock().give_back(pixels);
        drop(pages);
    }

    /// Takes whether the screen is dark; the recording shows black from then until the next frame.
    pub fn set_dark(&mut self, is_dark: bool, now_ns: u64) {
        if self.board.lock().set_dark(is_dark, now_ns) {
            self.board.notify();
        }
    }
}

use crate::base::time::NANOS_PER_MICRO;
use alloc::vec;
use alloc::vec::Vec;

const PAGES: usize = 3;

/// A frame for a recording, `time_us` on the monotonic clock, `skips` the frames passed over since the last.
pub struct Shot {
    pub pixels: Vec<u32>,
    pub time_us: u64,
    pub skips: u32,
}

/// A recording sender's next job.
pub enum Job {
    Begin(u64),
    Send(Shot),
    End(u64),
}

/// The fate of a frame the screen showed.
pub enum ShotOutcome {
    Waits,
    Shed(Vec<Vec<u32>>),
}

/// What screen and sender share of a recording; times arrive in nanoseconds and leave in microseconds.
#[derive(Default)]
pub struct Pages {
    is_on: bool,
    begin: Option<u64>,
    newest: Option<Shot>,
    end: Option<u64>,
    spare: Vec<Vec<u32>>,
    pages: usize,
    is_black: bool,
}

impl Pages {
    /// Begins a recording at `at_ns` on pages of that many pixels.
    pub fn begin(&mut self, at_ns: u64, pixels: usize) {
        (self.is_on, self.begin, self.end, self.is_black) = (true, Some(at_ns / NANOS_PER_MICRO), None, false);
        self.pass_over();
        while self.pages < PAGES {
            self.spare.push(vec![0; pixels]);
            self.pages += 1;
        }
    }

    /// Ends the recording at `at_ns`, after the newest frame.
    pub fn end(&mut self, at_ns: u64) {
        (self.is_on, self.end) = (false, Some(at_ns / NANOS_PER_MICRO));
    }

    /// Ends a recording the host stopped reading: its pages to free, and whether one begun anew since, which goes on, keeps the encoder.
    pub fn stop(&mut self) -> (Vec<Vec<u32>>, bool) {
        if self.begin.is_some() {
            return (Vec::new(), true);
        }
        (self.is_on, self.end) = (false, None);
        self.pass_over();
        (self.shed(), false)
    }

    /// The sender's next job: a beginning, then the newest frame, then an end.
    pub fn job(&mut self) -> Option<Job> {
        let begin = self.begin.take().map(Job::Begin);
        begin.or_else(|| self.newest.take().map(Job::Send)).or_else(|| self.end.take().map(Job::End))
    }

    /// A page for the next frame, while a recording runs.
    pub fn page(&mut self) -> Option<Vec<u32>> {
        if self.is_on { self.spare.pop() } else { None }
    }

    /// Takes a drawn frame the screen showed at `time_ns`, in place of an unsent one.
    pub fn take_shown(&mut self, pixels: Vec<u32>, time_ns: u64) -> ShotOutcome {
        if !self.is_on {
            return ShotOutcome::Shed(self.give_back(pixels));
        }
        let skips = self.pass_over().map_or(0, |skips| skips + 1);
        self.newest = Some(Shot { pixels, time_us: time_ns / NANOS_PER_MICRO, skips });
        ShotOutcome::Waits
    }

    /// Notes a dark screen: the recording shows black until the next frame; true where a black frame was queued.
    pub fn set_dark(&mut self, is_dark: bool, time_ns: u64) -> bool {
        if !is_dark {
            self.is_black = false;
            return false;
        }
        if self.is_black {
            return false;
        }
        let Some(mut black) = self.page() else {
            return false;
        };
        black.fill(0);
        self.take_shown(black, time_ns);
        self.is_black = true;
        true
    }

    /// Returns a page; gives back the pages to free.
    pub fn give_back(&mut self, page: Vec<u32>) -> Vec<Vec<u32>> {
        self.spare.push(page);
        self.shed()
    }

    fn shed(&mut self) -> Vec<Vec<u32>> {
        if self.is_on {
            return Vec::new();
        }
        self.pages -= self.spare.len();
        core::mem::take(&mut self.spare)
    }

    /// After a recording's end: the pages to free, and whether a new recording keeps the encoder.
    pub fn take_ended(&mut self) -> (Vec<Vec<u32>>, bool) {
        (self.shed(), self.is_on)
    }

    fn pass_over(&mut self) -> Option<u32> {
        let shot = self.newest.take()?;
        self.spare.push(shot.pixels);
        Some(shot.skips)
    }
}

#[cfg(test)]
mod tests {
    use super::super::support::SIDE;
    use super::*;

    const US: u64 = NANOS_PER_MICRO;

    fn newest(pages: &Pages) -> Option<(u64, u32, u32)> {
        pages.newest.as_ref().map(|shot| (shot.time_us, shot.skips, shot.pixels[0]))
    }

    #[test]
    fn newest_frame_replaces_and_sends() {
        let mut pages = Pages::default();
        assert!(pages.page().is_none(), "no recording, no page");
        pages.begin(5 * US + 999, SIDE);
        assert_eq!((pages.pages, pages.spare.len()), (PAGES, PAGES));
        let mut first = pages.page().unwrap();
        first[0] = 1;
        assert!(matches!(pages.take_shown(first, 16_000 * US), ShotOutcome::Waits), "the sender is woken for it");
        assert_eq!(newest(&pages), Some((16_000, 0, 1)));
        let mut second = pages.page().unwrap();
        second[0] = 2;
        pages.take_shown(second, 32_000 * US);
        assert_eq!(newest(&pages), Some((32_000, 1, 2)), "the first is passed over");
        assert_eq!((pages.pages, pages.spare.len()), (PAGES, PAGES - 1), "and its page is spare again");
        assert!(matches!(pages.job(), Some(Job::Begin(5))), "the beginning first, in whole microseconds");
        let Some(Job::Send(sent)) = pages.job() else { panic!("then the newest frame") };
        assert!(pages.job().is_none());
        pages.end(40_000 * US);
        assert!(matches!(pages.job(), Some(Job::End(40_000))), "then the end");
        let shed = pages.give_back(sent.pixels);
        assert_eq!(
            (pages.pages, pages.spare.len(), shed.len()),
            (0, 0, PAGES),
            "a recording that has ended lets its pages go"
        );
    }

    #[test]
    fn late_frames_return_pages() {
        let mut pages = Pages::default();
        pages.begin(0, SIDE);
        let (late, unshown) = (pages.page().unwrap(), pages.page().unwrap());
        assert!(pages.give_back(unshown).is_empty(), "nothing is shed while the recording runs");
        assert_eq!((pages.pages, pages.spare.len()), (PAGES, PAGES - 1));
        pages.end(1);
        assert!(
            matches!(pages.take_shown(late, 2), ShotOutcome::Shed(shed) if shed.len() == PAGES),
            "a frame after the end sheds every page"
        );
        assert_eq!((newest(&pages), pages.pages, pages.spare.len()), (None, 0, 0));
        pages.begin(3_000, SIDE);
        assert!(matches!(pages.job(), Some(Job::Begin(_))));
        let shown = pages.page().unwrap();
        pages.take_shown(shown, 4_000);
        let (shed, is_kept) = pages.stop();
        assert_eq!((shed.len(), is_kept), (PAGES, false), "a host that stopped reading: its pages and its encoder go");
        assert!(!pages.is_on && pages.newest.is_none() && pages.end.is_none() && pages.pages == 0);
        assert!(pages.job().is_none(), "and nothing more is sent");
        pages.begin(5_000, SIDE);
        assert!(matches!(pages.job(), Some(Job::Begin(_))));
        pages.begin(6 * US, SIDE);
        let (shed, is_kept) = pages.stop();
        assert!(
            shed.is_empty() && is_kept && pages.is_on && pages.page().is_some(),
            "one begun anew since goes on, with the encoder"
        );
        assert!(matches!(pages.job(), Some(Job::Begin(6))), "from its own beginning");
    }

    #[test]
    fn encoder_kept_for_restart() {
        let mut pages = Pages::default();
        pages.begin(0, SIDE);
        pages.end(US);
        let (shed, is_kept) = pages.take_ended();
        assert_eq!(
            (shed.len(), is_kept, pages.pages),
            (PAGES, false, 0),
            "an ended recording lets its pages and its encoder go"
        );
        pages.begin(2 * US, SIDE);
        pages.end(3 * US);
        assert!(matches!(pages.job(), Some(Job::Begin(2))) && matches!(pages.job(), Some(Job::End(3))));
        pages.begin(4 * US, SIDE);
        let (shed, is_kept) = pages.take_ended();
        assert!(shed.is_empty() && is_kept, "one begun anew while the end went keeps both");
    }

    #[test]
    fn black_once_when_dark() {
        let mut pages = Pages::default();
        assert!(!pages.set_dark(true, 1) && newest(&pages).is_none(), "nothing is recorded while no recording runs");
        pages.begin(2 * US, SIDE);
        assert!(pages.set_dark(true, 3_000 * US));
        assert_eq!(newest(&pages), Some((3_000, 0, 0)), "a recording begun in the dark begins black");
        assert!(!pages.set_dark(false, 3_500 * US));
        let mut lit = pages.page().unwrap();
        lit.fill(7);
        pages.take_shown(lit, 4_000 * US);
        assert!(pages.set_dark(true, 4_500 * US));
        assert_eq!(newest(&pages), Some((4_500, 2, 0)), "black in place of the frame before, the screen gone dark");
        let black = pages.newest.take().unwrap();
        assert!(black.pixels.iter().all(|&pixel| pixel == 0));
        assert!(!pages.set_dark(true, 5_000 * US) && newest(&pages).is_none(), "once");
        pages.set_dark(false, 6_000 * US);
        assert!(pages.set_dark(true, 7_000 * US));
        assert_eq!(newest(&pages), Some((7_000, 0, 0)), "and again when it goes dark again");
    }
}

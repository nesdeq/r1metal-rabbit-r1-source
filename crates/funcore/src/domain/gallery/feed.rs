//! The decoding state of a video on show.

use crate::codec::h264::dpb::Output;
use crate::hw::vdec::StartOutcome;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;

/// The pictures the decoder may have out beside its stream's: the one drawn last and one on its way.
pub const PICTURES_OUT: usize = 2;
/// The refusal of a video without a sample to begin at.
pub const NO_SAMPLE: &str = "no sample to begin at";
/// The refusal of a first picture without a free buffer.
pub const NO_BUFFER: &str = "no picture buffer is free for the first picture";
/// The refusal of a video without a picture.
pub const NO_PICTURE: &str = "no picture in the video";

/// The log line of the first of a run of undecodable samples.
pub fn to_skip_line(number: usize, outcome: StartOutcome) -> String {
    format!("gallery: passing over what begins at sample {number}: {outcome:?}")
}

/// The outcome of a sample's start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SampleStart {
    // The decoder is on it, or it has no picture.
    Done,
    // No picture buffer is free: it starts again once one is back.
    Waiting,
    // `is_first` of a run, which the log tells of.
    Skipped { is_first: bool },
}

impl SampleStart {
    const fn is_consumed(self) -> bool {
        !matches!(self, SampleStart::Waiting)
    }
}

/// The thread's next step toward a video's first picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FirstStep {
    Begin,
    End,
    // The decoder has no free buffer, so no first picture comes.
    Full,
    Draw,
}

/// The thread's next step in a round of drawing ahead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DrawStep {
    Begin,
    Draw,
    End,
    Rest,
}

#[derive(Clone, Copy)]
enum Round {
    Begin(usize),
    Count(usize),
    Draw(usize),
}

/// The feed of a video's samples into the decoder.
pub struct Feed {
    first_sample: usize,
    next_sample: usize,
    is_read: bool,
    is_skipping: bool,
    has_started: bool,
    round: Round,
    pictures: VecDeque<Output>,
    last_buffer: Option<usize>,
    has_drained: bool,
}

impl Feed {
    /// A feed whose stream begins at the key sample `first_sample`.
    pub const fn new(first_sample: usize) -> Feed {
        Feed {
            first_sample,
            next_sample: first_sample,
            is_read: false,
            is_skipping: false,
            has_started: false,
            round: Round::Draw(0),
            pictures: VecDeque::new(),
            last_buffer: None,
            has_drained: false,
        }
    }

    /// The next sample to read of a video of `samples` samples.
    pub fn next_unread(&self, samples: usize) -> Option<usize> {
        (!self.is_read && self.next_sample < samples).then_some(self.next_sample)
    }

    /// Notes that the next sample is read.
    pub fn mark_read(&mut self) {
        self.is_read = true;
    }

    /// The sample read and waiting to start.
    pub fn ready_sample(&self) -> Option<usize> {
        self.is_read.then_some(self.next_sample)
    }

    /// Notes the end of the samples; true the first time, when the decoder gives out what it holds.
    pub fn note_end(&mut self) -> bool {
        !core::mem::replace(&mut self.has_drained, true)
    }

    /// Takes the outcome of the read sample's start.
    pub fn take_outcome(&mut self, outcome: StartOutcome) -> SampleStart {
        let start = match outcome {
            StartOutcome::Done => SampleStart::Done,
            StartOutcome::Full => SampleStart::Waiting,
            StartOutcome::Unsupported(_) => SampleStart::Skipped { is_first: !self.is_skipping },
        };
        self.has_started = start.is_consumed();
        if self.has_started {
            self.next_sample += 1;
            self.is_read = false;
            self.is_skipping = start != SampleStart::Done;
        }
        start
    }

    /// Adds the pictures the decoder gave out.
    pub fn add_pictures(&mut self, pictures: impl IntoIterator<Item = Output>) {
        self.pictures.extend(pictures);
    }

    /// The next step toward a video's first picture.
    pub fn first_step(&mut self) -> FirstStep {
        if core::mem::take(&mut self.has_started) {
            FirstStep::End
        } else if !self.is_pending() {
            FirstStep::Draw
        } else if self.is_read {
            FirstStep::Full
        } else {
            FirstStep::Begin
        }
    }

    /// Begins a round of drawing `count` pictures.
    pub const fn begin_round(&mut self, count: usize) {
        self.round = Round::Begin(count);
    }

    /// The next step of the round.
    pub fn next_step(&mut self) -> DrawStep {
        if let Round::Begin(count) = self.round {
            self.round = Round::Count(count);
            if self.should_begin(count) {
                return DrawStep::Begin;
            }
        }
        if let Round::Count(count) = self.round {
            self.round = Round::Draw(self.drawable_count(count));
        }
        if let Round::Draw(remaining @ 1..) = self.round {
            self.round = Round::Draw(remaining - 1);
            return DrawStep::Draw;
        }
        if core::mem::take(&mut self.has_started) { DrawStep::End } else { DrawStep::Rest }
    }

    fn should_begin(&self, count: usize) -> bool {
        self.pictures.len() <= count
    }

    fn drawable_count(&self, count: usize) -> usize {
        count.min(self.pictures.len())
    }

    /// The oldest picture, and the buffer of the one drawn before it to give back.
    pub fn take_next(&mut self) -> Option<(Output, Option<usize>)> {
        let next = self.pictures.pop_front()?;
        Some((next, self.last_buffer.replace(next.buffer)))
    }

    /// The buffer of the picture drawn last, which stays out for a turn of the device.
    pub const fn last_buffer(&self) -> Option<usize> {
        self.last_buffer
    }

    fn is_pending(&self) -> bool {
        self.pictures.is_empty() && !self.has_drained
    }

    /// True once the stream ended and every picture is drawn.
    pub fn is_done(&self) -> bool {
        self.has_drained && self.pictures.is_empty()
    }

    /// Goes back to the stream's start; returns the waiting pictures' buffers to give back.
    pub fn rewind(&mut self) -> impl Iterator<Item = usize> + '_ {
        self.next_sample = self.first_sample;
        self.is_read = false;
        self.is_skipping = false;
        self.has_drained = false;
        self.pictures.drain(..).map(|picture| picture.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::vdec::Unsupported;
    use alloc::vec::Vec;

    fn picture(buffer: usize, stamp: u64) -> Output {
        Output { buffer, stamp }
    }

    #[test]
    fn sample_reads_and_skips() {
        let mut feed = Feed::new(2);
        assert_eq!(
            (feed.next_unread(5), feed.ready_sample()),
            (Some(2), None),
            "the stream begins from its key sample"
        );
        feed.mark_read();
        assert_eq!((feed.next_unread(5), feed.ready_sample()), (None, Some(2)), "read once");
        assert_eq!(
            (feed.take_outcome(StartOutcome::Full), feed.ready_sample()),
            (SampleStart::Waiting, Some(2)),
            "begun again once a buffer is back"
        );
        assert_eq!((feed.take_outcome(StartOutcome::Done), feed.next_unread(5)), (SampleStart::Done, Some(3)));
        let unsupported = StartOutcome::Unsupported(Unsupported::Mismatch);
        feed.mark_read();
        assert_eq!(feed.take_outcome(unsupported), SampleStart::Skipped { is_first: true });
        feed.mark_read();
        assert_eq!(
            feed.take_outcome(unsupported),
            SampleStart::Skipped { is_first: false },
            "the log is told of the first of a run"
        );
        assert_eq!(
            to_skip_line(3, unsupported),
            "gallery: passing over what begins at sample 3: Unsupported(Mismatch)"
        );
        assert!(
            SampleStart::Skipped { is_first: false }.is_consumed()
                && SampleStart::Done.is_consumed()
                && !SampleStart::Waiting.is_consumed()
        );
        assert_eq!(feed.next_unread(5), None, "the video has five samples");
        assert!(
            feed.is_pending() && feed.note_end() && !feed.note_end(),
            "the decoder gives out its last pictures once"
        );
        assert!(!feed.is_pending() && feed.is_done());
    }

    #[test]
    fn draw_order_and_rewind() {
        let mut feed = Feed::new(0);
        feed.add_pictures([picture(0, 0), picture(1, 1), picture(2, 2)]);
        assert_eq!(
            (feed.should_begin(2), feed.should_begin(3), feed.drawable_count(2), feed.drawable_count(5)),
            (false, true, 2, 3)
        );
        assert_eq!(feed.take_next(), Some((picture(0, 0), None)));
        assert_eq!(feed.take_next(), Some((picture(1, 1), Some(0))), "the one drawn before goes back");
        feed.mark_read();
        feed.take_outcome(StartOutcome::Done);
        assert_eq!(feed.rewind().collect::<Vec<usize>>(), [2], "those that waited go back");
        assert_eq!(
            (feed.last_buffer(), feed.next_unread(9), feed.is_done()),
            (Some(1), Some(0), false),
            "the last drawn stays out"
        );
        assert_eq!(feed.take_next(), None);
    }

    #[test]
    fn first_picture_steps() {
        let mut feed = Feed::new(0);
        assert_eq!(feed.first_step(), FirstStep::Begin);
        feed.mark_read();
        feed.take_outcome(StartOutcome::Full);
        assert_eq!(feed.first_step(), FirstStep::Full, "no buffer is free");
        feed.take_outcome(StartOutcome::Done);
        assert_eq!(feed.first_step(), FirstStep::End);
        assert_eq!(feed.first_step(), FirstStep::Begin, "no picture came of it");
        feed.add_pictures([picture(0, 0)]);
        assert_eq!(feed.first_step(), FirstStep::Draw);
    }

    #[test]
    fn draw_round_steps() {
        let mut feed = Feed::new(0);
        feed.add_pictures([picture(0, 0), picture(1, 1)]);
        feed.begin_round(1);
        assert_eq!(feed.next_step(), DrawStep::Draw, "enough wait: no sample begins");
        feed.take_next();
        assert_eq!(feed.next_step(), DrawStep::Rest);
        feed.begin_round(2);
        assert_eq!(feed.next_step(), DrawStep::Begin);
        feed.mark_read();
        feed.take_outcome(StartOutcome::Done);
        assert_eq!(feed.next_step(), DrawStep::Draw, "as many as wait");
        feed.take_next();
        assert_eq!((feed.next_step(), feed.next_step()), (DrawStep::End, DrawStep::Rest));
        feed.begin_round(1);
        assert_eq!(feed.next_step(), DrawStep::Begin);
        feed.mark_read();
        feed.take_outcome(StartOutcome::Full);
        assert_eq!(feed.next_step(), DrawStep::Rest, "begun again the next round");
    }
}

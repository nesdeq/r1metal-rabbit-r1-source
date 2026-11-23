//! What the calls' threads and the main loop share.

use super::picture::{Arrival, Figures, Frame, Shown};
use super::{Order, View};
use crate::base::handoff::Handover;
use crate::base::pixel::PixelFormat;
use crate::codec::http::Head;
use crate::codec::vp8::Step;
use crate::domain::camera::CallPicture;
use crate::domain::signal::state::CallReport;
use crate::gfx::Picture;
use crate::signal::address::Peer;
use crate::signal::command::{Command, Outbound};
use crate::signal::device::{Incoming, Membership};
use crate::signal::service::Relay;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::net::IpAddr;
use core::time::Duration;

// The most far frames that wait to be decoded: libwebrtc's, past which its low-latency path stops pacing frames to its decoder to catch up (video_stream_buffer_controller.cc, `kZeroPlayoutDelayMaxDecodeQueueSize`).
const MOST_WAITING: usize = 8;

/// What the thread asked of the world beside its sockets, as it was found.
#[derive(Clone, PartialEq, Eq)]
pub enum Answer {
    // The addresses the device holds in the stay the call's hold made.
    Addresses(Vec<IpAddr>),
    // What the calling server answered the request of that number: its head and its body; none where no answer came.
    Fetched(u32, Option<(Head, Vec<u8>)>),
}

// A fetched answer is told by its number and status alone: a join's body holds the server's ICE password.
impl core::fmt::Debug for Answer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Answer::Addresses(addresses) => f.debug_tuple("Addresses").field(addresses).finish(),
            Answer::Fetched(number, answer) => {
                let status = answer.as_ref().map(|(head, _)| head.status);
                f.debug_tuple("Fetched").field(number).field(&status).finish()
            }
        }
    }
}

/// What the main loop and the pictures' threads hand the calls' thread since it last looked.
#[derive(Default)]
pub struct Wishes {
    pub(super) orders: Vec<Order>,
    pub(super) signals: Vec<Incoming>,
    pub(super) relays: Option<Vec<Relay>>,
    pub(super) tokens: Vec<(Peer, Option<Membership>)>,
    pub(super) unsent: Vec<Outbound>,
    pub(super) answers: Vec<Answer>,
    // This side's picture as the pictures' thread coded it, for the far side.
    pub(super) frames: Vec<Frame>,
    // The senders of far frames that did not decode, of whom a key frame is asked: each one's side at the agent and its number on that way.
    pub(super) flawed: Vec<(u32, u32)>,
}

impl Wishes {
    fn is_empty(&self) -> bool {
        let is_told = self.relays.is_some() || !self.tokens.is_empty() || !self.unsent.is_empty();
        let is_asked = !self.orders.is_empty() || !self.signals.is_empty() || !self.answers.is_empty();
        !is_asked && !is_told && self.frames.is_empty() && self.flawed.is_empty()
    }
}

/// What the pictures' thread does next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Coding {
    // Codes the picture swapped in at that many bits a second, as a key frame where asked.
    Code { rate: u64, is_key: bool },
    // No picture goes to the far side: the thread lets go of its encoder.
    Rest,
}

/// What the far pictures' thread does next.
pub enum Decoding {
    // Decodes the frame and makes its picture in that format, as large as that room takes it.
    Decode { arrival: Arrival, room: (usize, usize), format: PixelFormat },
    // No call stands: the thread lets go of its decoders.
    Rest,
}

/// Whom what came of a far frame is news for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Told {
    // The main loop, for the far side's newest picture.
    Screen,
    // The calls' thread, which asks a key frame of the frame's sender.
    Calls,
    Nobody,
}

/// The calls' report to the main loop.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct Report {
    /// What the call layer asks of Signal's thread, in order.
    pub commands: Vec<Command>,
    /// The call as its screen shows it, none for no call, where that changed.
    pub view: Option<Option<View>>,
    /// The far side's newest picture came, swapped into the screen's.
    pub has_picture: bool,
}

/// The state the calls' threads and the main loop share.
pub struct State {
    wishes: Wishes,
    report: Report,
    view: Option<View>,
    // The room the far side's picture is fitted to, and the screen's pixel words it is made of.
    room: (usize, usize),
    format: PixelFormat,
    // The rate this side's picture is coded at, none while none goes, and whether its next frame is a key frame.
    video: Option<u64>,
    wants_key: bool,
    // The camera's newest picture for the far side, fresh while it waits to be coded, and whether the pictures' thread holds its encoder.
    sent: Handover<CallPicture, ()>,
    is_coding: bool,
    // The far side's frames that wait to be decoded, in order; the senders whose frames were dropped, of whom none but a key frame is taken until it comes; whether the far pictures' thread holds its decoders; and its newest picture, swapped between it and the screen so that neither allocates.
    arrivals: VecDeque<Arrival>,
    cut: Vec<(u32, u32)>,
    is_decoding: bool,
    shown: Handover<Picture, ()>,
    // What the pictures cost since it was last taken.
    figures: Figures,
    // A call stands, from the owner's order or an offer's arrival until its end is told.
    is_standing: bool,
}

impl State {
    /// A state with no call, whose far pictures are made of the screen's pixel words in `format`.
    pub fn new(format: PixelFormat) -> State {
        State {
            wishes: Wishes::default(),
            report: Report::default(),
            view: None,
            room: (0, 0),
            format,
            video: None,
            wants_key: false,
            sent: Handover::default(),
            is_coding: false,
            arrivals: VecDeque::new(),
            cut: Vec::new(),
            is_decoding: false,
            shown: Handover::default(),
            figures: Figures::default(),
            is_standing: false,
        }
    }

    /// Queues what the owner asks of a call.
    pub fn order(&mut self, order: Order) {
        self.wishes.orders.push(order);
    }

    /// Takes out of Signal's report what is the call layer's, leaving the log and the callees for the screens; true where there is news for the thread.
    pub fn take_news(&mut self, report: &mut CallReport) -> bool {
        let wishes = &mut self.wishes;
        let is_news = !report.signals.is_empty()
            || report.relays.is_some()
            || !report.tokens.is_empty()
            || !report.unsent.is_empty();
        wishes.signals.append(&mut report.signals);
        wishes.relays = report.relays.take().or(wishes.relays.take());
        wishes.tokens.append(&mut report.tokens);
        wishes.unsent.append(&mut report.unsent);
        is_news
    }

    /// Sets the room the far side's picture is fitted to, as the screen names it, for the next frame decoded.
    pub const fn want(&mut self, room: (usize, usize)) {
        self.room = room;
    }

    /// Sets the bits a second this side's picture is coded at, none while none goes, and whether its next frame is to be a key frame; true where the pictures' thread has news.
    pub fn set_video(&mut self, rate: Option<u64>, wants_key: bool) -> bool {
        let is_news = self.video.is_some() != rate.is_some();
        (self.video, self.wants_key) = (rate, self.wants_key || wants_key);
        is_news
    }

    /// True while the camera is to make a picture of each frame of its stream for the far side, at its rate: while this side's picture goes, so that the encoder, once free, takes the newest, never one older than a frame.
    pub const fn wants_picture(&self) -> bool {
        self.video.is_some()
    }

    /// Swaps in the camera's newest picture for the far side, in the place of one that still waits, which is never coded; `picture` holds the old one to write over.
    pub fn add_picture(&mut self, picture: &mut CallPicture) {
        if self.sent.is_fresh() {
            self.figures.note_replaced();
        }
        self.sent.put(picture, ());
    }

    /// The pictures' thread's next step, the picture that waits swapped into `picture`; none while it waits for one.
    pub fn next_picture(&mut self, picture: &mut CallPicture) -> Option<Coding> {
        let Some(rate) = self.video else {
            self.sent = Handover::default();
            return core::mem::take(&mut self.is_coding).then_some(Coding::Rest);
        };
        self.sent.take(picture, |()| true)?;
        self.is_coding = true;
        Some(Coding::Code { rate, is_key: core::mem::take(&mut self.wants_key) })
    }

    /// Queues a frame the pictures' thread coded, for the calls' thread, with how long its coding took and each of its steps; one that comes once no picture goes is dropped, as nothing sends it.
    pub fn add_frame(&mut self, frame: Frame, span: Duration, steps: &[(Step, Duration)]) {
        if self.video.is_none() {
            return;
        }
        self.figures.note_coded(&frame, span, steps);
        self.wishes.frames.push(frame);
    }

    // Queues the far side's frames for the far pictures' thread while a call stands. A frame that finds `MOST_WAITING` waiting, of whichever senders, has them dropped, and is dropped too but for a key frame; each sender whose frames were dropped is asked for a key frame once, and its frames are dropped until its own key frame comes, since none has anything to stand on. True where the far pictures' thread has news, and the senders to ask a key frame of.
    pub(super) fn add_arrivals(&mut self, arrivals: Vec<Arrival>) -> (bool, Vec<(u32, u32)>) {
        if !self.is_standing {
            return (self.is_decoding, Vec::new());
        }
        let (mut is_news, mut dropped) = (false, Vec::new());
        for arrival in arrivals {
            let (sender, is_key) = (arrival.sender(), arrival.is_key());
            if self.arrivals.len() == MOST_WAITING {
                // A key frame that comes at the bound is its own sender's new start: nothing is asked of that one.
                let waiting = self.arrivals.drain(..).map(|waiting| waiting.sender());
                for cut in waiting.chain([sender]).filter(|&cut| !(is_key && cut == sender)) {
                    if !self.cut.contains(&cut) {
                        self.cut.push(cut);
                        dropped.push(cut);
                    }
                }
            }
            if is_key {
                self.cut.retain(|&cut| cut != sender);
            } else if self.cut.contains(&sender) {
                continue;
            }
            is_news = true;
            self.arrivals.push_back(arrival);
            self.figures.note_waiting(self.arrivals.len());
        }
        (is_news, dropped)
    }

    /// The far pictures' thread's next step; none while it waits for a frame.
    pub fn next_arrival(&mut self) -> Option<Decoding> {
        if !self.is_standing {
            return core::mem::take(&mut self.is_decoding).then_some(Decoding::Rest);
        }
        let arrival = self.arrivals.pop_front()?;
        self.is_decoding = true;
        Some(Decoding::Decode { arrival, room: self.room, format: self.format })
    }

    /// Takes what came of a far frame, with how long its decoding and the making of its picture took: its picture, swapped in for the screen, `picture` then holding one to make the next in; or its sender for the calls' thread, which asks a key frame of it; nothing once no call stands. Whom that is news for.
    pub fn add_shown(&mut self, shown: Shown, picture: &mut Picture, decoding: Duration, making: Duration) -> Told {
        if !self.is_standing {
            return Told::Nobody;
        }
        match shown {
            Shown::Picture => {
                self.figures.note_decoded(decoding, Some(making));
                self.shown.put(picture, ());
                Told::Screen
            }
            Shown::Nothing => {
                self.figures.note_decoded(decoding, None);
                Told::Nobody
            }
            Shown::Flaw(sender) => {
                self.figures.note_decoded(decoding, None);
                self.wishes.flawed.push(sender);
                Told::Calls
            }
        }
    }

    /// Takes what the pictures cost since it was last taken.
    pub fn take_figures(&mut self) -> Figures {
        core::mem::take(&mut self.figures)
    }

    /// Queues what was found for the thread.
    pub fn add_answer(&mut self, answer: Answer) {
        self.wishes.answers.push(answer);
    }

    /// Takes what the thread is to follow.
    pub fn take_wishes(&mut self) -> Wishes {
        core::mem::take(&mut self.wishes)
    }

    // Notes how the calls stand: what goes to Signal, the call for its screen, and whether a call stands, the far frames of one that ended let go of; true where that is news for the main loop.
    pub(super) fn note(&mut self, mut commands: Vec<Command>, view: Option<View>, is_standing: bool) -> bool {
        let mut is_news = !commands.is_empty() || self.is_standing != is_standing;
        self.report.commands.append(&mut commands);
        self.is_standing = is_standing;
        if !is_standing {
            (self.arrivals, self.cut, self.shown) = (VecDeque::new(), Vec::new(), Handover::default());
        }
        if self.view != view {
            self.view.clone_from(&view);
            (self.report.view, is_news) = (Some(view), true);
        }
        is_news
    }

    /// Notes that the thread ended: no call stands and no picture goes from then on, so that nothing is wanted of the camera, coded or kept for a thread that is gone.
    pub fn end_thread(&mut self) {
        self.wishes = Wishes::default();
        (self.video, self.wants_key) = (None, false);
        self.sent = Handover::default();
        self.note(Vec::new(), None, false);
    }

    /// Takes the report, the far side's newest picture swapped into `picture`, which then holds the screen's old one for the far pictures' thread to make one in.
    pub fn take_report(&mut self, picture: &mut Picture) -> Report {
        let has_picture = self.shown.take(picture, |()| true).is_some();
        Report { has_picture, ..core::mem::take(&mut self.report) }
    }

    /// True while a call stands, which holds Signal's socket for its signals.
    pub const fn is_standing(&self) -> bool {
        self.is_standing
    }

    /// True while no call stands and nothing waits for the thread.
    pub fn is_off(&self) -> bool {
        !self.is_standing && self.wishes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::super::picture::samples::{arrival, number};
    use super::super::{Ending, Phase};
    use super::*;
    use crate::net::rtp::extension::{Orientation, Turn};
    use crate::signal::address::ServiceId;
    use crate::signal::address::tests::address;
    use crate::signal::call::Signal;
    use crate::signal::command::Target;

    const ALICE: ServiceId = ServiceId::Aci([1; 16]);
    const RGB: PixelFormat = PixelFormat::XRGB8888;

    fn view(phase: Phase) -> View {
        View {
            peer: Peer::Contact(ALICE),
            name: None,
            is_outgoing: true,
            is_video: false,
            phase,
            since: None,
            is_muted: false,
            is_video_on: false,
            has_far_video: false,
            members: Vec::new(),
        }
    }

    #[test]
    fn wishes_taken_once() {
        let mut state = State::new(RGB);
        assert!(state.is_off() && !state.take_news(&mut CallReport::default()), "nothing in Signal's report");
        let incoming = Incoming {
            sender: address(ALICE, 2),
            name: None,
            group_name: None,
            signals: alloc::vec![Signal::Busy { call: 1 }],
            age: 0,
            is_accepted: true,
            identity: [1; 32],
            own_identity: [2; 32],
            own_device: 3,
        };
        let busy = Outbound { target: Target::Contact(ALICE), device: None, signal: Signal::Busy { call: 1 } };
        let mut report = CallReport {
            signals: alloc::vec![incoming.clone()],
            relays: Some(Vec::new()),
            unsent: alloc::vec![busy.clone()],
            log: Some(Vec::new()),
            ..CallReport::default()
        };
        assert!(state.take_news(&mut report) && !state.is_off());
        assert!(
            report.signals.is_empty() && report.relays.is_none() && report.unsent.is_empty() && report.log.is_some(),
            "the call layer's taken, the log left for the screens"
        );
        assert!(!state.take_news(&mut CallReport::default()), "relays told before stay told");
        state.order(Order::End);
        state.add_answer(Answer::Addresses(Vec::new()));
        let wishes = state.take_wishes();
        assert_eq!(
            (wishes.orders, wishes.signals, wishes.relays, wishes.unsent, wishes.answers),
            (
                alloc::vec![Order::End],
                alloc::vec![incoming],
                Some(Vec::new()),
                alloc::vec![busy],
                alloc::vec![Answer::Addresses(Vec::new())]
            )
        );
        assert!(state.is_off() && state.take_wishes().is_empty(), "once");
    }

    #[test]
    fn report_of_changes() {
        let mut state = State::new(RGB);
        assert!(!state.note(Vec::new(), None, false), "no call, as before");
        let calling = view(Phase::Calling);
        assert!(state.note(alloc::vec![Command::Relays], Some(calling.clone()), true));
        assert!(state.is_standing() && !state.is_off(), "a call that stands holds the device awake");
        assert!(!state.note(Vec::new(), Some(calling.clone()), true), "the same again is no news");
        assert!(state.note(alloc::vec![Command::Seen], Some(calling.clone()), true), "a word for Signal is");
        let picture = |word: u32| Picture { words: alloc::vec![word; 4], width: 2 };
        let (mut made, mut screen) = (picture(1), picture(9));
        let add = |state: &mut State, made: &mut Picture| {
            state.add_shown(Shown::Picture, made, Duration::ZERO, Duration::ZERO)
        };
        assert_eq!(add(&mut state, &mut made), Told::Screen, "and so is a picture");
        made = picture(2);
        assert_eq!(add(&mut state, &mut made), Told::Screen);
        assert!(made == picture(1), "the picture the screen never took comes back to make the next in");
        assert!(!state.note(Vec::new(), Some(calling.clone()), true));
        let report = state.take_report(&mut screen);
        assert_eq!(
            report,
            Report {
                commands: alloc::vec![Command::Relays, Command::Seen],
                view: Some(Some(calling)),
                has_picture: true
            },
            "what goes to Signal in its order, the view where it changed, and a picture"
        );
        assert!(screen == picture(2), "the newest alone, swapped into the screen's");
        made = picture(3);
        add(&mut state, &mut made);
        assert!(made == picture(9), "the screen's old one comes back to make the next in");
        state.take_report(&mut screen);
        assert_eq!(state.take_report(&mut screen), Report::default(), "told once");
        let ended = view(Phase::Ended(Ending::HungUp));
        assert!(state.note(Vec::new(), Some(ended.clone()), false) && state.is_off());
        assert_eq!(state.take_report(&mut screen), Report { view: Some(Some(ended)), ..Report::default() });
        assert_eq!(
            add(&mut state, &mut made),
            Told::Nobody,
            "a picture decoded after the call's end is news for nobody"
        );
        let mut picture = CallPicture { at_ns: 5, ..CallPicture::default() };
        assert!(!state.wants_picture() && state.next_picture(&mut picture).is_none(), "no picture goes, none wanted");
        assert!(state.set_video(Some(300_000), true) && !state.set_video(Some(310_000), false), "its start is news");
        assert!(state.wants_picture() && state.next_picture(&mut picture).is_none(), "wanted of the camera");
        state.add_picture(&mut picture);
        assert!(state.wants_picture(), "wanted while one waits to be coded, which the next takes the place of");
        assert_eq!(state.next_picture(&mut picture), Some(Coding::Code { rate: 310_000, is_key: true }));
        assert!(picture.at_ns == 5, "swapped out");
        let made_at = |at_ns: u64| CallPicture { at_ns, ..CallPicture::default() };
        for at_ns in [6, 7, 8] {
            state.add_picture(&mut made_at(at_ns));
        }
        assert_eq!(
            state.next_picture(&mut picture),
            Some(Coding::Code { rate: 310_000, is_key: false }),
            "a key frame once for each asking"
        );
        assert_eq!(picture.at_ns, 8, "the newest of three made while one was coded");
        state.add_picture(&mut picture);
        assert!(state.set_video(None, false), "its end is news");
        assert_eq!(
            (state.next_picture(&mut picture), state.next_picture(&mut picture), state.wants_picture()),
            (Some(Coding::Rest), None, false),
            "no picture goes: the encoder let go of once, a picture that waited with it"
        );
        state.order(Order::Accept);
        state.note(Vec::new(), Some(view(Phase::Talking)), true);
        state.end_thread();
        assert!(state.is_off() && state.take_wishes().is_empty(), "an ended thread holds nothing and takes nothing");
        assert_eq!(state.take_report(&mut screen).view, Some(None), "and the screens are told no call stands");
    }

    // A key frame the pictures' thread coded, of nothing.
    fn coded() -> Frame {
        Frame {
            bytes: Vec::new(),
            is_key: true,
            size: (0, 0),
            frame_rate: 0,
            sampled: Duration::ZERO,
            orientation: Orientation { turn: Turn::None, is_flipped: false, is_back: false },
            rate: 300_000,
            quantizer: 64,
        }
    }

    #[test]
    fn replaced_pictures_counted() {
        let mut state = State::new(RGB);
        state.set_video(Some(300_000), false);
        let mut picture = CallPicture::default();
        for _ in 0..3 {
            state.add_picture(&mut CallPicture::default());
        }
        state.next_picture(&mut picture);
        state.add_frame(coded(), Duration::from_millis(40), &[]);
        assert!(
            state.take_figures().to_lines()[0]
                .ends_with(", 2 more made by the camera and replaced by a newer one before they were coded"),
            "three made while the encoder was busy: the newest coded, two never"
        );
    }

    #[test]
    fn thread_end_lets_video_go() {
        let mut state = State::new(RGB);
        state.note(Vec::new(), Some(view(Phase::Talking)), true);
        state.set_video(Some(300_000), true);
        let mut picture = CallPicture::default();
        state.add_picture(&mut CallPicture::default());
        assert_eq!(state.next_picture(&mut picture), Some(Coding::Code { rate: 300_000, is_key: true }));
        state.add_picture(&mut CallPicture::default());
        state.end_thread();
        assert!(
            !state.wants_picture() && state.is_off(),
            "the thread gone while a picture went: none wanted of the camera"
        );
        state.add_frame(coded(), Duration::from_millis(40), &[]);
        assert!(
            state.is_off() && state.take_wishes().is_empty() && state.take_figures().to_lines().is_empty(),
            "a frame coded once the thread is gone is neither kept nor counted"
        );
        state.add_picture(&mut CallPicture::default());
        assert_eq!(
            (state.next_picture(&mut picture), state.next_picture(&mut picture)),
            (Some(Coding::Rest), None),
            "the encoder let go of once, and neither the picture that waited nor one the camera still had is coded"
        );
    }

    // The frames' first bytes in the order the far pictures' thread takes them, and whether it rests after them.
    fn taken(state: &mut State) -> (Vec<u8>, bool) {
        let (mut bytes, mut is_rest) = (Vec::new(), false);
        while let Some(step) = state.next_arrival() {
            match step {
                Decoding::Decode { arrival, .. } => bytes.push(number(&arrival)),
                Decoding::Rest => is_rest = true,
            }
        }
        (bytes, is_rest)
    }

    #[test]
    fn far_frames_bounded() {
        const SENDER: (u32, u32) = (2, 2000);
        let mut state = State::new(RGB);
        let frames = |numbers: core::ops::Range<u8>| numbers.map(|number| arrival(number, number % 10 == 0, SENDER));
        assert_eq!(state.add_arrivals(frames(0..3).collect()), (false, Vec::new()), "no call: no frame taken");
        assert_eq!(taken(&mut state), (Vec::new(), false));
        state.note(Vec::new(), None, true);
        state.want((240, 300));
        assert_eq!(state.add_arrivals(frames(0..3).collect()), (true, Vec::new()), "a call's frames queued");
        let Some(Decoding::Decode { room, format, .. }) = state.next_arrival() else { panic!("a frame to decode") };
        assert_eq!((room, format), ((240, 300), RGB), "with the room the screen named and its pixel words");
        assert_eq!(taken(&mut state), (alloc::vec![1, 2], false), "the rest in order");
        assert_eq!(
            state.add_arrivals(frames(1..10).collect()),
            (true, alloc::vec![SENDER]),
            "a ninth while eight wait: all dropped, and a key frame asked of their sender"
        );
        assert_eq!(state.add_arrivals(frames(11..13).collect()), (false, Vec::new()), "nothing to stand on");
        assert_eq!(taken(&mut state), (Vec::new(), false));
        assert_eq!(state.add_arrivals(frames(20..22).collect()), (true, Vec::new()), "from a key frame on");
        assert_eq!(taken(&mut state), (alloc::vec![20, 21], false));
        assert_eq!(state.add_arrivals(frames(21..29).collect()), (true, Vec::new()));
        assert_eq!(
            state.add_arrivals(frames(30..32).collect()),
            (true, Vec::new()),
            "a key frame past the bound drops those that wait and needs nothing asked"
        );
        assert_eq!(taken(&mut state), (alloc::vec![30, 31], false));
        let (last, mut figures) = (frames(40..41).collect(), Figures::default());
        state.add_arrivals(last);
        (1..=8).for_each(|count| figures.note_waiting(count));
        assert_eq!(state.take_figures(), figures, "the most that waited at once");
        let flaw = state.add_shown(Shown::Flaw(SENDER), &mut Picture::default(), Duration::ZERO, Duration::ZERO);
        assert_eq!(flaw, Told::Calls);
        assert_eq!(
            state.take_wishes().flawed,
            [SENDER],
            "a frame that did not decode: its sender for the calls' thread"
        );
        state.note(Vec::new(), None, false);
        assert_eq!(state.add_arrivals(Vec::new()), (true, Vec::new()), "the call's end is news for the decoder");
        assert_eq!(
            taken(&mut state),
            (Vec::new(), true),
            "which lets go of its decoders once, the frame that waited with them"
        );
        assert_eq!(state.add_arrivals(Vec::new()), (false, Vec::new()));
    }

    // A group's call through the calling server: one side, two senders on it.
    #[test]
    fn far_frames_of_two_senders() {
        const ANN: (u32, u32) = (0, 0x1000);
        const BEN: (u32, u32) = (0, 0x2000);
        let mut state = State::new(RGB);
        state.note(Vec::new(), None, true);
        let frames = |numbers: &[(u8, (u32, u32))]| -> Vec<Arrival> {
            numbers.iter().map(|&(number, sender)| arrival(number, number % 10 == 0, sender)).collect()
        };
        assert_eq!(state.add_arrivals(frames(&[(10, ANN), (50, BEN), (11, ANN), (51, BEN)])), (true, Vec::new()));
        assert_eq!(taken(&mut state), (alloc::vec![10, 50, 11, 51], false), "taken in the order they came");
        let waiting = [(12, ANN), (52, BEN), (13, ANN), (53, BEN), (14, ANN), (54, BEN), (15, ANN), (55, BEN)];
        assert_eq!(state.add_arrivals(frames(&waiting)), (true, Vec::new()), "eight of the two together wait");
        assert_eq!(
            state.add_arrivals(frames(&[(56, BEN)])),
            (false, alloc::vec![ANN, BEN]),
            "a ninth over both: all dropped, a key frame asked once of each sender whose frames were"
        );
        assert_eq!(state.add_arrivals(frames(&[(16, ANN), (60, BEN), (61, BEN), (17, ANN)])), (true, Vec::new()));
        assert_eq!(
            taken(&mut state),
            (alloc::vec![60, 61], false),
            "one sender's key frame lets its own frames through, not the other's, which stand on frames dropped"
        );
        assert_eq!(state.add_arrivals(frames(&[(20, ANN), (21, ANN)])), (true, Vec::new()));
        assert_eq!(taken(&mut state), (alloc::vec![20, 21], false), "the other's from its own key frame on");
        let waiting = [(22, ANN), (62, BEN), (23, ANN), (63, BEN), (24, ANN), (64, BEN), (25, ANN), (65, BEN)];
        state.add_arrivals(frames(&waiting));
        assert_eq!(
            state.add_arrivals(frames(&[(30, ANN)])),
            (true, alloc::vec![BEN]),
            "a key frame past the bound: nothing asked of its own sender, a key frame of the other"
        );
        assert_eq!(state.add_arrivals(frames(&[(66, BEN), (31, ANN)])), (true, Vec::new()));
        assert_eq!(taken(&mut state), (alloc::vec![30, 31], false), "the other's frames held until its key frame");
        let flaw = state.add_shown(Shown::Flaw(BEN), &mut Picture::default(), Duration::ZERO, Duration::ZERO);
        assert_eq!(
            (flaw, state.take_wishes().flawed),
            (Told::Calls, alloc::vec![BEN]),
            "a flawed frame's sender alone"
        );
    }
}

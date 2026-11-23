use super::{Ending, Phase};
use crate::domain::cause::Cause;
use crate::signal::address::ServiceId;
use crate::signal::call::data::{Message, Sending};
use crate::signal::call::{Candidate, Hangup, Parameters, Signal};
use crate::signal::command::{Outbound, Target};
use crate::signal::device::Course;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::time::Duration;

// RingRTC's `TIME_OUT_PERIOD`: a call not talked in by then, counted from the owner's click or the offer's arrival, is over.
const SETUP_TIME: Duration = Duration::from_secs(60);
// RingRTC's `TICK_INTERVAL` times `SEND_RTP_DATA_MESSAGE_INTERVAL_TICKS`: how often the far side of a taken call is told again what it was told.
const RETELL_EVERY: Duration = Duration::from_secs(1);

// How far a call, or its way to one device of the far side, has come (RingRTC's `CallState` and `ConnectionState`, from the offer or the answer on).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Connecting,
    // The way stands and the call is not taken: it rings.
    Connected,
    // Taken before the way stands.
    Accepted,
    Talking,
    Reconnecting,
}

impl Stage {
    const fn is_accepted(self) -> bool {
        matches!(self, Stage::Accepted | Stage::Talking | Stage::Reconnecting)
    }

    const fn is_talked_in(self) -> bool {
        matches!(self, Stage::Talking | Stage::Reconnecting)
    }
}

// A change of the way to a device of the far side, as its ICE agent tells it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Way {
    Up,
    Down,
    Lost,
}

// What a step of a call's course asks of what carries the call.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Effect {
    // A signal to the far account: to all its devices, or to the one named.
    Signal(Outbound),
    // A word to a device of the far side over its way, as `rtp_data.proto` writes it.
    Tell(u32, Vec<u8>),
    // A device of the far side's way opens with the parameters it sent.
    Open(u32, Parameters),
    // The candidates a device sent for its way.
    Candidates(u32, Vec<Candidate>),
    // The way to a device is let go of.
    Close(u32),
    // The ways are sought anew on every network.
    Regather,
    // Sound and pictures flow from now on, over the way to that device.
    Media(u32),
    // What the far side sends of itself.
    Far(Sending),
    // The most the far side wants sent to it, in bits a second.
    Limit(u64),
    // The call log's word of the call.
    Log(Course),
}

// What an offer from the account a call is with does to that call and to itself (RingRTC's `check_for_collision`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Collision {
    // The call stays; the offer is answered busy.
    Busy,
    // The call stays; the offer is passed over.
    Kept,
    // The call ends with a hang-up, or without one where the far side began anew, and the offer's call begins.
    Replaced { with_hangup: bool },
    // The call ends with a hang-up and the offer is answered busy.
    Both,
}

struct Far {
    device: u32,
    stage: Stage,
    // What it is told over its way as it stands, every change counted in its sequence number.
    told: Message,
    // The newest word taken of what it sends of itself and of the most it wants sent, each by its sequence number.
    sending: Option<(u64, Sending)>,
    limit: Option<(u64, u64)>,
}

impl Far {
    fn new(device: u32) -> Far {
        Far { device, stage: Stage::Connecting, told: Message::default(), sending: None, limit: None }
    }

    // Changes what it is told and tells it all (`update_and_send_rtp_data_message`).
    fn tell(&mut self, change: impl FnOnce(&mut Message)) -> Effect {
        change(&mut self.told);
        self.told.seqnum = Some(self.told.seqnum.unwrap_or(0) + 1);
        Effect::Tell(self.device, self.told.write())
    }
}

// A one-to-one call from the owner's click or the offer's arrival to its end: RingRTC's `Call` with its `Connection`s, one for each device of the far side that answered.
pub(super) struct Call {
    id: u64,
    peer: ServiceId,
    // This device's number among the account's, for a call that came in: a hang-up that names it is its own doing, passed on.
    own_device: Option<u32>,
    is_outgoing: bool,
    is_video: bool,
    // The offer that came, until this side's parameters answer it, with the candidates that came for its way meanwhile.
    offer: Option<(Parameters, Vec<Candidate>)>,
    is_offered: bool,
    stage: Stage,
    ending: Option<Ending>,
    fars: Vec<Far>,
    // The device the call is with: the caller's, or the callee's that took it.
    active: Option<u32>,
    began: Duration,
    retell_at: Option<Duration>,
    has_rung: bool,
    sending: Sending,
}

impl Call {
    fn new(id: u64, peer: ServiceId, is_video: bool, began: Duration) -> Call {
        Call {
            id,
            peer,
            own_device: None,
            is_outgoing: true,
            is_video,
            offer: None,
            is_offered: false,
            stage: Stage::Connecting,
            ending: None,
            fars: Vec::new(),
            active: None,
            began,
            retell_at: None,
            has_rung: false,
            sending: Sending { is_video_on: Some(is_video), is_sharing_screen: None, is_audio_on: Some(true) },
        }
    }

    // A call the owner begins at `now`, with this side's picture where `is_video`.
    pub(super) fn outgoing(id: u64, peer: ServiceId, is_video: bool, now: Duration) -> Call {
        Call::new(id, peer, is_video, now)
    }

    // A call an offer of the device `caller` brings at `now`; this side's picture is not sent until the owner has it sent.
    pub(super) fn incoming(
        id: u64,
        (peer, caller): (ServiceId, u32),
        own_device: u32,
        (is_video, offer): (bool, Parameters),
        now: Duration,
    ) -> Call {
        let sending = Sending { is_video_on: Some(false), is_sharing_screen: None, is_audio_on: Some(true) };
        Call {
            own_device: Some(own_device),
            is_outgoing: false,
            offer: Some((offer, Vec::new())),
            active: Some(caller),
            sending,
            ..Call::new(id, peer, is_video, now)
        }
    }

    pub(super) const fn id(&self) -> u64 {
        self.id
    }

    pub(super) const fn peer(&self) -> ServiceId {
        self.peer
    }

    pub(super) const fn is_outgoing(&self) -> bool {
        self.is_outgoing
    }

    // True where the call was offered with a picture.
    pub(super) const fn is_video(&self) -> bool {
        self.is_video
    }

    pub(super) const fn ending(&self) -> Option<Ending> {
        self.ending
    }

    // How the call stands for its screen; none for one that came in and does not ring yet.
    pub(super) const fn phase(&self) -> Option<Phase> {
        Some(match (self.ending, self.stage, self.is_outgoing) {
            (Some(ending), _, _) => Phase::Ended(ending),
            (None, Stage::Connecting, true) => Phase::Calling,
            (None, Stage::Connecting, false) => return None,
            (None, Stage::Connected, true) => Phase::Ringing,
            (None, Stage::Connected, false) => Phase::Incoming,
            (None, Stage::Accepted, _) => Phase::Connecting,
            (None, Stage::Talking, _) => Phase::Talking,
            (None, Stage::Reconnecting, _) => Phase::Reconnecting,
        })
    }

    // What an offer of the call `id` from `device` of the account this call is with comes to.
    pub(super) fn collide(&self, device: u32, id: u64) -> Collision {
        match self.active {
            Some(active) if active != device => Collision::Busy,
            Some(_) if self.stage.is_talked_in() => Collision::Replaced { with_hangup: false },
            _ => match self.id.cmp(&id) {
                Ordering::Greater => Collision::Kept,
                Ordering::Less => Collision::Replaced { with_hangup: true },
                Ordering::Equal => Collision::Both,
            },
        }
    }

    fn far(&self, device: u32) -> Option<&Far> {
        self.fars.iter().find(|far| far.device == device)
    }

    fn far_mut(&mut self, device: u32) -> Option<&mut Far> {
        self.fars.iter_mut().find(|far| far.device == device)
    }

    fn signal(&self, device: Option<u32>, signal: Signal) -> Effect {
        Effect::Signal(Outbound { target: Target::Contact(self.peer), device, signal })
    }

    // Goes on with this side's parameters once its way is made (`Call::proceed`): the offer goes to every device of the callee's, or the offer that came is answered and its way opened.
    pub(super) fn proceed(&mut self, parameters: Parameters) -> Vec<Effect> {
        if self.ending.is_some() {
            return Vec::new();
        }
        let parameters = Some(parameters);
        let Some(((offered, candidates), caller)) = self.offer.take().zip(self.active) else {
            if !self.is_outgoing || core::mem::replace(&mut self.is_offered, true) {
                return Vec::new();
            }
            return vec![self.signal(None, Signal::Offer { call: self.id, is_video: self.is_video, parameters })];
        };
        self.fars.push(Far::new(caller));
        let answer = self.signal(Some(caller), Signal::Answer { call: self.id, parameters });
        let mut effects = vec![Effect::Open(caller, offered), answer];
        if !candidates.is_empty() {
            effects.push(Effect::Candidates(caller, candidates));
        }
        effects
    }

    // This side's candidates as they are found and lost (`send_buffered_local_ice_candidates`): to every device of a callee's, or to the caller's device.
    pub(super) fn tell_candidates(&self, candidates: Vec<Candidate>) -> Vec<Effect> {
        if self.ending.is_some() || candidates.is_empty() {
            return Vec::new();
        }
        let device = self.active.filter(|_| !self.is_outgoing);
        vec![self.signal(device, Signal::Candidates { call: self.id, candidates })]
    }

    // A signal of the call's from a device of the far side.
    pub(super) fn take_signal(&mut self, device: u32, signal: Signal) -> Vec<Effect> {
        if self.ending.is_some() || signal.call() != Some(self.id) {
            return Vec::new();
        }
        match signal {
            Signal::Answer { parameters: Some(parameters), .. } => self.take_answer(device, parameters),
            Signal::Candidates { candidates, .. } => self.take_candidates(device, candidates),
            Signal::Hangup { kind, device: named, .. } => self.take_hangup(device, kind, named),
            Signal::Busy { .. } => self.take_busy(device),
            Signal::Answer { .. } | Signal::Offer { .. } | Signal::Group { .. } => Vec::new(),
        }
    }

    // A device's first answer opens its way, while no device took the call (`Call::received_answer`).
    fn take_answer(&mut self, device: u32, parameters: Parameters) -> Vec<Effect> {
        let is_awaited = self.is_outgoing && self.is_offered && !self.stage.is_accepted();
        if !is_awaited || self.far(device).is_some() {
            return Vec::new();
        }
        self.fars.push(Far::new(device));
        vec![Effect::Open(device, parameters)]
    }

    // Candidates of a device that has not answered are passed over, where RingRTC ends the call: its clients send the answer first.
    fn take_candidates(&mut self, device: u32, mut candidates: Vec<Candidate>) -> Vec<Effect> {
        if let Some((_, waiting)) = &mut self.offer {
            if self.active == Some(device) {
                waiting.append(&mut candidates);
            }
            return Vec::new();
        }
        if self.far(device).is_none() {
            return Vec::new();
        }
        vec![Effect::Candidates(device, candidates)]
    }

    // A callee's device that is in another call ends this one for all its devices (`handle_received_busy`); once a device took the call, another's busy changes nothing.
    fn take_busy(&mut self, device: u32) -> Vec<Effect> {
        if !self.is_outgoing || self.stage.is_accepted() {
            return Vec::new();
        }
        let mut effects = self.pass_on(Hangup::Busy, device);
        effects.extend(self.finish(Ending::Busy, false, Course::Missed));
        effects
    }

    // A hang-up of a device's, through the service or over its way (`handle_received_hangup`).
    fn take_hangup(&mut self, sender: u32, kind: Hangup, named: Option<u32>) -> Vec<Effect> {
        let is_own = named.is_some() && named == self.own_device;
        if is_own || self.active.is_some_and(|active| active != sender) {
            return Vec::new();
        }
        let is_taken = self.stage.is_accepted();
        let (passed_on, ending, course) = match (kind, self.is_outgoing) {
            (Hangup::NeedPermission, true) => (Some(Hangup::NeedPermission), Ending::Unpermitted, Course::Missed),
            (Hangup::Normal, true) if !is_taken => (Some(Hangup::Declined), Ending::Declined, Course::RemoteHangup),
            (Hangup::Accepted, false) => (None, Ending::Elsewhere, Course::Accepted),
            (Hangup::Declined | Hangup::Busy, false) => (None, Ending::Elsewhere, Course::Missed),
            _ => (None, Ending::FarHungUp, Course::RemoteHangup),
        };
        let mut effects = match passed_on {
            Some(kind) if !is_taken => self.pass_on(kind, sender),
            _ => Vec::new(),
        };
        effects.extend(self.finish(ending, false, course));
        effects
    }

    // Tells the callee's other devices how the call ended on one of them, over their ways and through the service (`send_hangup_via_rtp_data_and_signaling_to_all_except`).
    fn pass_on(&mut self, kind: Hangup, device: u32) -> Vec<Effect> {
        let id = self.id;
        let others = self.fars.iter_mut().filter(|far| far.device != device);
        let mut effects: Vec<Effect> =
            others.map(|far| far.tell(|told| told.hangup = Some((id, kind, Some(device))))).collect();
        effects.push(self.signal(None, Signal::Hangup { call: id, kind, device: Some(device) }));
        effects
    }

    // RingRTC's `should_send_hangup_on_failure`: a call that came in tells of a failure only once its owner took it, so that the caller's ring goes on at the account's other devices.
    const fn tells_failure(&self) -> bool {
        self.is_outgoing || self.stage.is_accepted()
    }

    // Ends the call (`terminate_call`): where `with_hangup`, with a plain hang-up over every way and, unless the far side never heard of the call, through the service; every way is let go of.
    fn finish(&mut self, ending: Ending, with_hangup: bool, course: Course) -> Vec<Effect> {
        if self.ending.is_some() {
            return Vec::new();
        }
        let id = self.id;
        let mut effects = Vec::new();
        if with_hangup {
            let plain = move |told: &mut Message| told.hangup = Some((id, Hangup::Normal, None));
            effects.extend(self.fars.iter_mut().map(|far| far.tell(plain)));
            if !self.is_outgoing || self.is_offered {
                effects.push(self.signal(None, Signal::Hangup { call: id, kind: Hangup::Normal, device: None }));
            }
        }
        effects.extend(self.fars.drain(..).map(|far| Effect::Close(far.device)));
        effects.push(Effect::Log(course));
        (self.ending, self.retell_at, self.offer) = (Some(ending), None, None);
        effects
    }

    // A word a device sent over its way (`inject_received_via_rtp_data`).
    pub(super) fn take_word(&mut self, device: u32, word: &Message, now: Duration) -> Vec<Effect> {
        if self.ending.is_some() || self.far(device).is_none() {
            return Vec::new();
        }
        let id = self.id;
        let mut effects = Vec::new();
        if word.accepted == Some(id) && self.is_outgoing {
            effects.extend(self.take_accepted(device, now));
        }
        if let Some((call, kind, named)) = word.hangup
            && call == id
        {
            effects.extend(self.take_hangup(device, kind, named));
        }
        let is_heard = self.stage.is_talked_in() && self.active == Some(device);
        let Some((far, seqnum)) = self.far_mut(device).zip(word.seqnum) else { return effects };
        if let Some((call, sending)) = word.sending
            && call == id
            && far.sending.is_none_or(|(last, _)| seqnum > last)
        {
            let is_new = far.sending.is_none_or(|(_, held)| held != sending);
            far.sending = Some((seqnum, sending));
            effects.extend((is_new && is_heard).then_some(Effect::Far(sending)));
        }
        if let Some((call, limit)) = word.max_bitrate
            && call == id
            && far.limit.is_none_or(|(last, _)| seqnum > last)
        {
            let is_new = far.limit.is_none_or(|(_, held)| held != limit);
            far.limit = Some((seqnum, limit));
            effects.extend((is_new && is_heard).then_some(Effect::Limit(limit)));
        }
        effects
    }

    // The callee's device took the call (`handle_received_accepted_via_rtp_data`).
    fn take_accepted(&mut self, device: u32, now: Duration) -> Vec<Effect> {
        match self.far(device).map(|far| far.stage) {
            Some(Stage::Connecting) => self.advance(device, Stage::Accepted, now),
            Some(Stage::Connected) => self.advance(device, Stage::Talking, now),
            _ => Vec::new(),
        }
    }

    // A change of the way to a device (`handle_ice_connected`, `handle_ice_disconnected`, `Call::handle_ice_failed`).
    pub(super) fn note_way(&mut self, device: u32, way: Way, now: Duration) -> Vec<Effect> {
        let Some(stage) = self.far(device).map(|far| far.stage).filter(|_| self.ending.is_none()) else {
            return Vec::new();
        };
        match (way, stage) {
            (Way::Up, Stage::Connecting) => self.advance(device, Stage::Connected, now),
            (Way::Up, Stage::Accepted | Stage::Reconnecting) => self.advance(device, Stage::Talking, now),
            (Way::Down, Stage::Connected) => self.advance(device, Stage::Connecting, now),
            (Way::Down, Stage::Talking) => self.advance(device, Stage::Reconnecting, now),
            (Way::Lost, _) => self.lose_way(device),
            _ => Vec::new(),
        }
    }

    // A way that is lost for good ends the call where it is the one the call is with or the last; any other is let go of.
    fn lose_way(&mut self, device: u32) -> Vec<Effect> {
        if self.active.is_none() && self.fars.len() > 1 {
            self.fars.retain(|far| far.device != device);
            return vec![Effect::Close(device)];
        }
        if self.active.is_some_and(|active| active != device) {
            return Vec::new();
        }
        self.fail(Cause::NoNetwork)
    }

    // Moves the way to a device on, and the call with it (`handle_connection_observer_event`).
    fn advance(&mut self, device: u32, to: Stage, now: Duration) -> Vec<Effect> {
        let Some(far) = self.far_mut(device) else { return Vec::new() };
        far.stage = to;
        let is_active = self.active == Some(device);
        match (self.stage, to) {
            (Stage::Connecting, Stage::Connected) => {
                self.stage = Stage::Connected;
                self.ring()
            }
            (Stage::Connecting | Stage::Connected, Stage::Accepted) if self.is_outgoing => {
                self.stage = Stage::Accepted;
                self.settle_on(device)
            }
            (Stage::Connected, Stage::Talking) if self.is_outgoing => {
                let mut effects = self.settle_on(device);
                effects.extend(self.talk(device, now));
                effects
            }
            (Stage::Accepted, Stage::Talking) if is_active => {
                let mut effects = self.ring();
                effects.extend(self.talk(device, now));
                effects
            }
            (Stage::Talking, Stage::Reconnecting) if is_active => {
                self.stage = Stage::Reconnecting;
                vec![Effect::Regather]
            }
            (Stage::Reconnecting, Stage::Talking) if is_active => {
                self.stage = Stage::Talking;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    // The call rings, there or here, which the log is told once.
    fn ring(&mut self) -> Vec<Effect> {
        if core::mem::replace(&mut self.has_rung, true) { Vec::new() } else { vec![Effect::Log(Course::Ringing)] }
    }

    // The callee's device that took the call is the one it is with: the others are told so and let go of (`handle_remotely_accepted_connection`).
    fn settle_on(&mut self, device: u32) -> Vec<Effect> {
        self.active = Some(device);
        let mut effects = self.pass_on(Hangup::Accepted, device);
        let (kept, others): (Vec<Far>, Vec<Far>) =
            core::mem::take(&mut self.fars).into_iter().partition(|far| far.device == device);
        self.fars = kept;
        effects.extend(others.into_iter().map(|far| Effect::Close(far.device)));
        effects
    }

    // The call is taken and its way stands (`handle_connected_and_accepted_for_the_first_time`, `accept_locally`, `accept_remotely`): what the far side told meanwhile counts, it is told what this side sends and, by a callee, that the call is taken, and sound and pictures flow.
    fn talk(&mut self, device: u32, now: Duration) -> Vec<Effect> {
        (self.stage, self.retell_at) = (Stage::Talking, Some(now + RETELL_EVERY));
        let (id, sending, is_callee) = (self.id, self.sending, !self.is_outgoing);
        let Some(far) = self.far_mut(device) else { return Vec::new() };
        let mut effects = Vec::new();
        effects.extend(far.limit.map(|(_, limit)| Effect::Limit(limit)));
        effects.extend(far.sending.map(|(_, sending)| Effect::Far(sending)));
        effects.push(far.tell(|told| {
            told.accepted = told.accepted.or(is_callee.then_some(id));
            told.sending = Some((id, sending));
        }));
        effects.extend([Effect::Media(device), Effect::Log(Course::Accepted)]);
        effects
    }

    // The owner takes the call that rings here (`handle_accept_call`).
    pub(super) fn accept(&mut self, now: Duration) -> Vec<Effect> {
        let rings = !self.is_outgoing && self.ending.is_none() && self.stage == Stage::Connected;
        let Some(device) = self.active.filter(|_| rings) else { return Vec::new() };
        if let Some(far) = self.far_mut(device) {
            far.stage = Stage::Talking;
        }
        self.talk(device, now)
    }

    // The owner ends the call, or sends away the one that rings (`handle_hangup`).
    pub(super) fn end(&mut self) -> Vec<Effect> {
        self.finish(Ending::HungUp, true, Course::Hangup)
    }

    // What this side sends from now on, which the far side is told once the call is talked in.
    pub(super) fn set_sending(&mut self, sending: Sending) -> Vec<Effect> {
        self.sending = sending;
        let id = self.id;
        let is_told = self.ending.is_none() && self.stage.is_talked_in();
        let Some(far) = self.active.filter(|_| is_told).and_then(|device| self.far_mut(device)) else {
            return Vec::new();
        };
        vec![far.tell(|told| told.sending = Some((id, sending)))]
    }

    // A signal of the call's that the service did not take (`handle_message_send_failure`): candidates lost once a way stands change nothing, nor does a hang-up, where RingRTC ends the call for one passed on.
    pub(super) fn note_unsent(&mut self, signal: &Signal) -> Vec<Effect> {
        let has_way = matches!(self.stage, Stage::Connected | Stage::Talking | Stage::Reconnecting);
        let is_spared = match signal {
            Signal::Candidates { .. } => has_way,
            Signal::Hangup { .. } => true,
            _ => false,
        };
        if is_spared || signal.call() != Some(self.id) {
            return Vec::new();
        }
        self.fail(Cause::NoNetwork)
    }

    // The call cannot go on for a failure on this side (`internal_error`, `connection_failure`).
    pub(super) fn fail(&mut self, cause: Cause) -> Vec<Effect> {
        let with_hangup = self.tells_failure();
        self.finish(Ending::Failed(cause), with_hangup, Course::Missed)
    }

    // Ends for an offer of the far side's that takes the call's place (`Collision::Replaced`, `Collision::Both`).
    pub(super) fn give_way(&mut self, with_hangup: bool) -> Vec<Effect> {
        self.finish(Ending::FarHungUp, with_hangup, Course::Missed)
    }

    // What is due at `now`: the end of a call not talked in in time (`handle_call_timeout`), and the far side told again what it was told.
    pub(super) fn tick(&mut self, now: Duration) -> Vec<Effect> {
        if self.ending.is_some() {
            return Vec::new();
        }
        if !self.stage.is_talked_in() {
            if now < self.began + SETUP_TIME {
                return Vec::new();
            }
            let is_taken = self.stage == Stage::Accepted;
            let ending = if is_taken { Ending::Failed(Cause::NoNetwork) } else { Ending::Unanswered };
            return self.finish(ending, true, Course::Missed);
        }
        let Some(device) = self.active.filter(|_| self.retell_at.is_some_and(|at| now >= at)) else {
            return Vec::new();
        };
        self.retell_at = Some(now + RETELL_EVERY);
        self.far(device).map(|far| Effect::Tell(device, far.told.write())).into_iter().collect()
    }

    // The time until something is due; none once the call is over.
    pub(super) fn next_wait(&self, now: Duration) -> Option<Duration> {
        if self.ending.is_some() {
            return None;
        }
        let due = if self.stage.is_talked_in() { self.retell_at? } else { self.began + SETUP_TIME };
        Some(due.saturating_sub(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::call::Codec;

    const ALICE: ServiceId = ServiceId::Aci([0xA1; 16]);
    const OWN: u32 = 2;
    const CALL: u64 = 77;
    const NOW: Duration = Duration::from_secs(1_000);

    fn parameters(key: u8) -> Parameters {
        Parameters {
            public_key: [key; 32],
            ufrag: "Xy4+".into(),
            pwd: "0123456789abcdefghijkl".into(),
            max_bitrate: Some(2_000_000),
            encodes: vec![Codec::Vp8],
            decodes: vec![Codec::Vp8],
        }
    }

    fn to_all(signal: Signal) -> Effect {
        Effect::Signal(Outbound { target: Target::Contact(ALICE), device: None, signal })
    }

    fn hangup(kind: Hangup, device: Option<u32>) -> Signal {
        Signal::Hangup { call: CALL, kind, device }
    }

    fn told(effect: &Effect) -> (u32, Message) {
        let Effect::Tell(device, bytes) = effect else { panic!("a word over a way, not {effect:?}") };
        (*device, Message::read(bytes).expect("a word that reads"))
    }

    fn answer(key: u8) -> Signal {
        Signal::Answer { call: CALL, parameters: Some(parameters(key)) }
    }

    // A call begun here, offered, and answered by the devices 1 and 3 of the callee's.
    fn offered() -> Call {
        let mut call = Call::outgoing(CALL, ALICE, false, NOW);
        let offer = Signal::Offer { call: CALL, is_video: false, parameters: Some(parameters(9)) };
        assert_eq!(call.proceed(parameters(9)), [to_all(offer)], "the offer to every device of the callee's");
        assert!(call.proceed(parameters(9)).is_empty(), "once");
        assert_eq!(call.take_signal(1, answer(1)), [Effect::Open(1, parameters(1))]);
        assert_eq!(call.take_signal(3, answer(3)), [Effect::Open(3, parameters(3))]);
        call
    }

    // A call that came in from device 1 of the caller's, answered.
    fn answered() -> Call {
        let mut call = Call::incoming(CALL, (ALICE, 1), OWN, (true, parameters(1)), NOW);
        let early = vec![Candidate::Added("candidate:early".into())];
        let candidates = Signal::Candidates { call: CALL, candidates: early.clone() };
        assert!(call.take_signal(1, candidates).is_empty(), "candidates wait for the way");
        let answer = Effect::Signal(Outbound { target: Target::Contact(ALICE), device: Some(1), signal: answer(9) });
        assert_eq!(
            call.proceed(parameters(9)),
            [Effect::Open(1, parameters(1)), answer, Effect::Candidates(1, early)],
            "the caller's way opened, the answer to its device alone, then what came for the way meanwhile"
        );
        call
    }

    #[test]
    fn caller_rings_and_talks() {
        let mut call = offered();
        assert_eq!(call.phase(), Some(Phase::Calling));
        assert!(call.take_signal(1, answer(5)).is_empty(), "a device's second answer changes nothing");
        assert_eq!(call.note_way(3, Way::Up, NOW), [Effect::Log(Course::Ringing)], "a way stands: it rings there");
        assert_eq!(call.phase(), Some(Phase::Ringing));
        assert!(call.note_way(1, Way::Up, NOW).is_empty(), "told once");
        let accepted = Message { accepted: Some(CALL), seqnum: Some(1), ..Message::default() };
        let effects = call.take_word(1, &accepted, NOW);
        assert_eq!(
            told(&effects[0]),
            (3, Message { hangup: Some((CALL, Hangup::Accepted, Some(1))), seqnum: Some(1), ..Message::default() }),
            "the device that did not take it is told over its way which did"
        );
        assert_eq!(effects[1], to_all(hangup(Hangup::Accepted, Some(1))), "and all of them through the service");
        assert_eq!(effects[2], Effect::Close(3));
        let sending = Sending { is_video_on: Some(false), is_sharing_screen: None, is_audio_on: Some(true) };
        assert_eq!(
            told(&effects[3]),
            (1, Message { sending: Some((CALL, sending)), seqnum: Some(1), ..Message::default() }),
            "the device the call is with is told what this side sends, and never that the call is taken"
        );
        assert_eq!(effects[4..], [Effect::Media(1), Effect::Log(Course::Accepted)]);
        assert_eq!(call.phase(), Some(Phase::Talking));
        assert!(call.take_word(1, &accepted, NOW).is_empty(), "the word told again each second changes nothing");
        assert!(call.take_signal(3, answer(3)).is_empty(), "an answer after the call is taken opens no way");
    }

    #[test]
    fn caller_taken_before_way_stands() {
        let mut call = offered();
        let accepted = Message { accepted: Some(CALL), ..Message::default() };
        let effects = call.take_word(3, &accepted, NOW);
        assert_eq!(effects.len(), 3, "the other device told over its way and through the service, and let go of");
        assert_eq!((effects[2].clone(), call.phase()), (Effect::Close(1), Some(Phase::Connecting)));
        let effects = call.note_way(3, Way::Up, NOW);
        assert_eq!(effects[0], Effect::Log(Course::Ringing), "the log is told that it rang, though it never did");
        assert_eq!(effects[2..], [Effect::Media(3), Effect::Log(Course::Accepted)]);
        let other = Message { accepted: Some(CALL + 1), ..Message::default() };
        assert!(offered().take_word(3, &other, NOW).is_empty(), "another call's word");
    }

    #[test]
    fn callee_rings_accepts_retells() {
        let mut call = answered();
        assert_eq!(call.phase(), None, "nothing rings before the way stands");
        assert!(call.accept(NOW).is_empty(), "nor can it be taken");
        let status = |video, seqnum| Message {
            sending: Some((CALL, Sending { is_video_on: Some(video), ..Sending::default() })),
            max_bitrate: Some((CALL, 500_000)),
            seqnum: Some(seqnum),
            ..Message::default()
        };
        assert!(call.take_word(1, &status(true, 4), NOW).is_empty(), "what the caller sends waits for the accept");
        assert_eq!(call.note_way(1, Way::Up, NOW), [Effect::Log(Course::Ringing)]);
        assert_eq!(call.phase(), Some(Phase::Incoming));
        assert!(
            call.take_word(1, &Message { accepted: Some(CALL), ..Message::default() }, NOW).is_empty(),
            "a caller's accept is none"
        );
        call.set_sending(Sending { is_video_on: Some(true), is_sharing_screen: None, is_audio_on: Some(false) });
        let later = NOW + Duration::from_secs(5);
        let effects = call.accept(later);
        let sent = Sending { is_video_on: Some(true), is_sharing_screen: None, is_audio_on: Some(false) };
        assert_eq!(
            effects[..2],
            [Effect::Limit(500_000), Effect::Far(Sending { is_video_on: Some(true), ..Sending::default() })],
            "what the caller told while it rang counts now"
        );
        let whole =
            Message { accepted: Some(CALL), sending: Some((CALL, sent)), seqnum: Some(1), ..Message::default() };
        assert_eq!(told(&effects[2]), (1, whole), "taken, and what this side sends, in one word");
        assert_eq!(effects[3..], [Effect::Media(1), Effect::Log(Course::Accepted)]);
        assert_eq!(call.next_wait(later), Some(RETELL_EVERY));
        assert!(call.tick(later).is_empty());
        let again = call.tick(later + RETELL_EVERY);
        assert_eq!(again.len(), 1);
        assert_eq!(told(&again[0]), (1, whole), "told again each second as it stands, its number unchanged");
        assert!(call.take_word(1, &status(false, 4), later).is_empty(), "a word no newer than the last taken");
        assert_eq!(
            call.take_word(1, &status(false, 5), later),
            [Effect::Far(Sending { is_video_on: Some(false), ..Sending::default() })],
            "a newer one counts at once, and the limit that did not change is not told again"
        );
        let muted = Sending { is_audio_on: Some(true), ..sent };
        let effects = call.set_sending(muted);
        assert_eq!(
            told(&effects[0]).1,
            Message { sending: Some((CALL, muted)), seqnum: Some(2), ..whole },
            "a change of what this side sends is told at once, one number on"
        );
    }

    #[test]
    fn reconnects_and_fails() {
        let mut call = answered();
        call.note_way(1, Way::Up, NOW);
        assert!(call.note_way(1, Way::Down, NOW).is_empty() && call.phase() == Some(Phase::Incoming));
        call.accept(NOW);
        assert!(call.note_way(1, Way::Up, NOW).is_empty(), "a way taken while it was down stands again");
        assert_eq!(call.note_way(1, Way::Down, NOW), [Effect::Regather]);
        assert_eq!(call.phase(), Some(Phase::Reconnecting));
        assert!(call.note_way(1, Way::Up, NOW).is_empty() && call.phase() == Some(Phase::Talking));
        let effects = call.note_way(1, Way::Lost, NOW);
        assert_eq!(told(&effects[0]).1.hangup, Some((CALL, Hangup::Normal, None)));
        assert_eq!(
            effects[1..],
            [to_all(hangup(Hangup::Normal, None)), Effect::Close(1), Effect::Log(Course::Missed)],
            "a taken call that fails says so"
        );
        assert_eq!(call.phase(), Some(Phase::Ended(Ending::Failed(Cause::NoNetwork))));
        let mut ringing = answered();
        assert_eq!(
            ringing.note_way(1, Way::Lost, NOW),
            [Effect::Close(1), Effect::Log(Course::Missed)],
            "one not taken ends without a word, so that the owner's other devices ring on"
        );
        let mut caller = offered();
        assert_eq!(caller.note_way(1, Way::Lost, NOW), [Effect::Close(1)], "one way of several is let go of");
        let effects = caller.note_way(3, Way::Lost, NOW);
        assert_eq!(effects.len(), 4, "the last one ends the call, with a hang-up over it and through the service");
        assert_eq!(effects[1], to_all(hangup(Hangup::Normal, None)));
    }

    #[test]
    fn hangups_by_kind_and_sender() {
        let mut call = offered();
        let effects = call.take_signal(3, hangup(Hangup::Normal, None));
        assert_eq!(told(&effects[0]).0, 1, "a device declines: the other is told over its way");
        assert_eq!(told(&effects[0]).1.hangup, Some((CALL, Hangup::Declined, Some(3))));
        assert_eq!(
            effects[1..],
            [
                to_all(hangup(Hangup::Declined, Some(3))),
                Effect::Close(1),
                Effect::Close(3),
                Effect::Log(Course::RemoteHangup)
            ]
        );
        assert_eq!(call.ending(), Some(Ending::Declined));
        assert!(call.take_signal(1, hangup(Hangup::Normal, None)).is_empty(), "over once");
        let mut call = offered();
        let effects = call.take_signal(1, Signal::Busy { call: CALL });
        assert_eq!(effects[1], to_all(hangup(Hangup::Busy, Some(1))));
        assert_eq!((effects.last(), call.ending()), (Some(&Effect::Log(Course::Missed)), Some(Ending::Busy)));
        let mut call = offered();
        let effects = call.take_signal(1, hangup(Hangup::NeedPermission, Some(OWN)));
        assert_eq!(effects[1], to_all(hangup(Hangup::NeedPermission, Some(1))));
        assert_eq!(call.ending(), Some(Ending::Unpermitted));
        let mut call = offered();
        call.note_way(1, Way::Up, NOW);
        call.take_word(1, &Message { accepted: Some(CALL), ..Message::default() }, NOW);
        assert!(call.take_signal(3, hangup(Hangup::Normal, None)).is_empty(), "from a device it is not with");
        assert!(call.take_signal(3, Signal::Busy { call: CALL }).is_empty(), "nor a late busy");
        let word = Message { hangup: Some((CALL, Hangup::Normal, None)), ..Message::default() };
        assert_eq!(
            call.take_word(1, &word, NOW),
            [Effect::Close(1), Effect::Log(Course::RemoteHangup)],
            "the far side ends a taken call over its way: nothing is passed on"
        );
        assert_eq!(call.ending(), Some(Ending::FarHungUp));
        for (kind, named, ending, course) in [
            (Hangup::Accepted, Some(4), Some(Ending::Elsewhere), Some(Course::Accepted)),
            (Hangup::Declined, Some(4), Some(Ending::Elsewhere), Some(Course::Missed)),
            (Hangup::Busy, Some(4), Some(Ending::Elsewhere), Some(Course::Missed)),
            (Hangup::Normal, None, Some(Ending::FarHungUp), Some(Course::RemoteHangup)),
            (Hangup::Accepted, Some(OWN), None, None),
        ] {
            let mut call = answered();
            let effects = call.take_signal(1, hangup(kind, named));
            let logged = effects.last().map(|effect| match effect {
                Effect::Log(course) => *course,
                other => panic!("the log's word last, not {other:?}"),
            });
            assert_eq!((call.ending(), logged), (ending, course), "{kind:?} naming {named:?}");
        }
        assert!(answered().take_signal(5, hangup(Hangup::Normal, None)).is_empty(), "another device's");
        let other = Signal::Hangup { call: 1, kind: Hangup::Normal, device: None };
        assert!(answered().take_signal(1, other).is_empty(), "another call's");
    }

    #[test]
    fn owner_ends_and_time_runs_out() {
        let mut unoffered = Call::outgoing(CALL, ALICE, true, NOW);
        assert_eq!(unoffered.end(), [Effect::Log(Course::Hangup)], "nothing goes where the far side never heard");
        assert!(unoffered.proceed(parameters(9)).is_empty(), "nor an offer after the end");
        let mut call = answered();
        call.note_way(1, Way::Up, NOW);
        let effects = call.end();
        assert_eq!(told(&effects[0]).1.hangup, Some((CALL, Hangup::Normal, None)));
        assert_eq!(effects[1..], [to_all(hangup(Hangup::Normal, None)), Effect::Close(1), Effect::Log(Course::Hangup)]);
        assert_eq!((call.phase(), call.next_wait(NOW)), (Some(Phase::Ended(Ending::HungUp)), None));
        let mut call = offered();
        assert_eq!(call.next_wait(NOW + Duration::from_secs(20)), Some(Duration::from_secs(40)));
        assert!(call.tick(NOW + SETUP_TIME - Duration::from_millis(1)).is_empty());
        let effects = call.tick(NOW + SETUP_TIME);
        assert_eq!(effects.len(), 6, "a hang-up over both ways and through the service, both let go of, the log told");
        assert_eq!((effects[5].clone(), call.ending()), (Effect::Log(Course::Missed), Some(Ending::Unanswered)));
        let mut call = offered();
        call.take_word(1, &Message { accepted: Some(CALL), ..Message::default() }, NOW);
        call.tick(NOW + SETUP_TIME);
        assert_eq!(call.ending(), Some(Ending::Failed(Cause::NoNetwork)), "taken there, the way never stood");
    }

    #[test]
    fn unsent_signals() {
        let candidates = Signal::Candidates { call: CALL, candidates: Vec::new() };
        let mut call = offered();
        assert!(call.note_unsent(&Signal::Busy { call: 1 }).is_empty(), "another call's");
        assert!(call.note_unsent(&hangup(Hangup::Accepted, Some(1))).is_empty(), "a hang-up ends nothing more");
        call.note_way(1, Way::Up, NOW);
        assert!(call.note_unsent(&candidates).is_empty(), "candidates lost once a way stands");
        let mut call = offered();
        let effects = call.note_unsent(&candidates);
        assert_eq!(effects[2], to_all(hangup(Hangup::Normal, None)), "before that the call ends, and says so");
        assert_eq!(call.ending(), Some(Ending::Failed(Cause::NoNetwork)));
        let mut call = answered();
        assert_eq!(
            call.note_unsent(&answer(9)),
            [Effect::Close(1), Effect::Log(Course::Missed)],
            "a call that came in and was not taken ends without a word"
        );
    }

    #[test]
    fn candidates_by_side() {
        let found = vec![Candidate::Added("candidate:1".into())];
        let signal = Signal::Candidates { call: CALL, candidates: found.clone() };
        let mut caller = offered();
        assert_eq!(caller.tell_candidates(found.clone()), [to_all(signal.clone())], "a caller's to every device");
        assert_eq!(caller.take_signal(3, signal.clone()), [Effect::Candidates(3, found.clone())]);
        assert!(caller.take_signal(4, signal.clone()).is_empty(), "of a device that has not answered");
        let callee = answered();
        let to_caller = Effect::Signal(Outbound { target: Target::Contact(ALICE), device: Some(1), signal });
        assert_eq!(callee.tell_candidates(found), [to_caller], "a callee's to the caller's device");
        assert!(callee.tell_candidates(Vec::new()).is_empty());
    }

    #[test]
    fn collisions() {
        let mut call = offered();
        assert_eq!(
            [call.collide(1, CALL - 1), call.collide(1, CALL + 1), call.collide(1, CALL)],
            [Collision::Kept, Collision::Replaced { with_hangup: true }, Collision::Both],
            "glare: the call of the higher id stays"
        );
        call.note_way(1, Way::Up, NOW);
        call.take_word(1, &Message { accepted: Some(CALL), ..Message::default() }, NOW);
        assert_eq!(call.collide(3, CALL + 1), Collision::Busy, "from another device than the call is with");
        assert_eq!(call.collide(1, CALL - 1), Collision::Replaced { with_hangup: false }, "the far side began anew");
        let ringing = answered();
        assert_eq!(ringing.collide(1, CALL + 1), Collision::Replaced { with_hangup: true }, "not taken: glare");
        let mut call = offered();
        let effects = call.give_way(true);
        assert_eq!((effects.len(), effects.last()), (6, Some(&Effect::Log(Course::Missed))));
    }
}

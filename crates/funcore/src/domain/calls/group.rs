use super::direct::Way;
use super::{Ending, Member, Phase};
use crate::codec::http::{self, NOT_FOUND, SERVER_ERRORS};
use crate::domain::cause::Cause;
use crate::net::fetch::MAX_HOPS;
use crate::net::rtp::srtp::Master;
use crate::net::stun::Transport;
use crate::signal::Crypto;
use crate::signal::address::{Peer, ServiceId};
use crate::signal::call::frame;
use crate::signal::call::sfu::{self, Heartbeat, Inbox, Joined, Relayed};
use crate::signal::call::{ACCEPTED, DECLINED, GroupSignal, RINGING, SECRET_BYTES, Signal};
use crate::signal::command::{Outbound, Target};
use crate::signal::curve::Pair;
use crate::signal::device::{Course, Membership, call_of_era};
use crate::signal::service::Request;
use alloc::collections::{BTreeSet, VecDeque};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::net::SocketAddr;
use core::time::Duration;

// RingRTC's `tick`: the devices in the call are asked anew once the last answer is this old (`group_call.rs`, `request_remote_devices_from_sfu_if_older_than`).
const PEEK_AGE: Duration = Duration::from_secs(10);
// A peek unanswered this long is asked again, and after one that failed the next waits this long (`maybe_request_remote_devices`).
const PEEK_TIMEOUT: Duration = Duration::from_secs(5);
const PEEK_RETRY: Duration = Duration::from_secs(5);
// A device's leave has the devices asked at once and again this much later, for a server that had not heard of it yet (`handle_leaving_received`).
const LEAVING_PEEK: Duration = Duration::from_secs(2);
// A new secret sent after a member left is taken into use this much later (`MEDIA_SEND_KEY_ROTATION_DELAY_SECS`).
const ROTATION_DELAY: Duration = Duration::from_secs(5);
// The heartbeat and the video request go this often (`HEARTBEAT_INTERVAL`).
const BEAT: Duration = Duration::from_secs(1);
// The token and the members are asked anew this often (`MEMBERSHIP_PROOF_REQUEST_INTERVAL`).
const TOKEN_AGE: Duration = Duration::from_secs(24 * 60 * 60);
// How long a ring that came rings (`call_manager.rs`, `INCOMING_GROUP_CALL_RING_TIME`, which is `TIME_OUT_PERIOD`).
const RING_TIME: Duration = Duration::from_secs(60);
// The creator rings a group of fewer members than this (Signal Desktop's `isConversationTooBigToRing` under `global.calling.maxGroupCallRingSize`).
const RING_MEMBERS: usize = 16;
// The most this side sends, bits a second, with fewer devices beside it than `LARGE_CALL`, and with as many or more (`SMALL_CALL_MAX_SEND_RATE`, `LARGE_CALL_MAX_SEND_RATE`, `compute_send_rates`).
const SMALL_RATE: u64 = 1_000_000;
const LARGE_RATE: u64 = 671_000;
const LARGE_CALL: usize = 8;
// The most the server is asked to send this device, kbit/s: RingRTC's low data mode (`LOW_MAX_RECEIVE_RATE`), as the one-to-one call asks low data mode's of its far side.
const MAX_KBPS: u32 = 500;
// The height asked of the device shown, the lesser side of its picture as RingRTC asks it (`send_video_requests_to_sfu`): the largest a far picture shows at over the themes (`Ui::far_picture_room`), a square one in Rail's room of 382 by 430 above this side's.
const SHOWN_HEIGHT: u32 = 382;
// A height that asks nothing of a device (the calling server's `call.rs`, `requested_height_for`, and `simulcast.rs`).
const NO_VIDEO: u32 = 0;
// The leave goes to the server twice (`send_leave_to_sfu`).
const LEAVES: usize = 2;
// The calling server's answer to a join of a call that takes no more devices (`frontend/src/api.rs`).
const FULL: u16 = 413;
// The keys of devices not yet seen held at the most: the devices a call takes, each with as many secrets as a device keeps.
const HELD_KEYS: usize = sfu::CALL_DEVICES * frame::MOST_SECRETS;

type Secret = [u8; SECRET_BYTES];

// How far the call has come (RingRTC's `JoinState`, with a ring that came before it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Ringing,
    // The token and the members are asked, for the join.
    Asking,
    Joining,
    // Joined, and not yet let in.
    Pending,
    Joined,
}

// How the way to the server stands once the join's answer named it (RingRTC's `ConnectionState`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Connection {
    Connecting,
    Connected,
    Reconnecting,
}

// How the devices in the call were last asked of the server (RingRTC's `RemoteDevicesRequestState`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Peeks {
    Never,
    // On its way since then; `is_again` where another was wanted meanwhile.
    Asked { at: Duration, is_again: bool },
    Answered(Duration),
    Failed(Duration),
}

// What a step of a group call's course asks of what carries the call.
#[derive(PartialEq, Eq)]
pub(super) enum Effect {
    // Asks Signal's thread for the group's token and members (`Command::Token`), which `Call::take_membership` takes.
    Membership,
    // The join to fetch: its answer goes to `Call::take_joined`, a redirect to `Call::follow`.
    Join(Request<'static>),
    // A peek to fetch: its answer goes to `Call::take_peek`, a redirect to `Call::follow`.
    Peek(Request<'static>),
    // A signal for Signal's thread (`Command::Signal`).
    Signal(Outbound),
    // The server as the way's one far side: this device's demux id, which names its streams, how many members the group has, the server's ICE credentials, and the SRTP keys, the one this device sends under first.
    Open { demux_id: u32, members: usize, ufrag: String, pwd: String, keys: (Master, Master) },
    // An address of the server's, under TLS checked by the name.
    Server { address: SocketAddr, transport: Transport, name: Option<String> },
    // The payload of a data packet, for the server itself or for the other devices through it (RingRTC's `unreliable_send_data_inner`).
    Data { is_to_server: bool, payload: Vec<u8> },
    // The ways are sought anew on every network.
    Regather,
    // Sound and pictures flow from now on, sent at most at that many bits a second; none while no other device is in the call.
    Media(Option<u64>),
    // Another device muted this one: the microphone is heard no more.
    Muted,
    // The call log's word of the call, by the call's id, whether the owner began it and who began it.
    Log { call: u64, is_outgoing: bool, ringer: Option<ServiceId>, course: Course },
}

impl core::fmt::Debug for Effect {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Effect::Membership => f.write_str("Membership"),
            Effect::Join(request) => f.debug_tuple("Join").field(request).finish(),
            Effect::Peek(request) => f.debug_tuple("Peek").field(request).finish(),
            Effect::Signal(outbound) => f.debug_tuple("Signal").field(outbound).finish(),
            Effect::Open { .. } => f.write_str("Open"),
            Effect::Server { transport, .. } => {
                f.debug_struct("Server").field("transport", transport).finish_non_exhaustive()
            }
            Effect::Data { is_to_server, payload } => {
                f.debug_struct("Data").field("to_server", is_to_server).field("bytes", &payload.len()).finish()
            }
            Effect::Regather => f.write_str("Regather"),
            Effect::Media(rate) => f.debug_tuple("Media").field(rate).finish(),
            Effect::Muted => f.write_str("Muted"),
            Effect::Log { is_outgoing, course, .. } => {
                f.debug_struct("Log").field("outgoing", is_outgoing).field("course", course).finish_non_exhaustive()
            }
        }
    }
}

// A device in the call beside this one, as the last peek tells it: its owner, what opens its frames, its latest heartbeat with the RTP time it came at, and whether it told it leaves; it has no `Debug`, as it holds keys.
struct Device {
    demux_id: u32,
    user: ServiceId,
    frames: frame::Inbound,
    heartbeat: Heartbeat,
    heard: Option<u32>,
    is_leaving: bool,
}

impl Device {
    fn new(demux_id: u32, user: ServiceId) -> Device {
        Device {
            demux_id,
            user,
            frames: frame::Inbound::default(),
            heartbeat: Heartbeat::default(),
            heard: None,
            is_leaving: false,
        }
    }
}

// What the join's answer gave: this device's demux id and the call's id by its era.
#[derive(Clone, Copy)]
struct Seat {
    demux_id: u32,
    call: u64,
}

// A ring of the group's call that came: its id, which is the call's, and who rang.
#[derive(Clone, Copy)]
struct Ring {
    id: i64,
    ringer: ServiceId,
}

// The request to the server last made, and the redirects it followed.
struct Asked {
    is_join: bool,
    request: Request<'static>,
    hops: usize,
}

// A new secret sent and not yet taken into use, when it is, and whether another member left meanwhile.
struct Rotation {
    at: Duration,
    secret: Secret,
    is_owed: bool,
}

// A Signal group's call from the owner's order or a ring's arrival to its end, without what carries it: RingRTC's group call `Client`; it has no `Debug`, as it holds keys.
pub(super) struct Call {
    group: Peer,
    is_outgoing: bool,
    is_video_on: bool,
    is_muted: bool,
    // The device that muted this one, which its heartbeat names.
    muted_by: Option<u32>,
    stage: Stage,
    connection: Option<Connection>,
    ending: Option<Ending>,
    ring: Option<Ring>,
    began: Duration,
    // The ring this side sent while alone in the call, which its leave cancels.
    rang: Option<i64>,
    // This side's ICE credentials, the join's.
    ice: Option<(String, String)>,
    membership: Option<Membership>,
    token_at: Option<Duration>,
    pair: Option<Pair>,
    seat: Option<Seat>,
    frames: Option<frame::Outbound>,
    rotation: Option<Rotation>,
    devices: Vec<Device>,
    // The users of every device the last peek told, the owner's among them.
    users: BTreeSet<ServiceId>,
    // Keys of devices no peek told yet, oldest first, each with its sender.
    held: VecDeque<(ServiceId, u32, u8, Secret)>,
    peeks: Peeks,
    // When the devices are asked again after a leave.
    peeks_at: Vec<Duration>,
    asked: Option<Asked>,
    inbox: Inbox,
    // The device of the server's latest speaker beside this one.
    speaker: Option<u32>,
    beat_at: Option<Duration>,
    // The heights of the last video request, and whether one went since the last beat besides the beat's.
    requested: Vec<(u32, u32)>,
    is_requested: bool,
    rate: Option<u64>,
}

// Draws a secret; none where the library gives no random bytes.
fn draw_secret(crypto: &impl Crypto) -> Option<Secret> {
    let mut secret = [0u8; SECRET_BYTES];
    crypto.random(&mut secret).then_some(secret)
}

impl Call {
    fn new(group: Peer, stage: Stage, (is_outgoing, is_video_on): (bool, bool), now: Duration) -> Call {
        Call {
            group,
            is_outgoing,
            is_video_on,
            is_muted: false,
            muted_by: None,
            stage,
            connection: None,
            ending: None,
            ring: None,
            began: now,
            rang: None,
            ice: None,
            membership: None,
            token_at: None,
            pair: None,
            seat: None,
            frames: None,
            rotation: None,
            devices: Vec::new(),
            users: BTreeSet::new(),
            held: VecDeque::new(),
            peeks: Peeks::Never,
            peeks_at: Vec::new(),
            asked: None,
            inbox: Inbox::default(),
            speaker: None,
            beat_at: None,
            requested: Vec::new(),
            is_requested: false,
            rate: None,
        }
    }

    // A call the owner begins in a group at `now`, with this side's picture where `is_video`, joined under the ICE agent's credentials: the token and the members are asked first.
    pub(super) fn outgoing(
        group: Peer,
        is_video: bool,
        (ufrag, pwd): (&str, &str),
        now: Duration,
    ) -> (Call, Vec<Effect>) {
        let mut call = Call::new(group, Stage::Asking, (true, is_video), now);
        call.ice = Some((String::from(ufrag), String::from(pwd)));
        let effects = call.ask(now);
        (call, effects)
    }

    // A ring of a group's call that came from `ringer` at `now`, which the log is told.
    pub(super) fn ringing(group: Peer, (ring, ringer): (i64, ServiceId), now: Duration) -> (Call, Vec<Effect>) {
        let mut call = Call::new(group, Stage::Ringing, (false, false), now);
        call.ring = Some(Ring { id: ring, ringer });
        let effects = call.ring_log(Course::Ringing).into_iter().collect();
        (call, effects)
    }

    pub(super) const fn group(&self) -> Peer {
        self.group
    }

    pub(super) const fn is_outgoing(&self) -> bool {
        self.is_outgoing
    }

    pub(super) const fn ending(&self) -> Option<Ending> {
        self.ending
    }

    const fn is_seated(&self) -> bool {
        matches!(self.stage, Stage::Pending | Stage::Joined)
    }

    // True while the way to the server stands or is sought anew: what is sent goes over it.
    const fn is_carried(&self) -> bool {
        matches!(self.connection, Some(Connection::Connected | Connection::Reconnecting))
    }

    fn is_known(&self, demux_id: u32) -> bool {
        self.devices.iter().any(|device| device.demux_id == demux_id)
    }

    // How the call stands for its screen: it rings here, is joined, rings in the group while this side is alone in it after its ring, or is talked in.
    pub(super) fn phase(&self) -> Phase {
        if let Some(ending) = self.ending {
            return Phase::Ended(ending);
        }
        match (self.stage, self.connection) {
            (Stage::Ringing, _) => Phase::Incoming,
            (Stage::Joined, Some(Connection::Connected)) if self.rang.is_some() && self.devices.is_empty() => {
                Phase::Ringing
            }
            (Stage::Joined, Some(Connection::Connected)) => Phase::Talking,
            (Stage::Joined, Some(Connection::Reconnecting)) => Phase::Reconnecting,
            _ => Phase::Connecting,
        }
    }

    // The devices beside this one for the call's screen, each by its owner's name as the members give it.
    pub(super) fn members(&self) -> Vec<Member> {
        let members = self.membership.as_ref().map_or(&[][..], |membership| &membership.members[..]);
        let name = |user: ServiceId| members.iter().find(|member| member.service == user)?.name.clone();
        self.devices
            .iter()
            .map(|device| Member {
                name: name(device.user),
                is_speaking: self.speaker == Some(device.demux_id),
                is_muted: device.heartbeat.is_audio_muted == Some(true),
                has_video: device.heartbeat.is_video_muted == Some(false),
            })
            .collect()
    }

    // The device whose picture shows: the latest to speak, else the first in the call.
    pub(super) fn shown(&self) -> Option<u32> {
        let speaker = self.speaker.filter(|speaker| self.is_known(*speaker));
        speaker.or_else(|| self.devices.first().map(|device| device.demux_id))
    }

    // Seals the payload of a frame this side sends, sound or picture; none before the join or where the library fails.
    pub(super) fn seal(&mut self, crypto: &impl Crypto, frame: &[u8]) -> Option<Vec<u8>> {
        self.frames.as_mut()?.seal(crypto, frame)
    }

    // Opens the payload of a frame of the device of that demux id; none for a device no peek told, or a frame no key of its opens.
    pub(super) fn open(&mut self, crypto: &impl Crypto, demux_id: u32, sealed: &[u8]) -> Option<Vec<u8>> {
        self.devices.iter_mut().find(|device| device.demux_id == demux_id)?.frames.open(crypto, sealed)
    }

    // Asks for the token and the members, and again a day later.
    fn ask(&mut self, now: Duration) -> Vec<Effect> {
        self.token_at = Some(now + TOKEN_AGE);
        vec![Effect::Membership]
    }

    fn ring_log(&self, course: Course) -> Option<Effect> {
        let ring = self.ring?;
        Some(Effect::Log { call: ring.id as u64, is_outgoing: false, ringer: Some(ring.ringer), course })
    }

    // The answer to the ring that came, to the account's other devices (`accept_ring_if_needed`, `cancel_group_ring`).
    fn respond(&self, kind: u64) -> Vec<Effect> {
        let Some(ring) = self.ring else { return Vec::new() };
        let signal = GroupSignal::Answered { group: self.group, ring: ring.id, kind };
        vec![Effect::Signal(Outbound {
            target: Target::Own,
            device: None,
            signal: Signal::Group { signal, is_urgent: true },
        })]
    }

    // One signal to these users: to the group's members named where more than one of the others takes it, else to the one, and to the account's own devices where the owner is among them (`send_media_send_key_to_users_over_signaling`).
    fn to_users(&self, users: &BTreeSet<ServiceId>, signal: &GroupSignal) -> Vec<Effect> {
        let own = self.membership.as_ref().map(|membership| membership.own);
        let send = |target| {
            let signal = Signal::Group { signal: signal.clone(), is_urgent: false };
            Effect::Signal(Outbound { target, device: None, signal })
        };
        let others: Vec<ServiceId> = users.iter().copied().filter(|user| Some(*user) != own).collect();
        let mut effects = match others[..] {
            [] => Vec::new(),
            [one] => vec![send(Target::Contact(one))],
            _ => vec![send(Target::Members(self.group, others))],
        };
        if own.is_some_and(|own| users.contains(&own)) {
            effects.push(send(Target::Own));
        }
        effects
    }

    // This side's media key to these users, under its demux id.
    fn send_key(&self, users: &BTreeSet<ServiceId>, ratchet: u8, secret: Secret) -> Vec<Effect> {
        let Some(seat) = self.seat else { return Vec::new() };
        let signal =
            GroupSignal::MediaKey { group: self.group, demux_id: seat.demux_id, ratchet: u32::from(ratchet), secret };
        self.to_users(users, &signal)
    }

    // What this device tells the others through the server, sealed as its frames are, while joined and the way carries it (`broadcast_data_through_sfu`).
    fn relay(&mut self, crypto: &impl Crypto, relayed: Relayed) -> Option<Effect> {
        if self.stage != Stage::Joined || !self.is_carried() {
            return None;
        }
        let payload = self.frames.as_mut()?.seal(crypto, &relayed.write())?;
        Some(Effect::Data { is_to_server: false, payload })
    }

    fn heartbeat(&mut self, crypto: &impl Crypto) -> Option<Effect> {
        let heartbeat = Heartbeat {
            is_audio_muted: Some(self.is_muted),
            is_video_muted: Some(!self.is_video_on),
            is_presenting: None,
            is_sharing_screen: None,
            muted_by: self.muted_by,
        };
        self.relay(crypto, Relayed::Heartbeat(heartbeat))
    }

    // The height asked of each device: the one shown at the height it shows at, the rest none.
    fn requests(&self) -> Vec<(u32, u32)> {
        let shown = self.shown();
        let height = |demux_id| if Some(demux_id) == shown { SHOWN_HEIGHT } else { NO_VIDEO };
        self.devices.iter().map(|device| (device.demux_id, height(device.demux_id))).collect()
    }

    fn video_request(&mut self) -> Effect {
        self.requested = self.requests();
        let payload = sfu::write_video_request(&self.requested, MAX_KBPS, SHOWN_HEIGHT);
        Effect::Data { is_to_server: true, payload }
    }

    // The heartbeat while joined and the video request while let in or waiting to be, each over the way to the server.
    fn beat(&mut self, crypto: &impl Crypto) -> Vec<Effect> {
        if !self.is_seated() || !self.is_carried() {
            return Vec::new();
        }
        let mut effects: Vec<Effect> = self.heartbeat(crypto).into_iter().collect();
        effects.push(self.video_request());
        effects
    }

    // A video request at once where the heights asked changed, once between beats (`request_video`).
    fn rerequest(&mut self) -> Option<Effect> {
        let is_due = !self.is_requested && self.is_seated() && self.is_carried() && self.requests() != self.requested;
        is_due.then(|| {
            self.is_requested = true;
            self.video_request()
        })
    }

    // Whether sound and pictures flow and how fast, where that changed (`set_send_rates_inner`).
    fn flow(&mut self) -> Option<Effect> {
        let others = self.devices.len();
        let is_on = self.stage == Stage::Joined && self.is_carried() && others > 0;
        let rate = is_on.then_some(if others < LARGE_CALL { SMALL_RATE } else { LARGE_RATE });
        (rate != self.rate).then(|| {
            self.rate = rate;
            Effect::Media(rate)
        })
    }

    fn peek(&mut self, now: Duration) -> Vec<Effect> {
        let Some(membership) = &self.membership else { return Vec::new() };
        let request = sfu::peek_request(&sfu::participants_url(), &membership.token);
        self.peeks = Peeks::Asked { at: now, is_again: false };
        self.asked = Some(Asked { is_join: false, request: request.clone(), hops: 0 });
        vec![Effect::Peek(request)]
    }

    // The devices asked of the server as soon as may be: at once, or after the peek on its way, but not sooner after a failure than its wait (`request_remote_devices_as_soon_as_possible`).
    fn peek_soon(&mut self, now: Duration) -> Vec<Effect> {
        if !self.is_seated() {
            return Vec::new();
        }
        match self.peeks {
            Peeks::Asked { at, .. } if now < at + PEEK_TIMEOUT => {
                self.peeks = Peeks::Asked { at, is_again: true };
                Vec::new()
            }
            Peeks::Failed(at) if now < at + PEEK_RETRY => Vec::new(),
            _ => self.peek(now),
        }
    }

    // When the devices are next asked of the server by the clock.
    fn peek_due(&self) -> Option<Duration> {
        if !self.is_seated() || self.membership.is_none() {
            return None;
        }
        Some(match self.peeks {
            Peeks::Never => Duration::ZERO,
            Peeks::Asked { at, .. } => at + PEEK_TIMEOUT,
            Peeks::Answered(at) => at + PEEK_AGE,
            Peeks::Failed(at) => at + PEEK_RETRY,
        })
    }

    // Ends the call (`leave_inner`): a ring sent while alone cancelled, and where joined or waiting to be, the leave told through the server, over Signal to the users of the last peek and to the server twice.
    fn finish(&mut self, crypto: &impl Crypto, ending: Ending) -> Vec<Effect> {
        if self.ending.is_some() {
            return Vec::new();
        }
        let mut effects = Vec::new();
        if let Some(ring) = self.rang.take() {
            let signal = GroupSignal::Ring { group: self.group, ring, is_cancelled: true };
            let signal = Signal::Group { signal, is_urgent: true };
            effects.push(Effect::Signal(Outbound { target: Target::Group(self.group), device: None, signal }));
        }
        if let Some(seat) = self.seat.filter(|_| self.is_seated()) {
            effects.extend(self.relay(crypto, Relayed::Leaving));
            effects.extend(
                self.to_users(&self.users, &GroupSignal::Leaving { group: self.group, demux_id: seat.demux_id }),
            );
            if self.is_carried() {
                let leave = || Effect::Data { is_to_server: true, payload: sfu::write_leave() };
                effects.extend((0..LEAVES).map(|_| leave()));
            }
        }
        self.ending = Some(ending);
        effects
    }

    // Ends the call for a cause, as its leave ends it.
    pub(super) fn fail(&mut self, crypto: &impl Crypto, cause: Cause) -> Vec<Effect> {
        self.finish(crypto, Ending::Failed(cause))
    }

    // Whether the device whose picture shows sends one, as its heartbeat tells.
    pub(super) fn has_shown_video(&self) -> bool {
        let shown = self.shown();
        let sends = |device: &Device| Some(device.demux_id) == shown && device.heartbeat.is_video_muted == Some(false);
        self.devices.iter().any(sends)
    }

    // The owner takes the ring, without this side's picture, joined under the ICE agent's credentials: the account's other devices are told, and the token and the members asked.
    pub(super) fn accept(&mut self, (ufrag, pwd): (&str, &str), now: Duration) -> Vec<Effect> {
        if self.ending.is_some() || self.stage != Stage::Ringing {
            return Vec::new();
        }
        self.stage = Stage::Asking;
        self.ice = Some((String::from(ufrag), String::from(pwd)));
        let mut effects = self.respond(ACCEPTED);
        effects.extend(self.ring_log(Course::Accepted));
        effects.extend(self.ask(now));
        effects
    }

    // The owner sends the ring away, which the log keeps as missed as Signal Desktop comes to, or leaves the call (`leave`).
    pub(super) fn end(&mut self, crypto: &impl Crypto) -> Vec<Effect> {
        if self.ending.is_some() || self.stage != Stage::Ringing {
            return self.finish(crypto, Ending::HungUp);
        }
        self.ending = Some(Ending::HungUp);
        let mut effects = self.respond(DECLINED);
        effects.extend(self.ring_log(Course::Missed));
        effects
    }

    // The microphone heard no more, or again; the others are told at once (`set_outgoing_audio_muted`).
    pub(super) fn mute(&mut self, crypto: &impl Crypto, is_muted: bool) -> Vec<Effect> {
        if !is_muted || !self.is_muted {
            self.muted_by = None;
        }
        self.is_muted = is_muted;
        self.heartbeat(crypto).into_iter().collect()
    }

    // This side's picture sent, or no more; the others are told at once (`set_outgoing_video_muted`).
    pub(super) fn video(&mut self, crypto: &impl Crypto, is_on: bool) -> Vec<Effect> {
        self.is_video_on = is_on;
        self.heartbeat(crypto).into_iter().collect()
    }

    // The token and the members Signal's thread had: the join goes with the first; one that changes the members has the devices asked again. None fails a call that holds none.
    pub(super) fn take_membership(
        &mut self,
        crypto: &impl Crypto,
        membership: Option<Membership>,
        now: Duration,
    ) -> Vec<Effect> {
        if self.ending.is_some() || self.stage == Stage::Ringing {
            return Vec::new();
        }
        let Some(membership) = membership else {
            return if self.membership.is_none() { self.fail(crypto, Cause::NoNetwork) } else { Vec::new() };
        };
        let users = |membership: &Membership| -> BTreeSet<ServiceId> {
            membership.members.iter().map(|member| member.service).collect()
        };
        let is_changed = self.membership.as_ref().is_some_and(|held| users(held) != users(&membership));
        self.membership = Some(membership);
        match self.stage {
            Stage::Asking => self.join(crypto),
            _ if is_changed => self.peek_soon(now),
            _ => Vec::new(),
        }
    }

    // Joins with a key pair of the call's own and a first secret for its frames (`join`).
    fn join(&mut self, crypto: &impl Crypto) -> Vec<Effect> {
        let frames = draw_secret(crypto).and_then(|secret| frame::Outbound::new(crypto, secret));
        let made = Pair::new(crypto).zip(frames);
        let (Some((pair, frames)), Some((ufrag, pwd)), Some(membership)) = (made, &self.ice, &self.membership) else {
            return self.fail(crypto, Cause::SessionFailed);
        };
        let request = sfu::join_request(&sfu::participants_url(), &membership.token, ufrag, pwd, &pair.public);
        (self.pair, self.frames, self.stage) = (Some(pair), Some(frames), Stage::Joining);
        self.asked = Some(Asked { is_join: true, request: request.clone(), hops: 0 });
        vec![Effect::Join(request)]
    }

    // The join's answer, or the status of one refused, none where none came (`on_sfu_client_join_success`, `on_sfu_client_join_failure`): the server's side of the way, a peek at once, the creator's ring where the owner began the call, and the log's word.
    pub(super) fn take_joined(
        &mut self,
        crypto: &impl Crypto,
        answer: Result<Joined, Option<u16>>,
        now: Duration,
    ) -> Vec<Effect> {
        if self.ending.is_some() || self.stage != Stage::Joining {
            return Vec::new();
        }
        self.asked = None;
        let joined = match answer {
            Ok(joined) => joined,
            Err(Some(FULL)) => return self.finish(crypto, Ending::Full),
            Err(Some(status)) if SERVER_ERRORS.contains(&status) => return self.fail(crypto, Cause::NoAnswer),
            Err(Some(_)) => return self.fail(crypto, Cause::Refused),
            Err(None) => return self.fail(crypto, Cause::NoNetwork),
        };
        let keys = self.pair.as_ref().and_then(|pair| sfu::negotiate(crypto, pair, &joined.public_key));
        let (Some(keys), Some(membership)) = (keys, &self.membership) else {
            return self.fail(crypto, Cause::Refused);
        };
        let (own, members) = (membership.own, membership.members.len());
        let creator =
            membership.members.iter().find(|member| member.opaque == joined.creator).map(|member| member.service);
        let seat = Seat { demux_id: joined.demux_id, call: call_of_era(crypto, &joined.era) };
        self.stage = if joined.is_active { Stage::Joined } else { Stage::Pending };
        (self.seat, self.connection, self.beat_at) = (Some(seat), Some(Connection::Connecting), Some(now));
        let (demux_id, ufrag, pwd) = (joined.demux_id, joined.ufrag, joined.pwd);
        let mut effects = vec![Effect::Open { demux_id, members, ufrag, pwd, keys }];
        let tls = if joined.hostname.is_some() { joined.tls } else { Vec::new() };
        for (addresses, transport) in
            [(joined.udp, Transport::Udp), (joined.tcp, Transport::Tcp), (tls, Transport::Tls)]
        {
            let name = joined.hostname.clone().filter(|_| transport == Transport::Tls);
            effects.extend(addresses.into_iter().map(|address| Effect::Server {
                address,
                transport,
                name: name.clone(),
            }));
        }
        effects.extend(self.peek_soon(now));
        let is_own = creator == Some(own);
        if self.is_outgoing && is_own && members < RING_MEMBERS {
            let ring = seat.call as i64;
            let signal = Signal::Group {
                signal: GroupSignal::Ring { group: self.group, ring, is_cancelled: false },
                is_urgent: true,
            };
            effects.push(Effect::Signal(Outbound { target: Target::Group(self.group), device: None, signal }));
            self.rang = Some(ring);
        }
        if let Some(ringer) = creator {
            let course = if is_own { Course::Ringing } else { Course::Accepted };
            effects.push(Effect::Log { call: seat.call, is_outgoing: is_own, ringer: Some(ringer), course });
        }
        effects
    }

    // A peek's answer, or the status of one refused, none where none came; a call not found has no devices (`set_peek_result_inner`).
    pub(super) fn take_peek(
        &mut self,
        crypto: &impl Crypto,
        answer: Result<Vec<sfu::Member>, Option<u16>>,
        now: Duration,
    ) -> Vec<Effect> {
        if self.ending.is_some() || !self.is_seated() {
            return Vec::new();
        }
        self.asked = None;
        let devices = match answer {
            Ok(devices) => devices,
            Err(Some(NOT_FOUND)) => Vec::new(),
            Err(_) => {
                self.peeks = Peeks::Failed(now);
                return Vec::new();
            }
        };
        let is_again = matches!(self.peeks, Peeks::Asked { is_again: true, .. });
        self.peeks = Peeks::Answered(now);
        let mut effects = self.take_devices(crypto, &devices, now);
        if is_again && self.ending.is_none() {
            effects.extend(self.peek_soon(now));
        }
        effects
    }

    // The devices a peek told: each by its user as the members name it, this one let in where it is among them; the key stepped on for users of devices new to the call and a new one sent after a user left, and keys held for devices not yet seen taken.
    fn take_devices(&mut self, crypto: &impl Crypto, told: &[sfu::Member], now: Duration) -> Vec<Effect> {
        let (Some(membership), Some(seat)) = (&self.membership, self.seat) else { return Vec::new() };
        let user_of = |opaque: &str| {
            membership.members.iter().find(|member| member.opaque == opaque).map(|member| member.service)
        };
        let resolved: Vec<(u32, ServiceId)> =
            told.iter().filter_map(|device| Some((device.demux_id, user_of(device.user.as_deref()?)?))).collect();
        let users: BTreeSet<ServiceId> = resolved.iter().map(|(_, user)| *user).collect();
        let is_in = resolved.iter().any(|(demux_id, _)| *demux_id == seat.demux_id);
        let mut old = core::mem::take(&mut self.devices);
        let old_ids: BTreeSet<u32> = old.iter().map(|device| device.demux_id).collect();
        for (demux_id, user) in resolved {
            if demux_id == seat.demux_id || self.is_known(demux_id) {
                continue;
            }
            let kept = old.iter().position(|device| device.demux_id == demux_id && device.user == user);
            self.devices.push(kept.map_or_else(|| Device::new(demux_id, user), |at| old.swap_remove(at)));
        }
        if is_in && self.stage == Stage::Pending {
            self.stage = Stage::Joined;
        }
        let mut effects = Vec::new();
        let added: BTreeSet<ServiceId> = self
            .devices
            .iter()
            .filter(|device| !old_ids.contains(&device.demux_id))
            .map(|device| device.user)
            .collect();
        if !added.is_empty() {
            let is_advanced = self.frames.as_mut().is_some_and(|frames| frames.advance(crypto));
            let Some((ratchet, secret)) = self.frames.as_ref().filter(|_| is_advanced).map(frame::Outbound::state)
            else {
                return self.fail(crypto, Cause::SessionFailed);
            };
            effects.extend(self.send_key(&added, ratchet, secret));
            if let Some(pending) = self.rotation.as_ref().map(|rotation| rotation.secret) {
                effects.extend(self.send_key(&added, 0, pending));
            }
        }
        if self.users.iter().any(|user| !users.contains(user)) {
            effects.extend(self.rotate(crypto, now));
            if self.ending.is_some() {
                return effects;
            }
        }
        for (user, demux_id, ratchet, secret) in core::mem::take(&mut self.held) {
            effects.extend(self.take_key(crypto, (user, demux_id), (ratchet, secret), now));
        }
        if is_in {
            effects.extend(self.flow());
        }
        if !self.devices.is_empty() {
            self.rang = None;
        }
        self.users = users;
        effects.extend(self.rerequest());
        effects
    }

    // A new secret for this side's frames, sent at once to the users of the devices in the call and taken into use later; one asked while another waits follows it (`rotate_media_send_key_and_send_to_users_not_removed`).
    fn rotate(&mut self, crypto: &impl Crypto, now: Duration) -> Vec<Effect> {
        if let Some(rotation) = &mut self.rotation {
            rotation.is_owed = true;
            return Vec::new();
        }
        let Some(secret) = draw_secret(crypto) else { return self.fail(crypto, Cause::SessionFailed) };
        self.rotation = Some(Rotation { at: now + ROTATION_DELAY, secret, is_owed: false });
        let users: BTreeSet<ServiceId> = self.devices.iter().map(|device| device.user).collect();
        self.send_key(&users, 0, secret)
    }

    // A member's key for its device: taken where the last peek gives that device to it, held for a device not yet seen, which has the members asked anew once (`add_media_receive_key_or_store_for_later`).
    fn take_key(
        &mut self,
        crypto: &impl Crypto,
        (user, demux_id): (ServiceId, u32),
        (ratchet, secret): (u8, Secret),
        now: Duration,
    ) -> Vec<Effect> {
        if let Some(device) = self.devices.iter_mut().find(|device| device.demux_id == demux_id) {
            if device.user == user {
                device.frames.add(crypto, ratchet, secret);
            }
            return Vec::new();
        }
        let is_first = self.held.is_empty();
        if self.held.len() >= HELD_KEYS {
            self.held.pop_front();
        }
        self.held.push_back((user, demux_id, ratchet, secret));
        if is_first { self.ask(now) } else { Vec::new() }
    }

    // A device's leave, through the server or over Signal from the user the device is of: the devices are asked at once and again a little later (`handle_leaving_received`).
    fn take_leaving(&mut self, sender: Option<ServiceId>, demux_id: u32, now: Duration) -> Vec<Effect> {
        let Some(device) = self.devices.iter_mut().find(|device| device.demux_id == demux_id && !device.is_leaving)
        else {
            return Vec::new();
        };
        if sender.is_some_and(|sender| sender != device.user) {
            return Vec::new();
        }
        device.is_leaving = true;
        self.peeks_at.push(now + LEAVING_PEEK);
        self.peek_soon(now)
    }

    // A group signal a member sent, as Signal's gate passed it (`on_signaling_message_received`, `handle_received_call_message`): the ringer's cancel or another device's answer ends the ring here; a media key is taken or held, a leave taken, and either of a device not known has the devices asked.
    pub(super) fn take_signal(
        &mut self,
        crypto: &impl Crypto,
        sender: ServiceId,
        signal: &GroupSignal,
        now: Duration,
    ) -> Vec<Effect> {
        if self.ending.is_some() || signal.group() != self.group {
            return Vec::new();
        }
        let ring = self.ring.filter(|_| self.stage == Stage::Ringing);
        match *signal {
            GroupSignal::Ring { ring: id, is_cancelled: true, .. } => {
                if ring.is_some_and(|ring| ring.id == id && ring.ringer == sender) {
                    self.ending = Some(Ending::FarHungUp);
                }
                Vec::new()
            }
            GroupSignal::Answered { ring: id, kind, .. } => {
                if ring.is_some_and(|ring| ring.id == id) && kind != RINGING {
                    self.ending = Some(Ending::Elsewhere);
                }
                Vec::new()
            }
            GroupSignal::Ring { .. } => Vec::new(),
            _ if self.stage == Stage::Ringing => Vec::new(),
            GroupSignal::MediaKey { demux_id, ratchet, secret, .. } => {
                let mut effects = match u8::try_from(ratchet) {
                    Ok(ratchet) => self.take_key(crypto, (sender, demux_id), (ratchet, secret), now),
                    Err(_) => Vec::new(),
                };
                if !self.is_known(demux_id) {
                    effects.extend(self.peek_soon(now));
                }
                effects
            }
            GroupSignal::Leaving { demux_id, .. } => self.take_leaving(Some(sender), demux_id, now),
        }
    }

    // A message of the server's on its SSRC (`handle_sfu_to_device`): what is owed it acknowledged, the latest speaker beside this one kept, a device that joined or left asked after, and a removal the end.
    pub(super) fn take_update(&mut self, crypto: &impl Crypto, bytes: &[u8], now: Duration) -> Vec<Effect> {
        if self.ending.is_some() || !self.is_seated() {
            return Vec::new();
        }
        let updates = self.inbox.take(bytes);
        let ack = self.inbox.take_ack().map(|payload| Effect::Data { is_to_server: true, payload });
        let mut effects: Vec<Effect> = ack.into_iter().collect();
        if updates.iter().any(|update| update.is_removed) {
            effects.extend(self.fail(crypto, Cause::Refused));
            return effects;
        }
        for update in updates {
            if let Some(speaker) = update.speaker.filter(|speaker| self.is_known(*speaker)) {
                self.speaker = Some(speaker);
            }
            if update.is_changed {
                effects.extend(self.peek_soon(now));
            }
        }
        effects.extend(self.rerequest());
        effects
    }

    // What the device of that demux id relayed through the server, sealed as its frames are, and the RTP time it came at (`handle_rtp_received`): a newer heartbeat kept, a leave taken, an ask to mute this device followed; anything of a device not known has the devices asked.
    pub(super) fn take_relayed(
        &mut self,
        crypto: &impl Crypto,
        (demux_id, timestamp): (u32, u32),
        sealed: &[u8],
        now: Duration,
    ) -> Vec<Effect> {
        if self.ending.is_some() || !self.is_seated() {
            return Vec::new();
        }
        let Some(device) = self.devices.iter_mut().find(|device| device.demux_id == demux_id) else {
            return self.peek_soon(now);
        };
        let Some(relayed) = device.frames.open(crypto, sealed).and_then(|plain| Relayed::read(&plain)) else {
            return Vec::new();
        };
        match relayed {
            Relayed::Heartbeat(heartbeat) => {
                if timestamp > device.heard.unwrap_or(0) {
                    (device.heard, device.heartbeat) = (Some(timestamp), heartbeat);
                }
                Vec::new()
            }
            Relayed::Leaving => self.take_leaving(None, demux_id, now),
            Relayed::Mute(target) => self.take_mute(crypto, demux_id, target),
        }
    }

    // Another device's ask that this one mute: followed once, the microphone heard no more and the others told by whom (`handle_remote_mute_request`, `set_outgoing_audio_muted_remotely`).
    fn take_mute(&mut self, crypto: &impl Crypto, source: u32, target: u32) -> Vec<Effect> {
        let is_own = self.seat.is_some_and(|seat| seat.demux_id == target && seat.demux_id != source);
        if !is_own || self.is_muted {
            return Vec::new();
        }
        (self.is_muted, self.muted_by) = (true, Some(source));
        let mut effects: Vec<Effect> = self.heartbeat(crypto).into_iter().collect();
        effects.push(Effect::Muted);
        effects
    }

    // A change of the way to the server (`handle_ice_connection_state_changed`): one that never stood or is lost ends the call; one that broke is sought anew.
    pub(super) fn note_way(&mut self, crypto: &impl Crypto, way: Way, now: Duration) -> Vec<Effect> {
        let Some(connection) = self.connection.filter(|_| self.ending.is_none()) else { return Vec::new() };
        match (connection, way) {
            (Connection::Connecting, Way::Up) => {
                (self.connection, self.beat_at) = (Some(Connection::Connected), Some(now + BEAT));
                let mut effects = self.beat(crypto);
                effects.extend(self.flow());
                effects
            }
            (Connection::Connecting, _) | (_, Way::Lost) => {
                self.connection = None;
                self.fail(crypto, Cause::NoNetwork)
            }
            (Connection::Connected, Way::Down) => {
                self.connection = Some(Connection::Reconnecting);
                vec![Effect::Regather]
            }
            (Connection::Reconnecting, Way::Up) => {
                self.connection = Some(Connection::Connected);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    // A redirect of the request last made, which is made again at the address its location names, read against the request's own as a fetch reads one, a few times at the most; a location that names no address under TLS fails the request as one redirect too many does.
    pub(super) fn follow(&mut self, crypto: &impl Crypto, location: &str, now: Duration) -> Vec<Effect> {
        if self.ending.is_some() {
            return Vec::new();
        }
        let Some(asked) = self.asked.as_mut() else { return Vec::new() };
        let moved = http::Target::parse(&asked.request.url).and_then(|target| target.resolve(location));
        let Some(moved) = moved.filter(|_| asked.hops < MAX_HOPS) else {
            let is_join = asked.is_join;
            self.asked = None;
            if is_join {
                return self.fail(crypto, Cause::Refused);
            }
            self.peeks = Peeks::Failed(now);
            return Vec::new();
        };
        asked.hops += 1;
        asked.request.url = moved.to_address();
        let request = asked.request.clone();
        vec![if asked.is_join { Effect::Join(request) } else { Effect::Peek(request) }]
    }

    // What is due at `now`: the end of a ring nobody took, the token asked anew, a new secret taken into use, the devices asked, and the heartbeat and the video request.
    pub(super) fn tick(&mut self, crypto: &impl Crypto, now: Duration) -> Vec<Effect> {
        if self.ending.is_some() {
            return Vec::new();
        }
        if self.stage == Stage::Ringing {
            if now < self.began + RING_TIME {
                return Vec::new();
            }
            self.ending = Some(Ending::Unanswered);
            return self.ring_log(Course::Missed).into_iter().collect();
        }
        let mut effects = Vec::new();
        if self.token_at.is_some_and(|at| now >= at) {
            effects.extend(self.ask(now));
        }
        if let Some(rotation) = self.rotation.take_if(|rotation| now >= rotation.at) {
            if !self.frames.as_mut().is_some_and(|frames| frames.reset(crypto, rotation.secret)) {
                effects.extend(self.fail(crypto, Cause::SessionFailed));
                return effects;
            }
            if rotation.is_owed {
                effects.extend(self.rotate(crypto, now));
            }
        }
        let is_later = self.peeks_at.iter().any(|at| now >= *at);
        self.peeks_at.retain(|at| now < *at);
        if is_later {
            effects.extend(self.peek_soon(now));
        }
        if self.peek_due().is_some_and(|at| now >= at) {
            effects.extend(self.peek(now));
        }
        if self.beat_at.is_some_and(|at| now >= at) {
            (self.beat_at, self.is_requested) = (Some(now + BEAT), false);
            effects.extend(self.beat(crypto));
        }
        effects
    }

    // The time until something is due; none once the call is over.
    pub(super) fn next_wait(&self, now: Duration) -> Option<Duration> {
        if self.ending.is_some() {
            return None;
        }
        let due = if self.stage == Stage::Ringing {
            Some(self.began + RING_TIME)
        } else {
            let later = self.peeks_at.iter().min().copied();
            let rotation = self.rotation.as_ref().map(|rotation| rotation.at);
            [self.token_at, rotation, later, self.peek_due(), self.beat_at].into_iter().flatten().min()
        };
        due.map(|at| at.saturating_sub(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::code::unhex;
    use crate::base::json;
    use crate::base::math::xorshift64;
    use crate::signal::call::sfu::tests::{joined_or_left, numbered, speaker};
    use crate::signal::device::GroupMember;
    use crate::signal::tests::Standin;

    const GROUP: Peer = Peer::Group([7; 32]);
    const OWN: ServiceId = ServiceId::Aci([0x0E; 16]);
    const ANN: ServiceId = ServiceId::Aci([0xA1; 16]);
    const BEN: ServiceId = ServiceId::Aci([0xB2; 16]);
    const CAT: ServiceId = ServiceId::Aci([0xC3; 16]);
    const HERE: u32 = 0x1000;
    const ANN_DEVICE: u32 = 0x2000;
    const BEN_DEVICE: u32 = 0x3000;
    const CAT_DEVICE: u32 = 0x4000;
    const ERA: &str = "1122334455667788";
    const CALL: u64 = 0x1122_3344_5566_7788;
    const NOW: Duration = Duration::from_secs(1_000);
    const ICE: (&str, &str) = ("Ab12", "0123456789abcdefghijkl");
    const TOKEN: &str = "2:own:group:1700000000:1:mac";

    fn membership() -> Membership {
        let member =
            |service, opaque: &str, name: &str| GroupMember { service, opaque: opaque.into(), name: Some(name.into()) };
        Membership {
            token: TOKEN.into(),
            members: vec![
                member(OWN, "0e", "Me"),
                member(ANN, "a1", "Ann"),
                member(BEN, "b2", "Ben"),
                member(CAT, "c3", "Cat"),
            ],
            own: OWN,
        }
    }

    fn joined(server: &Pair, creator: &str) -> Joined {
        Joined {
            demux_id: HERE,
            udp: vec!["192.0.2.80:10000".parse().unwrap()],
            tcp: vec!["192.0.2.80:10000".parse().unwrap()],
            tls: vec!["192.0.2.80:443".parse().unwrap()],
            hostname: Some("sfu.example.org".into()),
            ufrag: "Sf4u".into(),
            pwd: "SfuPasswordOf32CharactersInBase6".into(),
            public_key: server.public,
            creator: creator.into(),
            era: ERA.into(),
            is_active: true,
        }
    }

    fn peek(devices: &[(u32, &str)]) -> Vec<sfu::Member> {
        let member = |(demux_id, user): &(u32, &str)| sfu::Member { demux_id: *demux_id, user: Some((*user).into()) };
        devices.iter().map(member).collect()
    }

    fn key(demux_id: u32, ratchet: u32, secret: Secret) -> GroupSignal {
        GroupSignal::MediaKey { group: GROUP, demux_id, ratchet, secret }
    }

    // This side's media keys among the effects: whom each goes to, its ratchet's step and its secret.
    fn keys(effects: &[Effect]) -> Vec<(Target, u32, Secret)> {
        let key = |effect: &Effect| match effect {
            Effect::Signal(Outbound {
                target,
                device: None,
                signal:
                    Signal::Group {
                        signal: GroupSignal::MediaKey { demux_id: HERE, ratchet, secret, .. },
                        is_urgent: false,
                    },
            }) => Some((target.clone(), *ratchet, *secret)),
            _ => None,
        };
        effects.iter().filter_map(key).collect()
    }

    fn peeks(effects: &[Effect]) -> usize {
        effects.iter().filter(|effect| matches!(effect, Effect::Peek(request) if request.method == "GET")).count()
    }

    const TO_SERVER: bool = true;
    const TO_DEVICES: bool = false;

    // The payloads among the effects for the server itself, or for the devices through it.
    fn data(effects: &[Effect], to_server: bool) -> Vec<&[u8]> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Data { is_to_server, payload } if *is_to_server == to_server => Some(&payload[..]),
                _ => None,
            })
            .collect()
    }

    fn to_server(payload: Vec<u8>) -> Effect {
        Effect::Data { is_to_server: true, payload }
    }

    fn leaving(target: Target) -> Effect {
        let signal = Signal::Group { signal: GroupSignal::Leaving { group: GROUP, demux_id: HERE }, is_urgent: false };
        Effect::Signal(Outbound { target, device: None, signal })
    }

    fn opened(frames: &mut frame::Inbound, crypto: &Standin, sealed: &[u8]) -> Option<Relayed> {
        Relayed::read(&frames.open(crypto, sealed)?)
    }

    fn sealed_beat(crypto: &Standin, frames: &mut frame::Outbound, heartbeat: Heartbeat) -> Vec<u8> {
        frames.seal(crypto, &Relayed::Heartbeat(heartbeat).write()).unwrap()
    }

    // A call the owner began, joined at `NOW` as the call's creator, the way to the server standing.
    fn begun(crypto: &Standin) -> Call {
        let (mut call, _) = Call::outgoing(GROUP, false, ICE, NOW);
        let server = Pair::new(crypto).unwrap();
        call.take_membership(crypto, Some(membership()), NOW);
        call.take_joined(crypto, Ok(joined(&server, "0e")), NOW);
        call.note_way(crypto, Way::Up, NOW);
        call
    }

    #[test]
    fn joins_and_talks() {
        let crypto = Standin::default();
        let (mut call, effects) = Call::outgoing(GROUP, false, ICE, NOW);
        assert_eq!((effects, call.phase()), (vec![Effect::Membership], Phase::Connecting), "the token asked first");
        let effects = call.take_membership(&crypto, Some(membership()), NOW);
        let [Effect::Join(request)] = &effects[..] else { panic!("the join, not {effects:?}") };
        assert_eq!(
            (request.method, request.url.as_str()),
            ("PUT", "https://sfu.voip.signal.org/v2/conference/participants"),
            "the join to the calling server"
        );
        let body = core::str::from_utf8(&request.body).unwrap();
        assert_eq!(
            (json::text_at(body, &["iceUfrag"]), json::text_at(body, &["icePwd"])),
            (Some(ICE.0), Some(ICE.1)),
            "under the ICE agent's credentials"
        );
        let public: [u8; 32] = unhex(json::text_at(body, &["dhePublicKey"]).unwrap()).unwrap().try_into().unwrap();
        let server = Pair::new(&crypto).unwrap();
        let effects = call.take_joined(&crypto, Ok(joined(&server, "0e")), NOW);
        let keys_agreed = sfu::negotiate(&crypto, &server, &public).unwrap();
        let ring = Signal::Group {
            signal: GroupSignal::Ring { group: GROUP, ring: CALL as i64, is_cancelled: false },
            is_urgent: true,
        };
        assert_eq!(
            effects[..4],
            [
                Effect::Open {
                    demux_id: HERE,
                    members: 4,
                    ufrag: "Sf4u".into(),
                    pwd: "SfuPasswordOf32CharactersInBase6".into(),
                    keys: keys_agreed
                },
                Effect::Server { address: "192.0.2.80:10000".parse().unwrap(), transport: Transport::Udp, name: None },
                Effect::Server { address: "192.0.2.80:10000".parse().unwrap(), transport: Transport::Tcp, name: None },
                Effect::Server {
                    address: "192.0.2.80:443".parse().unwrap(),
                    transport: Transport::Tls,
                    name: Some("sfu.example.org".into())
                },
            ],
            "the server's credentials and the keys the server comes to as well, then each of its addresses"
        );
        assert_eq!(peeks(&effects[4..5]), 1, "a peek at once");
        assert_eq!(
            effects[5..],
            [
                Effect::Signal(Outbound { target: Target::Group(GROUP), device: None, signal: ring }),
                Effect::Log { call: CALL, is_outgoing: true, ringer: Some(OWN), course: Course::Ringing },
            ],
            "the creator of a small group's call rings it, by the era's id, and the log keeps an outgoing ring"
        );
        let effects = call.note_way(&crypto, Way::Up, NOW);
        assert_eq!(data(&effects, TO_DEVICES).len(), 1, "a heartbeat through the server");
        assert_eq!(
            data(&effects, TO_SERVER),
            [&sfu::write_video_request(&[], MAX_KBPS, SHOWN_HEIGHT)[..]],
            "and the video request, of nobody yet"
        );
        assert_eq!(call.phase(), Phase::Ringing, "alone after its ring, the call rings in the group");
        let told = peek(&[(HERE, "0e"), (ANN_DEVICE, "a1"), (BEN_DEVICE, "b2")]);
        let effects = call.take_peek(&crypto, Ok(told), NOW + BEAT / 2);
        let [(target, 1, own)] = &keys(&effects)[..] else { panic!("one key, not {effects:?}") };
        assert_eq!(
            *target,
            Target::Members(GROUP, vec![ANN, BEN]),
            "the key stepped on goes to the two new users at once"
        );
        let request =
            sfu::write_video_request(&[(ANN_DEVICE, SHOWN_HEIGHT), (BEN_DEVICE, NO_VIDEO)], MAX_KBPS, SHOWN_HEIGHT);
        assert_eq!(
            effects[1..],
            [Effect::Media(Some(SMALL_RATE)), to_server(request)],
            "sound and pictures flow, and the first device's picture is asked at the height it shows at"
        );
        assert_eq!(call.phase(), Phase::Talking, "talked in once others are in the call");
        let mut ann = frame::Outbound::new(&crypto, [0xA1; 32]).unwrap();
        assert!(
            call.take_signal(&crypto, ANN, &key(ANN_DEVICE, 0, [0xA1; 32]), NOW).is_empty(),
            "a member's key for its own device is taken"
        );
        let muted = Heartbeat { is_audio_muted: Some(true), is_video_muted: Some(false), ..Heartbeat::default() };
        let beat = sealed_beat(&crypto, &mut ann, muted);
        assert!(call.take_relayed(&crypto, (ANN_DEVICE, 7), &beat, NOW).is_empty(), "a heartbeat is kept");
        let late = sealed_beat(&crypto, &mut ann, Heartbeat::default());
        call.take_relayed(&crypto, (ANN_DEVICE, 6), &late, NOW);
        let mut forged = frame::Outbound::new(&crypto, [0x55; 32]).unwrap();
        call.take_signal(&crypto, BEN, &key(ANN_DEVICE, 0, [0x55; 32]), NOW);
        let unmuted = Heartbeat { is_audio_muted: Some(false), ..muted };
        call.take_relayed(&crypto, (ANN_DEVICE, 8), &sealed_beat(&crypto, &mut forged, unmuted), NOW);
        let member = |name: &str, is_speaking, is_muted, has_video| Member {
            name: Some(name.into()),
            is_speaking,
            is_muted,
            has_video,
        };
        assert_eq!(
            call.members(),
            [member("Ann", false, true, true), member("Ben", false, false, false)],
            "by name, as the newest heartbeat tells; an older one and one under a key another user sent count for nothing"
        );
        let effects = call.take_update(&crypto, &numbered(speaker(BEN_DEVICE), 1, None), NOW);
        assert_eq!(effects, [to_server(vec![0x42, 0x02, 0x10, 0x02])], "the server's message acknowledged");
        assert_eq!((call.shown(), call.members()[1].is_speaking), (Some(BEN_DEVICE), true), "Ben speaks and shows");
        assert_eq!(call.next_wait(NOW), Some(BEAT), "the next beat due a second after the way stood");
        let effects = call.tick(&crypto, NOW + BEAT);
        let request =
            sfu::write_video_request(&[(ANN_DEVICE, NO_VIDEO), (BEN_DEVICE, SHOWN_HEIGHT)], MAX_KBPS, SHOWN_HEIGHT);
        assert_eq!(data(&effects, TO_SERVER), [&request[..]], "the speaker's picture asked at the next beat");
        let mut far = frame::Inbound::default();
        assert!(far.add(&crypto, 1, *own), "the far side takes the key");
        let heartbeat = Heartbeat { is_audio_muted: Some(false), is_video_muted: Some(true), ..Heartbeat::default() };
        assert_eq!(
            opened(&mut far, &crypto, data(&effects, TO_DEVICES)[0]),
            Some(Relayed::Heartbeat(heartbeat)),
            "this side's heartbeat opens under the key sent: heard, no picture"
        );
        let sealed = call.seal(&crypto, b"opus").unwrap();
        assert_eq!(far.open(&crypto, &sealed).as_deref(), Some(&b"opus"[..]), "a frame sealed under the same");
        assert_eq!(
            call.open(&crypto, ANN_DEVICE, &ann.seal(&crypto, b"vp8").unwrap()).as_deref(),
            Some(&b"vp8"[..]),
            "a frame of Ann's opens under her key"
        );
    }

    #[test]
    fn let_in_when_listed() {
        let crypto = Standin::default();
        let (mut call, _) = Call::outgoing(GROUP, true, ICE, NOW);
        let server = Pair::new(&crypto).unwrap();
        call.take_membership(&crypto, Some(membership()), NOW);
        let pending = Joined { is_active: false, ..joined(&server, "a1") };
        let effects = call.take_joined(&crypto, Ok(pending), NOW);
        assert!(
            matches!(
                effects.last(),
                Some(Effect::Log { is_outgoing: false, ringer: Some(ANN), course: Course::Accepted, .. })
            ),
            "another's call joined rings nobody, and the log keeps it joined"
        );
        let effects = call.note_way(&crypto, Way::Up, NOW);
        assert_eq!(
            (data(&effects, TO_DEVICES).len(), data(&effects, TO_SERVER).len(), call.phase()),
            (0, 1, Phase::Connecting),
            "not let in yet: no heartbeat, the video request alone"
        );
        let effects = call.take_peek(&crypto, Ok(peek(&[(ANN_DEVICE, "a1")])), NOW);
        assert_eq!((keys(&effects).len(), call.phase()), (1, Phase::Connecting), "a key while waiting, not yet in");
        call.take_peek(&crypto, Ok(peek(&[(ANN_DEVICE, "a1"), (HERE, "0e")])), NOW);
        assert_eq!(call.phase(), Phase::Talking, "let in once a peek lists this device");
        assert_eq!(data(&call.tick(&crypto, NOW + BEAT), TO_DEVICES).len(), 1, "and heard from");
    }

    #[test]
    fn key_schedule() {
        let crypto = Standin::default();
        let mut call = begun(&crypto);
        let at = |seconds| NOW + Duration::from_secs(seconds);
        let step = |call: &mut Call| {
            let frame = call.seal(&crypto, b"frame").unwrap();
            frame[frame.len() - frame::TAIL_BYTES]
        };
        let effects = call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (ANN_DEVICE, "a1")])), at(1));
        assert_eq!(
            keys(&effects).iter().map(|(target, step, _)| (target.clone(), *step)).collect::<Vec<_>>(),
            [(Target::Contact(ANN), 1)],
            "the first device beside this one: the key one step on, to its one user"
        );
        let effects = call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (ANN_DEVICE, "a1"), (BEN_DEVICE, "b2")])), at(2));
        let [(target, 2, _)] = &keys(&effects)[..] else { panic!("one key, not {effects:?}") };
        assert_eq!(
            (target, step(&mut call)),
            (&Target::Contact(BEN), 2),
            "a device that joined: the key one step on, to its user alone"
        );
        let effects = call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (BEN_DEVICE, "b2")])), at(3));
        let [(Target::Contact(BEN), 0, renewed)] = keys(&effects)[..] else { panic!("a new secret, not {effects:?}") };
        assert_eq!(step(&mut call), 2, "a user left: a new secret goes to those who stay at once, and waits");
        let effects = call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (BEN_DEVICE, "b2"), (CAT_DEVICE, "c3")])), at(4));
        let cat_keys: Vec<(Target, u32)> =
            keys(&effects).iter().map(|(target, step, _)| (target.clone(), *step)).collect();
        assert_eq!(
            cat_keys,
            [(Target::Contact(CAT), 3), (Target::Contact(CAT), 0)],
            "one joining meanwhile gets the key and the secret that waits"
        );
        assert_eq!(keys(&effects)[1].2, renewed, "the secret that waits is the one Ben was sent");
        let effects = call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (BEN_DEVICE, "b2")])), at(5));
        assert!(keys(&effects).is_empty(), "a second leave while a secret waits sends nothing yet");
        call.tick(&crypto, at(8) - Duration::from_millis(1));
        assert_eq!(step(&mut call), 3, "the secret waits five seconds");
        let effects = call.tick(&crypto, at(8));
        let mut ben = frame::Inbound::default();
        assert!(ben.add(&crypto, 0, renewed), "Ben takes the new secret");
        let frame = call.seal(&crypto, b"frame").unwrap();
        assert_eq!(ben.open(&crypto, &frame).as_deref(), Some(&b"frame"[..]), "then the new secret seals");
        let [(Target::Contact(BEN), 0, again)] = keys(&effects)[..] else { panic!("the owed secret, not {effects:?}") };
        assert_ne!(again, renewed, "and the leave owed has another sent at once");
        call.tick(&crypto, at(13));
        let mut later = frame::Inbound::default();
        assert!(later.add(&crypto, 0, again), "Ben takes the owed secret");
        assert!(
            later.open(&crypto, &call.seal(&crypto, b"frame").unwrap()).is_some(),
            "taken into use five seconds on"
        );
    }

    #[test]
    fn keys_held_for_unseen_devices() {
        let crypto = Standin::default();
        let mut call = begun(&crypto);
        call.take_peek(&crypto, Ok(peek(&[(HERE, "0e")])), NOW);
        let effects = call.take_signal(&crypto, ANN, &key(ANN_DEVICE, 0, [0xA1; 32]), NOW);
        assert_eq!(
            (effects.first(), peeks(&effects)),
            (Some(&Effect::Membership), 1),
            "a key of a device not seen is held: the members asked anew, and the devices"
        );
        let effects = call.take_signal(&crypto, ANN, &key(ANN_DEVICE, 1, [0xA2; 32]), NOW);
        assert!(!effects.contains(&Effect::Membership), "asked once while keys are held");
        assert!(
            call.take_signal(&crypto, ANN, &key(ANN_DEVICE, 256, [0xA3; 32]), NOW).is_empty() && call.held.len() == 2,
            "a step past a byte is no key"
        );
        call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (ANN_DEVICE, "a1")])), NOW);
        let mut ann = frame::Outbound::new(&crypto, [0xA1; 32]).unwrap();
        let muted = Heartbeat { is_audio_muted: Some(true), ..Heartbeat::default() };
        call.take_relayed(&crypto, (ANN_DEVICE, 1), &sealed_beat(&crypto, &mut ann, muted), NOW);
        assert!(call.held.is_empty() && call.members()[0].is_muted, "taken once the peek gives the device to its user");
        for demux_id in 0..2 * HELD_KEYS as u32 {
            call.take_signal(&crypto, CAT, &key(0x10_0000 + (demux_id << 4), 0, [0xC3; 32]), NOW);
        }
        assert_eq!(call.held.len(), HELD_KEYS, "no more held than devices of a call keep");
    }

    #[test]
    fn full_and_failed_joins() {
        let crypto = Standin::default();
        for (answer, ending) in [
            (Err(Some(FULL)), Ending::Full),
            (Err(Some(500)), Ending::Failed(Cause::NoAnswer)),
            (Err(Some(403)), Ending::Failed(Cause::Refused)),
            (Err(None), Ending::Failed(Cause::NoNetwork)),
        ] {
            let (mut call, _) = Call::outgoing(GROUP, false, ICE, NOW);
            call.take_membership(&crypto, Some(membership()), NOW);
            assert!(call.take_joined(&crypto, answer, NOW).is_empty(), "nothing told where nothing was joined");
            assert_eq!((call.phase(), call.next_wait(NOW)), (Phase::Ended(ending), None), "{ending:?}");
        }
        let (mut call, _) = Call::outgoing(GROUP, false, ICE, NOW);
        assert!(call.take_membership(&crypto, None, NOW).is_empty(), "nothing told before the join");
        assert_eq!(call.ending(), Some(Ending::Failed(Cause::NoNetwork)), "no token: no call");
        let mut call = begun(&crypto);
        assert!(
            call.take_membership(&crypto, None, NOW).is_empty() && call.ending().is_none(),
            "a token asked anew and not had changes nothing"
        );
    }

    #[test]
    fn rings_accepted_and_declined() {
        let crypto = Standin::default();
        let ring = (CALL as i64, ANN);
        let log = |course| Effect::Log { call: CALL, is_outgoing: false, ringer: Some(ANN), course };
        let answered = |kind| {
            let signal = Signal::Group {
                signal: GroupSignal::Answered { group: GROUP, ring: CALL as i64, kind },
                is_urgent: true,
            };
            Effect::Signal(Outbound { target: Target::Own, device: None, signal })
        };
        let (mut call, effects) = Call::ringing(GROUP, ring, NOW);
        assert_eq!(
            (effects, call.phase(), call.next_wait(NOW)),
            (vec![log(Course::Ringing)], Phase::Incoming, Some(RING_TIME)),
            "a ring rings here for a minute at the most, and the log keeps it ringing"
        );
        assert_eq!(
            call.accept(ICE, NOW),
            [answered(ACCEPTED), log(Course::Accepted), Effect::Membership],
            "taken: the account's other devices told, the log, the token asked"
        );
        assert!(!call.is_outgoing() && call.phase() == Phase::Connecting, "a ring taken is being joined");
        assert_eq!(
            peeks(&call.take_membership(&crypto, Some(membership()), NOW)),
            0,
            "no peek before the join's answer"
        );
        let server = Pair::new(&crypto).unwrap();
        let effects = call.take_joined(&crypto, Ok(joined(&server, "a1")), NOW);
        assert!(!effects.iter().any(|effect| matches!(effect, Effect::Signal(_))), "a ring answered rings nobody");
        assert_eq!(effects.last(), Some(&log(Course::Accepted)), "joined the ringer's call");
        let (mut call, _) = Call::ringing(GROUP, ring, NOW);
        assert_eq!(
            call.end(&crypto),
            [answered(DECLINED), log(Course::Missed)],
            "sent away: told, and missed in the log"
        );
        assert_eq!(call.phase(), Phase::Ended(Ending::HungUp), "the owner sent it away");
        let (mut call, _) = Call::ringing(GROUP, ring, NOW);
        assert!(call.tick(&crypto, NOW + RING_TIME - Duration::from_millis(1)).is_empty(), "it rings on");
        assert_eq!(call.tick(&crypto, NOW + RING_TIME), [log(Course::Missed)], "a minute and nobody took it");
        assert_eq!(call.phase(), Phase::Ended(Ending::Unanswered), "unanswered");
        let (mut call, _) = Call::ringing(GROUP, ring, NOW);
        let cancel = |id| GroupSignal::Ring { group: GROUP, ring: id, is_cancelled: true };
        call.take_signal(&crypto, BEN, &cancel(CALL as i64), NOW);
        call.take_signal(&crypto, ANN, &cancel(5), NOW);
        assert_eq!(call.phase(), Phase::Incoming, "a cancel from another, or of another ring");
        assert!(call.take_signal(&crypto, ANN, &cancel(CALL as i64), NOW).is_empty(), "nothing told of a cancel");
        assert_eq!(call.phase(), Phase::Ended(Ending::FarHungUp), "the ringer's cancel, which the gate logs");
        let (mut call, _) = Call::ringing(GROUP, ring, NOW);
        let elsewhere = |kind| GroupSignal::Answered { group: GROUP, ring: CALL as i64, kind };
        call.take_signal(&crypto, OWN, &elsewhere(RINGING), NOW);
        call.take_signal(&crypto, ANN, &key(ANN_DEVICE, 0, [1; 32]), NOW);
        assert_eq!(call.phase(), Phase::Incoming, "another device ringing, and a key while it rings");
        call.take_signal(&crypto, OWN, &elsewhere(ACCEPTED), NOW);
        assert_eq!(call.phase(), Phase::Ended(Ending::Elsewhere), "taken on another device, which the gate logs");
    }

    #[test]
    fn owner_leaves() {
        let crypto = Standin::default();
        let mut call = begun(&crypto);
        let effects = call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (ANN_DEVICE, "a1"), (BEN_DEVICE, "b2")])), NOW);
        let [(_, 1, own)] = keys(&effects)[..] else { panic!("the key, not {effects:?}") };
        let effects = call.end(&crypto);
        let mut far = frame::Inbound::default();
        assert!(far.add(&crypto, 1, own), "the far side takes the key");
        assert_eq!(
            opened(&mut far, &crypto, data(&effects, TO_DEVICES)[0]),
            Some(Relayed::Leaving),
            "through the server"
        );
        let leave = sfu::write_leave();
        assert_eq!(
            effects[1..],
            [
                leaving(Target::Members(GROUP, vec![ANN, BEN])),
                leaving(Target::Own),
                to_server(leave.clone()),
                to_server(leave),
            ],
            "over Signal to the users of the last peek, the account's own apart, and to the server twice"
        );
        assert_eq!((call.phase(), call.next_wait(NOW)), (Phase::Ended(Ending::HungUp), None), "over, nothing due");
        assert!(call.end(&crypto).is_empty() && call.tick(&crypto, NOW + BEAT).is_empty(), "over once");
        let mut alone = begun(&crypto);
        let effects = alone.end(&crypto);
        let cancel = Signal::Group {
            signal: GroupSignal::Ring { group: GROUP, ring: CALL as i64, is_cancelled: true },
            is_urgent: true,
        };
        assert_eq!(
            effects[0],
            Effect::Signal(Outbound { target: Target::Group(GROUP), device: None, signal: cancel }),
            "alone after its ring, the ring is cancelled"
        );
        assert_eq!(
            (data(&effects, TO_DEVICES).len(), data(&effects, TO_SERVER).len(), effects.len()),
            (1, LEAVES, 4),
            "then the leave through the server and to it, and over Signal to nobody, as no peek told anyone"
        );
        let mut joined_by = begun(&crypto);
        joined_by.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (ANN_DEVICE, "a1")])), NOW);
        joined_by.take_peek(&crypto, Ok(peek(&[(HERE, "0e")])), NOW);
        assert!(
            !joined_by
                .end(&crypto)
                .iter()
                .any(|effect| matches!(effect, Effect::Signal(Outbound { target: Target::Group(_), .. }))),
            "one that someone joined is not"
        );
    }

    #[test]
    fn mutes() {
        let crypto = Standin::default();
        let mut call = begun(&crypto);
        let effects = call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (ANN_DEVICE, "a1")])), NOW);
        let [(_, 1, own)] = keys(&effects)[..] else { panic!("the key, not {effects:?}") };
        let mut far = frame::Inbound::default();
        assert!(far.add(&crypto, 1, own), "the far side takes the key");
        let told =
            |effects: &[Effect], far: &mut frame::Inbound| match opened(far, &crypto, data(effects, TO_DEVICES)[0]) {
                Some(Relayed::Heartbeat(heartbeat)) => heartbeat,
                other => panic!("a heartbeat, not {other:?}"),
            };
        let effects = call.video(&crypto, true);
        assert_eq!(told(&effects, &mut far).is_video_muted, Some(false), "a picture sent is told at once");
        let mut ann = frame::Outbound::new(&crypto, [0xA1; 32]).unwrap();
        call.take_signal(&crypto, ANN, &key(ANN_DEVICE, 0, [0xA1; 32]), NOW);
        let mut ask = |target| ann.seal(&crypto, &Relayed::Mute(target).write()).unwrap();
        let asked = ask(HERE);
        let effects = call.take_relayed(&crypto, (ANN_DEVICE, 2), &asked, NOW);
        assert_eq!(effects.last(), Some(&Effect::Muted), "Ann's ask mutes this device");
        let heartbeat = told(&effects, &mut far);
        assert_eq!(
            (heartbeat.is_audio_muted, heartbeat.muted_by),
            (Some(true), Some(ANN_DEVICE)),
            "by her, as the others are told"
        );
        assert!(call.take_relayed(&crypto, (ANN_DEVICE, 3), &ask(HERE), NOW).is_empty(), "once");
        let heartbeat = told(&call.mute(&crypto, true), &mut far);
        assert_eq!(heartbeat.muted_by, Some(ANN_DEVICE), "the owner's own mute keeps who muted first");
        let heartbeat = told(&call.mute(&crypto, false), &mut far);
        assert_eq!((heartbeat.is_audio_muted, heartbeat.muted_by), (Some(false), None), "heard again, muted by nobody");
        assert!(
            call.take_relayed(&crypto, (ANN_DEVICE, 4), &ask(BEN_DEVICE), NOW).is_empty(),
            "an ask for another device"
        );
    }

    #[test]
    fn server_way_lost() {
        let crypto = Standin::default();
        let (mut never, _) = Call::outgoing(GROUP, false, ICE, NOW);
        assert!(never.note_way(&crypto, Way::Up, NOW).is_empty(), "no way before the join's answer");
        never.take_membership(&crypto, Some(membership()), NOW);
        never.take_joined(&crypto, Ok(joined(&Pair::new(&crypto).unwrap(), "a1")), NOW);
        assert!(never.note_way(&crypto, Way::Down, NOW).is_empty(), "nothing told over a way that never stood");
        assert_eq!(never.phase(), Phase::Ended(Ending::Failed(Cause::NoNetwork)), "which ends the call");
        let mut call = begun(&crypto);
        call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (ANN_DEVICE, "a1")])), NOW);
        assert_eq!(
            (call.note_way(&crypto, Way::Down, NOW), call.phase()),
            (vec![Effect::Regather], Phase::Reconnecting),
            "sought anew"
        );
        assert!(call.note_way(&crypto, Way::Up, NOW).is_empty() && call.phase() == Phase::Talking, "and found");
        assert_eq!(
            call.note_way(&crypto, Way::Lost, NOW),
            [leaving(Target::Contact(ANN)), leaving(Target::Own)],
            "lost for good: the leave over Signal alone"
        );
        assert_eq!(call.phase(), Phase::Ended(Ending::Failed(Cause::NoNetwork)), "which ends the call");
    }

    #[test]
    fn peek_schedule() {
        let crypto = Standin::default();
        let mut call = begun(&crypto);
        let at = |ms| NOW + Duration::from_millis(ms);
        let two = || peek(&[(HERE, "0e"), (ANN_DEVICE, "a1")]);
        let changed = joined_or_left().finish();
        assert_eq!(
            peeks(&call.take_update(&crypto, &changed, at(100))),
            0,
            "a change told while a peek is on its way waits"
        );
        assert_eq!(peeks(&call.take_peek(&crypto, Ok(two()), at(200))), 1, "for its answer");
        assert_eq!(
            (peeks(&call.tick(&crypto, at(5_199))), peeks(&call.tick(&crypto, at(5_200)))),
            (0, 1),
            "unanswered five seconds: asked again"
        );
        call.take_peek(&crypto, Ok(two()), at(5_300));
        assert_eq!(call.peek_due(), Some(at(15_300)), "the next due ten seconds on");
        assert_eq!(
            (peeks(&call.tick(&crypto, at(15_299))), peeks(&call.tick(&crypto, at(15_300)))),
            (0, 1),
            "asked by the clock"
        );
        assert!(call.take_peek(&crypto, Err(Some(500)), at(15_400)).is_empty(), "a peek that failed");
        let stranger = call.take_relayed(&crypto, (CAT_DEVICE, 1), b"sealed", at(16_000));
        assert_eq!(peeks(&stranger), 0, "after a failure, nothing sooner than five seconds");
        assert_eq!(
            (peeks(&call.tick(&crypto, at(20_399))), peeks(&call.tick(&crypto, at(20_400)))),
            (0, 1),
            "and five seconds on, asked again"
        );
        call.take_peek(&crypto, Ok(two()), at(20_500));
        let leave = GroupSignal::Leaving { group: GROUP, demux_id: ANN_DEVICE };
        assert_eq!(peeks(&call.take_signal(&crypto, BEN, &leave, at(21_000))), 0, "a leave from another user");
        assert_eq!(peeks(&call.take_signal(&crypto, ANN, &leave, at(21_000))), 1, "a leave: asked at once");
        call.take_peek(&crypto, Ok(two()), at(21_100));
        assert_eq!(
            (peeks(&call.tick(&crypto, at(22_999))), peeks(&call.tick(&crypto, at(23_000)))),
            (0, 1),
            "and two seconds later"
        );
        call.take_peek(&crypto, Ok(two()), at(23_100));
        assert_eq!(
            peeks(&call.take_relayed(&crypto, (CAT_DEVICE, 1), b"sealed", at(24_000))),
            1,
            "data of a device not known"
        );
        call.take_peek(&crypto, Ok(two()), at(24_100));
        assert_eq!(peeks(&call.take_signal(&crypto, CAT, &key(CAT_DEVICE, 0, [3; 32]), at(25_000))), 1, "a key of one");
        call.take_peek(&crypto, Ok(two()), at(25_100));
        assert_eq!(peeks(&call.take_update(&crypto, &changed, at(26_000))), 1, "a device joined or left");
        call.take_peek(&crypto, Ok(two()), at(26_100));
        assert_eq!(peeks(&call.take_peek(&crypto, Err(Some(NOT_FOUND)), at(26_200))), 0, "no call found: no devices");
        assert!(call.members().is_empty(), "nobody beside this device");
        let mut changed_members = membership();
        changed_members.members.pop();
        assert_eq!(peeks(&call.take_membership(&crypto, Some(changed_members), at(27_000))), 1, "the members changed");
        assert!(
            !call.tick(&crypto, at(26_199) + TOKEN_AGE).contains(&Effect::Membership),
            "the token not asked anew sooner than a day after it was last asked"
        );
        let day = call.tick(&crypto, at(26_200) + TOKEN_AGE);
        assert!(day.contains(&Effect::Membership), "the token asked anew a day after it was last asked");
    }

    #[test]
    fn redirects_followed() {
        let crypto = Standin::default();
        let (mut call, _) = Call::outgoing(GROUP, false, ICE, NOW);
        let effects = call.take_membership(&crypto, Some(membership()), NOW);
        let [Effect::Join(first)] = &effects[..] else { panic!("the join, not {effects:?}") };
        let region = "https://region.example/v2/conference/participants?region=x";
        for _ in 0..MAX_HOPS {
            let effects = call.follow(&crypto, region, NOW);
            let [Effect::Join(moved)] = &effects[..] else { panic!("the join again, not {effects:?}") };
            assert_eq!(
                (moved.method, moved.url.as_str(), &moved.headers, &moved.body),
                (first.method, region, &first.headers, &first.body),
                "the same request at the URL named"
            );
        }
        assert!(call.follow(&crypto, region, NOW).is_empty(), "no join past them");
        assert_eq!(call.ending(), Some(Ending::Failed(Cause::Refused)), "a few redirects at the most");
        let (mut call, _) = Call::outgoing(GROUP, false, ICE, NOW);
        call.take_membership(&crypto, Some(membership()), NOW);
        let [Effect::Join(beside)] = &call.follow(&crypto, "participants?region=y", NOW)[..] else {
            panic!("the join again")
        };
        assert_eq!(
            beside.url, "https://sfu.voip.signal.org/v2/conference/participants?region=y",
            "a location that names no server is read against the request's own address"
        );
        assert!(call.follow(&crypto, "http://region.example/v2/conference/participants", NOW).is_empty());
        assert_eq!(
            call.ending(),
            Some(Ending::Failed(Cause::Refused)),
            "a redirect into the clear is followed nowhere"
        );
        let mut call = begun(&crypto);
        let [Effect::Peek(moved)] = &call.follow(&crypto, region, NOW)[..] else { panic!("the peek again") };
        assert_eq!((moved.method, moved.url.as_str()), ("GET", region), "a peek followed the same way");
        for _ in 1..MAX_HOPS {
            call.follow(&crypto, region, NOW);
        }
        assert!(
            call.follow(&crypto, region, NOW).is_empty() && call.peeks == Peeks::Failed(NOW),
            "a peek redirected past them failed"
        );
        let mut call = begun(&crypto);
        assert!(
            call.follow(&crypto, "mailto:nobody@example.org", NOW).is_empty() && call.peeks == Peeks::Failed(NOW),
            "and so has one sent to what is no address"
        );
    }

    #[test]
    fn strangers_input() {
        let crypto = Standin::default();
        let fresh = || {
            let mut call = begun(&crypto);
            call.take_peek(&crypto, Ok(peek(&[(HERE, "0e"), (ANN_DEVICE, "a1")])), NOW);
            call.take_signal(&crypto, ANN, &key(ANN_DEVICE, 0, [0xA1; 32]), NOW);
            call
        };
        let mut call = fresh();
        let mut ann = frame::Outbound::new(&crypto, [0xA1; 32]).unwrap();
        let valid = sealed_beat(&crypto, &mut ann, Heartbeat::default());
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for round in 0..2_000u32 {
            state = xorshift64(state);
            let bytes: Vec<u8> = (0..state % 64).map(|at| (state >> (at % 57)) as u8).collect();
            let mut damaged = valid.clone();
            damaged[state as usize % valid.len()] ^= (state >> 8) as u8 | 1;
            damaged.truncate(valid.len() - (state >> 16) as usize % 3);
            let now = NOW + Duration::from_millis(u64::from(round));
            call.take_update(&crypto, &bytes, now);
            call.take_relayed(&crypto, (ANN_DEVICE, round), &bytes, now);
            call.take_relayed(&crypto, (ANN_DEVICE, round), &damaged, now);
            call.take_relayed(&crypto, (ANN_DEVICE, round), &ann.seal(&crypto, &bytes).unwrap(), now);
            call.take_relayed(&crypto, ((state as u32) << 4, round), &bytes, now);
            call.open(&crypto, ANN_DEVICE, &bytes);
            call.tick(&crypto, now);
            if call.ending().is_some() {
                call = fresh();
            }
        }
    }
}

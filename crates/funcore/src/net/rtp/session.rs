use super::allocation::Allocation;
use super::demux::{self, Kind as Arrival};
use super::dependency::{Descriptor as Dependency, Dti, Frame as Dependent, Structure};
use super::estimate::{Bounds, Estimator};
use super::extension::{AudioLevel, DEPENDENCY_DESCRIPTOR, Element, LAYERS_ALLOCATION, Orientation, Turn};
use super::feedback::{write_nack, write_pli};
use super::frames::{Assembler, Codec, Frame, Incoming};
use super::history::History;
use super::layout::{self, Kind, Own, Route};
use super::nack::Missing;
use super::pacer::{Class, Pacer, QUEUE_LIMIT};
use super::packet::{Header, Packet};
use super::receive::Receiver;
use super::rtcp::{self, Packet as Control, ReportBlock, write_receiver_report, write_sender_report};
use super::send::Sender;
use super::srtp::{Inbound, Master, Outbound};
use super::transport_cc::{Departures, Recorder};
use super::vp8::{self, Budget, Picture};
use super::{Crypto, TAG_BYTES, red, rtx, units};
use crate::base::math::xorshift64;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::time::Duration;

// The RTP clocks of Opus and of video (RFC 7587, 4.1; RFC 7741, 4.1).
const AUDIO_RATE: u32 = 48_000;
const VIDEO_RATE: u32 = 90_000;
// The most bytes of a video packet libwebrtc sends (media_constants.h:25, `kVideoMtu`), less the two an RTX packet adds for the original sequence number (rtp_sender.cc:560, `RtxPacketOverhead`; RFC 4588, 4).
const MOST_PACKET: usize = 1200;
const RTX_MORE: usize = 2;
// RTCP's schedule: reports each second with video and each 5 s for audio alone (video_send_stream.h:184; audio_send_stream.h:120, which RingRTC keeps, common/mod.rs:882), the first after half of one and each spread over half to one and a half (rtcp_sender.cc:175, 712); transport-wide feedback each 100 ms (transport_sequence_number_feedback_generator.cc:40); NACKs looked at each 20 ms, with 100 ms of round trip until one is known (nack_requester.h:44, nack_requester.cc:36); goog_cc's process interval of 25 ms (goog_cc_factory.cc:114-116).
const VIDEO_REPORTS: Duration = Duration::from_secs(1);
const AUDIO_REPORTS: Duration = Duration::from_secs(5);
const SPREAD_STEPS: u32 = 1000;
const FEEDBACK_INTERVAL: Duration = Duration::from_millis(100);
const NACK_INTERVAL: Duration = Duration::from_millis(20);
const DEFAULT_ROUND_TRIP: Duration = Duration::from_millis(100);
const TICK: Duration = Duration::from_millis(25);
// RFC 3550, 6.4.1: the report blocks one report carries.
const MOST_BLOCKS: usize = 31;
// RingRTC's data streams count their sequence numbers and timestamps from 1 (connection.rs:518, 1568; group_call.rs:1449-1450).
const FIRST_DATA: u32 = 1;
// RFC 7741, 4.2: a picture id of 15 bits, which libwebrtc begins at random (rtp_payload_params.cc:260).
const PICTURE_BITS: u16 = 0x7FFF;
const BITS_PER_KILOBIT: u64 = 1000;

// A frame the encoder gave, for this side to send.
#[derive(Clone, Copy)]
pub(crate) struct Encoded<'a> {
    // As the encoder wrote it, or a group call's frame encryption after it.
    pub(crate) bytes: &'a [u8],
    pub(crate) is_key: bool,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) frame_rate: u8,
    pub(crate) sampled: Duration,
    pub(crate) orientation: Orientation,
}

// What a datagram that came turns into.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Taken {
    Nothing,
    // An Opus packet by its extended sequence number and its RTP timestamp, and the level a group call's sender told.
    Audio { from: u32, sequence: u64, timestamp: u32, level: Option<AudioLevel>, payload: Vec<u8> },
    Video { from: u32, frames: Vec<Frame> },
    // A data message with its RTP timestamp, by which a group call tells a newer from an older.
    Data { from: u32, timestamp: u32, payload: Vec<u8> },
    // A message of the calling server itself.
    Server { payload: Vec<u8> },
    // The far side asks for a key frame.
    KeyFrame,
}

// Told by its kind, sender and sizes, never the sound, pictures or messages it carries.
impl core::fmt::Debug for Taken {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Taken::Nothing => formatter.write_str("Nothing"),
            Taken::Audio { from, sequence, timestamp, level, payload } => formatter
                .debug_struct("Audio")
                .field("from", from)
                .field("sequence", sequence)
                .field("timestamp", timestamp)
                .field("level", level)
                .field("bytes", &payload.len())
                .finish(),
            Taken::Video { from, frames } => {
                formatter.debug_struct("Video").field("from", from).field("frames", &frames.len()).finish()
            }
            Taken::Data { from, payload, .. } => {
                formatter.debug_struct("Data").field("from", from).field("bytes", &payload.len()).finish()
            }
            Taken::Server { payload } => formatter.debug_struct("Server").field("bytes", &payload.len()).finish(),
            Taken::KeyFrame => formatter.write_str("KeyFrame"),
        }
    }
}

// A stream of the far side's that came: its reports' state, and a video stream's missing packets and assembler.
struct Remote {
    ssrc: u32,
    from: u32,
    receiver: Receiver,
    missing: Missing,
    assembler: Option<Assembler>,
}

// A call's media over its one transport as RTP, RTCP and SRTP: what this side sends, paced at the rate transport-wide feedback allows, what it receives, put together, and the RTCP each side owes; ICE, keys, signalling and frame encryption are the caller's.
pub(crate) struct Session {
    kind: Kind,
    own: Own,
    outbound: Outbound,
    inbound: Inbound,
    audio: Sender,
    video: Sender,
    video_rtx: Sender,
    data: [u32; 2],
    has_video: bool,
    history: History,
    departures: Departures,
    estimator: Estimator,
    pacer: Pacer,
    recorder: Recorder,
    feedback_media: u32,
    remotes: Vec<Remote>,
    picture: u16,
    frame_number: u16,
    structure: Option<Structure>,
    allocation: Option<u32>,
    turn: Option<Orientation>,
    offsets: [u32; 2],
    random: u64,
    round_trip: Option<Duration>,
    next_report: Option<Duration>,
    next_feedback: Duration,
    next_nack: Duration,
    next_tick: Duration,
    out: VecDeque<Vec<u8>>,
}

impl Session {
    // A call's media of a kind under the SRTP masters of what this side sends and of what it receives, taking at most `most` streams of the far side, at a rate within the bounds; `random`, from random bytes, picks the first sequence numbers, picture id, frame number and timestamps, which RTP has begin at random (RFC 3550, 5.1). None where the library fails.
    pub(crate) fn new(
        kind: Kind,
        crypto: &impl Crypto,
        sending: &Master,
        receiving: &Master,
        most: usize,
        bounds: Bounds,
        random: u64,
    ) -> Option<Session> {
        let own = kind.own();
        let mut state = random;
        let mut next = || {
            state = xorshift64(state);
            state
        };
        let estimator = Estimator::new(bounds);
        let mut pacer = Pacer::default();
        pacer.set_rate(estimator.pacing_rate());
        Some(Session {
            kind,
            own,
            outbound: Outbound::new(crypto, sending)?,
            inbound: Inbound::new(crypto, receiving, most)?,
            audio: Sender::new(own.audio, AUDIO_RATE, next() as u16),
            video: Sender::new(own.video, VIDEO_RATE, next() as u16),
            video_rtx: Sender::new(own.video_rtx, VIDEO_RATE, next() as u16),
            data: [FIRST_DATA; 2],
            has_video: false,
            history: History::default(),
            departures: Departures::default(),
            estimator,
            pacer,
            recorder: Recorder::default(),
            feedback_media: 0,
            remotes: Vec::new(),
            picture: next() as u16 & PICTURE_BITS,
            frame_number: next() as u16,
            structure: None,
            allocation: None,
            turn: None,
            offsets: [next() as u32, next() as u32],
            random: next(),
            round_trip: None,
            next_report: None,
            next_feedback: Duration::ZERO,
            next_nack: Duration::ZERO,
            next_tick: Duration::ZERO,
            out: VecDeque::new(),
        })
    }

    // Takes the least and most rate anew, as the call's data mode changes them.
    pub(crate) fn set_bounds(&mut self, least: u64, most: u64) {
        self.estimator.set_bounds(least, most);
        self.pacer.set_rate(self.estimator.pacing_rate());
    }

    // The bits a second the video encoder may spend: the whole target, since audio carries no transport-wide number in Signal's calls (peer_connection.cc:677-681, group_call_sdp_builder.cc:205-209) and libwebrtc then leaves it out of the allocation and the pacer's count (audio_send_stream.cc:367-377).
    pub(crate) fn video_rate(&self) -> u64 {
        self.estimator.target()
    }

    // Queues an Opus packet sampled at a time with its level, which a group call's server reads (group_call_sdp_builder.cc:209); the stream's first packet carries the marker (RFC 7587, 4.1).
    pub(crate) fn send_audio(&mut self, opus: &[u8], sampled: Duration, level: AudioLevel, now: Duration) {
        let timestamp = units(sampled, AUDIO_RATE).wrapping_add(self.offsets[0]);
        let level = [Element::AudioLevel(level)];
        let is_group = matches!(self.kind, Kind::Group { .. });
        let header = Header {
            marker: self.audio.info(now).is_none(),
            payload_type: layout::OPUS,
            sequence: self.audio.next_sequence(),
            timestamp,
            ssrc: self.own.audio,
            csrcs: &[],
            extensions: if is_group { &level } else { &[] },
        };
        let mut packet = Vec::new();
        if header.write(&mut packet) {
            packet.extend_from_slice(opus);
            self.audio.note_sent(opus.len(), timestamp, sampled);
            self.pacer.push(Class::Unpaced, packet, now);
        }
    }

    // Queues a data message, to the calling server itself where `is_to_server` in a group call, else to the far side or the members through it, its sequence number and timestamp one counter (connection.rs:1559-1586; group_call.rs:4414-4437); RingRTC sends it unpaced.
    pub(crate) fn send_data(&mut self, payload: &[u8], is_to_server: bool, now: Duration) {
        let is_server = is_to_server && matches!(self.kind, Kind::Group { .. });
        let (ssrc, counter) = if is_server { (layout::SERVER, 1) } else { (self.own.data, 0) };
        let number = self.data[counter];
        self.data[counter] = number.wrapping_add(1);
        let header = Header {
            marker: false,
            payload_type: layout::DATA,
            sequence: number as u16,
            timestamp: number,
            ssrc,
            csrcs: &[],
            extensions: &[],
        };
        let mut packet = Vec::new();
        if header.write(&mut packet) {
            packet.extend_from_slice(payload);
            self.pacer.push(Class::Unpaced, packet, now);
        }
    }

    // Cuts a VP8 frame into packets and queues them, as libwebrtc's sender does (rtp_sender_video.cc:606-830); false where the pacer holds more than it drains in its limit, which drops the frame (pacing_controller.h:99-104), or the frame fits no packet. A one-to-one call's packets carry a picture id of 15 bits; a group call's carry the descriptor's one byte (rtp_sender_video.cc:91-102), the dependency descriptor on every packet with the structure on a key frame's first, and the layers allocation on a key frame's first and where the rate changed (rtp_sender_video.cc:424-513, 588-591); the orientation goes on the last packet of a key frame, of a frame turned anew or turned at all (rtp_sender_video.cc:397-402).
    pub(crate) fn send_video(&mut self, frame: Encoded<'_>, now: Duration) -> bool {
        if frame.bytes.is_empty() || self.pacer.queue_time(now) > QUEUE_LIMIT {
            return false;
        }
        let timestamp = units(frame.sampled, VIDEO_RATE).wrapping_add(self.offsets[1]);
        let has_turn = frame.is_key || self.turn != Some(frame.orientation) || frame.orientation.turn != Turn::None;
        let is_group = matches!(self.kind, Kind::Group { .. });
        let descriptor = vp8::Descriptor {
            is_start: true,
            picture: (!is_group).then_some(Picture { id: self.picture, is_long: true }),
            ..vp8::Descriptor::default()
        };
        let kbps = u32::try_from(self.video_rate() / BITS_PER_KILOBIT).unwrap_or(u32::MAX);
        let mut allocation = Vec::new();
        let mut dependency = None;
        if is_group {
            if frame.is_key {
                let offset = match &self.structure {
                    Some(structure) if structure.resolutions == [(frame.width, frame.height)] => structure.offset,
                    Some(structure) => structure.next_offset(),
                    None => 0,
                };
                self.structure = Some(Structure::single(offset, frame.width, frame.height));
            }
            let Some(structure) = self.structure.clone() else {
                return false;
            };
            if frame.is_key || self.allocation != Some(kbps) {
                let single = Allocation::single(kbps, frame.width, frame.height, frame.frame_rate);
                if !single.write(&mut allocation) {
                    return false;
                }
            }
            let fdiffs = if frame.is_key { Vec::new() } else { vec![1] };
            let info = Dependent { dtis: vec![Dti::Switch], fdiffs, ..Dependent::default() };
            let Some(mut chosen) = Dependency::of(&structure, &info, 0, frame.is_key, None) else {
                return false;
            };
            chosen.frame = self.frame_number;
            dependency = Some((chosen, structure));
        }
        let ssrc = self.own.video;
        let header = |is_first: bool, is_last: bool, sequence: u16, out: &mut Vec<u8>| -> bool {
            let mut written = Vec::new();
            let mut elements = vec![Element::TransportSequence(0)];
            if is_last && has_turn {
                elements.push(Element::Orientation(frame.orientation));
            }
            if let Some((chosen, structure)) = &dependency {
                let mut packet = chosen.clone();
                (packet.is_start, packet.is_end) = (is_first, is_last);
                if !is_first {
                    packet.structure = None;
                }
                if !packet.write(structure, &mut written) {
                    return false;
                }
                elements.push(Element::Other { id: DEPENDENCY_DESCRIPTOR, data: &written });
            }
            if is_first && !allocation.is_empty() {
                elements.push(Element::Other { id: LAYERS_ALLOCATION, data: &allocation });
            }
            let header = Header {
                marker: is_last,
                payload_type: layout::VP8,
                sequence,
                timestamp,
                ssrc,
                csrcs: &[],
                extensions: &elements,
            };
            header.write(out)
        };
        let mut sizes = [0; 4];
        for (size, (is_first, is_last)) in
            sizes.iter_mut().zip([(false, false), (true, false), (false, true), (true, true)])
        {
            let mut out = Vec::new();
            if !header(is_first, is_last, 0, &mut out) {
                return false;
            }
            *size = out.len();
        }
        let [middle, first, last, single] = sizes;
        let budget = Budget {
            bytes: (MOST_PACKET - RTX_MORE).saturating_sub(middle + descriptor.len()),
            first_less: first - middle,
            last_less: last - middle,
            single_less: single - middle,
        };
        let pieces = vp8::split(frame.bytes.len(), budget);
        if pieces.is_empty() {
            return false;
        }
        let class = if frame.is_key { Class::KeyFrame } else { Class::Video };
        let mut rest = frame.bytes;
        for (at, &size) in pieces.iter().enumerate() {
            let (is_first, is_last) = (at == 0, at + 1 == pieces.len());
            let (piece, after) = rest.split_at(size);
            rest = after;
            let mut packet = Vec::new();
            if !header(is_first, is_last, self.video.next_sequence(), &mut packet) {
                return false;
            }
            vp8::Descriptor { is_start: is_first, ..descriptor }.write(&mut packet);
            packet.extend_from_slice(piece);
            self.video.note_sent(descriptor.len() + piece.len(), timestamp, frame.sampled);
            self.pacer.push(class, packet, now);
        }
        if is_group {
            self.allocation = Some(kbps);
        }
        self.has_video = true;
        self.turn = Some(frame.orientation);
        self.picture = self.picture.wrapping_add(1) & PICTURE_BITS;
        self.frame_number = self.frame_number.wrapping_add(1);
        true
    }

    // Marks a key frame wanted of a sender, as where a frame of its did not decode.
    pub(crate) fn want_key(&mut self, from: u32) {
        let video = self.remotes.iter_mut().filter(|remote| remote.from == from);
        video.filter_map(|remote| remote.assembler.as_mut()).for_each(Assembler::want_key);
    }

    // The next datagram to send now, protected: RTCP that is due, then what the pacer lets go; none until `next_time`.
    pub(crate) fn poll(&mut self, crypto: &impl Crypto, now: Duration) -> Option<Vec<u8>> {
        self.run(crypto, now);
        if let Some(datagram) = self.out.pop_front() {
            return Some(datagram);
        }
        while let Some((class, mut packet)) = self.pacer.pop(now) {
            if matches!(class, Class::Video | Class::KeyFrame) {
                self.history.keep(&packet, now);
            }
            let bytes = packet.len() + TAG_BYTES;
            if self.departures.send(&mut packet, bytes, now) {
                self.estimator.note_sent(now);
            }
            if self.outbound.protect(crypto, &mut packet) {
                return Some(packet);
            }
        }
        None
    }

    // When `poll` has something to give next; a pass with nothing to do asks for no look: the estimator's goes by the numbered packets this side sent, which are its video's, the feedback's by the arrivals kept, the NACKs' by the far side's video.
    pub(crate) fn next_time(&self, now: Duration) -> Duration {
        if !self.out.is_empty() {
            return now;
        }
        let tick = self.has_video.then_some(self.next_tick);
        let feedback = self.recorder.has_arrivals().then_some(self.next_feedback);
        let nack = self.remotes.iter().any(|remote| remote.assembler.is_some()).then_some(self.next_nack);
        let schedule = [tick, self.next_report, feedback, nack];
        let next = schedule.into_iter().flatten().chain(self.pacer.next_time(now)).min().unwrap_or(now);
        next.max(now)
    }

    // Takes a datagram that came on the call's transport.
    pub(crate) fn take(&mut self, crypto: &impl Crypto, datagram: &mut [u8], now: Duration) -> Taken {
        match demux::kind_of(datagram) {
            Some(Arrival::Rtp { ssrc, payload_type }) => self.take_rtp(crypto, datagram, ssrc, payload_type, now),
            Some(Arrival::Rtcp) => self.take_rtcp(crypto, datagram, now),
            None => Taken::Nothing,
        }
    }

    // An RTP packet: every one that carries a transport-wide number is fed back, padding and what this side drops among them (rtp.rs:418-422, 448-450); RTX is unwrapped, and RED down to its primary block, ULPFEC dropped.
    fn take_rtp(
        &mut self,
        crypto: &impl Crypto,
        datagram: &mut [u8],
        ssrc: u32,
        payload_type: u8,
        now: Duration,
    ) -> Taken {
        let Some(opened) = self.inbound.unprotect(crypto, datagram) else {
            return Taken::Nothing;
        };
        let Some(number) = Packet::read(opened).map(|read| read.extensions.transport_sequence()) else {
            return Taken::Nothing;
        };
        if let Some(number) = number {
            self.recorder.note(number, now);
            self.feedback_media = ssrc;
        }
        let (route, packet) = match self.kind.route(ssrc, payload_type) {
            Some(Route::Rtx { ssrc: original, payload_type: original_type }) => {
                let Some(unwrapped) = rtx::unwrap(opened, original, original_type) else {
                    return Taken::Nothing;
                };
                (self.kind.route(original, original_type), unwrapped)
            }
            route => (route, opened),
        };
        let Some(read) = Packet::read(packet) else {
            return Taken::Nothing;
        };
        match route {
            Some(Route::Audio { from }) => {
                let receiver = &mut self.remote(read.ssrc, from, None).receiver;
                let Some(sequence) = receiver.receive(read.sequence, read.timestamp, now) else {
                    return Taken::Nothing;
                };
                Taken::Audio {
                    from,
                    sequence,
                    timestamp: read.timestamp,
                    level: read.extensions.audio_level(),
                    payload: read.payload.to_vec(),
                }
            }
            Some(Route::Video { from, codec, is_red }) => {
                let remote = self.remote(read.ssrc, from, Some(codec));
                // The reports' count begins anew at a jump (RFC 3550, A.1); what is missing goes by the packets' own numbers.
                remote.receiver.receive(read.sequence, read.timestamp, now);
                remote.missing.receive(read.sequence);
                let payload = match (is_red, red::primary(read.payload)) {
                    (false, _) => read.payload,
                    (true, Some((layout::VP8, primary))) => primary,
                    (true, _) => return Taken::Nothing,
                };
                let incoming = Incoming {
                    sequence: read.sequence,
                    timestamp: read.timestamp,
                    marker: read.marker,
                    dependency: read.extensions.get(DEPENDENCY_DESCRIPTOR),
                    orientation: read.extensions.orientation(),
                    payload,
                };
                let frames =
                    remote.assembler.as_mut().map(|assembler| assembler.take(incoming, now)).unwrap_or_default();
                if frames.is_empty() { Taken::Nothing } else { Taken::Video { from, frames } }
            }
            Some(Route::Data { from }) => {
                Taken::Data { from, timestamp: read.timestamp, payload: read.payload.to_vec() }
            }
            Some(Route::Server) => Taken::Server { payload: read.payload.to_vec() },
            Some(Route::Rtx { .. }) | None => Taken::Nothing,
        }
    }

    // A far side's stream by its SSRC, made as its first packet opens; the SRTP streams bound how many.
    fn remote(&mut self, ssrc: u32, from: u32, codec: Option<Codec>) -> &mut Remote {
        let at = self.remotes.iter().position(|remote| remote.ssrc == ssrc).unwrap_or_else(|| {
            let rate = if codec.is_some() { VIDEO_RATE } else { AUDIO_RATE };
            let assembler = codec.map(Assembler::new);
            self.remotes.push(Remote {
                ssrc,
                from,
                receiver: Receiver::new(ssrc, rate),
                missing: Missing::default(),
                assembler,
            });
            self.remotes.len() - 1
        });
        &mut self.remotes[at]
    }

    // An RTCP compound: reports, a NACK, a PLI or FIR of this side's video, transport-wide feedback and a REMB.
    fn take_rtcp(&mut self, crypto: &impl Crypto, datagram: &mut [u8], now: Duration) -> Taken {
        let Some(compound) = self.inbound.unprotect_rtcp(crypto, datagram) else {
            return Taken::Nothing;
        };
        let mut is_key_wanted = false;
        for packet in rtcp::read(compound) {
            match packet {
                Control::SenderReport(report) => {
                    if let Some(remote) = self.remotes.iter_mut().find(|remote| remote.ssrc == report.ssrc) {
                        remote.receiver.note_report(report.info.ntp, now);
                    }
                    report.blocks().for_each(|block| self.note_block(block, now));
                }
                Control::ReceiverReport(report) => report.blocks().for_each(|block| self.note_block(block, now)),
                Control::Nack(nack) if nack.media == self.own.video => {
                    let round_trip = self.round_trip.unwrap_or(DEFAULT_ROUND_TRIP);
                    for sequence in nack.lost() {
                        let mut out = Vec::new();
                        if self.history.resend_rtx(
                            sequence,
                            now,
                            round_trip,
                            &mut self.video_rtx,
                            layout::VP8_RTX,
                            &mut out,
                        ) {
                            self.pacer.push(Class::Retransmission, out, now);
                        }
                    }
                }
                Control::Pli(pli) => is_key_wanted |= pli.media == self.own.video,
                Control::Fir(fir) => is_key_wanted |= fir.entries().any(|(ssrc, _)| ssrc == self.own.video),
                Control::TransportFeedback(feedback) => {
                    let outcomes = self.departures.outcomes(&feedback);
                    self.estimator.take(&outcomes, now);
                    self.pacer.set_rate(self.estimator.pacing_rate());
                }
                Control::Remb(remb) => self.estimator.limit(remb.bitrate),
                _ => {}
            }
        }
        if is_key_wanted { Taken::KeyFrame } else { Taken::Nothing }
    }

    // A report block of this side's stream gives the round trip (RFC 3550, 6.4.1).
    fn note_block(&mut self, block: ReportBlock, now: Duration) {
        if block.ssrc != self.own.audio && block.ssrc != self.own.video {
            return;
        }
        if let Some(round_trip) = block.round_trip(now).filter(|round_trip| !round_trip.is_zero()) {
            self.round_trip = Some(round_trip);
            self.estimator.note_round_trip(round_trip);
        }
    }

    // What is due on the clock: the estimator's interval, reports, feedback, NACKs and PLIs, each RTCP datagram protected into `out`.
    fn run(&mut self, crypto: &impl Crypto, now: Duration) {
        if now >= self.next_tick {
            self.next_tick = now + TICK;
            self.estimator.tick(now);
            self.pacer.set_rate(self.estimator.pacing_rate());
        }
        let has_video = self.has_video || self.remotes.iter().any(|remote| remote.assembler.is_some());
        let interval = if has_video { VIDEO_REPORTS } else { AUDIO_REPORTS };
        let next_report = *self.next_report.get_or_insert(now + interval / 2);
        let mut compounds = Vec::new();
        if now >= next_report {
            self.random = xorshift64(self.random);
            let spread = SPREAD_STEPS / 2 + (self.random % u64::from(SPREAD_STEPS)) as u32;
            self.next_report = Some(now + interval * spread / SPREAD_STEPS);
            compounds.push(self.reports(now));
        }
        if now >= self.next_feedback {
            self.next_feedback = now + FEEDBACK_INTERVAL;
            let mut out = Vec::new();
            while self.recorder.write(&mut out, self.own.video, self.feedback_media) {
                compounds.push(core::mem::take(&mut out));
            }
        }
        if now >= self.next_nack {
            self.next_nack = now + NACK_INTERVAL;
            let round_trip = self.round_trip.unwrap_or(DEFAULT_ROUND_TRIP);
            for remote in self.remotes.iter_mut() {
                let Some(assembler) = remote.assembler.as_mut() else {
                    continue;
                };
                let mut out = Vec::new();
                if write_nack(&mut out, self.own.video, remote.ssrc, &remote.missing.due(now, round_trip)) {
                    compounds.push(core::mem::take(&mut out));
                }
                if assembler.needs_key(now) && write_pli(&mut out, self.own.video, remote.ssrc) {
                    compounds.push(out);
                }
            }
        }
        for mut compound in compounds.into_iter().filter(|compound| !compound.is_empty()) {
            if self.outbound.protect_rtcp(crypto, &mut compound) {
                self.out.push_back(compound);
            }
        }
    }

    // A sender report of each stream this side sent, then the report blocks of every stream received, as many receiver reports as they fill.
    fn reports(&mut self, now: Duration) -> Vec<u8> {
        let mut out = Vec::new();
        for sender in [&self.audio, &self.video] {
            if let Some(info) = sender.info(now) {
                write_sender_report(&mut out, sender.ssrc, &info, &[]);
            }
        }
        let blocks: Vec<ReportBlock> =
            self.remotes.iter_mut().filter_map(|remote| remote.receiver.block(now)).collect();
        for chunk in blocks.chunks(MOST_BLOCKS) {
            write_receiver_report(&mut out, self.own.audio, chunk);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::ivf;
    use crate::codec::vp8::is_key_frame;
    use crate::codec::vp8::tests::{CALL, CALL_CRCS, decode_frames};
    use crate::net::rtp::srtp::SALT_BYTES;
    use crate::net::wireguard::tests::Toy;

    const BOUNDS: Bounds = Bounds { least: 30_000, start: 300_000, most: 2_000_000 };
    // A link of 20 ms each way, run a millisecond at a time; a frame each 33 ms.
    const DELAY: Duration = Duration::from_millis(20);
    const STEP: Duration = Duration::from_millis(1);
    const FRAME: Duration = Duration::from_millis(33);
    const UPRIGHT: Orientation = Orientation { turn: Turn::None, is_flipped: false, is_back: false };
    const LEVEL: AudioLevel = AudioLevel { level: 30, is_voice: true };

    // The two sides of a call over the link, and what each took.
    struct Call {
        sides: [Session; 2],
        crypto: Toy,
        now: Duration,
        wire: VecDeque<(Duration, usize, Vec<u8>)>,
        taken: [Vec<Taken>; 2],
        sent: Vec<(usize, Vec<u8>)>,
    }

    fn masters() -> [Master; 2] {
        [Master::new(&[1; 16], &[2; SALT_BYTES]).unwrap(), Master::new(&[3; 16], &[4; SALT_BYTES]).unwrap()]
    }

    fn call(kinds: [Kind; 2]) -> Call {
        let (crypto, [first, second]) = (Toy::default(), masters());
        let sides = [
            Session::new(kinds[0], &crypto, &first, &second, 16, BOUNDS, 11).unwrap(),
            Session::new(kinds[1], &crypto, &second, &first, 16, BOUNDS, 22).unwrap(),
        ];
        Call {
            sides,
            crypto,
            now: Duration::ZERO,
            wire: VecDeque::new(),
            taken: [Vec::new(), Vec::new()],
            sent: Vec::new(),
        }
    }

    impl Call {
        // Runs both sides for a span: each sends what is due when its next time comes, which reaches the other 20 ms on unless `is_lost` drops it.
        fn run(&mut self, span: Duration, is_lost: &mut dyn FnMut(usize, &[u8]) -> bool) {
            let until = self.now + span;
            while self.now < until {
                for side in 0..2 {
                    if self.sides[side].next_time(self.now) > self.now {
                        continue;
                    }
                    while let Some(datagram) = self.sides[side].poll(&self.crypto, self.now) {
                        self.sent.push((side, datagram.clone()));
                        if !is_lost(side, &datagram) {
                            self.wire.push_back((self.now + DELAY, 1 - side, datagram));
                        }
                    }
                }
                while let Some((_, to, mut datagram)) = self.wire.pop_front_if(|(at, _, _)| *at <= self.now) {
                    let taken = self.sides[to].take(&self.crypto, &mut datagram, self.now);
                    if taken != Taken::Nothing {
                        self.taken[to].push(taken);
                    }
                }
                self.now += STEP;
            }
        }

        // Side 0 sends a frame now and runs a frame's time.
        fn frame(&mut self, bytes: &[u8], is_lost: &mut dyn FnMut(usize, &[u8]) -> bool) {
            let encoded = Encoded {
                bytes,
                is_key: is_key_frame(bytes),
                width: 99,
                height: 61,
                frame_rate: 30,
                sampled: self.now,
                orientation: UPRIGHT,
            };
            assert!(self.sides[0].send_video(encoded, self.now), "the frame is queued");
            self.run(FRAME, is_lost);
        }

        fn frames(&self, side: usize) -> Vec<Frame> {
            let taken = self.taken[side].iter().filter_map(|taken| match taken {
                Taken::Video { frames, .. } => Some(frames.clone()),
                _ => None,
            });
            taken.flatten().collect()
        }
    }

    fn none(_: usize, _: &[u8]) -> bool {
        false
    }

    fn decoded(frames: &[Frame]) -> Vec<u32> {
        let joined: Vec<Vec<u8>> = frames.iter().map(Frame::join).collect();
        decode_frames(joined.iter().map(Vec::as_slice)).unwrap()
    }

    fn audio_and_video(kinds: [Kind; 2], from: u32) {
        let mut call = call(kinds);
        let frames = ivf::frames(CALL);
        for (number, frame) in frames.iter().enumerate() {
            if number % 2 == 0 {
                call.sides[0].send_audio(&[number as u8; 80], call.now, LEVEL, call.now);
            }
            call.frame(frame, &mut none);
        }
        call.sides[0].send_data(b"hello", false, call.now);
        call.run(Duration::from_secs(2), &mut none);
        let joined: Vec<Vec<u8>> = call.frames(1).iter().map(Frame::join).collect();
        assert_eq!(joined, frames, "{kinds:?}: each frame whole, in order, byte for byte");
        assert_eq!(decoded(&call.frames(1)), CALL_CRCS, "{kinds:?}: and decoded to ffmpeg's pictures");
        let audio: Vec<(u32, Option<AudioLevel>, Vec<u8>)> = call.taken[1]
            .iter()
            .filter_map(|taken| match taken {
                Taken::Audio { from, level, payload, .. } => Some((*from, *level, payload.clone())),
                _ => None,
            })
            .collect();
        let level = matches!(kinds[0], Kind::Group { .. }).then_some(LEVEL);
        let expected: Vec<_> =
            (0..frames.len()).step_by(2).map(|number| (from, level, vec![number as u8; 80])).collect();
        assert_eq!(audio, expected, "{kinds:?}: each Opus packet, by its sender, with the level a group call tells");
        assert!(
            call.taken[1].contains(&Taken::Data { from, timestamp: FIRST_DATA, payload: b"hello".to_vec() }),
            "{kinds:?}: the data message, the first of its count"
        );
        let sender = &call.sides[0];
        assert!(
            sender.estimator.pacing_rate() < sender.video_rate() * 2,
            "{kinds:?}: feedback came back and was taken, which brings the pacer from 2.5 times the target to 1.1"
        );
        let round_trip = sender.round_trip.unwrap_or_default();
        assert!(
            round_trip >= DELAY * 2 && round_trip <= DELAY * 2 + Duration::from_millis(2),
            "{kinds:?}: the round trip from the far side's report: {round_trip:?}"
        );
        let receiver = &call.sides[1];
        assert!(receiver.round_trip.is_none(), "{kinds:?}: a side that only receives hears no report of its own");
    }

    #[test]
    fn direct_call_media() {
        audio_and_video([Kind::Direct { is_caller: true }, Kind::Direct { is_caller: false }], 1000);
    }

    #[test]
    fn key_frame_wanted_and_bounds() {
        let mut call = call([Kind::Direct { is_caller: true }, Kind::Direct { is_caller: false }]);
        let frames = ivf::frames(CALL);
        for frame in &frames[..3] {
            call.frame(frame, &mut none);
        }
        assert!(!call.taken[0].contains(&Taken::KeyFrame), "frames that decode ask for nothing");
        call.sides[1].want_key(1000);
        call.run(Duration::from_millis(300), &mut none);
        assert!(
            call.taken[0].contains(&Taken::KeyFrame),
            "a frame that did not decode has a PLI sent, once 200 ms passed since the last key frame came"
        );
        call.sides[0].set_bounds(400_000, 500_000);
        assert_eq!(call.sides[0].video_rate(), 400_000, "new bounds hold the rate");
        let now = call.now;
        assert!(call.sides[0].next_time(now) <= now + TICK, "the estimator's interval comes at least each 25 ms");
    }

    #[test]
    fn group_call_media() {
        audio_and_video([Kind::Group { demux: 0x1230 }, Kind::Group { demux: 0x4560 }], 0x1230);
    }

    // The RTP payload type and timestamp of a datagram, read in its clear header.
    fn kind(datagram: &[u8]) -> Option<(u8, u32)> {
        Packet::read(datagram).map(|read| (read.payload_type, read.timestamp))
    }

    #[test]
    fn lost_packet_resent() {
        let mut call = call([Kind::Direct { is_caller: true }, Kind::Direct { is_caller: false }]);
        let frames = ivf::frames(CALL);
        let mut video = 0;
        let mut is_lost = |side: usize, datagram: &[u8]| {
            let is_video =
                side == 0 && demux::kind_of(datagram) == Some(Arrival::Rtp { ssrc: 1003, payload_type: layout::VP8 });
            video += usize::from(is_video);
            is_video && video == 3
        };
        for frame in &frames {
            call.frame(frame, &mut is_lost);
        }
        call.run(Duration::from_secs(1), &mut is_lost);
        let resent = call
            .sent
            .iter()
            .filter(|(side, datagram)| *side == 0 && kind(datagram).is_some_and(|(kind, _)| kind == layout::VP8_RTX));
        assert_eq!(resent.count(), 1, "the packet lost is asked for by a NACK and sent again as RTX");
        assert_eq!(decoded(&call.frames(1)), CALL_CRCS, "every frame decodes, the one it completes among them");
    }

    #[test]
    fn pli_after_loss() {
        let mut call = call([Kind::Direct { is_caller: true }, Kind::Direct { is_caller: false }]);
        let frames = ivf::frames(CALL);
        let mut lost_timestamp = None;
        let mut seen = 0;
        let mut is_lost = |side: usize, datagram: &[u8]| {
            let Some((kind, timestamp)) = kind(datagram).filter(|_| side == 0) else {
                return false;
            };
            if kind == layout::VP8 {
                seen += 1;
                if seen == 14 {
                    lost_timestamp = Some(timestamp);
                }
            }
            (kind == layout::VP8 || kind == layout::VP8_RTX) && lost_timestamp == Some(timestamp)
        };
        let mut number = 0;
        while !call.taken[0].contains(&Taken::KeyFrame) && call.now < Duration::from_secs(10) {
            let frame = if number < frames.len() { frames[number] } else { frames[1 + number % (frames.len() - 1)] };
            call.frame(frame, &mut is_lost);
            number += 1;
        }
        let asked = call.now;
        assert!(asked < Duration::from_secs(10), "a PLI came back");
        let handed = call.frames(1).len();
        assert!(handed < number, "frames stopped at the one lost for good: {handed} of {number}");
        for frame in &frames[..3] {
            call.frame(frame, &mut is_lost);
        }
        call.run(Duration::from_millis(200), &mut is_lost);
        let after = &call.frames(1)[handed..];
        assert_eq!(after.len(), 3, "the key frame sent on the PLI and the two after it");
        let crcs = decoded(&call.frames(1));
        assert_eq!(crcs[crcs.len() - 3..], CALL_CRCS[..3], "the stream decodes again from the key frame");
    }

    #[test]
    fn nacks_past_a_late_pair() {
        let (crypto, [first, _]) = (Toy::default(), masters());
        let mut callee = call([Kind::Direct { is_caller: true }, Kind::Direct { is_caller: false }])
            .sides
            .into_iter()
            .nth(1)
            .unwrap();
        let mut far = Outbound::new(&crypto, &first).unwrap();
        let mut resent = 0u16;
        let mut take = |sequence: u16, is_resent: bool| {
            let mut payload = if is_resent { sequence.to_be_bytes().to_vec() } else { Vec::new() };
            vp8::Descriptor { is_start: true, ..vp8::Descriptor::default() }.write(&mut payload);
            payload.extend_from_slice(b"frame");
            resent += u16::from(is_resent);
            let (payload_type, ssrc, own) =
                if is_resent { (layout::VP8_RTX, 1013, resent) } else { (layout::VP8, 1003, sequence) };
            let timestamp = u32::from(sequence).wrapping_mul(VIDEO_RATE);
            let header =
                Header { marker: true, payload_type, sequence: own, timestamp, ssrc, csrcs: &[], extensions: &[] };
            let mut packet = Vec::new();
            assert!(header.write(&mut packet));
            packet.extend_from_slice(&payload);
            assert!(far.protect(&crypto, &mut packet));
            callee.take(&crypto, &mut packet, Duration::ZERO);
        };
        for sequence in (0xFFF0..=0xFFFF).chain(0..=150).filter(|sequence| ![20, 21].contains(sequence)) {
            take(sequence, false);
        }
        take(20, true);
        take(21, true);
        for sequence in [151, 152, 155] {
            take(sequence, false);
        }
        assert_eq!(
            callee.remotes[0].missing.due(Duration::ZERO, Duration::ZERO),
            [153, 154],
            "two packets in a row sent again 130 behind begin the reports' count anew (RFC 3550, A.1): they are missing no more, and what is lost after them is"
        );
    }

    // VP8 in RED as libwebrtc sends it where a one-to-one call lists RED and ULPFEC (rtp_sender_video.cc:800-811), a packet lost and resent as RTX of RED, and a ULPFEC packet in RED after the frame.
    #[test]
    fn red_and_its_rtx() {
        let (crypto, [first, _]) = (Toy::default(), masters());
        let mut callee = call([Kind::Direct { is_caller: true }, Kind::Direct { is_caller: false }])
            .sides
            .into_iter()
            .nth(1)
            .unwrap();
        let mut far = Outbound::new(&crypto, &first).unwrap();
        let key = ivf::frames(CALL)[0];
        let pieces = vp8::split(key.len(), Budget { bytes: 900, ..Budget::default() });
        let mut rest = key;
        let mut packets = Vec::new();
        for (at, size) in pieces.iter().enumerate() {
            let (piece, after) = rest.split_at(*size);
            rest = after;
            let mut payload = vec![layout::VP8];
            vp8::Descriptor {
                is_start: at == 0,
                picture: Some(Picture { id: 7, is_long: true }),
                ..vp8::Descriptor::default()
            }
            .write(&mut payload);
            payload.extend_from_slice(piece);
            packets.push((at + 1 == pieces.len(), payload));
        }
        assert!(packets.len() >= 3, "a key frame of several packets");
        let first = 100u16;
        let mut send = |payload_type: u8, ssrc: u32, marker: bool, payload: &[u8], sequence: u16| {
            let header = Header { marker, payload_type, sequence, timestamp: 9000, ssrc, csrcs: &[], extensions: &[] };
            let mut packet = Vec::new();
            assert!(header.write(&mut packet));
            packet.extend_from_slice(payload);
            assert!(far.protect(&crypto, &mut packet));
            callee.take(&crypto, &mut packet, Duration::ZERO)
        };
        for (at, (marker, payload)) in packets.iter().enumerate().filter(|(at, _)| *at != 1) {
            let taken = send(layout::RED, 1003, *marker, payload, first + at as u16);
            assert_eq!(taken, Taken::Nothing, "packet {at} in RED: the frame waits for its second packet");
        }
        let after = first + packets.len() as u16;
        assert_eq!(
            send(layout::RED, 1003, false, &[layout::ULPFEC, 1, 2, 3], after),
            Taken::Nothing,
            "ULPFEC in RED is dropped"
        );
        let rtx = [&(first + 1).to_be_bytes()[..], &packets[1].1].concat();
        let Taken::Video { from, frames } = send(layout::RED_RTX, 1013, false, &rtx, 1) else {
            panic!("the frame, completed by the RTX of RED")
        };
        assert_eq!((from, frames.len(), frames[0].join()), (1000, 1, key.to_vec()), "the key frame whole");
    }
}

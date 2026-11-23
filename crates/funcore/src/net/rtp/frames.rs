use super::dependency::{Descriptor as Dependency, Structure};
use super::extension::Orientation;
use super::{FIRST_CYCLE, extend, extend_bits, vp8, vp9};
use crate::net::window::Window;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::time::Duration;

// libwebrtc's bounds: the packets a stream's buffer keeps (rtp_video_stream_receiver2.cc, `kPacketBufferMaxSize`), the frames that wait for those they depend on and the packets that wait for their structure (rtp_seq_num_only_ref_finder.h, `kMaxStashedFrames`; rtp_video_stream_receiver2.cc, `stashed_packets_`).
const MOST_PACKETS: usize = 2048;
const MOST_WAITING: usize = 100;
const MOST_STASHED: usize = 100;
// The bytes held at most: as many packets as are kept, each of the most a RingRTC sender puts in one (media_constants.h, `kVideoMtu`).
const LARGEST_PACKET: usize = 1200;
const MOST_BYTES: usize = MOST_PACKETS * LARGEST_PACKET;
// video_receive_stream2.h: the time a key frame request waits for its key frame before it is asked again, and the time without a frame to hand after which one is asked for; video_receive_stream2.cc: a stream silent this long asks for none.
const KEY_WAIT: Duration = Duration::from_millis(200);
const FRAME_WAIT: Duration = Duration::from_secs(3);
const SILENCE: Duration = Duration::from_secs(5);
// A VP9 picture's frame ids without a dependency descriptor: room for its spatial layers (vp9_globals.h, `kMaxVp9NumberOfSpatialLayers`).
const LAYER_IDS: u64 = 8;
// RFC 7741, 4.3: the P bit of a frame's first byte, clear on a key frame.
const INTER_FRAME: u8 = 0x01;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Codec {
    Vp8,
    Vp9,
}

// A packet of the stream as it came, RTX and RED taken off: its header's fields, its dependency descriptor and orientation where it carries them, and its payload.
#[derive(Clone, Copy)]
pub(super) struct Incoming<'a> {
    pub(super) sequence: u16,
    pub(super) timestamp: u32,
    pub(super) marker: bool,
    pub(super) dependency: Option<&'a [u8]>,
    pub(super) orientation: Option<Orientation>,
    pub(super) payload: &'a [u8],
}

// A picture handed over: its codec, its layer frames in the order of their spatial layers, each as the sender's encoder or its frame encryption wrote it; what a call shows of someone, it has no `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) codec: Codec,
    pub(crate) is_key: bool,
    layers: Vec<Vec<u8>>,
    pub(crate) orientation: Option<Orientation>,
}

// What tells a stream's frames apart: the dependency descriptor's frame numbers, the payload descriptor's picture ids, or the sequence numbers alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ids {
    Dependency,
    Picture,
    Sequence,
}

// What a frame's first packet tells of it; a stream of sequence numbers alone has its id and references made as the frame completes.
#[derive(Clone, Default, Debug)]
struct Label {
    id: u64,
    refs: Vec<u64>,
    is_key: bool,
}

struct Stored {
    timestamp: u32,
    is_start: bool,
    is_end: bool,
    is_last: bool,
    label: Option<Label>,
    payload: Vec<u8>,
}

struct Waiting {
    timestamp: u32,
    label: Label,
    is_last: bool,
    last: u64,
    bytes: Vec<u8>,
}

struct Stashed {
    sequence: u16,
    timestamp: u32,
    marker: bool,
    dependency: Vec<u8>,
    orientation: Option<Orientation>,
    payload: Vec<u8>,
}

// What a packet's descriptors tell.
struct Parsed<'p> {
    ids: Ids,
    is_start: bool,
    is_end: bool,
    is_last: bool,
    label: Option<Label>,
    media: &'p [u8],
}

// A received video stream's frames put together from its packets: packets in any order, again or never, held until a whole frame stands, and frames handed over by picture once every frame they depend on was. What can never complete is dropped as libwebrtc drops it: what lies before a picture handed over, and all that is held where a bound is passed.
pub(super) struct Assembler {
    codec: Codec,
    ids: Option<Ids>,
    packets: BTreeMap<u64, Stored>,
    waiting: BTreeMap<u64, Waiting>,
    stash: Vec<Stashed>,
    bytes: usize,
    sequence: Option<u64>,
    cleared: Option<u64>,
    frame_number: Option<u64>,
    picture: Option<(u64, u32)>,
    structure: Option<Structure>,
    structure_frame: Option<u64>,
    active: Option<u32>,
    handed: Window,
    last_handed: Option<(u64, u32)>,
    orientation: Option<Orientation>,
    has_key: bool,
    wants_key: bool,
    asked: Option<Duration>,
    heard: Option<Duration>,
    key_heard: Option<Duration>,
    progress: Option<Duration>,
}

impl Incoming<'_> {
    fn stashed(&self, dependency: &[u8]) -> Stashed {
        Stashed {
            sequence: self.sequence,
            timestamp: self.timestamp,
            marker: self.marker,
            dependency: dependency.to_vec(),
            orientation: self.orientation,
            payload: self.payload.to_vec(),
        }
    }
}

impl Stashed {
    fn incoming(&self) -> Incoming<'_> {
        Incoming {
            sequence: self.sequence,
            timestamp: self.timestamp,
            marker: self.marker,
            dependency: Some(&self.dependency),
            orientation: self.orientation,
            payload: &self.payload,
        }
    }
}

impl Frame {
    // The picture as a decoder takes it: its layer frames one after the other, as libwebrtc joins them (frame_helpers.cc, `CombineAndDeleteFrames`) and libvpx's VP9 decoder reads them without a superframe index (vp9_dx_iface.c, `decoder_decode`).
    pub(crate) fn join(&self) -> Vec<u8> {
        self.layers.concat()
    }

    // The picture with each layer frame as `open` gives back what the sender's frame encryption sealed, one by one as libwebrtc opens them before it joins them (rtp_video_stream_receiver2.cc, `OnAssembledFrame`); none where one does not open.
    pub(crate) fn open(self, open: impl FnMut(&[u8]) -> Option<Vec<u8>>) -> Option<Frame> {
        let layers = self.layers.iter().map(Vec::as_slice).map(open).collect::<Option<Vec<_>>>()?;
        Some(Frame { layers, ..self })
    }
}

// Whether a 32-bit RTP timestamp lies after another, half their space either way.
fn is_after(timestamp: u32, other: u32) -> bool {
    (timestamp.wrapping_sub(other) as i32) > 0
}

impl Assembler {
    pub(super) fn new(codec: Codec) -> Assembler {
        Assembler {
            codec,
            ids: None,
            packets: BTreeMap::new(),
            waiting: BTreeMap::new(),
            stash: Vec::new(),
            bytes: 0,
            sequence: None,
            cleared: None,
            frame_number: None,
            picture: None,
            structure: None,
            structure_frame: None,
            active: None,
            handed: Window::default(),
            last_handed: None,
            orientation: None,
            has_key: false,
            wants_key: false,
            asked: None,
            heard: None,
            key_heard: None,
            progress: None,
        }
    }

    // Takes a packet of the stream received now; the pictures it lets be handed over, in their order.
    pub(crate) fn take(&mut self, packet: Incoming<'_>, now: Duration) -> Vec<Frame> {
        self.heard = Some(now);
        self.progress.get_or_insert(now);
        let mut frames = Vec::new();
        if self.insert(packet, now, &mut frames) {
            for stashed in core::mem::take(&mut self.stash) {
                self.insert(stashed.incoming(), now, &mut frames);
            }
        }
        frames
    }

    // Whether to ask the sender for a key frame now, by a PLI: once one is wanted, a frame came that leans on none handed over yet, nothing could be handed over for three seconds while packets came, or the buffers overflowed; asked again each 200 ms while none comes, not while one is arriving (video_receive_stream2.cc, `OnDecodableFrameTimeout`, `HandleKeyFrameGeneration`).
    pub(super) fn needs_key(&mut self, now: Duration) -> bool {
        let is_heard = self.heard.is_some_and(|heard| now.saturating_sub(heard) < SILENCE);
        if is_heard && self.progress.is_some_and(|progress| now.saturating_sub(progress) >= FRAME_WAIT) {
            self.wants_key = true;
        }
        let is_arriving = self.key_heard.is_some_and(|heard| now.saturating_sub(heard) < KEY_WAIT);
        let is_due = self.asked.is_none_or(|asked| now.saturating_sub(asked) >= KEY_WAIT);
        if self.wants_key && is_heard && !is_arriving && is_due {
            self.asked = Some(now);
            return true;
        }
        false
    }

    // Marks a key frame wanted, as where a frame handed over did not decode.
    pub(super) fn want_key(&mut self) {
        self.wants_key = true;
    }

    // Takes a packet in; true where it brought a new dependency structure, by which the stashed packets are read again.
    fn insert(&mut self, packet: Incoming<'_>, now: Duration, frames: &mut Vec<Frame>) -> bool {
        if packet.payload.is_empty() {
            return false;
        }
        if packet.orientation.is_some() {
            self.orientation = packet.orientation;
        }
        let structure_frame = self.structure_frame;
        let Some(parsed) = self.parse(packet) else {
            return false;
        };
        let is_new_structure = self.structure_frame != structure_frame;
        if self.ids.is_some_and(|ids| ids != parsed.ids) {
            self.reset();
        }
        self.ids = Some(parsed.ids);
        if parsed.label.as_ref().is_some_and(|label| label.is_key) {
            self.key_heard = Some(now);
        }
        let Some(at) = self.store(packet.sequence, packet.timestamp, &parsed) else {
            return is_new_structure;
        };
        if let Some(waiting) = self.complete(at) {
            self.wait(waiting);
            self.hand(now, frames);
        }
        is_new_structure
    }

    fn parse<'p>(&mut self, packet: Incoming<'p>) -> Option<Parsed<'p>> {
        if let Some(bytes) = packet.dependency {
            return self.parse_dependency(packet, bytes);
        }
        match self.codec {
            Codec::Vp8 => {
                let (descriptor, media) = vp8::Descriptor::read(packet.payload)?;
                let is_start = descriptor.is_start && descriptor.partition == 0;
                let is_key = is_start && media[0] & INTER_FRAME == 0;
                let (ids, label) = match descriptor.picture {
                    Some(picture) => {
                        let id = self.unwrap_picture(picture);
                        let refs = if is_key { Vec::new() } else { alloc::vec![id - 1] };
                        (Ids::Picture, Label { id, refs, is_key })
                    }
                    None => (Ids::Sequence, Label { is_key, ..Label::default() }),
                };
                let label = is_start.then_some(label);
                Some(Parsed { ids, is_start, is_end: packet.marker, is_last: true, label, media })
            }
            Codec::Vp9 => {
                let (descriptor, media) = vp9::Descriptor::read(packet.payload)?;
                let spatial = descriptor.layer.map_or(0, |layer| layer.spatial);
                let is_key = descriptor.is_key() && spatial == 0;
                let (ids, label) = match descriptor.picture {
                    Some(picture) => {
                        let base = self.unwrap_picture(picture) * LAYER_IDS;
                        let id = base + u64::from(spatial);
                        let mut refs = Vec::new();
                        if descriptor.layer.is_some_and(|layer| layer.is_inter_layer) {
                            refs.push(id.checked_sub(1)?);
                        }
                        if descriptor.is_inter_picture && descriptor.is_flexible {
                            for diff in &descriptor.diffs {
                                refs.push(base.checked_sub(u64::from(*diff) * LAYER_IDS)? + u64::from(spatial));
                            }
                        } else if descriptor.is_inter_picture {
                            refs.push(base.checked_sub(LAYER_IDS)? + u64::from(spatial));
                        }
                        (Ids::Picture, Label { id, refs, is_key })
                    }
                    None => (Ids::Sequence, Label { is_key, ..Label::default() }),
                };
                let label = descriptor.is_begin.then_some(label);
                // RFC 9628, 4.1: the marker bit ends a picture.
                let is_last = packet.marker;
                Some(Parsed { ids, is_start: descriptor.is_begin, is_end: descriptor.is_end, is_last, label, media })
            }
        }
    }

    // A packet by its dependency descriptor, as libwebrtc reads one (rtp_video_stream_receiver2.cc, `ParseGenericDependenciesExtension`): one it cannot read for want of its frame's structure is stashed until that comes where its frame is newer than the structure kept, and asks a key frame where none is kept.
    fn parse_dependency<'p>(&mut self, packet: Incoming<'p>, bytes: &[u8]) -> Option<Parsed<'p>> {
        let Some((descriptor, frame)) = Dependency::read(bytes, self.structure.as_ref()) else {
            let (_, _, _, number) = Dependency::mandatory(bytes)?;
            let id = self.frame_number.map_or(u64::from(number), |highest| extend(number, highest));
            if self.structure_frame.is_none_or(|kept| id > kept) {
                if self.stash.len() == MOST_STASHED {
                    self.stash.clear();
                }
                self.stash.push(packet.stashed(bytes));
            }
            if self.structure.is_none() {
                self.wants_key = true;
            }
            return None;
        };
        let media = match self.codec {
            Codec::Vp8 => vp8::Descriptor::read(packet.payload)?.1,
            Codec::Vp9 => vp9::Descriptor::read(packet.payload)?.1,
        };
        if descriptor.structure.is_some() && !descriptor.is_start {
            return None;
        }
        let id = self.unwrap_frame(descriptor.frame);
        let is_key = descriptor.structure.is_some();
        if let Some(structure) = descriptor.structure {
            if self.structure_frame.is_some_and(|kept| kept > id) {
                return None;
            }
            self.structure = Some(structure);
            (self.structure_frame, self.active) = (Some(id), None);
        }
        if descriptor.active.is_some() {
            self.active = descriptor.active;
        }
        let structure = self.structure.as_ref()?;
        let top = structure.top_spatial(self.active.unwrap_or(structure.all_targets()));
        let refs = frame.fdiffs.iter().map(|fdiff| id.checked_sub(u64::from(*fdiff))).collect::<Option<Vec<_>>>()?;
        let label = descriptor.is_start.then_some(Label { id, refs, is_key });
        Some(Parsed {
            ids: Ids::Dependency,
            is_start: descriptor.is_start,
            is_end: descriptor.is_end || packet.marker,
            is_last: frame.spatial >= top,
            label,
            media,
        })
    }

    fn unwrap_frame(&mut self, number: u16) -> u64 {
        let id =
            self.frame_number.map_or(FIRST_CYCLE << u16::BITS | u64::from(number), |highest| extend(number, highest));
        self.frame_number = Some(self.frame_number.map_or(id, |highest| highest.max(id)));
        id
    }

    // A picture id extended by those before it; a change between 7 and 15 bits begins its count anew.
    fn unwrap_picture(&mut self, picture: vp8::Picture) -> u64 {
        let bits = picture.bits();
        let id = match self.picture {
            Some((highest, kept)) if kept == bits => extend_bits(u64::from(picture.id), bits, highest),
            _ => FIRST_CYCLE << bits | u64::from(picture.id),
        };
        let highest = self.picture.filter(|(_, kept)| *kept == bits).map_or(id, |(highest, _)| highest.max(id));
        self.picture = Some((highest, bits));
        id
    }

    // Keeps a packet by its extended sequence number; none for one handed over or dropped already, one again, or one past a bound, which clears all.
    fn store(&mut self, sequence: u16, timestamp: u32, parsed: &Parsed<'_>) -> Option<u64> {
        let at =
            self.sequence.map_or(FIRST_CYCLE << u16::BITS | u64::from(sequence), |highest| extend(sequence, highest));
        if self.cleared.is_some_and(|cleared| at <= cleared) || self.packets.contains_key(&at) {
            return None;
        }
        let highest = self.sequence.unwrap_or(at);
        if highest.saturating_sub(at) >= (MOST_PACKETS / 2) as u64 {
            // packet_buffer.cc, `InsertPacket`: a jump far back begins the stream anew.
            self.reset();
            return self.store(sequence, timestamp, parsed);
        }
        if let Some(oldest) = at.checked_sub(MOST_PACKETS as u64) {
            let kept = self.packets.split_off(&oldest);
            self.bytes -= self.packets.values().map(|stored| stored.payload.len()).sum::<usize>();
            self.packets = kept;
        }
        if self.packets.len() == MOST_PACKETS || self.bytes + parsed.media.len() > MOST_BYTES {
            // packet_buffer.cc, `InsertPacket`: a full buffer is cleared and a key frame asked for.
            self.reset();
            return None;
        }
        self.sequence = Some(highest.max(at));
        self.bytes += parsed.media.len();
        let stored = Stored {
            timestamp,
            is_start: parsed.is_start,
            is_end: parsed.is_end,
            is_last: parsed.is_last,
            label: parsed.label.clone(),
            payload: parsed.media.to_vec(),
        };
        self.packets.insert(at, stored);
        Some(at)
    }

    // The frame a packet completes, its packets taken out: from a start to an end, every sequence number between them there, all of one timestamp.
    fn complete(&mut self, at: u64) -> Option<Waiting> {
        let timestamp = self.packets.get(&at)?.timestamp;
        let mut first = at;
        loop {
            let stored = self.packets.get(&first).filter(|stored| stored.timestamp == timestamp)?;
            if stored.is_start {
                break;
            }
            first = first.checked_sub(1)?;
        }
        let mut last = at;
        loop {
            let stored = self.packets.get(&last).filter(|stored| stored.timestamp == timestamp)?;
            if stored.is_end {
                break;
            }
            last += 1;
        }
        let mut label = self.packets.get(&first)?.label.clone()?;
        let is_last = self.packets.get(&last)?.is_last;
        if self.ids == Some(Ids::Sequence) {
            // rtp_seq_num_only_ref_finder.cc: a frame is known by its last sequence number and leans on the frame that ends just before it.
            label.id = last;
            label.refs = if label.is_key { Vec::new() } else { alloc::vec![first - 1] };
        }
        let mut bytes = Vec::new();
        for sequence in first..=last {
            if let Some(stored) = self.packets.remove(&sequence) {
                bytes.extend_from_slice(&stored.payload);
            }
        }
        Some(Waiting { timestamp, label, is_last, last, bytes })
    }

    // Holds a whole frame until what it depends on is handed over, as libwebrtc's frame buffer does (frame_buffer.cc, `InsertFrame`): one that leans on none before it, is older than one handed over but for a newer key frame, or finds the buffer full but for a key frame, is dropped.
    fn wait(&mut self, waiting: Waiting) {
        let Label { id, ref refs, is_key, .. } = waiting.label;
        let is_valid =
            refs.iter().enumerate().all(|(at, reference)| *reference < id && !refs[..at].contains(reference));
        let is_old = self.last_handed.is_some_and(|(handed, _)| id <= handed);
        let is_newer_key =
            is_key && self.last_handed.is_some_and(|(_, timestamp)| is_after(waiting.timestamp, timestamp));
        if !is_valid || is_old && !is_newer_key || self.waiting.contains_key(&id) {
            self.bytes -= waiting.bytes.len();
            return;
        }
        if is_old || self.waiting.len() == MOST_WAITING {
            if !is_key {
                self.bytes -= waiting.bytes.len();
                return;
            }
            self.drop_waiting(u64::MAX);
            (self.handed, self.last_handed) = (Window::default(), None);
        }
        if !self.has_key && !is_key {
            self.wants_key = true;
        }
        self.waiting.insert(id, waiting);
    }

    fn drop_waiting(&mut self, below: u64) {
        let kept = self.waiting.split_off(&below);
        self.bytes -= self.waiting.values().map(|waiting| waiting.bytes.len()).sum::<usize>();
        self.waiting = kept;
    }

    // Hands over each picture whose frames are all whole and lean only on frames handed over or of the picture itself, the first such in order of frame ids, dropping every frame before it (frame_buffer.cc, `FindNextAndLastDecodableTemporalUnit`): a picture ends at its last frame, or where the frame of the next id is of another timestamp.
    fn hand(&mut self, now: Duration, frames: &mut Vec<Frame>) {
        while let Some((first, last)) = self.decodable() {
            self.drop_waiting(first);
            let unit = self.waiting.split_off(&(last + 1));
            let unit = core::mem::replace(&mut self.waiting, unit);
            let mut picture = Frame {
                codec: self.codec,
                is_key: false,
                layers: Vec::with_capacity(unit.len()),
                orientation: self.orientation,
            };
            let mut end = 0;
            for (id, waiting) in unit {
                self.handed.take(id);
                self.bytes -= waiting.bytes.len();
                picture.is_key |= waiting.label.is_key && picture.layers.is_empty();
                picture.layers.push(waiting.bytes);
                self.last_handed = Some((id, waiting.timestamp));
                end = end.max(waiting.last);
            }
            let kept = self.packets.split_off(&(end + 1));
            self.bytes -= self.packets.values().map(|stored| stored.payload.len()).sum::<usize>();
            self.packets = kept;
            self.cleared = Some(self.cleared.map_or(end, |cleared| cleared.max(end)));
            self.progress = Some(now);
            if picture.is_key {
                (self.has_key, self.wants_key) = (true, false);
            }
            frames.push(picture);
        }
    }

    // The first and last frame ids of the first picture that may be handed over.
    fn decodable(&self) -> Option<(u64, u64)> {
        let frames: Vec<(u64, &Waiting)> = self.waiting.iter().map(|(id, waiting)| (*id, waiting)).collect();
        let mut start = 0;
        while start < frames.len() {
            let timestamp = frames[start].1.timestamp;
            let mut end = start;
            let mut is_whole = false;
            while end < frames.len() && frames[end].1.timestamp == timestamp {
                end += 1;
                if frames[end - 1].1.is_last {
                    is_whole = true;
                    break;
                }
            }
            is_whole |= end < frames.len() && frames[end].0 == frames[end - 1].0 + 1;
            let unit = &frames[start..end];
            let leans_on_known = unit.iter().all(|(_, waiting)| {
                waiting
                    .label
                    .refs
                    .iter()
                    .all(|reference| self.handed.has(*reference) || unit.iter().any(|(id, _)| id == reference))
            });
            if is_whole && leans_on_known {
                return Some((unit[0].0, unit[unit.len() - 1].0));
            }
            start = end;
        }
        None
    }

    // Lets go of all that is held, and wants a key frame to begin again from.
    fn reset(&mut self) {
        self.packets.clear();
        self.waiting.clear();
        self.stash.clear();
        self.bytes = 0;
        (self.sequence, self.cleared, self.frame_number, self.picture) = (None, None, None, None);
        (self.structure_frame, self.handed, self.last_handed) = (None, Window::default(), None);
        self.wants_key = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base::math::xorshift64;
    use crate::codec::ivf;
    use crate::codec::opus::support::read_packets;
    use crate::codec::vp8::is_key_frame;
    use crate::codec::vp8::tests::{
        ALTREF, ALTREF_CRCS, BILINEAR, BILINEAR_CRCS, CALL, CALL_CRCS, VERSIONS, VERSIONS_CRCS, decode_frames,
    };
    use crate::net::rtp::dependency::{Dti, Frame as Dependent, Template};
    use crate::net::rtp::packet::Packet;
    use crate::net::rtp::vp8::{Budget, Picture};
    use alloc::vec;

    // ffmpeg's RTP packets of the call fixture and of a VP9 test pattern, each behind its length, and the pattern's frames (made with `ffmpeg -f rtp` and `-f ivf`).
    const CALL_RTP: &[u8] = include_bytes!("../../../assets/video/rtp/call_vp8.rtp");
    const VP9_RTP: &[u8] = include_bytes!("../../../assets/video/rtp/testsrc_vp9.rtp");
    const VP9_IVF: &[u8] = include_bytes!("../../../assets/video/rtp/testsrc_vp9.ivf");
    // Packets of 300 bytes of VP8 data, so that most frames take several; 30 frames a second of the 90 kHz clock.
    const PIECE: usize = 300;
    const FRAME_TICKS: u32 = 3000;

    #[derive(Clone)]
    struct Sent {
        sequence: u16,
        timestamp: u32,
        marker: bool,
        dependency: Option<Vec<u8>>,
        payload: Vec<u8>,
    }

    impl Sent {
        fn incoming(&self) -> Incoming<'_> {
            Incoming {
                sequence: self.sequence,
                timestamp: self.timestamp,
                marker: self.marker,
                dependency: self.dependency.as_deref(),
                orientation: None,
                payload: &self.payload,
            }
        }
    }

    // VP8 frames cut into packets as this side's sender cuts them: a picture id of 15 bits that wraps, or under a group call's dependency descriptor the payload descriptor's one byte.
    fn packetize(frames: &[&[u8]], is_group: bool, first_sequence: u16) -> Vec<Sent> {
        let structure = Structure::single(0, 99, 61);
        let mut sent = Vec::new();
        let mut sequence = first_sequence;
        for (number, frame) in frames.iter().enumerate() {
            let is_key = is_key_frame(frame);
            let id = 0x7FF0u16.wrapping_add(number as u16) & 0x7FFF;
            let picture = (!is_group).then_some(Picture { id, is_long: true });
            let descriptor = vp8::Descriptor { picture, ..vp8::Descriptor::default() };
            let fdiffs = if is_key { Vec::new() } else { vec![1] };
            let info = Dependent { dtis: vec![Dti::Switch], fdiffs, ..Dependent::default() };
            let pieces = vp8::split(frame.len(), Budget { bytes: PIECE, ..Budget::default() });
            let mut rest = *frame;
            for (at, &size) in pieces.iter().enumerate() {
                let (is_first, is_last) = (at == 0, at + 1 == pieces.len());
                let (piece, after) = rest.split_at(size);
                rest = after;
                let mut payload = Vec::new();
                vp8::Descriptor { is_start: is_first, ..descriptor }.write(&mut payload);
                payload.extend_from_slice(piece);
                let dependency = is_group.then(|| {
                    let mut chosen = Dependency::of(&structure, &info, 0, is_key && is_first, None).unwrap();
                    (chosen.is_start, chosen.is_end, chosen.frame) = (is_first, is_last, number as u16);
                    let mut out = Vec::new();
                    assert!(chosen.write(&structure, &mut out));
                    out
                });
                let timestamp = number as u32 * FRAME_TICKS;
                sent.push(Sent { sequence, timestamp, marker: is_last, dependency, payload });
                sequence = sequence.wrapping_add(1);
            }
        }
        sent
    }

    // The packets as a network gives them: each window of eight in random order, one in twenty twice, and one in eight lost and sent again eight packets on, as a NACK brings it.
    fn deliver(sent: &[Sent], seed: u64) -> Vec<Sent> {
        let mut state = seed;
        let mut next = || {
            state = xorshift64(state);
            state
        };
        let (mut out, mut again): (Vec<Sent>, Vec<(usize, Sent)>) = (Vec::new(), Vec::new());
        for window in sent.chunks(8) {
            let mut window = window.to_vec();
            for at in (1..window.len()).rev() {
                window.swap(at, next() as usize % (at + 1));
            }
            for packet in window {
                if next() % 8 == 0 {
                    again.push((out.len() + 8, packet));
                    continue;
                }
                if next() % 20 == 0 {
                    out.push(packet.clone());
                }
                out.push(packet);
                while let Some(due) = again.iter().position(|(at, _)| *at <= out.len()) {
                    out.push(again.remove(due).1);
                }
            }
        }
        out.extend(again.into_iter().map(|(_, packet)| packet));
        out
    }

    fn held(assembler: &Assembler) -> (usize, usize, usize) {
        (assembler.bytes, assembler.packets.len(), assembler.waiting.len())
    }

    #[test]
    fn vp8_reordered_lost_doubled() {
        let streams: [(&str, &[u8], &[u32]); 4] = [
            ("call", CALL, &CALL_CRCS),
            ("altref", ALTREF, &ALTREF_CRCS),
            ("bilinear", BILINEAR, &BILINEAR_CRCS),
            ("versions", VERSIONS, &VERSIONS_CRCS),
        ];
        for (name, file, crcs) in streams {
            let frames = ivf::frames(file);
            for (is_group, seed) in
                [false, true].into_iter().flat_map(|is_group| (1..=4).map(move |seed| (is_group, seed)))
            {
                let mut assembler = Assembler::new(Codec::Vp8);
                let mut handed = Vec::new();
                for (at, packet) in deliver(&packetize(&frames, is_group, 0xFFF0), seed).iter().enumerate() {
                    handed.extend(assembler.take(packet.incoming(), Duration::from_millis(at as u64)));
                }
                let case = alloc::format!("{name}, dependency descriptor {is_group}, seed {seed}");
                assert_eq!(handed.len(), frames.len(), "{case}: every frame handed over once");
                assert!(
                    handed.iter().zip(&frames).all(|(frame, ivf)| frame.is_key == is_key_frame(ivf)),
                    "{case}: key frames told"
                );
                let joined: Vec<Vec<u8>> = handed.iter().map(Frame::join).collect();
                assert_eq!(
                    decode_frames(joined.iter().map(Vec::as_slice)).as_deref(),
                    Ok(crcs),
                    "{case}: decoded to ffmpeg's pictures"
                );
                assert_eq!(held(&assembler), (0, 0, 0), "{case}: nothing held once the last frame is handed over");
            }
        }
    }

    fn take_all(assembler: &mut Assembler, datagrams: &[&[u8]]) -> Vec<Frame> {
        let mut frames = Vec::new();
        for (at, datagram) in datagrams.iter().enumerate() {
            let read = Packet::read(datagram).unwrap();
            let incoming = Incoming {
                sequence: read.sequence,
                timestamp: read.timestamp,
                marker: read.marker,
                dependency: None,
                orientation: None,
                payload: read.payload,
            };
            frames.extend(assembler.take(incoming, Duration::from_millis(at as u64)));
        }
        frames
    }

    #[test]
    fn ffmpeg_packets() {
        let frames = take_all(&mut Assembler::new(Codec::Vp8), &read_packets(CALL_RTP));
        let joined: Vec<Vec<u8>> = frames.iter().map(Frame::join).collect();
        assert_eq!(joined, ivf::frames(CALL), "ffmpeg's VP8 packets of the call give its frames back, byte for byte");
        assert_eq!(decode_frames(joined.iter().map(Vec::as_slice)).as_deref(), Ok(&CALL_CRCS[..]), "and its pictures");
        assert!(frames.iter().all(|frame| frame.codec == Codec::Vp8), "each named VP8");
        let frames = take_all(&mut Assembler::new(Codec::Vp9), &read_packets(VP9_RTP));
        let joined: Vec<Vec<u8>> = frames.iter().map(Frame::join).collect();
        assert_eq!(
            joined,
            ivf::frames(VP9_IVF),
            "ffmpeg's VP9 packets, without picture ids, give the pattern's frames back"
        );
        assert!(frames.iter().all(|frame| frame.codec == Codec::Vp9), "each named VP9");
    }

    #[test]
    fn key_frame_after_loss() {
        let mut frames = ivf::frames(CALL);
        frames.extend([frames[0], frames[1], frames[2]]);
        let sent = packetize(&frames, false, 0);
        let lost = sent.iter().position(|packet| packet.timestamp == 5 * FRAME_TICKS).unwrap();
        let again = sent.iter().position(|packet| packet.timestamp == 20 * FRAME_TICKS).unwrap();
        let step = Duration::from_millis(150);
        let mut assembler = Assembler::new(Codec::Vp8);
        let (mut handed, mut now, mut progress, mut asked) = (Vec::new(), Duration::ZERO, Duration::ZERO, None);
        for (at, packet) in sent[..again].iter().enumerate() {
            if at != lost {
                let frames = assembler.take(packet.incoming(), now);
                if !frames.is_empty() {
                    progress = now;
                }
                handed.extend(frames);
            }
            if asked.is_none() && assembler.needs_key(now) {
                asked = Some(now);
            }
            now += step;
        }
        assert_eq!(handed.len(), 5, "the frames before the one lost for good");
        let asked = asked.unwrap();
        assert!(
            asked >= progress + FRAME_WAIT && asked < progress + FRAME_WAIT + step,
            "a key frame asked for 3 s after the last frame handed over: {asked:?} after {progress:?}"
        );
        assert!(!assembler.needs_key(asked + KEY_WAIT / 2), "not again within 200 ms");
        assert!(assembler.needs_key(asked + KEY_WAIT), "again after 200 ms while none comes");
        for packet in &sent[again..] {
            handed.extend(assembler.take(packet.incoming(), now));
            now += step;
        }
        assert_eq!(handed.len(), 8, "the next key frame and the frames after it");
        assert!(handed[5].is_key && !assembler.needs_key(now + KEY_WAIT), "a key frame handed over asks no more");
        let joined: Vec<Vec<u8>> = handed.iter().map(Frame::join).collect();
        assert_eq!(
            decode_frames(joined.iter().map(Vec::as_slice)).as_deref(),
            Ok(&[&CALL_CRCS[..5], &CALL_CRCS[..3]].concat()[..]),
            "the stream decodes again from the key frame on"
        );
        let mut fresh = Assembler::new(Codec::Vp8);
        for packet in sent.iter().filter(|packet| packet.timestamp == FRAME_TICKS) {
            assert!(fresh.take(packet.incoming(), Duration::ZERO).is_empty(), "a delta frame alone is not handed over");
        }
        assert!(
            fresh.needs_key(Duration::ZERO),
            "a whole delta frame with no key frame before it asks for one at once"
        );
    }

    // A picture of two spatial layers under a structure of two decode targets, as an SFU forwards VP9: each layer frame its own packet, ending in the marker.
    #[test]
    fn layer_frames_joined() {
        let template = |spatial: u8, dtis: [Dti; 2], fdiffs: &[u16]| Template {
            spatial,
            dtis: dtis.to_vec(),
            fdiffs: fdiffs.to_vec(),
            ..Template::default()
        };
        let both = [Dti::Switch, Dti::Switch];
        let upper = [Dti::NotPresent, Dti::Switch];
        let structure = Structure {
            targets: 2,
            templates: vec![
                template(0, both, &[]),
                template(0, both, &[2]),
                template(1, upper, &[1]),
                template(1, upper, &[2, 1]),
            ],
            resolutions: vec![(160, 120), (320, 240)],
            ..Structure::default()
        };
        let packet = |number: u16, template: usize, active: Option<u32>, data: &[u8]| {
            let chosen = &structure.templates[template];
            let frame = Dependent {
                spatial: chosen.spatial,
                dtis: chosen.dtis.clone(),
                fdiffs: chosen.fdiffs.clone(),
                ..Dependent::default()
            };
            let mut descriptor = Dependency::of(&structure, &frame, 0, number == 0, active).unwrap();
            (descriptor.is_start, descriptor.is_end, descriptor.frame) = (true, true, number);
            let mut dependency = Vec::new();
            assert!(descriptor.write(&structure, &mut dependency));
            let payload = [&[0x0C][..], data].concat();
            Sent {
                sequence: number,
                timestamp: u32::from(number / 2) * FRAME_TICKS,
                marker: true,
                dependency: Some(dependency),
                payload,
            }
        };
        let mut assembler = Assembler::new(Codec::Vp9);
        let mut take = |sent: Sent| assembler.take(sent.incoming(), Duration::ZERO);
        assert!(take(packet(0, 0, None, b"key0")).is_empty(), "the base layer alone is not the whole picture");
        let first = take(packet(1, 2, None, b"key1"));
        assert_eq!(first.len(), 1);
        assert_eq!(
            (first[0].is_key, first[0].layers.clone(), first[0].join()),
            (true, vec![b"key0".to_vec(), b"key1".to_vec()], b"key0key1".to_vec()),
            "both layer frames, in order, joined one after the other"
        );
        let opened = first[0].clone().open(|layer| layer.strip_prefix(b"key").map(<[u8]>::to_vec));
        assert_eq!(
            opened.map(|frame| frame.join()),
            Some(b"01".to_vec()),
            "each layer frame opened alone, then joined"
        );
        let refused = first[0].clone().open(|layer| (layer != b"key1").then(|| layer.to_vec()));
        assert!(refused.is_none(), "a layer frame that does not open leaves no picture");
        let second = take(packet(2, 1, Some(0b01), b"delta0"));
        assert_eq!(
            second.iter().map(|frame| frame.layers.clone()).collect::<Vec<_>>(),
            [vec![b"delta0".to_vec()]],
            "with the upper decode target let go, the base layer frame is the whole picture"
        );
    }

    // Random packets, and real ones with bits flipped, from a fixed seed: every call returns and nothing held passes its bound.
    #[test]
    fn strangers_packets() {
        const CASES: u32 = 20_000;
        let mut state = 0x0F0F_1234_5678_9ABCu64;
        let mut next = || {
            state = xorshift64(state);
            state
        };
        let frames = ivf::frames(CALL);
        let samples = [packetize(&frames, false, 0), packetize(&frames, true, 0)].concat();
        for codec in [Codec::Vp8, Codec::Vp9] {
            let mut assembler = Assembler::new(codec);
            for case in 0..CASES {
                let now = Duration::from_millis(u64::from(case));
                let mut sent = samples[next() as usize % samples.len()].clone();
                if next() % 2 == 0 {
                    sent.payload = (0..next() % 1300).map(|_| next() as u8).collect();
                    sent.dependency = (next() % 2 == 0).then(|| (0..next() % 40).map(|_| next() as u8).collect());
                    (sent.sequence, sent.timestamp, sent.marker) =
                        (next() as u16, (next() % 8) as u32 * FRAME_TICKS, next() % 2 == 0);
                } else {
                    let bytes = sent.payload.len() * 8;
                    sent.payload[next() as usize % bytes / 8] ^= 1 << (next() % 8);
                    if let Some(dependency) = sent.dependency.as_mut().filter(|dependency| !dependency.is_empty()) {
                        let at = next() as usize % dependency.len();
                        dependency[at] ^= 1 << (next() % 8);
                    }
                }
                assembler.take(sent.incoming(), now);
                assembler.needs_key(now);
                assert!(
                    assembler.bytes <= MOST_BYTES
                        && assembler.packets.len() <= MOST_PACKETS
                        && assembler.waiting.len() <= MOST_WAITING
                        && assembler.stash.len() <= MOST_STASHED,
                    "case {case}: what is held stays within its bounds"
                );
            }
        }
    }
}

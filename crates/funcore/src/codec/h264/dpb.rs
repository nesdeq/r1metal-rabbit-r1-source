//! The decoded picture buffer: references, reference lists and output order.

use super::read::{Header, MOST_FRAMES, Marking, Order, Sequence, Slice};
use alloc::vec::Vec;
use core::cmp::Reverse;

const SLOTS: usize = MOST_FRAMES as usize + 1;
const MOST_BUFFERS: usize = u32::BITS as usize;
const MAX_DPB_MBS: [(u32, u32); 20] = [
    (9, 396),
    (10, 396),
    (11, 900),
    (12, 2376),
    (13, 2376),
    (20, 2376),
    (21, 4752),
    (22, 8100),
    (30, 8100),
    (31, 18000),
    (32, 20480),
    (40, 32768),
    (41, 32768),
    (42, 34816),
    (50, 110_400),
    (51, 184_320),
    (52, 184_320),
    (60, 696_320),
    (61, 696_320),
    (62, 696_320),
];
const LOWER_PROFILES: [u32; 3] = [66, 77, 88];
const LEVEL_1B: u32 = 9;
const LEVEL_1_1: u32 = 11;

pub(crate) fn dpb_size(sequence: &Sequence) -> (usize, usize) {
    let is_1b = sequence.level == LEVEL_1_1 && sequence.third_constraint && LOWER_PROFILES.contains(&sequence.profile);
    let level = if is_1b { LEVEL_1B } else { sequence.level };
    let max_mbs = MAX_DPB_MBS.iter().find(|(idc, _)| *idc >= level).unwrap_or(&MAX_DPB_MBS[MAX_DPB_MBS.len() - 1]).1;
    let by_level = (u64::from(max_mbs) / (u64::from(sequence.columns) * u64::from(sequence.rows)).max(1))
        .min(u64::from(MOST_FRAMES)) as u32;
    let capacity = sequence.buffering.unwrap_or(by_level).max(sequence.references).clamp(1, MOST_FRAMES) as usize;
    let reorder = match (sequence.reorder, &sequence.order) {
        (Some(depth), _) => (depth as usize).min(capacity),
        (None, Order::Frame) => 0,
        (None, _) => capacity,
    };
    (capacity, reorder)
}

#[derive(Clone, Debug)]
struct Frame {
    slot: usize,
    buffer: Option<usize>,
    number: u32,
    top: i32,
    bottom: i32,
    is_reference: bool,
    is_waiting: bool,
    stamp: u64,
}

impl Frame {
    fn order(&self) -> i32 {
        self.top.min(self.bottom)
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct OrderCount {
    high: i64,
    low: i64,
    offset: i64,
    number: u32,
}

struct Current {
    frame: Frame,
    marking: Marking,
    numbers: u32,
    reference_limit: usize,
    count: OrderCount,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Entry {
    pub(crate) slot: usize,
    pub(crate) buffer: Option<usize>,
    pub(crate) number: i32,
    pub(crate) top: i32,
    pub(crate) bottom: i32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Target {
    pub(crate) slot: usize,
    pub(crate) buffer: usize,
    pub(crate) number: u32,
    pub(crate) numbers: u32,
    pub(crate) top: i32,
    pub(crate) bottom: i32,
    pub(crate) forward: Vec<Entry>,
    pub(crate) spare: usize,
}

/// A picture to show: its buffer stays untouched until it is given back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Output {
    pub buffer: usize,
    pub stamp: u64,
}

fn wrap(number: u32, current: u32, numbers: u32) -> i32 {
    if number > current { number as i32 - numbers as i32 } else { number as i32 }
}

#[derive(Default)]
pub(crate) struct Dpb {
    buffers: usize,
    out_buffers: u32,
    frames: Vec<Frame>,
    current: Option<Current>,
    capacity: usize,
    reorder: usize,
    count: OrderCount,
    last_number: u32,
    last_buffer: Option<usize>,
}

impl Dpb {
    pub(crate) fn new(buffers: usize) -> Dpb {
        Dpb { buffers: buffers.min(MOST_BUFFERS), ..Dpb::default() }
    }

    fn free_slot(&self) -> Option<usize> {
        (0..SLOTS).find(|slot| self.frames.iter().all(|frame| frame.slot != *slot))
    }

    fn is_spare(&self, buffer: usize) -> bool {
        let is_held = self
            .frames
            .iter()
            .chain(self.current.iter().map(|current| &current.frame))
            .any(|frame| frame.buffer == Some(buffer));
        self.out_buffers >> buffer & 1 == 0 && !is_held
    }

    fn remove_unused(&mut self) {
        self.frames.retain(|frame| frame.is_reference || frame.is_waiting);
    }

    fn slide(&mut self, current: u32, numbers: u32, reference_limit: usize) {
        while self.frames.iter().filter(|frame| frame.is_reference).count() >= reference_limit {
            let Some(oldest) = self
                .frames
                .iter_mut()
                .filter(|frame| frame.is_reference)
                .min_by_key(|frame| wrap(frame.number, current, numbers))
            else {
                break;
            };
            oldest.is_reference = false;
        }
        self.remove_unused();
    }

    fn output_next(&mut self, output: &mut Vec<Output>) -> bool {
        let Some(place) =
            (0..self.frames.len()).filter(|&at| self.frames[at].is_waiting).min_by_key(|&at| self.frames[at].order())
        else {
            return false;
        };
        let frame = &mut self.frames[place];
        frame.is_waiting = false;
        if let Some(buffer) = frame.buffer {
            self.out_buffers |= 1 << buffer;
            output.push(Output { buffer, stamp: frame.stamp });
        }
        self.remove_unused();
        true
    }

    fn bump(&mut self, output: &mut Vec<Output>) {
        while self.frames.iter().filter(|frame| frame.is_waiting).count() > self.reorder
            || self.frames.len() > self.capacity
        {
            if !self.output_next(output) {
                self.frames.remove(0);
            }
        }
    }

    fn fill_gap(&mut self, to: u32, numbers: u32, reference_limit: usize, output: &mut Vec<Output>) {
        // A stream whose numbers have grown shorter may have left a last number beyond them.
        let last = self.last_number % numbers;
        let missing = (to + numbers - last - 1) % numbers;
        for step in 0..missing {
            let number = (last + 1 + step) % numbers;
            if self.count.number > number {
                self.count.offset += i64::from(numbers);
            }
            self.count.number = number;
            if (missing - step) as usize > reference_limit {
                continue;
            }
            self.slide(number, numbers, reference_limit);
            if let Some(slot) = self.free_slot() {
                self.frames.push(Frame {
                    slot,
                    buffer: None,
                    number,
                    top: 0,
                    bottom: 0,
                    is_reference: true,
                    is_waiting: false,
                    stamp: 0,
                });
            }
            self.bump(output);
        }
        self.last_number = (to + numbers - 1) % numbers;
    }

    fn order_counts(&self, sequence: &Sequence, header: Header, slice: &Slice) -> (i32, i32, OrderCount) {
        let numbers = i64::from(sequence.numbers());
        let is_key = header.is_key_frame();
        let (previous, is_reference) =
            (if is_key { OrderCount::default() } else { self.count }, header.importance != 0);
        let offset = if previous.number <= slice.number { previous.offset } else { previous.offset + numbers };
        let mut next = OrderCount { offset, number: slice.number, ..previous };
        let (top, bottom) = match sequence.order {
            Order::Lsb { bits } => {
                let (range, low) = (1i64 << bits, i64::from(slice.order_low));
                let high = if low < previous.low && previous.low - low >= range / 2 {
                    previous.high + range
                } else if low > previous.low && low - previous.low > range / 2 {
                    previous.high - range
                } else {
                    previous.high
                };
                if is_reference {
                    (next.high, next.low) = (high, low);
                }
                (high + low, high + low + i64::from(slice.order_bottom))
            }
            Order::Frame => {
                let count = if is_key { 0 } else { 2 * (offset + i64::from(slice.number)) - i64::from(!is_reference) };
                (count, count)
            }
        };
        (top as i32, bottom as i32, next)
    }

    fn reference_list(&self, number: u32, numbers: u32) -> Vec<Entry> {
        let mut references: Vec<&Frame> = self.frames.iter().filter(|frame| frame.is_reference).collect();
        references.sort_by_key(|frame| Reverse(wrap(frame.number, number, numbers)));
        let entry = |frame: &Frame| Entry {
            slot: frame.slot,
            buffer: frame.buffer,
            number: wrap(frame.number, number, numbers),
            top: frame.top,
            bottom: frame.bottom,
        };
        references.into_iter().map(entry).collect()
    }

    pub(crate) fn begin(
        &mut self,
        sequence: &Sequence,
        header: Header,
        slice: &Slice,
        stamp: u64,
        output: &mut Vec<Output>,
    ) -> Option<Target> {
        self.finish(output);
        (0..self.buffers).find(|&buffer| self.is_spare(buffer))?;
        let (numbers, reference_limit) = (sequence.numbers(), sequence.references.max(1) as usize);
        (self.capacity, self.reorder) = dpb_size(sequence);
        if header.is_key_frame() {
            if !matches!(slice.marking, Marking::Key { discard: true, .. }) {
                while self.output_next(output) {}
            }
            self.frames.clear();
            self.last_number = 0;
        } else if slice.number != self.last_number && slice.number != (self.last_number + 1) % numbers {
            self.fill_gap(slice.number, numbers, reference_limit, output);
        }
        let (top, bottom, count) = self.order_counts(sequence, header, slice);
        let (slot, buffer) = (self.free_slot()?, (0..self.buffers).find(|&buffer| self.is_spare(buffer))?);
        let frame = Frame {
            slot,
            buffer: Some(buffer),
            number: slice.number,
            top,
            bottom,
            is_reference: false,
            is_waiting: false,
            stamp,
        };
        let forward = self.reference_list(slice.number, numbers);
        self.current = Some(Current { frame, marking: slice.marking, numbers, reference_limit, count });
        let spare = self.last_buffer.replace(buffer).unwrap_or(buffer);
        Some(Target { slot, buffer, number: slice.number, numbers, top, bottom, forward, spare })
    }

    pub(crate) fn finish(&mut self, output: &mut Vec<Output>) {
        let Some(mut current) = self.current.take() else {
            return;
        };
        let (number, numbers, reference_limit) = (current.frame.number, current.numbers, current.reference_limit);
        match current.marking {
            Marking::Unused => {}
            Marking::Key { .. } => current.frame.is_reference = true,
            Marking::Sliding => {
                self.slide(number, numbers, reference_limit);
                current.frame.is_reference = true;
            }
        }
        self.count = current.count;
        if current.frame.is_reference {
            self.last_number = current.frame.number;
        }
        current.frame.is_waiting = true;
        self.frames.push(current.frame);
        self.bump(output);
    }

    pub(crate) fn drain(&mut self, output: &mut Vec<Output>) {
        self.finish(output);
        while self.output_next(output) {}
    }

    pub(crate) fn give_back(&mut self, buffer: usize) {
        self.out_buffers &= !1u32.checked_shl(buffer as u32).unwrap_or(0);
    }

    pub(crate) fn clear(&mut self) {
        (self.current, self.count, self.last_number, self.last_buffer) = (None, OrderCount::default(), 0, None);
        self.frames.clear();
    }

    #[cfg(test)]
    fn free_buffers(&self) -> usize {
        (0..self.buffers).filter(|&buffer| self.is_spare(buffer)).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::h264::read::samples::*;
    use alloc::vec;

    const KEY: Header = Header { importance: 3, kind: 5 };
    const REFERENCE: Header = Header { importance: 2, kind: 1 };
    const NON_REFERENCE: Header = Header { importance: 0, kind: 1 };
    const PLAIN_KEY: Marking = Marking::Key { discard: false };

    fn sequence(order: Order, references: u32, number_bits: u32) -> Sequence {
        Sequence { level: 31, number_bits, order, references, ..sized(20, 15) }
    }

    fn slice(number: u32, order_low: u32, marking: Marking) -> Slice {
        Slice { kind: 5, number, order_low, marking, ..Slice::default() }
    }

    struct Record {
        target: Target,
        forward: Vec<u64>,
    }

    struct Player {
        dpb: Dpb,
        sequence: Sequence,
        stamps: Vec<u64>,
        stamp: u64,
    }

    impl Player {
        fn new(sequence: Sequence) -> Player {
            Player { dpb: Dpb::new(MOST_BUFFERS), sequence, stamps: Vec::new(), stamp: 0 }
        }

        fn take(&mut self, out: Vec<Output>) {
            for picture in out {
                self.dpb.give_back(picture.buffer);
                self.stamps.push(picture.stamp);
            }
        }

        fn picture(&mut self, header: Header, slice: &Slice) -> Record {
            let mut out = Vec::new();
            let target = self.dpb.begin(&self.sequence, header, slice, self.stamp, &mut out).unwrap();
            let stamp = |entry: &Entry| self.dpb.frames.iter().find(|frame| frame.slot == entry.slot).unwrap().stamp;
            let record = Record { forward: target.forward.iter().map(stamp).collect(), target };
            self.dpb.finish(&mut out);
            self.take(out);
            self.stamp += 1;
            record
        }

        fn reference(&mut self, number: u32, order_low: u32) -> Record {
            self.picture(REFERENCE, &slice(number, order_low, Marking::Sliding))
        }

        fn end(&mut self) {
            let mut out = Vec::new();
            self.dpb.drain(&mut out);
            self.take(out);
        }
    }

    #[test]
    fn list_entry_fields() {
        let mut player = Player::new(sequence(Order::Lsb { bits: 8 }, 3, 4));
        player.picture(KEY, &slice(0, 0, PLAIN_KEY));
        player.reference(1, 6);
        let target = player.reference(2, 2).target;
        assert_eq!((target.slot, target.buffer, target.number, target.top), (2, 2, 2, 2));
        let (near, far) = (
            Entry { slot: 1, buffer: Some(1), number: 1, top: 6, bottom: 6 },
            Entry { slot: 0, buffer: Some(0), number: 0, top: 0, bottom: 0 },
        );
        assert_eq!(target.forward, [near, far]);
    }

    #[test]
    fn stream_order_outputs_at_once() {
        let mut player =
            Player::new(Sequence { reorder: Some(0), buffering: Some(2), ..sequence(Order::Lsb { bits: 16 }, 1, 16) });
        player.picture(KEY, &slice(0, 0, PLAIN_KEY));
        for number in 1..5 {
            let record = player.reference(number, 2 * number);
            assert_eq!(record.forward, [u64::from(number) - 1]);
            assert_eq!(
                (record.target.forward[0].number, record.target.forward[0].top),
                (number as i32 - 1, 2 * number as i32 - 2)
            );
            assert_eq!(player.stamps.len(), number as usize + 1);
            assert!(record.target.slot < 2 && record.target.buffer < 2, "two slots and two buffers take turns");
        }
        assert_eq!(player.stamps, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn wrapped_frame_numbers() {
        let mut player = Player::new(sequence(Order::Frame, 3, 4));
        player.picture(KEY, &slice(0, 0, PLAIN_KEY));
        for number in 1..=17u32 {
            player.reference(number % 16, 0);
        }
        let record = player.reference(2, 0);
        assert_eq!(
            record.target.forward.iter().map(|entry| entry.number).collect::<Vec<_>>(),
            [1, 0, -1],
            "15 counts as -1 once the numbers began again"
        );
        assert_eq!(record.forward, [17, 16, 15]);
        assert_eq!(
            (record.target.top, record.target.bottom),
            (36, 36),
            "twice the number of frames since the key picture"
        );
        assert_eq!(
            player.stamps,
            (0..19).collect::<Vec<u64>>(),
            "counts that follow the frame numbers leave no picture waiting"
        );
    }

    #[test]
    fn order_count_wraps() {
        let mut player = Player::new(sequence(Order::Lsb { bits: 4 }, 1, 8));
        player.picture(KEY, &slice(0, 0, PLAIN_KEY));
        let lows = [6, 12, 2, 13, 3, 10, 0];
        let counts: Vec<i32> = (1..).zip(lows).map(|(number, low)| player.reference(number, low).target.top).collect();
        assert_eq!(
            counts,
            [6, 12, 18, 13, 19, 26, 32],
            "a step of half the range or more is one into the next round or back into the last"
        );
        let mut player = Player::new(sequence(Order::Lsb { bits: 4 }, 1, 8));
        player.picture(KEY, &slice(0, 0, PLAIN_KEY));
        player.reference(1, 6);
        player.reference(2, 12);
        let unreferenced = player.picture(NON_REFERENCE, &slice(3, 2, Marking::Unused)).target;
        let next = player.reference(3, 5).target;
        assert_eq!((unreferenced.top, next.top), (18, 5), "a picture nothing refers to does not move the round on");
        let target = player.picture(REFERENCE, &Slice { order_bottom: -3, ..slice(4, 12, Marking::Sliding) }).target;
        assert_eq!((target.top, target.bottom), (12, 9));
    }

    #[test]
    fn key_picture_flushes() {
        let mut player = Player::new(Sequence { reorder: Some(2), ..sequence(Order::Lsb { bits: 8 }, 2, 4) });
        player.picture(KEY, &slice(0, 0, PLAIN_KEY));
        player.reference(1, 8);
        player.picture(NON_REFERENCE, &slice(2, 4, Marking::Unused));
        assert_eq!(player.stamps, [0]);
        let key = player.picture(KEY, &slice(0, 0, PLAIN_KEY)).target;
        assert_eq!((player.stamps.clone(), key.forward.len(), key.top), (vec![0, 2, 1], 0, 0));
        player.reference(1, 6);
        player.picture(KEY, &slice(0, 0, Marking::Key { discard: true }));
        assert_eq!(player.stamps, [0, 2, 1], "pictures a key picture tells to drop are never shown");
        assert_eq!(player.dpb.free_buffers(), MOST_BUFFERS - 1, "and their buffers are free again");
        player.end();
        assert_eq!(player.stamps, [0, 2, 1, 5]);
    }

    #[test]
    fn frame_number_gaps() {
        let mut player = Player::new(sequence(Order::Frame, 3, 4));
        player.picture(KEY, &slice(0, 0, PLAIN_KEY));
        player.reference(1, 0);
        let target = player.reference(4, 0).target;
        assert_eq!(
            target.forward.iter().map(|entry| (entry.number, entry.buffer)).collect::<Vec<_>>(),
            [(3, None), (2, None), (1, Some(1))],
            "two frames without a picture, and the window let the key picture go"
        );
        let target = player.picture(NON_REFERENCE, &slice(5, 0, Marking::Unused)).target;
        assert_eq!((target.forward.len(), target.top), (3, 9));
        let far = player.reference(3, 0).target;
        assert_eq!(
            far.forward.iter().map(|entry| (entry.number, entry.buffer)).collect::<Vec<_>>(),
            [(2, None), (1, None), (0, None)],
            "of fourteen numbers left out only as many as the stream refers to are kept"
        );
        assert_eq!((far.top, player.stamps.clone()), (38, vec![0, 1, 2, 3, 4]), "the numbers began again on the way");
    }

    #[test]
    fn shorter_frame_numbers() {
        let mut player = Player::new(sequence(Order::Frame, 2, 8));
        player.picture(KEY, &slice(0, 0, PLAIN_KEY));
        for number in 1..=200 {
            player.reference(number, 0);
        }
        player.sequence = sequence(Order::Frame, 2, 4);
        let target = player.reference(3, 0).target;
        assert_eq!(
            (target.number, target.numbers),
            (3, 16),
            "the last number, 200, counts as 8 of the 16 there are now"
        );
    }

    #[test]
    fn waits_for_free_buffer() {
        let stream = Sequence { reorder: Some(0), ..sequence(Order::Frame, 1, 4) };
        let (mut dpb, mut out) = (Dpb::new(2), Vec::new());
        let first = dpb.begin(&stream, KEY, &slice(0, 0, PLAIN_KEY), 7, &mut out).unwrap();
        dpb.finish(&mut out);
        assert_eq!(
            (out.clone(), dpb.free_buffers()),
            (vec![Output { buffer: first.buffer, stamp: 7 }], 1),
            "shown and still referred to"
        );
        let second = dpb.begin(&stream, REFERENCE, &slice(1, 0, Marking::Sliding), 8, &mut out).unwrap();
        assert_eq!((second.buffer, dpb.free_buffers()), (1, 0));
        assert_eq!(
            dpb.begin(&stream, REFERENCE, &slice(2, 0, Marking::Sliding), 9, &mut out),
            None,
            "the first picture is out, the second referred to"
        );
        assert_eq!(out.len(), 2, "beginning the next picture ended the one before");
        dpb.give_back(first.buffer);
        let third = dpb.begin(&stream, REFERENCE, &slice(2, 0, Marking::Sliding), 9, &mut out).unwrap();
        assert_eq!((third.buffer, third.forward[0].buffer), (0, Some(1)));
        dpb.clear();
        assert_eq!(dpb.free_buffers(), 1, "what was lent stays lent when the frames are forgotten");
        dpb.give_back(1);
        dpb.give_back(40);
        assert_eq!(dpb.free_buffers(), 2);
    }

    #[test]
    fn size_by_stream_or_level() {
        let stream = sequence(Order::Lsb { bits: 8 }, 3, 4);
        assert_eq!(dpb_size(&stream), (16, 16), "level 3.1 holds 18000 macroblocks, more than 16 frames of 300");
        assert_eq!(dpb_size(&Sequence { columns: 80, rows: 45, ..stream }), (5, 5));
        assert_eq!(dpb_size(&Sequence { columns: 80, rows: 45, order: Order::Frame, ..stream }), (5, 0));
        assert_eq!(dpb_size(&Sequence { columns: 120, rows: 68, level: 40, ..stream }), (4, 4));
        assert_eq!(
            dpb_size(&Sequence { columns: 120, rows: 68, level: 20, ..stream }),
            (3, 3),
            "never less than the stream refers to"
        );
        assert_eq!(dpb_size(&Sequence { buffering: Some(6), reorder: Some(2), ..stream }), (6, 2));
        assert_eq!(dpb_size(&Sequence { buffering: Some(1), reorder: Some(9), ..stream }), (3, 3));
        assert_eq!(dpb_size(&Sequence { references: 0, buffering: Some(0), ..stream }), (1, 1));
        let small = Sequence { columns: 11, rows: 9, level: 11, profile: 66, ..stream };
        assert_eq!(
            (dpb_size(&small).0, dpb_size(&Sequence { third_constraint: true, ..small.clone() }).0),
            (9, 4),
            "level 1b holds 396 macroblocks where 1.1 holds 900"
        );
        assert_eq!(
            dpb_size(&Sequence { columns: 240, rows: 135, level: 99, ..stream }).0,
            16,
            "a level beyond the table counts as the highest"
        );
    }
}

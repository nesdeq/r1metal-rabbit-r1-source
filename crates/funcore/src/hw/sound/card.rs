//! The sound card as r1metal sets it: its controls and the kernel's buffering of its streams.

use super::loudness;
use super::volume::{self, UNITY};
use crate::base::bytes::{le32, until_zero};
use crate::base::time::MICROS_PER_SECOND;
use crate::codec::aac;
use alloc::format;
use alloc::string::String;
use core::ops::Range;

const ENUMERATED: i32 = 3;
const ITEMS_AT: usize = 0;
const NAME_AT: Range<usize> = 8..72;
/// The size of the value union of the kernel's `snd_ctl_elem_info`.
pub const DESCRIPTION_BYTES: usize = 128;
const SPEAKER: &str = "Speaker_Amp_Switch";
const SPEAKER_ON: [(&str, &str); 2] = [("Lineout_PGAR_GAIN", "-2Db"), (SPEAKER, "On")];
const SPEAKER_OFF: [(&str, &str); 1] = [(SPEAKER, "Off")];
const MICROPHONE_ON: [(&str, &str); 5] = [
    ("Audio_MIC1_Mode_Select", "DCCMODE"),
    ("Audio_MicSource1_Setting", "ADC1"),
    ("Audio_PGA1_Setting", "24Db"),
    ("Audio_ADC_1_Switch", "On"),
    ("Audio_Preamp1_Switch", "IN_ADC1"),
];
const MICROPHONE_OFF: [(&str, &str); 2] = [("Audio_Preamp1_Switch", "OPEN"), ("Audio_ADC_1_Switch", "Off")];

/// The controls that switch the speaker, each with its value, in their order.
pub const fn speaker(is_on: bool) -> &'static [(&'static str, &'static str)] {
    if is_on { &SPEAKER_ON } else { &SPEAKER_OFF }
}

/// The controls that switch the main microphone, each with its value, in their order.
pub const fn microphone(is_on: bool) -> &'static [(&'static str, &'static str)] {
    if is_on { &MICROPHONE_ON } else { &MICROPHONE_OFF }
}

/// The card's controls, each method one call of the kernel's.
pub trait Controls {
    type Error;
    fn names(&self) -> impl Iterator<Item = &[u8]>;
    // The control's kind, and its description with the name of that item.
    fn describe(&self, control: usize, item: u32) -> Result<(i32, [u8; DESCRIPTION_BYTES]), Self::Error>;
    fn write(&self, control: usize, item: u32) -> Result<(), Self::Error>;
    // The error of a control or a name the card does not have.
    fn missing(words: String) -> Self::Error;
    // The error of a control that is no list of names.
    fn invalid(words: String) -> Self::Error;
}

/// Sets a control that is a list of names to the item named `value`.
pub fn set_control<C: Controls>(card: &C, control: &str, value: &str) -> Result<(), C::Error> {
    let to_missing = |name: &str| C::missing(format!("the sound card has no {name}"));
    let index = card.names().position(|name| name == control.as_bytes()).ok_or_else(|| to_missing(control))?;
    let (kind, first) = card.describe(index, 0)?;
    if kind != ENUMERATED {
        return Err(C::invalid(format!("{control} is no list of names")));
    }
    for item in 0..le32(&first, ITEMS_AT).unwrap_or(0) {
        if until_zero(&card.describe(index, item)?.1[NAME_AT]) == value.as_bytes() {
            return card.write(index, item);
        }
    }
    Err(to_missing(&format!("{value} for {control}")))
}

/// One of the card's two streams.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Out,
    In,
}

/// The samples of a period of either stream: a packet's frame, 21 ms.
pub const PERIOD: usize = aac::FRAME;

/// The periods the kernel holds: 8, 171 ms, recorded, the most the audio SRAM takes beside the 4, 85 ms, to play.
pub const fn periods(direction: Direction) -> usize {
    match direction {
        Direction::In => 8,
        Direction::Out => 4,
    }
}

// The samples the kernel holds of a stream; of the speaker's, the longest delay of a block's sound.
pub(super) const fn buffer_samples(direction: Direction) -> usize {
    PERIOD * periods(direction)
}

// The kernel's tick (`CONFIG_HZ`), and the jiffies a softirq's pass keeps a CPU from a real-time thread woken on it at the most: its network receive 2 (net/core/dev.c, `netdev_budget_usecs`) and the rest one more (kernel/softirq.c, `MAX_SOFTIRQ_TIME`).
pub(super) const KERNEL_HZ: usize = 250;
const HOLD_JIFFIES: usize = 3;
const KERNEL_HOLD_US: usize = HOLD_JIFFIES * MICROS_PER_SECOND as usize / KERNEL_HZ;
const PERIOD_US: usize = PERIOD * MICROS_PER_SECOND as usize / aac::RATE as usize;

/// The periods the kernel holds of a call's speaker, 64 ms.
pub const CALL_PERIODS: usize = 3;
// Of the periods queued as a block is written, one outlasts the softirqs' hold, the one bound the kernel's sources give; the other is for what none bounds: hard and threaded interrupts, real-time threads above the speaker's, idle exit and a block's peak work, as the card's line of real calls tells the least the ring held.
const _: () = assert!((CALL_PERIODS - 2) * PERIOD_US > KERNEL_HOLD_US);

// The samples the kernel holds of a call's speaker, the longest delay of its block's sound.
pub(super) const CALL_BUFFER: usize = PERIOD * CALL_PERIODS;

/// A stream's software parameters, in samples.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Thresholds {
    pub wake_at: u64,
    pub start_at: u64,
    pub stop_at: u64,
    pub boundary: u64,
    pub silence: u64,
}

/// A stream's parameters for a ring of that many periods: playback starts with its first period and clears what was fetched, capture starts when told.
pub fn thresholds(direction: Direction, periods: usize) -> Thresholds {
    let (period, held) = (PERIOD as u64, (PERIOD * periods) as u64);
    // The kernel's own count, with its `long` for the limit: a silence size of the boundary is only taken as one when it is the kernel's.
    let mut boundary = held;
    while boundary * 2 <= isize::MAX as u64 - held {
        boundary *= 2;
    }
    Thresholds {
        wake_at: period,
        start_at: if direction == Direction::Out { period } else { held },
        stop_at: held,
        boundary,
        // A silence size of the boundary, which takes no threshold, has the kernel clear only what the fetch pointer has passed, so nothing ahead of the last sample written is touched while a writer copies into it.
        silence: if direction == Direction::Out { boundary } else { 0 },
    }
}

/// The level a block plays at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gain {
    Volume,
    Sounds,
}

/// The gains and the holders of the microphone and the speaker, shared by every thread of the card.
#[derive(Debug)]
pub struct CardState {
    volume: u32,
    sounds: u32,
    is_microphone_held: bool,
    // The largest sample of the block the microphone recorded last, nought once it is let go.
    peak: u32,
    is_speaker_held: bool,
    speaker_waiters: u32,
}

impl Default for CardState {
    // A card nobody holds, at full gain.
    fn default() -> CardState {
        CardState {
            volume: UNITY,
            sounds: UNITY,
            is_microphone_held: false,
            peak: 0,
            is_speaker_held: false,
            speaker_waiters: 0,
        }
    }
}

impl CardState {
    /// Takes the volume and the level of the interface's sounds, in percent.
    pub fn set_levels(&mut self, volume: u8, sounds: u8) {
        (self.volume, self.sounds) = (volume::gain(volume), volume::gain(sounds));
    }

    pub const fn gain(&self, gain: Gain) -> u32 {
        match gain {
            Gain::Volume => self.volume,
            Gain::Sounds => self.sounds,
        }
    }

    /// Takes the microphone; none while another holds it.
    pub fn take_microphone(&mut self) -> Option<()> {
        let is_free = !self.is_microphone_held;
        self.is_microphone_held |= is_free;
        is_free.then_some(())
    }

    pub const fn free_microphone(&mut self) {
        (self.is_microphone_held, self.peak) = (false, 0);
    }

    /// Takes the largest sample of a block the microphone recorded; true where the steps its level lights change.
    pub fn hear(&mut self, loudest: u32) -> bool {
        loudness(core::mem::replace(&mut self.peak, loudest)) != loudness(loudest)
    }

    /// The largest sample of the block the microphone recorded last.
    pub const fn peak(&self) -> u32 {
        self.peak
    }

    /// Counts a player waiting for the speaker.
    pub const fn await_speaker(&mut self) {
        self.speaker_waiters += 1;
    }

    /// Counts a player out that never came to wait.
    pub const fn stop_awaiting(&mut self) {
        self.speaker_waiters = self.speaker_waiters.saturating_sub(1);
    }

    /// Takes the speaker for a waiting player; none while another holds it.
    pub fn take_awaited_speaker(&mut self) -> Option<()> {
        let is_free = self.take_speaker();
        self.speaker_waiters -= u32::from(is_free);
        is_free.then_some(())
    }

    // Takes the speaker; false while another holds it.
    const fn take_speaker(&mut self) -> bool {
        let is_free = !self.is_speaker_held;
        self.is_speaker_held |= is_free;
        is_free
    }

    /// Takes the speaker for a sound played aside; false while another holds it or a player waits for it.
    pub const fn take_speaker_aside(&mut self) -> bool {
        self.speaker_waiters == 0 && self.take_speaker()
    }

    pub const fn free_speaker(&mut self) {
        self.is_speaker_held = false;
    }

    /// True while no player waits for the speaker a sound played aside holds.
    pub const fn may_keep_speaker(&self) -> bool {
        self.speaker_waiters == 0
    }

    const fn is_off(&self) -> bool {
        !self.is_microphone_held && !self.is_speaker_held
    }

    /// True while nothing of the card speaks against a suspend: it is off, and no player waits for the speaker.
    pub const fn is_quiet(&self) -> bool {
        self.is_off() && self.speaker_waiters == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::sound::PEAK_FLOOR;
    use core::cell::Cell;

    struct Card {
        kind: i32,
        item: Cell<Option<u32>>,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Refusal {
        Missing(String),
        Invalid(String),
    }

    impl Controls for Card {
        type Error = Refusal;

        fn names(&self) -> impl Iterator<Item = &[u8]> {
            [&b"Volume"[..], b"Switch"].into_iter()
        }

        fn describe(&self, _: usize, item: u32) -> Result<(i32, [u8; DESCRIPTION_BYTES]), Refusal> {
            let mut description = [0; DESCRIPTION_BYTES];
            description[ITEMS_AT] = 2;
            description[NAME_AT.start..NAME_AT.start + 3].copy_from_slice(if item == 1 { b"On\0" } else { b"Off" });
            Ok((self.kind, description))
        }

        fn write(&self, _: usize, item: u32) -> Result<(), Refusal> {
            self.item.set(Some(item));
            Ok(())
        }

        fn missing(words: String) -> Refusal {
            Refusal::Missing(words)
        }

        fn invalid(words: String) -> Refusal {
            Refusal::Invalid(words)
        }
    }

    #[test]
    fn item_by_name() {
        let card = Card { kind: ENUMERATED, item: Cell::new(None) };
        assert_eq!((set_control(&card, "Switch", "On"), card.item.get()), (Ok(()), Some(1)));
        assert_eq!(set_control(&card, "Mute", "On"), Err(Refusal::Missing("the sound card has no Mute".into())));
        assert_eq!(
            set_control(&card, "Switch", "Half"),
            Err(Refusal::Missing("the sound card has no Half for Switch".into()))
        );
        let numbers = Card { kind: 2, item: Cell::new(None) };
        assert_eq!(set_control(&numbers, "Volume", "On"), Err(Refusal::Invalid("Volume is no list of names".into())));
    }

    #[test]
    fn single_holders() {
        let mut card = CardState::default();
        assert!(card.is_off() && card.take_microphone().is_some() && card.take_microphone().is_none());
        assert!(card.hear(PEAK_FLOOR), "a first step of the microphone's level lights");
        assert!(!card.hear(PEAK_FLOOR + 1), "a block that lights the same steps is no news");
        assert_eq!(card.peak(), PEAK_FLOOR + 1, "and is kept all the same");
        card.free_microphone();
        assert_eq!(card.peak(), 0, "a microphone let go hears nothing");
        card.await_speaker();
        assert!(!card.may_keep_speaker() && card.take_awaited_speaker().is_some() && card.may_keep_speaker());
        assert!(!card.take_speaker() && !card.is_off());
        card.await_speaker();
        assert!(card.take_awaited_speaker().is_none() && !card.may_keep_speaker(), "the waiter stays one");
        card.free_speaker();
        assert!(card.take_awaited_speaker().is_some() && card.may_keep_speaker());
        card.free_speaker();
        card.await_speaker();
        assert!(card.is_off() && !card.is_quiet(), "a sound asked for keeps the device up before it has the speaker");
        card.stop_awaiting();
        assert!(card.is_quiet(), "one that never came to wait counts no more");
    }

    #[test]
    fn aside_gives_way() {
        let may_keep = |speaker_waiters| CardState { speaker_waiters, ..CardState::default() }.may_keep_speaker();
        assert_eq!([0, 1, 3].map(may_keep), [true, false, false]);
        let mut card = CardState::default();
        card.await_speaker();
        assert!(!card.take_speaker_aside() && card.is_off(), "nor takes the speaker a player waits for");
        assert!(card.take_awaited_speaker().is_some() && !card.take_speaker_aside(), "or holds");
        card.free_speaker();
        assert!(card.take_speaker_aside() && !card.take_speaker_aside(), "a free one it takes, once");
    }

    #[test]
    fn control_order() {
        assert_eq!(speaker(true), [("Lineout_PGAR_GAIN", "-2Db"), ("Speaker_Amp_Switch", "On")]);
        assert_eq!(speaker(false), [("Speaker_Amp_Switch", "Off")], "the line's gain stays as it is");
        assert_eq!(microphone(true).last(), Some(&("Audio_Preamp1_Switch", "IN_ADC1")));
        assert_eq!(microphone(false).first(), Some(&("Audio_Preamp1_Switch", "OPEN")), "and it is the first off");
    }

    #[test]
    fn kernel_buffers() {
        assert_eq!((periods(Direction::In), periods(Direction::Out), PERIOD), (8, 4, 1024));
        assert_eq!(
            (buffer_samples(Direction::In) + buffer_samples(Direction::Out)) * size_of::<i16>(),
            6 * 4096,
            "the six blocks of 4096 bytes of the audio SRAM"
        );
        assert_eq!(buffer_samples(Direction::Out), 4096, "the most samples between a block handed over and its sound");
    }

    #[test]
    fn stream_thresholds() {
        let (out, into) =
            (thresholds(Direction::Out, periods(Direction::Out)), thresholds(Direction::In, periods(Direction::In)));
        assert_eq!(
            (out.start_at, out.stop_at, out.wake_at),
            (1024, 4096, 1024),
            "playback starts with its first period"
        );
        assert_eq!((into.start_at, into.stop_at), (8192, 8192), "capture when it is told to");
        assert_eq!(
            out.silence, out.boundary,
            "playback has the kernel clear what the fetch pointer has passed: a silence size of the boundary takes no threshold"
        );
        assert_eq!(into.silence, 0, "capture asks for no silence");
        assert_eq!(
            (out.boundary, into.boundary),
            (4096 << 50, 8192 << 49),
            "the largest power of two times what is held that the kernel's long takes, which is how the kernel works it out"
        );
        let call = thresholds(Direction::Out, CALL_PERIODS);
        assert_eq!(
            (call.start_at, call.stop_at, call.wake_at, call.silence, call.boundary),
            (1024, 3072, 1024, 3072 << 51, 3072 << 51),
            "a call's speaker holds three periods, 64 ms, and starts with its first"
        );
    }
}

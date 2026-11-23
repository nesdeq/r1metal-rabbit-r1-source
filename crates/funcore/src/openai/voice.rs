// The realtime models' voice on its way down: pieces of 16 bit samples in base64 at the model's rate, brought to the card's.

use super::models::{interpreter, reader};
use crate::base::code::unbase64_onto;
use crate::base::json::unescape;
use crate::codec::aac;
use crate::hw::sound::resample::Doubler;
use alloc::borrow::Cow;
use alloc::vec::Vec;

// The doubler brings a live voice to the card's rate only from half of it.
const _: () = assert!(2 * interpreter::RATE == aac::RATE && 2 * reader::RATE == aac::RATE);

#[derive(Default)]
pub(crate) struct Downlink {
    bytes: Vec<u8>,
    doubler: Doubler,
    at_model_rate: Vec<i16>,
    at_card_rate: Vec<i16>,
}

impl Downlink {
    pub(crate) fn take(&mut self, piece: &str) -> bool {
        let mut at_model_rate = core::mem::take(&mut self.at_model_rate);
        at_model_rate.clear();
        let is_sound = self.decode(piece, &mut at_model_rate);
        if is_sound {
            self.at_card_rate.clear();
            self.doubler.double(&at_model_rate, &mut self.at_card_rate);
        }
        self.at_model_rate = at_model_rate;
        is_sound
    }

    pub(crate) const fn model_samples(&self) -> u64 {
        self.at_model_rate.len() as u64
    }

    pub(crate) fn voice(&self) -> &[i16] {
        &self.at_card_rate
    }

    pub(crate) fn clear_voice(&mut self) {
        self.at_card_rate.clear();
    }

    fn decode(&mut self, piece: &str, samples: &mut Vec<i16>) -> bool {
        self.bytes.clear();
        // A writer of JSON may escape the stroke that base64 uses.
        let text = if piece.contains('\\') { Cow::Owned(unescape(piece)) } else { Cow::Borrowed(piece) };
        if !unbase64_onto(&text, &mut self.bytes) || !self.bytes.len().is_multiple_of(2) {
            return false;
        }
        samples.extend(self.bytes.as_chunks::<2>().0.iter().copied().map(i16::from_le_bytes));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_down() {
        let (mut downlink, mut samples) = (Downlink::default(), alloc::vec![5i16]);
        assert!(downlink.decode("AAABAP//AAE=", &mut samples));
        assert_eq!(samples, [5, 0, 1, -1, 256], "added behind what waits to be played");
        assert!(
            downlink.decode("AAABAP\\/\\/AAE=", &mut samples) && samples.len() == 9,
            "an escaped stroke is a stroke"
        );
        assert!(
            !downlink.decode("AAAB", &mut samples) && !downlink.decode("not sound", &mut samples) && samples.len() == 9,
            "half a sample, or no base64 at all, is no sound"
        );
        assert!(downlink.decode("", &mut samples) && samples.len() == 9);
        assert!(
            downlink.take("AAABAA==") && (downlink.model_samples(), downlink.voice().len()) == (2, 4),
            "two samples of the card's for each of the model's"
        );
        assert!(!downlink.take("AAAB") && downlink.voice().len() == 4, "what is no sound leaves the last voice");
        downlink.clear_voice();
        assert!(downlink.voice().is_empty());
    }
}

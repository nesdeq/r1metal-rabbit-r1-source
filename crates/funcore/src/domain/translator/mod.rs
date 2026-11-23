//! The translator's microphone stream toward the service.

pub mod session;
pub mod state;

use crate::codec::aac;
use crate::hw::sound::record::Settle;
use crate::hw::sound::{PEAK_FLOOR, peak, to_span};
use crate::openai::models::interpreter;
use alloc::collections::VecDeque;
use core::iter::repeat_n;
use core::time::Duration;

const ECHO: Duration = Duration::from_millis(250);

/// The microphone's stream, its settling samples replaced by silence.
pub struct Uplink {
    settle: Settle,
}

impl Default for Uplink {
    fn default() -> Uplink {
        Uplink { settle: Settle::new(interpreter::RATE) }
    }
}

impl Uplink {
    /// Queues a microphone block.
    pub fn add_samples(&mut self, samples: &[i16], queue: &mut VecDeque<i16>) {
        let silence = self.settle.take(samples.len());
        queue.extend(repeat_n(0, silence));
        queue.extend(&samples[silence..]);
    }

    /// Queues silence for lost samples.
    pub fn add_lost(samples: usize, queue: &mut VecDeque<i16>) {
        queue.extend(repeat_n(0, samples));
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlockKind {
    // The model's voice, and how long from now the microphone counts as hearing it.
    Voice(Duration),
    Empty,
    // The service's silence, and its peak.
    Silence(u16),
}

// The service sends one unbroken stream, its silence too.
fn classify_block(block: &[i16], pending: usize) -> BlockKind {
    let loudest = peak(block);
    if loudest == 0 {
        BlockKind::Empty
    } else if u32::from(loudest) < PEAK_FLOOR {
        BlockKind::Silence(loudest)
    } else {
        BlockKind::Voice(to_span((pending + block.len()) as u64, aac::RATE) + ECHO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::sound::record::SETTLE_SAMPLES;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn settling_and_lost_as_silence() {
        let (mut uplink, mut queue) = (Uplink::default(), VecDeque::new());
        let settling = SETTLE_SAMPLES / 2;
        uplink.add_samples(&vec![5; settling - 10], &mut queue);
        uplink.add_samples(&[5; 30], &mut queue);
        assert_eq!(queue.len(), settling + 20);
        assert!(queue.iter().take(settling).all(|&sample| sample == 0));
        assert!(queue.iter().skip(settling).all(|&sample| sample == 5));
        Uplink::add_lost(7, &mut queue);
        uplink.add_samples(&[5; 3], &mut queue);
        assert_eq!(queue.iter().rev().take(10).copied().collect::<Vec<i16>>(), [5, 5, 5, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn block_kinds() {
        assert_eq!(classify_block(&[0; 8], 0), BlockKind::Empty);
        assert_eq!(classify_block(&[3, -63, 1], 0), BlockKind::Silence(63));
        assert_eq!(classify_block(&vec![-64; 1024], 3072), BlockKind::Voice(Duration::from_nanos(85_333_333) + ECHO));
    }
}

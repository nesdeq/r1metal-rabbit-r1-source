//! The bytes going out to the host.

use super::wire::{LOG_PREFIX, Reply, frame};
use alloc::collections::VecDeque;

/// The bytes waiting for the port; a line of the device's own is dropped while the host lags.
#[derive(Default)]
pub struct Outbox {
    bytes: VecDeque<u8>,
    last_queued: bool,
}

impl Outbox {
    /// Queues a line of the device's own, unless bytes still wait.
    pub fn log(&mut self, text: &str) {
        if self.bytes.is_empty() {
            self.bytes.extend(frame(LOG_PREFIX, text));
        }
    }

    /// Queues a whole answer.
    pub fn answer(&mut self, reply: &Reply) {
        self.bytes.extend(reply.bytes());
    }

    /// The next run of bytes to send; empty while none wait.
    pub fn next(&self) -> &[u8] {
        self.bytes.as_slices().0
    }

    /// Drops the bytes that went out.
    pub fn take_sent(&mut self, count: usize) {
        self.bytes.drain(..count.min(self.bytes.len()));
    }

    /// Drops what waits, after a port failure.
    pub fn clear(&mut self) {
        self.bytes.clear();
    }

    /// True while bytes wait.
    pub fn has_queued(&self) -> bool {
        !self.bytes.is_empty()
    }

    /// Whether bytes wait, where that changed: the port is watched for room from then on, or no longer.
    pub fn take_change(&mut self) -> Option<bool> {
        let is_queued = self.has_queued();
        (is_queued != core::mem::replace(&mut self.last_queued, is_queued)).then_some(is_queued)
    }
}

#[cfg(test)]
mod tests {
    use super::super::wire::TOO_LONG;
    use super::*;

    #[test]
    fn order_and_dropped_lines() {
        let mut outbox = Outbox::default();
        assert_eq!((outbox.next(), outbox.has_queued()), (&[][..], false), "nothing waits");
        assert_eq!(outbox.take_change(), None, "which was so from the start");
        outbox.log("booted");
        let mut reply = Reply::default();
        reply.data("87");
        outbox.answer(reply.ok());
        assert_eq!(outbox.next(), b"# booted\r\n> 87\r\nok\r\n", "the lines in order, as the wire has them");
        outbox.log("meanwhile");
        assert!(
            !outbox.next().ends_with(b"meanwhile\r\n"),
            "a line of the device's own is dropped while the host lags"
        );
        assert_eq!(
            (outbox.take_change(), outbox.take_change()),
            (Some(true), None),
            "that something waits is told once"
        );
        outbox.take_sent(10);
        assert_eq!(outbox.next(), b"> 87\r\nok\r\n", "what went out is taken off");
        outbox.take_sent(usize::MAX);
        assert_eq!(
            (outbox.has_queued(), outbox.take_change()),
            (false, Some(false)),
            "and once all is out, that is told"
        );
        outbox.answer(Reply::default().err(TOO_LONG));
        outbox.clear();
        assert!(!outbox.has_queued(), "a port that failed lets go of what waits");
    }
}

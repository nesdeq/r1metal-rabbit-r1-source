//! The host's lines waiting behind a verb in progress.

use super::answer::keep_refusal;
use super::wire::{Line, Reply};
use crate::hw::power::Power;
use alloc::collections::VecDeque;
use alloc::string::String;

/// The waiting lines, whether a verb is in progress, whether the port hung up during it, and the first way down asked for.
#[derive(Default)]
pub struct Queue {
    lines: VecDeque<Line>,
    is_waiting: bool,
    is_stale: bool,
    power: Option<Power>,
}

impl Queue {
    /// Adds the host's lines behind those that wait.
    pub fn add(&mut self, lines: impl IntoIterator<Item = Line>) {
        self.lines.extend(lines);
    }

    /// The line to run or to refuse next; none while a verb is in progress.
    pub fn next_line(&mut self) -> Option<Line> {
        if self.is_waiting { None } else { self.lines.pop_front() }
    }

    const fn begin_wait(&mut self) {
        self.is_waiting = true;
    }

    /// Ends a command: one another thread answers holds the lines behind it, any other is answered here and the first way down stands.
    pub fn end_command(&mut self, needs_thread: bool, power: Option<Power>, reply: &mut Reply) {
        if needs_thread {
            self.begin_wait();
        } else {
            reply.ok();
            self.power = self.power.or(power);
        }
    }

    /// Takes the way down the lines asked for.
    pub const fn take_power(&mut self) -> Option<Power> {
        self.power.take()
    }

    /// The port hung up: the waiting lines go, and a verb's answer has no reader.
    pub fn note_hang_up(&mut self) {
        self.lines.clear();
        self.is_stale = self.is_waiting;
    }

    /// Ends the verb in progress; true where its answer goes to the host, false where the port hung up meanwhile.
    pub const fn take_answer(&mut self) -> bool {
        self.is_waiting = false;
        !core::mem::replace(&mut self.is_stale, false)
    }

    /// The answer to what keeping a key or a tunnel's config came to; none where the port hung up meanwhile.
    pub fn take_keep_answer(&mut self, outcome: &Result<(), String>) -> Option<Reply> {
        self.take_answer().then(|| {
            let mut reply = Reply::default();
            match outcome {
                Ok(()) => reply.ok(),
                Err(reason) => reply.err(&keep_refusal(reason)),
            };
            reply
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_wait_and_hang_up() {
        let mut queue = Queue::default();
        queue.add(["ls /data".into(), Line::TooLong, "ping".into()]);
        assert_eq!(queue.next_line(), Some("ls /data".into()));
        queue.begin_wait();
        assert_eq!(queue.next_line(), None, "the lines behind it wait, one dropped for its length among them");
        assert!(queue.take_answer(), "its answer goes to the host");
        assert_eq!(
            [queue.next_line(), queue.next_line(), queue.next_line()],
            [Some(Line::TooLong), Some("ping".into()), None],
            "each in its turn"
        );
        queue.add(["dmesg".into()]);
        queue.begin_wait();
        queue.note_hang_up();
        assert!(!queue.take_answer(), "an answer after the port hung up is for nobody");
        assert_eq!(queue.next_line(), None, "and what waited is let go");
        assert!(queue.take_answer(), "the next verb's answer goes out again");
    }

    #[test]
    fn end_command_and_first_power() {
        let (mut queue, mut reply) = (Queue::default(), Reply::default());
        queue.add(["ls /data".into()]);
        queue.end_command(false, Some(Power::Reboot), &mut reply);
        queue.end_command(false, Some(Power::PowerOff), &mut reply);
        assert_eq!(reply.bytes(), b"ok\r\nok\r\n");
        assert_eq!((queue.take_power(), queue.take_power()), (Some(Power::Reboot), None));
        queue.end_command(true, None, &mut reply);
        assert_eq!(reply.bytes(), b"ok\r\nok\r\n", "one that waits is answered by its thread");
        assert_eq!(queue.next_line(), None);
    }

    #[test]
    fn keep_answers() {
        let mut queue = Queue::default();
        let answer = queue.take_keep_answer(&Ok(())).expect("the host waits");
        assert_eq!(answer.bytes(), b"ok\r\n");
        let refusal = queue.take_keep_answer(&Err("no room".into())).expect("the host waits");
        assert_eq!(refusal.bytes(), b"err not kept: no room\r\n");
        queue.begin_wait();
        queue.note_hang_up();
        assert!(queue.take_keep_answer(&Ok(())).is_none());
    }
}

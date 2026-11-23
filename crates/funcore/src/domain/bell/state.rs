//! State shared by the threads that take a request said or typed to the bell and the main loop.

use super::{Bell, Change, parse_reply};
use crate::domain::cause::Cause;
use crate::domain::voice::{Changes, Request, Voice};
use crate::openai::fault::Fault;
use alloc::string::String;

/// The transcriber's prompt for a request said.
pub const PROMPT: &str =
    "A request to set or cancel an alarm, a reminder or a timer: a time, a day or a length, and what it is for.";

/// The bell as the interface holds it, with the minutes its clock is ahead of UTC: what a request is read against.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Standing(pub Bell, pub i16);

/// The job of a request said or typed to the bell: the changes it asks, which the interface makes.
pub type Setter = Request<Standing, Change>;
/// The job's report for the main loop.
pub type Report = Changes<Change>;

type State = Voice<Setter>;

impl State {
    /// The model's input for a request at `utc`.
    pub fn to_input(&self, request: &str, utc: u64) -> String {
        let Standing(bell, offset) = self.standing();
        bell.to_input(request, utc, *offset)
    }

    /// Takes the model's reply to a request, or why none came; true if the screen has news.
    pub fn take_reply(&mut self, reply: Result<String, Fault>) -> bool {
        match reply {
            Ok(reply) => self.keep_changes(parse_reply(&reply), Cause::NoAnswer),
            Err(fault) => self.fail(&fault),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::bell::Due;
    use crate::domain::voice::Phase;

    // Friday 2026-10-16, 22:15 UTC.
    const FRIDAY: u64 = 1_792_188_900;

    #[test]
    fn reply_becomes_changes() {
        let mut state = State::new(true);
        assert!(state.is_keyed(), "a request typed needs the key as one said does");
        state.set_standing(Standing(Bell::parse("entry span 1792189200 tea\n"), 120));
        assert_eq!(
            state.to_input(" wake me\nat seven ", FRIDAY),
            "The device's clock shows saturday 17 october 2026, 00:15.\nSet, by the device's clock:\n- timer, 2026-10-17 00:20, 5:00 left: tea\nRequest: wake me at seven",
            "the clock two hours ahead, what is set by it, and the request on one line"
        );
        let reply = "set_reminder {\"on\":null,\"at\":\"07:00\",\"in_seconds\":null,\"text\":null}\ncancel_reminder {\"which\":\"tea\"}";
        assert!(state.take_reply(Ok(reply.into())));
        let report = state.take_report();
        assert_eq!(report.phase, Phase::Idle);
        assert_eq!(
            report.changes,
            [
                Change::Set { due: Due::Clock { day: None, minute: 420 }, text: String::new() },
                Change::Cancel("tea".into())
            ]
        );
        assert!(state.take_report().changes.is_empty(), "told once");
    }

    #[test]
    fn nothing_asked_or_no_reply() {
        let mut state = State::new(true);
        assert!(state.take_reply(Ok("NOTHING".into())));
        assert_eq!(
            state.take_report().phase,
            Phase::Failed(Cause::NoAnswer),
            "a request that asks nothing of the bell"
        );
        assert!(state.take_reply(Err(Fault::Offline("asleep".into()))));
        let report = state.take_report();
        assert!(report.phase == Phase::Failed(Cause::NoNetwork) && report.changes.is_empty());
    }
}

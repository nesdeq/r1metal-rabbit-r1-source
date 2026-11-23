// The voice notes of Signal's conversations, played one at a time on a thread of their own: what the screen wants played.

use crate::base::handoff::Latch;
use alloc::string::String;

// What the voice notes' thread and the main loop share.
pub(super) struct State {
    wanted: Latch<Option<u32>>,
    asked: Option<Option<(String, u32)>>,
}

impl Default for State {
    fn default() -> State {
        State { wanted: Latch::with(None), asked: None }
    }
}

impl State {
    // Takes the voice note the screen wants played, by its file and the number of its playing, once for every number; none stops the one that plays. True where that is news for the thread.
    pub(super) fn want(&mut self, wanted: Option<(&str, u32)>) -> bool {
        if self.wanted.change(wanted.map(|(_, round)| round)).is_none() {
            return false;
        }
        self.asked = Some(wanted.map(|(file, round)| (String::from(file), round)));
        true
    }

    // The newest order for the thread: the voice note to play from its beginning in place of any other, or none to stop; none while there is no order.
    pub(super) fn take_asked(&mut self) -> Option<Option<(String, u32)>> {
        self.asked.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_number_asked_once() {
        let mut state = State::default();
        assert!(!state.want(None) && state.take_asked().is_none(), "nothing plays, nothing is stopped");
        assert!(state.want(Some(("a.m4a", 3))) && !state.want(Some(("a.m4a", 3))));
        assert_eq!(state.take_asked(), Some(Some((String::from("a.m4a"), 3))));
        assert_eq!(state.take_asked(), None, "once");
        assert!(state.want(Some(("a.m4a", 4))), "the same note again is another playing");
        assert!(state.want(None) && state.take_asked() == Some(None), "stopped before the thread took the first");
    }
}

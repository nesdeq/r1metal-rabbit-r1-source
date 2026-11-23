use super::Shared;
use crate::openai::{Account, write};
use crate::{log, sys};
use r1_funcore::domain::signal::state::State;

pub(super) fn write_turns(shared: &Shared, account: &Account) -> ! {
    loop {
        let (contact, request) = shared.board.wait(State::take_turn);
        let text = write::write_auto_reply(account, &mut None, &request)
            .inspect_err(|fault| log!("signal: auto mode's turn: {fault}"))
            .ok();
        shared.board.lock().take_auto_answer(contact, text.as_deref(), sys::boottime_ms());
        shared.wake_thread();
        shared.board.raise();
    }
}

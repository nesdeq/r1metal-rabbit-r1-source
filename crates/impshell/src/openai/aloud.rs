use super::{Account, Session};
use crate::wifi::radio::NetworkDown;
use r1_funcore::openai::aloud;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::ledger::Kind;
use r1_funcore::openai::models::reader::{ADDRESS, TIMEOUT};
use std::io;

/// A reader's session: the realtime model's voice for passage after passage of one task, which its messages are made with.
pub type ReadingSession = Session<String>;

impl ReadingSession {
    /// Opens a session in the stay `down` and makes it a reader's of `task`: a passage can be asked for once the service says so.
    pub fn open(account: &Account, down: &NetworkDown, task: &str) -> Result<ReadingSession, Fault> {
        Session::reach(account, ADDRESS, down, TIMEOUT, task.into())?
            .begin(&aloud::session_update(task), aloud::SESSION_UPDATED)
    }

    #[cfg(test)]
    pub fn with_socket(socket: crate::net::websocket::Socket, task: &str) -> ReadingSession {
        Session { socket, context: task.into() }
    }

    /// Asks for a passage by its number in the text.
    pub fn ask(&mut self, passage: usize, words: &str) -> io::Result<()> {
        self.send(&aloud::response_create(passage, &self.context, words))
    }

    /// Asks the service to stop the passage on its way, by its response's name.
    pub fn stop(&mut self, response: &str) -> io::Result<()> {
        self.send(&aloud::response_cancel(response))
    }
}

/// Books a reading of that kind, all its passages together, at `micros` millionths of a dollar.
pub fn book_reading(account: &Account, kind: Kind, micros: u64) {
    account.add(|ledger, year, month| {
        ledger.add_reading(year, month, kind, micros);
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_key_no_session() {
        let account = crate::openai::tests::unusable();
        assert!(matches!(
            ReadingSession::open(&account, &crate::wifi::radio::tests::up(), aloud::TASK),
            Err(Fault::Temporary(_))
        ));
        book_reading(&account, Kind::NewsReading, 0);
    }
}

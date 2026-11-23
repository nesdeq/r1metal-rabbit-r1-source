use super::{Account, Session};
use crate::wifi::radio::NetworkDown;
use r1_funcore::net::websocket::Kind;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::live::{self, Uplink};
use r1_funcore::openai::models::interpreter::{ADDRESS, TIMEOUT};
use std::io;

/// A translator's session: the microphone's sound up, the live model's voice and words down.
pub type Interpreter = Session<Uplink>;

impl Interpreter {
    /// Opens a session in the stay `down` and starts it.
    pub fn open(account: &Account, down: &NetworkDown) -> Result<Interpreter, Fault> {
        Session::reach(account, ADDRESS, down, TIMEOUT, Uplink::default())?
            .begin(&live::session_start(), live::SESSION_STARTED)
    }

    /// Has the model introduce itself.
    pub fn greet(&mut self) -> io::Result<()> {
        self.send(&live::greeting())
    }

    pub fn send_samples(&mut self, samples: &[i16]) -> io::Result<()> {
        self.socket.send(Kind::Text, self.context.encode(samples).as_bytes())
    }

    /// Asks the service to end the session; it answers with the seconds paid for.
    pub fn end(&mut self) -> io::Result<()> {
        self.send(live::SESSION_CLOSE)
    }

    /// Hands work the model delegated back to it by name.
    pub fn hand_back(&mut self, work: &str) -> io::Result<()> {
        self.send(&live::hand_back(work))
    }
}

/// Books a session of `seconds` where it cost anything; returns the cost in millionths of a dollar.
pub fn book_session(account: &Account, seconds: u64) -> u64 {
    let micros = live::session_micros(seconds);
    account.add(|ledger, year, month| ledger.add_session(year, month, micros));
    micros
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_key_no_session() {
        let account = crate::openai::tests::unusable();
        assert!(matches!(Interpreter::open(&account, &crate::wifi::radio::tests::up()), Err(Fault::Temporary(_))));
        assert_eq!(
            (book_session(&account, 60), book_session(&account, 0)),
            (50_000, 0),
            "five cents a minute, and nothing for nothing"
        );
    }
}

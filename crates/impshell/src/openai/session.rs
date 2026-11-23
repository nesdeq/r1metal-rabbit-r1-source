use super::Account;
use crate::net::websocket::{Incoming, Socket};
use crate::wifi::radio::NetworkDown;
use r1_funcore::net::websocket::{Kind, Message};
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::refusal::{NO_SESSION, session_status};
use r1_funcore::openai::request::parse_target;
use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

/// A WebSocket session with a service, opened with the stored key: text messages both ways, and the context they are made with.
pub struct Session<S> {
    pub(super) socket: Socket,
    pub(super) context: S,
}

impl<S> Session<S> {
    pub(super) fn reach(
        account: &Account,
        address: &str,
        down: &NetworkDown,
        timeout: Duration,
        context: S,
    ) -> Result<Session<S>, Fault> {
        let (key, target) = (account.paying()?, parse_target(address)?);
        Ok(Session { socket: key.open(&target, timeout, down)?, context })
    }

    pub(super) fn begin(mut self, request: &str, ready: &str) -> Result<Session<S>, Fault> {
        let result = (|| -> Result<(), Fault> {
            self.send(request)?;
            loop {
                let Incoming::Message(message) = self.next(true)? else {
                    return Err(Fault::Temporary(NO_SESSION.into()));
                };
                let verdict = session_status(&message, ready);
                self.give_back(message);
                if let Some(verdict) = verdict {
                    return verdict.map_err(|refusal| Fault::from_refusal(None, &refusal));
                }
            }
        })();
        match result {
            Ok(()) => Ok(self),
            Err(fault) => {
                self.close();
                Err(fault)
            }
        }
    }

    pub(super) fn send(&mut self, message: &str) -> io::Result<()> {
        self.socket.send(Kind::Text, message.as_bytes())
    }

    pub(super) fn next(&mut self, should_wait: bool) -> io::Result<Incoming<String>> {
        self.socket.next_message(should_wait, Message::text)
    }

    /// What `parse` makes of the service's next whole message, or of none once the service has closed, without waiting for the network; none while nothing is whole.
    pub fn read_message<T>(&mut self, parse: impl FnOnce(Option<&str>) -> T) -> io::Result<Option<T>> {
        let message = match self.next(false)? {
            Incoming::Message(message) => message,
            Incoming::Nothing => return Ok(None),
            Incoming::Closed => return Ok(Some(parse(None))),
        };
        let result = parse(Some(&message));
        self.give_back(message);
        Ok(Some(result))
    }

    pub(super) fn give_back(&mut self, buffer: String) {
        self.socket.give_back(buffer.into_bytes());
    }

    /// The underlying socket, readable when the service may have sent something.
    pub fn fd(&self) -> RawFd {
        self.socket.fd()
    }

    /// Sets how long each wait on the service may take.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.socket.set_timeout(timeout);
    }

    /// Sends the close frame and ends the session.
    pub fn close(self) {
        self.socket.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::served::peer;
    use std::thread::JoinHandle;

    fn session_with(messages: &'static [&'static str]) -> (Session<()>, JoinHandle<String>) {
        let (socket, server) = peer(move |peer| {
            let first = peer.read_text().unwrap();
            messages.iter().for_each(|message| peer.write_text(message));
            if !messages.is_empty() {
                peer.read_text();
            }
            first
        });
        (Session { socket, context: () }, server)
    }

    #[test]
    fn begin_waits_for_ready_or_refusal() {
        let (session, server) = session_with(&[r#"{"type":"hello"}"#, r#"{"type":"ready"}"#]);
        let mut session = session.begin("begin", "ready").unwrap();
        assert_eq!(
            session.next(false).unwrap(),
            Incoming::Nothing,
            "what came before its word was passed over, and nothing since"
        );
        session.close();
        assert_eq!(server.join().unwrap(), "begin", "what begins it goes first");
        let (session, server) = session_with(&[r#"{"type":"error","error":{"code":"invalid_value","message":"No."}}"#]);
        assert!(
            matches!(session.begin("begin", "ready"), Err(Fault::Refused(text)) if text == "invalid_value: No."),
            "refused, with the service's words"
        );
        server.join().unwrap();
        let (session, server) = session_with(&[]);
        assert!(matches!(session.begin("begin", "ready"), Err(Fault::Temporary(_))), "closed before it stood");
        server.join().unwrap();
    }
}

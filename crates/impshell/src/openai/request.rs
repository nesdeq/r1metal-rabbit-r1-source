use super::Account;
use crate::net::tls::Stream;
use crate::net::web::{self, Answer};
use crate::net::websocket::{self, Socket};
use crate::sys::lock;
use crate::wifi::radio::NetworkDown;
use r1_funcore::codec::http::{self, GET, POST, READ_BYTES, Target};
use r1_funcore::net::tls::Purpose;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::key;
use r1_funcore::openai::models::TIMEOUT;
use r1_funcore::openai::refusal::{Refusal, parse_refusal};
use r1_funcore::openai::request::{Body, EVENTS, JSON, Step, check_length, compress_body, headers, session_headers};
use std::io::{self, Read};
use std::time::Duration;

struct Request<'a> {
    target: &'a Target,
    down: &'a NetworkDown,
    method: &'a str,
    accept: &'a str,
    kind: Option<&'a str>,
    body: &'a [u8],
}

fn reach(target: &Target, down: &NetworkDown) -> io::Result<Stream> {
    let mut stream = web::reach(target, Purpose::Http, down)?;
    stream.set_timeout(TIMEOUT);
    Ok(stream)
}

fn is_peer_close(error: &io::Error) -> bool {
    use io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    matches!(error.kind(), UnexpectedEof | ConnectionReset | BrokenPipe | ConnectionAborted)
}

fn read_whole(answer: &mut Answer<'_>) -> io::Result<Vec<u8>> {
    answer.read_checked(|bytes| check_length(bytes).map_err(io::Error::other))
}

fn open_refusal(error: &io::Error) -> Option<(u16, Refusal)> {
    let (status, body) = websocket::open_error(error)?;
    Some((status, parse_refusal(std::str::from_utf8(body).ok()?)?))
}

impl Account {
    fn connection(&self, target: &Target, down: &NetworkDown) -> io::Result<(Stream, bool)> {
        match self.0.pool.take(target, down) {
            Some(stream) => Ok((stream, true)),
            None => Ok((reach(target, down)?, false)),
        }
    }

    pub(super) fn prepare_connection(&self, target: &Target, down: &NetworkDown) -> io::Result<()> {
        let (stream, _) = self.connection(target, down)?;
        self.0.pool.put(target, stream);
        Ok(())
    }

    fn exchange(
        &self,
        (bearer, is_stored_key): (&str, bool),
        request: Request<'_>,
        mut on_step: impl FnMut(Step),
        mut read_body: impl FnMut(&mut Answer<'_>) -> io::Result<Vec<u8>>,
    ) -> io::Result<(u16, Vec<u8>)> {
        let Request { target, down, method, accept, kind, body } = request;
        let packed_body = compress_body(target, method, kind, body);
        let headers = headers(bearer, accept, kind, packed_body.is_some());
        let body = packed_body.as_deref().unwrap_or(body);
        let (mut stream, mut is_reused) = self.connection(target, down)?;
        loop {
            on_step(Step::Reached);
            let (error, is_sent) = match web::send(&mut stream, method, target, &headers, body) {
                Err(e) => (e, false),
                Ok(()) => {
                    on_step(Step::Sent);
                    match web::answer(&mut stream, method) {
                        Err(e) => (e, true),
                        Ok(mut answer) => {
                            let status = answer.head.status;
                            on_step(Step::Answered);
                            let result = read_body(&mut answer);
                            let is_open = result.is_ok() && answer.is_reusable();
                            drop(answer);
                            if is_open {
                                self.0.pool.put(target, stream);
                            } else {
                                stream.close();
                            }
                            let is_unknown = key::is_stored_unknown(is_stored_key, status, result.as_deref().ok());
                            lock(&self.0.state).note_refused(is_unknown);
                            return result.map(|body| (status, body));
                        }
                    }
                }
            };
            stream.close();
            if !http::should_retry(is_reused, is_peer_close(&error), is_sent, method) {
                return Err(error);
            }
            (stream, is_reused) = (reach(target, down)?, false);
        }
    }

    pub(super) fn check(&self, target: &Target, key: &str, down: &NetworkDown) -> io::Result<(u16, Vec<u8>)> {
        let bearer = key::to_bearer(key);
        let request = Request { target, down, method: GET, accept: JSON, kind: None, body: &[] };
        self.exchange((&bearer, false), request, |_| {}, read_whole)
    }
}

pub(super) struct Key {
    pub(super) bearer: String,
    pub(super) account: Account,
}

impl Key {
    // Asks and reads the answer whole, telling `on_step` how far the request has got.
    pub(super) fn ask(
        &self,
        (target, down): (&Target, &NetworkDown),
        method: &str,
        kind: Option<&str>,
        body: &[u8],
        on_step: impl FnMut(Step),
    ) -> io::Result<(u16, Vec<u8>)> {
        let request = Request { target, down, method, accept: JSON, kind, body };
        self.account.exchange((&self.bearer, true), request, on_step, read_whole)
    }

    pub(super) fn ask_stream(
        &self,
        (target, down): (&Target, &NetworkDown),
        kind: &str,
        body: &[u8],
        mut on_data: impl FnMut(&str),
    ) -> io::Result<(u16, Vec<u8>)> {
        let request = Request { target, down, method: POST, accept: EVENTS, kind: Some(kind), body };
        self.account.exchange(
            (&self.bearer, true),
            request,
            |_| {},
            |answer| {
                let (mut reader, mut piece) = (Body::new(answer.head.status), vec![0u8; READ_BYTES]);
                loop {
                    match answer.read(&mut piece)? {
                        0 => return Ok(reader.into_bytes()),
                        count => {
                            let events = reader.take(&piece[..count]).map_err(io::Error::other)?;
                            events.iter().for_each(|data| on_data(data));
                        }
                    }
                }
            },
        )
    }

    // Opens a session with the key; a refused opening is told to the account and comes back as the service's refusal.
    pub(super) fn open(&self, target: &Target, timeout: Duration, down: &NetworkDown) -> Result<Socket, Fault> {
        let result = Socket::reach(target, Purpose::Http, &session_headers(&self.bearer), timeout, down);
        result.map_err(|e| match open_refusal(&e) {
            Some((status, refusal)) => {
                self.account.note_refusal(Some(status), &refusal);
                Fault::from_refusal(Some(status), &refusal)
            }
            None => e.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::unusable;
    use super::*;
    use crate::net::served::{Connection, head, listen, serve_answers};
    use crate::wifi::radio::tests::up;
    use r1_funcore::codec::http::USER_AGENT;
    use r1_funcore::openai::request::{ANSWER_BYTES, TOO_LONG};
    use std::io::Write;
    use std::sync::mpsc;
    use std::thread;

    const FAKE: &str = "sk-proj-Fake0Key_-9";

    fn key_of(account: &Account) -> Key {
        Key { bearer: format!("Bearer {FAKE}"), account: account.clone() }
    }

    #[test]
    fn stream_by_event_refusal_whole() {
        let events = "event: response.created\ndata: {\"type\":\"response.created\"}\n\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}\n\n";
        let stream_answer =
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{events}");
        let refusal = "{\"error\":{\"message\":\"Slow down.\",\"code\":\"rate_limit_exceeded\"}}";
        let refused_answer = format!(
            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{refusal}",
            refusal.len()
        );
        let (target, receiver) =
            serve_answers("/v1/responses", vec![stream_answer.into_bytes(), refused_answer.into_bytes()]);
        let (down, account) = (up(), unusable());
        let key = key_of(&account);
        let mut data = Vec::new();
        let result = key.ask_stream((&target, &down), JSON, b"{}", |text| data.push(String::from(text)));
        assert_eq!(result.unwrap(), (200, Vec::new()));
        assert_eq!(
            data,
            ["{\"type\":\"response.created\"}", "{\"type\":\"response.output_text.delta\",\"delta\":\"Hi\"}"],
            "each event once, however the answer came in pieces"
        );
        let mut count = 0;
        let result = key.ask_stream((&target, &down), JSON, b"{}", |_| count += 1);
        assert_eq!(
            (result.unwrap(), count),
            ((429, refusal.as_bytes().to_vec()), 0),
            "a refusal is no stream: it comes whole"
        );
        let requests = receiver.recv().unwrap();
        assert!(
            requests[0].1.contains("\r\nAccept: text/event-stream\r\n")
                && requests[0].1.contains(&format!("\r\nAuthorization: Bearer {FAKE}\r\n"))
                && requests[0].2 == b"{}"
        );
        assert_eq!(
            requests.iter().map(|(number, ..)| *number).collect::<Vec<_>>(),
            [0, 1],
            "a stream its server closes leaves nothing open"
        );
        assert!(!account.take_refused(), "a service too busy knows the key");
    }

    #[test]
    fn overlong_answer_refused() {
        let (down, account) = (up(), unusable());
        let key = key_of(&account);
        let body = vec![b' '; ANSWER_BYTES + 1];
        let whole = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        let stream = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n".to_vec();
        let answers = vec![[whole, body.clone()].concat(), [stream, body].concat()];
        let (target, _requests) = serve_answers("/v1/responses", answers);
        let refused = |result: io::Result<(u16, Vec<u8>)>| result.map_err(|e| e.to_string());
        assert_eq!(
            refused(key.ask((&target, &down), GET, None, &[], |_| {})),
            Err(TOO_LONG.into()),
            "an answer a byte longer than any asked for is not taken"
        );
        assert_eq!(
            refused(key.ask_stream((&target, &down), JSON, b"{}", |_| {})),
            Err(TOO_LONG.into()),
            "nor a stream whose event has no end"
        );
    }

    #[test]
    fn stored_key_answers_reach_account() {
        let (down, account) = (up(), unusable());
        let key = key_of(&account);
        let refusal = |code: &str| {
            let body = format!("{{\"error\":{{\"message\":\"x\",\"code\":\"{code}\"}}}}");
            format!("HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
        };
        let (unknown, scope) = (refusal("invalid_api_key"), refusal("missing_scope"));
        let (target, receiver) = serve_answers("/v1/responses", vec![unknown.clone(), scope.clone(), unknown.clone()]);
        assert_eq!(account.check(&target, FAKE, &down).unwrap().0, 401);
        assert!(!account.take_refused(), "a key checked is none of the account's");
        assert_eq!(key.ask((&target, &down), POST, Some(JSON), b"{}", |_| {}).unwrap().0, 401);
        assert!(!account.take_refused(), "a key without the scope for a request is one the service knows");
        assert_eq!(key.ask((&target, &down), POST, Some(JSON), b"{}", |_| {}).unwrap().0, 401);
        assert!(account.take_refused(), "a request made with the kept key tells the account");
        let requests = receiver.recv().unwrap();
        assert!(
            requests[0].1.starts_with("GET /v1/responses HTTP/1.1\r\n")
                && requests[0].1.contains(&format!("\r\nUser-Agent: {USER_AGENT}\r\n"))
                && requests[0].1.contains(&format!("\r\nAuthorization: Bearer {FAKE}\r\n"))
                && requests[0].1.contains("\r\nAccept: application/json\r\n"),
            "{}",
            requests[0].1
        );
        assert!(
            requests[1].1.contains("\r\nContent-Type: application/json\r\n") && requests[1].2 == b"{}",
            "{}",
            requests[1].1
        );
        let (target, receiver) = serve_answers("/v1/responses", vec![scope, unknown]);
        let scope_fault = key.open(&target, TIMEOUT, &down).map(|_| ()).unwrap_err();
        assert_eq!(
            (scope_fault, account.take_refused()),
            (Fault::Refused("status 401, missing_scope, its words left out".into()), false),
            "a session the service opens none of says why"
        );
        let key_fault = key.open(&target, TIMEOUT, &down).map(|_| ()).unwrap_err();
        let requests = receiver.recv().unwrap();
        assert!(
            requests[0].1.contains("\r\nUpgrade: websocket\r\n")
                && requests[0].1.contains(&format!("\r\nAuthorization: Bearer {FAKE}\r\n"))
        );
        assert!(
            matches!(key_fault, Fault::Refused(_)) && account.take_refused(),
            "and one refused for the key tells the account"
        );
        assert!(open_refusal(&io::Error::other("gone")).is_none(), "any other error refuses nothing");
    }

    #[test]
    fn stay_reuses_its_connection() {
        let (account, answer) = (unusable(), b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec());
        let key = key_of(&account);
        let (first, second) = (up(), up());
        let (target, receiver) = serve_answers("/v1/responses", vec![answer.clone(); 5]);
        let mut steps = Vec::new();
        let result = key.ask((&target, &first), POST, Some(JSON), b"{}", |step| steps.push(step));
        assert_eq!((result.unwrap(), steps), ((200, b"ok".to_vec()), vec![Step::Reached, Step::Sent, Step::Answered]));
        assert_eq!(key.ask((&target, &first), GET, None, &[], |_| {}).unwrap().0, 200);
        assert_eq!(
            key.ask((&target, &second), GET, None, &[], |_| {}).unwrap().0,
            200,
            "another stay has connections of its own"
        );
        assert_eq!(key.ask((&target, &second), GET, None, &[], |_| {}).unwrap().0, 200);
        let third = up();
        account.prepare_connection(&target, &third).unwrap();
        account.prepare_connection(&target, &third).unwrap();
        assert_eq!(account.0.pool.idle_count(), 3, "a connection made ready is left open, and once");
        assert_eq!(key.ask((&target, &third), GET, None, &[], |_| {}).unwrap().0, 200);
        let numbers: Vec<usize> = receiver.recv().unwrap().iter().map(|(number, ..)| *number).collect();
        assert_eq!(
            numbers,
            [0, 0, 1, 1, 2],
            "each stay's second request went over its first's connection, and the request after a connection was made ready over that"
        );
        assert_eq!(account.0.pool.idle_count(), 3);
        first.raise();
        let (stream, is_reused) = account.connection(&target, &second).unwrap();
        assert!(is_reused && account.0.pool.idle_count() == 1, "what a stay that has ended left open is closed");
        stream.close();
        // A server that closes a connection left open as the next request comes has it go over a new one.
        let (target, listener) = listen("/v1/responses");
        let server = thread::spawn(move || {
            let mut first = Connection::accept(&listener);
            head(&mut first).unwrap();
            first.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
            head(&mut first).unwrap();
            drop(first);
            let mut second = Connection::accept(&listener);
            head(&mut second).unwrap();
            second.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nanew").unwrap();
        });
        let down = up();
        assert_eq!(key.ask((&target, &down), GET, None, &[], |_| {}).unwrap().1, b"ok");
        assert_eq!(key.ask((&target, &down), GET, None, &[], |_| {}).unwrap().1, b"anew");
        server.join().unwrap();
        // A post its server took over a connection left open and closed without an answer is not put again.
        let (target, listener) = listen("/v1/responses");
        let (sender, receiver) = mpsc::channel::<()>();
        let server = thread::spawn(move || {
            let mut first = Connection::accept(&listener);
            head(&mut first).unwrap();
            first.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
            head(&mut first).unwrap();
            drop(first);
            receiver.recv().unwrap();
            listener.set_nonblocking(true).unwrap();
            listener.accept().map(|_| ()).unwrap_err().kind()
        });
        let down = up();
        assert_eq!(key.ask((&target, &down), GET, None, &[], |_| {}).unwrap().1, b"ok");
        assert!(key.ask((&target, &down), POST, Some(JSON), b"{}", |_| {}).is_err(), "it may have been done");
        sender.send(()).unwrap();
        assert_eq!(server.join().unwrap(), io::ErrorKind::WouldBlock, "and no second connection came");
    }

    #[test]
    fn body_packed_where_smaller() {
        let account = unusable();
        let (key, down) = (key_of(&account), up());
        let answer = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_vec();
        let (target, receiver) = serve_answers("/v1/responses", vec![answer.clone(); 3]);
        let thread: String = (0..200u32)
            .map(|at| {
                let degrees = at.wrapping_mul(2_654_435_761) % 997;
                format!("{{\"type\":\"message\",\"role\":\"user\",\"content\":\"Wie wird das Wetter am {at}. Tag? {degrees} Grad.\"}},")
            })
            .collect();
        key.ask((&target, &down), POST, Some(JSON), thread.as_bytes(), |_| {}).unwrap();
        key.ask((&target, &down), POST, Some(JSON), b"{}", |_| {}).unwrap();
        let other_target = Target { path: "/v1/audio/transcriptions".into(), ..target.clone() };
        key.ask((&other_target, &down), POST, Some(JSON), thread.as_bytes(), |_| {}).unwrap();
        let requests = receiver.recv().unwrap();
        let (head, body) = (&requests[0].1, &requests[0].2);
        assert!(
            head.contains("\r\nContent-Encoding: zstd\r\n") && head.contains("\r\nContent-Type: application/json\r\n")
        );
        assert!(
            body.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) && body.len() < thread.len(),
            "{} bytes of {} packed",
            body.len(),
            thread.len()
        );
        assert!(!requests[1].1.contains("Content-Encoding") && requests[1].2 == b"{}", "what packing makes no smaller");
        assert!(!requests[2].1.contains("Content-Encoding") && requests[2].2 == thread.as_bytes(), "another address");
    }
}

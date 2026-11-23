use crate::log;
use crate::net::names;
use crate::net::tls::Stream;
use crate::sys::invalid;
use crate::wifi::radio::NetworkDown;
use r1_funcore::codec::http::{self, BAD_CHUNKS, BAD_HEADER, Framing, GET, Head, NO_BODY_END, READ_BYTES, Target};
use r1_funcore::net::fetch::{Fetched, ResponseKind, check_read};
use r1_funcore::net::tls::Purpose;
use r1_funcore::net::to_web_line;
use r1_funcore::signal::service::{NO_URL, Request};
use std::error::Error;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::time::Instant;

pub(super) fn bad_header_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, BAD_HEADER)
}

pub(super) fn truncated_error() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "the server closed in the middle of what it sent")
}

/// Resolves the target's host, connects to the first address that answers and does the TLS handshake of `purpose`; `down` ends it.
pub fn reach(target: &Target, purpose: Purpose, down: &NetworkDown) -> io::Result<Stream> {
    let start = Instant::now();
    let addresses = names::resolve_all(&[&target.host], down).swap_remove(0)?;
    connect(target, &addresses, purpose, start, down)
}

/// Connects to the first of the target's `addresses` that answers, in the order to try them, and does the TLS handshake of `purpose`; the log tells its times from `start`.
pub fn connect(
    target: &Target,
    addresses: &[IpAddr],
    purpose: Purpose,
    start: Instant,
    down: &NetworkDown,
) -> io::Result<Stream> {
    let addresses: Vec<SocketAddr> = addresses.iter().map(|address| SocketAddr::new(*address, target.port)).collect();
    let name_time = start.elapsed();
    let (stream, peer, connect_time) = Stream::reach(&addresses, &target.host, purpose, start, down)?;
    let (tls, tls_ms) = (stream.to_log_text(), start.elapsed().as_millis());
    log!("{}", to_web_line(&target.host, name_time.as_millis(), &peer, connect_time.as_millis(), &tls, tls_ms));
    Ok(stream)
}

pub(super) fn read_more(stream: &mut Stream, input: &mut Vec<u8>) -> io::Result<bool> {
    let length = input.len();
    input.resize(length + READ_BYTES, 0);
    let result = stream.read(&mut input[length..]);
    input.truncate(length + *result.as_ref().unwrap_or(&0));
    Ok(result? > 0)
}

pub(super) fn read_head(stream: &mut Stream, input: &mut Vec<u8>) -> io::Result<Head> {
    loop {
        match Head::take(input).map_err(|error| invalid(error.message()))? {
            Some(head) => return Ok(head),
            None if read_more(stream, input)? => {}
            None => return Err(truncated_error()),
        }
    }
}

/// An HTTP answer: its head, and `Read` over its body as it comes.
pub struct Answer<'a> {
    pub head: Head,
    stream: &'a mut Stream,
    input: Vec<u8>,
    body: Vec<u8>,
    offset: usize,
    framing: Framing,
    is_closing: bool,
}

pub fn send(
    stream: &mut Stream,
    method: &str,
    target: &Target,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<()> {
    stream.write_all(&http::write_request(method, target, headers, body).ok_or_else(bad_header_error)?)?;
    stream.flush()
}

fn ask<'a>(
    stream: &'a mut Stream,
    method: &str,
    target: &Target,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<Answer<'a>> {
    send(stream, method, target, headers, body)?;
    answer(stream, method)
}

/// Asks one request on a connection of its own, has `read` take its answer, and closes the connection whatever came of it.
pub fn ask_once<T>(
    mut stream: Stream,
    (method, target): (&str, &Target),
    (headers, body): (&[(&str, &str)], &[u8]),
    read: impl FnOnce(&mut Answer<'_>) -> io::Result<T>,
) -> io::Result<T> {
    let result = ask(&mut stream, method, target, headers, body).and_then(|mut answer| read(&mut answer));
    stream.close();
    result
}

/// Asks one request of Signal's services on a connection of its own, under Signal's roots; the answer's body is taken as far as the request lets it.
pub fn ask_request(request: &Request<'_>, down: &NetworkDown) -> io::Result<(Head, Vec<u8>)> {
    let target = Target::parse(&request.url).ok_or_else(|| io::Error::other(NO_URL))?;
    let stream = reach(&target, Purpose::Signal, down)?;
    let headers: Vec<(&str, &str)> =
        request.headers.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
    ask_once(stream, (request.method, &target), (&headers, &request.body), |answer| {
        let body = answer.read_checked(|bytes| request.check_size(bytes).map_err(io::Error::other))?;
        Ok((answer.head.clone(), body))
    })
}

/// Asks for the document at `target` on a connection of its own: the address its head sends the asker on to, as `check` reads it, or the document, read under the limits of a fetch from `start` on.
pub fn fetch<E: Into<Box<dyn Error + Send + Sync>>>(
    stream: Stream,
    target: &Target,
    headers: &[(&str, &str)],
    start: Instant,
    check: impl FnOnce(&Head) -> Result<ResponseKind, E>,
) -> io::Result<Fetched> {
    ask_once(stream, (GET, target), (headers, &[]), |answer| match check(&answer.head).map_err(io::Error::other)? {
        ResponseKind::Redirect(next) => Ok(Fetched::Redirect(next)),
        ResponseKind::Document => answer.read_document(start).map(Fetched::Document),
    })
}

/// Reads the head of the answer to a request of `method` already sent.
pub fn answer<'a>(stream: &'a mut Stream, method: &str) -> io::Result<Answer<'a>> {
    let mut input = Vec::new();
    let head = read_head(stream, &mut input)?;
    read_body(stream, head, input, method)
}

pub(super) fn read_body<'a>(
    stream: &'a mut Stream,
    head: Head,
    input: Vec<u8>,
    method: &str,
) -> io::Result<Answer<'a>> {
    let (framing, is_closing) = Framing::from_head(&head, method).map_err(|_| invalid(NO_BODY_END))?;
    Ok(Answer { head, stream, input, body: Vec::new(), offset: 0, framing, is_closing })
}

impl Answer<'_> {
    /// True where the connection carries the next request.
    pub fn is_reusable(&self) -> bool {
        self.framing.is_reusable(self.is_closing, self.input.len() + self.body.len() - self.offset)
    }

    /// Reads the body whole, `check` told its length after each read; its error ends the reading.
    pub fn read_checked(&mut self, mut check: impl FnMut(usize) -> io::Result<()>) -> io::Result<Vec<u8>> {
        let (mut body, mut piece) = (Vec::new(), vec![0u8; READ_BYTES]);
        loop {
            match self.read(&mut piece)? {
                0 => return Ok(body),
                count => body.extend_from_slice(&piece[..count]),
            }
            check(body.len())?;
        }
    }

    /// Reads the body whole as a fetched document's, under the limits of its size and of its time from `start`.
    pub fn read_document(&mut self, start: Instant) -> io::Result<Vec<u8>> {
        self.read_checked(|bytes| check_read(bytes, start.elapsed()).map_err(io::Error::other))
    }

    fn refill(&mut self) -> io::Result<()> {
        self.body.clear();
        self.offset = 0;
        if self.input.is_empty() && !read_more(self.stream, &mut self.input)? {
            return if self.framing.close() { Ok(()) } else { Err(truncated_error()) };
        }
        self.framing.take(&mut self.input, &mut self.body).map_err(|_| invalid(BAD_CHUNKS))
    }
}

impl Read for Answer<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.offset == self.body.len() {
            if self.framing.is_ended() {
                return Ok(0);
            }
            self.refill()?;
        }
        let count = buffer.len().min(self.body.len() - self.offset);
        buffer[..count].copy_from_slice(&self.body[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::served::{head, serve};
    use crate::wifi::radio::tests::up;

    fn fetch(pieces: &'static [&'static [u8]], should_close: bool) -> io::Result<(u16, Vec<u8>, String)> {
        let (target, server) = serve("/path?x=1", move |mut connection| {
            let (request, _) = head(&mut connection).unwrap();
            for piece in pieces {
                connection.write_all(piece).unwrap();
            }
            if !should_close {
                let _ = connection.read(&mut [0u8]);
            }
            request
        });
        let mut stream = reach(&target, Purpose::Http, &up())?;
        let result = (|| {
            let mut answer = ask(&mut stream, http::GET, &target, &[("Accept", "*/*")], &[])?;
            let mut body = Vec::new();
            answer.read_to_end(&mut body)?;
            Ok((answer.head.status, body))
        })();
        stream.close();
        let request = server.join().unwrap();
        result.map(|(status, body)| (status, body, request))
    }

    #[test]
    fn body_framings() {
        let (status, body, request) =
            fetch(&[b"HTTP/1.1 200 OK\r\nContent-Le", b"ngth: 11\r\n\r\nhello", b" world"], false).unwrap();
        assert_eq!((status, &body[..]), (200, &b"hello world"[..]), "by its length, on a connection that stays open");
        assert!(
            request.starts_with("GET /path?x=1 HTTP/1.1\r\nHost: 127.0.0.1:")
                && request.ends_with("\r\nAccept: */*\r\n\r\n"),
            "{request}"
        );
        let (status, body, _) = fetch(&[b"HTTP/1.1 103 Early Hints\r\nLink: </x>\r\n\r\nHTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel", b"lo\r\n6\r\n world\r\n0\r\n\r\n"], false).unwrap();
        assert_eq!(
            (status, &body[..]),
            (404, &b"hello world"[..]),
            "in chunks, after an answer that only said another follows"
        );
        let (status, body, _) = fetch(&[b"HTTP/1.0 200 OK\r\n\r\nhello", b" world"], true).unwrap();
        assert_eq!((status, &body[..]), (200, &b"hello world"[..]), "by the close");
        assert_eq!(
            fetch(&[b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\nhello"], true).map_err(|e| e.kind()),
            Err(io::ErrorKind::UnexpectedEof),
            "a body cut short is no body"
        );
        assert_eq!(fetch(&[b"SSH-2.0-OpenSSH\r\n\r\n"], true).map_err(|e| e.kind()), Err(io::ErrorKind::InvalidData));
    }

    #[test]
    fn request_without_address() {
        let request = Request::new("GET", "nonsense".into(), Vec::new(), Vec::new().into());
        let result = ask_request(&request, &up());
        assert_eq!(result.err().map(|e| e.to_string()), Some(String::from(NO_URL)), "it fails before the network");
    }

    #[test]
    fn connection_reuse_after_body() {
        let (target, server) = serve("/", |mut connection| {
            head(&mut connection);
            connection.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nfirst").unwrap();
            head(&mut connection);
            connection
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nsecond\r\n0\r\n\r\n")
                .unwrap();
            head(&mut connection);
            connection.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nthird").unwrap();
        });
        let ask_once = |stream: &mut Stream| {
            let mut answer = ask(stream, "GET", &target, &[], &[]).unwrap();
            let is_reusable = answer.is_reusable();
            let mut body = Vec::new();
            answer.read_to_end(&mut body).unwrap();
            (is_reusable, String::from_utf8(body).unwrap(), answer.is_reusable())
        };
        let mut stream = reach(&target, Purpose::Http, &up()).unwrap();
        assert_eq!(ask_once(&mut stream), (false, "first".into(), true), "not before its body is read, and then");
        assert_eq!(ask_once(&mut stream), (false, "second".into(), true), "the same connection again, in chunks");
        assert_eq!(ask_once(&mut stream), (false, "third".into(), false), "a server that says it closes keeps nothing");
        stream.close();
        server.join().unwrap();
        let (target, server) = serve("/", |mut connection| {
            head(&mut connection);
            connection.write_all(b"HTTP/1.1 200 OK\r\n\r\nuntil the close").unwrap();
        });
        let mut stream = reach(&target, Purpose::Http, &up()).unwrap();
        let mut answer = ask(&mut stream, "GET", &target, &[], &[]).unwrap();
        answer.read_to_end(&mut Vec::new()).unwrap();
        assert!(!answer.is_reusable(), "a body that runs to the close ends its connection");
        stream.close();
        server.join().unwrap();
    }
}

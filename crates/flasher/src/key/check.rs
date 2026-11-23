//! OpenAI's word on a key: the list of the models it may use asked for with it, which costs nothing, its connection and answer bounded in time and length, its name resolved by the host in the host's own time.

use r1_funcore::codec::http::{self, Framing, Head, READ_BYTES, Target};
use r1_funcore::net::race::TIMEOUT;
use r1_funcore::openai::key::{self, Verdict};
use r1_funcore::openai::models;
use r1_funcore::openai::request::{JSON, headers};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;

// The most of OpenAI's answer that is read: its head and a refusal lie well within; past it the list of the models is cut, which tells as much.
const ANSWER_BYTES: usize = 65536;

// What is no HTTP in OpenAI's answer.
fn no_http<T>(_: T) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "OpenAI's answer is no HTTP")
}

// A connection to the first of the server's addresses that takes one in time, which reads and writes in time or fails.
fn reached(target: &Target) -> io::Result<TcpStream> {
    let mut failed = io::Error::new(io::ErrorKind::NotFound, "OpenAI's name has no address");
    for address in (target.host.as_str(), target.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, TIMEOUT) {
            Ok(tcp) => {
                tcp.set_read_timeout(Some(TIMEOUT))?;
                tcp.set_write_timeout(Some(TIMEOUT))?;
                return Ok(tcp);
            }
            Err(e) => failed = e,
        }
    }
    Err(failed)
}

// The status and the body of OpenAI's answer to the check of a key.
fn answer(key: &str) -> io::Result<(u16, Vec<u8>)> {
    let target =
        Target::parse(models::check::ADDRESS).ok_or_else(|| io::Error::other("the check's address is no URL"))?;
    let server =
        ServerName::try_from(target.host.clone()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let session = ClientConnection::new(Arc::new(config), server).map_err(io::Error::other)?;
    let mut tls = StreamOwned::new(session, reached(&target)?);
    // The request the device's keeper sends, and a close, which ends the answer.
    let bearer = key::to_bearer(key);
    let mut headers = headers(&bearer, JSON, None, false);
    headers.push(("Connection", http::CLOSE));
    let request = http::write_request(http::GET, &target, &headers, &[])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, http::BAD_HEADER))?;
    tls.write_all(&request)?;
    tls.flush()?;
    // The answer is read to the close the request asked for, or as far as it is taken.
    let (mut received, mut piece) = (Vec::new(), vec![0u8; READ_BYTES]);
    while received.len() < ANSWER_BYTES {
        match tls.read(&mut piece[..READ_BYTES.min(ANSWER_BYTES - received.len())]) {
            Ok(0) => break,
            Ok(read) => received.extend_from_slice(&piece[..read]),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }
    let Some(head) = Head::take(&mut received).map_err(no_http)? else {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "OpenAI closed before it answered"));
    };
    let (mut framing, _) = Framing::from_head(&head, http::GET).map_err(no_http)?;
    let mut body = Vec::new();
    framing.take(&mut received, &mut body).map_err(no_http)?;
    Ok((head.status, body))
}

// What is wrong with a key as given, as `r1` tells it; none for one OpenAI takes.
pub(super) fn refused(given: &str) -> Option<String> {
    let Some(key) = key::parse_key(given) else {
        return Some("that is no OpenAI key: one begins with sk- and holds letters, digits, - and _ alone".into());
    };
    let (status, verdict) = match answer(key) {
        Ok((status, body)) => (status, key::parse_verdict(status, &body)),
        Err(e) => return Some(format!("OpenAI could not be asked: {e}")),
    };
    let told = match verdict {
        Verdict::Works => None,
        Verdict::Known => Some(format!("OpenAI knows the key, which may not list the models (status {status})")),
        Verdict::Refused => Some(format!("OpenAI does not know the key (status {status})")),
        Verdict::Unanswered => Some(format!("OpenAI said nothing of the key (status {status})")),
    };
    // A key the verdict keeps is taken, what is told of it only a note.
    if verdict.should_store() {
        told.iter().for_each(|told| eprintln!("{told}"));
        return None;
    }
    told
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_key_refused() {
        for given in ["", "pk-live-abc", "sk-two words", "sk-"] {
            assert!(refused(given).is_some_and(|why| why.starts_with("that is no OpenAI key")), "{given:?}");
        }
    }
}

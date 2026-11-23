use crate::net::dial;
use crate::net::web;
use crate::sys;
use crate::wifi::radio::NetworkDown;
use crate::{crypto, log};
use r1_funcore::codec::http::{GET, Target};
use r1_funcore::domain::place::{self, ANSWER_BYTES, HEADERS};
use r1_funcore::net::sntp::{self, MARK_BYTES, SERVERS};
use r1_funcore::net::tls::Purpose;
use r1_funcore::openai::chat::request::Location;
use std::io::{self, Read};
use std::net::SocketAddr;

pub(super) fn ask_time(down: &NetworkDown) -> io::Result<u64> {
    let mark = crypto::random::<MARK_BYTES>().ok_or_else(|| io::Error::other("no random number for the question"))?;
    let servers = SERVERS.map(|server| SocketAddr::new(server, sntp::PORT));
    let (transmit_ms, server, elapsed) =
        dial::exchange(&servers, &sntp::request(mark), |packet| sntp::parse_reply(packet, mark), down)?;
    log!("{}", sntp::to_answered_line(server.ip(), elapsed.as_millis()));
    Ok(sntp::arrival_seconds(transmit_ms, elapsed.as_millis() as u64))
}

pub(super) fn ask_place(down: &NetworkDown) -> io::Result<(i16, Option<Location>)> {
    let target = Target::parse(place::SERVICE).ok_or_else(|| io::Error::other(place::NO_URL))?;
    let stream = web::reach(&target, Purpose::Http, down)?;
    let (status, body) = web::ask_once(stream, (GET, &target), (&HEADERS, &[]), |answer| {
        let mut body = String::new();
        answer.take(ANSWER_BYTES).read_to_string(&mut body)?;
        Ok((answer.head.status, body))
    })?;
    let offset = place::parse_offset(&body).ok_or_else(|| sys::invalid(place::to_unplaced_text(status)))?;
    Ok((offset, place::parse_answer(&body)))
}

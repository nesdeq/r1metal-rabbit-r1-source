use crate::crypto::Library;
use crate::net::web;
use crate::wifi::radio::NetworkDown;
use crate::{log, sys};
use r1_funcore::signal::connection::to_dropped_line;
use r1_funcore::signal::device::{Device, Event};
use r1_funcore::signal::group::GroupFetch;
use r1_funcore::signal::to_tagged_line;

pub(super) fn fetch(device: &mut Device, mut fetch: GroupFetch, down: &NetworkDown) -> Vec<Event> {
    while let Some(request) = fetch.next_request(&Library) {
        match web::ask_request(&request, down) {
            Ok((head, body)) => fetch.take_answer(&Library, &head, &body, sys::realtime_ms()),
            Err(e) => fetch.abort(e.to_string()),
        }
    }
    let (line, events, drops) = device.take_group_fetch(&Library, fetch);
    log!("{}", to_tagged_line(&line));
    for reason in drops {
        log!("{}", to_dropped_line(&reason));
    }
    events
}

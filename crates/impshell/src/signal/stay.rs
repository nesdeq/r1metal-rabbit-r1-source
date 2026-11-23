use super::files::Traces;
use super::socket::{Anonymous, Service};
use super::{POKE, Resources, SOCKET, Shared, files, groups, names};
use crate::crypto::Library;
use crate::log;
use crate::net::websocket;
use crate::sys::{self, Epoll};
use crate::wifi::radio::{Hold, NetworkDown};
use r1_funcore::domain::signal::state::{State, UNLINKED_LINE};
use r1_funcore::signal::command::Command;
use r1_funcore::signal::connection::{self, Action, Connection, SILENT};
use r1_funcore::signal::device::Device;
use r1_funcore::signal::{service, to_tagged_line};
use std::io;
use std::time::Duration;

pub(super) fn open_socket(device: &Device, hold: &Hold) -> io::Result<Service> {
    Service::open(service::SOCKET_PATH, &service::socket_headers(Some(&device.account)), hold.down())
}

enum StayError {
    Socket(io::Error),
    Storage(io::Error),
}

impl From<io::Error> for StayError {
    fn from(e: io::Error) -> StayError {
        StayError::Socket(e)
    }
}

// True where the service no longer knows the device.
pub(super) fn run_stay(shared: &Shared, resources: &Resources, traces: &Traces, device: &mut Device) -> bool {
    shared.publish(device);
    shared.keep_sends(resources, device);
    let hold = match resources.online.hold() {
        Ok(hold) => hold,
        Err(fault) => {
            log!("signal: {fault}");
            shared.fail_and_wait(|state| state.fail(&fault));
            return false;
        }
    };
    let mut service = match open_socket(device, &hold) {
        Ok(service) => service,
        Err(e) if websocket::open_error(&e).is_some_and(|(status, _)| service::is_unlinked(status)) => {
            log!("{UNLINKED_LINE}");
            shared.board.lock().note_unlinked();
            shared.board.raise();
            files::forget(&resources.data, traces);
            return true;
        }
        Err(e) => {
            log!("signal: the socket: {e}");
            shared.fail_and_wait(|state| state.fail(&e.into()));
            return false;
        }
    };
    shared.board.lock().set_connected(true);
    shared.board.raise();
    let result = run_connection(shared, resources, device, &mut service, hold.down());
    service.close();
    shared.board.lock().set_connected(false);
    shared.board.raise();
    // A stay that broke, the service's close among the ways, is not followed by the next at once.
    match result {
        Ok(()) => {}
        Err(StayError::Socket(e)) => {
            log!("signal: the stay ended: {e}");
            shared.fail_and_wait(|state| state.fail(&e.into()));
        }
        Err(StayError::Storage(e)) => {
            log!("signal: the files are not written, and nothing goes to the service: {e}");
            shared.fail_and_wait(State::fail_storage);
        }
    }
    false
}

fn run_connection(
    shared: &Shared,
    resources: &Resources,
    device: &mut Device,
    service: &mut Service,
    down: &NetworkDown,
) -> Result<(), StayError> {
    let mut connection = Connection::new(device, &Library, (sys::realtime_ms(), sys::boottime_ms()));
    let mut epoll = Epoll::new()?;
    epoll.add(service.fd(), SOCKET)?;
    epoll.add(shared.poke.fd(), POKE)?;
    let mut anonymous = Anonymous::default();
    let result =
        serve_connection(shared, resources, (device, &mut connection), (service, &mut anonymous), (&mut epoll, down));
    anonymous.close(&mut epoll);
    shared.take_events(connection.end());
    result
}

fn serve_connection(
    shared: &Shared,
    resources: &Resources,
    (device, connection): (&mut Device, &mut Connection),
    (service, anonymous): (&mut Service, &mut Anonymous),
    (epoll, down): (&mut Epoll, &NetworkDown),
) -> Result<(), StayError> {
    loop {
        let mut actions = Vec::new();
        let is_due = shared.board.lock().is_republish_due();
        connection.begin_round(is_due);
        while let Some(frame) = service.next_frame(false)? {
            let (frame_actions, events, drop_reason) =
                connection.take_frame(device, &Library, frame, (sys::realtime_ms(), sys::boottime_ms()));
            actions.extend(frame_actions);
            if let Some(reason) = drop_reason {
                log!("{}", connection::to_dropped_line(&reason));
            }
            shared.take_events(events);
        }
        anonymous.read_answers(epoll, connection, device, &mut actions);
        shared.poke.clear();
        let commands: Vec<Command> = shared.board.lock().take_commands();
        resources.keep_sounds(connection.take_commands(device, &Library, commands, sys::realtime_ms()));
        let done = shared.board.lock().take_done();
        connection.take_transfers(device, &Library, done, sys::realtime_ms());
        shared.answer(Some(device));
        let is_asleep = shared.board.lock().is_asleep();
        let tick_actions = connection
            .tick(device, &Library, (sys::realtime_ms(), sys::boottime_ms()), is_asleep)
            .ok_or_else(|| io::Error::other(SILENT))?;
        actions.extend(tick_actions);
        shared.take_events(connection.take_events());
        for line in connection.take_log_lines() {
            log!("{}", to_tagged_line(&line));
        }
        connection.expire(device, sys::realtime_ms());
        // What a delivery or a sealing changed is kept before the service hears of it: an answered delivery comes no more, and a session's chain moves on only once it is kept. Where the files cannot be written the stay ends before anything goes, and memory keeps what it took until a write succeeds.
        files::write(&resources.data, device).map_err(StayError::Storage)?;
        if device.take_dropped() {
            shared.sweep(resources, device);
        }
        for action in actions {
            match action {
                Action::Frame(frame) => service.send(&frame)?,
                Action::Anonymous(frame) => anonymous.send(&frame, epoll, (&mut *connection, &mut *device), down),
                Action::Names(authorization) => {
                    names::read(device, authorization, down);
                    resources.write(device);
                    connection.note_changed();
                }
                Action::Groups(fetch) => {
                    let events = groups::fetch(device, *fetch, down);
                    shared.take_events(events);
                    resources.write(device);
                    connection.note_changed();
                }
            }
        }
        if !connection.is_anonymous_pending() {
            anonymous.close(epoll);
        }
        if connection.is_changed() {
            shared.publish(device);
        }
        let at = sys::boottime_ms();
        let round = shared.board.lock().take_round(connection.status(device, (sys::realtime_ms(), at)), device, at);
        if round.is_turn_due {
            shared.board.notify();
        }
        if round.should_leave {
            return Ok(());
        }
        epoll.wait(Some(Duration::from_millis(round.wait_ms)))?;
    }
}

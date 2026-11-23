use super::socket::Service;
use super::stay::open_socket;
use super::{POKE, Resources, SOCKET, Shared, files};
use crate::crypto::Library;
use crate::log;
use crate::sys::{self, Epoll};
use crate::wifi::radio::Hold;
use r1_funcore::domain::signal::state::{CodeStep, LinkStep};
use r1_funcore::signal::curve::Pair;
use r1_funcore::signal::device::Device;
use r1_funcore::signal::link::{self, ProvisioningMessage};
use r1_funcore::signal::service;
use std::io;

pub(super) fn link(shared: &Shared, resources: &Resources) -> Option<Device> {
    let device = run_link(shared, resources);
    if device.is_none() {
        shared.board.lock().note_link_end();
    }
    device
}

fn run_link(shared: &Shared, resources: &Resources) -> Option<Device> {
    shared.set_link_step(LinkStep::Connecting);
    let hold = match resources.online.hold() {
        Ok(hold) => hold,
        Err(fault) => {
            log!("signal: the link: {fault}");
            shared.set_link_step(LinkStep::from_fault(&fault));
            return None;
        }
    };
    let mut codes = link::Codes::default();
    loop {
        // The window may have closed while the network was joined, or as a code stood, the last among them.
        let step = shared.board.lock().next_code_step(&codes);
        match step {
            CodeStep::Stop => return None,
            CodeStep::Expired => {
                shared.board.raise();
                return None;
            }
            CodeStep::Open => {}
        }
        match provision(shared, &hold, &mut codes) {
            Ok(Some(provision)) => return finish_link(shared, resources, &hold, provision),
            Ok(None) => {}
            Err(e) => {
                log!("signal: the provisioning socket: {e}");
                shared.set_link_step(LinkStep::from_fault(&e.into()));
                return None;
            }
        }
    }
}

fn provision(shared: &Shared, hold: &Hold, codes: &mut link::Codes) -> io::Result<Option<link::Provision>> {
    let key = Pair::new(&Library).ok_or_else(|| io::Error::other(link::NO_KEY))?;
    let mut service = Service::open(service::PROVISIONING_PATH, &service::socket_headers(None), hold.down())?;
    let mut epoll = Epoll::new()?;
    epoll.add(service.fd(), SOCKET)?;
    epoll.add(shared.poke.fd(), POKE)?;
    codes.begin(sys::boottime_ms());
    loop {
        while let Some(frame) = service.next_frame(false)? {
            let (message, answer) = link::read_frame(&Library, &key.private, &frame);
            if let Some(answer) = answer {
                service.send(&answer)?;
            }
            match message {
                ProvisioningMessage::Address(provisioning) => {
                    shared.set_link_step(LinkStep::Code(link::address(&provisioning, &key.public)));
                }
                ProvisioningMessage::Account(provision) => {
                    service.close();
                    return Ok(Some(*provision));
                }
                ProvisioningMessage::Nothing => {}
            }
        }
        shared.poke.clear();
        let wait = shared.board.lock().to_code_wait(codes, sys::boottime_ms());
        if wait.is_none() {
            service.close();
            return Ok(None);
        }
        epoll.wait(wait)?;
    }
}

// The status of the next answer to one of the `count` first uploads.
fn read_status(service: &mut Service, count: usize) -> io::Result<u16> {
    loop {
        if let Some(status) = service.next_frame(true)?.and_then(|frame| link::parse_upload_status(&frame, count)) {
            return Ok(status);
        }
    }
}

fn finish_link(shared: &Shared, resources: &Resources, hold: &Hold, provision: link::Provision) -> Option<Device> {
    shared.set_link_step(LinkStep::MakingKeys);
    let fail = |step| {
        shared.set_link_step(step);
        None
    };
    let Some(setup) = link::prepare(&Library, &provision, sys::realtime_ms()) else {
        return fail(LinkStep::NO_KEYS);
    };
    let response = (|| -> io::Result<_> {
        let mut service = Service::open(service::SOCKET_PATH, &service::socket_headers(None), hold.down())?;
        service.send(&link::request_frame(&provision, &setup))?;
        loop {
            if let Some(answer) = service.next_frame(true)?.and_then(|frame| link::parse_answer(&frame, &provision)) {
                service.close();
                return Ok(answer);
            }
        }
    })();
    let answer = match response {
        Ok((status, answer)) => {
            log!("{}", link::to_answered_line(status));
            answer.map_err(LinkStep::from_refusal)
        }
        Err(e) => {
            log!("signal: the link request: {e}");
            Err(LinkStep::from_fault(&e.into()))
        }
    };
    let id = match answer {
        Ok(id) => id,
        Err(step) => return fail(step),
    };
    let mut device = link::make_device(&provision, setup, id);
    if let Err(e) = files::write(&resources.data, &mut device) {
        log!("signal: the new account is not written: {e}");
        return fail(LinkStep::UNWRITTEN);
    }
    log!("{}", link::to_linked_line(id));
    let result = (|| -> io::Result<()> {
        let mut service = open_socket(&device, hold)?;
        let uploads = link::first_uploads(&device);
        for upload in &uploads {
            service.send(upload)?;
        }
        for _ in &uploads {
            log!("{}", link::to_uploaded_line(read_status(&mut service, uploads.len())?));
        }
        service.close();
        Ok(())
    })();
    if let Err(e) = result {
        log!("signal: the first keys: {e}");
    }
    shared.board.lock().note_linked();
    shared.board.raise();
    Some(device)
}

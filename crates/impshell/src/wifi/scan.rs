use crate::log;
use crate::sys::{self, Epoll};
use crate::wifi::association::Association;
use crate::wifi::nl80211::Socket;
use crate::wifi::{Preempt, iface};
use r1_funcore::wifi::scan::{self, Event, Network, Scan, State};
use r1_funcore::wifi::{TIMEOUT, netlink, time_left};
use std::io;
use std::time::Instant;

const EVENTS: u64 = 0;
const NUDGE: u64 = 1;
const LINK_EVENTS: u64 = 2;
const LINK_FRAMES: u64 = 3;

// A scan beside a WiFi stay reads the stay's news meanwhile (`Association::serve`): a key the network renews is confirmed in the network's time.
pub(super) fn run(
    request: &Scan,
    preempt: &Preempt,
    mut link: Option<&mut Association>,
) -> io::Result<Option<Vec<Network>>> {
    if preempt.is_pending() {
        return Ok(None);
    }
    let interface = sys::iface::index(iface::NAME, true)?;
    let mut socket = Socket::open()?;
    let family = socket.wireless_family()?;
    socket.listen(family.scans)?;
    let trigger = request.to_attributes(interface);
    socket.send(family.id, netlink::ACKNOWLEDGE, scan::TRIGGER_SCAN, 0, &trigger)?;
    let mut epoll = Epoll::new()?;
    epoll.add(socket.fd(), EVENTS)?;
    epoll.add(preempt.nudge.fd(), NUDGE)?;
    if let Some(link) = &link {
        let (events, frames) = link.fds();
        epoll.add(events, LINK_EVENTS)?;
        epoll.add(frames, LINK_FRAMES)?;
    }
    let attributes = netlink::interface_attributes(interface, &[]);
    // A scan that is left has the driver told to end it: whatever the radio does next would wait for it.
    let abort = |socket: &mut Socket| {
        if let Err(e) = socket.send(family.id, 0, scan::ABORT_SCAN, 0, &attributes) {
            log!("radio: telling the driver to end the scan: {e}");
        }
    };
    let (mut state, mut trigger_time) = (State::Asked, Instant::now());
    'scan: loop {
        let Some(wait) = time_left(trigger_time.elapsed(), TIMEOUT) else {
            abort(&mut socket);
            return Err(io::Error::other(scan::LATE));
        };
        let ready = epoll.wait(Some(wait))?;
        if ready.contains(&NUDGE) && preempt.should_yield() {
            abort(&mut socket);
            return Ok(None);
        }
        if let Some(link) = link.as_deref_mut() {
            link.serve(ready.contains(&LINK_FRAMES), ready.contains(&LINK_EVENTS));
        }
        if !ready.contains(&EVENTS) {
            continue;
        }
        for message in netlink::messages(&socket.datagram()?) {
            match state.update(&message) {
                Event::Nothing => {}
                Event::Retry => {
                    socket.send(family.id, netlink::ACKNOWLEDGE, scan::TRIGGER_SCAN, 0, &trigger)?;
                    trigger_time = Instant::now();
                }
                Event::Ended => break 'scan,
                Event::Aborted => return Err(io::Error::other(scan::ABORTED)),
                Event::Refused(code) => return Err(io::Error::from_raw_os_error(code)),
            }
        }
    }
    let mut networks = Vec::new();
    socket.dump(family.id, scan::GET_SCAN, &attributes, |command, attributes| {
        networks.extend(scan::to_network(command, attributes));
    })?;
    Ok(Some(networks))
}

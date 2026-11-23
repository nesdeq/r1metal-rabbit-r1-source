use crate::wifi::Preempt;
use crate::wifi::association::Association;
use crate::{log, sys};
use r1_funcore::wifi::join::JoinError;
use r1_funcore::wifi::reach::{Next, Proof, to_reached_line};
use std::io;
use std::net::SocketAddr;
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

const NUDGE: usize = 0;
const LINK_EVENTS: usize = 1;
const LINK_FRAMES: usize = 2;
const FIRST_RESOLVER: usize = 3;
// A descriptor a wait passes over.
const NO_FD: i32 = -1;

// Asks each of `resolvers` for a connection at once and waits up to `limit` for the first to answer, reading the news of `link` meanwhile; the error where none answers, a wish comes first or the link is lost.
pub(super) fn prove(
    resolvers: &[SocketAddr],
    mut link: Option<&mut Association>,
    preempt: &Preempt,
    limit: Duration,
) -> io::Result<Result<(), JoinError>> {
    let (start, mut proof) = (Instant::now(), Proof::default());
    let log_failure =
        |resolver: &SocketAddr, e: &io::Error| log!("wifi: asking the resolver at {}: {e}", resolver.ip());
    let mut asked: Vec<(SocketAddr, OwnedFd)> = Vec::new();
    for resolver in resolvers {
        // A resolver whose asking fails at once, as where no route leads to it, counts as not asked.
        match sys::begin_connect(resolver) {
            Ok(socket) => {
                proof.note_asked();
                asked.push((*resolver, socket));
            }
            Err(e) => log_failure(resolver, &e),
        }
    }
    let (events, frames) = link.as_ref().map_or((NO_FD, NO_FD), |link| link.fds());
    loop {
        let wait = match proof.next(start.elapsed(), limit) {
            Next::Wait(wait) => wait,
            Next::Reached => return Ok(Ok(())),
            Next::Unreached => return Ok(Err(JoinError::Silent)),
        };
        let mut fds = vec![sys::readable(preempt.nudge.fd()), sys::readable(events), sys::readable(frames)];
        fds.extend(asked.iter().map(|(_, socket)| libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        }));
        sys::poll(&mut fds, Some(Instant::now() + wait))?;
        if fds[NUDGE].revents != 0 && preempt.should_yield() {
            return Ok(Err(JoinError::GaveWay));
        }
        if let Some(link) = link.as_deref_mut()
            && let Some(error) = link.read_news(fds[LINK_FRAMES].revents != 0, fds[LINK_EVENTS].revents != 0)?
        {
            return Ok(Err(error));
        }
        let mut waiting = Vec::with_capacity(asked.len());
        for ((resolver, socket), fd) in asked.drain(..).zip(&fds[FIRST_RESOLVER..]) {
            if fd.revents == 0 {
                waiting.push((resolver, socket));
                continue;
            }
            let result = sys::connect_result(socket.as_raw_fd());
            proof.note_end(result.is_ok());
            match result {
                Ok(()) => log!("{}", to_reached_line(&resolver.ip().to_string(), start.elapsed().as_millis())),
                Err(e) => log_failure(&resolver, &e),
            }
        }
        asked = waiting;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Board;
    use crate::sys::Wake;
    use crate::wifi::radio::State;
    use std::net::{Ipv4Addr, TcpListener};

    #[test]
    fn first_answer_proves_reach() {
        let (board, nudge) = (Board::new(State::default()).unwrap(), Wake::new().unwrap());
        let preempt = Preempt { nudge: &nudge, board: &board, urgent_count: 0 };
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let open = listener.local_addr().unwrap();
        // A port nothing listens on: bound and let go of.
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap();
        let limit = Duration::from_secs(1);
        assert_eq!(
            prove(&[closed, open], None, &preempt, limit).unwrap(),
            Ok(()),
            "one resolver that answers proves the network, whatever another does"
        );
        nudge.raise();
        assert_eq!(
            prove(&[open], None, &preempt, limit).unwrap(),
            Ok(()),
            "a nudge for a wish that waits its turn ends nothing"
        );
        let start = Instant::now();
        assert_eq!(prove(&[closed], None, &preempt, limit).unwrap(), Err(JoinError::Silent));
        assert!(start.elapsed() < limit, "refused by every resolver, the proof ends before its limit");
        assert_eq!(prove(&[], None, &preempt, limit).unwrap(), Err(JoinError::Silent), "nobody to ask proves nothing");
    }
}

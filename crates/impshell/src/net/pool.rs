use crate::net::tls::Stream;
use crate::sys::lock;
use crate::wifi::radio::NetworkDown;
use r1_funcore::codec::http::{Idle, Target, take_idle};
use std::sync::Mutex;

/// Idle connections, shared by the threads of one owner.
#[derive(Default)]
pub struct Pool(Mutex<Vec<Idle<NetworkDown, Stream>>>);

impl Pool {
    /// An idle connection of the stay `down` to the target's server, which that server has written nothing to since but what TLS takes in itself; drops what ended stays left and closes one its server is done with.
    pub fn take(&self, target: &Target, down: &NetworkDown) -> Option<Stream> {
        loop {
            let mut stream = {
                let mut idle = lock(&self.0);
                idle.retain(|idle| !idle.stay.is_raised());
                take_idle(&mut idle, target, down)
            }?;
            if matches!(stream.is_ready(), Ok(false)) {
                return Some(stream);
            }
            stream.close();
        }
    }

    /// Drops what ended stays left idle, sending nothing: the network under it is gone.
    pub fn drop_ended(&self) {
        lock(&self.0).retain(|idle| !idle.stay.is_raised());
    }

    /// Leaves a connection idle for the next request of its stay to the target's server.
    pub fn put(&self, target: &Target, stream: Stream) {
        let idle = Idle::new(stream.network_down().clone(), target.clone(), stream);
        lock(&self.0).push(idle);
    }

    #[cfg(test)]
    pub fn idle_count(&self) -> usize {
        lock(&self.0).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::served::{Connection, listen};
    use crate::wifi::radio::tests::up;
    use r1_funcore::net::tls::Purpose;
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

    fn connect_pair(listener: &TcpListener, target: &Target, down: &NetworkDown) -> (Stream, Connection) {
        let clone = listener.try_clone().unwrap();
        let server = thread::spawn(move || Connection::accept(&clone));
        let addresses = [listener.local_addr().unwrap()];
        let (stream, ..) = Stream::reach(&addresses, &target.host, Purpose::Http, Instant::now(), down).unwrap();
        (stream, server.join().unwrap())
    }

    #[test]
    fn idle_connection_reuse() {
        let (target, listener) = listen("/v1/responses");
        let other_server = Target { port: target.port.wrapping_add(1), ..target.clone() };
        let (pool, first, second) = (Pool::default(), up(), up());
        let (stream, connection) = connect_pair(&listener, &target, &first);
        let mut connections = vec![connection];
        pool.put(&target, stream);
        assert!(
            pool.take(&target, &second).is_none() && pool.take(&other_server, &first).is_none(),
            "neither another stay nor another server takes it"
        );
        let stream = pool
            .take(&target, &first)
            .expect("its own stay and server take it, the session tickets the server sent after the greeting no news");
        assert_eq!(pool.idle_count(), 0, "and it is no longer left open");
        pool.put(&target, stream);
        let (stream, connection) = connect_pair(&listener, &target, &second);
        connections.push(connection);
        pool.put(&target, stream);
        first.raise();
        pool.drop_ended();
        assert_eq!(pool.idle_count(), 1, "what a stay that ended left open goes as it ends, another stay's stays");
        second.raise();
        assert!(
            pool.take(&target, &up()).is_none() && pool.idle_count() == 0,
            "and on the way to a request, at the latest"
        );
        let third = up();
        let (mut stream, connection) = connect_pair(&listener, &target, &third);
        connections.push(connection);
        drop(connections);
        // The server's close has reached the connection, behind its session tickets, before it is left open.
        while !stream.is_ready().unwrap() {
            let mut pollfd = libc::pollfd { fd: stream.fd(), events: libc::POLLIN, revents: 0 };
            assert_eq!(unsafe { libc::poll(&mut pollfd, 1, 10_000) }, 1);
        }
        pool.put(&target, stream);
        assert!(
            pool.take(&target, &third).is_none() && pool.idle_count() == 0,
            "and a connection its server closed is not handed out"
        );
    }
}

//! ICE (RFC 8445) as a full agent of either role toward one far side or several that share its credentials, sockets and candidates (a caller's offer answered by each device of the callee), with trickle (RFC 8838), consent freshness (RFC 7675), TCP toward a server's passive candidates (RFC 6544) and relays through TURN: candidates as attribute lines, checks, nomination, and media over each side's selected pair.

mod agent;
mod candidate;
mod check;
mod gather;
#[cfg(test)]
mod tests;

pub use agent::Agent;
pub use candidate::Kind;

use crate::net::stun::Transport;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::{IpAddr, SocketAddr};

/// The number the agent names a socket by; the owner keeps its socket under it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Socket(u32);

/// Which side nominates the pair media goes over (RFC 8445, 6.1.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Controlling,
    Controlled,
}

/// How a far side stands; it begins connecting and tells each change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    // No pair is selected yet.
    Connecting,
    // A selected pair carries media under consent.
    Connected,
    // The selected pair lost its consent (RFC 7675, 5.1); checks go on.
    Disconnected,
    // Nothing was left to check for as long as consent lasts, and no pair succeeded.
    Failed,
}

/// The selected pair, without its addresses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Path {
    local: Kind,
    far: Kind,
    // How media leaves this side: to the far side, or to the relay for a relayed candidate.
    transport: Transport,
}

impl Path {
    // True where media goes through a relay on either side.
    pub(crate) fn is_relayed(&self) -> bool {
        self.local == Kind::Relay || self.far == Kind::Relay
    }
}

/// What the agent has its owner do, or tells it: one effect each, in this order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    // Opens a UDP socket on the local address, any port; `Agent::take_bound` takes what came of it.
    Bind { socket: Socket, local: IpAddr },
    // Connects a TCP stream from the local address to the far one, under TLS checked by the name where one is given; `Agent::take_connected` takes what came of it.
    Connect { socket: Socket, local: IpAddr, far: SocketAddr, name: Option<String> },
    // A datagram for the address, from the socket.
    Send { socket: Socket, to: SocketAddr, datagram: Vec<u8> },
    // Bytes for the stream, written as they are.
    Write { socket: Socket, bytes: Vec<u8> },
    Close(Socket),
    // A local candidate's attribute line, for every far side.
    Candidate(String),
    // A local candidate that is gone, for every far side, by its address.
    Removed(SocketAddr),
    // The pair media goes over to a far side changed; none while no pair is selected.
    Selected { side: u32, path: Option<Path> },
    State { side: u32, state: State },
    // A datagram that is no STUN from a far side, believed in nothing: media for the owner to authenticate.
    Deliver { side: u32, packet: Vec<u8> },
    Log(String),
}

const fn transport_word(transport: Transport) -> &'static str {
    match transport {
        Transport::Udp => "udp",
        Transport::Tcp => "tcp",
        Transport::Tls => "tls",
    }
}

fn to_state_line(side: u32, state: State, pairs: usize) -> String {
    let word = match state {
        State::Connecting => "connecting",
        State::Connected => "connected",
        State::Disconnected => "disconnected, consent lost",
        State::Failed => "failed",
    };
    format!("ice: side {side} {word}, {pairs} pairs")
}

fn to_path_line(side: u32, path: Option<Path>) -> String {
    match path {
        Some(path) => format!(
            "ice: side {side} media over {} from {} to {}",
            transport_word(path.transport),
            path.local.word(),
            path.far.word()
        ),
        None => format!("ice: side {side} no pair selected"),
    }
}

fn to_relay_line(transport: Transport, failure: crate::net::turn::Failure) -> String {
    use crate::net::turn::Failure;
    let reason = match failure {
        Failure::Timeout => String::from("no answer"),
        Failure::Refused(code) => format!("refused {code}"),
        Failure::Expired => String::from("credential run out"),
        Failure::Lost => String::from("allocation lost"),
        Failure::Unverified => String::from("integrity broken"),
    };
    format!("ice: relay over {} failed: {reason}", transport_word(transport))
}

fn to_discard_line(transport: Transport, code: Option<u16>, discard: crate::net::turn::Discard) -> String {
    use crate::net::turn::Discard;
    let answer = code.map_or_else(|| String::from("a success"), |code| format!("{code}"));
    let reason = match discard {
        Discard::Unsealed => "not sealed",
        Discard::Unverified => "integrity broken",
        Discard::Downgraded => "a downgrade",
    };
    format!("ice: relay over {} answered {answer}, passed over: {reason}", transport_word(transport))
}

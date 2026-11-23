// TURN (RFC 8656) as a client: a relayed address allocated under a long-term credential, its refreshes, its permissions and channels, and data to and from peers through it.

mod auth;
mod client;

pub(crate) use client::{Client, Discard, Event, Failure};

use crate::net::stun::Transport;
use alloc::string::String;
use core::net::SocketAddr;
use core::time::Duration;

// A relay and its credential, handed over for one call; its `Debug` tells no credential.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Server {
    pub(crate) address: SocketAddr,
    pub(crate) transport: Transport,
    // The name its certificate is checked by under TLS.
    pub(crate) name: Option<String>,
    pub(crate) username: String,
    pub(crate) password: String,
    // When the credential runs out, as time since boot.
    pub(crate) expires: Duration,
}

impl core::fmt::Debug for Server {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Server").field("transport", &self.transport).finish_non_exhaustive()
    }
}

#[cfg(test)]
pub(super) mod tests;

//! What the screens and the call layer ask of a stay on the socket.

use super::address::{Peer, ServiceId};
use super::attachment::Enclosure;
use super::call::Signal;
use super::device::{Course, Note};
use alloc::string::String;
use alloc::vec::Vec;

/// A message to send to a conversation, with the files it carries.
#[derive(Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub(crate) peer: Peer,
    pub(crate) body: String,
    pub(crate) is_robot: bool,
    pub(crate) enclosures: Vec<Enclosure>,
}

impl core::fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Outgoing").field("robot", &self.is_robot).finish_non_exhaustive()
    }
}

// Whom a call's signal goes to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Target {
    // Every device of a contact.
    Contact(ServiceId),
    // Every member of a group but the owner.
    Group(Peer),
    // These members of a group.
    Members(Peer, Vec<ServiceId>),
    // The account's other devices.
    Own,
}

/// A call's signal to send, written for one device of its recipient's alone where it names one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Outbound {
    pub(crate) target: Target,
    pub(crate) device: Option<u32>,
    pub(crate) signal: Signal,
}

/// What the screens and the call layer ask of a stay.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    Send(Outgoing),
    // The conversation shows: its messages are read.
    Read(Peer),
    // Auto mode for a contact's conversation, or none.
    Auto(Option<ServiceId>),
    // A call's signal, sent at once or never.
    Signal(Outbound),
    // What became of a call, for the log.
    Course(Note, Course),
    // The call log shows: its calls count as seen.
    Seen,
    // The relays of a one-to-one call.
    Relays,
    // The token of a group's call.
    Token(Peer),
}

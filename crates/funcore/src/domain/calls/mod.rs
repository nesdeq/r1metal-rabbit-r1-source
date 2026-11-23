//! Signal's calls: what the owner asks of a call and how one stands for the screens.

mod direct;
mod group;
mod machine;
mod picture;
pub mod state;

pub use machine::{Library, Machine, Step};
pub use picture::{Frame, Pictures, Sender};

use super::cause::Cause;
use crate::signal::address::Peer;
use alloc::string::String;
use alloc::vec::Vec;
use core::time::Duration;

/// What the owner asks of a call.
#[derive(Clone, PartialEq, Eq)]
pub enum Order {
    // Calls a contact, or joins a group's call, by the name its line shows, with this side's picture where `is_video`.
    Call { peer: Peer, name: Option<String>, is_video: bool },
    // Takes the call that rings.
    Accept,
    // Sends the call that rings away.
    Decline,
    // Ends the call that stands, rings at the far side or is on its way there.
    End,
    // Has the microphone heard no more, or again.
    Mute(bool),
    // Sends this side's picture, or no more.
    Video(bool),
}

impl core::fmt::Debug for Order {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Order::Call { is_video, .. } => f.debug_struct("Call").field("video", is_video).finish_non_exhaustive(),
            Order::Accept => f.write_str("Accept"),
            Order::Decline => f.write_str("Decline"),
            Order::End => f.write_str("End"),
            Order::Mute(is_muted) => f.debug_tuple("Mute").field(is_muted).finish(),
            Order::Video(is_on) => f.debug_tuple("Video").field(is_on).finish(),
        }
    }
}

/// Why a call is over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ending {
    // The owner ended it.
    HungUp,
    // The far side ended it.
    FarHungUp,
    // The far side sent it away.
    Declined,
    // The far side is in another call.
    Busy,
    // Nobody took it while it rang, there or here.
    Unanswered,
    // Another device of the owner's took it or sent it away.
    Elsewhere,
    // The far side takes no calls of this account before it accepts its request.
    Unpermitted,
    // A group's call takes no more devices.
    Full,
    Failed(Cause),
}

/// How a call stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    // On its way to the far side, which is not known to ring yet.
    Calling,
    // The far side rings.
    Ringing,
    // It rings here.
    Incoming,
    // Taken, the sound's way not yet open.
    Connecting,
    Talking,
    // The sound's way broke and is sought anew.
    Reconnecting,
    Ended(Ending),
}

/// A device in a group's call beside the owner's, as the call's screen shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Member {
    pub name: Option<String>,
    pub is_speaking: bool,
    pub is_muted: bool,
    pub has_video: bool,
}

impl core::fmt::Debug for Member {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Member")
            .field("speaking", &self.is_speaking)
            .field("muted", &self.is_muted)
            .field("video", &self.has_video)
            .finish_non_exhaustive()
    }
}

/// A call as its screen shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct View {
    pub peer: Peer,
    // The contact's or the group's name as Signal's list has it.
    pub name: Option<String>,
    pub is_outgoing: bool,
    // Begun as a video call: an offer of one, or a group's call.
    pub is_video: bool,
    pub phase: Phase,
    // When the talking began, by the paced clock.
    pub since: Option<Duration>,
    pub is_muted: bool,
    // This side's picture is sent.
    pub is_video_on: bool,
    // A picture of the far side comes.
    pub has_far_video: bool,
    // A group's call: who is in it beside the owner.
    pub members: Vec<Member>,
}

impl core::fmt::Debug for View {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("View")
            .field("outgoing", &self.is_outgoing)
            .field("video_call", &self.is_video)
            .field("phase", &self.phase)
            .field("muted", &self.is_muted)
            .field("video", &self.is_video_on)
            .field("far_video", &self.has_far_video)
            .field("members", &self.members.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::address::ServiceId;
    use alloc::format;

    #[test]
    fn debug_hides_names() {
        let member = Member { name: Some("Ann".into()), is_speaking: true, is_muted: false, has_video: false };
        let view = View {
            peer: Peer::Contact(ServiceId::Aci([4; 16])),
            name: Some("Ann".into()),
            is_outgoing: true,
            is_video: true,
            phase: Phase::Ended(Ending::Failed(Cause::NoNetwork)),
            since: None,
            is_muted: false,
            is_video_on: true,
            has_far_video: false,
            members: alloc::vec![member.clone()],
        };
        let call = Order::Call { peer: view.peer, name: Some("Ann".into()), is_video: true };
        assert_eq!(
            format!("{view:?} {member:?} {call:?} {:?}", Order::Mute(true)),
            "View { outgoing: true, video_call: true, phase: Ended(Failed(NoNetwork)), muted: false, video: true, far_video: false, members: 1, .. } Member { speaking: true, muted: false, video: false, .. } Call { video: true, .. } Mute(true)",
            "neither with whom a call is nor a name is told where formatted"
        );
    }
}

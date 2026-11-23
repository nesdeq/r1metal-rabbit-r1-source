use crate::base::time::setting::Part;
use crate::hw::power::Power;

/// What an event asks of the device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Power(Power),
    Sleep,
    // The button was pressed while the device sleeps.
    Wake,
    // A photo, or the start or end of a recording.
    Shoot,
    // A photo for the robot to see.
    Look,
    // Plays the open video, or holds the one that plays.
    Play,
    Time,
    Clock(Part, i32),
    // A finger on the battery with a cable attached.
    Charge,
}

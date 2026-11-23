//! A video on show, played and held by clicks.

use crate::input::pose::Pose;

/// What a video on show acts on next.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VideoStep {
    Request,
    End,
    // The pictures to draw ahead.
    Draw(usize),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Click {
    Play { is_restart: bool },
    Hold,
}

/// The screen's request of a video on show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ask {
    pub(super) is_wanted: bool,
    pub(super) clicks: u32,
    pub(super) pose: Pose,
}

/// The thread's next step for the screen's request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlayStep {
    // Whether it played.
    Leave(bool),
    // Whether the standing picture is drawn again for the new pose.
    Turn(bool),
    // Whether it plays from its start.
    Play(bool),
    Hold,
    Await,
}

/// A video on show as clicks play and hold it.
pub struct Player {
    pub(super) is_playing: bool,
    /// The pose its pictures are drawn for.
    pub pose: Pose,
    ask: Option<Ask>,
}

impl Player {
    /// A video that stands.
    pub const fn new(pose: Pose) -> Player {
        Player { is_playing: false, pose, ask: None }
    }

    pub const fn begin_pass(&mut self, ask: Ask) {
        self.ask = Some(ask);
    }

    /// The next step for the screen's request, every picture drawn where `is_done`.
    pub fn next_step(&mut self, is_done: bool) -> PlayStep {
        let Some(ask) = self.ask else {
            return PlayStep::Await;
        };
        if !ask.is_wanted {
            self.ask = None;
            return PlayStep::Leave(self.is_playing);
        }
        if let Some(needs_redraw) = self.take_pose(ask.pose) {
            return PlayStep::Turn(needs_redraw);
        }
        self.ask = None;
        match self.take_clicks(ask.clicks, is_done) {
            Some(Click::Play { is_restart }) => PlayStep::Play(is_restart),
            Some(Click::Hold) => PlayStep::Hold,
            None => PlayStep::Await,
        }
    }

    /// Notes that the video showed its last picture.
    pub const fn note_end(&mut self) {
        self.is_playing = false;
    }

    fn take_pose(&mut self, pose: Pose) -> Option<bool> {
        let has_turned = pose != self.pose;
        self.pose = pose;
        has_turned.then_some(!self.is_playing)
    }

    pub(super) fn take_clicks(&mut self, clicks: u32, is_done: bool) -> Option<Click> {
        if clicks.is_multiple_of(2) {
            return None;
        }
        self.is_playing = !self.is_playing;
        Some(if self.is_playing { Click::Play { is_restart: is_done } } else { Click::Hold })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_and_turns() {
        let mut player = Player::new(Pose::Upright);
        assert_eq!((player.take_clicks(2, false), player.is_playing), (None, false), "two clicks come to none");
        assert_eq!(player.take_clicks(1, false), Some(Click::Play { is_restart: false }));
        assert_eq!(player.take_pose(Pose::Upright), None);
        assert_eq!(
            (player.take_pose(Pose::RightUp), player.pose),
            (Some(false), Pose::RightUp),
            "one that plays draws its next pictures turned"
        );
        assert_eq!(player.take_clicks(3, false), Some(Click::Hold));
        assert_eq!(player.take_pose(Pose::Upright), Some(true), "one that stands is drawn again");
        assert_eq!(
            player.take_clicks(1, true),
            Some(Click::Play { is_restart: true }),
            "from its start where it is over"
        );
        player.note_end();
        assert_eq!(
            player.take_clicks(1, true),
            Some(Click::Play { is_restart: true }),
            "one that showed its last picture stands, and a click plays it again"
        );
    }

    #[test]
    fn pass_steps() {
        let ask = |is_wanted, clicks, pose| Ask { is_wanted, clicks, pose };
        let mut player = Player::new(Pose::Upright);
        assert_eq!(player.next_step(false), PlayStep::Await, "nothing asked");
        player.begin_pass(ask(true, 1, Pose::RightUp));
        assert_eq!(player.next_step(false), PlayStep::Turn(true));
        assert_eq!(player.next_step(true), PlayStep::Play(true));
        assert_eq!(player.next_step(false), PlayStep::Await);
        player.begin_pass(ask(true, 1, Pose::RightUp));
        assert_eq!(player.next_step(false), PlayStep::Hold);
        player.begin_pass(ask(true, 1, Pose::RightUp));
        assert_eq!(player.next_step(false), PlayStep::Play(false));
        player.begin_pass(ask(false, 1, Pose::Upright));
        assert_eq!(player.next_step(false), PlayStep::Leave(true), "left, as it played");
        player.note_end();
        player.begin_pass(ask(false, 0, Pose::Upright));
        assert_eq!(player.next_step(false), PlayStep::Leave(false));
    }
}

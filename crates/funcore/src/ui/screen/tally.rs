use crate::domain::chat::tools::Destination;
use crate::domain::tally::Tally;
use crate::domain::uses::Use;
use crate::ui::Ui;

const TALLY_USES: [Use; 4] = [
    Use::Open(Destination::Pictures),
    Use::Open(Destination::Videos),
    Use::Open(Destination::Notes),
    Use::Open(Destination::News),
];

impl Ui {
    /// True while no screen shows over the stages or the menus, the device awake.
    pub fn does_home_show(&self) -> bool {
        self.is_awake() && self.screen.is_none()
    }

    /// Takes the gallery's count of what the rolls hold; true if the screen changed.
    #[must_use]
    pub fn set_rolls(&mut self, rolls: Tally) -> bool {
        let Tally { notes, newest_note_s, has_edition, .. } = self.tally;
        self.set_tally(Tally { notes, newest_note_s, has_edition, ..rolls })
    }

    pub(super) fn set_shelves(&mut self, shelves: Tally) -> bool {
        let Tally { notes, newest_note_s, has_edition, .. } = shelves;
        self.set_tally(Tally { notes, newest_note_s, has_edition, ..self.tally })
    }

    /// Takes what the device holds; true if the screen changed.
    #[must_use]
    pub fn set_tally(&mut self, tally: Tally) -> bool {
        if core::mem::replace(&mut self.tally, tally) == tally {
            return false;
        }
        self.redeal_lines(&TALLY_USES) || self.is_menu_home()
    }
}

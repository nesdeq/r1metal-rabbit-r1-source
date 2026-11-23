mod address;
mod association;
mod iface;
mod nl80211;
pub mod radio;
mod reach;
mod scan;

use crate::sync::Board;
use crate::sys::Wake;
use radio::State;

struct Preempt<'a> {
    nudge: &'a Wake,
    board: &'a Board<State>,
    urgent_count: usize,
}

impl Preempt<'_> {
    fn is_pending(&self) -> bool {
        self.board.lock().has_more(self.urgent_count)
    }

    fn should_yield(&self) -> bool {
        self.nudge.clear();
        self.is_pending()
    }
}

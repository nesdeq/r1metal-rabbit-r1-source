use super::{Robot, files};
use crate::openai::write;
use crate::voiced::Shared;
use crate::wifi::radio::Hold;
use crate::{clock, log};
use r1_funcore::base::time::Moment;
use r1_funcore::base::time::stamp::parse_compact_day;
use r1_funcore::domain::chat::recollect::{self, Recollection};
use r1_funcore::domain::chat::transcript;
use r1_funcore::storage::CHAT_TALK;

pub(super) fn work(shared: &Shared<Robot>) -> ! {
    loop {
        shared.board.wait(|state| state.take_look_back().then_some(()));
        if !shared.is_ready() {
            continue;
        }
        let today = Moment::from_seconds(clock::local()).to_compact_date();
        // The network, kept from one day to the next.
        let mut hold = None;
        for (day, is_kept) in files::unsettled_days(&shared.job.data, &today) {
            // A sleep ends the settling between two days; a day that is asked for is seen to its end.
            if !shared.set_recalling(true) {
                break;
            }
            settle(shared, &mut hold, &day, is_kept);
            shared.set_recalling(false);
        }
    }
}

fn settle(shared: &Shared<Robot>, slot: &mut Option<Hold>, day: &str, is_kept: bool) {
    let words = match shared.job.data.text(CHAT_TALK, &transcript::file_name(day)) {
        Ok(words) => words,
        Err(e) => return log!("{}: {e}", recollect::to_unread_line(day)),
    };
    let (Some(moment), Some(words)) = (parse_compact_day(day), words) else {
        return log!("{}", recollect::to_unread_line(day));
    };
    let memory = shared.read_store(|store| store.memory.clone());
    let request = recollect::to_request(&memory, &moment, &words);
    let answer = write::write_recollection(&shared.account, slot, &request);
    if !is_kept {
        *slot = None;
    }
    let answer = match answer {
        Ok(answer) => answer,
        Err(fault) => return log!("chat: {day} is not settled: {fault}"),
    };
    let data = &shared.job.data;
    // Under the store's loan, which a day's forgetting takes too: the day's words are looked for and what it settled is written with no forgetting between.
    let is_settled = shared.change_store(|store| {
        match files::is_day_kept(data, day) {
            Ok(true) => {}
            Ok(false) => {
                log!("{}", recollect::to_forgotten_line(day));
                return false;
            }
            Err(e) => {
                log!("chat: whether {day} is kept cannot be told, and what it settled is not kept: {e}");
                return false;
            }
        }
        let text = match Recollection::parse(&answer) {
            Some(recollection) => {
                store.memory.apply_recollection(&memory, recollection.core, recollection.knowledge);
                files::write_memory(data, &store.memory);
                recollection.day
            }
            None => {
                log!("{}", recollect::to_unsectioned_line(day));
                answer
            }
        };
        match files::write_day(data, day, &moment, &text) {
            Ok(()) => log!("{}", recollect::to_settled_line(day, &text)),
            Err(e) => log!("chat: what {day} settled is not kept: {e}"),
        }
        true
    });
    if is_settled {
        shared.board.lock().job.mark_stale();
    }
}

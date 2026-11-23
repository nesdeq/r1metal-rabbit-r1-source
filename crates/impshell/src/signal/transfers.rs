use super::{Resources, Shared};
use crate::crypto::Library;
use crate::log;
use crate::net::web;
use r1_funcore::domain::signal::state::State;
use r1_funcore::signal::attachment::{Finish, Job, Step, Took, Transfer, to_finished_line};

// Carries a transfer out step by step, holding the network for it alone.
fn carry(shared: &Shared, resources: &Resources, job: Job) -> (Job, Finish) {
    let hold = match resources.online.hold() {
        Ok(hold) => hold,
        Err(fault) => {
            log!("signal: a transfer without the network: {fault}");
            shared.board.lock().hold_transfers();
            return (job, Finish::Broke);
        }
    };
    let mut transfer = Transfer::new(job.clone());
    loop {
        let took = match transfer.next() {
            Step::Finished(finish) => return (job, finish),
            Step::Read(directory, name) => {
                let file = resources.data.read(directory, name);
                Took::Read(file.inspect_err(|e| log!("signal: an attachment is not read: {e}")).ok().flatten())
            }
            Step::Keep(directory, name, bytes) => {
                let result = resources.data.put(directory, name, bytes);
                if let Err(e) = &result {
                    log!("signal: an attachment is not kept: {e}");
                }
                Took::Kept(result.is_ok())
            }
            Step::Ask(request) => match web::ask_request(&request, hold.down()) {
                Ok((head, body)) => Took::Answered(head, body),
                Err(e) => {
                    log!("signal: a transfer's request: {e}");
                    Took::Broke
                }
            },
        };
        transfer.take(&Library, took);
    }
}

// The thread that carries attachments up and down, one transfer at a time, beside the thread that keeps Signal linked.
pub(super) fn work(shared: &Shared, resources: &Resources) -> ! {
    loop {
        let job = shared.board.wait(State::take_job);
        let (job, finish) = carry(shared, resources, job);
        log!("{}", to_finished_line(&job, &finish));
        shared.board.lock().note_finished(job, finish);
        shared.wake_thread();
    }
}

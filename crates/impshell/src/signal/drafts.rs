use crate::log;
use crate::openai::{Account, transcript};
use crate::voiced::{Part, Shared, Worker};
use crate::wifi::radio::{Hold, NetworkDown};
use r1_funcore::domain::signal::drafts::{Draft, Recording};
use r1_funcore::domain::voice::Phase;
use r1_funcore::openai::fault::Fault;
use r1_funcore::openai::transcript::Subject;
use r1_funcore::signal::DRAFT_THREADS;
use std::ffi::CStr;
use std::io;

/// The worker that writes a spoken message down for a draft.
pub struct Writer;

/// The main loop's handle of the job that writes spoken messages down.
pub type Drafts = Part<Writer>;

impl Drafts {
    /// Starts the threads, idle until a conversation shows.
    pub fn start(account: Account) -> io::Result<Drafts> {
        Part::spawn(Writer, false, account)
    }

    /// Sets whether a message said is kept as a voice note in place of being written down.
    pub fn set_voice(&self, is_voice: bool) {
        self.shared.board.lock().job.set_voice(is_voice);
    }
}

impl Worker for Writer {
    type State = Draft;
    type Stay = Hold;
    const NAME: &str = "signal";
    const THREADS: [&str; 2] = DRAFT_THREADS;
    const FILE: &CStr = c"signal";
    const DOWN: fn(&Hold) -> &NetworkDown = Hold::down;

    fn stay(&self, shared: &Shared<Self>, _: bool) -> Result<Hold, Fault> {
        shared.account.hold()
    }

    fn turn(&self, shared: &Shared<Self>, hold: &mut Hold, recordings: Vec<Recording>) {
        for recording in recordings {
            shared.set_phase(Phase::Writing);
            let sound = (&recording.sound[..], recording.seconds);
            match transcript::transcribe(&shared.account, hold.down(), sound, (Subject::Message, ""), None) {
                Ok(words) => shared.tell(|state| state.take_words(words)),
                Err(fault) => {
                    log!("signal: the message said is not written down: {fault}");
                    shared.fail(&fault);
                }
            }
        }
    }
}

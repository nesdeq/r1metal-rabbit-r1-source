use crate::log;
use crate::sync::Fetcher;
use crate::sys;
use r1_funcore::base::time::suspended_ns;
use r1_funcore::hw::power::doze::Doze;
use std::fs;
use std::io;

const STATE: &str = "/sys/power/state";
const SUSPEND_TO_RAM: &str = "mem";
const WAKEUP_COUNT: &str = "/sys/power/wakeup_count";

/// The kernel's wake count, fetched by its own thread: the kernel answers only once nothing keeps it awake.
pub type Gate = Fetcher<(), Option<String>>;

impl Gate {
    /// Starts the thread that asks the kernel.
    pub fn new() -> io::Result<Gate> {
        Fetcher::start("suspend", |()| {
            fs::read_to_string(WAKEUP_COUNT)
                .inspect_err(|e| log!("sleep: asking the kernel what keeps it awake: {e}"))
                .ok()
        })
    }

    /// Takes the kernel's answer, if it came, and clears the question for the next.
    pub fn take_answer(&self) -> Option<Option<String>> {
        let answer = self.latest();
        self.want(None);
        answer
    }
}

/// Suspends the system where the parts are quiet still, and returns when it runs again; the kernel takes its count back only if nothing woke it since.
pub fn suspend(doze: &mut Doze, is_quiet: bool) {
    let Some(count) = doze.hand_back(is_quiet) else {
        return;
    };
    if let Err(e) = fs::write(WAKEUP_COUNT, &count) {
        return log!("sleep: handing the kernel its wake count back: {e}");
    }
    let start_clocks = (sys::monotonic_ns(), sys::boottime_ns());
    let result = fs::write(STATE, SUSPEND_TO_RAM);
    let end_clocks = (sys::monotonic_ns(), sys::boottime_ns());
    if let Err(e) = &result {
        log!("sleep: the kernel did not suspend: {e}");
    }
    doze.take_suspend(result.is_err(), end_clocks.0, suspended_ns(start_clocks, end_clocks));
}

use crate::storage::settings::Settler;
use crate::storage::{Data, logs};
use crate::sync::Board;
use crate::sys;
use crate::{log, usb};
use r1_funcore::hw::power::battery::{
    self, Charger, Reading, Recall, Record, Report, Sample, State, Step, Switch, UNSAVED_LINE,
};
use r1_funcore::hw::power::{Power, SHUTDOWN_TIMEOUT, usage};
use r1_funcore::hw::uevent::HostState;
use r1_funcore::logs::LineFile;
use std::fs;
use std::io;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

const VOLTAGE: &str = "/sys/class/power_supply/mtk-gauge/bat_vol_get";
const CURRENT: &str = "/sys/class/power_supply/mtk-gauge/battery_current_get";
const CHARGE: &str = "/sys/class/power_supply/mtk-gauge/coulomb_get";
const TEMPERATURE: &str = "/sys/class/power_supply/battery/temperature";
const STATUS: &str = "/sys/class/power_supply/battery/status";
const KERNEL_LEVEL: &str = "/sys/devices/platform/low_battery_throttling/low_battery_protect_level";
const CHARGE_SWITCH: &str = "/proc/mtk_battery_cmd/current_cmd";

fn read_sample(at_ns: u64, suspend_ms: u64, charger: Charger) -> io::Result<Sample> {
    Ok(Sample {
        at_ns,
        suspend_ms,
        voltage: sys::read_number(VOLTAGE)?,
        current: sys::read_number(CURRENT)?,
        charge: sys::read_number(CHARGE)?,
        temperature: sys::read_number(TEMPERATURE)?,
        charger,
    })
}

fn write_switch(change: &Switch) {
    log!("{}", change.line);
    // A kernel without the switch is asked once, and charges as it always did.
    if let Err(e) = fs::write(CHARGE_SWITCH, change.to_text()) {
        log!("battery: the kernel's switch for the charge: {e}");
    }
}

struct Shared {
    board: Board<State>,
    data: Data,
    keeper: Settler,
}

impl Shared {
    fn read_stored(&self, file: &str, action: &str) -> Option<Vec<u8>> {
        self.data.read(battery::DIRECTORY, file).inspect_err(|e| log!("battery: {action}: {e}")).ok().flatten()
    }

    fn save(&self, record: &Record) {
        if let Some(memory) = &record.memory
            && let Err(e) = self.data.put(battery::DIRECTORY, battery::FILE, memory.text().as_bytes())
        {
            log!("battery: keeping what the gauge has measured: {e}");
        }
        if let Some(usage) = record.usage
            && let Err(e) =
                self.data.put(battery::DIRECTORY, usage::FILE, usage.text(sys::realtime_seconds()).as_bytes())
        {
            log!("battery: keeping what the device has used: {e}");
        }
    }

    fn post(&self, reading: Reading) {
        self.board.lock().report.reading = Some(reading);
        self.board.raise();
    }
}

fn wait_sample(meter: &mut battery::Meter, shared: &Shared) {
    let mut state = shared.board.lock();
    loop {
        match meter.next_step(&mut state, sys::monotonic_ns()) {
            Step::Save(number) => {
                drop(state);
                shared.save(&meter.record());
                state = shared.board.lock();
                state.note_saved(number);
                shared.board.notify();
            }
            Step::Touch(is_hosted) => {
                drop(state);
                // A finger on the battery is answered at once, by the reading that shows: the gauge keeps its second.
                if let Some((change, reading)) = meter.touch(is_hosted) {
                    if let Some(change) = &change {
                        write_switch(change);
                    }
                    shared.post(reading);
                }
                state = shared.board.lock();
            }
            Step::Wait(wait) => state = shared.board.sleep_within(state, wait),
            Step::Sample => return,
        }
    }
}

fn recall(meter: &mut battery::Meter, shared: &Shared, counter: i32) {
    let stored = shared.read_stored(battery::FILE, "reading what the gauge kept");
    match meter.recall_memory(stored.as_deref(), counter) {
        Recall::Done(line) => line.iter().for_each(|line| log!("{line}")),
        Recall::Usage(line) => {
            log!("{line}");
            let utc = sys::realtime_seconds();
            let usage = shared.read_stored(usage::FILE, "reading what the device has used");
            if let Some(line) = meter.recall_usage(usage.as_deref(), utc) {
                log!("{line}");
            }
        }
    }
}

fn run(mut meter: battery::Meter, shared: &Shared) -> ! {
    meter.set_due(sys::monotonic_ns());
    loop {
        let status = battery::to_status(fs::read_to_string(STATUS).ok().as_deref());
        let now_ns = sys::monotonic_ns();
        let suspend_ms = meter.note_clocks(now_ns, sys::boottime_ns());
        match read_sample(now_ns, suspend_ms, meter.charger_of(&status)) {
            Ok(sample) => {
                // The partition is asked without waiting: while a first start formats it the cell is watched all the same.
                if meter.is_recall_due(shared.data.is_ready()) {
                    recall(&mut meter, shared, sample.charge);
                }
                let (is_hosted, is_settled) = (shared.board.lock().is_hosted, shared.data.is_settled());
                let actions = meter.take(sample, status, sys::read_number(KERNEL_LEVEL).ok(), is_hosted, is_settled);
                actions.lines.iter().for_each(|line| log!("{line}"));
                if let Some(shutdown) = &actions.shutdown {
                    log!("{}", shutdown.line);
                    shared.save(&shutdown.record);
                    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
                    shared.keeper.settle(deadline);
                    logs::settle(deadline);
                    log!("power off refused: {}", sys::reboot(Power::PowerOff));
                }
                if let Some(change) = &actions.switch {
                    write_switch(change);
                }
                logs::add_line(LineFile::Power, &actions.log_line);
                if let Some((line, snapshot)) = &actions.level {
                    log!("{line}");
                    logs::snapshot(snapshot);
                }
                if actions.should_save {
                    shared.save(&meter.record());
                }
                shared.post(actions.reading);
            }
            Err(e) => {
                if let Some(message) = meter.note_unread(e.to_string()) {
                    log!("battery unreadable: {message}");
                }
            }
        }
        meter.note_sampled(sys::monotonic_ns());
        wait_sample(&mut meter, shared);
    }
}

pub struct Meter {
    shared: Arc<Shared>,
}

impl Meter {
    /// Starts sampling; the gauge resumes from `data`, the cutoff waits for `keeper`.
    pub fn start(data: Data, keeper: Settler) -> io::Result<Meter> {
        log!("{}", battery::to_cutoff_line());
        // A host that took the device up before this began has said so already.
        let mut state = State::default();
        state.is_hosted = usb::is_configured();
        // The kernel's boot, which the coulomb counter starts anew with.
        let boot = sys::boot_id().inspect_err(|e| log!("battery: the kernel's boot: {e}")).ok();
        let shared = Arc::new(Shared { board: Board::new(state)?, data, keeper });
        let worker = Arc::clone(&shared);
        thread::Builder::new().name("battery".into()).spawn(move || run(battery::Meter::new(boot), &worker))?;
        Ok(Meter { shared })
    }

    /// Takes the gadget's state: a host makes the cable a computer's.
    pub fn set_host_state(&self, host: HostState) {
        let mut state = self.shared.board.lock();
        state.is_hosted = host.next_hosted(state.is_hosted);
    }

    /// Takes a finger on the battery, answered at once.
    pub fn touch(&self) {
        self.shared.board.lock().touch();
        self.shared.board.notify();
    }

    /// Has the thread, which alone writes the gauge's files, store its record for a coming way down, waiting for it until `deadline`.
    pub fn save(&self, deadline: Instant) {
        let number = self.shared.board.lock().ask_save();
        self.shared.board.notify();
        let limit = deadline.saturating_duration_since(Instant::now());
        if !self.shared.board.wait_while_within(self.shared.board.lock(), limit, |state| !state.is_saved(number)) {
            log!("{UNSAVED_LINE}");
        }
    }

    /// Readable while a reading waits.
    pub fn fd(&self) -> RawFd {
        self.shared.board.fd()
    }

    /// Takes the newest reading.
    pub fn latest(&self) -> Report {
        self.shared.board.take(State::take_report)
    }
}

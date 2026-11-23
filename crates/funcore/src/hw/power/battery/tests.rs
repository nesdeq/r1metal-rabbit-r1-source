use super::*;

const SECOND_MS: u64 = 1000;
const RUN_DOWN: &str = include_str!("../../../../assets/battery/rundown-20261013.txt");
const RUN_DOWN_FROM: usize = 2891;
const TAPER: &str = include_str!("../../../../assets/battery/taper-20261021.txt");
const TAPER_FLAT_S: u64 = 4600;
const BOOT: &str = "4d3c2b1a";

pub(crate) fn sample(at_ms: u64, voltage: i32, current: i32, charge: i32, charger: Charger) -> Sample {
    Sample { at_ns: at_ms * NANOS_PER_MS, suspend_ms: 0, voltage, current, charge, temperature: 25, charger }
}

fn readings(text: &str) -> Vec<Sample> {
    let reading = |(second, line): (usize, &str)| {
        let mut words = line.split(' ');
        let mut number = || words.next().unwrap().parse::<i32>().unwrap();
        let (voltage, current, charge) = (number(), number(), number());
        let charger = match words.next().unwrap() {
            "F" => Charger::Done,
            "C" => Charger::Charging,
            _ => Charger::Idle,
        };
        sample(second as u64 * SECOND_MS, voltage, current, charge, charger)
    };
    text.lines().enumerate().map(reading).collect()
}

fn full_at(counter: i32, capacity: f64, runs: f64) -> Gauge {
    let mut gauge = Gauge::default();
    let memory = Memory { out: Some(0.0), capacity, runs, is_cut_off: false, counter, boot: String::from(BOOT) };
    gauge.recall(memory, BOOT, counter);
    gauge
}

fn run_down(gauge: &mut Gauge) -> Vec<Option<u8>> {
    let levels = readings(RUN_DOWN).iter().skip(RUN_DOWN_FROM).map(|sample| gauge.update(sample)).collect();
    gauge.cut(Verdict::LowVoltage(3398));
    levels
}

#[test]
fn taper_full() {
    let mut gauge = Gauge::default();
    let mut full_at = None;
    for (second, sample) in readings(TAPER).iter().enumerate() {
        let level = gauge.update(sample);
        if gauge.take_lines().iter().any(|line| line.starts_with("full")) {
            assert!(full_at.is_none(), "read full twice in one charge, at {second} s");
            assert_eq!((level, gauge.figures().out), (Some(100), Some(0)), "the charge out counted from there");
            full_at = Some(second as u64);
        }
        assert_eq!(level.is_some(), full_at.is_some(), "{second} s: the level is known from the first full charge on");
    }
    let full_at = full_at.expect("a cell whose current has stopped falling at the charger's voltage is full");
    assert!(
        (TAPER_FLAT_S..TAPER_FLAT_S + 30 * 60).contains(&full_at),
        "read full {full_at} s in, where its current stopped falling {TAPER_FLAT_S} s in"
    );
}

#[test]
fn low_current_full() {
    let mut gauge = Gauge::default();
    let mut full_at = None;
    // From 100 mA, a twentieth less current each minute at the charger's voltage: it never stops falling.
    for second in 0..120 * 60u64 {
        let current = (1000.0 * 0.95f64.powi((second / 60) as i32)) as i32;
        gauge.update(&sample(second * SECOND_MS, 4380, current, 0, Charger::Charging));
        if let Some(line) = gauge.take_lines().iter().find(|line| line.starts_with("full")) {
            full_at.get_or_insert((second, line.clone()));
        }
    }
    let (second, line) = full_at.expect("read full");
    assert!(line.starts_with("full: its current fell under 15 mA"), "{line}");
    // The ten minutes before minute 42 are the first to mean under 15 mA: 100 mA times 0.95 to the 33rd times 0.80.
    assert!((42 * 60..44 * 60).contains(&second), "at {second} s");
}

#[test]
fn run_down_measures() {
    let first = readings(RUN_DOWN)[RUN_DOWN_FROM].charge;
    let mut rated = full_at(first, ASSUMED_CAPACITY, 0.0);
    let levels = run_down(&mut rated);
    let (figures, lines) = (rated.figures(), rated.take_lines());
    assert!(
        figures.is_measured && (4000..=4100).contains(&figures.capacity),
        "measured where the voltage reads the cell empty, near the recording's end: {figures:?}"
    );
    assert!(lines.iter().any(|line| line.ends_with("mAh from full to cutoff")), "{lines:?}");
    let rises: Vec<usize> =
        levels.windows(2).enumerate().filter(|(_, pair)| pair[1] > pair[0]).map(|(at, _)| at).collect();
    assert!(rises.is_empty(), "the level never rises while the cell is drawn from: at {rises:?} s");
    assert!(levels.last().unwrap().unwrap() <= 1, "near the cutoff the voltage decides: {:?}", levels.last());

    // Measured, the next run down shows what is left all the way.
    let mut measured = full_at(first, f64::from(figures.capacity as i32), 1.0);
    let samples = readings(RUN_DOWN);
    let levels = run_down(&mut measured);
    for (second, level) in levels.iter().enumerate() {
        let out = f64::from(first - samples[RUN_DOWN_FROM + second].charge);
        let left = 100.0 * (1.0 - out / f64::from(figures.capacity as i32));
        if left > 6.0 {
            let level = f64::from(level.unwrap());
            assert!((level - left).abs() <= 1.5, "{second} s: {level} % on show with {left:.1} % left");
        }
    }
    assert_eq!(levels.last(), Some(&Some(0)), "empty at the cutoff");
}

#[test]
fn counter_across_restarts() {
    let mut gauge = full_at(-100, 4000.0, 1.0);
    gauge.update(&Sample { suspend_ms: 3_600_000, ..sample(SECOND_MS, 3900, -30, -130, Charger::Idle) });
    assert_eq!(gauge.figures().out, Some(30), "3 mAh drawn in an hour's suspend");
    let memory = Memory::parse(&gauge.memory(BOOT).text()).unwrap();
    assert_eq!(memory, gauge.memory(BOOT), "kept as it was");
    let mut restarted = Gauge::default();
    restarted.recall(memory.clone(), BOOT, -150);
    assert_eq!(restarted.figures().out, Some(50), "the counter went on counting 2 mAh in the same boot");
    let mut rebooted = Gauge::default();
    rebooted.recall(memory, "another", -5);
    assert_eq!(rebooted.figures().out, Some(35), "a new boot's counter began at nothing");
    rebooted.update(&sample(2 * SECOND_MS, 3900, -30, -15, Charger::Idle));
    assert_eq!(rebooted.figures().out, Some(45), "and counts on from what it read at the recall");
}

#[test]
fn recalled_capacity() {
    let unmeasured = Memory::parse("out 3644\ncapacity 10000\nruns 0\ncounter 593\nboot b\n").unwrap();
    let mut gauge = Gauge::default();
    gauge.recall(unmeasured, "b", 593);
    assert_eq!((gauge.figures().capacity, gauge.figures().is_measured), (ASSUMED_CAPACITY as i64, false));
    let measured = Memory::parse("out 3644\ncapacity 4241\nruns 1\ncounter 593\nboot b\n").unwrap();
    gauge.recall(measured, "b", 593);
    assert_eq!((gauge.figures().capacity, gauge.figures().is_measured), (4241, true));
}

#[test]
fn memory_round_trip() {
    let unknown = Gauge::default().memory(BOOT);
    assert_eq!(Memory::parse(&unknown.text()), Some(unknown.clone()), "a gauge yet to see a full charge");
    assert!(!unknown.text().contains("out"));
    let text = "out 120\ncapacity 4000\nruns 1\ncut 1\ncounter -55\nboot 4d3c2b1a\n";
    assert!(Memory::parse(text).is_some_and(|memory| memory.is_cut_off));
    assert!(Memory::parse(&text.replace("cut 1\n", "")).is_some_and(|memory| !memory.is_cut_off), "no cut yet");
    for broken in [
        "",
        "out x\ncapacity 4000\nruns 1\ncounter -55\nboot 4d3c2b1a\n",
        "out 120\ncapacity 99999\nruns 1\ncounter -55\nboot 4d3c2b1a\n",
        "out 120\ncapacity 4000\nruns 5\ncounter -55\nboot 4d3c2b1a\n",
        "out 120\ncapacity 4000\nruns 1\ncounter 1.5\nboot 4d3c2b1a\n",
        "out 120\ncapacity 4000\nruns 1\ncut 2\ncounter -55\nboot 4d3c2b1a\n",
        "out 120\ncapacity 4000\nruns 1\ncounter -55\n",
        "soc 0.5\nvariance 0.1\ncapacity 4000\nruns 1\ncounter -55\n",
    ] {
        assert_eq!(Memory::parse(broken), None, "{broken:?}");
    }
}

#[test]
fn charger_debounce() {
    let mut gauge = full_at(0, 4000.0, 1.0);
    // A full battery put on a cable: the kernel charges for a sample before the firmware holds the charge.
    gauge.update(&sample(SECOND_MS, 4300, -10, 0, Charger::Idle));
    assert_eq!(gauge.update(&sample(2 * SECOND_MS, 4390, 600, 0, Charger::Charging)), Some(100));
    for second in 3..60 {
        let level = gauge.update(&sample(second * SECOND_MS, 4300, -10, 0, Charger::Idle));
        assert_eq!(level, Some(100), "{second} s: a charge of a second is none to hold the level under full by");
    }
    // A charge that goes on is one from 30 s on, whatever the kernel says for 10 s after a boot.
    let charging = |at: u64| sample(at * SECOND_MS, 4390, 600, 0, Charger::Charging);
    for second in 60..90 {
        assert_eq!(gauge.update(&charging(second)), Some(100), "{second} s");
    }
    let held: Vec<Option<u8>> = (90..100).map(|second| gauge.update(&charging(second))).collect();
    assert!(held.contains(&Some(99)), "a charge not yet read full holds the level under full: {held:?}");
    let start = gauge.taper.as_ref().map(|taper| taper.start_ms);
    for second in 100..110 {
        gauge.update(&sample(second * SECOND_MS, 4390, 600, 0, Charger::Held));
    }
    gauge.update(&charging(110));
    assert_eq!(
        (gauge.debounce.stable, gauge.taper.as_ref().map(|taper| taper.start_ms)),
        (Charger::Charging, start),
        "the kernel's 10 s of \"Not charging\" after a boot leave the charge and its taper as they were"
    );
    gauge.update(&sample(111 * SECOND_MS, 4300, -10, 0, Charger::Idle));
    assert_eq!(gauge.update(&sample(121 * SECOND_MS, 4300, -10, 0, Charger::Idle)), Some(100), "the cable pulled");
}

#[test]
fn level_rules() {
    // A window of samples at a voltage, a current and a count out since full; the level they leave, in percent, before it is shown.
    let level = |voltage: i32, current: i32, out: i32, charger: Charger| {
        let mut gauge = full_at(0, 4000.0, 1.0);
        for second in 1..=CUTOFF_WINDOW as u64 {
            gauge.update(&sample(second * SECOND_MS, voltage, current, -out, charger));
        }
        gauge.level(gauge.debounce.stable).map(|level| level as f64 / (LEVEL_SCALE / PERCENT) as f64)
    };
    assert_eq!(level(3900, -1000, 1000, Charger::Idle), Some(75.0), "100 mAh of 400 drawn");
    assert_eq!(
        level(3900, -1000, 4400, Charger::Idle),
        Some(1.0),
        "counted past its end, the cell still carries the device"
    );
    assert_eq!(level(3450, -1000, 1000, Charger::Idle), Some(2.5), "half way from 3.5 V to the cutoff, half of 5 %");
    assert_eq!(level(3395, -1000, 1000, Charger::Idle), Some(0.0), "under the cutoff's voltage");
    assert_eq!(
        level(3450, -2500, 1000, Charger::Idle),
        Some(75.0),
        "under a heavy load the voltage tells nothing of the end"
    );
    assert_eq!(level(4390, 1000, -50, Charger::Charging), Some(100.0), "a charge's first seconds are none yet");
    assert_eq!(Gauge::default().level(Charger::Idle), None, "nothing before the first full charge");
}

#[test]
fn level_slew() {
    let mut gauge = full_at(0, 4000.0, 1.0);
    gauge.update(&sample(SECOND_MS, 3900, -1000, 0, Charger::Idle));
    assert_eq!(gauge.update(&sample(2 * SECOND_MS, 3900, -1000, -400, Charger::Idle)), Some(100), "10 % gone at once");
    assert_eq!(gauge.update(&sample(12 * SECOND_MS, 3900, -1000, -400, Charger::Idle)), Some(99));
    assert_eq!(
        gauge.update(&Sample { suspend_ms: 60_000, ..sample(13 * SECOND_MS, 3900, -1000, -400, Charger::Idle) }),
        Some(93)
    );
    assert_eq!(gauge.update(&sample(43 * SECOND_MS, 3900, -1000, -400, Charger::Idle)), Some(90));
    assert_eq!(gauge.update(&sample(53 * SECOND_MS, 3900, -1000, 0, Charger::Idle)), Some(91), "and back up");
}

#[test]
fn voltage_empty() {
    let mut gauge = full_at(0, ASSUMED_CAPACITY, 0.0);
    let mut levels = Vec::new();
    for second in 1..=CUTOFF_WINDOW as u64 {
        levels.push(gauge.update(&sample(second * SECOND_MS, 3403, -1340, -4241, Charger::Idle)));
        assert_eq!(gauge.verdict(None), Verdict::Run, "{second} s: the mean stays over the cutoff");
    }
    assert_eq!(levels.last(), Some(&Some(0)), "nothing on show by the voltage");
    assert_eq!((gauge.figures().capacity, gauge.runs), (4241, 1.0), "the run measured where it showed nothing");
    let mut restarted = Gauge::default();
    restarted.recall(Memory::parse(&gauge.memory(BOOT).text()).unwrap(), "another", 0);
    assert_eq!(
        restarted.update(&sample(SECOND_MS, 3700, -100, 0, Charger::Idle)),
        Some(1),
        "switched on again it shows 1 % while it carries the device, not half of the rated charge"
    );
}

#[test]
fn cutoff_measures() {
    let mut gauge = full_at(0, ASSUMED_CAPACITY, 0.0);
    gauge.update(&sample(SECOND_MS, 3390, -1000, -4000, Charger::Idle));
    gauge.cut(Verdict::LowVoltage(3390));
    gauge.cut(Verdict::LowVoltage(3390));
    assert_eq!((gauge.figures().capacity, gauge.runs), (4000, 1.0), "taken whole the first time, and once");
    let mut restarted = Gauge::default();
    restarted.recall(Memory::parse(&gauge.memory(BOOT).text()).unwrap(), "another", 0);
    restarted.update(&sample(SECOND_MS, 3390, -1000, -30, Charger::Idle));
    restarted.cut(Verdict::LowVoltage(3390));
    assert_eq!(restarted.runs, 1.0, "switched on again after its cutoff it measures nothing until the next full");
    let mut again = full_at(0, 4000.0, 1.0);
    again.update(&sample(SECOND_MS, 3390, -1000, -3000, Charger::Idle));
    again.cut(Verdict::Run);
    assert_eq!(again.figures().capacity, 4000, "running on measures nothing");
    again.cut(Verdict::LowVoltage(3390));
    assert_eq!(again.figures().capacity, 3500, "half way the second time");
    let mut short = full_at(0, 4000.0, 1.0);
    short.update(&sample(SECOND_MS, 3390, -1000, -500, Charger::Idle));
    short.cut(Verdict::LowVoltage(3390));
    assert_eq!(short.figures().capacity, 4000, "50 mAh from full to cutoff is no cell's");
    let mut unknown = Gauge::default();
    unknown.update(&sample(SECOND_MS, 3390, -1000, -4000, Charger::Idle));
    unknown.cut(Verdict::LowVoltage(3390));
    assert!(!unknown.figures().is_measured, "without a full charge before it nothing is measured");
}

#[test]
fn goal_times() {
    let figures = Figures { out: Some(1320), capacity: 3600, is_measured: true, current: 3600 };
    assert_eq!(figures.time_to_goal(80), Some(600_000), "60 mAh to 80 % at 360 mA");
    assert_eq!(figures.time_to_goal(60), Some(0), "all counted in: the end is still to be read");
    assert_eq!(Figures { current: -100, ..figures }.time_to_goal(80), None, "no charge");
    assert_eq!(Figures { out: None, ..figures }.time_to_goal(80), None, "no count");
}

#[test]
fn cutoff_verdicts() {
    let mut gauge = Gauge::default();
    for second in 1..CUTOFF_WINDOW as u64 {
        gauge.update(&sample(second * SECOND_MS, 3390, -1000, 0, Charger::Idle));
        assert_eq!(gauge.verdict(None), Verdict::Run, "fewer samples than the window");
    }
    gauge.update(&sample(CUTOFF_WINDOW as u64 * SECOND_MS, 3390, -1000, 0, Charger::Idle));
    assert_eq!(gauge.verdict(None), Verdict::LowVoltage(3390));
    let mut charging = Gauge::default();
    for second in 1..=CUTOFF_WINDOW as u64 {
        charging.update(&sample(second * SECOND_MS, 3390, 500, 0, Charger::Charging));
        assert_eq!(charging.verdict(Some(KERNEL_LEVEL_EMPTY)), Verdict::Run, "charging on average");
    }
    let mut early = Gauge::default();
    early.update(&sample(SECOND_MS, 3700, -1000, 0, Charger::Idle));
    assert_eq!(
        early.verdict(Some(KERNEL_LEVEL_EMPTY)),
        Verdict::KernelLatch,
        "the latch while discharging: the cutoff, wherever the voltage stands and however few samples"
    );
    assert_eq!(Verdict::Run.reason(), None, "running on powers nothing off");
    let mut latched = full_at(0, 4000.0, 1.0);
    for second in 1..=CUTOFF_WINDOW as u64 {
        latched.update(&sample(second * SECOND_MS, 3480, -1000, -3900, Charger::Idle));
    }
    let verdict = latched.verdict(Some(KERNEL_LEVEL_EMPTY));
    assert_eq!(verdict, Verdict::KernelLatch, "the latch with the cutoff near: the cutoff");
    latched.cut(verdict);
    assert_eq!((latched.figures().capacity, latched.runs), (3950, 2.0), "and the run is measured");
    let mut heavy = Gauge::default();
    for second in 1..=CUTOFF_WINDOW as u64 {
        heavy.update(&sample(second * SECOND_MS, 3390, -2500, 0, Charger::Idle));
    }
    assert_eq!(
        heavy.verdict(Some(KERNEL_LEVEL_EMPTY)),
        Verdict::LowVoltage(3390),
        "with the latch set under a heavy load, a mean under the cutoff still cuts off"
    );
}

#[test]
fn cutoff_line() {
    assert_eq!(
        to_cutoff_line(),
        "battery cutoff: while discharging, a mean of 10 samples under 3400 mV or the kernel's low battery level at 2",
        "both rules of the verdict"
    );
}

#[test]
fn camera_reserve() {
    assert!(is_camera_reserve(9, Supply::Battery) && !is_camera_reserve(CAMERA_RESERVE, Supply::Battery));
    for supply in [Supply::Cable, Supply::Charging, Supply::Charged] {
        assert!(!is_camera_reserve(0, supply), "nothing is held back on a cable: {supply:?}");
    }
}

#[test]
fn low_level() {
    assert!(is_low(0, Supply::Battery) && is_low(14, Supply::Battery), "under a seventh of full");
    assert!(!is_low(15, Supply::Battery) && !is_low(100, Supply::Battery));
    for supply in [Supply::Cable, Supply::Charging, Supply::Charged] {
        assert!(!is_low(0, supply), "never low on a cable: {supply:?}");
    }
}

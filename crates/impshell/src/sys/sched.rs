use super::check;
use r1_funcore::hw::CORES;
use std::io;

const UTIL_CLAMP_MIN: u64 = 0x20;
const RESET_ON_FORK: u64 = 0x01;
const FIFO: u32 = 1;
const SOUND_PRIORITY: u32 = 3;
const DRAW_PRIORITY: u32 = 2;
const WORK_PRIORITY: u32 = 1;
const _: () = assert!(SOUND_PRIORITY > DRAW_PRIORITY && DRAW_PRIORITY > WORK_PRIORITY);

/// Keeps the calling thread on `cores`; left alone, the scheduler stacks threads on one core while others idle.
pub fn pin_to_cores(cores: &[usize]) -> io::Result<()> {
    let mut cpu_set = unsafe { std::mem::zeroed::<libc::cpu_set_t>() };
    cores.iter().for_each(|core| unsafe { libc::CPU_SET(*core, &mut cpu_set) });
    check(unsafe { libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &cpu_set) })?;
    Ok(())
}

/// Resets the calling thread to any core and the governor's clock; a thread inherits both from its starter.
pub fn reset_scheduling() -> io::Result<()> {
    pin_to_cores(&CORES)?;
    write_attr(SchedAttr { flags: UTIL_CLAMP_MIN, ..SchedAttr::default() })
}

const LOWEST_NICE: libc::c_int = 19;

/// Puts the calling thread and those it starts at the lowest priority.
pub fn set_background_priority() {
    unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, LOWEST_NICE) };
}

/// Starts a thread at the lowest priority, with those it starts, so no frame waits for it.
pub fn spawn_background(name: &str, work: impl FnOnce() + Send + 'static) -> io::Result<()> {
    let run = move || {
        set_background_priority();
        work();
    };
    std::thread::Builder::new().name(name.into()).spawn(run).map(drop)
}

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct SchedAttr {
    size: u32,
    policy: u32,
    flags: u64,
    nice: i32,
    priority: u32,
    runtime: u64,
    deadline: u64,
    period: u64,
    util_min: u32,
    util_max: u32,
}

fn write_attr(attr: SchedAttr) -> io::Result<()> {
    let attr = SchedAttr { size: size_of::<SchedAttr>() as u32, ..attr };
    check(unsafe { libc::syscall(libc::SYS_sched_setattr, 0, &raw const attr, 0) } as libc::c_int)?;
    Ok(())
}

fn read_attr() -> io::Result<SchedAttr> {
    let mut attr = SchedAttr::default();
    let size = size_of::<SchedAttr>();
    check(unsafe { libc::syscall(libc::SYS_sched_getattr, 0, &raw mut attr, size, 0) } as libc::c_int)?;
    Ok(attr)
}

/// Makes the calling thread real-time above worker threads, its core clocked by its load; threads it starts are ordinary.
pub fn set_drawing_priority() -> io::Result<()> {
    write_attr(SchedAttr {
        policy: FIFO,
        flags: RESET_ON_FORK | UTIL_CLAMP_MIN,
        priority: DRAW_PRIORITY,
        ..SchedAttr::default()
    })
}

/// Makes the calling thread real-time above the drawing one, for a call's sound, whose blocks take little time and are heard where they come late; threads it starts are ordinary.
pub fn set_sound_priority() -> io::Result<()> {
    write_attr(SchedAttr {
        policy: FIFO,
        flags: RESET_ON_FORK | UTIL_CLAMP_MIN,
        priority: SOUND_PRIORITY,
        ..SchedAttr::default()
    })
}

/// Puts the calling thread and those it starts above ordinary threads and under the drawing one, at `utilisation` or more.
pub fn set_worker_priority(utilisation: u32) -> io::Result<()> {
    write_attr(SchedAttr {
        policy: FIFO,
        flags: UTIL_CLAMP_MIN,
        priority: WORK_PRIORITY,
        util_min: utilisation,
        ..SchedAttr::default()
    })
}

fn with_clock_floor(attr: SchedAttr, utilisation: u32) -> SchedAttr {
    SchedAttr { flags: attr.flags & RESET_ON_FORK | UTIL_CLAMP_MIN, util_min: utilisation, util_max: 0, ..attr }
}

/// Asks at least `utilisation` of the clock of the calling thread's core while the thread runs there, keeping its policy and priority.
pub fn set_clock_floor(utilisation: u32) -> io::Result<()> {
    write_attr(with_clock_floor(read_attr()?, utilisation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_floor_keeps_policy() {
        let drawing = SchedAttr {
            size: 56,
            policy: FIFO,
            flags: RESET_ON_FORK,
            priority: DRAW_PRIORITY,
            util_max: 1024,
            ..SchedAttr::default()
        };
        assert_eq!(
            with_clock_floor(drawing, 1024),
            SchedAttr { flags: RESET_ON_FORK | UTIL_CLAMP_MIN, util_min: 1024, util_max: 0, ..drawing },
            "a real-time thread's request without its priority is refused as an invalid argument"
        );
        let ordinary = SchedAttr { size: 56, nice: 19, util_max: 1024, ..SchedAttr::default() };
        assert_eq!(
            with_clock_floor(ordinary, 262),
            SchedAttr { flags: UTIL_CLAMP_MIN, util_min: 262, util_max: 0, ..ordinary }
        );
        let attr = read_attr().unwrap();
        assert_eq!((attr.policy, attr.priority), (0, 0), "a thread of the tests is an ordinary one, and says so");
    }

    #[test]
    fn background_thread_priority() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let nice = || unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        let start_nice = nice();
        spawn_background("test", move || sender.send(nice()).unwrap()).unwrap();
        assert_eq!((receiver.recv().unwrap(), nice()), (LOWEST_NICE, start_nice));
    }
}

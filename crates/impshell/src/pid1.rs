use crate::power::thermal;
use crate::sys::{self, devnodes, klog};
use crate::{app, log, usb};
use r1_funcore::domain::pid1::{
    restart_pause, to_charging_restart_line, to_ended_line, to_not_init_line, to_start_line,
};
use r1_funcore::hw::power::Power;
use r1_funcore::logs::Logging;
use r1_funcore::net::tcp;
use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::Instant;

const BOOT_TAG: &str = "/proc/device-tree/chosen/atag,boot";

const MOUNTS: [(&str, &str, &str, &str); 5] = [
    ("proc", "/proc", "proc", ""),
    ("sysfs", "/sys", "sysfs", ""),
    ("tmpfs", "/dev", "tmpfs", "mode=0755"),
    ("configfs", "/config", "configfs", ""),
    ("pstore", sys::CRASH_STORE, "pstore", ""),
];

#[cfg(feature = "debug-shell")]
const TRACE_MOUNTS: [(&str, &str, &str, &str); 1] = [("tracefs", "/sys/kernel/tracing", "tracefs", "")];
#[cfg(not(feature = "debug-shell"))]
const TRACE_MOUNTS: [(&str, &str, &str, &str); 0] = [];

fn mount_all() {
    for (source, target, fstype, options) in MOUNTS.into_iter().chain(TRACE_MOUNTS) {
        let target = Path::new(target);
        let result = fs::create_dir_all(target).and_then(|()| sys::mount(source, target, fstype, options));
        if let Err(e) = result {
            log!("mount {}: {e}", target.display());
        }
    }
}

fn leave_charging_boot() {
    let boot_tag = fs::read(BOOT_TAG).inspect_err(|e| log!("{BOOT_TAG}: {e}")).unwrap_or_default();
    let cmdline = fs::read_to_string(sys::CMDLINE).inspect_err(|e| log!("{}: {e}", sys::CMDLINE)).unwrap_or_default();
    if let Some(line) = to_charging_restart_line(&boot_tag, &cmdline) {
        log!("{line}");
        log!("reboot refused: {}", sys::reboot(Power::Reboot));
    }
}

fn wait_for(child: libc::pid_t) -> io::Result<libc::c_int> {
    loop {
        let mut status = 0;
        match sys::check(unsafe { libc::waitpid(-1, &mut status, 0) }) {
            Ok(pid) if pid == child => return Ok(status),
            Err(e) if e.kind() != io::ErrorKind::Interrupted => return Err(e),
            _ => {}
        }
    }
}

pub fn run() -> ! {
    if let Some(line) = to_not_init_line(std::process::id()) {
        eprintln!("{line}");
        std::process::exit(1);
    }
    klog::init();
    klog::log_panics();
    // A flash that chose silence has it from the first line on.
    let is_silent = sys::read_flash().is_some_and(|flash| flash.is_silent());
    if is_silent {
        klog::mute();
    }
    log!("{}", to_start_line());
    mount_all();
    // Once /sys is mounted the kernel stops logging too, and forgets what it logged before.
    if is_silent {
        let _ = klog::set(Logging::Off);
    }
    if let Err(e) = klog::remove_rate_limit() {
        log!("kernel log: records stay limited: {e}");
    }
    sys::write_tcp("net", &tcp::BOOT_WRITES);
    // The kernel of the bootloader's charging boot powers off when the cable is pulled; a restart boots normally.
    leave_charging_boot();
    log!("{}", devnodes::coldplug().to_line());
    thermal::apply();
    // The frames function's control file is held here, so that the gadget stays on the bus whenever the app starts again.
    if let Err(e) = usb::frames::prepare() {
        log!("usb frames: {e}");
    }

    loop {
        let start = Instant::now();
        // This process logs as the switch the app last set has the kernel log, and the app starts so.
        klog::follow_kernel();
        match unsafe { libc::fork() } {
            0 => app::run(),
            -1 => {
                log!("fork failed ({}), running the app as process 1", io::Error::last_os_error());
                app::run();
            }
            child => {
                klog::drain();
                let result = wait_for(child);
                klog::follow_kernel();
                match result {
                    Ok(status) => log!("{}", to_ended_line(status)),
                    Err(e) => log!("lost track of the app ({e}), restarting it"),
                }
                thread::sleep(restart_pause(start.elapsed()));
            }
        }
    }
}

use crate::sys;
use r1_funcore::logs::snapshot::{STAT, add_part, add_tasks_title, begin, task_numbers};
use std::fs;
use std::path::Path;

const PROCESSES: &str = "/proc";
const THREADS: &str = "/proc/self/task";
const FILES: [&str; 16] = [
    "/proc/uptime",
    "/proc/loadavg",
    sys::MEMINFO,
    "/proc/stat",
    "/proc/interrupts",
    "/proc/vmstat",
    "/proc/diskstats",
    "/proc/net/dev",
    "/sys/power/suspend_stats/success",
    "/sys/power/suspend_stats/fail",
    sys::CLOCKS[0][0],
    sys::CLOCKS[0][1],
    sys::CLOCKS[1][0],
    sys::CLOCKS[1][1],
    // The chip's own thermal zone, named by its type.
    "/sys/class/thermal/thermal_zone3/type",
    "/sys/class/thermal/thermal_zone3/temp",
];

fn add_file(text: &mut Vec<u8>, name: &str, path: &Path) {
    if let Ok(content) = fs::read(path) {
        add_part(text, name, &content);
    }
}

fn add_tasks(text: &mut Vec<u8>, tasks: &str) {
    let Ok(entries) = fs::read_dir(tasks) else {
        return;
    };
    add_tasks_title(text, tasks);
    let names: Vec<String> = entries.flatten().filter_map(|entry| entry.file_name().into_string().ok()).collect();
    for number in task_numbers(names.iter().map(String::as_str)) {
        if let Ok(line) = fs::read(Path::new(tasks).join(number.to_string()).join(STAT)) {
            text.extend_from_slice(&line);
        }
    }
}

pub(super) fn take_snapshot(note: &str) -> Vec<u8> {
    let mut text = begin(note);
    for path in FILES {
        add_file(&mut text, path, Path::new(path));
    }
    add_tasks(&mut text, PROCESSES);
    add_tasks(&mut text, THREADS);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::scratch;

    #[test]
    fn snapshot_files() {
        let root = scratch("snap");
        fs::write(root.join("with"), b"a line\n").unwrap();
        fs::write(root.join("without"), b"no line end").unwrap();
        let mut text = Vec::new();
        add_file(&mut text, "with", &root.join("with"));
        add_file(&mut text, "nothing", &root.join("nothing"));
        add_file(&mut text, "without", &root.join("without"));
        assert_eq!(String::from_utf8(text).unwrap(), "== with\na line\n== without\nno line end\n");
        for (number, line) in [("12", "12 (init) S\n"), ("7", "7 (kthreadd) S\n"), ("self", "not a number\n")] {
            fs::create_dir_all(root.join(number)).unwrap();
            fs::write(root.join(number).join(STAT), line).unwrap();
        }
        let (mut text, tasks) = (Vec::new(), root.to_str().unwrap());
        add_tasks(&mut text, tasks);
        assert_eq!(
            String::from_utf8(text).unwrap(),
            format!("== {tasks}/*/stat\n7 (kthreadd) S\n12 (init) S\n"),
            "by their numbers, and only what has one"
        );
        let snapshot = String::from_utf8_lossy(&take_snapshot("19:31:38.600 -- sleep")).into_owned();
        assert!(
            snapshot.starts_with("19:31:38.600 -- sleep\n== /proc/uptime\n")
                && snapshot.contains("\n== /proc/meminfo\n")
                && snapshot.contains("\n== /proc/self/task/*/stat\n")
        );
        fs::remove_dir_all(root).unwrap();
    }
}

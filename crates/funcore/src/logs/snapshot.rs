//! The text of a system snapshot: its titled parts and the numbers of its tasks.

use alloc::format;
use alloc::vec::Vec;

const PART: &str = "==";
/// The kernel's file of a task's state.
pub const STAT: &str = "stat";
pub const SLEEP: &str = "sleep";
pub const WAKE: &str = "wake";
/// Why a snapshot is taken as a call's sound starts: with the one at its end it tells what the call's threads took of the cores.
pub const CALL_START: &str = "call talked in";
/// Why a snapshot is taken as a call's sound stops.
pub const CALL_END: &str = "call over";

/// A snapshot's first line, why it was taken.
pub fn begin(reason: &str) -> Vec<u8> {
    format!("{reason}\n").into_bytes()
}

fn add_title(snapshot: &mut Vec<u8>, name: &str) {
    snapshot.extend_from_slice(format!("{PART} {name}\n").as_bytes());
}

/// Adds the line that names the states of the tasks of a kernel's folder.
pub fn add_tasks_title(snapshot: &mut Vec<u8>, tasks: &str) {
    add_title(snapshot, &format!("{tasks}/*/{STAT}"));
}

/// Adds a system file after the line that names it, its last line ended.
pub fn add_part(snapshot: &mut Vec<u8>, name: &str, content: &[u8]) {
    add_title(snapshot, name);
    snapshot.extend_from_slice(content);
    if !content.ends_with(b"\n") {
        snapshot.push(b'\n');
    }
}

/// The numbered entries of a kernel's folder of tasks, sorted.
pub fn task_numbers<'a>(names: impl Iterator<Item = &'a str>) -> Vec<u32> {
    let mut numbers: Vec<u32> = names.filter_map(|name| name.parse().ok()).collect();
    numbers.sort_unstable();
    numbers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_and_tasks() {
        let mut snapshot = begin("10:00:00.000 -- the logs are on");
        add_part(&mut snapshot, "with", b"a line\n");
        add_part(&mut snapshot, "without", b"no line end");
        add_tasks_title(&mut snapshot, "/proc");
        assert_eq!(
            snapshot, b"10:00:00.000 -- the logs are on\n== with\na line\n== without\nno line end\n== /proc/*/stat\n",
            "why it was taken, each part under its name, and the tasks' states under the files they are read from"
        );
        assert_eq!(
            task_numbers(["12", "self", "7", "thread-self", "356"].into_iter()),
            [7, 12, 356],
            "numbers alone, in order"
        );
    }
}

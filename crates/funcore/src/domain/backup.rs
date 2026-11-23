//! A backup and a restore: what of the partition they take, what stays the device's own, and the steps by which a restore's files take the partition's place.

use super::flash;
use crate::cell;
use crate::hw::power::battery;
use crate::logs;
use crate::storage::paths::{self, new_name};
use crate::storage::{INTERFACE, PRIVATE, RESTORE};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// The directories at the partition's root a backup takes whole, each under the safe's key, which a USB host is shown while the drive is switched on.
pub const PUBLIC_ROOTS: [&str; 4] = ["camera", "audio", "news", "chat"];
// The private directory a restore took the place of, in `RESTORE` until it is removed.
const REPLACED: &str = "replaced";
// What the log names where a confirmed restore brought no root.
const NOTHING: &str = "nothing";
const UNCONFIRMED_LINE: &str = "data: a restore's files that nothing confirmed are gone";
const OTHER_SAFE_LINE: &str = "data: a restore's private directory is not of this safe, and left out";

/// What stays the device's own through a backup and a restore, each by its path from the partition's root: the battery's gauge and usage, the modem's and the SIM's files, the logs, and the stamp of the flash the device took.
pub fn device_entries() -> [String; 4] {
    [
        battery::DIRECTORY.into(),
        cell::DIRECTORY.into(),
        logs::DIRECTORY.into(),
        format!("{INTERFACE}/{}", flash::STAMP_FILE),
    ]
}

/// True for a file or directory, by its path from the partition's root, that a backup takes and a restore puts back: one under a public root or the private directory, written whole and none of the device's own.
pub fn is_backed_up(path: &str) -> bool {
    let root = path.split('/').next().unwrap_or_default();
    let is_own = |entry: &String| paths::check_scope(Some(&[entry.as_str()]), path).is_ok();
    paths::to_directory_path(path).is_ok()
        && (PUBLIC_ROOTS.contains(&root) || root == PRIVATE)
        && !paths::is_half_written(paths::split_file(path).1)
        && !device_entries().iter().any(is_own)
}

/// The directory a restore's files are staged in until a commit confirms them, from the partition's root.
pub fn staging() -> String {
    new_name(RESTORE)
}

/// The staged private directory and public roots, each made and encrypted under the safe's key before a file is staged in it.
pub fn staged_enclosed() -> Vec<String> {
    core::iter::once(PRIVATE).chain(PUBLIC_ROOTS).map(|name| format!("{}/{name}", staging())).collect()
}

/// True for an entry of the confirmed files that a start counts as staged: all but a public root that holds nothing, as the staging made it, which brings nothing and takes no root's place.
pub fn is_staged(name: &str, is_empty: bool) -> bool {
    !(is_empty && PUBLIC_ROOTS.contains(&name))
}

fn in_restore(path: &str) -> String {
    format!("{RESTORE}/{path}")
}

/// A step of a restore on the partition, its paths from the partition's root.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Step {
    // A file, or a directory with all it holds, goes; one that is not there is no error.
    Remove(String),
    // A file or directory takes a new place, the directories above it made first.
    Move { from: String, to: String },
    // What the steps before changed at the partition's root is written to the medium.
    Sync,
    // The free blocks are handed back to the medium.
    Trim,
}

/// The steps that make way for a new staging: what was staged or confirmed before goes.
pub fn stage_steps() -> [Step; 2] {
    [Step::Remove(RESTORE.into()), Step::Remove(staging())]
}

/// The steps that confirm the staged files: what is the device's own and what only a start puts there are taken out, and the files take their confirmed name.
pub fn commit_steps() -> Vec<Step> {
    let staging = staging();
    let mut steps: Vec<Step> =
        device_entries().iter().map(|entry| Step::Remove(format!("{staging}/{entry}"))).collect();
    steps.push(Step::Remove(format!("{staging}/{REPLACED}")));
    steps.extend([Step::Move { from: staging, to: RESTORE.into() }, Step::Sync]);
    steps
}

/// What a start finds of a restore on the partition.
#[derive(Default, Debug)]
pub struct Found {
    /// Whether files are staged that no commit confirmed.
    pub is_staging: bool,
    /// The entries of `RESTORE` that count as staged (`is_staged`); none where no restore is confirmed.
    pub staged: Option<Vec<String>>,
    /// Whether the private directory is there.
    pub has_private: bool,
    /// The device's own entries the private directory holds, known once the safe is open.
    pub own: Vec<String>,
    /// Whether the staged private directory is encrypted under the safe's key; none where it does not tell.
    pub is_same_safe: Option<bool>,
}

impl Found {
    fn has_staged(&self, name: &str) -> bool {
        self.staged.as_ref().is_some_and(|staged| staged.iter().any(|entry| entry == name))
    }

    fn takes_private(&self) -> bool {
        self.has_staged(PRIVATE) && self.is_same_safe == Some(true)
    }

    /// The steps before the safe is looked at: unconfirmed files go, and a staged private directory whose predecessor is put away takes its place, as a start cut short between the two left them.
    pub fn settle_steps(&self) -> Vec<Step> {
        let mut steps = Vec::new();
        if self.is_staging {
            steps.extend([Step::Remove(staging()), Step::Sync, Step::Trim]);
        }
        if self.has_staged(REPLACED) && self.has_staged(PRIVATE) && !self.has_private {
            steps.extend([Step::Move { from: in_restore(PRIVATE), to: PRIVATE.into() }, Step::Sync]);
        }
        steps
    }

    /// The log line of staged files that go because no commit confirmed them.
    pub fn to_unconfirmed_line(&self) -> Option<&'static str> {
        self.is_staging.then_some(UNCONFIRMED_LINE)
    }

    /// The log line of a staged private directory left out, once the safe is open, as under another key than the safe's.
    pub fn to_other_safe_line(&self) -> Option<&'static str> {
        (self.has_staged(PRIVATE) && !self.takes_private()).then_some(OTHER_SAFE_LINE)
    }

    /// The steps once the safe is open: the device's own entries go over into the staged private directory, which takes the private directory's place; each staged public root takes its root's place; what is left goes. Cut short anywhere, a start that finds what is left takes them up from there.
    pub fn apply_steps(&self) -> Vec<Step> {
        if self.staged.is_none() {
            return Vec::new();
        }
        let mut steps = Vec::new();
        if self.takes_private() {
            steps.push(Step::Remove(in_restore(REPLACED)));
            steps.extend(self.own.iter().map(|entry| Step::Move { from: entry.clone(), to: in_restore(entry) }));
            steps.push(Step::Move { from: PRIVATE.into(), to: in_restore(REPLACED) });
            steps.push(Step::Move { from: in_restore(PRIVATE), to: PRIVATE.into() });
        } else if self.has_staged(PRIVATE) {
            steps.push(Step::Remove(in_restore(PRIVATE)));
        }
        for root in PUBLIC_ROOTS.into_iter().filter(|root| self.has_staged(root)) {
            steps.extend([Step::Remove(root.into()), Step::Move { from: in_restore(root), to: root.into() }]);
        }
        steps.extend([Step::Remove(RESTORE.into()), Step::Sync, Step::Trim]);
        steps
    }

    /// The log line of a restore put in place, by what it brought; none where no restore was confirmed.
    pub fn to_applied_line(&self) -> Option<String> {
        self.staged.as_ref()?;
        let private = self.takes_private().then_some(PRIVATE);
        let brought: Vec<&str> = PUBLIC_ROOTS.into_iter().filter(|root| self.has_staged(root)).chain(private).collect();
        let brought = if brought.is_empty() { NOTHING.into() } else { brought.join(", ") };
        Some(format!("data: a restore put in place: {brought}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::DIRECTORIES;
    use alloc::vec;

    fn found(staged: &[&str]) -> Found {
        Found {
            staged: Some(staged.iter().map(|name| String::from(*name)).collect()),
            has_private: true,
            is_same_safe: Some(true),
            ..Found::default()
        }
    }

    fn moved(from: &str, to: &str) -> Step {
        Step::Move { from: from.into(), to: to.into() }
    }

    fn removed(path: &str) -> Step {
        Step::Remove(path.into())
    }

    #[test]
    fn device_entries_stay() {
        assert_eq!(
            device_entries(),
            [".r1metal/battery", ".r1metal/cell", ".r1metal/logs", ".r1metal/interface/flash"],
            "as docs/firmware/storage.md lists them"
        );
        let mut roots: Vec<&str> = Vec::new();
        for root in DIRECTORIES.iter().filter_map(|directory| directory.split('/').next()) {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        assert_eq!(roots, PUBLIC_ROOTS, "the roots of the directories every mount makes");
    }

    #[test]
    fn backed_up_paths() {
        for taken in [
            "camera",
            "camera/pictures/20261009_193512_original.jpg",
            "audio/notes/20261012_190328/20261012_190328.m4a",
            "news/20261016/20261016_news.pdf",
            "chat/core.md",
            ".r1metal",
            ".r1metal/signal/sessions/anna.1",
            ".r1metal/interface/settings",
            ".r1metal/interface",
            ".r1metal/wifi/secret",
            ".r1metal/openai/key",
            ".r1metal/battery.old",
        ] {
            assert!(is_backed_up(taken), "{taken}");
        }
        for left in [
            ".r1metal/battery",
            ".r1metal/battery/gauge",
            ".r1metal/cell/nvram/Z/NVRAM",
            ".r1metal/cell/sim",
            ".r1metal/logs/20261011/log",
            ".r1metal/interface/flash",
            ".r1metal/interface/settings.new",
            "chat/core.md.new",
            ".restore/camera/a.jpg",
            ".restore.new",
            ".r1metal.new/openai/key",
            ".r1metal.key",
            ".r1metal.key2",
            "lost+found",
            "",
            "/camera",
            "camera/../.r1metal/battery/gauge",
            "camera/",
        ] {
            assert!(!is_backed_up(left), "{left:?}");
        }
    }

    #[test]
    fn staging_and_commit() {
        assert_eq!(staging(), ".restore.new");
        assert_eq!(
            staged_enclosed(),
            [
                ".restore.new/.r1metal",
                ".restore.new/camera",
                ".restore.new/audio",
                ".restore.new/news",
                ".restore.new/chat"
            ],
            "what a restore stages lies under the safe's key as what it takes the place of does"
        );
        assert!(
            is_staged("camera", false)
                && !is_staged("camera", true)
                && is_staged("replaced", true)
                && is_staged(".r1metal", true),
            "a root the backup held nothing of takes no root's place"
        );
        assert_eq!(stage_steps(), [removed(".restore"), removed(".restore.new")]);
        assert_eq!(
            commit_steps(),
            [
                removed(".restore.new/.r1metal/battery"),
                removed(".restore.new/.r1metal/cell"),
                removed(".restore.new/.r1metal/logs"),
                removed(".restore.new/.r1metal/interface/flash"),
                removed(".restore.new/replaced"),
                moved(".restore.new", ".restore"),
                Step::Sync,
            ],
            "what a host staged of the device's own goes before the files are confirmed"
        );
    }

    #[test]
    fn settle_before_safe() {
        assert_eq!(Found::default().settle_steps(), [], "nothing staged, nothing confirmed");
        assert_eq!(Found::default().to_unconfirmed_line(), None);
        let unconfirmed = Found { is_staging: true, has_private: true, ..Found::default() };
        assert_eq!(unconfirmed.settle_steps(), [removed(".restore.new"), Step::Sync, Step::Trim]);
        assert_eq!(unconfirmed.to_unconfirmed_line(), Some("data: a restore's files that nothing confirmed are gone"));
        let cut = Found { has_private: false, ..found(&["replaced", ".r1metal", "camera"]) };
        assert_eq!(
            cut.settle_steps(),
            [moved(".restore/.r1metal", ".r1metal"), Step::Sync],
            "a start cut short between the private directory put away and its successor put in place"
        );
        assert_eq!(found(&[".r1metal", "camera"]).settle_steps(), [], "a confirmed restore waits for the safe");
        assert_eq!(
            Found { has_private: false, ..found(&["replaced"]) }.settle_steps(),
            [],
            "nothing staged takes the private directory's place"
        );
    }

    #[test]
    fn apply_after_safe() {
        assert_eq!(Found::default().apply_steps(), [], "no restore confirmed");
        assert_eq!(Found::default().to_applied_line(), None);
        let whole = Found {
            own: vec![".r1metal/battery".into(), ".r1metal/interface/flash".into()],
            ..found(&[".r1metal", "camera", "chat", "stray"])
        };
        assert_eq!(
            whole.apply_steps(),
            [
                removed(".restore/replaced"),
                moved(".r1metal/battery", ".restore/.r1metal/battery"),
                moved(".r1metal/interface/flash", ".restore/.r1metal/interface/flash"),
                moved(".r1metal", ".restore/replaced"),
                moved(".restore/.r1metal", ".r1metal"),
                removed("camera"),
                moved(".restore/camera", "camera"),
                removed("chat"),
                moved(".restore/chat", "chat"),
                removed(".restore"),
                Step::Sync,
                Step::Trim,
            ],
            "the device's own goes over first, then each root takes its place, and a name no backup holds goes with the rest"
        );
        assert_eq!(whole.to_applied_line().as_deref(), Some("data: a restore put in place: camera, chat, .r1metal"));
        assert_eq!(whole.to_other_safe_line(), None);
        let resumed = found(&["replaced", "chat"]);
        assert_eq!(
            resumed.apply_steps(),
            [removed("chat"), moved(".restore/chat", "chat"), removed(".restore"), Step::Sync, Step::Trim],
            "taken up where a start was cut short: the private directory is in place, one root still waits"
        );
        for is_same_safe in [Some(false), None] {
            let foreign = Found { is_same_safe, ..found(&[".r1metal", "news"]) };
            assert_eq!(
                foreign.apply_steps(),
                [
                    removed(".restore/.r1metal"),
                    removed("news"),
                    moved(".restore/news", "news"),
                    removed(".restore"),
                    Step::Sync,
                    Step::Trim
                ],
                "a private directory under another key, or under none, never takes the safe's place"
            );
            assert_eq!(foreign.to_applied_line().as_deref(), Some("data: a restore put in place: news"));
            assert_eq!(
                foreign.to_other_safe_line(),
                Some("data: a restore's private directory is not of this safe, and left out")
            );
        }
        assert_eq!(found(&[]).apply_steps(), [removed(".restore"), Step::Sync, Step::Trim]);
        assert_eq!(found(&[]).to_applied_line().as_deref(), Some("data: a restore put in place: nothing"));
    }
}

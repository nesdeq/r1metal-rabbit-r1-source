//! Paths inside the data partition.

use super::safe::{KEY_FILES, Kind};
use super::{PRIVATE, RESTORE};
use crate::base::file_names::{has_extension, is_plain};
use alloc::format;
use alloc::string::String;

const MOUNT_POINT_FIELD: usize = 1;
const NEW_SUFFIX: &str = ".new";
/// The error text of a path without a directory.
pub const NO_DIRECTORY: &str = "a file to replace needs a directory";
/// The error text of a path without a name.
pub const NO_NAME: &str = "a file needs a name";

/// True if the kernel's table of mounts has something mounted at `point`.
pub fn is_mounted_at(table: &str, point: &str) -> bool {
    let mount_point = |line: &'_ str| line.split_whitespace().nth(MOUNT_POINT_FIELD).map(String::from);
    table.lines().filter_map(mount_point).any(|entry| entry == point)
}

fn is_within(directory: &str) -> bool {
    directory.split('/').all(is_plain)
}

/// A directory's path from the partition's root; the error text if it leads out of the partition.
pub fn to_directory_path(directory: &str) -> Result<&str, String> {
    is_within(directory)
        .then_some(directory)
        .ok_or_else(|| format!("{directory:?} names no directory of the partition"))
}

/// A file's path from the partition's root; the error text if it leads out of its directory.
pub fn to_file_path(directory: &str, name: &str) -> Result<String, String> {
    if !is_within(directory) || !is_plain(name) {
        return Err(format!("{name:?} names no file of {directory:?}"));
    }
    Ok(format!("{directory}/{name}"))
}

/// Checks a directory is `scope`'s or under it, compared name by name; `None` is the whole partition.
pub fn check_scope(scope: Option<&[&str]>, directory: &str) -> Result<(), String> {
    let is_under = |reach: &&str| {
        let mut names = directory.split('/');
        reach.split('/').all(|name| names.next() == Some(name))
    };
    match scope {
        Some(reaches) if !reaches.iter().any(is_under) => {
            Err(format!("{directory:?} is none of the directories this part reaches"))
        }
        _ => Ok(()),
    }
}

/// Checks a part is handed the whole partition, the only scope that learns the mount point.
pub fn check_full_scope(scope: Option<&[&str]>) -> Result<(), String> {
    if scope.is_some() {
        return Err(String::from("a part handed some directories is not told where the partition is mounted"));
    }
    Ok(())
}

/// A directory as its parent and its own name.
pub fn split_folder(directory: &str) -> Option<(&str, &str)> {
    directory.rsplit_once('/')
}

/// A file from the partition's root as its directory and its name; one at the root has no directory.
pub fn split_file(file: &str) -> (&str, &str) {
    split_folder(file).unwrap_or(("", file))
}

/// The name a file's new content takes until it replaces the file.
pub fn new_name(name: &str) -> String {
    format!("{name}{NEW_SUFFIX}")
}

// True for a name of `new_name`'s form.
pub(crate) fn is_half_written(name: &str) -> bool {
    name.ends_with(NEW_SUFFIX)
}

/// What a listing of a directory holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Listed<'a> {
    // The files written whole.
    Files,
    // Every file, a half-written one too.
    AllFiles,
    // The files that end in that extension.
    Extension(&'a str),
    Folders,
}

impl Listed<'_> {
    /// True for an entry the listing holds, by its kind and its name.
    pub fn holds(self, kind: Kind, name: &str) -> bool {
        match self {
            Listed::Files => kind == Kind::File && !is_half_written(name),
            Listed::AllFiles => kind == Kind::File,
            Listed::Extension(extension) => kind == Kind::File && has_extension(name, extension),
            Listed::Folders => kind == Kind::Directory,
        }
    }
}

/// True for an entry a USB host is shown: all but the private directory, the safe's key files, written or half written, and a restore's files, staged or confirmed, at the root.
pub fn is_shown_to_hosts(name: &str, is_at_root: bool) -> bool {
    let is_key_file = KEY_FILES.iter().any(|file| name == *file || name == new_name(file));
    !is_at_root || name != PRIVATE && !is_key_file && name != RESTORE && name != new_name(RESTORE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{CHAT, CHAT_PRIVATE, CHAT_TALK, NEWS, PICTURES};

    #[test]
    fn mount_table() {
        let table = "sysfs /sys sysfs rw 0 0\n/dev/block/sdc63 /data f2fs rw,lazytime 0 0\n";
        assert!(is_mounted_at(table, "/data") && !is_mounted_at(table, "/drive") && !is_mounted_at(table, "f2fs"));
        assert!(!is_mounted_at("", "/data"));
    }

    #[test]
    fn paths_stay_inside() {
        assert!(is_within(PICTURES) && is_within(".r1metal/signal/sessions"));
        for outside in ["", "/", "news/", "/news", "news//x", "news/../.r1metal", "./news", "news/."] {
            assert!(!is_within(outside), "{outside:?}");
        }
        assert_eq!(
            (to_directory_path(NEWS), to_file_path("news/20261016", "20261016_news.md")),
            (Ok(NEWS), Ok("news/20261016/20261016_news.md".into()))
        );
        assert!(to_directory_path("news/..").is_err() && to_file_path(NEWS, "../x_news.md").is_err());
        assert!(to_file_path("news/../x", "x_news.md").is_err());
    }

    #[test]
    fn split_folders() {
        assert_eq!(split_folder(".r1metal/signal/sessions"), Some((".r1metal/signal", "sessions")));
        assert_eq!(split_folder(NEWS), None);
        assert_eq!(
            [split_file("camera/pictures/a.jpg"), split_file("a.jpg")],
            [("camera/pictures", "a.jpg"), ("", "a.jpg")],
            "a file at the root has no directory"
        );
    }

    #[test]
    fn new_names_and_hosts() {
        assert_eq!(
            (new_name("gauge"), is_half_written("gauge.new"), is_half_written("gauge")),
            ("gauge.new".into(), true, false)
        );
        assert!(
            !is_shown_to_hosts(PRIVATE, true) && is_shown_to_hosts(PRIVATE, false) && is_shown_to_hosts(PICTURES, true)
        );
        assert!(
            !is_shown_to_hosts(".restore", true) && !is_shown_to_hosts(".restore.new", true),
            "a restore's files hold what the private directory holds"
        );
        assert!(is_shown_to_hosts(".restore", false) && is_shown_to_hosts(".restored", true), "by the whole name");
        assert!(
            [".r1metal.key", ".r1metal.key2", ".r1metal.key.new", ".r1metal.key2.new"]
                .iter()
                .all(|name| !is_shown_to_hosts(name, true)),
            "the safe's key files, as they are written too"
        );
    }

    #[test]
    fn listed_entries() {
        let entries = [
            (Kind::File, "20261009_193512.jpg"),
            (Kind::File, "20261008_080000.JPG"),
            (Kind::File, "20261009_193512.jpg.new"),
            (Kind::File, "notes"),
            (Kind::Directory, "20261009_100000.jpg"),
            (Kind::Other, "link.jpg"),
        ];
        let held = |listed: Listed| entries.map(|(kind, name)| listed.holds(kind, name));
        assert_eq!(held(Listed::Files), [true, true, false, true, false, false], "no file half written");
        assert_eq!(held(Listed::AllFiles), [true, true, true, true, false, false]);
        assert_eq!(
            held(Listed::Extension("jpg")),
            [true, true, false, false, false, false],
            "files alone, by the extension in any case"
        );
        assert_eq!(held(Listed::Folders), [false, false, false, false, true, false], "whatever a folder's name");
    }

    #[test]
    fn scopes() {
        let chat: &[&str] = &[CHAT, CHAT_PRIVATE];
        for inside in [CHAT, CHAT_TALK, CHAT_PRIVATE, "chat/talk/20261016"] {
            assert!(check_scope(Some(chat), inside).is_ok(), "{inside:?}");
        }
        for outside in ["chatter", PRIVATE, ".r1metal/signal", NEWS, "talk/chat"] {
            assert!(check_scope(Some(chat), outside).is_err(), "{outside:?}");
        }
        assert!(check_scope(Some(&[]), CHAT).is_err());
        assert!(check_scope(None, ".r1metal/signal").is_ok());
        assert!(
            check_full_scope(None).is_ok()
                && check_full_scope(Some(chat)).is_err()
                && check_full_scope(Some(&[])).is_err()
        );
    }
}

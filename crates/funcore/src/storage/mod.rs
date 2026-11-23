//! The data partition's layout.

pub mod paths;
pub mod progress;
pub mod safe;

use alloc::string::String;
use alloc::vec::Vec;
use paths::Listed;

/// The partition's files, one effect per method.
pub trait Files {
    type Error: core::error::Error;

    fn has(&mut self, directory: &str, name: &str) -> Result<bool, Self::Error>;
    fn read(&mut self, directory: &str, name: &str) -> Result<Option<Vec<u8>>, Self::Error>;
    // Removing a file that is not there is no error.
    fn remove(&mut self, directory: &str, name: &str) -> Result<(), Self::Error>;
    fn free(&mut self) -> Result<u64, Self::Error>;
    // Replaces a file atomically, or makes it.
    fn put(&mut self, directory: &str, name: &str, content: &[u8]) -> Result<(), Self::Error>;
    // As `put`; false, with nothing written, if the folder is gone.
    fn put_into(&mut self, folder: &str, name: &str, content: &[u8]) -> Result<bool, Self::Error>;
    // Saves a new file and returns its name.
    fn save(&mut self, directory: &str, stem: &str, extension: &str, content: &[u8]) -> Result<String, Self::Error>;
    fn to_error(&self, reason: &str) -> Self::Error;
    // The sorted names of what a directory holds of `listed`; a missing directory, or a path that leads through a file, holds none.
    fn names(&mut self, directory: &str, listed: Listed) -> Result<Vec<String>, Self::Error>;
    // Makes a directory with the ones above it.
    fn make(&mut self, directory: &str) -> Result<(), Self::Error>;
    // Removes a folder of a directory with what it holds; a missing one is no error.
    fn discard(&mut self, directory: &str, folder: &str) -> Result<(), Self::Error>;
    // The partition's size in bytes and its block.
    fn blocks(&mut self) -> Result<(u64, u32), Self::Error>;
    // A file's or folder's size in bytes and its times of change, access and status change, in seconds of UTC; none if missing.
    fn stat(&mut self, directory: &str, name: &str) -> Result<Option<(u64, [u64; 3])>, Self::Error>;
}

pub const PARTITION: &str = "userdata";
pub const MOUNT_POINT: &str = "/data";
pub const FILESYSTEM: &str = "f2fs";
/// The filesystem label that marks a partition as r1metal's.
pub const LABEL: &str = "r1metal";
/// The directory for credentials, keys and settings, which USB hosts are not shown.
pub const PRIVATE: &str = ".r1metal";
/// The directory a restore's files wait in until the next start puts them in the partition's place, which USB hosts are not shown.
pub const RESTORE: &str = ".restore";
pub const PICTURES: &str = "camera/pictures";
pub const VIDEOS: &str = "camera/videos";
pub const NOTES: &str = "audio/notes";
pub const TALKS: &str = "audio/translations";
pub const NEWS: &str = "news";
pub const CHAT: &str = "chat";
pub const CHAT_TALK: &str = "chat/talk";
pub const CHAT_DAYS: &str = "chat/days";
pub const CHAT_PRIVATE: &str = ".r1metal/chat";
/// The directory of what the interface keeps: the settings, the stamp of the flash they took, the bell and the rundowns.
pub const INTERFACE: &str = ".r1metal/interface";
/// The directories made on every mount of the partition.
pub const DIRECTORIES: [&str; 8] = [PICTURES, VIDEOS, NOTES, TALKS, NEWS, CHAT, CHAT_TALK, CHAT_DAYS];
pub const PICTURE_EXTENSION: &str = "jpg";
pub const VIDEO_EXTENSION: &str = "mp4";
/// The error text of a file reached with no partition mounted.
pub const NOT_MOUNTED: &str = "the data partition is not mounted";

// 32 s of a video's 16 Mbit/s, looked at every half second, with room for what its index and the logs add.
const RESERVE_BYTES: u64 = 64 << 20;

/// True where a partition with `free` bytes free has room for a recording: a note, a message said or a video.
pub const fn has_room(free: u64) -> bool {
    free >= RESERVE_BYTES
}

/// True where a partition has no room for a recording; free space that cannot be read is no reason.
pub fn is_full(free: Option<u64>) -> bool {
    free.is_some_and(|free| !has_room(free))
}

/// Why a recording was not saved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SaveFault {
    NoStorage,
    Full,
    // The recording could not be made or read back.
    NotRecorded,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::format;
    use core::fmt;

    #[test]
    fn recording_reserve() {
        assert_eq!(
            [is_full(Some(64 << 20)), is_full(Some((64 << 20) - 1)), is_full(Some(0)), is_full(None)],
            [false, true, true, false],
            "a recording needs 64 MiB free, and the screen is told when there is less; free space that cannot be read is no reason"
        );
    }

    #[derive(PartialEq, Eq, Debug)]
    pub(crate) struct Refused(pub(crate) String);

    impl fmt::Display for Refused {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.0)
        }
    }

    impl core::error::Error for Refused {}

    #[derive(Default)]
    pub(crate) struct Disk {
        pub(crate) files: Vec<(String, String, Vec<u8>)>,
        // Directories made, which hold no file yet.
        pub(crate) folders_made: Vec<String>,
        pub(crate) free: u64,
        pub(crate) total: u64,
        pub(crate) block: u32,
        pub(crate) effects: Vec<String>,
        // The name of a file no `put` writes.
        pub(crate) unwritable: Option<String>,
        // What `stat` tells of every file's times.
        pub(crate) times: [u64; 3],
    }

    impl Disk {
        pub(crate) fn with_files(names: &[(&str, &str)]) -> Disk {
            let file =
                |(directory, name): &(&str, &str)| ((*directory).into(), (*name).into(), Vec::from(b"file".as_slice()));
            Disk { files: names.iter().map(file).collect(), free: u64::MAX, ..Disk::default() }
        }

        pub(crate) fn names(&self) -> Vec<&str> {
            self.files.iter().map(|file| file.1.as_str()).collect()
        }
    }

    impl Files for &mut Disk {
        type Error = Refused;

        fn has(&mut self, directory: &str, name: &str) -> Result<bool, Refused> {
            self.effects.push(format!("has {name}"));
            Ok(self.files.iter().any(|file| file.0 == directory && file.1 == name))
        }

        fn read(&mut self, directory: &str, name: &str) -> Result<Option<Vec<u8>>, Refused> {
            self.effects.push(format!("read {name}"));
            Ok(self.files.iter().find(|file| file.0 == directory && file.1 == name).map(|file| file.2.clone()))
        }

        fn remove(&mut self, directory: &str, name: &str) -> Result<(), Refused> {
            self.effects.push(format!("remove {name}"));
            self.files.retain(|file| file.0 != directory || file.1 != name);
            Ok(())
        }

        fn free(&mut self) -> Result<u64, Refused> {
            self.effects.push("free".into());
            Ok(self.free)
        }

        fn put(&mut self, directory: &str, name: &str, content: &[u8]) -> Result<(), Refused> {
            self.effects.push(format!("put {name}"));
            if self.unwritable.as_deref() == Some(name) {
                return Err(Refused(format!("{name} is not written")));
            }
            self.files.retain(|file| file.0 != directory || file.1 != name);
            self.files.push((directory.into(), name.into(), content.into()));
            Ok(())
        }

        fn put_into(&mut self, folder: &str, name: &str, content: &[u8]) -> Result<bool, Refused> {
            let has_folder = self.files.iter().any(|file| file.0 == folder);
            if has_folder {
                self.put(folder, name, content)?;
            }
            Ok(has_folder)
        }

        fn save(&mut self, directory: &str, stem: &str, extension: &str, content: &[u8]) -> Result<String, Refused> {
            let name = format!("{stem}.{extension}");
            self.effects.push(format!("save {name}"));
            self.files.push((directory.into(), name.clone(), content.into()));
            Ok(name)
        }

        fn to_error(&self, reason: &str) -> Refused {
            Refused(reason.into())
        }

        fn names(&mut self, directory: &str, listed: Listed) -> Result<Vec<String>, Refused> {
            if listed != Listed::Folders {
                self.effects.push(format!("names {directory}"));
                let mut names: Vec<String> =
                    self.files.iter().filter(|file| file.0 == directory).map(|file| file.1.clone()).collect();
                names.sort();
                return Ok(names);
            }
            self.effects.push(format!("folders {directory}"));
            let to_folder = |path: &String| {
                let below = path.strip_prefix(directory)?.strip_prefix('/')?;
                below.split('/').next().map(String::from)
            };
            let paths = self.files.iter().map(|file| &file.0).chain(&self.folders_made);
            let mut folders: Vec<String> = Vec::new();
            for folder in paths.filter_map(to_folder) {
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
            Ok(folders)
        }

        fn make(&mut self, directory: &str) -> Result<(), Refused> {
            self.effects.push(format!("make {directory}"));
            if !self.folders_made.iter().any(|folder| folder == directory) {
                self.folders_made.push(directory.into());
            }
            Ok(())
        }

        fn discard(&mut self, directory: &str, folder: &str) -> Result<(), Refused> {
            self.effects.push(format!("discard {folder}"));
            let path = format!("{directory}/{folder}");
            let is_within = |at: &str| at == path || at.strip_prefix(&path).is_some_and(|rest| rest.starts_with('/'));
            self.files.retain(|file| !is_within(&file.0));
            self.folders_made.retain(|made| !is_within(made));
            Ok(())
        }

        fn blocks(&mut self) -> Result<(u64, u32), Refused> {
            self.effects.push("blocks".into());
            Ok((self.total, self.block))
        }

        fn stat(&mut self, directory: &str, name: &str) -> Result<Option<(u64, [u64; 3])>, Refused> {
            self.effects.push(format!("stat {name}"));
            let file = self.files.iter().find(|file| file.0 == directory && file.1 == name);
            Ok(file.map(|file| (file.2.len() as u64, self.times)))
        }
    }
}

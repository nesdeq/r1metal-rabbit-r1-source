use crate::{log, sys};
use r1_funcore::base::file_names::{self, Names};
use r1_funcore::storage::paths::{self, Listed, NO_NAME};
use r1_funcore::storage::safe::{Kind, to_trim_line};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

const FITRIM: u32 = sys::ioc(3, b'X', 121, 24);

pub(super) fn write_over(directory: &Path, path: &Path, content: &[u8]) -> io::Result<()> {
    write_over_with(directory, path, |file| file.write_all(content))
}

// Writes a file anew beside itself, as `fill` writes it, commits it and puts it in its place.
pub(super) fn write_over_with(
    directory: &Path,
    path: &Path,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| sys::invalid(NO_NAME))?;
    let temporary = path.with_file_name(paths::new_name(name));
    let mut file = File::create(&temporary)?;
    fill(&mut file)?;
    file.sync_all()?;
    move_into_place(&temporary, path, directory)
}

pub(super) fn move_into_place(temporary: &Path, path: &Path, directory: &Path) -> io::Result<()> {
    fs::rename(temporary, path)?;
    File::open(directory)?.sync_all()
}

// True where something stands at the path; a path that leads through a file names nothing, and any other failure to look is told.
pub(super) fn exists(path: &Path) -> io::Result<bool> {
    match path.try_exists() {
        Err(e) if e.kind() == io::ErrorKind::NotADirectory => Ok(false),
        found => found,
    }
}

pub(super) struct Folder<'a>(pub(super) &'a Path);

impl Names for Folder<'_> {
    type Error = io::Error;

    fn has(&mut self, name: &str) -> io::Result<bool> {
        exists(&self.0.join(name))
    }
}

pub(super) fn names_in(directory: &Path, listed: Listed) -> io::Result<Vec<String>> {
    let kind_of = |entry: &fs::DirEntry| {
        entry.file_type().map_or(Kind::Other, |kind| Kind::from_type(kind.is_dir(), kind.is_file()))
    };
    let names = fs::read_dir(directory)?
        .flatten()
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry)))
        .filter(|(name, entry)| listed.holds(kind_of(entry), name))
        .map(|(name, _)| name)
        .collect();
    Ok(file_names::to_sorted(names))
}

pub(super) fn fitrim(root: &Path) -> io::Result<u64> {
    let mut range = [0u64, u64::MAX, 0];
    File::open(root).and_then(|root| unsafe { sys::ioctl(root.as_raw_fd(), FITRIM, &mut range) })?;
    Ok(range[1])
}

pub(super) fn trim(root: &Path) {
    match fitrim(root) {
        Ok(bytes) => log!("{}", to_trim_line(bytes)),
        Err(e) => log!("safe: handing the free blocks back: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitrim_number() {
        assert_eq!(FITRIM, 0xc018_5879);
    }
}

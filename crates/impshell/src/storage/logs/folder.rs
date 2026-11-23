use crate::log;
use crate::storage::files::names_in;
use crate::sys::klog::MAX_RECORD;
use r1_funcore::base::time::stamp::is_compact_day;
use r1_funcore::logs::lines::{LINE_END, Mend, to_cut_line};
use r1_funcore::logs::records::{end_of_log, last_number};
use r1_funcore::logs::{Kind, LOG};
use r1_funcore::storage::paths::Listed;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub(super) struct Folder {
    pub(super) day: String,
    pub(super) path: PathBuf,
    files: [Option<File>; Kind::ALL.len()],
}

fn mend(path: &Path) -> io::Result<u64> {
    let mut file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let (mut mend, mut block) = (Mend::new(file.metadata()?.len()), [0u8; MAX_RECORD]);
    while let Some((start, length)) = mend.next_piece(block.len()) {
        let piece = &mut block[..length];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(piece)?;
        mend.take(start, piece);
    }
    if let Some((end, is_open)) = mend.cut() {
        file.set_len(end)?;
        if is_open {
            file.seek(SeekFrom::End(0))?;
            file.write_all(LINE_END)?;
        }
    }
    Ok(mend.lost_bytes())
}

pub(super) fn days(root: &Path) -> io::Result<Vec<String>> {
    Ok(names_in(root, Listed::Folders)?.into_iter().filter(|name| is_compact_day(name)).collect())
}

impl Folder {
    pub(super) fn open(root: &Path, day: String) -> io::Result<Folder> {
        let path = root.join(&day);
        fs::create_dir_all(&path)?;
        let mut lost_bytes = 0;
        for kind in Kind::ALL {
            lost_bytes += mend(&path.join(kind.name()))?;
        }
        if let Some(line) = to_cut_line(lost_bytes, &day) {
            log!("{line}");
        }
        Ok(Folder { day, path, files: Default::default() })
    }

    pub(super) fn append(&mut self, kind: Kind, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let file = match &mut self.files[kind as usize] {
            Some(file) => file,
            slot => slot.insert(OpenOptions::new().create(true).append(true).open(self.path.join(kind.name()))?),
        };
        file.write_all(bytes)
    }

    pub(super) fn commit(&self) -> io::Result<()> {
        self.files.iter().flatten().try_for_each(File::sync_data)
    }

    pub(super) fn release(&mut self) {
        self.files = Default::default();
    }
}

pub(super) fn last_record_number(root: &Path) -> io::Result<Option<u64>> {
    let Some(day) = days(root)?.pop() else {
        return Ok(None);
    };
    let mut file = match File::open(root.join(day).join(LOG)) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let start = end_of_log(file.metadata()?.len(), MAX_RECORD);
    file.seek(SeekFrom::Start(start))?;
    let mut end = Vec::new();
    file.read_to_end(&mut end)?;
    Ok(last_number(&end, start))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::scratch;
    use r1_funcore::logs::{FRAMES, POWER};

    #[test]
    fn last_record_in_newest_day() {
        let root = scratch("last");
        assert_eq!(last_record_number(&root).unwrap(), None);
        for (day, record) in [("20261010", "6,900,1,-;yesterday\n"), ("20261011", "6,17,1,-;today\n")] {
            let mut folder = Folder::open(&root, day.into()).unwrap();
            folder.append(Kind::Log, format!("10:00:00.000 {record}").as_bytes()).unwrap();
            folder.append(Kind::Frames, b"").unwrap();
            assert!(
                folder.files[Kind::Frames as usize].is_none() && !folder.path.join(FRAMES).exists(),
                "a file nothing came for is not made"
            );
            folder.commit().unwrap();
        }
        fs::write(root.join("20261012.tar.gz"), b"an archive is no folder").unwrap();
        fs::write(root.join("boot"), b"a boot").unwrap();
        assert_eq!(last_record_number(&root).unwrap(), Some(17));
        fs::create_dir_all(root.join("20261013")).unwrap();
        assert_eq!(last_record_number(&root).unwrap(), None, "a newest day without a log has no last record");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mend_zero_tail() {
        let root = scratch("mend");
        let lines = b"20:54:20.948 6,6410,92989375,-; the last line committed\n".repeat(400);
        let file = |name: &str, content: &[u8]| {
            fs::write(root.join(name), content).unwrap();
            (mend(&root.join(name)).unwrap(), fs::read(root.join(name)).unwrap())
        };
        assert_eq!(file("whole", &lines), (0, lines.clone()), "a file that ends in a line is left alone");
        assert_eq!(file("zeros", &[&lines[..], &[0u8; 25_101]].concat()), (25_101, lines.clone()));
        assert_eq!(
            file("cut", &[&lines[..lines.len() - 9], &[0u8; 3]].concat()),
            (3, [&lines[..lines.len() - 9], b"\n"].concat()),
            "a line that was cut off is ended"
        );
        assert_eq!(file("nothing", &[0u8; 20_000]), (20_000, Vec::new()));
        assert_eq!((file("empty", b""), mend(&root.join("none")).unwrap()), ((0, Vec::new()), 0));
        let mut content = lines.clone();
        content[100] = 0;
        assert_eq!(file("inside", &content), (0, content.clone()), "only the end is looked at");
        fs::create_dir_all(root.join("20261011")).unwrap();
        fs::write(root.join("20261011").join(POWER), [&b"20:54:20.925 4396 mV\n"[..], &[0u8; 64]].concat()).unwrap();
        let folder = Folder::open(&root, "20261011".into()).unwrap();
        assert_eq!(
            fs::read(folder.path.join(POWER)).unwrap(),
            b"20:54:20.925 4396 mV\n",
            "a folder is mended when it is opened"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

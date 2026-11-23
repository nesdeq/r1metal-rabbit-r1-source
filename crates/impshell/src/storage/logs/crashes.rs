use super::now;
use crate::log;
use r1_funcore::fs::kedump;
use r1_funcore::logs::scrub::blank_signal;
use r1_funcore::logs::{self, CRASH, Crashes, DUMP_COPY, RECORD_COPY};
use r1_funcore::signal::THREADS;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;

// Writes a crash's copy, with Signal's lines blanked where a scrub's mark says the copy may hold lines from before it.
fn write_copy(folder: &Path, name: &str, bytes: &mut [u8], is_blanked: bool) -> io::Result<()> {
    if is_blanked {
        blank_signal(bytes, &THREADS);
    }
    let crashes = folder.join(CRASH);
    fs::create_dir_all(&crashes)?;
    let mut copy = File::create(crashes.join(logs::crash_copy_name(now().0, name)))?;
    copy.write_all(bytes)?;
    copy.sync_all()
}

pub(super) fn take_dump(partition: &Path, record: &Path, folder: &Path, is_blanked: bool) -> io::Result<bool> {
    let mut file = OpenOptions::new().read(true).write(true).open(partition)?;
    let mut head = [0u8; kedump::HEAD];
    file.read_exact_at(&mut head, 0)?;
    let Some(length) = kedump::table_length(&head) else {
        return Ok(false);
    };
    let size = file.seek(SeekFrom::End(0))?;
    let length = kedump::check_table(length, size).map_err(|overflow| io::Error::other(overflow.message()))?;
    let mut table = vec![0u8; length];
    file.read_exact_at(&mut table, 0)?;
    let extent = kedump::check_parts(length, &kedump::parts(&table), size)
        .map_err(|overflow| io::Error::other(overflow.message()))?;
    let mut dump = vec![0u8; extent as usize];
    file.read_exact_at(&mut dump, 0)?;
    write_copy(folder, DUMP_COPY, &mut dump, is_blanked)?;
    if let Ok(mut content) = fs::read(record) {
        write_copy(folder, RECORD_COPY, &mut content, is_blanked)?;
    }
    // The table goes only once its copy is committed; without it the bootloader finds nothing to hand back.
    file.write_all_at(&vec![0u8; length], 0)?;
    file.sync_all()?;
    Ok(true)
}

pub(super) fn take_crashes(store: &Path, folder: &Path, list: &str, is_blanked: bool) -> io::Result<(u32, String)> {
    let entries = match fs::read_dir(store) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((0, list.into())),
        Err(e) => return Err(e),
    };
    let mut crashes = Crashes::parse(list);
    // The store is left as it is: emptying it would drop what it has of the running boot.
    for entry in entries.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file())) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let mut record = match fs::read(entry.path()) {
            Ok(record) => record,
            Err(e) => {
                log!("logs: the crash store's {name}: {e}");
                continue;
            }
        };
        if crashes.add_record(&name, &record) {
            write_copy(folder, &name, &mut record, is_blanked)?;
        }
    }
    Ok((crashes.copy_count(), crashes.into_text()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::scratch;

    fn copies(folder: &Path) -> Vec<(String, Vec<u8>)> {
        let mut copies: Vec<(String, Vec<u8>)> = fs::read_dir(folder.join(CRASH))
            .unwrap()
            .flatten()
            .map(|entry| (entry.file_name().into_string().unwrap()[7..].to_string(), fs::read(entry.path()).unwrap()))
            .collect();
        copies.sort();
        copies
    }

    #[test]
    fn crash_record_copied_once() {
        let root = scratch("crash");
        let (store, folder) = (root.join("pstore"), root.join("20261011"));
        assert_eq!(take_crashes(&store, &folder, "", false).unwrap(), (0, String::new()), "no store, nothing to copy");
        fs::create_dir_all(&store).unwrap();
        fs::create_dir_all(&folder).unwrap();
        assert_eq!(take_crashes(&store, &folder, "", false).unwrap().0, 0);
        assert!(!folder.join(CRASH).exists(), "no folder for crashes without one");
        fs::write(store.join("console-ramoops-0"), b"last words").unwrap();
        fs::write(store.join("dmesg-ramoops-0"), b"panic").unwrap();
        let (count, list) = take_crashes(&store, &folder, "", false).unwrap();
        assert_eq!(
            (count, list.as_str()),
            (2, "console-ramoops-0 10 e9429783\ndmesg-ramoops-0 5 9ea78192\n"),
            "the checksums are zlib's"
        );
        assert_eq!(fs::read_dir(&store).unwrap().count(), 2, "the store keeps what it has of the running boot");
        assert_eq!(
            copies(&folder),
            [("console-ramoops-0".into(), b"last words".to_vec()), ("dmesg-ramoops-0".into(), b"panic".to_vec())]
        );
        assert_eq!(
            take_crashes(&store, &folder, &list, false).unwrap(),
            (0, list.clone()),
            "the bootloader hands the last crash back at every start: it is taken once"
        );
        assert_eq!(copies(&folder).len(), 2);
        fs::write(store.join("console-ramoops-0"), b"a newer crash").unwrap();
        fs::remove_file(store.join("dmesg-ramoops-0")).unwrap();
        fs::remove_dir_all(folder.join(CRASH)).unwrap();
        let (count, next_list) = take_crashes(&store, &folder, &list, false).unwrap();
        assert_eq!((count, copies(&folder)), (1, vec![("console-ramoops-0".into(), b"a newer crash".to_vec())]));
        assert!(
            next_list.starts_with("console-ramoops-0 13 ") && next_list.ends_with("dmesg-ramoops-0 5 9ea78192\n"),
            "what is gone from the store stays listed, should it come back"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copies_blanked_behind_a_scrub() {
        let root = scratch("crash-blanked");
        let (store, folder) = (root.join("pstore"), root.join("20261011"));
        fs::create_dir_all(&store).unwrap();
        fs::create_dir_all(&folder).unwrap();
        let console = b"<6>[ 9.1] (2)[412:signal]r1metal: signal: sent\n<0>[ 9.2] Kernel panic\n";
        fs::write(store.join("console-ramoops-0"), console).unwrap();
        let (count, list) = take_crashes(&store, &folder, "", true).unwrap();
        let mut blanked = console.to_vec();
        assert!(blank_signal(&mut blanked, &THREADS), "the store holds a line of Signal's");
        assert_eq!(
            (count, copies(&folder)),
            (1, vec![("console-ramoops-0".into(), blanked)]),
            "the copy made behind a scrub's mark holds none of Signal's lines, every byte else where it was"
        );
        assert_eq!(
            take_crashes(&store, &root.join("20261012"), &list, false).unwrap().0,
            0,
            "and the record is listed by what the store holds, so that it is taken once"
        );
        let (partition, record) = (root.join("expdb"), root.join("reboot-reason"));
        let mut disk = vec![0u8; 0x2000];
        disk[..4].copy_from_slice(&0xaee0_deadu32.to_le_bytes());
        disk[8..12].copy_from_slice(&0x1030u32.to_le_bytes());
        disk[0x38..0x3c].copy_from_slice(&0x1200u32.to_le_bytes());
        disk[0x3c..0x40].copy_from_slice(&(console.len() as u32).to_le_bytes());
        disk[0x50..0x5e].copy_from_slice(b"SYS_PSTORE_RAW");
        disk[0x1200..0x1200 + console.len()].copy_from_slice(console);
        fs::write(&partition, &disk).unwrap();
        let dumps = root.join("20261013");
        fs::create_dir_all(&dumps).unwrap();
        assert!(take_dump(&partition, &record, &dumps, true).unwrap());
        let (_, dump) = copies(&dumps).remove(0);
        assert_eq!(dump.len(), 0x1200 + console.len(), "the dump as long as it was");
        assert!(
            dump[0x1200..].ends_with(b"\n<0>[ 9.2] Kernel panic\n") && dump[0x1200..0x1200 + 46] == [b' '; 46],
            "its line of Signal's blanked where it stood"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dump_copied_then_table_cleared() {
        let root = scratch("dump");
        let (partition, record, folder) = (root.join("expdb"), root.join("reboot-reason"), root.join("20261015"));
        fs::create_dir_all(&folder).unwrap();
        let mut disk = vec![0u8; 0x4000];
        fs::write(&partition, &disk).unwrap();
        assert!(!take_dump(&partition, &record, &folder, false).unwrap(), "zeros, as on an R1 that never crashed");
        assert!(!folder.join(CRASH).exists());
        disk[..4].copy_from_slice(&0xaee0_deadu32.to_le_bytes());
        disk[8..12].copy_from_slice(&0x1030u32.to_le_bytes());
        disk[0x38..0x3c].copy_from_slice(&0x1200u32.to_le_bytes());
        disk[0x3c..0x40].copy_from_slice(&0x100u32.to_le_bytes());
        disk[0x50..0x62].copy_from_slice(b"SYS_RAMCONSOLE_RAW");
        disk[0x1200..0x1300].fill(0x47);
        disk[0x1300..].fill(0x5a);
        for (at, value, reason) in [
            (0x3c, 0x4000, "a table that names more than the partition holds is left as it is"),
            (8, u32::MAX, "and so is one longer than the partition, never read"),
        ] {
            let mut bad_disk = disk.clone();
            bad_disk[at..at + 4].copy_from_slice(&value.to_le_bytes());
            fs::write(&partition, &bad_disk).unwrap();
            assert!(take_dump(&partition, &record, &folder, false).is_err(), "{reason}");
            assert!(fs::read(&partition).unwrap() == bad_disk && !folder.join(CRASH).exists(), "{reason}");
        }
        fs::write(&partition, &disk).unwrap();
        fs::write(&record, "WDT status: 2 fiq step: 71  exception type: 2\n").unwrap();
        assert!(take_dump(&partition, &record, &folder, false).unwrap());
        assert_eq!(
            copies(&folder),
            [
                ("expdb".into(), disk[..0x1300].to_vec()),
                ("reboot-reason".into(), b"WDT status: 2 fiq step: 71  exception type: 2\n".to_vec())
            ],
            "the table and the parts it names, to the end of the last, and what the kernel read of the record"
        );
        let content = fs::read(&partition).unwrap();
        assert!(
            content[..0x1030].iter().all(|byte| *byte == 0) && content[0x1030..] == disk[0x1030..],
            "then the table alone is taken off"
        );
        assert!(!take_dump(&partition, &record, &folder, false).unwrap(), "and the next start finds nothing to take");
        fs::remove_dir_all(root).unwrap();
    }
}

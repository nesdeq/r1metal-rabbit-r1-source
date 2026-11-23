use crate::log;
use crate::storage::Data;
use crate::storage::nvram::{self, Partition};
use r1_funcore::cell::DIRECTORY;
use r1_funcore::cell::drives::{
    SEEDED_FILE, join_path, parse_seeded, to_drive_folders, to_seed_directory, to_seeded_line, to_seeded_text, to_seeds,
};
use r1_image::ext4::{Ext4, Kind};
use std::io;

// Copies what lies `under` the drive's folder of its partition into both of the drive's folders; returns the count of files.
fn copy(files: &mut Ext4<Partition>, data: &Data, (drive, folder): (u8, &str), under: &str) -> io::Result<usize> {
    let mut count = 0;
    for (name, kind) in files.list(&join_path(folder, under))? {
        let path = join_path(under, &name);
        match kind {
            Kind::Directory => {
                for (root, _) in to_drive_folders(drive) {
                    data.make(&to_seed_directory(root, drive, &path))?;
                }
                count += copy(files, data, (drive, folder), &path)?;
            }
            Kind::Regular => {
                let content = files.read(&join_path(folder, &path))?;
                for (root, _) in to_drive_folders(drive) {
                    data.put(&to_seed_directory(root, drive, under), &name, &content)?;
                }
                count += 1;
            }
            Kind::Other => {}
        }
    }
    Ok(count)
}

// Fills each drive the record does not name from the factory's partition, the modem's folder and the factory's copy alike, and adds it to the record.
pub(super) fn seed(data: &Data) -> io::Result<()> {
    let seeded = data.text(DIRECTORY, SEEDED_FILE)?.map(|text| parse_seeded(&text)).unwrap_or_default();
    let mut done = seeded.clone();
    for (drive, partition, folder) in to_seeds(&seeded) {
        let mut files = nvram::files(partition)?;
        for (root, name) in to_drive_folders(drive) {
            data.discard(root, &name)?;
            data.make(&to_seed_directory(root, drive, ""))?;
        }
        let count = copy(&mut files, data, (drive, folder), "")?;
        log!("{}", to_seeded_line(drive, partition, folder, count));
        done.push(drive);
        data.put(DIRECTORY, SEEDED_FILE, to_seeded_text(&done).as_bytes())?;
    }
    Ok(())
}

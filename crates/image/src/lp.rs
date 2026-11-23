//! The partition table of an Android super image: which runs of the image each of its partitions is made of.

use crate::{ReadAt, invalid, le32, le64};
use std::io;

const SECTOR: u64 = 512;
// The table's geometry lies behind the first 4096 bytes, twice over, each copy in 4096 bytes; the table itself follows.
const GEOMETRY_AT: u64 = 4096;
const GEOMETRY_BYTES: u64 = 4096;
// The bytes the geometry's own fields take of that.
const GEOMETRY_FIELDS: usize = 52;
const GEOMETRY_MAGIC: u32 = 0x616C_4467;
const HEADER_MAGIC: u32 = 0x414C_5030;
// The part of the header every version has: up to and with the place of the list of block devices.
const HEADER_BYTES: usize = 128;
const NAME_BYTES: usize = 36;
const PARTITION_BYTES: usize = 52;
const EXTENT_BYTES: usize = 24;
// What an extent is made of: a run of the device, or zeros.
const LINEAR: u32 = 0;
const ZERO: u32 = 1;

// A run of a partition: its length in bytes and the byte of the device it begins at; without one it reads as zeros.
struct Extent {
    bytes: u64,
    at: Option<u64>,
}

pub struct Table {
    partitions: Vec<(String, Vec<Extent>)>,
}

/// Reads the first copy of the partition table of a super image.
pub fn read(device: &mut impl ReadAt) -> io::Result<Table> {
    let mut geometry = [0u8; GEOMETRY_FIELDS];
    device.read_at(GEOMETRY_AT, &mut geometry)?;
    if le32(&geometry, 0)? != GEOMETRY_MAGIC {
        return Err(invalid("no super image: its geometry is not where it belongs"));
    }
    let most = le32(&geometry, 40)? as usize;
    let table_at = GEOMETRY_AT + 2 * GEOMETRY_BYTES;
    let mut header = [0u8; HEADER_BYTES];
    device.read_at(table_at, &mut header)?;
    let (header_bytes, tables_bytes) = (le32(&header, 8)? as usize, le32(&header, 44)? as usize);
    if le32(&header, 0)? != HEADER_MAGIC || header_bytes < HEADER_BYTES || header_bytes + tables_bytes > most {
        return Err(invalid("a super image without a partition table that can be read"));
    }
    let mut tables = vec![0u8; tables_bytes];
    device.read_at(table_at + header_bytes as u64, &mut tables)?;
    let list = |at: usize| {
        Ok::<_, io::Error>((
            le32(&header, at)? as usize,
            le32(&header, at + 4)? as usize,
            le32(&header, at + 8)? as usize,
        ))
    };
    let ((partitions_at, partitions, partition_bytes), (extents_at, extents, extent_bytes)) = (list(80)?, list(92)?);
    if partition_bytes < PARTITION_BYTES || extent_bytes < EXTENT_BYTES {
        return Err(invalid("a partition table with entries too small to be read"));
    }
    // A run's bytes, where it begins and where it ends are held within a device's reach here, so that no read of them overflows.
    let beyond = || invalid("a partition larger than any device");
    let extent = |index: usize| {
        let entry = tables
            .get(extents_at + index * extent_bytes..)
            .filter(|_| index < extents)
            .ok_or_else(|| invalid("a partition that names an extent the table lacks"))?;
        let bytes = le64(entry, 0)?.checked_mul(SECTOR).ok_or_else(beyond)?;
        let at = match (le32(entry, 8)?, le32(entry, 20)?) {
            (LINEAR, 0) => {
                let at = le64(entry, 12)?.checked_mul(SECTOR).ok_or_else(beyond)?;
                at.checked_add(bytes).ok_or_else(beyond)?;
                Some(at)
            }
            (ZERO, _) => None,
            _ => return Err(invalid("a partition on a device or of a kind this reader does not know")),
        };
        Ok(Extent { bytes, at })
    };
    let mut found = Vec::new();
    for index in 0..partitions {
        // Its extents are named past its name, so that an entry cut short is refused before its name is read.
        let entry = tables.get(partitions_at + index * partition_bytes..).unwrap_or_default();
        let (first, count) = (le32(entry, 40)? as usize, le32(entry, 44)? as usize);
        let name = entry[..NAME_BYTES].split(|&byte| byte == 0).next().unwrap_or_default();
        let name = String::from_utf8_lossy(name).into_owned();
        let runs = (first..first + count).map(&extent).collect::<io::Result<Vec<Extent>>>()?;
        runs.iter().try_fold(0u64, |whole, run| whole.checked_add(run.bytes)).ok_or_else(beyond)?;
        found.push((name, runs));
    }
    Ok(Table { partitions: found })
}

impl Table {
    #[cfg(test)]
    fn names(&self) -> impl Iterator<Item = &str> {
        self.partitions.iter().map(|(name, _)| name.as_str())
    }

    /// The partition of that name, to be read from the device its table was read from.
    pub fn partition<'a, D: ReadAt>(&'a self, name: &str, device: &'a mut D) -> Option<Partition<'a, D>> {
        let (_, extents) = self.partitions.iter().find(|(known, _)| known == name)?;
        Some(Partition { device, extents })
    }
}

pub struct Partition<'a, D> {
    device: &'a mut D,
    extents: &'a [Extent],
}

impl<D: ReadAt> ReadAt for Partition<'_, D> {
    fn read_at(&mut self, mut offset: u64, mut out: &mut [u8]) -> io::Result<()> {
        let mut start = 0u64;
        for extent in self.extents {
            if offset < start + extent.bytes && !out.is_empty() {
                let within = offset - start;
                let take = (extent.bytes - within).min(out.len() as u64) as usize;
                let (now, rest) = std::mem::take(&mut out).split_at_mut(take);
                match extent.at {
                    Some(at) => self.device.read_at(at + within, now)?,
                    None => now.fill(0),
                }
                (offset, out) = (offset + take as u64, rest);
            }
            start += extent.bytes;
        }
        if out.is_empty() { Ok(()) } else { Err(invalid("a read past the end of a partition")) }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // The extents of a partition, each as (sectors, first sector or none).
    type Runs<'a> = &'a [(u64, Option<u64>)];

    // A super image of `bytes` bytes whose table names those partitions, each with its extents.
    pub(crate) fn image(bytes: usize, partitions: &[(&str, Runs)]) -> Vec<u8> {
        let mut device = vec![0u8; bytes];
        let put =
            |device: &mut Vec<u8>, at: usize, value: u32| device[at..at + 4].copy_from_slice(&value.to_le_bytes());
        put(&mut device, GEOMETRY_AT as usize, GEOMETRY_MAGIC);
        put(&mut device, GEOMETRY_AT as usize + 40, 4096);
        let (mut names, mut runs) = (Vec::new(), Vec::new());
        for (name, extents) in partitions {
            let mut entry = [0u8; PARTITION_BYTES];
            entry[..name.len()].copy_from_slice(name.as_bytes());
            entry[40..44].copy_from_slice(&((runs.len() / EXTENT_BYTES) as u32).to_le_bytes());
            entry[44..48].copy_from_slice(&(extents.len() as u32).to_le_bytes());
            names.extend(entry);
            for (sectors, at) in *extents {
                runs.extend(sectors.to_le_bytes());
                runs.extend(if at.is_some() { LINEAR } else { ZERO }.to_le_bytes());
                runs.extend(at.unwrap_or(0).to_le_bytes());
                runs.extend(0u32.to_le_bytes());
            }
        }
        let table = (GEOMETRY_AT + 2 * GEOMETRY_BYTES) as usize;
        put(&mut device, table, HEADER_MAGIC);
        put(&mut device, table + 8, HEADER_BYTES as u32);
        put(&mut device, table + 44, (names.len() + runs.len()) as u32);
        for (at, value) in [
            (80, 0),
            (84, partitions.len()),
            (88, PARTITION_BYTES),
            (92, names.len()),
            (96, runs.len() / EXTENT_BYTES),
            (100, EXTENT_BYTES),
        ] {
            put(&mut device, table + at, value as u32);
        }
        device[table + HEADER_BYTES..table + HEADER_BYTES + names.len()].copy_from_slice(&names);
        device[table + HEADER_BYTES + names.len()..table + HEADER_BYTES + names.len() + runs.len()]
            .copy_from_slice(&runs);
        device
    }

    #[test]
    fn partition_read_by_extents() {
        let mut device = image(
            64 * 512,
            &[("system_a", &[(2, Some(40))]), ("vendor_a", &[(1, Some(50)), (1, None), (2, Some(60))])],
        );
        for sector in 40..64 {
            device[sector * 512..(sector + 1) * 512].fill(sector as u8);
        }
        let mut device = &device[..];
        let table = read(&mut device).unwrap();
        assert_eq!(table.names().collect::<Vec<_>>(), ["system_a", "vendor_a"]);
        assert!(table.partition("vendor_b", &mut device).is_none());
        let mut vendor = table.partition("vendor_a", &mut device).unwrap();
        let mut whole = vec![0xFFu8; 4 * 512];
        vendor.read_at(0, &mut whole).unwrap();
        assert!(
            whole[..512].iter().all(|&byte| byte == 50) && whole[512..1024].iter().all(|&byte| byte == 0),
            "a run of the device, then a run of zeros"
        );
        assert!(whole[1024..1536].iter().all(|&byte| byte == 60) && whole[1536..].iter().all(|&byte| byte == 61));
        let mut across = [0u8; 4];
        vendor.read_at(1534, &mut across).unwrap();
        assert_eq!(across, [60, 60, 61, 61]);
        assert!(vendor.read_at(4 * 512 - 1, &mut across).is_err(), "nothing lies past its end");
    }

    #[test]
    fn foreign_super_refused() {
        assert!(read(&mut &[0u8; 20000][..]).is_err());
        assert!(read(&mut &[0u8; 100][..]).is_err());
        let mut far = image(20000, &[("vendor_a", &[(1, Some(1))])]);
        far[(GEOMETRY_AT + 2 * GEOMETRY_BYTES) as usize + HEADER_BYTES + 44] = 9;
        assert!(read(&mut &far[..]).is_err(), "a partition that names extents the table lacks");
        for runs in [&[(u64::MAX, Some(1))][..], &[(1, Some(u64::MAX))], &[(1 << 54, None), (1 << 54, None)]] {
            let huge = image(20000, &[("vendor_a", runs)]);
            assert!(read(&mut &huge[..]).is_err(), "a run, a place or a partition past any device: {runs:?}");
        }
    }
}

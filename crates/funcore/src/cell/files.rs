//! The file service the modem keeps its NVRAM through (MediaTek's `ccci_fsd`): its operations over one folder per drive of the private directory.

use crate::base::bytes::le32;
use crate::base::time::Moment;
use crate::cell::drives::{DRIVES, EMPTY_DRIVES, FACTORY_DIRECTORY, NVRAM_DIRECTORY, join_path, to_drive_folder};
use crate::cell::stream::Request;
use crate::storage::Files;
use crate::storage::paths::Listed;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// The channel the modem's requests come on.
pub const CHANNEL: u32 = 14;
/// What a slot holds past the head (`FS_MAX_BUF_SIZE`): the parameters and 16 KiB of data.
pub const CAPACITY: usize = 16 + 4 + 4 + 4 * 12 + 0x4000 + 128;

const MOST_PARAMS: usize = 6;
const HANDLES: u32 = 128;
const SYSTEM_DRIVE: i32 = b'Z' as i32;
const PATH_MOST: usize = 4095;
const NAME_MOST: usize = 255;
// Where `ccci_fsd` puts a reply's bytes in the slot: past a read's result and length (`pFsBuf->Buffer + 6*sizeof(int)`), an OTP read's result (`4*sizeof(int)`), a compact read's results (`9*sizeof(int)`).
const READ_HEAD: usize = 6 * 4;
const OTP_READ_HEAD: usize = 4 * 4;
const CMPT_READ_HEAD: usize = 9 * 4;
// The fields of the compact operations' requests (`nvram_fs_para_cmpt_t`, `nvram_fs_para_cmptw_t`) and of their replies' first word pair.
const CMPT_STEPS_AT: usize = 0;
const CMPT_ERROR_AT: usize = 4;
const CMPT_FLAGS_AT: usize = 12;
const CMPT_READ_OFFSET_AT: usize = 20;
const CMPT_READ_WHENCE_AT: usize = 24;
const CMPT_READ_LENGTH_AT: usize = 32;
const CMPT_WRITE_OFFSET_AT: usize = 16;
const CMPT_WRITE_WHENCE_AT: usize = 20;
const CMPT_WRITE_LENGTH_AT: usize = 28;
const CMPT_WRITE_HANDLE_AT: usize = 32;
// The most a file of the modem's grows to: `nvdata`, the largest partition Rabbit OS keeps the drives on, holds 64 MiB, and a write past what it holds fails as `ccci_fsd` answers `ENOSPC`.
const MOST_FILE_BYTES: usize = 64 << 20;

const OPEN: u32 = 0x1001;
const SEEK: u32 = 0x1002;
const READ: u32 = 0x1003;
const WRITE: u32 = 0x1004;
const CLOSE: u32 = 0x1005;
const CLOSE_ALL: u32 = 0x1006;
const CREATE_DIR: u32 = 0x1007;
const REMOVE_DIR: u32 = 0x1008;
const FILE_SIZE: u32 = 0x1009;
const FOLDER_SIZE: u32 = 0x100A;
const RENAME: u32 = 0x100B;
const MOVE: u32 = 0x100C;
const COUNT: u32 = 0x100D;
const DISK_INFO: u32 = 0x100E;
const DELETE: u32 = 0x100F;
const ATTRIBUTES: u32 = 0x1010;
const OPEN_HINT: u32 = 0x1011;
const FIND_FIRST: u32 = 0x1012;
const FIND_NEXT: u32 = 0x1013;
const FIND_CLOSE: u32 = 0x1014;
const LOCK_FAT: u32 = 0x1015;
const UNLOCK_ALL: u32 = 0x1016;
const SHUTDOWN: u32 = 0x1017;
const X_DELETE: u32 = 0x1018;
const CLEAR_DISK_FLAG: u32 = 0x1019;
const GET_DRIVE: u32 = 0x101A;
const CLUSTER_SIZE: u32 = 0x101B;
const SET_DISK_FLAG: u32 = 0x101C;
const OTP_WRITE: u32 = 0x101D;
const OTP_READ: u32 = 0x101E;
const OTP_LENGTH: u32 = 0x101F;
const OTP_LOCK: u32 = 0x1020;
const RESTORE: u32 = 0x1021;
const CMPT_READ: u32 = 0x1022;
const BIN_REGION: u32 = 0x1023;
const CMPT_WRITE: u32 = 0x1024;
const FILE_DETAIL: u32 = 0x1025;

const NO_ERROR: i32 = 0;
const RESERVED_ERROR: i32 = -1;
const PARAM_ERROR: i32 = -2;
const DRIVE_NOT_FOUND: i32 = -4;
const TOO_MANY_FILES: i32 = -5;
const NO_MORE_FILES: i32 = -6;
const FILE_NOT_FOUND: i32 = -9;
const INVALID_HANDLE: i32 = -10;
const UNSUPPORTED: i32 = -12;
const ACCESS_DENIED: i32 = -16;
const GENERAL_FAILURE: i32 = -18;
const PATH_NOT_FOUND: i32 = -19;
const PATH_TOO_LONG: i32 = -49;

const READ_ONLY: u32 = 0x100;
const CREATE: u32 = 0x1_0000;
const CREATE_ALWAYS: u32 = 0x2_0000;
const FILE_BEGIN: u32 = 0;
const FILE_CURRENT: u32 = 1;
const FILE_END: u32 = 2;
const ATTRIBUTE_FOLDER: u8 = 0x10;
const FILE_TYPE: u32 = 0x4;
const DIR_TYPE: u32 = 0x8;
const RECURSIVE: u32 = 0x10;
const MOVE_COPY: u32 = 0x1;
const MOVE_KILL: u32 = 0x2;
const MOVE_OVERWRITE: u32 = 0x1_0000;
const COUNT_IN_CLUSTERS: u32 = 2;
const NO_ALTERNATE: u32 = 0x1;
const ONLY_ALTERNATE: u32 = 0x2;
const DRIVE_SYSTEM: u32 = 0x4;
const DRIVE_NORMAL: u32 = 0x8;
const DRIVE_REMOVABLE: u32 = 0x10;
const CMPT_OPEN: u32 = 1 << 0;
const CMPT_SIZE: u32 = 1 << 1;
const CMPT_SEEK: u32 = 1 << 2;
const CMPT_READ_STEP: u32 = 1 << 3;
const CMPT_CLOSE: u32 = 1 << 4;
const CMPT_WRITE_STEP: u32 = 1 << 5;

const DISK_INFO_BYTES: usize = 84;
const ENTRY_BYTES: usize = 52;
const SECTOR: u32 = 512;
const BYTES_PER_SECTOR_AT: usize = 0x30;
const SECTORS_PER_CLUSTER_AT: usize = 0x34;
const TOTAL_CLUSTERS_AT: usize = 0x38;
const FREE_CLUSTERS_AT: usize = 0x40;
// MediaTek's `ccci_fsd` tells the modem this many free clusters whatever the disk holds.
const FREE_CLUSTERS: u32 = 512;
const ATTRIBUTES_AT: usize = 11;
const LONG_NAME_AT: usize = 12;
const CREATED_AT: usize = 14;
const MODIFIED_AT: usize = 22;
const SIZE_AT: usize = 28;
const LONG_NAME: u8 = 1;
const DOS_EPOCH: u32 = 1980;

// Where a path of the modem leads: a drive and the names under its folder.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Place {
    drive: u8,
    names: Vec<String>,
}

impl Place {
    fn drive_folder(&self) -> String {
        to_drive_folder(NVRAM_DIRECTORY, self.drive)
    }

    // The directory the place lies in and its own name; the drive's folder for the drive itself.
    fn split(&self) -> (String, String) {
        match self.names.split_last() {
            Some((name, parents)) => (to_directory(&self.drive_folder(), parents), name.clone()),
            None => (String::from(NVRAM_DIRECTORY), String::from(char::from(self.drive))),
        }
    }

    fn directory(&self) -> String {
        to_directory(&self.drive_folder(), &self.names)
    }

    fn factory(&self) -> Option<(String, String)> {
        let (name, parents) = self.names.split_last()?;
        Some((to_directory(&to_drive_folder(FACTORY_DIRECTORY, self.drive), parents), name.clone()))
    }

    fn child(&self, name: &str) -> Place {
        let mut names = self.names.clone();
        names.push(String::from(name));
        Place { drive: self.drive, names }
    }

    // The folder the place lies in; the drive itself for a name right under it, and for the drive.
    fn parent(&self) -> Place {
        Place { drive: self.drive, names: self.names[..self.names.len().saturating_sub(1)].to_vec() }
    }

    fn is_within(&self, place: &Place) -> bool {
        self.drive == place.drive && self.names.starts_with(&place.names)
    }
}

fn to_directory(root: &str, names: &[String]) -> String {
    names.iter().fold(String::from(root), |path, name| join_path(&path, name))
}

// What a wide path of the modem names: the low byte of each unit up to the first whose low byte is 0, backslashes as slashes; a path with two dots in a row is refused.
fn to_text(wide: &[u8]) -> Result<String, i32> {
    let text: String = wide
        .chunks(2)
        .map(|unit| unit[0])
        .take_while(|&byte| byte != 0)
        .take(PATH_MOST)
        .map(|byte| if byte == b'\\' { '/' } else { char::from(byte) })
        .collect();
    if text.contains("..") { Err(ACCESS_DENIED) } else { Ok(text) }
}

fn locate(wide: &[u8]) -> Result<Place, i32> {
    let text = to_text(wide)?;
    let bytes = text.as_bytes();
    let drive = match bytes {
        [drive, b':', ..] if DRIVES.contains(drive) => *drive,
        _ => return Err(PATH_NOT_FOUND),
    };
    let names = text[2..].split('/').filter(|name| !name.is_empty() && *name != ".").map(String::from).collect();
    Ok(Place { drive, names })
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    Missing,
    File,
    Folder,
}

// Rabbit OS's partition keeps no file read-only, so a folder is the one attribute an entry has.
const fn to_attributes(kind: Kind) -> u8 {
    if matches!(kind, Kind::Folder) { ATTRIBUTE_FOLDER } else { 0 }
}

// `ccci_fsd` asks `open(2)` for `O_RDONLY` where the flags are read-only and make nothing, `O_RDWR` else.
const fn is_read_only(flags: u32) -> bool {
    flags & (READ_ONLY | CREATE | CREATE_ALWAYS) == READ_ONLY
}

// C's `localtime` packed as MediaTek's `ccci_fsd` packs it, the day standing in for the month as well, in UTC.
fn to_dos_time(seconds: u64) -> u32 {
    let moment = Moment::from_seconds(seconds);
    let day = u32::from(moment.day);
    u32::from(moment.second) & 0x1F
        | (u32::from(moment.minute) & 0x3F) << 5
        | (u32::from(moment.hour) & 0x1F) << 11
        | (day & 0x1F) << 16
        | (day & 0x0F) << 21
        | (moment.year.saturating_sub(DOS_EPOCH) & 0x7F) << 25
}

// A name in the modem's wide characters, slashes as backslashes, cut to `most` units with its end.
fn to_wide(name: &str, most: u32) -> Vec<u8> {
    if most == 0 {
        return Vec::new();
    }
    let count = name.len().min(most as usize - 1);
    let mut wide: Vec<u8> =
        name.bytes().take(count).flat_map(|byte| [if byte == b'/' { b'\\' } else { byte }, 0]).collect();
    wide.extend_from_slice(&[0, 0]);
    wide
}

// The case-sensitive match of `ccci_fsd`'s `PatMatch`: `*` any run, `?` any one byte.
fn matches(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => matches(rest, name) || (!name.is_empty() && matches(pattern, &name[1..])),
        Some((b'?', rest)) => !name.is_empty() && matches(rest, &name[1..]),
        Some((byte, rest)) => name.first() == Some(byte) && matches(rest, &name[1..]),
    }
}

fn word(value: i32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

fn number(param: Option<&&[u8]>) -> Option<u32> {
    le32(param?, 0)
}

struct Search {
    place: Place,
    pattern: String,
    attributes: u8,
    mask: u8,
    entries: Vec<(String, Kind)>,
    next: usize,
}

// A file the modem has open: its bytes, which every handle on it shares as `open(2)`'s descriptors share an inode, and its place, none once it is deleted or a rename replaced it.
struct Open {
    place: Option<Place>,
    content: Vec<u8>,
    is_changed: bool,
    handles: usize,
}

// The open files, each by a number of its own that outlives a rename.
#[derive(Default)]
struct Opens {
    files: BTreeMap<u64, Open>,
    next: u64,
}

impl Opens {
    fn find(&self, place: &Place) -> Option<u64> {
        self.files.iter().find(|(_, open)| open.place.as_ref() == Some(place)).map(|(number, _)| *number)
    }

    fn content(&self, place: &Place) -> Option<&[u8]> {
        self.files.values().find(|open| open.place.as_ref() == Some(place)).map(|open| open.content.as_slice())
    }

    fn add(&mut self, place: Place, content: Vec<u8>) -> u64 {
        let number = self.next;
        self.next += 1;
        self.files.insert(number, Open { place: Some(place), content, is_changed: false, handles: 0 });
        number
    }

    // What was written to a file's place, as a write into its inode, now in the handles open on it.
    fn replace(&mut self, place: &Place, content: &[u8]) {
        if let Some(open) = self.files.values_mut().find(|open| open.place.as_ref() == Some(place)) {
            (open.content, open.is_changed) = (content.to_vec(), false);
        }
    }

    // The handles open on a file at or under `place` keep its bytes, the file gone from it.
    fn detach(&mut self, place: &Place) {
        for open in self.files.values_mut().filter(|open| open.place.as_ref().is_some_and(|at| at.is_within(place))) {
            open.place = None;
        }
    }

    // The handles open on a file at or under `from` follow it to where a rename took it.
    fn follow(&mut self, from: &Place, to: &Place) {
        for at in self.files.values_mut().filter_map(|open| open.place.as_mut()).filter(|at| at.is_within(from)) {
            let mut names = to.names.clone();
            names.extend_from_slice(&at.names[from.names.len()..]);
            *at = Place { drive: to.drive, names };
        }
    }
}

enum Handle {
    File { open: u64, at: u64, flags: u32 },
    // A folder opened to be read, as `open(2)` opens one: a block long, never read or written.
    Folder { size: u32, at: u64 },
    Search(Box<Search>),
}

// The handles the modem holds, by their numbers.
#[derive(Default)]
struct Handles(BTreeMap<u32, Handle>);

/// The modem's open files and searches.
#[derive(Default)]
pub struct Service {
    handles: Handles,
    opens: Opens,
}

// The operations whose first parameter is a path.
const PATH_OPERATIONS: [u32; 16] = [
    OPEN,
    CREATE_DIR,
    REMOVE_DIR,
    FOLDER_SIZE,
    RENAME,
    MOVE,
    COUNT,
    DELETE,
    ATTRIBUTES,
    OPEN_HINT,
    FIND_FIRST,
    X_DELETE,
    RESTORE,
    CMPT_READ,
    CMPT_WRITE,
    FILE_DETAIL,
];

// The result of a reply: its first word, or a compact operation's error, the word after its steps done.
fn to_result(operation: u32, reply: &[Vec<u8>]) -> i32 {
    let first = reply.first().map_or(&[][..], Vec::as_slice);
    let error = matches!(operation, CMPT_READ | CMPT_WRITE).then(|| le32(first, CMPT_ERROR_AT)).flatten();
    error.or_else(|| le32(first, 0)).map_or(0, |word| word as i32)
}

// The debug log's line of a request: its operation, its first parameter as a path or a number, an open's flags, and its result.
fn to_trace(operation: u32, params: &[&[u8]], reply: &[Vec<u8>]) -> String {
    let first = params.first().map_or_else(String::new, |param| {
        if PATH_OPERATIONS.contains(&operation) {
            format!(" {}", to_text(param).unwrap_or_else(|error| format!("refused ({error})")))
        } else {
            number(Some(param)).map_or_else(String::new, |number| format!(" {number}"))
        }
    });
    let flags = match operation {
        OPEN | OPEN_HINT => number(params.get(1)).map_or_else(String::new, |flags| format!(" flags {flags:#x}")),
        _ => String::new(),
    };
    format!("cell: file service {operation:#x}{first}{flags} answered {}", to_result(operation, reply))
}

impl Service {
    /// Serves a request of the modem over `files`: the reply's parameters, the log's lines (a failure of the partition, the request where its result is an error), and the debug log's line of the request and its result.
    pub fn serve<F: Files>(&mut self, mut files: F, request: &Request) -> (Vec<Vec<u8>>, Vec<String>, String) {
        let mut fault = None;
        let asked = request.params(MOST_PARAMS, false);
        let params = match &asked {
            Some(asked) => {
                let mut disk = Disk { files: &mut files, fault: &mut fault, opens: &mut self.opens };
                self.handles.answer(&mut disk, request.operation, asked)
            }
            None => vec![word(PARAM_ERROR)],
        };
        let trace = to_trace(request.operation, asked.as_deref().unwrap_or_default(), &params);
        let fault = fault.map(|error| format!("cell: the file service's operation {:#x}: {error}", request.operation));
        let failure = (to_result(request.operation, &params) < 0).then(|| trace.clone());
        (params, fault.into_iter().chain(failure).collect(), trace)
    }

    /// Writes every changed file and lets go of every handle, as the modem stops; the failure's text, if any.
    pub fn close_all<F: Files>(&mut self, mut files: F) -> Option<String> {
        let mut fault = None;
        self.handles.close_all(&mut Disk { files: &mut files, fault: &mut fault, opens: &mut self.opens });
        fault.map(|error| format!("cell: writing the modem's files as it stops: {error}"))
    }
}

impl Handles {
    fn answer<F: Files>(&mut self, disk: &mut Disk<F>, operation: u32, params: &[&[u8]]) -> Vec<Vec<u8>> {
        let place = |at: usize| params.get(at).ok_or(PARAM_ERROR).and_then(|param| locate(param));
        let handle = |at: usize| number(params.get(at));
        match operation {
            OPEN | OPEN_HINT => {
                let result = place(0).and_then(|place| self.open(disk, place, number(params.get(1)).unwrap_or(0)));
                let mut reply = vec![word(result.unwrap_or_else(|error| error))];
                if operation == OPEN_HINT {
                    reply.push(params.get(2).map_or_else(Vec::new, |hint| hint.to_vec()));
                }
                reply
            }
            SEEK => vec![word(self.seek(disk, handle(0), number(params.get(1)), number(params.get(2))))],
            READ => {
                let most = CAPACITY - READ_HEAD;
                let length = number(params.get(1)).map(|length| (length as usize).min(most));
                let (result, bytes) = self.read(disk, handle(0), length);
                vec![word(result), (bytes.len() as u32).to_le_bytes().to_vec(), bytes]
            }
            WRITE => {
                let length = number(params.get(2)).map(|length| length as usize);
                let bytes = params.get(1).map(|bytes| &bytes[..length.unwrap_or(0).min(bytes.len())]);
                let (result, written) = self.write(disk, handle(0), bytes);
                vec![word(result), written.to_le_bytes().to_vec()]
            }
            CLOSE => vec![word(handle(0).map_or(INVALID_HANDLE, |handle| self.close(disk, handle)))],
            CLOSE_ALL => vec![word(self.close_all(disk))],
            CREATE_DIR => vec![word(place(0).map_or_else(|error| error, |place| make_folder(disk, &place)))],
            REMOVE_DIR => vec![word(place(0).map_or_else(|error| error, |place| remove_folder(disk, &place)))],
            FILE_SIZE => {
                let (result, size) = self.size(disk, handle(0));
                vec![word(result), size.to_le_bytes().to_vec()]
            }
            FOLDER_SIZE => {
                let flag = number(params.get(1)).unwrap_or(0);
                vec![word(place(0).map_or_else(|error| error, |place| folder_size(disk, &place, flag)))]
            }
            RENAME => vec![word(
                place(0)
                    .and_then(|from| Ok((from, place(1)?)))
                    .map_or_else(|error| error, |(from, to)| rename(disk, &from, &to)),
            )],
            MOVE => {
                let flag = number(params.get(2)).unwrap_or(0);
                vec![word(
                    place(0)
                        .and_then(|from| Ok((from, place(1)?)))
                        .map_or_else(|error| error, |(from, to)| move_file(disk, &from, &to, flag)),
                )]
            }
            COUNT => {
                let flag = number(params.get(1)).unwrap_or(0);
                vec![word(place(0).map_or_else(|error| error, |place| count(disk, &place, flag)))]
            }
            DISK_INFO => {
                let (result, info) = disk_info(disk);
                vec![word(result), info]
            }
            DELETE => vec![word(place(0).map_or_else(|error| error, |place| delete(disk, &place)))],
            ATTRIBUTES => vec![word(place(0).map_or_else(|error| error, |place| attributes(disk, &place)))],
            FIND_FIRST => {
                let attributes = params.get(1).and_then(|param| param.first()).copied().unwrap_or(0);
                let mask = params.get(2).and_then(|param| param.first()).copied().unwrap_or(0);
                let most = number(params.get(3)).unwrap_or(0);
                self.find_first(disk, place(0), (attributes, mask), most)
            }
            FIND_NEXT => self.find_next(disk, handle(0), number(params.get(1)).unwrap_or(0)),
            FIND_CLOSE => vec![word(self.find_close(handle(0)))],
            LOCK_FAT | CLEAR_DISK_FLAG | SET_DISK_FLAG => vec![word(NO_ERROR)],
            UNLOCK_ALL => vec![word(1)],
            SHUTDOWN => {
                self.close_all(disk);
                Vec::new()
            }
            X_DELETE => {
                let flag = number(params.get(1)).unwrap_or(0);
                vec![word(place(0).map_or_else(|error| error, |place| x_delete(disk, &place, flag)))]
            }
            GET_DRIVE => vec![word(get_drive(number(params.first()), number(params.get(1)), number(params.get(2))))],
            CLUSTER_SIZE => vec![word(cluster_size(disk, number(params.first())))],
            OTP_WRITE | OTP_LOCK => vec![word(UNSUPPORTED)],
            OTP_READ => {
                let length = number(params.get(2)).map_or(0, |length| (length as usize).min(CAPACITY - OTP_READ_HEAD));
                vec![word(UNSUPPORTED), vec![0; length]]
            }
            OTP_LENGTH => vec![word(UNSUPPORTED), 0u32.to_le_bytes().to_vec()],
            RESTORE => vec![word(place(0).map_or_else(|error| error, |place| restore(disk, &place)))],
            CMPT_READ => self.compact_read(disk, place(0), params.get(1).copied()),
            BIN_REGION => vec![word(PARAM_ERROR)],
            CMPT_WRITE => self.compact_write(disk, place(0), params.get(1).copied(), params.get(2).copied()),
            FILE_DETAIL => {
                let (result, detail) = place(0).map_or_else(|error| (error, [0; 3]), |place| file_detail(disk, &place));
                vec![word(result), detail.iter().flat_map(|time| time.to_le_bytes()).collect()]
            }
            _ => vec![word(PARAM_ERROR)],
        }
    }

    fn free_handle(&self) -> Option<u32> {
        (1..=HANDLES).find(|number| !self.0.contains_key(number))
    }

    fn open<F: Files>(&mut self, disk: &mut Disk<F>, place: Place, flags: u32) -> Result<i32, i32> {
        let number = self.free_handle().ok_or(TOO_MANY_FILES)?;
        match kind(disk, &place) {
            Kind::Folder if is_read_only(flags) => {
                let size = disk.blocks().map_or(0, |(_, block)| block);
                self.0.insert(number, Handle::Folder { size, at: 0 });
                return Ok(number as i32);
            }
            Kind::Folder => return Err(GENERAL_FAILURE),
            Kind::File if flags & CREATE_ALWAYS != 0 => put(disk, &place, &[])?,
            Kind::File => {}
            Kind::Missing if flags & (CREATE | CREATE_ALWAYS) != 0 => {
                if place.names.is_empty() || kind(disk, &place.parent()) != Kind::Folder {
                    return Err(FILE_NOT_FOUND);
                }
                put(disk, &place, &[])?;
            }
            Kind::Missing => return Err(FILE_NOT_FOUND),
        }
        let open = match disk.opens.find(&place) {
            Some(open) => open,
            None => {
                let content = read_disk(disk, &place)?;
                disk.opens.add(place, content)
            }
        };
        if let Some(file) = disk.opens.files.get_mut(&open) {
            file.handles += 1;
        }
        self.0.insert(number, Handle::File { open, at: 0, flags });
        Ok(number as i32)
    }

    fn seek<F: Files>(&mut self, disk: &Disk<F>, handle: Option<u32>, offset: Option<u32>, whence: Option<u32>) -> i32 {
        let (end, at) = match handle.and_then(|handle| self.0.get_mut(&handle)) {
            Some(Handle::File { open, at, .. }) => {
                (disk.opens.files.get(open).map_or(0, |file| file.content.len()) as i64, at)
            }
            Some(Handle::Folder { size, at }) => (i64::from(*size), at),
            _ => return INVALID_HANDLE,
        };
        let offset = i64::from(offset.unwrap_or(0) as i32);
        let base = match whence {
            Some(FILE_BEGIN) => 0,
            Some(FILE_CURRENT) => *at as i64,
            Some(FILE_END) => end,
            _ => return PARAM_ERROR,
        };
        match u64::try_from(base + offset) {
            Ok(position) if position <= i32::MAX as u64 => {
                *at = position;
                position as i32
            }
            _ => GENERAL_FAILURE,
        }
    }

    fn read<F: Files>(&mut self, disk: &Disk<F>, handle: Option<u32>, length: Option<usize>) -> (i32, Vec<u8>) {
        let (open, at) = match handle.and_then(|handle| self.0.get_mut(&handle)) {
            Some(Handle::File { open, at, .. }) => (*open, at),
            // `read(2)` of a folder fails with `EISDIR`, which `ccci_fsd` answers as a general failure.
            Some(Handle::Folder { .. }) => return (GENERAL_FAILURE, Vec::new()),
            _ => return (INVALID_HANDLE, Vec::new()),
        };
        let content = disk.opens.files.get(&open).map_or(&[][..], |file| file.content.as_slice());
        let start = (*at as usize).min(content.len());
        let end = start.saturating_add(length.unwrap_or(0)).min(content.len());
        *at = (start.max(*at as usize) + (end - start)) as u64;
        (NO_ERROR, content[start..end].to_vec())
    }

    fn write<F: Files>(&mut self, disk: &mut Disk<F>, handle: Option<u32>, bytes: Option<&[u8]>) -> (i32, u32) {
        let (open, at) = match handle.and_then(|handle| self.0.get_mut(&handle)) {
            Some(Handle::File { open, at, flags }) if !is_read_only(*flags) => (*open, at),
            // Opened to be read, as every folder is.
            Some(Handle::File { .. } | Handle::Folder { .. }) => return (GENERAL_FAILURE, 0),
            _ => return (INVALID_HANDLE, 0),
        };
        let Some(file) = disk.opens.files.get_mut(&open) else {
            return (INVALID_HANDLE, 0);
        };
        let bytes = bytes.unwrap_or_default();
        let (start, end) = (*at as usize, *at as usize + bytes.len());
        if end > MOST_FILE_BYTES {
            return (GENERAL_FAILURE, 0);
        }
        if file.content.len() < end {
            file.content.resize(end, 0);
        }
        file.content[start..end].copy_from_slice(bytes);
        (*at, file.is_changed) = (end as u64, true);
        (NO_ERROR, bytes.len() as u32)
    }

    fn close<F: Files>(&mut self, disk: &mut Disk<F>, handle: u32) -> i32 {
        match self.0.remove(&handle) {
            Some(Handle::File { open, .. }) => close_open(disk, open),
            Some(_) => NO_ERROR,
            None => INVALID_HANDLE,
        }
    }

    // Lets go of every handle, whatever a close before it came to: the first failure, if any.
    fn close_all<F: Files>(&mut self, disk: &mut Disk<F>) -> i32 {
        let handles: Vec<u32> = self.0.keys().copied().collect();
        handles.into_iter().fold(NO_ERROR, |first, handle| {
            let result = self.close(disk, handle);
            if first == NO_ERROR { result } else { first }
        })
    }

    fn size<F: Files>(&self, disk: &mut Disk<F>, handle: Option<u32>) -> (i32, u32) {
        match handle.and_then(|handle| self.0.get(&handle)) {
            Some(Handle::File { open, .. }) => {
                (NO_ERROR, disk.opens.files.get(open).map_or(0, |file| file.content.len() as u32))
            }
            Some(Handle::Folder { size, .. }) => (NO_ERROR, *size),
            Some(Handle::Search(_)) => disk.blocks().map_or((GENERAL_FAILURE, 0), |(_, block)| (NO_ERROR, block)),
            None => (INVALID_HANDLE, 0),
        }
    }

    fn find_first<F: Files>(
        &mut self,
        disk: &mut Disk<F>,
        place: Result<Place, i32>,
        (attributes, mask): (u8, u8),
        most: u32,
    ) -> Vec<Vec<u8>> {
        let mut place = match place {
            Ok(place) => place,
            Err(error) => return to_unfound(error),
        };
        let Some(pattern) = place.names.pop() else {
            return to_unfound(PATH_NOT_FOUND);
        };
        if pattern.len() > NAME_MOST {
            return to_unfound(PATH_TOO_LONG);
        }
        let Some(entries) = list(disk, &place) else {
            return to_unfound(FILE_NOT_FOUND);
        };
        let mut search = Search { place, pattern, attributes, mask, entries, next: 0 };
        let Some(found) = search.next() else {
            return to_unfound(NO_MORE_FILES);
        };
        let Some(number) = self.free_handle() else {
            return to_unfound(TOO_MANY_FILES);
        };
        let reply = to_found(disk, &search.place, &found, most, number as i32);
        self.0.insert(number, Handle::Search(Box::new(search)));
        reply
    }

    fn find_next<F: Files>(&mut self, disk: &mut Disk<F>, handle: Option<u32>, most: u32) -> Vec<Vec<u8>> {
        let Some(Handle::Search(search)) = handle.and_then(|handle| self.0.get_mut(&handle)) else {
            return to_unfound(INVALID_HANDLE);
        };
        match search.next() {
            Some(found) => to_found(disk, &search.place, &found, most, NO_ERROR),
            None => to_unfound(NO_MORE_FILES),
        }
    }

    fn find_close(&mut self, handle: Option<u32>) -> i32 {
        match handle.and_then(|handle| self.0.remove(&handle)) {
            Some(_) => NO_ERROR,
            None => INVALID_HANDLE,
        }
    }

    fn compact_read<F: Files>(
        &mut self,
        disk: &mut Disk<F>,
        place: Result<Place, i32>,
        request: Option<&[u8]>,
    ) -> Vec<Vec<u8>> {
        let field = |at: usize| request.and_then(|request| le32(request, at)).unwrap_or(0);
        let (steps, flags) = (field(CMPT_STEPS_AT), field(CMPT_FLAGS_AT));
        let (offset, whence) = (field(CMPT_READ_OFFSET_AT), field(CMPT_READ_WHENCE_AT));
        let length = (field(CMPT_READ_LENGTH_AT) as usize).min(CAPACITY - CMPT_READ_HEAD);
        let (mut done, mut size, mut bytes) = (0u32, 0u32, Vec::new());
        let outcome: Result<(), i32> = (|| {
            if steps & CMPT_OPEN == 0 {
                return Err(GENERAL_FAILURE);
            }
            let handle = self.open(disk, place?, flags)? as u32;
            done |= CMPT_OPEN;
            let result: Result<(), i32> = (|| {
                if steps & CMPT_SIZE != 0 {
                    size = check(self.size(disk, Some(handle)))?;
                    done |= CMPT_SIZE;
                }
                if steps & CMPT_SEEK != 0 {
                    check((self.seek(disk, Some(handle), Some(offset), Some(whence)), ()))?;
                    done |= CMPT_SEEK;
                }
                if steps & CMPT_READ_STEP != 0 {
                    bytes = check(self.read(disk, Some(handle), Some(length)))?;
                    done |= CMPT_READ_STEP;
                }
                Ok(())
            })();
            let close = self.close(disk, handle);
            result?;
            if steps & CMPT_CLOSE == 0 {
                return Err(GENERAL_FAILURE);
            }
            check((close, ()))?;
            done |= CMPT_CLOSE;
            Ok(())
        })();
        let error = outcome.err().unwrap_or(NO_ERROR);
        let result: Vec<u8> = [done, error as u32].iter().flat_map(|word| word.to_le_bytes()).collect();
        let read = bytes.len() as u32;
        vec![result, size.to_le_bytes().to_vec(), read.to_le_bytes().to_vec(), bytes]
    }

    fn compact_write<F: Files>(
        &mut self,
        disk: &mut Disk<F>,
        place: Result<Place, i32>,
        request: Option<&[u8]>,
        bytes: Option<&[u8]>,
    ) -> Vec<Vec<u8>> {
        let field = |at: usize| request.and_then(|request| le32(request, at)).unwrap_or(0);
        let (steps, flags) = (field(CMPT_STEPS_AT), field(CMPT_FLAGS_AT));
        let (offset, whence) = (field(CMPT_WRITE_OFFSET_AT), field(CMPT_WRITE_WHENCE_AT));
        let (length, handle) = (field(CMPT_WRITE_LENGTH_AT), field(CMPT_WRITE_HANDLE_AT) as i32);
        let bytes = bytes.map(|bytes| &bytes[..(length as usize).min(bytes.len())]);
        let (mut done, mut number, mut written) = (0u32, handle, 0u32);
        let outcome: Result<(), i32> = (|| {
            if handle < 0 {
                if steps & CMPT_OPEN == 0 {
                    return Err(PARAM_ERROR);
                }
                number = self.open(disk, place?, flags)?;
                done |= CMPT_OPEN;
            }
            let handle = number as u32;
            if steps & CMPT_SEEK != 0 {
                if let Err(error) = check((self.seek(disk, Some(handle), Some(offset), Some(whence)), ())) {
                    self.close(disk, handle);
                    return Err(error);
                }
                done |= CMPT_SEEK;
            }
            if steps & CMPT_WRITE_STEP != 0 {
                match check(self.write(disk, Some(handle), bytes)) {
                    Ok(count) => written = count,
                    Err(error) => {
                        self.close(disk, handle);
                        return Err(error);
                    }
                }
                done |= CMPT_WRITE_STEP;
            }
            if steps & CMPT_CLOSE != 0 {
                check((self.close(disk, handle), ()))?;
                done |= CMPT_CLOSE;
            }
            Ok(())
        })();
        let error = outcome.err().unwrap_or(NO_ERROR);
        let result: Vec<u8> = [done, error as u32].iter().flat_map(|word| word.to_le_bytes()).collect();
        vec![result, number.to_le_bytes().to_vec(), written.to_le_bytes().to_vec()]
    }
}

fn check<T>((result, value): (i32, T)) -> Result<T, i32> {
    if result < 0 { Err(result) } else { Ok(value) }
}

// The reply to a search that found nothing, or failed: its error, an empty entry and no name.
fn to_unfound(error: i32) -> Vec<Vec<u8>> {
    vec![word(error), vec![0; ENTRY_BYTES], Vec::new()]
}

impl Search {
    fn next(&mut self) -> Option<(String, Kind)> {
        while let Some((name, kind)) = self.entries.get(self.next).cloned() {
            self.next += 1;
            let attributes = to_attributes(kind);
            if matches(self.pattern.as_bytes(), name.as_bytes())
                && attributes & self.attributes == self.attributes
                && attributes & self.mask == 0
            {
                return Some((name, kind));
            }
        }
        None
    }
}

// The reply to a search that found an entry: `result`, the entry as a `FS_DOSDirEntry` and its name.
fn to_found<F: Files>(
    disk: &mut Disk<F>,
    folder: &Place,
    (name, kind): &(String, Kind),
    most: u32,
    result: i32,
) -> Vec<Vec<u8>> {
    let place = folder.child(name);
    let (directory, own) = place.split();
    let mut entry = [0u8; ENTRY_BYTES];
    entry[ATTRIBUTES_AT] = to_attributes(*kind);
    entry[LONG_NAME_AT] = if (name.len() as u32) < most { LONG_NAME } else { 0 };
    let stat = disk.stat(&directory, &own);
    let size = match kind {
        Kind::File => to_size(disk.opens, &place, stat),
        _ => disk.blocks().map_or(0, |(_, block)| block),
    };
    if let Some((_, [modified, accessed, _])) = stat {
        // The access time as the creation's, as `ccci_fsd`'s `FS_EntryLinuxToDos` fills it.
        entry[CREATED_AT..CREATED_AT + 4].copy_from_slice(&to_dos_time(accessed).to_le_bytes());
        entry[MODIFIED_AT..MODIFIED_AT + 4].copy_from_slice(&to_dos_time(modified).to_le_bytes());
    }
    entry[SIZE_AT..SIZE_AT + 4].copy_from_slice(&size.to_le_bytes());
    vec![word(result), entry.to_vec(), to_wide(name, most)]
}

// The partition as the service reaches it, keeping the first failure's text, and the files the modem has open.
struct Disk<'a, F: Files> {
    files: &'a mut F,
    fault: &'a mut Option<String>,
    opens: &'a mut Opens,
}

impl<F: Files> Disk<'_, F> {
    fn note<T>(&mut self, result: Result<T, F::Error>) -> Option<T> {
        result.map_err(|error| self.fault.get_or_insert_with(|| format!("{error}"))).ok()
    }

    fn read(&mut self, directory: &str, name: &str) -> Option<Option<Vec<u8>>> {
        let result = self.files.read(directory, name);
        self.note(result)
    }

    fn names(&mut self, directory: &str) -> Option<Vec<String>> {
        let result = self.files.names(directory, Listed::Files);
        self.note(result)
    }

    fn folders(&mut self, directory: &str) -> Option<Vec<String>> {
        let result = self.files.names(directory, Listed::Folders);
        self.note(result)
    }

    fn put(&mut self, directory: &str, name: &str, content: &[u8]) -> Option<()> {
        let result = self.files.put(directory, name, content);
        self.note(result)
    }

    fn remove(&mut self, directory: &str, name: &str) -> Option<()> {
        let result = self.files.remove(directory, name);
        self.note(result)
    }

    fn make(&mut self, directory: &str) -> Option<()> {
        let result = self.files.make(directory);
        self.note(result)
    }

    fn discard(&mut self, directory: &str, folder: &str) -> Option<()> {
        let result = self.files.discard(directory, folder);
        self.note(result)
    }

    fn blocks(&mut self) -> Option<(u64, u32)> {
        let result = self.files.blocks();
        self.note(result)
    }

    fn stat(&mut self, directory: &str, name: &str) -> Option<(u64, [u64; 3])> {
        let result = self.files.stat(directory, name);
        self.note(result).flatten()
    }
}

// A file's size as the modem last wrote it: that of its handles' bytes where it is open, else the partition's word of it.
fn to_size(opens: &Opens, place: &Place, stat: Option<(u64, [u64; 3])>) -> u32 {
    opens.content(place).map_or(stat.map_or(0, |(size, _)| size), |content| content.len() as u64) as u32
}

fn kind<F: Files>(disk: &mut Disk<F>, place: &Place) -> Kind {
    let (directory, name) = place.split();
    if disk.folders(&directory).is_some_and(|folders| folders.contains(&name)) {
        Kind::Folder
    } else if disk.names(&directory).is_some_and(|names| names.contains(&name)) {
        Kind::File
    } else {
        Kind::Missing
    }
}

fn list<F: Files>(disk: &mut Disk<F>, place: &Place) -> Option<Vec<(String, Kind)>> {
    if kind(disk, place) != Kind::Folder {
        return None;
    }
    let directory = place.directory();
    let mut entries: Vec<(String, Kind)> =
        disk.folders(&directory)?.into_iter().map(|name| (name, Kind::Folder)).collect();
    entries.extend(disk.names(&directory)?.into_iter().map(|name| (name, Kind::File)));
    entries.sort();
    Some(entries)
}

fn read_disk<F: Files>(disk: &mut Disk<F>, place: &Place) -> Result<Vec<u8>, i32> {
    let (directory, name) = place.split();
    disk.read(&directory, &name).ok_or(GENERAL_FAILURE)?.ok_or(FILE_NOT_FOUND)
}

// A file's bytes as the modem last wrote them: those of its handles where it is open.
fn read_file<F: Files>(disk: &mut Disk<F>, place: &Place) -> Result<Vec<u8>, i32> {
    match disk.opens.content(place) {
        Some(content) => Ok(content.to_vec()),
        None => read_disk(disk, place),
    }
}

fn put<F: Files>(disk: &mut Disk<F>, place: &Place, content: &[u8]) -> Result<(), i32> {
    let (directory, name) = place.split();
    disk.put(&directory, &name, content).ok_or(GENERAL_FAILURE)?;
    disk.opens.replace(place, content);
    Ok(())
}

// Lets go of a handle on an open file: what was written goes to the file, and the last handle lets go of its bytes.
fn close_open<F: Files>(disk: &mut Disk<F>, open: u64) -> i32 {
    let Some(file) = disk.opens.files.get_mut(&open) else {
        return NO_ERROR;
    };
    file.handles = file.handles.saturating_sub(1);
    let changed = file.place.clone().filter(|_| file.is_changed).map(|place| (place, file.content.clone()));
    let result = changed
        .map_or(NO_ERROR, |(place, content)| put(disk, &place, &content).map_or_else(|error| error, |()| NO_ERROR));
    if disk.opens.files.get(&open).is_some_and(|file| file.handles == 0) {
        disk.opens.files.remove(&open);
    }
    result
}

fn make_folder<F: Files>(disk: &mut Disk<F>, place: &Place) -> i32 {
    let is_on_drive = place.names.len() == 1;
    if EMPTY_DRIVES.contains(&place.drive) && is_on_drive && kind(disk, &place.parent()) != Kind::Folder {
        disk.make(&place.drive_folder());
    }
    if place.names.is_empty() || kind(disk, place) != Kind::Missing {
        return GENERAL_FAILURE;
    }
    if kind(disk, &place.parent()) != Kind::Folder {
        return FILE_NOT_FOUND;
    }
    disk.make(&place.directory()).map_or(GENERAL_FAILURE, |()| NO_ERROR)
}

fn remove_folder<F: Files>(disk: &mut Disk<F>, place: &Place) -> i32 {
    match list(disk, place) {
        Some(entries) if entries.is_empty() => {
            discard(disk, place, Kind::Folder).map_or_else(|error| error, |()| NO_ERROR)
        }
        Some(_) => GENERAL_FAILURE,
        None if kind(disk, place) == Kind::File => GENERAL_FAILURE,
        None => FILE_NOT_FOUND,
    }
}

// What the files under a folder take, each a cluster more than whole clusters, as `ccci_fsd` counts.
fn clusters<F: Files>(disk: &mut Disk<F>, place: &Place, block: u32) -> u32 {
    let mut taken = 0;
    for (name, kind) in list(disk, place).unwrap_or_default() {
        let child = place.child(&name);
        if kind == Kind::Folder {
            taken += clusters(disk, &child, block);
            continue;
        }
        let (directory, own) = child.split();
        let stat = disk.stat(&directory, &own);
        let size = to_size(disk.opens, &child, stat);
        if size > 0 && block > 0 {
            taken += size / block + 1;
        }
    }
    taken
}

fn folder_size<F: Files>(disk: &mut Disk<F>, place: &Place, flag: u32) -> i32 {
    match kind(disk, place) {
        Kind::Missing => return FILE_NOT_FOUND,
        Kind::File => return PARAM_ERROR,
        Kind::Folder => {}
    }
    let block = disk.blocks().map_or(0, |(_, block)| block);
    let taken = clusters(disk, place, block);
    if flag == COUNT_IN_CLUSTERS { taken as i32 } else { taken.wrapping_mul(block) as i32 }
}

// How many entries under a folder the flags count, which takes their names alone.
fn count_entries<F: Files>(disk: &mut Disk<F>, place: &Place, flags: u32) -> i32 {
    let mut found = 0;
    for (name, kind) in list(disk, place).unwrap_or_default() {
        if kind != Kind::Folder {
            found += i32::from(flags & FILE_TYPE != 0);
            continue;
        }
        found += i32::from(flags & DIR_TYPE != 0);
        if flags & RECURSIVE != 0 {
            found += count_entries(disk, &place.child(&name), flags);
        }
    }
    found
}

fn count<F: Files>(disk: &mut Disk<F>, place: &Place, flags: u32) -> i32 {
    match kind(disk, place) {
        Kind::Missing => FILE_NOT_FOUND,
        Kind::File => PARAM_ERROR,
        Kind::Folder => count_entries(disk, place, flags),
    }
}

fn delete<F: Files>(disk: &mut Disk<F>, place: &Place) -> i32 {
    match kind(disk, place) {
        Kind::File => discard(disk, place, Kind::File).map_or_else(|error| error, |()| NO_ERROR),
        Kind::Folder => GENERAL_FAILURE,
        Kind::Missing => FILE_NOT_FOUND,
    }
}

fn attributes<F: Files>(disk: &mut Disk<F>, place: &Place) -> i32 {
    match kind(disk, place) {
        Kind::Missing => FILE_NOT_FOUND,
        kind => i32::from(to_attributes(kind)),
    }
}

// Copies a file or a folder with what it holds.
fn copy<F: Files>(disk: &mut Disk<F>, from: &Place, to: &Place) -> Result<(), i32> {
    match kind(disk, from) {
        Kind::File => {
            let content = read_file(disk, from)?;
            put(disk, to, &content)
        }
        Kind::Folder => {
            disk.make(&to.directory()).ok_or(GENERAL_FAILURE)?;
            for (name, _) in list(disk, from).unwrap_or_default() {
                copy(disk, &from.child(&name), &to.child(&name))?;
            }
            Ok(())
        }
        Kind::Missing => Err(FILE_NOT_FOUND),
    }
}

// Takes an entry away; a handle open on a file in it keeps the file's bytes, as one on an unlinked inode does.
fn discard<F: Files>(disk: &mut Disk<F>, place: &Place, kind: Kind) -> Result<(), i32> {
    let (directory, name) = place.split();
    let done = match kind {
        Kind::File => disk.remove(&directory, &name),
        Kind::Folder => disk.discard(&directory, &name),
        Kind::Missing => Some(()),
    };
    disk.opens.detach(place);
    done.ok_or(GENERAL_FAILURE)
}

fn rename<F: Files>(disk: &mut Disk<F>, from: &Place, to: &Place) -> i32 {
    let (from_kind, to_kind) = (kind(disk, from), kind(disk, to));
    let outcome = match (from_kind, to_kind) {
        (Kind::Missing, _) => Err(FILE_NOT_FOUND),
        _ if from == to => Ok(()),
        _ if to.names.is_empty() || kind(disk, &to.parent()) != Kind::Folder => Err(FILE_NOT_FOUND),
        (Kind::File, Kind::Folder) | (Kind::Folder, Kind::File) => Err(GENERAL_FAILURE),
        (Kind::Folder, Kind::Folder) if list(disk, to).is_some_and(|entries| !entries.is_empty()) => {
            Err(GENERAL_FAILURE)
        }
        (Kind::Folder, _) if to.is_within(from) => Err(GENERAL_FAILURE),
        _ => discard(disk, to, to_kind).and_then(|()| copy(disk, from, to)).and_then(|()| {
            disk.opens.follow(from, to);
            discard(disk, from, from_kind)
        }),
    };
    outcome.map_or_else(|error| error, |()| NO_ERROR)
}

// `ccci_fsd`'s `FS_CCCI_Move`: a destination deleted first where asked, even where the copy then fails, a copy where asked, the source never taken away.
fn move_file<F: Files>(disk: &mut Disk<F>, from: &Place, to: &Place, flags: u32) -> i32 {
    if from == to || flags & (MOVE_COPY | MOVE_KILL) == 0 || flags & !(MOVE_COPY | MOVE_KILL | MOVE_OVERWRITE) != 0 {
        return PARAM_ERROR;
    }
    if flags & MOVE_OVERWRITE != 0 {
        let result = match kind(disk, to) {
            Kind::Folder => x_delete(disk, to, FILE_TYPE | DIR_TYPE | RECURSIVE),
            Kind::File => delete(disk, to),
            Kind::Missing => NO_ERROR,
        };
        if result < 0 {
            return result;
        }
    }
    if flags & MOVE_COPY == 0 {
        return GENERAL_FAILURE;
    }
    match kind(disk, from) {
        Kind::Missing => FILE_NOT_FOUND,
        Kind::Folder => GENERAL_FAILURE,
        Kind::File
            if to.names.is_empty() || kind(disk, &to.parent()) != Kind::Folder || kind(disk, to) == Kind::Folder =>
        {
            FILE_NOT_FOUND
        }
        Kind::File => copy(disk, from, to).map_or_else(|error| error, |()| NO_ERROR),
    }
}

fn delete_within<F: Files>(disk: &mut Disk<F>, place: &Place, flags: u32) -> Result<i32, i32> {
    let mut deleted = 0;
    for (name, kind) in list(disk, place).unwrap_or_default() {
        let child = place.child(&name);
        match kind {
            Kind::Folder => {
                if flags & RECURSIVE != 0 {
                    deleted += delete_within(disk, &child, flags)?;
                }
                if flags & DIR_TYPE != 0 {
                    if list(disk, &child).is_some_and(|entries| !entries.is_empty()) {
                        return Err(GENERAL_FAILURE);
                    }
                    discard(disk, &child, Kind::Folder)?;
                    deleted += 1;
                }
            }
            _ if flags & FILE_TYPE != 0 => {
                discard(disk, &child, Kind::File)?;
                deleted += 1;
            }
            _ => {}
        }
    }
    Ok(deleted)
}

fn x_delete<F: Files>(disk: &mut Disk<F>, place: &Place, flags: u32) -> i32 {
    if flags & (FILE_TYPE | DIR_TYPE | RECURSIVE) == 0 || flags & !(FILE_TYPE | DIR_TYPE | RECURSIVE) != 0 {
        return PARAM_ERROR;
    }
    match kind(disk, place) {
        Kind::Missing => return FILE_NOT_FOUND,
        Kind::File => return PARAM_ERROR,
        Kind::Folder => {}
    }
    let deleted = match delete_within(disk, place, flags) {
        Ok(deleted) => deleted,
        Err(error) => return error,
    };
    if flags & DIR_TYPE != 0 && flags & RECURSIVE != 0 {
        if list(disk, place).is_some_and(|entries| !entries.is_empty()) {
            return GENERAL_FAILURE;
        }
        return discard(disk, place, Kind::Folder).map_or_else(|error| error, |()| deleted + 1);
    }
    deleted
}

fn disk_info<F: Files>(disk: &mut Disk<F>) -> (i32, Vec<u8>) {
    let mut info = vec![0u8; DISK_INFO_BYTES];
    let Some((total, block)) = disk.blocks() else {
        return (RESERVED_ERROR, info);
    };
    let clusters = if block == 0 { 0 } else { (total / u64::from(block)) as u32 };
    for (at, value) in [
        (BYTES_PER_SECTOR_AT, SECTOR),
        (SECTORS_PER_CLUSTER_AT, block / SECTOR),
        (TOTAL_CLUSTERS_AT, clusters),
        (FREE_CLUSTERS_AT, FREE_CLUSTERS),
    ] {
        info[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    (NO_ERROR, info)
}

fn get_drive(kind: Option<u32>, serial: Option<u32>, alternate: Option<u32>) -> i32 {
    let (kind, serial, alternate) = (kind.unwrap_or(0), serial.unwrap_or(0), alternate.unwrap_or(0));
    let kinds = DRIVE_SYSTEM | DRIVE_NORMAL | DRIVE_REMOVABLE;
    let is_invalid = kind & kinds == 0
        || kind & !kinds != 0
        || (kind != DRIVE_REMOVABLE && serial > 2)
        || (kind == DRIVE_REMOVABLE && serial > 1)
        || serial < 1
        || (alternate != NO_ALTERNATE && alternate & NO_ALTERNATE != 0)
        || (alternate != ONLY_ALTERNATE && alternate & ONLY_ALTERNATE != 0);
    if is_invalid || kind != DRIVE_SYSTEM { PARAM_ERROR } else { SYSTEM_DRIVE }
}

fn cluster_size<F: Files>(disk: &mut Disk<F>, drive: Option<u32>) -> i32 {
    let Some(drive) = drive.and_then(|drive| u8::try_from(drive).ok()).filter(|drive| DRIVES.contains(drive)) else {
        return DRIVE_NOT_FOUND;
    };
    if kind(disk, &Place { drive, names: Vec::new() }) != Kind::Folder {
        return GENERAL_FAILURE;
    }
    disk.blocks().map_or(GENERAL_FAILURE, |(_, block)| block as i32)
}

// The file as the factory's partition held it when the modem's folders were first filled.
fn restore<F: Files>(disk: &mut Disk<F>, place: &Place) -> i32 {
    let Some((directory, name)) = place.factory() else {
        return GENERAL_FAILURE;
    };
    match disk.read(&directory, &name).flatten() {
        Some(content) if kind(disk, &place.parent()) == Kind::Folder => {
            put(disk, place, &content).map_or_else(|error| error, |()| NO_ERROR)
        }
        _ => GENERAL_FAILURE,
    }
}

fn file_detail<F: Files>(disk: &mut Disk<F>, place: &Place) -> (i32, [u64; 3]) {
    if kind(disk, place) == Kind::Missing {
        return (FILE_NOT_FOUND, [0; 3]);
    }
    let (directory, name) = place.split();
    match disk.stat(&directory, &name) {
        Some((_, times)) => (NO_ERROR, times),
        None => (GENERAL_FAILURE, [0; 3]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::stream::Slots;
    use crate::cell::stream::tests::{reply_params, request};
    use crate::storage::tests::Disk as Store;

    fn wide(path: &str) -> Vec<u8> {
        path.bytes().flat_map(|byte| [byte, 0]).chain([0, 0]).collect()
    }

    fn ask(service: &mut Service, store: &mut Store, operation: u32, params: &[&[u8]]) -> Vec<Vec<u8>> {
        let fragments = request(CHANNEL, 1, operation, params);
        let mut slots = Slots::default();
        let request = fragments.iter().find_map(|fragment| slots.take(fragment, CHANNEL, CAPACITY).unwrap());
        let (params, lines, trace) = service.serve(&mut *store, &request.unwrap());
        assert!(lines.iter().all(|line| *line == trace), "no failure of the partition: {lines:?}");
        let reply = request_reply(&fragments, &params);
        reply_params(&reply).1
    }

    fn request_reply(fragments: &[Vec<u8>], params: &[Vec<u8>]) -> Vec<Vec<u8>> {
        let mut slots = Slots::default();
        let request = fragments.iter().find_map(|fragment| slots.take(fragment, CHANNEL, CAPACITY).unwrap()).unwrap();
        request.reply(&params.iter().map(Vec::as_slice).collect::<Vec<_>>())
    }

    fn result(reply: &[Vec<u8>]) -> i32 {
        i32::from_le_bytes(reply[0][..4].try_into().unwrap())
    }

    fn store() -> Store {
        let mut store = Store::with_files(&[]);
        store.folders_made.extend([
            String::from(NVRAM_DIRECTORY),
            format!("{NVRAM_DIRECTORY}/Z"),
            format!("{NVRAM_DIRECTORY}/Z/NVRAM"),
            format!("{NVRAM_DIRECTORY}/Z/NVRAM/NVD_DATA"),
            format!("{NVRAM_DIRECTORY}/X"),
        ]);
        store.files.push((format!("{NVRAM_DIRECTORY}/Z/NVRAM/NVD_DATA"), "MP0B_001".into(), b"factory".to_vec()));
        store.files.push((format!("{NVRAM_DIRECTORY}/Z/NVRAM/NVD_DATA"), "MP0C_001".into(), vec![7; 5000]));
        store
    }

    fn stored<'a>(store: &'a Store, name: &str) -> Option<&'a [u8]> {
        store.files.iter().find(|file| file.1 == name).map(|file| file.2.as_slice())
    }

    #[test]
    fn paths() {
        assert_eq!(
            locate(&wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001")),
            Ok(Place { drive: b'Z', names: vec!["NVRAM".into(), "NVD_DATA".into(), "MP0B_001".into()] })
        );
        assert_eq!(locate(&wide("Z:")), Ok(Place { drive: b'Z', names: Vec::new() }), "the drive itself");
        assert_eq!(locate(&wide("Z:\\a\\\\.\\b")).map(|place| place.names.len()), Ok(2), "as Linux reads it");
        assert_eq!(locate(&wide("A:\\x")), Err(PATH_NOT_FOUND));
        assert_eq!(locate(&wide("z:\\x")), Err(PATH_NOT_FOUND), "a drive is a capital");
        assert_eq!(locate(&wide("Z:\\..\\x")), Err(ACCESS_DENIED));
        assert_eq!(locate(&wide("Z:\\a..b")), Err(ACCESS_DENIED), "two dots anywhere");
        let place = locate(&wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001")).unwrap();
        assert_eq!(place.split(), (format!("{NVRAM_DIRECTORY}/Z/NVRAM/NVD_DATA"), "MP0B_001".into()));
        assert_eq!(place.factory(), Some((format!("{FACTORY_DIRECTORY}/Z/NVRAM/NVD_DATA"), "MP0B_001".into())));
        assert_eq!(locate(&wide("Z:")).unwrap().split(), (NVRAM_DIRECTORY.into(), "Z".into()));
    }

    #[test]
    fn open_read_write_close() {
        let (mut service, mut store) = (Service::default(), store());
        let path = wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001");
        let handle = result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()]));
        assert_eq!(handle, 1, "the first handle");
        let reply = ask(&mut service, &mut store, READ, &[&1u32.to_le_bytes(), &4u32.to_le_bytes()]);
        assert_eq!((result(&reply), &reply[1][..], &reply[2][..]), (0, &4u32.to_le_bytes()[..], &b"fact"[..]));
        let reply =
            ask(&mut service, &mut store, SEEK, &[&1u32.to_le_bytes(), &(-2i32).to_le_bytes(), &2u32.to_le_bytes()]);
        assert_eq!(result(&reply), 5, "from the end, back two");
        let [back, on] = [-1i32, 1].map(|by| {
            let (handle, by, whence) = (1u32.to_le_bytes(), by.to_le_bytes(), FILE_CURRENT.to_le_bytes());
            result(&ask(&mut service, &mut store, SEEK, &[&handle, &by, &whence]))
        });
        assert_eq!((back, on), (4, 5), "from where the handle stands, back one and on one");
        let reply = ask(&mut service, &mut store, WRITE, &[&1u32.to_le_bytes(), b"ORY!", &4u32.to_le_bytes()]);
        assert_eq!((result(&reply), &reply[1][..]), (0, &4u32.to_le_bytes()[..]));
        assert!(store.effects.iter().all(|effect| !effect.starts_with("put")), "nothing written before the close");
        let reply = ask(&mut service, &mut store, FILE_SIZE, &[&1u32.to_le_bytes()]);
        assert_eq!(&reply[1][..], &9u32.to_le_bytes(), "grown past the end");
        assert_eq!(result(&ask(&mut service, &mut store, CLOSE, &[&1u32.to_le_bytes()])), 0);
        assert_eq!(stored(&store, "MP0B_001"), Some(&b"factoORY!"[..]));
        assert_eq!(result(&ask(&mut service, &mut store, CLOSE, &[&1u32.to_le_bytes()])), INVALID_HANDLE);
        let handle = result(&ask(&mut service, &mut store, OPEN, &[&path, &READ_ONLY.to_le_bytes()]));
        let reply = ask(&mut service, &mut store, WRITE, &[&(handle as u32).to_le_bytes(), b"x", &1u32.to_le_bytes()]);
        assert_eq!(result(&reply), GENERAL_FAILURE, "a file opened to be read takes no write");
        let handle = result(&ask(&mut service, &mut store, OPEN, &[&path, &(READ_ONLY | CREATE).to_le_bytes()]));
        let reply = ask(&mut service, &mut store, WRITE, &[&(handle as u32).to_le_bytes(), b"x", &1u32.to_le_bytes()]);
        assert_eq!(result(&reply), NO_ERROR, "read-only with `CREATE` is `O_RDWR` to `ccci_fsd`");
        let hint = [9u8, 8, 7, 6, 5, 4, 3, 2];
        let reply = ask(&mut service, &mut store, OPEN_HINT, &[&path, &READ_ONLY.to_le_bytes(), &hint]);
        assert!(
            result(&reply) > 0 && reply[1] == hint,
            "an open with a hint is an open, its hint handed back as it came"
        );
    }

    #[test]
    fn handles_share_a_file() {
        let (mut service, mut store) = (Service::default(), store());
        let path = wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001");
        let [first, second] =
            [0, 0].map(|_| result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])));
        let [first, second] = [first, second].map(|handle| (handle as u32).to_le_bytes());
        ask(&mut service, &mut store, WRITE, &[&first, b"F", &1u32.to_le_bytes()]);
        let reply = ask(&mut service, &mut store, READ, &[&second, &2u32.to_le_bytes()]);
        assert_eq!(&reply[2][..], b"Fa", "a write through one handle shows through the other at once");
        ask(&mut service, &mut store, CLOSE, &[&first]);
        assert_eq!(stored(&store, "MP0B_001"), Some(&b"Factory"[..]), "and reaches the file as one closes");
        ask(&mut service, &mut store, SEEK, &[&second, &1u32.to_le_bytes(), &FILE_BEGIN.to_le_bytes()]);
        ask(&mut service, &mut store, WRITE, &[&second, b"A", &1u32.to_le_bytes()]);
        ask(&mut service, &mut store, CLOSE, &[&second]);
        assert_eq!(stored(&store, "MP0B_001"), Some(&b"FActory"[..]), "the later close keeps the earlier's write");
        let handle = (result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])) as u32).to_le_bytes();
        ask(&mut service, &mut store, WRITE, &[&handle, b"gone", &4u32.to_le_bytes()]);
        assert_eq!(result(&ask(&mut service, &mut store, DELETE, &[&path])), 0);
        assert_eq!(result(&ask(&mut service, &mut store, CLOSE, &[&handle])), 0);
        assert_eq!(stored(&store, "MP0B_001"), None, "a file deleted while open does not come back as it closes");
        let (from, to) = (wide("Z:\\NVRAM\\NVD_DATA\\MP0C_001"), wide("Z:\\NVRAM\\NVD_DATA\\MP0C_002"));
        let handle = (result(&ask(&mut service, &mut store, OPEN, &[&from, &0u32.to_le_bytes()])) as u32).to_le_bytes();
        ask(&mut service, &mut store, WRITE, &[&handle, b"new", &3u32.to_le_bytes()]);
        assert_eq!(result(&ask(&mut service, &mut store, RENAME, &[&from, &to])), 0);
        assert_eq!(stored(&store, "MP0C_002").map(|content| &content[..3]), Some(&b"new"[..]), "renamed as written");
        ask(&mut service, &mut store, WRITE, &[&handle, b"!", &1u32.to_le_bytes()]);
        ask(&mut service, &mut store, CLOSE, &[&handle]);
        assert_eq!(stored(&store, "MP0C_001"), None, "the old name stays gone");
        assert_eq!(
            stored(&store, "MP0C_002").map(|content| &content[..4]),
            Some(&b"new!"[..]),
            "a renamed file is followed"
        );
    }

    #[test]
    fn limits() {
        let (mut service, mut store) = (Service::default(), store());
        let reply = ask(
            &mut service,
            &mut store,
            OTP_READ,
            &[&0u32.to_le_bytes(), &0u32.to_le_bytes(), &u32::MAX.to_le_bytes()],
        );
        assert_eq!(
            reply[1].len(),
            CAPACITY - OTP_READ_HEAD,
            "an OTP read's bytes as the slot holds them past the result"
        );
        let path = wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001");
        let handle = (result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])) as u32).to_le_bytes();
        let far = (MOST_FILE_BYTES as u32).to_le_bytes();
        assert_eq!(
            result(&ask(&mut service, &mut store, SEEK, &[&handle, &far, &FILE_BEGIN.to_le_bytes()])),
            MOST_FILE_BYTES as i32
        );
        let reply = ask(&mut service, &mut store, WRITE, &[&handle, b"x", &1u32.to_le_bytes()]);
        assert_eq!(
            result(&reply),
            GENERAL_FAILURE,
            "a file grows no larger than the partition the modem's drives lie on"
        );
        assert_eq!(
            &ask(&mut service, &mut store, FILE_SIZE, &[&handle])[1][..],
            &7u32.to_le_bytes(),
            "and is left as it was"
        );
    }

    #[test]
    fn create_and_missing() {
        let (mut service, mut store) = (Service::default(), store());
        let missing = wide("Z:\\NVRAM\\NVD_DATA\\NEW_001");
        assert_eq!(result(&ask(&mut service, &mut store, OPEN, &[&missing, &0u32.to_le_bytes()])), FILE_NOT_FOUND);
        let handle = result(&ask(&mut service, &mut store, OPEN, &[&missing, &CREATE.to_le_bytes()]));
        assert!(handle > 0 && store.files.iter().any(|file| file.1 == "NEW_001"), "made at once, as O_CREAT does");
        let nowhere = wide("Z:\\NOWHERE\\NEW_001");
        assert_eq!(
            result(&ask(&mut service, &mut store, OPEN, &[&nowhere, &CREATE.to_le_bytes()])),
            FILE_NOT_FOUND,
            "in a folder that is not there"
        );
        let always = ask(
            &mut service,
            &mut store,
            OPEN,
            &[&wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001"), &CREATE_ALWAYS.to_le_bytes()],
        );
        assert!(result(&always) > 0);
        assert!(store.files.iter().any(|file| file.1 == "MP0B_001" && file.2.is_empty()), "emptied");
        assert_eq!(
            result(&ask(&mut service, &mut store, OPEN, &[&wide("Z:\\NVRAM"), &0u32.to_le_bytes()])),
            GENERAL_FAILURE,
            "a folder opened as a file"
        );
        assert_eq!(
            result(&ask(&mut service, &mut store, OPEN, &[&wide("A:\\x"), &0u32.to_le_bytes()])),
            PATH_NOT_FOUND
        );
    }

    #[test]
    fn folder_opened_to_be_read() {
        let (mut service, mut store) = (Service::default(), store());
        store.block = 4096;
        let folder = wide("Z:\\NVRAM");
        let handle = result(&ask(&mut service, &mut store, OPEN, &[&folder, &READ_ONLY.to_le_bytes()]));
        assert_eq!(handle, 1, "as `open(2)` opens a folder with `O_RDONLY`");
        let handle = (handle as u32).to_le_bytes();
        let reply = ask(&mut service, &mut store, READ, &[&handle, &4u32.to_le_bytes()]);
        assert_eq!((result(&reply), &reply[1][..]), (GENERAL_FAILURE, &0u32.to_le_bytes()[..]), "EISDIR");
        let reply = ask(&mut service, &mut store, WRITE, &[&handle, b"x", &1u32.to_le_bytes()]);
        assert_eq!(result(&reply), GENERAL_FAILURE);
        let reply = ask(&mut service, &mut store, FILE_SIZE, &[&handle]);
        assert_eq!((result(&reply), &reply[1][..]), (0, &4096u32.to_le_bytes()[..]), "a block");
        let reply = ask(&mut service, &mut store, SEEK, &[&handle, &(-2i32).to_le_bytes(), &FILE_END.to_le_bytes()]);
        assert_eq!(result(&reply), 4094, "from the end, back two");
        assert_eq!(result(&ask(&mut service, &mut store, CLOSE, &[&handle])), 0);
        for flags in [READ_ONLY | CREATE, READ_ONLY | CREATE_ALWAYS] {
            let reply = ask(&mut service, &mut store, OPEN, &[&folder, &flags.to_le_bytes()]);
            assert_eq!(result(&reply), GENERAL_FAILURE, "`O_RDWR` with {flags:#x}");
        }
        assert!(service.handles.0.is_empty());
    }

    #[test]
    fn handles_run_out() {
        let (mut service, mut store) = (Service::default(), store());
        let path = wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001");
        for expected in 1..=HANDLES as i32 {
            assert_eq!(result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])), expected);
        }
        assert_eq!(result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])), TOO_MANY_FILES);
        assert_eq!(result(&ask(&mut service, &mut store, CLOSE_ALL, &[&[0]])), 0);
        assert_eq!(result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])), 1, "all let go");
        assert_eq!(service.opens.files.len(), 1, "the one file open, its bytes held once");
    }

    #[test]
    fn close_all_past_failure() {
        let (mut service, mut store) = (Service::default(), store());
        let [first, second] = ["MP0B_001", "MP0C_001"].map(|name| {
            let path = wide(&format!("Z:\\NVRAM\\NVD_DATA\\{name}"));
            (result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])) as u32).to_le_bytes()
        });
        ask(&mut service, &mut store, WRITE, &[&first, b"F", &1u32.to_le_bytes()]);
        ask(&mut service, &mut store, WRITE, &[&second, b"C", &1u32.to_le_bytes()]);
        store.unwritable = Some("MP0B_001".into());
        assert_eq!(
            service.close_all(&mut store).as_deref(),
            Some("cell: writing the modem's files as it stops: MP0B_001 is not written")
        );
        assert_eq!(
            stored(&store, "MP0C_001").map(|content| content[0]),
            Some(b'C'),
            "the file behind the one that failed is written all the same"
        );
        assert!(service.handles.0.is_empty() && service.opens.files.is_empty(), "and every handle is let go of");
    }

    #[test]
    fn folders_and_attributes() {
        let (mut service, mut store) = (Service::default(), store());
        let folder = wide("Z:\\NVRAM\\NEW");
        assert_eq!(result(&ask(&mut service, &mut store, CREATE_DIR, &[&folder])), 0);
        assert_eq!(result(&ask(&mut service, &mut store, CREATE_DIR, &[&folder])), GENERAL_FAILURE, "it is there");
        assert_eq!(result(&ask(&mut service, &mut store, CREATE_DIR, &[&wide("Z:\\A\\B")])), FILE_NOT_FOUND);
        assert_eq!(result(&ask(&mut service, &mut store, ATTRIBUTES, &[&folder])), 0x10);
        assert_eq!(result(&ask(&mut service, &mut store, ATTRIBUTES, &[&wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001")])), 0);
        assert_eq!(result(&ask(&mut service, &mut store, ATTRIBUTES, &[&wide("Z:\\NONE")])), FILE_NOT_FOUND);
        assert_eq!(
            result(&ask(&mut service, &mut store, REMOVE_DIR, &[&wide("Z:\\NVRAM")])),
            GENERAL_FAILURE,
            "not empty"
        );
        assert_eq!(result(&ask(&mut service, &mut store, REMOVE_DIR, &[&folder])), 0);
        assert_eq!(result(&ask(&mut service, &mut store, REMOVE_DIR, &[&folder])), FILE_NOT_FOUND);
        assert_eq!(
            result(&ask(&mut service, &mut store, CREATE_DIR, &[&wide("U:\\LOG")])),
            0,
            "the log drive is made on use"
        );
    }

    #[test]
    fn find_entries() {
        let (mut service, mut store) = (Service::default(), store());
        store.times = [1_747_392_631, 1_747_306_231, 0];
        let most = 64u32.to_le_bytes();
        let reply =
            ask(&mut service, &mut store, FIND_FIRST, &[&wide("Z:\\NVRAM\\NVD_DATA\\MP*"), &[0], &[0x10], &most]);
        assert_eq!(result(&reply), 1);
        assert_eq!(reply[1].len(), ENTRY_BYTES);
        assert_eq!((reply[1][ATTRIBUTES_AT], reply[1][LONG_NAME_AT]), (0, LONG_NAME));
        assert_eq!(&reply[1][SIZE_AT..SIZE_AT + 4], &7u32.to_le_bytes());
        assert_eq!(
            (le32(&reply[1], MODIFIED_AT), le32(&reply[1], CREATED_AT)),
            (Some(to_dos_time(1_747_392_631)), Some(to_dos_time(1_747_306_231))),
            "its time of change, and its time of access where a DOS entry has its making"
        );
        assert!(
            !store.effects.iter().any(|effect| effect.starts_with("read")),
            "an entry's size is the partition's word of it: no file is read for it"
        );
        assert_eq!(reply[2], wide("MP0B_001"), "the name and its end in wide characters");
        let reply = ask(&mut service, &mut store, FIND_NEXT, &[&1u32.to_le_bytes(), &4u32.to_le_bytes()]);
        assert_eq!((result(&reply), &reply[2][..]), (0, &wide("MP0")[..]), "cut to the most asked, the end in it");
        assert_eq!(reply[1][LONG_NAME_AT], 0);
        let reply = ask(&mut service, &mut store, FIND_NEXT, &[&1u32.to_le_bytes(), &most]);
        assert_eq!((result(&reply), reply[2].len()), (NO_MORE_FILES, 0));
        assert_eq!(result(&ask(&mut service, &mut store, FIND_CLOSE, &[&1u32.to_le_bytes()])), 0);
        let reply = ask(&mut service, &mut store, FIND_FIRST, &[&wide("Z:\\*"), &[0x10], &[0], &most]);
        assert_eq!((result(&reply), &reply[2][..]), (1, &wide("NVRAM")[..]), "folders where asked");
        let reply = ask(&mut service, &mut store, FIND_FIRST, &[&wide("Z:\\NVRAM\\NVD_DATA\\X*"), &[0], &[0], &most]);
        assert_eq!(result(&reply), NO_MORE_FILES);
        let reply = ask(&mut service, &mut store, FIND_FIRST, &[&wide("Z:\\NONE\\*"), &[0], &[0], &most]);
        assert_eq!(result(&reply), FILE_NOT_FOUND);
    }

    #[test]
    fn traces() {
        let (mut service, mut store) = (Service::default(), store());
        let mut serve = |operation: u32, params: &[&[u8]]| {
            let fragments = request(CHANNEL, 1, operation, params);
            let request = Slots::default().take(&fragments[0], CHANNEL, CAPACITY).unwrap().unwrap();
            let (_, lines, trace) = service.serve(&mut store, &request);
            (trace, lines)
        };
        let missing = String::from("cell: file service 0x1001 Z:/NVRAM/NVD_DATA/NONE flags 0x100 answered -9");
        assert_eq!(
            serve(OPEN, &[&wide("Z:\\NVRAM\\NVD_DATA\\NONE"), &READ_ONLY.to_le_bytes()]),
            (missing.clone(), vec![missing]),
            "a failure is told in the log as well"
        );
        let compact = String::from("cell: file service 0x1022 Z:/NVRAM/NVD_DATA/NONE answered -9");
        assert_eq!(
            serve(CMPT_READ, &[&wide("Z:\\NVRAM\\NVD_DATA\\NONE"), &compact_read_request(CMPT_OPEN, 0, 4)]),
            (compact.clone(), vec![compact]),
            "a compact operation's error, not the steps it did"
        );
        let unknown = String::from("cell: file service 0x1003 7 answered -10");
        assert_eq!(serve(READ, &[&7u32.to_le_bytes(), &4u32.to_le_bytes()]), (unknown.clone(), vec![unknown]));
        assert_eq!(
            serve(LOCK_FAT, &[]),
            (String::from("cell: file service 0x1015 answered 0"), vec![]),
            "a success is not"
        );
    }

    #[test]
    fn patterns() {
        assert!(matches(b"*", b"anything") && matches(b"MP?B_*", b"MP0B_001") && matches(b"a*b*c", b"axxbyyc"));
        assert!(!matches(b"mp*", b"MP0B") && !matches(b"?", b"") && !matches(b"a", b"ab"));
    }

    #[test]
    fn sizes_counts_and_deletes() {
        let (mut service, mut store) = (Service::default(), store());
        store.block = 4096;
        let folder = wide("Z:\\NVRAM");
        let clusters = ask(&mut service, &mut store, FOLDER_SIZE, &[&folder, &COUNT_IN_CLUSTERS.to_le_bytes()]);
        assert_eq!(result(&clusters), 1 + 2, "7 bytes take one cluster, 5000 two, each a cluster more than whole ones");
        assert_eq!(result(&ask(&mut service, &mut store, FOLDER_SIZE, &[&folder, &1u32.to_le_bytes()])), 3 * 4096);
        assert!(
            !store.effects.iter().any(|effect| effect.starts_with("read")),
            "a folder's size takes each file's from the partition: no file is read for it"
        );
        let files_only = (FILE_TYPE | RECURSIVE).to_le_bytes();
        store.effects.clear();
        assert_eq!(result(&ask(&mut service, &mut store, COUNT, &[&folder, &files_only])), 2);
        assert_eq!(result(&ask(&mut service, &mut store, COUNT, &[&folder, &DIR_TYPE.to_le_bytes()])), 1);
        assert!(
            !store.effects.iter().any(|effect| effect.starts_with("read") || effect == "blocks"),
            "a count takes the names alone: no file is read for it, nor the partition's block"
        );
        let file = wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001");
        assert_eq!(result(&ask(&mut service, &mut store, COUNT, &[&file, &FILE_TYPE.to_le_bytes()])), PARAM_ERROR);
        assert_eq!(result(&ask(&mut service, &mut store, DELETE, &[&file])), 0);
        assert_eq!(result(&ask(&mut service, &mut store, DELETE, &[&file])), FILE_NOT_FOUND);
        assert_eq!(result(&ask(&mut service, &mut store, DELETE, &[&folder])), GENERAL_FAILURE);
        let everything = (FILE_TYPE | DIR_TYPE | RECURSIVE).to_le_bytes();
        assert_eq!(
            result(&ask(&mut service, &mut store, X_DELETE, &[&folder, &everything])),
            1 + 1 + 1,
            "a file, its folder, the folder"
        );
        assert_eq!(result(&ask(&mut service, &mut store, ATTRIBUTES, &[&folder])), FILE_NOT_FOUND);
        assert_eq!(
            result(&ask(&mut service, &mut store, X_DELETE, &[&wide("Z:"), &0x20u32.to_le_bytes()])),
            PARAM_ERROR
        );
    }

    #[test]
    fn rename_and_move() {
        let (mut service, mut store) = (Service::default(), store());
        let (from, to) = (wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001"), wide("Z:\\NVRAM\\NVD_DATA\\MP0B_002"));
        assert_eq!(result(&ask(&mut service, &mut store, RENAME, &[&from, &to])), 0);
        assert!(
            !store.files.iter().any(|file| file.1 == "MP0B_001") && store.files.iter().any(|file| file.1 == "MP0B_002")
        );
        assert_eq!(result(&ask(&mut service, &mut store, RENAME, &[&from, &to])), FILE_NOT_FOUND);
        let copy = (MOVE_COPY).to_le_bytes();
        assert_eq!(result(&ask(&mut service, &mut store, MOVE, &[&to, &from, &copy])), 0);
        assert!(
            store.files.iter().any(|file| file.1 == "MP0B_001") && store.files.iter().any(|file| file.1 == "MP0B_002"),
            "a copy keeps its source"
        );
        assert_eq!(
            result(&ask(&mut service, &mut store, MOVE, &[&to, &from, &MOVE_KILL.to_le_bytes()])),
            GENERAL_FAILURE,
            "as ccci_fsd, which moves nothing for a kill alone"
        );
        assert_eq!(result(&ask(&mut service, &mut store, MOVE, &[&to, &to, &copy])), PARAM_ERROR);
        assert_eq!(result(&ask(&mut service, &mut store, MOVE, &[&to, &from, &0x8u32.to_le_bytes()])), PARAM_ERROR);
    }

    #[test]
    fn move_overwrites_first() {
        let (mut service, mut store) = (Service::default(), store());
        let file = |name: &str| wide(&format!("Z:\\NVRAM\\NVD_DATA\\{name}"));
        let (small, large, none) = (file("MP0B_001"), file("MP0C_001"), file("NONE"));
        let over = (MOVE_COPY | MOVE_OVERWRITE).to_le_bytes();
        assert_eq!(result(&ask(&mut service, &mut store, MOVE, &[&small, &large, &over])), 0);
        assert_eq!(stored(&store, "MP0C_001"), Some(&b"factory"[..]), "a copy over a file takes its place");
        assert_eq!(result(&ask(&mut service, &mut store, MOVE, &[&none, &large, &over])), FILE_NOT_FOUND);
        assert_eq!(stored(&store, "MP0C_001"), None, "the destination goes first, even where the copy then fails");
        let kill = (MOVE_KILL | MOVE_OVERWRITE).to_le_bytes();
        assert_eq!(result(&ask(&mut service, &mut store, MOVE, &[&none, &small, &kill])), GENERAL_FAILURE);
        assert_eq!(stored(&store, "MP0B_001"), None, "and where nothing is copied at all");
        let (folder, inside) = (wide("Z:\\NVRAM\\OLD"), wide("Z:\\NVRAM\\OLD\\X"));
        assert_eq!(result(&ask(&mut service, &mut store, CREATE_DIR, &[&folder])), 0);
        let handle = result(&ask(&mut service, &mut store, OPEN, &[&inside, &CREATE.to_le_bytes()]));
        assert_eq!(result(&ask(&mut service, &mut store, CLOSE, &[&(handle as u32).to_le_bytes()])), 0);
        let source = wide("Z:\\NVRAM\\SOURCE");
        let handle = result(&ask(&mut service, &mut store, OPEN, &[&source, &CREATE.to_le_bytes()]));
        let handle = (handle as u32).to_le_bytes();
        ask(&mut service, &mut store, WRITE, &[&handle, b"moved", &5u32.to_le_bytes()]);
        ask(&mut service, &mut store, CLOSE, &[&handle]);
        assert_eq!(result(&ask(&mut service, &mut store, MOVE, &[&source, &folder, &over])), 0);
        assert_eq!(
            (
                result(&ask(&mut service, &mut store, ATTRIBUTES, &[&folder])),
                stored(&store, "OLD"),
                stored(&store, "X")
            ),
            (0, Some(&b"moved"[..]), None),
            "a folder in the copy's way goes with what it holds, and the file stands in its place"
        );
    }

    #[test]
    fn folder_rename_carries_open_files() {
        let (mut service, mut store) = (Service::default(), store());
        let (from, to) = (wide("Z:\\NVRAM\\NVD_DATA"), wide("Z:\\NVRAM\\MOVED"));
        let path = wide("Z:\\NVRAM\\NVD_DATA\\MP0C_001");
        let handle = (result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])) as u32).to_le_bytes();
        ask(&mut service, &mut store, WRITE, &[&handle, b"new", &3u32.to_le_bytes()]);
        assert_eq!(result(&ask(&mut service, &mut store, RENAME, &[&from, &to])), 0);
        let moved = format!("{NVRAM_DIRECTORY}/Z/NVRAM/MOVED");
        let in_folder = |store: &Store, name: &str| {
            store.files.iter().find(|file| file.0 == moved && file.1 == name).map(|file| file.2.clone())
        };
        assert_eq!(in_folder(&store, "MP0B_001").as_deref(), Some(&b"factory"[..]), "a folder goes with its files");
        assert_eq!(
            in_folder(&store, "MP0C_001").map(|content| content[..3].to_vec()).as_deref(),
            Some(&b"new"[..]),
            "one open under it as its handle last wrote it"
        );
        assert!(!store.files.iter().any(|file| file.0.ends_with("NVD_DATA")), "and nothing stays behind");
        ask(&mut service, &mut store, WRITE, &[&handle, b"!", &1u32.to_le_bytes()]);
        assert_eq!(result(&ask(&mut service, &mut store, CLOSE, &[&handle])), 0);
        assert_eq!(
            in_folder(&store, "MP0C_001").map(|content| content[..4].to_vec()).as_deref(),
            Some(&b"new!"[..]),
            "the handle follows its file to where the folder went"
        );
        assert_eq!(
            result(&ask(&mut service, &mut store, RENAME, &[&to, &wide("Z:\\NVRAM\\MOVED\\IN")])),
            GENERAL_FAILURE,
            "a folder is not renamed into itself"
        );
    }

    #[test]
    fn detail_and_open_size() {
        let (mut service, mut store) = (Service::default(), store());
        store.times = [1_747_392_631, 1_747_392_632, 1_747_392_633];
        let path = wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001");
        let reply = ask(&mut service, &mut store, FILE_DETAIL, &[&path]);
        let times: Vec<u8> = store.times.iter().flat_map(|time| time.to_le_bytes()).collect();
        assert_eq!(
            (result(&reply), &reply[1][..]),
            (0, &times[..]),
            "its times of change, access and status change, 64 bits each"
        );
        let missing = ask(&mut service, &mut store, FILE_DETAIL, &[&wide("Z:\\NVRAM\\NVD_DATA\\NONE")]);
        assert_eq!((result(&missing), &missing[1][..]), (FILE_NOT_FOUND, &[0u8; 24][..]));
        let handle = (result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])) as u32).to_le_bytes();
        ask(&mut service, &mut store, WRITE, &[&handle, b"12345678", &8u32.to_le_bytes()]);
        let most = 64u32.to_le_bytes();
        let reply =
            ask(&mut service, &mut store, FIND_FIRST, &[&wide("Z:\\NVRAM\\NVD_DATA\\MP0B*"), &[0], &[0x10], &most]);
        assert_eq!(
            (le32(&reply[1], SIZE_AT), stored(&store, "MP0B_001").map(<[u8]>::len)),
            (Some(8), Some(7)),
            "an open file's size is that of its bytes as the modem last wrote them, not yet the partition's"
        );
    }

    #[test]
    fn disk_and_drives() {
        let (mut service, mut store) = (Service::default(), store());
        (store.block, store.total) = (4096, 1 << 30);
        let reply = ask(&mut service, &mut store, DISK_INFO, &[&[b'Z', 0], &1u32.to_le_bytes()]);
        let info = &reply[1];
        let field = |at: usize| le32(info, at).unwrap();
        assert_eq!((result(&reply), info.len()), (0, DISK_INFO_BYTES));
        assert_eq!([field(0x30), field(0x34), field(0x38), field(0x40)], [512, 8, (1 << 30) / 4096, 512]);
        assert_eq!(result(&ask(&mut service, &mut store, CLUSTER_SIZE, &[&u32::from(b'Z').to_le_bytes()])), 4096);
        assert_eq!(
            result(&ask(&mut service, &mut store, CLUSTER_SIZE, &[&u32::from(b'T').to_le_bytes()])),
            GENERAL_FAILURE,
            "a drive whose folder is not there"
        );
        assert_eq!(
            result(&ask(&mut service, &mut store, CLUSTER_SIZE, &[&u32::from(b'A').to_le_bytes()])),
            DRIVE_NOT_FOUND
        );
        let mut drive = |kind: u32, serial: u32, alternate: u32| {
            result(&ask(
                &mut service,
                &mut store,
                GET_DRIVE,
                &[&kind.to_le_bytes(), &serial.to_le_bytes(), &alternate.to_le_bytes()],
            ))
        };
        assert_eq!(drive(DRIVE_SYSTEM, 1, NO_ALTERNATE), i32::from(b'Z'));
        assert_eq!(
            [drive(DRIVE_NORMAL, 1, NO_ALTERNATE), drive(DRIVE_SYSTEM, 3, NO_ALTERNATE), drive(DRIVE_SYSTEM, 1, 3)],
            [PARAM_ERROR; 3]
        );
    }

    #[test]
    fn fixed_answers() {
        let (mut service, mut store) = (Service::default(), store());
        assert_eq!(result(&ask(&mut service, &mut store, LOCK_FAT, &[])), 0);
        assert_eq!(result(&ask(&mut service, &mut store, UNLOCK_ALL, &[])), 1);
        assert_eq!(result(&ask(&mut service, &mut store, BIN_REGION, &[&1u32.to_le_bytes()])), PARAM_ERROR);
        assert_eq!(result(&ask(&mut service, &mut store, OTP_LOCK, &[&0u32.to_le_bytes()])), UNSUPPORTED);
        assert!(ask(&mut service, &mut store, SHUTDOWN, &[]).is_empty(), "a shutdown is answered with no parameter");
        assert_eq!(result(&ask(&mut service, &mut store, 0x2000, &[])), PARAM_ERROR);
        let empty = request(CHANNEL, 0, OPEN, &[b""]);
        let request = Slots::default().take(&empty[0], CHANNEL, CAPACITY).unwrap().unwrap();
        assert_eq!(service.serve(&mut store, &request).0, [word(PARAM_ERROR)], "an empty parameter");
    }

    #[test]
    fn restore_from_factory() {
        let (mut service, mut store) = (Service::default(), store());
        store.files.push((format!("{FACTORY_DIRECTORY}/Z/NVRAM/NVD_DATA"), "MP0B_001".into(), b"pristine".to_vec()));
        let path = wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001");
        assert_eq!(result(&ask(&mut service, &mut store, RESTORE, &[&path])), 0);
        assert!(
            store
                .files
                .iter()
                .any(|file| file.0.starts_with(NVRAM_DIRECTORY) && file.1 == "MP0B_001" && file.2 == b"pristine")
        );
        assert_eq!(
            result(&ask(&mut service, &mut store, RESTORE, &[&wide("Z:\\NVRAM\\NVD_DATA\\MP0C_001")])),
            GENERAL_FAILURE
        );
    }

    fn compact_read_request(steps: u32, offset: u32, length: u32) -> Vec<u8> {
        let mut request = vec![0u8; 40];
        for (at, value) in [
            (CMPT_STEPS_AT, steps),
            (CMPT_READ_OFFSET_AT, offset),
            (CMPT_READ_WHENCE_AT, FILE_BEGIN),
            (CMPT_READ_LENGTH_AT, length),
        ] {
            request[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        request
    }

    #[test]
    fn compact_operations() {
        let (mut service, mut store) = (Service::default(), store());
        let path = wide("Z:\\NVRAM\\NVD_DATA\\MP0B_001");
        let all = CMPT_OPEN | CMPT_SIZE | CMPT_SEEK | CMPT_READ_STEP | CMPT_CLOSE;
        let reply = ask(&mut service, &mut store, CMPT_READ, &[&path, &compact_read_request(all, 3, 100)]);
        assert_eq!(&reply[0][..], &[all.to_le_bytes(), 0u32.to_le_bytes()].concat()[..]);
        assert_eq!(
            (&reply[1][..], &reply[2][..], &reply[3][..]),
            (&7u32.to_le_bytes()[..], &4u32.to_le_bytes()[..], &b"tory"[..])
        );
        let missing = wide("Z:\\NVRAM\\NVD_DATA\\NONE");
        let reply = ask(&mut service, &mut store, CMPT_READ, &[&missing, &compact_read_request(all, 0, 4)]);
        assert_eq!(&reply[0][..], &[0u32.to_le_bytes(), (FILE_NOT_FOUND as u32).to_le_bytes()].concat()[..]);
        let no_close = ask(&mut service, &mut store, CMPT_READ, &[&path, &compact_read_request(CMPT_OPEN, 0, 4)]);
        assert_eq!(
            le32(&no_close[0], CMPT_ERROR_AT),
            Some(GENERAL_FAILURE as u32),
            "a read the modem leaves open is closed and refused"
        );
        let mut write = vec![0u8; 44];
        for (at, value) in [
            (CMPT_STEPS_AT, CMPT_OPEN | CMPT_SEEK | CMPT_WRITE_STEP | CMPT_CLOSE),
            (CMPT_WRITE_OFFSET_AT, 2),
            (CMPT_WRITE_WHENCE_AT, FILE_BEGIN),
            (CMPT_WRITE_LENGTH_AT, 3),
            (CMPT_WRITE_HANDLE_AT, u32::MAX),
        ] {
            write[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let reply = ask(&mut service, &mut store, CMPT_WRITE, &[&path, &write, b"NEW"]);
        assert_eq!(le32(&reply[0], CMPT_ERROR_AT), Some(0));
        assert_eq!(
            (le32(&reply[1], 0), le32(&reply[2], 0)),
            (Some(1), Some(3)),
            "the handle it had, the bytes written"
        );
        assert_eq!(stored(&store, "MP0B_001"), Some(&b"faNEWry"[..]));
        let held = result(&ask(&mut service, &mut store, OPEN, &[&path, &0u32.to_le_bytes()])) as u32;
        let on_held = |steps: u32, whence: u32| {
            let mut request = vec![0u8; 44];
            for (at, value) in [
                (CMPT_STEPS_AT, steps),
                (CMPT_WRITE_WHENCE_AT, whence),
                (CMPT_WRITE_LENGTH_AT, 2),
                (CMPT_WRITE_HANDLE_AT, held),
            ] {
                request[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            request
        };
        let steps = CMPT_SEEK | CMPT_WRITE_STEP;
        let reply = ask(&mut service, &mut store, CMPT_WRITE, &[&path, &on_held(steps, FILE_BEGIN), b"ZZ"]);
        assert_eq!(
            (le32(&reply[0], CMPT_STEPS_AT), le32(&reply[0], CMPT_ERROR_AT), le32(&reply[1], 0), le32(&reply[2], 0)),
            (Some(steps), Some(0), Some(held), Some(2)),
            "on a handle the modem holds nothing is opened, and the handle stays the modem's"
        );
        assert_eq!(stored(&store, "MP0B_001"), Some(&b"faNEWry"[..]), "what it wrote waits for its close");
        let reply = ask(&mut service, &mut store, CMPT_WRITE, &[&path, &on_held(CMPT_SEEK, 9), b"ZZ"]);
        assert_eq!(le32(&reply[0], CMPT_ERROR_AT), Some(PARAM_ERROR as u32), "a seek from nowhere");
        assert_eq!(stored(&store, "MP0B_001"), Some(&b"ZZNEWry"[..]), "a step that fails closes the handle");
        assert_eq!(result(&ask(&mut service, &mut store, CLOSE, &[&held.to_le_bytes()])), INVALID_HANDLE);
    }

    #[test]
    fn dos_times() {
        let time = to_dos_time(1_747_392_631);
        assert_eq!((time & 0x1F, time >> 5 & 0x3F, time >> 11 & 0x1F), (31 & 0x1F, 50, 10));
        assert_eq!(
            (time >> 16 & 0x1F, time >> 21 & 0x0F, time >> 25),
            (16, 16 & 0x0F, 2025 - 1980),
            "the day twice, as ccci_fsd packs it"
        );
        assert_eq!(to_wide("a/b", 10), [b'a', 0, b'\\', 0, b'b', 0, 0, 0]);
        assert_eq!(to_wide("abcd", 3), [b'a', 0, b'b', 0, 0, 0], "cut to the most with its end");
        assert!(to_wide("abc", 0).is_empty());
    }
}

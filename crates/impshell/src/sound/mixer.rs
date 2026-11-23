use crate::sys::{self, IOC_BOTH, ioc};
use r1_funcore::base::bytes::until_zero;
use r1_funcore::hw::sound::card::{Controls, DESCRIPTION_BYTES, set_control};
use std::fs::{File, OpenOptions};
use std::io;
use std::mem::zeroed;
use std::os::fd::AsRawFd;

const NODE: &str = "/dev/snd/controlC0";
const ITEM_AT: usize = 4;
const NAME_BYTES: usize = 44;

#[repr(C)]
#[derive(Clone, Copy)]
struct Id {
    number: u32,
    interface: i32,
    device: u32,
    subdevice: u32,
    name: [u8; NAME_BYTES],
    index: u32,
}

#[repr(C)]
struct List {
    offset: u32,
    space: u32,
    used: u32,
    count: u32,
    ids: *mut Id,
    reserved: [u8; 50],
}

#[repr(C, align(8))]
struct Description([u8; DESCRIPTION_BYTES]);

#[repr(C)]
struct Info {
    id: Id,
    kind: i32,
    access: u32,
    count: u32,
    owner: i32,
    value: Description,
    dimensions: [u16; 4],
    reserved: [u8; 56],
}

#[repr(C)]
struct Value {
    id: Id,
    indirect: u32,
    values: [i64; 128],
    stamp: [i64; 2],
    reserved: [u8; 112],
}

const ELEMENT_LIST: u32 = ioc(IOC_BOTH, b'U', 0x10, size_of::<List>());
const ELEMENT_INFO: u32 = ioc(IOC_BOTH, b'U', 0x11, size_of::<Info>());
const ELEMENT_WRITE: u32 = ioc(IOC_BOTH, b'U', 0x13, size_of::<Value>());

pub(super) struct Mixer {
    file: File,
    ids: Vec<Id>,
}

impl Mixer {
    pub(super) fn open() -> io::Result<Mixer> {
        let file = OpenOptions::new().read(true).write(true).open(NODE)?;
        let mut list: List = unsafe { zeroed() };
        unsafe { sys::ioctl(file.as_raw_fd(), ELEMENT_LIST, &mut list) }?;
        let mut ids: Vec<Id> = vec![unsafe { zeroed() }; list.count as usize];
        list.space = list.count;
        list.ids = ids.as_mut_ptr();
        unsafe { sys::ioctl(file.as_raw_fd(), ELEMENT_LIST, &mut list) }?;
        ids.truncate(list.used as usize);
        Ok(Mixer { file, ids })
    }

    pub(super) fn set(&self, choices: &[(&str, &str)]) -> io::Result<()> {
        choices.iter().try_for_each(|&(control, value)| set_control(self, control, value))
    }
}

impl Controls for Mixer {
    type Error = io::Error;

    fn names(&self) -> impl Iterator<Item = &[u8]> {
        self.ids.iter().map(|id| until_zero(&id.name))
    }

    fn describe(&self, control: usize, item: u32) -> io::Result<(i32, [u8; DESCRIPTION_BYTES])> {
        let mut info: Info = unsafe { zeroed() };
        info.id = self.ids[control];
        info.value.0[ITEM_AT..ITEM_AT + 4].copy_from_slice(&item.to_ne_bytes());
        unsafe { sys::ioctl(self.file.as_raw_fd(), ELEMENT_INFO, &mut info) }?;
        Ok((info.kind, info.value.0))
    }

    fn write(&self, control: usize, item: u32) -> io::Result<()> {
        let mut value: Value = unsafe { zeroed() };
        value.id = self.ids[control];
        value.values[0] = i64::from(item);
        unsafe { sys::ioctl(self.file.as_raw_fd(), ELEMENT_WRITE, &mut value) }
    }

    fn missing(words: String) -> io::Error {
        io::Error::new(io::ErrorKind::NotFound, words)
    }

    fn invalid(words: String) -> io::Error {
        sys::invalid(words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r1_funcore::hw::sound::card;

    #[test]
    fn kernel_layouts() {
        assert_eq!((size_of::<Id>(), size_of::<List>(), size_of::<Info>(), size_of::<Value>()), (64, 80, 272, 1224));
        assert_eq!((ELEMENT_LIST, ELEMENT_INFO, ELEMENT_WRITE), (0xC050_5510, 0xC110_5511, 0xC4C8_5513));
        assert_eq!(std::mem::offset_of!(Value, values), 72, "a value's numbers stand behind eight bytes of its own");
        assert_eq!(std::mem::offset_of!(Info, value), 80);
        let lists = [card::speaker(true), card::speaker(false), card::microphone(true), card::microphone(false)];
        assert!(
            lists.iter().flat_map(|controls| controls.iter()).all(|(control, _)| control.len() < NAME_BYTES),
            "every control set has a name the kernel's field holds with its zero"
        );
    }
}

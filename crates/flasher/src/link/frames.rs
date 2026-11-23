//! Host end of the frames function: a running device's recording of its screen, read on a thread of its own.

use crate::Result;
use nusb::transfer::{Bulk, Direction, In, TransferError};
use nusb::{DeviceInfo, InterfaceInfo};
use r1_funcore::domain::recording::{TRANSFER_BYTES, USB_CLASS, USB_PROTOCOL, USB_SUBCLASS};
use std::thread;
use std::time::Duration;

// Transfers kept pending: one the device's next write lands in while the one before it is handed on.
const PENDING: usize = 2;

fn is_frames(interface: &InterfaceInfo) -> bool {
    (interface.class(), interface.subclass(), interface.protocol()) == (USB_CLASS, USB_SUBCLASS, USB_PROTOCOL)
}

/// True if the firmware a device runs has the frames function.
pub fn offered(info: &DeviceInfo) -> bool {
    info.interfaces().any(is_frames)
}

/// Claims the device's frames interface and reads it on a thread of its own, handing `deliver` every piece the device sent, or the error the reading ended with, for as long as it returns true.
pub fn read(
    info: &DeviceInfo,
    mut deliver: impl FnMut(std::result::Result<Vec<u8>, TransferError>) -> bool + Send + 'static,
) -> Result<()> {
    let (device, interface, _) = super::claim(info, is_frames)?;
    let address = super::bulk_address(&interface, Direction::In)
        .ok_or("the frames interface has no bulk endpoint toward the host")?;
    let mut endpoint = interface.endpoint::<Bulk, In>(address)?;
    thread::Builder::new().name("frames".into()).spawn(move || {
        // The device and the claim stay open for as long as the thread reads.
        let _open = (device, interface);
        while endpoint.pending() < PENDING {
            let buffer = endpoint.allocate(TRANSFER_BYTES);
            endpoint.submit(buffer);
        }
        loop {
            let Some(done) = endpoint.wait_next_complete(Duration::MAX) else { continue };
            let read = done.buffer[..].to_vec();
            if !read.is_empty() && !deliver(Ok(read)) {
                return;
            }
            if let Err(e) = done.status {
                deliver(Err(e));
                return;
            }
            endpoint.submit(done.buffer);
        }
    })?;
    Ok(())
}

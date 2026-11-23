//! The MediaTek preloader's boot mode handshake, spoken right after power on.

use super::{MEDIATEK, Transport, has_ids};
use crate::Result;
use nusb::DeviceInfo;

pub(super) const PRODUCT_ID: u16 = 0x2000;
const READY: &[u8] = b"READY";
const BOOT_TO_FASTBOOT: &[u8] = b"FASTBOOT";

pub(super) fn is_preloader(device: &DeviceInfo) -> bool {
    has_ids(device, MEDIATEK, PRODUCT_ID)
}

/// Opens the preloader's serial line the way a terminal would, which raises its DTR, then answers READY with the fastboot request.
pub fn request_fastboot(info: &DeviceInfo) -> Result<()> {
    let mut line = line(info)?;
    let mut heard = Vec::new();
    while !heard.windows(READY.len()).any(|window| window == READY) {
        heard.extend(line.receive()?);
    }
    line.send(BOOT_TO_FASTBOOT)
}

// On macOS the terminal its driver makes of the line: the driver keeps the control interface, and macOS refuses `r1` the requests to it that raise DTR.
#[cfg(target_os = "macos")]
fn line(info: &DeviceInfo) -> Result<impl Transport> {
    super::serial::Line::open(info)
}

// Elsewhere the line's own interfaces.
#[cfg(not(target_os = "macos"))]
fn line(info: &DeviceInfo) -> Result<impl Transport> {
    interfaces::open(info)
}

// The serial line through its interfaces: the data interface claimed from the kernel's driver, the control interface told what a terminal tells it.
#[cfg(not(target_os = "macos"))]
mod interfaces {
    use super::super::{CDC_DATA_CLASS, Pipe};
    use crate::Result;
    use nusb::transfer::{ControlOut, ControlType, Recipient};
    use nusb::{DeviceInfo, MaybeFuture};
    use std::time::Duration;

    const CDC_CONTROL_CLASS: u8 = 0x02;
    const SET_LINE_CODING: u8 = 0x20;
    const SET_CONTROL_LINE_STATE: u8 = 0x22;
    // 115200 baud little endian, one stop bit, no parity, eight data bits.
    const LINE_CODING: [u8; 7] = [0x00, 0xC2, 0x01, 0x00, 0, 0, 8];
    const DTR_AND_RTS: u16 = 0b11;
    // USB 2.0 section 9.2.6.4: the longest a control request with a data stage may take.
    const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

    fn class_request(request: u8, value: u16, index: u16, data: &[u8]) -> ControlOut<'_> {
        ControlOut { control_type: ControlType::Class, recipient: Recipient::Interface, request, value, index, data }
    }

    pub(super) fn open(info: &DeviceInfo) -> Result<Pipe> {
        let pipe = Pipe::open(info, |interface| interface.class() == CDC_DATA_CLASS)?;
        if let Some(control) = info.interfaces().find(|interface| interface.class() == CDC_CONTROL_CLASS) {
            // The handshake tells whether the line works; a refusal of either request is not taken for its answer.
            let index = control.interface_number() as u16;
            let device = &pipe.device;
            let _ = device.control_out(class_request(SET_LINE_CODING, 0, index, &LINE_CODING), CONTROL_TIMEOUT).wait();
            let _ = device
                .control_out(class_request(SET_CONTROL_LINE_STATE, DTR_AND_RTS, index, &[]), CONTROL_TIMEOUT)
                .wait();
        }
        Ok(pipe)
    }
}

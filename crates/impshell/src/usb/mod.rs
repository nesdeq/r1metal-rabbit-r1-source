mod console;
pub mod drive;
pub mod frames;
mod fuse;

pub use console::Console;

use crate::log;
use r1_funcore::base::text::to_kernel_text;
use r1_funcore::hw::uevent::HostState;
use r1_funcore::hw::usb::{
    self, CONFIGURATION_NAME, MANUFACTURER, PRODUCT_NAME, SET, Spared, inquiry, to_hardware_fields, to_identity_fields,
    to_language_dir, to_power_text,
};
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

const GADGET: &str = "/config/usb_gadget/g1";
const CONTROLLERS: &str = "/sys/class/udc";
const CONSOLE_FUNCTION: &str = "acm.gs0";
const DRIVE_FUNCTION: &str = "mass_storage.0";
const LUN: &str = "lun.0";
// The function that shows a host a network adapter (CDC NCM), and its file that tells the name of its network interface.
const NETWORK_FUNCTION: &str = "ncm.usb0";
const NETWORK_NAME_FILE: &str = "ifname";
const HOST_STATE: &str = "/sys/class/android_usb/android0/state";
const CONFIGURATION: &str = "b.1";
const CONSOLE_LINK: &str = "f1";
const DRIVE_LINK: &str = "f2";
const FRAMES_LINK: &str = "f3";
const NETWORK_LINK: &str = "f4";
// The links of the functions the gadget works without, by `Spared`.
const SPARED_LINKS: [&str; Spared::ALL.len()] = [NETWORK_LINK, FRAMES_LINK];
const MUSB_INIT_DONE: &str = "/sys/module/musb_hdrc/parameters/kernel_init_done";
const MUSB_SAVING: &str = "/sys/devices/platform/mt_usb/saving";

/// True while a host has configured the device.
pub fn is_configured() -> bool {
    fs::read_to_string(HOST_STATE).is_ok_and(|state| HostState::parse(&state).next_hosted(false))
}

fn function_path(name: &str) -> PathBuf {
    Path::new(GADGET).join("functions").join(name)
}

fn medium_file() -> PathBuf {
    function_path(DRIVE_FUNCTION).join(LUN).join("file")
}

fn controller() -> io::Result<String> {
    let first = fs::read_dir(CONTROLLERS)?.flatten().next();
    first
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no USB device controller"))
}

fn attach(function: &Path, link: &Path) -> io::Result<()> {
    if link.symlink_metadata().is_err() {
        symlink(function, link)?;
    }
    Ok(())
}

fn add_drive(config: &Path) -> io::Result<()> {
    let storage = function_path(DRIVE_FUNCTION);
    let lun = storage.join(LUN);
    fs::create_dir_all(&storage)?;
    fs::write(lun.join("ro"), SET)?;
    fs::write(lun.join("removable"), SET)?;
    fs::write(lun.join("inquiry_string"), inquiry())?;
    attach(&storage, &config.join(DRIVE_LINK))
}

// The network function, whose two ends take their hardware addresses of the gadget's serial number where it has one.
fn add_network(config: &Path, serial: Option<&str>) -> io::Result<()> {
    let network = function_path(NETWORK_FUNCTION);
    fs::create_dir_all(&network)?;
    for (file, address) in serial.map(to_hardware_fields).into_iter().flatten() {
        fs::write(network.join(file), address)?;
    }
    attach(&network, &config.join(NETWORK_LINK))
}

/// The name of the network interface the gadget shows a host an adapter by.
pub fn host_interface() -> io::Result<String> {
    fs::read_to_string(function_path(NETWORK_FUNCTION).join(NETWORK_NAME_FILE)).map(|name| to_kernel_text(&name))
}

/// Brings the gadget up and returns the name of the controller it is bound to.
pub fn start(serial: Option<&str>) -> io::Result<String> {
    if let Err(e) = fs::write(MUSB_INIT_DONE, SET) {
        log!("usb: {MUSB_INIT_DONE}: {e}");
    }
    let udc = controller()?;
    let gadget = Path::new(GADGET);
    if fs::read_to_string(gadget.join("UDC")).is_ok_and(|text| usb::is_bound(&text)) {
        return Ok(udc);
    }

    let language = to_language_dir();
    let strings = gadget.join("strings").join(&language);
    let console = function_path(CONSOLE_FUNCTION);
    let config = gadget.join("configs").join(CONFIGURATION);
    let config_strings = config.join("strings").join(&language);
    fs::create_dir_all(&strings)?;
    for (field, value) in to_identity_fields() {
        fs::write(gadget.join(field), value)?;
    }
    fs::write(strings.join("manufacturer"), MANUFACTURER)?;
    fs::write(strings.join("product"), PRODUCT_NAME)?;
    if let Some(serial) = serial {
        fs::write(strings.join("serialnumber"), serial)?;
    }
    fs::create_dir_all(&console)?;
    fs::create_dir_all(&config_strings)?;
    fs::write(config_strings.join("configuration"), CONFIGURATION_NAME)?;
    fs::write(config.join("MaxPower"), to_power_text())?;
    attach(&console, &config.join(CONSOLE_LINK))?;
    if let Err(e) = add_drive(&config) {
        log!("usb drive: {e}");
    }
    // The frames function goes in once process 1 has readied it.
    if frames::is_ready()
        && let Err(e) = attach(&frames::function_path(), &config.join(FRAMES_LINK))
    {
        log!("usb frames: {e}");
    }
    if let Err(e) = add_network(&config, serial) {
        log!("usb network: {e}");
    }
    if let Err(e) = fs::write(MUSB_SAVING, SET) {
        log!("usb: {MUSB_SAVING}: {e}");
    }
    // What the gadget works without goes out again, in funcore's order, where the controller refuses the gadget with it.
    let mut bound = fs::write(gadget.join("UDC"), &udc);
    for function in Spared::ALL {
        let link = config.join(SPARED_LINKS[function as usize]);
        if let Err(e) = &bound
            && link.symlink_metadata().is_ok()
        {
            log!("{}", function.to_refused_line(e));
            fs::remove_file(&link)?;
            bound = fs::write(gadget.join("UDC"), &udc);
        }
    }
    bound.map(|()| udc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medium_file_path() {
        assert_eq!(medium_file(), Path::new("/config/usb_gadget/g1/functions/mass_storage.0/lun.0/file"));
    }
}

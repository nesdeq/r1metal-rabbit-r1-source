use crate::crypto::Library;
use crate::log;
use crate::storage::nvram;
use crate::storage::settings::Networks;
use crate::sync::Fetcher;
use crate::sys;
use r1_funcore::domain::device::{self, BLUETOOTH_FILE, Fact, NVRAM, Sources};
use r1_funcore::wifi::station::{self, ADDRESS_BYTES};
use r1_funcore::wifi::wmt::WIFI_FILE;
use std::io;

fn read_factory_files() -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut files = nvram::files(NVRAM)?;
    Ok((files.read(WIFI_FILE)?, files.read(BLUETOOTH_FILE)?))
}

/// The address the device shows the network chosen last, with that network's name; none while it knows no network or has no secret.
pub fn station_address(networks: &Networks) -> Option<([u8; ADDRESS_BYTES], String)> {
    let (secret, name) = (networks.secret()?, networks.latest_name()?);
    Some((station::derive_address(&Library, &secret, name.as_bytes()), name))
}

fn read_sources(
    read_station: &impl Fn() -> Option<([u8; ADDRESS_BYTES], String)>,
    read_storage: &impl Fn() -> io::Result<(u64, u64)>,
) -> Sources {
    let nvram = read_factory_files().inspect_err(|e| log!("about: the factory's addresses on {NVRAM}: {e}")).ok();
    Sources {
        cmdline: sys::read_text(sys::CMDLINE, "the kernel's command line").unwrap_or_default(),
        nvram,
        station: read_station(),
        ..sys::read_sources(read_storage)
    }
}

/// The main loop's handle of the thread that gathers the about screen's facts at each request.
pub type About = Fetcher<(), Vec<Fact>>;

impl About {
    /// Starts the thread, given its two reads of other parts: the station address with its network's name, and the room on the data partition.
    pub fn new(
        read_station: impl Fn() -> Option<([u8; ADDRESS_BYTES], String)> + Send + 'static,
        read_storage: impl Fn() -> io::Result<(u64, u64)> + Send + 'static,
    ) -> io::Result<About> {
        Fetcher::start("about", move |()| device::facts(&read_sources(&read_station, &read_storage)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Data;
    use crate::storage::settings::Keeper;

    #[test]
    fn readable_sources() {
        let networks = Keeper::start(Data::unusable(), None).unwrap().networks();
        let sources = read_sources(&|| station_address(&networks), &|| Data::unusable().room());
        assert!(
            sources.version.is_some() && sources.cpuinfo.is_some() && sources.meminfo.is_some(),
            "this computer's kernel, CPU and memory"
        );
        assert_eq!(
            (sources.nvram, sources.storage_bytes, sources.station),
            (None, None, None),
            "this computer has no NVRAM partition of an R1, there is no data partition, and no network is known"
        );
    }
}

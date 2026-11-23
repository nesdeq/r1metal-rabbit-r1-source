//! The files drivers of our kernel ask for by name while they run: which go into the boot image, and where each comes from.

use crate::release::{SUPER, image};
use crate::{Result, of};
use r1_funcore::wifi::wmt;
use r1_image::ext4::Ext4;
use r1_image::{lp, sparse::Sparse};
use std::fs::File;
use std::path::Path;

// The partition of Rabbit's super image and the directory of it that hold what Rabbit OS hands the drivers.
const PARTITION: &str = "vendor_a";
const DIRECTORY: &str = "firmware";
// The names of the amplifier's settings and of gen4m's program in that directory.
const AMPLIFIER: &str = "aw87xxx_acf.bin";
const PROGRAM: &str = "WIFI_RAM_CODE_soc1_0_1_1.bin";
// The WMT driver's settings for this board, without which it does not start; the file is ours.
const SETTINGS: (&str, &[u8]) = ("WMT_SOC.cfg", include_bytes!("../../assets/WMT_SOC.cfg"));

// Rabbit's own files, taken as they are: the amplifier's settings, every patch the radio names to the WMT driver, and gen4m's program.
fn from_stock() -> impl Iterator<Item = &'static str> {
    [AMPLIFIER, wmt::PATCH].into_iter().chain(wmt::ROM_PATCHES).chain([PROGRAM])
}

// Every file for the boot image with its name, those of Rabbit's read out of the firmware in `stock`.
pub(super) fn files(stock: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let path = stock.join(image(SUPER));
    let mut image = Sparse::open(File::open(&path).map_err(|e| of(&path, e))?)?;
    let table = lp::read(&mut image)?;
    let partition = table
        .partition(PARTITION, &mut image)
        .ok_or_else(|| format!("{}: no partition {PARTITION}", path.display()))?;
    let mut vendor = Ext4::open(partition)?;
    let mut files = vec![(SETTINGS.0.to_string(), SETTINGS.1.to_vec())];
    for name in from_stock() {
        files.push((
            name.to_string(),
            vendor.read(&format!("{DIRECTORY}/{name}")).map_err(|e| format!("{name} in {}: {e}", path.display()))?,
        ));
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wmt_settings_lines() {
        let text = std::str::from_utf8(SETTINGS.1).unwrap();
        let lines: Vec<(&str, &str)> =
            text.lines().map(|line| line.split_once('=').expect("a key and a value")).collect();
        assert_eq!(lines, [("coex_wmt_ant_mode", "1"), ("co_clock_flag", "1")]);
        assert!(text.ends_with('\n') && !text.contains(' '));
    }

    #[test]
    #[ignore = "needs the stock firmware under stock/"]
    fn stock_driver_files() {
        let stock = crate::workspace(r1_image::STOCK);
        let files = files(&stock).unwrap();
        let sizes: Vec<(&str, usize)> = files.iter().map(|(name, bytes)| (name.as_str(), bytes.len())).collect();
        assert_eq!(
            sizes,
            [
                ("WMT_SOC.cfg", 36),
                ("aw87xxx_acf.bin", 850),
                ("soc1_0_patch_mcu_1_1_hdr.bin", 26574),
                ("soc1_0_ram_mcu_1_1_hdr.bin", 105916),
                ("soc1_0_ram_bt_1_1_hdr.bin", 571053),
                ("soc1_0_ram_wifi_1_1_hdr.bin", 196216),
                ("WIFI_RAM_CODE_soc1_0_1_1.bin", 639704)
            ]
        );
    }
}

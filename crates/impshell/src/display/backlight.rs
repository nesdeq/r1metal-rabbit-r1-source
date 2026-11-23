use crate::sys;
use r1_funcore::base::text::{DIGITS, write_digits};
use r1_funcore::domain::settings::backlight_level;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::OnceLock;

const DEVICE: &str = "/sys/class/leds/lcd-backlight";

struct Led {
    brightness: File,
    max: u32,
}

static LED: OnceLock<Led> = OnceLock::new();

fn led() -> io::Result<&'static Led> {
    if let Some(led) = LED.get() {
        return Ok(led);
    }
    let max = sys::read_number(format!("{DEVICE}/max_brightness"))?;
    let brightness = OpenOptions::new().write(true).open(format!("{DEVICE}/brightness"))?;
    Ok(LED.get_or_init(|| Led { brightness, max }))
}

fn write_level(file: &File, level: u32) -> io::Result<()> {
    file.write_at(write_digits(level, &mut [0; DIGITS]), 0).map(drop)
}

/// Sets the brightness in percent of the maximum; returns the level written.
pub fn set(percent: u8) -> io::Result<u32> {
    let led = led()?;
    let level = backlight_level(led.max, percent);
    write_level(&led.brightness, level)?;
    Ok(level)
}

pub fn off() -> io::Result<()> {
    write_level(&led()?.brightness, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn level_digits() {
        let path = std::env::temp_dir().join(format!("r1-backlight-{}", std::process::id()));
        let file = File::create(&path).unwrap();
        let write = |level: u32| {
            file.set_len(0).unwrap();
            write_level(&file, level).unwrap();
            fs::read_to_string(&path).unwrap()
        };
        assert_eq!(write(u32::MAX), "4294967295");
        assert_eq!((write(128), write(0)), ("128".into(), "0".into()), "each at the start, as sysfs takes it");
        fs::remove_file(path).unwrap();
    }
}

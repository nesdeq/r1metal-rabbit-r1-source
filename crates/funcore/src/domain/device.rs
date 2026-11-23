//! What the device tells of itself, read from what the kernel and the factory left.

use crate::base::text::{parse_number, to_gib_text, to_tenths};
use crate::input::to_names_text;
use crate::logs::writer::Status;
use crate::storage::progress;
use crate::wifi::station::ADDRESS_BYTES;
use crate::wifi::wmt::WIFI_AT;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const VERSION_HEAD: &str = "Linux version ";
const KEY_HARDWARE: &str = "Hardware";
const KEY_PROCESSOR: &str = "processor";
const KEY_IMPLEMENTER: &str = "CPU implementer";
const KEY_PART: &str = "CPU part";
const ARM: u64 = 0x41;
const CORTEX_A53: u64 = 0xd03;
const KHZ_PER_TENTH_GHZ: u32 = 100_000;
const KB_PER_GIB: u64 = 1 << 20;
const TENTHS_PER_WHOLE: u64 = 10;
const MEMORY_KEY: &str = "MemTotal:";
const IMEI_KEY: &str = "androidboot.lk_imei=";
const SERIAL_KEY: &str = "androidboot.serialno=";
const IDENTITY_KEYS: [&str; 5] =
    [IMEI_KEY, SERIAL_KEY, "androidboot.lk_barcode=", "androidboot.lk_bt_mac=", "androidboot.lk_wifi_mac="];
const REDACTED: &str = "(left out)";
const ABSENT: &str = "absent";
/// The partition of MediaTek's NVRAM files.
pub const NVRAM: &str = "nvdata";
pub const BLUETOOTH_FILE: &str = "APCFG/APRDEB/BT_Addr";
const BLUETOOTH_AT: usize = 0;
pub(crate) const FIRMWARE: &str = "R1METAL";
pub(crate) const HAS_EXTRAS: bool = cfg!(any(test, feature = "extras"));
const UNKNOWN: &str = "UNKNOWN";
const NONE: &str = "NONE";
const PER_NETWORK: &str = "ONE OF ITS OWN FOR EACH NETWORK";
const MAKER_LABEL: &str = "BUILT WITH \u{2665} BY";
const MAKER: &str = "github.com/nesdeq";

/// A part of the device the debug screen has a line for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Kernel,
    Screen,
    Input,
    Data,
    Logs,
    Usb,
    Modem,
}

impl Part {
    pub(crate) const fn to_name(self) -> &'static str {
        match self {
            Part::Kernel => "KERNEL",
            Part::Screen => "SCREEN",
            Part::Input => "INPUT",
            Part::Data => "DATA",
            Part::Logs => "LOG FILES",
            Part::Usb => "USB",
            Part::Modem => "MODEM",
        }
    }

    /// Its log line at the app's start.
    pub fn to_line(self, value: Option<&str>) -> String {
        format!("{}: {}", self.to_name().to_lowercase(), value.unwrap_or(ABSENT))
    }
}

/// The debug screen's facts as the app starts, in its lines' order: what was read of the kernel, the screen, the inputs and the gadget, and the parts that tell of themselves later as they stand until then.
pub fn start_facts(
    kernel: Option<String>,
    screen: Option<String>,
    inputs: &[&str],
    usb: Option<String>,
) -> Vec<(Part, Option<String>)> {
    Vec::from([
        (Part::Kernel, kernel),
        (Part::Screen, screen),
        (Part::Input, Some(to_names_text(inputs))),
        (Part::Data, Some(progress::State::default().progress().to_word())),
        (Part::Logs, Some(Status::Undecided.to_word())),
        (Part::Usb, usb),
        (Part::Modem, None),
    ])
}

/// The kernel that runs: its release, and the build's number with its `#`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Kernel {
    release: String,
    build: Option<String>,
}

/// The CPU: its SoC, its cores and whether they are Cortex-A53s, and its clocks in tenths of a GHz, the highest first.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cpu {
    soc: Option<String>,
    cores: usize,
    is_a53: bool,
    clocks: Vec<u32>,
}

/// A fact of the about list, none inside where it could not be had.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fact {
    Firmware,
    Kernel(Option<Kernel>),
    Cpu(Option<Cpu>),
    // Tenths of a GiB.
    Ram(Option<u64>),
    // Total and free bytes of the data partition.
    Storage(Option<(u64, u64)>),
    Imei(Option<String>),
    Wifi(Option<[u8; ADDRESS_BYTES]>),
    // The address shown to the network chosen last, and that network's name.
    WifiStation(Option<([u8; ADDRESS_BYTES], String)>),
    Bluetooth(Option<[u8; ADDRESS_BYTES]>),
    Maker,
}

fn to_address_text(bytes: [u8; ADDRESS_BYTES]) -> String {
    let parts: Vec<String> = bytes.iter().map(|byte| format!("{byte:02X}")).collect();
    parts.join(":")
}

fn to_kernel_text(kernel: &Kernel) -> String {
    let text = match &kernel.build {
        Some(build) => format!("LINUX {} {build}", kernel.release),
        None => format!("LINUX {}", kernel.release),
    };
    text.to_uppercase()
}

fn to_cpu_text(cpu: &Cpu) -> String {
    let kind = if cpu.is_a53 { "CORTEX-A53" } else { "CORES" };
    let clocks: Vec<String> = cpu.clocks.iter().map(|clock| to_tenths(i64::from(*clock))).collect();
    let mut parts: Vec<String> = cpu.soc.iter().cloned().collect();
    parts.push(format!("{} {kind}", cpu.cores));
    if !clocks.is_empty() {
        parts.push(format!("{} GHZ", clocks.join("/")));
    }
    parts.join(", ").to_uppercase()
}

impl Fact {
    const fn is_identity(&self) -> bool {
        matches!(self, Fact::Imei(_) | Fact::Wifi(_) | Fact::WifiStation(_) | Fact::Bluetooth(_))
    }

    pub(crate) const fn label(&self) -> &'static str {
        match self {
            Fact::Firmware => FIRMWARE,
            Fact::Kernel(_) => "KERNEL",
            Fact::Cpu(_) => "CPU",
            Fact::Ram(_) => "RAM",
            Fact::Storage(_) => "STORAGE",
            Fact::Imei(_) => "IMEI",
            Fact::Wifi(_) => "WIFI",
            Fact::WifiStation(_) => "WIFI SHOWN",
            Fact::Bluetooth(_) => "BLUETOOTH",
            Fact::Maker => MAKER_LABEL,
        }
    }

    pub(crate) fn to_value(&self) -> String {
        let or_unknown = |text: Option<String>| text.unwrap_or_else(|| UNKNOWN.into());
        match self {
            Fact::Firmware => crate::VERSION.to_string(),
            Fact::Kernel(kernel) => or_unknown(kernel.as_ref().map(to_kernel_text)),
            Fact::Cpu(cpu) => or_unknown(cpu.as_ref().map(to_cpu_text)),
            Fact::Ram(tenths) => or_unknown(tenths.map(|tenths| format!("{} GIB", to_tenths(tenths as i64)))),
            Fact::Storage(bytes) => or_unknown(bytes.map(|(total, free)| {
                format!("{} OF {} GIB USED", to_gib_text(total.saturating_sub(free)), to_gib_text(total))
            })),
            Fact::Imei(number) => number.as_deref().unwrap_or(NONE).to_string(),
            Fact::Wifi(address) | Fact::Bluetooth(address) => or_unknown(address.map(to_address_text)),
            Fact::WifiStation(Some((address, name))) => format!("{} TO {name}", to_address_text(*address)),
            Fact::WifiStation(None) => PER_NETWORK.into(),
            Fact::Maker => MAKER.to_string(),
        }
    }
}

/// Each fact's label and value.
pub fn to_lines(facts: &[Fact]) -> Vec<(&'static str, String)> {
    facts.iter().map(|fact| (fact.label(), fact.to_value())).collect()
}

/// What the about list is made from, each fact where it could be had.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Sources {
    pub version: Option<String>,
    pub cpuinfo: Option<String>,
    /// The highest clock of each cluster in kHz.
    pub clusters_khz: Vec<u32>,
    pub meminfo: Option<String>,
    pub cmdline: String,
    /// The NVRAM files of the factory's WiFi and Bluetooth addresses.
    pub nvram: Option<NvramFiles>,
    /// Total and free bytes of the data partition.
    pub storage_bytes: Option<(u64, u64)>,
    /// The address shown to the network chosen last, and that network's name.
    pub station: Option<([u8; ADDRESS_BYTES], String)>,
}

fn read_address(content: &[u8], at: usize) -> Option<[u8; ADDRESS_BYTES]> {
    content.get(at..at + ADDRESS_BYTES)?.try_into().ok()
}

fn parse_kernel(version: &str) -> Option<Kernel> {
    let release = version.strip_prefix(VERSION_HEAD)?.split_whitespace().next()?.to_string();
    let is_build =
        |word: &&str| word.len() > 1 && word.starts_with('#') && word[1..].bytes().all(|byte| byte.is_ascii_digit());
    let build = version.split_whitespace().find(is_build).map(String::from);
    Some(Kernel { release, build })
}

fn cpuinfo_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (name, value) = line.split_once(':')?;
    (name.trim() == key).then(|| value.trim())
}

fn parse_cpu(cpuinfo: &str, clusters_khz: &[u32]) -> Cpu {
    let first_value = |key| cpuinfo.lines().find_map(|line| cpuinfo_value(line, key));
    let soc = first_value(KEY_HARDWARE).filter(|soc| !soc.is_empty()).map(String::from);
    let cores = cpuinfo.lines().filter(|line| cpuinfo_value(line, KEY_PROCESSOR).is_some()).count();
    let is_a53 = first_value(KEY_IMPLEMENTER).and_then(parse_number) == Some(ARM)
        && first_value(KEY_PART).and_then(parse_number) == Some(CORTEX_A53);
    let mut clocks: Vec<u32> =
        clusters_khz.iter().map(|khz| (khz + KHZ_PER_TENTH_GHZ / 2) / KHZ_PER_TENTH_GHZ).collect();
    clocks.sort_unstable();
    clocks.dedup();
    clocks.reverse();
    Cpu { soc, cores, is_a53, clocks }
}

fn parse_memory(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find_map(|line| line.strip_prefix(MEMORY_KEY))?;
    let kb: u64 = line.split_whitespace().next()?.parse().ok()?;
    Some((kb * TENTHS_PER_WHOLE + KB_PER_GIB / 2) / KB_PER_GIB)
}

pub(super) fn cmdline_value<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline.split_whitespace().find_map(|word| word.strip_prefix(key))
}

fn parse_imei(cmdline: &str) -> Option<&str> {
    let number = cmdline_value(cmdline, IMEI_KEY)?;
    number.bytes().any(|digit| digit != b'0').then_some(number)
}

/// The device's serial number on the kernel's command line.
pub fn parse_serial_number(cmdline: &str) -> Option<&str> {
    cmdline_value(cmdline, SERIAL_KEY)
}

fn to_log_cmdline(cmdline: &str) -> String {
    let words: Vec<String> = cmdline
        .split_whitespace()
        .map(|word| match IDENTITY_KEYS.iter().find(|key| word.starts_with(**key)) {
            Some(key) => format!("{key}{REDACTED}"),
            None => word.to_string(),
        })
        .collect();
    words.join(" ")
}

/// The log line of the kernel's command line, without the value of any key that identifies this device.
pub fn to_cmdline_line(cmdline: &str) -> String {
    format!("cmdline: {}", to_log_cmdline(cmdline))
}

type NvramFiles = (Vec<u8>, Vec<u8>);

/// Every fact, in the order of the about screen.
pub fn facts(sources: &Sources) -> Vec<Fact> {
    let nvram_address =
        |file: fn(&NvramFiles) -> &[u8], at| sources.nvram.as_ref().and_then(|files| read_address(file(files), at));
    Vec::from([
        Fact::Firmware,
        Fact::Kernel(sources.version.as_deref().and_then(parse_kernel)),
        Fact::Cpu(sources.cpuinfo.as_deref().map(|text| parse_cpu(text, &sources.clusters_khz))),
        Fact::Ram(sources.meminfo.as_deref().and_then(parse_memory)),
        Fact::Storage(sources.storage_bytes),
        Fact::Imei(parse_imei(&sources.cmdline).map(String::from)),
        Fact::Wifi(nvram_address(|files| &files.0, WIFI_AT)),
        Fact::WifiStation(sources.station.clone()),
        Fact::Bluetooth(nvram_address(|files| &files.1, BLUETOOTH_AT)),
        Fact::Maker,
    ])
}

/// Every fact but those that identify this device: what may go beyond the screen.
pub fn public_facts(sources: &Sources) -> Vec<Fact> {
    let mut all = facts(sources);
    all.retain(|fact| !fact.is_identity());
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const CORE: &str = "BogoMIPS\t: 26.00\nFeatures\t: fp asimd evtstrm aes pmull sha1 sha2 crc32 cpuid\nCPU implementer\t: 0x41\nCPU architecture: 8\nCPU variant\t: 0x0\nCPU part\t: 0xd03\nCPU revision\t: 4\n\n";

    fn kernel_text(version: &str) -> String {
        facts(&Sources { version: Some(version.into()), ..Sources::default() })[1].to_value()
    }

    fn cpu_text(cpuinfo: &str, clusters_khz: &[u32]) -> String {
        let sources =
            Sources { cpuinfo: Some(cpuinfo.into()), clusters_khz: clusters_khz.to_vec(), ..Sources::default() };
        facts(&sources)[2].to_value()
    }

    #[test]
    fn kernel_release_and_build() {
        let version = "Linux version 4.19.191-r1metal (r1metal@r1metal) (clang version 23.1.3 (https://github.com/llvm/llvm-project 0d261d1ca552c95a8f007e061c787ac7132fbcbc), LLD 23.1.3 (https://github.com/llvm/llvm-project 0d261d1ca552c95a8f007e061c787ac7132fbcbc)) #197 SMP PREEMPT Thu Oct 15 10:12:11 CEST 2026\n";
        assert_eq!(kernel_text(version), "LINUX 4.19.191-R1METAL #197");
        assert_eq!(kernel_text("Linux version 6.1.0 (a@b) SMP"), "LINUX 6.1.0");
        assert_eq!((kernel_text("").as_str(), kernel_text("version 4.19").as_str()), (UNKNOWN, UNKNOWN));
    }

    #[test]
    fn cpu_soc_cores_clocks() {
        let cpuinfo: String = (0..8).map(|core| format!("processor\t: {core}\n{CORE}")).collect::<String>()
            + "Hardware\t: MT6765\nHardware\t: MT6765\n";
        assert_eq!(cpu_text(&cpuinfo, &[2_200_000, 1_600_000]), "MT6765, 8 CORTEX-A53, 2.2/1.6 GHZ");
        assert_eq!(cpu_text(&cpuinfo, &[1_600_000, 2_200_000, 2_200_000]), "MT6765, 8 CORTEX-A53, 2.2/1.6 GHZ");
        assert_eq!(cpu_text(&cpuinfo, &[]), "MT6765, 8 CORTEX-A53");
        let other = format!("processor\t: 0\n{}", CORE.replace("0xd03", "0xd05"));
        assert_eq!(cpu_text(&other, &[1_800_000]), "1 CORES, 1.8 GHZ");
    }

    #[test]
    fn address_and_gib_text() {
        assert_eq!(Fact::Ram(Some(38)).to_value(), "3.8 GIB");
        assert_eq!(to_address_text([0x02, 0x11, 0xab, 0x0c, 0xff, 0x00]), "02:11:AB:0C:FF:00");
    }

    #[test]
    fn missing_facts_unknown() {
        let (wifi, bluetooth) =
            (vec![1, 0, 8, 0, 0x00, 0x0c, 0xe7, 0x01, 0x02, 0x03, 0xff], vec![0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
        let sources = Sources {
            version: Some("Linux version 4.19.191-r1metal (a@b) #197 SMP".into()),
            cpuinfo: Some("processor\t: 0\nHardware\t: MT6765\n".into()),
            clusters_khz: vec![2_200_000],
            meminfo: Some("MemTotal: 3956404 kB\n".into()),
            cmdline: "androidboot.lk_imei=350000000000001".into(),
            nvram: Some((wifi.clone(), bluetooth.clone())),
            storage_bytes: Some((1 << 30, 1 << 29)),
            station: Some(([0x02, 0, 0, 0, 0, 0x09], "home".into())),
        };
        let values = |sources: &Sources| facts(sources).iter().map(Fact::to_value).collect::<Vec<String>>();
        assert_eq!(
            facts(&sources).iter().map(Fact::label).collect::<Vec<_>>(),
            ["R1METAL", "KERNEL", "CPU", "RAM", "STORAGE", "IMEI", "WIFI", "WIFI SHOWN", "BLUETOOTH", MAKER_LABEL]
        );
        assert_eq!(
            values(&sources),
            [
                crate::VERSION,
                "LINUX 4.19.191-R1METAL #197",
                "MT6765, 1 CORES, 2.2 GHZ",
                "3.8 GIB",
                "0.5 OF 1.0 GIB USED",
                "350000000000001",
                "00:0C:E7:01:02:03",
                "02:00:00:00:00:09 TO home",
                "02:11:22:33:44:55",
                MAKER
            ]
        );
        let short = Sources { nvram: Some((wifi[..9].to_vec(), bluetooth)), ..Sources::default() };
        assert_eq!(
            values(&short)[1..9],
            [UNKNOWN, UNKNOWN, UNKNOWN, UNKNOWN, NONE, UNKNOWN, PER_NETWORK, "02:11:22:33:44:55"]
        );
    }

    #[test]
    fn memory_and_imei() {
        assert_eq!(parse_memory("MemTotal:        3956404 kB\nMemFree:  3806376 kB\n"), Some(38));
        assert_eq!((parse_memory("MemFree: 1 kB\n"), parse_memory("MemTotal: much kB\n")), (None, None));
        let cmdline = "console=tty0 androidboot.lk_imei=350000000000001 androidboot.lk_bt_mac=00:00:00:00:00:00";
        assert_eq!(parse_imei(cmdline), Some("350000000000001"));
        assert_eq!(
            (
                parse_imei("androidboot.lk_imei=000000000000000"),
                parse_imei("androidboot.lk_imei="),
                parse_imei("console=tty0")
            ),
            (None, None, None)
        );
    }

    #[test]
    fn cmdline_redaction() {
        let cmdline = "console=tty0 androidboot.lk_barcode=111111 androidboot.lk_imei=350000000000001 androidboot.lk_bt_mac=02:11:22:33:44:55 androidboot.lk_wifi_mac=02:11:22:33:44:56 bootopt=64S3 androidboot.serialno=R1ABC123 androidboot.slot=a";
        assert_eq!(
            to_log_cmdline(cmdline),
            "console=tty0 androidboot.lk_barcode=(left out) androidboot.lk_imei=(left out) androidboot.lk_bt_mac=(left out) androidboot.lk_wifi_mac=(left out) bootopt=64S3 androidboot.serialno=(left out) androidboot.slot=a"
        );
        assert_eq!((parse_serial_number(cmdline), parse_serial_number("console=tty0")), (Some("R1ABC123"), None));
        assert_eq!(to_cmdline_line("console=tty0"), "cmdline: console=tty0");
    }

    #[test]
    fn part_lines() {
        assert_eq!(Part::Logs.to_line(Some("on")), "log files: on");
        assert_eq!(Part::Screen.to_line(None), "screen: absent");
        let facts = start_facts(Some("4.19.191".into()), None, &["keys", "touch"], Some("musb-hdrc".into()));
        assert_eq!(
            facts.iter().map(|(part, value)| part.to_line(value.as_deref())).collect::<Vec<_>>(),
            [
                "kernel: 4.19.191",
                "screen: absent",
                "input: keys touch",
                "data: looking for the partition",
                "log files: waiting for the settings",
                "usb: musb-hdrc",
                "modem: absent"
            ],
            "the debug screen's lines in their order, the partition and the log files as they stand before their first word"
        );
    }

    #[test]
    fn public_facts_skip_identity() {
        let sources = Sources { nvram: Some((vec![0; 16], vec![0; 6])), ..Sources::default() };
        assert_eq!(
            public_facts(&sources),
            [Fact::Firmware, Fact::Kernel(None), Fact::Cpu(None), Fact::Ram(None), Fact::Storage(None), Fact::Maker]
        );
    }
}

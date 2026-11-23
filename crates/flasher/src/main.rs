//! `r1`: builds r1metal, flashes it onto a Rabbit R1 and talks to it afterwards.

mod backup;
mod build;
mod flash;
mod interrupt;
mod key;
mod link;
mod prompt;
mod record;
mod release;
mod remote;
mod visibility;

use aws_lc_rs::digest;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use r1_funcore::base::code;
use r1_funcore::domain::console::command::{SHELL_VERBS, VERBS};
use r1_funcore::logs::Logging;
use release::{Holds, Stock, checked};
use std::fs;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const DEFAULT_IMAGE: &str = "out/boot.img";
const DEFAULT_OVERLAY: &str = "out/dtbo.img";
// What marks the workspace's root.
const FLASHER_MANIFEST: &str = "crates/flasher/Cargo.toml";
// What the name of a file ends in while it is made, before it is put in place whole.
const PART: &str = ".part";
// What `r1 --help` tells after the commands, before what this computer asks of the user.
const HOW_TO_FLASH: &str = "To flash: plug the R1 in powered off, or leave it running r1metal, and run `r1 flash`.
A locked bootloader is unlocked first, which erases user data; Rabbit allows the unlock per device in Rabbit OS's settings.";

// The doc comments in `Cli` and `Task` are the help texts clap prints, so they stay doc comments.
#[derive(Parser)]
#[command(name = "r1", version, about = "Build r1metal, flash it onto a Rabbit R1 and talk to it")]
struct Cli {
    /// The R1 a command is for where several are plugged in: its port or the end of its serial number, as `r1 devices` lists them
    #[arg(long, global = true, env = "R1_DEVICE")]
    device: Option<String>,
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand, PartialEq, Debug)]
enum Task {
    /// Write r1metal to the R1 and boot it
    ///
    /// Writes the boot image and the overlay to the slot the R1 does not boot from, or with --both to both slots, and reboots into them. What r1metal keeps stays, unless the bootloader has to be unlocked first or --wipe is given: both erase user data.
    ///
    /// Once the R1 is in fastboot, before it writes anything, it asks for an OpenAI key, checked with OpenAI, unless the R1 runs r1metal and keeps one that nothing erases: neither the unlock nor --wipe; Enter flashes without one.
    Flash {
        /// The boot image [default: out/boot.img of the workspace]
        image: Option<PathBuf>,
        /// The device tree overlay [default: out/dtbo.img of the workspace]
        #[arg(long)]
        overlay: Option<PathBuf>,
        /// Write both slots, the one it boots from last
        #[arg(long)]
        both: bool,
        /// Erase user data first: everything r1metal keeps
        #[arg(long)]
        wipe: bool,
        /// How much the R1 logs from its first line until LOG is set on it: off, on or debug
        #[arg(long, default_value = "on", value_parser = logging)]
        log: Logging,
    },
    /// Put Rabbit's firmware back and boot Rabbit OS, erasing user data
    ///
    /// Writes Rabbit's images of a slot (the bootloader, the firmware the chips load, boot, dtbo and the vbmeta images) to both slots, the one the R1 boots from last, then super and the logo, and boots Rabbit OS from slot a, which Rabbit's super fills. The preloader is never written; user data is erased.
    Stock {
        /// Directory of Rabbit's unpacked firmware [default: where setupcodebase.sh unpacks it]
        #[arg(env = "R1_STOCK")]
        stock: Option<PathBuf>,
    },
    /// Copy what a running R1 keeps into a file, encrypted under the passphrase of its safe
    ///
    /// Asks for the passphrase of the R1's safe, which the R1 checks, and copies its photos, videos, notes, translations, news, conversation and memory, settings, networks, OpenAI key and Signal's account and conversations into an age file under that passphrase. What is the device's own stays out: the battery's gauge and usage, the modem's and the SIM's files, the logs and the stamp of its flash. Needs firmware with the debug shell.
    Backup {
        /// The file written, in place of one that is there
        file: PathBuf,
    },
    /// Put a backup's files on a running R1 in place of what it keeps
    ///
    /// Asks for the backup's passphrase and for that of the R1's safe, stages the backup's files on the R1 and restarts it. Once its passphrase is typed, the R1 puts them in place of its own, each directory the backup holds whole, and keeps what is the device's own. Needs firmware with the debug shell, and room on the R1 for the backup beside what it keeps.
    Restore {
        /// The file `r1 backup` wrote
        file: PathBuf,
    },
    /// Give a running R1 an OpenAI key, checked with OpenAI first
    Key {
        /// Show the key as a QR code instead, for the R1's OPENAI KEY window to scan
        #[arg(long)]
        qr: bool,
    },
    /// List the R1s plugged in: what each runs, its port and its serial number
    Devices,
    /// Build the boot image and the overlay into out/ (Linux on x86-64 or arm64; macOS in a Linux VM)
    ///
    /// Builds the kernel and the firmware and packs them with Rabbit's driver files. The first build fetches the kernel's source, Rabbit's modules, LLVM and musl into kernel/, about 17 GB; scripts/setupcodebase.sh prepares and runs it. On macOS it builds in the Lima VM setupcodebase.sh makes, started if it is not running; every path given must lie in the workspace, which the VM sees.
    Build {
        /// Directory of Rabbit's unpacked firmware [default: where setupcodebase.sh unpacks it]
        #[arg(long, env = "R1_STOCK")]
        stock: Option<PathBuf>,
        /// The boot image written [default: out/boot.img of the workspace]
        #[arg(long)]
        out: Option<PathBuf>,
        /// The overlay written [default: out/dtbo.img of the workspace]
        #[arg(long)]
        overlay: Option<PathBuf>,
        /// Build the firmware with its extras, which the menus show only then: the Pokédex
        #[arg(long)]
        enable_extras: bool,
    },
    /// Report every item that reaches further than what uses it, and every item only tests use (Linux; macOS in the Linux VM)
    Visibility,
    /// Run a console command on a running R1
    ///
    /// `r1 sh cat /proc/cmdline` runs one; `r1 sh -` runs one per line of standard input over one connection; `r1 sh clock` sets the R1's clocks to this computer's.
    Sh {
        /// A verb and its arguments, or - for one command per line of standard input
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Copy the logs a running R1 keeps to this computer, or follow its log
    Logs {
        /// The directory they go into [default: logs/device of the workspace, which git ignores]
        local: Option<PathBuf>,
        /// Print the R1's log as it is written instead, until it disconnects
        #[arg(short, long, conflicts_with = "local")]
        follow: bool,
    },
    /// Copy a file or directory of a running R1 to this computer
    Pull {
        /// The file or directory on the R1
        remote: String,
        /// Where it goes on this computer
        local: PathBuf,
    },
    /// Copy a file of this computer to a running R1, in place of what is there
    Push {
        /// The file on this computer
        local: PathBuf,
        /// Where it goes on the R1
        remote: String,
    },
    /// Record a running R1's screen into an MP4 until Ctrl-C, with this computer's ffmpeg
    Record {
        /// The MP4 written
        out: PathBuf,
    },
    /// Print the bootloader's variables and its log, leaving the R1 in fastboot
    Bootloader,
}

// The logging a word names, as `--log` takes it.
fn logging(word: &str) -> std::result::Result<Logging, String> {
    Logging::parse(word).ok_or_else(|| format!("{word}: off, on or debug"))
}

// The workspace, where what `r1` makes and takes lies unless it is told another: the directory the current one lies in that holds the flasher's sources, else the current one. Found where `r1` runs, so that a build names no directory of the computer that built it.
fn workspace_root() -> PathBuf {
    let here = std::env::current_dir().unwrap_or_default();
    here.ancestors().find(|dir| dir.join(FLASHER_MANIFEST).is_file()).unwrap_or(&here).to_path_buf()
}

// A place of the workspace.
fn workspace(relative: impl AsRef<Path>) -> PathBuf {
    workspace_root().join(relative)
}

// The path given, as the shell has it, or the workspace's place.
fn given_or(given: Option<PathBuf>, default: &str) -> PathBuf {
    given.unwrap_or_else(|| workspace(default))
}

// Where a file is made before it is put in place whole: its name with `PART` after it.
fn part(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(PART);
    PathBuf::from(name)
}

// What went wrong with a file, told with its path.
fn of(path: &Path, wrong: impl std::fmt::Display) -> Box<dyn std::error::Error> {
    format!("{}: {wrong}", path.display()).into()
}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|e| of(path, e))
}

// The SHA-256 of what a sum took in, as `sha256sum` prints it.
fn to_hex(sum: digest::Context) -> String {
    code::hex(sum.finish().as_ref())
}

fn run(task: Task, choice: &link::Choice) -> Result<()> {
    let console = || remote::running_console(choice);
    match task {
        Task::Flash { image, overlay, both, wipe, log } => {
            // What is flashed is held to be a boot image and an overlay before anything is asked.
            let (image, overlay) = (given_or(image, DEFAULT_IMAGE), given_or(overlay, DEFAULT_OVERLAY));
            let unflashed = build::Unflashed::open(&read(&image)?).map_err(|e| of(&image, e))?;
            let trees = checked(&overlay, Holds::Overlay)?;
            flash::flash(choice, unflashed, &trees, both, wipe, log)
        }
        Task::Stock { stock } => {
            let mut stock = Stock::open(&given_or(stock, r1_image::STOCK))?;
            flash::stock(choice, &mut stock)
        }
        Task::Backup { file } => backup::backup(choice, &file),
        Task::Restore { file } => backup::restore(choice, &file),
        Task::Key { qr } => key::set(choice, qr),
        Task::Devices => link::devices(),
        Task::Build { stock, out, overlay, enable_extras } => build::build(
            &given_or(stock, r1_image::STOCK),
            &given_or(out, DEFAULT_IMAGE),
            &given_or(overlay, DEFAULT_OVERLAY),
            enable_extras,
        ),
        Task::Visibility => visibility::check_workspace(),
        Task::Sh { command } if command == [remote::BATCH_ARGUMENT] => remote::run_batch(&mut console()?),
        Task::Sh { command } => {
            let command = remote::command(&command.join(" "))?;
            remote::send_or_run(&mut console()?, &command)
        }
        Task::Logs { follow: true, .. } => console()?.follow(remote::show),
        Task::Logs { local, .. } => {
            remote::pull(&mut console()?, r1_funcore::logs::ON_DEVICE, &given_or(local, remote::DEFAULT_LOGS))
        }
        Task::Pull { remote, local } => remote::pull(&mut console()?, &remote, &local),
        Task::Push { local, remote } => remote::push(&mut console()?, &local, &remote),
        Task::Record { out } => record::record(choice, &out),
        Task::Bootloader => {
            let mut bootloader = flash::reach_fastboot(choice)?;
            bootloader.variables()?.iter().for_each(|line| println!("{line}"));
            println!();
            bootloader.oem("dump_pllk_log")?.iter().for_each(|line| println!("{line}"));
            Ok(())
        }
    }
}

// What `r1 --help` tells after the commands: how a flash begins, and what this computer asks of the user, where it asks anything.
fn after_help() -> String {
    match link::HOST_ASKS {
        Some(asks) => format!("{HOW_TO_FLASH}\n\n{asks}"),
        None => HOW_TO_FLASH.to_string(),
    }
}

// What `r1 sh --help` tells after its arguments: the verbs, and where each is told of.
fn verbs_help() -> String {
    let (verbs, debug) = (VERBS.join(", "), SHELL_VERBS.join(", "));
    format!(
        "Verbs: {verbs}.\nWith the debug shell, which a build has unless told otherwise: {debug}.\ndocs/firmware/console.md tells what each does."
    )
}

fn main() {
    let help = Cli::command().after_help(after_help()).mut_subcommand("sh", |sh| sh.after_long_help(verbs_help()));
    let matches = help.get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    if let Err(e) = run(cli.command, &link::Choice::new(cli.device)) {
        eprintln!("r1: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The R1 named and the command of a command line, its words parted by blanks; none for one `r1` refuses.
    pub(crate) fn parsed(line: &str) -> Option<(Option<String>, Task)> {
        Cli::try_parse_from(line.split(' ')).ok().map(|cli| (cli.device, cli.command))
    }

    #[test]
    fn command_lines() {
        let flash = |log| Task::Flash { image: None, overlay: None, both: false, wipe: false, log };
        let logs = |follow| Task::Logs { local: None, follow };
        let pull = Task::Pull { remote: "/dev/mmcblk0p6".into(), local: "nvdata.img".into() };
        let push = Task::Push { local: "loops".into(), remote: "/loops".into() };
        let record = Task::Record { out: "demo.mp4".into() };
        let lines = [
            ("r1 --device 5-4.2.2 pull /dev/mmcblk0p6 nvdata.img", Some((Some("5-4.2.2".into()), pull))),
            ("r1 --device 6789 push loops /loops", Some((Some("6789".into()), push))),
            ("r1 --device 5-4.2.3 record demo.mp4", Some((Some("5-4.2.3".into()), record))),
            ("r1 logs", Some((None, logs(false)))),
            ("r1 logs -f", Some((None, logs(true)))),
            ("r1 logs --follow", Some((None, logs(true)))),
            ("r1 logs -f here", None),
            ("r1 flash", Some((None, flash(Logging::On)))),
            ("r1 flash --log off", Some((None, flash(Logging::Off)))),
            ("r1 flash --log debug", Some((None, flash(Logging::Debug)))),
            ("r1 flash --log loud", None),
            ("r1 backup r1.age", Some((None, Task::Backup { file: "r1.age".into() }))),
            ("r1 restore r1.age", Some((None, Task::Restore { file: "r1.age".into() }))),
            ("r1 backup", None),
            ("r1 restore", None),
            ("r1 restore --stock stock", None),
            ("r1 stock", Some((None, Task::Stock { stock: None }))),
            ("r1 stock elsewhere", Some((None, Task::Stock { stock: Some("elsewhere".into()) }))),
        ];
        for (line, told) in lines {
            assert_eq!(parsed(line), told, "{line}");
        }
    }

    #[test]
    fn default_paths() {
        let Some((_, Task::Flash { image, overlay, .. })) = parsed("r1 flash elsewhere/boot.img") else {
            panic!("not a flash")
        };
        assert_eq!(given_or(image, DEFAULT_IMAGE), Path::new("elsewhere/boot.img"), "as the shell has it");
        assert_eq!(given_or(overlay, DEFAULT_OVERLAY), workspace(DEFAULT_OVERLAY));
        let Some((_, Task::Logs { local, follow: false })) = parsed("r1 logs") else {
            panic!("not a copy of the logs")
        };
        assert_eq!(given_or(local, remote::DEFAULT_LOGS), workspace("logs/device"), "where git ignores them");
        assert_eq!(part(Path::new("out/boot.img")), Path::new("out/boot.img.part"));
        assert!(workspace_root().join("Cargo.lock").is_file(), "the workspace, named without a way up through it");
        assert!(!workspace_root().to_string_lossy().contains(".."));
    }
}

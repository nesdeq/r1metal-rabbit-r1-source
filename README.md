# r1metal: the source

The source of [r1metal](https://github.com/nesdeq/r1metal-rabbit-r1), custom firmware for the Rabbit R1.

**To install r1metal you need nothing from here.** The images, the `r1` flasher for macOS and Linux, the install steps and the owner's manual are in [r1metal-rabbit-r1](https://github.com/nesdeq/r1metal-rabbit-r1), on its [release page](https://github.com/nesdeq/r1metal-rabbit-r1/releases/latest). This repository holds what they are built from and is updated with each release; `Cargo.toml` names the version.

## Start here

**This is a LARGE codebase:** 262,000 lines of Rust in 843 files with 2,900 tests, 265 kernel patches and 32 pages of documents. It stays workable because all of it follows two rules, without exception.

- **Functional core, imperative shell.** `funcore` makes every decision: it takes values and returns values, has no dependencies, no I/O, no threads and no clock, and is tested on the host. `impshell` performs the effects, system calls, threads, files and sockets, and decides nothing. For firmware this is the best design pattern there is, hands down: every decision is deterministic and tested without a device, the code that touches hardware is thin enough to read whole, and a bug is either a wrong decision, which a host test reproduces, or a wrong effect, which is one call.
- **Separation of concerns, across both layers.** One module, one concern, named for it. One concept, one implementation, in the lowest layer that owns it. A change extends a concept's one home and never builds a second beside it.

**Read these two pages in full before you read or change anything else, and point your coding agent at them first:**

1. **[docs/overview.md](docs/overview.md)**: the index of every document and the map of the source tree, path by path.
2. **[docs/architecture.md](docs/architecture.md)**: the law between the kernel, impshell and funcore, the rules every change follows, and the one home of each concept.

An agent that starts anywhere else writes a second copy of something that is already there. Tell it to read both pages first, whole, and to find the concept's home in `architecture.md` before it writes a line.

## What it is

r1metal replaces Android. Linux 4.19, built from Rabbit's published source with our patches, provides the drivers; one static Rust binary is `/init` and the entire userspace.

| Crate | Role |
|---|---|
| `crates/funcore` | the functional core: drawing, the interface, protocols, codecs, file formats, register sequences |
| `crates/impshell` | the imperative shell: `/init`, process 1 and the app, one thread per hardware part |
| `crates/flasher` | `r1`: build, flash, Rabbit's firmware put back, the console, backup and restore |
| `crates/image` | the boot image's formats, and the readers of Rabbit's images |
| `kernel/` | the kernel's patches and configuration |

Design priorities:

- **60 frames per second, always.** Anything that moves is redrawn at every display refresh; a missed refresh is a bug.
- **Every milliampere counts.** A part is powered only while something needs it. In sleep, only what a button press needs stays on, and the wake alarm fires only for the uses in one short list ([Sleep and clock](docs/firmware/sleep.md#the-alarms-list)).
- **Our own code down to the registers.** This repository drives the image sensor, raw capture, both hardware encoders, the hardware decoder and the radio bring-up, written from Rabbit's kernel source and from analysis of the stock libraries. The only closed binaries are the radio chip's firmware and the amplifier's profiles, which the build extracts from Rabbit's own image, and the LTE modem's firmware, which the bootloader loads from Rabbit's own partition.

## Features

- **Interface**: six themes over one screen tree, the robot, brightness, volume and the level of the interface's own sounds, themes and colors, clock, a touch keyboard, the power, about and debug screens ([Interface](docs/app/interface.md)).
- **Conversation**: one endless conversation, typed or spoken while the button is held. Replies use web search and a memory of the owner, open screens and set alarms, reminders and timers on request, and are spoken in one voice ([Chat](docs/app/chat.md)).
- **Bell**: any number of alarms, reminders and timers, each with a text of what it is for, set and cancelled by a request said on the bell's own list at home or by the conversation; a ring wakes a sleeping device and says what is due ([The bell](docs/app/interface.md#the-bell)).
- **Camera**: 60 fps viewfinders, 2448x2448 photos merged from bursts of two to eight frames, 720p60 H.264 video with sound, every photo redrawn as pixel art; rolls of photos and videos, with videos played through the hardware decoder at 60 fps with sound ([Camera](docs/app/camera.md)).
- **Audio**: voice notes recorded, transcribed, summarized and saved as Markdown and PDF; a live translator between two languages; text read aloud; our own AAC encoder and decoder ([Audio](docs/app/audio.md)).
- **News**: a daily edition of the rundowns the owner keeps, news and finance from 47 feeds on a new device; a request said on the rundowns' list or to the conversation adds a rundown on any subject, its feeds searched by the language model and read by the device before it is kept, or changes, switches or deletes one ([News](docs/app/news.md)).
- **Web**: a browser of text for the voice: a site, an address, a topic, a link's number or "back", said or typed, brings a page read out of its HTML on the device, its links numbered, or one the language model writes with its web search where the device cannot; the page is read aloud at a touch, and nothing is stored ([Interface](docs/app/interface.md#web)).
- **Signal**: the R1 linked to the owner's Signal account by a QR code, its conversations, one-to-one and in groups, read, typed and said, with their photos, voice notes and reactions, the owner's photos, videos and notes sent to them, and a contact's in auto mode answered by the language model, each reply marked; asleep, what waits is fetched as often as its settings say, and a message that comes plays the notice ([Signal](docs/app/signal.md)).
- **Calls**: Signal's audio and video calls with the owner's contacts and groups, made and taken on the R1 as a linked device, one-to-one through ICE and Signal's relays and in groups through Signal's calling server; our own Opus and VP8 encoders and decoders, echo canceller and jitter buffer ([Calls](docs/app/calls.md)).
- **Pokédex**: looks up a Pokémon name, said or typed, on PokéAPI in English, German, French, Spanish or Italian, and stores nothing; shown in firmware built with `r1 build --enable-extras` ([Interface](docs/app/interface.md#pokédex)).
- **OpenAI key**: pasted during `r1 flash`, or scanned in the key window over home from the QR code that `r1 key --qr` shows; checked with OpenAI and stored ([OpenAI](docs/app/openai.md#the-key)).
- **Passphrase**: chosen at the first start, typed at every start and changed in the settings, it opens the safe that encrypts everything the device keeps of its owner: the keys, passwords, Signal and settings, and the photos, videos, notes, translations, news and the conversation; a change of it seals the same key anew, with no cable and no restart ([Storage](docs/firmware/storage.md#the-safe)).
- **System**: the data partition, shown to a computer as a read-only USB drive only while `DRIVE` under `SETTINGS`, `USB DRIVE` is switched on, off on a new device ([Storage](docs/firmware/storage.md#usb-drive)); saved settings; our own battery gauge and cutoff; Rabbit's thermal policy; sleep and wake; time from SNTP and the UTC offset from a location service; logs in one folder per day, or none; WPA2 and WPA3 with IPv4 and IPv6; mobile data over the LTE modem where WiFi is not to be had, on the network's own APN, the SIM's PIN typed on the device, and a network of IPv6 alone reaching IPv4 through its NAT64 ([Network](docs/firmware/network.md#mobile-data)); the R1 as a USB modem that lends a computer on the cable the SIM's connection through the firmware's own address translation, through a WireGuard tunnel alone where `VPN` is on, with nothing leaving beside a tunnel that is down ([Network](docs/firmware/network.md#router)); DNS over TLS; HTTP and WebSocket under TLS only; screen recording over USB ([r1metal documents](docs/overview.md)).

## Setup

Building needs Linux on x86-64 or arm64, for which LLVM publishes the prebuilt release the build uses, or macOS with [Lima](https://lima-vm.io) (`brew install lima`), which builds in a Linux VM. Every computer builds with the same releases of LLVM and of Rust (`rust-toolchain.toml`), and the build records no path, user, host or time of the computer it runs on, so that a commit builds the same bytes on every computer with the same CPU, a Mac's VM among the arm64 ones. A build on x86-64 and one on arm64 differ: cargo's hash of a crate with a build script, as `libc` and `aws-lc-sys` have, takes in the CPU of the computer that builds, and the hashes reach the binary. `r1`'s other commands need no setup.

On macOS, `scripts/setupcodebase.sh` first makes the Lima VM `r1metal` from `lima/r1metal.yaml` and starts it: Debian on the Mac's CPU, with all its cores and half its memory, which installs the host tools at its first boot and sees the workspace at its path on the Mac. The VM keeps `kernel/src`, `kernel/vendor`, `kernel/toolchain`, `kernel/out` and `target/` on its own disk, at their places in the workspace, because the kernel's source holds names that differ only in case, which macOS's file system takes for one. The steps below then run in the VM. `r1 build` on macOS builds in the VM too, starting it if it is not running; every path it is given must lie in the workspace. A VM made for another clone stops the setup; `limactl delete r1metal` removes it.

`scripts/setupcodebase.sh` takes a fresh clone to a built `out/boot.img` and `out/dtbo.img`, skipping steps already done:

1. Checks for the host tools: `git`, `curl`, `tar`, `unzip`, `zstd`, `make`, `gcc`, `sha256sum`, `bc`, `bison`, `flex`, `perl`, `gzip`, `awk`, `cmp`, `install`, `sudo`, `udevadm`, `rustup`, `cargo` and the OpenSSL headers. On Debian 13, as in the VM, they come with `sudo apt install git curl ca-certificates tar unzip zstd make gcc libc6-dev bc bison flex perl gzip gawk diffutils libssl-dev rustup`.
2. Installs the release of Rust that `rust-toolchain.toml` names, with its target `aarch64-unknown-linux-musl`.
3. Downloads `rabbit_OS_v0.8.293.zip` from [Rabbit's firmware releases](https://github.com/rabbit-hmi-oss/firmware/releases) into `stock/v0.8.293/`, verifies its SHA-256 and unpacks it: `boot.img` (kernel header and device tree), `dtbo.img` (device tree overlay), `super.img` (the firmware files drivers load), and the rest of Rabbit's firmware, which `stock` writes back. The marker `stock/v0.8.293/r1metal-unpacked` records a complete unpack, so this step runs once.
4. Installs `udev/70-r1metal.rules` into `/etc/udev/rules.d/` with `sudo`, so a normal user can reach the R1 in each of its USB modes; the VM, which has no USB, skips this.
5. Runs `r1 build`. The first build downloads the kernel source, Rabbit's kernel modules, the toolchain and the C library source ([Kernel](docs/hardware/kernel.md)). `kernel/` then takes about 17 GB.

`scripts/backupdependencies.sh` copies everything the setup downloads into `dependencies/` (untracked), with checksums in `SHA256SUMS`: Rabbit's firmware archive, both kernel repositories as git bundles (rebuilt after the repository fetches), the toolchain archive, the C library source, and the workspace's crates as `cargo vendor` output (repacked when `Cargo.lock` changes). `setupcodebase.sh` takes everything but the crates from `dependencies/` before it uses the network, after verifying each checksum. The toolchain archive is the one for the CPU that builds. On macOS, `backupdependencies.sh` runs in the VM. The crates are a fallback in case crates.io loses one. `r1 build` never reads `dependencies/`.

## Use

`cargo r1 COMMAND` runs `r1` from the workspace (an alias in `.cargo/config.toml`); `cargo install --path crates/flasher` installs it as `r1`. `r1 --help` lists the commands and `r1 COMMAND --help` describes one in full. `r1` built for macOS and Linux, with the images it flashes, is on the [release page](https://github.com/nesdeq/r1metal-rabbit-r1/releases/latest).

```
cargo r1 flash                  # write r1metal to the slot the R1 is not booted from, then boot it
cargo r1 flash --both --wipe    # factory clean: erase user data, write both slots
cargo r1 flash --log off        # log nothing from the kernel's first line until LOG is switched on; --log debug also logs debug lines
cargo r1 stock                  # put Rabbit's firmware back and boot Rabbit OS, erasing user data
cargo r1 backup r1.age          # copy what the running R1 keeps into a file, encrypted under its safe's passphrase
cargo r1 restore r1.age         # put a backup's files on a running R1 in place of what it keeps
cargo r1 key                    # check an OpenAI key with OpenAI and store it on the running R1
cargo r1 key --qr               # check the key, then show it as a QR code for the R1's OPENAI KEY window
cargo r1 devices                # list connected R1s: what each runs, its port, its serial number
cargo r1 build                  # build out/boot.img and out/dtbo.img
cargo r1 build --enable-extras  # the same with the extras shown: the Pokédex
cargo r1 sh cat /proc/cmdline   # run one console command; sh - runs one per line of stdin
cargo r1 sh clock               # set the R1's clocks and UTC offset to this computer's
cargo r1 logs                   # copy every log on the R1 into logs/device; logs -f follows the live log instead
cargo r1 pull /data/camera/pictures pictures   # copy a file or directory from the R1
cargo r1 push NAME.mp4 /data/camera/videos/NAME.mp4   # copy a file to the R1, replacing what is there
cargo r1 record demo.mp4        # record the screen to an H.264 MP4 at 60 fps until Ctrl-C, using this computer's ffmpeg
cargo r1 bootloader             # print the bootloader's variables and log; the R1 stays in fastboot
```

- **Paths.** `build` and `stock` read Rabbit's firmware from `stock/v0.8.293/rabbit_OS_v0.8.293_20250516110545`, or from the directory given by `build --stock DIR`, `stock DIR` or `R1_STOCK`. `build` reads the files it needs from it before it builds anything. `build` writes `out/boot.img` and `out/dtbo.img` (`--out`, `--overlay`), `flash` reads them (`flash IMAGE --overlay FILE`), and `logs` writes into `logs/device` (`logs DIR`). Default paths are relative to the workspace that contains the current directory, else to the current directory; a path given on the command line is used as given. `stock` checks that each of Rabbit's images is what it is named for before it erases anything.
- **The OpenAI key.** After it has checked the boot image and the overlay and brought the R1 into fastboot, and before it writes anything, `flash` asks for an OpenAI key (pasted or typed, not echoed) and checks it with OpenAI. Enter flashes without a key. An abort at the question, by Ctrl-C or a key refused from input that is no terminal, leaves the R1 in fastboot with nothing written. An R1 that runs r1metal with its safe open and already stores a key is not asked, unless the flash erases user data, and with it that key: with `--wipe`, or where the bootloader has to be unlocked; one whose safe is shut cannot tell, and is asked. Without a key, the R1 shows a key window over home that scans the QR code from `r1 key --qr` ([OpenAI](docs/app/openai.md#the-key)). The key is stored in `/data/.r1metal/openai/key`, in the safe the passphrase opens ([Storage](docs/firmware/storage.md#the-safe)). Service costs are on [OpenAI](docs/app/openai.md).
- **The debug shell.** `pull`, `push`, `backup`, `restore`, `logs` (except `logs -f`) and the debug verbs of `sh` (listed by `r1 sh --help`) need firmware built with the debug shell, which is the default ([Console](docs/firmware/console.md)). `logs/` is untracked because logs contain network names, addresses and device identifiers ([Logs](docs/firmware/logs.md)).
- **Backup and restore.** `backup FILE` asks for the passphrase of the R1's safe, which the R1 checks, and copies what the R1 keeps into `FILE`: photos, videos, notes, translations, news, the conversation and the robot's memory, settings, known networks, the OpenAI key and its ledger, and Signal's account and conversations. `FILE` is an age file (age-encryption.org/v1) under that passphrase. What is the device's own stays out: the battery's gauge and usage, the modem's and the SIM's files, the logs and the stamp of its flash. `restore FILE` asks for the backup's passphrase and that of the R1's safe, stages the files on an R1 that runs r1metal with its safe open, and restarts it; once its passphrase is typed, the R1 puts each directory the backup holds in the place of its own and keeps what is the device's own. The R1 needs room for the backup beside what it keeps. A restored R1 is the Signal device the backup's R1 was: only one of the two may run with that account from then on ([Storage](docs/firmware/storage.md#backup-and-restore)).
- **Reaching the R1.** `flash`, `stock` and `bootloader` reach the R1 in any state. An R1 plugged in while powered off is caught in its preloader and sent into fastboot; an R1 running r1metal is sent the `bootloader` console command. With several R1s plugged in, a command refuses to run until `--device` or `R1_DEVICE` names one by its port or the end of its serial number, as `r1 devices` lists them (on macOS bus numbers start at 0, as in `0-1`). A flash follows its R1 by port through the preloader and the bootloader and touches no other.
- **macOS.** macOS shows "Allow accessory to connect?" each time the R1 appears on USB, after every reboot; `r1` prints this before it waits and continues once the R1 is allowed. The preloader listens only for about 2.5 s, so catching an R1 at power on (powered off, or running Rabbit OS) needs macOS to stop asking: System Settings > Privacy & Security > Security > Allow accessories to connect > Automatically When Unlocked. `r1` answers the preloader through the terminal macOS makes of its serial line, since macOS keeps the line's control interface for its own driver ([Boot](docs/hardware/boot.md#chain)).
- **Unlocking.** Rabbit permits the bootloader unlock per device: enable developer mode, then the unlock under device modification in Rabbit OS's settings, while online ([Rabbit's article](https://www.rabbit.tech/support/article/unlock-bootloader-rabbit-r1)). Unless the bootloader reports itself unlocked, `flash` and `stock` ask it whether it can unlock (`flashing get_unlock_ability`), `flash` before it asks for the key, and stop with nothing written if it cannot; otherwise they send `flashing unlock` without asking, and the R1 shows a prompt that needs no answer ([Boot](docs/hardware/boot.md#unlocking)).

| Command | `userdata` and `md_udc` |
|---|---|
| `flash` that had to unlock | erased; r1metal formats `userdata` on first boot |
| `flash` on an unlocked R1 | kept: r1metal keeps its own data partition and formats any other |
| `flash --wipe` | erased; r1metal formats `userdata` on first boot |
| `stock` | always erased: Rabbit OS cannot read r1metal's partition and formats an empty one itself (its fstab marks `userdata` `formattable`) |

## Develop

Every change follows [Architecture](docs/architecture.md#rules-for-every-change): read it in full before changing the code, and extend a concept's one home there rather than writing a second.

```
cargo test --workspace                                            # all tests that need no device
cargo test -p r1-funcore -p r1-image -p r1-flasher -- --ignored   # tests that need Rabbit's firmware under stock/ or the host's ffmpeg, ffprobe, poppler, librsvg, qrencode, zbarimg and zstd
cargo clippy --workspace --all-targets
cargo clippy -p r1-impshell --no-default-features --all-targets   # the firmware without the debug shell
cargo fmt --all -- --check
cargo r1 visibility                                               # every item that reaches further than what uses it, and every item only tests use; takes minutes
cargo test -p r1-impshell -- --ignored live_feeds_answer --nocapture   # fetch the news feeds over the host's network with impshell's code
cargo test -p r1-impshell -- --ignored live_site_answers --nocapture   # ask PokéAPI for the German names and an entry, and fetch its artwork, over the host's network with impshell's code
cargo test -p r1-impshell -- --ignored live_pages_read --nocapture     # fetch two pages over the host's network with impshell's code and read each out of its HTML as the browser does
cargo test -p r1-impshell -- --ignored service_caches_turns_12_hours --nocapture   # three turns of the conversation with OpenAI over the host's network, the key in OPENAI_API_KEY: none sent ahead, every cache write for 12 hours, read on the same socket and a new one
cargo run -p r1-funcore --example shot -- debug out/debug.png     # render a screen to a PNG on the host (out/ is git-ignored)
scripts/fetchicons.sh                                             # download every icon of Material Symbols Rounded and the older Material Icons Round from Google again
scripts/fetchglyphs.sh                                            # take what Spleen lacks from GNU Unifont and the emoji from Noto Emoji again
```

The first six commands are the gates every change must pass; `scripts/gates.sh` runs all six in turn, a clippy warning failing its gate, and exits non-zero if any gate failed. `r1-impshell` builds only on Linux, so on macOS each runs in the VM from the workspace's root, as in `limactl shell --workdir "$PWD" r1metal cargo test --workspace`; `scripts/gates.sh` runs there by itself.

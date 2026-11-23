# Kernel

Rabbit publishes the R1 kernel under the GPL: [android_kernel_rabbit_mt6765](https://github.com/rabbit-hmi-oss/android_kernel_rabbit_mt6765), branch `r1`. `r1 build` builds it (`crates/flasher/src/build/kernel.rs`):

| Step | What |
|---|---|
| Toolchain | `kernel/toolchain/llvm-23.1.3`: LLVM's 23.1.3 release for Linux on the CPU that builds, x86-64 or arm64, fetched once, SHA-256 checked, unpacked with `zstd -d --long=31` into a temporary directory and moved into place when complete |
| Source | `kernel/src`: Rabbit's tree at `8167c8c1087f`, fetched when the pinned commit is missing; a newly created tree gets every patch applied again |
| Modules | `kernel/vendor/mediatek/kernel_modules`: [android_kernel_modules_rabbit_mt6765](https://github.com/rabbit-hmi-oss/android_kernel_modules_rabbit_mt6765) at `0afdf059a0df`; WMT, the WiFi adaptor and gen4m are built into the kernel, so there are no module files |
| Patches | `kernel/patches/*.patch` on the kernel and `kernel/module-patches/*.patch` on the modules, applied in order with `git am`; reapplied only when a patch set or a pin changes, or when the tree no longer stands at the commit they left it at (`apply_patches`), as one put back from a backup does not; an `am` run stopped by a failing patch is aborted first |
| Config | `kernel/config` (Rabbit's, all 4401 options kept by `make olddefconfig`) with `kernel/config.changes` applied over it |
| Build | `Image.gz` (about 8.8 MB) in `kernel/out`, `-mcpu=cortex-a53`, ThinLTO, incremental |

The release string is `4.19.191-r1metal` (`CONFIG_LOCALVERSION`), built by `r1metal@r1metal`. `/proc/version` gives as the build's number the count of commits in the workspace's history, and as its time that of the last commit in UTC. Nothing else of the computer that builds reaches the kernel: make runs in the C locale, the source tree is named by its path from `kernel/out` (patches `0202` and `0203`), since ThinLTO's names for the local symbols it promotes take in the path each file was compiled by, and the compiler records the workspace's path as none (`-ffile-prefix-map`). Git ignores `kernel/src`, `kernel/vendor`, `kernel/toolchain` and `kernel/out`. To change the kernel, commit in `kernel/src` and export the commit with `git format-patch` into `kernel/patches`; module changes go into `kernel/module-patches`.

Rabbit's tree at the pin is the source of the shipped kernel: built from the stock configuration with the compiler it names (Android clang 11.0.1, build 6443078), it produces an `Image` of the stock size, 26019840 bytes, in which 58619 of 58647 functions have the stock size. The other 28 are later Rabbit fixes: USB audio and HID detection, GPU memory, the charger type property, the modem stop path.

The firmware's C code (AWS-LC) is compiled with the same LLVM release. `r1 build` puts its `bin` first on `PATH` for the firmware's `cargo build --locked`. `.cargo/config.toml` names `tools/cc-aarch64-musl`, which runs that `clang` for `cortex-a53`, and `llvm-ar`; the release profile compiles at `-O3`. The cores have `aes pmull sha1 sha2`, which AWS-LC's assembly uses. The build uses the Rust flags and compiler wrapper from `.cargo/config.toml` whatever `RUSTFLAGS` and `RUSTC_WRAPPER` the shell sets, and builds into the workspace's `target`, from which `r1 build` takes `init`. `crates/flasher/src/build/sysroot.rs` builds `kernel/toolchain/sysroot` from musl 1.2.5's headers (SHA-256 checked) and the kernel's `headers_install`, again whenever the kernel pin or a patch changes or the patches are applied anew.

## Patches

Each patch's commit message states what it changes and why; the messages are the patches' documentation. Other pages cite patches by number.

## Device tree

The base tree is the one dtb in stock `boot.img`, the overlay is stock `dtbo.img`. `crates/flasher/src/build/devicetree.rs` changes both, and checks each change against the tree it was written for, so any other tree stops the build:

- 12 PMIC rails marked always on: the bootloader leaves them on, nothing claims them, and the driver refuses to turn them off.
- The CPU rail's two modes, for EEM.
- The watchdog's unrequested interrupt removed.
- L2 cache nodes for both clusters.
- `pwr-regmap` on four clock controllers, so that a gate in an unpowered subsystem reads as off without a bus access, which DEVAPC reports as a violation.
- The charger's own end of charge removed (`enable_te`, [Power](power.md#charger)).
- The bridge to a second secure world disabled (`/nebula`), since the secure firmware does not serve it.
- In the overlay, the second EINT 144 pair of the PMIC without its trigger.

## Configuration

`kernel/config.changes` lists every departure from Rabbit's configuration in groups, each with its reason in a comment. It keeps what the firmware uses, including Bluetooth and the LTE modem drivers, and drops the rest, access control and the mitigations among it. Of Rabbit's hardening it keeps the stack protector (`CONFIG_STACKPROTECTOR_STRONG`) and the checks of copies to and from userspace (`CONFIG_HARDENED_USERCOPY`): the drivers built in parse what any transmitter in range sends, and a buffer overrun there panics the kernel, which restarts the device 1 s later (`CONFIG_PANIC_TIMEOUT=1`).

## Kernel facts the firmware is built around

| Setting | Consequence |
|---|---|
| `CONFIG_DEVTMPFS` off | init creates every device node: from `/sys/dev` at start, from `NETLINK_KOBJECT_UEVENT` after. The kernel lists a disk's partitions in `/sys/dev` before their uevents arrive, so a partition looked up by name gets its node from there at once (`sys::devnodes::partition_node`) |
| `CONFIG_RD_LZ4`, `CONFIG_RD_GZIP` | the ramdisk is LZ4 legacy or gzip |
| `CONFIG_PM_AUTOSLEEP` off, `CONFIG_PM_WAKELOCKS` | a suspend is a write to `/sys/power/state`; only a suspend started by writing back `/sys/power/wakeup_count` respects wakeup sources |
| `CONFIG_MTK_FB` on, `CONFIG_DRM` and `CONFIG_DRM_MEDIATEK` off | the display is fbdev `fb0`, 32 bit, flipped with `FBIOPAN_DISPLAY` |
| `CONFIG_VT`, `CONFIG_DEBUG_FS`, `CONFIG_GPIO_SYSFS`, `CONFIG_I2C_CHARDEV`, `CONFIG_DEVMEM` off | hardware is reached through its drivers, each register block through its own node's mapping; there is no `/dev/mem` |
| `CONFIG_USB_CONFIGFS_ACM`, `MASS_STORAGE`, `F_FS`, `NCM`; `CONFIG_AIO` | the gadget functions: the console, the drive, FunctionFS for screen recording, and the network adapter a computer is lent the modem's connection over (CDC NCM, which FunctionFS cannot be: `f_fs.c` refuses a class's own descriptors); the adapter's frames for the computer leave in one block 300 µs after the last, or once 31 frames or 16 kB have gathered (patch `0210`); asynchronous writes for screen recording |
| `CONFIG_TUN`, `CONFIG_PACKET`; `CONFIG_NETFILTER` off | the router's forwarder reads the device's own IPv4 packets from a TUN interface and the host's and the modem's from packet sockets, and translates addresses itself ([Network](../firmware/network.md#router)): the kernel filters and tracks nothing |
| an interface's last IPv4 address taken off drops its IPv4 routes (`fib_inetaddr_event`); a packet for an address the kernel does not hold is dropped without an answer while forwarding is off (`ip_error`) | a routed stay takes the connection's address off `ccmniN`, and the kernel resets or refuses none of the flows the forwarder translates ([Network](../firmware/network.md#the-routed-stay)) |
| `CONFIG_USB_CONFIGFS_UEVENT` | `USB_STATE` is `CONNECTED`, `CONFIGURED` or `DISCONNECTED`; a bus reset counts as a disconnect |
| `CONFIG_FUSE_FS` | the app serves the USB drive's medium |
| `CONFIG_PSTORE_RAM`, `CONFIG_PSTORE_CONSOLE` | the console of a boot that panicked is in `/sys/fs/pstore` at the next start |
| `CONFIG_LOG_BUF_SHIFT=19` | a 512 KiB log buffer |
| `printk.devkmsg` ratelimited by default | `/dev/kmsg` accepts 10 records per 5 s until `on` is written to `/proc/sys/kernel/printk_devkmsg` |
| `printk.silent` (patch `0063`) | while set, no record of any level is stored; setting it empties the buffer |
| `printk.debug` (patch `0188`) | while unset, the kernel's debug records are dropped before they are stored (`log_store`), a plain `printk` of one formatted first; while set, they are stored and every debug call site prints, each driver checking the switch at the print ([Logs](../firmware/logs.md#the-logging-switch)). On the command line it applies from the early parameters on. Drivers with logs of their own follow `printk.silent` through `printk_logging_notifier_register`; dynamic debug follows both switches |
| `CONFIG_PANIC_TIMEOUT=1` | the system reboots 1 s after process 1 dies |
| `CONFIG_SECURITY`, `CONFIG_MODULES` off | nothing is enforced, nothing is loaded |
| `CONFIG_F2FS_FS` with `F2FS_FS_COMPRESSION` off, `CONFIG_EXT4_FS` off | f2fs is the only disk filesystem |
| `CONFIG_FS_ENCRYPTION`, `CONFIG_KEYS` | f2fs encrypts directories (fscrypt, with the policies of version 2 that Android's 4.19 carries): the safe, which keeps the private directory and the public roots under one key ([Storage](../firmware/storage.md#the-safe)) |
| `CONFIG_HZ=250`, `CONFIG_NO_HZ_IDLE` | idle CPUs stop ticking |
| a neighbour's timer runs no sooner than 8 ms after it is set (patch `0209`) | a `retrans_time_ms` under 500 ms counts as written, so the ARP retry the firmware writes before a join is the one the kernel keeps ([Network](../firmware/network.md#addresses-and-leaving)) |

The USB controller (`musb-hdrc`) stays disconnected until `1` is written to `/sys/module/musb_hdrc/parameters/kernel_init_done`. In gadget mode it passes each request to its queue manager (QMU) as one descriptor with a 16-bit length (`prepare_tx_gpd` in `mtk_qmu.c`), so it rejects requests of 65536 bytes or more with `EOPNOTSUPP` (patch `0129`). Queued requests are sent back to back without the CPU.

## Log

With all patches applied, the kernel logs no record at warning level or above in normal operation: boot and shutdown, radio power cycles, joining and leaving a network, video recording with sound, screen recording, an aborted freeze, suspend and resume, cable changes, and a boot started by a cable, where a radio cycle logs the absent modem's messages at info. A clean build prints no compiler, make, tool or configuration warning.

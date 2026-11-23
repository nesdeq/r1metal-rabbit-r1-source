# Modem

The SoC's LTE modem (MD1), the kernel's CCCI driver that boots it and carries its traffic, and the services the modem asks of the AP. Kernel paths are relative to `kernel/src/drivers/misc/mediatek/`.

## The modem

| Fact | Value | Source |
|---|---|---|
| Firmware | `MOLY.LR12A.R3.MP.V230`, `TK_MD_BASIC_MDBIN_PCB01_MT6765_S00` | strings in `md1img.img` at file offsets 0x6C0 and 0x640 |
| Image type | 12, `ulwctg` (LTE, WCDMA, C2K, TD-SCDMA, GSM); the modem names itself `LWCTG one MD` | `md1rom`'s check header; `MD_LOAD_TYPE` in `include/mt-plat/mtk_ccci_common.h` |
| Generation | 6293 (`MD_GENERATION`) | `eccci/mt6765/ccci_config.h` |
| Partition | `md1img_a` and `md1img_b`, 100 MiB each; `r1 stock` writes `md1img_a` | scatter file |

`md1img.img` holds 21 images, each behind a 512-byte header with the magic 0x58881688, aligned to 16 bytes: `md1rom` (23325668 bytes), `md1drdi`, `md1dsp` (6914332 bytes), five certificates, six log filters, `md1_emfilter`, the modem database (`md1_mddbmeta`, `md1_mddbmetaodb`, `md1_mddb`, `md1_mdmlayout`) and `md1_file_map`. No band list is readable outside the proprietary database.

## Loading and enabling

- The bootloader loads `md1rom` and `md1dsp` from the active slot's `md1img` and hands the kernel a tag block in `/chosen` `ccci,modem_info_v2` (`ccci_util/ccci_util_lib_fo.c`). The kernel's own loader (`ccci_load_firmware`) has no caller.
- A boot a cable starts gets no modem: `get_modem_is_enabled` is false, the modem's platform device registers nothing, and there is no `ccci_monitor`, no port and no `ccmni` (patch `0130`).
- `/sys/kernel/ccci/boot` reads `md1:N`, N being the kernel's `MD_STATE`: 0 invalid (loaded, never started), 1 gated, 2 waiting for handshake 1, 3 waiting for handshake 2, 4 ready, 5 exception (`mtk_ccci_common.h`). Writing it fails.
- The status thread `ccci_poll1` wakes every 15 s on every boot with an enabled modem; it polls the modem only while the modem is ready and the modem logger's port is open (`eccci/fsm/ccci_fsm_poller.c`).

## Device nodes

Init creates them from `/sys/dev` and uevents ([Kernel](kernel.md#kernel-facts-the-firmware-is-built-around)); the majors are dynamic. Each char port has one opener: a second `open` fails with `EBUSY` (`eccci/port/port_proxy.c`).

| Node | Use | Header in `read` and `write` |
|---|---|---|
| `ccci_monitor` | the boot and stop ioctls; 16-byte monitor messages | 16-byte header, reads only |
| `ccci_fs` | the modem's file service ([File service](#file-service)) | user header |
| `ccci_rpc` | the two RPC ops the kernel leaves to userspace ([RPC](#rpc)) | user header |
| `ttyC0` | AT commands, raw text; a plain char device, no tty | kernel header, stripped on read and added on write |
| `ccci_ccb_ctrl` | the CCB control area, mapped ([CCB](#ccb)) | none |
| `ccci_md1_sta` | 56-byte status events | none |
| `ccci_ioctl0` to `ccci_ioctl4` | ioctls only | none |

- A port holds up to 32 received packets (`eccci/port/port_char.c`). A closed port drops what comes, except `ttyC0`, `ccci_fs`, `ccci_rpc`, `ccci_aud` and `ccci_udc`, which queue it; a close empties the queue (`port_dev_close`).
- `ccci_rpc` has no poll (`rpc_dev_fops` in `eccci/port/port_rpc.c`): epoll refuses it with `EPERM`, and a read blocks until a request comes.
- A write is at most 3456 bytes (`CCCI_MTU`), plus 20 bytes of header on `ccci_fs` and `ccci_rpc`. Before the modem is ready, only `ccci_fs` and `ccci_rpc` may write.
- On a user-header port the kernel sets `data[1]` to the write's length and the channel to the port's transmit channel, and keeps `data[0]` and `reserved`.

The ioctls (`eccci/inc/ccci_core.h`, magic `C`; arguments are `unsigned int` unless named):

| Ioctl | Number | Does |
|---|---|---|
| `GET_MD_STATE` | 0x80044301 | 0 invalid, 1 booting, 2 ready, 3 exception |
| `DO_STOP_MD` | 0x430C | stops the modem; the argument points to a word, nonzero for a flight-mode stop |
| `DO_START_MD` | 0x430D | starts the modem, only from a thread whose name begins with `ccci_mdinit`; from any other it is dropped and still returns 0 (`eccci/fsm/ccci_fsm_ioctl.c`) |
| `SET_BOOT_DATA` | 0x4040432F | 16 words ([Boot data](#boot-data)) |
| `GET_CCB_CONFIG_LENGTH` | 0x8004433F | number of CCB descriptors |
| `GET_CCB_CONFIG` | 0xC0184340 | one descriptor's sizes, by index |
| `CCB_CTRL_INFO` | 0xC0104347 | a CCB user's first descriptor, the control area's address and length |
| `ENTER_DEEP_FLIGHT`, `LEAVE_DEEP_FLIGHT` | 0x430E, 0x430F | a monitor message only |
| `SET_EFUN` | 0x40044337 | the clock buffer's flight flag only |

## Starting

1. `ccci_monitor` is opened and held: its close stops the modem (`force_md_stop`).
2. The CCB descriptors are written ([CCB](#ccb)) and the boot data set.
3. `DO_START_MD` from a thread named `ccci_mdinit`. The kernel's state machine (`eccci/fsm/ccci_fsm.c`) waits, polling every 20 ms for up to 30 s, until `ttyC0`, `ttyC1` and `ccci_md_log_ctrl` are closed; `ccci_fs` may stay open. After 30 s it goes on regardless.
4. It powers the modem ([Power](#power)), lets it run and waits up to 30 s for handshake 1, a control message with `reserved` 0x5555FFFF carrying the modem's feature request; it answers with the runtime data. Up to 30 s more for handshake 2 brings the state to ready. A timeout of either is an exception.
5. Handshake 1 and ready are status events 9 and 10 on `ccci_md1_sta`; a stop is 12, an exception 11. Each event is `{struct timeval, int md_id, int event_type, char reason[32]}`, 56 bytes; a new opener first gets the last event of type 9 or more (`ccci_util/ccci_util_broadcast.c`).

The start holds the wakeup source `md1_wakelock` until ready or a failure, and ready holds it 10 s more (`fsm_routine_start`). A monitor message is `{0xFFFFFFFF, message, 0xF0000000, argument}`: 0xFAF50001 force stop, 02 flight stop, 03 force start, 04 flight start, 05 reset, 06 exception, 07 battery information (`eccci/fsm/ccci_fsm_internal.h`).

## Stopping

A stop with the flight flag first waits up to 4 s for the modem to sleep after `AT+EPOF`, then powers it off; both stops end gated with the same hardware state, and hold `md1_wakelock` until the modem is off (`fsm_routine_stop`). The next start is a cold boot from the image in memory: the handshakes and the file service run again, and the SIM's PIN is asked again.

## Boot data

`SET_BOOT_DATA` takes 16 words (`MD_CFG_*` in `eccci/inc/ccci_modem.h`, read by `ccci_md_set_boot_data` in `eccci/ccci_modem.c`):

| Word | Meaning | r1metal |
|---|---|---|
| 0 | log mode, and in the high 16 bits a wait | 0 (idle) |
| 1 | SBP code, handed to the modem in the runtime data | the decimal number in `nvdata`'s `APCFG/APRDCL/CXP_SBP`, else word 0 of `APCFG/APRDCL/MD_SBP`, else 0, in the order of MediaTek's `ccci_mdinit` (`mdinit_platform.c`) |
| 2 | dump flag; 0xFFFFFFFF keeps the kernel's | 0xFFFFFFFF |
| 3 | SBP sub ID | 0 |
| 4 | RAT check flag: nonzero fails the start on an image without C2K | 0 |
| 5 to 10 | the RAT string, 24 bytes; empty means every RAT of the image | `C/Lf/Lt/W/T/G`, the stock `ro.vendor.mtk_ps1_rat` |
| 11 | world mode index, tried before the string; 0 skips it | 0 |

## CCB

The kernel offers the modem a cached buffer (CCB) for logging; the modem's buffer manager reads a table of descriptors from the CCB control area at its boot and asserts on an empty one. Rabbit OS's `ccci_ccb_init_users` (`libccci_util.so`) writes, for each of the three CCB users (DHL, MD monitor, META, indexes 0 to 2 for `CCB_CTRL_INFO`), one 64-byte descriptor per entry of the kernel's table (`eccci/port/port_smem.c` `ccb_configs`, 20 entries), from the user's first descriptor on, into the area mapped from `ccci_ccb_ctrl`:

| Offset | Value |
|---|---|
| 0x14, 0x18 | downlink page size, downlink buffer size |
| 0x1C | 0xEEFF0011 |
| 0x34, 0x38 | uplink page size, uplink buffer size |
| 0x3C | 0xEEFF0011 |

Every other byte is 0. r1metal writes the same words (`ccci::ccb_words`), and a table whose descriptors reach past the area `CCB_CTRL_INFO` tells fails the start with nothing written for that user.

## Stream framing

`ccci_fs` and `ccci_rpc` carry requests and replies in one framing (`ccci_fsd`, `ccci_rpcd`):

| Offset | Field |
|---|---|
| 0 | `data[0]`; bit 31 set on every fragment of a request or reply but its last |
| 4 | `data[1]`, the fragment's length with its header |
| 8 | channel (request 14 or 32, reply 15 or 33), sequence |
| 12 | `reserved`: the request's slot, 0 to 4 |
| 16 | operation; a reply sets the high 16 bits (`\| 0xFFFF0000`) |
| 20 | in the first fragment: the number of parameters, then each as `{u32 length, bytes, padding to 4}` |

A fragment carries at most 3456 bytes after its 20-byte header; the receiver appends each fragment's bytes after offset 20 to its slot's buffer, and the last fragment's header becomes the slot's. A reply copies the request's header, with the channel one higher.

## File service

The modem keeps its NVRAM on the AP. Its requests come on `ccci_fs` (MediaTek's `ccci_fsd`, built into Rabbit OS's `ccci_mdinit`):

| Op | Name | Parameters | Reply |
|---|---|---|---|
| 0x1001 | Open | path, flags | handle 1 to 128, or an error |
| 0x1002 | Seek | handle, offset, whence (0 start, 1 current, 2 end) | new position |
| 0x1003 | Read | handle, length | result, length read, bytes |
| 0x1004 | Write | handle, bytes, length | result, length written |
| 0x1005 | Close | handle | result; synced first where opened with `CREATE`, `CREATE_ALWAYS` or `COMMITTED` |
| 0x1006 | CloseAll | | result |
| 0x1007, 0x1008 | CreateDir, RemoveDir | path | result |
| 0x1009 | GetFileSize | handle | result, size |
| 0x100A | GetFolderSize | path, flag (2: in clusters) | size of the files, each counted in whole clusters |
| 0x100B | Rename | path, new path | result |
| 0x100C | Move | path, new path, flag (1 copy, 2 kill, 0x10000 overwrite) | result |
| 0x100D | Count | path, flag (0x4 files, 0x8 folders, 0x10 recursive) | count |
| 0x100E | GetDiskInfo | drive, flags | result, 84-byte `FS_DiskInfo` |
| 0x100F | Delete | path | result |
| 0x1010 | GetAttributes | path | 0x01 read-only, 0x10 folder, or an error |
| 0x1011 | OpenHint | path, flags, 8 bytes | handle, the 8 bytes |
| 0x1012 | FindFirst | pattern, attributes, attribute mask, name length | handle, 52-byte `FS_DOSDirEntry`, name |
| 0x1013 | FindNext | handle, name length | result, entry, name |
| 0x1014 | FindClose | handle | result |
| 0x1015, 0x1019, 0x101C | LockFAT, ClearDiskFlag, SetDiskFlag | | 0 |
| 0x1016 | UnlockAll | | 1 |
| 0x1017 | Shutdown | | closes every handle; no parameters |
| 0x1018 | XDelete | folder, flag (0x4, 0x8, 0x10) | number deleted |
| 0x101A | GetDrive | type, serial, mask | `Z` for the system drive (type 4) |
| 0x101B | GetClusterSize | drive letter | the drive's block size |
| 0x101D to 0x1020 | OTP | | MediaTek's OTP driver |
| 0x1021 | Restore | path | restores the file from the `nvram` partition's bin region |
| 0x1022 | CMPT read | path, 40-byte request | done mask and error, size, length read, bytes |
| 0x1023 | bin region access | type | -2 |
| 0x1024 | CMPT write | path, 44-byte request, bytes | done mask and error, handle, length written |
| 0x1025 | GetFileDetail | path | result, modified, accessed and changed times as `u64` |

- Paths are UTF-16LE; each unit's low byte is kept and `\` becomes `/`; `..` is refused.
- `Open` hands the path to `open(2)`: `O_RDONLY` for the flag 0x100 without `CREATE` (0x10000) or `CREATE_ALWAYS` (0x20000), else `O_RDWR`. A folder opens read-only alone, and a read or write of its handle fails with -18. The modem opens `Z:\NVRAM` with 0x900 (`OPEN_DIR` and read-only) as it boots, and an open that fails makes it assert.
- Drives: `Z:` `nvdata/md`, `X:` `protect1/md`, `Y:` `protect2/md`, `V:` `nvdata/md_cmn`, `S:` `nvcfg`, `U:` the modem's low-power log folder, `Q:` `/data/vendor_de/md`, `T:` and `R:` read-only folders of the vendor partition, `W:` the DSP image, which the bootloader has loaded already.
- Errors: -2 parameter, -4 drive not found, -5 too many files, -6 no more files, -9 file not found, -10 invalid handle, -16 access denied, -18 general failure, -19 path not found, -45 read-only, -49 path too long.
- `GetDiskInfo` writes bytes per sector 512 at 0x30, sectors per cluster at 0x34, total clusters at 0x38 and free clusters 512 at 0x40.
- `FindFirst` matches the pattern's last component, `*` and `?`, case-sensitive, skipping `.` and `..`; an entry is taken where its attributes hold every bit of `attributes` and none of `mask`. The name comes back in UTF-16LE with `/` as `\`, at most the length asked, and a zero unit.

## RPC

The kernel answers the modem's RPC requests itself (GPIO, ADC, SIM hot-plug interrupts, device tree values, random numbers) on the port `ccci_rpc_k`, except two it routes to `ccci_rpc` (`eccci/port/port_rpc.c`): 0x400F, an AP property by name, which MediaTek's `ccci_rpcd` answers with the property's length and its value with a NUL, and 0x4010, the SAR table index, which it answers with -2 like any operation it does not know.

## AT commands

`ttyC0` takes AT commands as raw text; replies end in `\r\n`. MediaTek's RIL drives the modem with standard and MediaTek commands: `AT+CPIN` and `AT+EPINC` (PIN and its remaining tries), `AT+CFUN=1` and `AT+CFUN=4` (radio on and off; `AT+CFUN=0` also powers the SIM off), `AT+EPOF` (detach and SIM off before a stop), `AT+CEREG` (registration), `AT+EIAAPN` (initial attach APN), `AT+EAPNACT` (a data connection on), `AT+EPDN` (its addresses), `AT+EIF` (the AP's interface up), `AT+CGCONTRDP` (DNS and MTU). Unsolicited lines include `+CEREG`, `+CGEV: ME PDN ACT`, `+CGEV: NW PDN DEACT`, `+EIF` and `+EPDN`.

After the kernel's ready the modem sends `+EIND: 64`, then `+EIND: 128` once its AT channel takes commands; a command written before `+EIND: 128` may go unanswered. MediaTek's `gsm0710muxd`, which holds `ttyC0` on Rabbit OS, waits for `+EIND: 128` before its first command, up to 10 s on a user build and 20 s on any other, then goes on without it.

## Data

`ccmni0` to `ccmni20` exist on every boot with a modem: raw IP (`ARPHRD_RAWIP`, no header, `IFF_NOARP`), MTU 1500 at most (`ccmni/ccmni.c`). The modem's network interface N is `ccmniN` both ways. The kernel sets no address, route or DNS on them.

## Power

| Step | Effect (`eccci/mt6765/md_sys1_platform.c`) |
|---|---|
| Power on | modem clocks ungated, `vmodem` set to 0.8 V, the MD1 power domain on, the modem's share of PBM reserved (patch `0150`), the modem's 26 MHz request (`SRCCLKENA`) set, the modem PLLs started, the modem watchdog disabled |
| Power off | power domain off, `SRCCLKENA` cleared, clocks gated, PBM share released; `vmodem` stays on, its off call is compiled out |

The suspend's wake sources include the modem's CCIF and CLDMA lines ([Power](power.md#suspend)), so every downlink packet and every AT line wakes the SoC. Beyond its start and its stop, the kernel holds a suspend off for a running modem only after what the modem sends: 500 ms after each packet a port queues (`port_recv_skb` in `eccci/port/port_proxy.c`), 1 s after each packet a `ccmni` receives (`ccmni/ccmni.c`), each wakeup the modem signals over CCIF (`eccci/modem_sys1.c`) and each wakeup of a CCB port (`eccci/port/port_smem.c`), and 10 s after an exception or a watchdog (`ccci_fsm_recv_md_interrupt`).

## SIM

The R1 has one slot. The modem powers the SIM and reads its hot-plug interrupt (`MD1_SIM1_HOT_PLUG_EINT`) through RPC; no AP driver touches it.

## Factory NVRAM

| Partition | Size | The modem's |
|---|---|---|
| `nvdata` (ext4) | 64 MiB | `md/` (`Z:`), `md_cmn/` (`V:`) |
| `protect1` (ext4) | 8 MiB | `md/` (`X:`) |
| `protect2` (ext4) | 10.9 MiB | `md/` (`Y:`), the mirror of `X:`, which the modem writes itself |
| `nvcfg` (ext4) | 32 MiB | `S:` |
| `nvram` | 64 MiB | the bin region `Restore` reads |

The modem writes its NVRAM while it runs and makes a missing file from its defaults where it may create files.

## In r1metal

The modem runs while the setting `MODEM` under MOBILE DATA is on ([Interface](../app/interface.md#mobile-data)). The APN a connection takes is chosen in [Cell](../firmware/cell.md#choices). funcore: `r1_funcore::cell` (`ccci.rs` the kernel's records, `stream.rs` the framing, `files.rs` the file service, `drives.rs` the drives and their first filling, `rpc.rs`, `at.rs`, `link.rs` the data interface, `sim.rs` the SIM's file, `attaches.rs` the record of attaches, `modem.rs` the state machine). impshell: `cell/` (the `ccci_mdinit` thread in `mod.rs`, the kernel's calls in `ccci.rs`, `ccmniN` in `link.rs`, the first filling of the drives in `seed.rs`).

| Step | What r1metal does |
|---|---|
| Thread | `ccci_mdinit` waits for the data partition, holding no suspend off meanwhile whatever is asked of it, opens `ccci_monitor` where `/sys/kernel/ccci/boot` names `md1`, and holds it for the app's life; the `modem monitor` thread reads it and answers the battery request with `SEND_BATTERY_INFO`. Beside it the `modem rpc` thread holds `ccci_rpc` for the app's life and answers each request as a blocking read brings it, as MediaTek's `ccci_rpcd` does. Without `md1` (a boot a cable started) the modem counts as absent |
| Files | The drives live under `.r1metal/cell/nvram/<drive>/` ([Storage](../firmware/storage.md#layout)). The first start fills each of `Z`, `V`, `X`, `Y` and `S` from its partition, read as ext4 (`r1_image::ext4`), into that folder and a pristine copy under `.r1metal/cell/factory/<drive>/`, which `Restore` reads; `.r1metal/cell/seeded` lists the drives filled whole. `U` and `Q` are made empty on use. An open file's bytes are kept in memory, one copy for every handle on it, as `open(2)`'s descriptors share an inode, and written at each `Close`, `CloseAll`, `Shutdown` and once the modem is stopped or its thread ends; a file deleted while open is not written back, and one renamed while open is written where it went. A file grows to 64 MiB at the most, `nvdata`'s size; a write past it answers -18. Times are UTC |
| Start | CCB descriptors, boot data (the SBP code read from `nvdata` before each start), `ccci_fs` opened, `DO_START_MD`; the kernel's status event 10 opens `ttyC0` |
| SIM | once `+EIND: 128` comes, or 10 s after `ttyC0` opened without it, as `gsm0710muxd` waits ([AT commands](#at-commands)): `ATE0Q0V1`, `AT+CMEE=1`, `AT+CGEREP=1,0`, `AT+CEREG=1`, `AT+EGTYPE=1`, `AT+CFUN=4`, `AT+CPIN?` (a busy SIM asked again every 500 ms, 20 times, as MediaTek's RIL does), `AT+EPINC`. The kept PIN or a typed one goes to the SIM as [Cell](../firmware/cell.md#choices) chooses; a kept one the SIM refuses is forgotten. A PIN typed on `SIM PIN` or given over the console goes at the SIM's next ask, the modem's next start included; it is dropped where the SIM turns out to ask for none, and at a stop or a failure. With `KEEP PIN` on it is kept in `.r1metal/cell/sim` once the SIM takes it; `KEEP PIN` off forgets the one kept. Then `AT+EIFCONFIG=3`, `AT+EIAREG=1`, `AT+EDALLOW=1` |
| Between stays | attached and idle for 15 min after a stay that began awake (`KEEP`), each stay keeping it anew, then radio off (`AT+CFUN=4`); a stay begun asleep, a fetch for the alarm, keeps nothing of its own, and one on a kept attach ends with the keep. Asleep, the clock's alarm wakes the device to end the keep ([Sleep and clock](../firmware/sleep.md#the-alarms-list)). Attached and idle for as long as the console holds the attach (`attach on`, [Console](../firmware/console.md#verbs)). Why these and what they cost: [Cell](../firmware/cell.md) |
| A stay | `AT+CFUN=1`, then `+CEREG` 1 or 5 within 75 s (5 tries of T3410, 3GPP TS 24.301; 3 is a refusal), `AT+EAPNACT=1,"APN","default",0` with the APN `+EIAREG` names, else the first `AT+CGCONTRDP` names, without its `.mncNNN.mccNNN.gprs`, and where neither names one yet, as at the first attach after the modem's start, the `+EIAREG` that comes within the attach's 75 s; then the interface from `+EIF ... ifup` or `+EPDN`, `AT+EIF=ID,"ifst","up",KIND`, `AT+EPDN`, `AT+CGCONTRDP` for the MTU and the network's DNS servers, which a connection of IPv6 alone hands the radio for its NAT64 ([Network](../firmware/network.md#mobile-data)) |
| Interface | `ccmniN` (N the interface id mod 100): down, IPv6 settings as WiFi's ([Network](../firmware/network.md#addresses-and-leaving)), the MTU, up, the IPv4 address as `/32` with the default route over the interface, the IPv6 link-local address from `+EIF` as `/64`, from which the kernel's SLAAC takes the interface id |
| A routed stay | where a computer on the cable is lent the connection, the radio's thread takes the interface's IPv4 address off it again once the modem's thread has set it up, and hands it to the router's forwarder, which reads and writes `ccmniN` through a packet socket; the modem's thread sets the interface up and leaves it the same either way ([Network](../firmware/network.md#the-routed-stay)) |
| Leaving | the interface's addresses off, `AT+EIF ... "down"`, then `AT+CFUN=4`, unless the attach is kept: then the radio stays on and the connection idle for the next stay, which takes it up again without an attach. A failed or lost connection waits 10 s (T3411), doubling to 12 min (T3402), before the next, and the wait's end is told the main loop (`State::note`), which lends the modem again from then; a connection the network refuses waits the 12 min at once, the back-off 3GPP TS 24.301 gives where the network names none, since the modem refuses every try meanwhile by itself, and the log tells the day's attaches. A radio the modem does not turn off at `AT+CFUN=4` counts on, and the next leaving turns it off |
| Loss | `+CGEV: NW PDN DEACT` or `ME PDN DEACT` of the connection, `+CGEV: NW DETACH` or `ME DETACH`, `+EIF ... ifdown`, or `+CEREG` 0 or 3 once registered; each ends a connection up or an attach kept between stays, and fails a join under way; a search (`+CEREG` 2) keeps them. The answer to a join's command that comes after its join was left is passed over |
| Log | `cell: the AT channel takes commands N ms after it opened`, or `cell: no +EIND: 128 in 10 s after the AT channel opened; its commands go regardless`; `cell: the SIM is ready N ms after the modem's start`, `cell: registered (1), N ms after the radio came on`, and `cell: online N ms after the ask`, attaching from the radio off, with the radio on already, or on the attach kept since the last stay; `cell: the attach kept 15 min after its last stay goes`; `cell: refused after N attaches in the last day: none for 12 min`; each file service request answered with an error, as `cell: file service 0x1001 Z:/NVRAM flags 0x0 answered -18`; a file of the SBP code that does not read, as `cell: the SBP code's APCFG/APRDCL/CXP_SBP: REASON` |
| Console | `at COMMAND` sends a command in its turn and logs its answer; `attach on` holds the attach between stays ([Console](../firmware/console.md#verbs)) |
| Stop | what the way down still sends (the interface's `"down"`, `AT+CFUN=4`, the console's commands), the SIM's and the setup's commands dropped and an answer to them passed over, then `AT+EPOF`, after which nothing is sent; `ttyC0` closed, `DO_STOP_MD` with the flight flag; status event 12 ends it and the open files are written. An exception, a stop of its own, a failed read of `ttyC0` or `ccci_fs` or a reply `ccci_fs` does not take fails the modem, but in a stop under way, which goes on ([Cell](../firmware/cell.md#choices)). A thread that can wait no more, its epoll or its read of `ccci_md1_sta` failing, stops the modem without the flight flag where it may run, writes the open files and ends, the modem ended until the app starts again |
| Commands | each answer waited for as long as [Cell](../firmware/cell.md#choices) chooses, but for a command that could not be written to `ttyC0`, a write that failed or the channel closed after a failed read, which ends at once as one unanswered; a command's arguments, which may hold the PIN, never reach the log, and the kernel's record of `ttyC0` stays out of the debug shell ([Console](../firmware/console.md#debug-shell)) |

## Sources

- MediaTek's `ccci_mdinit`, `ccci_fsd` and `ccci_rpcd` sources (Apache 2.0), as mirrored in StyxProject's `vendor_mediatek_opensource` at 68be90c0.
- Rabbit OS v0.8.293: `vendor/bin/ccci_mdinit`, `vendor/lib64/libccci_util.so`, `vendor/etc/init/init.cccimdinit.rc`, `vendor/bin/gsm0710muxd` (`poll_modem_ready`, `chat`).
- The MT6739 bring-up on the same modem generation: `nixos-agm-m7` at f5bffcb5, `docs/MODEM-RE.md` and `docs/CCCI-FS.md`.

## Measured on the R1

| What | Value |
|---|---|
| `ttyC0` opened at the kernel's ready to `+EIND: 128` | 1.4 to 1.5 s; a command written at the open goes unanswered (`/proc/ccci_dump` records each `ttyC0` read and write) |
| Start to the SIM ready, the kept PIN handed over | 5.2 s; the modem reaches ready on the factory's drives as copied, and makes a file it misses from its defaults |
| An attach from the radio off | registered 0.76 to 1.4 s after `AT+CFUN=1`; online 0.7 to 1.0 s after the ask |
| An attach kept between stays, taken up again | online 10 to 12 ms after the ask |
| IPv6 | works with the link-local address from `+EIF` alone |
| `AT+EIF ... "ifst"` on `ttyC0` | `"up"` answers `OK`; `"down"` answers `+CME ERROR: 65535` on every connection, kept or not, and the next stay works |
| `AT+CPSMS?`, `AT+CPSMS=1`; `AT+CEDRXS=?`, `AT+CEDRXS?`, `AT+CEDRXS=2,4,"0101"`, `AT+CEDRXRDP` | `+CME ERROR: 100` to each: the modem takes neither power saving mode nor extended DRX by these commands |
| Suspended, booted with the radio off | 4.1 mA, no wake, against 3.3 to 3.6 mA with the modem never started ([Power](power.md#draw)) |
| Suspended, attached and idle | 9.3 mA over the first hour, 5.6 mA (4.8 to 6.5) once it settles; one wake in the hour, the modem's `CCIF0` as it writes its NVRAM (`NA77_005`) |
| One attach from the radio off, to online and the radio off again | 0.15 to 0.22 mAh by the coulomb counter |
| A connection the network refuses | `AT+EAPNACT` answers `+CME ERROR: 4355`, a MediaTek code, within 16 ms of the registration on every try: the modem's own back-off |

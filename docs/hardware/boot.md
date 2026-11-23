# Boot

The boot chain, how `r1` reaches fastboot and unlocks the bootloader, verified boot, the A/B slots, Rabbit's firmware put back and the partitions written.

## Chain

The boot order is the preloader (USB `0e8d:2000`; it sends `READY` on its serial port and accepts `FASTBOOT`), then LK (fastboot, USB `0e8d:201c`), then the boot image in `boot_a` or `boot_b`. To reach fastboot, `r1` answers the preloader of an R1 being powered on with `FASTBOOT`. The preloader sends `READY` only once the host has raised DTR on its serial line, acknowledges `FASTBOOT` with `TOOBTSAF`, and boots on after about 2.5 s without a handshake. On Linux, `r1` claims the line's data interface and sends the CDC requests that set the line and raise DTR to its control interface. On macOS, `AppleUSBACMControl` holds the control interface exclusively and macOS refuses those requests, so `r1` opens the terminal macOS makes of the line (`/dev/cu.usbmodem…`, found under the device in the I/O Registry once macOS has made it), which raises DTR (`crates/flasher/src/link/serial.rs`). A running r1metal is sent the `bootloader` console command and reboots into LK by itself, and `r1` leaves the preloader alone on the way (`reach_fastboot` in `crates/flasher/src/flash.rs`).

| Fact | Value | Source |
|---|---|---|
| Boot image | header v2, page 2048, kernel at `0x40080000`, ramdisk at `0x51b00000`, dtb at `0x47880000` | stock `boot.img` |
| Command line in the image | `bootopt=64S3,32N2,64N2 buildvariant=user` | stock `boot.img` |
| Stock kernel | `4.19.191-g6d50255ed13e`, gzip, 11154410 bytes | stock `boot.img` |
| Ramdisk | LZ4 legacy | stock `boot.img` |
| Device tree | one dtb of 103765 bytes in a dt table; the overlay is in `dtbo` | stock `boot.img` |
| `boot_a`, `boot_b` | 32 MiB each | scatter file |
| Unused | `vendor_boot_a/b` 64 MiB, `init_boot_a/b` 8 MiB each | scatter file |
| AVB | stock `vbmeta`, flags 0, chains to `boot`, `vbmeta_system` and `vbmeta_vendor` with Rabbit's keys and holds the hash of `dtbo`; `vbmeta_system` holds the hashtrees of `system` and `product`, `vbmeta_vendor` that of `vendor` | `avbtool info_image` |
| `super` | Virtual A/B (`LP_HEADER_FLAG_VIRTUAL_AB_DEVICE`); Rabbit's `super.img` gives `system_a`, `product_a` and `vendor_a` their extents and the partitions of slot b none | stock `super_empty.img`, `super.img` |
| Download | at most 128 MiB (`max-download-size` `0x8000000`) | LK |

## Verified boot

`r1` flashes `crates/image/assets/vbmeta-open.img`: flags 3, no descriptors, so no image is verified. An unlocked device shows the orange state notice for 5 s and boots. The image is made with a throwaway key:

```
avbtool make_vbmeta_image --flags 3 --padding_size 4096 --algorithm SHA256_RSA4096 --key KEY.pem --rollback_index 0 --output crates/image/assets/vbmeta-open.img
```

## Unlocking

`flashing unlock` in stock `lk.img` (the routine at `0x28444` of the image body):

1. Draws MediaTek's prompt (Volume Up yes, Volume Down no).
2. Reads one key once and discards the result: the prompt accepts no answer.
3. Reads the last 4 bytes of `frp`. All zero: not allowed. No `frp` partition: allowed.
4. Not allowed: shows `Unlock failed...return to fastboot in 3s`, then answers `FAIL Unlock operation is not allowed`.
5. Allowed: erases `userdata`, `metadata`, `md_udc` and `cache` where they exist, unlocks, shows `Unlock Pass...return to fastboot in 3s` and answers `OKAY`.

`flashing get_unlock_ability` returns `unlock_ability = N` from the same 4 bytes. The flasher reads it before anything is asked or written and stops with nothing written when it is 0 (`may_unlock` in `crates/flasher/src/flash.rs`). Rabbit sets the bytes per device when its owner enables the unlock in Rabbit OS ([r1metal](../../README.md#use)).

## dm-verity warning

From `get_dm_verity_status` and `clear_dm_verity_status` in stock `lk.img`: LK shows the red warning screen when the status is 1 or the AVB hashtree error mode is 2 (EIO), waits 5 s for a key, then powers off. The status is 1 when:

| Condition | Persists |
|---|---|
| the low nibble of `WDT_NONRST_REG2` is 4 (reboot reason `dm-verity-dev-corrupt`) | sets the seccfg field below |
| bit 0 of `security_ao` register `0x88` is set | sets the seccfg field below |
| the seccfg field at `0x10` is nonzero | until cleared |
| seccfg cannot be read | no |

LK's AVB code never sets the field. The first read in a boot consumes the nibble; a boot from fastboot never reaches the check.

The field is cleared by a key press on the red screen, by erasing `seccfg`, or by a nonzero 32-bit word at offset `0x1000` of `misc`: LK then clears the field, skips the screen and zeroes the word. LK maps `misc` to the `para` partition, which also holds the slot control block at `0x800` and LK's environment at `0x20000`. When the active slot can be read, `flash` and `stock` write the first `0x1004` bytes of `para` with that word, and `set_active` then rewrites the slot control block.

## A/B slots

`para+0x800` holds AOSP's `bootloader_control`: magic `BCAB`, a priority, a try counter and a successful flag per slot, and a CRC32 over the first 28 bytes (`r1_funcore::hw::slots`).

| Command | Writes | Then |
|---|---|---|
| `flash` | `vbmeta`, `dtbo` and `boot` of the inactive slot | `set_active` on it; the other slot stays as the fallback |
| `flash --both` | both slots in turn, the active one last | `set_active` on the other slot, then on the active one |
| `stock` | Rabbit's images of a slot to both slots in turn, the active one last, after erasing `userdata` and `md_udc`; then `super` and `logo` | `set_active` on the other slot before the active one is written, then on `a` ([Stock](#stock)) |

If the active slot cannot be read, both slots are written and neither the slot choice nor `para` is touched. If `set_active` fails for a slot after the writes, the other slots are still activated and the R1 rebooted; `flash` or `stock` then exits with an error naming the slot, and the R1 boots whichever slot its bootloader chooses. If `stock` cannot activate the other slot, it stops before it writes the active one.

`flash` writes `boot`, `dtbo` and `vbmeta` alone: the slot it activates boots with the bootloader and the firmware already on it, Rabbit's from an update of Rabbit OS or from `stock`.

After boot, the firmware marks the slot named by `androidboot.slot_suffix` successful, as Android's boot control HAL does (`crates/impshell/src/storage/slots.rs`). It finds `para` by `PARTNAME` in `/sys/dev/block/*/uevent`, among the partitions of the boot device, the eMMC host the kernel names `bootdevice` (`r1_funcore::hw::uevent::partition_node`); a partition of the same name on an SD card is skipped. It then writes `0` to `/proc/pl_lk`. The kernel's log store then clears the SRAM flag the preloader sets at every start, so the preloader does not save the log of a boot that came up into the tail of `expdb`, and it records the boot phase in the last block of `expdb`, which it opens by partition name on the boot device (patch `0143`). If a boot dies before the mark, the preloader saves its log at the next start; until the mark, the kernel logs `not boot up, don't store log to expdb`.

## Stock

`r1 stock` writes all of Rabbit's firmware but the preloader and `userdata` (`stock` in `crates/flasher/src/flash.rs`):

1. Each image of a slot, in the order of Rabbit's scatter file: `md1img`, `spmfw`, `scp`, `sspm`, `gz`, `lk`, `boot`, `dtbo`, `tee`, `vbmeta`, `vbmeta_system`, `vbmeta_vendor`. The slot the R1 does not boot from is written first and made active, then the other. The preloader, never written, starts the bootloader of the active slot, so it always finds one written whole and fastboot stays in reach.
2. `super`, in sparse images of one download each (`r1_image::sparse::Sparse::parts`): each stands for the whole partition and skips all but its own run of chunks, as `fastboot` cuts one.
3. `logo`.
4. The update marker in `para`, then `set_active a`, the slot Rabbit's `super.img` fills.

Stock `vbmeta` turns verification back on, and the kernel checks `system`, `product` and `vendor` against the hashtrees in the slot's `vbmeta_system` and `vbmeta_vendor`. Without `super` and those two written, it checks whatever `super` holds against whatever they hold: an Android flashed by someone else, a Rabbit OS newer than v0.8.293, or a slot whose space Virtual A/B gave to the other. A mismatch is a dm-verity corruption, then LK's red screen and a power off ([dm-verity warning](#dm-verity-warning)); on a charger the R1 starts again into the same, until the slot's tries are spent and LK moves to the other.

Before anything is erased, `stock` holds each image to be what it is named for: the boot image, the overlay table and the vbmeta images parse as such, the others and `logo.bin` begin with MediaTek's partition header (`0x58881688`), `super.img` is a sparse image.

## Partitions touched

| By | Writes | Reads |
|---|---|---|
| Flasher | `boot_a/b`, `vbmeta_a/b`, `dtbo_a/b`, `para`; with `stock` also `md1img`, `spmfw`, `scp`, `sspm`, `gz`, `lk`, `tee`, `vbmeta_system` and `vbmeta_vendor` of both slots, `super` and `logo`; erases `userdata` and `md_udc` after an unlock, with `flash --wipe` and for `stock` | |
| Firmware | `para` (the slot mark), `userdata`, the table at the head of `expdb` once its dump is copied ([Logs](../firmware/logs.md)); through the kernel's log store, the boot phase in the last block of `expdb` | `nvdata`, for the factory WiFi and Bluetooth addresses, the WiFi calibration and the modem's SBP code; `nvdata`, `protect1`, `protect2` and `nvcfg` once, for the modem's factory NVRAM at its first start ([Modem](modem.md#in-r1metal)) |

Only `stock` writes `super`. The preloader is never written.

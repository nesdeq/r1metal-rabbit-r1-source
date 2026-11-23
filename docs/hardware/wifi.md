# WiFi

The connectivity core, its drivers and their behaviour. Scanning, joining and everything above them are described in [Network](../firmware/network.md).

The SoC's connectivity core is MediaTek's CONNAC generation 1 (`soc1_0`, step E1) with the MT6631 radio chip. The driver reads chip `0x6765`, version ID `0x10020300`, hardware and firmware version `0x8a00`. Rabbit's firmware for it is from branch `t-neptune-mp-soc1_0e1-2015-CERVINO_SOC1_0_E1_ASIC`, built 2023-06-16.

## Drivers

MediaTek's three drivers are built into the kernel ([Kernel](kernel.md)): WMT (power, clocks, the link to the chip's processor), the WiFi adaptor (`/dev/wmtWifi`) and gen4m (`wlan0`). None starts by itself; at boot only `wmt_detect` registers its device (major 154, `/dev/wmtdetect`).

**First start.** Rabbit OS runs `wmt_loader`, `wmt_launcher -p /vendor/firmware/ -o 1` and `wlan_assistant` (calibration from `nvdata`). r1metal ships none of them. The first time the radio is wanted, the radio thread makes the first two programs' calls itself and hands over the third's calibration file ([Factory calibration](#factory-calibration)). Code: `crates/impshell/src/wifi/radio/driver.rs`, with the ioctl numbers and the patch record; file names, what the files' headers say and the calibration in `r1_funcore::wifi::wmt`.

| Node | Call | Effect |
|---|---|---|
| `/dev/wmtdetect` | `COMBO_IOCTL_CONNSYS_SOC_HW_INIT` | Powers the block that identifies the chip; before this the chip ID reads 0 |
| | `COMBO_IOCTL_GET_SOC_CHIP_ID` | Returns `0x6765` |
| | `COMBO_IOCTL_SET_CHIP_ID` | Sets the chip ID in the driver |
| | `COMBO_IOCTL_MODULE_CLEANUP` | Removes the card driver that detection registered; without it the start fails with "already registered" |
| | `COMBO_IOCTL_DO_MODULE_INIT` | Starts WMT, the adaptor and gen4m, once per boot; the missing Bluetooth and GPS drivers are not a failure (module patch `0014`). Success shows as `/dev/stpwmt` and `/dev/wmtWifi` appearing |
| `/dev/stpwmt` | `WMT_IOCTL_SET_STP_MODE` `0x23` | Sets the link to BTIF; without it the driver gives up with "no hif info" |
| | `WMT_IOCTL_SET_ROM_PATCH_INFO`, three times | Names the processor's, Bluetooth's and WiFi's firmware file and where each goes in the 4 MiB shared with the chip |
| | `WMT_IOCTL_SET_PATCH_NUM`, `WMT_IOCTL_SET_PATCH_INFO` | The one classic patch and its address in the chip |

Type, slot and address are read from each file's header, as Rabbit's launcher does. With this information the driver sends no requests at power on; without it, it requests the files through `/dev/stpwmt` (`srh_rom_patch`, `srh_patch`) and waits 6 s. The radio thread reads that node while the core powers up and answers any request. If starting or configuring the drivers fails, they stay unusable until the next restart (`Setup::Failed`).

**Power on and off.** Writing `1` to `/dev/wmtWifi` returns once `wlan0` exists or power on has failed (`S` would also create a P2P interface). Writing `0` removes WiFi and, as WiFi is the only function in use, powers the core down. `/dev/stpwmt` is held open while the core is powered and closed afterwards: closing it early makes the driver shut down; holding it over an unpowered core makes a thermal worker access the chip. `wlan0` stays registered while the core is off. gen4m reports the end of a scan only when its interface goes down (`wlanStop`), and the kernel refuses new scans (`EBUSY`) while it counts one as running, so `wlan0` is brought down before `0` is written.

By the kernel log, a power on takes 1.30 s with calibration, 1.05 s of it RF calibration, and 0.34 s with kept calibration: power, clocks and copying the three firmware files about 0.12 s; classic patch, reset and settings 0.1 s; loading gen4m's firmware and registering the interface 0.04 to 0.08 s. The radio thread's radio on and off times are in [Network](../firmware/network.md#typical-timings).

**Calibration.** The driver keeps the calibration results in kernel memory (448 bytes Bluetooth, 8192 WiFi); MediaTek reuses them only after a chip fault. Module patch `0003` adds the parameter `/sys/module/wmt_drv/parameters/restore_calibration`, which the radio thread sets to `1` once when the drivers start; every power on then restores the kept results, and calibrates in full only if the restore fails. Where that write fails, the log says so and every power on calibrates in full; the radio works all the same. The results do not survive a restart, so the first power on of every boot calibrates. Kept results cost some performance: on 5 GHz the first rate after a join is lower with kept results than after a full calibration, on 2.4 GHz the same either way ([Network](../firmware/network.md#bands)), and the first scan after power on finds less: an active scan on one channel finds nothing, a passive scan on one channel may miss networks.

**Screen.** Module patch `0002` stops screen blank and unblank from switching the core's power; Rabbit's WMT driver powered the core at every unblank.

## Factory calibration

After the drivers start and before the core first powers up, the radio thread writes `WR-BUF:NVRAM` followed by the device's `APCFG/APRDEB/WIFI` file from `nvdata` to `/dev/wmtWifi`, as `wlan_assistant` does (`hand_calibration` in `driver.rs`, `wmt::to_calibration`). The partition is only read.

- gen4m keeps up to 8192 bytes of the file (`MAX_CFG_FILE_WIFI_REC_SIZE`) and applies it at every probe (`glLoadNvram`): the transmit power tables, the RF compensation and the band edges go to the chip with the manufacture data, and the file's flags decide whether 5 GHz is used.
- The copy has the MAC address (bytes 4 to 9) and the country code (bytes 10 and 11) zeroed. gen4m would use a given MAC address instead of the one it generates, and a code would select a country ([Channels and countries](#channels-and-countries)); with both zeroed, it generates its address and selects no country, as without the file.
- The R1's file is 2050 bytes, NVRAM version 1.8, both 5 GHz flags on.
- Module patch `0046` logs a file handed over before the first power on at info level; the driver has no device to update yet.
- Without the file the driver keeps its built-in settings, and the log records why.

## Files

The kernel looks in `/vendor/firmware` first (`firmware_class.path` on the command line). `r1 build` puts exactly these files in that ramdisk directory, which the radio reads the patches' heads from (`FIRMWARE` in `r1_funcore::wifi::wmt`, beside the patches' names):

| File | Bytes | Requested by | When | Why |
|---|---|---|---|---|
| `WMT_SOC.cfg` | 36 | WMT | drivers start | Required. r1metal's own (`crates/flasher/assets/WMT_SOC.cfg`): `coex_wmt_ant_mode=1`, `co_clock_flag=1`. Rabbit's four extra lines (GPS defaults, Bluetooth transmit power) cost a command per power on and change nothing |
| `soc1_0_ram_mcu_1_1_hdr.bin` | 105916 | WMT | every power on | The chip processor's firmware, at the start of shared memory |
| `soc1_0_ram_bt_1_1_hdr.bin` | 571053 | WMT | every power on | Bluetooth's firmware at `0x80000`; calibration runs both radios, and without it the processor faults 170 ms in |
| `soc1_0_ram_wifi_1_1_hdr.bin` | 196216 | WMT | every power on | WiFi's firmware at `0x140000` |
| `soc1_0_patch_mcu_1_1_hdr.bin` | 26574 | WMT | every power on | The classic patch, sent in 46 ms |
| `WIFI_RAM_CODE_soc1_0_1_1.bin` | 639704 | gen4m | every WiFi start | WiFi firmware `tk-0.2.0` of 2023-06-16 |
| `aw87xxx_acf.bin` | 850 | amplifier driver | 3.8 s after boot | Amplifier profiles; without the file the driver retries 41 times, a second apart |

Requested but absent: `regulatory.db` (cfg80211, once at boot, kernel patch `0053`) and gen4m's optional `wifi.cfg`, `wifi_sigma.cfg` and `txpowerctrl.cfg` (module patch `0016`).

All files except `WMT_SOC.cfg` are Rabbit's. `r1 build` extracts them from the pinned release's `super.img` without unpacking it to disk (`crates/flasher/src/build/blobs.rs`): the sparse image, `vendor_a` in the super partition table, `firmware/NAME` in its ext4. A test compares every file with a copy extracted by other tools. The device's flash is never read.

## Channels and countries

r1metal sets no country. The radio transmits first only on 2.4 GHz channels 1 to 11. On channels 12 and 13 and all of 5 GHz it only listens, and joins networks found there. It does not use channel 14. This matches the kernel's rule for an unknown country (`world_regdom` in `net/wireless/reg.c`), which allows listening on 14.

gen4m ignores the kernel's regulatory database and access points' country codes. It has rules for seven codes (US01, US, CN, NL, CZ, JP, TR), used only when a program sets one; none does ("No matched country code, use the default regulatory domain"). Module patch `0004` changes both defaults in `mgmt/rlm_domain.c`:

| Default | Rabbit's | r1metal's |
|---|---|---|
| Passive-only channels sent to the chip at every start (`arSupportedRegDomains_Passive`) | radar channels 52 to 64, 100 to 144 | all except 1 to 11 |
| Rules given to the kernel (`default_regdom_ww`) | no restrictions | 12, 13, 14 and all 5 GHz no-IR; 52 to 64 and 100 to 144 also DFS |

- Patch `0004` also stops a received beacon from lifting a no-IR mark (`REGULATORY_DISABLE_BEACON_HINTS`) and keeps channels 12 and 13 40 MHz wide.
- Every scan of the scan state machine is built in `scnSendScanReqV2` (`mgmt/scan_fsm.c`), where neither driver nor kernel enforces the marks. Module patch `0005` keeps a scan active only if it lists its channels and every one allows transmitting first; otherwise the scan goes out passive. This covers the driver's own scans (`mgmt/ais_fsm.c`: joining a network it has no beacon from, after four failed joins, after a lost link, roaming).
- The scheduled scan is built separately (`scnFsmSchedScanRequest`); r1metal never requests one.
- At every start gen4m enables on the wiphy only the channels of its no-country domain, 1 to 13 and the 5 GHz sets (`wlanUpdateChannelTable` from `wlanLoadManufactureData`, the last entry of `arSupportedRegDomains`). Channel 14 is therefore disabled regardless of the rules, and the kernel includes it in no scan.

The kernel marks, with the radio on or off once it has been on: 11 channels (2412 to 2462 MHz) to transmit on, 27 passive-only, 16 of those DFS, and channel 14 (2484 MHz) disabled. An active scan (`Scan::Active`) on channels 1, 6 and 11 goes out active; with any passive-only channel, or on all channels, it goes out passive.

## What gen4m does

- `NL80211_CMD_TRIGGER_SCAN` without an SSID is passive (`mtk_cfg80211_scan`, `SCAN_TYPE_PASSIVE_SCAN`). An active scan carries one empty SSID, which every network answers.
- It gives the chip no dwell time (`mgmt/ais_fsm.c`, all three time fields 0): the chip stays about 57 ms per channel in the first scan after power on and 108 ms after, against a beacon interval of 102.4 ms. All 38 channels take about 2.2 s in a first scan and 4.2 s after.
- `NL80211_CMD_CONNECT` with SSID, BSSID, frequency and an RSN element goes straight to authentication. gen4m writes its own RSN element into the association request (`assocGenerateConnIE`, `rsnGenerateRSNIE`), passes the RSN extension element through, and reports what it sent.
- SAE: gen4m asks the link's owning socket to authenticate (`NL80211_CMD_EXTERNAL_AUTH`) and passes up authentication frames registered with `NL80211_CMD_REGISTER_FRAME`, but only after an action frame type is registered too (`kalIndicateRxMgmtFrame`).
- Setting a management group key as default fails with `EFAULT` for a station (`mtk_cfg_set_default_mgmt_key`).
- It generates the MAC address from its prefix and the clock: the efuse holds none, and the calibration file it gets has the address zeroed.
- The chip does not doze between frames, whatever power mode the driver sets: an idle link costs as much as a busy one ([Network](../firmware/network.md#bands)). A powered core without a link costs nothing measurable on the gauge.
- Log levels: kernel patch `0013` and most module patches from `0006` on make the drivers log at the level of the message: a fault stays an error, routine news is info, a line per pass is debug, and the periodic status dumps are trace output (module patch `0023`). Module patch `0015` drops the chip's own log while no reader has its node open. None of them changes the lines that name networks found.
- Kernel patch `0012` makes the core's power off wait for the CONN bus protect acknowledge, instead of running into the recovery path and its register dump.

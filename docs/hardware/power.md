# Power

Current draw, suspend, the cell, the kernel's battery and charger drivers, the thermal policy, and where r1metal departs from Rabbit OS. The firmware's gauge and charge control are in [Battery](../firmware/battery.md).

## Measuring

Measure current with the coulomb counter, `/sys/class/power_supply/mtk-gauge/coulomb_get` (0.1 mAh), over a window with the console idle; 150 s resolves 2.4 mA. Single `battery_current_get` readings alias with loads that pulse at every display refresh, and frequent console reads add 40 to 50 mA. For idle figures run from the battery with USB off: an active USB port keeps the SoC out of SODI, its screen-on deep idle state. In the debug shell:

```
write /proc/mtk_battery_cmd/en_power_path 0
write /sys/devices/platform/mt_usb/musb-hdrc/cmode 0
```

`en_power_path 1` restores charging; USB, and with it the console, comes back at the next start. The trace event `mtk_idle_event/sodi` records each SODI entry. On a USB PD port the power path comes back on by itself: `rt-pd-manager` enables it at every change of the port's advertised current (`RpLvl Change`), which a port makes every few minutes on its own and at once when the battery's status changes, so a charge still running has to be held first, and a window on the battery lasts at most until the port's next change.

## Draw

| State | mA |
|---|---|
| Screen on, backlight off, USB off | 50 |
| Same, USB attached | 60 |
| Backlight 16, 64, 128, 255 of 255, USB off | 53, 64, 86, 122 |
| Stage theme idle, backlight 20 %, on battery | 65.5 |
| Stage theme idle, backlight 30 %, on the cable with the battery powering the device | 82 |
| Frame pacing test (`anim`), one frame per refresh | 110 |
| Video playing under the bar, 305x543 at 60 fps | 147 |
| Photo viewfinder, backlight 30 % | 226 |
| Video recording with viewfinder, backlight 30 % | 252 |
| Audio note recording, on battery | 101.5 |
| Home idle, backlight 40 %, battery powering the device, USB off | 85 |
| Same, microphone capture into an 8-block ring, which the kernel places in audio SRAM | 91 |
| Same, a 16-block ring, which does not fit in SRAM beside playback and goes to DRAM | 98 |
| Robot animated, compared with switched off | +2 to 4 |
| Suspended, by the coulomb counter | 3.3 to 3.6 |
| The look at the cell after a suspend ([Sleep and clock](../firmware/sleep.md#sleep)), for about 2 s | about 100 |
| The same after midnight, while the log packer compresses the day before ([Logs](../firmware/logs.md#archives)) | 114 to 136 |

A new frame every refresh costs about 28 mA over an idle screen, because it keeps the display pipeline out of idle. The thermal policy and logging cost nothing measurable. WiFi draw is in [Network](../firmware/network.md#bands), the modem's in [Modem](modem.md#measured-on-the-r1). Asleep the device draws about 84 mAh a day, under a tenth of a new 1000 mAh cell's charge.

Idle with USB off, the SoC is in SODI 91 % of the time, with 28 entries per second, ended by:

| Source | Per second | Detail |
|---|---|---|
| Wheel | 19 | the driver polls the hall sensor over I2C every 50 ms ([Device](device.md#parts)), on the unbound power-efficient workqueue, where the scheduler places each poll (patch `0006`) |
| Charger thread | 17 | about 170 I2C transfers every 10 s |
| Battery thread | 10 | one burst per sample |
| Display | 12 interrupts | each redraw keeps the pipeline out of idle for about 100 ms |
| CPU thermal zone | 6, with USB attached | polled every 200 ms by Rabbit's policy |
| GPU driver thread, vmstat | 2 | housekeeping |

While the robot is on the home screen, the accelerometer adds 50 wakeups per second ([System](../firmware/system.md#accelerometer)): each reading is a sensor hub interrupt that wakes three cores, the hub's `chre_kthread` and the main loop. With readings delivered every 100 ms it costs 3 to 4.5 mA. Each robot frame raises display interrupts, `ovl0` and `aal0` once and `rdma0` and `mutex` twice; without frames there are none, and the display's idle manager polls every 50 ms only while frames arrive. About 1 in 100 SODI entries meets such an interrupt as the core powers down; the SPM firmware then keeps the core up and the kernel logs it at info (patch `0147`).

## Suspend

| Part | Behaviour |
|---|---|
| Sleep controller (SPM) | turns the 26 MHz clock off; the modem, the connectivity core and the SSPM request no clock or rail |
| Wake sources (`wakesrc = 0xe87ddef`) | power button, pin interrupts (USB port, PMIC), system timer, USB, SCP, SSPM, four watchdogs, the modem and connectivity lines (the connectivity core unpowered, the modem as below) |
| Modem | with MOBILE DATA off, never started (`/sys/kernel/ccci/boot` reads `md1:0`); its radio rails are off, `vmodem` is on with no user. With it on, booted, its radio on only for an attach made or kept ([Modem](modem.md#in-r1metal)), its lines waking the SoC ([Modem](modem.md#power)) |
| Rails on while awake | `vcore`, `vproc`, `vsram_proc`, `vsram_others`, `vs1`, `vxo22`, `vio18`, `vio28`, `vaux18`, `vaud28`, `vusb33`, `vemc`, `vldo28`, `vmodem` |
| Rails off while awake | camera (`vcama`, `vcamd`, `vcamio`), connectivity (`vcn18`, `vcn28`, `vcn33_bt`, `vcn33_wifi`), SD card (`vmc`, `vmch`), `vibr`, `vefuse`, `vdram` |
| System timer | its interrupt is cleared at shutdown, so it does not wake the system at once (patch `0010`) |
| Coulomb counter | counts through a suspend, the gauge's sleep mode cleared at probe (patch `0201`, below); its interrupts are disabled across a suspend (patch `0011`) |
| SPM wake timer | wakes the system every 5401 s, MediaTek's period when no gauge sets one; the battery check where nothing else wakes the system during sleep |

Asleep the app and its cutoff do not run, the low-voltage comparators are off and so are the counter's interrupts: without the SPM's wake a sleeping R1 runs down to the PMIC's cut at 2.6 V, which syncs nothing and drains the cell deeper than the cutoff. The wakes cost about 1 mAh a day; a worn cell's last 5 % lasts about 6 h asleep, so a look every 90 min finds the end in time.

**The gauge's sleep mode.** `FG_SON_SLP_EN`, bit 8 of `FGADC_CON0` (0xD08), has the gauge sample a low current only now and then. The bootloader sets it, MediaTek's MT6765 suspend setting has it off, and the gauge driver clears it at probe (patch `0201`); nothing else in the driver writes it, and its zero-current detection writes bit 10 of the same register under its own mask. Set, it blinds the coulomb counter across a suspend: the counter stands still or moves either way while the cell is drawn from. Cleared, the counter falls 3.3 to 3.6 mA in every suspend and never rises.

At the first two suspends of a boot, MediaTek's code compares the PMIC with its reference table (`mtk_power_gs_6355_array.c`) and logs 12 registers as "Wrong Bit". None of them keeps on a clock buffer or rail the board would turn off, and two ADC clocks are held on by software.

| Registers | State |
|---|---|
| `DCXO_CW00`, `CW11`, `CW14` | clock buffers 3, 6 and 7 off (no NFC, no UFS), the connectivity and cellular buffers in co-buffer mode; only the SoC's buffer runs (`/sys/kernel/clk_buf/clk_buf_ctrl`) |
| `AUXADC_NAG_0` | gauge voltage averaging off |
| `BUCK_VPROC_CON0`, `LDO_VSRAM_PROC_CON0` | on: the CPU rails, whose voltages the SPM scales in its sequence |
| `BUCK_VCORE_OP_EN`, `LDO_VLDO28_OP_EN`, `OP_CFG` | not under the 26 MHz request |
| `AUXADC_NAG_CG0`, `LBAT_CG0` | left as the bootloader sets them: the clocks of the gauge's voltage alarm (NAG) and of the low-voltage comparators (LBAT) held on by software (`CK_SW_MODE`, `CK_SW_EN`); the table expects hardware control |
| `LDO_TREF_CON0` | TREF, the reference of the PMIC's measurements, on as the bootloader leaves it |

The USB-C port driver holds a wakeup source while a cable is attached (`tcpci_set_wake_lock_pd`) and for 500 ms after each port interrupt, so a cable plugged in during a suspend wakes the kernel and keeps it awake. The low-voltage comparators are off during a suspend (`lbat_service_suspend` in `pmic_lbat_service_mt6357.c`) and wake nothing. The kernel resumes in 49 ms, the panel init takes 599 ms (fixed delays in `ili9883_boe_mipi_hd.c`), and the screen lights 0.65 to 0.7 s after the button press.

**Sleep drain from the log.** After every wake that does not end the sleep, the firmware logs a reading taken 2 s after resume under the same load each time (`sleep: dark look`, see [Sleep and clock](../firmware/sleep.md)), with the coulomb counter and the time suspended. The counter's fall between two looks over the time suspended between them is the drain.

## The cell

| Fact | Measured |
|---|---|
| Rated | 1000 mAh |
| A full cell at rest | about 4.30 V, 45 min to 2 h after the charge |
| Temperature of the measurements on this page | 20 °C or more |

Under a load heavier than the waiting device's the voltage tells nothing of the end ([Battery](../firmware/battery.md#the-level)). A lasting heavy load near the end brings the 10-sample mean to the cutoff with charge left, and that run measures the capacity short.

## Battery in the kernel

- The kernel gauge algorithm (`mtk_battery`, started in every boot mode by patch `0003`) stores its state in the PMIC: the level in an RTC spare register, the state of charge and the NVRAM fail flag in `SYSTEM_INFO_CON0`. `r1 stock` leaves them; Rabbit's recovery writes the same registers.
- The coulomb counter counts in 0.1 mAh steps. The kernel starts it at zero at every boot (`fg_adc_reset`, from `battery_algo_init` through `fgr_dod_init` in `mtk_battery_algo.c`). The algorithm's other reset, in `fgr_imix_error_calibration`, runs only on `FG_INTR_DLPT_SD`, which nothing raises since patch `0206`. Charge put in while Linux does not run, by the bootloader before a boot on a charger, across process 1's restart out of the charging boot or during a flash on the cable, is never counted, and the level reads low by it until the next full charge. A run that crosses such a boot with no full charge between measures the capacity high by it.
- The firmware ignores the kernel's `capacity`, which starts from the level saved in the PMIC (on a first boot, one voltage looked up in the device tree's table) and then follows only the coulomb counter: voltage never corrects it, a full charge does not set it to 100, and it stops at 1 %. The tables (`arch/arm64/boot/dts/mediatek/bat_setting/mt6765_battery_table.dtsi`) are MediaTek's reference tables for this SoC, identical in all 159 values of each table; at an empty R1 cell they read 30 %, the level Rabbit OS shows.
- `status` reads `Discharging` for about 2 s when init starts, until the charger thread corrects it; `Charging` for one sample at a re-plug, before the kept hold applies; and `Not charging` for 10 s about 11 s after a boot on a charger, while it still charges at 57 to 140 mA.
- Low battery:
  - The kernel's gauge arms no power off at its own 0 % or 1 % (`SOC_ZERO_PERCENT`, `UISOC_ONE_PERCENT`) or on DLPT (`DLPT_SHUTDOWN`), and DLPT restarts nothing (patch `0206`). The firmware's cutoff decides ([Battery](../firmware/battery.md#cutoff)). Under it lies the kernel's one cut for an empty battery: every 10 s it reads the voltage and current, and powers off once the last 6 readings average under 3.35 V (`LOW_TMP_BAT_VOLTAGE_LOW_BOUND`) while discharging; a suspend empties its window (patch `0208`). It comes only where the firmware does not run. Every power off of the battery and charger drivers, this cut and those for heat at 60 °C, cold at −20 °C and a battery removed, syncs first (patch `0159`).
  - The PMIC's comparator samples the cell every 15 ms. The low-battery level goes to 1 on one sample under 3.25 V, to 2 on one sample under 3.1 V, and back to 0 once the cell stays above 3.4 V for 150 ms (`pmic_lbat_service_mt6357.c`, `mtk_low_battery_throttling.c`); the firmware reads it in `low_battery_protect_level`. At levels 1 and 2 the PPM's power throttling asks for a CPU budget of 600 mW (`ppm_pwrthro_low_bat_protect`), which the log shows as `[Power/PPM] (0x8)(600)`. Two more drivers share the comparator, which arms the highest low and the lowest high threshold of its users (`pmic_lbat_service_mt6357.c`): the fuel gauge at 3500, 3450 and 3200 mV, whose handler only logs, its enables never set (`mtk_battery_daemon.c`); and the Wi-Fi driver at 3550 and 3650 mV, which lowers its transmit power under the first and restores it over the second (`kal_bat_volt_notifier_callback` in gen4m's `gl_kal.c`).
  - DLPT checks every 10 s (20 s above 30 %); with the low-battery level at 2 on two checks in a row it tells the gauge the battery is empty (`ENERGY_EMPTY`), which since patch `0206` sets nothing off.
  - The PMIC cuts power at 2.6 V.
- DLPT computes, at each check, the current the cell can give from the voltage and its resistance figure `imix_r`, and hands it to its callbacks (`mtk_dynamic_loading_throttling.c`): the present current plus what takes the cell from its present voltage down to 3.40 V by that resistance (`DLPT_VOLT_MIN`, patch `0211`), the voltage the low-battery level clears at and the firmware's cutoff lies at. PBM shares DLPT's budget among the processors and the parts that reserve a share (`mtk_pbm.c`): the modem's 4000 mW (`MAX_MD1_POWER`) only while the modem has power, which is only while MOBILE DATA is on (patch `0150`); a battery voltage that fails to read counts as PBM's 4 V default (patch `0178`).
- `imix_r` comes from the bootloader at boot (`/chosen/atag,imix_r` and the auxadc's `imix_r` node: 170, 518 or 530 on different boots) and from the kernel from the first suspend on. Before the first suspend of a boot, and before each later one at a changed battery level, the kernel measures the cell twice with the PMIC's dummy load, about 100 mA for 50 ms, each sample taken 50 ms after the preceding step (patch `0146`), and keeps the mean when both pass. The auxadc exposes it as `in_resistance16_IMIX_R_raw` on its IIO device. A load lasting minutes sees more resistance than this: 0.95 to 1.5 Ω against 0.65 to 0.77 Ω.

## Charger

- A charge runs at a constant current, then at the constant voltage of 4.40 V, 4.39 V as the current falls (the chip's IR compensation); on a worn cell the constant voltage begins at about 77 %. The current then falls with a time constant of about 39 min and flattens at a floor that differs per unit and per charge: 14, 23 to 28, 45 to 51 mA. A fixed end current of 15 mA is not reached on most charges ([Battery](../firmware/battery.md#the-end-of-a-charge)).
- The MT6370 does not terminate a charge: the build removes `enable_te` from the tree's charger node (`crates/flasher/src/build/devicetree.rs`), because with termination on the chip ends a charge at 100 mA or more, well short of full, and restarts it every few seconds, putting 28.7 mAh more in over 30 min. Software JEITA sets only the voltage and the current (patch `0088`), by the temperatures in [Thermal](#thermal). A charge holds the constant voltage while the current tapers, until the firmware stops it at full ([Battery](../firmware/battery.md#charging)) or the chip's 12 h safety timer expires (`safety_timer` in the tree); the chip's own `Full` never comes.
- VBUS stays at 5 V: high-voltage charging is off (patch `0153`), since at the tree's 1 A limits a higher VBUS only heats the MT6370.
- `/proc/mtk_battery_cmd/current_cmd` takes two numbers (`mtk_charger.c`). A first number of 0 keeps the port's current limit; a second number of 1 stops charging, and 0 has the charger thread resume it unless software JEITA, the safety timer or VBUS holds it off (patch `0207`). The firmware writes `0 1` and `0 0`. The setting persists across unplug and replug until it is rewritten or the kernel restarts. The MT6370 sets `CHG_EN` again while its input is gone, as on a USB PD hard reset, which takes VBUS away for about a second; the driver keeps what `CHG_EN` was last set to and sets it so again as the power path comes back (patch `0205`).
- A held cell is not kept still: it gives 1 to 10 mA, the counter agreeing.
- A cable that switches the device on, after a power off with a charger attached or plugged into a device that is off, has the bootloader boot into power-off charging mode (boot mode 8, `androidboot.bootreason=usb`). Its kernel powers off when the charger is unplugged (`kpoc_power_off_check` in `mtk_charger.c`, syncing first, patch `0159`), and a power-on alarm of the RTC restarts it (`rtc-mt6397.c`). Process 1 restarts such a boot before the app starts ([System](../firmware/system.md#process-1)); a restart boots in mode 0 with the cable attached, as every `reboot` and `kernel_panic` boot on a computer's port does.
- An app restart on a cable charges up to the goal again from a held level under it: a restart cannot tell itself from a cable put in.
- Charging itself and the 60 °C power off are done by the kernel.

## Thermal

The kernel's thermal zones switch their coolers themselves; the policy is Rabbit's. At every boot, before the app starts, process 1 writes `crates/funcore/assets/thermal/thermal.conf` into the kernel as adapted by `r1_funcore::hw::power::thermal`. The file is Rabbit OS v0.8.293's `/vendor/etc/.tp/thermal.conf`, decoded (each byte is shifted by its position in the line, modulo 10): 49 pairs of a kernel file and the text to write to it. The firmware writes 43, and the kernel accepts all of them. The other 6 configure a second charger chip the board does not have; the kernel creates their files only when the tree's charger node names one (`charger_configuration` is 0). Software JEITA takes its temperatures from the tree's charger node.

| Sensor | At | Effect |
|---|---|---|
| Cell | below 0 °C | charging stops (software JEITA), until 3 °C |
| Cell | 0 to 45 °C | charge target 4.40 V at 1 A |
| Cell | 45 °C | at most 4.20 V and 500 mA (software JEITA): each charger pass lowers the voltage 10 mV while the current is over 250 mA, raises it 10 mV while under 150 mA, and holds it between (patch `0160`), never above 4.20 V; until under 43 °C; no full is read there ([Battery](../firmware/battery.md#the-end-of-a-charge)) |
| Cell | 55 °C | charging stops, until under 52 °C |
| Cell | 60 °C | power off, by the battery driver and zone `mtktsbattery` |
| Board | 43 to 46 °C | the die target falls from 85 to 55 °C, 10 °C per degree |
| Board | 45 °C | charge throttle: from 2000 mA, about 200 mA less per degree, then 50 mA less per second above 46 °C; the charger uses the lower of this and JEITA's current (patch `0007`); lifted below 44 °C. A throttled charge can flatten before the cell is full and read full a little short |
| Board | 53 °C | `/proc/driver/cl_cam_status` reads 1 (0 again below 48 °C); nothing is switched off, the camera code must check it |
| Board | 90 °C | power off |
| Board | 100 °C | reset |
| Die | 55 °C | the CPU power budget holds the die at its target, never below 685 mW (400 mW with the board at 69 °C or more) |
| Die | 117 °C | reset |

The board thermistor, `mtktsAP`, is closest to the case temperature. The die temperature is the highest of five SoC sensors. One line differs from Rabbit OS: `clctm` is written with mode 1, which computes the die target in the kernel from the same numbers; mode 2 takes it from Rabbit's closed daemon and without it stays at 75 °C. The policy adds no idle current.

To fake a temperature: `write /proc/mtktz/ZONE Tfake MILLIDEGREES` (`Tfake -275000` stops it) or `write /sys/class/power_supply/battery/temperature DEGREES` (`65535` stops it). `/proc/thermlmt` shows the CPU and GPU budgets and the die target; `/proc/bcctlmt` shows the charge limit.

## What powers the device off

| Who | When | Syncs first |
|---|---|---|
| Firmware cutoff | discharging, and the 10-sample mean under 3.40 V or the low-battery level at 2 ([Battery](../firmware/battery.md#cutoff)) | yes, after saving the gauge file |
| Firmware, asked | POWER OFF or REBOOT on the power screen, FASTBOOT on the debug screen, the console's `poweroff`, `reboot` and `bootloader`, three wrong passphrases | yes |
| Process 1 | a boot the bootloader started to charge, restarted before the app starts ([System](../firmware/system.md#process-1)) | yes |
| Battery driver, `low_bat_net_work` (patch `0208`) | 6 readings 10 s apart average under 3.35 V while discharging | yes |
| Battery driver, `power_misc_routine_thread` | the cell's thermistor reads 60 °C or more at a battery update; the conversion stops at 60 °C, so a shorted thermistor reads so too | yes |
| Charger driver | with a cable attached, the cell at −20 °C or colder; in power-off charging, the cable pulled | yes |
| RTC driver | in power-off charging, the power-on alarm due: `kernel_restart("kpoc")` (`rtc-mt6397.c`); process 1 leaves that boot before it can come | no |
| Thermal policy | the cell at 60 °C (`mtktsbattery-sysrst`, `kernel_power_off`; its zone is polled every second, the battery driver's check above at each battery update, at least every 60 s); the board at 90 °C and the PA zones at 120 °C (`mtk-cl-kshutdown`, `machine_power_off` at once); the board at 100 °C, the CPU at 117 °C, the PMIC at 136 °C, the charger and WMT zones at 120 °C (the other `-sysrst` coolers, `BUG()`) | no |
| Kernel oops or panic | any, an ended process 1 included: mrdump resets the device at once through PSCI | no |
| Hardware watchdog | a kernel stalled 31 s, where the bootloader left the watchdog running; the kernel pings it every 15.5 s | no |
| PMIC | the cell under 2.6 V: power lost, then the PMIC starts the device again (`androidboot.bootreason=2sec_reboot`); the power button held (one-key reset, `mt6357.dtsi`) | no |

`kernel_power_off` sets the RTC's power-off flag and restarts through PSCI; the bootloader then powers off (`kernel/reboot.c`). During a suspend only the PMIC, the SPM's wake every 5401 s and the watchdog run. Four power offs are compiled in and never fire: `LOW_BAT_VOLT`, which nothing raises on this gauge; the battery plug-out, whose interrupt only gauges from V2000 on request; the thermal core's `orderly_poweroff`, as no zone has a critical trip; and the MT6370's `poweroff` and `shipping_mode` sysfs files, which take a magic number nothing writes. The charger type driver's unplug power off (`CONFIG_TCPC_CLASS`), `check_battery_exist` (`#ifdef FIXME`) and the DRAM shmoo reboot (`RUNTIME_SHMOO`) are compiled out.

## Against Rabbit OS

Rabbit OS runs the same kernel drivers and thermal policy; r1metal departs from them where its patches and firmware decide:

| | Rabbit OS | r1metal |
|---|---|---|
| Battery level | MediaTek's gauge on MediaTek's reference tables for another 1000 mAh cell; at an empty R1 cell it shows 30 % | its own gauge: the charge counted out since a full charge against the charge this device measured from full to its cutoff; the kernel's `capacity` is ignored ([Battery](../firmware/battery.md#the-level)) |
| The counter asleep | the gauge's sleep mode stays as the bootloader sets it, and the coulomb counter stands still or moves either way across a suspend | patch `0201` clears it, and the counter counts through every suspend ([Suspend](#suspend)) |
| End of a charge | software JEITA switches the chip's termination on at 100 mA: the chip ends a charge short of full and restarts it every few seconds | termination off (patch `0088`, `enable_te` removed from the tree); the firmware reads full from the current's taper and stops the charge ([Charger](#charger)) |
| Warm cell, 45 to 55 °C | charged to 4.40 V while the current lies between 150 and 250 mA: the driver leaves the range's voltage at 0 there | at most 4.20 V, held where the current lies between 150 and 250 mA (patch `0160`) |
| Charging voltage | high-voltage charging on from the charger's probe | 5 V only (patch `0153`) |
| A charger told to stop | the MT6370 charges again after a USB PD hard reset while the kernel reports `Not charging` | stays stopped (patch `0205`) |
| Empty battery | the kernel powers off at its own 0 % or 1 % or on DLPT, and DLPT restarts the system after four checks under 3.1 V, none of them syncing first | the firmware decides, keeps what it keeps and syncs ([Battery](../firmware/battery.md#cutoff)); the kernel's own cut lies under it, at a minute's mean under 3.35 V (patch `0208`); the kernel's power offs, that cut and those for heat, cold and a battery removed, sync first (patch `0159`) |
| The power budget | DLPT budgets the cell down to 3.1 V, under the 3.25 V of the low-battery level and 0.5 V over the PMIC's cut: a worn cell that carries the budget is taken under the cut by a load the budget does not count | down to 3.40 V (patch `0211`, [Battery in the kernel](#battery-in-the-kernel)) |
| The die's target temperature | set by Rabbit's closed daemon (`clctm` mode 2) | computed in the kernel from the same numbers (`clctm` mode 1, [Thermal](#thermal)) |
| The connectivity core | powered at every screen unblank by Rabbit's WMT driver | powered only while a thread holds the network (module patch `0002`, [Network](../firmware/network.md)) |
| The modem | started at every boot by `ccci_mdinit` (`init.cccimdinit.rc`) | started only with MOBILE DATA on; its radio on only for a stay that WiFi cannot serve ([Network](../firmware/network.md#mobile-data)) and for the attach kept after one or held by the console ([Modem](modem.md#in-r1metal)) |

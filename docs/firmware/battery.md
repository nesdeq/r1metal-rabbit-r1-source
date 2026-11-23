# Battery

The firmware counts the battery level itself from the kernel's raw readings. It uses nothing from the stock gauge, whose device tree tables are MediaTek's reference tables for another cell ([Power](../hardware/power.md#battery-in-the-kernel)).

| Part | Code |
|---|---|
| Gauge and cutoff | `r1_funcore::hw::power::battery` |
| Charge control | `r1_funcore::hw::power::charge` |
| Usage and time to empty | `r1_funcore::hw::power::usage` |
| Battery thread's decisions: each sample's steps in order, what is logged, switched, kept and shown | `r1_funcore::hw::power::battery::Meter` |
| Battery thread: sysfs reads, the charge switch's write, the gauge and usage files, power off | `crates/impshell/src/power/battery.rs` |

The battery thread samples once per second: `bat_vol_get` (mV), `battery_current_get` (0.1 mA, positive while charging) and `coulomb_get` (the coulomb counter, 0.1 mAh) under `/sys/class/power_supply/mtk-gauge/`, and `temperature` (°C) and `status` under `/sys/class/power_supply/battery/`. It writes nothing to these files. It reads the kernel's boot from `/proc/sys/kernel/random/boot_id` once at start. The level is shown and told only once the data partition has settled; every sample goes to the power log from the first on, and to the interface, which takes its cable and, asleep, its look at the cell from every one ([Sleep and clock](sleep.md#sleep)).

## The level

The level is the charge counted out since the last full charge (`out`) against the charge this device gives from full to its cutoff (`C`): `(C − out) / C`.

| Point | Definition |
|---|---|
| 100 % | a full charge, read from the charge's own current ([The end of a charge](#the-end-of-a-charge)) |
| 0 % | the [cutoff](#cutoff), where the firmware powers the device off |

**Counting.** `out` moves with every change of the coulomb counter within a boot, awake and across suspends, and never goes under 0: the cell holds no more than full. The gauge driver clears the gauge's sleep mode so that the counter counts through a suspend (patch `0201`, [Power](../hardware/power.md#suspend)). The kernel starts the counter at zero at every boot.

**Before the first full charge** no level is known: the battery icon shows `battery_unknown` off the cable and `battery_status_good` on one, the power screen's lines that need a level say so, and every cable charges to full. The interface is told the supply all the same, so a cable keeps the device awake as always.

**Near the cutoff** the voltage decides. While the battery carries a load lighter than 150 mA (`LIGHT_CURRENT`, the device waiting) and the 10-sample mean voltage is under 3.50 V (`NEAR_END_MV`), the level is at most 5 % (`NEAR_END_LEVEL`), falling linearly to 0 % at 3.40 V. This cap holds until the next charge. Under heavier loads the voltage tells nothing of the end. While discharging away from the cutoff the level is at least 1 %.

**On a charge** not yet read full the level is at most 99 %.

**The charger's word** is read once a sample (`Meter::charger_of`), and the gauge, the charge's control, the device's use and the supply the interface is told all go by that reading; a word that could not be read leaves the charger as it last stood, and the log tells it as `?`. For the gauge the word counts once it has stood 30 s (`SETTLE_MS`); only "Discharging" counts at once. A cable put in on a held battery reads `Charging` for a sample before the kernel applies the hold it kept, and a boot on a charger reads `Not charging` for 10 s while still charging.

**Shown level.** It moves at most 1 % per 10 s (`SLEW_MS`), counting time asleep, in either direction. Near the cutoff it follows the level at once. The log records each full charge and how it was read, and each cutoff with the capacity it measured.

**Capacity.** Once after a full charge, where the discharge ends, `C` moves to the charge counted out since: where the voltage under a light load brings the level on show to 0 % (`EMPTY_LEVEL`, half a percent), or at the cutoff, whichever comes first: the first measurement replaces the 500 mAh assumed until then (`ASSUMED_CAPACITY`, about what a worn cell gives; the R1 is rated 1000 mAh), later ones move it a share of 1/runs, a quarter from the fourth on (`MAX_RUNS`). A figure outside 100 to 3000 mAh (`CAPACITIES`) is not taken. Until a run has measured it, the power screen marks `C` as assumed, and a gauge file whose runs are 0 is read with the assumed `C`. On its first run a cell that gives more reads 1 % from 500 mAh on until the voltage ends the run, which measures it.

## Charging

| Cable | Charged to |
|---|---|
| Charger | 100 % |
| Computer or hub | 80 % (`CEILING`). A touch on the battery icon while held there charges to 100 % until the cable is pulled; a touch while charging to 100 % holds the battery at its current level |

A cable counts as a computer's from the first `USB_STATE=CONFIGURED` until it is unplugged, otherwise as a charger's. At the goal, charging stops and the device runs from the cable with the battery held (`Charge::is_held`). A held battery charges again 5 % below its goal, at 95 % or 75 % (`RESUME`). A held cell is not kept still: it gives 1 to 10 mA ([Power](../hardware/power.md#charger)), which is counted, so the level falls on the cable until the charge resumes.

The thread decides at every sample and at once after a touch. It writes the kernel's charge switch only while a cable is attached and the decision changed. Unplugging writes nothing: the switch persists to the next cable, which is decided as it comes ([Power](../hardware/power.md#charger)).

### The end of a charge

The charger chip does not terminate a charge ([Power](../hardware/power.md#charger)); the gauge reads the end from the charge's current. While the charger's word is `Charging` and the system runs, it compares, at each whole minute, the mean of the last 10 one-minute means (the window) with the mean of the 10 before.

The charger holds its voltage where the window's voltage is at least 4.35 V (`CHARGER_MV`, the 4.40 V CV less its IR compensation), rose 2 mV or less, the current is positive and the cell is below 45 °C. Above 45 °C software JEITA lowers the CV voltage.

| End | Condition |
|---|---|
| Flat | at the charger's voltage the window fell less than 5 % against the previous window, two minutes in a row: the current has settled on its floor, 14 to 48 mA on R1 cells |
| Low | at the charger's voltage the window's current is under 15 mA (`LOW_CURRENT`), however it still falls |
| Long | 180 minutes at the charger's voltage, counted once two windows stand: about 200 minutes into the charge |

At the end `out` becomes 0, and charging stops where the goal is 100 %. A current that settles for a while and then falls on reads full at the first settling, a little short of the last of the charge. A suspend restarts the watch.

## Cutoff

The battery thread powers the device off when the battery discharges on average over 10 samples and either:

- the 10-sample mean voltage is below 3.40 V (`CUTOFF_MV`, where the kernel's low-battery level clears), or
- `/sys/devices/platform/low_battery_throttling/low_battery_protect_level` is 2 (`KERNEL_LEVEL_EMPTY`): the PMIC's comparator saw the cell under 3.1 V ([Power](../hardware/power.md#battery-in-the-kernel)), which a cell does only at its end.

Before powering off it saves the gauge file, waits for the settings' keeper to keep what it was handed and flushes the logs, for at most 10 s together (`power::SHUTDOWN_TIMEOUT`), and syncs. The 10-sample window ignores short load bursts. The firmware decides that the battery is empty; the kernel's own cut, at a minute's mean under 3.35 V, lies under it and comes only where the firmware does not run ([Power](../hardware/power.md#battery-in-the-kernel)). A battery that charges on average never cuts off. The cutoff cannot run during a suspend; the SPM's wake timer wakes the system so that it can ([Power](../hardware/power.md#suspend), [Sleep and clock](sleep.md)).

## Camera reserve

Off the cable, with a known level under 10 % (`CAMERA_RESERVE`, `is_camera_reserve`), the camera does not power up, the viewfinder of the window that scans a code included: its load takes a cell that low to the cutoff or browns the device out. With a measured capacity the shown level lies within 1.5 points of what is left down to the last 6 % ([Test data](#test-data)). The main loop hands the camera the reserve at every pass (`Camera::set_reserve`); a camera that runs as the level falls under 10 % runs on, the cutoff guarding it. Without a known level, and on a cable, nothing is held back ([Camera](camera.md#camera-session)).

## Time left

Off the cable, the time to the cutoff is the charge left, `C − out`, divided by the recent discharge rate (`Usage::time_to_empty`). The rate is the counted charge, awake and asleep, over the elapsed time, both weighted down exponentially with a 6 h time constant (`DRAW_MS`). On the cable the rate is frozen. The time between two runs of the app adds nothing to it.

While charging, the time to the goal (`Figures::time_to_goal`) is the charge to the goal at the mean current of the last 10 samples; once all is counted in while the end of the charge is still to be read, the screen says `NEARLY AT` and the goal.

## Files

Both files are in the data partition's private directory, `.r1metal/battery/`. They are written:
- when the shown level changes;
- when the gauge learns something;
- when a stretch begins (a cable plugged in or pulled);
- before every reboot or power off the app makes, which asks the thread for it (`Meter::save`);
- before the cutoff.

The battery thread alone writes them.

The thread checks at every sample whether the partition is mounted, never blocking while it is formatted or mounted, and loads the files once it is. A gauge file that does not parse is discarded: the level waits for the next full charge, and the capacity starts from the assumed 500 mAh.

`gauge`, one `key value` line per entry:

| Key | Content |
|---|---|
| `out` | charge counted out since the last full charge, 0.1 mAh; absent before the first |
| `capacity` | charge from full to cutoff, 0.1 mAh |
| `runs` | cutoffs that measured it, up to 4 |
| `cut` | 1 where the cutoff has measured the discharge since the last full charge, so that a device switched on again before a charge measures it once |
| `counter`, `boot` | the coulomb counter when written, and the kernel's boot it was read in |

An app restart in the same boot adds what the counter counted since the file was written. A new boot starts its counter at zero and adds nothing for the time between, as a device that is off draws next to nothing. A boot that could not be read is kept as `?` and counts as a new one, whatever boot the file was kept in (`UNKNOWN_BOOT`).

`usage`:

| Key | Content |
|---|---|
| `plugged`, `level` | the current stretch: 1 on the cable, 0 off it; the level at its start, absent where none was known |
| `lasted`, `awake`, `fed` | the stretch's length, time awake and time charging, ms |
| `from OUT LASTED FED` | where its count began |
| `draw CHARGE TIME` | weighted discharge off the cable, 0.1 mAh and ms |
| `saved N` | UTC second written; the next run adds the time since to the stretch |

## Battery rundown

The debug screen's `RUNDOWN` switch makes a discharge from full to cutoff a steady load:
- maximum backlight (the setting is unchanged);
- no idle sleep;
- a thread (`crates/impshell/src/rundown.rs`) that holds a network stay, starting a new one when it ends and a second after a join or a stay that failed (`rundown::Step`, `Run::next_step`), and pings 1.1.1.1 every second over a raw ICMP socket, each echo under the identifier `r1` (`r1_funcore::net::icmp`; echo logic and screen state in `r1_funcore::domain::rundown`).

The gauge reads the run like any discharge, and its cutoff measures the capacity. The rundown keeps running when its screen closes, stops when switched off, at sleep or at the cutoff, and is off at every start. The log records each stay, each change between answered and unanswered, and at the end the seconds run and the echoes sent and answered.

## Test data

Two recordings of a worn R1 cell, one sample per second, are replayed by `battery/tests.rs`; both predate patches `0201` and `0205` to `0208`.

- **`crates/funcore/assets/battery/rundown-20261013.txt`:** a discharge from a topped-up cell to the cutoff. The tests count it from full:
  - the voltage under a light load reads the cell empty near its end, which measures the capacity;
  - with the assumed capacity, the level never rises during the discharge and is at most 1 % at the end;
  - with the capacity measured, it stays within 1.5 points of what is left down to the last 6 %, and reads 0 % at the end.
- **`crates/funcore/assets/battery/taper-20261021.txt`:** a CV phase with termination off, its current settling on a floor (`TAPER_FLAT_S`). The test checks that the gauge reads full within 30 min after the current stops falling, not before, and shows no level before it.

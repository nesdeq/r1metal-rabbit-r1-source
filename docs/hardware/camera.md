# Camera

The Hi846 sensor, the MIPI receiver (seninf), the CAMSV raw DMA, and Rabbit's tuning data for the sensor. How the firmware drives them is in [Camera](../firmware/camera.md). Kernel paths are relative to `kernel/src/drivers/misc/mediatek/`.

## Sensor

The kernel driver is `imgsensor/src/common/v1/imgsensor.c` with `hi846_mipi_raw/hi846_mipi_raw_Sensor-4lane.c` (the Makefile builds the 4-lane file). The kernel handles power, the master clock, the I2C register tables, exposure and gain.

- Output: raw Bayer, Gb first, 10 bit, black level 64. 4 data lanes, master clock 24 MHz, I2C write id 0x40 at 400 kHz. Fixed focus.
- Registers: gain 1x to 16x in 0x0077 (0 is 1x); shutter in lines (minimum 6, margin 6) in 0x0073 and 0x0074; frame length 0x0006; line length 0x0008. Readback: output size 0x0A12 and 0x0A14, streaming 0x0A00. The id 0x0846 reads from 0x0F17 (high byte) and 0x0F16; the firmware refuses any other id.
- The driver declares to the receiver: `MIPI_OPHY_NCSI2`, 4 lanes, `SensorPacketECCOrder` 1, settle delay 85 in manual mode (stock ignores it), pixel rate 288000000 (capture), 144000000 (preview, slim video) and 72000000 (high speed).
- The camera regulators have an over-current notifier that kills the process holding the sensor open.

| Mode | Scenario id | Size | pclk Hz | Line length | Frame length | fps | Mbit/s per lane |
|---|---|---|---|---|---|---|---|
| preview | 0 | 1632x1224, 2x binned | 288000000 | 3800 | 2526 | 30 | 360 |
| capture | 1 | 3264x2448 | 288000000 | 3800 | 2526 | 30 | 720 |
| capture, slow | | 3264x2448 | 288000000 | 3800 | 5052 | 15 | 720 |
| video | | 3264x2448 | 288000000 | 3800 | 2526 | 30 | 720 |
| high speed | 3 | 640x480, a quarter each way, cropped | 288000000 | 3800 | 631 | 120 | 180 |
| slim video | 4 | 1280x720, 2x binned, cropped | 288000000 | 3800 | 842 | 90 | 360 |

The firmware uses preview, capture, high speed and slim video.

- fps = pclk / (line length x frame length), so the sensor reads 75789 lines per second. A longer frame lowers the rate; the 90 fps mode also runs at 60 fps, the 120 fps mode at 30.
- Full size is limited to 30 fps: a frame needs at least 2448 lines, and the link is at its ceiling (4 x 720 Mbit/s at 10 bit is 288 MP/s; 8 MP at 60 fps needs 479 MP/s). A catalog listing shows a 1080p crop at 60 fps; this driver has no table for it.
- Mode windows on the sensor (`imgsensor_winsize_info`): preview is the whole sensor halved, capture the whole sensor, slim video the middle 2560x1440 halved, starting at pixel 352, line 504, high speed the middle 2560x1920 quartered, starting at pixel 352, line 264. The tables agree: high speed reads lines 0x148 to 0x8C7 (`Y_ADDR_START_VACT` 0x0026, `Y_ADDR_END_VACT` 0x002C, the array's first line being 0x40 as preview's 0x0040 to 0x09CF shows) and starts its output at column 88 (`X_START` 0x0804, in output pixels; slim video's 176).
- How a mode reduces the sensor, by the tables against mainline Linux's names for the registers (`drivers/media/i2c/hi846.c`): across, the sensor bins (`HBIN_MODE` 0x0A22: 0 at full size, 0x0100 in preview and slim video, 0x0200 in high speed, each binned mode with bit 5 of the ISP word 0x0A04 set, without which only part of each line carries data, [Test pattern call](#test-pattern-call)). Down, the odd read increments are 0x11 at full size, 0x33 in preview and slim video and 0x77 in high speed, each beside an even increment of 0x11 (`Y_ODD_INC_VACT` 0x0032, `Y_ODD_INC_FOBP` 0x002E), which read as SMIA's increments of 1, 3 and 7 take every line, two of every four and two of every eight; and `BINNING_MODE` 0x000C changes with them (0x0022, 0x0122, 0x0322) with no bit named, so whether the lines between are summed in or left out cannot be told from the registers.
- Full size is as sensitive as binned preview: a photo at the preview exposure has the same brightness.

### Driver calls

`imgsensor_ioctl` on `/dev/kd_camera_hw`. Feature and scenario ids are `ACDK_SENSOR_FEATURE_ENUM` and `MSDK_SCENARIO_ID_ENUM` in `imgsensor/inc/kd_imgsensor_define.h`.

| Call | Number | Argument, little endian |
|---|---|---|
| `KDIMGSENSORIOC_X_SET_MCLK_PLL` | `_IOWR('i', 60, 12)` | on (u8, 3 pad), MHz (u32), TG (u8, 3 pad) |
| `KDIMGSENSORIOC_X_FEATURECONCTROL` | `_IOWR('i', 15, 24)` | sensor index (u32), feature id (u32), parameter pointer (u64), pointer to the length as u32 (u64) |
| `KDIMGSENSORIOC_X_CONTROL` | `_IOWR('i', 20, 24)` | sensor index (u32), scenario id (u32), two pointers (0 is accepted) |
| `KDIMGSENSORIOC_X_GET_CSI_CLK` | `_IOWR('i', 85, 4)` | meter number in (u32), kHz out; numbers are positions in `ckgen_array` of `drivers/clk/mediatek/clk-mt6765.c`, counted from 1 |
| `KDIMGSENSORIOC_DFS_UPDATE` | `_IOWR('i', 90, 4)` | an ISP clock level in MHz; not used |
| `KDIMGSENSORIOC_GET_SUPPORTED_ISP_CLOCKS` | `_IOWR('i', 95, 44)` | count (u8, 3 pad), ten levels in MHz (u32) |
| `KDIMGSENSORIOC_GET_CUR_ISP_CLOCK` | `_IOWR('i', 100, 4)` | level in MHz (u32) |

Features the firmware calls (`crates/impshell/src/camera/sensor.rs`):

| Feature | Id | Parameter |
|---|---|---|
| `SET_DRIVER` | 3107 | 4 zero bytes; probes the sensor and returns the driver index |
| `OPEN` | 3105 | 8 bytes; powers the sensor and writes the init table |
| `CLOSE` | 3106 | 8 bytes |
| `SET_ESHUTTER` | 3004 | lines (u64) |
| `SET_GAIN` | 3006 | gain x 64 (u64) |
| `SET_STREAMING_SUSPEND`, `SET_STREAMING_RESUME` | 3103, 3104 | 8 bytes; a nonzero resume parameter sets that shutter first |
| `GET_REGISTER`, `SET_REGISTER` | 3012, 3011 | address and value (two u32); a read returns a byte, a write is 16 bits |

The driver also has `SET_MAX_FRAME_RATE_BY_SCENARIO` (scenario and fps x 10, two u64), `GET_MIPI_PIXEL_RATE`, `GET_CROP_INFO` and `SET_TEST_PATTERN` (two u64; see below).

- Order: master clock on (1, 24 MHz, TG 0), `SET_DRIVER`, `OPEN`, work, `CLOSE`, master clock off, close the node. Closing the node powers everything off.
- Timing: power on takes 47 ms, the init table 105 ms. The probe in `SET_DRIVER` powers the sensor up and down once (79 ms). A mode call takes about 5 ms. Each streaming call ends in a 10 ms busy wait in the kernel.
- After any mode starts, the driver says to skip 3 frames (`pre_delay_frame`, `cap_delay_frame`). Shutter and gain take effect in the same frame.
- The gain call writes `gain / 4 - 16` to 0x0077, so the gain moves in steps of 1/16 (256 gives 0x30). The sensor treats the 16-bit write at the odd address as a write of the word at 0x0076.
- A shutter longer than the frame minus 6 lines stretches the frame to shutter plus 6 (5000 lines give 5006). A shorter shutter restores the mode's frame length. A mode call resets shutter and frame length to the table values and leaves the gain unchanged.
- The driver never shortens a frame. `set_max_framerate_by_scenario`, `set_video_mode` and every `SET_ESHUTTER` restore the table's 2526 lines. `SET_MAX_FRAME_RATE_BY_SCENARIO` with 600 on slim video sets frame length 1263 (60.008 fps); the next mode call undoes it. A frame length written to 0x0006 while streaming shows in the frames three frames later.
- Clock meters: `f_fseninf_ck` (27) 208000 kHz, `f_fcamtm_ck` (30) 208000 kHz, `hf_fmm_ck` (3) 228490 kHz, unchanged while streaming; `f_fcamtg_ck` (7) 0 with the master clock off, 24019 kHz on. Every meter call logs one kernel line.
- ISP clock levels are 457, 312 and 228 MHz; 228 is in force with no request. Stock picks the lowest level for which twice the level in Hz exceeds the sensor's pixel rate, and calls only when that differs from the level in force. For this sensor that is 228, so no clock call is needed.

### Test pattern call

`set_test_pattern_mode` writes the sensor's ISP control register 0x0A04 as a whole word: 0x0141 for on, 0x0142 for off (and 0x020A as 0x0200 or 0). The mode tables write 0x014A for full size and 0x016A for preview, slim video and high speed. Either call therefore clears bit 5 in a binned mode and bit 3 in every mode.

After the call (with 1 or 0), a binned mode still sends full-length lines with good checksums, but only the first pixels of each line carry data: 640 of 1280 in slim video, 832 of 1632 in preview, 192 of 640 in high speed. The receiver delivers the zeros to CAMSV. Full size is unaffected.

- Never make this call, not even to switch the pattern off. A mode change restores the correct word.
- A pattern in a binned mode would be the mode's own word with bit 0 set, plus 0x020A, written through `SET_REGISTER`.

## Tuning data

Rabbit's tuning for the sensor is in `vendor/lib64/hi846_mipi_raw_tuning.so` of the stock image, as tables of MediaTek's camera NVRAM (layout in MediaTek's published `camera_custom_nvram.h`; 512 means 1.0 in all of them). The values used are copied into `r1_funcore::hw::camera::colour` and `r1_funcore::hw::camera::shading`; the library is neither loaded nor shipped. All values are for a reference module: the sensor driver reads no calibration from the sensor, and its list of calibration memories (`cam_cal_list.c`) has no Hi846, so no per-unit calibration reaches the firmware.

| Table | Offset | Contents |
|---|---|---|
| White balance per light source | `0x250480`, seven identical copies | red and blue gains that make white grey: horizon 542, 1231; A 650, 1100; TL84 852, 976; CWF 901, 992; DNP 947, 800; D65 1097, 692 |
| Preset balances | `0x250628` | daylight 1028, 731; cloudy 1205, 633; shade 1293, 594; four more |
| Colour matrices | `0xbc290`, repeated at `0xbc710` | five, warmest light first, each three rows of 13-bit signed fields in MediaTek's register layout (two entries in a row's first word, the third in the second) |
| Gains of the matrices' lights | `0xbc890` | 600, 1492; 734, 1289; 952, 1069; 1018, 1046; 1318, 713 |

`colour.rs` builds the line of whites from horizon, A, DNP (5000 K), daylight, D65, cloudy and shade, and uses the five matrices with their gains. The fluorescent sources lie 5 to 9 % above that line. A second set of four milder matrices at `0xbc1f0` is not used. The two gain tables differ on green by about 14 %.

**Lens shading.** The library's default data routine (`impGetDefaultData`) copies one block per data type. Type 2, shading, is 1050652 bytes at `0x104120`:

| Part | Contents |
|---|---|
| First 16 bytes | version 5, sensor `0x846`, 17 grid points each way, 3264x2448 pixels |
| Three tables of 24576 bytes | each 16x16 blocks over the sensor, row-major; a block has four channels (B, Gb, Gr, R), a channel 12 sign-magnitude 16-bit numbers |
| A fourth table, after an empty one | unknown content; the rest is empty |

A channel's 12 numbers are a polynomial of its gain inside the block; 4096 means 1.0. The twelfth number is the gain at the block's first corner. Twice the sum of the third, seventh and tenth is the step to the next block's corner along the row. Twice the sum of the sixth, ninth and eleventh is the step to the corner below. Twice the sum of all eleven is the step to the diagonal corner. These weights fit every block of the three tables by least squares to within 3/4096 along a side, so all 17x17 grid gains are known, including the outer ones.

| Table | Green at the sensor's first corner | Red / green there | Blue / green there |
|---|---|---|---|
| first | 2.95 | 1.06 | 0.93 |
| second | 3.20 | 1.12 | 0.89 |
| third | 3.21 | 1.09 | 0.92 |

The lens passes about a third as much light at the corners as in the middle; red a little less than green, blue a little more. The three tables are for three lights; which ones is unknown. `shading::REFERENCE` is their mean, with the two greens merged: 17x17 gains per colour in 1/1024 units, 1024 to 1026 in the middle, 2919 to 3700 at the corners. Each table differs from the mean by at most 5 % in a colour's share at the corners.

## Kernel ISP driver

`cameraisp/src/mt6765/camera_isp.c` (with `inc/camera_isp.h`) never configures a pipeline:

- `ISP_ioctl`, magic `'k'`: register read and write (arrays of module, address, value), interrupt wait, clear, user keys, request marks, buffer control, ION import and free, larb MMU control, reset by module, wakelock, `ISP_VF_LOG`, clock and QoS calls.
- `ISP_ReadReg`, `ISP_WriteRegToHw`: modules are `ISP_DEV_NODE_ENUM` (CAM_A, CAM_B, CAMSV0 to CAMSV5, DIP_A, UNI_A); seninf is module 0xFF and 0xFE.
- `ISP_mmap` maps CAM_A, CAM_B, CAMSV, UNI_A (one page each), DIP_A (5 pages), seninf (up to 0x8000), MIPI RX (up to 0x6000) and GPIO. The `mmap` offset is the physical address.
- `ISP_open` enables the camera clocks and takes a wakelock. Closing the last handle clears every viewfinder bit, drops the wakelock and gates the clocks. Close the node before any suspend.
- The interrupt handlers only collect status and timestamps and wake waiters. A handler reads CAMSV `+0x1C` (read-to-clear) and passes the bits to every waiter. Never read `+0x1C` from userspace; never set bit 31 of `+0x18`.
- The image sensor variant in use (`imgsensor/src/common/v1`) has no kernel seninf driver; only `common/v1_1` has one, for other chips. In stock, closed 64-bit libraries program seninf, pass 1, 3A and pass 2.

CAMSV registers, offsets inside each 4 KiB block:

| Group | Registers |
|---|---|
| top | MODULE_EN 0x10, FMT_SEL 0x14, INT_EN 0x18, INT_STATUS 0x1C, SW_CTL 0x20, IMGO_FBC 0x2C, CLK_EN 0x30, DATE_CODE 0x3C, PAK 0x4C |
| frame buffer control | FBC_IMGO_CTL1 0x110, FBC_IMGO_CTL2 0x114, ENQ_ADDR 0x118, CUR_ADDR 0x11C |
| IMGO write DMA | BASE_ADDR 0x220, OFST_ADDR 0x228, XSIZE 0x230, YSIZE 0x234, STRIDE 0x238, CON 0x23C, CON2 0x240, CON3 0x244, CROP 0x248 |
| TG | SEN_MODE 0x500, VF_CON 0x504, SEN_GRAB_PXL 0x508, SEN_GRAB_LIN 0x50C, PATH_CFG 0x510, FRMSIZE_ST 0x538, INTER_ST 0x53C, TIME_STAMP 0x570 |

The same file names the CAM_A registers (names and offsets only, no bit fields): TG at 0x500, IMGO DMA 0x220, RRZO 0x250, AAO 0x280, frame buffer control 0x110, command queue 0x160, resizer 0x6E0.

### Calls on `/dev/camera-isp`

Numbers from `enum ISP_CMD_ENUM` in `inc/camera_isp.h`.

| Call | Number | Argument |
|---|---|---|
| `ISP_WAIT_IRQ` | `_IOW('k', 3, 56)` | type (3 for CAMSV0), dump flag, clear mode (0 none, 1 clear then wait, 2 clear only, 3 clear all), status type 0, status bits, user key (0 works unregistered), timeout ms, seven words of time info. A timeout returns EFAULT and logs one line |
| `ISP_CLEAR_IRQ` | `_IOW('k', 4, 16)` | type, user key (int), status type, status bits |
| `ISP_BUFFER_CTRL` | `_IOWR('k', 6, 40)` | control (u32), module (interrupt type), buffer id, 4 bytes pad, three pointers |
| `ISP_DEBUG_FLAG` | `_IOW('k', 8, 8)` | pointer to a u32 mask; open sets the interrupt log bit, 0 silences the per-frame lines |
| `ISP_GET_CUR_SOF` | `_IOWR('k', 15, 8)` | pointer to a u32: type in, frame start count out |
| `ISP_GET_DMA_ERR` | `_IOWR('k', 16, 8)` | pointer to fifteen u32: type in the first, error words out |
| `ISP_GET_INT_ERR` | `_IOR('k', 17, 8)` | pointer to ten u32, one per interrupt type; reading zeroes them |
| `ISP_GET_CUR_ISP_CLOCK` | `_IOWR('k', 28, 8)` | current and target clock |
| `ISP_VF_LOG` | `_IOW('k', 29, 8)` | pointer to three u32: 11 (on) or 10 (off), device index (6 for CAMSV0), 0 |
| `ISP_LARB_MMU_CTL` | `_IOW('k', 36, 12)` | larb (2, 3, 5, 6), register offset below 0x1000, value |

## Open drivers

- BayLibre's "Add Mediatek ISP3.0" series (v8): seninf and CAMSV for MT8365 (`mtk_seninf.c`, `mtk_camsv_hw.c`, `mtk_camsv_video.c`).
- `github.com/vpavlin/mt8183-camera`: that series ported to MT8183 and run on a Chromebook: sensor, D-PHY, seninf, CAMSV, DMA, ISP bypassed, 8-bit Bayer only (`mtk_camsv.h`, `mtk_camsv_reg.h`, `mtk_camsv_hw.c`, `mtk_seninf_reg.h`, `mtk_seninf.c`, register dumps).
- Mainline `drivers/phy/mediatek/phy-mtk-mipi-csi-0-5.c` names the analog words: word 0x00 has CPHY enable bit 0, EQ protect 1, bandgap filter 2, bandgap core 3, and per lane a clock mode and a clock select bit (5 and 6, 8 and 9, 11 and 12); words 0x18 to 0x20 are the equaliser; word 0x80 bits 17:16 the reset mode; word 0xA8 bits 2:0 the byte clock inverts. A port's second block is 0x1000 higher.

MT8183 has MT6765's block bases (seninf 0x1a040000 size 0x8000, CAMSV 0x1a050000 + n x 0x1000, pass 1 0x1a004000) and the same register order inside a CAMSV block. Only the group bases differ; MT6765's are confirmed on the device:

| Group | Open driver, offset in group | MT8183 base | MT6765 base |
|---|---|---|---|
| top | MODULE_EN +0x00, FMT_SEL +0x04, INT_EN +0x08, INT_STATUS +0x0C, SW_CTL +0x10, IMGO_FBC +0x1C, CLK_EN +0x20, PAK +0x3C | 0x510 | 0x10 |
| TG | SEN_MODE +0x10, VF_CON +0x14, SEN_GRAB_PXL +0x18, SEN_GRAB_LIN +0x1C, PATH_CFG +0x20, frame size +0x48, INTER_ST +0x4C | 0x220 | 0x4F0 |
| IMGO | BASE_ADDR +0x00, XSIZE +0x10, YSIZE +0x14, STRIDE +0x18, CON +0x1C, CON2 +0x20, CON3 +0x24 | 0x20 | 0x220 |
| frame buffer control | CTL1, CTL2 | 0x110, 0x114 | 0x110, 0x114 |

Bits from `mtk_camsv_reg.h`: MODULE_EN TG_EN 0, PAK_EN 2, PAK_SEL 3, IMGO_EN 4, DB_EN 30. FMT_SEL 0 RAW8, 1 RAW10, 2 RAW12, 3 YUV422. SW_CTL IMGO_RST_TRIG 0, IMGO_RST_ST 1, SW_RST 2. CLK_EN TG 0, PAK 2, DMA 15. Interrupts TG_ERR 4, TG_GBERR 5, SOF 7, PASS1_DON 10, IMGO_ERR 16. TG_SEN_MODE CMOS_EN 0, DBL_DATA_BUS_EN 1. TG_VF_CON VFDATA_EN 0. IMGO_CON FIFO size bits 7:0, max burst bits 31:28.

What also applies to MT6765:

- Reading camsys registers with the power domain off or the clocks gated can hang the bus.
- PAK must be on; with it off the DMA raises errors continuously. PAK codes differ per chip (MT8365 0 RAW14 to 3 RAW8, MT8183 0 RAW8 to 3 RAW14); stock on MT6765 writes MT8183's. The 10, 12 and 14-bit outputs use MediaTek's packing.
- seninf layout: per-input blocks 0x1000 apart; CTRL +0x200, test model +0x600, MIPI_RX_CON +0x800, CSI2 +0xA00, MUX +0xD00; `SENINF_TOP_MUX_CTRL` +0x08, `SENINF_TOP_CAM_MUX_CTRL` +0x10.
- `SENINF_TOP_CTRL` (+0x00) needs 0xC00 on MT8183. On the R1 it stays 0x80000000 through every capture and frames are complete; stock never writes it.
- The open drivers name no IOMMU port for the CAMSV write DMA. MT6765's port list has three candidates, SOC0 to SOC2 (43 to 45, larb 3); see [Codecs and buffers](codecs.md#m4u). The firmware switches translation on for all 21 ports of larb 3 and files frame buffers under SOC0.

## Stock register sequence

Read by disassembly from the 64-bit stock libraries (never loaded, never shipped): `libcameracustom.so` (board table), `libcam.halsensor.so` (`SeninfDrvImp`, `HalSensor`), `libimageio_plat_pipe.so` (`NormalSvIOPipe`, `CamsvIOPipe`), `libimageio_plat_drv.so` (classes `NORMALSV_*`, `CAMSV_*`), `libcamdrv_isp.so` (`IspDrvCamsv`, `IspDrvImp`). Two class families drive a CAMSV block: `CAMSV_*` carries side data (phase detection, virtual channels) and `NORMALSV_*` a full raw frame. Both write the same registers in the same order and differ only in the DMA thresholds. Every register change is a read-modify-write of one field. `r1_funcore::hw::camera::seninf` and `r1_funcore::hw::camera::camsv` follow this sequence.

**Access.** Stock opens `/dev/camera-isp` and `/dev/kd_camera_hw` and maps through the first: seninf `0x1a040000` length 0x8000, MIPI analog `0x11c10000` length 0x6000, pad config `0x10002000` length 0x1000, one page per CAMSV.

**Board wiring** (`libcameracustom.so`): the sensor is on master clock 0 and CSI port 2, facing back, orientation 90. Port 2 is the D-PHY-only port: sensor interface 5 (index 4), analog blocks CSI2A and CSI2B, four data lanes and one clock lane.

Names used below: `T = 0x1a040000` (seninf top), `S = T + 0x4000` (sensor interface 5), `M = T + 0x2D00` (mux 2), `A = 0x11c14000` (CSI2A), `B = 0x11c15000` (CSI2B), `C = 0x1a050000` (CAMSV0).

**Order** (`HalSensor::configure`): streaming suspend, receiver, scenario, exposure and gain, streaming resume.

### Receiver

Step 1 is part of sensor power on. For each stream, stock runs steps 14 and 15 first, then 2 to 13 (`configInputForCSI`).

| Step | Writes |
|---|---|
| 1 Master clock | the kernel call; then on sensor interface 1 `T+0x614 = 0x67` (time stamp clock step, same at on and off), `T+0x200 \|= 1`, `T+0x204 \|= 0x40`, wait 100 µs |
| 2 Port control | `T+0x24`: bit 0 = 0 (four lanes, one clock), bits 10:8 = 2, bit 31 = 1 |
| 3 Interface | `S+0x200`: bit 0 = 1, bits 15:12 = 0, bits 30:28 = 0 (pad select); `S+0x204`: bit 6 = 1, bit 5 = 0 |
| 4 Lane clock select | analog word 0x00: on A clear 5, set 6, clear 8, set 9, set 11, set 12; on B clear 5, set 6, clear 8, set 9, clear 11, set 12 |
| 5 Byte clock invert | `A+0xA8 \|= 7`, `B+0xA8 \|= 1` |
| 6 Equaliser | on A and B, word 0x18 bits 5:4, 7:6, 21:20, 23:22 = 1; word 0x1C bits 5:4, 7:6 = 1 |
| 7 | `0x11c10040 = 0x90` (always the CSI0A block) |
| 8 | on A and B, word 0x24 bits 31:24 = 0x40; word 0x80 bits 17:16 = 0 |
| 9 Trim | from efuse words e2 `0x11c501bc` and e3 `0x11c501c0`, only when both are nonzero (stock's `init` reads 804 bytes of `/proc/device-tree/chosen/atag,devinfo`; these are at byte offsets 0x1D0 and 0x1D4, ports 0 and 1 at 0x1A0 and 0x1A4). `A+0x08` bits 4:0 and 12:8 = e2[16:12], bits 20:16 and 28:24 = e2[11:7]; `A+0x0C` bits 4:0 and 12:8 = e2[6:2]; `B+0x08` bits 4:0 and 12:8 = e3[31:27], bits 20:16 and 28:24 = e3[26:22]; `B+0x0C` bits 4:0 and 12:8 = e3[24:20] |
| 10 Power | word 0x00 `\|= 8` on A and B (bandgap core), wait 30 µs, `\|= 4` (bandgap filter), wait 1 µs |
| 11 Offset calibration | on A and B, word 0x18 `\|= 1`, word 0x18 `\|= 0x10000`, word 0x1C `\|= 1`; wait 1 µs; poll `A+0x48 & 0x38` until nonzero, up to 100 times, 30 µs apart; the same for B |
| 12 Lane mux | `S+0x824` bits 25:24 = 1, 27:26 = 2, 29:28 = 0, 31:30 = 3 (top byte 0xC9); wait 30 µs |
| 13 CSI-2 | `S+0xA0C = 0` (no DPCM); `S+0xA08 = 0x1500` (settle 0x15); `S+0xA00`: clear 27 and 7, set 3:0 (four lanes) and 4, set 16 (ECC order 1), then set 25; `S+0xA40` bits 7:0 = 0x1B; `S+0xA74`: clear 11 and 10, bits 2:0 = 3; `S+0xAE8` bits 10:0 = 0; `S+0xB20 = 0x001DFF00`; `S+0xA20 &= ~1`; `S+0xA18 = 0x8000001A`; `S+0xA10 = 0xFFFFFFFF`; `S+0xA14 = 0xFFFFFFFF`; `S+0xB10 = 0x1F`. No reset pulse |
| 14 Mux 2 | `M+0x00 \|= 0x80000000`; bits 15:12 = 8 (MIPI source; 1 is the test model); `M+0x3C` bits 1:0 = 1; two pixels per clock: `M+0x3C` clear 4, `M+0x00` set 8; `M+0x00` bits 29:28 = 2, 27:22 = 0x1B, 21:16 = 0x1F; bits 10 and 9 (sync polarities) 0; `M+0x00 \|= 3` then `&= ~3` (reset pulse); `M+0x3C \|= 0x100` |
| 15 Top mux | `T+0x08` bits 11:8 = 4: mux 2 takes sensor interface 5 |

- With `T+0x10` at reset, mux 2 feeds CAMSV0, mux 3 CAMSV1, muxes 4 and 5 the next two; muxes 0 and 1 feed the raw ISP inputs. Stock sends a single camera's main stream to mux 0. It uses muxes 2 to 5 only for multiple cameras (with a virtual channel filter through `setSeninfVC`, `S+0xA3C`, `S+0xA44`) or for side streams at four pixels per clock. The firmware's setup, mux 2 into CAMSV0 at two pixels per clock without the filter, delivers complete frames.
- Health (`SeninfDrvImp::sendCommand`): `S+0xA14` interrupt status (0x9044 healthy, any bit of 0xB0 an error, write ones to clear), `S+0xA1C` packet counter, `S+0xB14`, mux `M+0x08` (bit 0 overrun, write ones to clear), `M+0x0C` size, `M+0x14` (width in bits 31:16, height in 15:0), `M+0x18`. Test model of sensor interface n: `T + n x 0x1000 + 0x608` (bit 0 enable, bits 15:8 vsync, 23:16 dummy pixels) and `+0x60C` (pixels 12:0, lines 29:16).
- Off is the same call with enable 0: `T+0x24` bit 31 = 0 (bits 0 and 10:8 as in step 2), `S+0x200` bit 0 = 0, `S+0x204` bit 6 = 0, `S+0xA00 &= ~0x1F`, then word 0x00 on A and B clear bit 3, then bit 2. The analog blocks are outside the camera power domain, so this is what switches the bandgaps off. Stock leaves every other analog word programmed, and its `uninit` covers ports 0 and 1 only.
- Deskew calibration (`calibrateSensor`) runs only above 1.5 Gbit/s per lane and behind a debug property; this sensor runs at 720 Mbit/s.

### CAMSV0

`NormalSvIOPipe::configPipe` runs TG config, top config, frame buffer control config and DMA config, then TG enable, frame buffer control enable and DMA enable. `enqueOutBuf` runs the DMA config again with the buffer's geometry, then queues the buffer. `start` is the top enable.

| Step | Writes |
|---|---|
| 1 Driver open | `IspDrvImp::init`, first user of the process: open, map, `ISP_WAKELOCK_CTRL` 1, `ISP_DFS_CTRL` 1, `ISP_RESET_BY_HWMODULE` with the device index (6) |
| 2 TG config | `C+0x500` clear 0, clear 18 and 17 (no subsampling); `C+0x574 = 0`; `C+0x500 \|= 0x10000`; `C+0x504` clear 1 (continuous); two pixels per clock: `C+0x500` set 1, clear 13; `C+0x508 = x \| (x + width) << 16`; `C+0x50C = y \| (y + height) << 16` (width in pixels, a multiple of 4); `C+0x510` stays at reset for plain Bayer |
| 3 Top config | `ISP_BUFFER_CTRL` control 1 (clear), module 3, buffer 0 (zeroes the software frame counter); reset handshake `C+0x20 = 4`, `0`, `1`, poll until 3, `0`; `C+0x10 \|= 1` (TG), `\|= 0x40000000` (double buffering); `C+0x14 = 1` (RAW10; 0 RAW8, 2 RAW12, 4 RAW14, 3 YUV, 7 JPEG); `C+0x18 = 0x001304B1`; `C+0x10` bit 5 = 0, bits 13:8 = 0; `C+0x10 \|= 4`, clear 3 (pack on); `C+0x4C` bits 2:0 = 1 (RAW10; 0 RAW8, 2 RAW12, 3 RAW14), bits 5:4 = 1 (two pixels; 0 one, 2 four); `C+0xE00 = 0`; `C+0x474 &= 0x7FFFFFFF`; `C+0x488 = 0xFFFFFFFF`; `C+0x218 \|= 0x61000000` |
| 4 Frame buffer control | `C+0x110 = 0`, then `\|= 0x10000` |
| 5 DMA config | `C+0x230` bytes per line minus 1; `C+0x234` lines minus 1; `C+0x238` the stride, ORed with 0x01030000 (32-bit bus at two pixels per clock; 0x01010000 for one, 0x01070000 for four; stride a multiple of 4, 2 or 8); `C+0x248` crop start in bus words (0); `C+0x23C = 0x80000080`; `C+0x240 = 0x00020002`; `C+0x244 = 0x00020002` (for side data: 0x0004F004, 0x0002F002) |
| 6 Enables | TG `C+0x500 \|= 1`; frame buffer control, refused while `C+0x504` bit 0 is set: `C+0x110` bits 31:24 = 0, `\|= 0x8000`, `\|= 0x400000`; DMA `C+0x10 \|= 0x10`, `C+0xE00 \|= 1` (frame header) |
| 7 Queue a buffer | step 5 again, `C+0x118` = address, `C+0xE04` = its header's address, `C+0x2C \|= 1`; refused unless `C+0x110` bit 15 is set. `C+0x220` is never written: the block takes each address from the queue |
| 8 Start | `C+0x30 \|= 1`, `\|= 4`, `\|= 0x8000`, refused while `C+0x500` bit 0 is clear; `ISP_CLEAR_IRQ` key 0, status type 0, status 0x40000000; `ISP_VF_LOG` 11, 6, which sets `C+0x504` bit 0 and resets the kernel's first-frame flag |

- RAW10 bytes per line are `width * 10 / 8`, rounded up to a multiple of 4 at queueing (5 bytes per 4 pixels):

| Frame | Bytes per line | Bytes per frame |
|---|---|---|
| 3264x2448 | 4080 | 9987840 |
| 1632x1224 | 2040 | 2496960 |
| 1280x720 | 1600 | 1152000 |
| 640x480 | 800 | 384000 |

- Mask 0x001304B1: vsync 0, TG error 4, grab error 5, frame start 7, pass 1 done 10, DMA error 16, DMA overrun 17, software pass 1 done 20. Stock waits on vsync as status 1, frame start as 0x80, a finished frame as 0x100000.
- Dequeue (`dequeOutBuf`): `C+0x114` bits 6:0 count queued buffers not yet filled; a count below the number queued means a frame is in memory. Otherwise stock calls `ISP_WAIT_IRQ` for 0x100000, clear mode 1 on the first try and 0 after, with a timeout of twice the frame time and at least 400 ms, five tries. The time stamp is the header's first word (`C+0x570` is the register copy); the 0x34 header bytes are zeroed.
- Stop (flag 0), top first: if `C+0x504` bit 0 is set, `ISP_VF_LOG` 10, 6, then wait for vsync (status 1, clear mode 1, 400 ms) and read `C+0x53C` until bits 13:8 read 1 (TG idle), at most ten waits. Then `C+0x10` clear 30, `C+0x10 = 0`, `C+0x14 = 0`, `C+0x18 = 0`, `C+0x110 = 0`, `C+0x10 |= 0x40000000`, the bookkeeping clear, the reset handshake, `C+0x30` clear bit 0, then 2, then 15. After the top: `C+0x110 = 0`, `C+0x10` clear 4, `C+0xE00` clear 0, `C+0x500` clear 0. A nonzero flag skips the wait and runs the handshake first.
- Driver close (`IspDrvImp::uninit`, last user): unmap, `ISP_WAKELOCK_CTRL` 0, `ISP_DFS_CTRL` 0, close. The firmware makes none of stock's open and close calls: opening the node already takes the wakelock and enables the clocks, and the reset is step 3's handshake with a bounded poll (every 10 µs for 200 µs; the kernel's version polls without a bound).
- Stall check: `C+0x34 = 0xF0`, `C+0x48C = 0x303`, read `C+0x38`. On a failed wait stock calls `ISP_GET_INT_ERR` and `ISP_GET_DMA_ERR`.

## Reset state

Read with both nodes open and the sensor unpowered, through read-only mappings; CAMSV `+0x1C` not read.

| Block | Values |
|---|---|
| seninf top | `T+0x00` 0x80000000, `T+0x08` 0x00043210 (mux n takes interface n + 1), `T+0x10` 0x76543210, `T+0x14` and `T+0x18` 0x00001100, `T+0x1C` to `T+0x24` 0 (port control of ports 0 to 2) |
| sensor interfaces 1 and 5, identical | `+0x200` 0, `+0x204` 0, `+0x208` 0x1B1F0002, `+0x604` 0x00010001, `+0x608` 0x00300004, `+0x614` 0, `+0x824` 0xE4000000, `+0xA00` 0x00086160, `+0xA08` 0x00002000, `+0xA10` 0x7DFFBFFF, `+0xA14` 0, `+0xA18` 0, `+0xA1C` 1, `+0xA20` 0xFFFFFFFF, `+0xA40` 0, `+0xA74` 1, `+0xA78` 0x00000688, `+0xAE8` 0, `+0xB10` 0x00000F1E, `+0xB20` 0x001DFF00 |
| muxes 0 to 3, identical | `+0x00` 0x06DF0080, `+0x04` 0x8000007F, `+0x08` 0, `+0x0C` 0, `+0x10` 0x0000C000, `+0x14` 0, `+0x18` 0, `+0x24` 0x30C30000, `+0x2C` 0x000E2000, `+0x30` 0x40000000, `+0x3C` 0 |
| analog CSI2A and CSI2B, identical | word 0x00 0x00000002, 0x04 0x00084444, 0x08 0x10101010, 0x0C 0x00001010, 0x10 0x00808080, 0x18 0x88508850, 0x1C 0x00008850, 0x24 0, 0x48 0, 0x80 0x00000002, 0xA4 0x0000002A, 0xA8 0x00000007 |
| analog CSI0A | word 0x00 0x00800002, 0x04 0x00084444, 0x08 0x10101010, 0x0C 0x00001010, 0x10 0x80808080, 0x14 0x00000080, 0x18 to 0x20 0x88508850, 0x28 0x0020000F, 0x30 0x10101010, 0x34 0x0020000F, 0x3C 0x10101010, 0x40 0, 0x80 0x00000002, 0xA4 0x0000002A, 0xA8 0x00000007 |
| CAMSV0 | `+0x10` 0x40000000, `+0x14` 0x00000100, `+0x18` 0, `+0x20` 2, `+0x3C` 0x20150120 (date code), `+0x40` 0x65930000, `+0x110` to `+0x11C` 0, `+0x218` 0, `+0x22C` 0x00800080, `+0x230` to `+0x238` 0, `+0x23C` 0x80000080, `+0x240` and `+0x244` 0x00800080, `+0x474` 0, `+0x478`, `+0x480`, `+0x488` 0xFFFFFFFF, `+0x500` 0, `+0x504` 0, `+0x508` 0x7FFF0000, `+0x50C` 0x3FFF0000, `+0x510` 0, `+0x514` 0x00001430, `+0x524` 0x00000010, `+0x528` 0x0FFFFFFF, `+0x53C` 0x00000102, `+0x570` 0, `+0xE00` 0x0000007F, `+0xE04` 0 |

- These values confirm the register map: the date code at `+0x3C`, TG idle (1) in `+0x53C` bits 13:8, the lane mux at its identity 0xE4, the bandgap bits clear.
- Several values stock writes are already the reset values: the equaliser fields, word 0xA8 at 7, mux fields 0x1B and 0x1F, `S+0xB20`, CAMSV `+0x23C` and `+0x488`.
- When both nodes are closed and opened again, seninf and CAMSV0 return to their reset values (the camera power domain drops), but the analog blocks keep every word. After a stop the firmware therefore writes the analog words back to their reset values (CSI2A and CSI2B words 0x18, 0x1C, 0x24, 0x08, 0x0C, 0x00; CSI0A word 0x40); calibration word 0x48 then returns to 0.

## Behaviour on the device

**Receiver** with stock's writes for slim video:

| Reading | Value |
|---|---|
| Offset calibration, word 0x48 | bits 0x38 set on CSI2A and CSI2B within step 11's polls |
| `S+0xA14` while streaming | 0x00009044, no bit of 0xB0 |
| `S+0xA1C` packet counter | counts to 0x2D1 (721), restarts each frame |
| `S+0xB14`, `M+0x0C` | 0 |
| `M+0x08` | 0x18, overrun bit clear |
| `M+0x14`, `M+0x18` | 0x050002D0: 1280 by 720 |
| Programmed words read back | `T+0x08` 0x00043410, `T+0x24` 0x80000200, `S+0x200` 1, `S+0x204` 0x40, `S+0x824` 0xC9000000, `S+0xA00` 0x0209617F, `S+0xA08` 0x1500, `M+0x00` 0xA6DF8180, `M+0x3C` 0x101, A word 0x00 0x00001A4E, B word 0x00 0x0000124E; after the stop A 0x00001A42, B 0x00001242 |

**Capture.** Capture, preview, slim video and high speed all deliver complete frames into CAMSV0 (sizes in [CAMSV0](#camsv0)). On a dark scene the frames show black level 64 with a few codes of noise, no pixel at zero, nothing written past the last line. The error words stay 0, there is no M4U fault, and the kernel log has only `CAMSV0 P1_SOF_n` and `P1_DON_0` lines.

- The first frame after the viewfinder bit is complete. At full size the first frame start comes 32 ms after `ISP_VF_LOG` and the frame finishes 32 ms later. In slim video a frame finishes 9.5 ms after its start, frames 11.1 ms apart; in preview 16.2 ms after its start (1224 lines).
- Packed RAW10 is one little-endian bit stream per line: pixel n occupies bits 10n to 10n+9. MIPI's layout (four high bytes, then the low bits) does not apply. Line 0 holds G and B, line 1 R and G.
- One pixel per clock (mux bit 8 clear, `C+0x500` bit 1 clear, `C+0x4C` bits 5:4 = 0, bus word 0x01010000) gives the same bytes.
- The TG window crops: 600 lines of 2448 give 600 complete lines; pixels 640 to 1280 give 800-byte lines.
- TG after a frame: `C+0x538` holds width and lines plus one (0x0CC00991 at full size), `C+0x530` the window's pixel count minus 2 (minus 1 at one pixel per clock). `C+0x53C` bits 13:8 return to 1 one vsync after the viewfinder bit is cleared.
- Receiver while capturing: `S+0xA14` reads 0x9044 in the binned modes. At full size bit 22 is set too (0x00409044), returns after a clear, and frames are complete. `M+0x34` holds the pixels per frame (0x0079EC00 at full size). Status bits latched at stream start are not a fault. A clean stream shows `S+0xA14` at 0x9044 with a mux height, and again at 0x9044 with no bit of 0xB0 after a clear.
- Frame buffer control: `C+0x114` reads 0x00000101 with one buffer queued and 0x00010100 after the frame. Each queue adds one to bits 6:0 (waiting) and 13:8 (queued in total); each frame subtracts one from 6:0 and adds one to 21:16; with nothing queued, frames count in 31:24 (dropped). Buffers fill completely, in queue order; waiting buffers are not touched.
- The block writes the frame header to whatever `C+0xE04` holds at frame start, so with buffers queued ahead each header lands two buffers late; one fixed header page avoids that. The header's first word is the frame start in microseconds on the block's clock, which starts with the first frame (the first header is zero); `C+0x570` is that clock running.
- With the nodes closed nothing of a run remains: the frame buffer control words read 0, no device mapping remains but the framebuffer's, the analog words are at reset.
- Images are sharp and undistorted, rotated by 90° as the orientation says.

## Hardware ISP

Pass 1 (CAM_A, CAM_B), 3A statistics and pass 2 (DIP) have no open implementation, as for every public driver of this family. Stock's logic is in closed 64-bit bionic libraries: `libcam.halsensor.so` (621168 bytes), `libcamdrv_isp.so` (244128), `libcam.iopipe.so` (694608), `libcam.hal3a.v3.so` (2685088), `lib3a.*.so`, `libcamalgo.*.so`, the service `camerahalserver`, and the CCU firmware `lib3a.ccu`. Demosaic, white balance, exposure, lens shading and noise handling are therefore done in r1metal's software.

## Sources

- https://github.com/vpavlin/mt8183-camera
- https://raw.githubusercontent.com/torvalds/linux/master/drivers/phy/mediatek/phy-mtk-mipi-csi-0-5.c
- https://www.gophotonics.com/products/cmos-image-sensors/sk-hynix/21-271-yacg4d0c9shc

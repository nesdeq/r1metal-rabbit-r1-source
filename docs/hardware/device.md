# Device

The R1's parts and the SoC blocks around them. Kernel paths are relative to `kernel/src/`.

## Parts

| Part | Facts |
|---|---|
| SoC | MediaTek MT6765 (Helio P35): 8 Cortex-A53 cores in two clusters, 2.2 and 1.6 GHz |
| PMIC | MT6357 |
| RAM | 3950444 kB visible to Linux of the 4 GiB the bootloader maps; CMA 344064 kB (`/proc/meminfo`) |
| Display | 480x640, panel driver `ili9883_boe_mipi_hd`. 60.0 Hz (median flip interval 16.670 ms, console `anim`) with patch `0002`; LK's 130 MHz DSI clock gives 57.4 Hz. `fb0` reports no refresh rate |
| Touch | Hynitron CST328 at I2C 0x1a, with an ESD check every 4.096 s. Its input device `mtk-tpd` (`event3`) registers after init has opened `mtk-kpd`, `mtk-pmic-keys` and `och1970_holl_key`, so init picks it up by uevent. It reports `ABS_MT_POSITION_X`, `ABS_MT_POSITION_Y`, `ABS_MT_TRACKING_ID` and `BTN_TOUCH`, about 35 times per second per finger, moving or not |
| Button | `KEY_POWER`, from both `mtk-kpd` and `mtk-pmic-keys` |
| Wheel | OCH1970 hall sensor; `och1970_holl_key` sends `KEY_UP` and `KEY_DOWN`. The driver (`drivers/input/touchscreen/och1970.c`) polls every 50 ms and reports one step when the angle is 30° or more past the last report; the rest of that turn is dropped, and after 3 s without a step it counts from the current angle |
| Sound | MT6357 converters and an AW87xxx amplifier, see [Sound](sound.md) |
| Radio | the SoC's connectivity core with the MT6631 radio chip, see [WiFi](wifi.md) |
| Camera | Hi846 sensor on an MS35774 swivel, see [Camera](camera.md) |
| Codecs | H.264 encoder and decoder and a JPEG encoder in one block, see [Codecs and buffers](codecs.md) |
| Accelerometer | behind the sensor hub, see [System](../firmware/system.md#accelerometer) |
| Storage | eMMC, 512-byte sectors. `userdata` is `mmcblk0p45`, 115361153024 bytes from sector 18841600. Discards are in 4 MiB units; a discarded block does not read back as zeros |
| Battery | 1000 mAh rated cell, MT6357 gauge with a 10 mΩ sense resistor, MT6370 charger; see [Power](power.md) and [Battery](../firmware/battery.md) |

## Swivel

`drivers/misc/step_motor_ms35774.c` exposes `/sys/devices/platform/step_motor_ms35774/orientation`: 0 (front) to 180 (rear); 90 turns the lens inward. The motor homes at probe and then reads 90. A read returns the old angle until a move finishes, so reading the file waits for the move. Only one move runs at a time.

## Display path

- `drivers/misc/mediatek/video/mt6765/videox/mtkfb.c`: `FBIOPAN_DISPLAY` is a page flip; `mtkfb_pan_display_impl` passes `fb_pa + yoffset * line_length` to the overlay engine without a copy. `FB_ACCEL_NONE`.
- `FBIOBLANK` blanks the panel and stops touch and wheel input; a system suspend does not blank it.
- `MTKFB_SET_OVERLAY_LAYER` and `MTKFB_SET_VIDEO_LAYERS` take layers by physical address (RGB565, RGB888, ARGB, YUV422, UYVY). Unused.

## GPU: not used

PowerVR GE8320, BVNC 22.87.104.18. Rabbit's kernel builds its kernel driver `pvrsrvkm` (DDK 1.13@5776728), bound to `13000000.mfg_doma`, with `/dev/dri/card0`, `renderD128` and `/dev/pvr_sync`; `rgx.fw` loads at the first open. The interface is one opaque ioctl, `DRM_IOCTL_PVR_SRVKM_CMD` with 27 bridge groups, and the userspace driver is Imagination's closed bionic libraries. Mesa's PowerVR driver does not support the GE8320, and mainline's `drivers/gpu/drm/imagination` supports other cores. r1metal's kernel builds neither the GPU driver nor DRM.

The GPU has no rail of its own; its voltage is a VCORE request.

| Step | kHz | V | Full load mW |
|---|---|---|---|
| 0 | 680000 | 0.80 | 746 |
| 1 | 500000 | 0.70 | 419 |
| 2 | 400000 | 0.65 | 293 |

Software rendering costs little: continuous animation keeps one little core at 400 MHz about 25 % busy, about 3 mW by the kernel's power tables (`/proc/upower/upower_debug`).

## MDP: not used

`/dev/mtk_cmdq`, ioctl magic `'x'`: `CMDQ_IOCTL_ASYNC_EXEC` 20 (`struct mdp_submit`), `CMDQ_IOCTL_ASYNC_WAIT` 21, readback 22 to 24, `QUERY_USAGE`, `QUERY_CAP_BITS`, `QUERY_DTS`, `NOTIFY_ENGINE`. A job is a list of `struct op_meta` (write, write an ION fd as an address, read, poll, wait, set or clear an event) that the kernel turns into GCE instructions. Engines: RDMA0, CCORR0, RSZ0, RSZ1, TDSHP0, COLOR0, WROT0, WDMA, CAMIN; source and destination addresses are at +0xF00 in RDMA, WROT and WDMA. Everything else is built by the closed `libdpframework.so`, and the registers are not documented publicly, so the firmware scales, rotates and converts colour in software.

## Block map

From `arch/arm64/boot/dts/mediatek/mt6765.dts` and the running device.

| Block | Base | IRQ (GIC SPI) | Node |
|---|---|---|---|
| seninf1 to seninf4 | 0x1a040000 + n x 0x1000 | | via `/dev/camera-isp` |
| MIPI RX analog csi0a to csi2b | 0x11c10000 to 0x11c15000 | | via `/dev/camera-isp` |
| Image sensor control | 0x1a040000 (`kd_camera_hw1`) | | `/dev/kd_camera_hw` |
| cam1 (UNI), cam2 (CAM_A), cam3 (CAM_B) | 0x1a003000, 0x1a004000, 0x1a005000 | 235, 236, 237 | `/dev/camera-isp` |
| camsv1 to camsv4 | 0x1a050000 to 0x1a053000 | 243, 244, 241, 242 | `/dev/camera-isp` |
| dip1 (pass 2) | 0x15022000 | 255 | `/dev/camera-isp` |
| ccu | 0x1a0b1000 | 246 | `/dev/ccu` |
| venc_gcon (codec top) | 0x17000000 | | `/dev/Vcodec`, `/dev/Vcodec2` |
| venc (H.264 encoder) | 0x17020000 | 230 | via `/dev/Vcodec` |
| venc_jpg (JPEG encoder) | 0x17030000 | 232 | `/proc/mtk_jpeg` |
| vdec | 0x17040000 | 233 | via `/dev/Vcodec` |
| mdp rdma0, ccorr0, rsz0, rsz1, wdma0, wrot0, tdshp0 | 0x14004000 to 0x1400a000 | 211, 226, 212, 213, 216, 215, none | `/dev/mtk_cmdq` |
| ION | | | `/dev/ion` |

- Codec clocks: `CLK_VENC_SET1_VENC`, `CLK_VENC_SET2_JPGENC`, `CLK_VENC_SET3_VDEC`. The `camera-isp` node holds `SCP_SYS_DIS`, `SCP_SYS_ISP`, `SCP_SYS_CAM`, `CLK_IMG_DIP`, `CLK_CAM`, `CLK_CAMTG` and `CLK_CAMSV0` to `CLK_CAMSV2`; the seninf gate, the MIPI gates and the master clock belong to `kd_camera_hw1`.
- No `/dev/video*`: the V4L platform and mem2mem drivers are off. Also present: `/dev/CAM_CAL_DRV`, `/dev/mdp_sync`, `/dev/mtk_disp_mgr`, `/dev/MTK_SMI`.
- Media options built: `CONFIG_MTK_IMGSENSOR`, `CONFIG_IMGSENSOR_MAIN="hi846_mipi_raw"`, `CONFIG_MTK_CAMERA_ISP`, `CONFIG_MTK_CCU`, `CONFIG_MTK_JPEG`, `CONFIG_MTK_VIDEOCODEC_DRIVER`, `CONFIG_MTK_CMDQ_V3`, `CONFIG_MTK_M4U`, `CONFIG_ION`, `CONFIG_MTK_ION`. Off: `CONFIG_MTK_LENS`, the flashlight drivers, `CONFIG_COMPAT`.

## Sources

- https://docs.mesa3d.org/drivers/powervr.html
- https://raw.githubusercontent.com/torvalds/linux/master/drivers/gpu/drm/imagination/pvr_drv.c
- https://lkml.iu.edu/hypermail/linux/kernel/2501.2/06543.html
- https://www.mediatek.com/products/smartphones/mediatek-helio-p35

# Codecs and buffers

The codec block (H.264 encoder, H.264 decoder, JPEG encoder), ION buffers and the M4U. How the firmware uses them is in [Camera](../firmware/camera.md) and [Media](../firmware/media.md). Kernel paths are relative to `kernel/src/drivers/misc/mediatek/`.

No vendor code runs. The encoder and decoder register sequences were traced from stock `libvcodecdrv.so` running in an emulator against a fake register block; `r1_funcore::hw::venc` and `r1_funcore::hw::vdec` reproduce them write for write. Mainline's `drivers/media/platform/mediatek/vcodec` is no register reference: it passes parameters to closed VPU or SCP firmware over IPI. MediaTek specifies H.264 encode up to 1080p30, and H.264 and HEVC decode up to 1080p30.

## Codec node

`/dev/Vcodec` is `videocodec/mt6765/videocodec_kernel_driver.c` with `common/` (`common/val/src/val.c`, `common/drv/src/dvfs_v2.c`) and `videocodec_kernel.c`.

- Ioctl magic `'M'`: `VCODEC_LOCKHW` 0x0d, `VCODEC_UNLOCKHW` 0x33, `VCODEC_WAITISR` 0x0b, `VCODEC_SET_FRAME_INFO` 0x37, `VCODEC_INC_ENC_EMI_USER` 0x15 to `VCODEC_DEC_DEC_EMI_USER` 0x18, `VCODEC_GET_CORE_NUMBER` 0x31, `VCODEC_MB` 0x34. The three the firmware uses are `_IOW` with size 4. Success returns 0xFF.
- `struct VAL_HW_LOCK_T`, 56 bytes on arm64: `pvHandle` (8), `u4HandleSize` (4, pad 4), `pvLock` (8), `u4TimeoutMs` (4, pad 4), `pvReserved` (8), `u4ReservedSize` (4), `eDriverType` (4), `bSecureInst` (1, pad 7). Driver types: 5 H.264 encode, 6 H.264 decode, 2 MPEG-4 decode, 9 MPEG-1/2 decode, 25 HEVC decode, 23 JPEG encode.
- `struct VAL_ISR_T`, 112 bytes: `pvHandle`, `u4HandleSize`, `eDriverType`, `pvIsrFunction`, `pvReserved`, `u4ReservedSize`, `u4TimeoutMs`, `u4IrqStatusNum`, `u4IrqStatus[16]`.
- The kernel identifies a lock owner by the physical address behind `pvHandle`. It finds that address by a page table walk, which fails on a huge page, so `pvHandle` must be a mapped user address.
- Locking powers the block, sets the selector at `0x17000020` (1 H.264 encode, 2 decode, 4 JPEG encode, 0 none) and enables the interrupt; unlocking reverses this. An owner's first lock requests the top codec clock step until its unlock.
- An encoder lock with timeout 0 fails immediately while another owner holds the block. Any other timeout waits for the unlock in 1 s steps, at most 30; stock and the firmware use 1000 ms (`r1_funcore::hw::LOCK_TIMEOUT_MS`). The first lock after boot checks the block without waiting (patch `0136`).
- `vcodec_suspend` refuses suspend with EBUSY while any lock is held. Closing the node's last file releases a forgotten lock and powers the block down.
- A wait that times out returns -2 (ENOENT) and logs one line. The interrupt handler reads the encoder status at +0x5C, acknowledges every set bit at +0x60 and passes the status to the waiter, but only while the holder's type is 5.
- `vcodec_mmap` maps physical ranges; the mmap offset is the address: 0x17002000 + 0x1000 and 0x17000000 + 0x50000 (codec top, larb 1 at 0x17010000, encoder 0x17020000, JPEG encoder 0x17030000, decoder 0x17040000). The registers respond only while the block is powered.
- The kernel itself touches only the encoder's interrupt status +0x5C and acknowledge +0x60 (SPS 0x1, PPS 0x2, FRM 0x4, DRAM 0x8, PAUSE 0x10, SWITCH 0x20), pause +0xAC, hard reset +0xA8, and the decoder's done status at decoder + 0x5000 + 0xA4 bit 16.
- Stock's programming is in `vendor/lib/libvcodecdrv.so` (1069976 bytes, 32-bit Thumb, symbols kept; there is no 64-bit version, and `CONFIG_COMPAT` is off). The encoder goes through `H264_WriteHW` from `H264_SeqLvlMMRSetting`, `H264_FrmLvlMMRSetting`, `H264_EncodeFrameEnc`, `H264_EncodeSPS`, `H264_EncodePPS` and `H264_AllocWorkBuf`, with a 16448-byte rate control microcode from `H264_GetRCCode`. The decoder is the `H264_HAL_*` layer under `Vdec_Drv_H264_decode`. `libh264enc_sa.ca7.so` and `libh264dec_sa.ca7.so` are software codecs.

## JPEG encoder

`jpeg/v2/`; MT6765 builds only `JPEG_ENC_DRIVER`. Node `/proc/mtk_jpeg`, magic `'x'`:

| Ioctl | Effect |
|---|---|
| `JPEG_ENC_IOCTL_INIT` `_IO('x', 11)` | claims the encoder (EBUSY if claimed), powers and resets it |
| `JPEG_ENC_IOCTL_CONFIG` `_IOW('x', 12, struct JPEG_ENC_DRV_IN)` | sets up a picture; resets the block every time |
| `JPEG_ENC_IOCTL_START` `_IO('x', 15)` | starts the encode |
| `JPEG_ENC_IOCTL_WAIT` `_IOWR('x', 13, struct JPEG_ENC_DRV_OUT)` | `timeout` ms (i64), pointers for `fileSize`, `result`, `cycleCount`; returns EFAULT and does a warm reset when the result is not 0 |
| `JPEG_ENC_IOCTL_DEINIT` `_IO('x', 14)` | releases the encoder, as closing does |

- `struct JPEG_ENC_DRV_IN` (`jpeg_drv.h`, 80 bytes): `dstBufferAddr`, `dstBufferSize`, `encWidth`, `encHeight` (u32), `enableEXIF`, `allocBuffer` (u8), `encQuality`, `encFormat`, `disableGMC`, `restartInterval`, `srcBufferAddr`, `srcChromaAddr`, `imgStride`, `memStride`, `totalEncDU`, `dstBufAddrOffset`, `dstBufAddrOffsetMask` (u32), `srcFd`, `srcFd2`, `dstFd` (int), `memHeight` (u32).
- Buffers are ION fds, which the driver maps; the address fields are only logged. `encFormat`: 0 YUYV, 1 YVYU, 2 NV12, 3 NV21. For NV12 and NV21 with `srcFd == srcFd2`, chroma starts at `memStride * memHeight`.
- Limits: width and height below 65536, `totalEncDU` at least 4, strides multiples of 16 (32 for YUYV and YVYU), sources 16-byte aligned, `dstBufferSize` at least 624, `dstBufAddrOffset` below the buffer size. `totalEncDU` = `ceil(w/16) * ceil(h/16) * 6 - 1` for 4:2:0 and `ceil(w/16) * ceil(h/8) * 4 - 1` for 4:2:2. YUYV stride is `2 * w`, byte order Y0 U Y1 V.
- `encQuality`: 0x0 Q60, 0x1 Q80, 0x2 Q90, 0x3 Q95, 0x4 Q39, 0x5 Q68, 0x6 Q84, 0x7 Q92, 0x9 Q48, 0xA Q74, 0xB Q87, 0xD Q34, 0xE Q64, 0xF Q82; 0x8 and 0xC are rejected. `enableEXIF` 0 writes a complete JFIF file.
- Result: 0 done, 1 stalled (destination full), 2 codec interrupt, 3 none. The reported size is the write pointer rounded up; the file ends at its last `FF D9`.
- The driver never sets the codec selector; with the selector at 0 the encoder runs to done and writes zeros. Required order: `INIT`, `VCODEC_LOCKHW` type 23, `CONFIG`, `START`, `WAIT`, `VCODEC_UNLOCKHW`, `DEINIT`. One `INIT` allows any number of `CONFIG`, `START`, `WAIT` rounds.
- The probe disables the interrupt it requested, and `INIT` enables it (patch `0075`).
- MediaTek specifies 232 MP/s (`jpeg_drv.c`). At quality 0x3: NV12 640x480 under 2 ms, NV12 2048x2048 12 ms, YUYV 2048x2048 15 ms.
- Registers while powered, at `0x17030000` through `/dev/Vcodec`: sources +0x170 and +0x174, destination +0x120, write pointer +0x124, stall address +0x128, block target +0x10C, blocks done +0x110, cycles +0x168.

## H.264 encoder

Stock's driver (`eVEncDrvCreate` copies `gH264_ENC_DRV_HW_IF`) selects the block by chip: MT6765 is 34 in `VAL_CHIP_NAME_T`, block 0x17020000, 0x1000 bytes. Parameter 6 (`PARAM_ENC`) allocates the work buffers; parameter 32 (`CONFIG_QP`) sets both quantisers and turns rate control off. Encode option 1 writes SPS and PPS in software; option 4 encodes a frame: lock, registers, trigger, wait, byte count, unlock. The firmware runs option 4 with rate control and reference compression off.

One frame; addresses are device addresses shifted right by 4:

| Step | Register | Value |
|---|---|---|
| reset | +0xA8 | 0, 0, read, 1, read |
| sequence | +0x04 | intra quantiser << 26, group length in bits 9:0 (0 for 1024 and up), the group length again << 14 when 30 or less |
| | +0x08 | 0x626E03AE with the inter quantiser << 10 |
| | +0x0C | 0x924E0000 |
| | +0x24 | macroblocks << 16, rows << 8, columns |
| | +0x28 | columns << 21, columns << 11, right padding to 16 << 5, bottom padding |
| | +0x30 | 0xC0200000 (CABAC bit 31, High bit 30, 8x8 transform bit 21) with `level_idc` << 22 |
| | +0x48, +0x4C, +0x50, +0x54 | 0, 0 (30 << 16 at 30 fps), 0x3308, 0x800 |
| | +0x12C | 0x1F4 |
| frame | +0x10 | 0x108400; a key frame adds 0x3006 and the low bit of its IDR count << 3; every inter frame except the first after a key frame adds 0x100 |
| | +0x14, +0x18 | frame number |
| | +0x1C | frame number of the last key frame |
| | +0x48 to +0x54 | as above |
| | +0x124, +0x128 | 0, 0x10000 |
| buffers | +0x68 | stream buffer bytes >> 7 |
| | +0x64 | stream buffer |
| | +0x6C, +0x70 | source luma, chroma (NV12) |
| | +0x94 | 0 |
| | +0x74, +0x78 | reconstructed luma, chroma |
| | +0x7C, +0x80 | reference luma, chroma |
| | +0x84, +0x88 | motion data written, read |
| start | +0xA4 | 0x3F |
| | +0x00 | 0x0200BFE0 |
| | +0xEC | 1 |
| | +0x58 | 4 |
| end | status 4 | 8: stream buffer full |
| | +0x98 | bytes written, start code included |
| | +0x9C | clock cycles |
| | +0xEC | 0 |

- The reconstructed and reference buffer pairs swap every frame, and so do the two motion buffers. Work buffers as stock allocates them for 1280x720: luma 2 x 942080, chroma 2 x 471040, motion 2 x 28800 bytes. Every address must be a multiple of 16.
- The hardware writes the whole slice: start code, NAL header, slice header (16-bit `frame_num`, POC type 0 with a 16-bit LSB counting by 2, one reference, `idr_pic_id` alternating 0 and 1, slice QP relative to 26) and data. The SPS and PPS must match: `log2_max_frame_num` 16, `log2_max_pic_order_cnt_lsb` 16, CABAC, two default reference indices, quantisers 26, deblocking control present, 8x8 transform. Stock's for 1280x720 High 4.1: `00000001 67 64 00 29 AC 1B 1A 80 50 05 BA 6A 02 02 02 0F 08 84 6E`, `00000001 68 EA 43 CB`.
- Stock's reference compression (`vendor.mtk.venc.h264.set.ufo.mode`) adds +0x164 = 0x11, +0x168 = row sizes, and larger luma buffers. With it, inter frames never finish on this device.
- Stock's rate control adds the microcode (`ucH264RCPUCode_v20170703`) at +0x8C and state at +0x90.
- A key frame takes about 2.8 million cycles, an inter frame 3.3 million: 7.2 ms at the top codec clock. Under the lock the eleven larb 1 words read 0x9: translation is on without a write.

## H.264 decoder

The decoder sequence is stock's `h264_dec_init`, `h264_dec_decode`, `h264_dec_getDisplayBuffer` and `h264_dec_getFreeBuffer`, write for write. The unit tests of `r1_funcore::hw::vdec` contain the 433 writes of one picture.

One mapping of 0x46000 bytes at 0x17000000 through `/dev/Vcodec`:

| Block | Offset | Contents |
|---|---|---|
| VLD | 0x40000 | stream feed, scaling lists |
| VLD_TOP | 0x40800 | picture size in macroblocks, the bit reader's buffer |
| MC | 0x41000 | output picture, reference addresses, line buffers, last macroblock done |
| AVC_VLD | 0x42000 | bit reader, parameter set and slice words, commands, errors |
| AVC_MV | 0x43000 | field words and order counts of the lists, motion buffers |
| PP | 0x44000 | unused |
| MISC | 0x45000 | clocks, interrupt, memory bounds, stop |

The hardware parses a whole picture with all its slices: the macroblocks, every slice header after the first, and the first header's list modifications and weight tables. The driver writes the rest of the first header as register words. The bit reader is a register window: reading `AVC_VLD + 4n` (n 1 to 32) advances it n bits and returns the next 32, with emulation prevention bytes removed. The driver advances it by its own parse of the header and checks the window at three points.

One picture (433 writes with one reference, 417 for a key picture), in stock's order:

| Step | Writes |
|---|---|
| Reset | `VLD+0x108 = 0x101`, `MISC+0x84 = 2`, `MISC+0x00` top bits kept with 0x00801FF0, `MISC+0xC8 = 0x1F7`, `+0xCC = 0x53E30182`, `+0x178 = 0xFFFFFFE1`, `+0xF4 = 0`, `+0x84 = 2`, `VLD+0x108 = 0`, `MISC+0xEC = 1`, `+0x04 = 0x10`, `+0x160 = 0x0065FF10` |
| Stream | `VLD+0xC0 = 0x40000`, `AVC_VLD+0x84 = 0xAE00E007`, bit 0 of `VLD_TOP+0x54`, `MC+0x24 = 6`, stream start and end in 64-byte units in `VLD+0xB4`, `+0xB8`, bit 28 of `VLD+0xEC`, reader buffer `VLD_TOP+0x28`, `AVC_VLD+0x28C = 1` then 0, read pointer `VLD+0xB0` twice, write end `VLD+0x110`, `AVC_VLD+0x28C = 1`, `VLD+0x108 = 0x100` then 0, `VLD+0xCC = 0`, fetch `VLD+0x8C = 0x100000` and wait for bit 0 of `VLD+0xE8`, fill `VLD+0x8C = 0x800000`, `AVC_VLD+0x28C = 0` |
| Start code | unless already found: `AVC_VLD+0x2D8 = 1`, wait for bit 0 to clear (bit 11 of `+0x2C8` also counts as found); then advance 32 bits |
| Reference lists | `MC+0x564 = 0`; `AVC_VLD+0x228 = 1` (clear lists) and wait; frame number range `AVC_VLD+0xA0`, picture number `+0xA4`. Per entry of a P list: address `MC+0x3DC`, number `AVC_VLD+0xA8` (bit 19 long term), a field word `AVC_MV+0x00` (slot from bit 23, bottom bit 22, top first bit 21, long term bit 20); order counts `AVC_MV+0x204`, `+0x208`, `+0x200`. B lists likewise at `MC+0x45C`, `+0x4DC`, `AVC_VLD+0x128`, `+0x1A8`, `AVC_MV+0x80`, `+0x100`, with order counts in the low 18 bits; the second list's motion buffers at `AVC_MV+0x180` |
| Scaling lists | four values per word through `VLD+0x260` (address, bit 9 before, bit 8 after) and `VLD+0x264`: six 4x4 lists from 0, two 8x8 at 0x60 and 0xA0; the driver applies the fallback rules |
| Parameter sets, slice | `AVC_VLD+0x88`, `VLD_TOP+0x68` (last macroblock row and column), `VLD_TOP+0x100 = 0`, `AVC_VLD+0x90`, `+0x94` (default list lengths, scaling flags bits 10 to 17, bit 18), `+0x8C` (first macroblock), `+0x98` (slice type, spatial direct, list lengths) |
| Hardware parsing | `AVC_VLD+0x22C = 1` and wait (list modifications); with weight tables, bit 0 of `MISC+0xF0`, `AVC_VLD+0x230 = 1` and wait, `AVC_VLD+0xF0`; then the reader is at the header's end |
| Picture | size in `MC+0x208`, `+0x20C`, `+0x260`, `+0x264`, `+0x22C`, `+0x980`, `VLD_TOP+0x70`, `+0x6C`; error mask 0xBFDFB0F7 in `AVC_VLD+0x280`, `+0x26C`; address `MC+0x560` (shifted in `+0x224`, `+0x854`), chroma offset `MC+0x55C` (`+0x228`, `+0x850`); motion buffer `AVC_MV+0x20C`; `AVC_VLD+0x9C`; for CABAC `AVC_VLD+0x234 = 1` and wait for bit 25 of `+0x25C` to clear; line buffers `MC+0xA20`, `+0xA24`; bounds `MISC+0x3C`, `+0x40`, `+0x54`, `+0x58`; `AVC_VLD+0x84 = 0xAE80E007`; start `AVC_VLD+0x238 = 1` |
| End | interrupt; read `MISC+0xA4`, `MISC+0xC8 = 0xF7`, `+0xCC = 0x13A20100`; last macroblock `MC+0x28`, `+0x2C`; errors `AVC_VLD+0x284` under the mask |
| Abort | bit 0 of `MISC+0x100`, wait for `MISC+0x104` to read 0x11, `VLD+0x108 = 0x101` then 0 |

- Addresses are device addresses: the stream's shifted by 6, a motion buffer's by 4, the picture's by 9 in `MC+0x224` and by 4 in `MC+0x854`.
- Buffers (cached ION, filed under M4U port 10 because the kernel's table has no decoder port): the stream, terminated as stock does, with 0x100 bytes of slack; 0x8000 for the bit reader; 0xF000 and 0x1E000 for line buffers; per slot a motion buffer of 64 bytes per macroblock plus 0x200; the pictures.
- Stored layout: luma in blocks of 16 columns by 32 lines along a block row; chroma after luma (at stride x lines) in 16x16 blocks, Cb and Cr interleaved; height rounded up to an even number of macroblock rows (720 stored as 736).
- Two stock habits are kept: empty list entries get the previous picture's address, and the B lists are written for every picture, for our recordings each the P list.
- Kernel: `VCODEC_LOCKHW` type 6 powers the block and sets the selector to 2 (about 105 µs; release 110 µs). `VCODEC_WAITISR` type 6 fails with ENOENT on timeout. The handler clears nothing unless bit 16 of `MISC+0xA4` is set.
- Release runs `vdec_polling_status`, which loops forever while `VLD+0xF4` has bit 15 set and bit 0 clear. The driver therefore checks that word first and aborts the picture.
- After a timed-out wait, a wait with timeout 0 drains the event. Waits count in 4 ms ticks and last one tick longer than requested. An interrupt between a wait's return and the reset of its flag is lost.
- Damaged slices mostly leave a picture that never finishes: no interrupt, no error bit, the last macroblock position frozen. Zeroed slices end with error bit 4 or 7. The abort always leaves the block idle.
- Start to interrupt: 1280x720 2.3 to 3.2 ms (5 to 6 ms when the CPU sleeps through the wait); 640x360 1.6 ms; 854x480 3.0 ms; 1920x1080 7.5 ms; at most 8.3 ms for an undamaged picture, at 1920x1088.

**Conformance.** The CRC32 of every displayed picture matches ffmpeg's for r1metal's recordings, and for the streams of x264 and of ffmpeg's conformance suite that use none of what the limits below name.

**Limits**: progressive video, 4:2:0 at 8 bits, no lossless macroblocks, at least 4 macroblocks each way, at most 128 across and 8160 in total (1920x1088), no slice groups, B slices or switching slices, no scaling lists, no long-term references or memory management commands, no order counts in cycles, one size per stream. `r1_funcore::codec::h264::read` refuses the coding tools among these and names the reason (`Flaw::Unsupported`); a size the decoder does not take is refused where its buffers are laid out (`hw::vdec::Layout::from_sequence`, told by `vdec::to_unfit_text`), and a second size in a stream as a sample its buffers were not made for (`Stream::fits`, `Unsupported::Mismatch`).

## ION

`staging/android/mtk_ion/` (`ion-ioctl.c`, `mtk/ion_drv.c`, `mtk/ion_drv.h`, `mtk/ion_mm_heap.c`); numbers in `uapi/mtk_ion/ion.h`, heap ids in `mtk_ion/mtk/mtk_ion.h`.

| Call | Layout |
|---|---|
| `ION_IOC_ALLOC` `_IOWR('I', 0, 32)` | `len` u64, `align` u64, `heap_id_mask` u32 (`1 << 10`, the multimedia heap), `flags` u32, `handle` int (out) |
| `ION_IOC_FREE` `_IOWR('I', 1, 4)` | handle |
| `ION_IOC_SHARE` `_IOWR('I', 4, 8)` | handle, dma-buf fd (out); mmap the fd for CPU access |
| `ION_IOC_CUSTOM` `_IOWR('I', 6, 16)` | command u32 (0 `ION_CMD_SYSTEM`, 1 `ION_CMD_MULTIMEDIA`), pad, pointer to the command's struct |

- Flags 0: uncached, write-combining, zeroed pages. Flags 3 (`ION_FLAG_CACHED | ION_FLAG_CACHED_NEEDS_SYNC`), which the firmware uses: a cached mapping created immediately, kept coherent by explicit syncs. Flag 1 alone maps on page faults.
- Cache sync: `ION_CMD_SYSTEM` with `struct ion_sys_data` (120 bytes): `sys_cmd` 0 at offset 0, handle at 8, user address at 16, size u32 at 24, type at 28 (0 clean, 1 invalidate, 2 flush). The range must lie within one mapping of an ION buffer.
- Sync rules: flush a new buffer whole once before any DMA (recycled pages were zeroed through the cache). Invalidate a buffer a DMA wrote before the CPU reads it (0.4 to 0.7 ms for 2.5 MB). Flush a buffer the CPU wrote before a DMA reads it.
- Device address: `ION_CMD_MULTIMEDIA` with `struct ion_mm_data` (80 bytes), `ION_MM_GET_IOVA` (`mm_cmd` 8 at 0): handle at 8, `module_id` at 16 (an M4U port), `security` 20, `coherent` 24, reserved 28 and 32, `phy_addr` u64 at 40, `len` u64 at 48; both go in as 0 and come back as address and size. The first call maps the buffer read-write and the address stays fixed. Later calls return the same address whatever port they name; freeing unmaps it. The JPEG driver and the scaler take fds and map them themselves.
- Pages may lie above 4 GiB (RAM 0x40000000 to 0x13fffefff); the M4U page table carries bit 32. `/proc/m4u_dbg/buffer` lists mapped buffers; `/proc/self/pagemap` shows nothing for them.

## M4U

`m4u/2.0/m4u.c` (MT6765 builds M4U 2.0), ports in `m4u/mt6765/m4u_port.h`. One page table serves all ports, so a device address is valid for any block. Each port has a control word in its larb at `0x380 + 4 * port`; bit 0 switches translation on.

| Larb | Base | Ports (global id) |
|---|---|---|
| 0, display and scaler | 0x14003000 | DISP_OVL0 0, DISP_2L_OVL0 1, DISP_RDMA0 2, DISP_WDMA0 3, MDP_RDMA0 4, MDP_WDMA0 5, MDP_WROT0 6, DISP_FAKE0 7 |
| 1, codecs | 0x17010000 | VENC_RCPU 8, VENC_REC 9, VENC_BSDMA 10, VENC_SV_COMV 11, VENC_RD_COMV 12, JPGENC_RDMA 13, JPGENC_BSDMA 14, VENC_CUR_LUMA 15, VENC_CUR_CHROMA 16, VENC_REF_LUMA 17, VENC_REF_CHROMA 18 |
| 2, pass 2 | 0x15021000 | CAM_IMGI 19 to CAM_RSC_WDMA 30 |
| 3, camera | 0x1a002000 | CAM_IMGO 31, RRZO 32, AAO 33, AFO 34, LSCI0 35, LSCI1 36, PDO 37, BPCI 38, LCSO 39, RSSO_A 40, RSSO_B 41, UFEO 42, SOC0 43, SOC1 44, SOC2 45 (the CAMSV write DMAs), CCUI 46, CCUO 47, CACI 48, RAWI_A 49, RAWI_B 50, CCUG 51 |

- Larb 1's words read 0x9 as soon as it has power. The decoder uses the same ports under other names.
- Larb 3's words lie outside every range `/dev/camera-isp` maps; `ISP_LARB_MMU_CTL` writes one whole word (larbs 2, 3, 5, 6). The words reset when the camera power domain drops, so the firmware writes 1 to all 21 larb 3 words at every camera power up, before the CAMSV DMA starts. A wrong address then causes a translation fault that names the port.
- The fault handler reads only the faulting port's control word in its own larb, which is powered while its master runs. `/proc/m4u_dbg/port` powers each larb around its read (patch `0031`).

## Sources

- https://raw.githubusercontent.com/torvalds/linux/master/drivers/media/platform/mediatek/vcodec/encoder/venc/venc_h264_if.c
- https://raw.githubusercontent.com/torvalds/linux/master/drivers/media/platform/mediatek/vcodec/encoder/venc_vpu_if.c
- https://raw.githubusercontent.com/torvalds/linux/master/drivers/media/platform/mediatek/vcodec/decoder/vdec/vdec_h264_if.c
- https://raw.githubusercontent.com/torvalds/linux/master/drivers/media/platform/mediatek/vcodec/decoder/vdec_vpu_if.c
- rsync://fate-suite.ffmpeg.org/fate-suite/h264-conformance/

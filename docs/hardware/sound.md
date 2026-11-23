# Sound

The sound card: its parts, controls and levels. How the firmware drives it is in [Media](../firmware/media.md#sound).

| Part | What |
|---|---|
| Card | card 0, `mt-snd-card`: MediaTek's MT6765 driver with the MT6357 converters (`sound/soc/mediatek/common_int`, `mt6765`, `codec/mt6357`) |
| Playback | `/dev/snd/pcmC0D0p` |
| Capture | `/dev/snd/pcmC0D1c` |
| Speaker | one, behind the MT6357 line output and an Awinic AW87xxx amplifier at I2C 0x58 on bus 0 |
| Microphones | two analog: main on `AIN0` (converter 1, left), second on `AIN2` (converter 2, right) |

No block decodes or encodes audio: there is no audio DSP, tunnelled playback or voice wakeup, and Rabbit's Android lists only software codecs (`media_codecs_mediatek_audio.xml`). The hardware converts, routes, mixes and amplifies.

## Controls

- **Speaker.** `Speaker_Amp_Switch` `On` powers the converter and the line output, and Rabbit's codec hook loads the amplifier profile `Music`; `Off` powers both down and sets the amplifier profile `Off` (patch `0009`). The profiles (`Music`, `Receiver`, `Off`) come from `aw87xxx_acf.bin`, which the boot image includes. `Lineout_PGAR_GAIN` ranges from `8Db` to `-10Db` in 1 dB steps, plus `-40Db`. The driver starts at `8Db`; Rabbit's Android uses `-2Db` (`PlaybackVolAna_AudioParam.xml`), and what r1metal sets is in [Media](../firmware/media.md#sound).
- **Microphones.** `Audio_MIC1_Mode_Select` (and `MIC2`) sets the coupling, `Audio_MicSource1_Setting` `ADC1` the source, `Audio_PGA1_Setting` (and `PGA2`) the preamp gain; then `Audio_ADC_1_Switch` (and `ADC_2`) and `Audio_Preamp1_Switch` `IN_ADC1` (`Audio_Preamp2_Switch` `IN_ADC3` for the second). To switch off, set the preamps to `OPEN` and the converter switches to `Off`. The sequences come from Rabbit's `audio_device.xml`. Rabbit uses mode 2, DC coupled (`GetMicDeviceMode` in `audio.primary.mt6765.so`). `DCCMODE` and `ACCMODE` give the same signal; `DCCMODE` settles in about 50 ms instead of 110 ms, with a thump of up to -26 dBFS meanwhile.
- **Controls with side effects.** Reading `Audio HP ImpeDance Setting` runs a headphone impedance measurement; reading `Audio IRQ1 CNT` or `Audio IRQ2 CNT` turns a clock on to read a register; writing `PMIC_REG_CLEAR` runs a test path through the speaker and both microphones; `Pmic_Loopback` and the `SineTable` controls route test signals. Listing the controls and their types is safe. Switch handlers read the value as 64 bits even for enumerations, so a write must zero the upper half.

## Rates and levels

| Measurement | Value |
|---|---|
| Rates | playback 16 and 48 kHz; capture 16, 24 and 48 kHz; playback at 24 kHz is refused |
| 1 kHz at -30 dBFS from the speaker (`-10Db`), preamp 18 dB | -47 dBFS at the main microphone, -50 dBFS at the second; harmonics 50 dB down |
| Noise floor at 48 kHz, preamp 0, 6, 12, 18, 24 dB | -88.0, -82.8, -77.7, -72.5, -67.4 dBFS |
| Preamp `30Db` | same as `24Db` |
| Playback at the microphone (`-2Db` out, 12 dB in) | 15 dB below the played level |

Each 6 dB preamp step raises the noise floor by 5.1 to 5.2 dB, so it gains less than 1 dB of signal-to-noise and costs 6 dB of headroom. Rabbit's tables use 12 dB for voice recognition and camera recording and the maximum for the sound recorder (`RecordVol_AudioParam.xml`, `VolumeGainMapUL_AudioParam.xml`). Speech next to the device at 12 dB peaks below -30 dBFS, leaving about 30 dB unused; at 24 dB, the preamp's maximum, the signal is 12 dB higher and the noise floor 10.3 dB higher, which is why r1metal's one gain for the main microphone ([Media](../firmware/media.md#sound)) is that.

## End of a stream

The AFE's fetch pointer (`AFE_DL1_CUR`) runs 0xc0 bytes (96 samples, 2 ms at 48 kHz) ahead of the audible output. The stock driver treats the end of every drained stream as an underflow, and its `fill_silence` callback does nothing, so the ALSA core's silence filling has no effect; patches `0022` and `0023` fix both. With `silence_size` at the boundary and `silence_threshold` 0, the core zeroes what the fetch pointer has passed, one period at a time, and never touches data ahead of `appl_ptr`.

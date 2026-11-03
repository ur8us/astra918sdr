# USB audio transceiver

Astra918 accepts mono S16_LE 12 kHz USB playback alongside 12 kHz recording,
CAT and 120 ksps I/Q. Select its playback device in WSJT-X or another
single-tone digital-mode application. USB/LSB selects CAT dial plus/minus the
measured audio tone. Supported audio tones span 100–5000 Hz.

The estimator measures RMS and instantaneous frequency over a rolling 10 ms
window. Firmware targets 1 ms frequency updates. Window settling, USB command
processing and relay switching add latency. This path supports FT8/FT4-style
single-tone FSK. Speech and simultaneous tones require a different modulator.

## Connections

The current Pico 2 firmware uses the following connections. GPIO numbers refer
to the RP2350 signals printed on the Pico 2 pinout. The DRM-1000 test-point
assignments also appear in the
[EEVblog hardware post](https://www.eevblog.com/forum/rf-microwave/astra918-cmx918rp2350-based-receiver-0-07-to-130-mhz/msg6368579/#msg6368579).

| RP2350 pin | CMX918 / DRM-1000 connection | Function |
| --- | --- | --- |
| GP0 | CMX918 pin 17, SPI MOSI / I²C data, DRM-1000 TP4 | I²C SDA, bidirectional register control |
| GP1 | CMX918 pin 18, SPI SCLK / I²C clock, DRM-1000 TP1 | I²C SCL, 400 kHz control clock |
| GP8 | CMX918 pin 23, I2S_DATA / SDA, DRM-1000 TP11 | I/Q serial data input to Pico 2 |
| GP9 | CMX918 pin 22, I2S_FRAME / CS, DRM-1000 TP10 | I/Q frame / SPI chip-select input to Pico 2 |
| GP10 | CMX918 pin 24, I2S_SCLK, DRM-1000 TP9 | I/Q serial clock input to Pico 2 |
| GP2 | CMX918 pin 14, RESETN | Chip reset, actively driven low then high |
| GP20 | CMX918 pin 8, CLK_OUT / EXT_LO_IN | LDIV input to the RP2350 clock divider during TX |
| GP21 | External RF circuit | Normal divided RF output |
| GP13 | External RF circuit | Inverted RF output by default, selectable In-phase |
| GP6 | External TX/RX relay or PTT circuit | PTT, high during TX |
| GP25 | Pico 2 onboard LED | TX indicator |
| GND | CMX918 / DRM-1000 GND and external circuit ground | Common ground |

CMX918 controls the I/Q data clock and frame signals. Pico 2 captures these
through PIO in SPI-slave format. Register control uses the separate I²C bus.

For DRM-1000, hold the module's own processor RESET input low so its processor
releases control of the CMX918. This is separate from CMX918 RESETN on pin 14,
which connects to GP2. Feed the module's 3.3 V supply through a 3.3 V LDO whose
input is Pico 2 VBUS (USB 5 V), as shown in the project block diagram.

The CMX918-to-Pico clock link labelled VCO in the block diagram carries the
L-divider output during transmission. The connections above match
[main.rs](../firmware/src/main.rs), [capture.rs](../firmware/src/capture.rs)
and [tx_output.rs](../firmware/src/tx_output.rs).

GP13 and GP21 are low in RX. The output uses GPOUT0 with the GP20 input
inverted. Differential mode inverts GP13 through its RP2350 OUTOVER field.
Both pins share the same clock divisor.

## Saved settings

| GUI control | Default | Range |
| --- | --- | --- |
| Enable transmit | Enabled | Enabled or disabled |
| Audio trigger level | −40 dBFS | −60 to −6 dBFS |
| Audio key delay | 10 ms | 0–255 ms, minimum one 1 ms tick |
| Audio unkey delay | 100 ms | 0–255 ms, minimum one 1 ms tick |
| TX/RX Relay delay | 50 ms | 0–1000 ms |
| TX outputs | Differential | Differential or In-phase |
| TX outputs drive strength | 12 mA | 2, 4, 8 or 12 mA |
| TX outputs edge rate | Fast | Slow or Fast |

Edits apply to the live transceiver. Explicit Save persists all current
settings. The receiver's I/Q compensation policies are saved independently.
Factory reset restores both RX and TX defaults.

```sh
target/release/astra918ctl tx-config on --threshold -40 --key-ms 10 --unkey-ms 100
target/release/astra918ctl tx-relay-delay 50
target/release/astra918ctl tx-outputs differential
target/release/astra918ctl tx-drive 12
target/release/astra918ctl tx-slew fast
target/release/astra918ctl save
```

Each `tx-relay-delay`, `tx-outputs`, `tx-drive` or `tx-slew` command queries
its value when the optional argument is omitted. Disable transmit to suppress
audio-triggered keying. USB ownership remains exclusive to one vendor client.

## TX and RX switching

TX entry pauses capture and resets CMX918 through GP2 with settling delays.
It programs a 1.5–1.9 GHz VCO, verifies PLL/reference lock, disables digital
processing with `$B9=0` and enables the LDIV test path with `$5D=2`.
PTT and the LED go high, firmware waits the relay delay, and RF output starts.
Audio is requalified after that wait. A tone that has already ended cannot
start RF output.

On returning to RX, RF outputs go low first. Firmware waits the relay delay,
releases PTT, resets CMX918 and restores the saved live receiver configuration,
including the selected I/Q compensation policy. Recoverable TX faults use
the same release sequence. A fatal firmware panic immediately mutes RF and
releases PTT.

Recording and I/Q deliver zero samples throughout TX and capture recovery.
The USB interfaces remain enumerated and I/Q sequence/sample counters remain
contiguous within each generation. CAT remains available. The USB recording,
CDC and vendor interface numbers retain their original assignments. Playback
adds interfaces 5/6 and endpoint `0x06`.

## Frequency planning

The planner tries LDIV at 250–300 MHz with an even external divisor first,
then an odd divisor in that range. It next tries 240–316 MHz, again preferring
even divisors. Within the first feasible tier it selects LDIV closest to
280 MHz. External divisors range from 1 to 65535.

While keyed, the CMX918 L divider and RP2350 divisor stay fixed. Audio and CAT
retunes write only changed N/F bytes in `$2B..$2F`. Retuning outside the fixed
plan's LDIV/VCO limits stops transmission and recovers RX. PLL/reference lock
is checked periodically. The CMX918 test output's quality is frequency dependent.

RX also uses explicit fractional PLL coefficients at whole-Hz tuning precision.
The GUI's USB audio tuning error reports nominal PLL/NCO quantization, including
the selected audio offset. Crystal/TCXO error requires an external reference.

## USB commands and compatibility

The complete packet and status layouts are in [PROTOCOL.md](PROTOCOL.md).

| Command | Hex ID | Operation |
| --- | --- | --- |
| TX configuration | 4A | Empty query or four-byte enable/threshold/audio-key/audio-unkey setter |
| Carrier control | 43 | `<BQ>` mode 0 stop, mode 1 output Hz, mode 2 LDIV Hz |
| LDIV frequency measurement | 44 | Eight FC0 status/result pairs during LDIV-only mode |
| USB audio tuning error | 45 | Signed i64 microhertz |
| TX drive strength | 46 | Empty query or one-byte mA setter |
| TX edge rate | 47 | Empty query or one-byte slow=0/fast=1 setter |
| TX outputs | 48 | Empty query or Differential=0/In-phase=1 setter |
| Relay delay | 49 | Empty query or u16 milliseconds setter |

Command `0x3E` retains the receiver I/Q compensation mask/value operation.
Combined-layout status advertises byte 120 bit 4, reports TX enable and
threshold at bytes 62/63, and retains I/Q settings/status at 123/124.
Audio key/unkey delays occupy 125/126 and relay delay occupies 60–61.
The shared host decoder also accepts the earlier separate RX and TX layouts.

Flash format 9 migrates receiver versions 1–4 and TX versions 4–8. Stored
capabilities distinguish the two version-4 layouts. Existing TX selections
are retained, and settings absent from older records receive their defaults.

## USB maintenance utilities

These scripts use PyUSB through the transceiver's vendor interface.

```sh
python3 scripts/tx_carrier.py 14074000
python3 scripts/tx_carrier.py 14074000 --leave
python3 scripts/tx_carrier.py --stop
python3 scripts/tx_carrier.py 14074000 --drive-ma 12 --slew fast --save
```

The carrier tool normally waits for a key and restores RX. `--leave` keeps
the carrier running after exit. `--stop` recovers RX. `--save` explicitly
saves the full configuration before starting the carrier, or after stopping it.
Ctrl-C cleanup waits for an active USB command before sending STOP.

The GUI's Test frequency toggle uses command `0x43` directly and stops its
carrier on disconnect or normal window close. After an interrupted host
connection, use the carrier tool's `--stop` to request and confirm RX recovery.

LDIV measurement holds GP13, GP21 and PTT low, temporarily assigns GP20 to
FC0, and restores its pad and clock configuration afterward. A result contains
eight little-endian `<II>` status/raw pairs with 31.25 Hz resolution per count.
DONE must be set and FAIL/DIED clear. Measurement is refused during keyed RF.

## Shared GPIO drive strength and transformer estimates

Drive strength and slew settings apply to both output pads. Differential mode
allows a transformer primary between GP13 and GP21. The following estimates
assume ideal complementary 3.3 V square waves into a 50 Ω secondary load.

| Drive setting | Primary/secondary turns | Reflected primary load | Ideal fundamental power |
| --- | --- | --- | --- |
| 4 mA | 4/1 | 800 Ω | 11 mW |
| 12 mA | 4/1 | 800 Ω | 11 mW |
| 12 mA | 3/1 | 450 Ω | 20 mW |

Actual output depends on pad loading, transformer and filter loss. The drive
setting selects a pad characteristic. RF current and power depend on the load.

## Validation

Offline coverage includes FT8/FT4 tone estimation, fixed-divider retunes,
relay event ordering, GPIO polarity/drive/slew, zero-filled USB stream continuity,
descriptor lifecycle, combined IQ/TX commands and migration of both saved layouts.
Builds cover RP2350A and RP2350B. Hardware RX/TX and relay timing checks for the
combined main-branch integration remain pending.

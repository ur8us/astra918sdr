# AST1 control and ASIQ stream, version 1

USB VID:PID `c0de:091a` is the development identity. Recording uses UAC1 0–1,
CDC 2–3, vendor 4 and playback UAC1 5–6. Playback OUT is `06`.
Vendor OUT `03`, control IN `84`, I/Q IN `85`. Do not claim
audio/CDC, detach their drivers, reset the device or set its configuration.
One vendor owner serializes commands; CAT has a separate bounded parser and
joins the same authoritative state machine. There is no unsolicited vendor
reply: poll status at 5 Hz. Firmware queues and stream buffers are bounded.

All binary integers are little endian. Control records are exactly 256 bytes:
magic `AST1` at 0, version byte 1 at 4, command at 5, status at 6 (zero requests),
zero at 7, sequence u32 at 8, payload length u16 at 12, zeros at 14–15, payload
at 16, then zero padding. Replies echo command/sequence. Reject mismatched or
malformed records and reconnect after partial-transfer timeout. Nonzero status
rejects a command without committing the proposed settings, except reference
selection: a failed clock switch retains the requested source in receiver state
so clients can report and recover from the fault.

| Command | ID | Payload |
|---|---|---|
| Version / Info | 10 / 11 | empty |
| Capabilities / Status | 12 / 13 | empty |
| Set / Get dial | 20 / 21 | u64 Hz / empty |
| Get rate | 23 | empty; fixed 120000 |
| Set / Get input | 24 / 25 | byte AUTO=0 LF=1 HF=2 VHF=3 / empty |
| Gain mode | 26 | block RF=0 IF=1; mode auto=0 manual=1 |
| Gain code / Get gains | 27 / 28 | block RF=0 IF=1 LF=2 attenuator=3, code / empty |
| Get wide filter | 2a | empty |
| Set / Get capacitor | 2d / 2e | u16 0–4095 / empty |
| I/Q start / stop | 30 / 31 | empty |
| Channel offset | 33 | i32 Hz |
| Audio mode | 34 | byte LSB=1 USB=2 |
| Audio passband | 35 | low u16, high u16 Hz; 0 ≤ low < high ≤ 5000 |
| Save / Retry | 36 / 37 | empty |
| Tune channel within spectrum | 38 | u64 dial Hz; changes dial/offset atomically, retaining the current RF center |
| Center channel and retune | 39 | u64 dial Hz; sets dial and RF center atomically with offset zero |
| Reference source | 3a | byte Internal=0, External=1; reconfigures CMX918 immediately |
| Logical GPIO values | 3b | byte affected-bit mask, byte desired values; bit 0 is GPIO0 |
| VFO sign | 3c | byte Auto=0, LO above signal=1, LO below signal=2 |
| IF frequency | 3d | byte Auto=0 (96 kHz), 96 kHz=1, 120 kHz=2 |
| I/Q compensation settings | 3e | byte affected-bit mask, byte desired values; bit 0 = continuous running, bit 1 = internal-pilot on large retune |
| Factory reset | 3f | empty; erase both saved-settings sectors and apply defaults now |
| Read CMX918 register | 40 | byte register address; reply is one byte |
| Write CMX918 register | 41 | byte register address, byte value; reply is the normal status snapshot |
| Calibrate I/Q now | 42 | empty; force one internal-pilot calibration at the current RF tune |
| Carrier control | 43 | `<BQ>` mode 0 stop, mode 1 RF Hz, mode 2 LDIV Hz, with a 40-byte start report |
| Measure frequency during LDIV test | 44 | empty, eight FC0 status/result pairs, LDIV-only mode |
| USB audio tuning error | 45 | empty, signed i64 nominal actual-minus-requested microhertz |
| TX outputs drive strength | 46 | empty query returns one byte, setter accepts 2/4/8/12 mA |
| TX outputs edge rate | 47 | empty query returns one byte, setter accepts slow=0 or fast=1 |
| TX outputs | 48 | empty query returns one byte, setter accepts Differential=0 or In-phase=1 |
| TX/RX Relay delay | 49 | empty query returns u16 milliseconds, setter accepts u16 0–1000, default 50 |
| TX configuration | 4a | empty query or four bytes, enable 0/1, signed RMS dBFS, audio key ms, audio unkey ms |

IDs above are hexadecimal. Setters normally reply with the complete status
snapshot. Raw register access requires I/Q to be stopped (status 6 otherwise),
uses one bounded I2C transaction, and does not change or save receiver settings.
Subsequent receiver configuration can overwrite a raw write. Arbitrary writes
can disturb reception, including USB audio, so callers must use the CMX918
register map. Legacy arbitrary sample-rate/FIR/BOOTSEL commands are not
part of this interface. Status codes: 0 OK, 1 command, 2 version, 3 length,
4 argument, 5 unsupported, 6 busy, 7 spectrum bounds, 8 I/O, 9 PLL, 10 internal.
Capabilities retain the inherited 204-byte layout; only rate slot zero (120000)
is selectable. Status byte 120 bit 0 advertises I/Q compensation settings;
bit 1 advertises factory reset;
bit 2 advertises one-shot internal-pilot calibration;
bit 3 advertises TX/RX relay delay;
bit 4 advertises the combined IQ/TX layout and command 4a;
bit 5 advertises VFO sign and IF selection,
bit 6 advertises reference selection and bit 7
advertises the eight logical GPIO values. This leaves the capabilities reply
compatible with existing clients. Do not mistake the inherited reserved rate
slots for support. Capabilities byte 31 bit 4 advertises raw register access.

Command 38 requires a configured receiver and validates the complete audio
passband inside the current spectrum. It changes the DSP epoch without writing
the CMX918 PLL or recalibrating RF. Command 39 performs an RF retune and resets
the channel offset. Both update the shared CAT dial in one transaction and
start a new I/Q generation. Older firmware rejects them with status 1; update
firmware before using command 38 for the applications' firmware audio offset
control. Existing CAT and command 20 tuning still preserve offset; command 33
still preserves dial. SDR++ and GUI center tuning send command 20 with
`center + audio offset`; their audio offset edits send command 38 with
`current center + new audio offset`. SDR++ Radio VFO offsets are independent
and never enter this firmware audio calculation. Command 39 remains available
for clients explicitly requesting zero-offset tuning.

The status payload is 128 bytes. Its authoritative fields are:

| Offset | Type | Meaning |
|---|---|---|
| 0 / 8 | u64 / u64 | Dial / whole-Hz hardware spectrum center |
| 16 / 20 / 24 | u32 each | Rate / hardware filter bandwidth / flags |
| 28 / 29 | u8 each | Requested / resolved input |
| 30 / 31 | u8 each | RF / IF gain mode (0 automatic) |
| 32 / 33 | u8 each | RF / IF gain codes |
| 34 / 35 | u8 each | I/Q enabled / configured |
| 36 | u32 | Stream generation |
| 40 / 44 / 48 / 52 | u32 each | I/Q drops / capture faults / USB faults / last error |
| 56 / 57 / 58 | u8 / u8 / i16 | Chip status / valid / raw RSSI |
| 60 / 62 / 63 | u16 / u8 / i8 | Relay delay ms / TX enable / TX RMS threshold dBFS, with byte 120 bit 4 |
| 64–75 | bytes | Unknown inherited loss counters, all ff |
| 76 / 78 / 79 | u16 / u8 / u8 | Capacitor / LF gain / LF attenuator |
| 80 / 88 | u64 / i32 | Exact spectrum center / signed channel offset |
| 92 / 94 | u8 / u16 | Audio mode / low edge |
| 93 | u8 | Shared TX output drive strength mA, with byte 127 bit 3 |
| 96 | u32 | Settings revision |
| 100 / 104 / 108 | u32 each | Audio underruns / overruns / USB stalls |
| 112 / 116 | u32 / u16 | Saved revision (ffffffff initially unsaved) / high edge |
| 118 / 119 | u8 / u8 | Requested reference source / logical GPIO bitmask |
| 120 / 121 / 122 | u8 / u8 / u8 | Feature bits / requested VFO sign / requested IF frequency |
| 123 / 124 | u8 / u8 | I/Q settings bits (same layout as command 3e) / pilot status: 0 never attempted, 1 complete, 2 unavailable or incomplete |
| 125 / 126 | u8 / u8 | Audio key/unkey milliseconds |
| 127 | u8 | Bit 0 TX support, 1 active TX, 2 tuning-error support, 3 drive support, 4 slew support, 5 Fast slew selected, 6 output-mode support, 7 In-phase selected |

The combined layout retains command 3e and the I/Q fields at 123/124. TX
configuration uses command 4a and bytes 62/63, 125/126. A TX-only legacy
layout has byte 127 bit 0 set, byte 120 bit 4 clear, and uses command 3e with
four bytes and status 123/124 for TX enable/threshold. The shared host decoder
distinguishes all three layouts. The GUI selects the appropriate TX command.
Command 3e I/Q edits and command 42 calibration return Busy during TX.
Carrier control and measurement details are in [TRX.md](TRX.md).

Other bytes are reserved. GPIO bits are receiver-owned state only; no physical
RP2350 pins are assigned. Settings apply immediately; only command 36 saves them.
Command 3f is the explicit destructive exception: it pauses I/Q, erases and
verifies both 4 KiB saved-settings sectors, resets the save sequence and applies
`Settings::default()` to the live receiver. Erasing both sectors prevents an
older alternating save from reappearing at boot. The status saved revision becomes
`ffffffff` (unsaved); later changes remain volatile until command 36. CAT and USB
audio interfaces remain enumerated. If RF reconfiguration fails after a
successful erase, the command still returns OK and status reports an RF fault;
Retry can restore reception. An erase/readback failure returns status 8. Do not
assume a successful reset without an OK reply.
I/Q running compensation defaults on; pilot-on-large-retune defaults off. Turning
the latter on calibrates on the first tune, on a change of RF input/reference/LO
sign/IF, or after the spectrum center has moved at least 500 kHz from the last
successful calibration. Audio offset edits using command 38 retain the center and
do not trigger pilot calibration. Pilot generation is turned off before reception
resumes. An unavailable pilot or a lock/readiness failure sets status byte 124 to 2 and does not
reject an otherwise valid RF tune; running compensation remains independent.
After a failed attempt, another automatic attempt is deferred until the center
moves 500 kHz from that failure or the RF path changes.
Status 124 is live diagnostic state, not a saved setting. Command 37 forces a
new pilot attempt when pilot-on-large-retune is enabled.
Command 42 forces a single attempt even when pilot-on-large-retune is off. It
requires an already configured receiver (status 8 otherwise), briefly pauses
I/Q capture, temporarily retunes to a documented discrete pilot frequency
(CMX918 datasheet Table 11), generates the internal tone, then switches the
pilot off and restores the requested RF tune before capture resumes. Its
normal status snapshot reports byte 124 as 1 for complete or 2 for unavailable
or incomplete; a pilot result of 2 leaves the RF tune valid. The command does
not change the automatic policy or write flash. I/O or restore failure returns
status 8 and marks an RF fault; Retry can reconfigure the receiver. No external
frequency generator is involved.
Auto sign uses LO above below 2 MHz and LO below from 2 MHz;
Auto IF uses 96 kHz. Forced LO below is rejected when the exact center is at
or below the selected IF. Sign/IF edits retune the same center and run the CMX918
IF-filter and PLL calibration; the dial and audio offset are retained. Flash record
format 9 reads receiver formats 1–4 and TX formats 4–8. Capabilities stored in
the record distinguish the two version-4 layouts. Fields absent in an older
format receive their current defaults. Formats 1 and 2 default sign/IF to Auto. Format 1
also defaults to Internal reference and clears all GPIO bits. The firmware state codecs in `firmware/src/astra.rs`
are normative. Gain tables and hardware constraints are in `controls.rs`.

An I/Q record is 2112 bytes: a 64-byte `ASIQ` header followed by 512 interleaved
signed ci16 pairs (I then Q). Header: magic 0, version 1 at 4, type 80 hex at 5,
header length u16 at 6, generation u32 at 8, sequence u32 at 12, first-sample
index u64 at 16, rate u32 at 24, count u16 at 28, reserved u16 at 30, exact center u64 at 32,
dial u64 at 40, settings revision u32 at 48, zero padding to 64.

Generation changes on reconfiguration/restart. It is legal to join at an
arbitrary first sequence/index; subsequent records within that generation
must be contiguous. Retunes discard stale queued generations. SDR++ publishes
matching frequency metadata before delivering the new samples. I/Q overflow
stops only I/Q; CAT/audio continue. Stop/disconnect must not reset USB audio.
Before Start, hosts should Stop and drain endpoint 85 to a quiet interval: a
previous interrupted transfer may leave a partial old frame in the USB FIFO.
Reset the frame parser only after draining. Keep I/Q reads independent of
control exchanges; a receiver reconfiguration can take longer than the I/Q
backpressure deadline. Join the reader before Stop/drain or transport teardown.
TX and RF recovery retain the requested USB stream with zero samples while
capture is unavailable. A false configured flag alone does not end that stream.
SDR++ keeps reading during control exchanges and discards its oldest decoded
frames if its UI queue fills while waiting for matching generation metadata.
Its control deadline is 15 seconds to cover the firmware's eight-second RF
configuration deadline plus reset and relay delays.

CAT uses semicolon-delimited ASCII, bounded 32-byte requests. `FA`/`MD` read
and set the dial/mode; `IF`, `ID`, `AI`, `FR`, `FW`, `FB`, `FT`, `PS`, `KS` and
`SL` are queries. The latter five answer WSJT-X/Hamlib initialization and
polling without introducing transmit or a second VFO: `FB` mirrors the shared
dial, `FT0` reports VFO A, `PS1` reports the powered receiver, `KS020` is a
fixed compatible keyer-speed value, and `SL` approximates the actual audio
low cutoff in the Kenwood 00..20 range. `AI0`, `FR0`, `RX` are receive-only
acknowledgements. `ZZST` reads health; `ZZRX` retries.
Setting the CDC CAT interface's line coding to 1200 baud enters RP2350 BOOTSEL
after the USB control transfer is acknowledged. This is a USB control request,
not a CAT text command or AST1 command; it does not save settings. Picotool's
`-f` vendor reset interface is not implemented.
Unsupported or invalid commands return `?;`; successful setters return no
text. Timeouts discard incomplete frames through their next delimiter. No TX
command is implemented. FW reports width in Hz, supported by Hamlib’s custom
width readback ([Hamlib 4.6.2 implementation](https://github.com/Hamlib/Hamlib/blob/4.6.2/rigs/kenwood/kenwood.c)).

Simulator ports N through N+3 carry the identical control/IQ/CAT and raw mono
PCM bytes. Test-only command 70 hex sets fault bits: bit0 fail next configure,
bit1 stall I/Q, bit2 simulated TX with zero I/Q/audio. Leaving simulated TX starts
a new receive generation. RF faults also send zeros while streaming is requested.
It is absent from the physical firmware. UTC-aligned WAV input
and PTY CAT are test adapters; simulated saves use the production CRC codec.

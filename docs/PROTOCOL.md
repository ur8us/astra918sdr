# AST1 control and ASIQ stream, version 1

USB VID:PID `c0de:091a` is the development identity. Five interfaces: UAC1 0–1,
CDC 2–3, vendor 4. Vendor OUT `03`, control IN `84`, I/Q IN `85`. Do not claim
audio/CDC, detach their drivers, reset the device or set its configuration.
One vendor owner serializes commands; CAT has a separate bounded parser and
joins the same authoritative state machine. There is no unsolicited vendor
reply: poll status at 5 Hz. Firmware queues and stream buffers are bounded.

All binary integers are little endian. Control records are exactly 256 bytes:
magic `AST1` at 0, version byte 1 at 4, command at 5, status at 6 (zero requests),
zero at 7, sequence u32 at 8, payload length u16 at 12, zeros at 14–15, payload
at 16, then zero padding. Replies echo command/sequence. Reject mismatched or
malformed records and reconnect after partial-transfer timeout. Nonzero status
rejects a command without committing the proposed settings.

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

IDs above are hexadecimal. Setters normally reply with the complete status
snapshot. Legacy arbitrary sample-rate/FIR/register/BOOTSEL commands are not
part of this interface. Status codes: 0 OK, 1 command, 2 version, 3 length,
4 argument, 5 unsupported, 6 busy, 7 spectrum bounds, 8 I/O, 9 PLL, 10 internal.
Capabilities retain the inherited 204-byte layout; only rate slot zero (120000)
is selectable. Do not mistake the inherited reserved rate slots for support.

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
| 0 / 8 | u64 / u64 | Dial / rounded hardware spectrum center |
| 16 / 20 / 24 | u32 each | Rate / hardware filter bandwidth / flags |
| 28 / 29 | u8 each | Requested / resolved input |
| 30 / 31 | u8 each | RF / IF gain mode (0 automatic) |
| 32 / 33 | u8 each | RF / IF gain codes |
| 34 / 35 | u8 each | I/Q enabled / configured |
| 36 | u32 | Stream generation |
| 40 / 44 / 48 / 52 | u32 each | I/Q drops / capture faults / USB faults / last error |
| 56 / 57 / 58 | u8 / u8 / i16 | Chip status / valid / raw RSSI |
| 64–75 | bytes | Unknown inherited loss counters, all ff |
| 76 / 78 / 79 | u16 / u8 / u8 | Capacitor / LF gain / LF attenuator |
| 80 / 88 | u64 / i32 | Exact spectrum center / signed channel offset |
| 92 / 94 | u8 / u16 | Audio mode / low edge |
| 96 | u32 | Settings revision |
| 100 / 104 / 108 | u32 each | Audio underruns / overruns / USB stalls |
| 112 / 116 | u32 / u16 | Saved revision (ffffffff initially unsaved) / high edge |

Other bytes are reserved. The firmware state codecs in `firmware/src/astra.rs`
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

CAT uses semicolon-delimited ASCII, bounded 32-byte requests. `FA`/`MD` read
and set the dial/mode, `IF`, `ID`, `AI`, `FR`, `FW` are queries; `AI0`, `FR0`,
`RX` are receive-only acknowledgements. `ZZST` reads health; `ZZRX` retries.
Unsupported or invalid commands return `?;`; successful setters return no
text. Timeouts discard incomplete frames through their next delimiter. No TX
command is implemented. FW reports width in Hz, supported by Hamlib’s custom
width readback ([Hamlib 4.6.2 implementation](https://github.com/Hamlib/Hamlib/blob/4.6.2/rigs/kenwood/kenwood.c)).

Simulator ports N through N+3 carry the identical control/IQ/CAT and raw mono
PCM bytes. Test-only command 70 hex sets fault bits: bit0 fail next configure,
bit1 stall I/Q. It is absent from the physical firmware. UTC-aligned WAV input
and PTY CAT are test adapters; simulated saves use the production CRC codec.
